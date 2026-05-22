# Porting Chromium's libva HEVC accelerator to Rust

The runtime availability probe in `src/probe.rs` is done. The full
decode FFI follows Chromium's
`media/gpu/vaapi/h265_vaapi_video_decoder_delegate.cc` (644 LOC,
BSD-3-Clause), sparse-checked-out under
`~/work/chromium/media/gpu/vaapi/`.

Use this as a checklist when implementing `decode_hevc`.

## Pipeline outline

1. **Display + context setup**:
   - `vaGetDisplayDRM(/dev/dri/renderD128)` → `VADisplay` (already
     in probe).
   - `vaCreateConfig(display, VAProfileHEVCMain, VAEntrypointVLD,
     attrs, &config)`.
   - `vaCreateSurfaces(display, RT_FORMAT_YUV420, w, h, &surface, 1)`.
   - `vaCreateContext(display, config, w, h, FLAG_PROGRESSIVE,
     &surface, 1, &context)`.

2. **Per-frame buffer submission**:
   - Build `VAPictureParameterBufferHEVC` from VPS+SPS+PPS+slice.
     The 38-arg ctor lives in `cros-libva 0.0.13` —
     `buffer/hevc.rs::PictureParameterBufferHEVC::new`. Chromium's
     mapping is in `FillPicParams` (line 119 of the delegate).
   - Build `VAIQMatrixBufferHEVC` from PPS scaling lists if
     `pps->scaling_list_data_present_flag`.
   - Build `VASliceParameterBufferHEVC` per slice (line 311).
   - Build `VASliceDataBufferType` with the raw NAL bytes.
   - `vaBeginPicture(display, context, surface)`.
   - `vaRenderPicture(display, context, [pic_param_buf, iq_buf,
     slice_param_buf, slice_data_buf], 4)`.
   - `vaEndPicture(display, context)`.
   - `vaSyncSurface(display, surface)`.

3. **Readback**:
   - `vaDeriveImage(display, surface, &image)` — returns a
     `VAImage` with `data_size`, `offsets[3]`, `pitches[3]`.
   - `vaMapBuffer(display, image.buf, &data_ptr)`.
   - Unpack NV12 / P010 from `data_ptr` using the same crop-aware
     pattern as `heic-backend-mediafoundation/src/pixels.rs`.
   - `vaUnmapBuffer` + `vaDestroyImage`.

## VAPictureParameterBufferHEVC field-by-field

Chromium's `FillPicParams` (h265_vaapi_video_decoder_delegate.cc:119)
maps SPS+PPS+slice fields into the libva struct. Headline groups:

* **Dimensions**: `pic_width_in_luma_samples`, `pic_height_in_luma_samples`.
* **Picture fields union** (`HevcPicFields` in cros-libva):
  - `chroma_format_idc`, `separate_colour_plane_flag`,
    `pcm_enabled_flag`, `scaling_list_enabled_flag`,
    `transform_skip_enabled_flag`, `amp_enabled_flag`,
    `strong_intra_smoothing_enabled_flag`,
    `sign_data_hiding_enabled_flag`, `constrained_intra_pred_flag`,
    `cu_qp_delta_enabled_flag`, `weighted_pred_flag`,
    `weighted_bipred_flag`, `transquant_bypass_enabled_flag`,
    `tiles_enabled_flag`, `entropy_coding_sync_enabled_flag`,
    `pps_loop_filter_across_slices_enabled_flag`,
    `loop_filter_across_tiles_enabled_flag`,
    `pcm_loop_filter_disabled_flag`.
* **Slice parsing fields union** (`HevcSliceParsingFields`):
  - `lists_modification_present_flag`,
    `long_term_ref_pics_present_flag`,
    `sps_temporal_mvp_enabled_flag`, `cabac_init_present_flag`,
    `output_flag_present_flag`, `dependent_slice_segments_enabled_flag`,
    `pps_slice_chroma_qp_offsets_present_flag`,
    `sample_adaptive_offset_enabled_flag`,
    `deblocking_filter_override_enabled_flag`,
    `pps_disable_deblocking_filter_flag`,
    `slice_segment_header_extension_present_flag`,
    `RapPicFlag`, `IdrPicFlag`, `IntraPicFlag`.
* **Bit-depths + log2-block-sizes**: directly named per HEVC spec.
* **Tile layout**: `num_tile_columns_minus1`, `num_tile_rows_minus1`,
  `column_width_minus1[19]`, `row_height_minus1[21]`.
* **Reference frames array**: 15-entry `[PictureHEVC; 15]` populated
  from the DPB. For HEIC tiles (I-frame only), all slots are
  `INVALID`.

## PORTING NOTES

* `cros-libva 0.0.13` has the safe FFI wrappers. Add as a dep behind
  an optional `decode-ffi` feature so users can still get the probe
  via the default `libloading` path without pulling pkg-config.
* The full delegate handles all HEVC profiles (Main, Main10,
  RangeExtension, SCC). HEIC only needs Main + Main10 — Range
  Extension fields can be no-op'd if `sps_range_extension_flag = 0`.
* DPB management collapses for HEIC: each tile is a self-contained
  IDR, no inter-frame references. Chromium's
  `h265_decoder.cc::CalcRefPicPocList` and related can be skipped.
* `recommended_backends()` already routes via this crate's
  `is_available`, so once the FFI is real, no parent-crate changes
  needed to make VA-API the preferred backend on supported hardware.

## Why not just port the whole thing?

Same reasoning as `heic-backend-d3d11va/PORTING.md`. The pragmatic
path:

1. Probe ships first (commit `c67dcd6`).
2. Real decode FFI behind a feature flag, initially tested on Intel
   iGPU with `intel-media-va-driver-non-free`.
3. Per-driver quirks added as issues land.
