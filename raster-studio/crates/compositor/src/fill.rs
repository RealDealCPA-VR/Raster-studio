//! W9-B: evaluating a live fill layer ([`layer_model::LayerKind::Fill`]).
//!
//! A fill layer has no tiles. Its pixels are a pure function of its
//! parameters and the pixel's **document** position, so any region can be
//! evaluated on its own and the answer never depends on which region asked —
//! the region-independence the rest of the traversal relies on.
//!
//! Like Photopea's fill layers it is unbounded: the fill covers the whole
//! canvas whatever the layer's transform, and only its layer mask (which does
//! follow the transform, through the mask's own pose) shapes it. The gradient
//! is fitted to the document and the pattern anchored at the document origin
//! (or, with "link with layer", at the layer's origin), so moving the layer
//! moves its mask and not its paint — again as Photopea draws it.
//!
//! Output is what every other content path writes into a [`Canvas`]:
//! premultiplied linear RGBA.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;

use color::{to_linear, ColorSpace};
use layer_model::{
    blend::unit, FillLayer, FillSource, Gradient, GradientFill, GradientStyle, PatternFill,
    PatternTile,
};
use raster::PixelRect;

use super::{hash_f32, hash_gradient, hash_rgba};
use crate::canvas::Canvas;

/// What the fill needs to know about where it is being drawn.
pub(crate) struct FillPlacement<'a> {
    pub space: &'a ColorSpace,
    /// The document canvas at this level: what a gradient is fitted to.
    pub doc: PixelRect,
    /// Level pixels per document pixel (`2^-level`).
    pub scale: f32,
    /// The layer's own origin in level pixels: the anchor of a pattern that
    /// is linked with the layer.
    pub layer_origin: [f32; 2],
}

/// Paint `fill` into every pixel of `out`.
pub(crate) fn paint(fill: &FillLayer, out: &mut Canvas, at: &FillPlacement<'_>) {
    match &fill.source {
        FillSource::Solid { color } => {
            let a = unit(color[3]);
            let lin = to_linear(at.space, [unit(color[0]), unit(color[1]), unit(color[2])]);
            let px = [lin[0] * a, lin[1] * a, lin[2] * a, a];
            for d in out.pixels_mut() {
                *d = px;
            }
        }
        FillSource::Gradient(g) => paint_gradient(g, out, at),
        FillSource::Pattern(p) => paint_pattern(p, out, at),
    }
}

/// Every parameter of `fill` that can change a pixel, into a tile cache key.
pub(crate) fn hash_fill(fill: &FillLayer, h: &mut DefaultHasher) {
    match &fill.source {
        FillSource::Solid { color } => {
            0u8.hash(h);
            hash_rgba(*color, h);
        }
        FillSource::Gradient(g) => {
            1u8.hash(h);
            hash_gradient(&g.gradient, h);
            g.style.hash(h);
            hash_f32(g.angle_deg, h);
            hash_f32(g.scale, h);
            g.reverse.hash(h);
            g.dither.hash(h);
            hash_f32(g.offset_px[0], h);
            hash_f32(g.offset_px[1], h);
        }
        FillSource::Pattern(p) => {
            2u8.hash(h);
            p.tile
                .as_ref()
                .map(|t| (t.content_hash(), t.width(), t.height()))
                .hash(h);
            hash_f32(p.scale, h);
            hash_f32(p.offset_px[0], h);
            hash_f32(p.offset_px[1], h);
            hash_f32(p.angle_deg, h);
            p.link_with_layer.hash(h);
        }
    }
}

fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

/// A small integer hash of a pixel position, in `0.0..1.0`: the dither's
/// noise, stable across frames and regions.
fn noise(x: i64, y: i64) -> f32 {
    let mut v = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    v ^= v >> 29;
    v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v ^= v >> 32;
    (v >> 40) as f32 / (1u64 << 24) as f32
}

fn paint_gradient(g: &GradientFill, out: &mut Canvas, at: &FillPlacement<'_>) {
    let fit = at.doc;
    if fit.is_empty() {
        return;
    }
    let ramp = Ramp::new(&g.gradient, at.space);
    let cx = fit.x as f32 + fit.width as f32 * 0.5 + finite_or(g.offset_px[0], 0.0) * at.scale;
    let cy = fit.y as f32 + fit.height as f32 * 0.5 + finite_or(g.offset_px[1], 0.0) * at.scale;
    let scale = if g.scale.is_finite() && g.scale > 0.0 {
        g.scale
    } else {
        1.0
    };
    let extent = (fit.width.max(fit.height) as f32 * 0.5 * scale).max(1.0);
    let a = finite_or(g.angle_deg, 0.0).to_radians();
    let (ca, sa) = (a.cos(), a.sin());
    // Half a code value of an 8-bit ramp, spread over the ramp's length: the
    // amount of position jitter that breaks a band without adding grain.
    let dither = if g.dither { 1.0 / 255.0 } else { 0.0 };
    let rect = out.rect();
    let w = rect.width as usize;
    for (i, d) in out.pixels_mut().iter_mut().enumerate() {
        let px = rect.x + (i % w) as i64;
        let py = rect.y + (i / w) as i64;
        let x = px as f32 + 0.5 - cx;
        let y = py as f32 + 0.5 - cy;
        // Image y grows downward, so a positive angle still runs upward.
        let u = x * ca - y * sa;
        let v = x * sa + y * ca;
        let mut t = match g.style {
            GradientStyle::Linear => 0.5 + u / (2.0 * extent),
            GradientStyle::Reflected => (u / extent).abs(),
            GradientStyle::Radial => (u * u + v * v).sqrt() / extent,
            GradientStyle::Diamond => (u.abs() + v.abs()) / extent,
            GradientStyle::Angle => 0.5 + v.atan2(u) / std::f32::consts::TAU,
        };
        if g.reverse {
            t = 1.0 - t;
        }
        if dither > 0.0 {
            t += (noise(px, py) - 0.5) * dither;
        }
        let rgb = ramp.rgb(t);
        let alpha = ramp.alpha(t);
        *d = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
    }
}

fn paint_pattern(p: &PatternFill, out: &mut Canvas, at: &FillPlacement<'_>) {
    let Some(tile) = p.tile.as_ref() else {
        return;
    };
    let sampler = TileSampler::new(tile, at.space);
    let scale = if p.scale.is_finite() && p.scale > 0.0 {
        p.scale.clamp(0.01, 1000.0)
    } else {
        1.0
    };
    let s = at.scale;
    let (mut ox, mut oy) = (0.0f32, 0.0f32);
    if p.link_with_layer {
        ox = finite_or(at.layer_origin[0] / s, 0.0);
        oy = finite_or(at.layer_origin[1] / s, 0.0);
    }
    ox += finite_or(p.offset_px[0], 0.0);
    oy += finite_or(p.offset_px[1], 0.0);
    let a = finite_or(p.angle_deg, 0.0).to_radians();
    let (ca, sa) = (a.cos(), a.sin());
    let rect = out.rect();
    let w = rect.width as usize;
    for (i, d) in out.pixels_mut().iter_mut().enumerate() {
        // The pixel centre in document pixels, relative to the anchor.
        let x = ((rect.x + (i % w) as i64) as f32 + 0.5) / s - ox;
        let y = ((rect.y + (i / w) as i64) as f32 + 0.5) / s - oy;
        // Into pattern space: undo the rotation, then the scale; texel
        // centres sit at integers, so a 1:1 pattern samples exactly.
        let u = (x * ca + y * sa) / scale - 0.5;
        let v = (-x * sa + y * ca) / scale - 0.5;
        *d = sampler.sample(u, v);
    }
}

/// Bilinear, wrapping reads of a pattern tile in premultiplied linear
/// colour, decoding only the texels a pixel actually touches.
struct TileSampler<'a> {
    tile: &'a PatternTile,
    space: &'a ColorSpace,
    lut: Option<[f32; 256]>,
}

impl<'a> TileSampler<'a> {
    fn new(tile: &'a PatternTile, space: &'a ColorSpace) -> Self {
        let lut = match space {
            ColorSpace::Srgb | ColorSpace::LinearSrgb => {
                let mut lut = [0.0f32; 256];
                for (i, slot) in lut.iter_mut().enumerate() {
                    *slot = to_linear(space, [i as f32 / 255.0; 3])[0];
                }
                Some(lut)
            }
            _ => None,
        };
        Self { tile, space, lut }
    }

    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let c = self.tile.pixel(x, y);
        let rgb = match &self.lut {
            Some(l) => [l[c[0] as usize], l[c[1] as usize], l[c[2] as usize]],
            None => to_linear(
                self.space,
                [
                    f32::from(c[0]) / 255.0,
                    f32::from(c[1]) / 255.0,
                    f32::from(c[2]) / 255.0,
                ],
            ),
        };
        let a = f32::from(c[3]) / 255.0;
        [rgb[0] * a, rgb[1] * a, rgb[2] * a, a]
    }

    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        if !u.is_finite() || !v.is_finite() {
            return [0.0; 4];
        }
        let (x0, y0) = (u.floor(), v.floor());
        let (tx, ty) = (u - x0, v - y0);
        let (xa, ya) = (x0 as i64, y0 as i64);
        let (p00, p10, p01, p11) = (
            self.texel(xa, ya),
            self.texel(xa + 1, ya),
            self.texel(xa, ya + 1),
            self.texel(xa + 1, ya + 1),
        );
        let mut out = [0.0f32; 4];
        for c in 0..4 {
            let top = p00[c] + (p10[c] - p00[c]) * tx;
            let bot = p01[c] + (p11[c] - p01[c]) * tx;
            out[c] = top + (bot - top) * ty;
        }
        out
    }
}

/// A gradient's stops, ready to sample: sorted stops, a midpoint that bends
/// the interpolation, a separate alpha ramp when one is given.
///
/// Colours interpolate in the document's **encoded** space and are decoded
/// to linear per sample, as Photoshop and Photopea draw a gradient fill: the
/// halfway point of a red-to-blue ramp is the encoded average, not the
/// lighter linear-light one.
struct Ramp<'a> {
    space: &'a ColorSpace,
    stops: Vec<(f32, [f32; 3], f32)>,
    alpha: Vec<(f32, f32, f32)>,
}

impl<'a> Ramp<'a> {
    fn new(g: &Gradient, space: &'a ColorSpace) -> Self {
        let mut stops: Vec<(f32, [f32; 3], f32)> = g
            .stops
            .iter()
            .filter(|s| s.position.is_finite())
            .map(|s| {
                (
                    s.position.clamp(0.0, 1.0),
                    [unit(s.color[0]), unit(s.color[1]), unit(s.color[2])],
                    unit(s.midpoint).clamp(0.05, 0.95),
                )
            })
            .collect();
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        if stops.is_empty() {
            stops.push((0.0, [0.0; 3], 0.5));
        }
        let source = if g.alpha_stops.is_empty() {
            &g.stops
        } else {
            &g.alpha_stops
        };
        let mut alpha: Vec<(f32, f32, f32)> = source
            .iter()
            .filter(|s| s.position.is_finite())
            .map(|s| {
                (
                    s.position.clamp(0.0, 1.0),
                    unit(s.color[3]),
                    if g.alpha_stops.is_empty() {
                        0.5
                    } else {
                        unit(s.midpoint).clamp(0.05, 0.95)
                    },
                )
            })
            .collect();
        alpha.sort_by(|a, b| a.0.total_cmp(&b.0));
        if alpha.is_empty() {
            alpha.push((0.0, 1.0, 0.5));
        }
        Self {
            space,
            stops,
            alpha,
        }
    }

    fn rgb(&self, t: f32) -> [f32; 3] {
        let (lo, hi, k) = span(&self.stops, t, |s| s.0, |s| s.2);
        let (a, b) = (&self.stops[lo], &self.stops[hi]);
        to_linear(
            self.space,
            [
                a.1[0] + (b.1[0] - a.1[0]) * k,
                a.1[1] + (b.1[1] - a.1[1]) * k,
                a.1[2] + (b.1[2] - a.1[2]) * k,
            ],
        )
    }

    fn alpha(&self, t: f32) -> f32 {
        let (lo, hi, k) = span(&self.alpha, t, |s| s.0, |s| s.2);
        let (a, b) = (self.alpha[lo], self.alpha[hi]);
        a.1 + (b.1 - a.1) * k
    }
}

/// The pair of stops `t` falls between, and how far between them once the
/// lower stop's midpoint has bent the ramp.
fn span<S>(
    stops: &[S],
    t: f32,
    pos: impl Fn(&S) -> f32,
    mid: impl Fn(&S) -> f32,
) -> (usize, usize, f32) {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut i = 0;
    while i + 1 < stops.len() && pos(&stops[i + 1]) <= t {
        i += 1;
    }
    if i + 1 >= stops.len() {
        return (i, i, 0.0);
    }
    let (a, b) = (pos(&stops[i]), pos(&stops[i + 1]));
    if b <= a {
        return (i, i + 1, 0.0);
    }
    let raw = ((t - a) / (b - a)).clamp(0.0, 1.0);
    let m = mid(&stops[i]);
    let k = if (m - 0.5).abs() < 1.0e-6 {
        raw
    } else {
        raw.powf(0.5f32.ln() / m.ln())
    };
    (i, i + 1, k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TestDoc;
    use crate::CompositeOptions;
    use layer_model::{GradientStop, Layer, LayerKind};
    use raster::TileCoord;

    fn fill_layer(source: FillSource) -> Layer {
        Layer::with_kind("Fill", LayerKind::Fill(FillLayer::new(source)))
    }

    /// The whole document, as straight-alpha RGBA8.
    fn composite(t: &TestDoc) -> Vec<u8> {
        let rect = PixelRect::new(0, 0, t.doc.width(), t.doc.height());
        crate::composite_region(&t.doc, &t.src, rect, 0, CompositeOptions::default())
            .unwrap()
            .to_rgba8(&t.doc.meta.color_space)
    }

    fn px(t: &TestDoc, img: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * t.doc.width() + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    #[test]
    fn a_solid_fill_layer_composites_its_colour_everywhere() {
        let mut t = TestDoc::new(300, 24);
        t.push(fill_layer(FillSource::Solid {
            color: [1.0, 0.0, 0.0, 1.0],
        }));
        let img = composite(&t);
        for y in 0..24 {
            for x in 0..300 {
                assert_eq!(px(&t, &img, x, y), [255, 0, 0, 255], "pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn a_fill_layer_covers_the_canvas_whatever_its_transform_and_its_mask_shapes_it() {
        let mut t = TestDoc::new(32, 32);
        let mut layer = fill_layer(FillSource::Solid {
            color: [0.0, 0.0, 1.0, 1.0],
        });
        layer.transform = glam::Affine2::from_translation(glam::Vec2::new(10.0, 7.0));
        let id = t.push(layer);
        let img = composite(&t);
        assert_eq!(
            px(&t, &img, 0, 0),
            [0, 0, 255, 255],
            "moving it moved no paint"
        );
        assert_eq!(px(&t, &img, 31, 31), [0, 0, 255, 255]);

        // A mask that hides the left half: only the right half is painted.
        t.doc.layers.get_mut(id).unwrap().transform = glam::Affine2::IDENTITY;
        let mask = t.attach_mask(id);
        t.paint_mask_with(
            mask,
            TileCoord::new(0, 0, 0),
            |x, _| {
                if x >= 16 {
                    255
                } else {
                    0
                }
            },
        );
        let img = composite(&t);
        assert_eq!(px(&t, &img, 4, 4)[3], 0, "masked out");
        assert_eq!(px(&t, &img, 24, 4), [0, 0, 255, 255], "masked in");
    }

    #[test]
    fn a_gradient_fill_runs_its_ramp_across_the_document() {
        let mut t = TestDoc::new(64, 8);
        t.push(fill_layer(FillSource::Gradient(GradientFill {
            gradient: Gradient {
                stops: vec![
                    GradientStop {
                        position: 0.0,
                        color: [1.0, 0.0, 0.0, 1.0],
                        midpoint: 0.5,
                    },
                    GradientStop {
                        position: 1.0,
                        color: [0.0, 0.0, 1.0, 1.0],
                        midpoint: 0.5,
                    },
                ],
                ..Gradient::default()
            },
            angle_deg: 0.0,
            ..GradientFill::default()
        })));
        let img = composite(&t);
        let left = px(&t, &img, 0, 4);
        let right = px(&t, &img, 63, 4);
        assert!(left[0] > 240 && left[2] < 40, "left is red: {left:?}");
        assert!(right[2] > 240 && right[0] < 40, "right is blue: {right:?}");
    }

    #[test]
    fn a_pattern_fill_tiles_its_pattern_from_the_document_origin() {
        let mut t = TestDoc::new(8, 8);
        let tile =
            PatternTile::new("Checks", 2, 1, vec![255, 255, 255, 255, 0, 0, 0, 255]).unwrap();
        t.push(fill_layer(FillSource::Pattern(PatternFill {
            tile: Some(tile),
            ..PatternFill::default()
        })));
        let img = composite(&t);
        for x in 0..8 {
            let want = if x % 2 == 0 { 255 } else { 0 };
            assert_eq!(px(&t, &img, x, 3), [want, want, want, 255], "x = {x}");
        }
    }

    #[test]
    fn changing_a_fill_parameter_changes_the_layer_signature() {
        let a = fill_layer(FillSource::Solid {
            color: [1.0, 0.0, 0.0, 1.0],
        });
        let mut b = a.clone();
        b.kind = LayerKind::Fill(FillLayer::solid([0.0, 1.0, 0.0, 1.0]));
        assert_ne!(
            super::super::layer_signature(&a),
            super::super::layer_signature(&b),
            "a recoloured fill must not be served from a cached tile"
        );
    }
}
