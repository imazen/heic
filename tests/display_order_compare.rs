//! Compare our decoder output against display-ordered ffmpeg reference
//!
//! Run: cargo test --release --test display_order_compare -- --nocapture --ignored

use std::path::Path;

#[test]
#[ignore]
fn compare_display_order() {
    let bitstream = Path::new("/home/lilith/work/heic/libde265-src/testdata/girlshy.h265");
    if !bitstream.exists() {
        eprintln!("SKIP: girlshy.h265 not found");
        return;
    }

    let ref_path = Path::new("/tmp/girlshy_ref_display.yuv");
    if !ref_path.exists() {
        eprintln!("SKIP: generate reference first with ffmpeg");
        return;
    }

    let data = std::fs::read(bitstream).unwrap();
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    let frames = decoder.decode_annex_b(&data).unwrap();

    let ref_data = std::fs::read(ref_path).unwrap();
    let w = 316u32;
    let h = 240u32;
    let luma_size = (w * h) as usize;
    let frame_size = luma_size + 2 * ((w / 2) * (h / 2)) as usize;

    eprintln!(
        "=== Display-order comparison (girlshy, cropped {}x{}) ===",
        w, h
    );
    let compare_count = frames.len().min(ref_data.len() / frame_size).min(15);
    for fi in 0..compare_count {
        let ref_y: Vec<u16> = ref_data[fi * frame_size..fi * frame_size + luma_size]
            .iter()
            .map(|&b| b as u16)
            .collect();

        // Extract cropped Y plane from our frame
        let f = &frames[fi];
        let stride = f.width as usize;
        let cw = f.cropped_width() as usize;
        let ch = f.cropped_height() as usize;
        let mut our_y = Vec::with_capacity(cw * ch);
        for y in f.crop_top..(f.height - f.crop_bottom) {
            let row_start = y as usize * stride + f.crop_left as usize;
            our_y.extend_from_slice(&f.y_plane[row_start..row_start + cw]);
        }

        let mut exact = 0u32;
        let mut max_diff = 0u16;
        let mut sse = 0u64;
        let len = our_y.len().min(ref_y.len());
        for i in 0..len {
            let d = (our_y[i] as i32 - ref_y[i] as i32).unsigned_abs() as u16;
            if d == 0 {
                exact += 1;
            } else {
                max_diff = max_diff.max(d);
                sse += d as u64 * d as u64;
            }
        }
        let mse = sse as f64 / len as f64;
        let psnr = if mse > 0.0 {
            10.0 * (255.0 * 255.0 / mse).log10()
        } else {
            f64::INFINITY
        };

        // Per-CTU-row breakdown
        let mut row_info = String::new();
        for row in 0..4u32 {
            let y_start = row * 64;
            if y_start >= h {
                break;
            }
            let y_end = ((row + 1) * 64).min(h);
            let mut row_exact = 0u32;
            let mut row_total = 0u32;
            for y in y_start..y_end {
                for x in 0..w {
                    let idx = (y * w + x) as usize;
                    if idx < len {
                        row_total += 1;
                        if our_y[idx] == ref_y[idx] {
                            row_exact += 1;
                        }
                    }
                }
            }
            let pct = if row_total > 0 {
                100.0 * row_exact as f64 / row_total as f64
            } else {
                0.0
            };
            row_info.push_str(&format!(" R{row}:{pct:.0}%"));
        }

        eprintln!(
            "  Frame {fi:2}: PSNR={psnr:6.1}dB exact={exact}/{len} ({:4.1}%) max={max_diff:3}{row_info}",
            100.0 * exact as f64 / len as f64
        );
    }
}
