//! Interleaved runtime-tier measurements with untimed exact pixel checks.
//! HEIC_BENCH_INPUTS supplies a colon-separated fixture list; every file must decode.
use heic::{DecoderConfig, PixelLayout};
use zenbench::prelude::*;

#[cfg(target_arch = "aarch64")]
type TierToken = archmage::NeonToken;
#[cfg(target_arch = "x86_64")]
type TierToken = archmage::X64V3Token;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn set_simd(enabled: bool) {
    TierToken::dangerously_disable_token_process_wide(!enabled)
        .expect("runtime tier must be toggleable; omit target-cpu=native");
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn set_simd(_enabled: bool) {
    panic!("tier isolation requires native ARM or x86_64");
}

fn bench_decode(suite: &mut Suite) {
    let paths = std::env::var_os("HEIC_BENCH_INPUTS").expect("set HEIC_BENCH_INPUTS explicitly");
    for path in std::env::split_paths(&paths) {
        let data: &'static [u8] =
            Box::leak(std::fs::read(&path).expect("fixture").into_boxed_slice());
        set_simd(false);
        let scalar = DecoderConfig::new()
            .decode(data, PixelLayout::Rgba8)
            .expect("scalar decode");
        set_simd(true);
        let simd = DecoderConfig::new()
            .decode(data, PixelLayout::Rgba8)
            .expect("SIMD decode");
        assert_eq!((scalar.width, scalar.height), (simd.width, simd.height));
        assert_eq!(scalar.data, simd.data, "{}", path.display());
        let pixels = u64::from(simd.width) * u64::from(simd.height);
        eprintln!(
            "{}: {}x{} exact tier pixels",
            path.display(),
            simd.width,
            simd.height
        );
        suite.compare(
            format!("decode/{}", path.file_name().unwrap().to_string_lossy()),
            move |g| {
                g.throughput(Throughput::Elements(pixels));
                for (name, enabled) in [("native", true), ("scalar", false)] {
                    g.bench(name, move |b| {
                        b.with_input(move || set_simd(enabled)).run(move |_| {
                            DecoderConfig::new()
                                .decode(data, PixelLayout::Rgba8)
                                .unwrap()
                        })
                    });
                }
            },
        );
    }
    set_simd(true);
}

fn bench_color(suite: &mut Suite) {
    for width in [17usize, 64, 256, 1024, 4096] {
        let height = width;
        // Padding exercises native strided inputs; output is tightly packed.
        let stride = width + 3;
        let y: &'static [u16] = Box::leak(
            (0..stride * height)
                .map(|i| ((i * 17 + 16) & 255) as u16)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let cb: &'static [u16] = Box::leak(
            (0..stride * height)
                .map(|i| ((i * 29 + 31) & 255) as u16)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let cr: &'static [u16] = Box::leak(
            (0..stride * height)
                .map(|i| ((i * 43 + 97) & 255) as u16)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let convert = move |out: &mut [u8]| {
            heic_core::color_convert::convert_444_to_rgb(
                y,
                cb,
                cr,
                stride,
                stride,
                0,
                height as u32,
                0,
                width as u32,
                0,
                false,
                1,
                out,
            );
        };
        let mut scalar = vec![0; width * height * 3];
        let mut simd = scalar.clone();
        set_simd(false);
        convert(&mut scalar);
        set_simd(true);
        convert(&mut simd);
        assert_eq!(scalar, simd);
        suite.compare(format!("color444/{width}x{height}"), move |g| {
            g.throughput(Throughput::Elements((width * height) as u64));
            for (name, enabled) in [("native", true), ("scalar", false)] {
                g.bench(name, move |b| {
                    b.with_input(move || {
                        set_simd(enabled);
                        vec![0; width * height * 3]
                    })
                    .run(move |mut out| {
                        convert(&mut out);
                        out
                    })
                });
            }
        });
    }
    set_simd(true);
}
zenbench::main!(bench_decode, bench_color);
