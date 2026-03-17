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

    // Dec265 nofilter is in decode order: I(0), B(4), B(2), B(1), B(3), B(6), B(5), B(7)
    let decode_order_pocs = [0i32, 4, 2, 1, 3, 6, 5, 7];

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

    // Compare B-frame POC=4 (our frames[4] vs dec265 decode index 1)
    if frames.len() > 4 && ref_data.len() >= 2 * frame_size {
        let ref_f4: Vec<u16> = ref_data[1*frame_size..1*frame_size+luma_size].iter().map(|&b| b as u16).collect();
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
