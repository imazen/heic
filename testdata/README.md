# Test Data

HEIC/HEIF test files for parser and decoder testing.

## Sources and Licenses

### `libheif-examples/`

Test files from the [libheif](https://github.com/strukturag/libheif) project.
Licensed under the MIT License (see `libheif-examples/COPYING`).

Includes HEVC-compressed images, uncompressed HEIF variants (RGB, YUV 4:2:0/4:2:2/4:4:4,
ABGR, 8/16-bit, tiled/untiled, various interleaving modes), and generic compressed
formats (zlib, brotli, deflate).

### `apple-hdr/`

HDR gain map sample from [apple-hdr-heic](https://github.com/johncf/apple-hdr-heic)
by John Charankattu. Licensed under the MIT License (see `apple-hdr/LICENSE`).

Contains an iPhone HDR photo with Apple `urn:com:apple:photo:2020:aux:hdrgainmap`
auxiliary image and XMP gain map metadata.

### `synthetic/`

Generated test images at various quality levels. No third-party license applies.
