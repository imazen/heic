//! Simple HEVC raw bitstream decoder for testing

use heic_decoder::hevc::{self, DecodedFrame};
use std::env;
use std::fs;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <input.hevc>", args[0]);
        eprintln!("Decode raw HEVC bitstream");
        process::exit(1);
    }

    let input_path = &args[1];
    let data = fs::read(input_path).expect("Failed to read input file");

    // Parse as raw HEVC - extract NAL units with start codes
    let mut nal_units = Vec::new();
    let mut pos = 0;
    
    while pos < data.len() {
        // Look for start code 0x00 0x00 0x00 0x01 or 0x00 0x00 0x01
        if pos + 3 < data.len() && data[pos] == 0 && data[pos+1] == 0 {
            let start_code_len = if data[pos+2] == 1 { 3 } else if pos + 4 < data.len() && data[pos+2] == 0 && data[pos+3] == 1 { 4 } else { 0 };
            
            if start_code_len > 0 {
                // Found start code, find next start code or end
                let nal_start = pos + start_code_len;
                let mut nal_end = data.len();
                
                for i in nal_start + 1..data.len() {
                    if i + 2 < data.len() && data[i] == 0 && data[i+1] == 0 && (data[i+2] == 1 || (i + 3 < data.len() && data[i+2] == 0 && data[i+3] == 1)) {
                        nal_end = i;
                        break;
                    }
                }
                
                if nal_end > nal_start {
                    nal_units.push(&data[nal_start..nal_end]);
                }
                pos = nal_end;
            } else {
                pos += 1;
            }
        } else {
            pos += 1;
        }
    }
    
    println!("Found {} NAL units", nal_units.len());
    for (i, nal) in nal_units.iter().enumerate() {
        let nal_type = if nal.len() > 0 { (nal[0] >> 1) & 0x3F } else { 0 };
        println!("NAL {}: type={} len={}", i, nal_type, nal.len());
    }
    
    // TODO: Actually decode the HEVC data
    println!("Raw HEVC parsing complete - full decoder not yet implemented");
}
