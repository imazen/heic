#!/usr/bin/env python3
"""Decode test_120x120.heic, save timestamped PPM, update scores.txt.

Usage:
    python decode_and_score.py <description> [--bins N]
    
Examples:
    python decode_and_score.py "baseline from origin/main" --bins 847
    python decode_and_score.py "fix SAO context"
"""

import subprocess
import sys
import os
import shutil
from datetime import datetime

def main():
    if len(sys.argv) < 2:
        print("Usage: python decode_and_score.py <description> [--bins N]")
        sys.exit(1)

    description = sys.argv[1]
    bins = "?"
    
    # Parse --bins argument
    for i, arg in enumerate(sys.argv):
        if arg == "--bins" and i + 1 < len(sys.argv):
            bins = sys.argv[i + 1]

    timestamp = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
    
    # Clean old trace files
    for f in ["rust_bins.txt", "rust_coeff_trace.txt"]:
        if os.path.exists(f):
            os.remove(f)

    # Build with tracing
    print("Building Rust decoder with tracing...")
    result = subprocess.run(
        ["cargo", "build", "--release", "--features", "trace-coefficients"],
        capture_output=True, text=True
    )
    if result.returncode != 0:
        print(f"Build failed:\n{result.stderr}")
        sys.exit(1)

    # Run decoder
    print("Decoding test_120x120.heic...")
    result = subprocess.run(
        [r".\target\release\heic-decoder.exe", "test_120x120.heic"],
        capture_output=True, text=True
    )
    print(result.stdout)
    if result.stderr:
        print(result.stderr)

    # Find output PPM (decoder may write output.ppm or similar)
    output_ppm = None
    for candidate in ["output.ppm", "rust_output.ppm", "decoded.ppm"]:
        if os.path.exists(candidate):
            output_ppm = candidate
            break

    if output_ppm:
        # Copy to timestamped location
        dest_name = f"{timestamp}_bin{bins}_{description.replace(' ', '_')[:40]}.ppm"
        dest_path = os.path.join("output", dest_name)
        shutil.copy2(output_ppm, dest_path)
        print(f"Saved: {dest_path}")
    else:
        print("Warning: No output PPM found")
        dest_name = "(no PPM)"

    # Update scores.txt
    scores_path = os.path.join("output", "scores.txt")
    padded_ts = timestamp.ljust(24)
    padded_bins = str(bins).ljust(5)
    padded_desc = description[:40].ljust(40)
    
    with open(scores_path, "a") as f:
        f.write(f"{padded_ts} | {padded_bins} | {padded_desc} |\n")
    
    print(f"Added entry to {scores_path}")
    print(f"Please view the PPM and add a score (0-10) in the last column.")

if __name__ == "__main__":
    main()
