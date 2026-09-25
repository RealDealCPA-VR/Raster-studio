//! W16-B: synthetic files in every colour mode, built here, read back.

use super::*;
use crate::bytes::Sink;
use crate::error::PsdError;
use crate::model::Rect;
use crate::{read, write};

/// A science whose results are easy to predict by hand: ideal inks, and Lab
/// passed through as `[L * 2.55, a + 128, b + 128]`.
fn ideal_cmyk(ink: [f32; 4]) -> [u8; 3] {
    let k = 1.0 - ink[3];
    [0, 1, 2].map(|i| ((1.0 - ink[i]) * k * 255.0).round() as u8)
}

fn lab_passthrough(lab: [f32; 3]) -> [u8; 3] {
    [
        (lab[0] * 2.55).round() as u8,
        (lab[1] + 128.0).round() as u8,
        (lab[2] + 128.0).round() as u8,
    ]
}

fn science() -> WorkingScience<'static> {
    WorkingScience {
        cmyk_to_rgb: &ideal_cmyk,
        lab_to_rgb: &lab_passthrough,
        duotone: None,
    }
}

fn header(mode: ColorMode, channels: u16, depth: Depth, w: u32, h: u32) -> PsdHeader {
    PsdHeader {
        channels,
        width: w,
        height: h,
        depth,
        color_mode: mode,
    }
}

/// A 2x1 CMYK file with one layer and alpha: the first pixel 100 % cyan
/// (stored `0`, inverted), the second 50 % black.
fn cmyk_file() -> PsdFile {
    let mut file = PsdFile::new(header(ColorMode::Cmyk, 5, Depth::Eight, 2, 1));
    let mut layer = PsdLayer::raster("Ink", Rect::sized(2, 1));
    layer.channels = vec![
        Channel::new(CHANNEL_ALPHA, vec![255, 128]),
        Channel::new(0, vec![0, 255]),
        Channel::new(1, vec![255, 255]),
        Channel::new(2, vec![255, 255]),
        Channel::new(3, vec![255, 128]),
    ];
    file.layers.push(layer);
    file.merged = Some(MergedImage {
        channels: vec![
            vec![0, 255],
            vec![255, 255],
            vec![255, 255],
            vec![255, 128],
            vec![255, 128],
        ],
    });
    file
}

#[test]
fn a_layered_cmyk_file_opens_as_cmyk_with_its_ink_numbers() {
    let back = read(&write(&cmyk_file()).unwrap()).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Cmyk);
    assert_eq!(back.header.channels, 5);
    let layer = &back.layers[0];
    assert_eq!(
        layer.channel(0).unwrap().data,
        vec![0, 255],
        "cyan, inverted"
    );
    assert_eq!(layer.channel(3).unwrap().data, vec![255, 128], "black");
    assert_eq!(back.merged.as_ref().unwrap().channels.len(), 5);
}

#[test]
fn cmyk_samples_are_decoded_as_inverted_ink_not_read_as_rgb() {
    let mut file = read(&write(&cmyk_file()).unwrap()).unwrap();
    let out = to_working_rgb(&mut file, &science()).unwrap();
    assert_eq!(out.source, ColorMode::Cmyk);
    assert_eq!(file.header.color_mode, ColorMode::Rgb);
    assert_eq!(file.header.channels, 4, "RGB + alpha");
    let layer = &file.layers[0];
    // 100 % cyan through ideal inks is (0, 255, 255); 50 % black is mid grey.
    assert_eq!(layer.channel(0).unwrap().data, vec![0, 128]);
    assert_eq!(layer.channel(1).unwrap().data, vec![255, 128]);
    assert_eq!(layer.channel(2).unwrap().data, vec![255, 128]);
    assert_eq!(layer.channel(CHANNEL_ALPHA).unwrap().data, vec![255, 128]);
    assert!(layer.channel(3).is_none(), "the black plane is consumed");
    let merged = file.merged.as_ref().unwrap();
    assert_eq!(merged.channels[0], vec![0, 128]);
    assert_eq!(merged.channels[3], vec![255, 128], "alpha rides along");
    // The working copy is an ordinary RGB file.
    assert_eq!(
        merged.to_rgba8(2, 1).unwrap(),
        vec![0, 255, 255, 255, 128, 128, 128, 128]
    );
}

#[test]
fn lab_at_eight_and_sixteen_bits_is_decoded_from_its_offset_encoding() {
    // L 100 / a 0 / b 0 is white; L 50 / a +20 / b -30.
    let eight = [vec![255u8, 128], vec![128, 148], vec![128, 98]];
    for depth in [Depth::Eight, Depth::Sixteen] {
        let mut file = PsdFile::new(header(ColorMode::Lab, 3, depth, 2, 1));
        let planes: Vec<Vec<u8>> = eight
            .iter()
            .map(|p| match depth {
                Depth::Sixteen => widen16(p),
                _ => p.clone(),
            })
            .collect();
        file.merged = Some(MergedImage {
            channels: planes.clone(),
        });
        let mut layer = PsdLayer::raster("Lab", Rect::sized(2, 1));
        for (id, p) in planes.iter().enumerate() {
            layer.channels.push(Channel::new(id as i16, p.clone()));
        }
        file.layers.push(layer);
        let mut back = read(&write(&file).unwrap()).unwrap();
        assert_eq!(back.header.color_mode, ColorMode::Lab);
        assert_eq!(back.header.depth, depth);
        let seen = std::cell::RefCell::new(Vec::new());
        let lab = |v: [f32; 3]| {
            seen.borrow_mut().push(v);
            lab_passthrough(v)
        };
        let science = WorkingScience {
            cmyk_to_rgb: &ideal_cmyk,
            lab_to_rgb: &lab,
            duotone: None,
        };
        let out = to_working_rgb(&mut back, &science).unwrap();
        assert_eq!(out.source_depth, depth);
        assert_eq!(back.header.depth, Depth::Eight);
        let got = seen.borrow();
        assert!((got[0][0] - 100.0).abs() < 1e-3 && got[0][1] == 0.0 && got[0][2] == 0.0);
        assert!((got[1][0] - 128.0 * 100.0 / 255.0).abs() < 1e-3);
        assert_eq!((got[1][1], got[1][2]), (20.0, -30.0), "{depth:?}");
        assert_eq!(back.layers[0].channel(0).unwrap().data, vec![255, 128]);
    }
}

fn indexed_file(transparent: Option<u8>) -> PsdFile {
    let table = IndexedTable {
        colors: vec![[10, 20, 30], [200, 100, 50], [0, 0, 0]],
        transparent,
    };
    let mut file = PsdFile::new(header(ColorMode::Indexed, 1, Depth::Eight, 3, 1));
    file.color_mode_data = table.mode_data();
    file.resources = table.resources();
    file.merged = Some(MergedImage {
        channels: vec![vec![1, 0, 2]],
    });
    file
}

#[test]
fn an_indexed_file_opens_through_its_palette_and_transparent_index() {
    let mut file = read(&write(&indexed_file(Some(2))).unwrap()).unwrap();
    assert_eq!(file.header.color_mode, ColorMode::Indexed);
    let table = IndexedTable::read(&file).unwrap();
    assert_eq!(table.colors.len(), 3, "resource 1046");
    assert_eq!(table.transparent, Some(2), "resource 1047");
    let out = to_working_rgb(&mut file, &science()).unwrap();
    assert_eq!(out.indexed.unwrap().colors[1], [200, 100, 50]);
    assert_eq!(file.header.channels, 4);
    assert!(file.color_mode_data.is_empty());
    assert_eq!(
        file.merged.as_ref().unwrap().to_rgba8(3, 1).unwrap(),
        vec![200, 100, 50, 255, 10, 20, 30, 255, 0, 0, 0, 0]
    );
    // No transparent index: no alpha channel is invented.
    let mut opaque = read(&write(&indexed_file(None)).unwrap()).unwrap();
    to_working_rgb(&mut opaque, &science()).unwrap();
    assert_eq!(opaque.header.channels, 3);
}

#[test]
fn a_palette_that_is_not_768_bytes_is_a_malformed_file() {
    let bytes = write(&indexed_file(None)).unwrap();
    // Shorten the declared mode data by one byte and drop that byte.
    let mut bad = bytes[..26].to_vec();
    bad.extend_from_slice(&767u32.to_be_bytes());
    bad.extend_from_slice(&bytes[30..30 + 767]);
    bad.extend_from_slice(&bytes[30 + 768..]);
    let err = read(&bad).unwrap_err();
    assert!(err.is_file_fault(), "{err}");
    assert!(
        matches!(err, PsdError::SectionLengthMismatch { declared: 767, .. }),
        "{err}"
    );
    // And a writer refuses to make one.
    let mut file = indexed_file(None);
    file.color_mode_data.pop();
    assert!(matches!(write(&file), Err(PsdError::InvalidDocument(_))));
}

/// A Bitmap file built by hand: 10 x 2, 1 bit per pixel, rows of two bytes.
fn bitmap_bytes(rle: bool) -> Vec<u8> {
    let (w, h) = (10u32, 2u32);
    // Row 0: black, white, black ... ; row 1: all black but the last pixel.
    let grey: Vec<u8> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            match y {
                0 if x % 2 == 0 => 0,
                0 => 255,
                _ if x == w - 1 => 255,
                _ => 0,
            }
        })
        .collect();
    let packed = pack_bitmap(&grey, w, h);
    let mut s = Sink::new();
    header(ColorMode::Bitmap, 1, Depth::Eight, w, h).write(&mut s);
    s.u32(0); // colour mode data
    s.u32(0); // resources
    s.u32(0); // layer and mask
    if rle {
        s.u16(1);
        let rows: Vec<Vec<u8>> = packed.chunks(2).map(crate::packbits::encode).collect();
        for r in &rows {
            s.u16(r.len() as u16);
        }
        for r in &rows {
            s.bytes(r);
        }
    } else {
        s.u16(0);
        s.bytes(&packed);
    }
    s.into_inner()
}

#[test]
fn a_one_bit_bitmap_file_opens_as_black_and_white_grey() {
    for rle in [false, true] {
        let mut file = read(&bitmap_bytes(rle)).unwrap();
        assert_eq!(file.header.color_mode, ColorMode::Bitmap);
        let plane = &file.merged.as_ref().unwrap().channels[0];
        assert_eq!(plane.len(), 20);
        assert_eq!(&plane[..4], &[0, 255, 0, 255], "rle={rle}");
        assert_eq!(&plane[10..], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 255]);
        to_working_rgb(&mut file, &science()).unwrap();
        assert_eq!(file.header.color_mode, ColorMode::Grayscale);
        assert_eq!(file.header.channels, 1);
    }
    // The writer does not pack bits.
    let file = read(&bitmap_bytes(false)).unwrap();
    assert!(matches!(write(&file), Err(PsdError::InvalidDocument(_))));
}

#[test]
fn a_multichannel_file_shows_three_inks_as_cmy_and_counts_the_rest() {
    let mut file = PsdFile::new(header(ColorMode::Multichannel, 4, Depth::Eight, 1, 1));
    file.merged = Some(MergedImage {
        channels: vec![vec![0], vec![255], vec![255], vec![0]],
    });
    assert!(
        !file.header.has_alpha(),
        "every Multichannel channel is ink"
    );
    let mut back = read(&write(&file).unwrap()).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Multichannel);
    let out = to_working_rgb(&mut back, &science()).unwrap();
    assert_eq!(out.dropped_channels, 1);
    assert_eq!(back.header.channels, 3);
    assert_eq!(
        back.merged.as_ref().unwrap().channels,
        vec![vec![0], vec![255], vec![255]]
    );

    let mut one = PsdFile::new(header(ColorMode::Multichannel, 1, Depth::Eight, 1, 1));
    one.merged = Some(MergedImage {
        channels: vec![vec![77]],
    });
    let mut back = read(&write(&one).unwrap()).unwrap();
    to_working_rgb(&mut back, &science()).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Grayscale);
    assert_eq!(back.merged.as_ref().unwrap().channels, vec![vec![77]]);
}

fn duotone_record() -> DuotoneRecord {
    DuotoneRecord {
        inks: vec![
            DuotoneInkRecord {
                color: InkColor::Cmyk([65535, 65535, 65535, 0]),
                name: "Black".into(),
                curve: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
            },
            DuotoneInkRecord {
                color: InkColor::Rgb([65535, 32768, 0]),
                name: "Orange".into(),
                curve: vec![[0.0, 0.0], [1.0, 0.8]],
            },
        ],
    }
}

#[test]
fn a_duotone_file_opens_as_its_grey_base_with_the_ink_record_kept() {
    let mut file = PsdFile::new(header(ColorMode::Duotone, 1, Depth::Eight, 2, 1));
    file.color_mode_data = duotone_record().encode();
    file.merged = Some(MergedImage {
        channels: vec![vec![0, 200]],
    });
    let back = read(&write(&file).unwrap()).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Duotone);
    assert_eq!(
        DuotoneRecord::parse(&back.color_mode_data).unwrap(),
        duotone_record()
    );

    // No inks applied: the grey base.
    let mut grey = back.clone();
    to_working_rgb(&mut grey, &science()).unwrap();
    assert_eq!(grey.header.color_mode, ColorMode::Grayscale);
    assert_eq!(grey.merged.as_ref().unwrap().channels, vec![vec![0, 200]]);

    // Inks applied through the caller's table.
    let mut lut = [[0u8; 3]; 256];
    for (g, e) in lut.iter_mut().enumerate() {
        *e = [g as u8, 7, 9];
    }
    let mut inked = back;
    let science = WorkingScience {
        duotone: Some(&lut),
        ..science()
    };
    to_working_rgb(&mut inked, &science).unwrap();
    assert_eq!(inked.header.color_mode, ColorMode::Rgb);
    assert_eq!(
        inked.merged.as_ref().unwrap().channels,
        vec![vec![0, 200], vec![7, 7], vec![9, 9]]
    );
}

#[test]
fn a_malformed_duotone_record_is_reported_not_guessed() {
    let good = duotone_record().encode();
    assert!(DuotoneRecord::parse(&good[..100])
        .unwrap_err()
        .contains("shorter"));
    let mut version = good.clone();
    version[1] = 9;
    assert!(DuotoneRecord::parse(&version)
        .unwrap_err()
        .contains("version"));
    let mut count = good.clone();
    count[3] = 7;
    assert!(DuotoneRecord::parse(&count).unwrap_err().contains("7 inks"));
    let mut curve = good;
    let at = 4 + 40 + 256; // first curve point of ink 1
    curve[at..at + 2].copy_from_slice(&5000i16.to_be_bytes());
    assert!(DuotoneRecord::parse(&curve)
        .unwrap_err()
        .contains("outside"));
    // A Duotone file with no record at all still opens, as its grey base.
    let mut file = PsdFile::new(header(ColorMode::Duotone, 1, Depth::Eight, 1, 1));
    file.merged = Some(MergedImage {
        channels: vec![vec![40]],
    });
    let mut back = read(&write(&file).unwrap()).unwrap();
    assert!(DuotoneRecord::parse(&back.color_mode_data).is_err());
    to_working_rgb(&mut back, &science()).unwrap();
    assert_eq!(back.merged.unwrap().channels, vec![vec![40]]);
}

#[test]
fn a_print_mode_file_without_a_composite_is_written_on_blank_paper() {
    let mut lab = PsdFile::new(header(ColorMode::Lab, 4, Depth::Sixteen, 1, 1));
    lab.layers.push(PsdLayer::raster("empty", Rect::default()));
    let back = read(&write(&lab).unwrap()).unwrap();
    assert_eq!(
        back.merged.unwrap().channels,
        vec![vec![0xff, 0xff], vec![0x80, 0], vec![0x80, 0], vec![0, 0]],
        "L 100, a 0, b 0, transparent"
    );
    let cmyk = PsdFile::new(header(ColorMode::Cmyk, 4, Depth::Eight, 1, 1));
    let back = read(&write(&cmyk).unwrap()).unwrap();
    assert_eq!(back.merged.unwrap().channels, vec![vec![0xff]; 4], "no ink");
}

#[test]
fn every_truncation_of_a_print_mode_file_is_an_error_never_a_panic() {
    let files = [
        write(&cmyk_file()).unwrap(),
        write(&indexed_file(Some(1))).unwrap(),
        bitmap_bytes(true),
        bitmap_bytes(false),
    ];
    for bytes in files {
        for cut in 0..bytes.len() {
            if let Ok(mut file) = read(&bytes[..cut]) {
                // Whatever parsed must convert without panicking.
                let _ = to_working_rgb(&mut file, &science());
            }
        }
    }
    // A Bitmap plane of the wrong size is refused by the unpacker.
    let mut budget = Budget::new(1 << 20);
    assert!(matches!(
        unpack_bitmap(&[0u8; 3], 10, 2, &mut budget),
        Err(PsdError::ChannelSizeMismatch { .. })
    ));
}

#[test]
fn rgb_separates_into_cmyk_lab_grey_and_indexed_and_writes_those_modes() {
    let rgb = [[0u8, 255, 255, 255], [128, 128, 128, 128]].concat();
    let base = || {
        let mut file = crate::from_rgba8(2, 1, &rgb).unwrap();
        let mut layer = PsdLayer::raster("L", Rect::sized(2, 1));
        layer.set_rgba8(&rgb).unwrap();
        file.layers.push(layer);
        file
    };
    // CMYK: ideal separation, written inverted.
    let mut cmyk = base();
    let mut sep = |c: [u8; 3]| {
        let k = 1.0 - f32::from(c.iter().copied().max().unwrap()) / 255.0;
        let ink = |v: u8| {
            if k >= 1.0 {
                0.0
            } else {
                (1.0 - f32::from(v) / 255.0 - k) / (1.0 - k)
            }
        };
        [ink(c[0]), ink(c[1]), ink(c[2]), k]
    };
    from_working_rgb(&mut cmyk, Separation::Cmyk(&mut sep)).unwrap();
    assert_eq!(cmyk.header.color_mode, ColorMode::Cmyk);
    assert_eq!(cmyk.header.channels, 5);
    let mut back = read(&write(&cmyk).unwrap()).unwrap();
    assert_eq!(back.layers[0].channel(0).unwrap().data, vec![0, 255]);
    assert_eq!(back.layers[0].channel(3).unwrap().data, vec![255, 128]);
    to_working_rgb(&mut back, &science()).unwrap();
    assert_eq!(
        back.layers[0].rgba8().unwrap(),
        rgb,
        "a round trip keeps the numbers"
    );

    // Lab.
    let mut lab = base();
    let to_lab = |c: [u8; 3]| {
        [
            f32::from(c[0]) / 2.55,
            f32::from(c[1]) - 128.0,
            f32::from(c[2]) - 128.0,
        ]
    };
    from_working_rgb(&mut lab, Separation::Lab(&to_lab)).unwrap();
    let mut back = read(&write(&lab).unwrap()).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Lab);
    to_working_rgb(&mut back, &science()).unwrap();
    assert_eq!(back.layers[0].rgba8().unwrap(), rgb);

    // Greyscale: Rec. 601 luma.
    let mut grey = base();
    from_working_rgb(&mut grey, Separation::Grayscale).unwrap();
    let back = read(&write(&grey).unwrap()).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Grayscale);
    assert_eq!(back.header.channels, 2);
    assert_eq!(
        back.layers[0].channel(0).unwrap().data,
        vec![luma601([0, 255, 255]), 128]
    );

    // Indexed is flat.
    let table = IndexedTable {
        colors: vec![[0, 255, 255], [128, 128, 128]],
        transparent: None,
    };
    let mut indexed = base();
    let mut index_of = |c: [u8; 3]| u8::from(c[0] == 128);
    assert!(matches!(
        from_working_rgb(&mut indexed, Separation::Indexed(&table, &mut index_of)),
        Err(PsdError::InvalidDocument(_))
    ));
    indexed.layers.clear();
    from_working_rgb(&mut indexed, Separation::Indexed(&table, &mut index_of)).unwrap();
    let mut back = read(&write(&indexed).unwrap()).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Indexed);
    assert_eq!(back.merged.as_ref().unwrap().channels[0], vec![0, 1]);
    to_working_rgb(&mut back, &science()).unwrap();
    assert_eq!(
        back.merged.as_ref().unwrap().channels[..3],
        [vec![0, 128], vec![255, 128], vec![255, 128]]
    );
}

#[test]
fn a_sixteen_bit_rgb_file_separates_at_eight_bits_and_widens_back() {
    let mut file = PsdFile::new(header(ColorMode::Rgb, 3, Depth::Sixteen, 1, 1));
    file.merged = Some(MergedImage {
        channels: vec![widen16(&[255]), widen16(&[0]), widen16(&[0])],
    });
    let mut sep = |_: [u8; 3]| [0.0, 1.0, 1.0, 0.0];
    from_working_rgb(&mut file, Separation::Cmyk(&mut sep)).unwrap();
    let merged = file.merged.as_ref().unwrap();
    assert_eq!(merged.channels[0], vec![0xff, 0xff], "no cyan");
    assert_eq!(merged.channels[1], vec![0, 0], "full magenta");
    let back = read(&write(&file).unwrap()).unwrap();
    assert_eq!(back.header.depth, Depth::Sixteen);
    assert_eq!(back.header.color_mode, ColorMode::Cmyk);
}
