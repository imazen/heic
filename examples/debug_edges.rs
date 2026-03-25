fn heic_base_dir() -> String {
    std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into())
}

fn main() {
    let data =
        std::fs::read(format!("{}/libheif/examples/example.heic", heic_base_dir())).expect("read");
    let decoder = heic::DecoderConfig::new();
    let frame = decoder.decode_to_frame(&data).expect("decode");

    let stride = frame.deblock_stride;
    let h4 = frame.height / 4;
    let w4 = frame.width / 4;

    let mut vert_count = 0;
    let mut horiz_count = 0;

    for by in 0..h4 {
        for bx in 0..w4 {
            let idx = (by * stride + bx) as usize;
            if idx < frame.deblock_flags.len() {
                if (frame.deblock_flags[idx] & 1) != 0 {
                    vert_count += 1;
                }
                if (frame.deblock_flags[idx] & 2) != 0 {
                    horiz_count += 1;
                }
            }
        }
    }

    eprintln!("Frame: {}x{}", frame.width, frame.height);
    eprintln!("Deblock grid: {}x{}", w4, h4);
    eprintln!("Vertical edge flags: {}", vert_count);
    eprintln!("Horizontal edge flags: {}", horiz_count);

    // Show edge positions for first few rows
    for by in 0..4 {
        let mut vpos = Vec::new();
        for bx in 0..w4 {
            let idx = (by * stride + bx) as usize;
            if idx < frame.deblock_flags.len() && (frame.deblock_flags[idx] & 1) != 0 {
                vpos.push(bx * 4);
            }
        }
        eprintln!("  y={}: vert edges at x={:?}", by * 4, vpos);
    }

    for bx in 0..4 {
        let mut hpos = Vec::new();
        for by in 0..h4 {
            let idx = (by * stride + bx) as usize;
            if idx < frame.deblock_flags.len() && (frame.deblock_flags[idx] & 2) != 0 {
                hpos.push(by * 4);
            }
        }
        eprintln!("  x={}: horiz edges at y={:?}", bx * 4, hpos);
    }

    // Count QP values
    let mut qp_hist = [0u32; 52];
    for qp in &frame.qp_map {
        if *qp >= 0 && (*qp as usize) < 52 {
            qp_hist[*qp as usize] += 1;
        }
    }
    eprintln!("\nQP histogram (non-zero):");
    for (qp, count) in qp_hist.iter().enumerate() {
        if *count > 0 {
            eprintln!("  QP {}: {} blocks", qp, count);
        }
    }
}
