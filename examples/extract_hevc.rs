/// Extract raw HEVC Annex B bitstream from a HEIC file for use with dec265
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        "/home/lilith/work/heic/libheif/examples/example.heic".to_string()
    });
    let data = std::fs::read(&path).expect("read");
    let container = heic_decoder::heif::parse(&data).expect("parse");

    // Find primary image item
    let item = container.primary_item().expect("no primary item");
    eprintln!("Primary item: id={}, type={:?}", item.id, item.item_type);

    let config = item.hevc_config.as_ref().expect("no hvcC config");

    // Get image data
    let image_data = container.get_item_data(item.id).expect("no image data");

    // Write Annex B: parameter sets + slice data
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    use std::io::Write;

    // Write parameter set NALs with start codes
    for nal_data in &config.nal_units {
        out.write_all(&[0, 0, 0, 1]).unwrap();
        out.write_all(nal_data).unwrap();
    }

    // Write slice NALs with start codes
    let length_size = (config.length_size_minus_one + 1) as usize;
    let mut pos = 0;
    while pos + length_size <= image_data.len() {
        let mut nal_len = 0u32;
        for i in 0..length_size {
            nal_len = (nal_len << 8) | image_data[pos + i] as u32;
        }
        pos += length_size;
        let end = pos + nal_len as usize;
        if end > image_data.len() {
            break;
        }
        out.write_all(&[0, 0, 0, 1]).unwrap();
        out.write_all(&image_data[pos..end]).unwrap();
        pos = end;
    }
}
