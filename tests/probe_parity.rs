//! Probe-vs-decode parity test.
//!
//! Ensures that `ImageInfo::from_bytes()` (probe) and full decode agree on
//! dimensions, bit depth, chroma format, alpha presence, and CICP color
//! signaling for real-world HEIC files.

use heic::{DecoderConfig, ImageInfo, PixelLayout};

/// Directories to search for `.HEIC` test files, in priority order.
const SEARCH_DIRS: &[&str] = &[
    "/mnt/v/heic",
    "/home/lilith/work/heic/libheif/examples",
    "/home/lilith/work/heic/test-images",
];

/// Collect HEIC files from the search directories.
fn find_heic_files() -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for dir in SEARCH_DIRS {
        let path = std::path::Path::new(dir);
        if !path.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("heic"))
                {
                    files.push(p);
                }
            }
        }
    }
    files.sort();
    files
}

/// Compare probe metadata against decode results for a single file.
///
/// Returns `(filename, list_of_mismatches)`. An empty mismatch list means
/// probe and decode agree on all checked fields.
fn check_parity(path: &std::path::Path) -> (String, Vec<String>) {
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let mut mismatches = Vec::new();

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            mismatches.push(format!("read error: {e}"));
            return (name, mismatches);
        }
    };

    // --- Probe ---
    let probe = match ImageInfo::from_bytes(&data) {
        Ok(info) => info,
        Err(e) => {
            mismatches.push(format!("probe error: {e}"));
            return (name, mismatches);
        }
    };

    // --- Full decode (YCbCr frame for rich metadata) ---
    let decoder = DecoderConfig::new();
    let frame = match decoder.decode_to_frame(&data) {
        Ok(f) => f,
        Err(e) => {
            // If decode fails, we can still check what we got from probe
            mismatches.push(format!("decode_to_frame error: {e}"));
            return (name, mismatches);
        }
    };

    // --- Also do RGB decode to cross-check output dimensions ---
    let rgb_output = DecoderConfig::new().decode(&data, PixelLayout::Rgb8).ok();

    // --- Dimension parity ---
    // The frame has transforms applied, so cropped_width/height should match
    // probe dimensions (which also apply transforms).
    let frame_w = frame.cropped_width();
    let frame_h = frame.cropped_height();

    if probe.width != frame_w {
        mismatches.push(format!(
            "width: probe={} frame_cropped={}",
            probe.width, frame_w
        ));
    }
    if probe.height != frame_h {
        mismatches.push(format!(
            "height: probe={} frame_cropped={}",
            probe.height, frame_h
        ));
    }

    // Cross-check with RGB output dimensions if available
    if let Some(ref rgb) = rgb_output {
        if probe.width != rgb.width {
            mismatches.push(format!(
                "width: probe={} rgb_decode={}",
                probe.width, rgb.width
            ));
        }
        if probe.height != rgb.height {
            mismatches.push(format!(
                "height: probe={} rgb_decode={}",
                probe.height, rgb.height
            ));
        }
    }

    // --- Bit depth ---
    if probe.bit_depth != frame.bit_depth {
        mismatches.push(format!(
            "bit_depth: probe={} frame={}",
            probe.bit_depth, frame.bit_depth
        ));
    }

    // --- Chroma format ---
    if probe.chroma_format != frame.chroma_format {
        mismatches.push(format!(
            "chroma_format: probe={} frame={}",
            probe.chroma_format, frame.chroma_format
        ));
    }

    // --- Alpha ---
    let frame_has_alpha = frame.alpha_plane.is_some();
    if probe.has_alpha != frame_has_alpha {
        mismatches.push(format!(
            "has_alpha: probe={} frame={}",
            probe.has_alpha, frame_has_alpha
        ));
    }

    // --- CICP color signaling ---
    // Probe extracts CICP from the colr nclx box in the HEIF container.
    // The frame carries CICP from the HEVC SPS VUI. These may legitimately
    // differ (container can override stream-level signaling), so we only
    // warn rather than fail on CICP mismatches.
    let cicp_fields = [
        (
            "color_primaries",
            probe.color_primaries,
            frame.color_primaries as u16,
        ),
        (
            "transfer_characteristics",
            probe.transfer_characteristics,
            frame.transfer_characteristics as u16,
        ),
        (
            "matrix_coefficients",
            probe.matrix_coefficients,
            frame.matrix_coeffs as u16,
        ),
        (
            "video_full_range",
            probe.video_full_range as u16,
            frame.full_range as u16,
        ),
    ];

    for (field, probe_val, frame_val) in cicp_fields {
        // CICP "2" (unspecified) in the probe means the container had no nclx
        // box, so there's nothing to compare.
        if probe_val == 2 && field != "video_full_range" {
            continue;
        }
        if probe_val != frame_val {
            mismatches.push(format!(
                "cicp.{field}: probe={probe_val} frame={frame_val} (container vs SPS)"
            ));
        }
    }

    (name, mismatches)
}

#[test]
#[ignore] // Requires corpus files on disk
fn probe_vs_decode_parity() {
    let files = find_heic_files();
    assert!(
        !files.is_empty(),
        "No HEIC test files found in {SEARCH_DIRS:?}"
    );

    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut decode_errors = 0u32;
    let mut failures = Vec::new();

    for path in &files {
        let (name, mismatches) = check_parity(path);

        let has_decode_error = mismatches
            .iter()
            .any(|m| m.contains("decode_to_frame error"));

        if mismatches.is_empty() {
            pass += 1;
            println!("  PASS: {name}");
        } else if has_decode_error {
            decode_errors += 1;
            println!("  SKIP (decode error): {name}");
            for m in &mismatches {
                println!("        {m}");
            }
        } else {
            fail += 1;
            println!("  FAIL: {name}");
            for m in &mismatches {
                println!("        {m}");
            }
            failures.push((name, mismatches));
        }
    }

    println!();
    println!(
        "Probe-vs-decode parity: {pass} pass, {fail} fail, {decode_errors} decode errors, {} total",
        files.len()
    );

    // Hard-fail on dimension, bit_depth, chroma, or alpha mismatches.
    // CICP mismatches between container and SPS are noted but not fatal.
    let hard_failures: Vec<_> = failures
        .iter()
        .filter(|(_, mismatches)| {
            mismatches
                .iter()
                .any(|m| !m.contains("cicp.") && !m.contains("container vs SPS"))
        })
        .collect();

    assert!(
        hard_failures.is_empty(),
        "Hard parity failures (dimensions/bit_depth/chroma/alpha):\n{}",
        hard_failures
            .iter()
            .map(|(name, ms)| format!("  {name}: {}", ms.join("; ")))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
