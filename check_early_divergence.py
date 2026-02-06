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

print(f"Checking first 75 decisions:")
for i in range(min(75, len(lib_splits), len(rust_splits))):
    if lib_splits[i] != rust_splits[i]:
        print(f"DIVERGENCE at decision #{i}")
        print(f"  libde265: {lib_splits[i]}")
        print(f"  Rust:     {rust_splits[i]}")
        break
else:
    print("All first 75 decisions match!")
    print(f"Decision #74: {lib_splits[74]}")
    print(f"Decision #75 libde265: {lib_splits[75]}")
    print(f"Decision #75 Rust:     {rust_splits[75]}")
