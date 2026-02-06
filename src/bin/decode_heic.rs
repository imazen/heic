//! Simple HEIC decoder binary for testing

use heic_decoder::HeicDecoder;
use std::env;
use std::fs;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <input.heic>", args[0]);
        process::exit(1);
    }

    let input_path = &args[1];
    let data = fs::read(input_path).expect("Failed to read input file");

    let decoder = HeicDecoder::new();
    
    // Also dump raw YUV for comparison with libde265
    match decoder.decode_to_frame(&data) {
        Ok(frame) => {
            let cw = frame.cropped_width();
            let ch = frame.cropped_height();
            println!("Decoded {}x{} frame (full {}x{})", cw, ch, frame.width, frame.height);
            println!("Bit depth: {}, Chroma format: {}", frame.bit_depth, frame.chroma_format);
            println!("Crop: left={} right={} top={} bottom={}", frame.crop_left, frame.crop_right, frame.crop_top, frame.crop_bottom);

            // Write RGB PPM
            let rgb = frame.to_rgb();
            let output_path = "output.ppm";
            let mut ppm = format!("P6\n{} {}\n255\n", cw, ch).into_bytes();
            ppm.extend_from_slice(&rgb);
            fs::write(output_path, &ppm).expect("Failed to write PPM");
            println!("Wrote RGB to {}", output_path);

            // Write raw YUV (I420 planar, same format as libde265 output)
            let shift = frame.bit_depth - 8;
            let mut yuv = Vec::new();
            // Y plane (cropped)
            for y in frame.crop_top..(frame.height - frame.crop_bottom) {
                for x in frame.crop_left..(frame.width - frame.crop_right) {
                    let idx = (y * frame.width + x) as usize;
                    yuv.push((frame.y_plane[idx] >> shift) as u8);
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
            fs::write("output.yuv", &yuv).expect("Failed to write YUV");
            println!("Wrote raw YUV to output.yuv ({} bytes)", yuv.len());
        }
        Err(e) => {
            eprintln!("Decode error: {:?}", e);
        }
    }
}
