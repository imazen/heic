use heic_decoder::heif::{FourCC, ItemProperty, ItemType};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/lilith/work/heic/libheif/examples/example.heic".to_string());
    let data = std::fs::read(&path).expect("Failed to read file");
    eprintln!("File: {} ({} bytes)", path, data.len());

    let container = heic_decoder::heif::parse(&data).expect("parse failed");
    eprintln!("Brand: {}", container.brand);
    eprintln!("Primary item ID: {}", container.primary_item_id);
    eprintln!();

    eprintln!("=== Item Infos ===");
    for info in &container.item_infos {
        eprintln!(
            "  Item #{}: type={} name={:?} hidden={}",
            info.item_id, info.item_type, info.item_name, info.hidden
        );
    }
    eprintln!();

    eprintln!("=== Item Locations ===");
    for loc in &container.item_locations {
        eprintln!(
            "  Item #{}: method={} base_offset={} extents={:?}",
            loc.item_id, loc.construction_method, loc.base_offset, loc.extents
        );
        let total: u64 = loc.extents.iter().map(|(_, l)| l).sum();
        eprintln!("    total data: {} bytes", total);
    }
    eprintln!();

    eprintln!("=== Item References ===");
    for iref in &container.item_references {
        eprintln!(
            "  {} from item #{} -> {:?}",
            iref.reference_type, iref.from_item_id, iref.to_item_ids
        );
    }
    eprintln!();

    eprintln!("=== Properties ===");
    for (i, prop) in container.properties.iter().enumerate() {
        match prop {
            ItemProperty::ImageExtents(ext) => {
                eprintln!("  [{}]: ispe {}x{}", i, ext.width, ext.height);
            }
            ItemProperty::HevcConfig(cfg) => {
                eprintln!(
                    "  [{}]: hvcC profile={} level={} nal_units={}",
                    i, cfg.general_profile_idc, cfg.general_level_idc, cfg.nal_units.len()
                );
            }
            ItemProperty::ColorInfo(_) => {
                eprintln!("  [{}]: colr", i);
            }
            ItemProperty::Unknown => {
                eprintln!("  [{}]: (unknown)", i);
            }
        }
    }
    eprintln!();

    if let Some(item) = container.primary_item() {
        eprintln!("=== Primary Item ===");
        eprintln!("  type: {:?}", item.item_type);
        eprintln!("  dimensions: {:?}", item.dimensions);
        eprintln!("  has hevc_config: {}", item.hevc_config.is_some());

        if item.item_type == ItemType::Grid {
            if let Some(grid_data) = container.get_item_data(item.id) {
                eprintln!(
                    "  grid descriptor ({} bytes): {:02x?}",
                    grid_data.len(),
                    grid_data
                );
                if grid_data.len() >= 8 {
                    let flags = grid_data[1];
                    let rows_minus1 = grid_data[2];
                    let cols_minus1 = grid_data[3];
                    if (flags & 1) != 0 && grid_data.len() >= 12 {
                        let w = u32::from_be_bytes([
                            grid_data[4],
                            grid_data[5],
                            grid_data[6],
                            grid_data[7],
                        ]);
                        let h = u32::from_be_bytes([
                            grid_data[8],
                            grid_data[9],
                            grid_data[10],
                            grid_data[11],
                        ]);
                        eprintln!(
                            "  grid: {}x{} tiles, output {}x{} (32-bit)",
                            cols_minus1 + 1,
                            rows_minus1 + 1,
                            w,
                            h
                        );
                    } else {
                        let w = u16::from_be_bytes([grid_data[4], grid_data[5]]) as u32;
                        let h = u16::from_be_bytes([grid_data[6], grid_data[7]]) as u32;
                        eprintln!(
                            "  grid: {}x{} tiles, output {}x{} (16-bit)",
                            cols_minus1 + 1,
                            rows_minus1 + 1,
                            w,
                            h
                        );
                    }
                }
            }

            let tile_ids = container.get_item_references(item.id, FourCC::DIMG);
            eprintln!("  tile items (dimg): {:?}", tile_ids);
        }

        if let Some(data) = container.get_item_data(item.id) {
            eprintln!("  data length: {} bytes", data.len());
        } else {
            eprintln!("  data: NOT FOUND");
        }
    }
}
