# Native Pulse contract controls

Run `just pulse-contract-check` from the HEIC root under the workspace resource
wrapper. This separate test crate runs the **same** `tests/pulse_progress.rs`
source with the same fixtures and assertions. It avoids loading the historical
zensim-regress PNG dependency into these unrelated API tests. No root regression
suite is replaced or skipped. The original `pulse-migration-check` remains.

The root dev graph currently fails the old zencodec0.1.26 DecodeOutput layout
assertion after paired zenpixels changes. Its parent is zensim-regress493c91c →
zenpng0.1.4. Moving the metric to a newer revision is not qualified: the newer
Profile A refers to a different model payload. Keep the metric and thresholds
until equivalence or an intentional migration is validated. These focused native
controls do not qualify that root graph or platform backend parity.

`HEIC_PULSE_FIXTURES` explicitly names the required fixture directory. Missing
fixtures fail; there is no fallback skip. The source fixture helper defaults to
the original root path when the ordinary root test target is used.
