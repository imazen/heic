// Decode a HEIC image and optionally write a PPM file.
//
// Usage:
//   cargo run --example decode -- image.heic              # print info
//   cargo run --example decode -- image.heic output.ppm   # decode to PPM
//   cargo run --example decode -- image.heic --info       # metadata only
//   cargo run --example decode -- image.heic --thumbnail  # decode thumbnail
//   cargo run --example decode -- image.heic --into       # use decode_into path

use heic::{DecoderConfig, ImageInfo, Limits, PixelLayout};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "Usage: {} <image.heic> [output.ppm] [--info] [--thumbnail] [--into]",
            args[0]
        );
        std::process::exit(1);
    }

    let input = &args[1];
    let data = std::fs::read(input).unwrap_or_else(|e| {
        eprintln!("Failed to read {}: {}", input, e);
        std::process::exit(1);
    });

    let flags: Vec<&str> = args.iter().skip(2).map(|s| s.as_str()).collect();
    let output_path = flags.iter().find(|s| !s.starts_with('-')).copied();
    let info_only = flags.contains(&"--info");
    let thumbnail = flags.contains(&"--thumbnail");
    let use_into = flags.contains(&"--into");

    // Probe metadata without decoding
    match ImageInfo::from_bytes(&data) {
        Ok(info) => {
            println!("Image: {}x{}", info.width, info.height);
            println!("  bit_depth:      {}", info.bit_depth);
            println!(
                "  chroma:         {}",
                match info.chroma_format {
                    0 => "monochrome",
                    1 => "4:2:0",
                    2 => "4:2:2",
                    3 => "4:4:4",
                    _ => "unknown",
                }
            );
            println!("  alpha:          {}", info.has_alpha);
            println!("  exif:           {}", info.has_exif);
            println!("  xmp:            {}", info.has_xmp);
            println!("  thumbnail:      {}", info.has_thumbnail);
            let rgb_size = info.output_buffer_size(PixelLayout::Rgb8).unwrap_or(0);
            println!(
                "  rgb buffer:     {} bytes ({:.1} MB)",
                rgb_size,
                rgb_size as f64 / 1e6
            );
        }
        Err(e) => {
            eprintln!("Probe failed: {}", e);
            std::process::exit(1);
        }
    }

    if info_only {
        return;
    }

    let decoder = DecoderConfig::new();

    // Extract metadata
    match decoder.extract_exif(&data) {
        Ok(Some(exif)) => println!("  EXIF:           {} bytes", exif.len()),
        Ok(None) => {}
        Err(e) => eprintln!("  EXIF error:     {}", e),
    }
    match decoder.extract_xmp(&data) {
        Ok(Some(xmp)) => println!("  XMP:            {} bytes", xmp.len()),
        Ok(None) => {}
        Err(e) => eprintln!("  XMP error:      {}", e),
    }

    // Thumbnail decode
    if thumbnail {
        match decoder.decode_thumbnail(&data, PixelLayout::Rgb8) {
            Ok(Some(thumb)) => {
                println!(
                    "\nThumbnail: {}x{} ({} bytes)",
                    thumb.width,
                    thumb.height,
                    thumb.data.len()
                );
                if let Some(path) = output_path {
                    write_ppm(path, &thumb.data, thumb.width, thumb.height);
                    println!("Wrote thumbnail to {}", path);
                }
            }
            Ok(None) => println!("\nNo thumbnail found"),
            Err(e) => eprintln!("\nThumbnail error: {}", e),
        }
        return;
    }

    // Set resource limits
    let mut limits = Limits::default();
    limits.max_pixels = Some(64_000_000);
    limits.max_memory_bytes = Some(512 * 1024 * 1024);

    // Decode
    let layout = PixelLayout::Rgb8;
    let start = std::time::Instant::now();

    if use_into {
        // decode_into: pre-allocate buffer, uses streaming for eligible grids
        let info = ImageInfo::from_bytes(&data).unwrap();
        let size = info.output_buffer_size(layout).unwrap();
        let mut buf = vec![0u8; size];
        let (w, h) = decoder
            .decode_request(&data)
            .with_output_layout(layout)
            .with_limits(&limits)
            .decode_into(&mut buf)
            .unwrap_or_else(|e| {
                eprintln!("Decode error: {}", e);
                std::process::exit(1);
            });
        let elapsed = start.elapsed();
        println!(
            "\nDecoded {}x{} in {:.1}ms (decode_into)",
            w,
            h,
            elapsed.as_secs_f64() * 1000.0
        );

        if let Some(path) = output_path {
            write_ppm(path, &buf, w, h);
            println!("Wrote {}", path);
        }
    } else {
        // decode: returns owned Vec
        let output = decoder
            .decode_request(&data)
            .with_output_layout(layout)
            .with_limits(&limits)
            .decode()
            .unwrap_or_else(|e| {
                eprintln!("Decode error: {}", e);
                std::process::exit(1);
            });
        let elapsed = start.elapsed();
        println!(
            "\nDecoded {}x{} in {:.1}ms ({} bytes)",
            output.width,
            output.height,
            elapsed.as_secs_f64() * 1000.0,
            output.data.len()
        );

        if let Some(path) = output_path {
            write_ppm(path, &output.data, output.width, output.height);
            println!("Wrote {}", path);
        }
    }
}

fn write_ppm(path: &str, rgb: &[u8], width: u32, height: u32) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).expect("create output file");
    write!(f, "P6\n{} {}\n255\n", width, height).unwrap();
    f.write_all(rgb).unwrap();
}
