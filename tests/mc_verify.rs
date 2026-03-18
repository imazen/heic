//! Manual MC verification: compute expected MC output from known MV and reference frame
//!
//! Run: cargo test --release --test mc_verify -- --nocapture --ignored

use std::path::Path;

/// Manually compute luma MC for one pixel given reference frame, MV, and position
fn manual_mc_luma_pixel(
    ref_y: &[u16],
    stride: usize,
    pic_w: i32,
    pic_h: i32,
    px: i32,
    py: i32,
    mv_x: i16,
    mv_y: i16,
    bit_depth: u8,
) -> i16 {
    let int_x = px + (mv_x as i32 >> 2);
    let int_y = py + (mv_y as i32 >> 2);
    let frac_x = (mv_x as i32 & 3) as usize;
    let frac_y = (mv_y as i32 & 3) as usize;

    const LUMA_FILTER: [[i16; 8]; 4] = [
        [0, 0, 0, 64, 0, 0, 0, 0],
        [-1, 4, -10, 58, 17, -5, 1, 0],
        [-1, 4, -11, 40, 40, -11, 4, -1],
        [0, 1, -5, 17, 58, -10, 4, -1],
    ];

    let shift1 = bit_depth as i32 - 8 + 6;
    let offset1 = 1i32 << (shift1 - 1);
    let max_val = (1i32 << bit_depth) - 1;

    let fetch = |x: i32, y: i32| -> i32 {
        let sx = x.clamp(0, pic_w - 1);
        let sy = y.clamp(0, pic_h - 1);
        ref_y[(sy as usize * stride + sx as usize)] as i32
    };

    if frac_x == 0 && frac_y == 0 {
        fetch(int_x, int_y) as i16
    } else if frac_y == 0 {
        let coeff = &LUMA_FILTER[frac_x];
        let mut sum = 0i32;
        for k in 0..8 {
            sum += fetch(int_x + k as i32 - 3, int_y) * coeff[k] as i32;
        }
        ((sum + offset1) >> shift1).clamp(0, max_val) as i16
    } else if frac_x == 0 {
        let coeff = &LUMA_FILTER[frac_y];
        let mut sum = 0i32;
        for k in 0..8 {
            sum += fetch(int_x, int_y + k as i32 - 3) * coeff[k] as i32;
        }
        ((sum + offset1) >> shift1).clamp(0, max_val) as i16
    } else {
        // 2D: H then V
        let coeff_h = &LUMA_FILTER[frac_x];
        let coeff_v = &LUMA_FILTER[frac_y];
        let mut tmp = [0i32; 8];
        for ky in 0..8 {
            let mut sum = 0i32;
            for kx in 0..8 {
                sum += fetch(int_x + kx as i32 - 3, int_y + ky as i32 - 3) * coeff_h[kx] as i32;
            }
            tmp[ky] = sum; // Keep at filter precision
        }
        let shift2 = 6i32;
        let total_shift = shift1 + shift2;
        let total_offset = 1i64 << (total_shift - 1);
        let mut sum = 0i64;
        for k in 0..8 {
            sum += tmp[k] as i64 * coeff_v[k] as i64;
        }
        (((sum + total_offset) >> total_shift) as i32).clamp(0, max_val) as i16
    }
}

#[test]
#[ignore]
fn verify_mc_first_pu() {
    let bitstream = Path::new("/home/lilith/work/heic/libde265-src/testdata/girlshy.h265");
    if !bitstream.exists() {
        eprintln!("SKIP: girlshy.h265 not found");
        return;
    }

    let data = std::fs::read(bitstream).unwrap();
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    let frames = decoder.decode_annex_b(&data).unwrap();

    // Frame 0: I-frame (reference for P-frame at POC=4 = frames[4])
    let iframe = &frames[0];
    let pframe = &frames[4]; // POC=4, P-frame
    let stride = iframe.width as usize;
    let pic_w = iframe.width as i32;
    let pic_h = iframe.height as i32;

    // Load reference
    let ref_path = Path::new("/tmp/girlshy_ref.yuv");
    if !ref_path.exists() {
        eprintln!("SKIP: reference YUV not generated");
        return;
    }
    let ref_data = std::fs::read(ref_path).unwrap();
    let w = 316u32;
    let h = 240u32;
    let luma_size = (320 * 240) as usize; // coded dimensions in reference YUV
    let frame_size = luma_size + 2 * ((320 / 2) * (240 / 2)) as usize;
    // IMPORTANT: dec265 outputs in DECODE order, not display order!
    // Decode order: I(0), P(4), B(2), B(1), B(3), P(9), ...
    // Our frames are sorted by POC (display order): 0, 1, 2, 3, 4, ...
    // So our frames[4] (POC=4) = reference index 1 (P-frame, 2nd decoded)
    let ref_decode_idx = 1; // P-frame at POC=4 is 2nd decoded frame
    let ref_f4: Vec<u16> = ref_data
        [ref_decode_idx * frame_size..ref_decode_idx * frame_size + luma_size]
        .iter()
        .map(|&b| b as u16)
        .collect();

    // Print pixel(0,0) for first 10 frames to determine ordering
    eprintln!("\n=== Our frames pixel(0,0) by POC ===");
    for (i, f) in frames.iter().enumerate().take(10) {
        eprintln!(
            "  frames[{i}] (coded {}x{}): pixel(0,0) = {}",
            f.width, f.height, f.y_plane[0]
        );
    }
    eprintln!("\n=== Reference YUV pixel(0,0) by index ===");
    let nref = ref_data.len() / frame_size;
    for i in 0..nref.min(10) {
        eprintln!("  ref[{i}]: pixel(0,0) = {}", ref_data[i * frame_size]);
    }

    // Check key pixel positions across CTU rows for frame 4
    eprintln!("\n=== Frame 4 key pixels ===");
    let stride4 = pframe.width as usize;
    let positions = [
        (0u32, 0u32),
        (0, 64),
        (0, 128),
        (0, 192),
        (100, 0),
        (100, 64),
        (100, 128),
    ];
    for &(x, y) in &positions {
        if (y as usize) < pframe.height as usize && (x as usize) < stride4 {
            let v = pframe.y_plane[y as usize * stride4 + x as usize];
            eprintln!("  ({x},{y}): ours={v}");
        }
    }

    // First PU: (0,0) 32x32, MV=(-27,-52) from L0 (I-frame)
    eprintln!("\n=== Manual MC verification: PU (0,0) 32x32, MV=(-27,-52) ===");
    let mv_x: i16 = -27;
    let mv_y: i16 = -52;

    // Compute expected MC prediction for first 8 pixels
    eprintln!("Pixel | I-frame | MC_pred | Our_out | Ref_out | Our-Ref | Our-MC(=residual)");
    for y in 0..4i32 {
        for x in 0..8i32 {
            let mc_pred =
                manual_mc_luma_pixel(&iframe.y_plane, stride, pic_w, pic_h, x, y, mv_x, mv_y, 8);
            let our_val = pframe.y_plane[y as usize * stride + x as usize];
            let ref_val = ref_f4[(y * 320 + x) as usize];
            let iframe_val = iframe.y_plane[y as usize * stride + x as usize];
            let our_ref_diff = our_val as i32 - ref_val as i32;
            let our_mc_diff = our_val as i32 - mc_pred as i32; // = our residual
            eprintln!(
                "({x},{y}) I={iframe_val:3} MC={mc_pred:3} O={our_val:3} R={ref_val:3} O-R={our_ref_diff:+3} residual={our_mc_diff:+3}"
            );
        }
    }

    // Second PU: (32,0) 32x32, MV=(-112,-19)
    eprintln!("\n=== Manual MC verification: PU (32,0) 32x32, MV=(-112,-19) ===");
    let mv_x2: i16 = -112;
    let mv_y2: i16 = -19;

    // Check pixels near the boundary (x=60-63, y=0)
    eprintln!("Pixel | MC_pred | Our_out | Ref_out | Our-Ref | residual");
    for y in 0..2i32 {
        for x in 60..64i32 {
            let gx = 32 + x; // global x (PU starts at 32)
            let mc_pred = manual_mc_luma_pixel(
                &iframe.y_plane,
                stride,
                pic_w,
                pic_h,
                gx,
                y,
                mv_x2,
                mv_y2,
                8,
            );
            let our_val = pframe.y_plane[y as usize * stride + gx as usize];
            let ref_val = ref_f4[(y * 320 + gx) as usize];
            let diff = our_val as i32 - ref_val as i32;
            let residual = our_val as i32 - mc_pred as i32;
            eprintln!(
                "({gx},{y}) MC={mc_pred:3} O={our_val:3} R={ref_val:3} O-R={diff:+3} res={residual:+3}"
            );
        }
    }
}
