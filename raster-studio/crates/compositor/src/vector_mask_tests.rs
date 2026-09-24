//! W9-G: vector masks reach the composite.
//!
//! Every layer here is opaque linear white, so a composited pixel's alpha *is*
//! the mask's resolved coverage and the expected numbers come straight from
//! the definition: 1 inside the path, 0 outside, density lerping the outside
//! toward 1, and the pixel mask multiplying on top.

use editor_core::Document;
use glam::{Affine2, Vec2};
use layer_model::{LayerId, LayerMask, MaskId, VectorMask};
use raster::{PixelRect, TileCoord};

use crate::canvas::Canvas;
use crate::composite::{composite_rect, CompositeOptions};
use crate::source::MemoryTileSource;
use crate::testkit::{solid_layer, TestDoc};
use crate::TileCompositor;

/// The upper-left half of a 16×16 canvas, split along the anti-diagonal.
const TRIANGLE: &str = "M0 0 L16 0 L0 16 Z";

fn full(doc: &Document, src: &MemoryTileSource) -> Canvas {
    let r = PixelRect::new(0, 0, doc.width(), doc.height());
    composite_rect(doc, src, r, 0, CompositeOptions::default()).expect("composite")
}

fn alpha(c: &Canvas, x: usize, y: usize) -> f32 {
    c.pixels()[y * c.width() as usize + x][3]
}

fn white_with_vector(v: VectorMask) -> (TestDoc, LayerId) {
    let mut t = TestDoc::linear(16, 16);
    let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
    t.doc
        .layers
        .get_mut(id)
        .unwrap()
        .set_mask(LayerMask::vector_only(MaskId::new(), v));
    (t, id)
}

#[test]
fn a_triangular_vector_mask_hides_outside_the_triangle() {
    let (t, _) = white_with_vector(VectorMask::new(TRIANGLE));
    let (doc, src) = t.finish();
    let c = full(&doc, &src);
    assert_eq!(alpha(&c, 2, 2), 1.0, "well inside the triangle");
    assert_eq!(alpha(&c, 13, 13), 0.0, "well outside the triangle");
    assert_eq!(
        alpha(&c, 7, 7),
        1.0,
        "the last whole pixel before the diagonal"
    );
    // Anti-aliased: a pixel the diagonal cuts through is partly covered.
    let edge = alpha(&c, 7, 8);
    assert!(
        edge > 0.0 && edge < 1.0,
        "the edge pixel is partial: {edge}"
    );
}

#[test]
fn vector_mask_density_fifty_percent_halves_the_hiding() {
    let mut v = VectorMask::new(TRIANGLE);
    v.set_density(0.5).unwrap();
    let (t, _) = white_with_vector(v);
    let (doc, src) = t.finish();
    let c = full(&doc, &src);
    assert!(
        (alpha(&c, 13, 13) - 0.5).abs() < 1e-6,
        "{}",
        alpha(&c, 13, 13)
    );
    assert_eq!(alpha(&c, 2, 2), 1.0);
}

#[test]
fn inverted_disabled_and_empty_vector_masks() {
    let mut v = VectorMask::new(TRIANGLE);
    v.inverted = true;
    let (t, _) = white_with_vector(v);
    let (doc, src) = t.finish();
    let c = full(&doc, &src);
    assert_eq!((alpha(&c, 2, 2), alpha(&c, 13, 13)), (0.0, 1.0), "inverted");

    let mut v = VectorMask::new(TRIANGLE);
    v.enabled = false;
    let (t, _) = white_with_vector(v);
    let (doc, src) = t.finish();
    assert_eq!(
        alpha(&full(&doc, &src), 13, 13),
        1.0,
        "disabled hides nothing"
    );

    let (t, _) = white_with_vector(VectorMask::hide_all());
    let (doc, src) = t.finish();
    assert!(
        full(&doc, &src).pixels().iter().all(|p| p[3] == 0.0),
        "hide all"
    );

    let (t, _) = white_with_vector(VectorMask::reveal_all());
    let (doc, src) = t.finish();
    assert!(
        full(&doc, &src).pixels().iter().all(|p| p[3] == 1.0),
        "reveal all"
    );
}

#[test]
fn a_vector_mask_feather_softens_its_edge() {
    let hard = {
        let (t, _) = white_with_vector(VectorMask::new("M0 0 L8 0 L8 16 L0 16 Z"));
        let (doc, src) = t.finish();
        full(&doc, &src)
    };
    let mut v = VectorMask::new("M0 0 L8 0 L8 16 L0 16 Z");
    v.set_feather_px(4.0).unwrap();
    let (t, _) = white_with_vector(v);
    let (doc, src) = t.finish();
    let soft = full(&doc, &src);
    assert_eq!((alpha(&hard, 7, 8), alpha(&hard, 8, 8)), (1.0, 0.0));
    let (inside, outside) = (alpha(&soft, 7, 8), alpha(&soft, 8, 8));
    assert!(
        inside < 1.0 && inside > 0.5,
        "inside edge softened: {inside}"
    );
    assert!(
        outside > 0.0 && outside < 0.5,
        "outside edge softened: {outside}"
    );
}

#[test]
fn the_vector_mask_multiplies_with_the_pixel_mask() {
    let mut t = TestDoc::linear(16, 16);
    let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
    let mask = t.attach_mask(id);
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 128);
    t.doc
        .layers
        .get_mut(id)
        .unwrap()
        .mask
        .as_mut()
        .unwrap()
        .vector = Some(Box::new(VectorMask::new(TRIANGLE)));
    let (doc, src) = t.finish();
    let c = full(&doc, &src);
    let k = 128.0 / 255.0;
    assert!(
        (alpha(&c, 2, 2) - k).abs() < 1e-6,
        "inside: pixel mask alone"
    );
    assert_eq!(alpha(&c, 13, 13), 0.0, "outside: the vector mask hides");
}

#[test]
fn a_vector_mask_rides_the_layer_transform() {
    let (mut t, id) = white_with_vector(VectorMask::new("M0 0 L4 0 L4 4 L0 4 Z"));
    // The layer's own pixels cover the canvas already; move it by (8, 8) and
    // fill the vacated corner too, so only the mask decides what shows.
    t.doc.layers.get_mut(id).unwrap().transform = Affine2::from_translation(Vec2::new(8.0, 8.0));
    for (x, y) in [(-1, -1), (-1, 0), (0, -1)] {
        t.paint_tile(id, TileCoord::new(x, y, 0), [255, 255, 255, 255]);
    }
    let (doc, src) = t.finish();
    let c = full(&doc, &src);
    assert_eq!(
        alpha(&c, 1, 1),
        0.0,
        "the path's layer-space origin moved away"
    );
    assert_eq!(alpha(&c, 9, 9), 1.0, "and landed at (8, 8)");
}

#[test]
fn editing_the_vector_mask_re_keys_cached_tiles() {
    let (t, id) = white_with_vector(VectorMask::new(TRIANGLE));
    let (mut doc, src) = t.finish();
    let mut tc = TileCompositor::new();
    let region = PixelRect::new(0, 0, 16, 16);
    let opts = CompositeOptions::default();
    let before = tc.composite_region(&doc, &src, region, 0, opts).unwrap();
    doc.layers
        .get_mut(id)
        .unwrap()
        .mask
        .as_mut()
        .unwrap()
        .vector = Some(Box::new(VectorMask::reveal_all()));
    let after = tc.composite_region(&doc, &src, region, 0, opts).unwrap();
    assert_eq!(alpha(&before, 13, 13), 0.0);
    assert_eq!(
        alpha(&after, 13, 13),
        1.0,
        "a stale cached tile would still hide"
    );
}
