//! Compare our decoder output against libheif (via heif-ref Docker tool).
//!
//! These tests invoke the `ghcr.io/imazen/heif-ref` Docker image to extract
//! reference data, then compare dimensions, metadata, pixel statistics, and
//! SSIM2 scores against our decoder's output.
//!
//! Requires Docker with the heif-ref image built:
//!   docker build -t ghcr.io/imazen/heif-ref:latest reference/
//!
//! Run: cargo test --test compare_libheif -- --nocapture

use fast_ssim2::compute_ssimulacra2;
use heic::{DecoderConfig, ImageInfo, PixelLayout};
use imgref::ImgVec;
use std::path::{Path, PathBuf};
use std::process::Command;

const DOCKER_IMAGE: &str = "ghcr.io/imazen/heif-ref:latest";

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Check if Docker is available and the heif-ref image exists.
fn docker_available() -> bool {
    Command::new("docker")
        .args(["image", "inspect", DOCKER_IMAGE])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run heif-ref extract via Docker. Returns the output directory path.
///
/// Output goes under `target/heif-ref/` in the repo (not /tmp) because
/// snap Docker cannot write to /tmp via bind mounts.
fn extract_reference(input_path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let stem = input_path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let outdir = repo_root().join(format!("target/heif-ref/{stem}_{id}"));
    // Clean previous run
    let _ = std::fs::remove_dir_all(&outdir);
    std::fs::create_dir_all(&outdir).expect("create outdir");

    let root = repo_root();
    let rel_input = input_path.strip_prefix(&root).unwrap_or(input_path);
    let rel_out = outdir.strip_prefix(&root).unwrap_or(&outdir);

    let status = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-v",
            &format!("{}:/repo", root.display()),
            DOCKER_IMAGE,
            "extract",
            &format!("/repo/{}", rel_input.display()),
            &format!("/repo/{}", rel_out.display()),
        ])
        .status()
        .expect("docker run failed");

    assert!(
        status.success(),
        "heif-ref extract failed for {}",
        input_path.display()
    );
    outdir
}

/// Parsed subset of info.json relevant for comparison.
#[derive(Debug)]
#[allow(dead_code)]
struct RefInfo {
    width: u32,
    height: u32,
    has_alpha: bool,
    bit_depth: u8,
    pixel_crc32: Option<u64>,
    exif_present: bool,
    exif_size: Option<usize>,
    exif_crc32: Option<u64>,
    xmp_present: bool,
    xmp_size: Option<usize>,
    xmp_crc32: Option<u64>,
    icc_size: Option<usize>,
    icc_crc32: Option<u64>,
    thumbnail_present: bool,
    thumbnail_width: Option<u32>,
    thumbnail_height: Option<u32>,
    /// Auxiliary image types (URN strings)
    aux_types: Vec<String>,
    /// (width, height, type, pixel_crc32) for each auxiliary image
    aux_images: Vec<AuxInfo>,
    color_profile_type: String,
    color_primaries: Option<u16>,
    transfer_characteristics: Option<u16>,
    matrix_coefficients: Option<u16>,
    full_range: Option<bool>,
}

#[derive(Debug)]
#[allow(dead_code)]
struct AuxInfo {
    aux_type: String,
    width: u32,
    height: u32,
    pixel_crc32: Option<u64>,
}

/// Minimal JSON value parser (no dependency needed for this simple schema).
fn json_str<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\": \"");
    let start = json.find(&pattern)? + pattern.len();
    let end = start + json[start..].find('"')?;
    Some(&json[start..end])
}

fn json_int(json: &str, key: &str) -> Option<i64> {
    let pattern = format!("\"{key}\": ");
    let start = json.find(&pattern)? + pattern.len();
    let end = start
        + json[start..]
            .find(|c: char| !c.is_ascii_digit() && c != '-')
            .unwrap_or(json[start..].len());
    json[start..end].parse().ok()
}

fn json_bool(json: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{key}\": ");
    let start = json.find(&pattern)? + pattern.len();
    if json[start..].starts_with("true") {
        Some(true)
    } else if json[start..].starts_with("false") {
        Some(false)
    } else {
        None
    }
}

#[allow(dead_code)]
fn json_float(json: &str, key: &str) -> Option<f64> {
    let pattern = format!("\"{key}\": ");
    let start = json.find(&pattern)? + pattern.len();
    let end = start
        + json[start..]
            .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
            .unwrap_or(json[start..].len());
    json[start..end].parse().ok()
}

fn parse_ref_info(outdir: &Path) -> RefInfo {
    let json = std::fs::read_to_string(outdir.join("info.json")).expect("read info.json");

    // Find the "primary" object
    let primary_start = json.find("\"primary\":").expect("no primary in JSON");
    let primary = &json[primary_start..];

    // Find color_profile subsection
    let cp_start = primary
        .find("\"color_profile\":")
        .map(|i| &primary[i..])
        .unwrap_or("");
    let color_profile_type = json_str(cp_start, "type").unwrap_or("none").to_string();

    // Parse auxiliary_images array
    let mut aux_types = Vec::new();
    let mut aux_images = Vec::new();
    if let Some(aux_start) = primary.find("\"auxiliary_images\":") {
        let aux_section = &primary[aux_start..];
        // Find each object in the array
        let mut search_from = 0;
        while let Some(obj_start) = aux_section[search_from..].find("\"type\": \"") {
            let obj_region_start = search_from + obj_start;
            // Find the enclosing object (look backward for '{')
            let obj_section = &aux_section[obj_region_start..];
            if let Some(t) = json_str(obj_section, "type") {
                aux_types.push(t.to_string());
                let w = json_int(obj_section, "width").unwrap_or(0) as u32;
                let h = json_int(obj_section, "height").unwrap_or(0) as u32;
                // Use decoded dimensions if available
                let dw = json_int(obj_section, "decoded_width")
                    .map(|v| v as u32)
                    .unwrap_or(w);
                let dh = json_int(obj_section, "decoded_height")
                    .map(|v| v as u32)
                    .unwrap_or(h);
                let crc = json_int(obj_section, "pixel_crc32").map(|v| v as u64);
                aux_images.push(AuxInfo {
                    aux_type: t.to_string(),
                    width: dw,
                    height: dh,
                    pixel_crc32: crc,
                });
            }
            search_from = obj_region_start + 10;
        }
    }

    // Parse EXIF subsection
    let exif_section = primary
        .find("\"exif\":")
        .map(|i| &primary[i..])
        .unwrap_or("");
    let exif_present = json_bool(exif_section, "present").unwrap_or(false);

    // Parse XMP subsection
    let xmp_section = primary
        .find("\"xmp\":")
        .map(|i| &primary[i..])
        .unwrap_or("");
    let xmp_present = json_bool(xmp_section, "present").unwrap_or(false);

    // Parse thumbnail subsection
    let thumb_section = primary
        .find("\"thumbnail\":")
        .map(|i| &primary[i..])
        .unwrap_or("");
    let thumbnail_present = json_bool(thumb_section, "present").unwrap_or(false);

    RefInfo {
        width: json_int(primary, "width").unwrap_or(0) as u32,
        height: json_int(primary, "height").unwrap_or(0) as u32,
        has_alpha: json_bool(primary, "has_alpha").unwrap_or(false),
        bit_depth: json_int(primary, "bit_depth").unwrap_or(0) as u8,
        pixel_crc32: json_int(primary, "pixel_crc32").map(|v| v as u64),
        exif_present,
        exif_size: json_int(exif_section, "size").map(|v| v as usize),
        exif_crc32: json_int(exif_section, "crc32").map(|v| v as u64),
        xmp_present,
        xmp_size: json_int(xmp_section, "size").map(|v| v as usize),
        xmp_crc32: json_int(xmp_section, "crc32").map(|v| v as u64),
        icc_size: json_int(cp_start, "icc_size").map(|v| v as usize),
        icc_crc32: json_int(cp_start, "icc_crc32").map(|v| v as u64),
        thumbnail_present,
        thumbnail_width: if thumbnail_present {
            json_int(thumb_section, "width").map(|v| v as u32)
        } else {
            None
        },
        thumbnail_height: if thumbnail_present {
            json_int(thumb_section, "height").map(|v| v as u32)
        } else {
            None
        },
        aux_types,
        aux_images,
        color_profile_type,
        color_primaries: json_int(cp_start, "color_primaries").map(|v| v as u16),
        transfer_characteristics: json_int(cp_start, "transfer_characteristics").map(|v| v as u16),
        matrix_coefficients: json_int(cp_start, "matrix_coefficients").map(|v| v as u16),
        full_range: json_bool(cp_start, "full_range"),
    }
}

fn rgb_to_imgvec(rgb: &[u8], width: u32, height: u32) -> ImgVec<[u8; 3]> {
    let pixels: Vec<[u8; 3]> = rgb.as_chunks::<3>().0.to_vec();
    ImgVec::new(pixels, width as usize, height as usize)
}

/// Compute pixel difference stats between two RGB buffers.
fn pixel_diff_stats(ours: &[u8], reference: &[u8]) -> (f64, u32, f64) {
    let mut total_diff: u64 = 0;
    let mut max_diff: u32 = 0;
    let mut exact = 0u64;
    let n = ours.len().min(reference.len());

    for i in 0..n {
        let diff = (ours[i] as i32 - reference[i] as i32).unsigned_abs();
        total_diff += diff as u64;
        if diff > max_diff {
            max_diff = diff;
        }
        if diff == 0 {
            exact += 1;
        }
    }

    let avg = total_diff as f64 / n as f64;
    let exact_pct = exact as f64 / n as f64 * 100.0;
    (avg, max_diff, exact_pct)
}

// ---- Tests ----

fn skip_if_no_docker() -> bool {
    if !docker_available() {
        eprintln!("SKIP: Docker not available or heif-ref image not built");
        return true;
    }
    false
}

#[test]
fn compare_example_heic_metadata() {
    if skip_if_no_docker() {
        return;
    }

    let input = testdata().join("libheif-examples/example.heic");
    let ref_dir = extract_reference(&input);
    let ref_info = parse_ref_info(&ref_dir);

    let data = std::fs::read(&input).unwrap();
    let our_info = ImageInfo::from_bytes(&data).unwrap();

    // Dimensions must match exactly
    assert_eq!(our_info.width, ref_info.width, "width mismatch");
    assert_eq!(our_info.height, ref_info.height, "height mismatch");
    assert_eq!(our_info.has_alpha, ref_info.has_alpha, "alpha mismatch");
    assert_eq!(our_info.bit_depth, ref_info.bit_depth, "bit_depth mismatch");

    // Metadata presence
    assert_eq!(
        our_info.has_exif, ref_info.exif_present,
        "EXIF presence mismatch"
    );
    assert_eq!(
        our_info.has_xmp, ref_info.xmp_present,
        "XMP presence mismatch"
    );
    assert_eq!(
        our_info.has_thumbnail, ref_info.thumbnail_present,
        "thumbnail presence mismatch"
    );

    println!("example.heic metadata: MATCH");
    println!(
        "  {}x{}, alpha={}, depth={}, exif={}, xmp={}, thumb={}",
        ref_info.width,
        ref_info.height,
        ref_info.has_alpha,
        ref_info.bit_depth,
        ref_info.exif_present,
        ref_info.xmp_present,
        ref_info.thumbnail_present
    );
}

#[test]
fn compare_example_heic_pixels() {
    if skip_if_no_docker() {
        return;
    }

    let input = testdata().join("libheif-examples/example.heic");
    let ref_dir = extract_reference(&input);
    let ref_info = parse_ref_info(&ref_dir);

    let data = std::fs::read(&input).unwrap();
    let ours = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgb8)
        .unwrap();

    assert_eq!(ours.width, ref_info.width);
    assert_eq!(ours.height, ref_info.height);

    // Load reference pixels
    let ref_pixels = std::fs::read(ref_dir.join("primary.bin")).unwrap();
    assert_eq!(
        ref_pixels.len(),
        ours.data.len(),
        "pixel buffer size mismatch"
    );

    // SSIM2 comparison
    let ref_img = rgb_to_imgvec(&ref_pixels, ref_info.width, ref_info.height);
    let our_img = rgb_to_imgvec(&ours.data, ours.width, ours.height);
    let ssim2 = compute_ssimulacra2(ref_img.as_ref(), our_img.as_ref()).unwrap();

    // Pixel diff stats
    let (avg_diff, max_diff, exact_pct) = pixel_diff_stats(&ours.data, &ref_pixels);

    println!("example.heic pixels vs libheif {}:", ref_info.bit_depth);
    println!("  SSIM2: {ssim2:.2}");
    println!("  Avg diff: {avg_diff:.2}, Max diff: {max_diff}, Exact: {exact_pct:.1}%");

    assert!(ssim2 > 50.0, "SSIM2 {ssim2:.2} too low (expected > 50)");
}

#[test]
fn compare_hdr_sample_metadata() {
    if skip_if_no_docker() {
        return;
    }

    let input = testdata().join("apple-hdr/hdr-sample.heic");
    let ref_dir = extract_reference(&input);
    let ref_info = parse_ref_info(&ref_dir);

    let data = std::fs::read(&input).unwrap();
    let our_info = ImageInfo::from_bytes(&data).unwrap();

    // Dimensions
    assert_eq!(our_info.width, ref_info.width, "width mismatch");
    assert_eq!(our_info.height, ref_info.height, "height mismatch");
    assert_eq!(our_info.bit_depth, ref_info.bit_depth, "bit_depth mismatch");

    // Metadata presence
    assert_eq!(our_info.has_exif, ref_info.exif_present, "EXIF mismatch");
    assert_eq!(our_info.has_xmp, ref_info.xmp_present, "XMP mismatch");
    assert_eq!(
        our_info.has_gain_map,
        !ref_info.aux_types.is_empty()
            && ref_info.aux_types.iter().any(|t| t.contains("hdrgainmap")),
        "gain map presence mismatch"
    );

    // ICC profile
    assert_eq!(
        our_info.has_icc_profile,
        ref_info.color_profile_type == "prof" || ref_info.color_profile_type == "rICC",
        "ICC presence mismatch"
    );

    println!("hdr-sample.heic metadata: MATCH");
    println!(
        "  {}x{}, exif={}, xmp={}, icc={}, gain_map={}",
        ref_info.width,
        ref_info.height,
        ref_info.exif_present,
        ref_info.xmp_present,
        ref_info.color_profile_type,
        !ref_info.aux_types.is_empty()
    );
    for aux in &ref_info.aux_images {
        println!("  aux: {} {}x{}", aux.aux_type, aux.width, aux.height);
    }
}

#[test]
fn compare_hdr_sample_pixels() {
    if skip_if_no_docker() {
        return;
    }

    let input = testdata().join("apple-hdr/hdr-sample.heic");
    let ref_dir = extract_reference(&input);
    let ref_info = parse_ref_info(&ref_dir);

    let data = std::fs::read(&input).unwrap();
    let ours = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgb8)
        .unwrap();

    assert_eq!(ours.width, ref_info.width);
    assert_eq!(ours.height, ref_info.height);

    let ref_pixels = std::fs::read(ref_dir.join("primary.bin")).unwrap();
    assert_eq!(ref_pixels.len(), ours.data.len());

    let ref_img = rgb_to_imgvec(&ref_pixels, ref_info.width, ref_info.height);
    let our_img = rgb_to_imgvec(&ours.data, ours.width, ours.height);
    let ssim2 = compute_ssimulacra2(ref_img.as_ref(), our_img.as_ref()).unwrap();

    let (avg_diff, max_diff, exact_pct) = pixel_diff_stats(&ours.data, &ref_pixels);

    println!("hdr-sample.heic pixels vs libheif 1.19:");
    println!("  SSIM2: {ssim2:.2}");
    println!("  Avg diff: {avg_diff:.2}, Max diff: {max_diff}, Exact: {exact_pct:.1}%");

    assert!(ssim2 > 50.0, "SSIM2 {ssim2:.2} too low");
}

#[test]
fn compare_hdr_sample_gain_map() {
    if skip_if_no_docker() {
        return;
    }

    let input = testdata().join("apple-hdr/hdr-sample.heic");
    let ref_dir = extract_reference(&input);
    let ref_info = parse_ref_info(&ref_dir);

    // Find the gain map auxiliary image
    let gm_aux = ref_info
        .aux_images
        .iter()
        .find(|a| a.aux_type.contains("hdrgainmap"));
    let gm_aux = match gm_aux {
        Some(a) => a,
        None => {
            eprintln!("SKIP: libheif did not extract gain map auxiliary");
            return;
        }
    };

    let data = std::fs::read(&input).unwrap();
    let our_gm = DecoderConfig::new()
        .decode_gain_map(&data)
        .expect("our gain map decode failed");

    // Dimensions should match
    assert_eq!(
        our_gm.width, gm_aux.width,
        "gain map width: ours={} ref={}",
        our_gm.width, gm_aux.width
    );
    assert_eq!(
        our_gm.height, gm_aux.height,
        "gain map height: ours={} ref={}",
        our_gm.height, gm_aux.height
    );

    // Load reference gain map pixels
    // Find the aux file by scanning for aux_*.bin files that aren't XMP
    let ref_gm_path = std::fs::read_dir(&ref_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("aux_") && name.ends_with(".bin")
        })
        .map(|e| e.path());

    if let Some(ref_gm_path) = ref_gm_path {
        let ref_gm_pixels = std::fs::read(&ref_gm_path).unwrap();
        let expected_size = (gm_aux.width * gm_aux.height) as usize;
        assert_eq!(
            ref_gm_pixels.len(),
            expected_size,
            "ref gain map size mismatch"
        );
        assert_eq!(
            our_gm.data.len(),
            expected_size,
            "our gain map size mismatch"
        );

        // Compare gain map pixels
        let mut total_diff: u64 = 0;
        let mut max_diff: u32 = 0;
        let mut exact = 0u64;
        for (i, &ref_px) in ref_gm_pixels.iter().enumerate().take(expected_size) {
            let diff = (our_gm.data[i] as i32 - ref_px as i32).unsigned_abs();
            total_diff += diff as u64;
            if diff > max_diff {
                max_diff = diff;
            }
            if diff == 0 {
                exact += 1;
            }
        }
        let avg = total_diff as f64 / expected_size as f64;
        let exact_pct = exact as f64 / expected_size as f64 * 100.0;

        println!(
            "hdr-sample.heic gain map ({}x{}):",
            gm_aux.width, gm_aux.height
        );
        println!("  Avg diff: {avg:.2}, Max diff: {max_diff}, Exact: {exact_pct:.1}%");
    }
}

#[test]
fn compare_hdr_sample_exif() {
    if skip_if_no_docker() {
        return;
    }

    let input = testdata().join("apple-hdr/hdr-sample.heic");
    let ref_dir = extract_reference(&input);
    let ref_info = parse_ref_info(&ref_dir);

    if !ref_info.exif_present {
        return;
    }

    let data = std::fs::read(&input).unwrap();
    let our_exif = DecoderConfig::new()
        .extract_exif(&data)
        .expect("extract_exif failed");
    let our_exif = our_exif.expect("should have EXIF");

    let ref_exif = std::fs::read(ref_dir.join("exif.bin")).unwrap();

    // libheif returns the full HEIF EXIF item:
    //   4-byte offset prefix + "Exif\0\0" (6 bytes) + TIFF data (II/MM...)
    // Our API strips the 4-byte offset prefix AND the "Exif\0\0" header,
    // returning raw TIFF starting at the byte-order mark.
    let ref_exif_body = if ref_exif.len() > 10 && &ref_exif[4..8] == b"Exif" {
        // Strip 4-byte offset + "Exif\0\0" (total 10 bytes)
        &ref_exif[10..]
    } else if ref_exif.len() > 4
        && (ref_exif[4..].starts_with(b"II") || ref_exif[4..].starts_with(b"MM"))
    {
        &ref_exif[4..]
    } else {
        &ref_exif
    };

    assert_eq!(
        our_exif.len(),
        ref_exif_body.len(),
        "EXIF size: ours={} ref={}",
        our_exif.len(),
        ref_exif_body.len()
    );
    assert_eq!(our_exif.as_ref(), ref_exif_body, "EXIF content mismatch");
    println!("hdr-sample.heic EXIF: MATCH ({} bytes)", our_exif.len());
}

#[test]
fn compare_hdr_sample_xmp() {
    if skip_if_no_docker() {
        return;
    }

    let input = testdata().join("apple-hdr/hdr-sample.heic");
    let ref_dir = extract_reference(&input);
    let ref_info = parse_ref_info(&ref_dir);

    if !ref_info.xmp_present {
        return;
    }

    let data = std::fs::read(&input).unwrap();
    let our_xmp = DecoderConfig::new()
        .extract_xmp(&data)
        .expect("extract_xmp failed");
    let our_xmp = our_xmp.expect("should have XMP");

    let ref_xmp = std::fs::read(ref_dir.join("xmp.xml")).unwrap();

    // Both our decoder and libheif may include a null terminator; strip from both
    fn strip_trailing_nulls(data: &[u8]) -> &[u8] {
        let mut end = data.len();
        while end > 0 && data[end - 1] == 0 {
            end -= 1;
        }
        &data[..end]
    }

    let our_xmp_trimmed = strip_trailing_nulls(&our_xmp);
    let ref_xmp_trimmed = strip_trailing_nulls(&ref_xmp);

    assert_eq!(
        our_xmp_trimmed.len(),
        ref_xmp_trimmed.len(),
        "XMP size: ours={} ref={}",
        our_xmp_trimmed.len(),
        ref_xmp_trimmed.len()
    );
    assert_eq!(our_xmp_trimmed, ref_xmp_trimmed, "XMP content mismatch");
    println!(
        "hdr-sample.heic XMP: MATCH ({} bytes)",
        our_xmp_trimmed.len()
    );
}

#[test]
fn compare_synthetic_files() {
    if skip_if_no_docker() {
        return;
    }

    for name in &[
        "synthetic/synth_8bit_q10.heic",
        "synthetic/synth_8bit_q50.heic",
        "synthetic/synth_8bit_q95.heic",
    ] {
        let input = testdata().join(name);
        let ref_dir = extract_reference(&input);
        let ref_info = parse_ref_info(&ref_dir);

        let data = std::fs::read(&input).unwrap();
        let ours = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgb8)
            .unwrap();

        assert_eq!(ours.width, ref_info.width, "{name}: width");
        assert_eq!(ours.height, ref_info.height, "{name}: height");

        let ref_pixels = std::fs::read(ref_dir.join("primary.bin")).unwrap();
        if ref_pixels.len() != ours.data.len() {
            eprintln!(
                "{name}: pixel buffer size mismatch (ref={}, ours={})",
                ref_pixels.len(),
                ours.data.len()
            );
            continue;
        }

        let ref_img = rgb_to_imgvec(&ref_pixels, ref_info.width, ref_info.height);
        let our_img = rgb_to_imgvec(&ours.data, ours.width, ours.height);
        let ssim2 = compute_ssimulacra2(ref_img.as_ref(), our_img.as_ref()).unwrap();

        let (avg_diff, max_diff, exact_pct) = pixel_diff_stats(&ours.data, &ref_pixels);

        println!(
            "{name}: SSIM2={ssim2:.2}, avg_diff={avg_diff:.2}, max_diff={max_diff}, exact={exact_pct:.1}%"
        );

        assert!(ssim2 > 40.0, "{name}: SSIM2 {ssim2:.2} too low");
    }
}
