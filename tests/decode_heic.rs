//! Integration tests for HEIC decoding

use heic_decoder::DecoderConfig;

fn heic_base_dir() -> String {
    std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into())
}

fn example_heic() -> String {
    format!("{}/libheif/examples/example.heic", heic_base_dir())
}

fn iphone_heic() -> String {
    format!(
        "{}/test-images/classic-car-iphone12pro.heic",
        heic_base_dir()
    )
}

#[test]
fn test_get_info() {
    let data = std::fs::read(example_heic()).expect("Failed to read test file");

    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("Failed to get info");
    println!("Decoded info: {}x{}", info.width, info.height);

    // example.heic is 1280x854 (cropped from 1280x856 via conformance window)
    assert_eq!(info.width, 1280, "Expected width 1280");
    assert_eq!(info.height, 854, "Expected height 854 (cropped)");
}

#[test]
#[ignore] // Ignore until coefficient decoding is fully implemented
fn test_decode() {
    let data = std::fs::read(example_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let image = decoder
        .decode(&data, heic_decoder::PixelLayout::Rgb8)
        .expect("Failed to decode");

    // example.heic is 1280x854 (cropped from 1280x856 via conformance window)
    assert_eq!(image.width, 1280, "Expected width 1280");
    assert_eq!(image.height, 854, "Expected height 854 (cropped)");

    // Check that we got RGB data (3 bytes per pixel)
    let expected_size = (image.width * image.height * 3) as usize;
    assert_eq!(image.data.len(), expected_size, "Unexpected data size");

    // Basic sanity check - data shouldn't be all zeros
    let non_zero = image.data.iter().any(|&b| b != 0);
    assert!(non_zero, "Image data is all zeros");

    // Print some stats
    let min_val = *image.data.iter().min().unwrap();
    let max_val = *image.data.iter().max().unwrap();
    let sum: u64 = image.data.iter().map(|&b| b as u64).sum();
    let avg = sum / image.data.len() as u64;
    println!("Pixel stats: min={}, max={}, avg={}", min_val, max_val, avg);

    // Print first 8x8 RGB block for comparison with reference
    println!("\n=== Our first 8x8 RGB block ===");
    for y in 0..8 {
        for x in 0..8 {
            let idx = (y * image.width as usize + x) * 3;
            let r = image.data[idx];
            let g = image.data[idx + 1];
            let b = image.data[idx + 2];
            print!("({:3},{:3},{:3}) ", r, g, b);
        }
        println!();
    }

    // Write to PPM for visual inspection
    let ppm_path = "/tmp/decoded_heic.ppm";
    let mut ppm = String::new();
    ppm.push_str(&format!("P6\n{} {}\n255\n", image.width, image.height));
    let mut file = std::fs::File::create(ppm_path).expect("Failed to create PPM");
    use std::io::Write;
    file.write_all(ppm.as_bytes())
        .expect("Failed to write PPM header");
    file.write_all(&image.data)
        .expect("Failed to write PPM data");
    println!("Wrote decoded image to: {}", ppm_path);
}

#[test]
#[ignore]
fn test_raw_yuv_values() {
    let data = std::fs::read(example_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    // Decode and examine raw YCbCr
    let frame = decoder.decode_to_frame(&data).expect("Failed to decode");

    // Analyze Y values in quadrants
    let mid_x = frame.width / 2;
    let mid_y = frame.height / 2;
    let mut quadrant_sums = [0u64; 4];
    let mut quadrant_counts = [0u64; 4];
    for y in 0..frame.height {
        for x in 0..frame.width {
            let idx = (y * frame.width + x) as usize;
            let val = frame.y_plane[idx] as u64;
            let q = if x < mid_x {
                if y < mid_y { 0 } else { 2 }
            } else if y < mid_y {
                1
            } else {
                3
            };
            quadrant_sums[q] += val;
            quadrant_counts[q] += 1;
        }
    }
    println!("\nY quadrant averages:");
    println!("  Top-Left: {}", quadrant_sums[0] / quadrant_counts[0]);
    println!("  Top-Right: {}", quadrant_sums[1] / quadrant_counts[1]);
    println!("  Bottom-Left: {}", quadrant_sums[2] / quadrant_counts[2]);
    println!("  Bottom-Right: {}", quadrant_sums[3] / quadrant_counts[3]);

    // Sample Y values along x=64 (CTU boundary) for different y
    println!("\nY values at x=64 for different rows:");
    for &y in &[0, 32, 64, 128, 256, 400] {
        if y < frame.height {
            let idx = (y * frame.width + 64) as usize;
            let vals: Vec<u16> = (0..8).map(|dx| frame.y_plane[idx + dx]).collect();
            println!("  y={:3}: {:?}", y, vals);
        }
    }

    // Sample Y values along y=64 for different x
    println!("\nY values at y=64 for different columns:");
    for &x in &[0, 64, 96, 120, 127, 128, 192, 256, 400, 640] {
        if x < frame.width {
            let idx = (64 * frame.width + x) as usize;
            let vals: Vec<u16> = (0..8).map(|dx| frame.y_plane[idx + dx]).collect();
            println!("  x={:3}: {:?}", x, vals);
        }
    }

    // Check the problematic row y=63 at different x
    println!("\nY values at y=63 (top border row for CTU row 1):");
    for &x in &[96, 104, 112, 120, 127] {
        if x < frame.width {
            let idx = (63 * frame.width + x) as usize;
            let vals: Vec<u16> = (0..8).map(|dx| frame.y_plane[idx + dx]).collect();
            println!("  x={:3}: {:?}", x, vals);
        }
    }

    println!(
        "Frame: {}x{}, bit_depth={}",
        frame.width, frame.height, frame.bit_depth
    );
    println!("Y plane: {} samples", frame.y_plane.len());
    println!("Cb plane: {} samples", frame.cb_plane.len());
    println!("Cr plane: {} samples", frame.cr_plane.len());

    // Y plane statistics with detailed histogram
    let y_min = frame.y_plane.iter().min().unwrap_or(&0);
    let y_max = frame.y_plane.iter().max().unwrap_or(&0);
    let y_sum: u64 = frame.y_plane.iter().map(|&v| v as u64).sum();
    let y_avg = y_sum / frame.y_plane.len().max(1) as u64;

    // Histogram in 32-value bins
    let mut hist = [0usize; 8];
    for &v in &frame.y_plane {
        hist[(v as usize / 32).min(7)] += 1;
    }
    println!("\nY plane: min={}, max={}, avg={}", y_min, y_max, y_avg);
    println!("  Histogram (32-bin):");
    for (i, count) in hist.iter().enumerate() {
        let pct = *count as f64 / frame.y_plane.len() as f64 * 100.0;
        println!(
            "    {:3}-{:3}: {:7} ({:5.1}%)",
            i * 32,
            (i + 1) * 32 - 1,
            count,
            pct
        );
    }

    // Cb plane statistics
    let cb_min = frame.cb_plane.iter().min().unwrap_or(&0);
    let cb_max = frame.cb_plane.iter().max().unwrap_or(&0);
    let cb_sum: u64 = frame.cb_plane.iter().map(|&v| v as u64).sum();
    let cb_avg = cb_sum / frame.cb_plane.len().max(1) as u64;
    println!("Cb plane: min={}, max={}, avg={}", cb_min, cb_max, cb_avg);

    // Cr plane statistics
    let cr_min = frame.cr_plane.iter().min().unwrap_or(&0);
    let cr_max = frame.cr_plane.iter().max().unwrap_or(&0);
    let cr_sum: u64 = frame.cr_plane.iter().map(|&v| v as u64).sum();
    let cr_avg = cr_sum / frame.cr_plane.len().max(1) as u64;
    println!("Cr plane: min={}, max={}, avg={}", cr_min, cr_max, cr_avg);

    println!("\n=== Raw YCbCr Values (first 8x8 Y block) ===");
    for y in 0..8 {
        let mut row = Vec::new();
        for x in 0..8 {
            let idx = (y * frame.width + x) as usize;
            row.push(format!("{:3}", frame.y_plane[idx]));
        }
        println!("  Y: {}", row.join(" "));
    }

    println!("\n=== Raw Cb/Cr (first 4x4 chroma block) ===");
    let c_stride = frame.width.div_ceil(2) as usize;
    for cy in 0..4 {
        let mut cb_row = Vec::new();
        let mut cr_row = Vec::new();
        for cx in 0..4 {
            let idx = cy * c_stride + cx;
            cb_row.push(format!("{:3}", frame.cb_plane[idx]));
            cr_row.push(format!("{:3}", frame.cr_plane[idx]));
        }
        println!("  Cb: {}  |  Cr: {}", cb_row.join(" "), cr_row.join(" "));
    }

    // Analyze chroma bias by CTU position
    // For 4:2:0, each CTU (64x64 luma) has 32x32 chroma
    println!("\n=== Chroma averages by CTU row ===");
    let c_height = frame.height.div_ceil(2) as usize;
    let ctu_chroma_size = 32usize;
    let num_ctu_rows = c_height.div_ceil(ctu_chroma_size);

    for ctu_row in 0..num_ctu_rows {
        let start_y = ctu_row * ctu_chroma_size;
        let end_y = ((ctu_row + 1) * ctu_chroma_size).min(c_height);

        let mut cb_sum = 0u64;
        let mut cr_sum = 0u64;
        let mut count = 0u64;

        for cy in start_y..end_y {
            for cx in 0..c_stride {
                let idx = cy * c_stride + cx;
                cb_sum += frame.cb_plane[idx] as u64;
                cr_sum += frame.cr_plane[idx] as u64;
                count += 1;
            }
        }

        if count > 0 {
            println!(
                "  CTU row {:2}: Cb avg={:3}, Cr avg={:3}",
                ctu_row,
                cb_sum / count,
                cr_sum / count
            );
        }
    }

    // Analyze chroma by CTU column for first row
    println!("\n=== Chroma averages by CTU column (first row) ===");
    let c_width = c_stride;
    let num_ctu_cols = c_width.div_ceil(ctu_chroma_size);

    for ctu_col in 0..num_ctu_cols {
        let start_x = ctu_col * ctu_chroma_size;
        let end_x = ((ctu_col + 1) * ctu_chroma_size).min(c_width);

        let mut cb_sum = 0u64;
        let mut cr_sum = 0u64;
        let mut count = 0u64;

        for cy in 0..ctu_chroma_size.min(c_height) {
            for cx in start_x..end_x {
                let idx = cy * c_stride + cx;
                cb_sum += frame.cb_plane[idx] as u64;
                cr_sum += frame.cr_plane[idx] as u64;
                count += 1;
            }
        }

        if count > 0 {
            println!(
                "  CTU col {:2}: Cb avg={:3}, Cr avg={:3}",
                ctu_col,
                cb_sum / count,
                cr_sum / count
            );
        }
    }

    // Analyze the boundary between CTU col 0 and 1
    println!("\n=== Chroma at CTU boundary (col 0 -> 1) ===");
    println!("Chroma values at x=28..35 (boundary at x=32), y=0..3:");
    for cy in 0..4 {
        let mut cb_row = Vec::new();
        let mut cr_row = Vec::new();
        for cx in 28..36 {
            let idx = cy * c_stride + cx;
            cb_row.push(format!("{:3}", frame.cb_plane[idx]));
            cr_row.push(format!("{:3}", frame.cr_plane[idx]));
        }
        println!(
            "  y={}: Cb=[{}]  Cr=[{}]",
            cy,
            cb_row.join(", "),
            cr_row.join(", ")
        );
    }
    println!("  (x=32 is start of CTU col 1)");

    // Also check what's at the very end of CTU col 0 (x=31)
    println!("\nChroma at right edge of CTU col 0 (x=31), all y:");
    let mut cb_at_31 = vec![];
    let mut cr_at_31 = vec![];
    for cy in 0..32.min(c_height) {
        let idx = cy * c_stride + 31;
        cb_at_31.push(frame.cb_plane[idx]);
        cr_at_31.push(frame.cr_plane[idx]);
    }
    let cb_avg: u64 = cb_at_31.iter().map(|&v| v as u64).sum::<u64>() / cb_at_31.len() as u64;
    let cr_avg: u64 = cr_at_31.iter().map(|&v| v as u64).sum::<u64>() / cr_at_31.len() as u64;
    println!("  x=31: Cb avg={}, Cr avg={}", cb_avg, cr_avg);
    println!("  first 8 Cb: {:?}", &cb_at_31[..8.min(cb_at_31.len())]);
    println!("  first 8 Cr: {:?}", &cr_at_31[..8.min(cr_at_31.len())]);
}

#[test]
fn test_extract_exif() {
    let data = std::fs::read(iphone_heic()).expect("read");
    let decoder = DecoderConfig::new();

    let exif = decoder.extract_exif(&data).expect("extract_exif");
    let exif = exif.expect("should have EXIF data");

    // EXIF TIFF data starts with byte-order mark: II (little-endian) or MM (big-endian)
    assert!(exif.len() > 8, "EXIF data too short: {} bytes", exif.len());
    assert!(
        &exif[..2] == b"II" || &exif[..2] == b"MM",
        "EXIF data should start with TIFF byte order mark, got {:02x?}",
        &exif[..2]
    );
    println!(
        "EXIF: {} bytes, byte order: {}",
        exif.len(),
        if exif[0] == b'I' {
            "little-endian"
        } else {
            "big-endian"
        }
    );
}

#[test]
fn test_extract_exif_none() {
    // example.heic has no EXIF
    let data = std::fs::read(example_heic()).expect("read");
    let decoder = DecoderConfig::new();
    let exif = decoder.extract_exif(&data).expect("extract_exif");
    assert!(exif.is_none(), "example.heic should not have EXIF");
}

#[test]
fn test_image_info_no_exif() {
    // example.heic: no EXIF, non-grid — probe should work
    let data = std::fs::read(example_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");
    assert!(!info.has_exif, "example.heic should not have EXIF");
    assert!(!info.has_xmp, "example.heic should not have XMP");
    println!(
        "ImageInfo: {}x{}, has_exif={}, has_xmp={}",
        info.width, info.height, info.has_exif, info.has_xmp
    );
}

#[test]
fn test_image_info_grid_with_exif() {
    // iPhone HEIC: grid image with EXIF + XMP
    let data = std::fs::read(iphone_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe grid image");
    assert!(info.has_exif, "iPhone HEIC should have EXIF");
    assert!(info.has_xmp, "iPhone HEIC should have XMP");
    // Post-transform dimensions: iPhone photo is 4032x3024 raw but has irot 90°
    // rotation, so final decoded output is 3024x4032 (portrait)
    assert_eq!(info.width, 3024);
    assert_eq!(info.height, 4032);
    println!(
        "Grid ImageInfo: {}x{}, bit_depth={}, has_exif={}, has_xmp={}",
        info.width, info.height, info.bit_depth, info.has_exif, info.has_xmp
    );
}

#[test]
fn test_extract_xmp() {
    let data = std::fs::read(iphone_heic()).expect("read");
    let decoder = DecoderConfig::new();
    let xmp = decoder.extract_xmp(&data).expect("extract_xmp");
    // XMP may or may not be present; just ensure no crash
    if let Some(xmp_data) = xmp {
        // XMP should start with XML-like content
        let start =
            std::str::from_utf8(&xmp_data[..xmp_data.len().min(100)]).unwrap_or("(non-utf8)");
        println!("XMP: {} bytes, starts with: {:?}", xmp_data.len(), start);
    } else {
        println!("No XMP found (expected for some files)");
    }
}

#[test]
fn test_decode_thumbnail() {
    let data = std::fs::read(example_heic()).expect("read");
    let decoder = DecoderConfig::new();
    let thumb = decoder
        .decode_thumbnail(&data, heic_decoder::PixelLayout::Rgb8)
        .expect("decode_thumbnail");
    let thumb = thumb.expect("example.heic should have a thumbnail");
    // Thumbnail should be 320x212 per the container dump
    assert_eq!(thumb.width, 320);
    assert_eq!(thumb.height, 212);
    assert_eq!(thumb.data.len(), 320 * 212 * 3);
    println!(
        "Thumbnail: {}x{}, {} bytes",
        thumb.width,
        thumb.height,
        thumb.data.len()
    );
}

#[test]
fn test_image_info_has_thumbnail() {
    let data = std::fs::read(example_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");
    assert!(
        info.has_thumbnail,
        "example.heic should report has_thumbnail=true"
    );
}

#[test]
fn test_decode_thumbnail_none() {
    // Nokia test files typically don't have thumbnails
    let nokia_path = format!("{}/test-images/nokia/C001.heic", heic_base_dir());
    if let Ok(data) = std::fs::read(nokia_path) {
        let decoder = DecoderConfig::new();
        let thumb = decoder
            .decode_thumbnail(&data, heic_decoder::PixelLayout::Rgb8)
            .expect("decode_thumbnail");
        if thumb.is_none() {
            println!("C001.heic has no thumbnail (expected)");
        } else {
            println!("C001.heic has a thumbnail (unexpected but OK)");
        }
    }
}

#[test]
fn test_image_info_matches_decoded_dimensions() {
    // Regression: ImageInfo returned raw (pre-transform) dimensions while decoder
    // applied irot/imir/clap transforms, causing dimension mismatch panics.
    // iPhone photos have irot 90°, making raw 4032x3024 → decoded 3024x4032.
    let data = std::fs::read(iphone_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");

    let decoder = DecoderConfig::new();
    let decoded = decoder
        .decode(&data, heic_decoder::PixelLayout::Rgb8)
        .expect("decode");

    assert_eq!(
        info.width, decoded.width,
        "ImageInfo width {} != decoded width {}",
        info.width, decoded.width
    );
    assert_eq!(
        info.height, decoded.height,
        "ImageInfo height {} != decoded height {}",
        info.height, decoded.height
    );
}

fn portrait_matte_heic() -> String {
    format!(
        "{}/test-images/openize-heic-net/Openize.Heic.Tests/TestsData/samples/iphone_portrait_photo.heic",
        heic_base_dir()
    )
}

fn mattes_heic() -> String {
    format!(
        "{}/test-images/openize-heic-net/Openize.Heic.Tests/TestsData/samples/iphone_iOS18_tmap+MatteMaps.heic",
        heic_base_dir()
    )
}

// ---------------------------------------------------------------------------
// MatteType URN parsing
// ---------------------------------------------------------------------------

#[test]
fn test_matte_type_from_urn() {
    use heic_decoder::MatteType;

    assert_eq!(
        MatteType::from_urn("urn:com:apple:photo:2018:aux:portraiteffectsmatte"),
        Some(MatteType::Portrait)
    );
    assert_eq!(
        MatteType::from_urn("urn:com:apple:photo:2019:aux:semanticskinmatte"),
        Some(MatteType::Skin)
    );
    assert_eq!(
        MatteType::from_urn("urn:com:apple:photo:2019:aux:semantichairmatte"),
        Some(MatteType::Hair)
    );
    assert_eq!(
        MatteType::from_urn("urn:com:apple:photo:2019:aux:semanticteethmatte"),
        Some(MatteType::Teeth)
    );
    assert_eq!(
        MatteType::from_urn("urn:com:apple:photo:2020:aux:semanticglassesmatte"),
        Some(MatteType::Glasses)
    );
    assert_eq!(
        MatteType::from_urn("urn:com:apple:photo:2020:aux:semanticskymatte"),
        Some(MatteType::Sky)
    );
    assert_eq!(MatteType::from_urn("urn:unknown:type"), None);
    assert_eq!(MatteType::from_urn(""), None);
}

#[test]
fn test_matte_type_urn_roundtrip() {
    use heic_decoder::MatteType;
    for &mt in MatteType::ALL {
        assert_eq!(MatteType::from_urn(mt.urn()), Some(mt));
    }
}

#[test]
fn test_matte_type_display() {
    use heic_decoder::MatteType;
    assert_eq!(format!("{}", MatteType::Portrait), "portrait");
    assert_eq!(format!("{}", MatteType::Skin), "skin");
    assert_eq!(format!("{}", MatteType::Sky), "sky");
}

// ---------------------------------------------------------------------------
// Matte extraction
// ---------------------------------------------------------------------------

#[test]
fn test_decode_matte_none_in_landscape() {
    // example.heic: no portrait mattes
    let data = std::fs::read(example_heic()).expect("read");
    let decoder = DecoderConfig::new();
    let matte = decoder
        .decode_matte(&data, heic_decoder::MatteType::Portrait)
        .expect("decode_matte");
    assert!(
        matte.is_none(),
        "example.heic should not have portrait matte"
    );
}

#[test]
fn test_decode_mattes_empty_for_landscape() {
    let data = std::fs::read(example_heic()).expect("read");
    let decoder = DecoderConfig::new();
    let mattes = decoder.decode_mattes(&data).expect("decode_mattes");
    assert!(
        mattes.is_empty(),
        "example.heic should have no mattes, got {}",
        mattes.len()
    );
}

#[test]
#[ignore] // Requires iPhone portrait photo with mattes; needs full HEVC decode
fn test_decode_mattes_multiple() {
    // iphone_iOS18_tmap+MatteMaps.heic has skin, sky, and portrait mattes
    let data = std::fs::read(mattes_heic()).expect("read mattes HEIC");
    let decoder = DecoderConfig::new();
    let mattes = decoder.decode_mattes(&data).expect("decode_mattes");

    println!("Found {} mattes:", mattes.len());
    for m in &mattes {
        println!(
            "  {} matte: {}x{}, {} bytes",
            m.matte_type,
            m.width,
            m.height,
            m.data.len()
        );
        // Validate pixel count
        assert_eq!(
            m.data.len(),
            (m.width * m.height) as usize,
            "{} matte pixel count mismatch",
            m.matte_type
        );
        // Check non-trivial data: not all zeros
        let nonzero = m.data.iter().any(|&v| v > 0);
        assert!(nonzero, "{} matte is all zeros", m.matte_type);
    }

    // Should have at least portrait and skin mattes
    let has_portrait = mattes
        .iter()
        .any(|m| m.matte_type == heic_decoder::MatteType::Portrait);
    let has_skin = mattes
        .iter()
        .any(|m| m.matte_type == heic_decoder::MatteType::Skin);
    assert!(has_portrait, "should have portrait matte");
    assert!(has_skin, "should have skin matte");
}

#[test]
#[ignore] // Requires iPhone portrait photo with mattes; needs full HEVC decode
fn test_decode_specific_matte() {
    let data = std::fs::read(mattes_heic()).expect("read");
    let decoder = DecoderConfig::new();

    let portrait = decoder
        .decode_matte(&data, heic_decoder::MatteType::Portrait)
        .expect("decode_matte")
        .expect("should have portrait matte");

    assert!(portrait.width > 0 && portrait.height > 0);
    assert_eq!(
        portrait.data.len(),
        (portrait.width * portrait.height) as usize
    );
    assert_eq!(portrait.matte_type, heic_decoder::MatteType::Portrait);
    println!(
        "Portrait matte: {}x{}, {} bytes",
        portrait.width,
        portrait.height,
        portrait.data.len()
    );

    // No glasses matte in this file
    let glasses = decoder
        .decode_matte(&data, heic_decoder::MatteType::Glasses)
        .expect("decode_matte");
    assert!(glasses.is_none(), "should not have glasses matte");
}

// ---------------------------------------------------------------------------
// EXIF/XMP/ICC byte extraction via ImageInfo
// ---------------------------------------------------------------------------

#[test]
fn test_image_info_exif_bytes() {
    let data = std::fs::read(iphone_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");

    assert!(info.has_exif, "should report has_exif=true");
    let exif = info.exif.expect("should have EXIF bytes");
    assert!(exif.len() > 8, "EXIF data too short: {} bytes", exif.len());
    assert!(
        &exif[..2] == b"II" || &exif[..2] == b"MM",
        "EXIF should start with TIFF byte order mark, got {:02x?}",
        &exif[..2]
    );
    println!(
        "ImageInfo EXIF: {} bytes, byte order: {}",
        exif.len(),
        if exif[0] == b'I' {
            "little-endian"
        } else {
            "big-endian"
        }
    );
}

#[test]
fn test_image_info_exif_bytes_none() {
    // example.heic has no EXIF
    let data = std::fs::read(example_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");
    assert!(!info.has_exif);
    assert!(
        info.exif.is_none(),
        "example.heic should have no EXIF bytes"
    );
}

#[test]
fn test_image_info_xmp_bytes() {
    let data = std::fs::read(iphone_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");

    assert!(info.has_xmp, "should report has_xmp=true");
    if let Some(xmp) = &info.xmp {
        let start = std::str::from_utf8(&xmp[..xmp.len().min(100)]).unwrap_or("(non-utf8)");
        println!(
            "ImageInfo XMP: {} bytes, starts with: {:?}",
            xmp.len(),
            start
        );
    }
}

#[test]
fn test_image_info_icc_profile_bytes() {
    // iPhone HEIC may use nclx rather than ICC profile.
    let data = std::fs::read(iphone_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");

    if info.has_icc_profile {
        let icc = info
            .icc_profile
            .as_ref()
            .expect("has_icc_profile=true but icc_profile is None");
        assert!(icc.len() > 32, "ICC profile too short: {} bytes", icc.len());
        println!("ICC profile: {} bytes", icc.len());
    } else {
        assert!(
            info.icc_profile.is_none(),
            "has_icc_profile=false but icc_profile is Some"
        );
        println!("No ICC profile (uses nclx color parameters)");
    }
}

#[test]
fn test_image_info_exif_matches_extract_exif() {
    // The bytes from ImageInfo::exif should match DecoderConfig::extract_exif()
    let data = std::fs::read(iphone_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");
    let decoder = DecoderConfig::new();
    let extracted = decoder.extract_exif(&data).expect("extract_exif");

    match (&info.exif, extracted.as_deref()) {
        (Some(from_info), Some(from_extract)) => {
            assert_eq!(
                from_info.as_slice(),
                from_extract,
                "ImageInfo.exif and extract_exif() should return identical bytes"
            );
        }
        (None, None) => {} // both absent, fine
        _ => panic!(
            "ImageInfo.exif={} but extract_exif()={}",
            info.exif.is_some(),
            extracted.is_some()
        ),
    }
}

#[test]
fn test_image_info_icc_matches_extract_icc() {
    let data = std::fs::read(iphone_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");
    let decoder = DecoderConfig::new();
    let extracted = decoder.extract_icc(&data).expect("extract_icc");

    match (&info.icc_profile, &extracted) {
        (Some(from_info), Some(from_extract)) => {
            assert_eq!(
                from_info, from_extract,
                "ImageInfo.icc_profile and extract_icc() should return identical bytes"
            );
        }
        (None, None) => {}
        _ => panic!(
            "ImageInfo.icc_profile={} but extract_icc()={}",
            info.icc_profile.is_some(),
            extracted.is_some()
        ),
    }
}

#[test]
fn test_portrait_image_info_metadata() {
    let data = std::fs::read(portrait_matte_heic()).expect("read");
    let info = heic_decoder::ImageInfo::from_bytes(&data).expect("probe");

    println!(
        "Portrait photo: {}x{}, has_exif={}, has_xmp={}, has_icc={}",
        info.width, info.height, info.has_exif, info.has_xmp, info.has_icc_profile
    );
    println!(
        "  exif: {} bytes, xmp: {} bytes, icc: {} bytes",
        info.exif.as_ref().map_or(0, |v| v.len()),
        info.xmp.as_ref().map_or(0, |v| v.len()),
        info.icc_profile.as_ref().map_or(0, |v| v.len()),
    );

    // Portrait photo should have EXIF at minimum
    assert!(info.has_exif, "portrait photo should have EXIF");
    assert!(info.exif.is_some(), "portrait photo should have EXIF bytes");
}
