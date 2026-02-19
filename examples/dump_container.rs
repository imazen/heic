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
            ItemProperty::CleanAperture(clap) => {
                eprintln!(
                    "  [{}]: clap w={}/{} h={}/{} hoff={}/{} voff={}/{}",
                    i, clap.width_n, clap.width_d, clap.height_n, clap.height_d,
                    clap.horiz_off_n, clap.horiz_off_d, clap.vert_off_n, clap.vert_off_d
                );
            }
            ItemProperty::Rotation(rot) => {
                eprintln!("  [{}]: irot angle={}°", i, rot.angle);
            }
            ItemProperty::AuxiliaryType(aux_type) => {
                eprintln!("  [{}]: auxC type={:?}", i, aux_type);
            }
            ItemProperty::Unknown => {
                eprintln!("  [{}]: (unknown)", i);
            }
        }
    }
    eprintln!();

    eprintln!("=== Property Associations ===");
    for assoc in &container.property_associations {
        eprintln!(
            "  Item #{}: properties={:?}",
            assoc.item_id,
            assoc.properties
                .iter()
                .map(|(idx, essential)| format!("{}({})", idx, if *essential { "essential" } else { "non-ess" }))
                .collect::<Vec<_>>()
        );
    }
    eprintln!();

    if let Some(item) = container.primary_item() {
        eprintln!("=== Primary Item ===");
        eprintln!("  type: {:?}", item.item_type);
        eprintln!("  dimensions: {:?}", item.dimensions);
        eprintln!("  has hevc_config: {}", item.hevc_config.is_some());
        eprintln!("  rotation: {:?}", item.rotation);
        eprintln!("  clean_aperture: {:?}", item.clean_aperture);

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

    // Dump auxiliary images
    if let Some(ref item) = container.primary_item() {
        let mut alpha_ids = container.find_auxiliary_items(item.id, "urn:mpeg:hevc:2015:auxid");
        alpha_ids.extend(container.find_auxiliary_items(item.id, "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"));
        if !alpha_ids.is_empty() {
            eprintln!();
            eprintln!("=== Alpha Plane ===");
            for &aid in &alpha_ids {
                if let Some(aux_item) = container.get_item(aid) {
                    eprintln!(
                        "  Item #{}: type={:?} dims={:?} aux_type={:?}",
                        aid, aux_item.item_type, aux_item.dimensions, aux_item.auxiliary_type
                    );
                }
            }
        }

        let gainmap_ids = container.find_auxiliary_items(item.id, "urn:com:apple:photo:2020:aux:hdrgainmap");
        if !gainmap_ids.is_empty() {
            eprintln!();
            eprintln!("=== HDR Gain Map ===");
            for &gid in &gainmap_ids {
                if let Some(gm_item) = container.get_item(gid) {
                    eprintln!(
                        "  Item #{}: type={:?} dims={:?} aux_type={:?}",
                        gid, gm_item.item_type, gm_item.dimensions, gm_item.auxiliary_type
                    );
                }
            }
        }
    }

    // Dump EXIF items
    eprintln!();
    eprintln!("=== EXIF Items ===");
    for info in &container.item_infos {
        if info.item_type == FourCC(*b"Exif") {
            if let Some(data) = container.get_item_data(info.item_id) {
                eprintln!("  Exif item #{}: {} bytes", info.item_id, data.len());
                // HEIF Exif: 4 bytes offset to TIFF header, then the TIFF data
                if data.len() > 10 {
                    let tiff_offset = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
                    let tiff_start = 4 + tiff_offset;
                    if tiff_start < data.len() {
                        let tiff_data = &data[tiff_start..];
                        eprintln!("    TIFF header offset: {}", tiff_offset);
                        eprintln!("    TIFF header bytes: {:02x?}", &tiff_data[..tiff_data.len().min(16)]);
                        // Check byte order
                        if tiff_data.len() > 2 {
                            let byte_order = if tiff_data[0..2] == [0x4D, 0x4D] {
                                "big-endian"
                            } else if tiff_data[0..2] == [0x49, 0x49] {
                                "little-endian"
                            } else {
                                "unknown"
                            };
                            eprintln!("    Byte order: {}", byte_order);

                            // Parse IFD0 to find orientation tag (0x0112)
                            let is_le = tiff_data[0] == 0x49;
                            let read_u16 = |offset: usize| -> u16 {
                                if is_le {
                                    u16::from_le_bytes([tiff_data[offset], tiff_data[offset + 1]])
                                } else {
                                    u16::from_be_bytes([tiff_data[offset], tiff_data[offset + 1]])
                                }
                            };
                            let read_u32 = |offset: usize| -> u32 {
                                if is_le {
                                    u32::from_le_bytes([tiff_data[offset], tiff_data[offset + 1], tiff_data[offset + 2], tiff_data[offset + 3]])
                                } else {
                                    u32::from_be_bytes([tiff_data[offset], tiff_data[offset + 1], tiff_data[offset + 2], tiff_data[offset + 3]])
                                }
                            };

                            // IFD0 offset is at byte 4-7 of TIFF header
                            if tiff_data.len() > 8 {
                                let ifd0_offset = read_u32(4) as usize;
                                if ifd0_offset < tiff_data.len() - 2 {
                                    let num_entries = read_u16(ifd0_offset);
                                    eprintln!("    IFD0 at offset {}, {} entries", ifd0_offset, num_entries);
                                    for i in 0..num_entries as usize {
                                        let entry_offset = ifd0_offset + 2 + i * 12;
                                        if entry_offset + 12 > tiff_data.len() { break; }
                                        let tag = read_u16(entry_offset);
                                        let typ = read_u16(entry_offset + 2);
                                        let count = read_u32(entry_offset + 4);
                                        let value = read_u16(entry_offset + 8);
                                        if tag == 0x0112 {
                                            eprintln!("    ** Orientation tag (0x0112): value={} (type={}, count={})", value, typ, count);
                                            let orient_str = match value {
                                                1 => "Normal",
                                                2 => "Mirrored horizontal",
                                                3 => "Rotated 180",
                                                4 => "Mirrored vertical",
                                                5 => "Mirrored horizontal + rotated 270 CW",
                                                6 => "Rotated 90 CW",
                                                7 => "Mirrored horizontal + rotated 90 CW",
                                                8 => "Rotated 270 CW (= 90 CCW)",
                                                _ => "Unknown",
                                            };
                                            eprintln!("    ** Orientation meaning: {}", orient_str);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
