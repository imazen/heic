# Shared core dependency selection (2026-10-29)

Normal zencodec/zenmeta dependencies use version requirements. Root development
patches retain the previous pinned source. An outer consumer can now select
one core revision without a second git-source copy.

Qualified through an isolated Zenpipe export: core e1c0ca5b, pixels f23ac753,
GIF a258e93c; HEIC c4f74f48 and RAW 63207707 source exports. No library source
or established assertions changed. All five application graph roots resolve
one core family. `just checked-publication-check` passes 16 unit and 35 native
controls; `cargo test -p zenpipe-io-check --features job --locked` passes 57
controls and its all-target strict Clippy passes. Run receipt: rc0, 66s,
peak RSS 0.68GiB. Full-profile `just api-profile-check` and
`just raw-gainmap-controls` pass, including strict consumer Clippy; receipt
rc0, 70s, peak RSS 0.95GiB. These are integration checks, not fresh standalone
codec/platform or complete release qualification.

Commands ran under run-heavy with 3G/jobs2 and timeout900. Logs:
`~/tmp/zenpixels-release-reconcile-20261029/zenpipe-portable2.log` and
`zenpipe-portable-profile.log` on the qualification host.
