//! W18-H: Bitmap and Duotone files written in their own mode, read back.

use super::*;
use crate::model::Rect;
use crate::{read, write, PsdHeader};

/// A 10x2 RGBA file with no layers: black, white, dark grey, light grey and
/// a transparent black pixel across its rows.
fn rgb_flat() -> (PsdFile, Vec<u8>) {
    let (w, h) = (10u32, 2u32);
    let mut rgba = Vec::new();
    let mut expected = Vec::new();
    for i in 0..w * h {
        let (px, bit) = match i % 5 {
            0 => ([0, 0, 0, 255], 0),
            1 => ([255, 255, 255, 255], 255),
            2 => ([90, 40, 60, 255], 0),
            3 => ([200, 190, 180, 255], 255),
            _ => ([0, 0, 0, 0], 255),
        };
        rgba.extend_from_slice(&px);
        expected.push(bit);
    }
    let mut file = PsdFile::new(PsdHeader::rgba8(w, h));
    file.merged = Some(MergedImage::from_rgba8(w, h, &rgba).unwrap());
    (file, expected)
}

#[test]
fn a_flat_rgb_file_is_written_as_a_one_bit_bitmap_file() {
    let (mut file, expected) = rgb_flat();
    from_working_rgb(&mut file, Separation::Bitmap).unwrap();
    let bytes = write(&file).unwrap();
    // On disk: Bitmap mode (0) at 1 bit, one channel.
    assert_eq!(&bytes[12..14], &1u16.to_be_bytes(), "one channel");
    assert_eq!(&bytes[22..24], &1u16.to_be_bytes(), "1 bit per sample");
    assert_eq!(&bytes[24..26], &0u16.to_be_bytes(), "Bitmap mode");
    let back = read(&bytes).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Bitmap);
    assert!(back.layers.is_empty());
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.merged.unwrap().channels, vec![expected]);
}

#[test]
fn a_bitmap_file_with_layers_or_the_wrong_shape_is_refused_not_written() {
    let (mut file, _) = rgb_flat();
    file.layers
        .push(PsdLayer::raster("Layer", Rect::sized(1, 1)));
    let err = from_working_rgb(&mut file.clone(), Separation::Bitmap).unwrap_err();
    assert!(err.to_string().contains("flat"), "{err}");
    // A Bitmap header with layers, two channels or a short plane.
    let mut bitmap = PsdFile::new(PsdHeader {
        channels: 1,
        width: 3,
        height: 1,
        depth: Depth::Eight,
        color_mode: ColorMode::Bitmap,
    });
    bitmap.merged = Some(MergedImage {
        channels: vec![vec![0, 255, 0]],
    });
    assert!(write(&bitmap).is_ok());
    let mut layered = bitmap.clone();
    layered
        .layers
        .push(PsdLayer::raster("Layer", Rect::sized(1, 1)));
    assert!(write(&layered).is_err());
    let mut short = bitmap.clone();
    short.merged = Some(MergedImage {
        channels: vec![vec![0, 255]],
    });
    assert!(write(&short).is_err());
    let mut two = bitmap;
    two.header.channels = 2;
    two.merged = Some(MergedImage {
        channels: vec![vec![0, 255, 0], vec![255; 3]],
    });
    assert!(write(&two).is_err());
}

fn record() -> DuotoneRecord {
    DuotoneRecord {
        inks: vec![
            DuotoneInkRecord {
                color: InkColor::Rgb([0, 0, 0]),
                name: "Black".into(),
                curve: vec![[0.0, 0.0], [1.0, 1.0]],
            },
            DuotoneInkRecord {
                color: InkColor::Rgb([65535, 32896, 0]),
                name: "Orange".into(),
                curve: vec![[0.0, 0.0], [0.5, 0.25], [1.0, 0.5]],
            },
        ],
    }
}

#[test]
fn an_rgb_file_is_written_as_duotone_with_its_grey_base_and_ink_record() {
    let (w, h) = (3u32, 1u32);
    let mut file = PsdFile::new(PsdHeader::rgba8(w, h));
    let mut layer = PsdLayer::raster("Photo", Rect::sized(w, h));
    layer.channels = vec![
        Channel::new(CHANNEL_ALPHA, vec![255, 255, 128]),
        Channel::new(0, vec![10, 20, 30]),
        Channel::new(1, vec![11, 21, 31]),
        Channel::new(2, vec![12, 22, 32]),
    ];
    file.layers.push(layer);
    file.merged = Some(MergedImage {
        channels: vec![
            vec![10, 20, 30],
            vec![11, 21, 31],
            vec![12, 22, 32],
            vec![255; 3],
        ],
    });
    // The grey each colour is printed from: here, its red minus nine.
    let mut grey_of = |rgb: [u8; 3]| rgb[0] - 9;
    let record = record();
    from_working_rgb(
        &mut file,
        Separation::Duotone {
            grey_of: &mut grey_of,
            record: &record,
        },
    )
    .unwrap();
    let bytes = write(&file).unwrap();
    assert_eq!(&bytes[24..26], &8u16.to_be_bytes(), "Duotone mode");
    let back = read(&bytes).unwrap();
    assert_eq!(back.header.color_mode, ColorMode::Duotone);
    assert_eq!(back.header.channels, 2, "grey base + alpha");
    assert_eq!(
        DuotoneRecord::parse(&back.color_mode_data).unwrap(),
        record,
        "the ink record comes back"
    );
    let layer = &back.layers[0];
    assert_eq!(layer.channel(0).unwrap().data, vec![1, 11, 21]);
    assert_eq!(
        layer.channel(CHANNEL_ALPHA).unwrap().data,
        vec![255, 255, 128]
    );
    assert!(layer.channel(1).is_none(), "one colour channel");
    let merged = back.merged.unwrap();
    assert_eq!(merged.channels[0], vec![1, 11, 21]);
}
