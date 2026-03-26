//! Extract raw HEVC bitstream (Annex B format) from HEIC container
//! for use with dec265 or other decoders.

use heic::heif;

fn heic_base_dir() -> String {
    std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into())
}

fn example_heic() -> String {
    format!("{}/libheif/examples/example.heic", heic_base_dir())
}
const OUTPUT_H265: &str = "/tmp/example.h265";

#[test]
#[ignore]
fn extract_annexb() {
    let data = std::fs::read(example_heic()).expect("Failed to read HEIC");
    let container = heif::parse(&data, &heic::Unstoppable).expect("Failed to parse container");

    let item = container.primary_item().expect("No primary item");
    let config = item.hevc_config.as_ref().expect("No HEVC config");
    let image_data = container.get_item_data(item.id).expect("No image data");
    let length_size = (config.length_size_minus_one + 1) as usize;

    let start_code: &[u8] = &[0x00, 0x00, 0x00, 0x01];
    let mut output = Vec::new();

    // Write parameter sets (VPS, SPS, PPS) with start codes
    for nal_data in &config.nal_units {
        output.extend_from_slice(start_code);
        output.extend_from_slice(nal_data);
    }

    // Write slice NAL units with start codes
    let mut pos = 0;
    while pos + length_size <= image_data.len() {
        let nal_len = match length_size {
            1 => image_data[pos] as usize,
            2 => u16::from_be_bytes([image_data[pos], image_data[pos + 1]]) as usize,
            4 => u32::from_be_bytes([
                image_data[pos],
                image_data[pos + 1],
                image_data[pos + 2],
                image_data[pos + 3],
            ]) as usize,
            _ => panic!("unsupported length size"),
        };
        pos += length_size;

        if pos + nal_len > image_data.len() {
            panic!("NAL length exceeds data");
        }

        output.extend_from_slice(start_code);
        output.extend_from_slice(&image_data[pos..pos + nal_len]);
        pos += nal_len;
    }

    std::fs::write(OUTPUT_H265, &output).expect("Failed to write .h265");
    println!("Wrote {} bytes to {}", output.len(), OUTPUT_H265);
    println!(
        "  {} parameter sets, length_size={}",
        config.nal_units.len(),
        length_size
    );
}
