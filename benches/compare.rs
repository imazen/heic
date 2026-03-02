// Head-to-head benchmark: pure Rust heic-decoder vs WASM-sandboxed libheif
//
// Compares decode, probe, and EXIF extraction across both implementations.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::path::Path;

const EXAMPLE_HEIC: &str = "/home/lilith/work/heic/libheif/examples/example.heic";
const IPHONE_HEIC: &str = "/home/lilith/work/heic/test-images/classic-car-iphone12pro.heic";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("wasm-module")
        .join("heic_decoder.wasm")
}

fn bench_decode_small(c: &mut Criterion) {
    let data = std::fs::read(EXAMPLE_HEIC).expect("read example.heic");
    let wasm = wasm_path();

    let mut group = c.benchmark_group("decode_1280x854");
    group.throughput(Throughput::Bytes(data.len() as u64));

    // Pure Rust
    let rust_dec = heic_decoder::DecoderConfig::new();
    group.bench_function("rust", |b| {
        b.iter(|| rust_dec.decode(&data, heic_decoder::PixelLayout::Rgb8).unwrap());
    });

    // WASM libheif
    if wasm.exists() {
        let wasm_dec = heic_wasm_rs::HeicDecoder::from_file(&wasm).unwrap();
        group.bench_function("wasm_libheif", |b| {
            b.iter(|| wasm_dec.decode(&data).unwrap());
        });
    } else {
        eprintln!("WASM module not found, skipping wasm_libheif variants");
    }

    group.finish();
}

fn bench_decode_large(c: &mut Criterion) {
    let path = Path::new(IPHONE_HEIC);
    if !path.exists() {
        eprintln!("iPhone test image not found, skipping decode_3024x4032");
        return;
    }
    let data = std::fs::read(path).expect("read iPhone HEIC");
    let wasm = wasm_path();

    let mut group = c.benchmark_group("decode_3024x4032");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.sample_size(10);

    // Pure Rust
    let rust_dec = heic_decoder::DecoderConfig::new();
    group.bench_function("rust", |b| {
        b.iter(|| rust_dec.decode(&data, heic_decoder::PixelLayout::Rgb8).unwrap());
    });

    // WASM libheif
    if wasm.exists() {
        let wasm_dec = heic_wasm_rs::HeicDecoder::from_file(&wasm).unwrap();
        group.bench_function("wasm_libheif", |b| {
            b.iter(|| wasm_dec.decode(&data).unwrap());
        });
    }

    group.finish();
}

fn bench_probe(c: &mut Criterion) {
    let data = std::fs::read(EXAMPLE_HEIC).expect("read example.heic");
    let wasm = wasm_path();

    let mut group = c.benchmark_group("probe_1280x854");
    group.throughput(Throughput::Bytes(data.len() as u64));

    // Pure Rust (no decoder init needed)
    group.bench_function("rust", |b| {
        b.iter(|| heic_decoder::ImageInfo::from_bytes(&data).unwrap());
    });

    // WASM libheif
    if wasm.exists() {
        let wasm_dec = heic_wasm_rs::HeicDecoder::from_file(&wasm).unwrap();
        group.bench_function("wasm_libheif", |b| {
            b.iter(|| wasm_dec.get_info(&data).unwrap());
        });
    }

    group.finish();
}

fn bench_exif(c: &mut Criterion) {
    let path = Path::new(IPHONE_HEIC);
    if !path.exists() {
        eprintln!("iPhone test image not found, skipping exif_extract");
        return;
    }
    let data = std::fs::read(path).expect("read iPhone HEIC");
    let wasm = wasm_path();

    let mut group = c.benchmark_group("exif_extract");
    group.throughput(Throughput::Bytes(data.len() as u64));

    // Pure Rust
    let rust_dec = heic_decoder::DecoderConfig::new();
    group.bench_function("rust", |b| {
        b.iter(|| rust_dec.extract_exif(&data).unwrap());
    });

    // WASM libheif
    if wasm.exists() {
        let wasm_dec = heic_wasm_rs::HeicDecoder::from_file(&wasm).unwrap();
        group.bench_function("wasm_libheif", |b| {
            b.iter(|| wasm_dec.get_exif(&data).unwrap());
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_decode_small,
    bench_decode_large,
    bench_probe,
    bench_exif,
);
criterion_main!(benches);
