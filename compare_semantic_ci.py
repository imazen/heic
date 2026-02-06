#!/usr/bin/env python3
"""Compare CABAC traces with semantic context mapping between libde265 and Rust.

Maps different context numbering schemes to canonical names, then finds
the first bin where both decoders are decoding DIFFERENT syntax elements.
"""

import re
import sys

def build_libde265_map():
    """Build libde265 ci -> (category, sub_index) mapping."""
    m = {}
    # From libde265/contextmodel.h
    m[0] = ("SAO_MERGE_FLAG", 0)
    m[1] = ("SAO_TYPE_IDX", 0)
    for i in range(3): m[2+i] = ("SPLIT_CU_FLAG", i)
    for i in range(3): m[5+i] = ("CU_SKIP_FLAG", i)
    for i in range(4): m[8+i] = ("PART_MODE", i)
    m[12] = ("PREV_INTRA_LUMA_PRED_FLAG", 0)
    m[13] = ("INTRA_CHROMA_PRED_MODE", 0)
    for i in range(2): m[14+i] = ("CBF_LUMA", i)
    for i in range(4): m[16+i] = ("CBF_CHROMA", i)
    for i in range(3): m[20+i] = ("SPLIT_TRANSFORM_FLAG", i)
    m[23] = ("CU_CHROMA_QP_OFFSET_FLAG", 0)
    m[24] = ("CU_CHROMA_QP_OFFSET_IDX", 0)
    for i in range(18): m[25+i] = ("LAST_SIG_COEFF_X_PREFIX", i)
    for i in range(18): m[43+i] = ("LAST_SIG_COEFF_Y_PREFIX", i)
    for i in range(4): m[61+i] = ("CODED_SUB_BLOCK_FLAG", i)
    for i in range(44): m[65+i] = ("SIG_COEFF_FLAG", i)
    for i in range(24): m[109+i] = ("COEFF_ABS_LEVEL_GREATER1_FLAG", i)
    for i in range(6): m[133+i] = ("COEFF_ABS_LEVEL_GREATER2_FLAG", i)
    for i in range(2): m[139+i] = ("CU_QP_DELTA_ABS", i)
    for i in range(2): m[141+i] = ("TRANSFORM_SKIP_FLAG", i)
    for i in range(2): m[143+i] = ("RDPCM_FLAG", i)
    for i in range(2): m[145+i] = ("RDPCM_DIR", i)
    m[147] = ("MERGE_FLAG", 0)
    m[148] = ("MERGE_IDX", 0)
    m[149] = ("PRED_MODE_FLAG", 0)
    for i in range(2): m[150+i] = ("ABS_MVD_GREATER01_FLAG", i)
    m[152] = ("MVP_LX_FLAG", 0)
    m[153] = ("RQT_ROOT_CBF", 0)
    for i in range(2): m[154+i] = ("REF_IDX_LX", i)
    for i in range(5): m[156+i] = ("INTER_PRED_IDC", i)
    m[161] = ("CU_TRANSQUANT_BYPASS_FLAG", 0)
    for i in range(8): m[162+i] = ("LOG2_RES_SCALE_ABS_PLUS1", i)
    for i in range(2): m[170+i] = ("RES_SCALE_SIGN_FLAG", i)
    return m

def build_rust_map():
    """Build Rust ci -> (category, sub_index) mapping."""
    m = {}
    m[0] = ("SAO_MERGE_FLAG", 0)
    m[1] = ("SAO_TYPE_IDX", 0)
    for i in range(3): m[2+i] = ("SPLIT_CU_FLAG", i)
    m[5] = ("CU_TRANSQUANT_BYPASS_FLAG", 0)
    for i in range(3): m[6+i] = ("CU_SKIP_FLAG", i)
    m[9] = ("PALETTE_MODE_FLAG", 0)
    m[10] = ("PRED_MODE_FLAG", 0)
    for i in range(4): m[11+i] = ("PART_MODE", i)
    m[15] = ("PREV_INTRA_LUMA_PRED_FLAG", 0)
    m[16] = ("INTRA_CHROMA_PRED_MODE", 0)
    for i in range(5): m[17+i] = ("INTER_PRED_IDC", i)
    m[22] = ("MERGE_FLAG", 0)
    m[23] = ("MERGE_IDX", 0)
    m[24] = ("MVP_LX_FLAG", 0)
    for i in range(2): m[25+i] = ("REF_IDX_LX", i)
    for i in range(2): m[27+i] = ("ABS_MVD_GREATER01_FLAG", i)  # GREATER0 and GREATER1
    m[29] = ("ABS_MVD_GREATER1_FLAG", 0)
    for i in range(3): m[30+i] = ("SPLIT_TRANSFORM_FLAG", i)
    for i in range(2): m[33+i] = ("CBF_LUMA", i)
    for i in range(5): m[35+i] = ("CBF_CHROMA", i)
    for i in range(2): m[40+i] = ("TRANSFORM_SKIP_FLAG", i)
    for i in range(18): m[42+i] = ("LAST_SIG_COEFF_X_PREFIX", i)
    for i in range(18): m[60+i] = ("LAST_SIG_COEFF_Y_PREFIX", i)
    for i in range(4): m[78+i] = ("CODED_SUB_BLOCK_FLAG", i)
    for i in range(44): m[82+i] = ("SIG_COEFF_FLAG", i)
    for i in range(24): m[126+i] = ("COEFF_ABS_LEVEL_GREATER1_FLAG", i)
    for i in range(6): m[150+i] = ("COEFF_ABS_LEVEL_GREATER2_FLAG", i)
    for i in range(2): m[156+i] = ("CU_QP_DELTA_ABS", i)
    m[158] = ("TRANSFORM_SKIP_FLAG_QP", 0)
    m[159] = ("TRANSFORM_SKIP_FLAG_QP", 1)
    m[160] = ("CU_CHROMA_QP_OFFSET_FLAG", 0)
    m[161] = ("CU_CHROMA_QP_OFFSET_IDX", 0)
    for i in range(8): m[162+i] = ("LOG2_RES_SCALE_ABS_PLUS1", i)
    for i in range(2): m[170+i] = ("RES_SCALE_SIGN_FLAG", i)
    return m

def parse_bin_with_ci(line):
    line = line.strip()
    m = re.match(
        r'BIN#(\d+)\s+ctx\s+r=(\d+)\s+v=(\d+)\s+bn=(-?\d+)\s+bin=(\d+)\s+s=(\d+)\s+m=(\d+)\s+ci=(-?\d+)',
        line
    )
    if m:
        return {
            'counter': int(m.group(1)),
            'r': int(m.group(2)),
            'v': int(m.group(3)),
            'bn': int(m.group(4)),
            'bin': int(m.group(5)),
            's': int(m.group(6)),
            'm': int(m.group(7)),
            'ci': int(m.group(8)),
        }
    return None

def extract_ctx_bins(filename):
    bins = []
    with open(filename, 'r') as f:
        for line in f:
            if line.startswith('BIN#'):
                parsed = parse_bin_with_ci(line.strip())
                if parsed:
                    bins.append(parsed)
    return bins

def main():
    ref_file = sys.argv[1] if len(sys.argv) > 1 else 'libde265_trace_50k_fresh.txt'
    test_file = sys.argv[2] if len(sys.argv) > 2 else 'rust_trace_50k_v6.txt'

    print(f"Reference: {ref_file}")
    print(f"Test:      {test_file}")
    print()

    ref_bins = extract_ctx_bins(ref_file)
    test_bins = extract_ctx_bins(test_file)

    print(f"Reference: {len(ref_bins)} context-coded bins")
    print(f"Test:      {len(test_bins)} context-coded bins")
    print()

    lib_map = build_libde265_map()
    rust_map = build_rust_map()

    min_len = min(len(ref_bins), len(test_bins))
    first_category_mismatch = None
    first_sub_mismatch = None
    first_state_mismatch = None
    category_mismatches = 0
    sub_mismatches = 0
    comparable = 0

    for i in range(min_len):
        r = ref_bins[i]
        t = test_bins[i]

        state_match = (r['r'] == t['r'] and r['v'] == t['v'] and
                      r['bn'] == t['bn'] and r['bin'] == t['bin'] and
                      r['s'] == t['s'] and r['m'] == t['m'])

        if not state_match and first_state_mismatch is None:
            first_state_mismatch = i

        if r['ci'] >= 0 and t['ci'] >= 0:
            ref_sem = lib_map.get(r['ci'], (f"UNKNOWN_{r['ci']}", 0))
            test_sem = rust_map.get(t['ci'], (f"UNKNOWN_{t['ci']}", 0))
            comparable += 1

            # Category mismatch = different syntax element type
            if ref_sem[0] != test_sem[0]:
                category_mismatches += 1
                if first_category_mismatch is None:
                    first_category_mismatch = i
                    print(f"FIRST CATEGORY MISMATCH at ctx bin #{i} (BIN#{r['counter']}):")
                    print(f"  Ref:  ci={r['ci']:3d} -> {ref_sem[0]}[{ref_sem[1]}]")
                    print(f"  Test: ci={t['ci']:3d} -> {test_sem[0]}[{test_sem[1]}]")
                    print(f"  State: {'MATCH' if state_match else 'DIFF'}")
                    print()

            # Sub-index mismatch = same syntax element, different context derivation
            elif ref_sem[1] != test_sem[1]:
                sub_mismatches += 1
                if first_sub_mismatch is None:
                    first_sub_mismatch = i
                    print(f"FIRST SUB-INDEX MISMATCH at ctx bin #{i} (BIN#{r['counter']}):")
                    print(f"  Ref:  ci={r['ci']:3d} -> {ref_sem[0]}[{ref_sem[1]}]")
                    print(f"  Test: ci={t['ci']:3d} -> {test_sem[0]}[{test_sem[1]}]")
                    print(f"  State: {'MATCH' if state_match else 'DIFF'}")
                    print()

    print("=" * 70)
    print(f"Comparable bins (both ci >= 0): {comparable}")
    print(f"Category mismatches (different syntax element): {category_mismatches}")
    print(f"Sub-index mismatches (same element, different ctx): {sub_mismatches}")
    print(f"First state divergence: ctx bin #{first_state_mismatch}")
    print()

    # Show bins around first category mismatch if exists
    if first_category_mismatch is not None:
        print(f"--- Context around category mismatch (ctx bin #{first_category_mismatch}) ---")
        start = max(0, first_category_mismatch - 5)
        end = min(min_len, first_category_mismatch + 10)
        for i in range(start, end):
            r = ref_bins[i]
            t = test_bins[i]
            ref_sem = lib_map.get(r['ci'], (f"UNK_{r['ci']}", 0)) if r['ci'] >= 0 else ("ci=-1", -1)
            test_sem = rust_map.get(t['ci'], (f"UNK_{t['ci']}", 0)) if t['ci'] >= 0 else ("ci=-1", -1)
            state_ok = "OK" if (r['r'] == t['r'] and r['s'] == t['s'] and r['m'] == t['m']) else "DIFF"
            marker = ">>>" if i == first_category_mismatch else "   "

            ref_label = f"{ref_sem[0]}[{ref_sem[1]}]" if ref_sem[1] >= 0 else ref_sem[0]
            test_label = f"{test_sem[0]}[{test_sem[1]}]" if test_sem[1] >= 0 else test_sem[0]

            sem_match = "=" if ref_sem == test_sem else ("~" if ref_sem[0] == test_sem[0] else "X")
            print(f"{marker} ctx#{i:5d} BIN#{r['counter']:5d}: ref={ref_label:40s} test={test_label:40s} {sem_match} state={state_ok}")

    # Also show bins around first sub-index mismatch
    if first_sub_mismatch is not None and first_sub_mismatch != first_category_mismatch:
        print(f"\n--- Context around sub-index mismatch (ctx bin #{first_sub_mismatch}) ---")
        start = max(0, first_sub_mismatch - 3)
        end = min(min_len, first_sub_mismatch + 5)
        for i in range(start, end):
            r = ref_bins[i]
            t = test_bins[i]
            ref_sem = lib_map.get(r['ci'], (f"UNK_{r['ci']}", 0)) if r['ci'] >= 0 else ("ci=-1", -1)
            test_sem = rust_map.get(t['ci'], (f"UNK_{t['ci']}", 0)) if t['ci'] >= 0 else ("ci=-1", -1)
            state_ok = "OK" if (r['r'] == t['r'] and r['s'] == t['s'] and r['m'] == t['m']) else "DIFF"
            marker = ">>>" if i == first_sub_mismatch else "   "

            ref_label = f"{ref_sem[0]}[{ref_sem[1]}]" if ref_sem[1] >= 0 else ref_sem[0]
            test_label = f"{test_sem[0]}[{test_sem[1]}]" if test_sem[1] >= 0 else test_sem[0]

            sem_match = "=" if ref_sem == test_sem else ("~" if ref_sem[0] == test_sem[0] else "X")
            print(f"{marker} ctx#{i:5d} BIN#{r['counter']:5d}: ref={ref_label:40s} test={test_label:40s} {sem_match} state={state_ok}")

if __name__ == '__main__':
    main()
