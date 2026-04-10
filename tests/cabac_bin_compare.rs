//! CABAC bin-level comparison: our decoder vs dec265 for MERGE_A first B-frame
//!
//! Run: cargo test --test cabac_bin_compare -- --nocapture --ignored

use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Parsed CABAC bin trace entry
#[derive(Debug, Clone)]
struct BinEntry {
    kind: char, // 'c' = context, 'x' = bypass
    range: u32,
    value: Option<u32>, // None for bypass
    state: Option<u8>,
    mps: Option<u8>,
    bin_val: u8,
    byte_pos: u32,
}

fn parse_bin_line(line: &str) -> Option<(u32, BinEntry)> {
    let line = line.trim();
    if !line.starts_with('B') {
        return None;
    }

    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }

    let index: u32 = parts[0][1..].parse().ok()?;
    let kind = parts[1].chars().next()?;
    if kind != 'c' && kind != 'x' {
        return None;
    }

    let get_field = |prefix: &str| -> Option<u32> {
        for p in &parts[2..] {
            if let Some(val) = p.strip_prefix(prefix) {
                return val.parse().ok();
            }
        }
        None
    };

    let range = get_field("r=")?;
    let bin_val = get_field("b=")? as u8;
    let byte_pos = get_field("bp=")?;

    let entry = if kind == 'c' {
        BinEntry {
            kind,
            range,
            value: get_field("v="),
            state: get_field("s=").map(|v| v as u8),
            mps: get_field("m=").map(|v| v as u8),
            bin_val,
            byte_pos,
        }
    } else {
        BinEntry {
            kind,
            range,
            value: None,
            state: None,
            mps: None,
            bin_val,
            byte_pos,
        }
    };
    Some((index, entry))
}

/// Parse bin lines, returning the LAST batch that starts from B0
/// (handles the case where the I-frame outputs B0..B499 and then
///  the B-frame counter resets and outputs another B0..B499)
fn parse_last_batch(stderr: &str, max_bins: usize) -> Vec<BinEntry> {
    let mut batches: Vec<Vec<BinEntry>> = Vec::new();
    let mut current: Vec<BinEntry> = Vec::new();

    for line in stderr.lines() {
        if let Some((idx, entry)) = parse_bin_line(line) {
            if idx == 0 && !current.is_empty() {
                // New batch starting from B0
                batches.push(core::mem::take(&mut current));
            }
            current.push(entry);
            if current.len() >= max_bins {
                // Don't keep accumulating beyond what we need
            }
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }

    // Return the last batch (which is the B-frame for our decoder)
    batches.into_iter().last().unwrap_or_default()
}

/// Get bin trace from dec265 (only traces first B-frame)
fn get_dec265_bins(bitstream: &Path, max_bins: usize) -> Vec<BinEntry> {
    let dec265_trace = Path::new("/home/lilith/work/heic/libde265-src/build-trace/dec265/dec265");
    assert!(
        dec265_trace.exists(),
        "dec265 trace build not found at {}",
        dec265_trace.display()
    );

    let output = Command::new(dec265_trace)
        .arg(bitstream)
        .arg("--quiet")
        .output()
        .expect("failed to run dec265");

    let stderr = String::from_utf8_lossy(&output.stderr);
    // dec265 only outputs one batch (the B-frame), so first batch is fine
    let mut bins = Vec::new();
    for line in stderr.lines() {
        if let Some((_idx, entry)) = parse_bin_line(line) {
            bins.push(entry);
            if bins.len() >= max_bins {
                break;
            }
        }
    }
    bins
}

/// Get bin trace from our decoder (last batch = B-frame)
fn get_our_bins(bitstream: &Path, max_bins: usize) -> Vec<BinEntry> {
    let output = Command::new("cargo")
        .args(["run", "--example", "bin_trace_emit"])
        .env("MERGE_A_BITSTREAM", bitstream.to_str().unwrap())
        .env("MERGE_A_MAX_BINS", max_bins.to_string())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("failed to run cargo example");

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Our decoder outputs I-frame bins then B-frame bins (both starting from B0)
    // Take the LAST batch which is the B-frame
    let bins = parse_last_batch(&stderr, max_bins);

    // Also print any SLICE_DATA/CTX lines for debugging
    for line in stderr.lines() {
        if line.starts_with("SLICE_DATA")
            || line.starts_with("SLICE_QP")
            || line.starts_with("CTX")
            || line.starts_with("CTU-CK")
        {
            eprintln!("  [ours] {line}");
        }
    }
    bins
}

#[test]
#[ignore]
fn compare_merge_a_bins() {
    let bitstream = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/conformance/vectors/MERGE_A_TI_3/MERGE_A_TI_3/MERGE_A_TI_3.bit"
    ));
    if !bitstream.exists() {
        eprintln!("SKIP: MERGE_A bitstream not found");
        return;
    }

    let max_bins = 500;

    eprintln!("\n=== Getting dec265 bins... ===");
    let dec265_bins = get_dec265_bins(bitstream, max_bins);
    eprintln!("  Got {} bins from dec265", dec265_bins.len());

    eprintln!("\n=== Getting our bins... ===");
    let our_bins = get_our_bins(bitstream, max_bins);
    eprintln!(
        "  Got {} bins from our decoder (last batch)",
        our_bins.len()
    );

    if dec265_bins.is_empty() {
        panic!("No bins captured from dec265");
    }
    if our_bins.is_empty() {
        panic!("No bins captured from our decoder");
    }

    // Print first 3 bins from each to confirm they're from B-frame
    eprintln!(
        "\n  dec265 first 3: {:?}",
        &dec265_bins[..3.min(dec265_bins.len())]
    );
    eprintln!("  ours first 3:   {:?}", &our_bins[..3.min(our_bins.len())]);

    // Compare bin by bin
    let compare_count = dec265_bins.len().min(our_bins.len());
    let mut first_divergence = None;
    let mut matching_count = 0;

    eprintln!("\n=== Bin-by-bin comparison (dec265 left | ours right) ===");
    eprintln!(
        "{:>5} | {:>4} {:>6} {:>8} {:>3} {:>4} | {:>4} {:>6} {:>8} {:>3} {:>4} | match?",
        "bin", "type", "range", "value", "b", "bp", "type", "range", "value", "b", "bp"
    );
    eprintln!("{}", "-".repeat(85));

    for i in 0..compare_count {
        let d = &dec265_bins[i];
        let o = &our_bins[i];

        let kind_ok = d.kind == o.kind;
        let range_ok = d.range == o.range;
        let bin_ok = d.bin_val == o.bin_val;
        let bp_ok = d.byte_pos == o.byte_pos;

        let all_ok = kind_ok && range_ok && bin_ok && bp_ok;

        if all_ok {
            matching_count += 1;
        }

        let show = i < 10
            || !all_ok
            || (first_divergence.is_some() && i <= first_divergence.unwrap() + 15);

        if show {
            let d_val = d.value.map_or("---".into(), |v| format!("{v}"));
            let o_val = o.value.map_or("---".into(), |v| format!("{v}"));
            let status = if all_ok {
                "OK".to_string()
            } else {
                let mut s = "DIFF".to_string();
                if !kind_ok {
                    s += " KIND";
                }
                if !range_ok {
                    s += " RANGE";
                }
                if !bin_ok {
                    s += " BIN";
                }
                if !bp_ok {
                    s += " BP";
                }
                s
            };
            eprintln!(
                "B{:<4} | {:>4} {:>6} {:>8} {:>3} {:>4} | {:>4} {:>6} {:>8} {:>3} {:>4} | {}",
                i,
                d.kind,
                d.range,
                d_val,
                d.bin_val,
                d.byte_pos,
                o.kind,
                o.range,
                o_val,
                o.bin_val,
                o.byte_pos,
                status
            );
        }

        if first_divergence.is_none() && !all_ok {
            first_divergence = Some(i);
        }
    }

    eprintln!("\n=== Summary ===");
    eprintln!("  Compared: {compare_count} bins");
    eprintln!("  Matching: {matching_count}");

    if let Some(div_idx) = first_divergence {
        let d = &dec265_bins[div_idx];
        let o = &our_bins[div_idx];
        eprintln!("\n  === FIRST DIVERGENCE at bin {div_idx} ===");
        eprintln!(
            "    dec265: kind={} r={} v={:?} s={:?} m={:?} b={} bp={}",
            d.kind, d.range, d.value, d.state, d.mps, d.bin_val, d.byte_pos
        );
        eprintln!(
            "    ours:   kind={} r={} v={:?} s={:?} m={:?} b={} bp={}",
            o.kind, o.range, o.value, o.state, o.mps, o.bin_val, o.byte_pos
        );

        // Analyze the divergence
        if d.kind != o.kind {
            eprintln!(
                "    -> BIN TYPE MISMATCH: dec265 decoded a {} bin, we decoded a {} bin",
                if d.kind == 'c' { "context" } else { "bypass" },
                if o.kind == 'c' { "context" } else { "bypass" }
            );
            eprintln!(
                "    -> This suggests different syntax element parsing (wrong branch, missing/extra SE)"
            );
        } else if d.range != o.range || d.byte_pos != o.byte_pos {
            eprintln!("    -> CABAC engine state diverged: ranges/byte-positions differ");
            if d.bin_val != o.bin_val {
                eprintln!("    -> Decoded bin values also differ");
            }
        } else if d.bin_val != o.bin_val {
            eprintln!("    -> Same CABAC state but different decoded value!");
            eprintln!(
                "    -> This suggests different context model selection (wrong context index)"
            );
            if d.kind == 'c' {
                eprintln!(
                    "    -> dec265 post-decode context: s={:?} m={:?}",
                    d.state, d.mps
                );
                eprintln!(
                    "    -> our pre-decode context:     s={:?} m={:?}",
                    o.state, o.mps
                );
            }
        }

        // Write comparison to file
        let out_path = "/tmp/merge_a_bin_compare.txt";
        let mut f = std::fs::File::create(out_path).unwrap();
        writeln!(f, "# MERGE_A first B-frame CABAC bin comparison").unwrap();
        writeln!(f, "# First divergence at bin {div_idx}").unwrap();
        for i in 0..compare_count {
            let d = &dec265_bins[i];
            let o = &our_bins[i];
            let ok = d.kind == o.kind
                && d.range == o.range
                && d.bin_val == o.bin_val
                && d.byte_pos == o.byte_pos;
            writeln!(
                f,
                "B{} {} {} {:?} {:?} {:?} {} {} | {} {:?} {:?} {:?} {} {} | {}",
                i,
                d.kind,
                d.range,
                d.value,
                d.state,
                d.mps,
                d.bin_val,
                d.byte_pos,
                o.range,
                o.value,
                o.state,
                o.mps,
                o.bin_val,
                o.byte_pos,
                if ok { "OK" } else { "DIFF" }
            )
            .unwrap();
        }
        eprintln!("\n  Full comparison written to {out_path}");
    } else {
        eprintln!("  All {compare_count} bins MATCH perfectly!");
    }
}
