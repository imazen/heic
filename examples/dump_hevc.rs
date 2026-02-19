// Dump the raw HEVC bitstream from a HEIC file in Annex B format
// Usage: cargo run --release --example dump_hevc <input.heic> <output.265>

fn main() {
    let input = std::env::args().nth(1).expect("input HEIC file path");
    let output = std::env::args().nth(2).expect("output .265 file path");

    let data = std::fs::read(&input).expect("read input");
    let container = heic_decoder::heif::parse(&data).expect("parse container");

    let primary_id = container.primary_item_id;
    let item = container.primary_item().expect("no primary item");

    let config = item.hevc_config.as_ref().expect("no hvcC config");
    let image_data = container.get_item_data(primary_id).expect("no image data");

    eprintln!("Config NAL units: {}", config.nal_units.len());
    eprintln!("Image data: {} bytes", image_data.len());
    eprintln!("Length size: {}", config.length_size_minus_one + 1);

    let mut out = Vec::new();
    let start_code = [0u8, 0, 0, 1];

    // Write parameter set NALs
    for nal_data in &config.nal_units {
        out.extend_from_slice(&start_code);
        out.extend_from_slice(nal_data);
        let nal_type = (nal_data[0] >> 1) & 0x3f;
        eprintln!("  Param NAL type={}, {} bytes", nal_type, nal_data.len());
    }

    // Write slice NALs
    let length_size = (config.length_size_minus_one + 1) as usize;
    let mut pos = 0;
    while pos + length_size <= image_data.len() {
        let nlen = match length_size {
            4 => u32::from_be_bytes(image_data[pos..pos + 4].try_into().unwrap()) as usize,
            2 => u16::from_be_bytes(image_data[pos..pos + 2].try_into().unwrap()) as usize,
            1 => image_data[pos] as usize,
            _ => panic!("bad length_size"),
        };
        pos += length_size;
        if pos + nlen > image_data.len() {
            break;
        }
        out.extend_from_slice(&start_code);
        out.extend_from_slice(&image_data[pos..pos + nlen]);
        let nal_type = (image_data[pos] >> 1) & 0x3f;
        eprintln!("  Slice NAL type={}, {} bytes", nal_type, nlen);
        pos += nlen;
    }

    std::fs::write(&output, &out).expect("write output");
    eprintln!("Written {} bytes to {}", out.len(), output);
}
