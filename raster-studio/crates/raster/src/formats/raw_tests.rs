//! W13-C: DNG develop, vendor-RAW refusals and routing.

use super::fixture::{dng, srgb16, Spec, Storage};
use crate::codec::{
    decode_surface_bytes, decode_surface_bytes_as, decode_surface_path, encode, probe_bytes,
    CodecError, ExportFormat, ImportFormat, ImportLimits, SurfacePixels,
};

/// Four moderate colours, one per quadrant (linear sRGB), and the quadrant a
/// pixel of an `w x h` image falls in.
const QUADS: [[f64; 3]; 4] = [
    [0.45, 0.20, 0.10],
    [0.12, 0.40, 0.18],
    [0.10, 0.15, 0.50],
    [0.30, 0.30, 0.30],
];

fn quadrants(w: u32, h: u32) -> impl Fn(u32, u32) -> [f64; 3] {
    move |x, y| QUADS[usize::from(x >= w / 2) + 2 * usize::from(y >= h / 2)]
}

fn rgba16(bytes: &[u8]) -> (u32, u32, Vec<u16>) {
    let s = decode_surface_bytes(bytes, ImportLimits::default())
        .unwrap_or_else(|e| panic!("decode failed: {e}"));
    assert_eq!(s.source_format, ImportFormat::Dng);
    let SurfacePixels::Rgba16(px) = s.pixels else {
        panic!("a DNG develops to 16 bits");
    };
    (s.width, s.height, px)
}

fn pixel(px: &[u16], w: u32, x: u32, y: u32) -> [u16; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

/// Within 1% of full scale of the scene colour, sRGB-encoded.
fn assert_colour(got: [u16; 4], want: [f64; 3], what: &str) {
    for c in 0..3 {
        let e = srgb16(want[c]);
        assert!(
            (i32::from(got[c]) - i32::from(e)).abs() <= 655,
            "{what}: channel {c} is {} but the scene is {e} ({got:?} vs {want:?})",
            got[c]
        );
    }
    assert_eq!(got[3], u16::MAX, "{what}: opaque");
}

fn check_quadrants(bytes: &[u8], w: u32, h: u32, what: &str) {
    let (ow, oh, px) = rgba16(bytes);
    assert_eq!((ow, oh), (w, h), "{what}");
    for (q, &(x, y)) in [
        (w / 4, h / 4),
        (3 * w / 4, h / 4),
        (w / 4, 3 * h / 4),
        (3 * w / 4, 3 * h / 4),
    ]
    .iter()
    .enumerate()
    {
        assert_colour(
            pixel(&px, w, x, y),
            QUADS[q],
            &format!("{what} quadrant {q}"),
        );
    }
}

/// A synthetic DNG, built from a known scene through a known camera matrix,
/// develops back to the scene's colours: every storage (16-bit, packed
/// 12-bit big-endian, lossless-JPEG tiles with and without restart
/// markers) and every Bayer phase.
#[test]
fn a_synthetic_dng_develops_to_its_scene_colours_in_every_storage_and_phase() {
    let (w, h) = (32, 32);
    for storage in [
        Storage::Uncompressed16,
        Storage::Packed12,
        Storage::LosslessJpegTiles {
            tile: 16,
            restart: false,
        },
        Storage::LosslessJpegTiles {
            tile: 16,
            restart: true,
        },
    ] {
        for pattern in [[0, 1, 1, 2], [2, 1, 1, 0], [1, 0, 2, 1], [1, 2, 0, 1]] {
            let spec = Spec {
                storage,
                pattern,
                big_endian: storage == Storage::Packed12,
                ..Spec::default()
            };
            let file = dng(&spec, quadrants(w, h));
            check_quadrants(&file, w, h, &format!("{storage:?} {pattern:?}"));
            let info = probe_bytes(&file, ImportLimits::default()).unwrap();
            assert_eq!(
                (info.width, info.height, info.format),
                (w, h, ImportFormat::Dng)
            );
            assert_eq!(info.pixel_format, crate::format::PixelFormat::Rgba16);
        }
    }
}

/// Tiles that overhang the image (a 24x20 image in 16x16 tiles) are cut at
/// the edge; a linear-raw DNG (three samples, no mosaic) develops too; so
/// does one with no `AsShotNeutral`, whose white balance then comes from the
/// colour matrix's response to D65.
#[test]
fn overhanging_tiles_linear_raw_and_a_missing_neutral_all_develop() {
    let (w, h) = (24, 20);
    let spec = Spec {
        width: w,
        height: h,
        storage: Storage::LosslessJpegTiles {
            tile: 16,
            restart: false,
        },
        ..Spec::default()
    };
    check_quadrants(&dng(&spec, quadrants(w, h)), w, h, "overhanging tiles");
    let linear = Spec {
        linear_raw: true,
        ..Spec::default()
    };
    check_quadrants(&dng(&linear, quadrants(32, 32)), 32, 32, "linear raw");
    let no_neutral = Spec {
        write_neutral: false,
        ..Spec::default()
    };
    check_quadrants(
        &dng(&no_neutral, quadrants(32, 32)),
        32,
        32,
        "no AsShotNeutral",
    );
}

/// The demosaic interpolates green along an edge, not across it: a hard
/// vertical edge between two neutral greys develops with no colour fringe.
/// (Averaging the four green neighbours, as plain bilinear does, puts a
/// fringe of several percent on both sides of it.)
#[test]
fn a_hard_neutral_edge_develops_without_colour_fringes() {
    for pattern in [[0, 1, 1, 2], [1, 0, 2, 1]] {
        let spec = Spec {
            pattern,
            ..Spec::default()
        };
        let file = dng(&spec, |x, _| if x < 15 { [0.05; 3] } else { [0.6; 3] });
        let (w, h, px) = rgba16(&file);
        let mut worst = 0;
        for y in 0..h {
            for x in 0..w {
                let p = pixel(&px, w, x, y);
                let spread = p[..3].iter().max().unwrap() - p[..3].iter().min().unwrap();
                worst = worst.max(spread);
            }
        }
        assert!(
            worst <= 400,
            "{pattern:?}: a neutral edge picked up a colour fringe of {worst}/65535"
        );
    }
}

/// `Orientation` 6 (rotate 90 clockwise) swaps the size and puts the scene's
/// top-left quadrant at the top right; 3 turns it upside down.
#[test]
fn the_orientation_tag_is_honoured() {
    let (w, h) = (32, 16);
    for (orientation, size, top_left_lands) in [
        (1u16, (32, 16), (4, 4)),
        (6, (16, 32), (12, 4)),
        (3, (32, 16), (28, 12)),
        (8, (16, 32), (4, 28)),
    ] {
        let spec = Spec {
            width: w,
            height: h,
            orientation,
            ..Spec::default()
        };
        let (ow, oh, px) = rgba16(&dng(&spec, quadrants(w, h)));
        assert_eq!((ow, oh), size, "orientation {orientation}");
        let (x, y) = top_left_lands;
        assert_colour(
            pixel(&px, ow, x, y),
            QUADS[0],
            &format!("orientation {orientation}"),
        );
    }
}

/// `ActiveArea` masks off the sensor border (written white here) and
/// `DefaultCrop` trims inside it: neither shows in the result.
#[test]
fn active_area_and_default_crop_are_applied() {
    let spec = Spec {
        width: 40,
        height: 36,
        active_area: Some([2, 4, 34, 36]),
        crop: Some([2, 2, 28, 28]),
        ..Spec::default()
    };
    // The scene is positioned inside the active area: 32x32.
    let file = dng(&spec, quadrants(32, 32));
    let (w, h, px) = rgba16(&file);
    assert_eq!((w, h), (28, 28));
    // Quadrant centres of the 32x32 active area, shifted by the crop.
    for (q, (x, y)) in [(6, 6), (22, 6), (6, 22), (22, 22)].into_iter().enumerate() {
        assert_colour(
            pixel(&px, w, x, y),
            QUADS[q],
            &format!("cropped quadrant {q}"),
        );
    }
    // No white border anywhere.
    assert!(px.chunks(4).all(|p| p[..3].iter().any(|&c| c < 60_000)));
}

/// A tiny deterministic generator for the damage tests.
fn lcg(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *seed >> 33
}

/// Truncated and bit-flipped DNGs (every storage) are errors or images,
/// never panics; so are the same bytes pushed through `probe`.
#[test]
fn damaged_dngs_error_and_never_panic() {
    let mut seed = 0x5eed;
    for storage in [
        Storage::Uncompressed16,
        Storage::Packed12,
        Storage::LosslessJpegTiles {
            tile: 16,
            restart: true,
        },
    ] {
        let spec = Spec {
            width: 24,
            height: 20,
            storage,
            ..Spec::default()
        };
        let good = dng(&spec, quadrants(24, 20));
        let mut cases: Vec<Vec<u8>> = (0..good.len())
            .step_by(7)
            .map(|n| good[..n].to_vec())
            .collect();
        for _ in 0..600 {
            let mut bad = good.clone();
            for _ in 0..1 + lcg(&mut seed) % 3 {
                let at = (lcg(&mut seed) as usize) % bad.len();
                bad[at] ^= 1 << (lcg(&mut seed) % 8);
            }
            cases.push(bad);
        }
        for (i, bytes) in cases.iter().enumerate() {
            let outcome = std::panic::catch_unwind(|| {
                let _ = super::decode(bytes, ImportLimits::default());
                let _ = super::probe(bytes, ImportLimits::default());
                let _ = decode_surface_bytes(bytes, ImportLimits::default());
            });
            assert!(outcome.is_ok(), "{storage:?} case {i} panicked");
        }
    }
    // Specific damage is reported, by name.
    let err = super::decode(b"II*\0\x08\0\0\0\0\0", ImportLimits::default()).unwrap_err();
    assert!(err.to_string().contains("DNG"), "{err}");
    let err = super::decode(b"not a tiff at all", ImportLimits::default()).unwrap_err();
    assert!(err.to_string().contains("DNG"), "{err}");
}

/// Declared dimensions are checked against the limits before any buffer.
#[test]
fn a_huge_declared_raw_is_refused_before_allocating() {
    let file = dng(&Spec::default(), quadrants(32, 32));
    let tight = ImportLimits {
        max_alloc_bytes: 1024,
        ..ImportLimits::default()
    };
    let err = decode_surface_bytes(&file, tight).unwrap_err();
    assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
}

/// A minimal TIFF: `Make`, a CFA photometric and a size, no `DNGVersion`.
fn vendor_tiff(make: &str) -> Vec<u8> {
    let make = format!("{make}\0");
    // Header, then one IFD of four entries at 8, then the Make string.
    let make_at = 8 + 2 + 4 * 12 + 4;
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend(4u16.to_le_bytes());
    for (tag, kind, count, value) in [
        (256u16, 4u16, 1u32, 64u32),
        (257, 4, 1, 64),
        (262, 3, 1, 32803),
        (271, 2, make.len() as u32, make_at),
    ] {
        out.extend(tag.to_le_bytes());
        out.extend(kind.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(value.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes());
    out.extend(make.as_bytes());
    out.extend([0u8; 64]);
    out
}

/// Each vendor RAW reaches the RAW reader by content and is refused naming
/// its format; each routed extension maps to the right reader; an ordinary
/// TIFF is not mistaken for a RAW; a DNG under a `.png` name is still a DNG.
#[test]
fn vendor_raws_are_refused_by_name_and_every_extension_routes() {
    let mut cr3 = vec![0, 0, 0, 24];
    cr3.extend_from_slice(b"ftypcrx \0\0\0\x01crx isom");
    let mut cr2 = b"II*\0\x10\0\0\0CR\x02\0\0\0\0\0".to_vec();
    cr2.extend([0u8; 32]);
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("Canon CR2", cr2),
        ("Canon CR3", cr3),
        ("Nikon NEF", vendor_tiff("NIKON CORPORATION")),
        ("Sony ARW", vendor_tiff("SONY")),
        ("Fujifilm RAF", {
            let mut v = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
            v.extend([0u8; 64]);
            v
        }),
        ("Olympus ORF", {
            let mut v = b"IIRO\x08\0\0\0".to_vec();
            v.extend([0u8; 64]);
            v
        }),
        ("Panasonic RW2", {
            let mut v = b"IIU\0\x08\0\0\0".to_vec();
            v.extend([0u8; 64]);
            v
        }),
    ];
    for (name, bytes) in &files {
        let err = decode_surface_bytes(bytes, ImportLimits::default()).unwrap_err();
        let text = err.to_string();
        assert!(matches!(err, CodecError::Unsupported(_)), "{name}: {text}");
        assert!(
            text.contains(name) && text.contains("DNG"),
            "{name}: {text}"
        );
        let text = probe_bytes(bytes, ImportLimits::default())
            .unwrap_err()
            .to_string();
        assert!(text.contains(name), "{name}: {text}");
    }
    for (ext, format) in [
        ("dng", ImportFormat::Dng),
        ("DNG", ImportFormat::Dng),
        ("cr2", ImportFormat::CameraRaw),
        ("cr3", ImportFormat::CameraRaw),
        ("nef", ImportFormat::CameraRaw),
        ("arw", ImportFormat::CameraRaw),
        ("raf", ImportFormat::CameraRaw),
        ("orf", ImportFormat::CameraRaw),
        ("rw2", ImportFormat::CameraRaw),
    ] {
        assert_eq!(ImportFormat::from_extension(ext), Some(format), "{ext}");
        assert!(ImportFormat::ALL.contains(&format));
    }
    assert!(ImportFormat::Dng.is_decodable_here());
    assert!(!ImportFormat::CameraRaw.is_decodable_here());

    let dir = std::env::temp_dir().join(format!("w13c-raw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let px: Vec<u8> = (0..4 * 3 * 4).map(|i| (i * 5) as u8).collect();
    let tiff = encode(ExportFormat::Tiff, 4, 3, &px).unwrap();
    // An ordinary TIFF is still a TIFF, by content and under its own name.
    let s = decode_surface_bytes(&tiff, ImportLimits::default()).unwrap();
    assert_eq!(s.source_format, ImportFormat::Tiff);
    // Opened by path, a TIFF named `.nef` is refused as a camera RAW rather
    // than opened as the preview a real NEF would hold (a bytes-only decode
    // has no name to go on and reads it as the TIFF above).
    let named_nef = dir.join("preview_only.nef");
    std::fs::write(&named_nef, &tiff).unwrap();
    let err = decode_surface_path(&named_nef, ImportLimits::default()).unwrap_err();
    assert!(err.to_string().contains("DNG"), "{err}");
    // The extension-fallback refusal reads as a sentence.
    assert!(
        err.to_string()
            .contains("opening camera RAW files is not supported"),
        "{err}"
    );
    // A DNG under a `.png` name opens as the DNG it is.
    let lying = dir.join("really_a_dng.png");
    std::fs::write(&lying, dng(&Spec::default(), quadrants(32, 32))).unwrap();
    let s = decode_surface_path(&lying, ImportLimits::default()).unwrap();
    assert_eq!(s.source_format, ImportFormat::Dng);
    let named_dng = dir.join("photo.DNG");
    std::fs::write(&named_dng, dng(&Spec::default(), quadrants(32, 32))).unwrap();
    let s = decode_surface_path(&named_dng, ImportLimits::default()).unwrap();
    assert_eq!(s.source_format, ImportFormat::Dng);
    let _ = std::fs::remove_dir_all(&dir);
    // A hint does not turn garbage into a DNG.
    let err = decode_surface_bytes_as(b"garbage!", ImportLimits::default(), ImportFormat::Dng)
        .unwrap_err();
    assert!(err.to_string().contains("DNG"), "{err}");
}

/// The compressions a DNG may use but this reader does not are named.
#[test]
fn unsupported_dng_compressions_are_named() {
    let good = dng(&Spec::default(), quadrants(32, 32));
    // The raw IFD's Compression entry: find tag 259 = 1 (SHORT) and patch it.
    let needle = [0x03, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00];
    let at = good
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the fixture writes Compression = 1");
    for (code, word) in [(34892u16, "lossy"), (52546, "JPEG XL"), (8, "deflate")] {
        let mut bad = good.clone();
        bad[at + 8..at + 10].copy_from_slice(&code.to_le_bytes());
        let text = decode_surface_bytes(&bad, ImportLimits::default())
            .unwrap_err()
            .to_string();
        assert!(text.contains(word), "{code}: {text}");
    }
}
