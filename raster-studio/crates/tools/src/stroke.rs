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
use editor_core::{Command, PixelKey, Selection};
use filters::{blur::gaussian_blur, sharpen::unsharp_mask, EdgeMode, FilterBuffer};
use glam::{IVec2, Vec2};
use raster::PixelRect;
use serde::{Deserialize, Serialize};

use crate::brush::{BrushSettings, Dab, DabEmitter};
use crate::error::ToolError;
use crate::patch::{ColorPatch, CoveragePatch, MAX_PATCH_TILES};
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
            let mut px = Vec::with_capacity((w as usize) * (h as usize));
            for i in 0..(w as usize) * (h as usize) {
                let sf = src_full.pixels()[i];
                let sl = src_low.pixels()[i];
                let dl = dst_low.pixels()[i];
                px.push([
                    (sf[0] - sl[0] + dl[0]).max(0.0),
                    (sf[1] - sl[1] + dl[1]).max(0.0),
                    (sf[2] - sl[2] + dl[2]).max(0.0),
                    dl[3].clamp(0.0, 1.0).max(sf[3]),
                ]);
            }
            Prepared {
                aux: Some(FilterBuffer::from_pixels(w, h, px)?),
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

/// Composite a stroke's coverage plane onto a layer patch, once.
pub fn apply_stroke(
    patch: &mut ColorPatch,
    buf: &StrokeBuffer,
    op: &StrokeOp,
    sources: &StrokeSources<'_>,
    base_color: [f32; 4],
    opacity: f32,
    selection: &Selection,
) -> Result<(), ToolError> {
    let covered = coverage_over(patch, buf);
    let prep = prepare(op, patch, &covered, sources, base_color)?;
    let rect = buf.rect();
    let opacity = opacity.clamp(0.0, 1.0);
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
    let sigma = 0.5 + 2.5 * strength;
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
            const MID: f32 = 128.0 / 255.0;
            let d = blurred_v - MID;
            let graded = (MID + d * gain).clamp(0.0, 1.0);
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
    emitter: Option<DabEmitter>,
    /// The colour under the first sample, for the tolerance-driven ops.
    base_color: [f32; 4],
    offset: IVec2,
}

impl StrokeTool {
    pub fn new(id: ToolId, settings: BrushSettings, op: StrokeOp) -> Self {
        Self {
            id,
            settings,
            op,
            clone: CloneSource::default(),
            use_foreground: true,
            emitter: None,
            base_color: [0.0; 4],
            offset: IVec2::ZERO,
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
        let Some(rect) = StrokeBuffer::bounds_of(dabs, clip) else {
            return Ok(());
        };
        let buf = StrokeBuffer::rasterize(dabs, rect)?;

        let delta = match ctx.paint_target {
            PaintTarget::Mask => {
                if !self.op.works_on_mask() {
                    return Err(ToolError::UnsupportedOnMask);
                }
                let mut patch = CoveragePatch::load(ctx.tiles, key, rect)?;
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
                            &ctx.selection,
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
                            &ctx.selection,
                        );
                    }
                }
                patch.commit(ctx.tiles, key)?
            }
            PaintTarget::Layer => {
                let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
                if let StrokeOp::Smudge { strength } = self.op {
                    apply_smudge(
                        &mut patch,
                        dabs,
                        // `rect` is already `bounds_of(dabs, ctx.canvas)`: the
                        // dabs' union clipped to the document.
                        rect,
                        strength,
                        self.settings.opacity,
                        &ctx.selection,
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
                        Some(ColorPatch::load(ctx.tiles, src_key, src_rect)?)
                    } else {
                        None
                    };
                    let sources = StrokeSources {
                        source: source.as_ref(),
                        offset: self.offset,
                        pattern: ctx.pattern.as_ref(),
                    };
                    apply_stroke(
                        &mut patch,
                        &buf,
                        &self.op,
                        &sources,
                        self.base_color,
                        self.settings.opacity,
                        &ctx.selection,
                    )?;
                }
                patch.commit(ctx.tiles, key)?
            }
        };

        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }
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
    }

    fn is_active(&self) -> bool {
        self.emitter.is_some()
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

    /// Card 061: the tool-specific float options reach the op — the registry
    /// declares them, and a refused value is surfaced by the shell rather
    /// than silently ignored (an options-bar control that did nothing was the
    /// defect this seam exists to prevent). The brush-shared keys (size,
    /// hardness, spacing, opacity, ...) travel through `set_brush` instead.
    ///
    /// RECORDED dead controls (pre-existing, Choice/Bool keys): Dodge/Burn
    /// `range` and Sponge `mode` (Choice — the trait's default `set_choice`
    /// accepts and drops), CloneStamp/HealingBrush `aligned` (Bool —
    /// refused), and the selection tools' `mode`, Gradient `shape`, Type
    /// `font_family` (Choice no-ops). Wiring them is follow-up work; the
    /// registry contract test below keeps the float set from regrowing
    /// silently.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        let v = match setting {
            ToolSetting::Float(v) => v,
            _ => {
                return Err(ToolError::UnknownOption {
                    key: key.to_owned(),
                })
            }
        };
        match (key, &mut self.op) {
            ("strength", StrokeOp::RefineBoundary { strength }) => {
                *strength = v.clamp(0.0, 1.0);
                Ok(())
            }
            ("radius", StrokeOp::Blur { radius }) => {
                *radius = v.clamp(0.1, 64.0);
                Ok(())
            }
            ("amount", StrokeOp::Sharpen { amount, .. }) => {
                *amount = v.clamp(0.0, 4.0);
                Ok(())
            }
            ("tolerance", StrokeOp::ColorReplacement { tolerance, .. })
            | ("tolerance", StrokeOp::BackgroundErase { tolerance, .. }) => {
                *tolerance = v.clamp(0.0, 1.0);
                Ok(())
            }
            ("softness", StrokeOp::Healing { softness }) => {
                // The registry's Softness is a Gaussian sigma in PIXELS
                // (0.5..64, default 4) — not a 0..1 strength.
                *softness = v.clamp(0.5, 64.0);
                Ok(())
            }
            ("strength", StrokeOp::Smudge { strength }) => {
                *strength = v.clamp(0.0, 1.0);
                Ok(())
            }
            ("exposure", StrokeOp::Dodge { exposure, .. })
            | ("exposure", StrokeOp::Burn { exposure, .. }) => {
                *exposure = v.clamp(0.0, 1.0);
                Ok(())
            }
            ("amount", StrokeOp::Sponge { amount, .. }) => {
                *amount = v.clamp(0.0, 1.0);
                Ok(())
            }
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
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
