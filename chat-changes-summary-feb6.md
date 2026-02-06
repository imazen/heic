This is a disaster recovery plan to restore the Rust HEIC decoder to its best state (approximately "5/10" quality—recognizable but artifact-heavy) before the accidental `git checkout` wiped the progress.

Based on the chat history, here is the reconstructed list of changes required to restore the code.

### ⚠️ Crucial Warning regarding Scan Order
The chat concluded that **changing the 4x4 diagonal scan order tables** in `residual.rs` improved CABAC parity but **broke visual reconstruction** (swapping pixel X/Y positions).
*   **To restore 5/10 visual quality:** Do **NOT** apply the scan table reordering.
*   **To restore CABAC bin parity:** Apply the scan table reordering.
*   *Recommendation:* Apply all changes below, but skip the "Scan Order Tables" section in `residual.rs` first to check visual output.

---

### 1. Build Configuration (`Cargo.toml`)
*   **Edition:** Change to `edition = "2021"`.
*   **Dependencies:** Comment out or remove `heic-wasm-rs` (caused build failures).
*   **Features:** Ensure a feature flag (like `trace-coefficients`) exists if you plan to debug again.

### 2. Core CABAC Math (`src/hevc/cabac.rs`)
This was the most critical fix for arithmetic precision.
*   **Renormalization Bug:** In `decode_bin` (MPS path), change `self.range = scaled_range >> 7` to **`self.range = scaled_range >> 6`**.
*   **WPP Support:** Add a `pub fn reinit(&mut self)` method to handle Wavefront Parallel Processing row resets (reading 2 bytes to re-align).
*   **Context Init Values:** Update `INIT_VALUES` array:
    *   Fix **PART_MODE** (indices ~11-14): Ensure values match libde265 (specifically the first value was `154`, changed to `184`).
    *   Fix **CU_QP_DELTA_ABS**: Ensure values are `154, 154`.

### 3. Slice Header Parsing (`src/hevc/slice.rs`)
*   **Entry Points:** Modify the struct and parsing logic to **store the actual `entry_point_offsets`** (vector of u32/usize) instead of just skipping them/counting them. This is required for WPP navigation.

### 4. CTU Decoding Logic (`src/hevc/ctu.rs`)
This file had the most logic restructuring.

**A. WPP / Slice Loop:**
*   Rewrite `decode_slice` loop.
*   Implement check for **end of CTU row**: If row changes, perform CABAC `reinit()`.
*   Implement Context Save/Restore for WPP (saving state after the second CTU of a row).

**B. Intra Prediction (NxN vs 2Nx2N):**
*   **Syntax Order Fix:** In `decode_coding_unit` (NxN path), change the decode order.
    *   *Old:* Decode Flag, then Mode, then Flag, then Mode...
    *   *New:* Decode **all 4 `prev_intra_luma_pred_flag`s** first. THEN decode the modes (`mpm` or `rem`) for each PU.
*   **Mode Storage:**
    *   Add `intra_mode_map` to `SliceContext` (stores mode per 4x4 block).
    *   Implement `set_intra_mode(x, y, width, height, mode)`.
    *   Update `get_neighbor_intra_mode` to actually read from this map (it was a stub returning DC).
    *   Call `set_intra_mode` immediately after decoding a mode (both for 2Nx2N and inside the NxN loop).

**C. Transform Tree (Recursion & Chroma):**
*   **Signature Change:** Update `decode_transform_tree` to accept:
    *   `intra_split_flag` (bool)
    *   `chroma_mode` (IntraPredMode)
    *   `luma_modes` (Array of modes, not just one, to handle NxN PU selection).
*   **Split Logic:**
    *   In `split_transform_flag` decoding condition, add `&& !(intra_split_flag && trafo_depth == 0)`.
    *   If `intra_split_flag && trafo_depth == 0`, force `split = true` (implicit split).
*   **Chroma Handling:**
    *   In `decode_transform_tree_inner`: Pass `chroma_mode` down.
    *   In `decode_transform_unit_leaf`: Only decode chroma if `log2_size >= 3` (inherited logic).
    *   In the split path (parent): Decode chroma residuals *after* processing all 4 children.

**D. QP Delta:**
*   **Logic:** Implement `decode_cu_qp_delta_abs` (using Exp-Golomb).
*   **Call Site:** Call this in `decode_transform_unit_leaf` if `(cbf_luma || cbf_cb || cbf_cr) && !is_cu_qp_delta_coded`.
*   **Reset Logic:** Change the `is_cu_qp_delta_coded` reset condition in `decode_coding_quadtree`.
    *   *Correct Logic:* Reset when `log2_cb_size >= log2_ctb_size - diff_cu_qp_delta_depth` (usually log2=5 / 32x32 blocks), NOT 16x16.

### 5. Residual Decoding (`src/hevc/residual.rs`)
*   **Context Derivation:** In `calc_sig_coeff_flag_ctx`, remove the `.min(3)` clamp on the prefix lookup. This was causing desync in 32x32 blocks.
*   **Scan Order Logic:**
    *   Update `get_scan_order` to accept `c_idx`.
    *   For 4:2:0 Chroma (`c_idx > 0`), ensure 4x4 blocks use the correct scan (often diagonal) even if luma uses Mode 26 (Horizontal).
    *   *Note:* Ensure `get_scan_sub_block` uses the proper Horizontal/Vertical tables (this was likely fixed).

### 6. Inverse Transform (`src/hevc/transform.rs`)
*   **IDCT 32x32:** Fix symmetry logic.
    *   *Bug:* Using `col % 16`.
    *   *Fix:* Should be `31 - col` for the second half of the butterfly.

---

### Summary of "The Recognizable State"
To achieve the 5/10 result, you need:
1.  **CABAC `>> 6` fix.**
2.  **WPP `reinit` logic.**
3.  **NxN syntax order fix.**
4.  **Intra mode neighbor storage.**
5.  **Correct `cu_qp_delta` reset logic.**

*Avoid* re-applying the "Scan Table Reordering" in `residual.rs` initially, as that was identified as the specific change that turned the "poor but recognizable" image into "unrecognizable garbage."

Based on the comprehensive review of the markdown files generated during the debugging session (`CODE_STATE_TIMELINE.md`, `ROOT_CAUSE_FOUND.md`, `SLICE_INVESTIGATION_SUMMARY.md`, etc.), here is the secondary recovery document.

This document details specific, granular code changes that were identified in the intermediate reports but might be missed in a high-level summary. These are critical for restoring the **CABAC Bit Parity** state you achieved.

---

# 🛠️ Deep Recovery Log: Detailed Code Changes

**Restoration Target:** State at ~1:23 AM Feb 6 (High CABAC Parity / "Full Decode" State)
**Source:** Derived from intermediate debugging reports and trace analysis logs.

---

## 1. `src/hevc/cabac.rs` - The Context & Init Fixes
*Ref: `ROOT_CAUSE_FOUND.md`, `CABAC_PARITY_INVESTIGATION_SUMMARY.md`*

We identified that the Rust decoder's context array was misaligned with the H.265 spec/libde265.

*   **Context Offset Shift (Crucial for Parity):**
    *   **Action:** You must insert `SAO_MERGE_FLAG` (index 0) and `SAO_TYPE_IDX` (index 1) at the start of the `pub mod context` block.
    *   **Result:** `SPLIT_CU_FLAG` must move from index 0 to **index 2**.
    *   **Cascade:** **All** subsequent context constants must be incremented by 2.

*   **Init Value Corrections:**
    *   **Action:** Update `INIT_VALUES` array.
    *   **PART_MODE:** The values at indices ~11-14 (in new alignment) were incorrect. The first value must be changed from `154` to **`184`**.
    *   **CU_QP_DELTA_ABS:** Ensure the values for this context are `154, 154`.

*   **WPP Re-initialization:**
    *   **Action:** Add `pub fn reinit(&mut self)` to `CabacDecoder`.
    *   **Logic:** It needs to discard the current bit/byte buffer and read 2 fresh bytes from the current `byte_pos` to re-align, per WPP requirements.

---

## 2. `src/hevc/ctu.rs` - The Logic Engine
*Ref: `CODE_STATE_TIMELINE.md`, Trace Logs*

This file underwent massive changes (~995 lines) to fix logic flow and add debug infrastructure.

*   **Intra Mode Storage (The "Stub" Fix):**
    *   **Problem:** `get_neighbor_intra_mode` was a stub returning `DC`.
    *   **Fix:**
        1.  Add `intra_mode_map: Vec<u8>` to `SliceContext`.
        2.  Implement `set_intra_mode(x, y, w, h, mode)`.
        3.  **Critical:** Call `set_intra_mode` immediately after decoding a mode in both the `Part2Nx2N` path and inside the `PartNxN` loop.
        4.  Update `get_neighbor_intra_mode` to read from this map.

*   **NxN Syntax Order (Spec Compliance):**
    *   **Fix:** In `decode_coding_unit` for `PartNxN`:
        *   Loop 1: Decode **all 4** `prev_intra_luma_pred_flag`s.
        *   Loop 2: Decode the modes (`mpm` or `rem`) for each PU.
        *   *Note:* The previous code interleaved them (Flag, Mode, Flag, Mode), which caused immediate CABAC desync.

*   **Transform Tree & Intra Split:**
    *   **Fix:** In `decode_transform_tree`, pass `intra_split_flag` (true if PartNxN).
    *   **Logic:** If `intra_split_flag && trafo_depth == 0`:
        *   **Force** `split_transform_flag = true`.
        *   Do **not** decode the flag from the bitstream.
        *   Increment `max_trafo_depth` by 1.

*   **QP Delta Reset Logic:**
    *   **Fix:** In `decode_coding_quadtree`, change the `is_cu_qp_delta_coded` reset condition.
    *   **Correct Formula:** Reset if `log2_cb_size >= log2_ctb_size - diff_cu_qp_delta_depth` (typically reset at 32x32, NOT 16x16).

---

## 3. `src/hevc/residual.rs` - The "Min(3)" & Scan Order
*Ref: `CODE_STATE_TIMELINE.md`, `analyze_scan_order.py`*

This contains the changes that improved parity (to 110k bins) but broke visuals.

*   **The `.min(3)` Fix (Keep this):**
    *   **Location:** `decode_last_sig_coeff_prefix`.
    *   **Change:** Remove `.min(3)` clamp on the prefix context derivation.
    *   **Effect:** Allows `ctxIdxInc` to go up to 4 or higher for 32x32 blocks. This was required to match libde265's parsing of large TUs.

*   **The 4x4 Scan Order Reordering (The "Double-Edged Sword"):**
    *   **Location:** `SCAN_ORDER_4X4_DIAG` table.
    *   **Change:** The scan order was changed to match the H.265 spec's "Up-Right" anti-diagonal traversal (X goes 0->N within a diagonal).
    *   **Old (Visuals Good):** `(0,0), (1,0), (0,1)...`
    *   **New (Parity Good):** `(0,0), (0,1), (1,0)...` (Swapped positions 1 and 2).
    *   **Decision:** To restore **CABAC parity**, you must apply this change. To restore **Visuals**, you likely need to revert this *or* fix the way coefficients are written to the buffer to match this new scan order.

*   **Chroma Scan Logic:**
    *   **Change:** Updated `get_scan_order` to accept `c_idx`.
    *   **Logic:** Ensure 4:2:0 Chroma blocks (c_idx > 0) use the correct scan order (often Diagonal) even if the Luma mode (Angular 26) dictates Horizontal.

---

## 4. `src/hevc/transform.rs` - Mathematical Correctness
*Ref: `CODE_STATE_TIMELINE.md`*

*   **DCT32 Symmetry:**
    *   **Fix:** In `get_dct32_coef`:
    *   For even rows (`col >= 16`): Change `col % 16` to `31 - col`.
    *   For odd rows (`col >= 16`): Change logic to use `31 - col` and negate result.

---

## 5. `src/hevc/slice.rs` - Entry Points
*Ref: `SLICE_INVESTIGATION_SUMMARY.md`*

*   **Storage:** Change `entry_point_offsets` from being skipped/discarded to being stored in a `Vec<u32>` in the `SliceHeader`.
*   **Usage:** These offsets are required by the `ctu.rs` loop to seek the reader to the correct byte position at the start of every WPP row.

---

## Summary Checklist for Recovery

1.  [ ] **CABAC:** Add SAO contexts (0,1), shift others +2. Fix `reinit()`. Fix `>> 6` renormalization.
2.  [ ] **CTU:** Add `intra_mode_map`. Fix NxN decode order. Fix `cu_qp_delta` reset logic.
3.  [ ] **Residual:** Remove `.min(3)`. Update scan tables (decide on visual vs parity priority).
4.  [ ] **Slice:** Store entry points.
5.  [ ] **Transform:** Fix DCT32 symmetry.

Restoring these specific logic changes will return the codebase to the state where it successfully parsed 280/280 CTUs with high bin parity (but garbled output due to the scan order mismatch).