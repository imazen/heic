//! Print gain-map provenance + ISO 21496-1 parameters for HEIC files.
//!
//! Shows which mechanism produced the gain map (`HeifTmap` vs `AppleAuxItem`),
//! the decoded gain-map geometry, and — when the file carries ISO 21496-1
//! binary metadata (HEIF Amendment 1 `tmap`) — the parsed parameters and
//! direction. Useful for triaging real iPhone (iOS 18+) / Samsung HDR files.
//!
//! With `--reconstruct`, also runs the zencodec adapter's
//! `GainMapRender::ReconstructHdr` path and prints the output descriptor,
//! peak linear value, and derived content-light-level envelope.
//!
//! Usage:
//!   cargo run --example gain_map_info --features backend-rust,zencodec -- [--reconstruct] <file>...

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let reconstruct = args.first().is_some_and(|a| a == "--reconstruct");
    if reconstruct {
        args.remove(0);
    }
    if args.is_empty() {
        eprintln!("usage: gain_map_info [--reconstruct] <file.heic>...");
        std::process::exit(2);
    }
    for path in &args {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                println!("{path}: read failed: {e}");
                continue;
            }
        };
        match heic::DecoderConfig::new().decode_gain_map(&data) {
            Ok(gm) => {
                println!(
                    "{path}: origin={:?} {}x{} bit_depth={} xmp={}B iso21496={}B",
                    gm.origin,
                    gm.width,
                    gm.height,
                    gm.bit_depth,
                    gm.xmp.as_ref().map_or(0, |x| x.len()),
                    gm.iso21496.as_ref().map_or(0, |x| x.len()),
                );
                if let Some(iso) = &gm.iso21496 {
                    match zencodec::gainmap::parse_iso21496_fmt(
                        iso,
                        zencodec::gainmap::Iso21496Format::AvifTmap,
                    ) {
                        Ok(p) => println!(
                            "  iso: base_hr={:.4} alt_hr={:.4} ch0[min={:.4} max={:.4} gamma={:.4}] use_base_cs={} dir={:?}",
                            p.base_hdr_headroom,
                            p.alternate_hdr_headroom,
                            p.channels[0].min,
                            p.channels[0].max,
                            p.channels[0].gamma,
                            p.use_base_color_space,
                            p.direction(),
                        ),
                        Err(e) => println!("  iso parse FAILED: {e:?}"),
                    }
                }
            }
            Err(e) => println!("{path}: decode_gain_map failed: {e}"),
        }
        if reconstruct {
            reconstruct_hdr(&data);
        }
    }
}

/// Decode via the zencodec adapter with `ReconstructHdr` and report the
/// HDR rendition's descriptor, peak linear value, and envelope.
fn reconstruct_hdr(data: &[u8]) {
    use zencodec::decode::{Decode as _, DecodeJob as _, DecoderConfig as _};
    let result = heic::HeicDecoderConfig::new()
        .job()
        .with_gain_map_render(zencodec::GainMapRender::ReconstructHdr {
            target_headroom: None,
        })
        .decoder(std::borrow::Cow::Borrowed(data), &[])
        .and_then(|d| d.decode());
    match result {
        Ok(out) => {
            let desc = out.pixels().descriptor();
            let max = out
                .pixels()
                .contiguous_bytes()
                .chunks_exact(4)
                .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .fold(0.0f32, f32::max);
            let cll = out.info().source_color.content_light_level;
            println!(
                "  reconstruct: {}x{} {:?}/{:?} max_linear={max:.3} cll={:?}",
                out.width(),
                out.height(),
                desc.channel_type(),
                desc.transfer(),
                cll.map(|c| c.max_content_light_level),
            );
        }
        Err(e) => println!("  reconstruct FAILED: {e}"),
    }
}
