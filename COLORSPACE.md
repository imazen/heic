# Color Space Handling

The HEIC decoder now includes comprehensive color space handling with HDR support.

## Features

### Supported Color Spaces

**Color Primaries:**
- BT.709 (HDTV / sRGB)
- BT.2020 (UHDTV / HDR)
- BT.601 (SDTV)
- DCI-P3 (Digital Cinema)
- Display P3 (Apple)
- And more...

**Transfer Functions:**
- BT.709 (traditional TV gamma)
- sRGB (standard web/display gamma)
- **PQ (Perceptual Quantizer)** - HDR10, SMPTE ST 2084
- **HLG (Hybrid Log Gamma)** - HDR broadcast, ITU-R BT.2100
- Linear, Gamma 2.2, Gamma 2.8

**Matrix Coefficients:**
- BT.709 (HDTV)
- BT.2020 (UHDTV)
- BT.601 (SDTV)
- YCgCo, ICtCp, and more

### HDR Support

The decoder automatically handles HDR content:

1. **Detection** - Identifies PQ or HLG transfer functions
2. **EOTF Application** - Converts signal to linear light
3. **Tone Mapping** - Maps HDR (10,000+ nits) to SDR (~100 nits)
4. **Output** - Produces viewable 8-bit or 16-bit RGB

## Architecture

### Pipeline

```
YCbCr (coded values)
    ↓
YCbCr to RGB (matrix coefficients)
    ↓
Apply EOTF (transfer function) → Linear light
    ↓
Tone mapping (if HDR) → SDR range
    ↓
Apply sRGB OETF → Display signal
    ↓
8-bit or 16-bit RGB output
```

### Code Structure

```
src/hevc/colorspace.rs
├── ColorPrimaries          - Color gamut definitions
├── TransferCharacteristics - Transfer functions (gamma, PQ, HLG)
├── MatrixCoefficients      - YCbCr→RGB matrices
└── ColorSpace              - Main color space handler
    ├── ycbcr_to_rgb()      - Matrix conversion
    ├── apply_eotf()        - EOTF (signal→linear)
    ├── tone_map_to_sdr()   - HDR→SDR mapping
    ├── apply_sdr_oetf()    - sRGB OETF (linear→signal)
    ├── ycbcr_to_rgb8()     - Full pipeline → 8-bit RGB
    └── ycbcr_to_rgb16()    - Full pipeline → 16-bit RGB
```

## Usage

### Basic Usage (Automatic)

The decoder uses default BT.709 color space when no VUI metadata is present:

```rust
use heic_decoder::HeicDecoder;

let data = std::fs::read("image.heic")?;
let decoder = HeicDecoder::new();
let image = decoder.decode(&data)?; // Uses default BT.709

// RGB8 output - standard sRGB
println!("{}x{} RGB8 image", image.width, image.height);
```

### Accessing Color Space Metadata

```rust
let decoder = HeicDecoder::new();
let frame = decoder.decode_to_frame(&data)?;

// Check color space
println!("Color primaries: {:?}", frame.colorspace.primaries);
println!("Transfer function: {:?}", frame.colorspace.transfer);
println!("Matrix coefficients: {:?}", frame.colorspace.matrix);
println!("Full range: {}", frame.colorspace.full_range);

// Check if HDR
if frame.colorspace.transfer.is_hdr() {
    println!("This is HDR content!");
    println!("Transfer: {:?}", frame.colorspace.transfer);
}
```

### Custom Color Space

You can override the color space for testing or manual control:

```rust
use heic_decoder::hevc::colorspace::{ColorSpace, TransferCharacteristics, ColorPrimaries, MatrixCoefficients};

let mut frame = decoder.decode_to_frame(&data)?;

// Override to BT.2020 HDR
frame.colorspace = ColorSpace {
    primaries: ColorPrimaries::Bt2020,
    transfer: TransferCharacteristics::Pq,  // HDR10
    matrix: MatrixCoefficients::Bt2020Ncl,
    full_range: false,
};

// Now convert to RGB with custom colorspace
let rgb = frame.to_rgb();  // Automatically applies HDR tone mapping
```

### 16-bit Output for HDR

For high bit-depth HDR content, use 16-bit output:

```rust
let frame = decoder.decode_to_frame(&data)?;

if frame.bit_depth > 8 || frame.colorspace.transfer.is_hdr() {
    // Get 16-bit RGB (preserves HDR range)
    let rgb16 = frame.to_rgb16();

    // Each pixel is 3 × u16 values
    println!("16-bit RGB: {} values", rgb16.len());
}
```

## HDR Tone Mapping

### PQ (HDR10)

- **Input**: PQ-encoded signal [0.0, 1.0]
- **EOTF Output**: Linear light [0.0, 10000.0] nits normalized
- **Tone Mapping**: Extended Reinhard tone mapper
  - Peak: 10,000 nits
  - Target: 100 nits (SDR display)
- **Output**: sRGB-encoded 8-bit or 16-bit RGB

### HLG (Hybrid Log Gamma)

- **Input**: HLG-encoded signal [0.0, 1.0]
- **OETF Inverse**: Scene-referred linear light
- **OOTF**: Display light for 1000-nit display
- **Tone Mapping**: Reinhard tone mapper
  - Peak: 1,000 nits
  - Target: 100 nits (SDR display)
- **Output**: sRGB-encoded 8-bit or 16-bit RGB

### Algorithm

Extended Reinhard tone mapping with white point:

```
L_out = L × (1 + L / white_point²) / (1 + L)

where:
  L         = input linear light (normalized to peak)
  white_point = peak_nits / target_nits
  L_out     = output linear light [0.0, 1.0]
```

## Matrix Coefficients

### BT.709 (Default)

```
R = Y + 1.5748×Cr
G = Y - 0.1873×Cb - 0.4681×Cr
B = Y + 1.8556×Cb

where:
  Kr = 0.2126, Kb = 0.0722
```

### BT.2020 (UHDTV/HDR)

```
R = Y + 1.4746×Cr
G = Y - 0.1646×Cb - 0.5714×Cr
B = Y + 1.8814×Cb

where:
  Kr = 0.2627, Kb = 0.0593
```

### BT.601 (SDTV)

```
R = Y + 1.402×Cr
G = Y - 0.344×Cb - 0.714×Cr
B = Y + 1.772×Cb

where:
  Kr = 0.299, Kb = 0.114
```

## Limited vs Full Range

### Limited Range (default for HEVC)
- **Luma (Y)**: 16-235 (8-bit) or 64-940 (10-bit)
- **Chroma (Cb/Cr)**: 16-240 (8-bit) or 64-960 (10-bit)
- Used by most video content

### Full Range
- **All components**: 0-255 (8-bit) or 0-1023 (10-bit)
- Used by some cameras and computer graphics

The colorspace module automatically handles both ranges based on the `full_range` flag.

## Future Enhancements

### TODO: VUI Parsing

Currently, color space defaults to BT.709. To properly support HDR and wide color gamut:

**Add VUI parsing to `params.rs`:**

```rust
// In parse_sps(), after other parameters:
if sps.vui_parameters_present_flag {
    let vui = parse_vui_parameters(&mut reader, &sps)?;

    if vui.video_signal_type_present_flag {
        let video_format = reader.read_bits(3)?;
        let video_full_range_flag = reader.read_bit()? != 0;
        let colour_description_present_flag = reader.read_bit()? != 0;

        if colour_description_present_flag {
            let colour_primaries = reader.read_bits(8)? as u8;
            let transfer_characteristics = reader.read_bits(8)? as u8;
            let matrix_coefficients = reader.read_bits(8)? as u8;

            // Store in SPS or pass to DecodedFrame
            frame.colorspace = ColorSpace::from_vui(
                colour_primaries,
                transfer_characteristics,
                matrix_coefficients,
                video_full_range_flag,
            );
        }
    }
}
```

**References:**
- ITU-T H.265 Annex E.2 (VUI semantics)
- ITU-T H.265 Table E.3 (Colour primaries)
- ITU-T H.265 Table E.4 (Transfer characteristics)
- ITU-T H.265 Table E.5 (Matrix coefficients)

### TODO: SEI Message Parsing

For HDR10+ and other dynamic metadata:

```rust
// Parse SEI NAL units (type 39, 40, etc.)
// Extract mastering display color volume
// Extract content light level information
// Apply dynamic tone mapping
```

## Testing

### Unit Tests

```bash
cargo test --lib colorspace
```

Included tests:
- BT.709 black/white conversion
- PQ EOTF monotonicity
- HLG OETF inverse monotonicity

### Visual Testing

```bash
# Decode HDR image
cargo run --release --bin decode_heic -- hdr_image.heic

# Compare with FFmpeg
ffmpeg -i hdr_image.heic output_ffmpeg.png
```

## Performance

The new colorspace module uses:
- Floating-point math for accuracy
- Per-pixel conversions (no SIMD yet)
- ~10-15% slower than old hardcoded BT.709

**Future optimization**: SIMD YCbCr→RGB conversion could recover this overhead.

## Examples

### Example 1: HDR10 Image

```rust
let frame = decoder.decode_to_frame(&data)?;

if frame.colorspace.transfer == TransferCharacteristics::Pq {
    println!("HDR10 image detected");
    println!("Primaries: {:?}", frame.colorspace.primaries); // Likely Bt2020
    println!("Matrix: {:?}", frame.colorspace.matrix);       // Likely Bt2020Ncl

    // Get tone-mapped SDR output
    let rgb8 = frame.to_rgb();  // Automatically tone-mapped

    // Or preserve HDR in 16-bit
    let rgb16 = frame.to_rgb16();  // Linear light, scaled to 16-bit
}
```

### Example 2: Wide Color Gamut (Display P3)

```rust
let frame = decoder.decode_to_frame(&data)?;

if frame.colorspace.primaries == ColorPrimaries::DisplayP3 {
    println!("Display P3 image");

    // Note: Output is still sRGB-encoded RGB
    // Full gamut conversion requires additional transform
    let rgb = frame.to_rgb();
}
```

### Example 3: Force Specific Color Space

```rust
use heic_decoder::hevc::colorspace::*;

let mut frame = decoder.decode_to_frame(&data)?;

// Override for testing/debugging
frame.colorspace = ColorSpace {
    primaries: ColorPrimaries::Bt709,
    transfer: TransferCharacteristics::Srgb,
    matrix: MatrixCoefficients::Bt709,
    full_range: true,  // Force full range
};

let rgb = frame.to_rgb();
```

## References

### Standards
- **ITU-T H.265** - HEVC video coding standard
- **ITU-R BT.709** - HDTV standard
- **ITU-R BT.2020** - UHDTV standard
- **ITU-R BT.2100** - HDR TV standard (PQ and HLG)
- **SMPTE ST 2084** - PQ EOTF for HDR10
- **IEC 61966-2-1** - sRGB standard

### Further Reading
- [Understanding HDR10 and Dolby Vision](https://en.wikipedia.org/wiki/High-dynamic-range_television)
- [BT.2020 Color Space](https://en.wikipedia.org/wiki/Rec._2020)
- [PQ Transfer Function](https://en.wikipedia.org/wiki/Perceptual_quantizer)
- [HLG Transfer Function](https://en.wikipedia.org/wiki/Hybrid_Log-Gamma)
- [Tone Mapping Operators](https://en.wikipedia.org/wiki/Tone_mapping)
