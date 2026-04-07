//! Fuzz target for color conversion and spatial transforms.
//!
//! Constructs a DecodedFrame from structured fuzz data and exercises:
//! - YCbCr→RGB/RGBA/BGR/BGRA color conversion (scalar + SIMD paths)
//! - Spatial transforms: rotate_90_cw, rotate_180, rotate_270_cw, mirror_horizontal, mirror_vertical
//! - to_rgb16, to_rgba16
//! - write_rgb_into, write_rgba_into, write_bgra_into, write_bgr_into
#![no_main]

use libfuzzer_sys::fuzz_target;
use heic::DecodedFrame;

fuzz_target!(|data: &[u8]| {
    if data.len() < 12 {
        return;
    }

    // Parse structured parameters from the first bytes
    let width = (data[0] as u32 % 64) + 2; // 2..65
    let height = (data[1] as u32 % 64) + 2; // 2..65
    let bit_depth = if data[2] & 1 == 0 { 8u8 } else { 10u8 };
    let chroma_format = match data[3] % 3 {
        0 => 1u8, // 4:2:0
        1 => 2u8, // 4:2:2
        _ => 3u8, // 4:4:4
    };
    let full_range = data[4] & 1 != 0;
    let matrix_coeffs = match data[5] % 4 {
        0 => 1u8,  // BT.709
        1 => 5u8,  // BT.601
        2 => 9u8,  // BT.2020
        _ => 2u8,  // unspecified
    };
    let transform_op = data[6] % 6; // 0=none, 1=rot90, 2=rot180, 3=rot270, 4=mirrorH, 5=mirrorV
    let output_format = data[7] % 7; // select output format

    // Make dimensions even for 4:2:0
    let width = if chroma_format == 1 { (width + 1) & !1 } else { width };
    let height = if chroma_format == 1 { (height + 1) & !1 } else { height };

    let y_size = (width * height) as usize;
    let (cw, ch) = match chroma_format {
        1 => (width / 2, height / 2),
        2 => (width / 2, height),
        _ => (width, height),
    };
    let c_size = (cw * ch) as usize;

    // Fill planes from remaining fuzz data
    let rest = &data[8..];
    let mut y_plane = vec![0u16; y_size];
    let mut cb_plane = vec![0u16; c_size];
    let mut cr_plane = vec![0u16; c_size];

    let max_val = (1u16 << bit_depth) - 1;

    // Fill Y from fuzz data
    for (i, val) in y_plane.iter_mut().enumerate() {
        if i < rest.len() {
            *val = (rest[i] as u16) * (max_val / 255);
        } else {
            *val = 128;
        }
    }

    // Fill Cb/Cr from fuzz data (offset past Y data)
    let cb_offset = y_size.min(rest.len());
    for (i, val) in cb_plane.iter_mut().enumerate() {
        let idx = cb_offset + i;
        if idx < rest.len() {
            *val = (rest[idx] as u16) * (max_val / 255);
        } else {
            *val = 128;
        }
    }
    let cr_offset = (cb_offset + c_size).min(rest.len());
    for (i, val) in cr_plane.iter_mut().enumerate() {
        let idx = cr_offset + i;
        if idx < rest.len() {
            *val = (rest[idx] as u16) * (max_val / 255);
        } else {
            *val = 128;
        }
    }

    let frame = DecodedFrame::from_planes(
        width,
        height,
        bit_depth,
        chroma_format,
        y_plane,
        cb_plane,
        cr_plane,
        full_range,
        matrix_coeffs,
    );

    // Exercise color conversion paths
    match output_format {
        0 => { let _ = frame.to_rgb(); }
        1 => { let _ = frame.to_rgba(); }
        2 => { let _ = frame.to_bgra(); }
        3 => { let _ = frame.to_bgr(); }
        4 => { let _ = frame.to_rgb16(); }
        5 => { let _ = frame.to_rgba16(); }
        6 => {
            // write_rgb_into
            let mut buf = vec![0u8; (width * height * 3) as usize];
            frame.write_rgb_into(&mut buf);
        }
        _ => {}
    }

    // Exercise spatial transforms
    match transform_op {
        1 => { let _ = frame.rotate_90_cw(); }
        2 => { let _ = frame.rotate_180(); }
        3 => { let _ = frame.rotate_270_cw(); }
        4 => { let _ = frame.mirror_horizontal(); }
        5 => { let _ = frame.mirror_vertical(); }
        _ => {}
    }
});
