//! W11-A: adjustment layers leave a document as their `.psd` adjustment keys
//! and come back live, through the real export and import routes
//! (`psd_from_document` / `document_from_psd`) and the real compositor.

use super::*;

const W: u32 = 32;
const H: u32 = 16;

/// A horizontal hue sweep with a vertical value ramp, so every adjustment
/// below moves real pixels.
fn sweep() -> DecodedImage {
    let mut rgba8 = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let t = x as f32 / (W - 1) as f32;
            let v = 0.2 + 0.8 * (y as f32 / (H - 1) as f32);
            let r = (1.0 - t) * v;
            let g = (1.0 - (2.0 * t - 1.0).abs()) * v;
            let b = t * v;
            for c in [r, g, b] {
                rgba8.push((c * 255.0).round() as u8);
            }
            rgba8.push(255);
        }
    }
    DecodedImage {
        width: W,
        height: H,
        rgba8,
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
    }
}

fn render(document: &Document, tiles: &MemoryTileSource) -> Vec<u8> {
    compositor::composite_region(
        document,
        tiles,
        raster::PixelRect::new(0, 0, document.width(), document.height()),
        0,
        compositor::CompositeOptions::default(),
    )
    .unwrap()
    .to_rgba8(&document.meta.color_space)
}

fn with_adjustments(kinds: &[(&str, AdjustmentKind)]) -> ImportedDocument {
    let mut imported = document_from_image(&sweep(), "adjusted", 10).unwrap();
    for (name, kind) in kinds {
        imported
            .document
            .layers
            .push_root(Layer::with_kind(
                *name,
                LayerKind::Adjustment(layer_model::AdjustmentLayer { kind: kind.clone() }),
            ))
            .unwrap();
    }
    imported
}

fn adjustment_named(document: &Document, name: &str) -> AdjustmentKind {
    let id = document
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| document.layers.get(*id).is_some_and(|l| l.name == name))
        .unwrap_or_else(|| panic!("no layer called {name}"));
    match &document.layers.get(id).unwrap().kind {
        LayerKind::Adjustment(a) => a.kind.clone(),
        other => panic!("{name} came back as {other:?}, not a live adjustment"),
    }
}

fn q(n: i32) -> f32 {
    n as f32 / 255.0
}

#[test]
fn levels_curves_and_hue_saturation_export_and_reimport_live_with_an_equal_composite() {
    let kinds = [
        (
            "Levels 1",
            AdjustmentKind::Levels {
                black: q(20),
                white: q(230),
                gamma: 1.3,
            },
        ),
        (
            "Curves 1",
            AdjustmentKind::Curves {
                points: vec![[0.0, q(10)], [q(96), q(140)], [1.0, q(245)]],
            },
        ),
        (
            "Hue/Saturation 1",
            AdjustmentKind::HueSaturation {
                hue: 40.0,
                saturation: -0.25,
                lightness: 0.1,
            },
        ),
    ];
    let first = with_adjustments(&kinds);
    let before = render(&first.document, &first.tiles);
    // The adjustments really do something, or equality below proves nothing.
    let plain = document_from_image(&sweep(), "plain", 10).unwrap();
    assert_ne!(before, render(&plain.document, &plain.tiles));

    let (bytes, notes) = psd_from_document(&first.document, &first.tiles, &before).unwrap();
    assert!(
        notes.summary().is_none(),
        "these adjustments export without loss: {:?}",
        notes.summary()
    );

    let back = document_from_psd(&bytes, "adjusted.psd", 10).unwrap();
    assert!(
        back.notes.summary().is_none(),
        "and import without loss: {:?}",
        back.notes.summary()
    );
    let doc = &back.imported.document;
    for (name, kind) in &kinds {
        assert_eq!(&adjustment_named(doc, name), kind, "{name} changed");
    }

    let after = render(doc, &back.imported.tiles);
    assert_eq!(before.len(), after.len());
    let worst = before
        .iter()
        .zip(&after)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(worst <= 1, "the reopened composite differs by {worst}/255");
}

#[test]
fn every_psd_adjustment_kind_survives_the_document_round_trip() {
    let lut: Vec<[f32; 3]> = (0..8)
        .map(|i| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32])
        .collect();
    let kinds = [
        ("Invert", AdjustmentKind::Invert),
        (
            "Brightness",
            AdjustmentKind::BrightnessContrast {
                brightness: q(30),
                contrast: 0.2,
            },
        ),
        (
            "Balance",
            AdjustmentKind::ColorBalance {
                shadows: [0.1, 0.0, -0.1],
                midtones: [0.0, 0.2, 0.0],
                highlights: [-0.3, 0.0, 0.0],
            },
        ),
        (
            "B&W",
            AdjustmentKind::BlackAndWhite {
                weights: [0.4, 0.6, 0.4, 0.6, 0.2, 0.8],
                tint: None,
            },
        ),
        (
            "Filter",
            AdjustmentKind::PhotoFilter {
                color_srgb: [1.0, 0.0, 0.0],
                density: 0.3,
                preserve_luminosity: true,
            },
        ),
        (
            "Mixer",
            AdjustmentKind::ChannelMixer {
                rows: [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 0.5, 0.5, 0.0],
                    [0.0, 0.0, 1.0, 0.1],
                ],
                monochrome: false,
            },
        ),
        ("Posterize", AdjustmentKind::Posterize { levels: 5 }),
        ("Threshold", AdjustmentKind::Threshold { level: q(100) }),
        (
            "Map",
            AdjustmentKind::GradientMap {
                stops: vec![(0.0, [0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 0.0])],
                reverse: false,
            },
        ),
        (
            "Selective",
            AdjustmentKind::SelectiveColor {
                ranges: [[0.1, 0.0, 0.0, 0.0]; 9],
                relative: true,
            },
        ),
        (
            "Exposure",
            AdjustmentKind::ExposureFull {
                stops: 0.5,
                offset: 0.0,
                gamma: 1.25,
            },
        ),
        (
            "Vibrance",
            AdjustmentKind::Vibrance {
                vibrance: 0.4,
                saturation: -0.1,
            },
        ),
        (
            "Lookup",
            AdjustmentKind::ColorLookup {
                name: "Identity".into(),
                size: 2,
                table: lut,
            },
        ),
    ];
    let first = with_adjustments(&kinds);
    let composite = render(&first.document, &first.tiles);
    let (bytes, notes) = psd_from_document(&first.document, &first.tiles, &composite).unwrap();
    assert!(notes.summary().is_none(), "{:?}", notes.summary());
    let back = document_from_psd(&bytes, "all.psd", 10).unwrap();
    assert!(back.notes.summary().is_none(), "{:?}", back.notes.summary());
    for (name, kind) in &kinds {
        assert_eq!(
            &adjustment_named(&back.imported.document, name),
            kind,
            "{name} changed"
        );
    }
}

#[test]
fn a_kind_a_psd_cannot_carry_still_exports_empty_and_says_so() {
    let first = with_adjustments(&[("Desaturate 1", AdjustmentKind::Desaturate)]);
    let composite = render(&first.document, &first.tiles);
    let (_, notes) = psd_from_document(&first.document, &first.tiles, &composite).unwrap();
    let told = notes.summary().expect("the loss is reported");
    assert!(told.contains("Desaturate 1"), "{told}");
}

fn psd_with(name: &str, key: [u8; 4], data: Vec<u8>) -> Vec<u8> {
    let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(4, 4));
    let mut base = psd::PsdLayer::raster("Base", psd::Rect::sized(4, 4));
    base.set_rgba8(&[90u8, 60, 30, 255].repeat(16)).unwrap();
    let mut adj = psd::PsdLayer::raster(name, psd::Rect::default());
    adj.pixel_data_irrelevant = true;
    adj.adjustment = Some(psd::Adjustment { key, data });
    file.layers = vec![base, adj];
    psd::write(&file).unwrap()
}

#[test]
fn a_malformed_adjustment_payload_is_a_report_line_not_a_panic() {
    // A Levels block with a gamma of zero: refused by the decoder.
    let mut data = vec![0u8, 2];
    for record in 0..29 {
        for v in [0i16, 255, 0, 255, if record == 0 { 0 } else { 100 }] {
            data.extend_from_slice(&v.to_be_bytes());
        }
    }
    let import = document_from_psd(&psd_with("Levels 1", *b"levl", data), "bad.psd", 10).unwrap();
    let told = import.notes.summary().expect("the refusal is reported");
    assert!(told.contains("Levels 1 (levl)"), "{told}");
    let line = import
        .notes
        .layers()
        .iter()
        .find(|l| l.name == "Levels 1")
        .unwrap();
    assert_eq!(line.outcome, PsdLayerOutcome::Unsupported);
    assert!(line.detail.contains("gamma is 0"), "{:?}", line.detail);
    let doc = &import.imported.document;
    let id = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| doc.layers.get(*id).is_some_and(|l| l.name == "Levels 1"))
        .unwrap();
    assert!(matches!(
        doc.layers.get(id).unwrap().kind,
        LayerKind::Raster(_)
    ));
}

#[test]
fn settings_the_model_cannot_hold_are_named_in_the_report() {
    let mut data = psd::adjustments::encode(&AdjustmentKind::HueSaturation {
        hue: 10.0,
        saturation: 0.0,
        lightness: 0.0,
    })
    .unwrap()
    .data;
    // The reds range's saturation (header 16 bytes, bounds 8, hue 2).
    data[26..28].copy_from_slice(&30i16.to_be_bytes());
    let import = document_from_psd(&psd_with("Hue 1", *b"hue2", data), "range.psd", 10).unwrap();
    assert_eq!(
        adjustment_named(&import.imported.document, "Hue 1"),
        AdjustmentKind::HueSaturation {
            hue: 10.0,
            saturation: 0.0,
            lightness: 0.0,
        }
    );
    let told = import
        .notes
        .summary()
        .expect("the range settings are named");
    assert!(
        told.contains("per-colour-range hue/saturation settings") && told.contains("Hue 1"),
        "{told}"
    );
}

/// Review round 2: a setting the `.psd` layout cannot store (Brightness +80,
/// a Black & White weight of -250%, Posterize 256) is not clamped and saved
/// as a different value. Export names the layer and the reason, and the
/// reopened file has no live layer claiming the changed value.
#[test]
fn settings_a_psd_cannot_store_are_reported_on_export_not_saved_clamped() {
    let kinds = [
        (
            "Brightness 1",
            AdjustmentKind::BrightnessContrast {
                brightness: 0.8,
                contrast: 0.0,
            },
            "brightness 0.8",
        ),
        (
            "Black & White 1",
            AdjustmentKind::BlackAndWhite {
                weights: [-2.5, 0.6, 0.4, 0.6, 0.2, 0.8],
                tint: None,
            },
            "the Rd weight -2.5",
        ),
        (
            "Posterize 1",
            AdjustmentKind::Posterize { levels: 256 },
            "posterize levels 256",
        ),
    ];
    // One document per kind: the summary lists only the first two names.
    for (name, kind, reason) in &kinds {
        let first = with_adjustments(&[(*name, kind.clone())]);
        let composite = render(&first.document, &first.tiles);
        let (bytes, notes) = psd_from_document(&first.document, &first.tiles, &composite).unwrap();
        let told = notes.summary().expect("the unstorable setting is reported");
        assert!(
            told.contains(&format!("{name} (")) && told.contains(reason),
            "{name} / {reason} missing from: {told}"
        );
        let back = document_from_psd(&bytes, "clamped.psd", 10).unwrap();
        let doc = &back.imported.document;
        let id = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| doc.layers.get(*id).is_some_and(|l| l.name == *name))
            .unwrap_or_else(|| panic!("no layer called {name}"));
        assert!(
            !matches!(doc.layers.get(id).unwrap().kind, LayerKind::Adjustment(_)),
            "{name} reopened live with a value other than the one saved"
        );
    }
}
