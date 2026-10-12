# Metadata integration API audit

Baseline: [heic 0.1.6](https://crates.io/crates/heic/0.1.6), downloaded source
inspected on 2026-10-12. Integration starts at `e5ad6684`; it incorporates
[PR 51](https://github.com/imazen/heic/pull/51) (`e8008485`) and
[PR 57](https://github.com/imazen/heic/pull/57) (`628ebc30`).

## This integration

- Add the default-off `inventory` feature and implement the shared
  `DecodeJob::inventory` method. Disabled builds keep the callable method and
  return `UnsupportedOperation::Inventory`; both generic and dynamic dispatch
  retain configured input limits and cancellation categories.
- Public metadata types remain HEIC/zencodec types. The zenmeta Apple parser
  is private implementation detail of the `zencodec` adapter; native extraction
  needs neither zenmeta nor zencodecs/zenpipe.
- Requested gain-map failures propagate. Present ISO metadata cannot silently
  fall back to Apple metadata; component requests reject missing/invalid
  parameters. This changes error behavior, not the method signatures.
- Inventory reuses box/property/extent parsers, and its additional inspection
  helpers and track-role collection are excluded from normal builds.

## Published baseline versus existing main

`just metadata-semver` forces a patch comparison even though this checkout
already says 0.2.0. Otherwise cargo-semver-checks skips every check because it
infers a breaking release. The forced comparison reports 221 passing and two
failing checks (31 skipped): removed default `std`, and `DecodedFrame` reported
as missing.

The latter needs manual interpretation: the root path still exists through
`hevc::picture` re-exporting `heic_core::frame::DecodedFrame`. All published
public fields and method names remain. Its six allocating conversion methods
now return `heic_core::HevcError` rather than `heic::HevcError`; these are
distinct enums. That is a real pre-existing break from `edf1c0c`, even though
the path itself remains available. The tool result is not evidence that the
rest of the re-exported type is compatible.

Other pre-existing breaks are already queued in CHANGELOG: explicit backend
selection, located probe/HEVC errors, the reshaped AllBackendsFailed variant,
and shared CodecError associated types at the zencodec boundary. Source review
is required alongside the automated report, which does not enumerate all of
these changes. The metadata extraction does not justify hiding them or
claiming the entire unreleased 0.2 tree is patch-compatible.

## Reused validation

`just metadata-check` runs the original inventory, mutation, stable fuzz
regression and library suites, plus generic/dynamic feature-contract checks.
`just metadata-portability` runs the same inventory coverage on i686 and checks
the native no_std decoder on WASI. The oversized-mdat test helper uses
subtraction-based bounds checks so its original assertion is reachable on
32-bit platforms too.

`just metadata-oracles` compares the 119 bundled inputs against ExifTool and
FFmpeg. ExifTool reports no mismatches. FFmpeg accepts 31 inputs and rejects
88; on the accepted subset all 41 packet NALs match, and all 120 inserted
parameter sets match hvcC. These framing checks are not a claim of decoder
parity on the rejected inputs. Existing mutation tests independently verify
that bytes labeled unread cannot affect decoded pixels.

`just metadata-decode-regression` also passes the existing bundled decode,
public behavior and documentation tests; `just metadata-final-check` checks
all workspace targets with clippy and asserts mid-walk timeout preservation.

Temporary git pins are intentional until the shared crates are released.
All normal dependencies share zencodec `702a6260`; ultrahdr-core uses
`1d86bffc` with explicit f16 support. Publication requires replacing these
pins and re-running the published-surface comparison.
