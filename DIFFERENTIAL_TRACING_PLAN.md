# Differential Tracing Plan - libde265 vs Rust HEVC Decoder

**Goal**: Find the exact point where coefficient decoding diverges between libde265 and the Rust implementation by adding synchronized tracing to both decoders.

**Strategy**: Trace at coefficient granularity (not end-to-end metrics) to avoid "local optima" where wrong contexts produce plausible values.

---

## Architecture Overview

### Components
1. **libde265** (C++) - Reference decoder at `D:\Rust-projects\heic-decoder-rs\libde265`
2. **Rust decoder** - Our implementation at `src/hevc/`
3. **Comparison tool** - Script to diff trace outputs and find first divergence

### Test Image
- **File**: `example.heic` (1280x854, 280 CTUs, 718 KB)
- **First known corruption**: Call #157, byte 1112, coefficient value 839 (should be small)
- **Current status**: 109/280 CTUs decode before CABAC desync

---

## Phase 1: libde265 Tracing Infrastructure

### Files to Modify

#### 1. `libde265/libde265/slice.cc` - Coefficient output tracing

Add tracing at the **end** of `residual_coding()` function (after line ~3400):

```cpp
// At end of residual_coding(), before return
#ifdef TRACE_COEFFICIENTS
{
    FILE* trace_file = fopen("libde265_coeff_trace.txt", "a");
    if (trace_file) {
        static int call_counter = 0;
        fprintf(trace_file, "CALL#%d x0=%d y0=%d log2=%d cIdx=%d scan=%d last_x=%d last_y=%d\n",
                call_counter++, x0, y0, log2TrafoSize, cIdx, scanIdx,
                LastSignificantCoeffX, LastSignificantCoeffY);
        
        // Print all non-zero coefficients in scan order
        int nT = 1 << log2TrafoSize;
        fprintf(trace_file, "COEFFS:");
        for (int i = 0; i < nT * nT; i++) {
            int x = ScanOrderPos[i].x;
            int y = ScanOrderPos[i].y;
            int coeff = coeff_buf[x + y * nT];
            if (coeff != 0) {
                fprintf(trace_file, " [%d,%d]=%d", x, y, coeff);
            }
        }
        fprintf(trace_file, "\n");
        
        // Print CABAC state after decoding
        fprintf(trace_file, "CABAC_STATE: range=%u value=%u bits_needed=%d byte_pos=%ld\n",
                tctx->cabac_decoder.range,
                tctx->cabac_decoder.value,
                tctx->cabac_decoder.bits_needed,
                (long)(tctx->cabac_decoder.bitstream_curr - tctx->cabac_decoder.bitstream_start));
        fprintf(trace_file, "\n");
        fclose(trace_file);
    }
}
#endif
```

#### 2. `libde265/libde265/slice.cc` - Detailed CABAC operation tracing (optional, for deep debug)

Add finer-grained tracing inside coefficient decoding loops:

```cpp
// Inside the coefficient decoding loop (around line 3200-3300)
#ifdef TRACE_CABAC_OPS
{
    FILE* trace_file = fopen("libde265_cabac_ops.txt", "a");
    if (trace_file) {
        fprintf(trace_file, "sig_coeff[%d,%d] ctx=%d val=%d state=(%u,%u,%d)\n",
                xC, yC, ctx_idx, sig_coeff_flag,
                tctx->cabac_decoder.range, tctx->cabac_decoder.value,
                tctx->cabac_decoder.bits_needed);
        fclose(trace_file);
    }
}
#endif
```

#### 3. `libde265/libde265/cabac.h` - Expose CABAC state

Ensure CABAC decoder state is accessible:

```cpp
// In CABAC_decoder class, make these public or add getters:
uint32_t get_range() const { return range; }
uint32_t get_value() const { return value; }
int get_bits_needed() const { return bits_needed; }
const uint8_t* get_current_pos() const { return bitstream_curr; }
```

#### 4. Build system modifications

Create a traced build script `build_libde265_traced.ps1`:

```powershell
# Build libde265 with coefficient tracing enabled

cd D:\Rust-projects\heic-decoder-rs\libde265

# Create build directory
if (-not (Test-Path build-traced)) {
    New-Item -ItemType Directory -Path build-traced
}

cd build-traced

# Configure with tracing flags
cmake .. -DCMAKE_BUILD_TYPE=Release `
         -DCMAKE_CXX_FLAGS="-DTRACE_COEFFICIENTS -DTRACE_CABAC_OPS"

# Build
cmake --build . --config Release

Write-Host "Traced libde265 built successfully!"
Write-Host "Decoder: build-traced/dec265/dec265.exe"
```

### Expected Output Format

**libde265_coeff_trace.txt**:
```
CALL#0 x0=0 y0=0 log2=3 cIdx=0 scan=0 last_x=2 last_y=3
COEFFS: [0,0]=-45 [1,0]=18 [2,0]=-4 [3,0]=1 [0,1]=-30 [1,1]=-21 ...
CABAC_STATE: range=328 value=13090 bits_needed=-7 byte_pos=42

CALL#1 x0=8 y0=0 log2=3 cIdx=0 scan=0 last_x=3 last_y=2
COEFFS: [0,0]=12 [1,0]=-5 [2,1]=3 ...
CABAC_STATE: range=295 value=8120 bits_needed=-5 byte_pos=47

...

CALL#157 x0=104 y0=0 log2=2 cIdx=1 scan=2 last_x=2 last_y=3
COEFFS: [0,0]=0 [1,0]=-9 [2,0]=1064 [3,0]=4 ...  # ← First large coefficient!
CABAC_STATE: range=420 value=53696 bits_needed=-2 byte_pos=1112
```

---

## Phase 2: Rust Decoder Tracing

### Files to Modify

#### 1. `src/hevc/residual.rs` - Add trace output

At the end of `decode_residual()` function:

```rust
#[cfg(feature = "trace-coefficients")]
{
    use std::fs::OpenOptions;
    use std::io::Write;
    
    static CALL_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let call_num = CALL_COUNTER.fetch_add(1, Ordering::SeqCst);
    
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("rust_coeff_trace.txt")
        .unwrap();
    
    writeln!(file, "CALL#{} x0={} y0={} log2={} cIdx={} scan={} last_x={} last_y={}",
             call_num, x0, y0, log2_trafo_size, c_idx, scan_idx, last_x, last_y).unwrap();
    
    // Print non-zero coefficients
    write!(file, "COEFFS:").unwrap();
    for (idx, &coeff) in coeff_buf.iter().enumerate() {
        if coeff != 0 {
            let x = scan_order[idx].0;
            let y = scan_order[idx].1;
            write!(file, " [{},{}]={}", x, y, coeff).unwrap();
        }
    }
    writeln!(file).unwrap();
    
    // Print CABAC state
    writeln!(file, "CABAC_STATE: range={} value={} bits_needed={} byte_pos={}",
             cabac.range, cabac.value, cabac.bits_needed, cabac.byte_pos()).unwrap();
    writeln!(file).unwrap();
}
```

#### 2. `src/hevc/cabac.rs` - Expose byte position

Add method to CABAC decoder:

```rust
impl CabacDecoder {
    pub fn byte_pos(&self) -> usize {
        // Calculate position from bitstream_curr pointer offset
        // Implementation depends on your BitstreamReader structure
        self.bitstream.position()
    }
}
```

#### 3. `Cargo.toml` - Add feature flag

```toml
[features]
trace-coefficients = []
```

### Build and Run

```powershell
# Build with tracing
cargo build --release --features trace-coefficients

# Run on test image
.\target\release\heic-decoder.exe example.heic output.png
```

---

## Phase 3: Comparison Tool

### Script: `compare_traces.py`

```python
#!/usr/bin/env python3
"""Compare libde265 and Rust coefficient traces to find divergence point."""

import sys
import re

def parse_trace(filename):
    """Parse trace file into structured data."""
    calls = []
    with open(filename, 'r') as f:
        lines = f.readlines()
    
    i = 0
    while i < len(lines):
        if lines[i].startswith('CALL#'):
            # Parse call header
            match = re.match(r'CALL#(\d+) x0=(\d+) y0=(\d+) log2=(\d+) cIdx=(\d+) '
                           r'scan=(\d+) last_x=(\d+) last_y=(\d+)', lines[i])
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
                
                # Parse coefficients
                if i+1 < len(lines) and lines[i+1].startswith('COEFFS:'):
                    coeff_str = lines[i+1][7:].strip()
                    coeffs = []
                    for match in re.finditer(r'\[(\d+),(\d+)\]=(-?\d+)', coeff_str):
                        coeffs.append((int(match.group(1)), int(match.group(2)), 
                                     int(match.group(3))))
                    call_data['coeffs'] = coeffs
                
                # Parse CABAC state
                if i+2 < len(lines) and lines[i+2].startswith('CABAC_STATE:'):
                    match = re.match(r'CABAC_STATE: range=(\d+) value=(\d+) '
                                   r'bits_needed=(-?\d+) byte_pos=(\d+)', lines[i+2])
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

def compare_calls(libde265_call, rust_call):
    """Compare two calls and return differences."""
    diffs = []
    
    # Check metadata
    for key in ['x0', 'y0', 'log2', 'c_idx', 'scan', 'last_x', 'last_y']:
        if libde265_call.get(key) != rust_call.get(key):
            diffs.append(f"{key}: libde265={libde265_call.get(key)} rust={rust_call.get(key)}")
    
    # Check coefficients
    lib_coeffs = set(libde265_call.get('coeffs', []))
    rust_coeffs = set(rust_call.get('coeffs', []))
    
    if lib_coeffs != rust_coeffs:
        missing_in_rust = lib_coeffs - rust_coeffs
        extra_in_rust = rust_coeffs - lib_coeffs
        if missing_in_rust:
            diffs.append(f"Missing coeffs in Rust: {missing_in_rust}")
        if extra_in_rust:
            diffs.append(f"Extra coeffs in Rust: {extra_in_rust}")
    
    # Check CABAC state
    if 'cabac' in libde265_call and 'cabac' in rust_call:
        lib_cabac = libde265_call['cabac']
        rust_cabac = rust_call['cabac']
        for key in ['range', 'value', 'bits_needed', 'byte_pos']:
            if lib_cabac.get(key) != rust_cabac.get(key):
                diffs.append(f"CABAC {key}: libde265={lib_cabac.get(key)} rust={rust_cabac.get(key)}")
    
    return diffs

def main():
    if len(sys.argv) != 3:
        print("Usage: compare_traces.py <libde265_trace.txt> <rust_trace.txt>")
        sys.exit(1)
    
    print("Parsing libde265 trace...")
    libde265_calls = parse_trace(sys.argv[1])
    print(f"Found {len(libde265_calls)} calls")
    
    print("Parsing Rust trace...")
    rust_calls = parse_trace(sys.argv[2])
    print(f"Found {len(rust_calls)} calls")
    
    print("\nComparing traces...\n")
    
    first_divergence = None
    for i in range(min(len(libde265_calls), len(rust_calls))):
        diffs = compare_calls(libde265_calls[i], rust_calls[i])
        if diffs:
            if first_divergence is None:
                first_divergence = i
                print(f"🔴 FIRST DIVERGENCE at CALL#{i}")
                print(f"   libde265: x0={libde265_calls[i]['x0']} y0={libde265_calls[i]['y0']} "
                      f"log2={libde265_calls[i]['log2']} cIdx={libde265_calls[i]['c_idx']}")
                print(f"   Rust:     x0={rust_calls[i]['x0']} y0={rust_calls[i]['y0']} "
                      f"log2={rust_calls[i]['log2']} cIdx={rust_calls[i]['c_idx']}")
                print()
                for diff in diffs:
                    print(f"   - {diff}")
                print()
            
            # Show first 10 divergences
            if i < first_divergence + 10:
                if i > first_divergence:
                    print(f"CALL#{i}: {len(diffs)} differences")
        else:
            if first_divergence is not None and i == first_divergence + 1:
                print(f"✅ CALL#{i}: Coefficients match again (may be lucky)")
    
    if first_divergence is None:
        print("✅ All calls match!")
    else:
        print(f"\n📊 Summary:")
        print(f"   First divergence: CALL#{first_divergence}")
        print(f"   Total calls compared: {min(len(libde265_calls), len(rust_calls))}")
        
        # Show the problematic TU details
        print(f"\n🎯 Focus debugging on:")
        lib_call = libde265_calls[first_divergence]
        print(f"   Position: ({lib_call['x0']}, {lib_call['y0']})")
        print(f"   Block size: {1 << lib_call['log2']}x{1 << lib_call['log2']}")
        print(f"   Component: {'Luma' if lib_call['c_idx'] == 0 else 'Chroma'}")
        print(f"   Scan type: {['Diagonal', 'Horizontal', 'Vertical'][lib_call['scan']]}")

if __name__ == '__main__':
    main()
```

---

## Phase 4: Execution Plan

### Step 1: Build Traced libde265

```powershell
cd D:\Rust-projects\heic-decoder-rs\libde265

# Create build directory
mkdir build-traced -Force
cd build-traced

# Configure with CMake (add tracing defines)
cmake .. -G "Visual Studio 17 2022" -A x64 `
    -DCMAKE_CXX_FLAGS="/DTRACE_COEFFICIENTS"

# Build
cmake --build . --config Release

# Verify executable
ls .\dec265\Release\dec265.exe
```

### Step 2: Build Traced Rust Decoder

```powershell
cd D:\Rust-projects\heic-decoder-rs

# First fix Cargo.toml issues
# 1. Change edition = "2024" to edition = "2021"
# 2. Comment out heic-wasm-rs dev-dependency

# Build with tracing
cargo build --release --features trace-coefficients
```

### Step 3: Get Test Image

```powershell
# Option 1: Download from libheif examples
# git clone --depth 1 https://github.com/strukturag/libheif.git temp-libheif
# cp temp-libheif/examples/example.heic .

# Option 2: Use any HEIC file you have
# cp "path\to\your\iphone_photo.heic" example.heic
```

### Step 4: Run Both Decoders

```powershell
# Clear old traces
rm libde265_coeff_trace.txt -ErrorAction SilentlyContinue
rm rust_coeff_trace.txt -ErrorAction SilentlyContinue

# Run libde265
.\libde265\build-traced\dec265\Release\dec265.exe example.heic -o libde265_out.yuv

# Run Rust decoder
.\target\release\heic-decoder.exe example.heic rust_out.png
```

### Step 5: Compare Traces

```powershell
python compare_traces.py libde265_coeff_trace.txt rust_coeff_trace.txt
```

### Expected Output

```
Parsing libde265 trace...
Found 842 calls
Parsing Rust trace...
Found 394 calls

Comparing traces...

🔴 FIRST DIVERGENCE at CALL#23
   libde265: x0=64 y0=0 log2=3 cIdx=0
   Rust:     x0=64 y0=0 log2=3 cIdx=0

   - COEFFS: Different at position [2,1]: libde265=3 rust=723
   - CABAC byte_pos: libde265=124 rust=125

📊 Summary:
   First divergence: CALL#23
   Total calls compared: 394

🎯 Focus debugging on:
   Position: (64, 0)
   Block size: 8x8
   Component: Luma
   Scan type: Diagonal
```

---

## Phase 5: Deep Dive Debugging

Once you find the first divergence (e.g., CALL#23), add more detailed tracing:

### Enhanced CABAC Operation Tracing

In both libde265 and Rust, trace EVERY CABAC operation for that specific TU:

```cpp
// libde265 - in residual_coding() around the problematic call
if (call_counter == 23) {  // The divergence point
    enable_detailed_cabac_trace = true;
}

// Then in each CABAC decode operation:
if (enable_detailed_cabac_trace) {
    fprintf(trace, "sig_coeff[%d,%d] ctx=%d -> %d (state: %u,%u,%d)\n",
            xC, yC, ctx_idx, sig_flag, range, value, bits_needed);
}
```

```rust
// Rust - similar in decode_residual()
if call_num == 23 {
    self.enable_detailed_trace = true;
}

// In CABAC operations:
if self.enable_detailed_trace {
    eprintln!("sig_coeff[{},{}] ctx={} -> {} (state: {},{},{})",
              x, y, ctx_idx, sig_flag, range, value, bits_needed);
}
```

Compare these detailed traces to find the EXACT operation where they diverge.

---

## Success Criteria

1. **Find first divergence point** - Identify exact TU where coefficients differ
2. **Narrow to operation** - Identify which CABAC operation (sig_coeff, greater1, etc.)
3. **Fix context derivation** - Correct the Rust implementation
4. **Verify convergence** - Re-run traces, divergence should move later or disappear

---

## Troubleshooting

### libde265 won't build
- Check Visual Studio 2022 is installed
- Try Ninja generator: `cmake .. -G Ninja -DCMAKE_BUILD_TYPE=Release`
- Install CMake: `winget install Kitware.CMake`

### Rust won't build
- Fix Cargo.toml edition to "2021"
- Comment out heic-wasm-rs dependency
- Check Rust version: `rustup show` (need 1.92+)

### Traces don't align
- Verify both decoders use same input file
- Check byte_pos increases monotonically
- Ensure no buffering (flush after each write)

### Can't find test image
- Use any iPhone HEIC photo
- Or create one: `ffmpeg -i input.jpg -c:v libx265 output.heic`

---

## Timeline Estimate

| Phase | Task | Time |
|-------|------|------|
| 1 | Add libde265 tracing | 2-3 hours |
| 2 | Add Rust tracing | 1-2 hours |
| 3 | Build comparison tool | 1 hour |
| 4 | Run and compare | 30 min |
| 5 | Deep dive debugging | 4-8 hours |
| **Total** | **End-to-end** | **1-2 days** |

---

## Next Steps

After finding the divergence:
1. Study H.265 spec section for that operation (you have ITU-T spec now!)
2. Compare context derivation logic line-by-line
3. Check scan order, neighbor calculations, state machine
4. Fix Rust implementation
5. Re-run traces to verify fix

This is the **exact methodology** recommended in CLAUDE.md and CABAC-DEBUG-HANDOFF.md. It avoids "local optima" by comparing at the most granular level.
