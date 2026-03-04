// Head-to-head benchmark: pure Rust heic-decoder vs native libheif (SSE) vs WASM libheif
//
// Native libheif is linked directly via raw FFI against the system library (1.12 + libde265 SSE).

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::path::Path;

fn heic_base_dir() -> String {
    std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into())
}

fn example_heic() -> String {
    format!("{}/libheif/examples/example.heic", heic_base_dir())
}

fn iphone_heic() -> String {
    format!("{}/test-images/classic-car-iphone12pro.heic", heic_base_dir())
}

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("wasm-module")
        .join("heic_decoder.wasm")
}

// --- Raw FFI to system libheif (1.12) ---

#[allow(non_camel_case_types, dead_code)]
mod ffi {
    use std::ffi::c_char;
    use std::os::raw::c_int;

    pub const HEIF_COLORSPACE_RGB: c_int = 1;
    pub const HEIF_CHROMA_INTERLEAVED_RGB: c_int = 10;
    pub const HEIF_CHANNEL_INTERLEAVED: c_int = 10;

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct heif_error {
        pub code: c_int,
        pub subcode: c_int,
        pub message: *const c_char,
    }

    // Opaque types
    pub enum heif_context {}
    pub enum heif_image_handle {}
    pub enum heif_image {}
    pub enum heif_decoding_options {}

    #[link(name = "heif")]
    unsafe extern "C" {
        pub fn heif_context_alloc() -> *mut heif_context;
        pub fn heif_context_free(ctx: *mut heif_context);
        pub fn heif_context_read_from_memory_without_copy(
            ctx: *mut heif_context,
            mem: *const u8,
            size: usize,
            options: *const std::ffi::c_void,
        ) -> heif_error;
        pub fn heif_context_get_primary_image_handle(
            ctx: *mut heif_context,
            handle: *mut *mut heif_image_handle,
        ) -> heif_error;
        pub fn heif_image_handle_release(handle: *const heif_image_handle);
        pub fn heif_image_handle_get_width(handle: *const heif_image_handle) -> c_int;
        pub fn heif_image_handle_get_height(handle: *const heif_image_handle) -> c_int;
        pub fn heif_image_handle_has_alpha_channel(handle: *const heif_image_handle) -> c_int;
        pub fn heif_decode_image(
            handle: *const heif_image_handle,
            out_img: *mut *mut heif_image,
            colorspace: c_int,
            chroma: c_int,
            options: *const heif_decoding_options,
        ) -> heif_error;
        pub fn heif_image_get_plane_readonly(
            img: *const heif_image,
            channel: c_int,
            out_stride: *mut c_int,
        ) -> *const u8;
        pub fn heif_image_release(img: *const heif_image);
        pub fn heif_image_handle_get_number_of_metadata_blocks(
            handle: *const heif_image_handle,
            type_filter: *const c_char,
        ) -> c_int;
        pub fn heif_image_handle_get_list_of_metadata_block_IDs(
            handle: *const heif_image_handle,
            type_filter: *const c_char,
            ids: *mut u32,
            count: c_int,
        ) -> c_int;
        pub fn heif_image_handle_get_metadata_size(
            handle: *const heif_image_handle,
            metadata_id: u32,
        ) -> usize;
        pub fn heif_image_handle_get_metadata(
            handle: *const heif_image_handle,
            metadata_id: u32,
            out_data: *mut u8,
        ) -> heif_error;
    }
}

/// Decode via native system libheif (with SSE-enabled libde265)
fn native_libheif_decode(data: &[u8]) -> Vec<u8> {
    unsafe {
        let ctx = ffi::heif_context_alloc();
        assert!(!ctx.is_null());
        let err = ffi::heif_context_read_from_memory_without_copy(
            ctx,
            data.as_ptr(),
            data.len(),
            std::ptr::null(),
        );
        assert_eq!(err.code, 0, "read_from_memory failed");

        let mut handle: *mut ffi::heif_image_handle = std::ptr::null_mut();
        let err = ffi::heif_context_get_primary_image_handle(ctx, &mut handle);
        assert_eq!(err.code, 0, "get_primary_image_handle failed");

        let w = ffi::heif_image_handle_get_width(handle) as usize;
        let h = ffi::heif_image_handle_get_height(handle) as usize;

        let mut img: *mut ffi::heif_image = std::ptr::null_mut();
        let err = ffi::heif_decode_image(
            handle,
            &mut img,
            ffi::HEIF_COLORSPACE_RGB,
            ffi::HEIF_CHROMA_INTERLEAVED_RGB,
            std::ptr::null(),
        );
        assert_eq!(err.code, 0, "decode_image failed");

        let mut stride: i32 = 0;
        let plane = ffi::heif_image_get_plane_readonly(
            img,
            ffi::HEIF_CHANNEL_INTERLEAVED,
            &mut stride,
        );
        assert!(!plane.is_null());
        let stride = stride as usize;

        let mut out = Vec::with_capacity(w * h * 3);
        for row in 0..h {
            let src = std::slice::from_raw_parts(plane.add(row * stride), w * 3);
            out.extend_from_slice(src);
        }

        ffi::heif_image_release(img);
        ffi::heif_image_handle_release(handle);
        ffi::heif_context_free(ctx);

        out
    }
}

/// Probe via native system libheif (context + handle, no pixel decode)
fn native_libheif_probe(data: &[u8]) -> (i32, i32, bool) {
    unsafe {
        let ctx = ffi::heif_context_alloc();
        let err = ffi::heif_context_read_from_memory_without_copy(
            ctx,
            data.as_ptr(),
            data.len(),
            std::ptr::null(),
        );
        assert_eq!(err.code, 0);

        let mut handle: *mut ffi::heif_image_handle = std::ptr::null_mut();
        let err = ffi::heif_context_get_primary_image_handle(ctx, &mut handle);
        assert_eq!(err.code, 0);

        let w = ffi::heif_image_handle_get_width(handle);
        let h = ffi::heif_image_handle_get_height(handle);
        let alpha = ffi::heif_image_handle_has_alpha_channel(handle) != 0;

        ffi::heif_image_handle_release(handle);
        ffi::heif_context_free(ctx);

        (w, h, alpha)
    }
}

/// Extract EXIF via native system libheif
fn native_libheif_exif(data: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let ctx = ffi::heif_context_alloc();
        let err = ffi::heif_context_read_from_memory_without_copy(
            ctx,
            data.as_ptr(),
            data.len(),
            std::ptr::null(),
        );
        assert_eq!(err.code, 0);

        let mut handle: *mut ffi::heif_image_handle = std::ptr::null_mut();
        let err = ffi::heif_context_get_primary_image_handle(ctx, &mut handle);
        assert_eq!(err.code, 0);

        let exif_filter = c"Exif".as_ptr();
        let count = ffi::heif_image_handle_get_number_of_metadata_blocks(handle, exif_filter);

        let result = if count > 0 {
            let mut ids = vec![0u32; count as usize];
            ffi::heif_image_handle_get_list_of_metadata_block_IDs(
                handle,
                exif_filter,
                ids.as_mut_ptr(),
                count,
            );
            let size = ffi::heif_image_handle_get_metadata_size(handle, ids[0]);
            let mut buf = vec![0u8; size];
            let err = ffi::heif_image_handle_get_metadata(handle, ids[0], buf.as_mut_ptr());
            if err.code == 0 { Some(buf) } else { None }
        } else {
            None
        };

        ffi::heif_image_handle_release(handle);
        ffi::heif_context_free(ctx);

        result
    }
}

fn bench_decode_small(c: &mut Criterion) {
    let data = std::fs::read(&example_heic()).expect("read example.heic");
    let wasm = wasm_path();

    let mut group = c.benchmark_group("decode_1280x854");
    group.throughput(Throughput::Bytes(data.len() as u64));

    // Pure Rust
    let rust_dec = heic_decoder::DecoderConfig::new();
    group.bench_function("rust", |b| {
        b.iter(|| rust_dec.decode(&data, heic_decoder::PixelLayout::Rgb8).unwrap());
    });

    // Native libheif (system, with SSE)
    group.bench_function("native_libheif", |b| {
        b.iter(|| native_libheif_decode(&data));
    });

    // WASM libheif
    if wasm.exists() {
        let wasm_dec = heic_wasm_rs::HeicDecoder::from_file(&wasm).unwrap();
        group.bench_function("wasm_libheif", |b| {
            b.iter(|| wasm_dec.decode(&data).unwrap());
        });
    } else {
        eprintln!("WASM module not found, skipping wasm_libheif variants");
    }

    group.finish();
}

fn bench_decode_large(c: &mut Criterion) {
    let iphone = iphone_heic();
    let path = Path::new(&iphone);
    if !path.exists() {
        eprintln!("iPhone test image not found, skipping decode_3024x4032");
        return;
    }
    let data = std::fs::read(path).expect("read iPhone HEIC");
    let wasm = wasm_path();

    let mut group = c.benchmark_group("decode_3024x4032");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.sample_size(10);

    // Pure Rust (sequential — force 1 thread even if parallel feature is on)
    #[cfg(feature = "parallel")]
    {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let rust_dec = heic_decoder::DecoderConfig::new();
        group.bench_function("rust_1thread", |b| {
            b.iter(|| {
                pool.install(|| rust_dec.decode(&data, heic_decoder::PixelLayout::Rgb8).unwrap())
            });
        });
    }
    #[cfg(not(feature = "parallel"))]
    {
        let rust_dec = heic_decoder::DecoderConfig::new();
        group.bench_function("rust", |b| {
            b.iter(|| rust_dec.decode(&data, heic_decoder::PixelLayout::Rgb8).unwrap());
        });
    }

    // Pure Rust (parallel — use all cores)
    #[cfg(feature = "parallel")]
    {
        let rust_dec = heic_decoder::DecoderConfig::new();
        group.bench_function("rust_parallel", |b| {
            b.iter(|| rust_dec.decode(&data, heic_decoder::PixelLayout::Rgb8).unwrap());
        });
    }

    // Native libheif (single-threaded — libheif 1.12 has no threading)
    group.bench_function("native_libheif", |b| {
        b.iter(|| native_libheif_decode(&data));
    });

    // WASM libheif
    if wasm.exists() {
        let wasm_dec = heic_wasm_rs::HeicDecoder::from_file(&wasm).unwrap();
        group.bench_function("wasm_libheif", |b| {
            b.iter(|| wasm_dec.decode(&data).unwrap());
        });
    }

    group.finish();
}

fn bench_probe(c: &mut Criterion) {
    let data = std::fs::read(&example_heic()).expect("read example.heic");
    let wasm = wasm_path();

    let mut group = c.benchmark_group("probe_1280x854");
    group.throughput(Throughput::Bytes(data.len() as u64));

    // Pure Rust (no decoder init needed)
    group.bench_function("rust", |b| {
        b.iter(|| heic_decoder::ImageInfo::from_bytes(&data).unwrap());
    });

    // Native libheif
    group.bench_function("native_libheif", |b| {
        b.iter(|| native_libheif_probe(&data));
    });

    // WASM libheif
    if wasm.exists() {
        let wasm_dec = heic_wasm_rs::HeicDecoder::from_file(&wasm).unwrap();
        group.bench_function("wasm_libheif", |b| {
            b.iter(|| wasm_dec.get_info(&data).unwrap());
        });
    }

    group.finish();
}

fn bench_exif(c: &mut Criterion) {
    let iphone = iphone_heic();
    let path = Path::new(&iphone);
    if !path.exists() {
        eprintln!("iPhone test image not found, skipping exif_extract");
        return;
    }
    let data = std::fs::read(path).expect("read iPhone HEIC");
    let wasm = wasm_path();

    let mut group = c.benchmark_group("exif_extract");
    group.throughput(Throughput::Bytes(data.len() as u64));

    // Pure Rust
    let rust_dec = heic_decoder::DecoderConfig::new();
    group.bench_function("rust", |b| {
        b.iter(|| rust_dec.extract_exif(&data).unwrap());
    });

    // Native libheif
    group.bench_function("native_libheif", |b| {
        b.iter(|| native_libheif_exif(&data));
    });

    // WASM libheif
    if wasm.exists() {
        let wasm_dec = heic_wasm_rs::HeicDecoder::from_file(&wasm).unwrap();
        group.bench_function("wasm_libheif", |b| {
            b.iter(|| wasm_dec.get_exif(&data).unwrap());
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_decode_small,
    bench_decode_large,
    bench_probe,
    bench_exif,
);
criterion_main!(benches);
