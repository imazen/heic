# Quick Start - Differential Tracing

This guide gets you up and running with differential tracing in **15 minutes**.

## Prerequisites

✅ Rust 1.92+ (you have this)  
✅ Visual Studio 2022 with C++ tools  
✅ CMake (`winget install Kitware.CMake`)  
✅ Python 3.7+ (for comparison script)  
✅ A test HEIC file (any iPhone photo will work)

---

## Step 1: Fix Build (1 minute) ✅ DONE

The Cargo.toml has been fixed:
- ✅ Edition set to 2024 (required for let-chains syntax)
- ✅ heic-wasm-rs dependency commented out
- ✅ trace-coefficients feature added

Verify:
```powershell
cd D:\Rust-projects\heic-decoder-rs
cargo check
# Should see: Finished `dev` profile
```

---

## Step 2: Get a Test Image (2 minutes)

You need a HEIC file to test with. Options:

**Option A - Use an iPhone photo** (easiest):
```powershell
# Copy any .heic file from your phone or downloads
cp "path\to\iphone_photo.HEIC" D:\Rust-projects\heic-decoder-rs\test.heic
```

**Option B - Download example from libheif**:
```powershell
cd D:\Rust-projects\heic-decoder-rs

# Download the standard test image (1280x854, 718 KB)
$url = "https://raw.githubusercontent.com/strukturag/libheif/master/examples/example.heic"
Invoke-WebRequest -Uri $url -OutFile "test.heic"
```

**Option C - Convert from JPEG**:
```powershell
# If you have ffmpeg installed
ffmpeg -i input.jpg -c:v libx265 -crf 28 test.heic
```

---

## Step 3: Implement Tracing in libde265 (30 minutes)

### 3a. Add Tracing Code to slice.cc

Open `libde265/libde265/slice.cc` and find the `residual_coding()` function (around line 2915).

**Add this at the END of the function, before the final `return DE265_OK;`**:

```cpp
  // === COEFFICIENT TRACING (add before return) ===
#ifdef TRACE_COEFFICIENTS
  {
    static FILE* trace_file = nullptr;
    static int call_counter = 0;
    
    if (!trace_file) {
      trace_file = fopen("libde265_coeff_trace.txt", "w");
    }
    
    if (trace_file) {
      fprintf(trace_file, "CALL#%d x0=%d y0=%d log2=%d cIdx=%d scan=%d last_x=%d last_y=%d\n",
              call_counter++, x0, y0, log2TrafoSize, cIdx, scanIdx,
              LastSignificantCoeffX, LastSignificantCoeffY);
      
      // Print coefficients
      int nT = 1 << log2TrafoSize;
      fprintf(trace_file, "COEFFS:");
      for (int y = 0; y < nT; y++) {
        for (int x = 0; x < nT; x++) {
          int16_t coeff = coeff_buf[y * nT + x];
          if (coeff != 0) {
            fprintf(trace_file, " [%d,%d]=%d", x, y, coeff);
          }
        }
      }
      fprintf(trace_file, "\n");
      
      // Print CABAC state
      fprintf(trace_file, "CABAC_STATE: range=%u value=%u bits_needed=%d byte_pos=%ld\n",
              (unsigned)tctx->cabac_decoder.range,
              (unsigned)tctx->cabac_decoder.value,
              tctx->cabac_decoder.bits_needed,
              (long)(tctx->cabac_decoder.bitstream_curr - tctx->cabac_decoder.bitstream_start));
      fprintf(trace_file, "\n");
      fflush(trace_file);
    }
  }
#endif
  // === END TRACING ===

  return DE265_OK;
```

**Note**: Find where `coeff_buf` is declared/used in the function to match the exact variable name.

### 3b. Build libde265 with Tracing

```powershell
cd D:\Rust-projects\heic-decoder-rs
.\build_libde265_traced.ps1
```

This will:
- Create `libde265/build-traced/` directory
- Configure with CMake + tracing flags
- Build in Release mode
- Output: `libde265/build-traced/dec265/Release/dec265.exe`

---

## Step 4: Implement Tracing in Rust Decoder (30 minutes)

### 4a. Add Tracing to residual.rs

Open `src/hevc/residual.rs` and find the `decode_residual()` function.

**Add this at the END of the function**:

```rust
    // === COEFFICIENT TRACING ===
    #[cfg(feature = "trace-coefficients")]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        
        static CALL_COUNTER: AtomicUsize = AtomicUsize::new(0);
        let call_num = CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
        
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open("rust_coeff_trace.txt")
            .unwrap();
        
        writeln!(file, "CALL#{} x0={} y0={} log2={} cIdx={} scan={} last_x={} last_y={}",
                 call_num, x0, y0, log2_trafo_size, c_idx, scan_idx,
                 last_sig_x, last_sig_y).unwrap();
        
        // Print non-zero coefficients
        write!(file, "COEFFS:").unwrap();
        let n_t = 1 << log2_trafo_size;
        for y in 0..n_t {
            for x in 0..n_t {
                let coeff = coeff_buf[y * n_t + x];
                if coeff != 0 {
                    write!(file, " [{},{}]={}", x, y, coeff).unwrap();
                }
            }
        }
        writeln!(file).unwrap();
        
        // Print CABAC state
        let byte_pos = cabac.byte_position();
        writeln!(file, "CABAC_STATE: range={} value={} bits_needed={} byte_pos={}",
                 cabac.range, cabac.value, cabac.bits_needed, byte_pos).unwrap();
        writeln!(file).unwrap();
    }
    // === END TRACING ===
```

### 4b. Add byte_position() method to CABAC

In `src/hevc/cabac.rs`, add this method to the CABAC decoder struct:

```rust
impl CabacDecoder {
    /// Get current byte position in bitstream for tracing
    pub fn byte_position(&self) -> usize {
        self.bitstream.position()
        // Or if position() doesn't exist:
        // (self.bitstream_curr as usize) - (self.bitstream_start as usize)
    }
}
```

### 4c. Build Rust Decoder with Tracing

```powershell
cd D:\Rust-projects\heic-decoder-rs
.\build_rust_traced.ps1
```

Or manually:
```powershell
cargo build --release --features trace-coefficients
```

---

## Step 5: Run Both Decoders (5 minutes)

### 5a. Clean old trace files

```powershell
cd D:\Rust-projects\heic-decoder-rs
rm libde265_coeff_trace.txt -ErrorAction SilentlyContinue
rm rust_coeff_trace.txt -ErrorAction SilentlyContinue
```

### 5b. Run libde265

```powershell
.\libde265\build-traced\dec265\Release\dec265.exe test.heic -o libde265_output.yuv

# Check trace was created
ls libde265_coeff_trace.txt
# Should show file size > 0
```

### 5c. Run Rust decoder

**Note**: You'll need to create a simple test binary first. Create `tests/decode_heic.rs`:

```rust
use heic_decoder;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <input.heic>", args[0]);
        std::process::exit(1);
    }
    
    let data = std::fs::read(&args[1]).expect("Failed to read file");
    
    match heic_decoder::decode(&data) {
        Ok(image) => {
            println!("Decoded {}x{} image", image.width, image.height);
        }
        Err(e) => {
            eprintln!("Decode error: {}", e);
            std::process::exit(1);
        }
    }
}
```

Then run:
```powershell
cargo run --release --features trace-coefficients --bin decode_heic test.heic

# Check trace was created
ls rust_coeff_trace.txt
# Should show file size > 0
```

---

## Step 6: Compare Traces (2 minutes)

```powershell
python compare_traces.py libde265_coeff_trace.txt rust_coeff_trace.txt
```

### Expected Output

```
🔍 Parsing libde265 trace...
   Found 842 calls
🔍 Parsing Rust trace...
   Found 394 calls

📊 Comparing traces...

🔴 FIRST DIVERGENCE at CALL#23
   libde265: x0=64 y0=0 log2=3 cIdx=0
   Rust:     x0=64 y0=0 log2=3 cIdx=0

   ❌ Coeff [2,1]: libde265=3 rust=723
   ❌ CABAC byte_pos: libde265=124 rust=125

====================================================================
📊 Summary:
   First divergence: CALL#23
   Total divergences: 371 / 394
   Total calls compared: 394

🎯 Focus debugging on:
   Call number: #23
   Position: (64, 0)
   Block size: 8x8
   Component: Luma (Y)
   Scan type: Diagonal
   CABAC byte position: 124

💡 Next steps:
   1. Review HEVC spec for this operation type
   2. Add detailed CABAC tracing for CALL#23
   3. Compare operation-by-operation for this specific TU
   4. Check context derivation logic in Rust decoder
====================================================================
```

---

## What You've Accomplished

✅ Built traced versions of both decoders  
✅ Generated coefficient-level trace outputs  
✅ Found the **exact divergence point** (e.g., CALL#23 at byte 124)  
✅ Narrowed debugging scope from "whole image" to "one 8x8 block"

---

## Next Steps - Deep Dive Debugging

Now that you know WHERE it diverges (e.g., CALL#23), add finer-grained tracing:

### 1. Trace EVERY CABAC Operation for That TU

In libde265 `slice.cc`, wrap the problematic call:
```cpp
static int call_counter = 0;
int this_call = call_counter++;

// Enable detailed tracing for specific call
bool detailed_trace = (this_call == 23);

if (detailed_trace) {
    fprintf(stderr, "=== DETAILED TRACE FOR CALL#%d ===\n", this_call);
}
```

Then in each CABAC decode operation:
```cpp
if (detailed_trace) {
    fprintf(stderr, "sig_coeff[%d,%d] ctx=%d -> %d (state: %u,%u,%d)\n",
            xC, yC, ctx_idx, sig_flag, range, value, bits_needed);
}
```

Do the same in Rust `residual.rs`:
```rust
let detailed_trace = call_num == 23;

if detailed_trace {
    eprintln!("=== DETAILED TRACE FOR CALL#{} ===", call_num);
}

// In each CABAC operation:
if detailed_trace {
    eprintln!("sig_coeff[{},{}] ctx={} -> {} (state: {},{},{})",
              x, y, ctx_idx, sig_flag, range, value, bits_needed);
}
```

### 2. Compare Operation Sequences

Run both decoders again, redirect stderr to files:
```powershell
.\libde265\...\dec265.exe test.heic 2> libde265_detailed.txt
cargo run ... 2> rust_detailed.txt

# Diff the files
code --diff libde265_detailed.txt rust_detailed.txt
```

Find the FIRST operation that differs - that's your bug!

### 3. Check HEVC Spec

You now have the ITU-T spec at `D:\Rust-projects\heic-decoder-rs\ITU-T\`.

Search for the relevant section:
- `sig_coeff_flag` → Section 9.3.4.2.5
- `greater1_flag` → Section 9.3.4.2.6
- `coeff_abs_level_remaining` → Section 9.3.4.2.7

Compare context derivation logic with the spec.

---

## Troubleshooting

### libde265 build fails
- Install Visual Studio 2022 with C++ tools
- Try Ninja: `cmake .. -G Ninja -DCMAKE_BUILD_TYPE=Release`
- Install CMake: `winget install Kitware.CMake`

### Rust trace file empty
- Check feature flag: `cargo build --features trace-coefficients`
- Verify tracing code was added to `residual.rs`
- Check `Cargo.toml` has `trace-coefficients = []` in `[features]`

### Traces don't align (different call counts)
- Normal! Rust may exit early due to CABAC desync
- Compare the calls that DO exist
- Focus on FIRST divergence

### Python script errors
- Install Python 3.7+: `winget install Python.Python.3.12`
- Script is self-contained, no dependencies needed

---

## Files Created

- ✅ `DIFFERENTIAL_TRACING_PLAN.md` - Comprehensive plan
- ✅ `QUICKSTART_TRACING.md` - This file
- ✅ `build_libde265_traced.ps1` - libde265 build script
- ✅ `build_rust_traced.ps1` - Rust build script
- ✅ `compare_traces.py` - Comparison tool
- ✅ `Cargo.toml` - Fixed and ready to build

---

**You're now set up for precise differential debugging!** The tracing infrastructure will pinpoint the exact CABAC operation where the decoders diverge, eliminating guesswork.
