//! Compare decoder support: which files can each decoder handle?

use std::path::PathBuf;
use std::time::Instant;

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
    let our_decoder = heic_decoder::HeicDecoder::new();
    let wasm_decoder = heic_wasm_rs::HeicDecoder::from_file(
        std::path::Path::new("/home/lilith/work/heic/wasm-module/heic_decoder.wasm"),
    ).expect("Failed to load WASM decoder");

    let strip_prefix = base_dir.clone();

    let mut both_ok = 0u32;
    let mut ours_only = 0u32;
    let mut libheif_only = 0u32;
    let mut both_fail = 0u32;

    let mut libheif_only_files = Vec::new();
    let mut ours_only_files = Vec::new();

    for file in &files {
        let name = file.strip_prefix(&strip_prefix).unwrap_or(file).display().to_string();

        let data = match std::fs::read(file) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let our_result = our_decoder.decode(&data);
        let ref_result = wasm_decoder.decode(&data);

        let our_ok = our_result.is_ok();
        let ref_ok = ref_result.is_ok();

        let our_info = match &our_result {
            Ok(img) => {
                let alpha = if img.has_alpha { " [ALPHA]" } else { "" };
                format!("{}x{}{}", img.width, img.height, alpha)
            }
            Err(e) => format!("{e}"),
        };

        let ref_info = match &ref_result {
            Ok(img) => {
                let alpha = if img.has_alpha { " [ALPHA]" } else { "" };
                format!("{}x{}{}", img.width, img.height, alpha)
            }
            Err(e) => format!("{e}"),
        };

        let tag = match (our_ok, ref_ok) {
            (true, true) => {
                // Check dimension/alpha match
                let our = our_result.as_ref().unwrap();
                let rf = ref_result.as_ref().unwrap();
                let dim_match = our.width == rf.width && our.height == rf.height;
                let alpha_match = our.has_alpha == rf.has_alpha;
                both_ok += 1;
                if !dim_match {
                    "BOTH OK (DIM MISMATCH)"
                } else if !alpha_match {
                    "BOTH OK (ALPHA MISMATCH)"
                } else {
                    "BOTH OK"
                }
            }
            (true, false) => { ours_only += 1; ours_only_files.push(name.clone()); "OURS ONLY" }
            (false, true) => { libheif_only += 1; libheif_only_files.push((name.clone(), our_info.clone())); "LIBHEIF ONLY" }
            (false, false) => { both_fail += 1; "BOTH FAIL" }
        };

        eprintln!("{:65} {:25}  ours={:40}  libheif={}", name, tag, our_info, ref_info);
    }

    let total = both_ok + ours_only + libheif_only + both_fail;
    eprintln!();
    eprintln!("======================================================");
    eprintln!("Total: {} files", total);
    eprintln!("  Both OK:     {} ({:.0}%)", both_ok, both_ok as f64 / total as f64 * 100.0);
    eprintln!("  Ours only:   {}", ours_only);
    eprintln!("  libheif only: {}", libheif_only);
    eprintln!("  Both fail:   {}", both_fail);
    eprintln!("======================================================");

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
}
