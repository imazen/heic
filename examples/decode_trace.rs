fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/lilith/work/heic/libheif/examples/example.heic".to_string());
    let output_path = std::env::args().nth(2);
    let data = std::fs::read(&path).expect("Failed to read test file");
    let decoder = heic_decoder::HeicDecoder::new();
    let frame = decoder.decode_to_frame(&data).expect("decode failed");

    // Write PPM if output path provided
    if let Some(ppm_path) = &output_path {
        let w = frame.cropped_width();
        let h = frame.cropped_height();
        let rgb = frame.to_rgb();
        let header = format!("P6\n{} {}\n255\n", w, h);
        let mut ppm = Vec::with_capacity(header.len() + rgb.len());
        ppm.extend_from_slice(header.as_bytes());
        ppm.extend_from_slice(&rgb);
        std::fs::write(ppm_path, &ppm).expect("write PPM");
        eprintln!("Wrote PPM: {}x{} to {}", w, h, ppm_path);
    }

    // Write raw YUV (cropped) for comparison with libde265 --disable-deblocking --disable-sao
    let w = frame.cropped_width() as usize;
    let h = frame.cropped_height() as usize;
    let cw = w / 2;
    let ch = h / 2;

    let mut yuv = Vec::with_capacity(w * h + cw * ch * 2);

    // Y plane (cropped)
    for y in frame.crop_top..(frame.crop_top + h as u32) {
        for x in frame.crop_left..(frame.crop_left + w as u32) {
            yuv.push(frame.y_plane[(y * frame.width + x) as usize] as u8);
        }
    }

    // Cb plane (cropped, half res)
    let c_stride = frame.c_stride();
    let crop_cy = frame.crop_top / 2;
    let crop_cx = frame.crop_left / 2;
    for y in crop_cy..(crop_cy + ch as u32) {
        for x in crop_cx..(crop_cx + cw as u32) {
            yuv.push(frame.cb_plane[y as usize * c_stride + x as usize] as u8);
        }
    }

    // Cr plane (cropped, half res)
    for y in crop_cy..(crop_cy + ch as u32) {
        for x in crop_cx..(crop_cx + cw as u32) {
            yuv.push(frame.cr_plane[y as usize * c_stride + x as usize] as u8);
        }
    }

    std::fs::write("/tmp/our_decoder.yuv", &yuv).expect("write YUV");
    eprintln!("Wrote YUV: {}x{} ({} bytes)", w, h, yuv.len());
}
