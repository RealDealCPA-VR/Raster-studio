//! W9-H: layer-style fidelity through the real composite — exterior effect
//! modes against the backdrop, contours, Blend If and repeated effects.
//!
//! Every document here is linear, so an 8-bit code `v` decodes to exactly
//! `v / 255` and Blend If's encoded values are the linear ones.

use layer_model::effects::{Contour, ContourPreset, ShadowInstance};
use layer_model::{BlendMode, FillStyle, GlowEffect, LayerId, ShadowEffect};
use raster::{PixelRect, TileCoord};

use crate::testkit::{solid_layer, TestDoc};
use crate::{composite_rect, Canvas, CompositeOptions};

fn full(t: TestDoc) -> Canvas {
    let (doc, src) = t.finish();
    composite_rect(
        &doc,
        &src,
        PixelRect::new(0, 0, doc.width(), doc.height()),
        0,
        CompositeOptions::default(),
    )
    .expect("composite")
}

#[track_caller]
fn close(got: [f32; 4], want: [f32; 4], tol: f32, what: &str) {
    for c in 0..4 {
        assert!(
            (got[c] - want[c]).abs() <= tol,
            "{what}: channel {c} was {} want {} ({got:?} vs {want:?})",
            got[c],
            want[c]
        );
    }
}

/// A 16x16 opaque white square at (16, 16) on a 64x64 document.
fn square(t: &mut TestDoc) -> LayerId {
    let id = t.push_raster("Block");
    t.paint_tile_with(id, TileCoord::new(0, 0, 0), |x, y| {
        if (16..32).contains(&x) && (16..32).contains(&y) {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    id
}

/// A crisp shadow thrown `distance` pixels along the light at `angle`.
fn shadow(mode: BlendMode, color: [f32; 4], angle: f32, distance: f32) -> ShadowEffect {
    ShadowEffect {
        blend_mode: mode,
        color,
        opacity: 1.0,
        angle_deg: angle,
        use_global_light: false,
        distance_px: distance,
        spread: 0.0,
        size_px: 0.0,
        noise: 0.0,
        knockout: true,
    }
}

/// The square over a full-canvas backdrop of `backdrop`, with `shadow` on it.
fn shadowed(backdrop: [u8; 4], s: ShadowEffect) -> Canvas {
    let mut t = TestDoc::linear(64, 64);
    solid_layer(&mut t, "Backdrop", backdrop);
    let id = square(&mut t);
    t.doc.layers.get_mut(id).unwrap().effects.drop_shadow = Some(s);
    full(t)
}

// Light at 180 degrees throws the shadow 8 px to the right: (36, 24) is in
// the shadow and clear of the square.
const IN_SHADOW: (i64, i64) = (36, 24);

#[test]
fn a_multiply_black_shadow_darkens_a_white_backdrop_and_vanishes_on_black() {
    let black = [0.0, 0.0, 0.0, 1.0];
    let on_white = shadowed(
        [255, 255, 255, 255],
        shadow(BlendMode::Multiply, black, 180.0, 8.0),
    );
    close(
        on_white.get(IN_SHADOW.0, IN_SHADOW.1),
        [0.0, 0.0, 0.0, 1.0],
        1e-5,
        "black multiplied into white",
    );
    let on_black = shadowed(
        [0, 0, 0, 255],
        shadow(BlendMode::Multiply, black, 180.0, 8.0),
    );
    close(
        on_black.get(IN_SHADOW.0, IN_SHADOW.1),
        [0.0, 0.0, 0.0, 1.0],
        1e-5,
        "black multiplied into black is black: the shadow cannot be seen",
    );
}

#[test]
fn a_shadows_blend_mode_acts_on_the_backdrop_not_on_an_empty_buffer() {
    // Multiply by red over mid grey keeps only the red of the grey. Drawn into
    // an empty buffer first (the old model) it painted plain red instead.
    let grey = shadowed(
        [128, 128, 128, 255],
        shadow(BlendMode::Multiply, [1.0, 0.0, 0.0, 1.0], 180.0, 8.0),
    );
    let g = 128.0 / 255.0;
    close(
        grey.get(IN_SHADOW.0, IN_SHADOW.1),
        [g, 0.0, 0.0, 1.0],
        1e-5,
        "red multiplied into grey",
    );
    // Multiply by white is the identity: a white Multiply shadow is
    // invisible on any backdrop.
    let white = shadowed(
        [128, 64, 32, 255],
        shadow(BlendMode::Multiply, [1.0, 1.0, 1.0, 1.0], 180.0, 8.0),
    );
    close(
        white.get(IN_SHADOW.0, IN_SHADOW.1),
        [128.0 / 255.0, 64.0 / 255.0, 32.0 / 255.0, 1.0],
        1e-5,
        "white multiplied into the backdrop changes nothing",
    );
    // And a Screen outer glow on white is invisible too.
    let mut t = TestDoc::linear(64, 64);
    solid_layer(&mut t, "Backdrop", [255, 255, 255, 255]);
    let id = square(&mut t);
    t.doc.layers.get_mut(id).unwrap().effects.outer_glow = Some(GlowEffect {
        blend_mode: BlendMode::Screen,
        fill: FillStyle::Solid([1.0, 0.0, 0.0, 1.0]),
        opacity: 1.0,
        size_px: 6.0,
        ..GlowEffect::default()
    });
    close(
        full(t).get(33, 24),
        [1.0, 1.0, 1.0, 1.0],
        1e-5,
        "red screened onto white is white",
    );
}

#[test]
fn the_layers_opacity_still_fades_a_backdrop_blended_shadow() {
    let mut t = TestDoc::linear(64, 64);
    solid_layer(&mut t, "Backdrop", [255, 255, 255, 255]);
    let id = square(&mut t);
    let layer = t.doc.layers.get_mut(id).unwrap();
    layer.effects.drop_shadow = Some(shadow(
        BlendMode::Multiply,
        [0.0, 0.0, 0.0, 1.0],
        180.0,
        8.0,
    ));
    layer.opacity = 0.5;
    close(
        full(t).get(IN_SHADOW.0, IN_SHADOW.1),
        [0.5, 0.5, 0.5, 1.0],
        1e-5,
        "half a black multiply over white",
    );
}

#[test]
fn two_drop_shadows_both_render() {
    let black = [0.0, 0.0, 0.0, 1.0];
    let mut t = TestDoc::linear(64, 64);
    solid_layer(&mut t, "Backdrop", [255, 255, 255, 255]);
    let id = square(&mut t);
    let fx = &mut t.doc.layers.get_mut(id).unwrap().effects;
    // One to the right (light at 180), one downward (light at 90).
    fx.drop_shadow = Some(shadow(BlendMode::Normal, black, 180.0, 8.0));
    fx.extras.drop_shadows.push(ShadowInstance {
        effect: shadow(BlendMode::Normal, black, 90.0, 8.0),
        contour: Contour::default(),
    });
    let out = full(t);
    close(
        out.get(IN_SHADOW.0, IN_SHADOW.1),
        black,
        1e-5,
        "the first shadow",
    );
    close(out.get(24, 36), black, 1e-5, "the second shadow");
    close(
        out.get(40, 40),
        [1.0; 4],
        1e-5,
        "neither reaches the corner",
    );
}

#[test]
fn a_contour_reshapes_a_glows_falloff() {
    let glow = |contour: Contour| {
        let mut t = TestDoc::linear(64, 64);
        let id = square(&mut t);
        let fx = &mut t.doc.layers.get_mut(id).unwrap().effects;
        fx.outer_glow = Some(GlowEffect {
            blend_mode: BlendMode::Normal,
            fill: FillStyle::Solid([1.0, 0.0, 0.0, 1.0]),
            opacity: 1.0,
            size_px: 8.0,
            range: 1.0,
            ..GlowEffect::default()
        });
        fx.extras.contours.outer_glow = contour;
        full(t)
    };
    let linear = glow(Contour::default());
    let cone = glow(Contour::preset(ContourPreset::Cone));
    // The glow's coverage is its falloff passed through the contour: a cone
    // lifts a mid value toward full and takes full coverage to zero.
    for x in [32, 33, 35, 38] {
        let (l, c) = (linear.get(x, 24)[3], cone.get(x, 24)[3]);
        let want = 1.0 - (2.0 * l - 1.0).abs();
        assert!(
            (c - want).abs() < 1.0e-3,
            "x={x}: linear {l} through the cone is {want}, drew {c}"
        );
    }
    assert_ne!(linear.get(33, 24), cone.get(33, 24));
    // And a contour is region independent like everything else.
    let (doc, src) = {
        let mut t = TestDoc::linear(64, 64);
        let id = square(&mut t);
        let fx = &mut t.doc.layers.get_mut(id).unwrap().effects;
        fx.outer_glow = Some(GlowEffect {
            size_px: 8.0,
            ..GlowEffect::default()
        });
        fx.extras.contours.outer_glow = Contour::preset(ContourPreset::Ring);
        t.finish()
    };
    let whole = composite_rect(
        &doc,
        &src,
        PixelRect::new(0, 0, 64, 64),
        0,
        CompositeOptions::default(),
    )
    .unwrap();
    let part = composite_rect(
        &doc,
        &src,
        PixelRect::new(30, 10, 12, 20),
        0,
        CompositeOptions::default(),
    )
    .unwrap();
    for y in 10..30 {
        for x in 30..42 {
            assert_eq!(part.get(x, y), whole.get(x, y), "({x}, {y})");
        }
    }
}

#[test]
fn blend_if_underlying_128_to_255_hides_the_layer_over_dark_pixels() {
    let mut t = TestDoc::linear(64, 64);
    let back = t.push_raster("Backdrop");
    // Left half dark (code 26), right half light (code 230).
    t.paint_tile_with(back, TileCoord::new(0, 0, 0), |x, _| {
        if x < 32 {
            [26, 26, 26, 255]
        } else {
            [230, 230, 230, 255]
        }
    });
    let top = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
    t.doc
        .layers
        .get_mut(top)
        .unwrap()
        .effects
        .extras
        .blend_if
        .gray
        .underlying
        .black = [128.0 / 255.0, 128.0 / 255.0];
    let out = full(t);
    let dark = 26.0 / 255.0;
    close(
        out.get(8, 8),
        [dark, dark, dark, 1.0],
        1e-5,
        "over dark pixels the layer is hidden",
    );
    close(
        out.get(48, 8),
        [1.0, 0.0, 0.0, 1.0],
        1e-5,
        "over light pixels it shows",
    );
}

#[test]
fn blend_if_this_layer_hides_its_own_bright_pixels_and_keys_the_tile_cache() {
    let build = || {
        let mut t = TestDoc::linear(64, 64);
        solid_layer(&mut t, "Backdrop", [0, 0, 255, 255]);
        let top = t.push_raster("Ramp");
        t.paint_tile_with(top, TileCoord::new(0, 0, 0), |x, _| {
            if x < 32 {
                [40, 40, 40, 255]
            } else {
                [220, 220, 220, 255]
            }
        });
        t.doc
            .layers
            .get_mut(top)
            .unwrap()
            .effects
            .extras
            .blend_if
            .gray
            .this_layer
            .white = [0.5, 0.5];
        let (doc, src) = t.finish();
        (doc, src, top)
    };
    let (doc, src, top) = build();
    let out = composite_rect(
        &doc,
        &src,
        PixelRect::new(0, 0, 64, 64),
        0,
        CompositeOptions::default(),
    )
    .unwrap();
    let d = 40.0 / 255.0;
    close(out.get(8, 8), [d, d, d, 1.0], 1e-5, "dark pixels stay");
    close(
        out.get(48, 8),
        [0.0, 0.0, 1.0, 1.0],
        1e-5,
        "bright pixels past the white end are hidden",
    );

    // The tile cache must not serve the first answer for the second range.
    let mut cache = crate::TileCompositor::with_capacity(4);
    let coord = TileCoord::new(0, 0, 0);
    let opts = CompositeOptions::default();
    let first = cache.composite_tile(&doc, &src, coord, opts).unwrap();
    let mut doc2 = doc.clone();
    doc2.layers
        .get_mut(top)
        .unwrap()
        .effects
        .extras
        .blend_if
        .gray
        .this_layer
        .white = [1.0, 1.0];
    let second = cache.composite_tile(&doc2, &src, coord, opts).unwrap();
    assert_ne!(
        first.pixels(),
        second.pixels(),
        "a moved Blend If handle must re-key the tile"
    );
}
