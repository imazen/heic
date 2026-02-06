#!/usr/bin/env python3
import sys

# Parse split traces
def parse_split_trace(filename):
    splits = []
    with open(filename, 'r') as f:
        for line in f:
            if line.startswith('SPLIT'):
                parts = line.split()
                x0 = int(parts[1].split('=')[1])
                y0 = int(parts[2].split('=')[1])
                log2 = int(parts[3].split('=')[1])
                depth = int(parts[4].split('=')[1])
                split = int(parts[5].split('=')[1])
                splits.append((x0, y0, log2, depth, split))
    return splits

lib_splits = parse_split_trace('libde265_split_trace.txt')
rust_splits = parse_split_trace('rust_split_trace.txt')

print(f"libde265: {len(lib_splits)} split decisions")
print(f"Rust:     {len(rust_splits)} split decisions")
print()

print("First 10 decisions comparison:")
print()
for i in range(min(10, len(lib_splits), len(rust_splits))):
    lib = lib_splits[i]
    rust = rust_splits[i]
    
    match = "MATCH" if lib == rust else "DIFFER"
    print(f"#{i} {match}")
    print(f"  libde265: x={lib[0]:3} y={lib[1]:3} log2={lib[2]} depth={lib[3]} split={lib[4]}")
    print(f"  Rust:     x={rust[0]:3} y={rust[1]:3} log2={rust[2]} depth={rust[3]} split={rust[4]}")
    print()
