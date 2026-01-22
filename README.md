# heic-decoder

[![Build Status](https://github.com/imazen/heic-decoder-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/imazen/heic-decoder-rs/actions)
[![Crates.io](https://img.shields.io/crates/v/heic-decoder.svg)](https://crates.io/crates/heic-decoder)
[![Documentation](https://docs.rs/heic-decoder/badge.svg)](https://docs.rs/heic-decoder)
[![License](https://img.shields.io/crates/l/heic-decoder.svg)](LICENSE)

Pure Rust HEIC/HEIF image decoder. No C/C++ dependencies.

## Status

**Work in Progress** - Not yet ready for production use.

### Completed
- HEIF container parsing (ISOBMFF boxes)
- NAL unit parsing
- VPS/SPS/PPS parameter set parsing
- Slice header parsing
- CTU/CU quad-tree decoding
- Intra prediction (35 modes)
- Inverse DCT/DST transforms
- CABAC decoder with adaptive Golomb-Rice
- Frame buffer with YCbCr→RGB conversion

### In Progress
- Chroma plane accuracy improvements
- Sub-block scan tables for 16x16 and 32x32 TUs
- Conformance window cropping

### Not Yet Implemented
- Deblocking filter
- SAO (Sample Adaptive Offset)
- Inter prediction (P/B slices) - only I-slices needed for HEIC
- SIMD optimization

## Usage

```rust
use heic_decoder::decode;

let heic_data = std::fs::read("image.heic")?;
let image = decode(&heic_data)?;

println!("{}x{}", image.width, image.height);
// Access RGB pixel data via image.data
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## AI-Generated Code Notice

Developed with Claude (Anthropic). Not all code manually reviewed. Review critical paths before production use.
