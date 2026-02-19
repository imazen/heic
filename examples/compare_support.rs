//! Compare decoder output: pixelwise and metadata comparison against libheif.

use std::path::PathBuf;

fn find_heic_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(find_heic_files(&path));
            } else if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                if ext == "heic" || ext == "heif" || ext == "hif" {
                    files.push(path);
                }
            }
        }
    }
    files
}

/// Per-channel pixel comparison stats
struct PixelStats {
    /// Number of pixels compared (per channel)
    pixel_count: u64,
    /// Number of channels that match exactly
    exact_matches: u64,
    /// Maximum absolute difference across all channels
    max_diff: u8,
    /// Sum of absolute differences (for MAE)
    sad: u64,
    /// Sum of squared differences (for PSNR)
    ssd: u64,
    /// Histogram of differences [0..=255]
    diff_histogram: [u64; 256],
}

impl PixelStats {
    fn new() -> Self {
        Self {
            pixel_count: 0,
            exact_matches: 0,
            max_diff: 0,
            sad: 0,
            ssd: 0,
            diff_histogram: [0; 256],
        }
    }

    fn add_sample(&mut self, ours: u8, reference: u8) {
        self.pixel_count += 1;
        let diff = ours.abs_diff(reference);
        if diff == 0 {
            self.exact_matches += 1;
        }
        if diff > self.max_diff {
            self.max_diff = diff;
        }
        self.sad += diff as u64;
        self.ssd += (diff as u64) * (diff as u64);
        self.diff_histogram[diff as usize] += 1;
    }

    fn mae(&self) -> f64 {
        if self.pixel_count == 0 {
            return 0.0;
        }
        self.sad as f64 / self.pixel_count as f64
    }

    fn psnr(&self) -> f64 {
        if self.pixel_count == 0 || self.ssd == 0 {
            return f64::INFINITY;
        }
        let mse = self.ssd as f64 / self.pixel_count as f64;
        10.0 * (255.0_f64 * 255.0 / mse).log10()
    }

    fn exact_pct(&self) -> f64 {
        if self.pixel_count == 0 {
            return 100.0;
        }
        self.exact_matches as f64 / self.pixel_count as f64 * 100.0
    }
}

fn compare_pixels(
    ours: &heic_decoder::DecodeOutput,
    reference: &heic_wasm_rs::DecodedImage,
) -> Option<PixelStats> {
    if ours.width != reference.width || ours.height != reference.height {
        return None;
    }

    // Determine bytes per pixel from layout / has_alpha
    let our_bpp = ours.layout.bytes_per_pixel();
    let ref_bpp = if reference.has_alpha { 4 } else { 3 };

    // Compare only the common channels (RGB)
    let channels = 3usize;
    let w = ours.width as usize;
    let h = ours.height as usize;

    let expected_our_len = w * h * our_bpp;
    let expected_ref_len = w * h * ref_bpp;
    if ours.data.len() < expected_our_len || reference.data.len() < expected_ref_len {
        return None;
    }

    let mut stats = PixelStats::new();

    for y in 0..h {
        for x in 0..w {
            let our_base = (y * w + x) * our_bpp;
            let ref_base = (y * w + x) * ref_bpp;

            for c in 0..channels {
                stats.add_sample(ours.data[our_base + c], reference.data[ref_base + c]);
            }
        }
    }

    Some(stats)
}

fn main() {
    let base_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/lilith/work/heic/test-images".to_string());

    let mut files = find_heic_files(std::path::Path::new(&base_dir));
    files.sort();

    // Skip uncompressed HEIF files — not HEVC-encoded
    files.retain(|f| {
        let name = f.file_name().unwrap().to_string_lossy();
        !name.starts_with("uncompressed_")
    });

    // Load decoders
    let our_decoder = heic_decoder::DecoderConfig::new();
    let wasm_decoder = heic_wasm_rs::HeicDecoder::from_file(std::path::Path::new(
        "/home/lilith/work/heic/wasm-module/heic_decoder.wasm",
    ))
    .expect("Failed to load WASM decoder");

    let strip_prefix = base_dir.clone();

    let mut both_ok = 0u32;
    let mut ours_only = 0u32;
    let mut libheif_only = 0u32;
    let mut both_fail = 0u32;

    let mut libheif_only_files = Vec::new();
    let mut ours_only_files = Vec::new();

    // Aggregate stats across all files
    let mut global_stats = PixelStats::new();
    let mut pixel_exact_files = 0u32;
    let mut pixel_close_files = 0u32; // max_diff <= 3
    let mut pixel_diff_files = Vec::new(); // files with differences

    eprintln!(
        "{:65} {:12} {:>6} {:>6} {:>8} {:>8} {:>8}  meta",
        "file", "status", "dims", "alpha", "max_d", "MAE", "PSNR"
    );
    eprintln!("{}", "-".repeat(140));

    for file in &files {
        let name = file
            .strip_prefix(&strip_prefix)
            .unwrap_or(file)
            .display()
            .to_string();

        let data = match std::fs::read(file) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let our_result = our_decoder.decode(&data, heic_decoder::PixelLayout::Rgba8);
        let ref_result = wasm_decoder.decode(&data);

        let our_ok = our_result.is_ok();
        let ref_ok = ref_result.is_ok();

        match (our_ok, ref_ok) {
            (true, true) => {
                both_ok += 1;
                let ours = our_result.unwrap();
                let reference = ref_result.unwrap();

                let dim_match = ours.width == reference.width && ours.height == reference.height;
                let alpha_match = ours.layout.has_alpha() == reference.has_alpha;

                let dim_str = if dim_match {
                    format!("{}x{}", ours.width, ours.height)
                } else {
                    format!(
                        "{}x{} vs {}x{}",
                        ours.width, ours.height, reference.width, reference.height
                    )
                };

                let alpha_str = match (ours.layout.has_alpha(), reference.has_alpha) {
                    (true, true) => "both",
                    (false, false) => "none",
                    (true, false) => "OURS",
                    (false, true) => "REF",
                };

                if let Some(stats) = compare_pixels(&ours, &reference) {
                    // Accumulate global stats
                    global_stats.pixel_count += stats.pixel_count;
                    global_stats.exact_matches += stats.exact_matches;
                    global_stats.sad += stats.sad;
                    global_stats.ssd += stats.ssd;
                    if stats.max_diff > global_stats.max_diff {
                        global_stats.max_diff = stats.max_diff;
                    }
                    for i in 0..256 {
                        global_stats.diff_histogram[i] += stats.diff_histogram[i];
                    }

                    if stats.max_diff == 0 {
                        pixel_exact_files += 1;
                        eprintln!(
                            "{:65} {:12} {:>6} {:>6} {:>8} {:>8} {:>8}  {}",
                            name,
                            "EXACT",
                            dim_str,
                            alpha_str,
                            "0",
                            "0.000",
                            "inf",
                            if dim_match && alpha_match {
                                "ok"
                            } else {
                                "META DIFF"
                            }
                        );
                    } else {
                        if stats.max_diff <= 3 {
                            pixel_close_files += 1;
                        }
                        pixel_diff_files.push((
                            name.clone(),
                            stats.max_diff,
                            stats.mae(),
                            stats.psnr(),
                            stats.exact_pct(),
                            dim_str.clone(),
                        ));
                        eprintln!(
                            "{:65} {:12} {:>6} {:>6} {:>8} {:>8.3} {:>7.1}dB  {}",
                            name,
                            if stats.max_diff <= 3 {
                                "CLOSE"
                            } else {
                                "DIFFERS"
                            },
                            dim_str,
                            alpha_str,
                            stats.max_diff,
                            stats.mae(),
                            stats.psnr(),
                            if dim_match && alpha_match {
                                "ok"
                            } else {
                                "META DIFF"
                            }
                        );
                    }
                } else {
                    eprintln!(
                        "{:65} {:12} {:>6} {:>6} {:>8} {:>8} {:>8}  can't compare",
                        name, "NO COMPARE", dim_str, alpha_str, "-", "-", "-"
                    );
                }
            }
            (true, false) => {
                ours_only += 1;
                ours_only_files.push(name.clone());
                let ours = our_result.unwrap();
                eprintln!(
                    "{:65} {:12} {:>6} {:>6}",
                    name,
                    "OURS ONLY",
                    format!("{}x{}", ours.width, ours.height),
                    if ours.layout.has_alpha() {
                        "alpha"
                    } else {
                        "none"
                    }
                );
            }
            (false, true) => {
                libheif_only += 1;
                let err = our_result.unwrap_err();
                libheif_only_files.push((name.clone(), format!("{err}")));
                let reference = ref_result.unwrap();
                eprintln!(
                    "{:65} {:12} {:>6} {:>6}  err={}",
                    name,
                    "LIBHEIF ONLY",
                    format!("{}x{}", reference.width, reference.height),
                    if reference.has_alpha { "alpha" } else { "none" },
                    err
                );
            }
            (false, false) => {
                both_fail += 1;
                eprintln!("{:65} {:12}", name, "BOTH FAIL");
            }
        }
    }

    let total = both_ok + ours_only + libheif_only + both_fail;
    eprintln!();
    eprintln!("================================================================");
    eprintln!("=== DECODE SUPPORT ===");
    eprintln!("Total: {} files", total);
    eprintln!(
        "  Both OK:      {} ({:.0}%)",
        both_ok,
        both_ok as f64 / total as f64 * 100.0
    );
    eprintln!("  Ours only:    {}", ours_only);
    eprintln!("  libheif only: {}", libheif_only);
    eprintln!("  Both fail:    {}", both_fail);
    eprintln!();
    eprintln!(
        "=== PIXEL COMPARISON ({} files where both decode) ===",
        both_ok
    );
    eprintln!("  Pixel-exact:     {}", pixel_exact_files);
    eprintln!("  Close (max<=3):  {}", pixel_close_files);
    eprintln!("  With diffs:      {}", pixel_diff_files.len());
    eprintln!();
    eprintln!("=== GLOBAL PIXEL STATS (RGB channels, all files) ===");
    eprintln!("  Total samples:   {}", global_stats.pixel_count);
    eprintln!("  Exact match:     {:.4}%", global_stats.exact_pct());
    eprintln!("  Max difference:  {}", global_stats.max_diff);
    eprintln!("  MAE:             {:.4}", global_stats.mae());
    eprintln!("  PSNR:            {:.2} dB", global_stats.psnr());

    // Difference distribution
    eprintln!();
    eprintln!("=== DIFFERENCE DISTRIBUTION ===");
    let nonzero: u64 = global_stats.pixel_count - global_stats.exact_matches;
    if nonzero > 0 {
        eprintln!(
            "  diff=0:  {} ({:.2}%)",
            global_stats.diff_histogram[0],
            global_stats.diff_histogram[0] as f64 / global_stats.pixel_count as f64 * 100.0
        );
        for d in 1..=global_stats.max_diff as usize {
            if global_stats.diff_histogram[d] > 0 {
                eprintln!(
                    "  diff={}: {} ({:.4}%)",
                    d,
                    global_stats.diff_histogram[d],
                    global_stats.diff_histogram[d] as f64 / global_stats.pixel_count as f64 * 100.0
                );
            }
        }
    } else {
        eprintln!("  All pixels exact match!");
    }

    // Per-file diff details sorted by max_diff descending
    if !pixel_diff_files.is_empty() {
        eprintln!();
        eprintln!("=== FILES WITH PIXEL DIFFERENCES (sorted by max diff) ===");
        let mut sorted = pixel_diff_files;
        sorted.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
        });
        for (name, max_diff, mae, psnr, exact_pct, dims) in &sorted {
            eprintln!(
                "  {:60} {:>6}  max={:>3}  MAE={:.3}  PSNR={:.1}dB  exact={:.2}%",
                name, dims, max_diff, mae, psnr, exact_pct
            );
        }
    }

    if !libheif_only_files.is_empty() {
        eprintln!();
        eprintln!("=== Files libheif decodes but we don't ===");
        for (name, err) in &libheif_only_files {
            eprintln!("  {} -> {}", name, err);
        }
    }

    if !ours_only_files.is_empty() {
        eprintln!();
        eprintln!("=== Files we decode but libheif doesn't ===");
        for name in &ours_only_files {
            eprintln!("  {}", name);
        }
    }
    eprintln!("================================================================");
}
