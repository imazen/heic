# Native Pulse contract qualification

Source b7cf5a60 (full revision in the pinned test source history), with core
feb09f9fd25e90e49a30d7161db8f3b558dfb2a6, zenpixels
283655c5424e0ad7ebc6d2acd94670fac1e29158 and UltraHDR
7e9d296c9e037e626234c9f0e8673d48278f83f0. This is a scoped API qualification,
not decoder-quality, throughput, whole-memory or platform-backend certification.

`just pulse-contract-check`:10 passed,0 failed,0 ignored; strict test-crate
Clippy passed. run-heavy3G/jobs2, timeout900: rc0,5s,peakRSS0.40GiB,
minimum available14677MiB,peakload0.41. The original source tests are used directly
by the separate manifest. Pixel comparisons are same-native, not an independent
oracle. No metric scores, tolerances or historical tool revision changed.

`cargo clippy --manifest-path tools/pulse-contract-check/Cargo.toml --locked
-p heic --lib -- -D warnings`: passes on the identical source/lock. See the wrapper
receipt below. Explicit --features on this nonmember package first failed Cargo
argument validation; the graph already activates the required HEIC features.

Controls exercise typed/erased native jobs, U8/U16 samples, child planning,
timeout preservation, retained pull sessions, stop composition, unchanged output
on cancellation, real row progress and cancellation after grid work. Caller root
completion remains with the caller. None does not disable explicit stop policy.

## Failures retained and scope

Initial export preparation assumed a tracked root Cargo.lock, but none exists.
The next manifest edit accidentally inserted bridge lines into a comment and
failed parsing before resolution; corrected before compilation. Root resolution
from the committed fuzz lock exposed old core0.1.26 through the unchanged metric's
PNG decoder. It fails its DecodeOutput size assertion with paired pixels; root
suite remains blocked, not silently omitted from qualification.

The first focused run passed8/10; two grid controls used files that took the
buffered path. Additional primary-item guards showed the named Nokia file selects
Hvc1 and example.heic has no grid. The corrected helper selects the existing
Nokia derived grid via pitm only, validates eligibility and retains all original
pixel/progress/cancellation assertions. No new binary fixtures are committed.
These failed attempts remain in the coordinator receipts. Old cov_zencodec grid
fixture claims need correction before claiming that root suite covers streaming.

The contract lock is committed under tools/pulse-contract-check. The full root
comparison-tool graph, standalone fuzz graph, no_std/WASM, generated API and all
platform backend/oracle suites remain unqualified for this candidate.

Library lint wrapper: `run-heavy: done rc=0 0s | peak-RSS 0.03GiB | min-avail 15160MiB | peak-load 0.26`.
