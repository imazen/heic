fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/lilith/work/heic/test-images/example_q10.heic".to_string());
    let data = std::fs::read(&path).expect("read");
    let decoder = heic_decoder::DecoderConfig::new();
    let frame = decoder.decode_to_frame(&data).expect("decode");
    
    eprintln!("QP map stride: {}", frame.deblock_stride);
    
    // Edge at x=96 (bx=24), y=0 (by=0)
    for by in 0..4u32 {
        for bx in 20..30u32 {
            let idx = (by * frame.deblock_stride + bx) as usize;
            if idx < frame.qp_map.len() {
                let qp = frame.qp_map[idx];
                let flags = frame.deblock_flags[idx];
                let v = if flags & 1 != 0 { "V" } else { "." };
                let h = if flags & 2 != 0 { "H" } else { "." };
                eprint!("  qp={:3}{}{}", qp, v, h);
            }
        }
        eprintln!();
    }
    
    let bx_edge = 96u32 / 4;
    let idx_q = (0 * frame.deblock_stride + bx_edge) as usize;
    let idx_p = (0 * frame.deblock_stride + bx_edge - 1) as usize;
    
    eprintln!();
    eprintln!("Edge at x=96, y=0:");
    eprintln!("  P side (bx={}): qp={}", bx_edge-1, frame.qp_map[idx_p]);
    eprintln!("  Q side (bx={}): qp={}", bx_edge, frame.qp_map[idx_q]);
    let qp_l = (frame.qp_map[idx_p] as i32 + frame.qp_map[idx_q] as i32 + 1) >> 1;
    eprintln!("  qp_l = {}", qp_l);
}
