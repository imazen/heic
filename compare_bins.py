#!/usr/bin/env python3
"""Compare CABAC bin traces between libde265 and Rust decoder.

Extracts context-coded bins (BIN#) from both traces and finds the first divergence.
Bypass bins are counted but not compared directly (libde265 uses batch bypass).
"""

import re
import sys

def parse_bin_line(line):
    """Parse a BIN# line and return a dict of values."""
    line = line.strip()
    # BIN#N ctx r=R v=V bn=BN bin=B s=S m=M
    m = re.match(r'BIN#(\d+)\s+ctx\s+r=(\d+)\s+v=(\d+)\s+bn=(-?\d+)\s+bin=(\d+)\s+s=(\d+)\s+m=(\d+)', line)
    if m:
        return {
            'counter': int(m.group(1)),
            'type': 'ctx',
            'range': int(m.group(2)),
            'value': int(m.group(3)),
            'bits_needed': int(m.group(4)),
            'bin': int(m.group(5)),
            'state': int(m.group(6)),
            'mps': int(m.group(7)),
        }
    return None

def parse_bypass_line(line):
    """Parse a BYP# line."""
    line = line.strip()
    m = re.match(r'BYP#(\d+)\s+r=(\d+)\s+v=(\d+)\s+bn=(-?\d+)\s+bin=(\d+)', line)
    if m:
        return {
            'counter': int(m.group(1)),
            'type': 'byp',
            'range': int(m.group(2)),
            'value': int(m.group(3)),
            'bits_needed': int(m.group(4)),
            'bin': int(m.group(5)),
        }
    return None

def parse_terminate_line(line):
    """Parse a TRM# line."""
    line = line.strip()
    m = re.match(r'TRM#(\d+)\s+r=(\d+)\s+v=(\d+)\s+bn=(-?\d+)\s+bin=(\d+)', line)
    if m:
        return {
            'counter': int(m.group(1)),
            'type': 'trm',
            'range': int(m.group(2)),
            'value': int(m.group(3)),
            'bits_needed': int(m.group(4)),
            'bin': int(m.group(5)),
        }
    return None

def extract_bins(filename):
    """Extract all bin entries from a trace file."""
    ctx_bins = []
    all_bins = []
    with open(filename, 'r') as f:
        for line in f:
            line = line.strip()
            parsed = None
            if line.startswith('BIN#'):
                parsed = parse_bin_line(line)
            elif line.startswith('BYP#'):
                parsed = parse_bypass_line(line)
            elif line.startswith('TRM#'):
                parsed = parse_terminate_line(line)
            
            if parsed:
                all_bins.append(parsed)
                if parsed['type'] == 'ctx':
                    ctx_bins.append(parsed)
    
    return ctx_bins, all_bins

def main():
    ref_file = sys.argv[1] if len(sys.argv) > 1 else 'libde265_bins.txt'
    test_file = sys.argv[2] if len(sys.argv) > 2 else 'rust_bins.txt'
    
    print(f"Reference: {ref_file}")
    print(f"Test:      {test_file}")
    print()
    
    ref_ctx, ref_all = extract_bins(ref_file)
    test_ctx, test_all = extract_bins(test_file)
    
    print(f"Reference: {len(ref_ctx)} context-coded bins, {len(ref_all)} total bins")
    print(f"Test:      {len(test_ctx)} context-coded bins, {len(test_all)} total bins")
    print()
    
    # Compare context-coded bins
    min_len = min(len(ref_ctx), len(test_ctx))
    first_diff = None
    
    for i in range(min_len):
        r = ref_ctx[i]
        t = test_ctx[i]
        
        # Compare key fields
        match = (r['range'] == t['range'] and 
                 r['value'] == t['value'] and 
                 r['bits_needed'] == t['bits_needed'] and
                 r['bin'] == t['bin'] and
                 r['state'] == t['state'] and
                 r['mps'] == t['mps'])
        
        if not match:
            first_diff = i
            print(f"=== FIRST DIVERGENCE at context bin #{i} ===")
            print(f"  Ref  (counter={r['counter']}): r={r['range']} v={r['value']} bn={r['bits_needed']} bin={r['bin']} s={r['state']} m={r['mps']}")
            print(f"  Test (counter={t['counter']}): r={t['range']} v={t['value']} bn={t['bits_needed']} bin={t['bin']} s={t['state']} m={t['mps']}")
            print()
            
            # Show context around the divergence in ref
            print("--- Reference bins around divergence ---")
            for j in range(max(0, i-3), min(len(ref_ctx), i+5)):
                b = ref_ctx[j]
                marker = " >>>" if j == i else "    "
                print(f"{marker} ctx#{j} (#{b['counter']}): r={b['range']} v={b['value']} bn={b['bits_needed']} bin={b['bin']} s={b['state']} m={b['mps']}")
            
            print()
            print("--- Test bins around divergence ---")
            for j in range(max(0, i-3), min(len(test_ctx), i+5)):
                b = test_ctx[j]
                marker = " >>>" if j == i else "    "
                print(f"{marker} ctx#{j} (#{b['counter']}): r={b['range']} v={b['value']} bn={b['bits_needed']} bin={b['bin']} s={b['state']} m={b['mps']}")
            
            # Also show all bins around the divergence point
            print()
            ref_counter = r['counter']
            test_counter = t['counter']
            
            print(f"--- All reference bins near counter #{ref_counter} ---")
            for b in ref_all:
                if abs(b['counter'] - ref_counter) <= 5:
                    marker = " >>>" if b['counter'] == ref_counter else "    "
                    if b['type'] == 'ctx':
                        print(f"{marker} BIN#{b['counter']} r={b['range']} v={b['value']} bn={b['bits_needed']} bin={b['bin']} s={b['state']} m={b['mps']}")
                    else:
                        print(f"{marker} {b['type'].upper()}#{b['counter']} r={b['range']} v={b['value']} bn={b['bits_needed']} bin={b['bin']}")
            
            print()
            print(f"--- All test bins near counter #{test_counter} ---")
            for b in test_all:
                if abs(b['counter'] - test_counter) <= 5:
                    marker = " >>>" if b['counter'] == test_counter else "    "
                    if b['type'] == 'ctx':
                        print(f"{marker} BIN#{b['counter']} r={b['range']} v={b['value']} bn={b['bits_needed']} bin={b['bin']} s={b['state']} m={b['mps']}")
                    else:
                        print(f"{marker} {b['type'].upper()}#{b['counter']} r={b['range']} v={b['value']} bn={b['bits_needed']} bin={b['bin']}")
            
            break
    
    if first_diff is None:
        if len(ref_ctx) == len(test_ctx):
            print(f"All {min_len} context-coded bins match perfectly!")
        else:
            print(f"First {min_len} context-coded bins match, but counts differ:")
            print(f"  Reference has {len(ref_ctx)}, Test has {len(test_ctx)}")
    else:
        # Count matching bins before divergence
        print(f"\n{first_diff} context-coded bins matched before divergence.")

if __name__ == '__main__':
    main()
