//! heic#20 bisect: compare the zencodec adapter's `ReconstructHdr` against
//! the raw composition the hdr-corpus-convert v2 tool used (raw decode +
//! `decode_gain_map` + Apple MakerNote params + `apply_gainmap`), stage by
//! stage, to attribute the ±1 PQ16 LSB divergence measured on
//! `1520_…_ip15pro_…heic`.
//!
//! Usage: gm20_probe <file.heic>

use heic::{DecoderConfig, PixelLayout};
use ultrahdr_core::gainmap::apply::apply_gainmap;
use ultrahdr_core::{
    ColorPrimaries, GainMap, HdrOutputFormat, PixelFormat, TransferFunction, Unstoppable,
    from_apple_headroom, parse_exif_for_apple_hdr, pixel_buffer_from_vec,
};
use zencodec::decode::{Decode, DecodeJob, DecoderConfig as _};

fn f32_diff_summary(label: &str, a: &[u8], b: &[u8]) {
    if a.len() != b.len() {
        println!("{label}: LENGTHS differ ({} vs {})", a.len(), b.len());
        return;
    }
    let fa: Vec<f32> = a
        .chunks_exact(4)
        .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let fb: Vec<f32> = b
        .chunks_exact(4)
        .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let n = fa.iter().zip(&fb).filter(|(x, y)| x != y).count();
    let maxd = fa
        .iter()
        .zip(&fb)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    println!(
        "{label}: {n} of {} f32 values differ, max abs {maxd:e}",
        fa.len()
    );
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: gm20_probe <file.heic>");
    let data = std::fs::read(&path).unwrap();

    // ── Raw v2 composition ──────────────────────────────────────────────
    let dec = DecoderConfig::new();
    let prim = dec
        .decode(&data, PixelLayout::Rgb8)
        .expect("raw base decode");
    let (w, h) = (prim.width, prim.height);
    let gm_dec = dec.decode_gain_map(&data).expect("raw gain-map decode");
    let exif = dec
        .extract_exif(&data)
        .ok()
        .flatten()
        .expect("EXIF present");
    let info = parse_exif_for_apple_hdr(&exif).expect("Apple HDR maker tags");
    let params = from_apple_headroom(&info).expect("headroom params");
    let stops = params.alternate_hdr_headroom;
    let boost_v2 = 2.0f32.powf(stops as f32);
    let boost_adapter_route = params.linear_alternate_headroom() as f32;
    println!("stops (f64 log2): {stops:?}  bits={:016x}", stops.to_bits());
    println!(
        "boost  powf(2, stops as f32) = {boost_v2:?} bits={:08x}",
        boost_v2.to_bits()
    );
    println!(
        "boost  linear_alt_headroom() as f32 = {boost_adapter_route:?} bits={:08x}  routes_equal={}",
        boost_adapter_route.to_bits(),
        boost_v2.to_bits() == boost_adapter_route.to_bits()
    );

    let gm = GainMap {
        width: gm_dec.width,
        height: gm_dec.height,
        channels: 1,
        data: gm_dec.data,
    };
    let make_sdr = || {
        pixel_buffer_from_vec(
            prim.data.clone(),
            w,
            h,
            PixelFormat::Rgb8,
            ColorPrimaries::DisplayP3, // v2 resolved Apple unspecified+ICC -> P3
            TransferFunction::Srgb,
        )
        .expect("sdr buffer")
    };
    let raw_v2 = apply_gainmap(
        &make_sdr(),
        &gm,
        &params,
        boost_v2,
        HdrOutputFormat::LinearFloat,
        Unstoppable,
    )
    .expect("raw apply (v2 boost)");
    let raw_adapter_boost = apply_gainmap(
        &make_sdr(),
        &gm,
        &params,
        boost_adapter_route.max(1.0),
        HdrOutputFormat::LinearFloat,
        Unstoppable,
    )
    .expect("raw apply (adapter boost)");

    // ── Adapter ReconstructHdr ──────────────────────────────────────────
    let out = heic::HeicDecoderConfig::new()
        .job()
        .with_orientation(zencodec::OrientationHint::Correct)
        .with_gain_map_render(zencodec::GainMapRender::ReconstructHdr {
            target_headroom: None,
        })
        .decoder(data.as_slice().into(), &[])
        .expect("adapter decoder")
        .decode()
        .expect("adapter ReconstructHdr");
    println!(
        "adapter out: {}x{} {:?}",
        out.pixels().width(),
        out.pixels().rows(),
        out.pixels().descriptor().pixel_format()
    );

    let adapter_bytes = out.pixels().contiguous_bytes();
    f32_diff_summary(
        "adapter vs raw(v2 boost)        ",
        &adapter_bytes,
        raw_v2.as_slice().as_strided_bytes(),
    );
    f32_diff_summary(
        "adapter vs raw(adapter boost)   ",
        &adapter_bytes,
        raw_adapter_boost.as_slice().as_strided_bytes(),
    );
    f32_diff_summary(
        "raw(v2 boost) vs raw(adapter boost)",
        raw_v2.as_slice().as_strided_bytes(),
        raw_adapter_boost.as_slice().as_strided_bytes(),
    );
}
