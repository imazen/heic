//! Microbenchmark for `parse_annexb` (exposed via `hevc::decode` error path).
//!
//! Reads a HEIC, extracts NALs, reassembles as Annex-B, then times
//! `hevc::bitstream::parse_nal_units` (Annex-B branch) across many iterations.
//!
//! Usage: cargo run --release --example bench_parse_annexb -- <path.heic>

use std::time::Instant;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/mnt/v/heic/22BB086F-C17C-4A9D-897B-4AE72FB2B9A0.HEIC".to_string());
    let heic_data = std::fs::read(&path).expect("read heic");
    let container = heic::heif::parse(&heic_data, &heic::Unstoppable).expect("parse heic");
    // Find any item that has hvcC config (grid items don't; their tiles do).
    let mut config = None;
    let mut image_data_vec: Vec<u8> = Vec::new();
    for info in &container.item_infos {
        let Some(item) = container.get_item(info.item_id) else {
            continue;
        };
        if let Some(cfg) = item.hevc_config.as_ref() {
            let data = container.get_item_data(item.id).expect("item data");
            config = Some(cfg.clone());
            image_data_vec = data.into_owned();
            eprintln!(
                "Using item id={} type={:?} size={}",
                item.id,
                item.item_type,
                image_data_vec.len()
            );
            break;
        }
    }
    let config = config.expect("no item with hvcC in container");
    let image_data = &image_data_vec[..];

    // Assemble Annex-B buffer (parameter sets + slice NALs).
    let mut annexb: Vec<u8> = Vec::new();
    for nal_data in &config.nal_units {
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(nal_data);
    }
    let length_size = (config.length_size_minus_one + 1) as usize;
    let mut pos = 0usize;
    while pos + length_size <= image_data.len() {
        let mut nal_len = 0u32;
        for i in 0..length_size {
            nal_len = (nal_len << 8) | image_data[pos + i] as u32;
        }
        pos += length_size;
        let nal_len = nal_len as usize;
        if pos + nal_len > image_data.len() {
            break;
        }
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&image_data[pos..pos + nal_len]);
        pos += nal_len;
    }

    eprintln!("Annex-B stream: {} bytes", annexb.len());

    // Use the public path: hevc::decode calls parse_nal_units → parse_annexb.
    // parse_annexb alone isn't pub; we time the parse step by stopping at info.
    // Easier: call parse_nal_units via a tiny helper in the crate? It's pub in
    // `hevc::bitstream`, but that module is not re-exported publicly. Route via
    // `get_info` which parses NALs and reads the SPS — dominated by parse for
    // large streams. For a clean measurement, just run the whole info probe.

    // Warmup
    for _ in 0..20 {
        let _ = heic::hevc::get_info(&annexb);
    }

    let iters = 500u32;
    let t0 = Instant::now();
    let mut sink = 0usize;
    for _ in 0..iters {
        let info = heic::hevc::get_info(&annexb).expect("get_info");
        sink = sink.wrapping_add(info.width as usize);
    }
    let elapsed = t0.elapsed();
    let per = elapsed / iters;
    println!(
        "get_info (parses Annex-B NALs + parses SPS) over {} bytes: {:?}/iter ({} iters total {:?}), sink={}",
        annexb.len(),
        per,
        iters,
        elapsed,
        sink
    );
    let throughput_gib_s =
        (annexb.len() as f64 * iters as f64) / elapsed.as_secs_f64() / (1024.0 * 1024.0 * 1024.0);
    println!("Throughput: {:.2} GiB/s", throughput_gib_s);
}
