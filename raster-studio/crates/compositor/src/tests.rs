//! End-to-end behaviour of the compositor.
//!
//! These are the tests that decide whether the editor shows the right picture.
//! Where a number is asserted it is computed from the definition of the
//! operation — in linear light, by hand or by an independent implementation
//! written from the spec — never by calling the code under test twice.

use color::ColorSpace;
use editor_core::Document;
use glam::{Affine2, Vec2};
use layer_model::{
    AdjustmentKind, BlendMode, ClippingMode, GroupBlending, Layer, LayerId, MaskKind,
};
use raster::{PixelRect, TileCoord, TILE_SIZE};

use crate::canvas::Canvas;
use crate::composite::{composite_rect, composite_region, composite_subtree, CompositeOptions};
use crate::error::CompositeError;
use crate::source::MemoryTileSource;
use crate::testkit::{solid_layer, TestDoc};
use crate::{BlendSpace, CacheStats, TileCompositor};

fn rect(x: i64, y: i64, w: u32, h: u32) -> PixelRect {
    PixelRect::new(x, y, w, h)
}

fn opts() -> CompositeOptions {
    CompositeOptions::default()
}

/// Composite the whole document at level 0.
fn full(doc: &Document, src: &MemoryTileSource) -> Canvas {
    composite_rect(doc, src, rect(0, 0, doc.width(), doc.height()), 0, opts()).expect("composite")
}

#[track_caller]
fn assert_px(got: [f32; 4], want: [f32; 4], tol: f32, what: &str) {
    for i in 0..4 {
        assert!(
            (got[i] - want[i]).abs() <= tol,
            "{what}: channel {i} was {} want {} (got {got:?}, want {want:?})",
            got[i],
            want[i]
        );
    }
}

// ---------------------------------------------------------------- exact value

#[test]
fn red_over_blue_at_half_opacity_is_exact_in_linear_light() {
    // sRGB 255 decodes to linear 1.0 exactly, so the whole composite is
    // hand-computable:
    //   backdrop premultiplied = (0, 0, 1, 1)
    //   source straight = (1, 0, 0), effective alpha = 0.5
    //   Cs' = (1 - 1)*Cs + 1*B(Cb, Cs) = Cs                      (Normal)
    //   Co  = 0.5*(1,0,0) + (0,0,1)*(1 - 0.5) = (0.5, 0, 0.5)
    //   ao  = 0.5 + 1*(1 - 0.5) = 1
    let mut t = TestDoc::new(4, 4);
    solid_layer(&mut t, "Blue", [0, 0, 255, 255]);
    let red = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
    t.doc.layers.get_mut(red).unwrap().opacity = 0.5;
    let (doc, src) = t.finish();

    let out = full(&doc, &src);
    for px in out.pixels() {
        assert_px(*px, [0.5, 0.0, 0.5, 1.0], 1e-6, "red over blue at 50%");
    }

    // And the same pixel, encoded back to 8-bit sRGB for presentation.
    let want = (color::linear_to_srgb(0.5) * 255.0).round() as u8;
    assert_eq!(out.to_rgba8(&ColorSpace::Srgb)[0..4], [want, 0, want, 255]);

    // The naive "blend the bytes" answer would be 128 — this is the whole
    // reason the compositor works in linear light.
    assert_ne!(want, 128);
}

#[test]
fn stored_bytes_decode_through_the_documents_transfer_function() {
    // The most basic form of the linear-light invariant, and the one the
    // red/blue test above cannot see: sRGB 0 and 255 decode to 0.0 and 1.0
    // whether or not anyone applied the transfer function, so the proof has to
    // come from a mid-tone.
    let mut t = TestDoc::new(4, 4);
    solid_layer(&mut t, "Mid grey", [128, 128, 128, 255]);
    let (doc, src) = t.finish();

    let want = color::srgb_to_linear(128.0 / 255.0);
    assert!(
        (want - 0.5).abs() > 0.2,
        "premise: the encoded and linear values must be far apart, got {want}"
    );
    assert_px(
        full(&doc, &src).pixels()[0],
        [want, want, want, 1.0],
        1e-6,
        "mid grey decoded to linear",
    );

    // The same bytes in a linear document decode to themselves — the transfer
    // function comes from the document, not from an assumption.
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Mid grey", [128, 128, 128, 255]);
    let (doc, src) = t.finish();
    let v = 128.0 / 255.0;
    assert_px(
        full(&doc, &src).pixels()[0],
        [v, v, v, 1.0],
        1e-6,
        "linear document",
    );
}

#[test]
fn a_half_alpha_layer_produces_a_premultiplied_buffer() {
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Half red", [255, 0, 0, 128]);
    let (doc, src) = t.finish();
    let a = 128.0 / 255.0;
    let out = full(&doc, &src);
    assert_px(out.pixels()[0], [a, 0.0, 0.0, a], 1e-6, "premultiplied");
    // Straight alpha recovers the original colour.
    assert_px(out.to_straight()[0], [1.0, 0.0, 0.0, a], 1e-5, "straight");
}

// ------------------------------------------------------------- 27 blend modes

/// An independent implementation of all 27 blend functions, written from the
/// W3C compositing spec and the Photoshop definitions rather than from
/// `layer_model::blend`.
mod reference {
    use layer_model::BlendMode;

    fn clamp01(v: f32) -> f32 {
        v.clamp(0.0, 1.0)
    }

    fn multiply(b: f32, s: f32) -> f32 {
        b * s
    }

    fn screen(b: f32, s: f32) -> f32 {
        b + s - b * s
    }

    fn hard_light(b: f32, s: f32) -> f32 {
        if s <= 0.5 {
            multiply(b, 2.0 * s)
        } else {
            screen(b, 2.0 * s - 1.0)
        }
    }

    fn color_dodge(b: f32, s: f32) -> f32 {
        if b <= 0.0 {
            0.0
        } else if s >= 1.0 {
            1.0
        } else {
            (b / (1.0 - s)).min(1.0)
        }
    }

    fn color_burn(b: f32, s: f32) -> f32 {
        if b >= 1.0 {
            1.0
        } else if s <= 0.0 {
            0.0
        } else {
            1.0 - ((1.0 - b) / s).min(1.0)
        }
    }

    fn soft_light(b: f32, s: f32) -> f32 {
        if s <= 0.5 {
            b - (1.0 - 2.0 * s) * b * (1.0 - b)
        } else {
            let d = if b <= 0.25 {
                ((16.0 * b - 12.0) * b + 4.0) * b
            } else {
                b.sqrt()
            };
            b + (2.0 * s - 1.0) * (d - b)
        }
    }

    fn channel(mode: BlendMode, b: f32, s: f32) -> f32 {
        let v = match mode {
            BlendMode::Normal | BlendMode::Dissolve => s,
            BlendMode::Darken => b.min(s),
            BlendMode::Multiply => multiply(b, s),
            BlendMode::ColorBurn => color_burn(b, s),
            BlendMode::LinearBurn => b + s - 1.0,
            BlendMode::Lighten => b.max(s),
            BlendMode::Screen => screen(b, s),
            BlendMode::ColorDodge => color_dodge(b, s),
            BlendMode::LinearDodge => b + s,
            BlendMode::Overlay => hard_light(s, b),
            BlendMode::SoftLight => soft_light(b, s),
            BlendMode::HardLight => hard_light(b, s),
            BlendMode::VividLight => {
                if s <= 0.5 {
                    color_burn(b, 2.0 * s)
                } else {
                    color_dodge(b, 2.0 * s - 1.0)
                }
            }
            BlendMode::LinearLight => b + 2.0 * s - 1.0,
            BlendMode::PinLight => {
                if s <= 0.5 {
                    b.min(2.0 * s)
                } else {
                    b.max(2.0 * s - 1.0)
                }
            }
            BlendMode::HardMix => {
                if b + s >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            BlendMode::Difference => (b - s).abs(),
            BlendMode::Exclusion => b + s - 2.0 * b * s,
            BlendMode::Subtract => b - s,
            BlendMode::Divide => {
                if s <= 0.0 {
                    1.0
                } else {
                    b / s
                }
            }
            _ => unreachable!("non-separable mode reached the per-channel path"),
        };
        clamp01(v)
    }

    fn lum(c: [f32; 3]) -> f32 {
        0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
    }

    fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
        let l = lum(c);
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        if n < 0.0 {
            for v in c.iter_mut() {
                *v = l + (*v - l) * l / (l - n);
            }
        }
        if x > 1.0 {
            for v in c.iter_mut() {
                *v = l + (*v - l) * (1.0 - l) / (x - l);
            }
        }
        c
    }

    fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
        let d = l - lum(c);
        clip_color([c[0] + d, c[1] + d, c[2] + d])
    }

    fn sat(c: [f32; 3]) -> f32 {
        c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
    }

    fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
        // Rank the channels, then rebuild by the W3C rule.
        let mut idx = [0usize, 1, 2];
        idx.sort_by(|a, b| c[*a].total_cmp(&c[*b]));
        let (lo, mid, hi) = (idx[0], idx[1], idx[2]);
        let mut out = [0.0f32; 3];
        if c[hi] > c[lo] {
            out[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
            out[hi] = s;
        }
        out[lo] = 0.0;
        out
    }

    /// `B(Cb, Cs)` for every mode, in the same straight-colour domain the
    /// compositor uses.
    pub fn blend(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
        let out = match mode {
            BlendMode::DarkerColor => {
                if lum(cb) <= lum(cs) {
                    cb
                } else {
                    cs
                }
            }
            BlendMode::LighterColor => {
                if lum(cb) >= lum(cs) {
                    cb
                } else {
                    cs
                }
            }
            BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
            BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
            BlendMode::Color => set_lum(cs, lum(cb)),
            BlendMode::Luminosity => set_lum(cb, lum(cs)),
            sep => [
                channel(sep, cb[0], cs[0]),
                channel(sep, cb[1], cs[1]),
                channel(sep, cb[2], cs[2]),
            ],
        };
        [clamp01(out[0]), clamp01(out[1]), clamp01(out[2])]
    }
}

/// Backdrop and source bytes chosen so that, in a linear document, they decode
/// to distinct values with distinct luminosities and distinct saturations —
/// otherwise `Hue`/`Color` and `DarkerColor`/`LighterColor` would agree by
/// accident and the test would not tell them apart.
const BACKDROP: [u8; 4] = [204, 153, 102, 255];
const SOURCE: [u8; 4] = [51, 102, 204, 255];

fn as_linear(bytes: [u8; 4]) -> [f32; 3] {
    [
        bytes[0] as f32 / 255.0,
        bytes[1] as f32 / 255.0,
        bytes[2] as f32 / 255.0,
    ]
}

#[test]
fn every_blend_mode_matches_an_independent_reference() {
    let cb = as_linear(BACKDROP);
    let cs = as_linear(SOURCE);
    // The premise of the fixture: these four quantities must all differ, or
    // several modes collapse onto each other and the test proves less than it
    // looks like it does.
    assert_ne!(cb, cs);

    for mode in BlendMode::ALL {
        let mut t = TestDoc::linear(4, 4);
        solid_layer(&mut t, "Backdrop", BACKDROP);
        let top = solid_layer(&mut t, "Source", SOURCE);
        t.doc.layers.get_mut(top).unwrap().blend_mode = mode;
        let (doc, src) = t.finish();

        // Both layers are opaque, so the W3C model collapses to `Co = B(Cb, Cs)`
        // with `ao = 1` — the blend function alone, which is what the reference
        // computes.
        let want = reference::blend(mode, cb, cs);
        let got = full(&doc, &src).pixels()[0];
        assert_px(
            got,
            [want[0], want[1], want[2], 1.0],
            2e-5,
            &format!("{mode:?} ({})", mode.label()),
        );
    }
}

#[test]
fn the_blend_reference_agrees_with_hand_computed_values() {
    // A reference implementation nobody has checked is just a second bug. Six
    // modes with arithmetic simple enough to do on paper.
    let b = [0.8f32, 0.6, 0.4];
    let s = [0.2f32, 0.4, 0.8];
    let cases = [
        (BlendMode::Normal, [0.2, 0.4, 0.8]),
        (BlendMode::Multiply, [0.16, 0.24, 0.32]),
        (BlendMode::Screen, [0.84, 0.76, 0.88]),
        (BlendMode::Darken, [0.2, 0.4, 0.4]),
        (BlendMode::Lighten, [0.8, 0.6, 0.8]),
        (BlendMode::Difference, [0.6, 0.2, 0.4]),
    ];
    for (mode, want) in cases {
        let got = reference::blend(mode, b, s);
        for i in 0..3 {
            assert!(
                (got[i] - want[i]).abs() < 1e-6,
                "{mode:?} channel {i}: {got:?} want {want:?}"
            );
        }
    }
    // Luminosity: Lum(b) = .24+.354+.044 = .638, Lum(s) = .06+.236+.088 = .384,
    // so every channel of b moves by -0.254 and stays in gamut.
    let lumi = reference::blend(BlendMode::Luminosity, b, s);
    for i in 0..3 {
        assert!(
            (lumi[i] - (b[i] - 0.254)).abs() < 1e-5,
            "Luminosity {lumi:?}"
        );
    }
}

#[test]
fn every_blend_mode_shows_the_top_layer_over_an_empty_document() {
    // A blend function evaluated against nothing is black for Multiply, Darken,
    // ColorBurn and several others. The `(1 - ab)` term in the W3C model is
    // what keeps the top layer of a document visible.
    let cs = as_linear(SOURCE);
    for mode in BlendMode::ALL {
        let mut t = TestDoc::linear(4, 4);
        let top = solid_layer(&mut t, "Only", SOURCE);
        t.doc.layers.get_mut(top).unwrap().blend_mode = mode;
        let (doc, src) = t.finish();
        assert_px(
            full(&doc, &src).pixels()[0],
            [cs[0], cs[1], cs[2], 1.0],
            1e-6,
            &format!("{mode:?} over nothing"),
        );
    }
}

#[test]
fn the_encoded_blend_space_is_opt_in() {
    let mut t = TestDoc::new(4, 4);
    solid_layer(&mut t, "Backdrop", [128, 128, 128, 255]);
    let top = solid_layer(&mut t, "Source", [128, 128, 128, 255]);
    t.doc.layers.get_mut(top).unwrap().blend_mode = BlendMode::Multiply;
    let (doc, src) = t.finish();

    let region = rect(0, 0, 4, 4);
    let linear = composite_rect(&doc, &src, region, 0, opts()).unwrap();
    let encoded = composite_rect(
        &doc,
        &src,
        region,
        0,
        CompositeOptions {
            blend_space: BlendSpace::Encoded,
            ..opts()
        },
    )
    .unwrap();

    let v = color::srgb_to_linear(128.0 / 255.0);
    assert_px(
        linear.pixels()[0],
        [v * v, v * v, v * v, 1.0],
        1e-6,
        "default is linear",
    );
    // Encoded Multiply of mid grey lands only about 0.005 higher in linear
    // terms — small in absolute value because both ends of the round trip
    // compress, but an entirely different operation.
    let want_encoded = color::srgb_to_linear(color::linear_to_srgb(v).powi(2));
    assert_px(
        encoded.pixels()[0],
        [want_encoded, want_encoded, want_encoded, 1.0],
        1e-5,
        "encoded multiply",
    );
    assert!(
        (encoded.pixels()[0][0] - linear.pixels()[0][0]).abs() > 1e-3,
        "encoded {:?} vs linear {:?}",
        encoded.pixels()[0],
        linear.pixels()[0]
    );
}

// ---------------------------------------------------------------------- order

#[test]
fn layers_composite_bottom_up() {
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Bottom", [255, 0, 0, 255]);
    solid_layer(&mut t, "Top", [0, 0, 255, 255]);
    let (doc, src) = t.finish();
    // The tree's root list is top-most first, and `push` puts each new layer on
    // top, so the last one pushed wins.
    assert_px(
        full(&doc, &src).pixels()[0],
        [0.0, 0.0, 1.0, 1.0],
        1e-6,
        "top layer wins",
    );
}

#[test]
fn an_invisible_or_zero_opacity_layer_contributes_nothing() {
    for hide in [0, 1] {
        let mut t = TestDoc::linear(4, 4);
        solid_layer(&mut t, "Bottom", [255, 0, 0, 255]);
        let top = solid_layer(&mut t, "Top", [0, 0, 255, 255]);
        let layer = t.doc.layers.get_mut(top).unwrap();
        if hide == 0 {
            layer.visible = false;
        } else {
            layer.opacity = 0.0;
        }
        let (doc, src) = t.finish();
        assert_px(
            full(&doc, &src).pixels()[0],
            [1.0, 0.0, 0.0, 1.0],
            1e-6,
            "hidden layer",
        );
    }
}

#[test]
fn fill_opacity_scales_the_layer_like_opacity_does_while_there_are_no_effects() {
    // Documented crate-level gap: with no layer effects rendered, `fill` and
    // `opacity` are indistinguishable. This test is the record of that, and it
    // fails the day effects land — which is when the two must diverge.
    let mut t = TestDoc::linear(4, 4);
    let id = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
    t.doc.layers.get_mut(id).unwrap().fill_opacity = 0.25;
    let (doc, src) = t.finish();
    assert_px(
        full(&doc, &src).pixels()[0],
        [0.25, 0.0, 0.0, 0.25],
        1e-6,
        "fill opacity",
    );
}

// --------------------------------------------------------------------- groups

#[test]
fn group_opacity_is_not_per_child_opacity() {
    // Two opaque children stacked in a group at 50%: the group buffer is opaque
    // red, so the result is red at alpha 0.5.
    let mut grouped = TestDoc::linear(4, 4);
    let g = grouped.push_group("G");
    let blue = grouped.push_child(g, Layer::raster("Blue"));
    grouped.fill(blue, [0, 0, 255, 255]);
    let red = grouped.push_child(g, Layer::raster("Red"));
    grouped.fill(red, [255, 0, 0, 255]);
    grouped.doc.layers.get_mut(g).unwrap().opacity = 0.5;
    let (doc, src) = grouped.finish();
    let group_result = full(&doc, &src).pixels()[0];
    assert_px(group_result, [0.5, 0.0, 0.0, 0.5], 1e-6, "group at 50%");

    // The same two layers at 50% each, with no group: the blue shows through
    // and the alpha compounds to 0.75.
    let mut flat = TestDoc::linear(4, 4);
    let blue = solid_layer(&mut flat, "Blue", [0, 0, 255, 255]);
    let red = solid_layer(&mut flat, "Red", [255, 0, 0, 255]);
    flat.doc.layers.get_mut(blue).unwrap().opacity = 0.5;
    flat.doc.layers.get_mut(red).unwrap().opacity = 0.5;
    let (doc, src) = flat.finish();
    let flat_result = full(&doc, &src).pixels()[0];
    assert_px(flat_result, [0.5, 0.0, 0.25, 0.75], 1e-6, "children at 50%");

    assert_ne!(
        group_result, flat_result,
        "flattening a group and pushing its opacity down is not the same picture"
    );
}

#[test]
fn a_group_blend_mode_applies_to_the_group_as_a_unit() {
    // Backdrop v, group containing two opaque v children, group set to
    // Multiply. As a unit: v*v. Applied per child it would be v*v*v.
    let v = 128.0 / 255.0;
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Backdrop", [128, 128, 128, 255]);
    let g = t.push_group("G");
    let a = t.push_child(g, Layer::raster("A"));
    t.fill(a, [128, 128, 128, 255]);
    let b = t.push_child(g, Layer::raster("B"));
    t.fill(b, [128, 128, 128, 255]);
    t.doc.layers.get_mut(g).unwrap().blend_mode = BlendMode::Multiply;
    let (doc, src) = t.finish();

    let got = full(&doc, &src).pixels()[0];
    assert_px(got, [v * v, v * v, v * v, 1.0], 1e-5, "group multiply");
    assert!(
        (got[0] - v * v * v).abs() > 0.05,
        "each child must not multiply separately: {got:?}"
    );
}

#[test]
fn a_pass_through_group_lets_an_adjustment_reach_the_backdrop() {
    // Same document twice, differing only in the group's blending mode.
    let build = |blending: GroupBlending| {
        let mut t = TestDoc::linear(4, 4);
        solid_layer(&mut t, "Backdrop", [64, 64, 64, 255]);
        let g = t.push_group("G");
        t.set_group_blending(g, blending);
        let adj = Layer::with_kind(
            "Exposure",
            layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: AdjustmentKind::Exposure { stops: 1.0 },
            }),
        );
        t.push_child(g, adj);
        let (doc, src) = t.finish();
        full(&doc, &src).pixels()[0]
    };

    let v = 64.0 / 255.0;
    assert_px(
        build(GroupBlending::PassThrough),
        [2.0 * v, 2.0 * v, 2.0 * v, 1.0],
        1e-5,
        "pass through reaches the backdrop",
    );
    // Isolated: the group's own buffer starts empty, so the adjustment has
    // nothing to act on and the group contributes nothing.
    assert_px(
        build(GroupBlending::Isolated),
        [v, v, v, 1.0],
        1e-6,
        "isolated group cannot see the backdrop",
    );
}

#[test]
fn a_pass_through_group_with_a_blend_mode_falls_back_to_isolated() {
    // `GroupBlending::Isolated`'s own docs require isolation whenever the
    // group's blend mode is not Normal; a pass-through group has no buffer for
    // a blend mode to act on.
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Backdrop", [64, 64, 64, 255]);
    let g = t.push_group("G");
    t.set_group_blending(g, GroupBlending::PassThrough);
    t.doc.layers.get_mut(g).unwrap().blend_mode = BlendMode::Multiply;
    let adj = Layer::with_kind(
        "Exposure",
        layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
            kind: AdjustmentKind::Exposure { stops: 1.0 },
        }),
    );
    t.push_child(g, adj);
    let (doc, src) = t.finish();
    let v = 64.0 / 255.0;
    assert_px(
        full(&doc, &src).pixels()[0],
        [v, v, v, 1.0],
        1e-6,
        "non-Normal pass-through composites isolated",
    );
}

#[test]
fn nested_groups_composite_depth_first() {
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Backdrop", [255, 0, 0, 255]);
    let outer = t.push_group("Outer");
    let inner = t.push_child(outer, Layer::group("Inner"));
    let leaf = t.push_child(inner, Layer::raster("Leaf"));
    t.fill(leaf, [0, 0, 255, 255]);
    // Half at each of the two group levels: 0.25 in total.
    t.doc.layers.get_mut(outer).unwrap().opacity = 0.5;
    t.doc.layers.get_mut(inner).unwrap().opacity = 0.5;
    let (doc, src) = t.finish();
    assert_px(
        full(&doc, &src).pixels()[0],
        [0.75, 0.0, 0.25, 1.0],
        1e-6,
        "nested group opacity",
    );
}

// ------------------------------------------------------------------- clipping

/// A layer whose alpha ramps from 0 to 255 across the tile, so a clip test can
/// check every partial coverage at once.
fn alpha_ramp_layer(t: &mut TestDoc, name: &str, rgb: [u8; 3]) -> LayerId {
    let id = t.push_raster(name);
    t.paint_tile_with(id, TileCoord::new(0, 0, 0), move |x, _| {
        [rgb[0], rgb[1], rgb[2], x as u8]
    });
    id
}

#[test]
fn a_clipping_mask_limits_the_clipped_layer_to_the_base_alpha() {
    let mut t = TestDoc::linear(TILE_SIZE, 4);
    alpha_ramp_layer(&mut t, "Base", [255, 0, 0]);
    let green = solid_layer(&mut t, "Green", [0, 255, 0, 255]);
    t.doc.layers.get_mut(green).unwrap().clipping = ClippingMode::ClipToBelow;
    let (doc, src) = t.finish();

    let out = full(&doc, &src);
    for x in 0..TILE_SIZE as i64 {
        let a = x as f32 / 255.0;
        let a = a.min(1.0);
        // The clipped green replaces the base's colour, but only inside the
        // base's shape, and the alpha is the base's alpha untouched.
        assert_px(
            out.get(x, 0),
            [0.0, a, 0.0, a],
            1e-5,
            &format!("clipped at x={x}"),
        );
    }
}

#[test]
fn a_clipping_group_never_grows_past_its_base() {
    // With plain `over` instead of `atop`, an opaque clipped layer on a
    // half-transparent base composites to alpha 0.75 and the shape visibly
    // spreads.
    let mut t = TestDoc::linear(4, 4);
    let base = solid_layer(&mut t, "Base", [255, 0, 0, 128]);
    let green = solid_layer(&mut t, "Green", [0, 255, 0, 255]);
    t.doc.layers.get_mut(green).unwrap().clipping = ClippingMode::ClipToBelow;
    let (doc, src) = t.finish();
    let _ = base;

    let a = 128.0 / 255.0;
    assert_px(full(&doc, &src).pixels()[0], [0.0, a, 0.0, a], 1e-6, "atop");
}

#[test]
fn a_hidden_base_hides_everything_clipped_to_it() {
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Backdrop", [255, 0, 0, 255]);
    let base = solid_layer(&mut t, "Base", [0, 0, 255, 255]);
    let green = solid_layer(&mut t, "Green", [0, 255, 0, 255]);
    t.doc.layers.get_mut(green).unwrap().clipping = ClippingMode::ClipToBelow;
    t.doc.layers.get_mut(base).unwrap().visible = false;
    let (doc, src) = t.finish();
    assert_px(
        full(&doc, &src).pixels()[0],
        [1.0, 0.0, 0.0, 1.0],
        1e-6,
        "the whole clipping group is gone",
    );
}

#[test]
fn a_clipper_with_nothing_beneath_it_draws_normally() {
    // Matches `LayerTree::clipping_group`, which reports no clipping group for
    // a run of clippers that reaches the bottom of its sibling list.
    let mut t = TestDoc::linear(4, 4);
    let green = solid_layer(&mut t, "Green", [0, 255, 0, 255]);
    t.doc.layers.get_mut(green).unwrap().clipping = ClippingMode::ClipToBelow;
    let (doc, src) = t.finish();
    assert!(doc.layers.clipping_group(green).is_none(), "premise");
    assert_px(
        full(&doc, &src).pixels()[0],
        [0.0, 1.0, 0.0, 1.0],
        1e-6,
        "dangling clipper",
    );
}

#[test]
fn a_run_of_clippers_all_clip_to_the_same_base() {
    let mut t = TestDoc::linear(4, 4);
    let base = solid_layer(&mut t, "Base", [255, 0, 0, 128]);
    let mid = solid_layer(&mut t, "Mid", [0, 0, 255, 255]);
    let top = solid_layer(&mut t, "Top", [0, 255, 0, 128]);
    t.doc.layers.get_mut(mid).unwrap().clipping = ClippingMode::ClipToBelow;
    t.doc.layers.get_mut(top).unwrap().clipping = ClippingMode::ClipToBelow;
    let (doc, src) = t.finish();
    let group = doc.layers.clipping_group(base).expect("a clipping group");
    assert_eq!(group.base, base);
    assert_eq!(group.clipped, vec![top, mid], "premise: both clip to base");

    // Inside the base's shape: blue, then green at 128/255 over it. Alpha stays
    // at the base's 128/255 throughout.
    let a = 128.0 / 255.0;
    let want_g = a;
    let want_b = 1.0 - a;
    let got = full(&doc, &src).pixels()[0];
    assert_px(got, [0.0, want_g * a, want_b * a, a], 1e-5, "two clippers");
}

#[test]
fn a_clipped_adjustment_adjusts_only_its_base() {
    // Backdrop white; base red covering the left half; an Exposure(-1) clipped
    // to the base. The white on the right must be untouched — an unclipped
    // adjustment would halve it.
    let mut t = TestDoc::linear(TILE_SIZE, 4);
    solid_layer(&mut t, "White", [255, 255, 255, 255]);
    let base = t.push_raster("Base");
    t.paint_tile_with(base, TileCoord::new(0, 0, 0), |x, _| {
        if x < 128 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    let adj = t.push_adjustment("Darken", AdjustmentKind::Exposure { stops: -1.0 });
    t.doc.layers.get_mut(adj).unwrap().clipping = ClippingMode::ClipToBelow;
    let (doc, src) = t.finish();

    let out = full(&doc, &src);
    assert_px(out.get(10, 0), [0.5, 0.0, 0.0, 1.0], 1e-5, "base is halved");
    assert_px(
        out.get(200, 0),
        [1.0, 1.0, 1.0, 1.0],
        1e-6,
        "the backdrop outside the base is untouched",
    );
}

#[test]
fn a_clipped_adjustments_linked_mask_moves_with_its_transform_too() {
    // The clipping path renders adjustments through its own arm, so it needs
    // the same proof: inside a clipping group a linked mask still travels with
    // the layer.
    let mut t = TestDoc::linear(512, 8);
    let base = solid_layer(&mut t, "Grey", [64, 64, 64, 255]);
    assert!(t.doc.layers.contains(base));
    let adj = t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
    let mask = t.attach_mask(adj);
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 255);
    {
        let l = t.doc.layers.get_mut(adj).unwrap();
        l.clipping = ClippingMode::ClipToBelow;
        l.transform = Affine2::from_translation(Vec2::new(TILE_SIZE as f32, 0.0));
    }
    let (doc, src) = t.finish();

    let v = 64.0 / 255.0;
    let out = full(&doc, &src);
    assert_px(out.get(10, 0), [v, v, v, 1.0], 1e-6, "left of the mask");
    assert_px(
        out.get(300, 0),
        [2.0 * v, 2.0 * v, 2.0 * v, 1.0],
        1e-5,
        "under the moved mask",
    );
}

// ---------------------------------------------------------------- adjustments

#[test]
fn an_adjustment_layer_changes_what_is_below_it_and_nothing_above() {
    let mut t = TestDoc::linear(TILE_SIZE, 4);
    solid_layer(&mut t, "Below", [64, 64, 64, 255]);
    t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
    // An opaque layer above the adjustment, covering only the right half.
    let above = t.push_raster("Above");
    t.paint_tile_with(above, TileCoord::new(0, 0, 0), |x, _| {
        if x >= 128 {
            [64, 64, 64, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    let (doc, src) = t.finish();

    let v = 64.0 / 255.0;
    let out = full(&doc, &src);
    assert_px(
        out.get(10, 0),
        [2.0 * v, 2.0 * v, 2.0 * v, 1.0],
        1e-5,
        "below the adjustment",
    );
    assert_px(
        out.get(200, 0),
        [v, v, v, 1.0],
        1e-6,
        "above the adjustment: unchanged",
    );
}

#[test]
fn an_adjustment_never_changes_alpha() {
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Half", [255, 0, 0, 128]);
    t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 2.0 });
    let (doc, src) = t.finish();
    let a = 128.0 / 255.0;
    let out = full(&doc, &src);
    let got = out.pixels()[0];
    assert!((got[3] - a).abs() < 1e-6, "alpha moved: {got:?}");
    // The working space is scene-referred and unclamped, exactly as the `color`
    // crate documents: two stops above a 1.0 red really is 4.0, premultiplied
    // by the layer's own alpha. Clamping is a display-side concern.
    assert!(
        (got[0] - 4.0 * a).abs() < 1e-4,
        "not scene-referred: {got:?}"
    );
    // ...and it does clamp on the way to 8 bits.
    assert_eq!(out.to_rgba8(&ColorSpace::LinearSrgb)[0], 255);
}

#[test]
fn adjustment_opacity_mixes_between_the_original_and_the_adjusted() {
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Grey", [64, 64, 64, 255]);
    let adj = t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
    t.doc.layers.get_mut(adj).unwrap().opacity = 0.5;
    let (doc, src) = t.finish();
    let v = 64.0 / 255.0;
    assert_px(
        full(&doc, &src).pixels()[0],
        [1.5 * v, 1.5 * v, 1.5 * v, 1.0],
        1e-5,
        "half-strength adjustment",
    );
}

#[test]
fn an_adjustment_over_nothing_leaves_nothing() {
    let mut t = TestDoc::linear(4, 4);
    t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 3.0 });
    let (doc, src) = t.finish();
    assert!(full(&doc, &src).pixels().iter().all(|p| *p == [0.0; 4]));
}

#[test]
fn fill_opacity_scales_an_adjustment_like_opacity_does() {
    // The crate documents one weight for a layer's own contribution — opacity ×
    // fill opacity — and an adjustment layer is not an exception to it. Without
    // this the slider would be hashed into the tile key (it is) and change
    // nothing, which is an eviction that repaints an identical picture.
    let v = 64.0 / 255.0;
    let brightened = |fill: f32| {
        let mut t = TestDoc::linear(4, 4);
        solid_layer(&mut t, "Grey", [64, 64, 64, 255]);
        let adj = t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
        t.doc.layers.get_mut(adj).unwrap().fill_opacity = fill;
        let (doc, src) = t.finish();
        full(&doc, &src).pixels()[0]
    };
    // Exposure(+1) doubles, so the mix runs v -> 2v with the fill weight.
    assert_px(
        brightened(1.0),
        [2.0 * v, 2.0 * v, 2.0 * v, 1.0],
        1e-5,
        "fill 1.0",
    );
    assert_px(
        brightened(0.5),
        [1.5 * v, 1.5 * v, 1.5 * v, 1.0],
        1e-5,
        "fill 0.5",
    );
    assert_px(brightened(0.0), [v, v, v, 1.0], 1e-6, "fill 0.0");
    // ...and it composes with opacity rather than replacing it.
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Grey", [64, 64, 64, 255]);
    let adj = t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
    {
        let l = t.doc.layers.get_mut(adj).unwrap();
        l.opacity = 0.5;
        l.fill_opacity = 0.5;
    }
    let (doc, src) = t.finish();
    assert_px(
        full(&doc, &src).pixels()[0],
        [1.25 * v, 1.25 * v, 1.25 * v, 1.0],
        1e-5,
        "opacity 0.5 x fill 0.5",
    );
}

#[test]
fn an_adjustments_linked_mask_moves_with_its_transform() {
    // An adjustment layer has no pixels, so its transform can only move its
    // mask — and a *linked* mask travels with the layer exactly as a raster
    // layer's content does. Grey document, Exposure(+1) masked in only over
    // the first tile, then translated a whole tile to the right: the brightening
    // must land where the mask was moved to, not where it was painted.
    let mut t = TestDoc::linear(512, 8);
    solid_layer(&mut t, "Grey", [64, 64, 64, 255]);
    let adj = t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
    let mask = t.attach_mask(adj);
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 255);
    t.doc.layers.get_mut(adj).unwrap().transform =
        Affine2::from_translation(Vec2::new(TILE_SIZE as f32, 0.0));
    let (doc, src) = t.finish();

    let v = 64.0 / 255.0;
    let out = full(&doc, &src);
    assert_px(
        out.get(10, 0),
        [v, v, v, 1.0],
        1e-6,
        "the mask left the left half behind",
    );
    assert_px(
        out.get(300, 0),
        [2.0 * v, 2.0 * v, 2.0 * v, 1.0],
        1e-5,
        "the mask moved with the layer",
    );

    // An *unlinked* mask stays in document space, so the same document with the
    // link cut brightens the other half.
    let mut t2 = TestDoc::linear(512, 8);
    solid_layer(&mut t2, "Grey", [64, 64, 64, 255]);
    let adj2 = t2.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
    let mask2 = t2.attach_mask(adj2);
    t2.paint_mask_tile(mask2, TileCoord::new(0, 0, 0), 255);
    {
        let l = t2.doc.layers.get_mut(adj2).unwrap();
        l.transform = Affine2::from_translation(Vec2::new(TILE_SIZE as f32, 0.0));
        l.mask.as_mut().unwrap().linked = false;
    }
    let (doc2, src2) = t2.finish();
    let out2 = full(&doc2, &src2);
    assert_px(
        out2.get(10, 0),
        [2.0 * v, 2.0 * v, 2.0 * v, 1.0],
        1e-5,
        "an unlinked mask did not move",
    );
    assert_px(
        out2.get(300, 0),
        [v, v, v, 1.0],
        1e-6,
        "and nothing beyond it",
    );
}

// ---------------------------------------------------------------------- masks

#[test]
fn a_soft_gradient_mask_produces_the_expected_partial_alpha() {
    let mut t = TestDoc::linear(TILE_SIZE, 4);
    let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
    let mask = t.attach_mask(id);
    t.paint_mask_with(mask, TileCoord::new(0, 0, 0), |x, _| x as u8);
    let (doc, src) = t.finish();

    let out = full(&doc, &src);
    for x in 0..TILE_SIZE as i64 {
        let cov = (x as f32 / 255.0).min(1.0);
        // White is 1.0 in linear, premultiplied by the coverage.
        assert_px(
            out.get(x, 0),
            [cov, cov, cov, cov],
            1e-6,
            &format!("mask coverage at x={x}"),
        );
    }
}

#[test]
fn mask_density_and_inversion_reach_the_composite() {
    let build = |density: f32, inverted: bool| {
        let mut t = TestDoc::linear(4, 4);
        let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
        let mask = t.attach_mask(id);
        t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 0);
        {
            let m = t.doc.layers.get_mut(id).unwrap().mask.as_mut().unwrap();
            m.set_density(density).unwrap();
            m.inverted = inverted;
        }
        let (doc, src) = t.finish();
        full(&doc, &src).pixels()[0][3]
    };

    // A black mask at full density hides everything.
    assert!(build(1.0, false).abs() < 1e-6);
    // Density is a fade of the mask, so half density hides half.
    assert!((build(0.5, false) - 0.5).abs() < 1e-6);
    // Inverted, a black mask reveals.
    assert!((build(1.0, true) - 1.0).abs() < 1e-6);
}

#[test]
fn a_disabled_mask_hides_nothing() {
    let mut t = TestDoc::linear(4, 4);
    let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
    let mask = t.attach_mask(id);
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 0);
    t.doc
        .layers
        .get_mut(id)
        .unwrap()
        .mask
        .as_mut()
        .unwrap()
        .enabled = false;
    let (doc, src) = t.finish();
    assert!((full(&doc, &src).pixels()[0][3] - 1.0).abs() < 1e-6);
}

#[test]
fn a_layer_with_a_mask_but_no_mask_tiles_is_fully_hidden() {
    // An absent mask tile is zero coverage, which `editor-core` documents as
    // "the layer fully hidden" — not "no mask".
    let mut t = TestDoc::linear(4, 4);
    let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
    t.attach_mask(id);
    let (doc, src) = t.finish();
    assert!(full(&doc, &src).pixels().iter().all(|p| *p == [0.0; 4]));
}

#[test]
fn an_unrasterized_vector_mask_is_ignored_rather_than_hiding_the_layer() {
    // A vector mask is a path rasterized on demand and this crate has no
    // rasterizer for one. Reading its coverage tiles finds nothing, and "no
    // tiles" means zero coverage — which would hide the layer completely
    // rather than merely failing to render an extra. Ignoring the mask is the
    // wrong answer in the direction that keeps the content on screen; see the
    // crate docs' "Not yet" list.
    let build = |density: f32| {
        let mut t = TestDoc::linear(4, 4);
        let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
        t.attach_mask(id);
        let mask = t.doc.layers.get_mut(id).unwrap().mask.as_mut().unwrap();
        mask.kind = MaskKind::Vector;
        mask.set_density(density).unwrap();
        let (doc, src) = t.finish();
        full(&doc, &src).pixels()[0]
    };
    assert_px(build(1.0), [1.0; 4], 1e-6, "unrasterized vector mask");
    // Half density is the discriminating case: reading the absent tiles would
    // give coverage 0.5 here, and the mask being ignored outright gives 1.0.
    assert_px(build(0.5), [1.0; 4], 1e-6, "and its density with it");
}

#[test]
fn a_vector_mask_that_has_been_rasterized_into_tiles_is_honoured() {
    // The escape hatch stays open: whatever the kind says, stored coverage
    // tiles are what the compositor reads, so a rasterizer filling them in
    // needs no change here.
    let mut t = TestDoc::linear(4, 4);
    let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
    let mask = t.attach_mask(id);
    t.doc
        .layers
        .get_mut(id)
        .unwrap()
        .mask
        .as_mut()
        .unwrap()
        .kind = MaskKind::Vector;
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 64);
    let (doc, src) = t.finish();
    let got = full(&doc, &src).pixels()[0];
    let k = 64.0 / 255.0;
    assert_px(got, [k, k, k, k], 1e-6, "rasterized vector mask");
}

#[test]
fn a_feathered_mask_softens_a_hard_edge() {
    // 64 rows tall, sampled at row 32, so the row under test sits well clear of
    // the mask tile's own top edge: coverage outside a stored mask tile is
    // genuinely zero, so a feathered mask really does fade at the tile
    // boundary, and row 0 would measure that instead of the vertical edge this
    // test is about.
    let build = |feather: f32| {
        let mut t = TestDoc::linear(TILE_SIZE, 64);
        let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
        let mask = t.attach_mask(id);
        t.paint_mask_with(
            mask,
            TileCoord::new(0, 0, 0),
            |x, _| {
                if x < 128 {
                    255
                } else {
                    0
                }
            },
        );
        t.doc
            .layers
            .get_mut(id)
            .unwrap()
            .mask
            .as_mut()
            .unwrap()
            .set_feather_px(feather)
            .unwrap();
        let (doc, src) = t.finish();
        let out = full(&doc, &src);
        (0..TILE_SIZE as i64)
            .map(|x| out.get(x, 32)[3])
            .collect::<Vec<_>>()
    };

    let hard = build(0.0);
    assert_eq!(hard[127], 1.0, "a hard mask has no ramp");
    assert_eq!(hard[128], 0.0);

    let soft = build(12.0);
    // Far from the edge nothing changed.
    assert!((soft[100] - 1.0).abs() < 1e-4, "{}", soft[100]);
    assert!(soft[160] < 1e-4, "{}", soft[160]);
    // At the edge the step became a ramp centred near a half.
    assert!(
        (soft[127] - 0.5).abs() < 0.2,
        "edge sample is {}",
        soft[127]
    );
    // And it is monotone across the transition.
    for x in 118..138 {
        assert!(
            soft[x] >= soft[x + 1] - 1e-6,
            "not monotone at {x}: {} then {}",
            soft[x],
            soft[x + 1]
        );
    }
}

#[test]
fn a_mask_on_a_group_masks_the_whole_group() {
    let mut t = TestDoc::linear(4, 4);
    let g = t.push_group("G");
    let child = t.push_child(g, Layer::raster("Red"));
    t.fill(child, [255, 0, 0, 255]);
    let mask = t.attach_mask(g);
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 128);
    let (doc, src) = t.finish();
    let a = 128.0 / 255.0;
    assert_px(
        full(&doc, &src).pixels()[0],
        [a, 0.0, 0.0, a],
        1e-6,
        "group mask",
    );
}

// ----------------------------------------------------------------- transforms

#[test]
fn a_translated_layer_moves_by_exactly_the_translation() {
    let mut t = TestDoc::linear(TILE_SIZE, 4);
    let id = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
    t.doc.layers.get_mut(id).unwrap().transform = Affine2::from_translation(Vec2::new(10.0, 0.0));
    let (doc, src) = t.finish();

    let out = full(&doc, &src);
    assert_eq!(out.get(9, 0), [0.0; 4], "nothing before the translation");
    assert_px(out.get(10, 0), [1.0, 0.0, 0.0, 1.0], 1e-6, "moved content");
    assert_px(out.get(200, 0), [1.0, 0.0, 0.0, 1.0], 1e-6, "still red");
}

#[test]
fn an_explicit_identity_transform_is_bit_identical_to_none() {
    let build = |t: Affine2| {
        let mut d = TestDoc::linear(TILE_SIZE, 8);
        let id = d.push_raster("Ramp");
        d.paint_tile_with(id, TileCoord::new(0, 0, 0), |x, y| {
            [x as u8, y as u8, 128, 255]
        });
        d.doc.layers.get_mut(id).unwrap().transform = t;
        let (doc, src) = d.finish();
        full(&doc, &src).pixels().to_vec()
    };
    assert_eq!(
        build(Affine2::IDENTITY),
        build(Affine2::from_translation(Vec2::ZERO)),
        "an identity transform must not resample"
    );
}

#[test]
fn a_non_finite_transform_renders_untransformed_rather_than_vanishing() {
    let mut t = TestDoc::linear(4, 4);
    let id = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
    t.doc.layers.get_mut(id).unwrap().transform =
        Affine2::from_translation(Vec2::new(f32::NAN, 0.0));
    let (doc, src) = t.finish();
    assert_px(
        full(&doc, &src).pixels()[0],
        [1.0, 0.0, 0.0, 1.0],
        1e-6,
        "NaN transform",
    );
}

#[test]
fn a_two_times_scale_doubles_the_content() {
    let mut t = TestDoc::linear(TILE_SIZE, 64);
    let id = t.push_raster("Half");
    // Red for the first 20 columns, transparent afterwards.
    t.paint_tile_with(id, TileCoord::new(0, 0, 0), |x, _| {
        if x < 20 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    t.doc.layers.get_mut(id).unwrap().transform = Affine2::from_scale(Vec2::splat(2.0));
    let (doc, src) = t.finish();

    // Row 10 rather than row 0: a 2x scale samples at half-pixel offsets, so
    // the very first output row bilinearly mixes the layer's top edge with the
    // transparency above it. That is correct, and it is not what this test is
    // about.
    let out = full(&doc, &src);
    assert_px(out.get(10, 10), [1.0, 0.0, 0.0, 1.0], 1e-6, "inside");
    assert_px(out.get(30, 10), [1.0, 0.0, 0.0, 1.0], 1e-6, "still inside");
    assert_eq!(out.get(60, 10), [0.0; 4], "past the doubled extent");
}

#[test]
fn a_downscaled_layer_keeps_the_last_row_and_column_it_stores() {
    // Only what the layer stores is sampled, so the edge of what it stores is
    // where the answer changes. A bound even one pixel short shows up here as a
    // half-transparent last column rather than a solid one, because the
    // bilinear tap that fell outside it read transparency.
    let mut t = TestDoc::linear(TILE_SIZE, TILE_SIZE);
    let id = t.push_raster("Half size");
    t.paint_tile(id, TileCoord::new(0, 0, 0), [255, 0, 0, 255]);
    t.doc.layers.get_mut(id).unwrap().transform = Affine2::from_scale(Vec2::splat(0.5));
    let (doc, src) = t.finish();

    let out = full(&doc, &src);
    // The tile is 256 wide, halved to 128: column 127 samples source column 255,
    // the last one stored, and must be as solid as any other.
    assert_px(out.get(127, 60), [1.0, 0.0, 0.0, 1.0], 1e-6, "last column");
    assert_px(out.get(60, 127), [1.0, 0.0, 0.0, 1.0], 1e-6, "last row");
    assert_px(out.get(126, 126), [1.0, 0.0, 0.0, 1.0], 1e-6, "inside");
    assert_eq!(out.get(128, 60), [0.0; 4], "past the halved extent");
}

#[test]
fn a_layer_scaled_far_down_still_composites_and_leaves_the_rest_of_the_document_alone() {
    // Dragging a placed image small is an ordinary edit. The pre-image of one
    // output tile under a 1:50 scale is 12800x12800 — past the canvas ceiling —
    // so an implementation that allocates it refuses the whole frame and the
    // renderer gets no pixels at all, not even the layers that are fine.
    for scale in [0.05f32, 0.03, 0.02, 0.01] {
        let mut t = TestDoc::linear(1024, 512);
        let under = solid_layer(&mut t, "Blue", [0, 0, 255, 255]);
        let over = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
        t.doc.layers.get_mut(over).unwrap().transform = Affine2::from_scale(Vec2::splat(scale));
        assert_ne!(under, over);
        let (doc, src) = t.finish();

        let region = rect(0, 0, 1024, 512);
        let out = composite_region(&doc, &src, region, 0, opts())
            .unwrap_or_else(|e| panic!("scale {scale}: {e}"));
        // `composite_rect` runs the traversal in one pass over the whole rect,
        // which is where the pre-image is largest.
        let direct =
            composite_rect(&doc, &src, region, 0, opts()).unwrap_or_else(|e| panic!("{e}"));

        // The layer still lands where the transform says: a document pixel `p`
        // samples the layer at `p / scale`, so everything inside
        // `1024 * scale` wide is the minified red.
        let inside = ((512.0 * scale) as i64).max(1) - 1;
        assert_px(
            out.get(inside, 0),
            [1.0, 0.0, 0.0, 1.0],
            1e-6,
            &format!("scale {scale}: minified layer at x={inside}"),
        );
        assert_px(
            direct.get(inside, 0),
            [1.0, 0.0, 0.0, 1.0],
            1e-6,
            &format!("scale {scale}: same by composite_rect"),
        );
        // Past its minified extent the layer beneath is untouched.
        let outside = (1024.0 * scale) as i64 + 4;
        assert_px(
            out.get(outside, 300),
            [0.0, 0.0, 1.0, 1.0],
            1e-6,
            &format!("scale {scale}: the other layer at x={outside}"),
        );
    }
}

#[test]
fn an_extreme_anisotropic_transform_collapses_to_nothing_instead_of_aborting_the_frame() {
    // Finite (so it is not treated as the identity) and determinant 1.0 (so it
    // is not singular), but a tile maps back to something 4e30 pixels wide. The
    // layer has to vanish, quietly, on a rayon worker in the middle of a frame.
    let mut t = TestDoc::linear(TILE_SIZE, 64);
    solid_layer(&mut t, "Blue", [0, 0, 255, 255]);
    let over = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
    t.doc.layers.get_mut(over).unwrap().transform = Affine2::from_scale(Vec2::new(1e-30, 1e30));
    let (doc, src) = t.finish();

    let region = rect(0, 0, TILE_SIZE, 64);
    for (label, out) in [
        ("region", composite_region(&doc, &src, region, 0, opts())),
        ("rect", composite_rect(&doc, &src, region, 0, opts())),
        (
            "subtree",
            composite_subtree(&doc, &src, over, region, 0, opts()),
        ),
    ] {
        let out = out.unwrap_or_else(|e| panic!("{label}: {e}"));
        let want = if label == "subtree" {
            [0.0; 4]
        } else {
            [0.0, 0.0, 1.0, 1.0]
        };
        for p in out.pixels() {
            assert_px(*p, want, 1e-6, label);
        }
    }
    // And through the cached, tile-parallel path, which is where the panic
    // would have taken out a worker.
    let mut tc = TileCompositor::new();
    let cached = tc.composite_region(&doc, &src, region, 0, opts()).unwrap();
    assert_px(cached.pixels()[0], [0.0, 0.0, 1.0, 1.0], 1e-6, "cached");
}

#[test]
fn a_minified_layer_whose_content_is_too_wide_to_hold_at_once_is_still_exact() {
    // Two tiles, 2304 pixels apart, so the layer's stored extent is 2560x2560:
    // at 1:10 the pre-image of an output tile is 2562x2562 and clipping it to
    // what the layer stores leaves 6.5 Mpx, past `MAX_PREIMAGE_PIXELS`. That
    // forces the destination-splitting path, and a split that reassembled
    // wrongly would show up as a seam or a shifted image here.
    let mut t = TestDoc::linear(TILE_SIZE, TILE_SIZE);
    let id = t.push_raster("Far apart");
    t.paint_tile(id, TileCoord::new(0, 0, 0), [255, 0, 0, 255]);
    // Source x 1024..1280 and 1280..1536: the destination's first split falls at
    // x = 128, which is exactly the join between them. Without content on both
    // sides of the seam, a split that dropped or shifted a column would go
    // unnoticed.
    t.paint_tile(id, TileCoord::new(4, 0, 0), [0, 0, 255, 255]);
    t.paint_tile(id, TileCoord::new(5, 0, 0), [0, 255, 255, 255]);
    t.paint_tile(id, TileCoord::new(9, 9, 0), [0, 255, 0, 255]);
    t.doc.layers.get_mut(id).unwrap().transform = Affine2::from_scale(Vec2::splat(0.1));
    let (doc, src) = t.finish();

    let region = rect(0, 0, TILE_SIZE, TILE_SIZE);
    let whole = composite_rect(&doc, &src, region, 0, opts()).unwrap();
    // Tile (0,0) covers source 0..256, which lands in 0..25.6.
    assert_px(whole.get(5, 5), [1.0, 0.0, 0.0, 1.0], 1e-6, "near tile");
    // Tile (9,9) covers source 2304..2560, which lands in 230.4..256.
    assert_px(whole.get(240, 240), [0.0, 1.0, 0.0, 1.0], 1e-6, "far tile");
    // Tiles (4,0) and (5,0) meet exactly at the split at x = 128.
    for x in 124..128 {
        assert_px(
            whole.get(x, 10),
            [0.0, 0.0, 1.0, 1.0],
            1e-6,
            &format!("left of the split at x={x}"),
        );
    }
    for x in 128..132 {
        assert_px(
            whole.get(x, 10),
            [0.0, 1.0, 1.0, 1.0],
            1e-6,
            &format!("right of the split at x={x}"),
        );
    }
    // The gap between them stores nothing.
    assert_eq!(whole.get(100, 100), [0.0; 4], "no tile there");

    // Sub-windows split at different places than the whole rect does, so
    // agreeing pixel for pixel is what proves the split is a decomposition
    // rather than a different picture.
    for window in [
        rect(0, 0, 40, 40),
        rect(120, 0, 40, 40),
        rect(120, 120, 40, 40),
        rect(228, 228, 30, 30),
    ] {
        let part = composite_rect(&doc, &src, window, 0, opts()).unwrap();
        let want = whole.sub(window).unwrap();
        for (i, (got, expect)) in part.pixels().iter().zip(want.pixels()).enumerate() {
            assert_px(*got, *expect, 1e-6, &format!("window {window:?} pixel {i}"));
        }
    }
    // And the tiled path agrees with the one-pass path.
    let tiled = composite_region(&doc, &src, region, 0, opts()).unwrap();
    for (i, (a, b)) in tiled.pixels().iter().zip(whole.pixels()).enumerate() {
        assert_px(*a, *b, 1e-6, &format!("tiled pixel {i}"));
    }
}

/// A layer whose stored tiles are thousands of tiles apart, scaled down so far
/// that even a single destination pixel's pre-image cannot be held.
fn wildly_minified() -> TestDoc {
    let mut t = TestDoc::linear(4, 4);
    let id = t.push_raster("Speck");
    // At 1:10000 the pixel at (0,0) samples source (5000, 5000), which is
    // inside tile (19,19).
    t.paint_tile(id, TileCoord::new(19, 19, 0), [255, 0, 0, 255]);
    // Far away, purely to make the layer's stored extent enormous.
    t.paint_tile(id, TileCoord::new(999, 999, 0), [0, 255, 0, 255]);
    t.doc.layers.get_mut(id).unwrap().transform = Affine2::from_scale(Vec2::splat(1.0e-4));
    t
}

#[test]
fn a_single_pixel_whose_preimage_cannot_be_held_still_samples_the_right_place() {
    // The end of the line for bounding a pre-image: one destination pixel maps
    // back to a 10000x10000 region, and splitting the destination further is
    // not possible. Only the samples bilinear can actually read are kept, which
    // is a window at the centre of that region — so the pixel still shows what
    // the transform says it should.
    let t = wildly_minified();
    let (doc, src) = t.finish();
    let out = composite_rect(&doc, &src, rect(0, 0, 2, 2), 0, opts()).unwrap();
    assert_px(
        out.get(0, 0),
        [1.0, 0.0, 0.0, 1.0],
        1e-6,
        "sampled tile (19,19)",
    );
    // (1,1) samples source (15000, 15000), where the layer stores nothing.
    assert_eq!(out.get(1, 1), [0.0; 4], "nothing stored there");
}

#[test]
fn a_minified_layers_cache_key_still_notices_a_repaint() {
    // The sampled rect spans hundreds of thousands of tile coordinates, so the
    // key cannot enumerate them one at a time. Whatever it hashes instead has
    // to still change when the bytes behind a tile change, or the cache serves
    // a stale picture.
    let mut t = wildly_minified();
    let id = t.doc.layers.root()[0];
    let region = rect(0, 0, 4, 4);

    let mut tc = TileCompositor::new();
    let before = tc
        .composite_region(&t.doc, &t.src, region, 0, opts())
        .unwrap();
    assert_px(before.get(0, 0), [1.0, 0.0, 0.0, 1.0], 1e-6, "red to start");

    t.paint_tile(id, TileCoord::new(19, 19, 0), [0, 0, 255, 255]);
    tc.reset_stats();
    let after = tc
        .composite_region(&t.doc, &t.src, region, 0, opts())
        .unwrap();
    assert_eq!(tc.stats(), CacheStats { hits: 0, misses: 1 });
    assert_px(
        after.get(0, 0),
        [0.0, 0.0, 1.0, 1.0],
        1e-6,
        "repaint is visible",
    );
}

// -------------------------------------------------------- canvas and extents

#[test]
fn pixels_outside_the_canvas_are_not_part_of_the_image() {
    // A 100x100 document stores one 256x256 tile; a full-tile fill writes the
    // padding too. None of it is the image.
    let mut t = TestDoc::linear(100, 100);
    let id = t.push_raster("Red");
    t.paint_tile(id, TileCoord::new(0, 0, 0), [255, 0, 0, 255]);
    let (doc, src) = t.finish();

    let out = composite_rect(&doc, &src, rect(0, 0, TILE_SIZE, TILE_SIZE), 0, opts()).unwrap();
    assert_px(
        out.get(99, 99),
        [1.0, 0.0, 0.0, 1.0],
        1e-6,
        "last real pixel",
    );
    assert_eq!(out.get(100, 0), [0.0; 4], "padding is not image content");
    assert_eq!(out.get(0, 100), [0.0; 4]);
}

#[test]
fn a_region_outside_the_document_composites_to_nothing() {
    let mut t = TestDoc::linear(64, 64);
    solid_layer(&mut t, "Red", [255, 0, 0, 255]);
    let (doc, src) = t.finish();
    let out = composite_region(&doc, &src, rect(500, 500, 32, 32), 0, opts()).unwrap();
    assert!(out.pixels().iter().all(|p| *p == [0.0; 4]));
}

#[test]
fn an_empty_region_is_an_empty_canvas() {
    let mut t = TestDoc::linear(64, 64);
    solid_layer(&mut t, "Red", [255, 0, 0, 255]);
    let (doc, src) = t.finish();
    let out = composite_region(&doc, &src, rect(0, 0, 0, 0), 0, opts()).unwrap();
    assert!(out.pixels().is_empty());
}

// ------------------------------------------------------------------ mip level

#[test]
fn a_mip_level_reads_the_tiles_stored_at_that_level() {
    let mut t = TestDoc::linear(512, 512);
    let id = t.push_raster("Half res");
    // Only a level-1 tile exists.
    t.paint_tile(id, TileCoord::new(0, 0, 1), [255, 0, 0, 255]);
    let (doc, src) = t.finish();

    let level1 = composite_rect(&doc, &src, rect(0, 0, 256, 256), 1, opts()).unwrap();
    assert_px(level1.pixels()[0], [1.0, 0.0, 0.0, 1.0], 1e-6, "level 1");

    // Level 0 has no tiles of its own; the compositor reads levels, it does not
    // synthesise them.
    let level0 = composite_rect(&doc, &src, rect(0, 0, 256, 256), 0, opts()).unwrap();
    assert!(level0.pixels().iter().all(|p| *p == [0.0; 4]));
}

#[test]
fn a_level_past_the_mip_chain_is_refused() {
    let t = TestDoc::linear(512, 512);
    let (doc, src) = t.finish();
    // level_count(512, 512) == 10, so 10 is one past the end.
    assert_eq!(raster::mipmap::level_count(512, 512), 10, "premise");
    let err = composite_rect(&doc, &src, rect(0, 0, 4, 4), 10, opts()).unwrap_err();
    assert_eq!(
        err,
        CompositeError::NoSuchLevel {
            level: 10,
            width: 512,
            height: 512
        }
    );
    assert!(composite_rect(&doc, &src, rect(0, 0, 4, 4), 9, opts()).is_ok());
}

#[test]
fn a_transform_translation_shrinks_with_the_mip_level() {
    let mut t = TestDoc::linear(512, 8);
    let id = t.push_raster("Moved");
    t.paint_tile(id, TileCoord::new(0, 0, 1), [255, 0, 0, 255]);
    t.doc.layers.get_mut(id).unwrap().transform = Affine2::from_translation(Vec2::new(20.0, 0.0));
    let (doc, src) = t.finish();

    // 20 document pixels is 10 pixels at level 1.
    let out = composite_rect(&doc, &src, rect(0, 0, 256, 4), 1, opts()).unwrap();
    assert_eq!(out.get(9, 0), [0.0; 4], "still before the shifted content");
    assert_px(out.get(10, 0), [1.0, 0.0, 0.0, 1.0], 1e-6, "shifted by 10");
}

// -------------------------------------------------------- region independence

/// A document exercising most of the traversal at once: stacked rasters, a
/// masked and feathered layer, a group with its own blend mode and opacity, an
/// adjustment layer, a clipping group, and a translated layer.
fn busy_document() -> (Document, MemoryTileSource) {
    let mut t = TestDoc::new(512, 128);

    let bottom = t.push_raster("Gradient");
    for tx in 0..2 {
        t.paint_tile_with(bottom, TileCoord::new(tx, 0, 0), move |x, y| {
            [(x / 2) as u8, y as u8, (128 + tx * 40) as u8, 255]
        });
    }

    t.push_adjustment(
        "Levels",
        AdjustmentKind::Levels {
            black: 0.1,
            white: 0.85,
            gamma: 1.3,
        },
    );

    let g = t.push_group("Group");
    let inner = t.push_child(g, Layer::raster("Inner"));
    t.fill(inner, [30, 200, 90, 255]);
    let masked = t.push_child(g, Layer::raster("Masked"));
    t.fill(masked, [255, 40, 10, 255]);
    let mask = t.attach_mask(masked);
    for tx in 0..2 {
        t.paint_mask_with(mask, TileCoord::new(tx, 0, 0), move |x, _| {
            ((x + tx as u32 * 97) % 256) as u8
        });
    }
    t.doc
        .layers
        .get_mut(masked)
        .unwrap()
        .mask
        .as_mut()
        .unwrap()
        .set_feather_px(6.0)
        .unwrap();
    {
        let group = t.doc.layers.get_mut(g).unwrap();
        group.blend_mode = BlendMode::Overlay;
        group.opacity = 0.7;
    }

    let base = t.push_raster("Clip base");
    for tx in 0..2 {
        t.paint_tile_with(base, TileCoord::new(tx, 0, 0), |x, _| {
            [10, 10, 240, (x % 256) as u8]
        });
    }
    let clipped = t.push_raster("Clipped");
    t.fill(clipped, [250, 250, 40, 200]);
    {
        let l = t.doc.layers.get_mut(clipped).unwrap();
        l.clipping = ClippingMode::ClipToBelow;
        l.blend_mode = BlendMode::Multiply;
    }

    // A transformed adjustment with a linked, feathered mask: the mask is read
    // in the layer's own space and resampled forward, so this is the arrangement
    // that catches a region-dependent pre-image or feather halo.
    let tinted = t.push_adjustment(
        "Tint",
        AdjustmentKind::HueSaturation {
            hue: 20.0,
            saturation: 0.3,
            lightness: 0.0,
        },
    );
    let tint_mask = t.attach_mask(tinted);
    for tx in 0..2 {
        t.paint_mask_with(tint_mask, TileCoord::new(tx, 0, 0), move |x, y| {
            ((x * 3 + y + tx as u32 * 61) % 256) as u8
        });
    }
    {
        let l = t.doc.layers.get_mut(tinted).unwrap();
        l.transform = Affine2::from_translation(Vec2::new(-23.5, 7.25));
        l.mask.as_mut().unwrap().set_feather_px(3.0).unwrap();
    }

    let moved = t.push_raster("Moved");
    t.paint_tile(moved, TileCoord::new(0, 0, 0), [90, 20, 200, 120]);
    {
        let l = t.doc.layers.get_mut(moved).unwrap();
        l.transform = Affine2::from_translation(Vec2::new(137.0, 5.0));
        l.opacity = 0.6;
    }

    t.finish()
}

#[test]
fn compositing_a_region_equals_the_sub_rect_of_compositing_everything() {
    let (doc, src) = busy_document();
    let whole = composite_region(&doc, &src, rect(0, 0, 512, 128), 0, opts()).unwrap();

    for window in [
        rect(0, 0, 17, 13),
        rect(200, 30, 120, 60),
        // Straddling the tile boundary at x = 256, which is where a tiling bug
        // shows up.
        rect(250, 0, 20, 128),
        rect(400, 100, 200, 100),
        rect(-40, -10, 80, 40),
    ] {
        let want = whole.sub(window).unwrap();
        // Both paths, because they are independent in different ways:
        // `composite_region` re-assembles the same tiles, while `composite_rect`
        // runs the whole traversal over a rect that is not tile-aligned — which
        // is what catches anything that quietly depends on a buffer-local
        // coordinate rather than an image-space one.
        for (label, part) in [
            (
                "region",
                composite_region(&doc, &src, window, 0, opts()).unwrap(),
            ),
            (
                "rect",
                composite_rect(&doc, &src, window, 0, opts()).unwrap(),
            ),
        ] {
            for (i, (got, expect)) in part.pixels().iter().zip(want.pixels()).enumerate() {
                assert_px(
                    *got,
                    *expect,
                    1e-6,
                    &format!("{label} window {window:?} pixel {i}"),
                );
            }
        }
    }
}

#[test]
fn tiling_does_not_change_the_answer() {
    // `composite_rect` runs the traversal once over the whole rect;
    // `composite_region` runs it per tile and reassembles. Same picture.
    let (doc, src) = busy_document();
    let region = rect(0, 0, 512, 128);
    let direct = composite_rect(&doc, &src, region, 0, opts()).unwrap();
    let tiled = composite_region(&doc, &src, region, 0, opts()).unwrap();
    for (i, (a, b)) in direct.pixels().iter().zip(tiled.pixels()).enumerate() {
        assert_px(*a, *b, 1e-6, &format!("pixel {i}"));
    }
}

#[test]
fn dissolve_is_stable_across_regions() {
    // Dissolve's noise is hashed from the absolute image coordinate. Hashing a
    // tile-local one would make the pattern jump every time the viewport moved.
    let mut t = TestDoc::linear(512, 8);
    solid_layer(&mut t, "Backdrop", [0, 0, 0, 255]);
    let top = solid_layer(&mut t, "Dissolving", [255, 255, 255, 255]);
    {
        let l = t.doc.layers.get_mut(top).unwrap();
        l.blend_mode = BlendMode::Dissolve;
        l.opacity = 0.5;
    }
    let (doc, src) = t.finish();

    let whole = composite_region(&doc, &src, rect(0, 0, 512, 8), 0, opts()).unwrap();
    let window = rect(200, 2, 100, 4);
    assert_eq!(
        composite_region(&doc, &src, window, 0, opts()).unwrap(),
        whole.sub(window).unwrap()
    );
    // The untiled path too: its buffer starts at x = 200, so a noise function
    // keyed on a buffer-local coordinate would give a different pattern here
    // and an identical one in every tile.
    assert_eq!(
        composite_rect(&doc, &src, window, 0, opts()).unwrap(),
        whole.sub(window).unwrap()
    );

    // And it really did dissolve rather than doing nothing.
    let lit = whole.pixels().iter().filter(|p| p[0] > 0.5).count();
    let total = whole.pixels().len();
    assert!(
        lit > total / 4 && lit < total * 3 / 4,
        "{lit} of {total} pixels kept"
    );
}

// ------------------------------------------------------------- cache equality

#[test]
fn a_dirty_tile_recomposite_equals_a_full_recomposite() {
    let (doc, src) = busy_document();
    let region = rect(0, 0, 512, 128);

    let mut tc = TileCompositor::new();
    let before = tc.composite_region(&doc, &src, region, 0, opts()).unwrap();
    assert_eq!(tc.stats(), CacheStats { hits: 0, misses: 2 });

    // Repaint one tile of the bottom layer.
    let bottom = *doc.layers.root().last().unwrap();
    let mut edited = TestDoc {
        doc: doc.clone(),
        src: src.clone(),
    };
    edited.paint_tile(bottom, TileCoord::new(1, 0, 0), [200, 10, 10, 255]);
    let (doc2, src2) = edited.finish();

    tc.reset_stats();
    // Mark only the tile that changed. (`invalidate_layer` is deliberately
    // coarser — it drops every tile the layer covers — so it would recompute
    // both tiles and prove nothing about incrementality.)
    assert!(tc.invalidate_tile(TileCoord::new(1, 0, 0)));
    let incremental = tc
        .composite_region(&doc2, &src2, region, 0, opts())
        .unwrap();

    // A cold compositor, same document: the answers must be identical.
    let mut cold = TileCompositor::new();
    let fresh = cold
        .composite_region(&doc2, &src2, region, 0, opts())
        .unwrap();
    assert_eq!(incremental, fresh, "incremental composite drifted");
    assert_ne!(incremental, before, "the edit must be visible");

    // And the untouched tile really was reused rather than recomputed.
    assert_eq!(tc.stats(), CacheStats { hits: 1, misses: 1 });
}

#[test]
fn a_cached_composite_matches_the_uncached_one() {
    let (doc, src) = busy_document();
    let region = rect(0, 0, 512, 128);
    let plain = composite_region(&doc, &src, region, 0, opts()).unwrap();
    let mut tc = TileCompositor::new();
    let cached = tc.composite_region(&doc, &src, region, 0, opts()).unwrap();
    assert_eq!(plain, cached);
    // Second time round, entirely from cache.
    assert_eq!(
        tc.composite_region(&doc, &src, region, 0, opts()).unwrap(),
        plain
    );
}

#[test]
fn painting_a_new_tile_into_an_inverted_linked_mask_is_never_served_stale() {
    // An inverted mask covers everything its tiles do *not*, so the tiles it
    // holds bound nothing: a tile painted anywhere in the pre-image changes the
    // picture. A key that hashed only the neighbourhood of the tiles already
    // stored would go stale exactly here.
    let mut t = TestDoc::linear(TILE_SIZE, 8);
    solid_layer(&mut t, "Grey", [64, 64, 64, 255]);
    let adj = t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
    let mask = t.attach_mask(adj);
    t.doc
        .layers
        .get_mut(adj)
        .unwrap()
        .mask
        .as_mut()
        .unwrap()
        .inverted = true;
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 255);
    // Half scale, so the visible tile's pre-image reaches mask tile (1, 0) —
    // which holds nothing yet, and under inversion therefore covers fully.
    t.doc.layers.get_mut(adj).unwrap().transform = Affine2::from_scale(Vec2::splat(0.5));

    let region = rect(0, 0, TILE_SIZE, 8);
    let v = 64.0 / 255.0;
    let mut tc = TileCompositor::new();
    let before = tc
        .composite_region(&t.doc, &t.src, region, 0, opts())
        .unwrap();
    // Left half: mask tile (0,0) reads 255, inverted to 0, so no brightening.
    assert_px(before.get(10, 0), [v, v, v, 1.0], 1e-6, "masked out");
    // Right half: no mask tile at all, inverted to full coverage.
    assert_px(
        before.get(200, 0),
        [2.0 * v, 2.0 * v, 2.0 * v, 1.0],
        1e-5,
        "uncovered, so brightened",
    );

    // Paint the tile the right half samples. Nothing calls `invalidate_*`.
    t.paint_mask_tile(mask, TileCoord::new(1, 0, 0), 255);
    let after = tc
        .composite_region(&t.doc, &t.src, region, 0, opts())
        .unwrap();
    let fresh = TileCompositor::new()
        .composite_region(&t.doc, &t.src, region, 0, opts())
        .unwrap();
    assert_eq!(after, fresh, "the cache served a stale tile");
    assert_ne!(after, before, "the new mask tile must be visible");
    assert_px(
        after.get(200, 0),
        [v, v, v, 1.0],
        1e-6,
        "now masked out too",
    );
}

#[test]
fn repainting_a_transformed_adjustments_linked_mask_is_never_served_stale() {
    // The regression this crate was rejected for. One document, one MaskId:
    // the mask tile's *bytes* are repainted and nothing calls `invalidate_*`,
    // so only the cache key can notice. It can only notice if the rect it
    // hashes is the rect the traversal reads — which for a linked mask on a
    // transformed adjustment layer is the pre-image, not the document rect.
    let mut t = TestDoc::linear(1024, 8);
    solid_layer(&mut t, "Grey", [64, 64, 64, 255]);
    let adj = t.push_adjustment("Brighten", AdjustmentKind::Exposure { stops: 1.0 });
    let mask = t.attach_mask(adj);
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 255);
    // Two tiles right: far enough that the pre-image of the leftmost tile and
    // the document rect of that same tile share no mask tile at all, which is
    // exactly the arrangement in which a wrongly-chosen hash rect goes stale.
    t.doc.layers.get_mut(adj).unwrap().transform =
        Affine2::from_translation(Vec2::new(2.0 * TILE_SIZE as f32, 0.0));

    let region = rect(0, 0, 1024, 8);
    let v = 64.0 / 255.0;
    let mut tc = TileCompositor::new();
    let before = tc
        .composite_region(&t.doc, &t.src, region, 0, opts())
        .unwrap();
    assert_px(before.get(10, 0), [v, v, v, 1.0], 1e-6, "left of the mask");
    assert_px(
        before.get(600, 0),
        [2.0 * v, 2.0 * v, 2.0 * v, 1.0],
        1e-5,
        "under the moved mask",
    );

    // Repaint that one mask tile black. Same layer, same mask id, same
    // coordinate — only the content hash changes.
    t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 0);
    let after = tc
        .composite_region(&t.doc, &t.src, region, 0, opts())
        .unwrap();

    let mut cold = TileCompositor::new();
    let fresh = cold
        .composite_region(&t.doc, &t.src, region, 0, opts())
        .unwrap();
    assert_eq!(after, fresh, "the cache served a stale tile");
    assert_ne!(after, before, "the repaint must be visible");
    for x in [10i64, 600] {
        assert_px(
            after.get(x, 0),
            [v, v, v, 1.0],
            1e-6,
            "a black mask brightens nothing",
        );
    }
}

#[test]
fn changing_the_blend_space_option_invalidates_cached_tiles() {
    let mut t = TestDoc::new(256, 256);
    solid_layer(&mut t, "Backdrop", [128, 128, 128, 255]);
    let top = solid_layer(&mut t, "Source", [128, 128, 128, 255]);
    t.doc.layers.get_mut(top).unwrap().blend_mode = BlendMode::Multiply;
    let (doc, src) = t.finish();

    let region = rect(0, 0, 256, 256);
    let mut tc = TileCompositor::new();
    let linear = tc.composite_region(&doc, &src, region, 0, opts()).unwrap();
    let encoded = tc
        .composite_region(
            &doc,
            &src,
            region,
            0,
            CompositeOptions {
                blend_space: BlendSpace::Encoded,
                ..opts()
            },
        )
        .unwrap();
    assert_ne!(linear, encoded, "the option is part of the cache key");
    assert_eq!(tc.stats(), CacheStats { hits: 0, misses: 2 });
}

// -------------------------------------------------------------------- subtree

#[test]
fn compositing_a_subtree_ignores_everything_else() {
    let mut t = TestDoc::linear(4, 4);
    solid_layer(&mut t, "Backdrop", [255, 0, 0, 255]);
    let g = t.push_group("G");
    let child = t.push_child(g, Layer::raster("Blue"));
    t.fill(child, [0, 0, 255, 255]);
    t.doc.layers.get_mut(g).unwrap().opacity = 0.5;
    let (doc, src) = t.finish();

    let sub = composite_subtree(&doc, &src, g, rect(0, 0, 4, 4), 0, opts()).unwrap();
    assert_px(
        sub.pixels()[0],
        [0.0, 0.0, 0.5, 0.5],
        1e-6,
        "the group alone, over nothing",
    );

    let missing = composite_subtree(&doc, &src, LayerId::new(), rect(0, 0, 4, 4), 0, opts());
    assert!(matches!(missing, Err(CompositeError::LayerNotFound(_))));
}

#[test]
fn a_document_with_no_layers_composites_to_transparency() {
    let t = TestDoc::linear(16, 16);
    let (doc, src) = t.finish();
    assert!(full(&doc, &src).pixels().iter().all(|p| *p == [0.0; 4]));
}

#[test]
fn a_zero_area_document_has_no_pixels_at_any_level() {
    let mut t = TestDoc::linear(0, 0);
    let id = t.push_raster("Red");
    t.paint_tile(id, TileCoord::new(0, 0, 0), [255, 0, 0, 255]);
    let (doc, src) = t.finish();
    let out = composite_rect(&doc, &src, rect(0, 0, 8, 8), 0, opts()).unwrap();
    assert!(out.pixels().iter().all(|p| *p == [0.0; 4]));
}

#[test]
fn a_p3_document_decodes_through_its_own_space() {
    let mut a = TestDoc::new(4, 4);
    a.doc.meta.color_space = ColorSpace::DisplayP3;
    solid_layer(&mut a, "Red", [255, 0, 0, 255]);
    let (p3_doc, p3_src) = a.finish();

    let mut b = TestDoc::new(4, 4);
    solid_layer(&mut b, "Red", [255, 0, 0, 255]);
    let (srgb_doc, srgb_src) = b.finish();

    let p3 = full(&p3_doc, &p3_src).pixels()[0];
    let srgb = full(&srgb_doc, &srgb_src).pixels()[0];
    assert_ne!(p3, srgb, "P3 red is a wider red than sRGB red");
    // Both are opaque and both are red-dominant; only the working values move.
    assert_eq!(p3[3], 1.0);
    assert!(p3[0] > p3[1] && p3[0] > p3[2]);
}
