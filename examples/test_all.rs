/// Test all HEIC files: recursively find .heic files and attempt to decode each.
///
/// Categorizes results as: DECODE OK, CONTAINER PARSE, HEVC DECODE, or UNSUPPORTED.
/// Usage: cargo run --release --example test_all [dir]

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

    // Skip uncompressed HEIF files (ISO 23001-17) — not HEVC-encoded
    files.retain(|f| {
        let name = f.file_name().unwrap().to_string_lossy();
        !name.starts_with("uncompressed_")
    });

    let decoder = heic_decoder::HeicDecoder::new();

    let mut ok = 0u32;
    let mut container_err = 0u32;
    let mut hevc_err = 0u32;
    let mut other_err = 0u32;
    let mut total_time_ms = 0u128;

    // Error categories for summary
    let mut error_groups: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    let strip_prefix = base_dir.clone();

    for file in &files {
        let name = file
            .strip_prefix(&strip_prefix)
            .unwrap_or(file)
            .display()
            .to_string();

        eprint!("{:65} ", name);

        let data = match std::fs::read(file) {
            Ok(d) => d,
            Err(e) => {
                let msg = format!("READ ERROR: {e}");
                eprintln!("{msg}");
                other_err += 1;
                error_groups.entry(msg).or_default().push(name);
                continue;
            }
        };

        let start = Instant::now();
        match decoder.decode_to_frame(&data) {
            Ok(frame) => {
                let elapsed = start.elapsed().as_millis();
                total_time_ms += elapsed;
                let w = frame.cropped_width();
                let h = frame.cropped_height();
                eprintln!("OK  {}x{}  ({}ms)", w, h, elapsed);
                ok += 1;
            }
            Err(e) => {
                let err_str = format!("{e}");

                // Categorize errors
                let short = if err_str.contains("container")
                    || err_str.contains("ftyp")
                    || err_str.contains("brand")
                    || err_str.contains("primary")
                    || err_str.contains("Missing image data")
                    || err_str.contains("item")
                    || err_str.contains("grid")
                    || err_str.contains("hdlr")
                    || err_str.contains("No primary")
                    || err_str.contains("box")
                {
                    container_err += 1;
                    "CONTAINER"
                } else if err_str.contains("Unsupported")
                    || err_str.contains("unsupported")
                    || err_str.contains("not supported")
                {
                    other_err += 1;
                    "UNSUPPORTED"
                } else {
                    hevc_err += 1;
                    "HEVC"
                };

                eprintln!("{}: {}", short, err_str);
                error_groups.entry(err_str).or_default().push(name);
            }
        }
    }

    let total = ok + container_err + hevc_err + other_err;

    eprintln!();
    eprintln!("====================================================");
    eprintln!("=== Results: {ok} OK, {container_err} container, {hevc_err} HEVC, {other_err} other out of {total} ===");
    eprintln!("====================================================");
    eprintln!("Total decode time: {}ms", total_time_ms);
    eprintln!();

    if !error_groups.is_empty() {
        eprintln!("=== Error categories ===");
        for (err, files) in &error_groups {
            eprintln!("  [{}x] {}", files.len(), err);
            for f in files.iter().take(5) {
                eprintln!("        {}", f);
            }
            if files.len() > 5 {
                eprintln!("        ... and {} more", files.len() - 5);
            }
        }
    }
}
