//! Rounding difference analysis: compare our decoder pixel-by-pixel against dec265
//!
//! Run: cargo test --test rounding_analysis -- --nocapture --ignored

use std::path::Path;

#[test]
#[ignore]
fn analyze_merge_a_rounding() {
    let bs = find_bit("conformance/vectors/MERGE_A_TI_3");
    let bs = match bs {
        Some(p) => p,
        None => {
            eprintln!("SKIP: MERGE_A not downloaded");
            return;
        }
    };

    let ref_path = Path::new("conformance/vectors/MERGE_A_TI_3/reference.yuv");
    if !ref_path.exists() {
        eprintln!("SKIP: reference.yuv not generated");
        return;
    }

    let data = std::fs::read(&bs).unwrap();
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    let frames = decoder.decode_annex_b(&data).unwrap();

    let ref_data = std::fs::read(ref_path).unwrap();
    let w = 416u32;
    let h = 240u32;
    let luma_size = (w * h) as usize;
    let frame_size = luma_size + 2 * ((w / 2) * (h / 2)) as usize;

    eprintln!("=== MERGE_A Rounding Analysis ===");
    eprintln!(
        "Decoded {} frames, reference has {} frames\n",
        frames.len(),
        ref_data.len() / frame_size
    );

    // Frame 0: I-frame (should be exact)
    let our_y0 = crop_y(&frames[0], w, h);
    let ref_y0: Vec<u16> = ref_data[..luma_size].iter().map(|&b| b as u16).collect();
    let _diffs0 = pixel_analysis(&our_y0, &ref_y0, w, h, "Frame 0 (I)");

    // Frame 1: first inter frame
    if frames.len() > 1 {
        let our_y1 = crop_y(&frames[1], w, h);
        let ref_y1: Vec<u16> = ref_data[frame_size..frame_size + luma_size]
            .iter()
            .map(|&b| b as u16)
            .collect();
        pixel_analysis(&our_y1, &ref_y1, w, h, "Frame 1 (inter)");

        // Check: pixels that match the I-frame exactly
        let mut match_iframe = 0u32;
        let mut diff_from_iframe_ours = 0u32;
        let mut diff_from_iframe_ref = 0u32;
        for i in 0..luma_size.min(our_y1.len()).min(ref_y1.len()) {
            let our_v = our_y1[i];
            let ref_v = ref_y1[i];
            let iframe_v = ref_y0[i];
            if our_v == iframe_v {
                match_iframe += 1;
            }
            if our_v != iframe_v {
                diff_from_iframe_ours += 1;
            }
            if ref_v != iframe_v {
                diff_from_iframe_ref += 1;
            }
        }
        eprintln!(
            "  Our pixels matching I-frame: {match_iframe}/{luma_size} ({:.1}%)",
            100.0 * match_iframe as f64 / luma_size as f64
        );
        eprintln!("  Our pixels differing from I-frame: {diff_from_iframe_ours}");
        eprintln!("  Ref pixels differing from I-frame: {diff_from_iframe_ref}");

        // For pixels that SHOULD match I-frame (where ref==iframe), do ours also match?
        let mut should_match_count = 0u32;
        let mut actually_match = 0u32;
        let mut mismatch_diff_hist = [0u32; 16];
        for i in 0..luma_size.min(our_y1.len()).min(ref_y1.len()) {
            if ref_y1[i] == ref_y0[i] {
                // Reference says this pixel didn't change from I-frame
                should_match_count += 1;
                if our_y1[i] == ref_y0[i] {
                    actually_match += 1;
                } else {
                    let diff =
                        (our_y1[i] as i32 - ref_y0[i] as i32).unsigned_abs().min(15) as usize;
                    mismatch_diff_hist[diff] += 1;
                }
            }
        }
        eprintln!("\n  Pixels where ref==I-frame (skip/zero-residual regions):");
        eprintln!("    Total: {should_match_count}");
        eprintln!(
            "    Ours also match: {actually_match} ({:.1}%)",
            100.0 * actually_match as f64 / should_match_count.max(1) as f64
        );
        if should_match_count > actually_match {
            eprintln!("    Our mismatches in these regions:");
            for (d, c) in mismatch_diff_hist.iter().enumerate() {
                if *c > 0 {
                    eprintln!("      diff={d}: {c} pixels");
                }
            }
        }

        // Signed error analysis: ours - ref
        let mut positive_errors = 0i64; // ours > ref
        let mut negative_errors = 0i64; // ours < ref
        let mut error_sum = 0i64;
        for i in 0..luma_size.min(our_y1.len()).min(ref_y1.len()) {
            let err = our_y1[i] as i64 - ref_y1[i] as i64;
            error_sum += err;
            if err > 0 {
                positive_errors += 1;
            }
            if err < 0 {
                negative_errors += 1;
            }
        }
        let total_errors = positive_errors + negative_errors;
        eprintln!("\n  Signed error analysis (ours - ref):");
        eprintln!("    Mean error: {:.3}", error_sum as f64 / luma_size as f64);
        eprintln!(
            "    Positive (ours > ref): {positive_errors} ({:.1}%)",
            100.0 * positive_errors as f64 / total_errors.max(1) as f64
        );
        eprintln!(
            "    Negative (ours < ref): {negative_errors} ({:.1}%)",
            100.0 * negative_errors as f64 / total_errors.max(1) as f64
        );

        // Check: for the FIRST CU (0,0) 32x32, what fraction of pixels match?
        let mut cu00_exact = 0u32;
        let mut cu00_total = 0u32;
        for y in 0..32u32.min(h) {
            for x in 0..32u32.min(w) {
                let i = (y * w + x) as usize;
                cu00_total += 1;
                if i < our_y1.len() && i < ref_y1.len() && our_y1[i] == ref_y1[i] {
                    cu00_exact += 1;
                }
            }
        }
        eprintln!(
            "\n  First CU (0,0) 32x32: {cu00_exact}/{cu00_total} exact ({:.1}%)",
            100.0 * cu00_exact as f64 / cu00_total as f64
        );

        // Check per-CTU exact rate
        let ctb = 64u32;
        eprintln!("  Per-CTU exact pixel rate:");
        for cty in 0..h.div_ceil(ctb) {
            for ctx in 0..w.div_ceil(ctb) {
                let mut exact = 0u32;
                let mut total = 0u32;
                for dy in 0..ctb.min(h - cty * ctb) {
                    for dx in 0..ctb.min(w - ctx * ctb) {
                        let i = ((cty * ctb + dy) * w + ctx * ctb + dx) as usize;
                        total += 1;
                        if i < our_y1.len() && i < ref_y1.len() && our_y1[i] == ref_y1[i] {
                            exact += 1;
                        }
                    }
                }
                eprint!(" {:.0}%", 100.0 * exact as f64 / total.max(1) as f64);
            }
            eprintln!();
        }
    }
}

fn pixel_analysis(ours: &[u16], reference: &[u16], _w: u32, _h: u32, name: &str) -> usize {
    let len = ours.len().min(reference.len());
    let mut exact = 0usize;
    let mut diff_hist = [0u32; 256];
    for i in 0..len {
        let a = ours[i];
        let b = reference[i];
        if a == b {
            exact += 1;
            continue;
        }
        let d = (a as i32 - b as i32).unsigned_abs().min(255) as usize;
        diff_hist[d] += 1;
    }
    let total_diff = len - exact;
    eprintln!(
        "{name}: {exact}/{len} exact ({:.1}%), {total_diff} different",
        100.0 * exact as f64 / len as f64
    );
    if total_diff > 0 {
        let small: u32 = diff_hist[1..=3].iter().sum();
        eprintln!(
            "  diff=1: {}, diff=2: {}, diff=3: {}, small(1-3): {} ({:.1}%)",
            diff_hist[1],
            diff_hist[2],
            diff_hist[3],
            small,
            100.0 * small as f64 / total_diff as f64
        );
    }
    total_diff
}

fn crop_y(frame: &heic_decoder::DecodedFrame, _w: u32, _h: u32) -> Vec<u16> {
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
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    walkdir(&dir)
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "bit"))
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut r = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                r.extend(walkdir(&p));
            } else {
                r.push(p);
            }
        }
    }
    r
}
