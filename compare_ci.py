#!/usr/bin/env python3
"""Compare context indices (ci) between libde265 and Rust CABAC traces.

Finds the first bin where both decoders report ci >= 0 but the values differ.
This reveals structural divergence (different syntax elements being decoded).
"""

import re
import sys

def parse_bin_with_ci(line):
    """Parse a BIN# line and return (counter, r, v, bn, bin, s, m, ci)."""
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
    """Extract context-coded bins with ci values."""
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

    # Count bins where ref has ci >= 0
    ref_with_ci = sum(1 for b in ref_bins if b['ci'] >= 0)
    print(f"Reference bins with ci >= 0: {ref_with_ci}")
    print()

    # Find first r/v/bn/bin/s/m mismatch
    min_len = min(len(ref_bins), len(test_bins))
    first_state_mismatch = None
    first_ci_mismatch_both_known = None
    first_ci_mismatch_any = None
    ci_mismatches_with_ref_known = []

    for i in range(min_len):
        r = ref_bins[i]
        t = test_bins[i]

        # Check state match
        state_match = (r['r'] == t['r'] and r['v'] == t['v'] and
                      r['bn'] == t['bn'] and r['bin'] == t['bin'] and
                      r['s'] == t['s'] and r['m'] == t['m'])

        if not state_match and first_state_mismatch is None:
            first_state_mismatch = i

        # Check ci match when ref has ci >= 0
        if r['ci'] >= 0 and t['ci'] >= 0:
            if r['ci'] != t['ci'] and first_ci_mismatch_both_known is None:
                first_ci_mismatch_both_known = i

        if r['ci'] >= 0:
            if r['ci'] != t['ci']:
                ci_mismatches_with_ref_known.append(i)
                if first_ci_mismatch_any is None:
                    first_ci_mismatch_any = i

    print(f"First state (r/v/bn/bin/s/m) mismatch: ctx bin #{first_state_mismatch}")
    if first_state_mismatch is not None:
        r = ref_bins[first_state_mismatch]
        t = test_bins[first_state_mismatch]
        print(f"  Ref  BIN#{r['counter']}: r={r['r']} v={r['v']} bn={r['bn']} bin={r['bin']} s={r['s']} m={r['m']} ci={r['ci']}")
        print(f"  Test BIN#{t['counter']}: r={t['r']} v={t['v']} bn={t['bn']} bin={t['bin']} s={t['s']} m={t['m']} ci={t['ci']}")
    print()

    print(f"First ci mismatch (both ci >= 0): ctx bin #{first_ci_mismatch_both_known}")
    if first_ci_mismatch_both_known is not None:
        r = ref_bins[first_ci_mismatch_both_known]
        t = test_bins[first_ci_mismatch_both_known]
        print(f"  Ref  BIN#{r['counter']}: ci={r['ci']} r={r['r']} v={r['v']} bn={r['bn']} s={r['s']} m={r['m']}")
        print(f"  Test BIN#{t['counter']}: ci={t['ci']} r={t['r']} v={t['v']} bn={t['bn']} s={t['s']} m={t['m']}")
    print()

    print(f"First ci mismatch (ref ci >= 0): ctx bin #{first_ci_mismatch_any}")
    if first_ci_mismatch_any is not None:
        r = ref_bins[first_ci_mismatch_any]
        t = test_bins[first_ci_mismatch_any]
        print(f"  Ref  BIN#{r['counter']}: ci={r['ci']} r={r['r']} v={r['v']} bn={r['bn']} s={r['s']} m={r['m']}")
        print(f"  Test BIN#{t['counter']}: ci={t['ci']} r={t['r']} v={t['v']} bn={t['bn']} s={t['s']} m={t['m']}")
    print()

    # Show all ci mismatches where ref has ci >= 0 (first 20)
    if ci_mismatches_with_ref_known:
        print(f"CI mismatches where ref has ci >= 0 (first 20 of {len(ci_mismatches_with_ref_known)}):")
        for idx in ci_mismatches_with_ref_known[:20]:
            r = ref_bins[idx]
            t = test_bins[idx]
            state_match = "MATCH" if (r['r'] == t['r'] and r['v'] == t['v'] and r['bn'] == t['bn']) else "DIFF"
            print(f"  ctx#{idx} BIN#{r['counter']}: ref_ci={r['ci']:3d} test_ci={t['ci']:3d} state={state_match}")
    else:
        print("No ci mismatches found where ref ci >= 0.")

    # Show distribution of ci values in ref around the state divergence
    if first_state_mismatch is not None:
        print(f"\n--- Bins around state divergence (ctx bin #{first_state_mismatch}) ---")
        start = max(0, first_state_mismatch - 10)
        end = min(min_len, first_state_mismatch + 5)
        for i in range(start, end):
            r = ref_bins[i]
            t = test_bins[i]
            marker = ">>>" if i == first_state_mismatch else "   "
            state = "OK" if (r['r'] == t['r'] and r['v'] == t['v']) else "DIFF"
            ci_info = f"ref_ci={r['ci']:3d} test_ci={t['ci']:3d}" + (" MISMATCH" if r['ci'] != t['ci'] else "")
            print(f"{marker} ctx#{i:5d} BIN#{r['counter']:5d}: {ci_info} state={state}")

if __name__ == '__main__':
    main()
