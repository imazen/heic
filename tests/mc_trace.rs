//! Targeted MC tracing for inter prediction debugging
//!
//! Run: cargo test --test mc_trace -- --nocapture --ignored

use std::path::Path;

#[test]
#[ignore]
fn compare_nofilter() {
    let bitstream = Path::new("/home/lilith/work/heic/libde265-src/testdata/girlshy.h265");
    if !bitstream.exists() {
        eprintln!("SKIP: girlshy.h265 not found");
        return;
    }
    let ref_path = Path::new("/tmp/girlshy_nofilter.yuv");
    if !ref_path.exists() {
        eprintln!("SKIP: run dec265 --disable-deblocking --disable-sao first");
        return;
    }

    let data = std::fs::read(bitstream).unwrap();
    let mut decoder = heic::VideoDecoder::new(16);
    decoder.disable_loop_filters = true;
    let frames = decoder.decode_annex_b(&data).unwrap();

    let ref_data = std::fs::read(ref_path).unwrap();
    let w = 320u32; // coded width (before crop)
    let h = 240u32;
    let luma_size = (w * h) as usize;
    let frame_size = luma_size + 2 * ((w / 2) * (h / 2)) as usize;

    // Also compare against FILTERED reference to check if our flag works
    let filtered_ref = Path::new("/tmp/girlshy_ref.yuv");
    if filtered_ref.exists() {
        let fref_data = std::fs::read(filtered_ref).unwrap();
        let fref_y: Vec<u16> = fref_data[..luma_size].iter().map(|&b| b as u16).collect();
        let stride = frames[0].width as usize;
        let mut exact = 0u32;
        for y in 0..h as usize {
            for x in 0..316usize {
                if fref_y[y * w as usize + x] == frames[0].y_plane[y * stride + x] {
                    exact += 1;
                }
            }
        }
        let total = 316 * h;
        eprintln!(
            "Frame 0 sanity: our unfiltered vs dec265 FILTERED: {exact}/{total} ({:.1}%)",
            100.0 * exact as f64 / total as f64
        );
    }

    // First diff in unfiltered I-frame
    {
        let ref_y: Vec<u16> = ref_data[..luma_size].iter().map(|&b| b as u16).collect();
        let stride = frames[0].width as usize;
        for y in 0..h as usize {
            for x in 0..316usize {
                let rv = ref_y[y * w as usize + x];
                let ov = frames[0].y_plane[y * stride + x];
                if rv != ov {
                    let d = ov as i32 - rv as i32;
                    eprintln!(
                        "First unfiltered I-frame diff: ({x},{y}) ours={ov} dec265={rv} diff={d:+}"
                    );
                    // Show 4x4 around it
                    let bx = (x / 4) * 4;
                    let by = (y / 4) * 4;
                    for dy in 0..4 {
                        eprint!("  ");
                        for dx in 0..4 {
                            let gx = bx + dx;
                            let gy = by + dy;
                            let r = ref_y[gy * w as usize + gx];
                            let o = frames[0].y_plane[gy * stride + gx];
                            eprint!("{}({:+}) ", o, o as i32 - r as i32);
                        }
                        eprintln!();
                    }
                    break;
                }
            }
        }
    }

    eprintln!("=== No-filter comparison (MC + residual only, cropped) ===");
    let cw = 316u32; // cropped width
    let compare_count = frames.len().min(ref_data.len() / frame_size).min(10);
    for fi in 0..compare_count {
        let ref_y: Vec<u16> = ref_data[fi * frame_size..fi * frame_size + luma_size]
            .iter()
            .map(|&b| b as u16)
            .collect();
        let stride = frames[fi].width as usize;
        let mut exact = 0u32;
        let mut max_diff = 0u16;
        let mut sse = 0u64;
        let mut count = 0u32;
        // Only compare cropped region (skip crop_right=4 columns on right)
        for y in 0..h as usize {
            for x in 0..cw as usize {
                let rv = ref_y[y * w as usize + x];
                let ov = frames[fi].y_plane[y * stride + x];
                let d = (ov as i32 - rv as i32).unsigned_abs() as u16;
                count += 1;
                if d == 0 {
                    exact += 1;
                } else {
                    max_diff = max_diff.max(d);
                    sse += d as u64 * d as u64;
                }
            }
        }
        let mse = sse as f64 / count as f64;
        let psnr = if mse > 0.0 {
            10.0 * (255.0 * 255.0 / mse).log10()
        } else {
            f64::INFINITY
        };
        eprintln!(
            "  Frame {fi}: PSNR={psnr:.1}dB, exact={exact}/{count} ({:.1}%), max_diff={max_diff}",
            100.0 * exact as f64 / count as f64
        );
    }
}

#[test]
#[ignore]
fn trace_girlshy_p_frame_mvs() {
    let bitstream = Path::new("/home/lilith/work/heic/libde265-src/testdata/girlshy.h265");
    if !bitstream.exists() {
        eprintln!("SKIP: girlshy.h265 not found");
        return;
    }

    let data = std::fs::read(bitstream).unwrap();
    let mut decoder = heic::VideoDecoder::new(16);
    // Enable MV tracing for the first inter frame (P-frame at POC=4)
    decoder.mv_trace_next_inter = true;
    let _ = decoder.decode_annex_b(&data).unwrap();
    // Output goes to stderr via MV_TRACE eprintln
}

#[test]
#[ignore]
fn compare_cabac_bins() {
    let bitstream = Path::new("/home/lilith/work/heic/libde265-src/testdata/girlshy.h265");
    if !bitstream.exists() {
        eprintln!("SKIP: girlshy.h265 not found");
        return;
    }

    let data = std::fs::read(bitstream).unwrap();
    let mut decoder = heic::VideoDecoder::new(16);
    // Enable per-bin tracing for the first inter frame
    decoder.mv_trace_next_inter = true; // this resets SE counter; we also need bin trace
    // Enable bin trace: first 200 bins
    heic::cabac_bin_trace(200);
    let _ = decoder.decode_annex_b(&data).unwrap();
}

#[test]
#[ignore]
fn trace_girlshy_frame1() {
    let bitstream = Path::new("/home/lilith/work/heic/libde265-src/testdata/girlshy.h265");
    if !bitstream.exists() {
        eprintln!("SKIP: girlshy.h265 not found");
        return;
    }

    let data = std::fs::read(bitstream).unwrap();
    let mut decoder = heic::VideoDecoder::new(16);
    let frames = decoder.decode_annex_b(&data).unwrap();

    // Load reference YUV
    let ref_path = Path::new("/tmp/girlshy_ref.yuv");
    if !ref_path.exists() {
        eprintln!("SKIP: reference YUV not generated (run conformance test first)");
        return;
    }
    let ref_data = std::fs::read(ref_path).unwrap();
    let w = 316u32;
    let h = 240u32;
    let luma_size = (w * h) as usize;
    let frame_size = luma_size + 2 * ((w / 2) * (h / 2)) as usize;

    eprintln!("Decoded {} frames", frames.len());

    // Frame 0: I-frame
    let our_f0 = &frames[0];
    let ref_f0: Vec<u16> = ref_data[..luma_size].iter().map(|&b| b as u16).collect();

    // Frame 1: first inter frame
    let our_f1 = &frames[1];
    let ref_f1: Vec<u16> = ref_data[frame_size..frame_size + luma_size]
        .iter()
        .map(|&b| b as u16)
        .collect();

    // Print I-frame and inter-frame pixel values for the first 8x8 block
    eprintln!("\n=== Frame 0 (I) vs Frame 1 (inter) at (0,0) 8x8 block ===");
    eprintln!("Legend: I=I-frame, R1=ref-frame1, O1=our-frame1, D=diff(O1-R1)");

    let stride0 = our_f0.width as usize;
    let stride1 = our_f1.width as usize;

    for y in 0..8u32 {
        eprint!("  y={y}: ");
        for x in 0..8u32 {
            let ref_idx = (y * w + x) as usize;
            let our0_idx = y as usize * stride0 + x as usize;
            let our1_idx = y as usize * stride1 + x as usize;

            let i_val = our_f0.y_plane[our0_idx]; // I-frame pixel (known perfect)
            let r1_val = ref_f1[ref_idx]; // Reference frame 1 pixel
            let o1_val = our_f1.y_plane[our1_idx]; // Our frame 1 pixel

            let diff = o1_val as i32 - r1_val as i32;
            eprint!("I{i_val:3}/R{r1_val:3}/O{o1_val:3}({diff:+3})  ");
        }
        eprintln!();
    }

    // Frame 4 (P-frame, POC=4) — only references I-frame, so errors are pure MC bugs
    if frames.len() > 4 && ref_data.len() >= 5 * frame_size {
        let our_f4 = &frames[4];
        let ref_f4: Vec<u16> = ref_data[4 * frame_size..4 * frame_size + luma_size]
            .iter()
            .map(|&b| b as u16)
            .collect();
        let stride4 = our_f4.width as usize;

        eprintln!("\n=== Frame 4 (P, POC=4, refs I-frame only) at (0,0) 16x8 block ===");
        for y in 0..8u32 {
            eprint!("  y={y}: ");
            for x in 0..16u32.min(w) {
                let ref_idx = (y * w + x) as usize;
                let our4_idx = y as usize * stride4 + x as usize;
                let i_idx = y as usize * stride0 + x as usize;

                let _i_val = our_f0.y_plane[i_idx];
                let r4_val = ref_f4[ref_idx];
                let o4_val = our_f4.y_plane[our4_idx];
                let diff = o4_val as i32 - r4_val as i32;

                eprint!("{o4_val:3}({diff:+3}) ");
            }
            eprintln!();
        }

        // Find first PU-sized block where ALL pixels match (suggests correct MC there)
        // and first PU-sized block where all differ
        let mut first_exact_block = None;
        let mut first_bad_block = None;
        for by in 0..(h / 4) {
            for bx in 0..(w / 4) {
                let mut all_exact = true;
                let mut all_bad = true;
                for dy in 0..4u32 {
                    for dx in 0..4u32 {
                        let gy = by * 4 + dy;
                        let gx = bx * 4 + dx;
                        if gy < h && gx < w {
                            let ref_val = ref_f4[(gy * w + gx) as usize];
                            let our_val = our_f4.y_plane[(gy as usize * stride4 + gx as usize)];
                            if our_val != ref_val {
                                all_exact = false;
                            }
                            if our_val == ref_val {
                                all_bad = false;
                            }
                        }
                    }
                }
                if all_exact && first_exact_block.is_none() {
                    first_exact_block = Some((bx * 4, by * 4));
                }
                if all_bad && first_bad_block.is_none() {
                    first_bad_block = Some((bx * 4, by * 4));
                }
            }
        }
        eprintln!("\nFirst fully exact 4x4 block: {:?}", first_exact_block);
        eprintln!("First fully wrong 4x4 block: {:?}", first_bad_block);

        // Find the very first differing pixel
        let mut first_diff_pos = None;
        for y in 0..h {
            for x in 0..w {
                let ref_val = ref_f4[(y * w + x) as usize];
                let our_val = our_f4.y_plane[(y as usize * stride4 + x as usize)];
                if our_val != ref_val && first_diff_pos.is_none() {
                    first_diff_pos = Some((x, y, our_val, ref_val));
                }
            }
        }
        if let Some((x, y, ours, reference)) = first_diff_pos {
            let diff = ours as i32 - reference as i32;
            eprintln!(
                "First differing pixel in frame 4: ({x},{y}) ours={ours} ref={reference} diff={diff:+}"
            );
            eprintln!(
                "  CTU ({},{}), local ({},{})",
                x / 64,
                y / 64,
                x % 64,
                y % 64
            );

            // Show surrounding context: 8x8 block around the first diff
            let bx = (x / 8) * 8;
            let by = (y / 8) * 8;
            eprintln!("  Surrounding 8x4 (ours/ref/diff):");
            for dy in 0..4u32.min(h - by) {
                eprint!("    ");
                for dx in 0..8u32.min(w - bx) {
                    let gx = bx + dx;
                    let gy = by + dy;
                    let rv = ref_f4[(gy * w + gx) as usize];
                    let ov = our_f4.y_plane[gy as usize * stride4 + gx as usize];
                    let d = ov as i32 - rv as i32;
                    eprint!("{ov:3}({d:+3}) ");
                }
                eprintln!();
            }

            // Manually verify MC for the I-frame reference at the MV position
            // PU at (32,0) MV=(-112,-19): ref_pos = (32-28, 0-5) = (4, -5)
            // For pixel (62,0): ref_x = 4 + (62-32) = 34, ref_y = -5, frac=(0,1)
            // With all y-clamped to 0: should be I-frame[34, 0]
            let iframe_val_at_34_0 = our_f0.y_plane[34];
            eprintln!("  I-frame[34,0] = {iframe_val_at_34_0}");
            eprintln!("  I-frame row 0, cols 30..40:");
            eprint!("   ");
            for cx in 30..40u32.min(our_f0.width) {
                eprint!(" {:3}", our_f0.y_plane[cx as usize]);
            }
            eprintln!();

            // Show first 5 diffs with pixel positions
            eprintln!("\n  First 20 differing pixels in frame 4:");
            let mut diff_count = 0;
            for gy in 0..h {
                for gx in 0..w {
                    let rv = ref_f4[(gy * w + gx) as usize];
                    let ov = our_f4.y_plane[gy as usize * stride4 + gx as usize];
                    if ov != rv {
                        let d = ov as i32 - rv as i32;
                        eprintln!("    ({gx},{gy}) ours={ov} ref={rv} diff={d:+}");
                        diff_count += 1;
                        if diff_count >= 20 {
                            break;
                        }
                    }
                }
                if diff_count >= 20 {
                    break;
                }
            }
        }

        // Pixel at (0,0): check what MC from I-frame with common MVs would give
        eprintln!("\n=== MC reverse-engineering for frame 4 pixel (0,0) ===");
        let our_val = our_f4.y_plane[0];
        let ref_val = ref_f4[0];
        eprintln!(
            "  Our: {our_val}, Ref: {ref_val}, I-frame: {}",
            our_f0.y_plane[0]
        );
        // Check what value we'd get with zero MV uni-pred from I-frame
        eprintln!("  Zero MV uni-pred from I: {}", our_f0.y_plane[0]);

        // Check nearby I-frame pixels to see if MV points somewhere
        eprintln!("  I-frame 16x16 at (0,0):");
        for y in 0..16u32 {
            eprint!("    ");
            for x in 0..16u32 {
                eprint!("{:3} ", our_f0.y_plane[y as usize * stride0 + x as usize]);
            }
            eprintln!();
        }
    }

    // Also check: what does bi-pred with zero MV from frame 0 produce?
    // For bi-pred integer: (pixel<<6 + pixel<<6 + 64) >> 7 = pixel
    // For uni-pred integer: pixel (direct copy)
    eprintln!("\n=== Expected MC outputs for pixel (0,0) ===");
    let p00 = our_f0.y_plane[0] as i32;
    let uni_pred = p00;
    let bi_pred = (p00 * 64 + p00 * 64 + 64) >> 7;
    eprintln!("  I-frame pixel: {p00}");
    eprintln!("  Uni-pred (zero MV from I): {uni_pred}");
    eprintln!("  Bi-pred (zero MV, both from I): {bi_pred}");
    eprintln!("  Ref frame 1 pixel: {}", ref_f1[0]);
    eprintln!("  Our frame 1 pixel: {}", our_f1.y_plane[0]);

    // Check frame counts
    eprintln!("\nRef frame count: {}", ref_data.len() / frame_size);
    eprintln!("Our frame count: {}", frames.len());

    // Print cropped dimensions
    for (i, f) in frames.iter().enumerate().take(3) {
        eprintln!(
            "  Frame {i}: coded={}x{}, cropped={}x{}, crop=({},{},{},{})",
            f.width,
            f.height,
            f.cropped_width(),
            f.cropped_height(),
            f.crop_left,
            f.crop_right,
            f.crop_top,
            f.crop_bottom
        );
    }

    // Are frame 0 and frame 1 in OUR decoder the same frame? (bug: duplicate I-frame)
    let mut same_count = 0u32;
    for i in 0..luma_size
        .min(our_f0.y_plane.len())
        .min(our_f1.y_plane.len())
    {
        if our_f0.y_plane[i] == our_f1.y_plane[i] {
            same_count += 1;
        }
    }
    eprintln!(
        "\nFrame 0 vs Frame 1 identity check: {same_count}/{} same pixels ({:.1}%)",
        luma_size,
        100.0 * same_count as f64 / luma_size as f64
    );

    // Per-frame PSNR for first 10 frames
    eprintln!("\n=== Per-frame Y-plane comparison (first 10 frames) ===");
    let compare_count = frames.len().min(ref_data.len() / frame_size).min(10);
    #[allow(clippy::needless_range_loop)]
    for fi in 0..compare_count {
        let ref_y_start = fi * frame_size;
        let ref_y: Vec<u16> = ref_data[ref_y_start..ref_y_start + luma_size]
            .iter()
            .map(|&b| b as u16)
            .collect();

        let our_y = &frames[fi].y_plane;
        let stride = frames[fi].width as usize;
        let mut sse = 0u64;
        let mut exact = 0u32;
        let mut max_diff = 0u16;
        let _cw = frames[fi].cropped_width() as usize;
        let _ch = frames[fi].cropped_height() as usize;

        for y in 0..h as usize {
            for x in 0..w as usize {
                let ref_val = ref_y[y * w as usize + x];
                let our_val = our_y[y * stride + x];
                let diff = (our_val as i32 - ref_val as i32).unsigned_abs() as u16;
                if diff == 0 {
                    exact += 1;
                } else {
                    max_diff = max_diff.max(diff);
                    sse += (diff as u64) * (diff as u64);
                }
            }
        }
        let mse = sse as f64 / luma_size as f64;
        let psnr = if mse > 0.0 {
            10.0 * (255.0 * 255.0 / mse).log10()
        } else {
            f64::INFINITY
        };
        eprintln!(
            "  Frame {fi}: PSNR={psnr:.1}dB, exact={exact}/{luma_size} ({:.1}%), max_diff={max_diff}",
            100.0 * exact as f64 / luma_size as f64
        );
    }
}
