#!/usr/bin/env python3
"""Compare context indices (ci) between libde265 and Rust CABAC traces.

Maps ci= values to syntax element types (accounting for different base offsets)
and finds the first structural divergence where the decoders parse different elements.
"""

import re
import sys

# Syntax element ranges for libde265 (from contextmodel.h)
LIBDE265_RANGES = [
    (0, 0, "SAO_MERGE"),
    (1, 1, "SAO_TYPE"),
    (2, 4, "SPLIT_CU"),
    (5, 7, "CU_SKIP"),
    (8, 11, "PART_MODE"),
    (12, 12, "PREV_INTRA"),
    (13, 13, "INTRA_CHROMA"),
    (14, 15, "CBF_LUMA"),
    (16, 19, "CBF_CHROMA"),
    (20, 22, "SPLIT_TRANSFORM"),
    (23, 23, "CU_CHROMA_QP_OFF_FLAG"),
    (24, 24, "CU_CHROMA_QP_OFF_IDX"),
    (25, 42, "LAST_SIG_X"),
    (43, 60, "LAST_SIG_Y"),
    (61, 64, "CODED_SUB_BLOCK"),
    (65, 108, "SIG_COEFF"),
    (109, 132, "COEFF_GT1"),
    (133, 138, "COEFF_GT2"),
    (139, 140, "CU_QP_DELTA"),
    (141, 142, "TRANSFORM_SKIP"),
    (161, 161, "CU_TRANSQUANT_BYPASS"),
]

# Syntax element ranges for Rust (from cabac.rs context module)
RUST_RANGES = [
    (0, 2, "SPLIT_CU"),        # SPLIT_CU_FLAG = 0
    (3, 3, "CU_TRANSQUANT_BYPASS"),
    (4, 6, "CU_SKIP"),         # CU_SKIP_FLAG = 4
    (7, 8, "MERGE"),           # MERGE_FLAG = 7, MERGE_IDX = 8
    (9, 12, "PART_MODE"),      # PART_MODE = 9
    (13, 13, "PREV_INTRA"),    # PREV_INTRA_LUMA_PRED_FLAG = 13
    (14, 14, "INTRA_CHROMA"),  # INTRA_CHROMA_PRED_MODE = 14
    (28, 30, "SPLIT_TRANSFORM"),  # SPLIT_TRANSFORM_FLAG = 28
    (31, 32, "CBF_LUMA"),      # CBF_LUMA = 31
    (33, 36, "CBF_CHROMA"),    # CBF_CBCR = 33
    (40, 57, "LAST_SIG_X"),    # LAST_SIG_COEFF_X_PREFIX = 40
    (58, 75, "LAST_SIG_Y"),    # LAST_SIG_COEFF_Y_PREFIX = 58
    (76, 79, "CODED_SUB_BLOCK"),  # CODED_SUB_BLOCK_FLAG = 76
    (80, 123, "SIG_COEFF"),    # SIG_COEFF_FLAG = 80
    (124, 147, "COEFF_GT1"),   # COEFF_ABS_LEVEL_GREATER1_FLAG = 124
    (148, 153, "COEFF_GT2"),   # COEFF_ABS_LEVEL_GREATER2_FLAG = 148
    (154, 154, "SAO_MERGE"),   # SAO_MERGE_FLAG = 154
    (155, 155, "SAO_TYPE"),    # SAO_TYPE_IDX = 155
    (156, 157, "CU_QP_DELTA"), # CU_QP_DELTA_ABS = 156
]

def get_element(ci, ranges):
    """Map ci value to (element_name, offset_within_element)."""
    if ci < 0:
        return "UNKNOWN", -1
    for start, end, name in ranges:
        if start <= ci <= end:
            return name, ci - start
    return f"UNK({ci})", ci

def parse_all_entries(filename):
    """Extract all BIN#, BYP#, TRM# entries in order, even if mid-line."""
    entries = []
    bin_pattern = re.compile(
        r'BIN#(\d+)\s+ctx\s+r=(\d+)\s+v=(\d+)\s+bn=(-?\d+)\s+bin=(\d+)\s+s=(\d+)\s+m=(\d+)\s+ci=(-?\d+)'
    )
    byp_pattern = re.compile(r'BYP#(\d+)\s+r=(\d+)\s+v=(\d+)\s+bn=(-?\d+)\s+bin=(\d+)')
    trm_pattern = re.compile(r'TRM#(\d+)\s+r=(\d+)\s+v=(\d+)\s+bn=(-?\d+)\s+bin=(\d+)')
    with open(filename, 'r') as f:
        for line in f:
            # A single line can contain multiple entries (DEBUG + BYP# on same line)
            found = []
            for m in bin_pattern.finditer(line):
                found.append((m.start(), {
                    'type': 'BIN',
                    'counter': int(m.group(1)),
                    'r': int(m.group(2)),
                    'v': int(m.group(3)),
                    'bn': int(m.group(4)),
                    'bin': int(m.group(5)),
                    's': int(m.group(6)),
                    'm': int(m.group(7)),
                    'ci': int(m.group(8)),
                }))
            for m in byp_pattern.finditer(line):
                found.append((m.start(), {'type': 'BYP', 'counter': int(m.group(1))}))
            for m in trm_pattern.finditer(line):
                found.append((m.start(), {'type': 'TRM', 'counter': int(m.group(1))}))
            found.sort(key=lambda x: x[0])
            for _, entry in found:
                entries.append(entry)
    return entries
def main():
    ref_file = sys.argv[1] if len(sys.argv) > 1 else 'libde265_bins_heic.txt'
    test_file = sys.argv[2] if len(sys.argv) > 2 else 'rust_bins.txt'

    print(f"Reference: {ref_file}")
    print(f"Test:      {test_file}")
    print()

    ref_entries = parse_all_entries(ref_file)
    test_entries = parse_all_entries(test_file)

    print(f"Reference: {len(ref_entries)} total entries")
    print(f"Test:      {len(test_entries)} total entries")
    print()

    # Walk through entries in lockstep
    min_len = min(len(ref_entries), len(test_entries))
    
    first_type_mismatch = None
    first_element_mismatch = None
    first_state_mismatch = None
    first_counter_drift = None
    match_count = 0
    
    for i in range(min_len):
        r = ref_entries[i]
        t = test_entries[i]
        
        # Check counter drift (global counter should match if entry types match)
        if first_counter_drift is None and r['counter'] != t['counter']:
            first_counter_drift = i
        
        # Check entry type mismatch (BIN vs BYP vs TRM)
        if r['type'] != t['type']:
            if first_type_mismatch is None:
                first_type_mismatch = i
                print(f"TYPE DIVERGENCE at entry #{i}:")
                print(f"  ref: {r['type']}#{r['counter']}")
                print(f"  test: {t['type']}#{t['counter']}")
                break
            continue
        
        if r['type'] == 'BIN':
            # Check CABAC state match
            state_match = (r['r'] == t['r'] and r['v'] == t['v'] and
                          r['bn'] == t['bn'] and r['bin'] == t['bin'] and
                          r['s'] == t['s'] and r['m'] == t['m'])
            
            if not state_match and first_state_mismatch is None:
                first_state_mismatch = i
            
            # Check syntax element match
            if r['ci'] >= 0 and t['ci'] >= 0:
                r_elem, r_off = get_element(r['ci'], LIBDE265_RANGES)
                t_elem, t_off = get_element(t['ci'], RUST_RANGES)
                
                if r_elem != t_elem:
                    if first_element_mismatch is None:
                        first_element_mismatch = i
                elif r_off != t_off:
                    if first_element_mismatch is None:
                        first_element_mismatch = i
            elif r['ci'] >= 0 or t['ci'] >= 0:
                pass  # One unknown, can't compare
            
            if state_match:
                match_count += 1

    print(f"Matching entries (state match): {match_count}")
    print()
    
    if first_type_mismatch is not None:
        print(f"=== FIRST TYPE MISMATCH at entry #{first_type_mismatch} ===")
        start = max(0, first_type_mismatch - 10)
        end = min(min_len, first_type_mismatch + 5)
        for i in range(start, end):
            r = ref_entries[i]
            t = test_entries[i]
            marker = ">>>" if i == first_type_mismatch else "   "
            r_info = f"{r['type']}#{r['counter']}"
            t_info = f"{t['type']}#{t['counter']}"
            if r['type'] == 'BIN' and t['type'] == 'BIN':
                r_elem, r_off = get_element(r['ci'], LIBDE265_RANGES)
                t_elem, t_off = get_element(t['ci'], RUST_RANGES)
                r_info += f" ci={r['ci']}({r_elem}+{r_off})"
                t_info += f" ci={t['ci']}({t_elem}+{t_off})"
            print(f"{marker} [{i:5d}] ref={r_info:40s} test={t_info}")
        print()
    
    if first_element_mismatch is not None:
        print(f"=== FIRST ELEMENT MISMATCH at entry #{first_element_mismatch} ===")
        r = ref_entries[first_element_mismatch]
        t = test_entries[first_element_mismatch]
        r_elem, r_off = get_element(r['ci'], LIBDE265_RANGES)
        t_elem, t_off = get_element(t['ci'], RUST_RANGES)
        print(f"  ref:  BIN#{r['counter']} ci={r['ci']} -> {r_elem}+{r_off}")
        print(f"  test: BIN#{t['counter']} ci={t['ci']} -> {t_elem}+{t_off}")
        print()
        start = max(0, first_element_mismatch - 10)
        end = min(min_len, first_element_mismatch + 5)
        for i in range(start, end):
            r = ref_entries[i]
            t = test_entries[i]
            marker = ">>>" if i == first_element_mismatch else "   "
            if r['type'] == 'BIN' and t['type'] == 'BIN':
                r_elem, r_off = get_element(r['ci'], LIBDE265_RANGES)
                t_elem, t_off = get_element(t['ci'], RUST_RANGES)
                state = "OK" if (r['r'] == t['r'] and r['v'] == t['v'] and r['s'] == t['s']) else "DIFF"
                elem_match = "OK" if r_elem == t_elem and r_off == t_off else "ELEM_DIFF"
                print(f"{marker} [{i:5d}] BIN# ref={r['counter']:5d} ci={r['ci']:3d}({r_elem:20s}+{r_off:2d})  test={t['counter']:5d} ci={t['ci']:3d}({t_elem:20s}+{t_off:2d}) state={state} {elem_match}")
            else:
                print(f"{marker} [{i:5d}] ref={r['type']}#{r['counter']}  test={t['type']}#{t['counter']}")
        print()
    
    if first_state_mismatch is not None:
        print(f"=== FIRST STATE MISMATCH at entry #{first_state_mismatch} ===")
        r = ref_entries[first_state_mismatch]
        t = test_entries[first_state_mismatch]
        if r['type'] == 'BIN' and t['type'] == 'BIN':
            r_elem, r_off = get_element(r['ci'], LIBDE265_RANGES)
            t_elem, t_off = get_element(t['ci'], RUST_RANGES)
            print(f"  ref:  BIN#{r['counter']} r={r['r']} v={r['v']} bn={r['bn']} bin={r['bin']} s={r['s']} m={r['m']} ci={r['ci']}({r_elem}+{r_off})")
            print(f"  test: BIN#{t['counter']} r={t['r']} v={t['v']} bn={t['bn']} bin={t['bin']} s={t['s']} m={t['m']} ci={t['ci']}({t_elem}+{t_off})")
        print()
        start = max(0, first_state_mismatch - 15)
        end = min(min_len, first_state_mismatch + 5)
        for i in range(start, end):
            r = ref_entries[i]
            t = test_entries[i]
            marker = ">>>" if i == first_state_mismatch else "   "
            if r['type'] == 'BIN' and t['type'] == 'BIN':
                r_elem, r_off = get_element(r['ci'], LIBDE265_RANGES)
                t_elem, t_off = get_element(t['ci'], RUST_RANGES)
                state = "OK" if (r['r'] == t['r'] and r['v'] == t['v'] and r['s'] == t['s'] and r['m'] == t['m']) else "DIFF"
                print(f"{marker} [{i:5d}] BIN# ref={r['counter']:5d}(ci={r['ci']:3d} {r_elem:15s}+{r_off:2d} s={r['s']:2d} m={r['m']})  test={t['counter']:5d}(ci={t['ci']:3d} {t_elem:15s}+{t_off:2d} s={t['s']:2d} m={t['m']}) {state}")
            elif r['type'] == t['type']:
                print(f"{marker} [{i:5d}] {r['type']}# ref={r['counter']:5d}  test={t['counter']:5d}")
            else:
                print(f"{marker} [{i:5d}] ref={r['type']}#{r['counter']}  test={t['type']}#{t['counter']}")

    if first_type_mismatch is None and first_element_mismatch is None and first_state_mismatch is None:
        print(f"All {min_len} entries match perfectly!")

    if first_counter_drift is not None:
        print(f"\n=== FIRST COUNTER DRIFT at entry #{first_counter_drift} ===")
        r = ref_entries[first_counter_drift]
        t = test_entries[first_counter_drift]
        print(f"  ref: {r['type']}#{r['counter']}")
        print(f"  test: {t['type']}#{t['counter']}")
        print(f"  drift: {t['counter'] - r['counter']}")
        print()
        start = max(0, first_counter_drift - 5)
        end = min(min_len, first_counter_drift + 10)
        for i in range(start, end):
            r = ref_entries[i]
            t = test_entries[i]
            marker = ">>>" if i == first_counter_drift else "   "
            drift = t['counter'] - r['counter']
            r_info = f"{r['type']}#{r['counter']}"
            t_info = f"{t['type']}#{t['counter']}"
            if r['type'] == 'BIN':
                r_elem, r_off = get_element(r.get('ci', -1), LIBDE265_RANGES)
                r_info += f" ci={r.get('ci',-1)}({r_elem}+{r_off})"
            if t['type'] == 'BIN':
                t_elem, t_off = get_element(t.get('ci', -1), RUST_RANGES)
                t_info += f" ci={t.get('ci',-1)}({t_elem}+{t_off})"
            print(f"{marker} [{i:5d}] ref={r_info:45s} test={t_info:45s} drift={drift:+d}")

if __name__ == '__main__':
    main()
