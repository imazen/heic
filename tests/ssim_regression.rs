//! Per-frame SSIMULACRA2 regression test against dec265 reference
//!
//! Computes SSIMULACRA2 for each frame of MERGE_A and AMVP_A,
//! comparing our RGB output against dec265's reference YUV.
//!
//! Run: cargo test --test ssim_regression -- --nocapture --ignored

use fast_ssim2::compute_ssimulacra2;
use imgref::ImgVec;
use std::path::Path;

#[test]
#[ignore]
fn ssim2_per_frame_merge_a() {
    run_ssim2_test("MERGE_A_TI_3", 416, 240);
}

#[test]
#[ignore]
fn ssim2_per_frame_amvp_a() {
    run_ssim2_test("AMVP_A_MTK_4", 416, 240);
}

fn run_ssim2_test(vector_name: &str, w: u32, h: u32) {
    let bs = find_bit(&format!("conformance/vectors/{vector_name}"));
    let bs = match bs {
        Some(p) => p,
        None => { eprintln!("SKIP: {vector_name} not downloaded"); return; }
    };

    let ref_path = find_ref(&format!("conformance/vectors/{vector_name}"));
    let ref_path = match ref_path {
        Some(p) => p,
        None => { eprintln!("SKIP: reference.yuv not found"); return; }
    };

    // Decode with our decoder
    let data = std::fs::read(&bs).unwrap();
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    let frames = decoder.decode_annex_b(&data).unwrap();

    // Load reference YUV
    let ref_data = std::fs::read(&ref_path).unwrap();
    let luma_size = (w * h) as usize;
    let chroma_size = ((w / 2) * (h / 2)) as usize;
    let frame_size = luma_size + 2 * chroma_size;
    let ref_count = ref_data.len() / frame_size;

    let compare_count = frames.len().min(ref_count);
    eprintln!("\n=== {vector_name} SSIMULACRA2 ({compare_count} frames) ===");

    let mut worst_ssim = f64::MAX;
    let mut best_ssim = f64::MIN;
    let mut sum_ssim = 0.0f64;

    for (i, frame) in frames.iter().enumerate().take(compare_count) {
        // Compare Y-plane only (avoids color conversion differences)
        let cw = frame.cropped_width() as usize;
        let ch = frame.cropped_height() as usize;

        // Extract cropped Y plane from our decoder
        let our_y = crop_y(frame);

        // Extract reference Y plane
        let ref_offset = i * frame_size;
        let ref_y_raw = &ref_data[ref_offset..ref_offset + luma_size];

        // Build grayscale images (expand Y to pseudo-RGB for SSIM2)
        let our_pixels: Vec<[u8; 3]> = our_y.iter()
            .map(|&v| { let b = v.min(255) as u8; [b, b, b] })
            .collect();
        let ref_pixels: Vec<[u8; 3]> = ref_y_raw.iter()
            .map(|&v| [v, v, v])
            .collect();

        if our_pixels.len() != cw * ch || ref_pixels.len() != w as usize * h as usize {
            eprintln!("  Frame {i}: dimension mismatch");
            continue;
        }

        // Use cropped dimensions for both (reference YUV is at coded dimensions)
        let our_img = ImgVec::new(our_pixels, cw, ch);
        // Crop reference to match
        let mut ref_cropped = Vec::with_capacity(cw * ch);
        for y in 0..ch {
            for x in 0..cw {
                ref_cropped.push(ref_pixels[y * w as usize + x]);
            }
        }
        let ref_img = ImgVec::new(ref_cropped, cw, ch);

        match compute_ssimulacra2(ref_img.as_ref(), our_img.as_ref()) {
            Ok(ssim) => {
                worst_ssim = worst_ssim.min(ssim);
                best_ssim = best_ssim.max(ssim);
                sum_ssim += ssim;
                if i < 10 || ssim < 50.0 {
                    eprintln!("  Frame {i}: SSIM2 = {ssim:.2}");
                }
            }
            Err(e) => {
                eprintln!("  Frame {i}: SSIM2 error: {e:?}");
            }
        }
    }

    if compare_count > 0 {
        let avg = sum_ssim / compare_count as f64;
        eprintln!("\n  Summary: best={best_ssim:.2} worst={worst_ssim:.2} avg={avg:.2}");
        eprintln!("  Frames compared: {compare_count}");
    }
}

/// Simple BT.601 YUV420 to RGB conversion for reference frames
#[allow(dead_code)]
fn yuv420_to_rgb(y: &[u8], cb: &[u8], cr: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(w * h * 3);
    for py in 0..h {
        for px in 0..w {
            let yv = y[py * w + px] as i32;
            let cbv = cb[(py / 2) * (w / 2) + (px / 2)] as i32 - 128;
            let crv = cr[(py / 2) * (w / 2) + (px / 2)] as i32 - 128;
            let r = (yv + ((359 * crv + 128) >> 8)).clamp(0, 255) as u8;
            let g = (yv + ((-88 * cbv - 183 * crv + 128) >> 8)).clamp(0, 255) as u8;
            let b = (yv + ((454 * cbv + 128) >> 8)).clamp(0, 255) as u8;
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
    }
    rgb
}

fn crop_y(frame: &heic_decoder::DecodedFrame) -> Vec<u16> {
    let cw = frame.cropped_width();
    let stride = frame.width as usize;
    let mut out = Vec::with_capacity((cw * frame.cropped_height()) as usize);
    for y in frame.crop_top..(frame.height - frame.crop_bottom) {
        let s = y as usize * stride + frame.crop_left as usize;
        out.extend_from_slice(&frame.y_plane[s..s + cw as usize]);
    }
    out
}

fn find_bit(dir: &str) -> Option<std::path::PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    walkdir(&dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "bit"))
}

fn find_ref(dir: &str) -> Option<std::path::PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    walkdir(&dir).into_iter().find(|p| {
        p.file_name().is_some_and(|n| n == "reference.yuv")
    })
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut r = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() { r.extend(walkdir(&p)); } else { r.push(p); }
        }
    }
    r
}
