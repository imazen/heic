# Pre-Flight Checklist - Differential Tracing Setup

Run through this checklist before starting the tracing process.

## ✅ Environment Setup

- [x] **Rust 1.92+** installed
  ```powershell
  rustc --version  # Should show: rustc 1.92.0 or higher
  ```

- [ ] **Visual Studio 2022** with C++ development tools
  - Check: Open "Visual Studio Installer"
  - Verify: "Desktop development with C++" workload installed
  - If not: Install from https://visualstudio.microsoft.com/

- [ ] **CMake** installed
  ```powershell
  cmake --version  # Should show version 3.x
  # If not: winget install Kitware.CMake
  ```

- [ ] **Python 3.7+** installed
  ```powershell
  python --version  # Should show Python 3.x
  # If not: winget install Python.Python.3.12
  ```

## ✅ Repository Setup

- [x] **Project builds successfully**
  ```powershell
  cd D:\Rust-projects\heic-decoder-rs
  cargo check  # Should show: Finished `dev` profile
  ```

- [x] **Traced build works**
  ```powershell
  cargo build --release --features trace-coefficients
  # Should show: Finished `release` profile
  ```

- [x] **Cargo.toml fixed**
  - Edition: 2024 ✓
  - heic-wasm-rs: Commented out ✓
  - trace-coefficients feature: Added ✓

- [x] **Documentation available**
  - [x] DIFFERENTIAL_TRACING_PLAN.md
  - [x] QUICKSTART_TRACING.md
  - [x] LOCALIZATION_COMPLETE.md
  - [x] CLAUDE.md (original debugging notes)
  - [x] CABAC-DEBUG-HANDOFF.md

- [x] **Scripts ready**
  - [x] build_libde265_traced.ps1
  - [x] build_rust_traced.ps1
  - [x] compare_traces.py

- [x] **Reference materials**
  - [x] ITU-T H.265 spec at: `D:\Rust-projects\heic-decoder-rs\ITU-T\`
  - [x] libde265 source at: `D:\Rust-projects\heic-decoder-rs\libde265\`

## 📋 Pre-Tracing Tasks

Before you can run differential tracing, complete these:

### 1. Get Test Image
- [ ] **Obtain a HEIC file** (choose one method):
  
  **Option A** - Use iPhone photo:
  ```powershell
  cp "path\to\your\iphone_photo.HEIC" D:\Rust-projects\heic-decoder-rs\test.heic
  ```
  
  **Option B** - Download example:
  ```powershell
  cd D:\Rust-projects\heic-decoder-rs
  $url = "https://raw.githubusercontent.com/strukturag/libheif/master/examples/example.heic"
  Invoke-WebRequest -Uri $url -OutFile "test.heic"
  ```
  
  **Option C** - Convert from JPEG (requires ffmpeg):
  ```powershell
  ffmpeg -i input.jpg -c:v libx265 test.heic
  ```

- [ ] **Verify test file exists**:
  ```powershell
  ls D:\Rust-projects\heic-decoder-rs\test.heic
  # Should show file with size > 0 KB
  ```

### 2. Add Tracing to libde265

- [ ] **Open file**: `libde265\libde265\slice.cc`

- [ ] **Find function**: `residual_coding()` (around line 2915)

- [ ] **Add tracing code** at END of function (before `return DE265_OK;`)
  - See QUICKSTART_TRACING.md section 3a for exact code
  - Uses `#ifdef TRACE_COEFFICIENTS`
  - Outputs to `libde265_coeff_trace.txt`

- [ ] **Verify variable names match**:
  - `coeff_buf` - coefficient buffer
  - `LastSignificantCoeffX`, `LastSignificantCoeffY` - last sig coeff
  - `scanIdx` - scan type
  - `tctx->cabac_decoder.range` - CABAC state

- [ ] **Save file**

### 3. Build Traced libde265

- [ ] **Run build script**:
  ```powershell
  cd D:\Rust-projects\heic-decoder-rs
  .\build_libde265_traced.ps1
  ```

- [ ] **Verify build succeeded**:
  ```powershell
  ls libde265\build-traced\dec265\Release\dec265.exe
  # Should exist
  ```

- [ ] **Test run** (quick check):
  ```powershell
  .\libde265\build-traced\dec265\Release\dec265.exe --help
  # Should show libde265 help
  ```

### 4. Add Tracing to Rust Decoder

- [ ] **Open file**: `src\hevc\residual.rs`

- [ ] **Find function**: `decode_residual()` or similar

- [ ] **Add tracing code** at END of function
  - See QUICKSTART_TRACING.md section 4a for exact code
  - Uses `#[cfg(feature = "trace-coefficients")]`
  - Outputs to `rust_coeff_trace.txt`

- [ ] **Add byte_position() method** to CABAC decoder (`src\hevc\cabac.rs`)
  - See QUICKSTART_TRACING.md section 4b

- [ ] **Save files**

### 5. Build Traced Rust Decoder

- [ ] **Run build script**:
  ```powershell
  cd D:\Rust-projects\heic-decoder-rs
  .\build_rust_traced.ps1
  ```
  
  Or manually:
  ```powershell
  cargo build --release --features trace-coefficients
  ```

- [ ] **Verify build succeeded**:
  ```powershell
  ls target\release\heic-decoder.exe
  # Should exist (or whatever your binary name is)
  ```

### 6. Create Test Binary (if needed)

If the main crate doesn't have a binary target:

- [ ] **Create test harness** in `tests\decode_heic.rs` or `examples\decode.rs`
  - See QUICKSTART_TRACING.md section 5c for example code

- [ ] **Build and test**:
  ```powershell
  cargo build --release --features trace-coefficients --example decode
  # or
  cargo test --release --features trace-coefficients --test decode_heic -- --nocapture
  ```

## 🚀 Ready to Trace

Once all boxes above are checked:

### Run Differential Tracing

```powershell
# 1. Clean old traces
cd D:\Rust-projects\heic-decoder-rs
rm libde265_coeff_trace.txt -ErrorAction SilentlyContinue
rm rust_coeff_trace.txt -ErrorAction SilentlyContinue

# 2. Run libde265
.\libde265\build-traced\dec265\Release\dec265.exe test.heic -o libde265_out.yuv

# 3. Verify trace created
ls libde265_coeff_trace.txt  # Should exist with size > 0

# 4. Run Rust decoder
.\target\release\heic-decoder.exe test.heic
# Or: cargo run --release --features trace-coefficients --example decode test.heic

# 5. Verify trace created
ls rust_coeff_trace.txt  # Should exist with size > 0

# 6. Compare traces
python compare_traces.py libde265_coeff_trace.txt rust_coeff_trace.txt
```

### Expected Result

You should see output like:
```
🔴 FIRST DIVERGENCE at CALL#23
   Position: (64, 0)
   Block size: 8x8
   Component: Luma (Y)
   
🎯 Focus debugging on: CALL#23
```

This tells you EXACTLY where to look!

## 🔧 Troubleshooting

### libde265 build fails
- [ ] Check Visual Studio 2022 is installed with C++ tools
- [ ] Try Ninja generator: `cmake .. -G Ninja`
- [ ] Check CMake version: `cmake --version`

### Rust build fails
- [ ] Check edition is 2024 in Cargo.toml
- [ ] Check heic-wasm-rs is commented out
- [ ] Run `cargo clean` and try again

### Trace files empty
- [ ] Verify tracing code was added to source files
- [ ] Check `#ifdef TRACE_COEFFICIENTS` in C++ code
- [ ] Check `#[cfg(feature = "trace-coefficients")]` in Rust code
- [ ] Ensure feature flag passed to cargo: `--features trace-coefficients`

### Python script fails
- [ ] Check Python 3.7+: `python --version`
- [ ] Script has no dependencies, should work out of box
- [ ] Check trace files exist and are non-empty

## 📊 Success Criteria

You'll know the setup is complete when:

✅ Both decoders build with tracing enabled  
✅ Both produce trace files when run  
✅ Python script finds divergence point  
✅ You know exact TU/call where they differ  

At that point, proceed to deep-dive debugging in DIFFERENTIAL_TRACING_PLAN.md Phase 5.

---

## Quick Reference

| Task | File | Line | What to Add |
|------|------|------|-------------|
| libde265 trace | `libde265/libde265/slice.cc` | ~3400 | fprintf trace code |
| Rust trace | `src/hevc/residual.rs` | End of `decode_residual()` | writeln! trace code |
| CABAC helper | `src/hevc/cabac.rs` | In impl | `byte_position()` method |

---

**Ready?** Once all checkboxes are ✓, you're good to go! Follow QUICKSTART_TRACING.md for step-by-step execution.
