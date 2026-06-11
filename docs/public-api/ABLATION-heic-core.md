# ABLATION-heic-core.md — conservative public-API ablation report

**Date:** 2026-06-11  
**Snapshot commit:** 8a77714  
**Snapshot file:** `docs/public-api/heic-core.txt` (319 items default, 321 all-features)  
**Grep template (from `/home/lilith/work`, exclude target/.jj/heic):**  
```
grep -r --include="*.rs" "<symbol>" /home/lilith/work/ --exclude-dir="target" --exclude-dir=".jj" --exclude-dir="heic"
```

---

## Summary

| Snapshot items | Flagged A | Flagged B | % flagged |
|----------------|-----------|-----------|-----------|
| 319 | 0 | 3 groups | ~1% (B only) |

**Conservative stance:** heic-core has zero external dependents — it is not listed in any `Cargo.toml` outside the heic workspace. All consumers are internal: the `heic` crate re-exports `DecodedFrame`; the backend crates (`heic-backend-*`) depend on it for `HvccParams`, `sps::ParsedSps/ParsedPps`, `nal::*`, and `HevcBackend`.

Because heic-core is an internal implementation crate with no external dependents, the bar for B-flagging is higher: we flag only items that are **clearly leaking parse internals** with no plausible external consumer and no reason to be pub rather than pub(crate).

---

## Known consumers (evidence gathered this scan)

| Consumer | Items used |
|----------|-----------|
| `heic` crate (`src/lib.rs`, `src/decode.rs`) | `DecodedFrame`, re-exports via `pub use heic_core::frame::DecodedFrame` |
| `heic-backend-mediafoundation` | `HvccParams`, `BackendError`, `DecodedFrame`, `HevcBackend`, `nal::*` |
| `heic-backend-vaapi` | `HvccParams`, `BackendError`, `DecodedFrame`, `HevcBackend`, `sps::{ParsedSps, ParsedPps}`, `HevcScalingListData` |
| `heic-backend-d3d11va`, `-mediacodec`, `-videotoolbox` | `HvccParams`, `BackendError`, `DecodedFrame`, `HevcBackend` |
| Zero external crates | — heic-core not in any Cargo.toml outside heic workspace |

---

## Flagged items

### B — `pub(crate)` / hidden-via-crate-boundary candidates (no external consumers, implementation internals)

These are flagged as **B proposals** for the next planned breaking release cycle. Since heic-core has no external dependents today, any breaking change here is entirely internal. That said, they should be narrowed to communicate intended API boundaries.

**Group 1: `heic_core::color_convert` module**

`pub fn heic_core::color_convert::convert_420_to_rgb(...)`, `convert_444_to_rgb(...)`, `rgb_to_ycbcr8(...)` — 3 free functions. Used only within heic-core's `frame.rs` and by `heic/src/decode.rs` via `crate::hevc::color_convert::*` (note: accessed as a sibling module, not via `heic_core::` path). No external crate uses these directly. These are pixel-math helpers, not a stable API surface. **B proposal: `pub(crate)` within heic-core; heic gets them via heic-core's internal exports already.**

| Item | Call sites found |
|------|-----------------|
| `heic_core::color_convert::convert_420_to_rgb` | `heic_core/src/frame.rs` (×2), `heic/src/decode.rs` via `crate::hevc::color_convert` |
| `heic_core::color_convert::convert_444_to_rgb` | `heic_core/src/frame.rs` (×1) |
| `heic_core::color_convert::rgb_to_ycbcr8` | `heic/src/decode.rs` via `crate::hevc::color_convert` |

**Group 2: `heic_core::nal` free functions**

`pub fn heic_core::nal::annexb_parameter_sets(...)` and `hvcc_to_annexb(...)` — used in `heic-backend-mediafoundation/src/imp.rs` (`use heic_core::{..., nal}`). Since backends are within the heic workspace and there are no external dependents, these could be `pub(crate)`. However, `annexb_parameter_sets` / `hvcc_to_annexb` are well-named utilities that could legitimately serve future backend integrations. **Conservative: note only, leave as pub — a future backend author might need them. Do NOT flag as B.**

**Group 3: `heic_core::sps::{ParsedSps, ParsedPps, SpsRangeExtension, HevcScalingListData}` — all fields pub**

These raw HEVC parameter-set structs with every field public are consumed by `heic-backend-vaapi` (which constructs `ParsedSps::default()` in tests and reads many fields in `from_sps_pps`). They are necessary for the backend contract surface and cannot be narrowed without redesigning the VA-API backend. **KEEP as pub — the vaapi backend legitimately uses them.**

---

## Items reviewed and explicitly kept

**`heic_core::frame::DecodedFrame`** (with all pub fields): Re-exported by `heic` crate, returned by `DecoderConfig::decode_to_frame()`. The pub fields (`y_plane`, `cb_plane`, `cr_plane`, `alpha_plane`, crop fields, color fields) are readable by the `hdr-corpus-convert` tool via `DecoderConfig::decode_to_frame`. KEEP.

**`heic_core::HvccParams<'a>`** (with all pub fields): The backend dispatch contract. Every backend's `decode_hevc` takes `&HvccParams`. Fields are read directly in backend impls. KEEP.

**`heic_core::HevcBackend` trait**: The plugin point for all backends. KEEP.

**`heic_core::BackendError`**: Returned from all backends. KEEP.

**`heic_core::error::HevcError`**: Propagated into `heic::HevcError`. KEEP.
