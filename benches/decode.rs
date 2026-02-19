// Criterion benchmarks for HEIC decode performance

use criterion::{Criterion, criterion_group, criterion_main};
use heic_decoder::{DecoderConfig, ImageInfo, PixelLayout};

const EXAMPLE_HEIC: &str = "/home/lilith/work/heic/libheif/examples/example.heic";
const IPHONE_HEIC: &str = "/home/lilith/work/heic/test-images/classic-car-iphone12pro.heic";

fn bench_decode_rgb(c: &mut Criterion) {
    let data = std::fs::read(EXAMPLE_HEIC).expect("read test file");
    let decoder = DecoderConfig::new();

    c.bench_function("decode_rgb_1280x854", |b| {
        b.iter(|| decoder.decode(&data, PixelLayout::Rgb8).unwrap())
    });
}

fn bench_decode_rgba(c: &mut Criterion) {
    let data = std::fs::read(EXAMPLE_HEIC).expect("read test file");
    let decoder = DecoderConfig::new();

    c.bench_function("decode_rgba_1280x854", |b| {
        b.iter(|| decoder.decode(&data, PixelLayout::Rgba8).unwrap())
    });
}

fn bench_decode_to_frame(c: &mut Criterion) {
    let data = std::fs::read(EXAMPLE_HEIC).expect("read test file");
    let decoder = DecoderConfig::new();

    c.bench_function("decode_to_frame_1280x854", |b| {
        b.iter(|| decoder.decode_to_frame(&data).unwrap())
    });
}

fn bench_probe(c: &mut Criterion) {
    let data = std::fs::read(EXAMPLE_HEIC).expect("read test file");

    c.bench_function("probe_1280x854", |b| {
        b.iter(|| ImageInfo::from_bytes(&data).unwrap())
    });
}

fn bench_extract_exif(c: &mut Criterion) {
    let data = std::fs::read(IPHONE_HEIC).expect("read test file");
    let decoder = DecoderConfig::new();

    c.bench_function("extract_exif", |b| {
        b.iter(|| decoder.extract_exif(&data).unwrap())
    });
}

fn bench_decode_thumbnail(c: &mut Criterion) {
    let data = std::fs::read(EXAMPLE_HEIC).expect("read test file");
    let decoder = DecoderConfig::new();

    c.bench_function("decode_thumbnail_320x212", |b| {
        b.iter(|| decoder.decode_thumbnail(&data, PixelLayout::Rgb8).unwrap())
    });
}

fn bench_decode_iphone(c: &mut Criterion) {
    let path = std::path::Path::new(IPHONE_HEIC);
    if !path.exists() {
        return;
    }
    let data = std::fs::read(IPHONE_HEIC).expect("read test file");
    let decoder = DecoderConfig::new();

    c.bench_function("decode_rgb_3024x4032", |b| {
        b.iter(|| decoder.decode(&data, PixelLayout::Rgb8).unwrap())
    });
}

criterion_group!(
    benches,
    bench_decode_rgb,
    bench_decode_rgba,
    bench_decode_to_frame,
    bench_probe,
    bench_extract_exif,
    bench_decode_thumbnail,
    bench_decode_iphone,
);
criterion_main!(benches);
