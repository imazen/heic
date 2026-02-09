//! Simple HEIC decoder binary for testing

use heic_decoder::HeicDecoder;
use std::env;
use std::fs;
use std::path::Path;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <input.heic>", args[0]);
        process::exit(1);
    }

    let input_path = &args[1];
    let data = fs::read(input_path).expect("Failed to read input file");

    // Generate timestamp and base filename
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H%M%S").to_string();
    let input_stem = Path::new(input_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let base_name = format!("output_{}_{}", input_stem, timestamp);

    let decoder = HeicDecoder::new();
    
    // Also dump raw YUV for comparison with libde265
    match decoder.decode_to_frame(&data) {
        Ok(frame) => {
            let cw = frame.cropped_width();
            let ch = frame.cropped_height();
            println!("Decoded {}x{} frame (full {}x{})", cw, ch, frame.width, frame.height);
            println!("Bit depth: {}, Chroma format: {}", frame.bit_depth, frame.chroma_format);
            println!("Crop: left={} right={} top={} bottom={}", frame.crop_left, frame.crop_right, frame.crop_top, frame.crop_bottom);

            // Print color space info
            println!("\nColor Space:");
            println!("  Primaries: {:?}", frame.colorspace.primaries);
            println!("  Transfer: {:?}", frame.colorspace.transfer);
            println!("  Matrix: {:?}", frame.colorspace.matrix);
            println!("  Full Range: {}", frame.colorspace.full_range);
            if frame.colorspace.transfer.is_hdr() {
                println!("  ⚠️  HDR detected! Output will be tone-mapped to SDR.");
            }

            // Write RGB PPM
            let rgb = frame.to_rgb();
            let ppm_path = format!("{}.ppm", base_name);
            let mut ppm = format!("P6\n{} {}\n255\n", cw, ch).into_bytes();
            ppm.extend_from_slice(&rgb);
            fs::write(&ppm_path, &ppm).expect("Failed to write PPM");
            println!("Wrote RGB to {}", ppm_path);

            // Write raw YUV (I420 planar, same format as libde265 output)
            let shift = frame.bit_depth - 8;
            let mut yuv = Vec::new();
            // Y plane (cropped)
            for y in frame.crop_top..(frame.height - frame.crop_bottom) {
                for x in frame.crop_left..(frame.width - frame.crop_right) {
                    let idx = (y * frame.width + x) as usize;
                    let val = (frame.y_plane[idx] >> shift) as u8;
                    yuv.push(val);
                }
            }
            // Cb plane (cropped, half resolution for 4:2:0)
            let chroma_w = cw / 2;
            let chroma_h = ch / 2;
            let c_stride = frame.c_stride();
            let cx_start = frame.crop_left / 2;
            let cy_start = frame.crop_top / 2;
            for cy in cy_start..(cy_start + chroma_h) {
                for cx in cx_start..(cx_start + chroma_w) {
                    let idx = cy as usize * c_stride + cx as usize;
                    yuv.push((frame.cb_plane[idx] >> shift) as u8);
                }
            }
            // Cr plane
            for cy in cy_start..(cy_start + chroma_h) {
                for cx in cx_start..(cx_start + chroma_w) {
                    let idx = cy as usize * c_stride + cx as usize;
                    yuv.push((frame.cr_plane[idx] >> shift) as u8);
                }
            }
            let yuv_path = format!("{}.yuv", base_name);
            fs::write(&yuv_path, &yuv).expect("Failed to write YUV");
            println!("Wrote raw YUV to {} ({} bytes)", yuv_path, yuv.len());
        }
        Err(e) => {
            eprintln!("Decode error: {:?}", e);
        }
    }
}
