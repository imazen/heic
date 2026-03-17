//! Debug MERGE_A B-frame output
//! Run: cargo test --release --test merge_a_debug -- --nocapture --ignored
use std::path::Path;

#[test]
#[ignore]
fn check_merge_a_nofilter() {
    let merge_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/vectors/MERGE_A_TI_3");
    let bit = walkdir(&merge_dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "bit"));
    let bit = match bit { Some(b) => b, None => { eprintln!("SKIP"); return; } };
    let ref_path = Path::new("/tmp/merge_a_nofilter.yuv");
    if !ref_path.exists() { eprintln!("SKIP: generate nofilter ref first"); return; }

    let data = std::fs::read(&bit).unwrap();
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    decoder.disable_loop_filters = true;
    let frames = decoder.decode_annex_b(&data).unwrap();

    let ref_data = std::fs::read(ref_path).unwrap();
    let w = 416u32; let h = 240u32;
    let luma_size = (w * h) as usize;
    let frame_size = luma_size + 2 * ((w/2) * (h/2)) as usize;

    // Dec265 nofilter is in DISPLAY order (POC 0,1,2,3,4,5,6,7)
    eprintln!("=== MERGE_A no-filter comparison (our nofilter vs dec265 nofilter) ===");
    // Compare I-frame (POC=0) — should be identical since no MC involved
    let stride = frames[0].width as usize;
    let ref_y: Vec<u16> = ref_data[..luma_size].iter().map(|&b| b as u16).collect();
    let mut exact = 0u32;
    for y in 0..h as usize {
        for x in 0..w as usize {
            if frames[0].y_plane[y * stride + x] == ref_y[y * w as usize + x] { exact += 1; }
        }
    }
    eprintln!("  I-frame (POC=0): {exact}/{luma_size} exact ({:.1}%)", 100.0 * exact as f64 / luma_size as f64);

    // Compare all frames
    for fi in 0..frames.len().min(ref_data.len() / frame_size) {
        let ref_fi: Vec<u16> = ref_data[fi*frame_size..fi*frame_size+luma_size].iter().map(|&b| b as u16).collect();
        let stride2 = frames[fi].width as usize;
        let mut exact2 = 0u32;
        let mut max_diff2 = 0u16;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let ov = frames[fi].y_plane[y * stride2 + x];
                let rv = ref_fi[y * w as usize + x];
                let d = (ov as i32 - rv as i32).unsigned_abs() as u16;
                if d == 0 { exact2 += 1; } else { max_diff2 = max_diff2.max(d); }
            }
        }
        eprintln!("  POC={fi} (nofilter): {exact2}/{luma_size} exact ({:.1}%) max={max_diff2}", 100.0 * exact2 as f64 / luma_size as f64);
    }

    // Detailed diff analysis for POC=4
    eprintln!("\nPOC=4 unfiltered diff analysis:");
    let ref_f4_nf: Vec<u16> = ref_data[4*frame_size..4*frame_size+luma_size].iter().map(|&b| b as u16).collect();
    let stride3 = frames[4].width as usize;
    let mut by_region = std::collections::HashMap::new();
    let mut first_diffs: Vec<(usize,usize,u16,u16)> = Vec::new();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let ov = frames[4].y_plane[y * stride3 + x];
            let rv = ref_f4_nf[y * w as usize + x];
            if ov != rv {
                let region = (x / 64, y / 64);
                *by_region.entry(region).or_insert(0u32) += 1;
                if first_diffs.len() < 20 { first_diffs.push((x, y, ov, rv)); }
            }
        }
    }
    eprintln!("  Diffs by 64x64 CTU:");
    let mut regions: Vec<_> = by_region.iter().collect();
    regions.sort_by_key(|((rx,ry),_)| (*ry, *rx));
    for ((rx, ry), count) in &regions {
        eprintln!("    CTU({},{}) = {} diffs", rx*64, ry*64, count);
    }
    eprintln!("  First 20 diff pixels:");
    for (x, y, ov, rv) in &first_diffs {
        eprintln!("    ({x:3},{y:3}) ours={ov:3} ref={rv:3} diff={:+}", *ov as i32 - *rv as i32);
    }

    // Compare B-frame POC=4 (our frames[4] vs dec265 display index 4)
    if frames.len() > 4 && ref_data.len() >= 5 * frame_size {
        let ref_f4: Vec<u16> = ref_data[4*frame_size..4*frame_size+luma_size].iter().map(|&b| b as u16).collect();
        let stride = frames[4].width as usize;
        let mut exact = 0u32;
        let mut max_diff = 0u16;
        let mut first_diff = None;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let ov = frames[4].y_plane[y * stride + x];
                let rv = ref_f4[y * w as usize + x];
                let d = (ov as i32 - rv as i32).unsigned_abs() as u16;
                if d == 0 { exact += 1; } else {
                    max_diff = max_diff.max(d);
                    if first_diff.is_none() { first_diff = Some((x, y, ov, rv)); }
                }
            }
        }
        eprintln!("  B-frame POC=4 (nofilter): {exact}/{luma_size} exact ({:.1}%) max_diff={max_diff}",
            100.0 * exact as f64 / luma_size as f64);
        if let Some((x, y, ov, rv)) = first_diff {
            eprintln!("    First diff: ({x},{y}) ours={ov} dec265={rv} diff={}", ov as i32 - rv as i32);
        }
    }
}

#[test]
#[ignore]
fn check_merge_a_pixels() {
    let merge_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/vectors/MERGE_A_TI_3");
    let bit = walkdir(&merge_dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "bit"));
    let bit = match bit { Some(b) => b, None => { eprintln!("SKIP"); return; } };
    let ref_path = merge_dir.join("reference_display.yuv");
    if !ref_path.exists() { eprintln!("SKIP: no display ref"); return; }

    let data = std::fs::read(&bit).unwrap();
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    let frames = decoder.decode_annex_b(&data).unwrap();

    let ref_data = std::fs::read(&ref_path).unwrap();
    let w = 416u32; let h = 240u32;
    let luma_size = (w * h) as usize;
    let frame_size = luma_size + 2 * ((w/2) * (h/2)) as usize;

    eprintln!("Decoded {} frames, ref has {} frames", frames.len(), ref_data.len() / frame_size);
    for fi in 0..frames.len().min(8) {
        let our_px = frames[fi].y_plane[0];
        let ref_px = ref_data[fi * frame_size] as u16;
        eprintln!("  frames[{fi}] pixel(0,0): ours={our_px} ref={ref_px} diff={}", our_px as i32 - ref_px as i32);
    }
    for fi in 0..frames.len().min(ref_data.len() / frame_size) {
        let f = &frames[fi];
        let stride = f.width as usize;
        let ref_y: Vec<u16> = ref_data[fi*frame_size..fi*frame_size+luma_size].iter().map(|&b| b as u16).collect();
        
        let mut exact = 0u32; let mut max_diff = 0u16; let mut sse = 0u64;
        let mut first_diff = None;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let ov = f.y_plane[y * stride + x];
                let rv = ref_y[y * w as usize + x];
                let d = (ov as i32 - rv as i32).unsigned_abs() as u16;
                if d == 0 { exact += 1; } else {
                    max_diff = max_diff.max(d); sse += d as u64 * d as u64;
                    if first_diff.is_none() { first_diff = Some((x, y, ov, rv)); }
                }
            }
        }
        let mse = sse as f64 / luma_size as f64;
        let psnr = if mse > 0.0 { 10.0 * (255.0*255.0/mse).log10() } else { f64::INFINITY };
        let uninit = f.y_plane.iter().filter(|&&v| v == u16::MAX).count();
        eprint!("  Fr{fi}: PSNR={psnr:6.1}dB exact={:5.1}% max={max_diff:3} uninit={uninit}", 100.0 * exact as f64 / luma_size as f64);
        if let Some((x, y, ov, rv)) = first_diff {
            eprint!(" 1st_diff=({x},{y}) ours={ov} ref={rv}");
        }
        eprintln!();
    }
}

#[test]
#[ignore]
fn trace_deblock_poc4() {
    // Remove stale trace file
    let _ = std::fs::remove_file("/tmp/our_deblock_trace.txt");

    let merge_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/vectors/MERGE_A_TI_3");
    let bit = walkdir(&merge_dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "bit"));
    let bit = match bit { Some(b) => b, None => { eprintln!("SKIP"); return; } };

    let data = std::fs::read(&bit).unwrap();

    // We only want to trace frame POC=4 (the first B-frame)
    // Enable trace before decoding
    heic_decoder::enable_deblock_trace();

    let mut decoder = heic_decoder::VideoDecoder::new(16);
    let frames = decoder.decode_annex_b(&data).unwrap();

    eprintln!("Decoded {} frames", frames.len());
    eprintln!("Trace written to /tmp/our_deblock_trace.txt");

    // Also dump the first 20 diff pixels in frame 4 vs reference
    let ref_path = merge_dir.join("reference_display.yuv");
    if ref_path.exists() {
        let ref_data = std::fs::read(&ref_path).unwrap();
        let w = 416u32; let h = 240u32;
        let luma_size = (w * h) as usize;
        let frame_size = luma_size + 2 * ((w/2) * (h/2)) as usize;

        let fi = 4; // POC=4
        let f = &frames[fi];
        let stride = f.width as usize;
        let ref_y: Vec<u16> = ref_data[fi*frame_size..fi*frame_size+luma_size].iter().map(|&b| b as u16).collect();

        let mut diffs = Vec::new();
        for y in 0..h as usize {
            for x in 0..w as usize {
                let ov = f.y_plane[y * stride + x];
                let rv = ref_y[y * w as usize + x];
                let d = ov as i32 - rv as i32;
                if d != 0 {
                    diffs.push((x, y, ov, rv, d));
                }
            }
        }

        eprintln!("Frame 4: {} diffs out of {}", diffs.len(), luma_size);
        eprintln!("First 40 diff pixels:");
        for (x, y, ov, rv, d) in diffs.iter().take(40) {
            let nearest_v = (*x / 8) * 8;
            let nearest_h = (*y / 8) * 8;
            eprintln!("  ({x:3},{y:3}) ours={ov:3} ref={rv:3} diff={d:+3} near_vert_edge={nearest_v} near_horiz_edge={nearest_h}");
        }
    }
}

#[test]
#[ignore]
fn dump_deblock_flags_poc4() {
    let merge_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/vectors/MERGE_A_TI_3");
    let bit = walkdir(&merge_dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "bit"));
    let bit = match bit { Some(b) => b, None => { eprintln!("SKIP"); return; } };

    let data = std::fs::read(&bit).unwrap();
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    // We need access to frames with deblock_flags intact
    // decode_annex_b returns frames in display order, frame[4] = POC 4
    let frames = decoder.decode_annex_b(&data).unwrap();

    // Examine frame 4 (POC=4)
    let f = &frames[4];
    let stride = f.deblock_stride;

    eprintln!("Frame 4 deblock flags around x=368 (bx=92):");
    eprintln!("  width={} height={} deblock_stride={}", f.width, f.height, stride);
    eprintln!("  deblock_flags len={}", f.deblock_flags.len());

    // Dump flags for bx=92 (x=368), by range [0..60] (y=0..240)
    let bx = 368u32 / 4; // = 92
    for by in 0..60u32 {
        let idx = (by * stride + bx) as usize;
        if idx < f.deblock_flags.len() {
            let flags = f.deblock_flags[idx];
            if flags != 0 {
                let vert = (flags & 1) != 0;  // DEBLOCK_FLAG_VERT
                let horiz = (flags & 2) != 0; // DEBLOCK_FLAG_HORIZ
                let pb_vert = (flags & 4) != 0;  // DEBLOCK_PB_EDGE_VERT
                let pb_horiz = (flags & 8) != 0; // DEBLOCK_PB_EDGE_HORIZ
                eprintln!("  bx={bx} by={by} (x={}, y={}) flags={flags:#04x} TB_V={vert} TB_H={horiz} PB_V={pb_vert} PB_H={pb_horiz}",
                    bx * 4, by * 4);
            }
        }
    }

    // Also check the edge flags for x=304 (where first diff at (302,43) is)
    eprintln!("\nFrame 4 deblock flags around x=304 (bx=76):");
    let bx2 = 304u32 / 4; // = 76
    for by in 8..14u32 {
        let idx = (by * stride + bx2) as usize;
        if idx < f.deblock_flags.len() {
            let flags = f.deblock_flags[idx];
            let vert = (flags & 1) != 0;
            let horiz = (flags & 2) != 0;
            let pb_vert = (flags & 4) != 0;
            let pb_horiz = (flags & 8) != 0;
            eprintln!("  bx={bx2} by={by} (x={}, y={}) flags={flags:#04x} TB_V={vert} TB_H={horiz} PB_V={pb_vert} PB_H={pb_horiz}",
                bx2 * 4, by * 4);
        }
    }
}

#[test]
#[ignore]
fn compare_filter_impact() {
    let merge_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/vectors/MERGE_A_TI_3");
    let bit = walkdir(&merge_dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "bit"));
    let bit = match bit { Some(b) => b, None => { eprintln!("SKIP"); return; } };
    let ref_nofilter_path = Path::new("/tmp/merge_a_nofilter.yuv");
    let ref_filtered_path = merge_dir.join("reference_display.yuv");
    if !ref_nofilter_path.exists() || !ref_filtered_path.exists() { eprintln!("SKIP"); return; }

    let data = std::fs::read(&bit).unwrap();
    let w = 416u32; let h = 240u32;
    let luma_size = (w * h) as usize;
    let frame_size = luma_size + 2 * ((w/2) * (h/2)) as usize;

    // Decode with filters
    let mut dec = heic_decoder::VideoDecoder::new(16);
    let filtered = dec.decode_annex_b(&data).unwrap();

    // Decode without filters
    let mut dec2 = heic_decoder::VideoDecoder::new(16);
    dec2.disable_loop_filters = true;
    let unfiltered = dec2.decode_annex_b(&data).unwrap();

    let ref_nofilter = std::fs::read(ref_nofilter_path).unwrap();
    let ref_filtered = std::fs::read(&ref_filtered_path).unwrap();

    // Compare POC=4 (display index 4)
    let fi = 4;
    let stride = filtered[fi].width as usize;
    let ref_nf: Vec<u16> = ref_nofilter[fi*frame_size..fi*frame_size+luma_size].iter().map(|&b| b as u16).collect();
    let ref_f: Vec<u16> = ref_filtered[fi*frame_size..fi*frame_size+luma_size].iter().map(|&b| b as u16).collect();

    let mut only_unfiltered = 0u32;
    let mut only_filtered = 0u32;
    let mut both = 0u32;
    let mut neither = 0u32;
    let mut filter_introduced = Vec::new();

    for y in 0..h as usize {
        for x in 0..w as usize {
            let uf_ours = unfiltered[fi].y_plane[y * stride + x];
            let uf_ref = ref_nf[y * w as usize + x];
            let f_ours = filtered[fi].y_plane[y * stride + x];
            let f_ref = ref_f[y * w as usize + x];

            let uf_diff = uf_ours != uf_ref;
            let f_diff = f_ours != f_ref;

            match (uf_diff, f_diff) {
                (true, true) => both += 1,
                (true, false) => only_unfiltered += 1,  // filtering fixed an error
                (false, true) => {
                    only_filtered += 1;  // filtering introduced an error
                    if filter_introduced.len() < 30 {
                        filter_introduced.push((x, y,
                            uf_ours, uf_ref, // unfiltered: ours vs ref (should match)
                            f_ours, f_ref,   // filtered: ours vs ref (differ)
                        ));
                    }
                }
                (false, false) => neither += 1,
            }
        }
    }

    eprintln!("POC=4 filter impact analysis:");
    eprintln!("  Both unfiltered and filtered differ: {both}");
    eprintln!("  Only unfiltered differs (filtering fixed): {only_unfiltered}");
    eprintln!("  Only filtered differs (filtering introduced): {only_filtered}");
    eprintln!("  Neither differs: {neither}");
    eprintln!("  Total diff unfiltered: {} ({:.1}%)", both + only_unfiltered, 100.0 * (both + only_unfiltered) as f64 / luma_size as f64);
    eprintln!("  Total diff filtered: {} ({:.1}%)", both + only_filtered, 100.0 * (both + only_filtered) as f64 / luma_size as f64);

    if !filter_introduced.is_empty() {
        eprintln!("\nFirst 30 pixels where filtering introduced error:");
        for (x, y, uf_o, uf_r, f_o, f_r) in &filter_introduced {
            let nearest_vx = (*x / 8) * 8;
            let dist_vx = (*x as i32 - nearest_vx as i32).abs().min((*x as i32 - nearest_vx as i32 - 8).abs());
            let nearest_hy = (*y / 8) * 8;
            let dist_hy = (*y as i32 - nearest_hy as i32).abs().min((*y as i32 - nearest_hy as i32 - 8).abs());
            eprintln!("  ({x:3},{y:3}) unf_ours={uf_o:3} unf_ref={uf_r:3} (match!) filt_ours={f_o:3} filt_ref={f_r:3} diff={} dist_v={dist_vx} dist_h={dist_hy}",
                *f_o as i32 - *f_r as i32);
        }
    }
}

#[test]
#[ignore]
fn dump_bs_derivation_poc4() {
    let merge_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/vectors/MERGE_A_TI_3");
    let bit = walkdir(&merge_dir).into_iter().find(|p| p.extension().is_some_and(|e| e == "bit"));
    let bit = match bit { Some(b) => b, None => { eprintln!("SKIP"); return; } };

    let data = std::fs::read(&bit).unwrap();

    // We need to access inter context data. Let me decode and inspect.
    // Instead of using decode_annex_b, manually decode with access to SliceContext.
    // Actually, let's just use the VideoDecoder and inspect the frame's flags directly.
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    let frames = decoder.decode_annex_b(&data).unwrap();

    // Unfortunately we can't access the inter deblock context from the test.
    // Let me instead add a bs=0 trace to deblock.rs.
    // For now, let's compare the deblock flags between our I-frame and B-frame at x=368

    let f0 = &frames[0]; // I-frame
    let f4 = &frames[4]; // POC=4 B-frame
    let stride = f0.deblock_stride;

    eprintln!("Comparing deblock flags between I-frame and B-frame at x=368:");
    let bx = 92u32; // x=368
    for by in 8..16u32 {
        let idx = (by * stride + bx) as usize;
        let flags0 = f0.deblock_flags[idx];
        let flags4 = f4.deblock_flags[idx];
        let qp0 = f0.qp_map[idx];
        let qp4 = f4.qp_map[idx];

        // Also check QP on P side (bx-1)
        let idx_p = (by * stride + bx - 1) as usize;
        let qp0_p = f0.qp_map[idx_p];
        let qp4_p = f4.qp_map[idx_p];

        eprintln!("  by={by} (y={}) I:flags={flags0:#04x} qp_q={qp0} qp_p={qp0_p} | B:flags={flags4:#04x} qp_q={qp4} qp_p={qp4_p}",
            by * 4);
    }
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
