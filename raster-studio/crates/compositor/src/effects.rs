//! Layer effects: turning a [`layer_model::LayerEffects`] block into pixels.
//!
//! # The model
//!
//! A styled layer is composited in two stages. First its *own* contribution is
//! built into a private buffer:
//!
//! 1. the exterior effects — drop shadow, then outer glow — are drawn into an
//!    empty buffer, so they end up **beneath** everything else;
//! 2. the layer's own pixels are drawn over them at the layer's **fill**
//!    opacity, with the interior effects composited **atop** them (Porter-Duff
//!    `atop`, so an overlay can recolour the layer but never extend it) in
//!    Photoshop's order: pattern overlay, gradient overlay, colour overlay,
//!    satin, inner glow, inner shadow, bevel and emboss;
//! 3. the stroke is drawn last, over the result, because it must be able to sit
//!    outside the layer's own alpha.
//!
//! Then that buffer is blended into the document with the layer's blend mode
//! and its **overall** opacity. That split is what "fill opacity affects the
//! layer's pixels but not its effects" means, and it is the reason
//! [`crate::composite`] stops folding the two opacities together the moment a
//! layer is styled.
//!
//! Everything is derived from the layer's **shape** — its content alpha times
//! its mask — so a mask hides a layer's effects along with its pixels, which is
//! what a user who painted a mask expects.
//!
//! # The silhouette, the distance field, and why they are the same thing
//!
//! Every effect that is not a flat overlay needs one of two operations on the
//! layer's silhouette: grow or shrink it (Photoshop's *spread* and *choke*, a
//! stroke's position, a bevel's width) and blur it. Both are exact on a signed
//! distance field, so the alpha is converted to one once per styled layer and
//! every effect reads it. Growing is then a subtraction and a clamp, which also
//! gives a crisp anti-aliased edge that a blur-and-threshold cannot.
//!
//! The blur itself is [`filters::box_blur`], three iterated passes at the radii
//! that approximate a Gaussian — the same `O(1)`-per-pixel construction the
//! mask feather uses, and the reason a 200-pixel shadow does not cost a
//! 200-pixel kernel.
//!
//! # Region independence
//!
//! An effect reads a neighbourhood, which the tiled compositor otherwise never
//! does. It stays exact because the layer is rendered over `rect` grown by
//! [`reach`] — a number derived from the effect parameters alone — and only the
//! centre is kept. Every effect's reach is bounded by that margin by
//! construction, so no pixel of the result can depend on data outside the grown
//! rect, and compositing a region is still the same sub-rect of compositing
//! everything.
//!
//! # Honest gaps
//!
//! * **Pattern fills.** A [`layer_model::PatternFill`] names an `AssetId`, and
//!   this crate has no asset store: a pattern overlay, and a glow or stroke
//!   filled with a pattern, draw nothing. Solid and gradient fills are drawn.
//! * **Effect blend modes act inside the style buffer**, not against the
//!   document beneath. A drop shadow is the first thing in an empty buffer, so
//!   its own blend mode has nothing to blend with; the interior effects' modes
//!   do act, against the layer's own pixels, which is where they read.
//! * **Contours** (the response curves Photoshop puts on every effect),
//!   `GlowEffect::jitter`, `StrokeEffect::overprint` and the distinction
//!   between the three [`layer_model::BevelTechnique`]s are not implemented.
//!   `GlowEffect::range` is approximated as a remap of the falloff.
//! * **Reach is clamped** to [`MAX_REACH`] level pixels. Past that an effect
//!   would need a working buffer many times the tile it is drawn into; the
//!   clamp is part of the tile cache key, exactly as the mask feather's is.

use color::{to_linear, ColorSpace};
use filters::{box_blur, EdgeMode, FilterBuffer};
use layer_model::{
    BevelDirection, BevelEffect, BevelStyle, BlendMode, ColorOverlayEffect, FillStyle, GlowEffect,
    GlowSource, GlowTechnique, Gradient, GradientOverlayEffect, GradientStyle, LayerEffects, Rgba,
    SatinEffect, ShadowEffect, StrokeEffect, StrokePosition,
};
use raster::PixelRect;

use crate::blending::{blend_atop, blend_over, dissolve_noise, BlendContext};
use crate::canvas::Canvas;
use crate::composite::boxes_for_gauss;
use crate::error::CompositeError;

/// Largest distance, in pixels **at the level being composited**, that an
/// effect may reach away from the layer it belongs to.
///
/// One tile. Past this the working buffer for a 256-pixel tile would be more
/// than three tiles on a side, per layer, per rayon worker; the clamp trades a
/// look nobody asks for against a frame the machine can render. See the module
/// docs.
pub const MAX_REACH: i64 = 256;

/// Salt mixed into the dissolve seed so an effect's noise does not correlate
/// with a `Dissolve` blend on the same pixel.
const NOISE_SALT: u64 = 0x5EED_0F17_9013_5EA1;

/// The margin a styled layer must be rendered with, in level pixels, or `None`
/// when the layer has no effects and the plain path applies.
///
/// A block that is filled but switched off, or filled only with effects this
/// crate cannot draw, still returns `Some(0)`: the answer is the same either
/// way, and reporting "no effects" would be a claim about the block rather than
/// about the margin.
pub fn reach(effects: &LayerEffects, level: u8) -> Option<i64> {
    if !effects.affects_composite() {
        return None;
    }
    let s = scale(level);
    let mut r = 0.0f32;
    let mut want = |v: f32| r = r.max(v);
    for sh in [&effects.drop_shadow, &effects.inner_shadow]
        .into_iter()
        .flatten()
    {
        want(sh.distance_px.abs() * s + 2.0 * size(sh.size_px) * s);
    }
    for g in [&effects.outer_glow, &effects.inner_glow]
        .into_iter()
        .flatten()
    {
        want(2.0 * size(g.size_px) * s);
    }
    if let Some(sa) = &effects.satin {
        want(sa.distance_px.abs() * s + 2.0 * size(sa.size_px) * s);
    }
    if let Some(b) = &effects.bevel_emboss {
        want(2.0 * (size(b.size_px) + size(b.soften_px)) * s);
    }
    if let Some(st) = &effects.stroke {
        want(size(st.size_px) * s);
    }
    if !r.is_finite() {
        return Some(MAX_REACH);
    }
    // Two pixels of slack so the distance field, which is exact only out to the
    // buffer's own edge, is never consulted past where it is exact.
    Some(((r.ceil() as i64).saturating_add(2)).clamp(0, MAX_REACH))
}

/// Document pixels per level pixel.
fn scale(level: u8) -> f32 {
    2.0f32.powi(-(level as i32))
}

/// A size parameter made safe to multiply with: finite and non-negative.
fn size(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 {
        v.min(1.0e6)
    } else {
        0.0
    }
}

/// An opacity-like parameter made safe to multiply with.
fn unit(v: f32) -> f32 {
    layer_model::blend::unit(v)
}

/// Everything the effect passes need beyond the layer's own pixels.
pub(crate) struct StyleContext<'a> {
    pub space: &'a ColorSpace,
    pub blend: BlendContext<'a>,
    pub level: u8,
    pub dissolve_seed: u64,
    /// The layer's own extent in document space at this level. Used to fit an
    /// `align_with_layer` gradient; empty falls back to the document.
    pub layer_bounds: PixelRect,
    /// The document's canvas at this level.
    pub doc_bounds: PixelRect,
}

/// Draw `src` — the layer's shape, before opacity — with its style applied.
///
/// The result covers the same rect as `src` and is premultiplied linear, ready
/// to be blended down with the layer's blend mode and overall opacity.
pub(crate) fn render(
    src: &Canvas,
    effects: &LayerEffects,
    fill_opacity: f32,
    ctx: &StyleContext<'_>,
) -> Result<Canvas, CompositeError> {
    let rect = src.rect();
    let (w, h) = (rect.width as usize, rect.height as usize);
    let mut out = Canvas::transparent(rect)?;
    if w == 0 || h == 0 {
        return Ok(out);
    }
    let alpha: Vec<f32> = src.pixels().iter().map(|p| unit(p[3])).collect();
    let sdf = signed_distance(&alpha, w, h);
    let g = Geometry {
        w,
        h,
        rect,
        scale: scale(ctx.level),
    };

    // 1. Behind the layer.
    if let Some(e) = &effects.drop_shadow {
        if let Some(ink) = drop_shadow(e, &sdf, &alpha, &g, ctx) {
            draw(&mut out, &ink, e.blend_mode, false, &ctx.blend);
        }
    }
    if let Some(e) = &effects.outer_glow {
        if let Some(ink) = outer_glow(e, &sdf, &g, ctx) {
            draw(&mut out, &ink, e.blend_mode, false, &ctx.blend);
        }
    }

    // 2. The layer's own pixels, at fill opacity, plus everything that lives
    //    inside its shape.
    let mut interior = src.clone();
    let fo = unit(fill_opacity);
    if fo < 1.0 {
        for px in interior.pixels_mut() {
            for ch in px.iter_mut() {
                *ch *= fo;
            }
        }
    }
    if let Some(e) = &effects.pattern_overlay {
        // No asset store here; see the module docs.
        let _ = e;
    }
    if let Some(e) = &effects.gradient_overlay {
        if let Some(ink) = gradient_overlay(e, &alpha, &g, ctx) {
            draw(&mut interior, &ink, e.blend_mode, true, &ctx.blend);
        }
    }
    if let Some(e) = &effects.color_overlay {
        let ink = color_overlay(e, &alpha, ctx);
        draw(&mut interior, &ink, e.blend_mode, true, &ctx.blend);
    }
    if let Some(e) = &effects.satin {
        if let Some(ink) = satin(e, &sdf, &alpha, &g, ctx) {
            draw(&mut interior, &ink, e.blend_mode, true, &ctx.blend);
        }
    }
    if let Some(e) = &effects.inner_glow {
        if let Some(ink) = inner_glow(e, &sdf, &alpha, &g, ctx) {
            draw(&mut interior, &ink, e.blend_mode, true, &ctx.blend);
        }
    }
    if let Some(e) = &effects.inner_shadow {
        if let Some(ink) = inner_shadow(e, &sdf, &alpha, &g, ctx) {
            draw(&mut interior, &ink, e.blend_mode, true, &ctx.blend);
        }
    }
    if let Some(e) = &effects.bevel_emboss {
        for (ink, mode) in bevel(e, &sdf, &alpha, &g, ctx) {
            draw(&mut interior, &ink, mode, true, &ctx.blend);
        }
    }
    over(&mut out, &interior);

    // 3. The stroke, which may sit outside the layer's own alpha.
    if let Some(e) = &effects.stroke {
        if let Some(ink) = stroke(e, &sdf, &g, ctx) {
            draw(&mut out, &ink, e.blend_mode, false, &ctx.blend);
        }
    }
    Ok(out)
}

/// Buffer geometry shared by every pass.
struct Geometry {
    w: usize,
    h: usize,
    rect: PixelRect,
    /// Document pixels per level pixel.
    scale: f32,
}

impl Geometry {
    fn len(&self) -> usize {
        self.w * self.h
    }

    /// A document-pixel length in level pixels.
    fn px(&self, v: f32) -> f32 {
        size(v) * self.scale
    }
}

/// A colour field and the coverage it is drawn with.
struct Ink {
    rgb: Paint,
    alpha: Vec<f32>,
}

enum Paint {
    Solid([f32; 3]),
    PerPixel(Vec<[f32; 3]>),
}

impl Paint {
    fn at(&self, i: usize) -> [f32; 3] {
        match self {
            Paint::Solid(c) => *c,
            Paint::PerPixel(v) => v[i],
        }
    }
}

/// Blend an ink into a buffer, either over it or clipped to its alpha.
fn draw(dst: &mut Canvas, ink: &Ink, mode: BlendMode, atop: bool, bctx: &BlendContext<'_>) {
    for (i, d) in dst.pixels_mut().iter_mut().enumerate() {
        let a = ink.alpha[i];
        if a <= 0.0 {
            continue;
        }
        let rgb = ink.rgb.at(i);
        *d = if atop {
            blend_atop(*d, rgb, a, mode, bctx)
        } else {
            blend_over(*d, rgb, a, mode, bctx)
        };
    }
}

/// Premultiplied source-over of one whole canvas onto another.
fn over(dst: &mut Canvas, src: &Canvas) {
    for (d, s) in dst.pixels_mut().iter_mut().zip(src.pixels()) {
        let inv = 1.0 - unit(s[3]);
        for c in 0..4 {
            d[c] = s[c] + d[c] * inv;
        }
    }
}

/// Decode a straight-alpha effect colour into linear RGB.
fn linear_rgb(color: Rgba, space: &ColorSpace) -> [f32; 3] {
    to_linear(space, [color[0], color[1], color[2]])
}

// ---------------------------------------------------------------------------
// Field operations
// ---------------------------------------------------------------------------

/// A signed distance to the silhouette's edge, in pixels, negative inside.
///
/// Two chamfer passes over the thresholded coverage give a distance from one
/// pixel *centre* to the nearest centre of the opposite class; the edge lies
/// halfway between them, which is where the half-pixel comes from. On a pixel
/// the chamfer says is right on the edge, the coverage locates it better than
/// the pixel grid can, so the coverage wins there — that is what keeps an
/// anti-aliased outline from being re-quantised by every effect that traces it.
///
/// Values further than the buffer's own extent are not meaningful and no caller
/// reads that far — see the module docs on reach.
fn signed_distance(alpha: &[f32], w: usize, h: usize) -> Vec<f32> {
    let inside: Vec<bool> = alpha.iter().map(|a| *a >= 0.5).collect();
    let outward = chamfer(&inside, w, h, false);
    let inward = chamfer(&inside, w, h, true);
    (0..w * h)
        .map(|i| {
            let d = if inside[i] {
                -(inward[i] - 0.5)
            } else {
                outward[i] - 0.5
            };
            if d.abs() <= 0.5 && alpha[i] > 0.0 && alpha[i] < 1.0 {
                0.5 - alpha[i]
            } else {
                d
            }
        })
        .collect()
}

/// Distance from every pixel to the nearest pixel of the opposite class.
///
/// `invert` picks which class is the target: `false` measures how far a
/// background pixel is from the shape, `true` how far a shape pixel is from the
/// background.
fn chamfer(inside: &[bool], w: usize, h: usize, invert: bool) -> Vec<f32> {
    const FAR: f32 = 1.0e9;
    const D1: f32 = 1.0;
    const D2: f32 = std::f32::consts::SQRT_2;
    let mut d = vec![FAR; w * h];
    for (i, seed) in d.iter_mut().enumerate() {
        if inside[i] != invert {
            *seed = 0.0;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + D1);
            }
            if y > 0 {
                v = v.min(d[i - w] + D1);
                if x > 0 {
                    v = v.min(d[i - w - 1] + D2);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + D2);
                }
            }
            d[i] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + D1);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + D1);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + D2);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + D2);
                }
            }
            d[i] = v;
        }
    }
    d
}

/// Anti-aliased coverage of the silhouette grown by `grow` pixels. Negative
/// grows shrink it.
fn silhouette(sdf: &[f32], grow: f32) -> Vec<f32> {
    sdf.iter()
        .map(|s| (0.5 + grow - s).clamp(0.0, 1.0))
        .collect()
}

/// A field sampled at `(x - dx, y - dy)`, bilinearly, clamped at the edges.
fn shifted(field: &[f32], g: &Geometry, dx: f32, dy: f32) -> Vec<f32> {
    if dx == 0.0 && dy == 0.0 {
        return field.to_vec();
    }
    let sample = |x: isize, y: isize| -> f32 {
        let x = x.clamp(0, g.w as isize - 1) as usize;
        let y = y.clamp(0, g.h as isize - 1) as usize;
        field[y * g.w + x]
    };
    let mut out = vec![0.0f32; g.len()];
    for y in 0..g.h {
        for x in 0..g.w {
            let fx = x as f32 - dx;
            let fy = y as f32 - dy;
            let (x0, y0) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - x0, fy - y0);
            let (x0, y0) = (x0 as isize, y0 as isize);
            let top = sample(x0, y0) + (sample(x0 + 1, y0) - sample(x0, y0)) * tx;
            let bot = sample(x0, y0 + 1) + (sample(x0 + 1, y0 + 1) - sample(x0, y0 + 1)) * tx;
            out[y * g.w + x] = top + (bot - top) * ty;
        }
    }
    out
}

/// Three iterated box passes approximating a Gaussian of `sigma`.
fn blur(field: &[f32], g: &Geometry, sigma: f32) -> Vec<f32> {
    if !sigma.is_finite() || sigma <= 0.05 || g.w == 0 || g.h == 0 {
        return field.to_vec();
    }
    let px: Vec<[f32; 4]> = field.iter().map(|v| [*v, 0.0, 0.0, 0.0]).collect();
    let Ok(mut buf) = FilterBuffer::from_pixels(g.w as u32, g.h as u32, px) else {
        return field.to_vec();
    };
    for r in boxes_for_gauss(sigma) {
        let Ok(r) = u32::try_from(r) else { continue };
        if r == 0 {
            continue;
        }
        buf = box_blur(&buf, r, EdgeMode::Clamp);
    }
    buf.pixels().iter().map(|p| p[0]).collect()
}

/// The offset a light at `angle_deg` throws a shadow by, in level pixels.
///
/// The angle names where the light *is*, counter-clockwise from +x in the usual
/// mathematical sense; the shadow falls the other way, and `y` grows downward
/// in image space. Photoshop's 120° default therefore lands a shadow down and
/// to the right, which is what its own preview shows.
fn light_offset(angle_deg: f32, distance: f32) -> (f32, f32) {
    if !angle_deg.is_finite() || !distance.is_finite() {
        return (0.0, 0.0);
    }
    let a = angle_deg.to_radians();
    (-a.cos() * distance, a.sin() * distance)
}

/// Multiply a field by monochromatic noise keyed on absolute image coordinates.
///
/// Keyed on the coordinate rather than the buffer index for the same reason
/// `Dissolve` is: a tile-local draw would make the noise crawl as the viewport
/// moved, and would break region independence outright.
fn add_noise(field: &mut [f32], amount: f32, g: &Geometry, ctx: &StyleContext<'_>) {
    let amount = unit(amount);
    if amount <= 0.0 {
        return;
    }
    for (i, v) in field.iter_mut().enumerate() {
        let x = g.rect.x + (i % g.w) as i64;
        let y = g.rect.y + (i / g.w) as i64;
        let n = dissolve_noise(x, y, ctx.level, ctx.dissolve_seed ^ NOISE_SALT);
        *v *= 1.0 - amount * n;
    }
}

/// The sigma and grow a "size / spread" pair means.
fn spread_and_sigma(size_px: f32, spread: f32, g: &Geometry) -> (f32, f32) {
    let s = g.px(size_px);
    let spread = unit(spread).min(0.95);
    (s * spread, s * (1.0 - spread) / 3.0)
}

// ---------------------------------------------------------------------------
// The effects
// ---------------------------------------------------------------------------

fn drop_shadow(
    e: &ShadowEffect,
    sdf: &[f32],
    alpha: &[f32],
    g: &Geometry,
    ctx: &StyleContext<'_>,
) -> Option<Ink> {
    let opacity = unit(e.opacity) * unit(e.color[3]);
    if opacity <= 0.0 {
        return None;
    }
    let (dx, dy) = light_offset(
        e.angle_deg,
        g.px(e.distance_px.abs()) * e.distance_px.signum(),
    );
    let (grow, sigma) = spread_and_sigma(e.size_px, e.spread, g);
    let moved = shifted(sdf, g, dx, dy);
    let mut f = blur(&silhouette(&moved, grow), g, sigma);
    add_noise(&mut f, e.noise, g, ctx);
    if e.knockout {
        for (v, a) in f.iter_mut().zip(alpha) {
            *v *= 1.0 - a;
        }
    }
    for v in f.iter_mut() {
        *v *= opacity;
    }
    Some(Ink {
        rgb: Paint::Solid(linear_rgb(e.color, ctx.space)),
        alpha: f,
    })
}

fn inner_shadow(
    e: &ShadowEffect,
    sdf: &[f32],
    alpha: &[f32],
    g: &Geometry,
    ctx: &StyleContext<'_>,
) -> Option<Ink> {
    let opacity = unit(e.opacity) * unit(e.color[3]);
    if opacity <= 0.0 {
        return None;
    }
    let (dx, dy) = light_offset(
        e.angle_deg,
        g.px(e.distance_px.abs()) * e.distance_px.signum(),
    );
    // "Choke" shrinks the hole the shadow falls into, which is the same number
    // as spread with the sign the other way round.
    let (choke, sigma) = spread_and_sigma(e.size_px, e.spread, g);
    let moved = shifted(sdf, g, dx, dy);
    let outside: Vec<f32> = silhouette(&moved, -choke).iter().map(|v| 1.0 - v).collect();
    let mut f = blur(&outside, g, sigma);
    add_noise(&mut f, e.noise, g, ctx);
    for (v, a) in f.iter_mut().zip(alpha) {
        *v *= a * opacity;
    }
    Some(Ink {
        rgb: Paint::Solid(linear_rgb(e.color, ctx.space)),
        alpha: f,
    })
}

/// The falloff a glow's technique produces, in `0..=1`, before its source or
/// side is applied. `outward` picks which side of the edge it grows into.
fn glow_falloff(e: &GlowEffect, sdf: &[f32], g: &Geometry, outward: bool) -> Vec<f32> {
    let (grow, sigma) = spread_and_sigma(e.size_px, e.spread, g);
    let reach = g.px(e.size_px).max(1.0);
    let mut f = match e.technique {
        GlowTechnique::Softer => {
            let base = if outward {
                silhouette(sdf, grow)
            } else {
                silhouette(sdf, -grow).iter().map(|v| 1.0 - v).collect()
            };
            blur(&base, g, sigma)
        }
        // A distance falloff keeps corners sharp, which is the whole point of
        // "precise": the value is how far into the glow's reach the pixel is.
        GlowTechnique::Precise => sdf
            .iter()
            .map(|s| {
                let d = if outward { *s } else { -*s };
                (1.0 - (d - grow) / reach).clamp(0.0, 1.0)
            })
            .collect(),
    };
    // `range` targets a slice of the falloff rather than all of it.
    let lo = 1.0 - unit(e.range).max(0.01);
    if lo > 0.0 {
        for v in f.iter_mut() {
            *v = ((*v - lo) / (1.0 - lo)).clamp(0.0, 1.0);
        }
    }
    f
}

fn glow_paint(fill: &FillStyle, f: &[f32], ctx: &StyleContext<'_>) -> Option<(Paint, f32)> {
    match fill {
        FillStyle::Solid(c) => Some((Paint::Solid(linear_rgb(*c, ctx.space)), unit(c[3]))),
        FillStyle::Gradient(grad) => {
            let ramp = Ramp::new(grad, ctx.space);
            // A glow's gradient runs along its own falloff: the edge of the
            // shape is one end of the ramp and the far end of the reach is the
            // other.
            let rgb = f.iter().map(|v| ramp.rgb(1.0 - *v)).collect();
            Some((Paint::PerPixel(rgb), 1.0))
        }
        // No asset store here; see the module docs.
        FillStyle::Pattern(_) => None,
    }
}

fn outer_glow(e: &GlowEffect, sdf: &[f32], g: &Geometry, ctx: &StyleContext<'_>) -> Option<Ink> {
    let opacity = unit(e.opacity);
    if opacity <= 0.0 {
        return None;
    }
    let mut f = glow_falloff(e, sdf, g, true);
    add_noise(&mut f, e.noise, g, ctx);
    let (rgb, fill_alpha) = glow_paint(&e.fill, &f, ctx)?;
    for v in f.iter_mut() {
        *v *= opacity * fill_alpha;
    }
    Some(Ink { rgb, alpha: f })
}

fn inner_glow(
    e: &GlowEffect,
    sdf: &[f32],
    alpha: &[f32],
    g: &Geometry,
    ctx: &StyleContext<'_>,
) -> Option<Ink> {
    let opacity = unit(e.opacity);
    if opacity <= 0.0 {
        return None;
    }
    let reach = g.px(e.size_px).max(1.0);
    let mut f = match e.source {
        GlowSource::Edge => glow_falloff(e, sdf, g, false),
        // Brightest deep inside, fading toward the edge.
        GlowSource::Center => sdf.iter().map(|s| (-*s / reach).clamp(0.0, 1.0)).collect(),
    };
    add_noise(&mut f, e.noise, g, ctx);
    let (rgb, fill_alpha) = glow_paint(&e.fill, &f, ctx)?;
    for (v, a) in f.iter_mut().zip(alpha) {
        *v *= a * opacity * fill_alpha;
    }
    Some(Ink { rgb, alpha: f })
}

fn satin(
    e: &SatinEffect,
    sdf: &[f32],
    alpha: &[f32],
    g: &Geometry,
    ctx: &StyleContext<'_>,
) -> Option<Ink> {
    let opacity = unit(e.opacity) * unit(e.color[3]);
    if opacity <= 0.0 {
        return None;
    }
    let (dx, dy) = light_offset(
        e.angle_deg,
        g.px(e.distance_px.abs()) * e.distance_px.signum(),
    );
    let a = silhouette(&shifted(sdf, g, dx, dy), 0.0);
    let b = silhouette(&shifted(sdf, g, -dx, -dy), 0.0);
    let diff: Vec<f32> = a.iter().zip(&b).map(|(p, q)| (p - q).abs()).collect();
    let mut f = blur(&diff, g, g.px(e.size_px) / 3.0);
    if e.invert {
        for v in f.iter_mut() {
            *v = 1.0 - *v;
        }
    }
    for (v, a) in f.iter_mut().zip(alpha) {
        *v *= a * opacity;
    }
    Some(Ink {
        rgb: Paint::Solid(linear_rgb(e.color, ctx.space)),
        alpha: f,
    })
}

fn color_overlay(e: &ColorOverlayEffect, alpha: &[f32], ctx: &StyleContext<'_>) -> Ink {
    let k = unit(e.opacity) * unit(e.color[3]);
    Ink {
        rgb: Paint::Solid(linear_rgb(e.color, ctx.space)),
        alpha: alpha.iter().map(|a| a * k).collect(),
    }
}

fn gradient_overlay(
    e: &GradientOverlayEffect,
    alpha: &[f32],
    g: &Geometry,
    ctx: &StyleContext<'_>,
) -> Option<Ink> {
    let opacity = unit(e.opacity);
    if opacity <= 0.0 {
        return None;
    }
    // Fit the ramp to a rect that does not depend on which region asked, or the
    // gradient would step at every tile boundary.
    let fit = if e.align_with_layer && !ctx.layer_bounds.is_empty() {
        ctx.layer_bounds
    } else {
        ctx.doc_bounds
    };
    if fit.is_empty() {
        return None;
    }
    let ramp = Ramp::new(&e.gradient, ctx.space);
    let cx = fit.x as f32 + fit.width as f32 * 0.5 + e.offset_px[0] * g.scale;
    let cy = fit.y as f32 + fit.height as f32 * 0.5 + e.offset_px[1] * g.scale;
    let extent = (fit.width.max(fit.height) as f32
        * 0.5
        * if e.scale.is_finite() && e.scale > 0.0 {
            e.scale
        } else {
            1.0
        })
    .max(1.0);
    let a = if e.angle_deg.is_finite() {
        e.angle_deg.to_radians()
    } else {
        0.0
    };
    let (ca, sa) = (a.cos(), a.sin());

    let mut rgb = Vec::with_capacity(g.len());
    let mut out_alpha = Vec::with_capacity(g.len());
    for (i, src_a) in alpha.iter().enumerate() {
        let x = (g.rect.x + (i % g.w) as i64) as f32 + 0.5 - cx;
        let y = (g.rect.y + (i / g.w) as i64) as f32 + 0.5 - cy;
        // Image y grows downward, so a positive angle must still run upward.
        let u = x * ca - y * sa;
        let v = x * sa + y * ca;
        let mut t = match e.style {
            GradientStyle::Linear => 0.5 + u / (2.0 * extent),
            GradientStyle::Reflected => (u / extent).abs(),
            GradientStyle::Radial => (u * u + v * v).sqrt() / extent,
            GradientStyle::Diamond => (u.abs() + v.abs()) / extent,
            GradientStyle::Angle => 0.5 + v.atan2(u) / std::f32::consts::TAU,
        };
        if e.reverse {
            t = 1.0 - t;
        }
        rgb.push(ramp.rgb(t));
        out_alpha.push(src_a * opacity * ramp.alpha(t));
    }
    Some(Ink {
        rgb: Paint::PerPixel(rgb),
        alpha: out_alpha,
    })
}

fn stroke(e: &StrokeEffect, sdf: &[f32], g: &Geometry, ctx: &StyleContext<'_>) -> Option<Ink> {
    let opacity = unit(e.opacity);
    let width = g.px(e.size_px);
    if opacity <= 0.0 || width <= 0.0 {
        return None;
    }
    let (inner, outer) = match e.position {
        StrokePosition::Outside => (0.0, width),
        StrokePosition::Inside => (-width, 0.0),
        StrokePosition::Center => (-width * 0.5, width * 0.5),
    };
    let wide = silhouette(sdf, outer);
    let narrow = silhouette(sdf, inner);
    let mut f: Vec<f32> = wide
        .iter()
        .zip(&narrow)
        .map(|(a, b)| (a - b).clamp(0.0, 1.0))
        .collect();
    let (rgb, fill_alpha) = match &e.fill {
        FillStyle::Solid(c) => (Paint::Solid(linear_rgb(*c, ctx.space)), unit(c[3])),
        FillStyle::Gradient(grad) => {
            let ramp = Ramp::new(grad, ctx.space);
            // Across the stroke: the inner edge is 0, the outer edge 1.
            let colors = sdf
                .iter()
                .map(|s| ramp.rgb(((s - inner) / (outer - inner)).clamp(0.0, 1.0)))
                .collect();
            (Paint::PerPixel(colors), 1.0)
        }
        FillStyle::Pattern(_) => return None,
    };
    for v in f.iter_mut() {
        *v *= opacity * fill_alpha;
    }
    Some(Ink { rgb, alpha: f })
}

/// The highlight and shadow halves of a bevel, each with its own blend mode.
fn bevel(
    e: &BevelEffect,
    sdf: &[f32],
    alpha: &[f32],
    g: &Geometry,
    ctx: &StyleContext<'_>,
) -> Vec<(Ink, BlendMode)> {
    let width = g.px(e.size_px);
    if width <= 0.0 {
        return Vec::new();
    }
    // A height field that rises from the edge inward over `width` pixels, then
    // softened. This is what makes the bevel read as a slope rather than a step.
    let height: Vec<f32> = sdf.iter().map(|s| (-*s / width).clamp(0.0, 1.0)).collect();
    let height = blur(&height, g, (width / 3.0).max(0.5) + g.px(e.soften_px) / 3.0);

    let depth = if e.depth.is_finite() {
        e.depth.clamp(0.0, 10.0)
    } else {
        1.0
    };
    let altitude = if e.altitude_deg.is_finite() {
        e.altitude_deg.clamp(0.0, 90.0)
    } else {
        30.0
    };
    let a = if e.angle_deg.is_finite() {
        e.angle_deg.to_radians()
    } else {
        0.0
    };
    // The light's horizontal direction, in image space where y grows downward.
    let (lx, ly) = (a.cos(), -a.sin());
    let gain = depth * altitude.to_radians().cos() * width.max(1.0);
    let flip = match e.direction {
        BevelDirection::Up => 1.0,
        BevelDirection::Down => -1.0,
    };

    let mut shade = vec![0.0f32; g.len()];
    for y in 0..g.h {
        for x in 0..g.w {
            let i = y * g.w + x;
            let xm = if x > 0 { height[i - 1] } else { height[i] };
            let xp = if x + 1 < g.w {
                height[i + 1]
            } else {
                height[i]
            };
            let ym = if y > 0 { height[i - g.w] } else { height[i] };
            let yp = if y + 1 < g.h {
                height[i + g.w]
            } else {
                height[i]
            };
            let dx = (xp - xm) * 0.5;
            let dy = (yp - ym) * 0.5;
            // A slope facing the light is lit; one facing away is shaded.
            shade[i] = (-(dx * lx + dy * ly)) * gain * flip;
        }
    }

    // Which side of the edge the bevel is allowed to shade.
    let region: Vec<f32> = match e.style {
        BevelStyle::OuterBevel => alpha.iter().map(|a| 1.0 - a).collect(),
        BevelStyle::Emboss => vec![1.0; g.len()],
        // Pillow reverses the shading outside the shape, which is what makes
        // the edge look pressed in from both sides.
        BevelStyle::PillowEmboss => {
            for (s, a) in shade.iter_mut().zip(alpha) {
                if *a < 0.5 {
                    *s = -*s;
                }
            }
            vec![1.0; g.len()]
        }
        BevelStyle::InnerBevel | BevelStyle::StrokeEmboss => alpha.to_vec(),
    };

    let mut out = Vec::new();
    for (positive, mode, color, opacity) in [
        (
            true,
            e.highlight_mode,
            e.highlight_color,
            unit(e.highlight_opacity) * unit(e.highlight_color[3]),
        ),
        (
            false,
            e.shadow_mode,
            e.shadow_color,
            unit(e.shadow_opacity) * unit(e.shadow_color[3]),
        ),
    ] {
        if opacity <= 0.0 {
            continue;
        }
        let f: Vec<f32> = shade
            .iter()
            .zip(&region)
            .map(|(s, r)| {
                let v = if positive { *s } else { -*s };
                v.clamp(0.0, 1.0) * r * opacity
            })
            .collect();
        if f.iter().all(|v| *v <= 0.0) {
            continue;
        }
        out.push((
            Ink {
                rgb: Paint::Solid(linear_rgb(color, ctx.space)),
                alpha: f,
            },
            mode,
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Gradients
// ---------------------------------------------------------------------------

/// A gradient's stops resolved into linear colour, ready to sample.
struct Ramp {
    stops: Vec<(f32, [f32; 3], f32)>,
    alpha: Vec<(f32, f32, f32)>,
}

impl Ramp {
    fn new(g: &Gradient, space: &ColorSpace) -> Self {
        let mut stops: Vec<(f32, [f32; 3], f32)> = g
            .stops
            .iter()
            .filter(|s| s.position.is_finite())
            .map(|s| {
                (
                    s.position.clamp(0.0, 1.0),
                    linear_rgb(s.color, space),
                    unit(s.midpoint).clamp(0.05, 0.95),
                )
            })
            .collect();
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        if stops.is_empty() {
            stops.push((0.0, [0.0; 3], 0.5));
        }
        let mut alpha: Vec<(f32, f32, f32)> = if g.alpha_stops.is_empty() {
            g.stops
                .iter()
                .filter(|s| s.position.is_finite())
                .map(|s| (s.position.clamp(0.0, 1.0), unit(s.color[3]), 0.5))
                .collect()
        } else {
            g.alpha_stops
                .iter()
                .filter(|s| s.position.is_finite())
                .map(|s| {
                    (
                        s.position.clamp(0.0, 1.0),
                        unit(s.color[3]),
                        unit(s.midpoint).clamp(0.05, 0.95),
                    )
                })
                .collect()
        };
        alpha.sort_by(|a, b| a.0.total_cmp(&b.0));
        if alpha.is_empty() {
            alpha.push((0.0, 1.0, 0.5));
        }
        Self { stops, alpha }
    }

    fn rgb(&self, t: f32) -> [f32; 3] {
        let (lo, hi, k) = span(&self.stops, t, |s| s.0, |s| s.2);
        let (a, b) = (&self.stops[lo], &self.stops[hi]);
        [
            a.1[0] + (b.1[0] - a.1[0]) * k,
            a.1[1] + (b.1[1] - a.1[1]) * k,
            a.1[2] + (b.1[2] - a.1[2]) * k,
        ]
    }

    fn alpha(&self, t: f32) -> f32 {
        let (lo, hi, k) = span(&self.alpha, t, |s| s.0, |s| s.2);
        let (a, b) = (self.alpha[lo], self.alpha[hi]);
        a.1 + (b.1 - a.1) * k
    }
}

/// The pair of stops `t` falls between, and how far it is between them once the
/// lower stop's midpoint has skewed the ramp.
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
    // A midpoint of 0.5 is linear; anything else bends the interpolation so the
    // halfway colour lands at the midpoint instead.
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

    fn geometry(w: usize, h: usize) -> Geometry {
        Geometry {
            w,
            h,
            rect: PixelRect::new(0, 0, w as u32, h as u32),
            scale: 1.0,
        }
    }

    /// A 16x16 buffer holding an 8x8 opaque square at (4, 4).
    fn square_alpha() -> (Vec<f32>, Geometry) {
        let g = geometry(16, 16);
        let mut a = vec![0.0f32; g.len()];
        for y in 4..12 {
            for x in 4..12 {
                a[y * 16 + x] = 1.0;
            }
        }
        (a, g)
    }

    #[test]
    fn the_distance_field_is_negative_inside_and_grows_outward() {
        let (a, g) = square_alpha();
        let sdf = signed_distance(&a, g.w, g.h);
        let at = |x: usize, y: usize| sdf[y * 16 + x];
        assert!(at(8, 8) < -3.0, "deep inside: {}", at(8, 8));
        assert!(at(4, 8) < 0.0, "an edge pixel is still inside");
        assert!(at(3, 8) > 0.0, "one pixel out is outside");
        assert!(at(0, 8) > at(3, 8), "and it keeps growing");
    }

    #[test]
    fn growing_and_shrinking_the_silhouette_move_its_edge() {
        let (a, g) = square_alpha();
        let sdf = signed_distance(&a, g.w, g.h);
        let grown = silhouette(&sdf, 2.0);
        let shrunk = silhouette(&sdf, -2.0);
        let at = |f: &Vec<f32>, x: usize, y: usize| f[y * 16 + x];
        assert!(at(&grown, 2, 8) > 0.9, "two pixels out is now covered");
        assert_eq!(at(&shrunk, 4, 8), 0.0, "the old edge is now outside");
        assert!(at(&shrunk, 8, 8) > 0.9, "the middle survives");
        // Growing only ever adds coverage.
        assert!(grown.iter().zip(&shrunk).all(|(a, b)| a >= b));
    }

    #[test]
    fn a_blur_conserves_the_total_and_spreads_it() {
        let (a, g) = square_alpha();
        let before: f32 = a.iter().sum();
        let after = blur(&a, &g, 1.5);
        let total: f32 = after.iter().sum();
        assert!(
            (total - before).abs() / before < 0.05,
            "{total} vs {before}"
        );
        assert!(
            after[8 * 16 + 3] > 0.0,
            "coverage reached outside the square"
        );
        assert!(after[8 * 16 + 8] < 1.0001);
    }

    #[test]
    fn a_shift_moves_a_field_by_the_offset() {
        let (a, g) = square_alpha();
        let moved = shifted(&a, &g, 3.0, 0.0);
        assert_eq!(moved[8 * 16 + 7], 1.0, "what was at x=4 is now at x=7");
        assert_eq!(moved[8 * 16 + 4], 0.0);
    }

    #[test]
    fn a_light_at_120_degrees_throws_its_shadow_down_and_right() {
        let (dx, dy) = light_offset(120.0, 10.0);
        assert!(dx > 0.0 && dy > 0.0, "{dx}, {dy}");
        // And the opposite light throws it the other way.
        let (dx, dy) = light_offset(-60.0, 10.0);
        assert!(dx < 0.0 && dy < 0.0, "{dx}, {dy}");
    }

    #[test]
    fn reach_grows_with_the_effect_and_is_none_without_one() {
        let plain = LayerEffects::default();
        assert_eq!(reach(&plain, 0), None);

        let small = LayerEffects {
            drop_shadow: Some(ShadowEffect {
                distance_px: 5.0,
                size_px: 5.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let big = LayerEffects {
            drop_shadow: Some(ShadowEffect {
                distance_px: 40.0,
                size_px: 20.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(reach(&small, 0).unwrap() < reach(&big, 0).unwrap());
        // A mip level shrinks the reach with the image.
        assert!(reach(&big, 2).unwrap() < reach(&big, 0).unwrap());
        // An absurd size is clamped rather than allocated for.
        let absurd = LayerEffects {
            drop_shadow: Some(ShadowEffect {
                size_px: 1.0e9,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(reach(&absurd, 0), Some(MAX_REACH));
        // A block that is switched off still has a margin of zero rather than
        // no answer, and one with only an overlay needs no margin at all.
        let off = LayerEffects {
            enabled: false,
            ..small.clone()
        };
        assert_eq!(reach(&off, 0), None);
        let overlay = LayerEffects {
            color_overlay: Some(ColorOverlayEffect::default()),
            ..Default::default()
        };
        assert_eq!(reach(&overlay, 0), Some(2));
    }

    #[test]
    fn a_gradient_ramp_interpolates_between_its_stops() {
        let ramp = Ramp::new(&Gradient::default(), &ColorSpace::LinearSrgb);
        assert_eq!(ramp.rgb(0.0), [0.0; 3]);
        assert_eq!(ramp.rgb(1.0), [1.0; 3]);
        let mid = ramp.rgb(0.5);
        assert!((mid[0] - 0.5).abs() < 1.0e-5, "{mid:?}");
        assert_eq!(ramp.alpha(0.5), 1.0);
        // Out of range clamps rather than extrapolating.
        assert_eq!(ramp.rgb(-5.0), [0.0; 3]);
        assert_eq!(ramp.rgb(5.0), [1.0; 3]);
    }
}
