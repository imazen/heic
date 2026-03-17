//! Helper for bin trace capture
fn main() {
    let path = std::env::var("MERGE_A_BITSTREAM")
        .unwrap_or_else(|_| {
            "/home/lilith/work/zen/heic-decoder-rs/conformance/vectors/MERGE_A_TI_3/MERGE_A_TI_3/MERGE_A_TI_3.bit".to_string()
        });
    let max_bins: u32 = std::env::var("MERGE_A_MAX_BINS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    let data = std::fs::read(&path).unwrap();
    let mut decoder = heic_decoder::VideoDecoder::new(16);
    decoder.mv_trace_next_inter = true;
    heic_decoder::cabac_bin_trace(max_bins);
    let _ = decoder.decode_annex_b(&data);
}
