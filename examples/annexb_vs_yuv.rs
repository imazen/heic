//! Decode a raw Annex-B HEVC bitstream with `heic::VideoDecoder` and compare
//! every frame, plane by plane, against a reference planar YUV file (as
//! written by libde265's `dec265 -o`): native bit depth (8-bit = 1 byte,
//! >8-bit = little-endian u16), conformance-window cropped, chroma planes
//! sized per `chroma_format` (4:0:0 none, 4:2:0 w/2 x h/2, 4:2:2 w/2 x h,
//! 4:4:4 w x h).
//!
//! Usage:
//!   cargo run --release --features backend-rust,std --example annexb_vs_yuv \
//!       <stream.bit> <reference.yuv> [--write out.yuv]
//!
//! Exit status is non-zero on any sample mismatch, so it doubles as a gate for
//! the ITU RExt conformance vectors (Main_422_10_A/B_RExt_Sony,
//! ADJUST_IPRED_ANGLE_A_RExt_Mitsubishi, GENERAL_10b_422_RExt_Sony) that
//! `conformance/fetch.sh` does not cover.

use std::io::Write;

fn plane_dims(chroma_format: u8, w: u32, h: u32) -> (u32, u32) {
    match chroma_format {
        0 => (0, 0),
        1 => (w.div_ceil(2), h.div_ceil(2)),
        2 => (w.div_ceil(2), h),
        _ => (w, h),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: annexb_vs_yuv <stream.bit> <reference.yuv> [--write out.yuv]");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1]).expect("read bitstream");
    let reference = std::fs::read(&args[2]).expect("read reference yuv");
    let write_path = args
        .iter()
        .position(|a| a == "--write")
        .and_then(|i| args.get(i + 1).cloned());

    let mut decoder = heic::VideoDecoder::new(16);
    let frames = match decoder.decode_annex_b(&data) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("decode failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("decoded {} frame(s)", frames.len());

    let mut out = write_path.map(|p| std::fs::File::create(p).expect("create output"));
    let mut ref_pos = 0usize;
    let mut any_mismatch = false;

    for (fi, f) in frames.iter().enumerate() {
        let w = f.cropped_width();
        let h = f.cropped_height();
        let (cw, ch) = plane_dims(f.chroma_format, w, h);
        let (sub_x, sub_y) = match f.chroma_format {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let bytes_per = if f.bit_depth > 8 { 2 } else { 1 };
        eprintln!(
            "frame {fi}: {w}x{h} chroma_format={} bit_depth={} chroma {cw}x{ch}",
            f.chroma_format, f.bit_depth
        );

        let planes: [(&str, &[u16], usize, u32, u32, u32, u32); 3] = [
            ("Y", &f.y_plane, f.y_stride(), f.crop_left, f.crop_top, w, h),
            (
                "Cb",
                &f.cb_plane,
                f.c_stride(),
                f.crop_left / sub_x,
                f.crop_top / sub_y,
                cw,
                ch,
            ),
            (
                "Cr",
                &f.cr_plane,
                f.c_stride(),
                f.crop_left / sub_x,
                f.crop_top / sub_y,
                cw,
                ch,
            ),
        ];
        for (name, plane, stride, x0, y0, pw, ph) in planes {
            let n = (pw * ph) as usize;
            let need = n * bytes_per;
            if reference.len() < ref_pos + need {
                eprintln!("  {name}: reference file too short (frame {fi})");
                any_mismatch = true;
                break;
            }
            let refp = &reference[ref_pos..ref_pos + need];
            ref_pos += need;
            let mut mismatches = 0usize;
            let mut max_diff = 0i32;
            let mut first = None;
            for y in 0..ph {
                for x in 0..pw {
                    let o = plane[(y0 + y) as usize * stride + (x0 + x) as usize];
                    let i = (y * pw + x) as usize;
                    let e = if bytes_per == 2 {
                        u16::from_le_bytes([refp[2 * i], refp[2 * i + 1]])
                    } else {
                        u16::from(refp[i])
                    };
                    if let Some(ref mut file) = out {
                        if bytes_per == 2 {
                            file.write_all(&o.to_le_bytes()).unwrap();
                        } else {
                            file.write_all(&[o as u8]).unwrap();
                        }
                    }
                    if o != e {
                        mismatches += 1;
                        max_diff = max_diff.max((i32::from(o) - i32::from(e)).abs());
                        if first.is_none() {
                            first = Some((x, y, o, e));
                        }
                    }
                }
            }
            if mismatches == 0 {
                eprintln!("  {name}: exact ({n} samples)");
            } else {
                any_mismatch = true;
                let (x, y, o, e) = first.unwrap();
                eprintln!(
                    "  {name}: {mismatches}/{n} samples differ, max |diff| {max_diff}, first at ({x},{y}) ours={o} ref={e}"
                );
            }
        }
    }
    if ref_pos != reference.len() {
        eprintln!(
            "reference has {} trailing bytes (frame count mismatch?)",
            reference.len() - ref_pos
        );
        any_mismatch = true;
    }
    if any_mismatch {
        std::process::exit(1);
    }
    eprintln!("ALL FRAMES EXACT");
}
