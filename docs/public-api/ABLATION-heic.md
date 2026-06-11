# ABLATION-heic.md — conservative public-API ablation report

**Date:** 2026-06-11  
**Snapshot commit:** 8a77714  
**Snapshot file:** `docs/public-api/heic.txt` (438 items default, 536 all-features)  
**Grep template (run from `/home/lilith/work`, exclude target/.jj/heic):**  
```
grep -r --include="*.rs" "<symbol>" /home/lilith/work/ --exclude-dir="target" --exclude-dir=".jj" --exclude-dir="heic"
```

---

## Summary

| Crate | Snapshot items | Flagged A | Flagged B | % flagged |
|-------|---------------|-----------|-----------|-----------|
| heic (default) | 438 | 2 | 0 | 0.5% |
| heic (all-features diff) | +98 | 0 | 0 | — |

**Conservative stance:** 436 of 438 items are KEEP. Two debug-infra functions are flagged for `#[doc(hidden)]` — they remain callable, just unlisted in generated docs.

---

## Known consumers (evidence gathered this scan)

| Consumer | Items used |
|----------|-----------|
| `hdr-corpus-convert/src/main.rs` | `DecoderConfig`, `ImageInfo`, `PixelLayout`, `color_primaries` field |
| `hdr-corpus-convert/src/bin/probe.rs` | `ImageInfo::from_bytes` |
| `zencodecs/src/dyn_dispatch.rs` + `codecs/heic.rs` | `HeicDecoderConfig`, `HeicDecodeJob` |
| `zencodecs/src/lib.rs` | `pub use heic::{HeicDecodeJob, HeicDecoderConfig}` |
| `imageflow-zencodecs-v2` | `HeicDecoderConfig` |
| Backends (`heic-backend-mediafoundation`, `-vaapi`) | `HvccParams`, `nal::*`, `sps::{ParsedSps, ParsedPps}` |

---

## Flagged items

### A — `#[doc(hidden)]` candidates (debug infra, zero external consumers)

| Item | Location | Evidence | Proposed action |
|------|----------|----------|-----------------|
| `pub fn heic::cabac_bin_trace(u32)` | `src/lib.rs:224` | Only called in `examples/bin_trace_emit.rs` and `tests/mc_trace.rs` (intra-repo). Zero external grep hits. | A: `#[doc(hidden)]` — keeps it callable for debugging without advertising it as API |
| `pub fn heic::enable_deblock_trace()` | `src/lib.rs` (re-exported from `src/hevc/mod.rs`) | Only called at `src/lib.rs:` (passthrough) and inside heic test/example files. Zero external grep hits. | A: `#[doc(hidden)]` — same rationale |

---

## Items reviewed and explicitly kept

**heic crate core decode API (438 items, bulk):** `DecoderConfig`, `DecodeRequest`, `DecodeOutput`, `ImageInfo`, `Limits`, `PixelLayout`, `HeicError`, `HevcError`, `ProbeError`, `RowSink`, `recommended_backends`, `Result` — all have confirmed external consumers or are essential decode API surface. KEEP.

**Zencodec adapter layer** (in all-features diff): `HeicDecodeJob`, `HeicDecoderConfig`, `HeicStreamDecoder`, `HeicZenDecoder` — wired into zencodecs and imageflow. KEEP.

**`HeicAuxiliaryInfo`** (all-features): Populated in `src/codec.rs:854` and accessed via `output.extensions().get::<HeicAuxiliaryInfo>()`. No external grep hits as of this scan, but it is the intentional extension-point for zencodec consumers to access depth/gainmap flags without recalling the raw decode API. KEEP — it is the deliberate API for future consumers, not an accidental leak.

**`DepthMap`, `DepthRepresentationInfo`, `DepthRepresentationType`**: Returned by `decode_depth()`. No external consumers found this scan, but the API is clearly intentional (public purpose: depth metadata for photography apps). KEEP.

**`HdrGainMap`, `GainMapOrigin`**: Returned by `decode_gain_map()`, called in `hdr-corpus-convert`. KEEP.

**`SegmentationMatte`, `AuxiliaryImageDescriptor`, `AuxiliaryImageType`**: Returned by `decode_matte()`/`decode_mattes()`/`auxiliary_images()`. No external hits this scan. KEEP — these are the deliberate API for portrait segmentation consumers; the crate README advertises them.

**`ImageInfo` pub fields** (`chroma_format`, `color_primaries`, `matrix_coefficients`, `transfer_characteristics`, `video_full_range`, `bit_depth`, CICP fields): `color_primaries` confirmed read in `hdr-corpus-convert/src/main.rs:84`. KEEP.

---

## Backend crates note

See `ABLATION-heic-backends.md` for the 5 backend crates. The 4 small backends (17 items each) are backend-trait contract surface only — 0 flagged. The VA-API backend (134 items) exposes `va_hevc` submodule with FFI layout types used internally by the backend's own tests and `from_sps_pps` constructor. See that file for details.
