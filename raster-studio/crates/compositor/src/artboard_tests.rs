//! W8-C: an artboard clips its children to its rect (Photopea), in both group
//! modes, and through the tile cache.

use glam::{Affine2, Vec2};
use layer_model::{Artboard, GroupBlending, Layer, LayerKind, RasterLayer};
use raster::PixelRect;

use crate::composite::{composite_region, CompositeOptions};
use crate::testkit::TestDoc;

const BOARD: Artboard = Artboard {
    x: 8,
    y: 4,
    width: 16,
    height: 12,
    background: [1.0, 1.0, 1.0, 1.0],
};

/// A 32x24 document: an artboard group over BOARD whose plate is untouched
/// (transparent) and whose child is solid red over the whole canvas.
fn artboard_doc(blending: GroupBlending) -> (editor_core::Document, crate::MemoryTileSource) {
    let mut t = TestDoc::linear(32, 24);
    let group = t.push_group("Artboard");
    t.set_group_blending(group, blending);
    t.push_child(
        group,
        Layer::with_kind(
            "Artboard Background",
            LayerKind::Raster(RasterLayer {
                artboard: Some(BOARD),
                ..RasterLayer::default()
            }),
        ),
    );
    let child = t.push_child(group, Layer::raster("Red"));
    t.fill(child, [255, 0, 0, 255]);
    t.finish()
}

fn alpha_at(c: &crate::Canvas, x: i64, y: i64) -> f32 {
    c.get(x, y)[3]
}

#[test]
fn an_artboards_children_are_clipped_to_its_rect_in_both_group_modes() {
    for blending in [GroupBlending::PassThrough, GroupBlending::Isolated] {
        let (doc, src) = artboard_doc(blending);
        let out = composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 32, 24),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        // Inside the rect: the red child.
        assert!(alpha_at(&out, 8, 4) > 0.99, "{blending:?}");
        assert!(alpha_at(&out, 23, 15) > 0.99, "{blending:?}");
        // One pixel outside each edge: nothing.
        for (x, y) in [(7, 8), (24, 8), (12, 3), (12, 16), (0, 0), (31, 23)] {
            assert_eq!(alpha_at(&out, x, y), 0.0, "{blending:?} ({x},{y})");
        }
    }
}

#[test]
fn a_plain_group_is_not_clipped_and_the_clip_follows_a_moved_artboard() {
    // A group with no plate is not an artboard: its child fills the canvas.
    let mut t = TestDoc::linear(32, 24);
    let group = t.push_group("Group");
    let child = t.push_child(group, Layer::raster("Red"));
    t.fill(child, [255, 0, 0, 255]);
    let (doc, src) = t.finish();
    let out = composite_region(
        &doc,
        &src,
        PixelRect::new(0, 0, 32, 24),
        0,
        CompositeOptions::default(),
    )
    .unwrap();
    assert!(alpha_at(&out, 0, 0) > 0.99);

    // Moving the artboard group moves its clip: the rect is read in the
    // group's own space.
    let (mut doc, src) = artboard_doc(GroupBlending::Isolated);
    let group = doc.layers.root()[0];
    doc.layers.get_mut(group).unwrap().transform = Affine2::from_translation(Vec2::new(4.0, 0.0));
    let out = composite_region(
        &doc,
        &src,
        PixelRect::new(0, 0, 32, 24),
        0,
        CompositeOptions::default(),
    )
    .unwrap();
    assert_eq!(alpha_at(&out, 8, 8), 0.0, "the old left edge is outside");
    assert!(alpha_at(&out, 12, 8) > 0.99);
    assert!(alpha_at(&out, 27, 8) > 0.99);
    assert_eq!(alpha_at(&out, 28, 8), 0.0);
}

#[test]
fn the_clip_holds_through_the_tile_cache() {
    let (doc, src) = artboard_doc(GroupBlending::PassThrough);
    let mut tc = crate::TileCompositor::new();
    let cached = tc
        .composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 32, 24),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
    assert_eq!(alpha_at(&cached, 0, 0), 0.0);
    assert!(alpha_at(&cached, 10, 10) > 0.99);
}
