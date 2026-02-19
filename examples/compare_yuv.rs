/// Compare our decoded YUV against dec265's raw YUV output
/// Usage: cargo run --release --example compare_yuv <input.heic> <ref.yuv>

fn main() {
    let input = std::env::args().nth(1).expect("input HEIC file path");
    let ref_path = std::env::args().nth(2).expect("reference YUV file path");

    let data = std::fs::read(&input).expect("read input");
    let decoder = heic_decoder::HeicDecoder::new();
    let frame = decoder.decode_to_frame(&data).expect("decode HEIC");

    let w = frame.width;
    let h = frame.height;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    eprintln!("Decoded: {}x{} (chroma {}x{})", w, h, cw, ch);

    let ref_data = std::fs::read(&ref_path).expect("read reference");
    let expected_size = (w * h + cw * ch * 2) as usize;
    eprintln!("Reference YUV: {} bytes (expected {})", ref_data.len(), expected_size);
    assert_eq!(ref_data.len(), expected_size, "YUV size mismatch");

    let ref_y = &ref_data[..(w * h) as usize];
    let ref_cb = &ref_data[(w * h) as usize..(w * h + cw * ch) as usize];
    let ref_cr = &ref_data[(w * h + cw * ch) as usize..];

    // Compare Y plane
    let (y_psnr, y_first_diff) = compare_plane(&frame.y_plane, ref_y, w, h, "Y");
    let (cb_psnr, cb_first_diff) = compare_plane(&frame.cb_plane, ref_cb, cw, ch, "Cb");
    let (cr_psnr, cr_first_diff) = compare_plane(&frame.cr_plane, ref_cr, cw, ch, "Cr");

    eprintln!("\nPSNR: Y={:.2} dB, Cb={:.2} dB, Cr={:.2} dB", y_psnr, cb_psnr, cr_psnr);

    if let Some((x, y, ours, ref_val)) = y_first_diff {
        eprintln!("First Y diff at ({}, {}): ours={} ref={}", x, y, ours, ref_val);
    }
    if let Some((x, y, ours, ref_val)) = cb_first_diff {
        eprintln!("First Cb diff at ({}, {}): ours={} ref={}", x, y, ours, ref_val);
    }
    if let Some((x, y, ours, ref_val)) = cr_first_diff {
        eprintln!("First Cr diff at ({}, {}): ours={} ref={}", x, y, ours, ref_val);
    }

    // Dump our Y plane for visual comparison
    let mut our_yuv = Vec::with_capacity(expected_size);
    for &v in &frame.y_plane {
        our_yuv.push(v.min(255) as u8);
    }
    for &v in &frame.cb_plane {
        our_yuv.push(v.min(255) as u8);
    }
    for &v in &frame.cr_plane {
        our_yuv.push(v.min(255) as u8);
    }
    std::fs::write("/tmp/ours_sample1.yuv", &our_yuv).expect("write our YUV");
    eprintln!("Written our YUV to /tmp/ours_sample1.yuv");
}

fn compare_plane(
    ours: &[u16],
    reference: &[u8],
    w: u32,
    h: u32,
    name: &str,
) -> (f64, Option<(u32, u32, u16, u8)>) {
    let size = (w * h) as usize;
    assert_eq!(ours.len(), size, "{} plane size mismatch: {} vs {}", name, ours.len(), size);

    let mut mse_sum = 0u64;
    let mut diff_count = 0u32;
    let mut max_diff = 0u32;
    let mut first_diff = None;

    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            let our_val = ours[idx];
            let ref_val = reference[idx];
            let d = (our_val as i32 - ref_val as i32).unsigned_abs();
            if d > 0 {
                diff_count += 1;
                if first_diff.is_none() {
                    first_diff = Some((x, y, our_val, ref_val));
                }
            }
            max_diff = max_diff.max(d);
            mse_sum += (d * d) as u64;
        }
    }

    let mse = mse_sum as f64 / size as f64;
    let psnr = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    };

    eprintln!(
        "{}: PSNR={:.2} dB, diff_pixels={}/{}, max_diff={}",
        name, psnr, diff_count, size, max_diff
    );

    (psnr, first_diff)
}
