# ABLATION-heic-backends.md — conservative public-API ablation report (5 backend crates)

**Date:** 2026-06-11  
**Snapshot commit:** 8a77714  
**Crates covered:** heic-backend-d3d11va, heic-backend-mediacodec, heic-backend-mediafoundation, heic-backend-vaapi, heic-backend-videotoolbox

---

## Summary

| Crate | Snapshot items | Flagged A | Flagged B | % flagged |
|-------|----------------|-----------|-----------|-----------|
| heic-backend-d3d11va | 17 | 0 | 0 | 0% |
| heic-backend-mediacodec | 17 | 0 | 0 | 0% |
| heic-backend-mediafoundation | 17 | 0 | 0 | 0% |
| heic-backend-videotoolbox | 17 | 0 | 0 | 0% |
| heic-backend-vaapi | 134 | 0 | 1 group | <1% |

---

## heic-backend-d3d11va, heic-backend-mediacodec, heic-backend-mediafoundation, heic-backend-videotoolbox

Each of these four crates exposes exactly 17 items: the backend struct (`D3d11VaBackend` / `MediaCodecBackend` / `MfBackend` / `VideoToolboxBackend`), its `new()` / `default()` constructors, and the 3-method `HevcBackend` trait impl (`decode_hevc`, `is_available`, `name`). This is the minimum backend contract surface — it cannot be narrowed further. **0 items flagged.**

---

## heic-backend-vaapi (134 items)

### Consumer evidence

The vaapi backend is not depended upon by any crate outside the heic workspace. Its public API is used exclusively by:
- `heic/src/lib.rs` — wires `VaApiBackend` as a selectable backend variant
- `heic-backend-vaapi/src/va_hevc.rs` — uses `pic_fields::*` and `slice_parsing_fields::*` constants internally, and exports `from_sps_pps` + the VA picture struct types for testing

### Flagged items

**Group B: `heic_backend_vaapi::va_hevc` submodule** (~100 of 134 items)

The `va_hevc` module exposes:
- `VaPictureHevc` (repr(C) FFI struct, pub fields) — used in the vaapi backend's `from_sps_pps` function
- `VaPictureParameterBufferHevc` (repr(C) FFI struct) — same
- `from_sps_pps` free function — takes `ParsedSps`/`ParsedPps`, returns `VaPictureParameterBufferHevc`
- `pic_fields::*` constants (~25) — bitmask constants for libva struct packing
- `slice_parsing_fields::*` constants (~15) — same

These are VA-API FFI layout types, not a library API. No crate outside the heic workspace uses them. The constants exist as `pub` because they are referenced in unit tests within `va_hevc.rs` itself via `use pic_fields::*`.

**B proposal:** Make `va_hevc` module `pub(crate)` within `heic-backend-vaapi`. This is a workspace-internal crate so no external semver breakage occurs. The tests remain valid. The `from_sps_pps` function is only used within the vaapi backend's decode path.

**Conservative note:** Since heic-backend-vaapi has no external dependents and this is workspace-internal, this is a low-risk cleanup but it is a breaking change to the module's pub API. Queue for the next time heic-backend-vaapi bumps its minor version.

### Items kept without flag

**`heic_backend_vaapi::VaApiBackend`** struct + `HevcBackend` impl (17 items, same as other backends): backend contract surface. KEEP.
