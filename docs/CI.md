# CI & runtime-test strategy

How `heic` is tested, what runs where, and the plan for the runtime-decode gaps
on the native backends. Written 2026-05-31 alongside the deep safety review.

## Principle

`heic` decodes **untrusted input**, and the parent + `heic-core` are
`#![forbid(unsafe_code)]`. The whole memory-safety surface is the five native
FFI backend crates. So CI has two jobs:

1. **Catch panics / wrong pixels / OOB on real input** in the pure-Rust path —
   cheap, fully hosted, must be exhaustive.
2. **Actually execute the native FFI** — needs the target OS, and for VA-API /
   D3D11VA a real GPU. This is where the gaps are.

## Per-backend runtime coverage

| Backend | Compile / clippy | Runtime decode in CI | Where |
|---|---|---|---|
| `backend-rust` | all 6 OSes + i686 + wasm32 | ✅ corpus gate on all 6 OSes + i686 (32-bit) | `ci.yml` |
| MediaFoundation | windows-latest (x64+arm64) | ✅ `windows-11-arm` (HEVC AppX side-loaded) | `ci.yml` |
| VideoToolbox | macOS arm64 + Intel | ✅ `macos-latest` + `macos-15-intel` | `ci.yml` |
| MediaCodec | aarch64-android (NDK) | ✅ **Android emulator** (x86_64, software HEVC) | `mediacodec-runtime.yml` |
| VA-API | ubuntu (libva-dev) | ⏳ self-hosted Linux+GPU only | `vaapi-runtime.yml` |
| D3D11VA | windows (x64+arm64) | ⏳ self-hosted Windows+GPU only | `d3d11va-runtime.yml` |

`✅` = runs on every push. `⏳` = workflow exists but needs a self-hosted GPU
runner registered (see below) — until then those two run compile-only on hosted
CI.

## Tier 1 — hosted, free, already wired (`ci.yml`, `fuzz.yml`)

- **Corpus decode gate** (`tests/corpus_decode.rs`): decodes all 95 committed
  `testdata/` files (incl. 10-bit Apple HDR, grids, uncompressed HEIF) on every
  OS; loud-fails if `testdata/` is missing (no graceful skip). The main matrix
  no longer runs only `cargo test --lib`.
- **i686 + 32-bit decode correctness**: the `cross` job runs the corpus gate, not
  just `--lib` — exercises the 32-bit overflow/wrap fixes (NAL `checked_add`,
  `stsc` clamp, zencodec `checked_mul`).
- **Overflow-checked job**: corpus + every fuzz-regression seed under
  `-C overflow-checks=on` (the HEVC param-set overflow bugs only panic with
  checks on).
- **All 7 fuzz targets** run nightly (was 5).
- **`cargo-semver-checks`** (informational on 0.x), **`cargo doc -D warnings`**,
  and **`av1` + `unci`** lint/unit gating — all were missing.
- **MF runtime AppX step** now fails loud when the secret is set but install
  fails (was `continue-on-error` → the only MF runtime gate could silently
  no-op).

### Conformance suite — still a gap

The 49 ITU-T HEVC conformance vectors are **not committed** (only
`conformance/fetch.sh`), and the test hardcodes a local `dec265` path and
skips at every step, so "49/49 pass" is a local-only claim with no CI gate. To
close it, pick one:

- **(cheap, recommended first)** commit golden PSNR/hash expectations per vector
  and a gated CI job that runs `conformance/fetch.sh` (workflow-level env gate,
  not an in-test file check) + asserts against the goldens — no `dec265` needed.
- **(heavier)** build/install `dec265` in CI and run the existing comparison
  with hard `assert!`s, `dec265` path made configurable.

## Tier 2 — VA-API & D3D11VA need a GPU. Bounded-cost plan.

GitHub-hosted runners can't cover these: no GPU on Linux/Windows hosted images
(the one GitHub T4 GPU runner needs a Team/Enterprise plan and exercises NVDEC
via a Firefox-only `nvidia-vaapi-driver` shim, not the real `libva` / DXVA
paths). The bounded-cost answer is **owned hardware**:

> **Recommended: a self-hosted runner on an Intel N100/N150 mini-PC** (~$150–300
> one-time). Its iGPU does real VA-API HEVC decode on Linux (iHD driver) and
> real D3D11VA HEVC decode on Windows — one box can cover both by dual-boot.
> **Marginal cost per job is $0**, so no code bug, crash, or leaked credential
> can ever produce a runaway bill. Cost is bounded *by construction*, not by a
> policy that has to fire correctly. (Cloud GPU with budget guardrails is
> *bounded-after-the-fact* at best and adds real misconfig surface for this
> low-volume use case — not worth it here.)

### To activate (one-time, user action)

1. **Provision** an Intel-iGPU mini-PC. Linux: install `libva` + the iHD driver
   (Kaby-Lake-or-newer for 10-bit HEVC), confirm `vainfo` lists HEVC decode and
   `/dev/dri/renderD128` exists. Windows: install GPU drivers; the iGPU does
   DXVA HEVC out of the box.
2. **Register an ephemeral, repo-scoped self-hosted runner** with labels
   `[self-hosted, linux, vaapi]` and/or `[self-hosted, windows, d3d11va]`.
   Prefer JIT/ephemeral registration (one job then deregister) so state can't
   persist between jobs; keep **no secrets** on the box.
3. **Harden for the public repo**: Settings → Actions → Fork pull request
   workflows → **"Require approval for all outside collaborators"**. The GPU
   workflows already gate to push-to-main / `workflow_dispatch` / PRs labeled
   `gpu-ci` (label = write access), so fork code never runs unapproved.
4. **Org budget backstop (free, do regardless)**: set an Actions **product-level
   budget with "Stop usage when budget limit is reached"** (e.g. $20/mo) on the
   org. Even though self-hosted jobs bill $0, this caps any *accidental* hosted
   GPU/runner spend (including a leaked-token spree) at a provable maximum,
   satisfying "a leaked secret can't bill $50k" at the org level.

Once a runner is up, `vaapi-runtime.yml` and `d3d11va-runtime.yml` start
gating real decode automatically. Both run `examples/backend_decode` with a
**single forced backend (no pure-Rust fallback)**, so a native-FFI failure is a
hard error, plus the bit-exact-vs-rust synthetic-corpus gate.

## Implementation order

1. **VA-API self-hosted first** — highest coverage-per-effort; iHD is the most
   common real VA-API target and the largest unsafe surface (~115 lines).
2. **D3D11VA self-hosted second** — same hardware class (can dual-boot the same
   box); Windows ephemeral isolation is fiddlier.
3. **Conformance goldens** — closes the "49/49 is local-only" gap cheaply.

## The locally-verifiable runtime tool

`examples/backend_decode <file.heic> <backend>` forces one backend, no
fallback, and exits non-zero on failure (`OK <backend> WxH` on success). It's
the shared primitive the emulator + GPU gates run, and it works on a dev box
today (`backend_decode file.heic rust`).
