# heic-decoder

[![Build Status](https://github.com/imazen/heic-decoder-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/imazen/heic-decoder-rs/actions)
[![Crates.io](https://img.shields.io/crates/v/heic-decoder.svg)](https://crates.io/crates/heic-decoder)
[![Documentation](https://docs.rs/heic-decoder/badge.svg)](https://docs.rs/heic-decoder)
[![License](https://img.shields.io/crates/l/heic-decoder.svg)](LICENSE)

Pure Rust HEIC/HEIF image decoder. No C/C++ dependencies, no unsafe code.

- `#![forbid(unsafe_code)]` — zero unsafe blocks
- `no_std + alloc` compatible (works on wasm32)
- AVX2 SIMD acceleration with automatic scalar fallback
- Decodes 1280x854 in ~57ms on x86-64

## Status

**Work in Progress** — decodes most HEIC files, but not yet production-ready.

104 of 162 test files decode successfully. SSIM scores range from 50-77 dB against libheif reference output.

### What works
- HEIF container parsing (ISOBMFF boxes, grid images, overlays)
- Full HEVC I-frame decoding (VPS/SPS/PPS, CABAC, intra prediction, transforms)
- Deblocking filter and SAO (Sample Adaptive Offset)
- YCbCr→RGB with BT.601/BT.709/BT.2020 matrices (full + limited range)
- 10-bit HEVC (transparent downconvert to 8-bit output)
- Alpha plane decoding, HDR gain map extraction
- EXIF/XMP metadata extraction (zero-copy)
- Thumbnail decode, image rotation/mirror transforms
- HEVC scaling lists (custom dequantization matrices)
- AVX2 SIMD for color conversion and IDCT 8x8/16x16

### Known limitations
- I-slices only (sufficient for HEIC still images, no inter prediction)
- 4:4:4 chroma partially supported

## Usage

```rust
use heic_decoder::{DecoderConfig, PixelLayout};

let data = std::fs::read("image.heic")?;
let output = DecoderConfig::new().decode(&data, PixelLayout::Rgba8)?;
println!("{}x{} image, {} bytes", output.width, output.height, output.data.len());
```

### Full control with limits and cancellation

```rust
use heic_decoder::{DecoderConfig, PixelLayout, Limits};

let limits = Limits {
    max_width: Some(8192),
    max_height: Some(8192),
    max_pixels: Some(64_000_000),
    max_memory_bytes: Some(512 * 1024 * 1024),
};

let output = DecoderConfig::new()
    .decode_request(&data)
    .with_output_layout(PixelLayout::Rgba8)
    .with_limits(&limits)
    .decode()?;
```

### Probe without decoding

```rust
use heic_decoder::ImageInfo;

let info = ImageInfo::from_bytes(&data)?;
println!("{}x{}, alpha={}, exif={}", info.width, info.height, info.has_alpha, info.has_exif);
```

### Zero-copy into pre-allocated buffer

```rust
let info = ImageInfo::from_bytes(&data)?;
let mut buf = vec![0u8; info.output_buffer_size(PixelLayout::Rgba8).unwrap()];
DecoderConfig::new()
    .decode_request(&data)
    .with_output_layout(PixelLayout::Rgba8)
    .decode_into(&mut buf)?;
```

### Metadata extraction

```rust
let decoder = DecoderConfig::new();
let exif: Option<&[u8]> = decoder.extract_exif(&data)?;   // raw TIFF bytes
let xmp: Option<&[u8]> = decoder.extract_xmp(&data)?;     // raw XML bytes
let thumb = decoder.decode_thumbnail(&data, PixelLayout::Rgb8)?; // smaller preview
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## AI-Generated Code Notice

Developed with Claude (Anthropic). Not all code manually reviewed. Review critical paths before production use.
