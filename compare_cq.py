#!/usr/bin/env python3

def parse_cq_trace(filename):
    splits = []
    with open(filename, 'r') as f:
        for line in f:
            if line.startswith('CQ_SPLIT'):
                parts = line.split()
                x0 = int(parts[1].split('=')[1])
                y0 = int(parts[2].split('=')[1])
                log2 = int(parts[3].split('=')[1])
                depth = int(parts[4].split('=')[1])
                split = int(parts[5].split('=')[1])
                splits.append((x0, y0, log2, depth, split))
    return splits

lib_splits = parse_cq_trace('libde265_cq_trace.txt')
rust_splits = parse_cq_trace('rust_cq_trace.txt')

print(f"libde265: {len(lib_splits)} CQ split decisions")
print(f"Rust:     {len(rust_splits)} CQ split decisions")
print()

# Find first divergence
divergence_idx = None
for i in range(min(len(lib_splits), len(rust_splits))):
    if lib_splits[i] != rust_splits[i]:
        divergence_idx = i
        break

if divergence_idx is None:
    if len(lib_splits) == len(rust_splits):
        print("PERFECT MATCH! All CQ split decisions are identical!")
    else:
        print(f"Matches up to {min(len(lib_splits), len(rust_splits))} decisions")
        print(f"Length mismatch: libde265 has {len(lib_splits)}, Rust has {len(rust_splits)}")
else:
    print(f"FIRST DIVERGENCE at decision #{divergence_idx}")
    print()
    lib = lib_splits[divergence_idx]
    rust = rust_splits[divergence_idx]
    print(f"libde265: x={lib[0]:3} y={lib[1]:3} log2={lib[2]} depth={lib[3]} split={lib[4]}")
    print(f"Rust:     x={rust[0]:3} y={rust[1]:3} log2={rust[2]} depth={rust[3]} split={rust[4]}")
    
    # Show context (previous 3 and next 3)
    print(f"\nContext (decisions {max(0, divergence_idx-3)} to {min(len(lib_splits), divergence_idx+4)}):")
    for i in range(max(0, divergence_idx-3), min(len(lib_splits), divergence_idx+4)):
        if i < len(rust_splits):
            match = "OK" if lib_splits[i] == rust_splits[i] else "!!"
            lib = lib_splits[i]
            rust = rust_splits[i]
            marker = ">>>" if i == divergence_idx else "   "
            print(f"{marker} #{i:3} {match}  lib:({lib[0]:3},{lib[1]:3}) log2={lib[2]} d={lib[3]} s={lib[4]}  rust:({rust[0]:3},{rust[1]:3}) log2={rust[2]} d={rust[3]} s={rust[4]}")
