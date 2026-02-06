#!/usr/bin/env python3
"""Compare libde265 and Rust coefficient traces to find divergence point."""

import sys
import re
from typing import List, Dict, Tuple, Optional

def parse_trace(filename: str) -> List[Dict]:
    """Parse trace file into structured data."""
    calls = []
    
    try:
        with open(filename, 'r', encoding='utf-8') as f:
            lines = f.readlines()
    except FileNotFoundError:
        print(f"ERROR: Trace file not found: {filename}")
        sys.exit(1)
    
    i = 0
    while i < len(lines):
        line = lines[i].strip()
        
        if line.startswith('CALL#'):
            # Parse call header
            match = re.match(
                r'CALL#(\d+) x0=(\d+) y0=(\d+) log2=(\d+) cIdx=(\d+) '
                r'scan=(\d+) last_x=(\d+) last_y=(\d+)',
                line
            )
            
            if match:
                call_data = {
                    'num': int(match.group(1)),
                    'x0': int(match.group(2)),
                    'y0': int(match.group(3)),
                    'log2': int(match.group(4)),
                    'c_idx': int(match.group(5)),
                    'scan': int(match.group(6)),
                    'last_x': int(match.group(7)),
                    'last_y': int(match.group(8)),
                }
                
                # Parse coefficients line
                if i + 1 < len(lines) and lines[i + 1].strip().startswith('COEFFS:'):
                    coeff_str = lines[i + 1].strip()[7:]
                    coeffs = []
                    for match in re.finditer(r'\[(\d+),(\d+)\]=(-?\d+)', coeff_str):
                        x = int(match.group(1))
                        y = int(match.group(2))
                        val = int(match.group(3))
                        coeffs.append((x, y, val))
                    call_data['coeffs'] = coeffs
                
                # Parse CABAC state
                if i + 2 < len(lines) and lines[i + 2].strip().startswith('CABAC_STATE:'):
                    match = re.match(
                        r'CABAC_STATE: range=(\d+) value=(\d+) '
                        r'bits_needed=(-?\d+) byte_pos=(\d+)',
                        lines[i + 2].strip()
                    )
                    if match:
                        call_data['cabac'] = {
                            'range': int(match.group(1)),
                            'value': int(match.group(2)),
                            'bits_needed': int(match.group(3)),
                            'byte_pos': int(match.group(4)),
                        }
                
                calls.append(call_data)
                i += 4  # Skip to next call
            else:
                i += 1
        else:
            i += 1
    
    return calls


def compare_calls(libde265_call: Dict, rust_call: Dict) -> List[str]:
    """Compare two calls and return list of differences."""
    diffs = []
    
    # Check metadata
    for key in ['x0', 'y0', 'log2', 'c_idx', 'scan', 'last_x', 'last_y']:
        if libde265_call.get(key) != rust_call.get(key):
            diffs.append(
                f"{key}: libde265={libde265_call.get(key)} "
                f"rust={rust_call.get(key)}"
            )
    
    # Check coefficients
    lib_coeffs = set(libde265_call.get('coeffs', []))
    rust_coeffs = set(rust_call.get('coeffs', []))
    
    if lib_coeffs != rust_coeffs:
        # Find specific differences
        for (x, y, lib_val) in lib_coeffs:
            rust_val = next((v for (rx, ry, v) in rust_coeffs if rx == x and ry == y), None)
            if rust_val is None:
                diffs.append(f"Coeff [{x},{y}]: libde265={lib_val} rust=missing")
            elif rust_val != lib_val:
                diffs.append(f"Coeff [{x},{y}]: libde265={lib_val} rust={rust_val}")
        
        for (x, y, rust_val) in rust_coeffs:
            if not any((lx == x and ly == y) for (lx, ly, _) in lib_coeffs):
                diffs.append(f"Coeff [{x},{y}]: libde265=missing rust={rust_val}")
    
    # Check CABAC state
    if 'cabac' in libde265_call and 'cabac' in rust_call:
        lib_cabac = libde265_call['cabac']
        rust_cabac = rust_call['cabac']
        
        for key in ['range', 'value', 'bits_needed', 'byte_pos']:
            if lib_cabac.get(key) != rust_cabac.get(key):
                diffs.append(
                    f"CABAC {key}: libde265={lib_cabac.get(key)} "
                    f"rust={rust_cabac.get(key)}"
                )
    
    return diffs


def main():
    if len(sys.argv) != 3:
        print("Usage: compare_traces.py <libde265_trace.txt> <rust_trace.txt>")
        print()
        print("Example:")
        print("  python compare_traces.py libde265_coeff_trace.txt rust_coeff_trace.txt")
        sys.exit(1)
    
    libde265_file = sys.argv[1]
    rust_file = sys.argv[2]
    
    print("🔍 Parsing libde265 trace...")
    libde265_calls = parse_trace(libde265_file)
    print(f"   Found {len(libde265_calls)} calls")
    
    print("🔍 Parsing Rust trace...")
    rust_calls = parse_trace(rust_file)
    print(f"   Found {len(rust_calls)} calls")
    
    if len(libde265_calls) == 0 or len(rust_calls) == 0:
        print("\n❌ ERROR: One or both trace files are empty or malformed")
        sys.exit(1)
    
    print("\n📊 Comparing traces...\n")
    
    first_divergence: Optional[int] = None
    divergence_count = 0
    
    for i in range(min(len(libde265_calls), len(rust_calls))):
        diffs = compare_calls(libde265_calls[i], rust_calls[i])
        
        if diffs:
            divergence_count += 1
            
            if first_divergence is None:
                first_divergence = i
                lib_call = libde265_calls[i]
                rust_call = rust_calls[i]
                
                print("🔴 FIRST DIVERGENCE at CALL#{}".format(i))
                print(f"   libde265: x0={lib_call['x0']} y0={lib_call['y0']} "
                      f"log2={lib_call['log2']} cIdx={lib_call['c_idx']}")
                print(f"   Rust:     x0={rust_call['x0']} y0={rust_call['y0']} "
                      f"log2={rust_call['log2']} cIdx={rust_call['c_idx']}")
                print()
                
                for diff in diffs[:10]:  # Show first 10 differences
                    print(f"   ❌ {diff}")
                
                if len(diffs) > 10:
                    print(f"   ... and {len(diffs) - 10} more differences")
                
                print()
            
            # Show first few divergences
            elif i < first_divergence + 5:
                print(f"CALL#{i}: {len(diffs)} difference(s)")
                for diff in diffs[:3]:
                    print(f"   - {diff}")
        else:
            # Match after divergence
            if first_divergence is not None and i == first_divergence + 1:
                print(f"✅ CALL#{i}: Coefficients match (possible lucky context)")
    
    # Summary
    print("\n" + "=" * 70)
    
    if first_divergence is None:
        print("✅ SUCCESS: All calls match perfectly!")
        print()
        print("The decoders produce identical coefficient outputs.")
    else:
        lib_call = libde265_calls[first_divergence]
        
        print("📊 Summary:")
        print(f"   First divergence: CALL#{first_divergence}")
        print(f"   Total divergences: {divergence_count} / {min(len(libde265_calls), len(rust_calls))}")
        print(f"   Total calls compared: {min(len(libde265_calls), len(rust_calls))}")
        
        if len(libde265_calls) != len(rust_calls):
            print(f"   ⚠️  Call count mismatch: libde265={len(libde265_calls)} rust={len(rust_calls)}")
        
        print()
        print("🎯 Focus debugging on:")
        print(f"   Call number: #{first_divergence}")
        print(f"   Position: ({lib_call['x0']}, {lib_call['y0']})")
        print(f"   Block size: {1 << lib_call['log2']}x{1 << lib_call['log2']}")
        print(f"   Component: {'Luma (Y)' if lib_call['c_idx'] == 0 else 'Chroma (Cb/Cr)'}")
        
        scan_names = ['Diagonal', 'Horizontal', 'Vertical']
        scan_idx = lib_call.get('scan', 0)
        print(f"   Scan type: {scan_names[scan_idx] if scan_idx < 3 else scan_idx}")
        
        if 'cabac' in lib_call:
            print(f"   CABAC byte position: {lib_call['cabac']['byte_pos']}")
        
        print()
        print("💡 Next steps:")
        print("   1. Review HEVC spec for this operation type")
        print("   2. Add detailed CABAC tracing for CALL#{}".format(first_divergence))
        print("   3. Compare operation-by-operation for this specific TU")
        print("   4. Check context derivation logic in Rust decoder")
    
    print("=" * 70)


if __name__ == '__main__':
    main()
