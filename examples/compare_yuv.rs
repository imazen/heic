/// Compare our decoded YUV against dec265's raw YUV output
/// Usage: cargo run --release --example compare_yuv <input.heic> <ref.yuv>
///
/// dec265 outputs the conformance-window-cropped frame, so we extract
/// the cropped region from our full-size planes for comparison.

fn main() {
    let input = std::env::args().nth(1).expect("input HEIC file path");
    let ref_path = std::env::args().nth(2).expect("reference YUV file path");

    let data = std::fs::read(&input).expect("read input");
    let decoder = heic_decoder::DecoderConfig::new();
    let frame = decoder.decode_to_frame(&data).expect("decode HEIC");

    // Use cropped dimensions (conformance window applied) to match dec265 output
    let w = frame.cropped_width();
    let h = frame.cropped_height();
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    eprintln!(
        "Decoded: {}x{} (cropped from {}x{}, chroma {}x{})",
        w, h, frame.width, frame.height, cw, ch
    );

    let ref_data = std::fs::read(&ref_path).expect("read reference");
    let expected_size = (w * h + cw * ch * 2) as usize;
    eprintln!(
        "Reference YUV: {} bytes (expected {})",
        ref_data.len(),
        expected_size
    );
    assert_eq!(ref_data.len(), expected_size, "YUV size mismatch");

    let ref_y = &ref_data[..(w * h) as usize];
    let ref_cb = &ref_data[(w * h) as usize..(w * h + cw * ch) as usize];
    let ref_cr = &ref_data[(w * h + cw * ch) as usize..];

    // Extract cropped Y plane
    let our_y = extract_cropped_plane(
        &frame.y_plane,
        frame.width,
        frame.crop_left,
        frame.crop_top,
        w,
        h,
    );
    // Extract cropped chroma planes (crop offsets halved for 4:2:0)
    let chroma_crop_left = frame.crop_left / 2;
    let chroma_crop_top = frame.crop_top / 2;
    let chroma_stride = frame.width.div_ceil(2);
    let our_cb = extract_cropped_plane(
        &frame.cb_plane,
        chroma_stride,
        chroma_crop_left,
        chroma_crop_top,
        cw,
        ch,
    );
    let our_cr = extract_cropped_plane(
        &frame.cr_plane,
        chroma_stride,
        chroma_crop_left,
        chroma_crop_top,
        cw,
        ch,
    );

    // Compare planes
    let (y_psnr, y_first_diff) = compare_plane(&our_y, ref_y, w, h, "Y");
    let (cb_psnr, cb_first_diff) = compare_plane(&our_cb, ref_cb, cw, ch, "Cb");
    let (cr_psnr, cr_first_diff) = compare_plane(&our_cr, ref_cr, cw, ch, "Cr");

    eprintln!(
        "\nPSNR: Y={:.2} dB, Cb={:.2} dB, Cr={:.2} dB",
        y_psnr, cb_psnr, cr_psnr
    );

    if let Some((x, y, ours, ref_val)) = y_first_diff {
        eprintln!("First Y diff at ({}, {}): ours={} ref={}", x, y, ours, ref_val);
    }
    if let Some((x, y, ours, ref_val)) = cb_first_diff {
        eprintln!(
            "First Cb diff at ({}, {}): ours={} ref={}",
            x, y, ours, ref_val
        );
    }
    if let Some((x, y, ours, ref_val)) = cr_first_diff {
        eprintln!(
            "First Cr diff at ({}, {}): ours={} ref={}",
            x, y, ours, ref_val
        );
    }
}

/// Extract a cropped region from a full-size plane
fn extract_cropped_plane(
    plane: &[u16],
    stride: u32,
    crop_left: u32,
    crop_top: u32,
    width: u32,
    height: u32,
) -> Vec<u16> {
    let mut out = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        let src_y = crop_top + y;
        let src_start = (src_y * stride + crop_left) as usize;
        out.extend_from_slice(&plane[src_start..src_start + width as usize]);
    }
    out
}

fn compare_plane(
    ours: &[u16],
    reference: &[u8],
    w: u32,
    h: u32,
    name: &str,
) -> (f64, Option<(u32, u32, u16, u8)>) {
    let size = (w * h) as usize;
    assert_eq!(
        ours.len(),
        size,
        "{} plane size mismatch: {} vs {}",
        name,
        ours.len(),
        size
    );

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
