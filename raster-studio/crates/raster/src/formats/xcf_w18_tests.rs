//! W18-H: indexed XCF images and every precision above 8 bits.

use super::tests::{write_xcf_with, TestLayer};
use super::*;
use crate::codec::SurfacePixels;

fn px(s: &DecodedSurface, x: u32) -> [u8; 4] {
    let SurfacePixels::Rgba8(v) = &s.pixels else {
        panic!("8-bit")
    };
    let i = (x * 4) as usize;
    [v[i], v[i + 1], v[i + 2], v[i + 3]]
}

/// One `w` x 1 layer of type `kind` holding `pixels` (big-endian samples).
fn layer(kind: u32, w: u32, pixels: Vec<u8>) -> TestLayer {
    let mut l = TestLayer::rgba("deep", 0, 0, w, 1, [0; 4]);
    l.kind = kind;
    l.pixels = pixels;
    l
}

#[test]
fn an_indexed_image_opens_through_its_colour_map() {
    let map = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    for compression in [0u8, 1, 2] {
        // Indexed + alpha: red, green, half-transparent blue, and an index
        // past the map (black, as GIMP shows it).
        let l = layer(5, 4, vec![0, 255, 1, 255, 2, 128, 7, 255]);
        let file = write_xcf_with(11, 4, 1, 2, 150, 1, compression, &[l], &map);
        let doc = read(&file, ImportLimits::default()).unwrap();
        assert!(doc.indexed && !doc.grey);
        assert_eq!(doc.colormap, map);
        let s = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(px(&s, 0), [255, 0, 0, 255], "compression {compression}");
        assert_eq!(px(&s, 1), [0, 255, 0, 255]);
        assert_eq!(px(&s, 2), [0, 0, 255, 128]);
        assert_eq!(px(&s, 3), [0, 0, 0, 255]);
        // Indexed without alpha, from an old (version 0) file.
        let l = layer(4, 2, vec![2, 1]);
        let file = write_xcf_with(0, 2, 1, 2, 0, 1, compression, &[l], &map);
        let s = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(px(&s, 0), [0, 0, 255, 255]);
        assert_eq!(px(&s, 1), [0, 255, 0, 255]);
    }
}

fn be16(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

#[test]
fn sixteen_and_thirty_two_bit_integer_images_are_read_at_eight_bits() {
    for compression in [0u8, 1, 2] {
        // 16-bit non-linear (250), RGBA; the low byte counts (0x0100 is 1).
        let l = layer(1, 1, be16(&[0x80ff, 0x0100, 0x4000, 0xffff]));
        let file = write_xcf_with(11, 1, 1, 0, 250, 2, compression, &[l], &[]);
        let doc = read(&file, ImportLimits::default()).unwrap();
        assert_eq!(doc.sample, XcfSample::U16);
        assert!(
            doc.notes.iter().any(|n| n.contains("16-bit integer")),
            "{:?}",
            doc.notes
        );
        let s = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(px(&s, 0), [128, 1, 64, 255], "compression {compression}");
        // 32-bit non-linear (350), grey + alpha.
        let grey: Vec<u8> = [0x8080_8080u32, 0xffff_ffff]
            .iter()
            .flat_map(|x| x.to_be_bytes())
            .collect();
        let file = write_xcf_with(11, 1, 1, 1, 350, 4, compression, &[layer(3, 1, grey)], &[]);
        let s = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(px(&s, 0), [128, 128, 128, 255]);
    }
    // Version 4's code 1 is 16-bit gamma.
    let l = layer(0, 1, be16(&[0, 0x8080, 0xffff]));
    let file = write_xcf_with(4, 1, 1, 0, 1, 2, 1, &[l], &[]);
    let s = decode(&file, ImportLimits::default()).unwrap();
    assert_eq!(px(&s, 0), [0, 128, 255, 255]);
}

#[test]
fn float_images_are_clipped_and_linear_light_is_converted_to_srgb() {
    // 32-bit float linear (600): white, linear 0.2158 (sRGB 128), 2.0
    // (past white, clipped) and alpha 1.0.
    let floats: Vec<u8> = [1.0f32, 0.2158, 2.0, 1.0]
        .iter()
        .flat_map(|x| x.to_be_bytes())
        .collect();
    let file = write_xcf_with(11, 1, 1, 0, 600, 4, 2, &[layer(1, 1, floats)], &[]);
    let doc = read(&file, ImportLimits::default()).unwrap();
    assert!(doc.linear);
    assert!(
        doc.notes.iter().any(|n| n.contains("clipped")),
        "{:?}",
        doc.notes
    );
    let s = decode(&file, ImportLimits::default()).unwrap();
    assert_eq!(px(&s, 0), [255, 128, 255, 255]);
    // 16-bit half non-linear (550): 1.0 and 0.5, and a NaN read as 0.
    let l = layer(0, 1, be16(&[0x3c00, 0x3800, 0x7e00]));
    let file = write_xcf_with(11, 1, 1, 0, 550, 2, 0, &[l], &[]);
    let s = decode(&file, ImportLimits::default()).unwrap();
    assert_eq!(px(&s, 0), [255, 128, 0, 255]);
    // 64-bit double perceptual (775).
    let doubles: Vec<u8> = [0.0f64, 0.5, 1.0]
        .iter()
        .flat_map(|x| x.to_be_bytes())
        .collect();
    let file = write_xcf_with(12, 1, 1, 0, 775, 8, 1, &[layer(0, 1, doubles)], &[]);
    let s = decode(&file, ImportLimits::default()).unwrap();
    assert_eq!(px(&s, 0), [0, 128, 255, 255]);
    // A half-float linear alpha stays linear: only colour is converted.
    let l = layer(3, 1, be16(&[0x3400, 0x3400]));
    let file = write_xcf_with(11, 1, 1, 1, 500, 2, 1, &[l], &[]);
    let s = decode(&file, ImportLimits::default()).unwrap();
    assert_eq!(px(&s, 0)[3], 64);
    assert_eq!(px(&s, 0)[0], linear_to_srgb8(0.25));
}

#[test]
fn malformed_deep_and_indexed_files_error_and_never_panic() {
    let deep = write_xcf_with(
        11,
        70,
        3,
        0,
        250,
        2,
        1,
        &[layer(1, 70, vec![0x5a; 70 * 8])],
        &[],
    );
    let map = [[1, 2, 3]; 4];
    let indexed = write_xcf_with(11, 70, 1, 2, 150, 1, 2, &[layer(5, 70, vec![3; 140])], &map);
    let indexed_rle = write_xcf_with(0, 70, 1, 2, 0, 1, 1, &[layer(4, 70, vec![9; 70])], &map);
    for good in [deep, indexed, indexed_rle] {
        for n in (0..good.len()).step_by(5) {
            let _ = decode(&good[..n], ImportLimits::default());
        }
        for i in (0..good.len()).step_by(3) {
            let mut bad = good.clone();
            bad[i] ^= 0xa5;
            let _ = decode(&bad, ImportLimits::default());
        }
    }
    // An indexed image at a deep precision is malformed, not guessed.
    let bad = write_xcf_with(11, 1, 1, 2, 250, 2, 0, &[], &map);
    assert!(decode(&bad, ImportLimits::default()).is_err());
    // A hierarchy whose depth disagrees with the precision is refused.
    let l = layer(0, 1, vec![1, 2, 3]);
    let mismatched = write_xcf_with(11, 1, 1, 0, 250, 1, 0, &[l], &[]);
    let err = decode(&mismatched, ImportLimits::default()).unwrap_err();
    assert!(err.to_string().contains("hierarchy"), "{err}");
}
