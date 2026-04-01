# heif-ref: Reference HEIF Extraction Tool

Extracts all metadata and decoded pixels from HEIF files using libheif,
for comparison testing against our pure-Rust decoder.

## Docker Image

Build the image (requires Docker):

```bash
docker build -t ghcr.io/imazen/heif-ref:latest reference/
```

Update the libheif version by passing `--build-arg LIBHEIF_TAG=v1.21.2`.

## Usage

```bash
# Extract everything from a HEIC file
docker run --rm -v $(pwd):/data ghcr.io/imazen/heif-ref \
  extract /data/testdata/libheif-examples/example.heic /data/ref-output/

# Check versions
docker run --rm ghcr.io/imazen/heif-ref version
```

## Output Format

The `extract` command writes to the output directory:

| File | Contents |
|------|----------|
| `info.json` | All metadata: dimensions, color profile, pixel stats, CRC32 checksums |
| `primary.bin` | Decoded RGB8 pixels (`width * height * 3` bytes, row-major) |
| `thumbnail.bin` | Thumbnail RGB8 (if present) |
| `aux_<id>.bin` | Auxiliary images as grayscale (gain map, depth, alpha, mattes) |
| `aux_<id>_xmp.xml` | XMP metadata for auxiliary items (gain map metadata) |
| `exif.bin` | Raw EXIF bytes (if present) |
| `xmp.xml` | Raw XMP bytes (if present) |
| `icc.bin` | Raw ICC profile (if present) |

### info.json schema

```jsonc
{
  "schema_version": 1,
  "libheif_version": "1.19.7",
  "primary": {
    "width": 1280, "height": 854,
    "ispe_width": 1280, "ispe_height": 856,
    "has_alpha": false, "bit_depth": 8,
    "color_profile": {
      "type": "nclx",  // or "prof", "rICC", "none"
      "color_primaries": 1,
      "transfer_characteristics": 13,
      "matrix_coefficients": 6,
      "full_range": true
    },
    "pixel_crc32": 1606104031,
    "pixel_stats": {
      "r_min": 0, "r_max": 255, "r_mean": 122.73,
      "g_min": 0, "g_max": 255, "g_mean": 126.49,
      "b_min": 0, "b_max": 255, "b_mean": 121.11
    },
    "exif": { "present": true, "size": 2454, "crc32": 3476050486 },
    "xmp": { "present": true, "size": 363, "crc32": 2064621829 },
    "thumbnail": { "present": true, "width": 320, "height": 212 },
    "auxiliary_images": [
      {
        "id": 10,
        "type": "urn:com:apple:photo:2020:aux:hdrgainmap",
        "width": 756, "height": 426,
        "decoded_width": 756, "decoded_height": 426,
        "pixel_crc32": 123456789,
        "xmp_size": 363
      }
    ]
  }
}
```

## Local Build (without Docker)

If you have libheif-dev installed:

```bash
gcc -O2 -Wall -o heif-ref reference/heif_ref.c $(pkg-config --cflags --libs libheif)
```

Auxiliary image extraction requires libheif >= 1.14.
