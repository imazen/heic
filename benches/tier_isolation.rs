//! SIMD-tier isolation: the native top tier vs the same decoder forced to scalar.
//!
//! `decode.rs` measures absolute decode time and `compare.rs` measures this
//! crate against libheif. Neither can tell you whether the SIMD paths are
//! earning their keep — a kernel slower than its own scalar fallback is
//! invisible in both. This bench decodes the same files twice, once with the
//! native SIMD token disabled. (The same gap in linear-srgb was hiding a real
//! regression.)
//!
//! Unlike `decode.rs`, this does NOT hardcode a Linux home path: it discovers
//! files from the shared `codec-corpus` HEIC conformance set, honouring
//! `HEIC_CORPUS_DIR`, and skips cleanly when the corpus is absent rather than
//! failing or silently benchmarking nothing.
//!
//! Run: `cargo bench --bench tier_isolation --features backend-rust,_dev`
//! Do NOT build with `-C target-cpu=native`: that pins the tier at compile
//! time, after which it cannot be disabled and this bench skips rather than
//! silently reporting the SIMD path under both labels.

use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use heic::{DecoderConfig, PixelLayout};

#[cfg(target_arch = "aarch64")]
type TierToken = archmage::NeonToken;
#[cfg(target_arch = "x86_64")]
type TierToken = archmage::X64V3Token;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
const TIER_NAME: &str = if cfg!(target_arch = "aarch64") {
    "neon"
} else {
    "v3(avx2)"
};

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn set_simd(enabled: bool) -> bool {
    use archmage::SimdToken;
    TierToken::dangerously_disable_token_process_wide(!enabled).is_ok()
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn set_simd(_enabled: bool) -> bool {
    false
}

/// Root of the shared HEIC conformance corpus. Repo-relative by default so the
/// bench survives being run from another machine; override with
/// `HEIC_CORPUS_DIR`.
fn corpus_dir() -> PathBuf {
    if let Ok(d) = std::env::var("HEIC_CORPUS_DIR") {
        return PathBuf::from(d);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codec-corpus")
        .join("heic-conformance")
        .join("valid")
}

/// Decodable files, largest first, capped — larger files spend proportionally
/// more time in pixel kernels and less in container parsing, which is what this
/// bench is trying to isolate.
fn vectors(limit: usize) -> Vec<(String, Vec<u8>)> {
    let dir = corpus_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let mut found: Vec<(PathBuf, u64)> = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "heic") {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                found.push((p, len));
            }
        }
    }
    found.sort_by_key(|(_, len)| std::cmp::Reverse(*len));

    let mut out = Vec::new();
    for (path, _) in found {
        if out.len() >= limit {
            break;
        }
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        // Only keep files this build can actually decode, so the bench never
        // reports a number for a file that errored out.
        if DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .is_err()
        {
            continue;
        }
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push((name, data));
    }
    out
}

fn bench_tiers(c: &mut Criterion) {
    if !set_simd(true) || !set_simd(false) {
        eprintln!(
            "[tier_isolation] no toggleable SIMD tier on this target, or the tier is \
             compile-time guaranteed (drop -C target-cpu=native, build with --features _dev). \
             Skipping."
        );
        return;
    }
    set_simd(true);

    let vecs = vectors(3);
    if vecs.is_empty() {
        eprintln!(
            "[tier_isolation] no decodable .heic found under {}. Set HEIC_CORPUS_DIR. Skipping.",
            corpus_dir().display()
        );
        return;
    }
    eprintln!("[tier_isolation] comparing {TIER_NAME} vs forced scalar");

    for (name, data) in &vecs {
        let mut group = c.benchmark_group(format!("decode/{name}"));
        for (arm, simd) in [(TIER_NAME, true), ("scalar", false)] {
            group.bench_function(arm, |b| {
                set_simd(simd);
                b.iter(|| {
                    DecoderConfig::new()
                        .decode(std::hint::black_box(data), PixelLayout::Rgba8)
                        .unwrap()
                })
            });
        }
        set_simd(true);
        group.finish();
    }
    set_simd(true);
}

/// The 4:4:4 colour-conversion kernel, isolated.
///
/// Added 2026-08-01 alongside its NEON arm. `convert_444_to_rgb` dispatched
/// `[v3, scalar]` — an AVX2 arm but no NEON arm — so every aarch64 4:4:4
/// decode ran the scalar per-pixel loop. The whole-decode bench above cannot
/// surface that: heic decode is CABAC-bound (it measures ~1.00x overall), so
/// colour conversion is a small enough fraction that a fully-scalar kernel
/// disappears into the noise. That is exactly why this one is measured on its
/// own.
fn bench_color_444(c: &mut Criterion) {
    if !set_simd(true) || !set_simd(false) {
        eprintln!("[color_444] SIMD tier not toggleable here. Skipping.");
        return;
    }
    set_simd(true);

    // 1920x1080 4:4:4, the shape this path exists for.
    const W: usize = 1920;
    const H: usize = 1080;
    let mut s = 0x9e37_79b9u32;
    let mut plane = || -> Vec<u16> {
        (0..W * H)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((s >> 16) & 0xFF) as u16
            })
            .collect()
    };
    let (y, cb, cr) = (plane(), plane(), plane());
    let mut rgb = vec![0u8; W * H * 3];

    let mut group = c.benchmark_group("convert_444_to_rgb");
    group.throughput(criterion::Throughput::Elements((W * H) as u64));
    for (arm, simd) in [(TIER_NAME, true), ("scalar", false)] {
        group.bench_function(arm, |b| {
            set_simd(simd);
            b.iter(|| {
                heic_core::color_convert::convert_444_to_rgb(
                    std::hint::black_box(&y),
                    &cb,
                    &cr,
                    W,
                    W,
                    0,
                    H as u32,
                    0,
                    W as u32,
                    0,
                    false,
                    1,
                    &mut rgb,
                )
            })
        });
    }
    set_simd(true);
    group.finish();
}

criterion_group!(benches, bench_tiers, bench_color_444);
criterion_main!(benches);
