//! Integration tests for HEIC decoding

use heic_decoder::{HeicDecoder, heif};

const EXAMPLE_HEIC: &str = "/home/lilith/work/heic/libheif/examples/example.heic";

#[test]
fn test_get_info() {
    let data = std::fs::read(EXAMPLE_HEIC).expect("Failed to read test file");

    // Debug: print container info
    let container = heif::parse(&data).expect("Failed to parse container");
    println!("Primary item ID: {}", container.primary_item_id);
    println!("Item infos: {} items", container.item_infos.len());
    for info in &container.item_infos {
        println!("  Item {}: type={:?}, name={:?}", info.item_id, info.item_type, info.item_name);
    }
    println!("Property associations: {} entries", container.property_associations.len());
    for assoc in &container.property_associations {
        println!("  Item {}: {:?}", assoc.item_id, assoc.properties);
    }
    println!("Image extents: {} entries", container.image_extents.len());
    for (i, ext) in container.image_extents.iter().enumerate() {
        println!("  Property {}: {}x{}", i, ext.width, ext.height);
    }
    println!("HEVC configs: {} entries", container.hevc_configs.len());

    if let Some(item) = container.primary_item() {
        println!("Primary item: {:?}", item.item_type);
        println!("  ID: {}", item.id);
        println!("  Name: {:?}", item.name);
        println!("  Dimensions from ispe: {:?}", item.dimensions);
        if let Some(ref config) = item.hevc_config {
            println!("  HEVC config: {} NAL units, length_size={}",
                     config.nal_units.len(), config.length_size_minus_one + 1);
        } else {
            println!("  No HEVC config");
        }
    }

    let decoder = HeicDecoder::new();
    let info = decoder.get_info(&data).expect("Failed to get info");
    println!("Decoded info: {}x{}", info.width, info.height);

    // example.heic is 1280x856 (SPS reports 856 due to 8-pixel rounding)
    // The ispe reports 1280x854 which is the actual cropped size
    assert_eq!(info.width, 1280, "Expected width 1280");
    // Height from SPS is 856 (rounded to 8), but ispe says 854
    assert!((854..=856).contains(&info.height), "Expected height ~854-856");
}

#[test]
#[ignore] // Ignore until coefficient decoding is fully implemented
fn test_decode() {
    let data = std::fs::read(EXAMPLE_HEIC).expect("Failed to read test file");
    let decoder = HeicDecoder::new();

    let image = decoder.decode(&data).expect("Failed to decode");

    // example.heic is 1280x856 (SPS has 856 due to 8-pixel rounding, ispe says 854)
    assert_eq!(image.width, 1280, "Expected width 1280");
    assert!((854..=856).contains(&image.height), "Expected height ~854-856");

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
    file.write_all(ppm.as_bytes()).expect("Failed to write PPM header");
    file.write_all(&image.data).expect("Failed to write PPM data");
    println!("Wrote decoded image to: {}", ppm_path);
}

#[test]
#[ignore]
fn test_raw_yuv_values() {
    let data = std::fs::read(EXAMPLE_HEIC).expect("Failed to read test file");
    let decoder = HeicDecoder::new();

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

    println!("Frame: {}x{}, bit_depth={}", frame.width, frame.height, frame.bit_depth);
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
        println!("    {:3}-{:3}: {:7} ({:5.1}%)", i*32, (i+1)*32-1, count, pct);
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
}
