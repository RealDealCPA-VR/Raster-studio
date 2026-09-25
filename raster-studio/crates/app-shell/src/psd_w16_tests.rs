//! W16-J: Save as PSD keeps smart filters live (Photoshop's `filterFX`),
//! linked smart objects linked (`lnkE`), and writes an adjustment a `.psd`
//! has no layer for as its effect's pixels — never an empty layer. Every
//! test goes through the real routes: `psd_from_document` out,
//! `document_from_psd` back, the real compositor with the application's
//! smart-filter runner.

use std::collections::BTreeMap;

use super::*;
use layer_model::{SmartFilter, SmartParam};

const W: u32 = 40;
const H: u32 = 24;

fn find(doc: &Document, name: &str) -> LayerId {
    doc.layers
        .iter_depth_first()
        .into_iter()
        .find(|id| doc.layers.get(*id).is_some_and(|l| l.name == name))
        .unwrap_or_else(|| panic!("no layer called {name}"))
}

fn render_layer(doc: &Document, tiles: &MemoryTileSource, id: LayerId) -> Vec<u8> {
    compositor::composite_subtree(
        doc,
        tiles,
        id,
        raster::PixelRect::new(0, 0, doc.width(), doc.height()),
        0,
        compositor::CompositeOptions::default(),
    )
    .expect("the layer renders")
    .to_rgba8(&doc.meta.color_space)
}

fn render(doc: &Document, tiles: &MemoryTileSource) -> Vec<u8> {
    compositor::composite_region(
        doc,
        tiles,
        raster::PixelRect::new(0, 0, doc.width(), doc.height()),
        0,
        compositor::CompositeOptions::default(),
    )
    .unwrap()
    .to_rgba8(&doc.meta.color_space)
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// A 12x12 source with hard edges, so a blur visibly changes it.
fn checker_source() -> Vec<u8> {
    (0..144u32)
        .flat_map(|i| {
            let (x, y) = (i % 12, i / 12);
            if (x / 3 + y / 3) % 2 == 0 {
                [250, 30, 20, 255]
            } else {
                [10, 40, 240, 255]
            }
        })
        .collect()
}

fn gaussian(radius: f32) -> SmartFilter {
    let mut params = BTreeMap::new();
    params.insert("radius".to_string(), SmartParam::Float(radius));
    params.insert("edge".to_string(), SmartParam::Choice(0));
    SmartFilter::new("GaussianBlur", params)
}

/// A document holding one smart object named `SO` over `origin`, with
/// `filters`, scaled 2x and moved to (6, 0).
fn smart_document(origin: AssetOrigin, filters: Vec<SmartFilter>) -> (Document, MemoryTileSource) {
    crate::menu_bridge::install_smart_filter_runner();
    let mut doc = Document::new(W, H, "so");
    let mut tiles = MemoryTileSource::new();
    let asset = AssetId::new();
    let linked = matches!(origin, AssetOrigin::Linked { .. });
    doc.set_asset_origin(AssetRecord {
        id: asset,
        origin,
        source_size: Some((12, 12)),
    });
    let so = doc
        .layers
        .push_root(Layer::with_kind(
            "SO",
            LayerKind::SmartObject(SmartObjectLayer {
                asset,
                linked,
                filters,
                filter_mask: None,
            }),
        ))
        .unwrap();
    doc.layers.get_mut(so).unwrap().transform =
        glam::Affine2::from_translation(glam::vec2(6.0, 0.0))
            * glam::Affine2::from_scale(glam::Vec2::new(2.0, 2.0));
    let edits = tile_edits_for_rgba(&checker_source(), psd::Rect::sized(12, 12), &mut tiles);
    doc.pixels
        .apply(PixelKey::Layer(so), &TileDelta::new(edits).unwrap());
    (doc, tiles)
}

fn png_of_checker() -> Vec<u8> {
    raster::encode(raster::ExportFormat::Png, 12, 12, &checker_source()).unwrap()
}

fn embedded() -> AssetOrigin {
    AssetOrigin::Embedded {
        name: "checker.png".into(),
        bytes: png_of_checker(),
    }
}

fn save(doc: &Document, tiles: &MemoryTileSource) -> (Vec<u8>, PsdNotes) {
    let composite = render(doc, tiles);
    psd_from_document(doc, tiles, &composite).unwrap()
}

fn smart_of(doc: &Document, id: LayerId) -> &SmartObjectLayer {
    match &doc.layers.get(id).unwrap().kind {
        LayerKind::SmartObject(o) => o,
        other => panic!("came back as {other:?}, not a smart object"),
    }
}

/// The finding's first case: a smart object with a Gaussian Blur smart
/// filter is saved with the filter as Photoshop's `filterFX` on its `SoLd`
/// and opens with the filter live — the same stack, rendering the same.
#[test]
fn a_smart_object_with_a_gaussian_blur_round_trips_with_the_filter_live() {
    let mut blur = gaussian(2.5);
    blur.opacity = 0.75;
    blur.blend_mode = BlendMode::Multiply;
    let (doc, tiles) = smart_document(embedded(), vec![blur.clone()]);
    let (bytes, notes) = save(&doc, &tiles);
    assert!(notes.is_empty(), "nothing falls back: {notes:?}");

    // The file: a placed layer whose SoLd carries a GsnB filterFX entry.
    let file = psd::read(&bytes).unwrap();
    let record = file.layers.iter().find(|l| l.name == "SO").unwrap();
    assert!(psd::placed::PlacedLayer::is_placed(record));
    let fx = psd::placed::smart_filters::filter_fx_of(record, &psd::ReadOptions::default())
        .expect("the smart filter is in SoLd");
    let Some(psd::Value::List(list)) = fx.get("filterFXList") else {
        panic!("no filterFXList: {fx:?}");
    };
    let Some(psd::Value::Descriptor(entry)) = list.first() else {
        panic!("an empty filterFXList");
    };
    assert_eq!(entry.descriptor("Fltr").unwrap().class_id, "GsnB");
    assert!(file.extra.iter().any(|b| b.key == *b"lnk2"));

    // Opened again: a smart object whose filter is live.
    let back = document_from_psd(&bytes, "back.psd", 10).unwrap();
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    let (bdoc, btiles) = (&back.imported.document, &back.imported.tiles);
    let so = find(bdoc, "SO");
    assert_eq!(smart_of(bdoc, so).filters, vec![blur]);
    let before = render_layer(&doc, &tiles, find(&doc, "SO"));
    let after = render_layer(bdoc, btiles, so);
    assert!(worst(&before, &after) <= 1, "renders the same");

    // Live, not baked: switching the filter off changes the picture back to
    // the unfiltered source.
    let mut off = bdoc.clone();
    if let Some(Layer {
        kind: LayerKind::SmartObject(o),
        ..
    }) = off.layers.get_mut(so)
    {
        o.filters[0].enabled = false;
    }
    let unfiltered = render_layer(&off, btiles, so);
    assert!(
        worst(&after, &unfiltered) > 20,
        "the filter is applied by the compositor, not baked into the source"
    );
}

/// The finding's second case: a linked smart object is saved as a link
/// (`liFE` in `lnkE`, no embedded copy) and opens linked to the same file.
#[test]
fn a_linked_smart_object_round_trips_as_linked() {
    let dir = std::env::temp_dir().join(format!("rs-w16j-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("linked checker.png");
    std::fs::write(&path, png_of_checker()).unwrap();

    let (doc, tiles) = smart_document(
        AssetOrigin::Linked { path: path.clone() },
        vec![gaussian(1.5)],
    );
    let (bytes, notes) = save(&doc, &tiles);
    assert!(notes.is_empty(), "nothing falls back: {notes:?}");
    let file = psd::read(&bytes).unwrap();
    assert!(file.extra.iter().any(|b| b.key == *b"lnkE"));
    assert!(
        !file.extra.iter().any(|b| b.key == *b"lnk2"),
        "no embedded copy"
    );

    let back = document_from_psd(&bytes, "back.psd", 10).unwrap();
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    let (bdoc, btiles) = (&back.imported.document, &back.imported.tiles);
    let so = find(bdoc, "SO");
    let object = smart_of(bdoc, so);
    assert!(object.linked, "still linked");
    assert_eq!(
        bdoc.asset_origin(object.asset),
        Some(&AssetOrigin::Linked { path: path.clone() })
    );
    assert_eq!(object.filters, vec![gaussian(1.5)]);
    let before = render_layer(&doc, &tiles, find(&doc, "SO"));
    let after = render_layer(bdoc, btiles, so);
    assert!(worst(&before, &after) <= 1, "renders the same");

    // The linked file gone: the layer opens as its pixels, and says why.
    std::fs::remove_file(&path).unwrap();
    let gone = document_from_psd(&bytes, "gone.psd", 10).unwrap();
    let told = gone.notes.summary().expect("the loss is reported");
    assert!(
        told.contains("SO") && told.contains("cannot be read"),
        "{told}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A filter Photoshop has no smart filter for keeps the rendered pixels,
/// and the note names the filter.
#[test]
fn a_filter_photoshop_lacks_keeps_the_pixels_and_is_named() {
    let (doc, tiles) = smart_document(
        embedded(),
        vec![SmartFilter::new("LensBlur", BTreeMap::new())],
    );
    let (bytes, notes) = save(&doc, &tiles);
    let told = notes.summary().expect("the fallback is reported");
    assert!(told.contains("SO") && told.contains("Lens Blur"), "{told}");
    let file = psd::read(&bytes).unwrap();
    let record = file.layers.iter().find(|l| l.name == "SO").unwrap();
    assert!(!psd::placed::PlacedLayer::is_placed(record));
    assert!(record.bounds.width() > 0, "its pixels were written");
}

/// A horizontal hue sweep with a vertical value ramp under every
/// adjustment, so each one moves real pixels.
fn sweep_document() -> ImportedDocument {
    let mut rgba8 = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let t = x as f32 / (W - 1) as f32;
            let v = 0.2 + 0.8 * (y as f32 / (H - 1) as f32);
            for c in [(1.0 - t) * v, (1.0 - (2.0 * t - 1.0).abs()) * v, t * v] {
                rgba8.push((c * 255.0).round() as u8);
            }
            rgba8.push(255);
        }
    }
    let image = DecodedImage {
        width: W,
        height: H,
        rgba8,
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
    };
    document_from_image(&image, "sweep", 10).unwrap()
}

/// Every adjustment kind Photoshop has no adjustment layer for.
fn no_payload_kinds() -> Vec<(&'static str, AdjustmentKind)> {
    vec![
        (
            "Auto",
            AdjustmentKind::Auto {
                mode: layer_model::AutoAdjustment::Contrast,
                clip: 0.01,
            },
        ),
        ("Desaturate", AdjustmentKind::Desaturate),
        ("Equalize", AdjustmentKind::Equalize),
        (
            "Shadows/Highlights",
            AdjustmentKind::ShadowsHighlights {
                shadows: [0.6, 0.5, 8.0],
                highlights: [0.3, 0.5, 8.0],
            },
        ),
        (
            "Replace Color",
            AdjustmentKind::ReplaceColor {
                color: [1.0, 0.0, 0.0],
                fuzziness: 0.6,
                hue: 90.0,
                saturation: 0.0,
                lightness: 0.0,
            },
        ),
        (
            "HDR Toning",
            AdjustmentKind::HdrToning {
                radius: 6.0,
                strength: 1.0,
                gamma: 1.2,
                exposure: 0.5,
                detail: 0.5,
                vibrance: 0.2,
                saturation: 0.1,
            },
        ),
        (
            "Match Color",
            AdjustmentKind::MatchColor {
                source_mean: [60.0, 20.0, -10.0],
                source_std: [20.0, 10.0, 10.0],
                target_mean: [50.0, 0.0, 0.0],
                target_std: [25.0, 15.0, 15.0],
                luminance: 1.0,
                color_intensity: 1.0,
                fade: 0.0,
                neutralize: false,
            },
        ),
    ]
}

/// The finding's third case: no adjustment exports as an empty layer. Each
/// kind a `.psd` has no layer for is written as a pixel layer carrying its
/// effect — the reopened document composites like the original — and the
/// note names it.
#[test]
fn no_adjustment_exports_as_an_empty_layer() {
    for (name, kind) in no_payload_kinds() {
        let mut first = sweep_document();
        first
            .document
            .layers
            .push_root(Layer::with_kind(
                name,
                LayerKind::Adjustment(layer_model::AdjustmentLayer { kind: kind.clone() }),
            ))
            .unwrap();
        let (doc, tiles) = (&first.document, &first.tiles);
        let original = render(doc, tiles);
        let (bytes, notes) = save(doc, tiles);
        let told = notes
            .summary()
            .expect("the rasterised adjustment is reported");
        assert!(
            told.contains(name) && told.contains("pixel layer"),
            "{name}: {told}"
        );

        let file = psd::read(&bytes).unwrap();
        let record = file.layers.iter().find(|l| l.name == name).unwrap();
        assert!(
            record.adjustment.is_some()
                || (record.bounds.width() > 0
                    && record.bounds.height() > 0
                    && !record.pixel_data_irrelevant),
            "{name} was written as an empty layer"
        );

        let back = document_from_psd(&bytes, "back.psd", 10).unwrap();
        let (bdoc, btiles) = (&back.imported.document, &back.imported.tiles);
        let reopened = render(bdoc, btiles);
        let d = worst(&original, &reopened);
        assert!(d <= 2, "{name}: the reopened composite differs by {d}");
        // And the adjustment did something here, so "the same" is not "the
        // base alone" — except Auto and Equalize, analyses the compositor
        // renders as a pass-through (it is never given the statistics), so
        // their pixel layer is the layers under them, unchanged.
        let mut hidden = doc.clone();
        hidden.layers.get_mut(find(doc, name)).unwrap().visible = false;
        let base_only = render(&hidden, tiles);
        assert!(
            worst(&base_only, &reopened) > 0 || matches!(name, "Auto" | "Equalize"),
            "{name} changed nothing, so this case proves nothing"
        );
    }
}

/// Writes the W16-J fixtures for an independent reader (psd-tools):
/// `RS_W16J_PSD_DIR=dir cargo test -p app-shell --lib -- --ignored w16j_fixture`
/// writes `filtered.psd` (embedded, Gaussian Blur) and `linked.psd` (a link
/// to `dir/linked.png`).
#[test]
#[ignore]
fn w16j_fixture_for_independent_readers() {
    let Ok(dir) = std::env::var("RS_W16J_PSD_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let (doc, tiles) = smart_document(embedded(), vec![gaussian(2.5)]);
    std::fs::write(dir.join("filtered.psd"), save(&doc, &tiles).0).unwrap();
    let path = dir.join("linked.png");
    std::fs::write(&path, png_of_checker()).unwrap();
    let (doc, tiles) = smart_document(AssetOrigin::Linked { path }, vec![gaussian(1.5)]);
    std::fs::write(dir.join("linked.psd"), save(&doc, &tiles).0).unwrap();
}
