//! W7-B: pattern fills draw — the Pattern Overlay, and a stroke or glow
//! filled with a pattern — from the tile the fill carries.
//!
//! Every document here is linear, so an 8-bit code `v` decodes to exactly
//! `v / 255` and the expected pixels can be written by hand.

use glam::{Affine2, Vec2};
use layer_model::{
    BlendMode, FillStyle, GlowEffect, LayerEffects, PatternFill, PatternOverlayEffect, PatternTile,
    StrokeEffect, StrokePosition,
};
use raster::{PixelRect, TileCoord};

use crate::testkit::{solid_layer, TestDoc};
use crate::{composite_rect, Canvas, CompositeOptions, TileCompositor};

/// A 3x2 tile of six distinct opaque colours, so a period of 3 across and 2
/// down is visible and no two neighbours agree.
fn six() -> PatternTile {
    let px: [[u8; 4]; 6] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
        [0, 255, 255, 255],
        [255, 0, 255, 255],
    ];
    PatternTile::new("Six", 3, 2, px.concat()).unwrap()
}

fn overlay(tile: PatternTile, scale: f32) -> LayerEffects {
    LayerEffects {
        pattern_overlay: Some(PatternOverlayEffect {
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            pattern: PatternFill {
                tile: Some(tile),
                scale,
                ..PatternFill::default()
            },
        }),
        ..LayerEffects::default()
    }
}

/// The tile's straight RGBA8 at `(x, y)` of the tiling, as the linear
/// premultiplied value a composite of it reads (every tile here is opaque).
fn want(tile: &PatternTile, x: i64, y: i64) -> [f32; 4] {
    let p = tile.pixel(x, y);
    [
        f32::from(p[0]) / 255.0,
        f32::from(p[1]) / 255.0,
        f32::from(p[2]) / 255.0,
        f32::from(p[3]) / 255.0,
    ]
}

#[track_caller]
fn close(got: [f32; 4], want: [f32; 4], what: &str) {
    for c in 0..4 {
        assert!(
            (got[c] - want[c]).abs() <= 1e-4,
            "{what}: got {got:?}, want {want:?}"
        );
    }
}

/// A white layer over the whole of a `w`x`h` linear document, styled.
fn white_styled(
    w: u32,
    h: u32,
    effects: LayerEffects,
) -> (
    editor_core::Document,
    crate::MemoryTileSource,
    layer_model::LayerId,
) {
    let mut t = TestDoc::linear(w, h);
    let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
    t.doc.layers.get_mut(id).unwrap().effects = effects;
    let (doc, src) = t.finish();
    (doc, src, id)
}

fn all(doc: &editor_core::Document, src: &crate::MemoryTileSource) -> Canvas {
    composite_rect(
        doc,
        src,
        PixelRect::new(0, 0, doc.width(), doc.height()),
        0,
        CompositeOptions::default(),
    )
    .unwrap()
}

#[test]
fn a_pattern_overlay_at_full_opacity_over_white_composites_the_pattern_tiled() {
    let tile = six();
    let (doc, src, _) = white_styled(16, 16, overlay(tile.clone(), 1.0));
    let out = all(&doc, &src);
    for y in 0..16 {
        for x in 0..16 {
            close(out.get(x, y), want(&tile, x, y), &format!("({x},{y})"));
        }
    }
}

#[test]
fn a_pattern_overlay_with_no_tile_leaves_the_layer_as_it_was() {
    let mut effects = overlay(six(), 1.0);
    effects.pattern_overlay.as_mut().unwrap().pattern.tile = None;
    let (doc, src, _) = white_styled(8, 8, effects);
    let out = all(&doc, &src);
    close(out.get(3, 3), [1.0; 4], "an untiled overlay draws nothing");
}

#[test]
fn scale_two_doubles_the_period() {
    let tile = six();
    let (doc1, src1, _) = white_styled(24, 24, overlay(tile.clone(), 1.0));
    let (doc2, src2, _) = white_styled(24, 24, overlay(tile, 2.0));
    let one = all(&doc1, &src1);
    let two = all(&doc2, &src2);
    // Period at scale 1 is the tile: 3 across, 2 down.
    for y in 0..20 {
        for x in 0..18 {
            close(
                one.get(x, y),
                one.get(x + 3, y + 2),
                "scale 1 repeats every tile",
            );
            // At scale 2 the picture repeats every 6 across and 4 down ...
            close(
                two.get(x, y),
                two.get(x + 6, y + 4),
                "scale 2 repeats every two tiles",
            );
        }
    }
    // ... and not every 3: the period really doubled rather than holding.
    let mut differs = false;
    for y in 0..16 {
        for x in 0..16 {
            let (a, b) = (two.get(x, y), two.get(x + 3, y));
            differs |= (0..4).any(|c| (a[c] - b[c]).abs() > 1e-3);
        }
    }
    assert!(differs, "at scale 2 the 3-pixel shift must not be a period");
}

#[test]
fn changing_the_pattern_invalidates_the_cached_tile() {
    let (doc, src, id) = white_styled(32, 32, overlay(six(), 1.0));
    let mut cache = TileCompositor::with_capacity(8);
    let coord = TileCoord::new(0, 0, 0);
    let first = cache
        .composite_tile(&doc, &src, coord, CompositeOptions::default())
        .unwrap();
    // The same document again hits ...
    let hits = cache.stats().hits;
    cache
        .composite_tile(&doc, &src, coord, CompositeOptions::default())
        .unwrap();
    assert_eq!(cache.stats().hits, hits + 1);

    // ... a different pattern, every other parameter equal, must miss.
    let mut changed = doc.clone();
    let mut bytes = six().rgba8().to_vec();
    bytes[0..4].copy_from_slice(&[10, 20, 30, 255]);
    let other = PatternTile::new("Six", 3, 2, bytes).unwrap();
    changed
        .layers
        .get_mut(id)
        .unwrap()
        .effects
        .pattern_overlay
        .as_mut()
        .unwrap()
        .pattern
        .tile = Some(other.clone());
    let hits = cache.stats().hits;
    let second = cache
        .composite_tile(&changed, &src, coord, CompositeOptions::default())
        .unwrap();
    assert_eq!(
        cache.stats().hits,
        hits,
        "a changed pattern was served stale"
    );
    close(second.get(0, 0), want(&other, 0, 0), "the new pattern drew");
    assert_ne!(first.get(0, 0), second.get(0, 0));
}

#[test]
fn a_region_composites_the_same_pattern_as_the_whole_document() {
    let tile = six();
    let (doc, src, _) = white_styled(40, 40, overlay(tile, 1.5));
    let whole = all(&doc, &src);
    let part = composite_rect(
        &doc,
        &src,
        PixelRect::new(7, 11, 13, 9),
        0,
        CompositeOptions::default(),
    )
    .unwrap();
    for y in 11..20 {
        for x in 7..20 {
            close(part.get(x, y), whole.get(x, y), "region independence");
        }
    }
}

#[test]
fn link_with_layer_anchors_the_pattern_to_the_layer_not_the_document() {
    // A 10x10 white block moved to (5, 4): linked, its top-left pixel shows
    // the tile's (0, 0); unlinked, the document's (5, 4).
    let tile = six();
    let build = |link: bool| {
        let mut t = TestDoc::linear(32, 32);
        let id = t.push_raster("Block");
        t.paint_tile_with(id, TileCoord::new(0, 0, 0), |x, y| {
            if x < 10 && y < 10 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let layer = t.doc.layers.get_mut(id).unwrap();
        layer.transform = Affine2::from_translation(Vec2::new(5.0, 4.0));
        let mut effects = overlay(tile.clone(), 1.0);
        effects
            .pattern_overlay
            .as_mut()
            .unwrap()
            .pattern
            .link_with_layer = link;
        layer.effects = effects;
        t.finish()
    };
    let (doc, src) = build(true);
    let linked = all(&doc, &src);
    let (doc, src) = build(false);
    let unlinked = all(&doc, &src);
    for (dx, dy) in [(0, 0), (1, 0), (2, 1), (4, 3)] {
        close(
            linked.get(5 + dx, 4 + dy),
            want(&tile, dx, dy),
            "linked: the layer's corner is the tile's origin",
        );
        close(
            unlinked.get(5 + dx, 4 + dy),
            want(&tile, 5 + dx, 4 + dy),
            "unlinked: the document's origin is",
        );
    }
    close(
        linked.get(20, 20),
        [0.0; 4],
        "the overlay stays inside the layer",
    );
}

#[test]
fn a_stroke_filled_with_a_pattern_draws_the_pattern_in_its_band() {
    // A 4-pixel outside stroke round an 8x8 block at (8, 8): the band pixels
    // take the pattern's colours, which the old code left empty.
    let tile = six();
    let mut t = TestDoc::linear(32, 32);
    let id = t.push_raster("Block");
    t.paint_tile_with(id, TileCoord::new(0, 0, 0), |x, y| {
        if (8..16).contains(&x) && (8..16).contains(&y) {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    let fill = PatternFill {
        tile: Some(tile.clone()),
        link_with_layer: false,
        ..PatternFill::default()
    };
    t.doc.layers.get_mut(id).unwrap().effects = LayerEffects {
        stroke: Some(StrokeEffect {
            size_px: 4.0,
            position: StrokePosition::Outside,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            fill: FillStyle::Pattern(fill),
            ..StrokeEffect::default()
        }),
        ..LayerEffects::default()
    };
    let (doc, src) = t.finish();
    let out = all(&doc, &src);
    for (x, y) in [(5, 10), (6, 12), (10, 5), (17, 13), (12, 18)] {
        close(
            out.get(x, y),
            want(&tile, x, y),
            &format!("stroke band ({x},{y})"),
        );
    }
    close(
        out.get(12, 12),
        [1.0; 4],
        "the layer's own pixels are untouched",
    );
    close(out.get(1, 1), [0.0; 4], "nothing past the band");
}

#[test]
fn an_outer_glow_filled_with_a_pattern_is_not_empty() {
    let tile = six();
    let mut t = TestDoc::linear(32, 32);
    let id = t.push_raster("Block");
    t.paint_tile_with(id, TileCoord::new(0, 0, 0), |x, y| {
        if (8..16).contains(&x) && (8..16).contains(&y) {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    t.doc.layers.get_mut(id).unwrap().effects = LayerEffects {
        outer_glow: Some(GlowEffect {
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            size_px: 4.0,
            spread: 1.0,
            fill: FillStyle::Pattern(PatternFill {
                tile: Some(tile.clone()),
                link_with_layer: false,
                ..PatternFill::default()
            }),
            ..GlowEffect::default()
        }),
        ..LayerEffects::default()
    };
    let (doc, src) = t.finish();
    let out = all(&doc, &src);
    // Spread 1 makes the glow solid out to its size: just outside the block
    // it is the pattern, at full strength.
    close(out.get(6, 12), want(&tile, 6, 12), "glow pixel");
    close(out.get(12, 17), want(&tile, 12, 17), "glow pixel");
}
