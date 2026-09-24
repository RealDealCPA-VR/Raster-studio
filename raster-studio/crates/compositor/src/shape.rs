//! Shape layers: from a [`layer_model::ShapeLayer`]'s path to coverage.
//!
//! As with [`crate::text`], nothing here rasterises anything itself. `vector`
//! owns the scan converter, the fill rules, the stroker with its caps, joins,
//! miter limit and dashes, and the guarantee that no caller input panics; this
//! module parses the layer's stored path, scales it to the mip level, asks
//! `vector` for the two coverage masks a shape can have, and caches the answer.
//!
//! # Fill and stroke are one mask each, not one image
//!
//! Coverage is geometry, and geometry does not depend on the document's colour
//! space; the paint does. Caching the *masks* and colouring them per composite
//! is what keeps one cache correct for documents in different colour spaces,
//! and keeps the entry the size of two bytes per pixel rather than sixteen.
//!
//! # Known limits
//!
//! * A path whose coverage would exceed [`MAX_SHAPE_PIXELS`] is not drawn. That
//!   is 16 megapixels of mask — a shape four thousand pixels on a side — and
//!   the alternative is allocating a buffer sized by geometry a file can name
//!   but nobody can see.
//! * `vector` refuses malformed path data, and a shape whose `path_svg` does
//!   not parse contributes nothing rather than failing the frame.
//!
//! # W9-F: stroke alignment and fill paint
//!
//! An `Inside` or `Outside` stroke is a centred stroke of twice the width,
//! multiplied by the path's own interior coverage (inside) or by its
//! complement (outside) — coverage arithmetic, so the result agrees with the
//! fill along the edge they share. A gradient or pattern fill changes only
//! the paint, never the coverage, and is evaluated per pixel by
//! [`FillShader`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use color::{to_linear, ColorSpace};
use layer_model::{
    GradientStyle, ShapeCap, ShapeFill, ShapeFillRule, ShapeGradientFill, ShapeJoin, ShapeLayer,
    ShapeStroke, ShapeStrokeAlign,
};
use raster::PixelRect;
use vector::{Affine, Cap, Dash, FillOptions, FillRule, Join, Path, StrokeStyle};

/// Largest coverage buffer a single shape layer is rasterised into.
///
/// 2^24 pixels: one byte per pixel per mask, two masks, so 32 MiB at the
/// ceiling. Well inside [`crate::MAX_CANVAS_PIXELS`] on purpose — this is an
/// intermediate held once per shape layer, not a result a caller asked for.
pub const MAX_SHAPE_PIXELS: u64 = 1 << 24;

/// How many rasterised shapes are kept before the cache is dropped wholesale.
const MAX_CACHED_SHAPES: usize = 32;

/// The rasterised coverage of one shape layer, in its own pixel space at one
/// mip level.
///
/// Both slices are `rect.width * rect.height` bytes, row-major over `rect`, and
/// either may be empty when that half of the shape is not painted.
pub(crate) struct ShapeCoverage {
    pub rect: PixelRect,
    pub fill: Vec<u8>,
    pub stroke: Vec<u8>,
    /// W9-F: the rect the fill alone covers — what a gradient fill is fitted
    /// to, so an outside stroke does not stretch the ramp. Empty when unfilled.
    pub fill_rect: PixelRect,
}

impl ShapeCoverage {
    fn is_empty(&self) -> bool {
        self.rect.is_empty() || (self.fill.is_empty() && self.stroke.is_empty())
    }
}

/// Everything about a shape that decides its coverage — the geometry and the
/// stroke's shape, but not either paint's colour.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ShapeKey {
    svg: String,
    level: u8,
    filled: bool,
    rule: u8,
    stroke: Option<StrokeKey>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct StrokeKey {
    width_bits: u32,
    cap: u8,
    join: u8,
    miter_bits: u32,
    dash_bits: Vec<u32>,
    offset_bits: u32,
    align: u8,
}

fn cache() -> MutexGuard<'static, HashMap<ShapeKey, Arc<ShapeCoverage>>> {
    static CACHE: OnceLock<Mutex<HashMap<ShapeKey, Arc<ShapeCoverage>>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn key_for(layer: &ShapeLayer, level: u8) -> ShapeKey {
    ShapeKey {
        svg: layer.path_svg.clone(),
        level,
        filled: layer.fill.is_some(),
        rule: match layer.fill_rule {
            ShapeFillRule::NonZero => 0,
            ShapeFillRule::EvenOdd => 1,
        },
        stroke: layer.stroke.as_ref().map(|s| StrokeKey {
            width_bits: s.width_px.to_bits(),
            cap: match s.cap {
                ShapeCap::Butt => 0,
                ShapeCap::Round => 1,
                ShapeCap::Square => 2,
            },
            join: match s.join {
                ShapeJoin::Miter => 0,
                ShapeJoin::Round => 1,
                ShapeJoin::Bevel => 2,
            },
            miter_bits: s.miter_limit.to_bits(),
            dash_bits: s.dash.iter().map(|v| v.to_bits()).collect(),
            offset_bits: s.dash_offset.to_bits(),
            align: match s.align {
                ShapeStrokeAlign::Inside => 0,
                ShapeStrokeAlign::Center => 1,
                ShapeStrokeAlign::Outside => 2,
            },
        }),
    }
}

/// The layer's coverage in its own space at `level`, or `None` when there is
/// nothing to draw.
pub(crate) fn coverage(layer: &ShapeLayer, level: u8) -> Option<Arc<ShapeCoverage>> {
    if !layer.is_drawable() {
        return None;
    }
    let key = key_for(layer, level);
    let mut cache = cache();
    if let Some(hit) = cache.get(&key) {
        return (!hit.is_empty()).then(|| Arc::clone(hit));
    }
    let built = Arc::new(rasterize(layer, level)?);
    if cache.len() >= MAX_CACHED_SHAPES {
        cache.clear();
    }
    cache.insert(key, Arc::clone(&built));
    (!built.is_empty()).then_some(built)
}

/// The rect the layer paints in its own space at `level`, empty when it paints
/// nothing.
pub(crate) fn ink_bounds(layer: &ShapeLayer, level: u8) -> PixelRect {
    coverage(layer, level).map_or(PixelRect::new(0, 0, 0, 0), |c| c.rect)
}

fn rasterize(layer: &ShapeLayer, level: u8) -> Option<ShapeCoverage> {
    let path = vector::parse_svg(&layer.path_svg).ok()?;
    // A transform is authored in level-0 pixels, and so is a path.
    let s = f64::from(2.0f32.powi(-(level as i32)));
    let path = if level == 0 {
        path
    } else {
        path.transform(&Affine::scale(s, s))
    };

    let rule = match layer.fill_rule {
        ShapeFillRule::NonZero => FillRule::NonZero,
        ShapeFillRule::EvenOdd => FillRule::EvenOdd,
    };
    let fill = layer
        .fill
        .is_some()
        .then(|| vector::fill(&path, &FillOptions::with_rule(rule)).ok())
        .flatten();
    let stroke = layer
        .stroke
        .as_ref()
        .and_then(|st| aligned_stroke(&path, st, s, rule));

    let fill_rect = fill.as_ref().map(mask_rect).unwrap_or(EMPTY);
    let rect = union(fill_rect, stroke.as_ref().map(|(r, _)| *r).unwrap_or(EMPTY));
    if rect.is_empty() || u64::from(rect.width) * u64::from(rect.height) > MAX_SHAPE_PIXELS {
        return None;
    }
    Some(ShapeCoverage {
        rect,
        fill: fill.map(|m| resample_into(&m, rect)).unwrap_or_default(),
        stroke: stroke
            .map(|(r, data)| resample_raw(r, &data, rect))
            .unwrap_or_default(),
        fill_rect,
    })
}

/// W9-F: the stroke's coverage placed by its alignment, as a rect and a
/// row-major buffer over it. `Center` is the plain stroke; `Inside` and
/// `Outside` are a stroke twice as wide, kept where the path's interior is
/// (inside) or is not (outside).
fn aligned_stroke(
    path: &Path,
    st: &ShapeStroke,
    s: f64,
    rule: FillRule,
) -> Option<(PixelRect, Vec<u8>)> {
    if st.align == ShapeStrokeAlign::Center {
        let m = stroke_mask(path, st, s)?;
        return Some((mask_rect(&m), m.coverage().to_vec()));
    }
    let doubled = ShapeStroke {
        width_px: st.width_px * 2.0,
        ..st.clone()
    };
    let band = stroke_mask(path, &doubled, s)?;
    let interior = vector::fill(path, &FillOptions::with_rule(rule)).ok();
    let band_rect = mask_rect(&band);
    let inside = st.align == ShapeStrokeAlign::Inside;
    let rect = match (&interior, inside) {
        (Some(f), true) => intersect(band_rect, mask_rect(f)),
        (None, true) => EMPTY,
        (_, false) => band_rect,
    };
    if rect.is_empty() {
        return None;
    }
    let band_px = resample_into(&band, rect);
    let interior_px = interior
        .as_ref()
        .map(|f| resample_into(f, rect))
        .unwrap_or_else(|| vec![0; band_px.len()]);
    let out = band_px
        .iter()
        .zip(&interior_px)
        .map(|(b, f)| {
            let keep = if inside {
                u32::from(*f)
            } else {
                255 - u32::from(*f)
            };
            ((u32::from(*b) * keep + 127) / 255) as u8
        })
        .collect();
    Some((rect, out))
}

fn intersect(a: PixelRect, b: PixelRect) -> PixelRect {
    let (x0, y0) = (a.x.max(b.x), a.y.max(b.y));
    let (x1, y1) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    if x1 <= x0 || y1 <= y0 {
        return EMPTY;
    }
    PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
}

/// The stroke's outline, rasterised. `s` scales document pixels to level ones.
fn stroke_mask(path: &Path, st: &ShapeStroke, s: f64) -> Option<vector::CoverageMask> {
    let width = f64::from(st.width_px) * s;
    if !width.is_finite() || width <= 0.0 {
        return None;
    }
    let dash = (!st.dash.is_empty()).then(|| Dash {
        pattern: st.dash.iter().map(|v| f64::from(*v) * s).collect(),
        offset: f64::from(st.dash_offset) * s,
    });
    let style = StrokeStyle {
        width,
        cap: match st.cap {
            ShapeCap::Butt => Cap::Butt,
            ShapeCap::Round => Cap::Round,
            ShapeCap::Square => Cap::Square,
        },
        join: match st.join {
            ShapeJoin::Miter => Join::Miter,
            ShapeJoin::Round => Join::Round,
            ShapeJoin::Bevel => Join::Bevel,
        },
        miter_limit: f64::from(st.miter_limit),
        dash,
        tolerance: vector::DEFAULT_TOLERANCE,
    };
    let outline = vector::stroke(path, &style).ok()?;
    // A stroke outline is a closed, positively-oriented region: it is filled
    // non-zero whatever rule the shape's *interior* uses, or a dash's two
    // overlapping caps would cancel each other out.
    vector::fill(&outline, &FillOptions::with_rule(FillRule::NonZero)).ok()
}

const EMPTY: PixelRect = PixelRect::new(0, 0, 0, 0);

fn mask_rect(m: &vector::CoverageMask) -> PixelRect {
    let o = m.origin();
    PixelRect::new(i64::from(o.x), i64::from(o.y), m.width(), m.height())
}

fn union(a: PixelRect, b: PixelRect) -> PixelRect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    match (u32::try_from(x1 - x0), u32::try_from(y1 - y0)) {
        (Ok(w), Ok(h)) => PixelRect::new(x0, y0, w, h),
        _ => a,
    }
}

/// Copy a `vector` mask into a buffer covering `rect`, zero outside it.
fn resample_into(m: &vector::CoverageMask, rect: PixelRect) -> Vec<u8> {
    resample_raw(mask_rect(m), m.coverage(), rect)
}

/// Copy a row-major buffer over `src` into a buffer covering `rect`, zero
/// outside `src`.
fn resample_raw(src: PixelRect, data: &[u8], rect: PixelRect) -> Vec<u8> {
    let mut out = vec![0u8; rect.width as usize * rect.height as usize];
    let stride = src.width as usize;
    let x0 = rect.x.max(src.x);
    let x1 = rect.right().min(src.right());
    let y0 = rect.y.max(src.y);
    let y1 = rect.bottom().min(src.bottom());
    for y in y0..y1 {
        let drow = (y - rect.y) as usize * rect.width as usize;
        let srow = (y - src.y) as usize * stride;
        for x in x0..x1 {
            out[drow + (x - rect.x) as usize] = data[srow + (x - src.x) as usize];
        }
    }
    out
}

// ---------------------------------------------------------------------------
// W9-F: fill paint
// ---------------------------------------------------------------------------

/// What paints a shape's fill coverage, resolved for one composite: premultiplied
/// linear RGBA per pixel of the layer's own space at one mip level.
pub(crate) enum FillShader {
    Solid([f32; 4]),
    Gradient {
        ramp: LinearRamp,
        style: GradientStyle,
        reverse: bool,
        cx: f32,
        cy: f32,
        extent: f32,
        ca: f32,
        sa: f32,
    },
    Pattern {
        tile: Arc<DecodedTile>,
        /// Level pixels per document pixel.
        s: f32,
        scale: f32,
        ox: f32,
        oy: f32,
        ca: f32,
        sa: f32,
    },
}

impl FillShader {
    /// The shader for `layer`'s fill, or `None` when it is unfilled (or a
    /// pattern fill carries no pixels).
    pub(crate) fn new(
        layer: &ShapeLayer,
        cov: &ShapeCoverage,
        level: u8,
        space: &ColorSpace,
    ) -> Option<Self> {
        let finite = |v: f32, or: f32| if v.is_finite() { v } else { or };
        match layer.fill_kind() {
            ShapeFill::None => None,
            ShapeFill::Solid(c) => Some(FillShader::Solid(premul_linear(c, space))),
            ShapeFill::Gradient(g) => Some(gradient_shader(
                g,
                cov.fill_rect,
                2.0f32.powi(-(level as i32)),
                space,
            )),
            ShapeFill::Pattern(p) => {
                let tile = p.tile.as_ref()?;
                let s = 2.0f32.powi(-(level as i32));
                let scale = if p.scale.is_finite() && p.scale > 0.0 {
                    p.scale.clamp(0.01, 1000.0)
                } else {
                    1.0
                };
                let a = finite(p.angle_deg, 0.0).to_radians();
                Some(FillShader::Pattern {
                    tile: decode_tile(tile, space),
                    s,
                    scale,
                    ox: finite(p.offset_px[0], 0.0),
                    oy: finite(p.offset_px[1], 0.0),
                    ca: a.cos(),
                    sa: a.sin(),
                })
            }
        }
    }

    /// Premultiplied linear paint at level pixel `(x, y)`.
    pub(crate) fn at(&self, x: i64, y: i64) -> [f32; 4] {
        match self {
            FillShader::Solid(c) => *c,
            FillShader::Gradient {
                ramp,
                style,
                reverse,
                cx,
                cy,
                extent,
                ca,
                sa,
            } => {
                let px = x as f32 + 0.5 - cx;
                let py = y as f32 + 0.5 - cy;
                // Image y grows downward, so a positive angle runs upward.
                let u = px * ca - py * sa;
                let v = px * sa + py * ca;
                let mut t = match style {
                    GradientStyle::Linear => 0.5 + u / (2.0 * extent),
                    GradientStyle::Reflected => (u / extent).abs(),
                    GradientStyle::Radial => (u * u + v * v).sqrt() / extent,
                    GradientStyle::Diamond => (u.abs() + v.abs()) / extent,
                    GradientStyle::Angle => 0.5 + v.atan2(u) / std::f32::consts::TAU,
                };
                if *reverse {
                    t = 1.0 - t;
                }
                ramp.at(t)
            }
            FillShader::Pattern {
                tile,
                s,
                scale,
                ox,
                oy,
                ca,
                sa,
            } => {
                // The pixel centre in document pixels, anchored at the layer's
                // own origin (so the pattern travels with the layer).
                let dx = (x as f32 + 0.5) / s - ox;
                let dy = (y as f32 + 0.5) / s - oy;
                let u = (dx * ca + dy * sa) / scale;
                let v = (-dx * sa + dy * ca) / scale;
                tile.at(u.floor() as i64, v.floor() as i64)
            }
        }
    }
}

fn premul_linear(c: [f32; 4], space: &ColorSpace) -> [f32; 4] {
    let a = layer_model::blend::unit(c[3]);
    let lin = to_linear(space, [c[0], c[1], c[2]]);
    [lin[0] * a, lin[1] * a, lin[2] * a, a]
}

fn finite_or0(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

fn gradient_shader(
    g: &ShapeGradientFill,
    fit: PixelRect,
    level_scale: f32,
    space: &ColorSpace,
) -> FillShader {
    let scale = if g.scale.is_finite() && g.scale > 0.0 {
        g.scale.clamp(0.01, 1000.0)
    } else {
        1.0
    };
    let a = if g.angle_deg.is_finite() {
        g.angle_deg.to_radians()
    } else {
        0.0
    };
    FillShader::Gradient {
        ramp: LinearRamp::new(&g.gradient, space),
        style: g.style,
        reverse: g.reverse,
        cx: fit.x as f32 + fit.width as f32 * 0.5 + finite_or0(g.offset_px[0]) * level_scale,
        cy: fit.y as f32 + fit.height as f32 * 0.5 + finite_or0(g.offset_px[1]) * level_scale,
        extent: (fit.width.max(fit.height) as f32 * 0.5 * scale).max(1.0),
        ca: a.cos(),
        sa: a.sin(),
    }
}

/// A gradient's stops in linear light, sampled with each stop's midpoint.
pub(crate) struct LinearRamp {
    colors: Vec<(f32, [f32; 3], f32)>,
    alphas: Vec<(f32, f32, f32)>,
}

impl LinearRamp {
    fn new(g: &layer_model::Gradient, space: &ColorSpace) -> Self {
        let clamp_mid = |m: f32| layer_model::blend::unit(m).clamp(0.05, 0.95);
        let mut colors: Vec<(f32, [f32; 3], f32)> = g
            .stops
            .iter()
            .filter(|s| s.position.is_finite())
            .map(|s| {
                (
                    s.position.clamp(0.0, 1.0),
                    to_linear(space, [s.color[0], s.color[1], s.color[2]]),
                    clamp_mid(s.midpoint),
                )
            })
            .collect();
        colors.sort_by(|a, b| a.0.total_cmp(&b.0));
        if colors.is_empty() {
            colors.push((0.0, [0.0; 3], 0.5));
        }
        let source = if g.alpha_stops.is_empty() {
            &g.stops
        } else {
            &g.alpha_stops
        };
        let mut alphas: Vec<(f32, f32, f32)> = source
            .iter()
            .filter(|s| s.position.is_finite())
            .map(|s| {
                (
                    s.position.clamp(0.0, 1.0),
                    layer_model::blend::unit(s.color[3]),
                    clamp_mid(s.midpoint),
                )
            })
            .collect();
        alphas.sort_by(|a, b| a.0.total_cmp(&b.0));
        if alphas.is_empty() {
            alphas.push((0.0, 1.0, 0.5));
        }
        Self { colors, alphas }
    }

    /// The pair of stops `t` falls between and the midpoint-skewed fraction.
    fn span(positions: &[(f32, f32)], t: f32) -> (usize, usize, f32) {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let last = positions.len() - 1;
        if t <= positions[0].0 {
            return (0, 0, 0.0);
        }
        if t >= positions[last].0 {
            return (last, last, 0.0);
        }
        let hi = positions.iter().position(|p| p.0 > t).unwrap_or(last);
        let lo = hi.saturating_sub(1);
        let (p0, mid) = positions[lo];
        let p1 = positions[hi].0;
        let f = if p1 > p0 { (t - p0) / (p1 - p0) } else { 0.0 };
        // Photoshop's midpoint: the fraction that lands halfway is `mid`.
        let k = f.powf(0.5f32.ln() / mid.ln());
        (lo, hi, k)
    }

    fn at(&self, t: f32) -> [f32; 4] {
        let cp: Vec<(f32, f32)> = self.colors.iter().map(|c| (c.0, c.2)).collect();
        let (lo, hi, k) = Self::span(&cp, t);
        let (a, b) = (self.colors[lo].1, self.colors[hi].1);
        let rgb = [
            a[0] + (b[0] - a[0]) * k,
            a[1] + (b[1] - a[1]) * k,
            a[2] + (b[2] - a[2]) * k,
        ];
        let ap: Vec<(f32, f32)> = self.alphas.iter().map(|c| (c.0, c.2)).collect();
        let (lo, hi, k) = Self::span(&ap, t);
        let alpha = self.alphas[lo].1 + (self.alphas[hi].1 - self.alphas[lo].1) * k;
        [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha]
    }
}

/// A pattern tile decoded to premultiplied linear RGBA.
pub(crate) struct DecodedTile {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl DecodedTile {
    fn at(&self, u: i64, v: i64) -> [f32; 4] {
        let x = u.rem_euclid(self.w as i64) as usize;
        let y = v.rem_euclid(self.h as i64) as usize;
        self.px[y * self.w + x]
    }
}

/// Decoded tiles, keyed by content and the colour space they were decoded
/// for; dropped wholesale past a small bound.
fn decode_tile(tile: &layer_model::PatternTile, space: &ColorSpace) -> Arc<DecodedTile> {
    type Key = (u64, u32, u32, String);
    static TILES: OnceLock<Mutex<HashMap<Key, Arc<DecodedTile>>>> = OnceLock::new();
    let key = (
        tile.content_hash(),
        tile.width(),
        tile.height(),
        format!("{space:?}"),
    );
    let map = TILES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = map.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
        return Arc::clone(hit);
    }
    let px = tile
        .rgba8()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| {
            premul_linear(
                [
                    f32::from(c[0]) / 255.0,
                    f32::from(c[1]) / 255.0,
                    f32::from(c[2]) / 255.0,
                    f32::from(c[3]) / 255.0,
                ],
                space,
            )
        })
        .collect();
    let decoded = Arc::new(DecodedTile {
        w: tile.width() as usize,
        h: tile.height() as usize,
        px,
    });
    let mut map = map.lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() >= MAX_CACHED_SHAPES {
        map.clear();
    }
    map.insert(key, Arc::clone(&decoded));
    decoded
}

/// W9-F: fold the paint-only shape fields (fill paint, stroke alignment)
/// into a layer fingerprint, so a tile cache never serves a stale gradient.
pub(crate) fn hash_paint<H: std::hash::Hasher>(layer: &ShapeLayer, h: &mut H) {
    use layer_model::ShapeFillPaint;
    use std::hash::Hash;
    if let Some(st) = &layer.stroke {
        st.align.hash(h);
    }
    let stops = |v: &[layer_model::GradientStop], h: &mut H| {
        v.len().hash(h);
        for s in v {
            s.position.to_bits().hash(h);
            s.midpoint.to_bits().hash(h);
            for c in s.color {
                c.to_bits().hash(h);
            }
        }
    };
    match &layer.fill_paint {
        ShapeFillPaint::Solid => 0u8.hash(h),
        ShapeFillPaint::Gradient(g) => {
            1u8.hash(h);
            stops(&g.gradient.stops, h);
            stops(&g.gradient.alpha_stops, h);
            g.gradient.smoothness.to_bits().hash(h);
            g.style.hash(h);
            g.angle_deg.to_bits().hash(h);
            g.reverse.hash(h);
            g.scale.to_bits().hash(h);
            g.offset_px[0].to_bits().hash(h);
            g.offset_px[1].to_bits().hash(h);
        }
        ShapeFillPaint::Pattern(p) => {
            2u8.hash(h);
            p.tile
                .as_ref()
                .map(|t| (t.content_hash(), t.width(), t.height()))
                .hash(h);
            p.scale.to_bits().hash(h);
            p.offset_px[0].to_bits().hash(h);
            p.offset_px[1].to_bits().hash(h);
            p.angle_deg.to_bits().hash(h);
            p.link_with_layer.hash(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> ShapeLayer {
        ShapeLayer::from_svg("M10 10 L40 10 L40 40 L10 40 Z")
    }

    #[test]
    fn a_filled_square_covers_its_interior_and_nothing_else() {
        let cov = coverage(&square(), 0).expect("coverage");
        assert_eq!(cov.rect, PixelRect::new(10, 10, 30, 30));
        assert!(cov.stroke.is_empty(), "no stroke asked for");
        let at = |x: i64, y: i64| {
            cov.fill
                [(y - cov.rect.y) as usize * cov.rect.width as usize + (x - cov.rect.x) as usize]
        };
        assert_eq!(at(25, 25), 255);
        assert_eq!(at(10, 10), 255, "the top-left pixel is fully inside");
        assert_eq!(at(39, 39), 255);
    }

    #[test]
    fn the_even_odd_rule_punches_a_hole_the_non_zero_rule_does_not() {
        // Two nested squares wound the same way: non-zero merges them, even-odd
        // makes the inner one a hole.
        let svg = "M0 0 L40 0 L40 40 L0 40 Z M10 10 L30 10 L30 30 L10 30 Z";
        let mut nonzero = ShapeLayer::from_svg(svg);
        nonzero.fill_rule = ShapeFillRule::NonZero;
        let mut evenodd = ShapeLayer::from_svg(svg);
        evenodd.fill_rule = ShapeFillRule::EvenOdd;

        let a = coverage(&nonzero, 0).unwrap();
        let b = coverage(&evenodd, 0).unwrap();
        let centre = |c: &ShapeCoverage| {
            c.fill[(20 - c.rect.y) as usize * c.rect.width as usize + (20 - c.rect.x) as usize]
        };
        assert_eq!(centre(&a), 255, "non-zero fills the middle");
        assert_eq!(centre(&b), 0, "even-odd leaves a hole");
    }

    #[test]
    fn a_stroke_lands_on_the_path_and_widens_the_covered_rect() {
        let mut s = square();
        s.fill = None;
        s.stroke = Some(ShapeStroke {
            width_px: 4.0,
            ..Default::default()
        });
        let cov = coverage(&s, 0).expect("coverage");
        assert!(cov.fill.is_empty());
        // Half the width either side of the path, so the rect grows by 2.
        assert_eq!(cov.rect, PixelRect::new(8, 8, 34, 34));
        let at = |x: i64, y: i64| {
            cov.stroke
                [(y - cov.rect.y) as usize * cov.rect.width as usize + (x - cov.rect.x) as usize]
        };
        assert_eq!(at(10, 25), 255, "on the left edge of the square");
        assert_eq!(at(25, 25), 0, "the interior is not stroked");
        assert_eq!(at(8, 8), 255, "the outer corner of the join");
    }

    #[test]
    fn a_dashed_stroke_leaves_gaps_a_solid_one_does_not() {
        let mut solid = square();
        solid.fill = None;
        solid.stroke = Some(ShapeStroke {
            width_px: 2.0,
            ..Default::default()
        });
        let mut dashed = solid.clone();
        dashed.stroke.as_mut().unwrap().dash = vec![4.0, 4.0];

        let ink = |s: &ShapeLayer| -> u64 {
            coverage(s, 0)
                .unwrap()
                .stroke
                .iter()
                .map(|v| u64::from(*v))
                .sum()
        };
        let (a, b) = (ink(&solid), ink(&dashed));
        assert!(b > 0, "a dashed stroke still draws");
        assert!(
            b < a * 3 / 4,
            "half on / half off must cost much less ink: {b} vs {a}"
        );
    }

    /// W9-F: alignment moves the stroke off the path's centre line. Outside
    /// grows the stroked bounds by the full width; inside keeps them to the
    /// shape's own; centre grows them by half.
    #[test]
    fn outside_alignment_grows_the_stroked_bounds_by_the_width() {
        let rect_for = |align: ShapeStrokeAlign| {
            let mut s = square();
            s.fill = None;
            s.stroke = Some(ShapeStroke {
                width_px: 4.0,
                align,
                ..Default::default()
            });
            coverage(&s, 0).expect("coverage").rect
        };
        assert_eq!(
            rect_for(ShapeStrokeAlign::Center),
            PixelRect::new(8, 8, 34, 34)
        );
        assert_eq!(
            rect_for(ShapeStrokeAlign::Outside),
            PixelRect::new(6, 6, 38, 38),
            "the whole width sits outside the 10..40 square"
        );
        assert_eq!(
            rect_for(ShapeStrokeAlign::Inside),
            PixelRect::new(10, 10, 30, 30)
        );

        let mut out = square();
        out.fill = None;
        out.stroke = Some(ShapeStroke {
            width_px: 4.0,
            align: ShapeStrokeAlign::Outside,
            ..Default::default()
        });
        let cov = coverage(&out, 0).unwrap();
        let at = |x: i64, y: i64| {
            cov.stroke
                [(y - cov.rect.y) as usize * cov.rect.width as usize + (x - cov.rect.x) as usize]
        };
        assert_eq!(at(7, 25), 255, "just outside the left edge is stroked");
        assert_eq!(at(11, 25), 0, "just inside is not");
    }

    /// W9-F: a gradient-filled rectangle composites a ramp through the real
    /// compositor — black at the left edge, white at the right, rising
    /// monotonically in between — and a pattern fill tiles its pixels.
    #[test]
    fn a_gradient_filled_rectangle_composites_a_ramp() {
        use crate::composite::{composite_rect, CompositeOptions};
        use crate::testkit::TestDoc;
        use layer_model::{Layer, LayerKind, ShapeFillPaint, ShapeGradientFill};

        let mut t = TestDoc::linear(80, 20);
        let mut shape = ShapeLayer::from_svg("M0 0 L80 0 L80 20 L0 20 Z");
        shape.fill_paint = ShapeFillPaint::Gradient(ShapeGradientFill {
            // Left to right: black (stop 0) to white (stop 1).
            angle_deg: 0.0,
            ..ShapeGradientFill::default()
        });
        t.push(Layer::with_kind("Ramp", LayerKind::Shape(shape)));
        let (doc, src) = t.finish();
        let out = composite_rect(
            &doc,
            &src,
            PixelRect::new(0, 0, 80, 20),
            0,
            CompositeOptions::default(),
        )
        .expect("composite");
        let red: Vec<f32> = (0..80).map(|x| out.get(x, 10)[0]).collect();
        assert!(red[0] < 0.02, "black at the left: {}", red[0]);
        assert!(red[79] > 0.98, "white at the right: {}", red[79]);
        assert!((red[40] - 0.5).abs() < 0.03, "halfway grey: {}", red[40]);
        assert!(
            red.windows(2).all(|w| w[1] >= w[0]),
            "a ramp never steps back: {red:?}"
        );
        assert_eq!(out.get(40, 10)[3], 1.0, "the ramp is opaque");

        // A pattern fill: a 2x1 tile, red then blue, repeated across.
        let mut t = TestDoc::linear(8, 4);
        let mut shape = ShapeLayer::from_svg("M0 0 L8 0 L8 4 L0 4 Z");
        let tile = layer_model::PatternTile::new("rb", 2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255])
            .unwrap();
        shape.fill_paint = ShapeFillPaint::Pattern(layer_model::PatternFill {
            tile: Some(tile),
            ..Default::default()
        });
        t.push(Layer::with_kind("Tiles", LayerKind::Shape(shape)));
        let (doc, src) = t.finish();
        let out = composite_rect(
            &doc,
            &src,
            PixelRect::new(0, 0, 8, 4),
            0,
            CompositeOptions::default(),
        )
        .expect("composite");
        assert_eq!(out.get(0, 1), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(out.get(1, 1), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(out.get(6, 2), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_mip_level_scales_the_geometry_rather_than_the_output() {
        let full = coverage(&square(), 0).unwrap();
        let half = coverage(&square(), 1).unwrap();
        assert_eq!(full.rect, PixelRect::new(10, 10, 30, 30));
        assert_eq!(half.rect, PixelRect::new(5, 5, 15, 15));
    }

    #[test]
    fn unparseable_or_unpainted_shapes_draw_nothing_instead_of_failing() {
        assert!(coverage(&ShapeLayer::from_svg("not a path at all"), 0).is_none());
        assert!(coverage(&ShapeLayer::default(), 0).is_none(), "no geometry");
        let mut bare = square();
        bare.fill = None;
        assert!(coverage(&bare, 0).is_none(), "nothing to paint it with");
        // Geometry too large to rasterise is declined, not allocated.
        let huge = ShapeLayer::from_svg("M0 0 L100000 0 L100000 100000 Z");
        assert!(coverage(&huge, 0).is_none());
    }

    #[test]
    fn coverage_is_cached_by_geometry_not_by_colour() {
        let mut a = square();
        a.fill = Some([1.0, 0.0, 0.0, 1.0]);
        let mut b = square();
        b.fill = Some([0.0, 1.0, 0.0, 1.0]);
        assert!(
            Arc::ptr_eq(&coverage(&a, 0).unwrap(), &coverage(&b, 0).unwrap()),
            "two colours of one shape share one rasterisation"
        );
        // A different stroke is different geometry, so a different entry.
        let mut c = a.clone();
        c.stroke = Some(ShapeStroke::default());
        assert!(!Arc::ptr_eq(
            &coverage(&a, 0).unwrap(),
            &coverage(&c, 0).unwrap()
        ));
    }
}
