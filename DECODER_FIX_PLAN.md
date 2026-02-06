# HEIC Decoder Fix Plan — Systematic Differential Tracing

**Created**: 2026-02-07  
**Goal**: Produce a correct decode of `test_120x120.heic` by systematically eliminating CABAC and spec divergences.

---

## Strategy Overview

### Lessons From Previous Attempts

1. **What worked**: Differential tracing (libde265 vs Rust bin-by-bin) achieved ~12,000 bins of parity with partial decode showing recognizable image.
2. **What failed**: Pushing past 12k bins introduced regressions. Fixes beyond that point degraded output instead of improving it. Chasing CABAC parity as the *sole* metric led into local optima.
3. **What also failed**: Ad-hoc spec-adherence fixes (SAO rewrite, PART_MODE init, cbf_luma condition) without trace verification — each fix changed bin counts but output remained gray/wrong.

### Two-Phase Approach

| Phase | Focus | Method | Success Metric |
|-------|-------|--------|----------------|
| **Phase 1** (bins 0–12,000) | CABAC arithmetic parity | Differential bin tracing against libde265 | 12,000+ consecutive matching bins; partial image visually recognizable |
| **Phase 2** (post-parity) | H.265 spec adherence | Manual spec audit of non-CABAC files (intra.rs, transform.rs, picture.rs, ctu.rs control flow) | Pixel-level match or ≤2 LSB diff vs reference PPM |

### Why This Order

- CABAC parity ensures we read the bitstream correctly (right syntax elements, right context indices, right values).
- Only *after* we read the bitstream correctly does it make sense to verify reconstruction (transforms, prediction, filtering).
- Fixing reconstruction before bitstream parsing is confirmed correct = building on sand.

---

## Phase 1: CABAC Bin Parity (Target: 12,000+ matching bins)

### Step 1.1 — Establish Baseline

1. **Reset to a clean code state**: Identify which commit produces the best starting point. Candidates:
   - `origin/main` (665dc65) — pre-fix baseline, all original code
   - `HEAD` (490c171) — includes SAO fix, PART_MODE init, SIG_COEFF_FLAG fixes
   - Individual cherrypicks from HEAD onto origin/main

2. **Generate reference trace**: Build libde265 with bin tracing (`-DTRACE_CABAC_OPS`), decode `test_120x120.heic`, capture `libde265_bins.txt`.

3. **Generate Rust trace**: Build with `--features trace-coefficients`, decode same file, capture `rust_bins.txt`.

4. **Run compare_bins.py**: Find first divergence point. Record baseline parity count.

### Step 1.2 — Iterative Fix Loop

```
REPEAT:
  1. Run compare_bins.py → find first divergence (bin #N)
  2. Examine context around bin #N in both traces
  3. Identify which syntax element is being decoded at bin #N
  4. Check H.265 spec for that syntax element's context derivation
  5. Fix Rust code
  6. Rebuild, re-run, re-compare
  7. Record new parity count, generate timestamped PPM, add to score file
  8. Commit with message: "fix: [element] context — parity now N bins"
UNTIL parity ≥ 12,000 bins
```

### Step 1.3 — Validation Checkpoint

At 12,000+ bins parity:
- Decode should produce partial image with recognizable content
- Score should be ≥ 4/10 (user visual assessment)
- CABAC state (range, value, bits_needed) should match libde265 exactly at bin 12,000

### Available Tools

| Tool | Purpose |
|------|---------|
| `compare_bins.py` | Compare context-coded bins between two trace files |
| `compare_traces.py` | Compare coefficient-level traces (CALL# format) |
| `compare_ci.py` | Compare context index traces |
| `compare_cq.py` | Compare context+quantized traces |
| `compare_semantic_ci.py` | Semantic comparison of context indices |
| `build_libde265_traced.ps1` | Build libde265 with tracing macros |
| `build_rust_traced.ps1` | Build Rust decoder with trace feature |
| `hevc-compare` crate | FFI tests comparing individual CABAC operations |

---

## Phase 2: H.265 Spec Adherence (Non-CABAC Files)

### Files to Audit (in order)

1. **`src/hevc/ctu.rs`** — Control flow: split_cu_flag, transform tree traversal, cbf flag propagation, pcm_flag handling
2. **`src/hevc/intra.rs`** — Intra prediction algorithms: DC, Planar, Angular modes; reference sample filtering (§8.4.4.2.3); strong intra smoothing
3. **`src/hevc/transform.rs`** — Inverse transforms: DST4 matrix, DCT4/8/16/32 matrices; dequantization scaling; clipping
4. **`src/hevc/picture.rs`** — YCbCr→RGB conversion: matrix coefficients, full-range vs limited-range, chroma upsampling
5. **`src/hevc/residual.rs`** — Sign data hiding, scan tables, coefficient decode sequencing

### Audit Method

For each file:
1. Read H.265 spec section (ITU-T file in repo)
2. Read libde265 reference implementation
3. Compare with Rust code line-by-line
4. Document deviations with spec section references
5. Fix deviations
6. Generate timestamped PPM, add to score file
7. Commit

### Known Issues to Verify

- [ ] Missing `intra_prediction_sample_filtering` (§8.4.4.2.3) — 3-tap smoothing before prediction
- [ ] Missing strong intra smoothing for 32×32 blocks
- [ ] pcm_flag decode missing from `decode_coding_unit` (Part2Nx2N path)
- [ ] SAO applied correctly (decode vs reconstruction)
- [ ] Deblocking filter absent (needed for visual quality, not bitstream parsing)

---

## Infrastructure

### Timestamped Output PPMs

Every decode attempt produces a PPM named:
```
output/YYYY-MM-DD_HH-MM-SS_binNNNNN_description.ppm
```
Example: `output/2026-02-07_14-30-22_bin00847_baseline.ppm`

This preserves a history of every output for regression comparison.

### Interactive Scoring File

File: `output/scores.txt`

Format:
```
# HEIC Decoder Output Scores
# Score 0-10: 0=garbage, 5=recognizable, 10=pixel-perfect
# Leave score blank, fill in after viewing PPM
#
# Timestamp              | Bins  | Description                    | Score
2026-02-07_14-30-22      | 847   | baseline from origin/main      |
2026-02-07_14-45-10      | 1203  | fix SAO context count          |
2026-02-07_15-12-44      | 3891  | fix sig_coeff_flag ctx offset  |
```

Agent checks this file periodically. User fills in score column after viewing PPM.

### Git Discipline

- Every fix gets its own commit: `fix: <element> — parity now N bins`
- Before each fix session: `git stash` or branch
- Tag milestones: `git tag parity-12k` when target reached

---

## Test Files

| File | Dimensions | CTUs | Notes |
|------|-----------|------|-------|
| `test_120x120.heic` | 120×120 | 4 (2×2 grid) | Primary test — small, fast iteration |
| `example.heic` | 1280×854 | 280 (20×14 grid) | Full test — use after 12k bins achieved |

### Reference Files

| File | Source | Purpose |
|------|--------|---------|
| `libde265_out.ppm` | libde265 decode | Ground truth PPM |
| `reference_rgb.ppm` | From reference decode | RGB reference |
| `reference_420.yuv` | From reference decode | Raw YUV reference |
| `test_120x120_ref.png` | ffmpeg/other decode | 120×120 reference |

---

## Success Criteria

### Phase 1 Complete When:
- [ ] ≥12,000 consecutive CABAC bins match libde265
- [ ] Output PPM shows recognizable image content (user score ≥ 4/10)
- [ ] No decoding errors or panics

### Phase 2 Complete When:
- [ ] Output PPM visually matches reference (user score ≥ 8/10)
- [ ] Per-pixel RGB diff: avg < 5, max < 30
- [ ] All 4 CTUs of test_120x120.heic decode correctly

### Ultimate Goal:
- [ ] Pixel-perfect or near-perfect decode of test_120x120.heic
- [ ] Acceptable decode of example.heic (1280×854)

---

## Risk Mitigation

1. **If parity stalls around 5k bins**: Check if the fix requires changes to context init tables. Compare INIT_VALUES array element-by-element against H.265 Table 9-4 through 9-42.
2. **If output degrades despite increasing parity**: A reconstruction bug exists. Checkpoint code, then bisect which fix caused regression.
3. **If libde265 trace format changes**: Update `compare_bins.py` parser. Keep parser modular.
4. **If stuck for >2 hours on a single divergence**: Document the state, commit WIP, move to next divergence or switch to Phase 2 audit.

---

## Quick Reference — Commands

```powershell
# Build libde265 traced
.\build_libde265_traced.ps1

# Build Rust traced
cargo build --release --features trace-coefficients

# Decode with Rust (generates rust_bins.txt + output PPM)
.\target\release\heic-decoder.exe test_120x120.heic

# Compare bins
python compare_bins.py libde265_bins.txt rust_bins.txt

# Generate timestamped PPM (wrapper — see Phase 1 details)
# Automatic: decode script copies output.ppm → output/TIMESTAMP_binNNNNN_desc.ppm
```
