#![cfg(all(feature = "backend-rust", feature = "zencodec"))]

#[path = "support/phase3_fixtures.rs"]
mod fixtures;
use fixtures::*;

use heic::{DecoderConfig, HeicDecoderConfig};
use zencodec::decode::{
    Decode, DecodeJob, DecodeOutput, DecodeRowSink, DecoderConfig as _, RowStrip, SinkError,
};
use zencodec::{CodecErrorExt, GainMapRender, OrientationHint};
use zenpixels::{
    Cicp, ColorSpace, DiffuseWhite, Orientation, PixelFormat, PixelViewMut, Primaries, SampleType,
    SignalRange, Transfer,
};

fn decode(data: &[u8], hint: OrientationHint, render: GainMapRender) -> DecodeOutput {
    HeicDecoderConfig::new()
        .job()
        .with_orientation(hint)
        .with_gain_map_render(render)
        .decoder(data.into(), &[])
        .unwrap()
        .decode()
        .unwrap()
}

fn words(output: &DecodeOutput) -> Vec<u16> {
    let view = output.pixels();
    (0..view.height())
        .flat_map(|y| view.row_as::<u16>(y).unwrap())
        .copied()
        .collect()
}

fn widen(code: u16, bits: u8) -> u16 {
    let max = (1u32 << bits) - 1;
    ((u32::from(code) * 65535 + max / 2) / max) as u16
}

fn bt709(y: u16, cb: u16, cr: u16, bits: u8, full_range: bool) -> [u16; 3] {
    let q = f64::from(1u16 << (bits - 8));
    let max = f64::from((1u16 << bits) - 1);
    let (offset, y_span, c_span) = if full_range {
        (0.0, max, max)
    } else {
        (16.0 * q, 219.0 * q, 224.0 * q)
    };
    let y = (f64::from(y) - offset) / y_span;
    let cb = (f64::from(cb) - 128.0 * q) / c_span;
    let cr = (f64::from(cr) - 128.0 * q) / c_span;
    let r = y + 2.0 * (1.0 - 0.2126) * cr;
    let b = y + 2.0 * (1.0 - 0.0722) * cb;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    [r, g, b].map(|x| (x.clamp(0.0, 1.0) * max).round() as u16)
}

#[test]
fn high_bit_rgb_uses_native_chroma_and_coded_crop_phase() {
    for (bits, nals) in [(10, YUV10), (12, YUV12)] {
        let q = 1 << (bits - 8);
        for full_range in [false, true] {
            let mut base = image(nals, W, H, bits, 1);
            base.properties.push(nclx(1, 1, 1, full_range));
            let data = heif(std::slice::from_ref(&base), &[]);
            let frame = DecoderConfig::new().decode_to_frame(&data).unwrap();
            assert_eq!(
                (frame.bit_depth, frame.chroma_format, frame.full_range),
                (bits, 1, full_range)
            );
            let mut expected = Vec::new();
            for y in 0..H {
                for x in 0..W {
                    let luma = (30 + (x + 2 * y) % 45) as u16 * q;
                    let cb = (96 + 56 * (x / 2 % 2)) as u16 * q;
                    let cr = (110 + 22 * (y / 2 % 2)) as u16 * q;
                    assert_eq!(frame.y_plane[(y * W + x) as usize], luma);
                    assert_eq!(frame.cb_plane[(y / 2 * (W / 2) + x / 2) as usize], cb);
                    assert_eq!(frame.cr_plane[(y / 2 * (W / 2) + x / 2) as usize], cr);
                    expected.extend(bt709(luma, cb, cr, bits, full_range).map(|v| widen(v, bits)));
                }
            }
            let anchor = match (bits, full_range) {
                (10, false) => [0, 131, 0],
                (10, true) => [7, 178, 0],
                (12, false) => [0, 525, 0],
                (12, true) => [26, 711, 0],
                _ => unreachable!(),
            };
            assert_eq!(&expected[..3], &anchor.map(|v| widen(v, bits)));
            assert_eq!(frame.to_rgb16().unwrap(), expected);
            assert_eq!(
                words(&decode(
                    &data,
                    OrientationHint::Preserve,
                    GainMapRender::BaseOnly
                )),
                expected
            );

            base.properties.push(aperture_x1());
            let data = heif(&[base], &[]);
            let cropped = DecoderConfig::new().decode_to_frame(&data).unwrap();
            let expected: Vec<_> = expected
                .as_chunks::<{ W as usize * 3 }>()
                .0
                .iter()
                .flat_map(|row| row[3..].iter().copied())
                .collect();
            assert_eq!((cropped.crop_left, cropped.cropped_width()), (1, W - 1));
            assert_eq!(cropped.to_rgb16().unwrap(), expected);
            assert_eq!(
                words(&decode(
                    &data,
                    OrientationHint::Preserve,
                    GainMapRender::BaseOnly
                )),
                expected
            );
        }
    }
}

fn alpha(small: bool) -> Item {
    let mut item = if small {
        image(GREY_SMALL, 16, 16, 8, 0)
    } else {
        image(GREY, W, H, 8, 0)
    };
    item.properties
        .extend([nclx(1, 13, 1, true), auxc(b"urn:mpeg:hevc:2015:auxid:1")]);
    item
}

#[test]
fn mixed_depth_alpha_has_its_own_exact_grid() {
    let mut base = image(YUV12, W, H, 12, 1);
    base.properties.push(nclx(1, 1, 1, false));
    let data = heif(&[base, alpha(false)], &[(b"auxl", 2, &[1])]);
    let frame = DecoderConfig::new().decode_to_frame(&data).unwrap();
    let wide = frame.to_rgba16().unwrap();
    let packed = frame.to_rgba().unwrap();
    assert_eq!((frame.bit_depth, frame.alpha_bit_depth), (12, Some(8)));
    assert_eq!(wide[3], 7710);
    for (i, pixel) in wide.as_chunks::<4>().0.iter().enumerate() {
        let code = ((i + 30) % 256) as u8;
        assert_eq!(pixel[3], u16::from(code) * 257);
        assert_eq!(packed[i * 4 + 3], code);
    }
    let out = decode(&data, OrientationHint::Preserve, GainMapRender::BaseOnly);
    assert_eq!(words(&out), wide);
    let depth = out.pixels().facts().depth.unwrap();
    assert_eq!((depth.color_bits(), depth.alpha_bits()), (12, 8));
}

#[derive(Default)]
struct PaddedSink {
    data: Vec<u8>,
    stride: usize,
    width: u32,
    height: u32,
}

impl DecodeRowSink for PaddedSink {
    fn begin(&mut self, w: u32, h: u32, spec: PixelFormat) -> Result<(), SinkError> {
        assert_eq!(spec.layout, zenpixels::SampleLayout::RGBA8);
        self.stride = w as usize * 4 + 7;
        self.data = vec![0xcd; self.stride * h as usize];
        self.width = w;
        self.height = h;
        Ok(())
    }
    fn max_buffer_rows(&self) -> std::num::NonZeroU32 {
        std::num::NonZeroU32::new(3).unwrap()
    }
    fn provide_next_buffer(
        &mut self,
        strip: RowStrip,
        format: PixelFormat,
    ) -> Result<PixelViewMut<'_>, SinkError> {
        assert!(strip.height <= 3);
        let bytes = &mut self.data
            [strip.y as usize * self.stride..(strip.y + strip.height) as usize * self.stride];
        Ok(PixelViewMut::new_with_format_cow(
            bytes,
            strip.width,
            strip.height,
            self.stride,
            zenpixels::FormatCow::from_owned(format),
        )
        .unwrap())
    }
}

#[test]
fn registered_alpha_follows_both_nonsquare_transform_orders() {
    for rot_first in [true, false] {
        let mut base = image(GREY, W, H, 8, 0);
        let r = bx(b"irot", &[3]);
        let m = bx(b"imir", &[1]);
        base.properties.push(nclx(1, 13, 1, true));
        base.properties
            .extend(if rot_first { [r, m] } else { [m, r] });
        let data = heif(&[base, alpha(true)], &[(b"auxl", 2, &[1])]);
        let p = decode(&data, OrientationHint::Preserve, GainMapRender::BaseOnly);
        let corrected = decode(&data, OrientationHint::Correct, GainMapRender::BaseOnly);
        let intrinsic = if rot_first {
            Orientation::Transpose
        } else {
            Orientation::Transverse
        };
        assert_eq!(p.info().orientation, intrinsic);
        assert_eq!(corrected.info().orientation, Orientation::Identity);
        assert_eq!(corrected.extras::<Orientation>(), Some(&intrinsic));
        assert_eq!((corrected.width(), corrected.height()), (H, W));
        let mut sink = PaddedSink::default();
        let push = HeicDecoderConfig::new()
            .job()
            .with_orientation(OrientationHint::Correct)
            .push_decoder((&data[..]).into(), &mut sink, &[])
            .unwrap();
        assert_eq!(push.orientation_applied, intrinsic);
        for y in 0..W {
            let active = corrected.pixels().row(y);
            for x in 0..H {
                let (sx, sy) = if rot_first {
                    (y, x)
                } else {
                    (W - 1 - y, H - 1 - x)
                };
                assert_eq!(
                    active[x as usize * 4 + 3],
                    p.pixels().row(sy)[sx as usize * 4 + 3]
                );
            }
            let row = &sink.data[y as usize * sink.stride..(y + 1) as usize * sink.stride];
            assert_eq!(&row[..active.len()], active);
            assert_eq!(&row[active.len()..], &[0xcd; 7]);
        }
    }
}

fn tmap_case(rgb: bool, multi: bool, alternate: bool, declared: bool, reverse: bool) -> Vec<u8> {
    let mut base = if reverse {
        image(YUV12, W, H, 12, 1)
    } else {
        image(GREY, W, H, 8, 0)
    };
    if reverse {
        base.properties.push(nclx(9, 16, 9, false));
        base.properties.extend(hdr_props());
    } else {
        base.properties.push(nclx(1, 13, 1, true));
    }
    let mut gain = if rgb {
        image(RGB_GAIN, 16, 16, 8, 3)
    } else {
        image(GREY_SMALL, 16, 16, 8, 0)
    };
    gain.properties.push(nclx(1, 13, 0, true));
    let mut tmap = Item {
        kind: *b"tmap",
        data: iso(multi, alternate, reverse),
        properties: vec![ispe(W, H)],
    };
    if declared {
        tmap.properties.push(if reverse {
            nclx(1, 13, 1, true)
        } else {
            nclx(9, 16, 9, false)
        });
    }
    heif(&[base, gain, tmap], &[(b"dimg", 3, &[1, 2])])
}

#[test]
fn hdr_base_retains_pq_precision_and_refuses_lower_rendition_before_sink() {
    let data = tmap_case(false, false, false, true, true);
    let mut primary_tmap = data.clone();
    let pitm = primary_tmap.windows(4).position(|w| w == b"pitm").unwrap();
    primary_tmap[pitm + 9] = 3;
    for data in [data, primary_tmap] {
        let base = decode(&data, OrientationHint::Preserve, GainMapRender::BaseOnly);
        let same = decode(
            &data,
            OrientationHint::Preserve,
            GainMapRender::ReconstructHdr {
                target_headroom: None,
            },
        );
        assert!(matches!(
            same.pixel_format().color,
            ColorSpace::Rgb {
                primaries: Primaries::Bt2020,
                transfer: Transfer::Pq,
                ..
            }
        ));
        assert_eq!(same.pixel_format().layout.sample, SampleType::U16);
        assert_eq!(same.pixel_format(), base.pixel_format());
        assert_eq!(words(&same), words(&base));
        assert_eq!(same.pixels().facts(), base.pixels().facts());
        assert_eq!(same.pixels().hdr(), base.pixels().hdr());
        assert_eq!(
            same.pixels()
                .hdr()
                .content_light_level
                .unwrap()
                .max_content_light_level,
            1000
        );
        let mut sink = PaddedSink::default();
        let error = HeicDecoderConfig::new()
            .job()
            .with_gain_map_render(GainMapRender::ReconstructHdr {
                target_headroom: Some(1.0),
            })
            .push_decoder((&data[..]).into(), &mut sink, &[])
            .unwrap_err();
        assert_eq!(
            error.unsupported_operation(),
            Some(&zencodec::UnsupportedOperation::PixelFormat)
        );
        assert!(sink.data.is_empty());
    }
}

#[test]
fn iso_gain_sample_arity_is_independent_of_exact_parameter_arity() {
    for rgb in [false, true] {
        for multi in [false, true] {
            let data = tmap_case(rgb, multi, true, true, false);
            let comp = decode(&data, OrientationHint::Correct, GainMapRender::Components);
            let map = comp.extras::<zencodec::decode::DecodedGainMap>().unwrap();
            let actual = map.pixels.as_view();
            assert_eq!(
                (map.width(), map.height(), map.channels()),
                (16, 16, if rgb { 3 } else { 1 })
            );
            for y in 0..16 {
                let expected = if rgb {
                    [48, 80, 112].repeat(16)
                } else {
                    (0..16)
                        .map(|x| ((y * 7 + x * 11 + 30) % 256) as u8)
                        .collect()
                };
                assert_eq!(actual.row(y), expected);
            }
            let p = &map.metadata.params;
            assert_eq!(p.iso_channel_count(), Some(if multi { 3 } else { 1 }));
            assert_eq!(
                map.metadata.alternate_cicp,
                Some(Cicp::new(9, 16, 9, false))
            );
            let exact = p.exact_metadata().unwrap();
            assert_eq!(exact.channel(0).max_gain_stops.numerator, 2);
            assert_eq!(exact.channel(0).max_gain_stops.denominator.get(), 2);
            assert_eq!(
                exact.channel(2).max_gain_stops.numerator,
                if multi { 6 } else { 2 }
            );
            let original = comp.extras::<heic::HdrGainMap>().unwrap();
            assert_eq!(
                original.iso21496.as_ref().unwrap(),
                &iso(multi, true, false)
            );
            assert_eq!(
                zencodec::gainmap::serialize_iso21496_fmt(
                    p,
                    zencodec::gainmap::Iso21496Format::AvifTmap
                )
                .unwrap(),
                iso(multi, true, false)
            );
            let out = decode(
                &data,
                OrientationHint::Correct,
                GainMapRender::ReconstructHdr {
                    target_headroom: None,
                },
            );
            assert!(matches!(
                out.pixel_format().color,
                ColorSpace::Rgb {
                    primaries: Primaries::Bt2020,
                    transfer: Transfer::Linear,
                    ..
                }
            ));
            assert_eq!(out.pixel_format().color.white(), Some(DiffuseWhite::BT2408));
        }
    }
    let missing = tmap_case(true, true, true, false, false);
    for render in [
        GainMapRender::Components,
        GainMapRender::ReconstructHdr {
            target_headroom: None,
        },
    ] {
        let error = HeicDecoderConfig::new()
            .job()
            .with_gain_map_render(render)
            .decoder((&missing[..]).into(), &[])
            .unwrap()
            .decode()
            .unwrap_err();
        assert_eq!(
            error.unsupported_operation(),
            Some(&zencodec::UnsupportedOperation::PixelFormat)
        );
    }
}

#[test]
fn iso_components_register_to_preserved_base_coordinates() {
    let mut base = image(GREY, W, H, 8, 0);
    base.properties
        .extend([nclx(1, 13, 1, true), bx(b"irot", &[3])]);
    let mut map = image(GREY, W, H, 8, 0);
    map.properties
        .extend([nclx(1, 13, 1, true), bx(b"irot", &[3])]);
    let tmap = Item {
        kind: *b"tmap",
        data: iso(false, false, false),
        properties: vec![ispe(H, W)],
    };
    let data = heif(&[base, map, tmap], &[(b"dimg", 3, &[1, 2])]);
    let p = decode(&data, OrientationHint::Preserve, GainMapRender::Components);
    let c = decode(&data, OrientationHint::Correct, GainMapRender::Components);
    let pm = p
        .extras::<zencodec::decode::DecodedGainMap>()
        .unwrap()
        .pixels
        .as_view();
    let cm = c
        .extras::<zencodec::decode::DecodedGainMap>()
        .unwrap()
        .pixels
        .as_view();
    assert_eq!(
        (pm.width(), pm.height(), cm.width(), cm.height()),
        (W, H, H, W)
    );
    for y in 0..W {
        for x in 0..H {
            assert_eq!(cm.row(y)[x as usize], pm.row(H - 1 - x)[y as usize]);
        }
    }
}

fn profile(gray: bool) -> Vec<u8> {
    use zenpixels_convert::icc_profiles::SynthesizedIcc;
    let profile = if gray {
        SynthesizedIcc::for_gray_cicp(Cicp::DISPLAY_P3)
    } else {
        SynthesizedIcc::for_cicp(Cicp::DISPLAY_P3)
    };
    let SynthesizedIcc::Profile(bytes) = profile else {
        panic!("test profile required");
    };
    bytes.into_owned()
}

#[test]
fn restricted_icc_recognizes_rgb_gray_and_separate_negative_cases() {
    for (gray, tag, valid) in [
        (false, b"rICC", true),
        (true, b"rICC", true),
        (false, b"rICC", false),
        (false, b"ricc", true),
    ] {
        let mut icc = profile(gray);
        if !valid {
            icc[36] = b'X';
        }
        let mut item = image(GREY, W, H, 8, 0);
        item.properties.extend([
            nclx(1, 1, 1, false),
            bx(b"colr", &[tag.as_slice(), &icc].concat()),
        ]);
        let data = heif(&[item], &[]);
        let present = valid && tag == b"rICC";
        let native = DecoderConfig::new();
        assert_eq!(
            native.extract_icc(&data).unwrap().as_deref(),
            present.then_some(icc.as_slice())
        );
        assert!(native.decode(&data, heic::PixelLayout::Rgb8).is_ok());
        let cfg = HeicDecoderConfig::new();
        let info = cfg.clone().job().probe_full(&data).unwrap();
        assert_eq!(
            info.source_color.icc_profile.as_deref(),
            present.then_some(icc.as_slice())
        );
        assert_eq!(
            info.source_color.color_authority,
            if present {
                zencodec::ColorAuthority::Icc
            } else {
                zencodec::ColorAuthority::Cicp
            }
        );
        if present && gray {
            let spec = zencodec::helpers::pixel_format_for_decoded_pixels(
                zenpixels::SampleLayout::GRAY8,
                &info.source_color,
                None,
            )
            .unwrap();
            assert!(
                matches!(spec.color, ColorSpace::Icc { color_channels: 1, profile, .. } if *profile == icc)
            );
        } else {
            let out = decode(&data, OrientationHint::Preserve, GainMapRender::BaseOnly);
            if present {
                assert!(
                    matches!(out.pixel_format().color, ColorSpace::Icc { color_channels: 3, ref profile, .. } if profile.as_ref() == icc.as_slice())
                );
            } else {
                assert!(matches!(
                    out.pixel_format().color,
                    ColorSpace::Rgb {
                        primaries: Primaries::Bt709,
                        ..
                    }
                ));
            }
        }
        #[cfg(feature = "inventory")]
        {
            use zencodec::inventory::{Disposition, MetadataKind, PartKind};
            let inv = cfg.job().inventory(&data).unwrap();
            let at = data
                .windows(8)
                .position(|w| w[..4] == *b"colr" && w[4..] == *tag)
                .unwrap() as u64
                + 8;
            let leaf = inv
                .parts()
                .iter()
                .find(|p| {
                    p.range().start <= at && at < p.range().end && p.kind() == PartKind::Property
                })
                .unwrap();
            assert_eq!(
                leaf.disposition(),
                if present {
                    Disposition::Metadata(MetadataKind::Icc)
                } else if valid {
                    Disposition::Unknown
                } else {
                    Disposition::Malformed
                }
            );
        }
    }
}

#[test]
fn raw_nclx_icc_and_all_hdr_metadata_survive_bake_and_identity_conversion() {
    let icc = profile(false);
    let mut base = image(GREY, W, H, 8, 0);
    base.properties.extend([
        nclx(1, 1, 1, false),
        bx(b"colr", &[b"prof".as_slice(), &icc].concat()),
        bx(b"irot", &[3]),
        bx(b"imir", &[1]),
    ]);
    base.properties.extend(hdr_props());
    let data = heif(&[base], &[]);
    let stored = decode(&data, OrientationHint::Preserve, GainMapRender::BaseOnly);
    let correct = decode(&data, OrientationHint::Correct, GainMapRender::BaseOnly);
    assert_eq!(
        stored.info().source_color.color_authority,
        zencodec::ColorAuthority::Icc
    );
    assert_eq!(
        stored.info().source_color.icc_profile.as_deref(),
        Some(icc.as_slice())
    );
    let origin = stored.pixels().origin().unwrap();
    assert_eq!(origin.cicp, Some(Cicp::new(1, 1, 1, false)));
    assert_eq!(origin.icc.as_deref(), Some(icc.as_slice()));
    let hdr = stored.pixels().hdr();
    assert_eq!(
        hdr.content_light_level,
        Some(zenpixels::ContentLightLevel::new(1000, 400))
    );
    assert_eq!(
        hdr.mastering,
        Some(zenpixels::MasteringDisplay::new(
            [[0.68, 0.32], [0.265, 0.69], [0.15, 0.06]],
            [0.3127, 0.329],
            1000.0,
            0.005
        ))
    );
    let baked =
        zenpixels_convert::apply_orientation(&stored.pixels(), Orientation::Transpose).unwrap();
    // The checked identity copy retains ICC-authoritative metadata. An
    // ImagePlan independently exercises identity on the active PQ route below.
    let identical = baked.as_view().to_tight_buf().unwrap();
    for owner in [&baked, &identical, correct.buffer()] {
        assert_eq!(owner.origin(), origin);
        assert_eq!(owner.format(), stored.pixels().format());
        assert_eq!(owner.hdr(), hdr);
        assert_eq!(
            owner.as_view().as_contiguous_bytes(),
            correct.pixels().as_contiguous_bytes()
        );
    }
    for y in 0..W {
        for x in 0..H {
            assert_eq!(
                correct.pixels().row(y)[x as usize * 3..x as usize * 3 + 3],
                stored.pixels().row(x)[y as usize * 3..y as usize * 3 + 3]
            );
        }
    }
    let pq = tmap_case(false, false, false, true, true);
    let pq = decode(&pq, OrientationHint::Preserve, GainMapRender::BaseOnly);
    let same = zenpixels_convert::convert_to_buf_with_peak(
        &pq.pixels(),
        pq.pixel_format(),
        &Default::default(),
        zenpixels_convert::PeakSource::Declared,
    )
    .unwrap();
    assert_eq!(
        same.as_view().as_contiguous_bytes(),
        pq.pixels().as_contiguous_bytes()
    );
    assert_eq!(same.hdr(), hdr);
    assert_eq!(same.origin().cicp, Some(Cicp::new(9, 16, 9, false)));
}

#[test]
fn apple_one_times_gain_is_exact_linear_base_and_separate_from_nominal_capacity() {
    let apple = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/apple-hdr/hdr-sample.heic"
    ))
    .unwrap();
    let exif = DecoderConfig::new().extract_exif(&apple).unwrap().unwrap();
    let exif_item = Item {
        kind: *b"Exif",
        data: [&[0, 0, 0, 0], exif.as_ref()].concat(),
        properties: vec![],
    };
    let mut base = image(GREY, W, H, 8, 0);
    base.properties.push(nclx(1, 13, 1, true));
    let mut gain = image(RGB_GAIN, 16, 16, 8, 3);
    gain.properties
        .push(auxc(b"urn:com:apple:photo:2020:aux:hdrgainmap"));
    let data = heif(
        &[base, alpha(false), gain, exif_item],
        &[(b"auxl", 2, &[1]), (b"auxl", 3, &[1]), (b"cdsc", 4, &[1])],
    );
    let base = decode(&data, OrientationHint::Correct, GainMapRender::BaseOnly);
    let unity = decode(
        &data,
        OrientationHint::Correct,
        GainMapRender::ReconstructHdr {
            target_headroom: Some(1.0),
        },
    );
    let full = decode(
        &data,
        OrientationHint::Correct,
        GainMapRender::ReconstructHdr {
            target_headroom: None,
        },
    );
    let view = unity.pixels();
    assert!(matches!(
        view.format().color,
        ColorSpace::Rgb {
            primaries: Primaries::Bt709,
            transfer: Transfer::Linear,
            range: SignalRange::Full,
            alpha: zenpixels::AlphaMode::Straight,
            ..
        }
    ));
    assert_eq!(view.format().color.white(), Some(DiffuseWhite::BT2408));
    let mut peak = 0.0f64;
    let mut sum = 0.0f64;
    let stops = full
        .info()
        .gain_map
        .info()
        .unwrap()
        .params
        .alternate_hdr_headroom;
    let factor = (80.0 / 255.0 * stops).exp2();
    for y in 0..H {
        let row = view.row_as::<f32>(y).unwrap().as_chunks::<4>().0;
        let bright = full.pixels().row_as::<f32>(y).unwrap().as_chunks::<4>().0;
        for (x, (&out, &hdr)) in row.iter().zip(bright).enumerate() {
            let code = ((y as usize * W as usize + x + 30) % 256) as u8;
            assert_eq!(base.pixels().row(y)[x * 4..x * 4 + 4], [code; 4]);
            let encoded = f32::from(code) / 255.0;
            let linear = linear_srgb::default::srgb_to_linear(encoded);
            assert_eq!(
                out.map(f32::to_bits),
                [linear, linear, linear, encoded].map(f32::to_bits)
            );
            let mathematically_linear = if encoded <= 0.04045 {
                (f64::from(encoded)) / 12.92
            } else {
                ((f64::from(encoded) + 0.055) / 1.055).powf(2.4)
            };
            assert!((f64::from(linear) - mathematically_linear).abs() < 5e-6);
            let expected = (f64::from(linear) + 1.0 / 64.0) * factor - 1.0 / 64.0;
            assert!((f64::from(hdr[0]) - expected).abs() < 3e-6);
            assert_eq!(hdr[3].to_bits(), encoded.to_bits());
            let nits = f64::from(hdr[0]) * 203.0;
            peak = peak.max(nits);
            sum += nits;
        }
    }
    let measured = full.pixels().hdr().content_light_level.unwrap();
    assert_eq!(
        measured,
        zenpixels::ContentLightLevel::new(
            peak.ceil() as u16,
            (sum / f64::from(W * H)).ceil() as u16
        )
    );
    assert_eq!(full.info().source_color.content_light_level, Some(measured));
    let nominal = full.pixels().hdr().mastering.unwrap();
    assert_eq!(nominal, unity.pixels().hdr().mastering.unwrap());
    assert_eq!(nominal.max_luminance, 203.0 * stops.exp2() as f32);
    assert!(f32::from(measured.max_content_light_level) < nominal.max_luminance);
}
