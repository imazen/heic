//! Slice investigator for HEVC bitstreams
//!
//! This tool analyzes HEVC bitstreams to understand slice structures,
//! NAL unit distribution, and slice segmentation for debugging purposes.
//! It creates detailed reports that can be analyzed offline.

use heic_decoder::hevc::{self, bitstream};
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::process;

struct SliceInvestigationReport {
    filename: String,
    total_bytes: usize,
    nal_units: Vec<NalInfo>,
    slice_headers: Vec<SliceInfo>,
    pps_map: HashMap<u8, heic_decoder::hevc::params::Pps>,
    sps_map: HashMap<u8, heic_decoder::hevc::params::Sps>,
    summary: SummaryStats,
}

struct NalInfo {
    index: usize,
    nal_type: bitstream::NalType,
    nal_type_raw: u8,
    layer_id: u8,
    temporal_id: u8,
    payload_size: usize,
    payload_start_offset: usize,
    slice_segment_address: Option<u32>,
    first_slice_segment_in_pic: Option<bool>,
    slice_type: Option<u8>,
}

struct SliceInfo {
    nal_index: usize,
    slice_segment_address: u32,
    first_slice_segment_in_pic: bool,
    slice_type: u8,
    num_entry_point_offsets: u32,
    slice_qp_delta: i8,
    slice_qp_y: i32,
}

struct SummaryStats {
    total_nals: usize,
    total_slices: usize,
    slice_types: HashMap<u8, usize>,
    ctu_counts: Vec<u32>,
    unique_addresses: Vec<u32>,
    pic_width_in_ctbs: u32,
    pic_height_in_ctbs: u32,
    total_expected_ctus: u32,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <input.hevc>", args[0]);
        eprintln!("Analyze HEVC bitstream slice structure and generate investigation report");
        process::exit(1);
    }

    let input_path = &args[1];
    let data = fs::read(input_path).expect("Failed to read input file");

    println!("Starting slice investigation for: {}", input_path);
    
    match investigate_slices(&data, input_path) {
        Ok(report) => {
            let output_filename = format!("slice_investigation_report_{}.txt", 
                input_path.rsplit_once('/').or_else(|| input_path.rsplit_once('\\')).map(|(_, name)| name).unwrap_or(input_path)
                    .trim_end_matches(".hevc")
                    .trim_end_matches(".hvc"));
            
            write_report(&report, &output_filename);
            println!("Slice investigation complete!");
            println!("Report saved to: {}", output_filename);
            print_summary(&report.summary);
        }
        Err(e) => {
            eprintln!("Error during slice investigation: {}", e);
            process::exit(1);
        }
    }
}

fn investigate_slices(data: &[u8], filename: &str) -> Result<SliceInvestigationReport, Box<dyn std::error::Error>> {
    println!("Parsing NAL units...");
    
    // Parse NAL units
    let nal_units = bitstream::parse_nal_units(data)?;
    println!("Found {} NAL units", nal_units.len());

    let mut report = SliceInvestigationReport {
        filename: filename.to_string(),
        total_bytes: data.len(),
        nal_units: Vec::new(),
        slice_headers: Vec::new(),
        pps_map: HashMap::new(),
        sps_map: HashMap::new(),
        summary: SummaryStats {
            total_nals: 0,
            total_slices: 0,
            slice_types: HashMap::new(),
            ctu_counts: Vec::new(),
            unique_addresses: Vec::new(),
            pic_width_in_ctbs: 0,
            pic_height_in_ctbs: 0,
            total_expected_ctus: 0,
        },
    };

    // First pass: collect parameter sets
    for (_i, nal) in nal_units.iter().enumerate() {
        match nal.nal_type {
            bitstream::NalType::SpsNut => {
                if let Ok(sps) = heic_decoder::hevc::params::parse_sps(&nal.payload) {
                    println!("  SPS {}: {}x{}", sps.sps_id, sps.pic_width_in_luma_samples, sps.pic_height_in_luma_samples);
                    report.sps_map.insert(sps.sps_id.clone(), sps.clone());
                }
            }
            bitstream::NalType::PpsNut => {
                if let Ok(pps) = heic_decoder::hevc::params::parse_pps(&nal.payload) {
                    println!("  PPS {}: pps_id={}", pps.pps_id, pps.pps_id);
                    report.pps_map.insert(pps.pps_id.clone(), pps.clone());
                }
            }
            _ => {}
        }
    }

    // Second pass: analyze all NALs and extract slice info
    let mut cumulative_payload_offset = 0;
    for (i, nal) in nal_units.iter().enumerate() {
        // Calculate where this NAL's payload starts in the original data
        let payload_start_offset = cumulative_payload_offset;
        
        // Update offset for next NAL (add header + payload size)
        cumulative_payload_offset += 2 + nal.payload.len(); // 2-byte header + payload
        
        let mut nal_info = NalInfo {
            index: i,
            nal_type: nal.nal_type,
            nal_type_raw: (nal.raw_data[0] >> 1) & 0x3F,
            layer_id: nal.nuh_layer_id,
            temporal_id: nal.nuh_temporal_id_plus1 - 1,
            payload_size: nal.payload.len(),
            payload_start_offset,
            slice_segment_address: None,
            first_slice_segment_in_pic: None,
            slice_type: None,
        };

        // If this is a slice NAL, try to extract slice header info
        if nal.nal_type.is_slice() {
            // Try parsing with each available PPS until one works
            let mut slice_parsed = false;
            for (&pps_id, pps) in &report.pps_map {
                // Find the SPS that this PPS references
                if let Some(sps) = report.sps_map.get(&pps.sps_id) {
                    match heic_decoder::hevc::slice::SliceHeader::parse(nal, sps, pps) {
                        Ok(parse_result) => {
                            let header = parse_result.header;
                            
                            // Record slice info
                            let slice_info = SliceInfo {
                                nal_index: i,
                                slice_segment_address: header.slice_segment_address,
                                first_slice_segment_in_pic: header.first_slice_segment_in_pic_flag,
                                slice_type: header.slice_type as u8,
                                num_entry_point_offsets: header.num_entry_point_offsets,
                                slice_qp_delta: header.slice_qp_delta,
                                slice_qp_y: header.slice_qp_y,
                            };
                            
                            report.slice_headers.push(slice_info);
                            
                            // Update NAL info with slice details
                            nal_info.slice_segment_address = Some(header.slice_segment_address);
                            nal_info.first_slice_segment_in_pic = Some(header.first_slice_segment_in_pic_flag);
                            nal_info.slice_type = Some(header.slice_type as u8);
                            
                            // Update stats
                            *report.summary.slice_types.entry(header.slice_type as u8).or_insert(0) += 1;
                            
                            // Calculate CTU counts if this is the first slice
                            if header.first_slice_segment_in_pic_flag {
                                let pic_width_in_ctbs = sps.pic_width_in_ctbs();
                                let pic_height_in_ctbs = sps.pic_height_in_ctbs();
                                
                                // Calculate how many CTUs this slice covers
                                let start_ctb = header.slice_segment_address;
                                let remaining_ctus = pic_width_in_ctbs * pic_height_in_ctbs - start_ctb;
                                
                                report.summary.ctu_counts.push(remaining_ctus);
                                report.summary.pic_width_in_ctbs = pic_width_in_ctbs;
                                report.summary.pic_height_in_ctbs = pic_height_in_ctbs;
                                report.summary.total_expected_ctus = pic_width_in_ctbs * pic_height_in_ctbs;
                            }
                            
                            report.summary.total_slices += 1;
                            slice_parsed = true;
                            
                            println!("  Slice {}: addr={}, type={}, qp_delta={}, qp_y={}, entry_points={}", 
                                     i, header.slice_segment_address, header.slice_type as u8, 
                                     header.slice_qp_delta, header.slice_qp_y, header.num_entry_point_offsets);
                            break;
                        }
                        Err(_) => {
                            // Try next PPS
                            continue;
                        }
                    }
                }
            }
            
            if !slice_parsed {
                eprintln!("  Warning: Could not parse slice header for NAL {} with any available PPS", i);
            }
        }

        report.nal_units.push(nal_info);
    }

    // Collect unique slice addresses
    let mut unique_addresses: Vec<u32> = report.slice_headers.iter()
        .map(|s| s.slice_segment_address)
        .collect();
    unique_addresses.sort();
    unique_addresses.dedup();
    report.summary.unique_addresses = unique_addresses;
    report.summary.total_nals = nal_units.len();

    Ok(report)
}

fn extract_sps_pps_ids(payload: &[u8]) -> Option<(u8, u8)> {
    if payload.len() < 2 {
        return None;
    }
    
    let mut reader = bitstream::BitstreamReader::new(payload);
    
    // Skip first_slice_segment_in_pic_flag
    if reader.read_bit().is_err() {
        return None;
    }
    
    // Try to read pps_id
    let pps_id = match reader.read_ue() {
        Ok(id) => id as u8,
        Err(_) => return None,
    };
    
    Some((0, pps_id)) // Simplified - in practice, we'd need to find the SPS ID from PPS
}

fn write_report(report: &SliceInvestigationReport, filename: &str) {
    let mut file = File::create(filename).expect("Could not create report file");
    
    writeln!(file, "HEVC SLICE INVESTIGATION REPORT").unwrap();
    writeln!(file, "================================").unwrap();
    writeln!(file, "").unwrap();
    
    writeln!(file, "File: {}", report.filename).unwrap();
    writeln!(file, "Total bytes: {}", report.total_bytes).unwrap();
    writeln!(file, "Total NAL units: {}", report.summary.total_nals).unwrap();
    writeln!(file, "Total slices: {}", report.summary.total_slices).unwrap();
    writeln!(file, "").unwrap();
    
    // Parameter Sets
    writeln!(file, "PARAMETER SETS").unwrap();
    writeln!(file, "==============").unwrap();
    for (id, sps) in &report.sps_map {
        writeln!(file, "SPS {}: {}x{}, CTBs: {}x{}", 
                 id, 
                 sps.pic_width_in_luma_samples, 
                 sps.pic_height_in_luma_samples,
                 sps.pic_width_in_ctbs(),
                 sps.pic_height_in_ctbs()).unwrap();
    }
    for (id, pps) in &report.pps_map {
        writeln!(file, "PPS {}: pps_id={}", id, pps.pps_id).unwrap();
    }
    writeln!(file, "").unwrap();
    
    // NAL Units
    writeln!(file, "NAL UNITS").unwrap();
    writeln!(file, "=========").unwrap();
    for nal in &report.nal_units {
        writeln!(file, "[{}] Type: {:?} ({}), Size: {}, Layer: {}, Temporal: {}", 
                 nal.index, 
                 nal.nal_type, 
                 nal.nal_type_raw,
                 nal.payload_size,
                 nal.layer_id,
                 nal.temporal_id).unwrap();
        
        if let Some(addr) = nal.slice_segment_address {
            writeln!(file, "    Slice Info: addr={}, first={}, type={}", 
                     addr, 
                     nal.first_slice_segment_in_pic.unwrap_or(false),
                     nal.slice_type.unwrap_or(255)).unwrap();
        }
    }
    writeln!(file, "").unwrap();
    
    // Slice Headers
    writeln!(file, "SLICE HEADERS").unwrap();
    writeln!(file, "==============").unwrap();
    for slice in &report.slice_headers {
        writeln!(file, "NAL {}: addr={}, first={}, type={}, entry_points={}, qp_delta={}, qp_y={}", 
                 slice.nal_index,
                 slice.slice_segment_address,
                 slice.first_slice_segment_in_pic,
                 slice.slice_type,
                 slice.num_entry_point_offsets,
                 slice.slice_qp_delta,
                 slice.slice_qp_y).unwrap();
    }
    writeln!(file, "").unwrap();
    
    // Summary Statistics
    writeln!(file, "SUMMARY STATISTICS").unwrap();
    writeln!(file, "==================").unwrap();
    writeln!(file, "Picture CTB dimensions: {}x{}", 
             report.summary.pic_width_in_ctbs, 
             report.summary.pic_height_in_ctbs).unwrap();
    writeln!(file, "Total expected CTUs: {}", report.summary.total_expected_ctus).unwrap();
    writeln!(file, "Unique slice addresses: {:?}", report.summary.unique_addresses).unwrap();
    
    writeln!(file, "Slice type distribution:").unwrap();
    for (slice_type, count) in &report.summary.slice_types {
        let type_name = match slice_type {
            0 => "B",
            1 => "P", 
            2 => "I",
            _ => "Unknown",
        };
        writeln!(file, "  {}: {} slices", type_name, count).unwrap();
    }
    
    if !report.summary.ctu_counts.is_empty() {
        writeln!(file, "CTU coverage per first slice: {:?}", report.summary.ctu_counts).unwrap();
    }
    
    writeln!(file, "").unwrap();
    
    // Analysis
    writeln!(file, "ANALYSIS").unwrap();
    writeln!(file, "========").unwrap();
    
    if report.summary.total_slices > 1 {
        writeln!(file, "✓ Multiple slices detected - this may explain why decoder stops early").unwrap();
        writeln!(file, "  - Unique addresses: {:?}", report.summary.unique_addresses).unwrap();
        
        if report.summary.unique_addresses.contains(&0) {
            writeln!(file, "  - Contains slice starting at address 0").unwrap();
        }
        
        if report.summary.unique_addresses.len() > 1 {
            writeln!(file, "  - Multiple slice segments suggest tiled/multi-slice encoding").unwrap();
        }
    } else {
        writeln!(file, "✓ Single slice detected - issue may be elsewhere").unwrap();
    }
    
    if report.summary.slice_types.get(&2).copied().unwrap_or(0) == report.summary.total_slices {
        writeln!(file, "✓ All slices are I-type (expected for still images)").unwrap();
    } else {
        writeln!(file, "⚠ Mixed slice types detected (unexpected for still images)").unwrap();
    }
    
    if report.summary.total_expected_ctus > 0 {
        let covered_ctus: u32 = report.summary.ctu_counts.iter().sum();
        writeln!(file, "Coverage: {}/{} CTUs ({:.1}%)", 
                 covered_ctus, 
                 report.summary.total_expected_ctus,
                 (covered_ctus as f64 / report.summary.total_expected_ctus as f64) * 100.0).unwrap();
    }
}

fn print_summary(summary: &SummaryStats) {
    println!("");
    println!("INVESTIGATION SUMMARY:");
    println!("  Total NALs: {}", summary.total_nals);
    println!("  Total slices: {}", summary.total_slices);
    println!("  Picture CTBs: {}x{} (total: {})", 
             summary.pic_width_in_ctbs, 
             summary.pic_height_in_ctbs, 
             summary.total_expected_ctus);
    println!("  Slice types: {:?}", summary.slice_types);
    println!("  Unique slice addresses: {:?}", summary.unique_addresses);
    
    if summary.total_expected_ctus > 0 {
        let covered_ctus: u32 = summary.ctu_counts.iter().sum();
        println!("  Coverage: {}/{} CTUs ({:.1}%)", 
                 covered_ctus, 
                 summary.total_expected_ctus,
                 (covered_ctus as f64 / summary.total_expected_ctus as f64) * 100.0);
    }
    
    if summary.total_slices > 1 {
        println!("  ⚠ Multiple slices detected - may need multi-slice support");
    }
}