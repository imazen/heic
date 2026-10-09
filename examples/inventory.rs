//! Print the structural inventory of HEIF/HEIC files: every box, item,
//! item extent and property, with what the zencodec decode does with it.
//!
//! ```text
//! cargo run --example inventory --features "backend-rust,zencodec" -- FILE...
//! ```

use zencodec::decode::{DecodeJob, DecoderConfig};

fn main() {
    let mut failed = false;
    for path in std::env::args().skip(1) {
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{path}: {e}");
                failed = true;
                continue;
            }
        };
        let job = heic::HeicDecoderConfig::new().job();
        match job.inventory(&data) {
            Ok(Some(inv)) => {
                println!("== {path}");
                print!("{inv}");
                if let Err(e) = inv.validate() {
                    println!("INVALID: {e}");
                    failed = true;
                }
            }
            Ok(None) => println!("{path}: no inventory"),
            Err(e) => {
                eprintln!("{path}: {e}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
