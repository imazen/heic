#!/usr/bin/env python3
"""Analyze the scan order change and its impact on coefficient positioning"""

# OLD scan order (from git diff - possibly wrong but was producing recognizable output)
old_scan = [
    (0, 0), (1, 0), (0, 1), (2, 0), (1, 1), (0, 2), (3, 0), (2, 1),
    (1, 2), (0, 3), (3, 1), (2, 2), (1, 3), (3, 2), (2, 3), (3, 3),
]

# NEW scan order (from current code - matches H.265 anti-diagonal)
new_scan = [
    (0, 0), (0, 1), (1, 0), (0, 2), (1, 1), (2, 0), (0, 3), (1, 2),
    (2, 1), (3, 0), (1, 3), (2, 2), (3, 1), (2, 3), (3, 2), (3, 3),
]

print('SCAN ORDER CHANGE ANALYSIS')
print('=' * 80)
print()
print('OLD ORDER (produced recognizable output at block 300):')
for i in range(0, 16, 4):
    row = old_scan[i:i+4]
    print(f'  Scan#  {i:2d}-{i+3:2d}: ' + ', '.join(f'({x},{y})' for x,y in row))
print()
print('NEW ORDER (claimed to be H.265 spec-compliant):')
for i in range(0, 16, 4):
    row = new_scan[i:i+4]
    print(f'  Scan# {i:2d}-{i+3:2d}: ' + ', '.join(f'({x},{y})' for x,y in row))
print()
print('CRITICAL DIFFERENCES:')
print('-' * 80)

# Find differing positions
for i in range(16):
    if old_scan[i] != new_scan[i]:
        old_x, old_y = old_scan[i]
        new_x, new_y = new_scan[i]
        print(f"  Scan position #{i:2d}:")
        print(f"    OLD: coefficient[{old_x},{old_y}] (scan index {i})")
        print(f"    NEW: coefficient[{new_x},{new_y}] (scan index {i})")
        print()

print()
print('IMPACT ON RECONSTRUCTION:')
print('-' * 80)
print("""
  1. CABAC decodes coefficients in scan order (positions 0, 1, 2, ...)
  2. Each position maps to an (x,y) coordinate in the 4x4 block
  
  OLD: scan_position_0 -> pixel(0,0), position_1 -> pixel(1,0), etc.
  NEW: scan_position_0 -> pixel(0,0), position_1 -> pixel(0,1), etc.
  
  PROBLEM: If CABAC bin stream is the same, but scan positions now
  point to different pixels, coefficients get placed in wrong pixels!
  
  EXAMPLE: Coefficient value 23 was going to position (1,0) before,
  now it goes to position (0,1) - catastrophic mispositioning!

  This explains:
  ✓ Why prediction=128 and residual=23 gives output=151 in Rust
  ✓ Why libde265 gets output=168 (prediction=145 after filtering)
  ✓ Why earlier block-300 state showed recognizable output
  ✓ Why current "full decode" produces garbage
  
  HYPOTHESIS: The encoder used the OLD scan order, but we changed
  to the NEW order. Now coefficients go to the wrong pixel positions,
  resulting in garbage output.
""")
