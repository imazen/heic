# heic-backend-vaapi

Linux libva (VA-API) HEVC decoder backend for the [`heic`](../) crate.

Wraps libva's `VAEntrypointVLD` HEVC decoder so that the heic
dispatcher can route HEIC decode to:

* **NVIDIA GPUs** via [`nvidia-vaapi-driver`](https://github.com/elFarto/nvidia-vaapi-driver)
  (translates VA-API → CUDA NVDEC)
* **Intel iGPUs** via `intel-media-va-driver` (`iHD_drv_video.so`)
* **AMD GPUs** via Mesa's `radeonsi_drv_video.so`

The same crate runs unmodified on any of these.

## Production deployment (Linux server + GPU)

```bash
# 1. Install libva + a VA-API driver matching your GPU.
sudo apt-get install -y libva-dev libva2 libva-drm2 vainfo

# NVIDIA:
sudo apt-get install -y nvidia-vaapi-driver
export LIBVA_DRIVER_NAME=nvidia   # auto-detection usually works too

# Intel iGPU:
sudo apt-get install -y intel-media-va-driver-non-free

# AMD:
sudo apt-get install -y mesa-va-drivers

# 2. Verify HEVC decode is exposed.
vainfo | grep HEVC
# Expected: `VAProfileHEVCMain  : VAEntrypointVLD` (and Main10 on 10-bit-capable GPUs)

# 3. Build heic with the backend feature.
cargo add heic --features backend-rust,backend-vaapi,std
```

Inside your code:

```rust
use heic::{Backend, DecoderConfig, PixelLayout};

let output = DecoderConfig::new()
    .with_backends(&[Backend::Vaapi, Backend::Rust])  // VA-API first, rust fallback
    .decode(&heic_bytes, PixelLayout::Rgba8)?;
```

The dispatcher tries VA-API first; if libva isn't installed, no DRM
render node is available, or the bitstream uses a feature the VA-API
driver doesn't support, it transparently falls through to the pure-Rust
backend.

## WSL2 (Windows Subsystem for Linux)

WSL2 has no `/dev/dri` (the kernel ships DRM compiled-in but no DRM
device is exposed — the GPU is reached through `/dev/dxg`). The probe
detects this and `is_available()` returns false, so the dispatcher
falls through to the next backend.

Two options to actually get VA-API running on WSL:

1. **Build `nvidia-vaapi-driver` from source** with a small patch to
   `findGPUIndexFromFd`'s fallback path so it picks the first EGL
   device with `EGL_CUDA_DEVICE_NV` when no DRM render-node file
   matches. Even with the patch, WSL's libcuda paravirt doesn't
   expose EGL stream entry points (`cuEGLStreamProducerReturnFrame`),
   so the driver init fails honestly and the probe correctly reports
   unavailable — same effect as not installing the driver. The
   patch is documented in
   [`src/lib.rs`](src/lib.rs)'s module docs.

2. **Mount a real `/dev/dri` into WSL** via a custom WSL kernel with
   the appropriate DRM ioctl support. This is involved enough that
   most WSL users skip VA-API entirely and rely on the rust backend.

For end-to-end runtime validation on a real Linux+GPU host, use the
`vaapi-runtime.yml` GitHub Actions workflow (`workflow_dispatch`) or
the [`vaapi_decode_test`](../examples/vaapi_decode_test.rs) example
directly:

```bash
LIBVA_DRIVER_NAME=nvidia cargo run --release \
    -p heic --example vaapi_decode_test \
    --features 'backend-rust,backend-vaapi,std'
```

The example walks the bundled corpus, decodes each file via both
backends, and reports per-file similarity. Expected output on a
healthy NVIDIA host:

```
OK testdata/libheif-examples/example.heic: 1280x854 max=[2,2,1] mean=[0.21,0.18,0.15] diff=4.2%
OK testdata/synthetic/synth_8bit_q50.heic: 256x256 max=[0,0,0] mean=[0.00,0.00,0.00] diff=0.0%
OK testdata/apple-hdr/hdr-sample.heic: 1512x850 max=[1,1,1] mean=[0.08,0.07,0.06] diff=0.2%
...
```

## Compile-only on systems without `libva-dev`

The crate loads every libva symbol via `libloading` at runtime, so it
compiles cleanly on systems that have neither `libva.so.2` nor
`libva-dev` installed. The probe runs at decode time and falls
through to the rust backend when libva isn't present — useful for
shipping a single binary that auto-detects the available decoder.
