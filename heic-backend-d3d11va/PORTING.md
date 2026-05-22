# Porting Chromium's D3D11 HEVC accelerator to Rust

The runtime availability probe in `src/probe.rs` is done. The full
decode FFI follows Chromium's `media/gpu/windows/d3d11_h265_accelerator.cc`
(704 LOC, BSD-3-Clause) which is sparse-checked-out under
`~/work/chromium/media/gpu/windows/`.

Use this file as a checklist when implementing `decode_hevc`.

## Pipeline outline

1. **Device + decoder creation** (`H265Decoder::CreateAcceleratedVideoDecoder`):
   - `D3D11CreateDevice(D3D_DRIVER_TYPE_HARDWARE)` — already in probe.
   - `ID3D11VideoDevice::CreateVideoDecoder(profile, &config, &decoder)`
     where `config` comes from `GetVideoDecoderConfig(profile, fmt, i)`
     looking for `ConfigBitstreamRaw == 1` (short-format slice control).
   - `ID3D11Texture2D` output texture, `ArraySize` ≥ 1 picture buffer
     entries, `BindFlags = DECODER`, `Format = NV12 or P010`.

2. **Per-slice submission** (PicParamsFromSPS/PPS/SliceHeader/Pic):
   - `SubmitFrameMetadata` builds `DXVA_PicParams_HEVC[_Rext]`
     (line 81–135 of accelerator).
   - `SubmitSlice` builds `DXVA_Slice_HEVC_Short` per slice and copies
     raw NAL bytes into the BITSTREAM buffer (line 470).
   - `SubmitDecode` calls `SubmitDecoderBuffers` with all 4 buffer types
     in one go (line 677).

3. **Readback**:
   - Create a staging texture (CPU-readable, no GPU bindings).
   - `CopySubresourceRegion` from decoder output → staging.
   - `Map` the staging texture, read NV12 / P010, unpack via the same
     pattern as `heic-backend-mediafoundation/src/pixels.rs`.

## DXVA_PicParams_HEVC field-by-field

Chromium uses a `SPS_TO_PP` macro to copy SPS field names that match
DXVA struct field names; the helper at line 175-180 of the accelerator
defines it. The non-trivial cases (different naming, derived values)
are explicit:

| DXVA field | SPS / PPS source | Notes |
|---|---|---|
| `PicWidthInMinCbsY` | `sps->pic_width_in_luma_samples >> min_cb_log2_size_y` | formula 7-14 |
| `PicHeightInMinCbsY` | `sps->pic_height_in_luma_samples >> min_cb_log2_size_y` | formula 7-16 |
| `chroma_format_idc` | `sps->chroma_format_idc` | direct |
| `separate_colour_plane_flag` | `sps->separate_colour_plane_flag` | direct |
| `bit_depth_luma_minus8` | `sps->bit_depth_luma_minus8` | direct |
| `bit_depth_chroma_minus8` | `sps->bit_depth_chroma_minus8` | direct |
| `log2_max_pic_order_cnt_lsb_minus4` | `sps->log2_max_pic_order_cnt_lsb_minus4` | direct |
| `sps_max_dec_pic_buffering_minus1` | `sps->sps_max_dec_pic_buffering_minus1[highest_tid]` | per A.4.1 |
| `log2_min_luma_coding_block_size_minus3` | direct |
| `log2_diff_max_min_luma_coding_block_size` | direct |
| `log2_min_transform_block_size_minus2` | `sps->log2_min_luma_transform_block_size_minus2` | renamed |
| `log2_diff_max_min_transform_block_size` | `sps->log2_diff_max_min_luma_transform_block_size` | renamed |
| `max_transform_hierarchy_depth_inter/intra` | direct |
| `num_short_term_ref_pic_sets` | direct |
| `num_long_term_ref_pics_sps` | direct |
| `scaling_list_enabled_flag` | direct |
| `amp_enabled_flag` | direct |
| `sample_adaptive_offset_enabled_flag` | direct |
| `pcm_enabled_flag` | direct |
| ... 30 more fields from SPS + PPS ... | see `PicParamsFromSPS/PPS` |

The corresponding fields are already exposed on our pure-Rust
`heic::hevc::params::Sps` (just made `pub(crate)`; promote selected
fields to `pub` if exposing to this backend, or pull the parser into
`heic-core` so it's reusable across crates).

## PORTING NOTES

* Chromium's parser is `media/parsers/h265_parser.cc` (2308 LOC). Our
  pure-Rust `src/hevc/params.rs` parses the same fields; the only gaps
  are RangeExtension (rext) and SCC profiles (HEIC main-profile decode
  doesn't need them).
* The `DXVA_PicParams_HEVC` struct definition lives in the Windows
  10 SDK (`dxva.h`). The `windows` Rust crate doesn't expose it
  directly; either generate via `bindgen`, hand-write the struct
  with `#[repr(C)]`, or extract from the Windows-rs metadata
  using `windows-bindgen`.
* DPB management for I-frame-only HEIC content collapses: each tile is
  a self-contained IDR, no reference frames carry across access units.
  Chromium's full DPB logic in `h265_decoder.cc` (1330 LOC) can be
  collapsed to a single-frame buffer for HEIC.

## Why not just port the whole thing?

A faithful port of `d3d11_h265_accelerator.cc` (~700 LOC) plus the
field-by-field SPS/PPS struct definitions (~250 LOC) plus the slice
control submission (~200 LOC) is ~1200 LOC of mechanical C++ → Rust
work. The runtime-tested behavior on real GPUs depends on driver
quirks (Intel iHD on Tiger Lake, AMD Mesa VCN, NVIDIA NVDEC) that
the Chromium tree has accumulated workarounds for over years. A
fresh Rust port without the same hardware fleet to test against will
inevitably miss edge cases.

The pragmatic path:
1. Probe lands first (this commit).
2. Decode FFI lands behind a feature flag, initially only verified
   against Intel iGPU + libheif test vectors.
3. Per-driver workarounds added as users file issues.
