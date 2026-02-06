//! Extract raw HEVC bitstream from HEIC file for libde265 testing

use heic_decoder::heif;
use std::env;
use std::fs;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <input.heic> <output.hevc>", args[0]);
        eprintln!("Extracts raw HEVC bitstream from HEIC container for libde265 testing");
        process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    let data = fs::read(input_path).expect("Failed to read input file");

    // Parse HEIF container
    let container = heif::parse(&data).expect("Failed to parse HEIF container");

    println!("HEIF Container Info:");
    println!("  Primary item ID: {}", container.primary_item_id);
    println!("  Item infos: {} items", container.item_infos.len());
    
    for info in &container.item_infos {
        println!(
            "    Item {}: type={:?}, name={:?}",
            info.item_id, info.item_type, info.item_name
        );
    }

    // Get primary item
    let primary_item = container.primary_item()
        .expect("No primary item found");

    println!("\nPrimary Item:");
    println!("  ID: {}", primary_item.id);
    println!("  Type: {:?}", primary_item.item_type);
    
    // Extract HEVC data - concatenate config NAL units and image data
    let mut hevc_data = Vec::new();
    
    if let Some(ref config) = primary_item.hevc_config {
        println!("  HEVC config: {} NAL units", config.nal_units.len());
        println!("  Length size: {}", config.length_size_minus_one + 1);
        
        // Add config NAL units with start codes
        for (i, nal) in config.nal_units.iter().enumerate() {
            println!("    NAL {}: {} bytes", i, nal.len());
            hevc_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // Start code
            hevc_data.extend_from_slice(nal);
        }
    }
    
    // Get the actual image data from mdat
    if let Some(item_data) = container.get_item_data(primary_item.id) {
        println!("\nImage data: {} bytes", item_data.len());
        
        // The image data is already in HEVC format with length prefixes
        // Convert length-prefixed NALs to start-code-prefixed NALs
        let length_size = primary_item.hevc_config.as_ref()
            .map(|c| (c.length_size_minus_one + 1) as usize)
            .unwrap_or(4);
        
        let mut pos = 0;
        let mut nal_count = 0;
        while pos + length_size <= item_data.len() {
            let nal_len = match length_size {
                1 => item_data[pos] as usize,
                2 => u16::from_be_bytes([item_data[pos], item_data[pos+1]]) as usize,
                4 => u32::from_be_bytes([item_data[pos], item_data[pos+1], item_data[pos+2], item_data[pos+3]]) as usize,
                _ => break,
            };
            
            if pos + length_size + nal_len > item_data.len() {
                break;
            }
            
            hevc_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // Start code
            hevc_data.extend_from_slice(&item_data[pos+length_size..pos+length_size+nal_len]);
            
            pos += length_size + nal_len;
            nal_count += 1;
        }
        println!("  Extracted {} NAL units from image data", nal_count);
    } else {
        println!("\nWarning: No image data found (HEVC config only)");
    }
    
    if hevc_data.is_empty() {
        eprintln!("Error: No HEVC data to write");
        process::exit(1);
    }
    
    fs::write(output_path, &hevc_data).expect("Failed to write HEVC file");
    println!("\nWrote {} bytes of HEVC data to {}", hevc_data.len(), output_path);
}
