//! W15-C: a warped text layer leaves a document with its warp in the `TySh`
//! warp descriptor (every Warp Text style and a Custom mesh) and comes back
//! with the same warp and the same outline, through the real export and
//! import routes (`psd_from_document` / `document_from_psd`) and the real
//! text engine.

use super::*;
use layer_model::text::{TextLayer, TextWarp, WarpStyle};

/// The fixture face in both libraries the anchor and the render use.
fn dejavu() -> &'static str {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let bytes = dejavu::sans::regular().to_vec();
        compositor::load_font(bytes.clone());
        text_engine::register_session_font(bytes);
    });
    "DejaVu Sans"
}

fn warped(warp: TextWarp) -> TextLayer {
    TextLayer {
        text: "Warp me".into(),
        font_family: dejavu().into(),
        size_px: 32.0,
        warp,
        ..TextLayer::default()
    }
}

/// A dragged Custom mesh: every handle off its flat position.
fn dragged_mesh() -> [[f32; 2]; 16] {
    let mut m = [[0.0f32; 2]; 16];
    for (i, p) in m.iter_mut().enumerate() {
        let (c, r) = ((i % 4) as f32, (i / 4) as f32);
        *p = [c / 3.0 + 0.05 * r, r / 3.0 - 0.3 * (c - 1.5).abs() + 0.2];
    }
    m
}

/// Export `text` (placed at 14, 30) and import it again: the layer before
/// and after.
fn round_trip(text: TextLayer) -> (Layer, Layer) {
    let mut doc = Document::new(220, 120, "w15c");
    let tiles = MemoryTileSource::new();
    let mut layer = Layer::with_kind("Warped", LayerKind::Text(text));
    layer.transform = glam::Affine2::from_translation(glam::Vec2::new(14.0, 30.0));
    doc.layers.push_root(layer.clone()).unwrap();
    let composite = vec![0u8; 220 * 120 * 4];
    let (bytes, _) = psd_from_document(&doc, &tiles, &composite).unwrap();
    let import = document_from_psd(&bytes, "w15c.psd", 10).unwrap();
    let back = &import.imported.document;
    let id = back
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| back.layers.get(*id).is_some_and(|l| l.name == "Warped"))
        .expect("the layer imported");
    (layer, back.layers.get(id).unwrap().clone())
}

fn text_of(layer: &Layer) -> &TextLayer {
    match &layer.kind {
        LayerKind::Text(t) => t,
        other => panic!("imported as {other:?}, not text"),
    }
}

/// Every outline point the layer renders, in document space.
fn outline(layer: &Layer) -> Vec<glam::Vec2> {
    let run = text_engine::TextRun::from(text_of(layer));
    let svg = text_engine::with_shared_library(|library| {
        let shaped = text_engine::shape(library, &run);
        text_engine::outline_svg(library, &shaped)
    });
    let numbers: Vec<f32> = svg
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().expect("a number"))
        .collect();
    assert!(numbers.len() > 40, "the text has outlines: {svg}");
    let (pairs, rest) = numbers.as_chunks::<2>();
    assert!(rest.is_empty(), "coordinates come in pairs");
    pairs
        .iter()
        .map(|&[x, y]| layer.transform.transform_point2(glam::Vec2::new(x, y)))
        .collect()
}

fn max_distance(a: &[glam::Vec2], b: &[glam::Vec2]) -> f32 {
    assert_eq!(a.len(), b.len(), "the same outline shape");
    a.iter()
        .zip(b)
        .map(|(p, q)| p.distance(*q))
        .fold(0.0, f32::max)
}

fn assert_warp_close(a: &TextWarp, b: &TextWarp) {
    assert_eq!(a.style, b.style);
    for (x, y) in [
        (a.bend, b.bend),
        (a.horizontal, b.horizontal),
        (a.vertical, b.vertical),
    ] {
        assert!((x - y).abs() < 1e-5, "{a:?} vs {b:?}");
    }
    match (a.mesh, b.mesh) {
        (None, None) => {}
        (Some(m), Some(n)) => {
            for (p, q) in m.iter().zip(n) {
                assert!(
                    (p[0] - q[0]).abs() < 1e-5 && (p[1] - q[1]).abs() < 1e-5,
                    "{m:?} vs {n:?}"
                );
            }
        }
        other => panic!("mesh lost: {other:?}"),
    }
}

/// W15-C: every Warp Text style and a dragged Custom mesh survive export
/// and import with equal parameters, and the re-imported layer renders the
/// same warped outline within 1 px — which differs from the unbent outline
/// by more than that, so a lost warp cannot pass.
#[test]
fn every_warp_style_and_a_custom_mesh_survive_export_and_import() {
    let styles = WarpStyle::ALL.into_iter().chain([WarpStyle::Custom]);
    for style in styles {
        let warp = TextWarp {
            style,
            bend: 0.6,
            horizontal: -0.2,
            vertical: 0.15,
            mesh: (style == WarpStyle::Custom).then(dragged_mesh),
        };
        let (before, after) = round_trip(warped(warp));
        assert_warp_close(&text_of(&after).warp, &warp);
        let (a, b) = (outline(&before), outline(&after));
        let d = max_distance(&a, &b);
        assert!(d <= 1.0, "{style:?}: the outline moved {d} px");

        // Anti-vacuity: the same style at zero strength (a flat Custom
        // mesh) draws an outline more than 1 px away from the warped one.
        let mut flat = before.clone();
        if let LayerKind::Text(t) = &mut flat.kind {
            t.warp = TextWarp {
                style,
                bend: 0.0,
                horizontal: 0.0,
                vertical: 0.0,
                mesh: None,
            };
        }
        let apart = max_distance(&a, &outline(&flat));
        assert!(apart > 1.0, "{style:?} bends the outline ({apart} px)");
    }
}

/// An unwarped layer still writes (and reads back) no warp.
#[test]
fn an_unwarped_layer_comes_back_unwarped() {
    let (_, after) = round_trip(warped(TextWarp::default()));
    assert!(text_of(&after).warp.is_default());
}

/// A Photoshop Shell warp (which the layer model lacks) imports as its Arc
/// counterpart, and the report names it rather than dropping it silently.
#[test]
fn a_shell_warp_imports_as_arc_and_is_reported() {
    let engine = psd::engine_data::from_text_layer(&warped(TextWarp::default()));
    let spec = psd::text::WarpSpec {
        kind: psd::text::WarpKind::ShellLower,
        value: 40.0,
        ..psd::text::WarpSpec::NONE
    };
    let transform = [1.0, 0.0, 0.0, 1.0, 5.0, 40.0];
    let raw = psd::text::build_styled_warped(&engine, transform, (0, 0, 10, 10), &spec);
    let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(64, 48));
    let mut layer = psd::PsdLayer::raster("Shell", psd::Rect::default());
    layer.text = Some(psd::TextData {
        transform,
        text: Some("Warp me".into()),
        raw,
    });
    file.layers.push(layer);
    let import = document_from_psd(&psd::write(&file).unwrap(), "s.psd", 10).unwrap();
    let doc = &import.imported.document;
    let id = doc.layers.iter_depth_first()[0];
    let warp = text_of(doc.layers.get(id).unwrap()).warp;
    assert_eq!(warp.style, WarpStyle::ArcLower);
    assert!((warp.bend - 0.4).abs() < 1e-6, "{warp:?}");
    let notes = import.notes.notes().join("\n");
    assert!(
        notes.contains("the Shell Lower warp (imported as Arc Lower)"),
        "{notes}"
    );
}
