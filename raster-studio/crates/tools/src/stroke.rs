//! From dabs to one undoable command.
//!
//! This is the path the headline bug was missing: a stroke used to be captured
//! and dropped. Now every stroke tool runs the same five steps on pointer-up —
//!
//! 1. union the dab bounds, clipped to the canvas, into a region;
//! 2. load the tiles covering that region into a [`crate::patch::ColorPatch`]
//!    (or [`crate::patch::CoveragePatch`] for a mask);
//! 3. rasterise every dab into a [`StrokeBuffer`], a single plane of coverage;
//! 4. composite that plane onto the patch **once**;
//! 5. commit the patch, and emit exactly one [`Command::PaintTiles`].
//!
//! Step 3 and step 4 being separate is what stops overlapping dabs from
//! darkening each other. Within a stroke, flow accumulates in the coverage
//! plane (`a ← a + (1−a)·flow·dab`), which saturates at 1.0 no matter how many
//! dabs pile up; the stroke's *opacity* is applied once in step 4. Composite
//! per-dab instead and a scribble over one spot goes black — the artefact every
//! naive brush has.

use color::{linear_srgb_luminance, linear_to_srgb, premultiply, unpremultiply};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use editor_core::{Command, PixelKey, PixelTarget, Selection, TileDelta};
use filters::{blur::gaussian_blur, sharpen::unsharp_mask, EdgeMode, FilterBuffer};
use glam::{IVec2, Vec2};
use layer_model::BlendMode;
use raster::{PixelRect, TileCoord, TileHash, TILE_SIZE};
use serde::{Deserialize, Serialize};

use crate::brush::{BrushSettings, Dab, DabEmitter};
use crate::error::ToolError;
use crate::patch::{ColorPatch, CoveragePatch, TileBox, MAX_PATCH_TILES};
use crate::tiles::TileAccess;
use crate::tool::ToolSetting;
use crate::tool::{PaintTarget, Pattern, PointerEvent, Tool, ToolContext, ToolId};

/// Which tones a dodge/burn/sponge dab acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToneRange {
    Shadows,
    #[default]
    Midtones,
    Highlights,
}

impl ToneRange {
    /// How strongly this range claims a pixel of the given **encoded**
    /// luminance.
    ///
    /// Encoded, not linear: "midtones" is a statement about how bright
    /// something *looks*, and mid grey is 0.5 on the display curve, not 0.5 in
    /// light. A Gaussian rather than a hard band, so a dodge does not leave a
    /// visible seam where the range ends.
    pub fn weight(self, encoded_luma: f32) -> f32 {
        let center = match self {
            ToneRange::Shadows => 0.15,
            ToneRange::Midtones => 0.5,
            ToneRange::Highlights => 0.85,
        };
        let sigma = 0.3;
        let d = (encoded_luma.clamp(0.0, 1.0) - center) / sigma;
        (-0.5 * d * d).exp()
    }
}

/// Sponge direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SpongeMode {
    #[default]
    Desaturate,
    Saturate,
}

/// What a stroke does to the pixels under it.
///
/// One enum rather than one trait per tool, because the *stroke* machinery —
/// stamping, accumulation, clipping, committing — is identical for all of them
/// and only the per-pixel rule differs.
#[derive(Debug, Clone, PartialEq)]
pub enum StrokeOp {
    /// Lay down `color` (straight-alpha **linear** RGBA).
    Paint {
        color: [f32; 4],
    },
    /// Remove coverage.
    Erase,
    /// Card 061: regrade the coverage INSIDE the painted band — a local
    /// smooth (the band's stair-steps and noise average out) followed by a
    /// mild re-grade that keeps the edge defined. Definite coverage is
    /// untouched by construction: a plateau is a fixed point of both stages,
    /// so hard interior/exterior stays hard. Deterministic manual refinement
    /// — automatic matting is explicitly out of scope.
    RefineBoundary {
        /// Drives both the smooth's reach (sigma) and the re-grade's gain,
        /// 0.0..=1.0.
        strength: f32,
    },
    /// Replace pixels within `tolerance` of the colour first touched, keeping
    /// their luminance so texture survives.
    ColorReplacement {
        color: [f32; 4],
        tolerance: f32,
    },
    /// Erase pixels within `tolerance` of the colour first touched, leaving
    /// everything else — the background eraser.
    BackgroundErase {
        tolerance: f32,
    },
    /// Copy from elsewhere in the image.
    CloneStamp,
    /// Copy from the active pattern.
    PatternStamp,
    /// Copy *texture* from elsewhere while keeping the destination's colour and
    /// shading — the healing brush.
    Healing {
        softness: f32,
    },
    /// Diffuse the surrounding pixels inward over the dab — spot healing, which
    /// needs no source.
    SpotHealing,
    Blur {
        radius: f32,
    },
    Sharpen {
        amount: f32,
        radius: f32,
    },
    /// Drag colour along the stroke. Sequential by nature, so it is the one op
    /// that does not go through the coverage plane.
    Smudge {
        strength: f32,
    },
    Dodge {
        exposure: f32,
        range: ToneRange,
    },
    Burn {
        exposure: f32,
        range: ToneRange,
    },
    Sponge {
        amount: f32,
        mode: SpongeMode,
    },
}

impl StrokeOp {
    /// `true` when this op reads pixels from a second location.
    pub fn needs_source(&self) -> bool {
        matches!(self, StrokeOp::CloneStamp | StrokeOp::Healing { .. })
    }

    /// `true` when this op lays a source colour *over* the layer — the
    /// [`Blend::Over`] preparations: painting, the clone stamp and the
    /// pattern stamp — and therefore has a colour a paint blend mode can act
    /// on. The retouching ops mix toward a computed target ([`Blend::Lerp`])
    /// or take coverage away ([`Blend::Erase`]); there is no source colour
    /// to blend, so [`crate::BLEND_MODE_KEY`] is refused for them rather than
    /// accepted and ignored. The options bar offers the Mode combo by this
    /// same predicate ([`crate::composites_strokes`]).
    pub fn composites_source(&self) -> bool {
        matches!(
            self,
            StrokeOp::Paint { .. } | StrokeOp::CloneStamp | StrokeOp::PatternStamp
        )
    }

    /// `true` when this op is meaningful on an 8-bit coverage mask.
    ///
    /// Only painting and erasing are. A mask stores how much of the layer
    /// shows through, so a clone stamp, a dodge or a sponge has nothing to
    /// operate on; those are refused with
    /// [`crate::ToolError::UnsupportedOnMask`] rather than silently retargeted
    /// at the layer behind the mask. Blurring a mask is a real operation, but
    /// it is a filter over the coverage plane rather than a stamped dab, so it
    /// is not one of these either.
    pub fn works_on_mask(&self) -> bool {
        matches!(
            self,
            StrokeOp::Paint { .. } | StrokeOp::Erase | StrokeOp::RefineBoundary { .. }
        )
    }
}

/// How a prepared per-pixel value combines with what is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Blend {
    /// Source-over: the value is laid *on top*, respecting its own alpha.
    Over,
    /// Mix toward the value, keeping the destination's alpha shape.
    Lerp,
    /// Scale the destination's coverage down.
    Erase,
}

/// A single plane of stroke coverage, `0..=1`, over the region a stroke
/// touches.
#[derive(Debug, Clone)]
pub struct StrokeBuffer {
    rect: PixelRect,
    data: Vec<f32>,
}

impl StrokeBuffer {
    /// The tight region the dabs cover, clipped to `clip`.
    pub fn bounds_of(dabs: &[Dab], clip: PixelRect) -> Option<PixelRect> {
        let mut lo = IVec2::new(i32::MAX, i32::MAX);
        let mut hi = IVec2::new(i32::MIN, i32::MIN);
        for d in dabs {
            let (a, b) = d.bounds();
            lo = lo.min(a);
            hi = hi.max(b);
        }
        if lo.x >= hi.x || lo.y >= hi.y {
            return None;
        }
        let x0 = (lo.x as i64).max(clip.x);
        let y0 = (lo.y as i64).max(clip.y);
        let x1 = (hi.x as i64).min(clip.right());
        let y1 = (hi.y as i64).min(clip.bottom());
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// Rasterise every dab into one accumulating plane.
    ///
    /// Accumulation is `a ← a + (1 − a)·flow·coverage`, which is exactly
    /// "paint over what is already wet": it approaches 1.0 and never exceeds
    /// it, so overlapping dabs build up smoothly instead of summing.
    pub fn rasterize(dabs: &[Dab], rect: PixelRect) -> Result<Self, ToolError> {
        let area = (rect.width as u64) * (rect.height as u64);
        let max_area = MAX_PATCH_TILES * (raster::TILE_SIZE as u64) * (raster::TILE_SIZE as u64);
        if rect.is_empty() {
            return Err(ToolError::Degenerate);
        }
        if area > max_area {
            return Err(ToolError::RegionTooLarge {
                tiles: area / ((raster::TILE_SIZE as u64) * (raster::TILE_SIZE as u64)),
                max: MAX_PATCH_TILES,
            });
        }
        let mut data = vec![0.0f32; area as usize];
        let w = rect.width as i64;
        for d in dabs {
            let (lo, hi) = d.bounds();
            let x0 = (lo.x as i64).max(rect.x);
            let y0 = (lo.y as i64).max(rect.y);
            let x1 = (hi.x as i64).min(rect.right());
            let y1 = (hi.y as i64).min(rect.bottom());
            for y in y0..y1 {
                for x in x0..x1 {
                    let c = d.coverage_pixel(x as i32, y as i32) * d.flow;
                    if c <= 0.0 {
                        continue;
                    }
                    let i = ((y - rect.y) * w + (x - rect.x)) as usize;
                    let a = data[i];
                    data[i] = a + (1.0 - a) * c;
                }
            }
        }
        Ok(Self { rect, data })
    }

    pub fn rect(&self) -> PixelRect {
        self.rect
    }

    pub fn get(&self, p: IVec2) -> f32 {
        let x = p.x as i64;
        let y = p.y as i64;
        if x < self.rect.x || y < self.rect.y || x >= self.rect.right() || y >= self.rect.bottom() {
            return 0.0;
        }
        let i = ((y - self.rect.y) * self.rect.width as i64 + (x - self.rect.x)) as usize;
        self.data[i]
    }

    /// W4-B: the tight bounds of the pixels inside `r` with any coverage, or
    /// `None` when there are none.
    pub fn covered_within(&self, r: PixelRect) -> Option<PixelRect> {
        let r = intersect(r, self.rect)?;
        let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        let w = self.rect.width as i64;
        for y in r.y..r.bottom() {
            let row = ((y - self.rect.y) * w - self.rect.x) as isize;
            for x in r.x..r.right() {
                if self.data[(row + x as isize) as usize] > 0.0 {
                    x0 = x0.min(x);
                    x1 = x1.max(x + 1);
                    y0 = y0.min(y);
                    y1 = y1.max(y + 1);
                }
            }
        }
        (x1 > x0).then(|| PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// The largest coverage anywhere in the plane.
    pub fn peak(&self) -> f32 {
        self.data.iter().copied().fold(0.0f32, f32::max)
    }
}

/// Straight-alpha linear colour -> premultiplied.
fn premul(c: [f32; 4]) -> [f32; 4] {
    premultiply([
        c[0].max(0.0),
        c[1].max(0.0),
        c[2].max(0.0),
        c[3].clamp(0.0, 1.0),
    ])
}

/// Perceptual luminance of a premultiplied pixel, on the display curve.
fn encoded_luma(px: [f32; 4]) -> f32 {
    let s = unpremultiply(px);
    linear_to_srgb(linear_srgb_luminance([s[0], s[1], s[2]]).clamp(0.0, 1.0))
}

/// How close two premultiplied pixels are, `1.0` identical and `0.0` further
/// apart than `tolerance`.
fn similarity(a: [f32; 4], b: [f32; 4], tolerance: f32) -> f32 {
    let t = tolerance.max(1e-4);
    let sa = unpremultiply(a);
    let sb = unpremultiply(b);
    let d = ((sa[0] - sb[0]).powi(2) + (sa[1] - sb[1]).powi(2) + (sa[2] - sb[2]).powi(2)).sqrt()
        / 3f32.sqrt();
    (1.0 - d / t).clamp(0.0, 1.0)
}

/// Where the pixels a source-reading op copies from live.
pub struct StrokeSources<'a> {
    /// A read-only patch aligned to the document, for clone and heal.
    pub source: Option<&'a ColorPatch>,
    /// Document-space offset added to a destination point to find its source.
    pub offset: IVec2,
    pub pattern: Option<&'a Pattern>,
}

/// How far the radius may be doubled looking for a pixel outside the healed
/// region, and how much blurred weight counts as "found one".
const HEAL_ESCALATIONS: usize = 5;
const HEAL_MIN_WEIGHT: f32 = 1e-3;

/// The low frequencies of `src`, measured **outside** the region `covered`
/// marks.
///
/// A frequency-split heal asks "what colour and shading does the destination
/// have here, ignoring the blemish". Blurring the destination as it stands
/// answers a different question: it folds the blemish's own colour straight
/// back into the estimate, so anything wider than roughly `2·sigma` survives
/// the heal as a visible ghost — a dark 10×10 spot on a light field comes back
/// half-dark rather than gone.
///
/// This is a normalised convolution instead. Every sample is weighted by
/// `1 − coverage`, the weighted image and the weights are blurred with the same
/// kernel, and the quotient is a blur that never saw a pixel the heal is about
/// to replace.
///
/// When the region is so much wider than `sigma` that no uncovered pixel is
/// within reach, the radius doubles (up to [`HEAL_ESCALATIONS`] times) until
/// one is — that is how a wide blemish still gets its surroundings rather than
/// itself. Pixels no radius reaches — a plane covered edge to edge, where there
/// is no outside at all — keep the plain blur, which is the only answer left.
pub fn low_frequency_outside(
    src: &FilterBuffer,
    covered: &[f32],
    sigma: f32,
) -> Result<FilterBuffer, ToolError> {
    let n = src.len();
    if covered.len() != n {
        return Err(ToolError::Filter(filters::FilterError::BadLength {
            width: src.width(),
            height: src.height(),
            expected: n,
            got: covered.len(),
        }));
    }
    let (w, h) = (src.width(), src.height());
    let sigma = if sigma.is_finite() {
        sigma.max(0.5)
    } else {
        0.5
    };

    let mut weighted = Vec::with_capacity(n);
    let mut weights = Vec::with_capacity(n);
    for (px, cov) in src.pixels().iter().zip(covered) {
        let k = (1.0 - cov).clamp(0.0, 1.0);
        weighted.push([px[0] * k, px[1] * k, px[2] * k, px[3] * k]);
        weights.push([k; 4]);
    }
    let weighted = FilterBuffer::from_pixels(w, h, weighted)?;
    let weights = FilterBuffer::from_pixels(w, h, weights)?;

    // The plain blur is the floor: it is what every pixel keeps if no radius
    // ever reaches outside the region.
    let mut out = gaussian_blur(src, sigma, EdgeMode::Clamp);
    let mut done = vec![false; n];
    let mut remaining = n;
    let mut radius = sigma;
    for _ in 0..HEAL_ESCALATIONS {
        if remaining == 0 {
            break;
        }
        let num = gaussian_blur(&weighted, radius, EdgeMode::Clamp);
        let den = gaussian_blur(&weights, radius, EdgeMode::Clamp);
        let (np, dp) = (num.pixels(), den.pixels());
        let op = out.pixels_mut();
        for i in 0..n {
            if done[i] {
                continue;
            }
            let d = dp[i][3];
            if d > HEAL_MIN_WEIGHT {
                op[i] = [np[i][0] / d, np[i][1] / d, np[i][2] / d, np[i][3] / d];
                done[i] = true;
                remaining -= 1;
            }
        }
        radius *= 2.0;
    }
    Ok(out)
}

/// W4-B: how far a Gaussian of `sigma` reads either side of a pixel — the
/// truncated kernel's own radius, so a window grown by it holds every tap.
fn gaussian_reach(sigma: f32) -> i64 {
    (filters::blur::gaussian_kernel(sigma).len() / 2) as i64
}

/// The sigma [`low_frequency_outside`] actually uses for a requested one.
fn heal_sigma(sigma: f32) -> f32 {
    if sigma.is_finite() {
        sigma.max(0.5)
    } else {
        0.5
    }
}

/// W4-B: how far [`low_frequency_outside`] can read around a pixel — the
/// kernel of its widest escalation.
fn heal_reach(sigma: f32) -> i64 {
    let s = heal_sigma(sigma);
    let widest = s * 2f32.powi(HEAL_ESCALATIONS as i32 - 1);
    gaussian_reach(s).max(gaussian_reach(widest))
}

/// `r` grown by `n` pixels on every side.
fn grow(r: PixelRect, n: i64) -> PixelRect {
    PixelRect::new(
        r.x - n,
        r.y - n,
        (r.width as i64 + 2 * n) as u32,
        (r.height as i64 + 2 * n) as u32,
    )
}

/// The smallest rect holding both.
fn union_rect(a: PixelRect, b: PixelRect) -> PixelRect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = a.right().max(b.right());
    let y1 = a.bottom().max(b.bottom());
    PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
}

fn origin_of(r: PixelRect) -> IVec2 {
    IVec2::new(r.x as i32, r.y as i32)
}

/// Whether a dab can put any coverage inside `r`.
fn dab_touches(d: &Dab, r: PixelRect) -> bool {
    let (lo, hi) = d.bounds();
    (lo.x as i64) < r.right()
        && (hi.x as i64) > r.x
        && (lo.y as i64) < r.bottom()
        && (hi.y as i64) > r.y
}

/// The pixels of `plane` (whose top-left pixel is `origin`) inside `rect`,
/// which the plane must hold.
fn crop(plane: &FilterBuffer, origin: IVec2, rect: PixelRect) -> Result<FilterBuffer, ToolError> {
    let stride = plane.width() as i64;
    let src = plane.pixels();
    let mut px = Vec::with_capacity(rect.width as usize * rect.height as usize);
    for y in rect.y..rect.bottom() {
        let start = ((y - origin.y as i64) * stride + (rect.x - origin.x as i64)) as usize;
        px.extend_from_slice(&src[start..start + rect.width as usize]);
    }
    Ok(FilterBuffer::from_pixels(rect.width, rect.height, px)?)
}

/// W4-B: [`low_frequency_outside`] for the pixels of `window` only.
///
/// Each escalation reads just `window` grown by that radius's kernel reach,
/// clamped to `bounds` (the one place an edge is clamped: the document), so a
/// window pixel's answer does not depend on how much plane lies around it —
/// which is what lets a stroke be recomputed one tile at a time, and the
/// release and the live preview agree byte for byte. `plane` (top-left at
/// `origin`) must hold `bounds`, and `bounds` must hold `window`; `covered`
/// is the stroke's coverage, read where it lies.
fn low_frequency_outside_at(
    plane: &FilterBuffer,
    origin: IVec2,
    covered: &StrokeBuffer,
    sigma: f32,
    window: PixelRect,
    bounds: PixelRect,
) -> Result<FilterBuffer, ToolError> {
    let sigma = heal_sigma(sigma);
    let area = |r: f32| intersect(grow(window, gaussian_reach(r)), bounds).unwrap_or(window);
    let a0 = area(sigma);
    let plain = gaussian_blur(&crop(plane, origin, a0)?, sigma, EdgeMode::Clamp);
    let mut out = crop(&plain, origin_of(a0), window)?;
    let n = out.len();
    let mut done = vec![false; n];
    let mut remaining = n;
    let mut radius = sigma;
    for _ in 0..HEAL_ESCALATIONS {
        if remaining == 0 {
            break;
        }
        let a = area(radius);
        let src = crop(plane, origin, a)?;
        let mut weighted = Vec::with_capacity(src.len());
        let mut weights = Vec::with_capacity(src.len());
        let aw = a.width as i64;
        for (i, px) in src.pixels().iter().enumerate() {
            let (x, y) = (a.x + i as i64 % aw, a.y + i as i64 / aw);
            let k = (1.0 - covered.get(IVec2::new(x as i32, y as i32))).clamp(0.0, 1.0);
            weighted.push([px[0] * k, px[1] * k, px[2] * k, px[3] * k]);
            weights.push([k; 4]);
        }
        let weighted = FilterBuffer::from_pixels(a.width, a.height, weighted)?;
        let weights = FilterBuffer::from_pixels(a.width, a.height, weights)?;
        let num = crop(
            &gaussian_blur(&weighted, radius, EdgeMode::Clamp),
            origin_of(a),
            window,
        )?;
        let den = crop(
            &gaussian_blur(&weights, radius, EdgeMode::Clamp),
            origin_of(a),
            window,
        )?;
        let (np, dp) = (num.pixels(), den.pixels());
        let op = out.pixels_mut();
        for i in 0..n {
            if done[i] {
                continue;
            }
            let d = dp[i][3];
            if d > HEAL_MIN_WEIGHT {
                op[i] = [np[i][0] / d, np[i][1] / d, np[i][2] / d, np[i][3] / d];
                done[i] = true;
                remaining -= 1;
            }
        }
        radius *= 2.0;
    }
    Ok(out)
}

/// The healing brush's frequency split: the source's detail over the
/// destination's low frequencies. One formula for the whole-patch path
/// ([`prepare`]) and the per-tile one ([`local_aux`]).
fn heal_mix(
    src_full: &FilterBuffer,
    src_low: &FilterBuffer,
    dst_low: &FilterBuffer,
) -> Result<FilterBuffer, ToolError> {
    let mut px = Vec::with_capacity(src_full.len());
    for ((sf, sl), dl) in src_full
        .pixels()
        .iter()
        .zip(src_low.pixels())
        .zip(dst_low.pixels())
    {
        px.push([
            (sf[0] - sl[0] + dl[0]).max(0.0),
            (sf[1] - sl[1] + dl[1]).max(0.0),
            (sf[2] - sl[2] + dl[2]).max(0.0),
            dl[3].clamp(0.0, 1.0).max(sf[3]),
        ]);
    }
    Ok(FilterBuffer::from_pixels(
        src_full.width(),
        src_full.height(),
        px,
    )?)
}

/// W4-B: the plane a neighbourhood op (blur, sharpen, the healing brushes)
/// mixes toward, for the pixels of `window` only. Every read stays inside
/// `window` grown by the op's reach and clamped to `bounds`, so the answer
/// is the same whichever larger `plane` the window was cut from.
fn local_aux(
    op: &StrokeOp,
    plane: &FilterBuffer,
    origin: IVec2,
    covered: &StrokeBuffer,
    sources: &StrokeSources<'_>,
    window: PixelRect,
    bounds: PixelRect,
) -> Result<FilterBuffer, ToolError> {
    let area = |reach: i64| intersect(grow(window, reach), bounds).unwrap_or(window);
    match op {
        StrokeOp::Blur { radius } => {
            let s = radius.max(0.1);
            let a = area(gaussian_reach(s));
            let blurred = gaussian_blur(&crop(plane, origin, a)?, s, EdgeMode::Clamp);
            crop(&blurred, origin_of(a), window)
        }
        StrokeOp::Sharpen { amount, radius } => {
            let s = radius.max(0.1);
            let a = area(gaussian_reach(s));
            let sharp = unsharp_mask(
                &crop(plane, origin, a)?,
                amount.max(0.0),
                s,
                0.0,
                EdgeMode::Clamp,
            );
            crop(&sharp, origin_of(a), window)
        }
        StrokeOp::SpotHealing => {
            low_frequency_outside_at(plane, origin, covered, 6.0, window, bounds)
        }
        StrokeOp::Healing { softness } => {
            let src = sources.source.ok_or(ToolError::Degenerate)?;
            let off = sources.offset;
            let sigma = softness.max(0.5);
            let a = area(gaussian_reach(sigma));
            let mut px = Vec::with_capacity(a.width as usize * a.height as usize);
            for y in a.y..a.bottom() {
                for x in a.x..a.right() {
                    px.push(src.get(IVec2::new(x as i32, y as i32) + off));
                }
            }
            let src_full = FilterBuffer::from_pixels(a.width, a.height, px)?;
            let src_low = gaussian_blur(&src_full, sigma, EdgeMode::Clamp);
            let src_full = crop(&src_full, origin_of(a), window)?;
            let src_low = crop(&src_low, origin_of(a), window)?;
            let dst_low = low_frequency_outside_at(plane, origin, covered, sigma, window, bounds)?;
            heal_mix(&src_full, &src_low, &dst_low)
        }
        // Only the ops `StrokeTool::neighbourhood_reach` names come here.
        _ => Err(ToolError::Degenerate),
    }
}

/// W4-B: a smudge stroke in progress, kept between samples.
///
/// Smudge is a sequence — each dab mixes in what the previous one left — so
/// it cannot be recomputed per tile from scratch. Instead the working plane
/// (linear light, one [`ColorPatch`] per tile, loaded the first time a dab
/// reaches it) and the carried colour persist across samples, and each
/// sample applies only its new dabs. The release finishes the same run, so
/// the committed tiles are the last preview's exactly, and a sample costs
/// its own dabs, not the stroke so far.
///
/// The run is only valid against the committed pixels it was loaded from:
/// each plane remembers the committed hash its tile had when it was loaded
/// ([`SmudgeRun::sources`]), and [`SmudgeRun::is_current`] checks them. When
/// the document moved under a held button (an undo, a redo, a history jump),
/// the run is thrown away and the walk restarts from the pixels as they are
/// now, so neither the preview nor the release puts undone pixels back.
#[derive(Debug, Default)]
struct SmudgeRun {
    planes: BTreeMap<TileCoord, ColorPatch>,
    sources: BTreeMap<TileCoord, Option<TileHash>>,
    carried: Option<[f32; 4]>,
    applied: usize,
}

impl SmudgeRun {
    /// Whether every tile the walk loaded still holds, in `access`, the
    /// committed bytes it was loaded from — false once the document moved
    /// under the stroke.
    fn is_current(&self, access: &dyn TileAccess, key: PixelKey) -> bool {
        self.sources
            .iter()
            .all(|(coord, hash)| access.tile_hash(key, *coord) == *hash)
    }

    fn plane(
        &mut self,
        access: &dyn TileAccess,
        key: PixelKey,
        coord: TileCoord,
    ) -> Result<&mut ColorPatch, ToolError> {
        Ok(match self.planes.entry(coord) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                let (ox, oy) = coord.pixel_origin();
                let rect = PixelRect::new(ox, oy, TILE_SIZE, TILE_SIZE);
                self.sources.insert(coord, access.tile_hash(key, coord));
                e.insert(ColorPatch::load(access, key, rect)?)
            }
        })
    }

    fn get(
        &mut self,
        access: &dyn TileAccess,
        key: PixelKey,
        p: IVec2,
    ) -> Result<[f32; 4], ToolError> {
        let ts = TILE_SIZE as i32;
        let coord = TileCoord::new(p.x.div_euclid(ts), p.y.div_euclid(ts), 0);
        Ok(self.plane(access, key, coord)?.get(p))
    }

    /// Apply the dabs after the last applied one — [`apply_smudge`]'s walk,
    /// resumable — and report the tiles they reached.
    #[allow(clippy::too_many_arguments)]
    fn advance(
        &mut self,
        access: &dyn TileAccess,
        key: PixelKey,
        dabs: &[Dab],
        clip: PixelRect,
        strength: f32,
        opacity: f32,
        selection: &Selection,
    ) -> Result<BTreeSet<TileCoord>, ToolError> {
        let strength = strength.clamp(0.0, 1.0);
        let opacity = opacity.clamp(0.0, 1.0);
        let mut touched = BTreeSet::new();
        if clip.is_empty() || self.applied >= dabs.len() {
            self.applied = self.applied.max(dabs.len());
            return Ok(touched);
        }
        let mut carried = match self.carried {
            Some(c) => c,
            None => {
                let first = dabs[0].center;
                self.get(
                    access,
                    key,
                    IVec2::new(first.x.round() as i32, first.y.round() as i32),
                )?
            }
        };
        for d in &dabs[self.applied..] {
            let (lo, hi) = d.bounds();
            let x0 = (lo.x as i64).max(clip.x);
            let y0 = (lo.y as i64).max(clip.y);
            let x1 = (hi.x as i64).min(clip.right());
            let y1 = (hi.y as i64).min(clip.bottom());
            if x1 > x0 && y1 > y0 {
                let reach = PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
                for (_, coord) in TileBox::covering(reach)?.coords() {
                    let (ox, oy) = coord.pixel_origin();
                    let (tx0, ty0) = (x0.max(ox), y0.max(oy));
                    let tx1 = x1.min(ox + TILE_SIZE as i64);
                    let ty1 = y1.min(oy + TILE_SIZE as i64);
                    let plane = self.plane(access, key, coord)?;
                    for y in ty0..ty1 {
                        for x in tx0..tx1 {
                            let (x, y) = (x as i32, y as i32);
                            let p = IVec2::new(x, y);
                            let c = d.coverage_pixel(x, y) * d.flow;
                            if c <= 0.0 {
                                continue;
                            }
                            let a = c * opacity * selection.coverage_at(p);
                            if a <= 0.0 {
                                continue;
                            }
                            let dst = plane.get(p);
                            plane.set(
                                p,
                                [
                                    dst[0] + (carried[0] - dst[0]) * a * strength,
                                    dst[1] + (carried[1] - dst[1]) * a * strength,
                                    dst[2] + (carried[2] - dst[2]) * a * strength,
                                    dst[3] + (carried[3] - dst[3]) * a * strength,
                                ],
                            );
                        }
                    }
                    touched.insert(coord);
                }
            }
            let centre = self.get(
                access,
                key,
                IVec2::new(d.center.x.round() as i32, d.center.y.round() as i32),
            )?;
            let pickup = strength;
            carried = [
                carried[0] + (centre[0] - carried[0]) * (1.0 - pickup),
                carried[1] + (centre[1] - carried[1]) * (1.0 - pickup),
                carried[2] + (centre[2] - carried[2]) * (1.0 - pickup),
                carried[3] + (centre[3] - carried[3]) * (1.0 - pickup),
            ];
        }
        self.carried = Some(carried);
        self.applied = dabs.len();
        Ok(touched)
    }

    /// Encode the given tiles of the run into `access` and return the delta
    /// that installs them over the committed target.
    fn encode(
        &self,
        access: &mut dyn TileAccess,
        key: PixelKey,
        coords: impl IntoIterator<Item = TileCoord>,
    ) -> Result<TileDelta, ToolError> {
        let mut edits = Vec::new();
        for coord in coords {
            if let Some(plane) = self.planes.get(&coord) {
                edits.extend(plane.commit(access, key)?.iter().copied());
            }
        }
        Ok(TileDelta::new(edits)?)
    }
}

/// The stroke's coverage, resampled onto a patch's plane.
fn coverage_over(patch: &ColorPatch, buf: &StrokeBuffer) -> Vec<f32> {
    let (w, h) = (patch.width() as i32, patch.height() as i32);
    let o = patch.origin();
    let mut v = Vec::with_capacity((w as usize) * (h as usize));
    for y in 0..h {
        for x in 0..w {
            v.push(buf.get(IVec2::new(o.x + x, o.y + y)));
        }
    }
    v
}

/// The per-pixel target value and how it blends.
struct Prepared {
    aux: Option<FilterBuffer>,
    gate: Option<Vec<f32>>,
    blend: Blend,
}

/// Build the per-pixel target plane for an op, aligned to `patch`.
fn prepare(
    op: &StrokeOp,
    patch: &ColorPatch,
    covered: &[f32],
    sources: &StrokeSources<'_>,
    base_color: [f32; 4],
) -> Result<Prepared, ToolError> {
    let (w, h) = (patch.width(), patch.height());
    let origin = patch.origin();
    let constant = |c: [f32; 4]| -> Result<FilterBuffer, ToolError> {
        Ok(FilterBuffer::filled(w, h, premul(c))?)
    };
    let mapped = |f: &dyn Fn([f32; 4], IVec2) -> [f32; 4]| -> Result<FilterBuffer, ToolError> {
        let mut px = Vec::with_capacity((w as usize) * (h as usize));
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let p = IVec2::new(origin.x + x, origin.y + y);
                px.push(f(patch.get(p), p));
            }
        }
        Ok(FilterBuffer::from_pixels(w, h, px)?)
    };

    Ok(match op {
        // Card 061: RefineBoundary is a MASK-only op — the mask commit path
        // regrades the coverage itself (apply_refine_to_mask); reaching here
        // means the Content target, which refuses loudly instead of doing
        // nothing.
        StrokeOp::RefineBoundary { .. } => {
            return Err(ToolError::UnsupportedOnMask);
        }
        StrokeOp::Paint { color } => Prepared {
            aux: Some(constant(*color)?),
            gate: None,
            blend: Blend::Over,
        },
        StrokeOp::Erase => Prepared {
            aux: None,
            gate: None,
            blend: Blend::Erase,
        },
        StrokeOp::ColorReplacement { color, tolerance } => {
            let repl = premul(*color);
            let gate = collect_gate(patch, |dst| similarity(dst, base_color, *tolerance));
            // Keep the destination's own luminance so shading and texture
            // survive the recolour — replacing the flat colour is what makes
            // this different from painting.
            let aux = mapped(&|dst, _| {
                let l_dst = linear_srgb_luminance(unpremultiply(dst)[..3].try_into().unwrap());
                let s = unpremultiply(repl);
                let l_src = linear_srgb_luminance([s[0], s[1], s[2]]).max(1e-4);
                let k = (l_dst / l_src).clamp(0.0, 8.0);
                premultiply([s[0] * k, s[1] * k, s[2] * k, unpremultiply(dst)[3]])
            })?;
            Prepared {
                aux: Some(aux),
                gate: Some(gate),
                blend: Blend::Lerp,
            }
        }
        StrokeOp::BackgroundErase { tolerance } => Prepared {
            aux: None,
            gate: Some(collect_gate(patch, |dst| {
                similarity(dst, base_color, *tolerance)
            })),
            blend: Blend::Erase,
        },
        StrokeOp::CloneStamp => {
            let src = sources.source.ok_or(ToolError::Degenerate)?;
            let off = sources.offset;
            let aux = mapped(&|_, p| src.get(p + off))?;
            Prepared {
                aux: Some(aux),
                gate: None,
                blend: Blend::Over,
            }
        }
        StrokeOp::PatternStamp => {
            let pat = sources.pattern.ok_or(ToolError::Degenerate)?;
            let aux = mapped(&|_, p| {
                let s = pat.sample(p.x as i64, p.y as i64);
                premultiply([
                    color::srgb8_to_linear(s[0]),
                    color::srgb8_to_linear(s[1]),
                    color::srgb8_to_linear(s[2]),
                    s[3] as f32 / 255.0,
                ])
            })?;
            Prepared {
                aux: Some(aux),
                gate: None,
                blend: Blend::Over,
            }
        }
        StrokeOp::Healing { softness } => {
            let src = sources.source.ok_or(ToolError::Degenerate)?;
            let off = sources.offset;
            // Frequency split: take the *detail* from the source and the
            // *colour and shading* from the destination. That is what makes a
            // heal blend into its surroundings where a clone leaves a patch.
            // (A true Poisson solve would match gradients exactly; this
            // approximates it with a low-pass split, and says so.)
            //
            // The destination's low-frequency term comes from *outside* the
            // dab — see [`low_frequency_outside`]. Taking it from under the dab
            // would blur the blemish back into its own repair.
            let sigma = softness.max(0.5);
            let src_full = mapped(&|_, p| src.get(p + off))?;
            let src_low = gaussian_blur(&src_full, sigma, EdgeMode::Clamp);
            let dst_low = low_frequency_outside(patch.buffer(), covered, sigma)?;
            Prepared {
                aux: Some(heal_mix(&src_full, &src_low, &dst_low)?),
                gate: None,
                blend: Blend::Lerp,
            }
        }
        StrokeOp::SpotHealing => Prepared {
            // Nothing to sample from, so the surroundings are diffused inward.
            // "Inward" is the whole trick: the average is taken over the
            // pixels the dab does *not* cover, so the blemish contributes
            // nothing to what replaces it. A plain blur here would leave a
            // ghost of the spot exactly where the spot was.
            aux: Some(low_frequency_outside(patch.buffer(), covered, 6.0)?),
            gate: None,
            blend: Blend::Lerp,
        },
        StrokeOp::Blur { radius } => Prepared {
            aux: Some(gaussian_blur(
                patch.buffer(),
                radius.max(0.1),
                EdgeMode::Clamp,
            )),
            gate: None,
            blend: Blend::Lerp,
        },
        StrokeOp::Sharpen { amount, radius } => Prepared {
            aux: Some(unsharp_mask(
                patch.buffer(),
                amount.max(0.0),
                radius.max(0.1),
                0.0,
                EdgeMode::Clamp,
            )),
            gate: None,
            blend: Blend::Lerp,
        },
        StrokeOp::Smudge { .. } => {
            // Handled by `apply_smudge`; never reaches the plane compositor.
            Prepared {
                aux: None,
                gate: None,
                blend: Blend::Lerp,
            }
        }
        StrokeOp::Dodge { exposure, range } => {
            let e = exposure.clamp(0.0, 1.0);
            let r = *range;
            Prepared {
                aux: Some(mapped(&|dst, _| {
                    let w = r.weight(encoded_luma(dst)) * e;
                    let s = unpremultiply(dst);
                    premultiply([
                        s[0] + (1.0 - s[0]) * w,
                        s[1] + (1.0 - s[1]) * w,
                        s[2] + (1.0 - s[2]) * w,
                        s[3],
                    ])
                })?),
                gate: None,
                blend: Blend::Lerp,
            }
        }
        StrokeOp::Burn { exposure, range } => {
            let e = exposure.clamp(0.0, 1.0);
            let r = *range;
            Prepared {
                aux: Some(mapped(&|dst, _| {
                    let w = r.weight(encoded_luma(dst)) * e;
                    let s = unpremultiply(dst);
                    premultiply([s[0] * (1.0 - w), s[1] * (1.0 - w), s[2] * (1.0 - w), s[3]])
                })?),
                gate: None,
                blend: Blend::Lerp,
            }
        }
        StrokeOp::Sponge { amount, mode } => {
            let a = amount.clamp(0.0, 1.0);
            let m = *mode;
            Prepared {
                aux: Some(mapped(&|dst, _| {
                    let s = unpremultiply(dst);
                    let g = linear_srgb_luminance([s[0], s[1], s[2]]);
                    let k = match m {
                        SpongeMode::Desaturate => 1.0 - a,
                        SpongeMode::Saturate => 1.0 + a,
                    };
                    premultiply([
                        (g + (s[0] - g) * k).max(0.0),
                        (g + (s[1] - g) * k).max(0.0),
                        (g + (s[2] - g) * k).max(0.0),
                        s[3],
                    ])
                })?),
                gate: None,
                blend: Blend::Lerp,
            }
        }
    })
}

fn collect_gate(patch: &ColorPatch, f: impl Fn([f32; 4]) -> f32) -> Vec<f32> {
    patch.buffer().pixels().iter().map(|p| f(*p)).collect()
}

/// The source pixel a source-over dab lays down once the paint blend mode
/// has had its say: the W3C model's `Cs' = (1 − αb)·Cs + αb·B(Cb, Cs)`,
/// evaluated in straight linear colour and handed back premultiplied so the
/// `Over` arm below composites it exactly as it would an unblended one.
///
/// Where the destination is transparent the source shows unblended (there
/// is nothing to blend with), which is what makes Multiply over an empty
/// layer paint the colour rather than nothing. `Normal` is the identity and
/// is short-circuited by the caller, so an untouched Mode combo leaves the
/// paint path byte-identical to what it was.
fn blended_source(mode: BlendMode, src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    if src[3] <= 0.0 {
        return src;
    }
    let s = unpremultiply(src);
    let d = unpremultiply(dst);
    let ab = dst[3].clamp(0.0, 1.0);
    let b = mode.blend_rgb([d[0], d[1], d[2]], [s[0], s[1], s[2]]);
    premultiply([
        (1.0 - ab) * s[0] + ab * b[0],
        (1.0 - ab) * s[1] + ab * b[1],
        (1.0 - ab) * s[2] + ab * b[2],
        s[3],
    ])
}

/// How a whole stroke lands on the layer: its opacity ceiling and the paint
/// blend mode the options bar's Mode combo holds.
///
/// The mode acts on the source-over ops — painting, the clone stamp and the
/// pattern stamp — which are the ones that lay a colour *on top*; the
/// retouching ops mix toward a computed target and have no source colour to
/// blend, so they read only the opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeBlend {
    /// Ceiling on the whole stroke's coverage, `0..=1`.
    pub opacity: f32,
    /// The paint blend mode; `Normal` is source-over exactly as before.
    pub mode: BlendMode,
}

impl StrokeBlend {
    /// Source-over at `opacity`, the stroke every tool painted before the
    /// Mode combo reached one.
    pub fn normal(opacity: f32) -> Self {
        Self {
            opacity,
            mode: BlendMode::Normal,
        }
    }
}

/// Composite a stroke's coverage plane onto a layer patch, once.
pub fn apply_stroke(
    patch: &mut ColorPatch,
    buf: &StrokeBuffer,
    op: &StrokeOp,
    sources: &StrokeSources<'_>,
    base_color: [f32; 4],
    blend: StrokeBlend,
    selection: &Selection,
) -> Result<(), ToolError> {
    let covered = coverage_over(patch, buf);
    let prep = prepare(op, patch, &covered, sources, base_color)?;
    let rect = buf.rect();
    let opacity = blend.opacity.clamp(0.0, 1.0);
    let blend_mode = blend.mode;
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let p = IVec2::new(x as i32, y as i32);
            let cov = buf.get(p);
            if cov <= 0.0 {
                continue;
            }
            let Some(i) = patch.index_of(p) else {
                continue;
            };
            let gate = prep.gate.as_ref().map(|g| g[i]).unwrap_or(1.0);
            let a = cov * opacity * selection.coverage_at(p) * gate;
            if a <= 0.0 {
                continue;
            }
            let dst = patch.get(p);
            let out = match prep.blend {
                Blend::Erase => [
                    dst[0] * (1.0 - a),
                    dst[1] * (1.0 - a),
                    dst[2] * (1.0 - a),
                    dst[3] * (1.0 - a),
                ],
                Blend::Over => {
                    let s = prep.aux.as_ref().map(|b| b.pixels()[i]).unwrap_or([0.0; 4]);
                    let s = if blend_mode == BlendMode::Normal {
                        s
                    } else {
                        blended_source(blend_mode, s, dst)
                    };
                    let sa = s[3] * a;
                    [
                        s[0] * a + dst[0] * (1.0 - sa),
                        s[1] * a + dst[1] * (1.0 - sa),
                        s[2] * a + dst[2] * (1.0 - sa),
                        sa + dst[3] * (1.0 - sa),
                    ]
                }
                Blend::Lerp => {
                    let s = prep.aux.as_ref().map(|b| b.pixels()[i]).unwrap_or(dst);
                    [
                        dst[0] + (s[0] - dst[0]) * a,
                        dst[1] + (s[1] - dst[1]) * a,
                        dst[2] + (s[2] - dst[2]) * a,
                        dst[3] + (s[3] - dst[3]) * a,
                    ]
                }
            };
            patch.set(p, out);
        }
    }
    Ok(())
}

/// Smudge: drag colour along the stroke.
///
/// The one op that cannot use the coverage plane. Smudging is a *sequence* —
/// each dab picks up what the previous one left behind — so it walks the dabs
/// in order, carrying a colour and mixing it into every pixel it passes over.
/// It still commits once, so it is still one command.
///
/// `clip` is the region the stroke is allowed to write, and it is not optional.
/// A [`ColorPatch`] is tile-aligned, so `patch.index_of` accepts points up to
/// `TILE_SIZE - 1` px *past* the document on every side; guarding on that alone
/// let a dab that overhangs the canvas edge paint outside the document, where
/// the pixels are invisible but still hash into the committed tile, enlarge the
/// emitted delta, and reappear if the canvas is later grown or the layer
/// translated. Every other op goes through [`apply_stroke`], which iterates the
/// [`StrokeBuffer`]'s rect and is therefore already canvas-clipped by
/// [`StrokeBuffer::bounds_of`]; this is the same clip, applied by hand.
pub fn apply_smudge(
    patch: &mut ColorPatch,
    dabs: &[Dab],
    clip: PixelRect,
    strength: f32,
    opacity: f32,
    selection: &Selection,
) {
    let strength = strength.clamp(0.0, 1.0);
    let opacity = opacity.clamp(0.0, 1.0);
    let Some(first) = dabs.first() else {
        return;
    };
    if clip.is_empty() {
        return;
    }
    let mut carried = patch.get(IVec2::new(
        first.center.x.round() as i32,
        first.center.y.round() as i32,
    ));
    for d in dabs {
        let (lo, hi) = d.bounds();
        let x0 = (lo.x as i64).max(clip.x);
        let y0 = (lo.y as i64).max(clip.y);
        let x1 = (hi.x as i64).min(clip.right());
        let y1 = (hi.y as i64).min(clip.bottom());
        for y in y0..y1 {
            for x in x0..x1 {
                let (x, y) = (x as i32, y as i32);
                let p = IVec2::new(x, y);
                if patch.index_of(p).is_none() {
                    continue;
                }
                let c = d.coverage_pixel(x, y) * d.flow;
                if c <= 0.0 {
                    continue;
                }
                let a = c * opacity * selection.coverage_at(p);
                if a <= 0.0 {
                    continue;
                }
                let dst = patch.get(p);
                let mixed = [
                    dst[0] + (carried[0] - dst[0]) * a * strength,
                    dst[1] + (carried[1] - dst[1]) * a * strength,
                    dst[2] + (carried[2] - dst[2]) * a * strength,
                    dst[3] + (carried[3] - dst[3]) * a * strength,
                ];
                patch.set(p, mixed);
            }
        }
        // Pick up what is under the dab centre for the next one.
        let centre = patch.get(IVec2::new(
            d.center.x.round() as i32,
            d.center.y.round() as i32,
        ));
        let pickup = strength;
        carried = [
            carried[0] + (centre[0] - carried[0]) * (1.0 - pickup),
            carried[1] + (centre[1] - carried[1]) * (1.0 - pickup),
            carried[2] + (centre[2] - carried[2]) * (1.0 - pickup),
            carried[3] + (centre[3] - carried[3]) * (1.0 - pickup),
        ];
    }
}

/// Composite a stroke onto a mask's coverage.
/// Card 061's unit-level probe: the boundary-refine regrade on a synthetic
/// stair-step ramp, driven through the real coverage machinery. See the
/// shell-level tests for the route; this pins the MATH.
#[cfg(test)]
pub(crate) mod refine_tests {
    use super::*;
    use crate::patch::CoveragePatch;
    use crate::tiles::MemoryTiles;
    use editor_core::pixels::PixelKey;
    use glam::IVec2;
    use layer_model::MaskId;
    use raster::{PixelRect, TILE_SIZE};

    /// A one-tile store: the stair-step coverage tile under one mask id
    /// (single-channel mask bytes via `MemoryTiles::put`).
    fn stair_store() -> (MemoryTiles, PixelKey) {
        let ts = TILE_SIZE as usize;
        let mut tile = vec![0u8; ts * ts];
        let steps = [0u8, 0, 64, 64, 128, 128, 192, 192];
        for y in 0..16usize {
            for (x, v) in steps.iter().enumerate() {
                tile[y * ts + x] = *v;
            }
            // The ramp tops out at a solid 255 plateau to the row's end, so
            // the band's top step is not sitting next to a hard zero.
            for x in 8..16usize {
                tile[y * ts + x] = 255;
            }
        }
        let key = PixelKey::Mask(MaskId::new());
        let mut tiles = MemoryTiles::new();
        tiles.put(key, raster::TileCoord::new(0, 0, 0), tile);
        (tiles, key)
    }

    #[test]
    fn the_refine_band_smooths_stairs_and_keeps_plateaus_hard() {
        // A stair-step ramp: 0,0,64,64,128,128,192,192,255,255 across a row.
        let ts = TILE_SIZE as usize;
        let mut tile = vec![0u8; ts * ts];
        let steps = [0u8, 0, 64, 64, 128, 128, 192, 192, 255, 255];
        for y in 0..16usize {
            for (x, v) in steps.iter().enumerate() {
                tile[y * ts + x] = *v;
            }
        }
        let (tiles, key) = stair_store();
        let rect = PixelRect::new(0, 0, 16, 16);
        let mut patch = CoveragePatch::load(&tiles, key, rect).unwrap();

        // The band covers the stair region only (dabs through x 1..9).
        let dabs: Vec<Dab> = (1..9)
            .map(|x| Dab {
                center: Vec2::new(x as f32 + 0.5, 8.5),
                radius: 1.0,
                hardness: 1.0,
                flow: 1.0,
                angle: 0.0,
                roundness: 1.0,
                aliased: false,
            })
            .collect();
        let buf = StrokeBuffer::rasterize(&dabs, rect).unwrap();
        apply_refine_to_mask(&mut patch, &buf, 0.6, 1.0, &Selection::None);

        // The stairs became a MONOTONE ramp (no more flat double-steps) —
        // the smooth + re-grade turned the flat 64/64 and 192/192 pairs into
        // a rising sequence.
        let mut prev = patch.get(IVec2::new(1, 8));
        let mut rose = false;
        for x in 2..9 {
            let v = patch.get(IVec2::new(x, 8));
            assert!(
                v >= prev - 0.02,
                "the band's ramp is monotone at x={x}: {v} after {prev}"
            );
            rose |= v > prev + 0.02;
            prev = v;
        }
        assert!(rose, "the band actually regraded the stairs");
        // Outside the band, the coverage is byte-identical (bounded work):
        // the plateau pixels the dabs never touched hold their 255.
        assert_eq!(patch.get(IVec2::new(10, 8)), 1.0, "unpainted stays put");
        assert_eq!(patch.get(IVec2::new(15, 8)), 1.0);

        // The round-1 critical: a stroke whose bounding rect does NOT start
        // at the document origin (the patch's tile box is displaced) must
        // regrade the band, not erase it. Same stair fixture, shifted.
        let shifted_key = PixelKey::Mask(MaskId::new());
        let mut shifted_tiles = MemoryTiles::new();
        let mut shifted = vec![0u8; TILE_SIZE as usize * TILE_SIZE as usize];
        for y in 0..16usize {
            for (x, v) in steps.iter().enumerate() {
                shifted[(y + 4) * TILE_SIZE as usize + x] = *v;
            }
            for x in 8..16usize {
                shifted[(y + 4) * TILE_SIZE as usize + x] = 255;
            }
        }
        shifted_tiles.put(shifted_key, raster::TileCoord::new(1, 0, 0), shifted);
        // The stroke rect sits at x 256..272 (tile 1) y 4..20 — displaced.
        let rect2 = PixelRect::new(256, 4, 16, 16);
        let mut patch2 = CoveragePatch::load(&shifted_tiles, shifted_key, rect2).unwrap();
        let dabs2: Vec<Dab> = (1..9)
            .map(|x| Dab {
                center: Vec2::new((256 + x) as f32 + 0.5, (4 + 8) as f32 + 0.5),
                radius: 1.0,
                hardness: 1.0,
                flow: 1.0,
                angle: 0.0,
                roundness: 1.0,
                aliased: false,
            })
            .collect();
        let buf2 = StrokeBuffer::rasterize(&dabs2, rect2).unwrap();
        apply_refine_to_mask(&mut patch2, &buf2, 0.6, 1.0, &Selection::None);
        // The band's ramp still rises (not erased to zero).
        let mut prev2 = patch2.get(IVec2::new(257, 12));
        let mut rose2 = false;
        for x in 258..264 {
            let v = patch2.get(IVec2::new(x, 12));
            assert!(v >= prev2 - 0.02, "the displaced band is monotone at {x}");
            rose2 |= v > prev2 + 0.02;
            prev2 = v;
        }
        assert!(rose2, "the displaced stroke regraded, not erased");

        // The "hard interior" clause: a CONSTANT region is a fixed point of
        // blur + re-grade — a wide dab over a solid plateau moves nothing.
        let mut constant = vec![128u8; TILE_SIZE as usize * TILE_SIZE as usize];
        for y in 0..16usize {
            for x in 0..16usize {
                constant[y * TILE_SIZE as usize + x] = 128;
            }
        }
        let key2 = PixelKey::Mask(MaskId::new());
        let mut tiles2 = MemoryTiles::new();
        tiles2.put(key2, raster::TileCoord::new(0, 0, 0), constant);
        let mut patch2 = CoveragePatch::load(&tiles2, key2, rect).unwrap();
        let dabs2: Vec<Dab> = (2..14)
            .map(|x| Dab {
                center: Vec2::new(x as f32 + 0.5, 8.5),
                radius: 2.0,
                hardness: 1.0,
                flow: 1.0,
                angle: 0.0,
                roundness: 1.0,
                aliased: false,
            })
            .collect();
        let buf2 = StrokeBuffer::rasterize(&dabs2, rect).unwrap();
        apply_refine_to_mask(&mut patch2, &buf2, 0.8, 1.0, &Selection::None);
        for x in [2, 6, 10, 13] {
            assert_eq!(
                patch2.get(IVec2::new(x, 8)),
                128.0 / 255.0,
                "the hard interior (a constant plateau) retains its coverage at x={x}"
            );
        }
    }

    /// Card 061 (review round 1): the advertised Strength option reaches the
    /// op — `set_setting` mutates the op's strength (the round-1 defect was
    /// a silent no-op slider; the shell surfaces refusals instead).
    #[test]
    fn the_strength_option_reaches_the_refine_op() {
        use crate::tool::ToolSetting;
        let mut tool = StrokeTool::new(
            ToolId::RefineBoundary,
            BrushSettings::default(),
            StrokeOp::RefineBoundary { strength: 0.5 },
        );
        tool.set_setting("strength", ToolSetting::Float(0.9))
            .unwrap();
        match &tool.op {
            StrokeOp::RefineBoundary { strength } => {
                assert_eq!(*strength, 0.9, "the option landed in the op");
            }
            other => panic!("unexpected op: {other:?}"),
        }
        // An unknown key is an ERROR the shell surfaces, never a no-op.
        assert!(tool.set_setting("bogus", ToolSetting::Float(1.0)).is_err());
        // The whole float-option family flows the same way (Blur's radius was
        // dead the same way before this fix).
        let mut blur = StrokeTool::new(
            ToolId::Blur,
            BrushSettings::default(),
            StrokeOp::Blur { radius: 3.0 },
        );
        blur.set_setting("radius", ToolSetting::Float(12.0))
            .unwrap();
        assert!(matches!(blur.op, StrokeOp::Blur { radius: 12.0 }));
    }
}

pub fn apply_stroke_to_mask(
    patch: &mut CoveragePatch,
    buf: &StrokeBuffer,
    value: f32,
    opacity: f32,
    selection: &Selection,
) {
    let rect = buf.rect();
    let opacity = opacity.clamp(0.0, 1.0);
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let p = IVec2::new(x as i32, y as i32);
            let a = buf.get(p) * opacity * selection.coverage_at(p);
            if a <= 0.0 {
                continue;
            }
            patch.blend(p, value, a);
        }
    }
}

/// The blur a boundary-refine stroke measures its band with.
fn refine_sigma(strength: f32) -> f32 {
    0.5 + 2.5 * strength.clamp(0.0, 1.0)
}

/// The boundary refine's regrade of one blurred coverage sample: pushed away
/// from the byte-space midpoint 128/255, so a constant field is a fixed
/// point. Shared by [`apply_refine_to_mask`] and the per-tile stroke path.
fn refine_grade(blurred: f32, gain: f32) -> f32 {
    const MID: f32 = 128.0 / 255.0;
    (MID + (blurred - MID) * gain).clamp(0.0, 1.0)
}

/// Card 061: the boundary-refine brush's coverage regrade.
///
/// The pipeline over the stroke's bounding rect, in order:
///
/// 1. **Smooth** — a gaussian blur of the coverage with
///    `sigma = 0.5 + 2.5 * strength`, which averages the band's stair-steps
///    and sensor noise into a monotone ramp. A plateau is a fixed point, so
///    definite coverage inside the band keeps its value.
/// 2. **Re-grade** — the blurred sample is pushed away from the
///    byte-space midpoint 128 by `gain = 1 + strength` (the same midpoint
///    card 060's contrast curve uses; a MILDER gain than its `1 + 3·strength`,
///    because a brush stroke re-runs per dab pass and must stay repeatable),
///    so the smoothed edge stays defined instead of going mushy.
///    Saturation points (0 and 255) and the midpoint are exact fixed points;
///    INTERMEDIATE constant values harden (200 → ~243 at strength 0.6) —
///    that is what a contrast regrade does, the user painted there, and
///    unpainted definite coverage never moves.
///
/// Only pixels the dabs actually cover move toward the regraded value (the
/// same `dab alpha × opacity × selection` band gate the paint path
/// uses), so the work and the effect are bounded to the painted region —
/// unpainted coverage is byte-identical. This is manual morphology; it is
/// deliberately NOT presented as automatic matting or hair extraction.
pub fn apply_refine_to_mask(
    patch: &mut CoveragePatch,
    buf: &StrokeBuffer,
    strength: f32,
    opacity: f32,
    selection: &Selection,
) {
    let strength = strength.clamp(0.0, 1.0);
    let rect = buf.rect();
    let opacity = opacity.clamp(0.0, 1.0);
    // The working plane is the PATCH's own tile box, in PATCH-LOCAL
    // coordinates (`CoveragePatch::get` takes ABSOLUTE document coordinates;
    // `to_buffer` produces the matching patch-aligned plane — the round-1
    // defect mixed the two and erased bands away from the document origin).
    let (w, h) = (patch.width(), patch.height());
    let Ok(src) = patch.to_buffer() else {
        return;
    };
    let sigma = refine_sigma(strength);
    let blurred = gaussian_blur(&src, sigma, EdgeMode::Clamp);
    let gain = 1.0 + strength;
    for y in 0..rect.height as i64 {
        for x in 0..rect.width as i64 {
            let p = IVec2::new((rect.x + x) as i32, (rect.y + y) as i32);
            let a = buf.get(p) * opacity * selection.coverage_at(p);
            if a <= 0.0 {
                continue;
            }
            let lx = rect.x + x - patch.origin().x as i64;
            let ly = rect.y + y - patch.origin().y as i64;
            if lx < 0 || ly < 0 || lx >= w as i64 || ly >= h as i64 {
                // The stroke rect and the patch box are built from the same
                // union; desync would degrade quietly, so catch it in dev.
                debug_assert!(false, "stroke rect outside the patch box");
                continue;
            }
            let blurred_v = blurred.get(lx as u32, ly as u32)[0];
            // The re-grade: push the blurred sample away from the midpoint —
            // the BYTE-space midpoint 128/255 (the same midpoint card 060's
            // contrast curve uses; the GAIN is milder here — 1+strength vs
            // 1+3·strength — because a brush stroke re-runs per dab pass and
            // must stay repeatable), so a constant field is an exact fixed
            // point.
            let graded = refine_grade(blurred_v, gain);
            let original = patch.get(p);
            let mixed = original * (1.0 - a) + graded * a;
            patch.set(p, mixed);
        }
    }
}

/// Clone-source bookkeeping shared by the clone stamp and the healing brush.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CloneSource {
    /// Document point set with alt-click.
    pub anchor: Option<Vec2>,
    /// Keep the source/destination offset across strokes.
    pub aligned: bool,
    /// Read the source from somewhere other than the layer being painted.
    pub key: Option<PixelKey>,
    offset: Option<IVec2>,
}

impl CloneSource {
    /// Set the sample point; clears any locked-in offset so the next stroke
    /// starts from the new anchor.
    pub fn set_anchor(&mut self, p: Vec2) {
        self.anchor = Some(p);
        self.offset = None;
    }

    /// Decide the offset for a stroke starting at `start`.
    ///
    /// Non-aligned: every stroke restarts at the anchor, so the offset is
    /// recomputed each time. Aligned: the offset is fixed the first time a
    /// stroke is made after the anchor was set, and every later stroke keeps
    /// it — which is what lets you rebuild a large area in several passes.
    pub fn begin_stroke(&mut self, start: Vec2) -> Option<IVec2> {
        let anchor = self.anchor?;
        let fresh = IVec2::new(
            (anchor.x - start.x).round() as i32,
            (anchor.y - start.y).round() as i32,
        );
        if self.aligned {
            Some(*self.offset.get_or_insert(fresh))
        } else {
            self.offset = Some(fresh);
            Some(fresh)
        }
    }

    /// The offset currently in force, if a stroke has begun.
    pub fn offset(&self) -> Option<IVec2> {
        self.offset
    }
}

/// The one tool type behind every stroke-driven tool in the palette.
///
/// Brush, pencil, eraser, clone stamp, healing brush, blur, dodge and the rest
/// differ only in their [`StrokeOp`] and their default [`BrushSettings`]; the
/// gesture handling, stamping, clipping and command emission are shared, which
/// is why fixing the "emits nothing" bug once fixed it for all of them.
pub struct StrokeTool {
    id: ToolId,
    pub settings: BrushSettings,
    pub op: StrokeOp,
    pub clone: CloneSource,
    /// Take the colour from [`ToolContext::foreground`] at stroke start rather
    /// than from whatever is baked into `op`. On for the palette's tools,
    /// which is why picking a colour changes what the brush paints.
    pub use_foreground: bool,
    /// The paint blend mode the options bar's Mode combo holds — how a
    /// source-over dab combines with the pixels under it. Reaches the tool
    /// through [`Tool::set_setting`] under [`crate::BLEND_MODE_KEY`].
    pub blend_mode: BlendMode,
    emitter: Option<DabEmitter>,
    /// The colour under the first sample, for the tolerance-driven ops.
    base_color: [f32; 4],
    offset: IVec2,
    /// W4-B: how many of the emitter's dabs the live preview has already
    /// laid in. [`StrokeTool::live_paint`] recomputes only the tiles the dabs
    /// after this index can change; reset with every new stroke.
    previewed: usize,
    /// W4-B: the smudge walk so far, shared by the live preview and the
    /// release; `None` outside a smudge stroke.
    smudge: Option<SmudgeRun>,
}

impl StrokeTool {
    pub fn new(id: ToolId, settings: BrushSettings, op: StrokeOp) -> Self {
        Self {
            id,
            settings,
            op,
            clone: CloneSource::default(),
            use_foreground: true,
            blend_mode: BlendMode::Normal,
            emitter: None,
            base_color: [0.0; 4],
            offset: IVec2::ZERO,
            previewed: 0,
            smudge: None,
        }
    }

    /// The dabs stamped so far, for a live preview overlay.
    pub fn dabs(&self) -> &[Dab] {
        self.emitter.as_ref().map(|e| e.dabs()).unwrap_or(&[])
    }

    /// Sample the colour a tolerance-driven op measures against.
    fn sample_base(&mut self, ctx: &ToolContext<'_>, p: Vec2) {
        let key = match ctx.sample_key() {
            Ok(k) => k,
            Err(_) => return,
        };
        let pt = IVec2::new(p.x.round() as i32, p.y.round() as i32);
        let rect = PixelRect::new(pt.x as i64, pt.y as i64, 1, 1);
        if let Ok(patch) = ColorPatch::load(ctx.tiles, key, rect) {
            self.base_color = patch.get(pt);
        }
    }

    /// Turn the finished stroke into one command.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        self.previewed = 0;
        let smudge = self.smudge.take();
        let Some(emitter) = self.emitter.take() else {
            return Ok(());
        };
        let dabs = emitter.dabs();
        if dabs.is_empty() {
            return Ok(());
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        // Card 058: dabs live in the PAINT TARGET's space, so the clip is
        // the canvas expressed there — for a transformed mask, `ctx.canvas`
        // (document space) would clip a visible strip of the mask.
        let clip = ctx.paint_space_canvas.unwrap_or(ctx.canvas);
        let delta = if let (PaintTarget::Layer, StrokeOp::Smudge { strength }) =
            (ctx.paint_target, &self.op)
        {
            // W4-B: finish the walk the live preview began (or run it whole
            // when nothing previewed), so the release is the preview exactly.
            // A walk loaded from pixels the document no longer holds (an
            // undo while the button was down) is discarded and re-run from
            // the pixels as they are now: its cached planes would otherwise
            // commit the undone bytes back.
            let mut run = smudge
                .filter(|run| run.is_current(&*ctx.tiles, key))
                .unwrap_or_default();
            run.advance(
                &*ctx.tiles,
                key,
                dabs,
                clip,
                *strength,
                self.settings.opacity,
                &ctx.selection,
            )?;
            let coords: Vec<TileCoord> = run.planes.keys().copied().collect();
            run.encode(&mut *ctx.tiles, key, coords)?
        } else {
            let Some(rect) = StrokeBuffer::bounds_of(dabs, clip) else {
                return Ok(());
            };
            let env = RenderEnv {
                key,
                paint_target: ctx.paint_target,
                selection: &ctx.selection,
                pattern: ctx.pattern.as_ref(),
            };
            match self.neighbourhood_reach(ctx.paint_target) {
                Some(reach) => {
                    let coords: Vec<TileCoord> =
                        TileBox::covering(rect)?.coords().map(|(_, c)| c).collect();
                    self.render_local(&env, &mut *ctx.tiles, dabs, &coords, clip, reach)?
                        .0
                }
                None => self.render(&env, &mut *ctx.tiles, dabs, rect)?,
            }
        };
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }

    /// Rasterise `dabs` over `rect` onto the target's committed pixels and
    /// store the result through `access`: for the pointwise ops, the one
    /// computation both the release ([`StrokeTool::commit`]) and the live
    /// preview ([`StrokeTool::live_paint`]) run, so the two cannot disagree
    /// about what a stroke paints. (The neighbourhood ops share
    /// [`StrokeTool::render_local`] instead, and smudge its [`SmudgeRun`].)
    fn render(
        &self,
        env: &RenderEnv<'_>,
        access: &mut dyn TileAccess,
        dabs: &[Dab],
        rect: PixelRect,
    ) -> Result<TileDelta, ToolError> {
        let key = env.key;
        let buf = StrokeBuffer::rasterize(dabs, rect)?;
        Ok(match env.paint_target {
            PaintTarget::Mask => {
                if !self.op.works_on_mask() {
                    return Err(ToolError::UnsupportedOnMask);
                }
                let mut patch = CoveragePatch::load(access, key, rect)?;
                // The coverage a dab paints. Painting white on a mask reveals
                // and black conceals, which is exactly the brush colour's
                // luminance — so a grey brush paints partial coverage without
                // needing a separate control.
                //
                // The fallback refuses rather than inventing a value: it is
                // unreachable through `works_on_mask` above, and if that gate
                // ever widens, the new op has to decide what it means here.
                match &self.op {
                    StrokeOp::RefineBoundary { strength } => {
                        // Card 061: LOCAL refinement, bounded to the painted
                        // band — the pipeline runs once over the stroke's
                        // bounding rect, and only pixels the dabs touch move
                        // toward the regraded value.
                        apply_refine_to_mask(
                            &mut patch,
                            &buf,
                            *strength,
                            self.settings.opacity,
                            env.selection,
                        );
                    }
                    _ => {
                        let value = match &self.op {
                            StrokeOp::Erase => 0.0,
                            StrokeOp::Paint { color } => crate::patch::mask_coverage_of(*color),
                            _ => return Err(ToolError::UnsupportedOnMask),
                        };
                        apply_stroke_to_mask(
                            &mut patch,
                            &buf,
                            value,
                            self.settings.opacity,
                            env.selection,
                        );
                    }
                }
                patch.commit(access, key)?
            }
            PaintTarget::Layer => {
                let mut patch = ColorPatch::load(access, key, rect)?;
                if let StrokeOp::Smudge { strength } = self.op {
                    apply_smudge(
                        &mut patch,
                        dabs,
                        // `rect` is already `bounds_of(dabs, clip)`: the
                        // dabs' union clipped to the document.
                        rect,
                        strength,
                        self.settings.opacity,
                        env.selection,
                    );
                } else {
                    let source = if self.op.needs_source() {
                        let src_key = self.clone.key.unwrap_or(key);
                        let src_rect = PixelRect::new(
                            rect.x + self.offset.x as i64,
                            rect.y + self.offset.y as i64,
                            rect.width,
                            rect.height,
                        );
                        Some(ColorPatch::load(access, src_key, src_rect)?)
                    } else {
                        None
                    };
                    let sources = StrokeSources {
                        source: source.as_ref(),
                        offset: self.offset,
                        pattern: env.pattern,
                    };
                    apply_stroke(
                        &mut patch,
                        &buf,
                        &self.op,
                        &sources,
                        self.base_color,
                        StrokeBlend {
                            opacity: self.settings.opacity,
                            mode: self.blend_mode,
                        },
                        env.selection,
                    )?;
                }
                patch.commit(access, key)?
            }
        })
    }

    /// W4-B: how far around a pixel a neighbourhood op reads to decide it,
    /// or `None` for the ops that read only the pixel itself (its coverage,
    /// its committed value, the selection there and a source at a fixed
    /// offset) — and for smudge, which is a sequence ([`SmudgeRun`]).
    ///
    /// For a neighbourhood op a tile's result is decided by the committed
    /// pixels and the dabs within this reach of the tile, so the stroke is
    /// rendered one tile at a time ([`StrokeTool::render_local`]) by the
    /// release and the live preview alike, and a preview sample recomputes
    /// only the tiles within reach of its new dabs.
    fn neighbourhood_reach(&self, target: PaintTarget) -> Option<i64> {
        match (target, &self.op) {
            (PaintTarget::Layer, StrokeOp::Blur { radius })
            | (PaintTarget::Layer, StrokeOp::Sharpen { radius, .. }) => {
                Some(gaussian_reach(radius.max(0.1)))
            }
            (PaintTarget::Layer, StrokeOp::Healing { softness }) => {
                let s = softness.max(0.5);
                Some(gaussian_reach(s).max(heal_reach(s)))
            }
            (PaintTarget::Layer, StrokeOp::SpotHealing) => Some(heal_reach(6.0)),
            (PaintTarget::Mask, StrokeOp::RefineBoundary { strength }) => {
                Some(gaussian_reach(refine_sigma(*strength)))
            }
            _ => None,
        }
    }

    /// W4-B: render a neighbourhood op over the tiles `coords`, each tile
    /// from the committed pixels and the dabs within `reach` of it, and store
    /// the result through `access`. Reports the delta and the tiles it
    /// actually computed (those some dab reaches).
    ///
    /// Every read for a tile stays inside the tile grown by `reach` and
    /// clipped to `clip` (the one clamped edge is the document's), so a
    /// tile's bytes are the same whether the release renders the whole
    /// stroke or a preview sample renders the few tiles its new dabs reach.
    fn render_local(
        &self,
        env: &RenderEnv<'_>,
        access: &mut dyn TileAccess,
        dabs: &[Dab],
        coords: &[TileCoord],
        clip: PixelRect,
        reach: i64,
    ) -> Result<(TileDelta, Vec<TileCoord>), ToolError> {
        let mut windows = Vec::new();
        for coord in coords {
            let (ox, oy) = coord.pixel_origin();
            let tile = PixelRect::new(ox, oy, TILE_SIZE, TILE_SIZE);
            if let Some(w) = intersect(tile, clip) {
                if dabs.iter().any(|d| dab_touches(d, w)) {
                    windows.push((*coord, w));
                }
            }
        }
        let Some(need) = windows
            .iter()
            .filter_map(|(_, w)| intersect(grow(*w, reach), clip))
            .reduce(union_rect)
        else {
            return Ok((TileDelta::default(), Vec::new()));
        };
        // Only the dabs that reach the region, in stroke order: per pixel the
        // accumulation sees the same dabs in the same order as the whole.
        let near: Vec<Dab> = dabs
            .iter()
            .copied()
            .filter(|d| dab_touches(d, need))
            .collect();
        let buf = StrokeBuffer::rasterize(&near, need)?;
        // Only covered pixels are written, so each window's work shrinks to
        // the covered pixels' bounds inside it; the rest of the tile keeps
        // its committed bytes, exactly as the whole-stroke render leaves it.
        let windows: Vec<(TileCoord, PixelRect)> = windows
            .into_iter()
            .filter_map(|(c, w)| buf.covered_within(w).map(|cw| (c, cw)))
            .collect();
        let Some(need) = windows
            .iter()
            .filter_map(|(_, w)| intersect(grow(*w, reach), clip))
            .reduce(union_rect)
        else {
            return Ok((TileDelta::default(), Vec::new()));
        };
        let key = env.key;
        let opacity = self.settings.opacity.clamp(0.0, 1.0);
        let delta = match env.paint_target {
            PaintTarget::Layer => {
                let mut patch = ColorPatch::load(access, key, need)?;
                let source = if self.op.needs_source() {
                    let src_key = self.clone.key.unwrap_or(key);
                    let src_rect = PixelRect::new(
                        need.x + self.offset.x as i64,
                        need.y + self.offset.y as i64,
                        need.width,
                        need.height,
                    );
                    Some(ColorPatch::load(access, src_key, src_rect)?)
                } else {
                    None
                };
                let sources = StrokeSources {
                    source: source.as_ref(),
                    offset: self.offset,
                    pattern: env.pattern,
                };
                let bounds = intersect(patch.rect(), clip).ok_or(ToolError::Degenerate)?;
                let mut writes = Vec::new();
                for (_, w) in &windows {
                    let aux = local_aux(
                        &self.op,
                        patch.buffer(),
                        patch.origin(),
                        &buf,
                        &sources,
                        *w,
                        bounds,
                    )?;
                    for y in w.y..w.bottom() {
                        for x in w.x..w.right() {
                            let p = IVec2::new(x as i32, y as i32);
                            let cov = buf.get(p);
                            if cov <= 0.0 {
                                continue;
                            }
                            let a = cov * opacity * env.selection.coverage_at(p);
                            if a <= 0.0 {
                                continue;
                            }
                            let dst = patch.get(p);
                            let s = aux.get((x - w.x) as u32, (y - w.y) as u32);
                            writes.push((
                                p,
                                [
                                    dst[0] + (s[0] - dst[0]) * a,
                                    dst[1] + (s[1] - dst[1]) * a,
                                    dst[2] + (s[2] - dst[2]) * a,
                                    dst[3] + (s[3] - dst[3]) * a,
                                ],
                            ));
                        }
                    }
                }
                for (p, px) in writes {
                    patch.set(p, px);
                }
                patch.commit(access, key)?
            }
            PaintTarget::Mask => {
                let StrokeOp::RefineBoundary { strength } = self.op else {
                    return Err(ToolError::UnsupportedOnMask);
                };
                let mut patch = CoveragePatch::load(access, key, need)?;
                let plane = patch.to_buffer()?;
                let origin = patch.origin();
                let bounds = intersect(patch.rect(), clip).ok_or(ToolError::Degenerate)?;
                let sigma = refine_sigma(strength);
                let gain = 1.0 + strength.clamp(0.0, 1.0);
                let mut writes = Vec::new();
                for (_, w) in &windows {
                    let a = intersect(grow(*w, gaussian_reach(sigma)), bounds).unwrap_or(*w);
                    let blurred = crop(
                        &gaussian_blur(&crop(&plane, origin, a)?, sigma, EdgeMode::Clamp),
                        origin_of(a),
                        *w,
                    )?;
                    for y in w.y..w.bottom() {
                        for x in w.x..w.right() {
                            let p = IVec2::new(x as i32, y as i32);
                            let amount = buf.get(p) * opacity * env.selection.coverage_at(p);
                            if amount <= 0.0 {
                                continue;
                            }
                            let v = blurred.get((x - w.x) as u32, (y - w.y) as u32)[0];
                            let graded = refine_grade(v, gain);
                            let original = patch.get(p);
                            writes.push((p, original * (1.0 - amount) + graded * amount));
                        }
                    }
                }
                for (p, v) in writes {
                    patch.set(p, v);
                }
                patch.commit(access, key)?
            }
        };
        Ok((delta, windows.into_iter().map(|(c, _)| c).collect()))
    }

    /// W4-B: the stroke so far, as the tiles the release would commit — or
    /// `None` when no dab arrived since the last call.
    ///
    /// Runs the release's own computation against the committed pixels, but
    /// stores the bytes in a private side store: nothing is written to
    /// `ctx.tiles`, nothing is emitted, and the stroke carries on exactly as
    /// if this had not been asked. With smoothing off, a release at the last
    /// previewed position adds no dab, so what it commits is these bytes
    /// exactly. With smoothing on, [`DabEmitter::finish`] walks the lagging
    /// smoothed point on to the raw release position, and the release also
    /// commits that tail's dabs, which no preview showed. A walk loaded from
    /// pixels the document has since replaced (an undo mid-drag) is re-run
    /// from the current pixels, by the next preview and by the release.
    ///
    /// Incremental for every op: an answer recomputes only the tiles the
    /// new dabs can change and lists just those, so a sample costs its own
    /// dabs, not the stroke so far.
    ///
    /// * Pointwise ops (paint, erase, the stamps, dodge, burn, sponge, colour
    ///   replacement): the tiles the new dabs reach, each from every dab that
    ///   touches it ([`StrokeTool::render`] over the one tile).
    /// * Neighbourhood ops (blur, sharpen, the healing brushes, the mask
    ///   refine): the tiles within the op's reach
    ///   ([`StrokeTool::neighbourhood_reach`]) of the new dabs, each from the
    ///   dabs within reach of it ([`StrokeTool::render_local`] — the path the
    ///   release takes too). The healing brushes' reach is the widest radius
    ///   their low-frequency estimate can escalate to (192 px at the default
    ///   softness, 288 px for spot healing), so a sample recomputes every
    ///   tile the stroke covers within that distance: bounded, but heavier
    ///   than a blur's.
    /// * Smudge: the new dabs are applied to the walk the preview keeps
    ///   ([`SmudgeRun`]), and the tiles they reached are re-encoded.
    ///
    /// Only the first answer of a stroke sets [`LivePaint::replace`].
    pub fn live_paint(
        &mut self,
        ctx: &mut ToolContext<'_>,
    ) -> Result<Option<LivePaint>, ToolError> {
        let Some(emitter) = &self.emitter else {
            return Ok(None);
        };
        let dabs = emitter.dabs();
        let from = self.previewed.min(dabs.len());
        if from == dabs.len() {
            return Ok(None);
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let clip = ctx.paint_space_canvas.unwrap_or(ctx.canvas);
        let env = RenderEnv {
            key,
            paint_target: ctx.paint_target,
            selection: &ctx.selection,
            pattern: ctx.pattern.as_ref(),
        };
        let mut side = SideStore {
            base: &*ctx.tiles,
            stored: HashMap::new(),
        };
        let replace = from == 0;
        let mut tiles = Vec::new();
        let answer = |side: &mut SideStore<'_>, delta: &TileDelta, coord: TileCoord| match delta
            .get(coord)
        {
            Some(hash) => side.live_tile(hash),
            None => LiveTile::Committed,
        };
        if let (PaintTarget::Layer, StrokeOp::Smudge { strength }) = (ctx.paint_target, &self.op) {
            // A walk the document moved under restarts from the current
            // pixels, re-answering every tile it reaches.
            if !self
                .smudge
                .as_ref()
                .is_none_or(|run| run.is_current(side.base, key))
            {
                self.smudge = None;
            }
            let run = self.smudge.get_or_insert_with(SmudgeRun::default);
            let touched = run.advance(
                side.base,
                key,
                dabs,
                clip,
                *strength,
                self.settings.opacity,
                env.selection,
            )?;
            let delta = run.encode(&mut side, key, touched.iter().copied())?;
            for coord in touched {
                tiles.push((coord, answer(&mut side, &delta, coord)));
            }
        } else if let Some(reach) = self.neighbourhood_reach(ctx.paint_target) {
            if let Some(fresh) = StrokeBuffer::bounds_of(&dabs[from..], clip) {
                if let Some(area) = intersect(grow(fresh, reach), clip) {
                    let coords: Vec<TileCoord> =
                        TileBox::covering(area)?.coords().map(|(_, c)| c).collect();
                    let (delta, computed) =
                        self.render_local(&env, &mut side, dabs, &coords, clip, reach)?;
                    for coord in computed {
                        tiles.push((coord, answer(&mut side, &delta, coord)));
                    }
                }
            }
        } else if replace {
            if let Some(rect) = StrokeBuffer::bounds_of(dabs, clip) {
                let delta = self.render(&env, &mut side, dabs, rect)?;
                for edit in delta.iter() {
                    tiles.push((edit.coord, side.live_tile(edit.hash)));
                }
            }
        } else if let Some(fresh) = StrokeBuffer::bounds_of(&dabs[from..], clip) {
            for (_, coord) in TileBox::covering(fresh)?.coords() {
                let (ox, oy) = coord.pixel_origin();
                let tile = PixelRect::new(ox, oy, TILE_SIZE, TILE_SIZE);
                let Some(rect) = intersect(tile, clip) else {
                    continue;
                };
                let delta = self.render(&env, &mut side, dabs, rect)?;
                tiles.push((coord, answer(&mut side, &delta, coord)));
            }
        }
        self.previewed = dabs.len();
        Ok(Some(LivePaint {
            target,
            key,
            replace,
            tiles,
        }))
    }
}

/// W4-B: one tile of a live stroke preview, as the release would leave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveTile {
    /// The stroke leaves this tile exactly as committed.
    Committed,
    /// The stroke leaves this tile empty: fully transparent, or zero coverage
    /// on a mask.
    Cleared,
    /// The tile's encoded bytes with the stroke so far laid in.
    Bytes(Vec<u8>),
}

/// W4-B: the in-flight stroke a [`StrokeTool`] publishes on every Move
/// sample, through [`Tool::live_paint`]. A preview lens, not an edit: the
/// shell lays these tiles over the committed target for display only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePaint {
    /// What the release's `PaintTiles` will name.
    pub target: PixelTarget,
    /// The tile map `target` resolves to.
    pub key: PixelKey,
    /// `true` on a stroke's first answer: every tile the preview held before
    /// is stale — drop them and keep only [`Self::tiles`]. `false` means
    /// `tiles` updates the held preview in place, and a tile it does not
    /// list is unchanged.
    pub replace: bool,
    /// The tiles this answer recomputed.
    pub tiles: Vec<(TileCoord, LiveTile)>,
}

/// What [`StrokeTool::render`] reads from the context, split out so the
/// release can hand it `ctx.tiles` mutably while these stay borrowed.
struct RenderEnv<'e> {
    key: PixelKey,
    paint_target: PaintTarget,
    selection: &'e Selection,
    pattern: Option<&'e Pattern>,
}

/// The live preview's tile access: reads through to the committed store and
/// writes into a private map, so a preview never adds a blob to the
/// document's store.
struct SideStore<'b> {
    base: &'b dyn TileAccess,
    stored: HashMap<TileHash, Vec<u8>>,
}

impl SideStore<'_> {
    fn live_tile(&mut self, hash: Option<TileHash>) -> LiveTile {
        match hash {
            None => LiveTile::Cleared,
            Some(h) => match self.stored.get(&h) {
                Some(bytes) => LiveTile::Bytes(bytes.clone()),
                None => match self.base.bytes(h) {
                    Some(bytes) => LiveTile::Bytes(bytes.to_vec()),
                    None => LiveTile::Committed,
                },
            },
        }
    }
}

impl TileAccess for SideStore<'_> {
    fn tile_hash(&self, key: PixelKey, coord: TileCoord) -> Option<TileHash> {
        self.base.tile_hash(key, coord)
    }

    fn bytes(&self, hash: TileHash) -> Option<&[u8]> {
        match self.stored.get(&hash) {
            Some(bytes) => Some(bytes.as_slice()),
            None => self.base.bytes(hash),
        }
    }

    fn store(&mut self, data: Vec<u8>) -> TileHash {
        let hash = TileHash::of(&data);
        self.stored.entry(hash).or_insert(data);
        hash
    }
}

/// The overlap of two rects, or `None` when they do not overlap.
fn intersect(a: PixelRect, b: PixelRect) -> Option<PixelRect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    (x1 > x0 && y1 > y0).then(|| PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

impl Tool for StrokeTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        // Card 040: paint tools write the ACTIVE layer's pixels in LAYER
        // space — map every sample through the shell's document→layer
        // mapping so a moved/scaled layer is painted where the pointer
        // displays. Brush radius is defined in layer pixels: under a
        // nonuniform display scale the dab stays circular in layer space.
        // DOCUMENTED LIMITATION: the document selection's coverage is tested
        // in the same (layer) space as the dabs — a selection drawn for an
        // untransformed view constrains a transformed layer only where the
        // two coincide. Full selection re-mapping rides a later card.
        let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
        let pos = to_layer.transform_point2(event.pos);
        // Alt-click on a source-reading tool sets the sample point instead of
        // starting a stroke — the gesture every clone stamp uses.
        if self.op.needs_source() && event.modifiers.alt {
            self.clone.set_anchor(pos);
            return Ok(());
        }
        if self.op.needs_source() {
            self.offset = self.clone.begin_stroke(pos).ok_or(ToolError::Degenerate)?;
        }
        if self.use_foreground {
            let fg = ctx.foreground;
            match &mut self.op {
                StrokeOp::Paint { color } | StrokeOp::ColorReplacement { color, .. } => *color = fg,
                _ => {}
            }
        }
        match &self.op {
            StrokeOp::ColorReplacement { .. } | StrokeOp::BackgroundErase { .. } => {
                self.sample_base(ctx, to_layer.transform_point2(event.pos))
            }
            _ => {}
        }
        self.emitter = Some(DabEmitter::begin(self.settings, pos, event.pressure)?);
        self.previewed = 0;
        self.smudge = None;
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let Some(e) = &mut self.emitter {
            // Card 040: every sample maps document → layer space.
            let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
            e.extend(to_layer.transform_point2(event.pos), event.pressure)?;
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let Some(e) = &mut self.emitter {
            let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
            e.finish(to_layer.transform_point2(event.pos), event.pressure)?;
        }
        self.commit(ctx)
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.emitter = None;
        self.previewed = 0;
        self.smudge = None;
    }

    fn is_active(&self) -> bool {
        self.emitter.is_some()
    }

    /// W4-B: the stroke so far, for the shell's live preview lens.
    fn live_paint(&mut self, ctx: &mut ToolContext<'_>) -> Result<Option<LivePaint>, ToolError> {
        StrokeTool::live_paint(self, ctx)
    }

    /// Take the application's brush — but never mid-stroke.
    ///
    /// [`DabEmitter`] is built from `settings` at pointer-down and `commit`
    /// reads `settings.opacity` at pointer-up, so a change accepted between
    /// them would finish a stroke under settings its dabs were never spaced
    /// for. A caller may therefore hand this the current brush on every event.
    fn set_brush(&mut self, brush: BrushSettings) {
        if self.emitter.is_none() {
            self.settings = brush;
        }
    }

    fn brush(&self) -> Option<BrushSettings> {
        Some(self.settings)
    }

    /// Every option the registry declares for a stroke tool reaches the tool
    /// here, whatever its kind — a refused value is surfaced by the shell
    /// rather than silently ignored (an options-bar control that did nothing
    /// was the defect this seam exists to prevent), and a key whose value
    /// arrives as the wrong kind is [`ToolError::OptionKindMismatch`] rather
    /// than [`ToolError::UnknownOption`], so the two failures read apart.
    ///
    /// Four families:
    ///
    /// * the brush-shared keys ([`crate::registry::BRUSH_OPTION_KEYS`]) write
    ///   the brush, with the same never-mid-stroke rule as [`Tool::set_brush`]
    ///   — a shell that routes them through `set_brush` instead loses nothing;
    /// * the op's own floats (`exposure`, `radius`, `tolerance`, …) write the
    ///   op;
    /// * the op's own choices and flags — Dodge/Burn `range` (Shadows,
    ///   Midtones, Highlights), Sponge `mode` (Desaturate, Saturate), the
    ///   clone stamp's and healing brush's `aligned` — write the op or the
    ///   clone source;
    /// * the options bar's paint blend mode ([`crate::BLEND_MODE_KEY`], a
    ///   Choice indexing [`BlendMode::ALL`]) writes [`Self::blend_mode`] —
    ///   for the ops that composite a source colour
    ///   ([`StrokeOp::composites_source`]: Brush, Pencil, Clone Stamp,
    ///   Pattern Stamp); the retouching ops refuse it as unknown, because
    ///   [`apply_stroke`] has no colour of theirs to blend and an accepted
    ///   key that changed nothing would be the dead control this seam
    ///   exists to prevent.
    ///
    /// A choice index past the end of its list is clamped to the last entry,
    /// the same rule the options bar's own `conform` applies.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        let mismatch = || {
            Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            })
        };
        let unknown = || {
            Err(ToolError::UnknownOption {
                key: key.to_owned(),
            })
        };

        // The paint blend mode: answered only where `apply_stroke` will
        // read it — the `Blend::Over` ops. A Lerp/Erase op accepting the key
        // would hold a mode its compositing never consults.
        if key == crate::BLEND_MODE_KEY {
            if !self.op.composites_source() {
                return unknown();
            }
            return match setting {
                ToolSetting::Choice(index) => {
                    let last = BlendMode::ALL.len() - 1;
                    self.blend_mode =
                        crate::blend_mode_from_choice(index.min(last)).unwrap_or(BlendMode::Normal);
                    Ok(())
                }
                _ => mismatch(),
            };
        }

        // The brush-shared keys: the brush is part of what a stroke tool is,
        // so the tool answers them directly as well as through `set_brush`.
        if crate::registry::BRUSH_OPTION_KEYS.contains(&key) {
            let mut brush = self.settings;
            match (key, setting) {
                ("size", ToolSetting::Float(v)) => brush.size = v,
                ("hardness", ToolSetting::Float(v)) => brush.hardness = v,
                ("spacing", ToolSetting::Float(v)) => brush.spacing = v,
                ("angle", ToolSetting::Float(v)) => brush.angle = v,
                ("roundness", ToolSetting::Float(v)) => brush.roundness = v,
                ("opacity", ToolSetting::Float(v)) => brush.opacity = v,
                ("flow", ToolSetting::Float(v)) => brush.flow = v,
                ("smoothing", ToolSetting::Float(v)) => brush.smoothing = v,
                ("size_pressure", ToolSetting::Bool(v)) => brush.size_pressure = v,
                ("flow_pressure", ToolSetting::Bool(v)) => brush.flow_pressure = v,
                _ => return mismatch(),
            }
            // Validated the way `DabEmitter::begin` would validate it: a
            // NaN size or a zero spacing is refused here rather than at the
            // press, where it would abort the stroke.
            let brush = brush.validated()?;
            self.set_brush(brush);
            return Ok(());
        }

        match (key, setting, &mut self.op) {
            // ----- the op's own floats ------------------------------------
            (
                "strength",
                ToolSetting::Float(v),
                StrokeOp::RefineBoundary { strength } | StrokeOp::Smudge { strength },
            ) => {
                *strength = v.clamp(0.0, 1.0);
                Ok(())
            }
            ("radius", ToolSetting::Float(v), StrokeOp::Blur { radius }) => {
                *radius = v.clamp(0.1, 64.0);
                Ok(())
            }
            ("amount", ToolSetting::Float(v), StrokeOp::Sharpen { amount, .. }) => {
                *amount = v.clamp(0.0, 4.0);
                Ok(())
            }
            (
                "tolerance",
                ToolSetting::Float(v),
                StrokeOp::ColorReplacement { tolerance, .. }
                | StrokeOp::BackgroundErase { tolerance, .. },
            ) => {
                *tolerance = v.clamp(0.0, 1.0);
                Ok(())
            }
            ("softness", ToolSetting::Float(v), StrokeOp::Healing { softness }) => {
                // The registry's Softness is a Gaussian sigma in PIXELS
                // (0.5..64, default 4) — not a 0..1 strength.
                *softness = v.clamp(0.5, 64.0);
                Ok(())
            }
            (
                "exposure",
                ToolSetting::Float(v),
                StrokeOp::Dodge { exposure, .. } | StrokeOp::Burn { exposure, .. },
            ) => {
                *exposure = v.clamp(0.0, 1.0);
                Ok(())
            }
            ("amount", ToolSetting::Float(v), StrokeOp::Sponge { amount, .. }) => {
                *amount = v.clamp(0.0, 1.0);
                Ok(())
            }
            // ----- the op's own choices and flags -------------------------
            (
                "range",
                ToolSetting::Choice(index),
                StrokeOp::Dodge { range, .. } | StrokeOp::Burn { range, .. },
            ) => {
                *range = match index {
                    0 => ToneRange::Shadows,
                    1 => ToneRange::Midtones,
                    _ => ToneRange::Highlights,
                };
                Ok(())
            }
            ("mode", ToolSetting::Choice(index), StrokeOp::Sponge { mode, .. }) => {
                *mode = if index == 0 {
                    SpongeMode::Desaturate
                } else {
                    SpongeMode::Saturate
                };
                Ok(())
            }
            ("aligned", ToolSetting::Bool(v), StrokeOp::CloneStamp | StrokeOp::Healing { .. }) => {
                self.clone.aligned = v;
                Ok(())
            }
            // ----- a known key with the wrong kind of value ---------------
            ("strength", _, StrokeOp::RefineBoundary { .. } | StrokeOp::Smudge { .. })
            | ("radius", _, StrokeOp::Blur { .. })
            | ("amount", _, StrokeOp::Sharpen { .. } | StrokeOp::Sponge { .. })
            | (
                "tolerance",
                _,
                StrokeOp::ColorReplacement { .. } | StrokeOp::BackgroundErase { .. },
            )
            | ("softness", _, StrokeOp::Healing { .. })
            | ("exposure" | "range", _, StrokeOp::Dodge { .. } | StrokeOp::Burn { .. })
            | ("mode", _, StrokeOp::Sponge { .. })
            | ("aligned", _, StrokeOp::CloneStamp | StrokeOp::Healing { .. }) => mismatch(),
            _ => unknown(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Dab;

    fn dab(x: f32, y: f32, r: f32, flow: f32) -> Dab {
        Dab {
            center: Vec2::new(x, y),
            radius: r,
            hardness: 1.0,
            angle: 0.0,
            roundness: 1.0,
            flow,
            aliased: false,
        }
    }

    #[test]
    fn overlapping_dabs_saturate_instead_of_summing() {
        let rect = PixelRect::new(0, 0, 40, 40);
        let dabs: Vec<Dab> = (0..10).map(|_| dab(20.0, 20.0, 6.0, 1.0)).collect();
        let buf = StrokeBuffer::rasterize(&dabs, rect).unwrap();
        assert!((buf.get(IVec2::new(20, 20)) - 1.0).abs() < 1e-6);
        assert!(buf.peak() <= 1.0 + 1e-6);
    }

    #[test]
    fn flow_below_one_builds_up_across_dabs() {
        let rect = PixelRect::new(0, 0, 40, 40);
        let one = StrokeBuffer::rasterize(&[dab(20.0, 20.0, 6.0, 0.25)], rect).unwrap();
        let three = StrokeBuffer::rasterize(
            &[
                dab(20.0, 20.0, 6.0, 0.25),
                dab(20.0, 20.0, 6.0, 0.25),
                dab(20.0, 20.0, 6.0, 0.25),
            ],
            rect,
        )
        .unwrap();
        let a = one.get(IVec2::new(20, 20));
        let b = three.get(IVec2::new(20, 20));
        assert!((a - 0.25).abs() < 1e-5, "one dab laid {a}");
        // 1 - 0.75^3 = 0.578125
        assert!((b - 0.578125).abs() < 1e-4, "three dabs laid {b}");
        assert!(b < 0.75, "accumulation must not be a plain sum");
    }

    #[test]
    fn a_low_frequency_estimate_never_looks_at_the_region_it_is_repairing() {
        // A light plane with a hard dark hole punched in the middle of it.
        let mut buf = FilterBuffer::filled(16, 16, [0.8, 0.8, 0.8, 1.0]).unwrap();
        let mut covered = vec![0.0f32; 16 * 16];
        for y in 5..11u32 {
            for x in 5..11u32 {
                buf.set(x, y, [0.05, 0.05, 0.05, 1.0]);
                covered[(y * 16 + x) as usize] = 1.0;
            }
        }

        let out = low_frequency_outside(&buf, &covered, 2.0).unwrap();
        let c = out.get(8, 8);
        assert!(
            (c[0] - 0.8).abs() < 0.02,
            "the hole leaked into its own repair: {c:?}"
        );
        assert!((c[3] - 1.0).abs() < 0.02, "alpha drifted: {c:?}");

        // A plain blur, by contrast, is dragged nearly all the way down to the
        // hole's own value — which is the defect this function exists to fix.
        let plain = gaussian_blur(&buf, 2.0, EdgeMode::Clamp).get(8, 8);
        assert!(
            plain[0] < 0.3,
            "the fixture is not a hard enough hole to prove anything: {plain:?}"
        );

        // Covered edge to edge there is no outside to look at, so the plain
        // blur is the documented fallback rather than a divide by zero.
        let all = vec![1.0f32; 16 * 16];
        let fallback = low_frequency_outside(&buf, &all, 2.0).unwrap();
        assert_eq!(
            fallback.pixels(),
            gaussian_blur(&buf, 2.0, EdgeMode::Clamp).pixels()
        );

        // A mismatched coverage plane is a refusal, not an index panic.
        assert!(low_frequency_outside(&buf, &[0.0f32; 4], 2.0).is_err());
    }

    #[test]
    fn tone_ranges_peak_where_they_should_and_overlap_smoothly() {
        assert!(ToneRange::Shadows.weight(0.15) > ToneRange::Shadows.weight(0.85));
        assert!(ToneRange::Highlights.weight(0.85) > ToneRange::Highlights.weight(0.15));
        assert!((ToneRange::Midtones.weight(0.5) - 1.0).abs() < 1e-6);
        assert!(ToneRange::Midtones.weight(0.5) > ToneRange::Midtones.weight(0.0));
    }

    #[test]
    fn a_clone_source_keeps_its_offset_only_when_aligned() {
        let mut cs = CloneSource {
            aligned: false,
            ..Default::default()
        };
        cs.set_anchor(Vec2::new(100.0, 100.0));
        assert_eq!(
            cs.begin_stroke(Vec2::new(10.0, 10.0)),
            Some(IVec2::new(90, 90))
        );
        assert_eq!(
            cs.begin_stroke(Vec2::new(50.0, 50.0)),
            Some(IVec2::new(50, 50))
        );

        let mut al = CloneSource {
            aligned: true,
            ..Default::default()
        };
        al.set_anchor(Vec2::new(100.0, 100.0));
        assert_eq!(
            al.begin_stroke(Vec2::new(10.0, 10.0)),
            Some(IVec2::new(90, 90))
        );
        assert_eq!(
            al.begin_stroke(Vec2::new(50.0, 50.0)),
            Some(IVec2::new(90, 90)),
            "an aligned clone must not re-anchor on the second stroke"
        );
        // A new anchor releases the lock.
        al.set_anchor(Vec2::new(0.0, 0.0));
        assert_eq!(
            al.begin_stroke(Vec2::new(10.0, 10.0)),
            Some(IVec2::new(-10, -10))
        );

        assert_eq!(CloneSource::default().begin_stroke(Vec2::ZERO), None);
    }
}

/// W4-B: the live stroke preview, pinned against the release it previews.
#[cfg(test)]
mod live_tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tiles::MemoryTiles;
    use layer_model::{LayerId, MaskId};
    use raster::{PixelFormat, Tile, TileCoord};

    const CANVAS: PixelRect = PixelRect {
        x: 0,
        y: 0,
        width: 512,
        height: 256,
    };

    /// Two layer tiles of a busy opaque gradient (so dodge, sponge, blur and
    /// the clone stamp all have something to change) and one mask tile.
    fn store(layer: LayerId, mask: MaskId) -> MemoryTiles {
        let ts = TILE_SIZE as usize;
        let mut tiles = MemoryTiles::new();
        for tx in 0..2 {
            let mut data = vec![0u8; Tile::byte_len(PixelFormat::Rgba8)];
            for y in 0..ts {
                for x in 0..ts {
                    let i = (y * ts + x) * 4;
                    data[i] = (x as u8).wrapping_mul(3).wrapping_add(tx * 40);
                    data[i + 1] = (y as u8).wrapping_mul(5);
                    data[i + 2] = ((x + y) as u8).wrapping_mul(7);
                    data[i + 3] = 255;
                }
            }
            tiles.put(
                PixelKey::Layer(layer),
                TileCoord::new(tx as i32, 0, 0),
                data,
            );
        }
        tiles.put(
            PixelKey::Mask(mask),
            TileCoord::new(0, 0, 0),
            vec![128u8; ts * ts],
        );
        tiles
    }

    fn fold(map: &mut BTreeMap<TileCoord, LiveTile>, live: LivePaint) {
        if live.replace {
            map.clear();
        }
        for (coord, tile) in live.tiles {
            match tile {
                LiveTile::Committed => {
                    map.remove(&coord);
                }
                other => {
                    map.insert(coord, other);
                }
            }
        }
    }

    /// Drive one stroke across the tile seam, folding every live answer, and
    /// return (the folded preview, the committed tiles, whether any answer was
    /// incremental).
    fn drive(
        mut tool: StrokeTool,
        target: PaintTarget,
    ) -> (
        BTreeMap<TileCoord, LiveTile>,
        BTreeMap<TileCoord, LiveTile>,
        bool,
    ) {
        let layer = LayerId::new();
        let mask = MaskId::new();
        let mut tiles = store(layer, mask);
        let blobs = tiles.blob_count();
        let path = [
            (200.0, 100.0),
            (230.0, 110.0),
            (262.0, 118.0),
            (300.0, 130.0),
            (330.0, 150.0),
        ];
        let mut preview = BTreeMap::new();
        let mut incremental = false;
        let commands = {
            let mut ctx = ToolContext::new(&mut tiles, CANVAS).with_layer(layer);
            ctx.active_mask = Some(mask);
            ctx.paint_target = target;
            ctx.pattern = Some(Pattern::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap());
            ctx.foreground = [0.1, 0.8, 0.3, 1.0];
            tool.clone.set_anchor(Vec2::new(40.0, 40.0));
            let (x, y) = path[0];
            tool.on_pointer_down(&mut ctx, PointerEvent::at(x, y))
                .unwrap();
            for (x, y) in &path[1..] {
                tool.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
                    .unwrap();
                let live = tool.live_paint(&mut ctx).unwrap().expect("new dabs");
                incremental |= !live.replace;
                fold(&mut preview, live);
                assert!(ctx.commands().is_empty(), "a preview emitted a command");
                assert_eq!(
                    tool.live_paint(&mut ctx).unwrap(),
                    None,
                    "no new dab, no new answer"
                );
            }
            let (x, y) = path[path.len() - 1];
            tool.on_pointer_up(&mut ctx, PointerEvent::at(x, y))
                .unwrap();
            ctx.drain()
        };
        assert_eq!(commands.len(), 1, "the release is one command");
        let Command::PaintTiles { delta, .. } = &commands[0] else {
            panic!("the release emitted {commands:?}");
        };
        let mut committed = BTreeMap::new();
        for edit in delta.iter() {
            let tile = match edit.hash {
                Some(h) => LiveTile::Bytes(tiles.bytes(h).expect("committed blob").to_vec()),
                None => LiveTile::Cleared,
            };
            committed.insert(edit.coord, tile);
        }
        // Only the release's own blobs reached the store; every preview blob
        // stayed in the tool's side store.
        assert!(tiles.blob_count() <= blobs + delta.len());
        (preview, committed, incremental)
    }

    #[test]
    fn the_last_preview_is_byte_equal_to_what_the_release_commits() {
        let ops: Vec<(&str, StrokeOp, PaintTarget)> = vec![
            (
                "paint",
                StrokeOp::Paint { color: [0.0; 4] },
                PaintTarget::Layer,
            ),
            ("erase", StrokeOp::Erase, PaintTarget::Layer),
            (
                "dodge",
                StrokeOp::Dodge {
                    exposure: 0.5,
                    range: ToneRange::Midtones,
                },
                PaintTarget::Layer,
            ),
            (
                "sponge",
                StrokeOp::Sponge {
                    amount: 0.5,
                    mode: SpongeMode::Desaturate,
                },
                PaintTarget::Layer,
            ),
            ("clone", StrokeOp::CloneStamp, PaintTarget::Layer),
            ("pattern", StrokeOp::PatternStamp, PaintTarget::Layer),
            ("blur", StrokeOp::Blur { radius: 3.0 }, PaintTarget::Layer),
            (
                "sharpen",
                StrokeOp::Sharpen {
                    amount: 1.0,
                    radius: 2.0,
                },
                PaintTarget::Layer,
            ),
            (
                "healing",
                StrokeOp::Healing { softness: 4.0 },
                PaintTarget::Layer,
            ),
            ("spot healing", StrokeOp::SpotHealing, PaintTarget::Layer),
            (
                "smudge",
                StrokeOp::Smudge { strength: 0.6 },
                PaintTarget::Layer,
            ),
            (
                "mask paint",
                StrokeOp::Paint { color: [1.0; 4] },
                PaintTarget::Mask,
            ),
            (
                "mask refine",
                StrokeOp::RefineBoundary { strength: 0.6 },
                PaintTarget::Mask,
            ),
        ];
        for (name, op, target) in ops {
            let settings = BrushSettings {
                size: 24.0,
                hardness: 0.5,
                spacing: 0.1,
                ..BrushSettings::default()
            };
            let tool = StrokeTool::new(ToolId::Brush, settings, op);
            let (preview, committed, incremental) = drive(tool, target);
            assert!(
                !committed.is_empty(),
                "{name}: the stroke committed nothing"
            );
            assert!(
                committed.len() >= 2,
                "{name}: the fixture stroke must cross the tile seam"
            );
            assert_eq!(
                preview, committed,
                "{name}: the last preview is not the committed stroke"
            );
            assert!(
                incremental,
                "{name}: every answer after the first must be incremental"
            );
        }
    }

    #[test]
    fn a_pointwise_answer_recomputes_only_the_tiles_the_new_dabs_reach() {
        let layer = LayerId::new();
        let mask = MaskId::new();
        let mut tiles = store(layer, mask);
        let mut ctx = ToolContext::new(&mut tiles, CANVAS).with_layer(layer);
        let mut tool = StrokeTool::new(
            ToolId::Brush,
            BrushSettings {
                size: 10.0,
                spacing: 0.1,
                ..BrushSettings::default()
            },
            StrokeOp::Paint { color: [1.0; 4] },
        );
        tool.on_pointer_down(&mut ctx, PointerEvent::at(20.0, 20.0))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(40.0, 20.0))
            .unwrap();
        let first = tool.live_paint(&mut ctx).unwrap().unwrap();
        assert!(first.replace, "a stroke's first answer starts the preview");
        // Far across the seam: the new dabs reach tile (1, 0) only.
        tool.on_pointer_move(&mut ctx, PointerEvent::at(400.0, 20.0))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(420.0, 20.0))
            .unwrap();
        let _ = tool.live_paint(&mut ctx).unwrap().unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(440.0, 20.0))
            .unwrap();
        let late = tool.live_paint(&mut ctx).unwrap().unwrap();
        assert!(!late.replace);
        let coords: Vec<TileCoord> = late.tiles.iter().map(|(c, _)| *c).collect();
        assert_eq!(coords, vec![TileCoord::new(1, 0, 0)]);
        tool.cancel(&mut ctx);
        assert_eq!(tool.live_paint(&mut ctx).unwrap(), None, "cancel ends it");
    }

    /// A canvas eight tiles wide, every layer tile busy and every mask tile a
    /// hard 0/255 edge, for the long drags below.
    const WIDE: PixelRect = PixelRect {
        x: 0,
        y: 0,
        width: 8 * TILE_SIZE,
        height: TILE_SIZE,
    };

    fn wide_store(layer: LayerId, mask: MaskId) -> MemoryTiles {
        let ts = TILE_SIZE as usize;
        let mut tiles = MemoryTiles::new();
        for tx in 0..8u8 {
            let mut data = vec![0u8; Tile::byte_len(PixelFormat::Rgba8)];
            for y in 0..ts {
                for x in 0..ts {
                    let i = (y * ts + x) * 4;
                    data[i] = (x as u8).wrapping_mul(3).wrapping_add(tx * 29);
                    data[i + 1] = (y as u8).wrapping_mul(5);
                    data[i + 2] = ((x + y) as u8).wrapping_mul(7);
                    data[i + 3] = 255;
                }
            }
            tiles.put(
                PixelKey::Layer(layer),
                TileCoord::new(tx as i32, 0, 0),
                data,
            );
            // A hard edge along the stroke's row, so the refine has a band.
            let mask_tile: Vec<u8> = (0..ts * ts)
                .map(|i| if (i / ts) < ts / 2 { 255 } else { 0 })
                .collect();
            tiles.put(
                PixelKey::Mask(mask),
                TileCoord::new(tx as i32, 0, 0),
                mask_tile,
            );
        }
        tiles
    }

    /// The brush and every op that reads more than its own pixel.
    fn neighbourhood_ops() -> Vec<(&'static str, StrokeOp, PaintTarget)> {
        vec![
            (
                "paint",
                StrokeOp::Paint {
                    color: [0.0, 0.0, 0.0, 1.0],
                },
                PaintTarget::Layer,
            ),
            ("blur", StrokeOp::Blur { radius: 3.0 }, PaintTarget::Layer),
            (
                "sharpen",
                StrokeOp::Sharpen {
                    amount: 1.0,
                    radius: 2.0,
                },
                PaintTarget::Layer,
            ),
            (
                "healing",
                StrokeOp::Healing { softness: 4.0 },
                PaintTarget::Layer,
            ),
            ("spot healing", StrokeOp::SpotHealing, PaintTarget::Layer),
            (
                "smudge",
                StrokeOp::Smudge { strength: 0.6 },
                PaintTarget::Layer,
            ),
            (
                "mask refine",
                StrokeOp::RefineBoundary { strength: 0.6 },
                PaintTarget::Mask,
            ),
        ]
    }

    /// W4-B: a preview sample costs its own dabs, not the stroke so far. On a
    /// drag across eight tiles, every answer after the first — for the
    /// neighbourhood ops (blur, sharpen, the healing brushes, the mask
    /// refine) and smudge as much as for the brush — lists only tiles within
    /// the op's reach of that sample's new dabs, never a tile the stroke left
    /// behind, and never restarts the preview. An answer lists exactly the
    /// tiles it recomputed, so this pins the work per sample.
    #[test]
    fn every_answer_recomputes_only_the_tiles_within_reach_of_its_new_dabs() {
        for (name, op, target) in neighbourhood_ops() {
            let layer = LayerId::new();
            let mask = MaskId::new();
            let mut tiles = wide_store(layer, mask);
            let mut ctx = ToolContext::new(&mut tiles, WIDE).with_layer(layer);
            ctx.active_mask = Some(mask);
            ctx.paint_target = target;
            let mut tool = StrokeTool::new(
                ToolId::Brush,
                BrushSettings {
                    size: 16.0,
                    spacing: 0.1,
                    ..BrushSettings::default()
                },
                op,
            );
            tool.use_foreground = false;
            tool.clone.set_anchor(Vec2::new(20.0, 60.0));
            let reach = tool.neighbourhood_reach(target).unwrap_or(0);
            tool.on_pointer_down(&mut ctx, PointerEvent::at(20.0, 128.0))
                .unwrap();
            let mut answers = 0;
            let mut widest = 0;
            for i in 1..=100 {
                let before = tool.dabs().len();
                let x = 20.0 + i as f32 * 20.0;
                tool.on_pointer_move(&mut ctx, PointerEvent::at(x, 128.0))
                    .unwrap();
                let fresh = StrokeBuffer::bounds_of(&tool.dabs()[before..], WIDE);
                let Some(live) = tool.live_paint(&mut ctx).unwrap() else {
                    continue;
                };
                answers += 1;
                if i == 1 {
                    assert!(live.replace, "{name}: the first answer starts the preview");
                    continue;
                }
                assert!(!live.replace, "{name}: sample {i} restarted the preview");
                let fresh = fresh.expect("an answer comes with new dabs");
                let lo = (fresh.x - reach).div_euclid(TILE_SIZE as i64) as i32;
                let hi = (fresh.right() - 1 + reach).div_euclid(TILE_SIZE as i64) as i32;
                for (coord, _) in &live.tiles {
                    assert!(
                        (lo..=hi).contains(&coord.x),
                        "{name}: sample {i} (new dabs {fresh:?}, reach {reach}) \
                         recomputed tile {coord:?}"
                    );
                }
                widest = widest.max(live.tiles.len());
            }
            assert!(answers >= 90, "{name}: only {answers} answers");
            assert!(
                widest <= 3,
                "{name}: an answer recomputed {widest} tiles of a one-row stroke"
            );
            // The stroke did span the whole row: what stayed cheap was the
            // sample, not the stroke.
            tool.on_pointer_up(&mut ctx, PointerEvent::at(2020.0, 128.0))
                .unwrap();
            let commands = ctx.drain();
            let Some(Command::PaintTiles { delta, .. }) = commands.first() else {
                panic!("{name}: the release emitted {commands:?}");
            };
            assert!(
                delta.len() >= 7,
                "{name}: the release touched {} tiles",
                delta.len()
            );
        }
    }

    /// W4-B: a healing answer recomputes a tile its new dabs only come near.
    /// The spot heal's estimate for a covered pixel reads the coverage
    /// around it, so dabs that approach the seam from tile (0, 0) change the
    /// pixels an earlier leg covered in tile (1, 0) without touching that
    /// tile. The answer must list tile (1, 0), and the folded preview must
    /// still be what the release commits.
    #[test]
    fn a_heal_answer_recomputes_the_tiles_its_new_dabs_only_approach() {
        let layer = LayerId::new();
        let mask = MaskId::new();
        let mut tiles = store(layer, mask);
        let mut tool = StrokeTool::new(
            ToolId::SpotHealing,
            BrushSettings {
                size: 8.0,
                spacing: 0.1,
                ..BrushSettings::default()
            },
            StrokeOp::SpotHealing,
        );
        // Down the seam's right side (tile (1, 0) only), round the bottom,
        // and back up toward the seam on its left side (tile (0, 0) only).
        let path = [
            (262.0, 40.0),
            (262.0, 120.0),
            (262.0, 200.0),
            (200.0, 200.0),
            (247.0, 120.0),
        ];
        let mut preview = BTreeMap::new();
        let mut last = None;
        let commands = {
            let mut ctx = ToolContext::new(&mut tiles, CANVAS).with_layer(layer);
            tool.on_pointer_down(&mut ctx, PointerEvent::at(path[0].0, path[0].1))
                .unwrap();
            for (x, y) in &path[1..] {
                let before = tool.dabs().len();
                tool.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
                    .unwrap();
                let fresh = StrokeBuffer::bounds_of(&tool.dabs()[before..], CANVAS).unwrap();
                let live = tool.live_paint(&mut ctx).unwrap().expect("new dabs");
                last = Some((
                    fresh,
                    live.tiles.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
                ));
                fold(&mut preview, live);
            }
            let (x, y) = path[path.len() - 1];
            tool.on_pointer_up(&mut ctx, PointerEvent::at(x, y))
                .unwrap();
            ctx.drain()
        };
        let (fresh, listed) = last.unwrap();
        assert!(
            fresh.right() <= TILE_SIZE as i64,
            "the last leg's dabs must stay in tile (0, 0): {fresh:?}"
        );
        assert!(
            listed.contains(&TileCoord::new(1, 0, 0)),
            "the last answer did not recompute the tile its dabs approached: {listed:?}"
        );
        let Some(Command::PaintTiles { delta, .. }) = commands.first() else {
            panic!("the release emitted {commands:?}");
        };
        let mut committed = BTreeMap::new();
        for edit in delta.iter() {
            let tile = match edit.hash {
                Some(h) => LiveTile::Bytes(tiles.bytes(h).expect("committed blob").to_vec()),
                None => LiveTile::Cleared,
            };
            committed.insert(edit.coord, tile);
        }
        assert_eq!(committed.len(), 2);
        assert!(
            preview == committed,
            "the folded heal preview is not what the release committed"
        );
    }

    /// W4-B: the resumable smudge walk is the whole-patch walk. A stroke well
    /// inside the canvas, released without any preview, commits exactly what
    /// [`apply_smudge`] over the stroke's bounds produces.
    #[test]
    fn the_resumable_smudge_walk_commits_what_the_whole_patch_walk_does() {
        let layer = LayerId::new();
        let mask = MaskId::new();
        let mut tiles = wide_store(layer, mask);
        let mut expected_tiles = tiles.clone();
        let mut tool = StrokeTool::new(
            ToolId::Smudge,
            BrushSettings {
                size: 20.0,
                spacing: 0.1,
                ..BrushSettings::default()
            },
            StrokeOp::Smudge { strength: 0.7 },
        );
        let path = [(200.0, 60.0), (260.0, 90.0), (330.0, 120.0), (420.0, 100.0)];
        let (dabs, commands) = {
            let mut ctx = ToolContext::new(&mut tiles, WIDE).with_layer(layer);
            tool.on_pointer_down(&mut ctx, PointerEvent::at(path[0].0, path[0].1))
                .unwrap();
            for (x, y) in &path[1..] {
                tool.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
                    .unwrap();
            }
            let dabs = tool.dabs().to_vec();
            let (x, y) = path[path.len() - 1];
            tool.on_pointer_up(&mut ctx, PointerEvent::at(x, y))
                .unwrap();
            (dabs, ctx.drain())
        };
        let Command::PaintTiles { delta, .. } = &commands[0] else {
            panic!("the release emitted {commands:?}");
        };
        let key = PixelKey::Layer(layer);
        let rect = StrokeBuffer::bounds_of(&dabs, WIDE).unwrap();
        let mut patch = ColorPatch::load(&expected_tiles, key, rect).unwrap();
        apply_smudge(&mut patch, &dabs, rect, 0.7, 1.0, &Selection::default());
        let expected = patch.commit(&mut expected_tiles, key).unwrap();
        assert!(expected.len() >= 2, "the fixture stroke must cross a seam");
        assert_eq!(delta, &expected, "the resumable walk diverged");

        // The same stroke previewed after every sample — the walk resumed
        // once per answer — still commits the whole-patch walk.
        let mut tiles = wide_store(layer, mask);
        let mut ctx = ToolContext::new(&mut tiles, WIDE).with_layer(layer);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(path[0].0, path[0].1))
            .unwrap();
        for (x, y) in &path[1..] {
            tool.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
                .unwrap();
            assert!(tool.live_paint(&mut ctx).unwrap().is_some());
        }
        let (x, y) = path[path.len() - 1];
        tool.on_pointer_up(&mut ctx, PointerEvent::at(x, y))
            .unwrap();
        let commands = ctx.drain();
        let Some(Command::PaintTiles { delta, .. }) = commands.first() else {
            panic!("the previewed release emitted {commands:?}");
        };
        assert_eq!(delta, &expected, "resuming the walk per sample diverged");
    }

    /// W4-B: the windowed low-frequency estimate is the whole-plane one, for
    /// a window at least the estimate's reach from the plane's edges.
    #[test]
    fn the_windowed_heal_estimate_matches_the_whole_plane() {
        let (w, h) = (160u32, 150u32);
        let px: Vec<[f32; 4]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                [(x * 0.013).fract(), (y * 0.021).fract(), 0.3, 1.0]
            })
            .collect();
        let plane = FilterBuffer::from_pixels(w, h, px).unwrap();
        let covered: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as i64 - 80, (i / w) as i64 - 75);
                if x * x + y * y < 400 {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        let sigma = 1.0;
        let reach = heal_reach(sigma);
        let whole = low_frequency_outside(&plane, &covered, sigma).unwrap();
        let covered = StrokeBuffer {
            rect: PixelRect::new(0, 0, w, h),
            data: covered,
        };
        let window = PixelRect::new(reach, reach, w - 2 * reach as u32, h - 2 * reach as u32);
        let full = PixelRect::new(0, 0, w, h);
        let local =
            low_frequency_outside_at(&plane, IVec2::ZERO, &covered, sigma, window, full).unwrap();
        assert_eq!(local, crop(&whole, IVec2::ZERO, window).unwrap());
    }
}
