//! W18-J: every filter with a Photoshop equivalent saves as a live smart
//! filter and opens with equal parameters (through `psd_from_document` and
//! `document_from_psd`), the rest are named with their reason, and a
//! smart-filter mask round-trips through the document's `FEid` block on
//! the same routes.

use std::collections::BTreeMap;

use super::*;
use layer_model::{SmartFilter, SmartParam};
use psd::placed::smart_filters as sf;

const W: u32 = 40;
const H: u32 = 24;

fn find(doc: &Document, name: &str) -> LayerId {
    doc.layers
        .iter_depth_first()
        .into_iter()
        .find(|id| doc.layers.get(*id).is_some_and(|l| l.name == name))
        .unwrap_or_else(|| panic!("no layer called {name}"))
}

fn source() -> Vec<u8> {
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

/// A document holding one embedded smart object `SO` over a 12x12 checker,
/// at `transform`, with `filters` and `filter_mask`.
fn smart_document(
    filters: Vec<SmartFilter>,
    filter_mask: Option<layer_model::LayerMask>,
    transform: glam::Affine2,
) -> (Document, MemoryTileSource, LayerId) {
    let mut doc = Document::new(W, H, "so");
    let mut tiles = MemoryTileSource::new();
    let asset = AssetId::new();
    doc.set_asset_origin(AssetRecord {
        id: asset,
        origin: AssetOrigin::Embedded {
            name: "checker.png".into(),
            bytes: raster::encode(raster::ExportFormat::Png, 12, 12, &source()).unwrap(),
        },
        source_size: Some((12, 12)),
    });
    let so = doc
        .layers
        .push_root(Layer::with_kind(
            "SO",
            LayerKind::SmartObject(SmartObjectLayer {
                asset,
                linked: false,
                filters,
                filter_mask,
            }),
        ))
        .unwrap();
    doc.layers.get_mut(so).unwrap().transform = transform;
    let edits = tile_edits_for_rgba(&source(), psd::Rect::sized(12, 12), &mut tiles);
    doc.pixels
        .apply(PixelKey::Layer(so), &TileDelta::new(edits).unwrap());
    (doc, tiles, so)
}

fn save(doc: &Document, tiles: &MemoryTileSource) -> (Vec<u8>, PsdNotes) {
    let composite = vec![0u8; (doc.width() * doc.height() * 4) as usize];
    psd_from_document(doc, tiles, &composite).unwrap()
}

fn smart_of(doc: &Document, id: LayerId) -> &SmartObjectLayer {
    match &doc.layers.get(id).unwrap().kind {
        LayerKind::SmartObject(o) => o,
        other => panic!("came back as {other:?}, not a smart object"),
    }
}

fn key_of(id: ui::menu::FilterId) -> String {
    format!("{id:?}")
}

/// The smart filter the Filter dialog for `id` stores at its defaults.
fn at_defaults(id: ui::menu::FilterId) -> SmartFilter {
    use ui::dialogs::ParamValue as P;
    let spec = ui::dialogs::filter_by_id(id).expect("every filter has a dialog");
    let values = ui::dialogs::FilterParams::defaults(spec.params);
    let params: BTreeMap<String, SmartParam> = spec
        .params
        .iter()
        .filter_map(|o| {
            let v = match values.get(o.key)? {
                P::Float(v) => SmartParam::Float(v),
                P::Int(v) => SmartParam::Int(v),
                P::Bool(v) => SmartParam::Bool(v),
                P::Choice(v) => SmartParam::Choice(v as u32),
                P::Color(c) => SmartParam::Color(c),
            };
            Some((o.key.to_string(), v))
        })
        .collect();
    SmartFilter::new(key_of(id), params)
}

/// Every Filter-menu entry is either written as a Photoshop smart filter or
/// named, with its reason, as written as pixels — never both, never neither.
#[test]
fn every_filter_is_mapped_or_named_rasterised_with_a_reason() {
    for &id in ui::menu::FilterId::ALL {
        let key = key_of(id);
        let mapped = sf::has_photoshop_equivalent(&key);
        let reason = sf::rasterised_reason(&key);
        assert!(
            mapped != reason.is_some(),
            "{key}: mapped {mapped}, {reason:?}"
        );
    }
    for (key, _) in sf::MAPPED.iter().chain(sf::RASTERISED) {
        assert!(
            ui::menu::FilterId::ALL.iter().any(|id| key_of(*id) == *key),
            "{key} is not a filter of this build"
        );
    }
}

/// W18-J's round trip through the real routes: a smart object carrying
/// every mapped filter at its dialog defaults (and Wrap / Mirror edges on
/// three of them) saves as a placed layer whose filters are all live, and
/// opens with the same stack, parameter for parameter.
#[test]
fn every_mapped_filter_saves_live_and_reopens_with_equal_parameters() {
    crate::menu_bridge::install_smart_filter_runner();
    let mut stack: Vec<SmartFilter> = ui::menu::FilterId::ALL
        .iter()
        .copied()
        .filter(|id| sf::has_photoshop_equivalent(&key_of(*id)))
        .map(at_defaults)
        .collect();
    assert_eq!(stack.len(), sf::MAPPED.len());
    for f in &mut stack {
        let edge = match f.filter.as_str() {
            "GaussianBlur" => 2,
            "Wave" => 1,
            "Offset" => 2,
            _ => continue,
        };
        f.params
            .insert("edge".to_string(), SmartParam::Choice(edge));
    }
    stack[3].opacity = 0.5;
    stack[3].blend_mode = layer_model::BlendMode::Screen;
    stack[5].enabled = false;
    let (doc, tiles, _) = smart_document(
        stack.clone(),
        None,
        glam::Affine2::from_translation(glam::vec2(6.0, 0.0)),
    );
    let (bytes, notes) = save(&doc, &tiles);
    assert!(
        notes.summary().is_none_or(|s| !s.contains("SO")),
        "the object is not a fallback: {:?}",
        notes.summary()
    );
    let file = psd::read(&bytes).unwrap();
    let record = file.layers.iter().find(|l| l.name == "SO").unwrap();
    assert!(psd::placed::PlacedLayer::is_placed(record));
    let back = document_from_psd(&bytes, "back.psd", 10).unwrap();
    let bdoc = &back.imported.document;
    assert_eq!(smart_of(bdoc, find(bdoc, "SO")).filters, stack);
}

fn gradient_mask(tiles: &mut MemoryTileSource, doc: &mut Document) -> layer_model::LayerMask {
    let mask = layer_model::LayerMask::new(MaskId::new());
    let coverage: Vec<u8> = (0..144u32).map(|i| ((i % 12) * 23) as u8).collect();
    let edits = tile_edits_for_coverage(
        Some(&coverage),
        psd::Rect::sized(12, 12),
        0,
        &DocRect::from_psd(psd::Rect::sized(12, 12)).tiles(),
        tiles,
    );
    doc.pixels
        .apply(PixelKey::Mask(mask.id), &TileDelta::new(edits).unwrap());
    mask
}

fn mask_coverage(doc: &Document, tiles: &MemoryTileSource, mask: MaskId) -> Vec<u8> {
    let map = doc
        .pixels
        .tiles(PixelKey::Mask(mask))
        .expect("the mask has tiles");
    coverage_from_tiles(map, tiles, DocRect::from_psd(psd::Rect::sized(12, 12)))
}

/// Save as PSD writes a masked smart object live: the mask's settings in
/// its `filterFXStyle`, its pixels (in canvas pixels, at the object's
/// translation) in the document's `FEid` block — not the raster fallback.
/// File > Open attaches the mask again: pixels, switch, link, invert,
/// density and feather. Both halves are the real routes
/// (`psd_from_document`, `document_from_psd`).
#[test]
fn a_smart_filter_mask_round_trips_through_feid() {
    for (inverted, density, feather, enabled) in [(false, 1.0, 0.0, true), (true, 0.5, 3.0, false)]
    {
        let (mut doc, mut tiles, so) = smart_document(
            vec![at_defaults(ui::menu::FilterId::GaussianBlur)],
            None,
            glam::Affine2::from_translation(glam::vec2(6.0, 0.0)),
        );
        let mut mask = gradient_mask(&mut tiles, &mut doc);
        mask.inverted = inverted;
        mask.enabled = enabled;
        mask.set_density(density).unwrap();
        mask.set_feather_px(feather).unwrap();
        if let LayerKind::SmartObject(o) = &mut doc.layers.get_mut(so).unwrap().kind {
            o.filter_mask = Some(mask.clone());
        }
        let (bytes, notes) = save(&doc, &tiles);
        assert!(
            notes.summary().is_none_or(|s| !s.contains("SO")),
            "the masked object is not a fallback: {:?}",
            notes.summary()
        );
        let file = psd::read(&bytes).unwrap();
        let record = file.layers.iter().find(|l| l.name == "SO").unwrap();
        assert!(psd::placed::PlacedLayer::is_placed(record));
        let entries = sf::filter_effects_of(&file.extra);
        assert_eq!(entries.len(), 1);
        let written = entries[0].mask.as_ref().expect("the mask is written");
        // Canvas pixels: the object's (6, 0) translation.
        assert_eq!((written.rect.left, written.rect.top), (6, 0));

        let back = document_from_psd(&bytes, "back.psd", 10).unwrap();
        let (bdoc, btiles) = (&back.imported.document, &back.imported.tiles);
        let id = find(bdoc, "SO");
        let got = smart_of(bdoc, id)
            .filter_mask
            .clone()
            .expect("File > Open attached the mask");
        assert_eq!(got.inverted, inverted);
        assert_eq!(got.enabled, enabled);
        assert_eq!(got.linked, mask.linked);
        assert!((got.density() - density).abs() < 1.0 / 255.0);
        assert_eq!(got.feather_px(), feather);
        assert_eq!(
            mask_coverage(bdoc, btiles, got.id),
            mask_coverage(&doc, &tiles, mask.id)
        );
        assert_eq!(smart_of(bdoc, id).filters, smart_of(&doc, so).filters);
    }
}

/// A `.psd` names a filter mask by the file the object places: a masked
/// object whose file a second smart object also places keeps its pixels,
/// named, so the second object never opens wearing its mask.
#[test]
fn a_filter_mask_on_a_shared_file_is_refused_by_name() {
    let (mut doc, mut tiles, so) = smart_document(
        vec![at_defaults(ui::menu::FilterId::GaussianBlur)],
        None,
        glam::Affine2::IDENTITY,
    );
    let mask = gradient_mask(&mut tiles, &mut doc);
    let twin = Layer::with_kind("Twin", doc.layers.get(so).unwrap().kind.clone());
    if let LayerKind::SmartObject(o) = &mut doc.layers.get_mut(so).unwrap().kind {
        o.filter_mask = Some(mask);
    }
    let twin = doc.layers.push_root(twin).unwrap();
    let edits = tile_edits_for_rgba(&source(), psd::Rect::sized(12, 12), &mut tiles);
    doc.pixels
        .apply(PixelKey::Layer(twin), &TileDelta::new(edits).unwrap());
    let (bytes, notes) = save(&doc, &tiles);
    let told = notes.summary().expect("the fallback is reported");
    assert!(told.contains("another smart object also"), "{told}");
    let back = document_from_psd(&bytes, "back.psd", 10).unwrap();
    let bdoc = &back.imported.document;
    assert!(smart_of(bdoc, find(bdoc, "Twin")).filter_mask.is_none());
}

/// A filter mask under a scale cannot be put in canvas pixels: the object
/// keeps its pixels, named with the reason.
#[test]
fn a_filter_mask_under_a_scale_is_refused_by_name() {
    let (mut doc, mut tiles, so) = smart_document(
        vec![at_defaults(ui::menu::FilterId::GaussianBlur)],
        None,
        glam::Affine2::from_scale(glam::Vec2::new(2.0, 2.0)),
    );
    let mask = gradient_mask(&mut tiles, &mut doc);
    if let LayerKind::SmartObject(o) = &mut doc.layers.get_mut(so).unwrap().kind {
        o.filter_mask = Some(mask);
    }
    let layer = doc.layers.get(so).unwrap();
    let LayerKind::SmartObject(object) = &layer.kind else {
        unreachable!()
    };
    let mut extras = PsdExportExtras::default();
    let why = smart_blocks(&doc, object, layer.transform, &mut extras).unwrap_err();
    assert!(why.contains("rotated, scaled"), "{why}");
}
