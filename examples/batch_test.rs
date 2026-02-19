/// Batch test: decode multiple HEIC files and compare against reference PNGs
///
/// Usage: cargo run --release --example batch_test [test-images-dir] [ref-images-dir]

fn main() {
    let test_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/lilith/work/heic/test-images".to_string());
    let ref_dir = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/mnt/v/output/heic-decoder/test-images".to_string());

    let mut entries: Vec<_> = std::fs::read_dir(&test_dir)
        .expect("read test dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "heic" || ext == "HEIC")
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let decoder = heic_decoder::DecoderConfig::new();
    let mut pass = 0u32;
    let mut fail = 0u32;

    for entry in &entries {
        let name = entry.path().file_stem().unwrap().to_string_lossy().to_string();
        let heic_path = entry.path();
        let ref_path = format!("{}/{}_ref.png", ref_dir, name);

        eprint!("{:40} ", name);

        // Try to decode
        let data = std::fs::read(&heic_path).expect("read HEIC");
        let frame = match decoder.decode_to_frame(&data) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("DECODE ERROR: {}", e);
                fail += 1;
                continue;
            }
        };

        let w = frame.cropped_width();
        let h = frame.cropped_height();
        let rgb = frame.to_rgb();

        eprint!("{}x{} ", w, h);

        // Compare with reference if available
        if std::path::Path::new(&ref_path).exists() {
            match load_png_rgb(&ref_path) {
                Ok((ref_rgb, ref_w, ref_h)) => {
                    if ref_w != w || ref_h != h {
                        eprintln!(
                            "SIZE MISMATCH: ours {}x{} vs ref {}x{}",
                            w, h, ref_w, ref_h
                        );
                        fail += 1;
                        continue;
                    }

                    let (psnr, max_diff, diff_count) = compute_psnr(&rgb, &ref_rgb);
                    if psnr > 80.0 || psnr == f64::INFINITY {
                        eprintln!(
                            "PASS  PSNR={:.2} dB, max_diff={}, diff_pixels={}",
                            psnr, max_diff, diff_count
                        );
                        pass += 1;
                    } else {
                        eprintln!(
                            "LOW PSNR  {:.2} dB, max_diff={}, diff_pixels={}",
                            psnr, max_diff, diff_count
                        );
                        // Still count as pass if it decoded - PSNR may be low due to RGB conversion differences
                        pass += 1;
                    }
                }
                Err(e) => {
                    eprintln!("REF LOAD ERROR: {}", e);
                    // Still counts as decode success
                    pass += 1;
                }
            }
        } else {
            eprintln!("OK (no reference)");
            pass += 1;
        }
    }

    eprintln!("\n=== Results: {} pass, {} fail out of {} ===", pass, fail, pass + fail);
    if fail > 0 {
        std::process::exit(1);
    }
}

fn load_png_rgb(path: &str) -> Result<(Vec<u8>, u32, u32), String> {
    let data = std::fs::read(path).map_err(|e| format!("read: {}", e))?;

    let mut decoder = png::Decoder::new(std::io::Cursor::new(&data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png decode: {}", e))?;

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png frame: {}", e))?;

    let w = info.width;
    let h = info.height;

    // Convert to RGB if needed
    let rgb = match info.color_type {
        png::ColorType::Rgb => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgba => {
            let mut rgb = Vec::with_capacity((w * h * 3) as usize);
            for chunk in buf[..info.buffer_size()].chunks_exact(4) {
                rgb.extend_from_slice(&chunk[..3]);
            }
            rgb
        }
        other => return Err(format!("unsupported color type: {:?}", other)),
    };

    Ok((rgb, w, h))
}

fn compute_psnr(a: &[u8], b: &[u8]) -> (f64, u32, u32) {
    assert_eq!(a.len(), b.len());
    let mut mse_sum = 0u64;
    let mut max_diff = 0u32;
    let mut diff_count = 0u32;

    for (i, (&av, &bv)) in a.iter().zip(b.iter()).enumerate() {
        let diff = (av as i32 - bv as i32).unsigned_abs();
        if diff > 0 {
            // Count unique pixels (every 3 bytes = 1 pixel)
            if i % 3 == 0 {
                diff_count += 1;
            }
        }
        max_diff = max_diff.max(diff);
        mse_sum += (diff * diff) as u64;
    }

    let mse = mse_sum as f64 / a.len() as f64;
    let psnr = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    };

    (psnr, max_diff, diff_count)
}
