//! W16-B: synthetic files in each Photoshop colour mode, opened through the
//! real import (`document_from_psd`) and saved through the real export
//! (`psd_from_document`).

use compositor::{composite_region, CompositeOptions};
use editor_core::color_mode::mode;
use psd::colour_modes::{DuotoneInkRecord, DuotoneRecord, IndexedTable, InkColor};
use psd::{Channel, ColorMode, Depth, MergedImage, PsdFile, PsdHeader, PsdLayer, Rect};
use raster::PixelRect;

use super::super::{document_from_psd, psd_from_document, PsdImport};

fn header(mode: ColorMode, channels: u16, depth: Depth, w: u32, h: u32) -> PsdHeader {
    PsdHeader {
        channels,
        width: w,
        height: h,
        depth,
        color_mode: mode,
    }
}

fn flatten(import: &PsdImport) -> Vec<u8> {
    let doc = &import.imported.document;
    composite_region(
        doc,
        &import.imported.tiles,
        PixelRect::new(0, 0, doc.width(), doc.height()),
        0,
        CompositeOptions::default(),
    )
    .unwrap()
    .to_rgba8(&doc.meta.color_space)
}

/// Open `bytes`, then save the document through Save as PSD and read the
/// saved file back with the `psd` crate.
fn open_and_save(bytes: &[u8]) -> (PsdImport, psd::PsdFile, Vec<String>) {
    let import = document_from_psd(bytes, "in.psd", 10).unwrap();
    let composite = flatten(&import);
    let (saved, notes) = psd_from_document(
        &import.imported.document,
        &import.imported.tiles,
        &composite,
    )
    .unwrap();
    (import, psd::read(&saved).unwrap(), notes.notes().to_vec())
}

/// Ink numbers as Photoshop stores them (inverted) for the separations this
/// build's CMYK model makes of a spread of colours.
fn cmyk_samples() -> Vec<[u8; 4]> {
    [
        [200u8, 30, 40],
        [20, 120, 200],
        [240, 220, 40],
        [128, 128, 128],
        [255, 255, 255],
        [60, 160, 90],
        [180, 140, 200],
        [230, 120, 60],
    ]
    .iter()
    .map(|rgb| {
        let c = color::cmyk::rgb8_to_cmyk(*rgb);
        [c.c, c.m, c.y, c.k].map(|v| 255 - (v * 255.0).round() as u8)
    })
    .collect()
}

fn planar(samples: &[[u8; 4]], k: usize) -> Vec<Vec<u8>> {
    (0..k)
        .map(|c| samples.iter().map(|s| s[c]).collect())
        .collect()
}

#[test]
fn a_layered_cmyk_psd_opens_in_cmyk_mode_and_saves_back_as_the_same_ink_numbers() {
    let samples = cmyk_samples();
    let w = samples.len() as u32;
    let planes = planar(&samples, 4);
    let mut file = PsdFile::new(header(ColorMode::Cmyk, 5, Depth::Eight, w, 1));
    let mut layer = PsdLayer::raster("Ink", Rect::sized(w, 1));
    layer
        .channels
        .push(Channel::new(psd::CHANNEL_ALPHA, vec![255; w as usize]));
    for (id, p) in planes.iter().enumerate() {
        layer.channels.push(Channel::new(id as i16, p.clone()));
    }
    file.layers.push(layer);
    let mut merged = planes.clone();
    merged.push(vec![255; w as usize]);
    file.merged = Some(MergedImage { channels: merged });

    let (import, saved, notes) = open_and_save(&psd::write(&file).unwrap());
    let doc = &import.imported.document;
    assert_eq!(doc.meta.color_mode, mode::CMYK, "CMYK opens in CMYK mode");
    assert!(import.notes.is_empty(), "{:?}", import.notes.notes());
    // Each pixel is the ink model's colour for its inks, not the samples
    // read as RGB.
    let rgba = flatten(&import);
    for (i, s) in samples.iter().enumerate() {
        let ink = s.map(|v| 1.0 - f32::from(v) / 255.0);
        let want = color::cmyk::cmyk_to_rgb8(color::cmyk::Cmyk {
            c: ink[0],
            m: ink[1],
            y: ink[2],
            k: ink[3],
        });
        assert_eq!(&rgba[i * 4..i * 4 + 3], &want, "pixel {i}");
    }

    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(saved.header.color_mode, ColorMode::Cmyk);
    assert_eq!(saved.header.channels, 5);
    let layer = &saved.layers[0];
    for (c, want) in planes.iter().enumerate() {
        let got = &layer.channel(c as i16).unwrap().data;
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!(g.abs_diff(*w) <= 1, "ink {c} pixel {i}: {g} vs {w}");
        }
        let merged = &saved.merged.as_ref().unwrap().channels[c];
        for (g, w) in merged.iter().zip(want) {
            assert!(g.abs_diff(*w) <= 1, "merged ink {c}: {g} vs {w}");
        }
    }
}

#[test]
fn a_lab_psd_opens_in_lab_mode_and_saves_back_within_one_code() {
    // In-gamut Lab numbers (8-bit encoding), from real colours.
    let samples: Vec<[u8; 4]> = [
        [200u8, 30, 40],
        [20, 120, 200],
        [240, 220, 40],
        [128, 128, 128],
        [60, 160, 90],
        [180, 140, 200],
        [250, 250, 250],
    ]
    .iter()
    .map(|rgb| {
        let lab = color::model::rgb_to_lab(rgb.map(|v| f32::from(v) / 255.0));
        [
            (lab[0] * 2.55).round() as u8,
            (lab[1] + 128.0).round() as u8,
            (lab[2] + 128.0).round() as u8,
            0,
        ]
    })
    .collect();
    let w = samples.len() as u32;
    let planes = planar(&samples, 3);
    for depth in [Depth::Eight, Depth::Sixteen] {
        let widen = |p: &Vec<u8>| -> Vec<u8> {
            match depth {
                Depth::Sixteen => p
                    .iter()
                    .flat_map(|v| (u16::from(*v) * 257).to_be_bytes())
                    .collect(),
                _ => p.clone(),
            }
        };
        let mut file = PsdFile::new(header(ColorMode::Lab, 3, depth, w, 1));
        file.merged = Some(MergedImage {
            channels: planes.iter().map(widen).collect(),
        });
        let (import, saved, _) = open_and_save(&psd::write(&file).unwrap());
        assert_eq!(import.imported.document.meta.color_mode, mode::LAB);
        assert_eq!(saved.header.color_mode, ColorMode::Lab, "{depth:?}");
        assert_eq!(saved.header.depth, depth, "the depth is kept");
        let layer = &saved.layers[0];
        for (c, want) in planes.iter().enumerate() {
            let got = &layer.channel(c as i16).unwrap().data;
            let got: Vec<u8> = match depth {
                Depth::Sixteen => got.chunks(2).map(|b| b[0]).collect(),
                _ => got.clone(),
            };
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                assert!(
                    g.abs_diff(*w) <= 1,
                    "{depth:?} Lab {c} pixel {i}: {g} vs {w}"
                );
            }
        }
    }
}

#[test]
fn an_indexed_psd_opens_in_indexed_mode_and_saves_back_as_a_palette() {
    let table = IndexedTable {
        colors: vec![[10, 20, 30], [200, 100, 50], [0, 0, 0]],
        transparent: Some(2),
    };
    let mut file = PsdFile::new(header(ColorMode::Indexed, 1, Depth::Eight, 4, 1));
    file.color_mode_data = table.mode_data();
    file.resources = table.resources();
    file.merged = Some(MergedImage {
        channels: vec![vec![1, 0, 2, 1]],
    });
    let (import, saved, _) = open_and_save(&psd::write(&file).unwrap());
    assert_eq!(import.imported.document.meta.color_mode, mode::INDEXED);
    assert_eq!(
        flatten(&import),
        vec![200, 100, 50, 255, 10, 20, 30, 255, 0, 0, 0, 0, 200, 100, 50, 255]
    );
    assert_eq!(saved.header.color_mode, ColorMode::Indexed);
    assert!(saved.layers.is_empty(), "an Indexed file is flat");
    let back = IndexedTable::read(&saved).unwrap();
    let t = back
        .transparent
        .expect("the transparent pixel keeps an index");
    let index = &saved.merged.as_ref().unwrap().channels[0];
    assert_eq!(index[2], t);
    assert_eq!(back.lookup(index[0]), [200, 100, 50]);
    assert_eq!(back.lookup(index[1]), [10, 20, 30]);
    assert_eq!(index[3], index[0]);
}

#[test]
fn a_greyscale_psd_opens_in_grayscale_mode_and_saves_as_greyscale() {
    let mut file = PsdFile::new(header(ColorMode::Grayscale, 1, Depth::Eight, 3, 1));
    file.merged = Some(MergedImage {
        channels: vec![vec![0, 90, 255]],
    });
    let (import, saved, notes) = open_and_save(&psd::write(&file).unwrap());
    assert_eq!(import.imported.document.meta.color_mode, mode::GRAYSCALE);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(saved.header.color_mode, ColorMode::Grayscale);
    assert_eq!(saved.merged.as_ref().unwrap().channels[0], vec![0, 90, 255]);
}

#[test]
fn a_one_bit_bitmap_psd_opens_in_bitmap_mode_and_saves_as_greyscale() {
    let (w, h) = (9u32, 1u32);
    let grey = [0u8, 255, 0, 0, 255, 255, 0, 255, 0];
    let mut s = psd::bytes::Sink::new();
    header(ColorMode::Bitmap, 1, Depth::Eight, w, h).write(&mut s);
    s.u32(0);
    s.u32(0);
    s.u32(0);
    s.u16(0);
    s.bytes(&psd::colour_modes::pack_bitmap(&grey, w, h));
    let (import, saved, notes) = open_and_save(&s.into_inner());
    assert_eq!(import.imported.document.meta.color_mode, mode::BITMAP);
    let rgba = flatten(&import);
    let got: Vec<u8> = rgba.chunks(4).map(|p| p[0]).collect();
    assert_eq!(got, grey);
    assert_eq!(saved.header.color_mode, ColorMode::Grayscale);
    assert!(notes.iter().any(|n| n.contains("1-bit")), "{notes:?}");
}

fn duotone_file(record: Vec<u8>) -> Vec<u8> {
    let mut file = PsdFile::new(header(ColorMode::Duotone, 1, Depth::Eight, 2, 1));
    file.color_mode_data = record;
    file.merged = Some(MergedImage {
        channels: vec![vec![0, 255]],
    });
    psd::write(&file).unwrap()
}

#[test]
fn a_duotone_psd_opens_with_its_inks_applied_or_as_its_grey_base() {
    let record = DuotoneRecord {
        inks: vec![
            DuotoneInkRecord {
                color: InkColor::Rgb([0, 0, 0]),
                name: "Black".into(),
                curve: vec![[0.0, 0.0], [1.0, 1.0]],
            },
            DuotoneInkRecord {
                color: InkColor::Rgb([65535, 32896, 0]),
                name: "Orange".into(),
                curve: vec![[0.0, 0.0], [1.0, 0.5]],
            },
        ],
    };
    let import = document_from_psd(&duotone_file(record.encode()), "d.psd", 10).unwrap();
    assert_eq!(import.imported.document.meta.color_mode, mode::DUOTONE);
    let spec = color::duotone::DuotoneSpec {
        inks: vec![
            color::duotone::DuotoneInk {
                color: [0, 0, 0],
                curve: vec![[0.0, 0.0], [1.0, 1.0]],
            },
            color::duotone::DuotoneInk {
                color: [255, 128, 0],
                curve: vec![[0.0, 0.0], [1.0, 0.5]],
            },
        ],
    };
    let rgba = flatten(&import);
    assert_eq!(&rgba[..3], &spec.render(0));
    assert_eq!(&rgba[4..7], &spec.render(255));

    // An unreadable record: the greyscale base, and the report says why.
    let import = document_from_psd(&duotone_file(vec![1, 2, 3]), "d.psd", 10).unwrap();
    assert_eq!(import.imported.document.meta.color_mode, mode::GRAYSCALE);
    assert_eq!(&flatten(&import)[..8], &[0, 0, 0, 255, 255, 255, 255, 255]);
    assert!(import
        .notes
        .notes()
        .iter()
        .any(|n| n.contains("Duotone ink record")));
}

#[test]
fn a_multichannel_psd_shows_its_inks_as_cmy_and_reports_the_rest() {
    let mut file = PsdFile::new(header(ColorMode::Multichannel, 4, Depth::Eight, 1, 1));
    file.merged = Some(MergedImage {
        channels: vec![vec![255], vec![255], vec![255], vec![0]],
    });
    let import = document_from_psd(&psd::write(&file).unwrap(), "m.psd", 10).unwrap();
    assert_eq!(import.imported.document.meta.color_mode, mode::RGB);
    assert_eq!(&flatten(&import)[..3], &[255, 255, 255], "no ink is paper");
    assert!(
        import
            .notes
            .notes()
            .iter()
            .any(|n| n.contains("1 further channel")),
        "{:?}",
        import.notes.notes()
    );
}

#[test]
fn a_malformed_palette_is_an_open_error_not_a_guess() {
    let table = IndexedTable {
        colors: vec![[1, 2, 3]],
        transparent: None,
    };
    let mut file = PsdFile::new(header(ColorMode::Indexed, 1, Depth::Eight, 1, 1));
    file.color_mode_data = table.mode_data();
    file.merged = Some(MergedImage {
        channels: vec![vec![0]],
    });
    let good = psd::write(&file).unwrap();
    let mut bad = good[..26].to_vec();
    bad.extend_from_slice(&100u32.to_be_bytes());
    bad.extend_from_slice(&good[30..130]);
    bad.extend_from_slice(&good[30 + 768..]);
    let err = document_from_psd(&bad, "bad.psd", 10)
        .err()
        .expect("refused");
    assert!(err.to_string().contains("768-byte palette"), "{err}");
}

#[test]
fn an_rgb_document_saved_in_cmyk_mode_is_written_as_cmyk() {
    // An RGB file converted by Image > Mode > CMYK in the editor is a CMYK
    // document; Save as PSD writes CMYK samples, and reopening it keeps both
    // the mode and the pixels.
    let rgba = [200u8, 30, 40, 255, 20, 120, 200, 255];
    let mut file = psd::from_rgba8(2, 1, &rgba).unwrap();
    let mut layer = PsdLayer::raster("L", Rect::sized(2, 1));
    layer.set_rgba8(&rgba).unwrap();
    file.layers.push(layer);
    let mut import = document_from_psd(&psd::write(&file).unwrap(), "rgb.psd", 10).unwrap();
    import.imported.document.meta.color_mode = mode::CMYK;
    let composite = flatten(&import);
    let (bytes, _) = psd_from_document(
        &import.imported.document,
        &import.imported.tiles,
        &composite,
    )
    .unwrap();
    assert_eq!(
        psd::read(&bytes).unwrap().header.color_mode,
        ColorMode::Cmyk
    );
    let again = document_from_psd(&bytes, "cmyk.psd", 10).unwrap();
    assert_eq!(again.imported.document.meta.color_mode, mode::CMYK);
    let back = flatten(&again);
    for (a, b) in back.iter().zip(&composite) {
        assert!(a.abs_diff(*b) <= 1, "{back:?} vs {composite:?}");
    }
}
