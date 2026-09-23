//! HDR Toning: local-adaptation tone mapping by a base/detail decomposition.
//!
//! # The algorithm
//!
//! Everything happens on **log2 luminance** (`L = log2(Y + 1/4096)`, `Y` the
//! Rec. 709 luminance of the linear pixel), because a tone mapper's job is to
//! rescale *ratios*: a stop is a stop wherever it sits.
//!
//! 1. Two edge-preserving smoothings of `L` are taken with a self-guided
//!    **guided filter** (He, Sun & Tang, 2010) — alpha-weighted box means, so
//!    transparent pixels contribute nothing. `B` is the *base* at the Edge
//!    Glow radius; `F` is a *fine* base at a tenth of it (at least one pixel,
//!    never more than the radius). The guided filter keeps a step whose local
//!    variance is large against its regulariser ([`BASE_EPSILON`], in stops²)
//!    and smooths texture that is small against it, which is what keeps the
//!    halo ("edge glow") around strong edges bounded.
//! 2. That splits `L` exactly into three bands:
//!    `L = B + (F − B) + (L − F)` — the base, the local contrast at the Edge
//!    Glow scale, and the fine detail.
//! 3. The output is
//!    `L' = g(B) + (1 + strength)·(F − B) + (1 + detail)·(L − F)`, where
//!    `g(b) = P + (b − P) / gamma + exposure` compresses (gamma > 1) or expands
//!    the base about middle grey `P = log2(0.18)` and shifts it by `exposure`
//!    stops.
//! 4. The pixel is rescaled by `Y' / Y` (`Y' = 2^L' − 1/4096`), which keeps
//!    its chromaticity; a black pixel, which has no ratio to keep, is lifted
//!    neutrally. `Y'` is floored at zero: a negative luminance is not a tone.
//! 5. Vibrance and saturation are the [`Vibrance`] adjustment on the result.
//!
//! With `strength = detail = 0`, `gamma = 1` and `exposure = 0` step 3 is
//! `L' = L` term for term, so the operator is the identity up to `f32`
//! rounding — `zero_strength_and_detail_is_the_identity` measures it through
//! the real spatial path, not through [`HdrToning::is_identity`]'s shortcut.
//!
//! # Per pixel
//!
//! [`HdrToning::apply`] is the neighbourhood-free form the dispatcher uses
//! (base = the pixel itself, so the two contrast bands are empty): gamma,
//! exposure, vibrance and saturation only. The radius, strength and detail are
//! honoured by [`HdrToning::apply_premultiplied_rgba_spatial`], which is what
//! Image ▸ Adjustments ▸ HDR Toning runs.

use color::ColorSpace;

use crate::color_ops::Vibrance;
use crate::error::{in_range, AdjustmentError};
use crate::space::{EncodedRgb, LinearRgb};

/// Largest Edge Glow radius, in layer pixels.
pub const MAX_HDR_RADIUS: f32 = 500.0;
/// Largest Edge Glow strength (local-contrast gain above 1).
pub const MAX_HDR_STRENGTH: f32 = 4.0;
/// Smallest base gamma.
pub const MIN_HDR_GAMMA: f32 = 0.1;
/// Largest base gamma.
pub const MAX_HDR_GAMMA: f32 = 5.0;
/// Largest exposure shift either way, in stops.
pub const MAX_HDR_EXPOSURE: f32 = 5.0;
/// Smallest detail gain (`-1` removes the fine band).
pub const MIN_HDR_DETAIL: f32 = -1.0;
/// Largest detail gain.
pub const MAX_HDR_DETAIL: f32 = 3.0;

/// The guided filter's regulariser for both bases, in stops². A step whose
/// local variance is well above it is kept in the base; texture well below it
/// goes to the contrast bands.
pub const BASE_EPSILON: f32 = 0.1;

/// Offset inside the logarithm, so black is a finite `-12` stops.
const LOG_FLOOR: f32 = 1.0 / 4096.0;

/// The fine base's radius as a fraction of the Edge Glow radius.
const FINE_FRACTION: f32 = 0.1;

/// Middle grey in log2 luminance, the pivot `gamma` turns about.
fn pivot() -> f32 {
    0.18f32.log2()
}

/// HDR Toning's validated parameters. See the [module docs](self).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrToning {
    radius: f32,
    strength: f32,
    gamma: f32,
    exposure: f32,
    detail: f32,
    color: Vibrance,
}

impl HdrToning {
    /// Changes nothing (the radius is kept at Photoshop's opening 15 px).
    pub const IDENTITY: Self = Self {
        radius: 15.0,
        strength: 0.0,
        gamma: 1.0,
        exposure: 0.0,
        detail: 0.0,
        color: Vibrance::IDENTITY,
    };

    /// Build one.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::OutOfRange`] / [`AdjustmentError::NotFinite`] for a
    /// parameter outside its documented range.
    pub fn new(
        radius: f32,
        strength: f32,
        gamma: f32,
        exposure: f32,
        detail: f32,
        vibrance: f32,
        saturation: f32,
    ) -> Result<Self, AdjustmentError> {
        Ok(Self {
            radius: in_range("radius", radius, 0.0, MAX_HDR_RADIUS)?,
            strength: in_range("strength", strength, 0.0, MAX_HDR_STRENGTH)?,
            gamma: in_range("gamma", gamma, MIN_HDR_GAMMA, MAX_HDR_GAMMA)?,
            exposure: in_range("exposure", exposure, -MAX_HDR_EXPOSURE, MAX_HDR_EXPOSURE)?,
            detail: in_range("detail", detail, MIN_HDR_DETAIL, MAX_HDR_DETAIL)?,
            color: Vibrance::new(vibrance, saturation)?,
        })
    }

    /// Edge Glow radius, layer pixels.
    pub fn radius(&self) -> f32 {
        self.radius
    }

    /// Edge Glow strength.
    pub fn strength(&self) -> f32 {
        self.strength
    }

    /// Base gamma.
    pub fn gamma(&self) -> f32 {
        self.gamma
    }

    /// Exposure, stops.
    pub fn exposure(&self) -> f32 {
        self.exposure
    }

    /// Fine-detail gain.
    pub fn detail(&self) -> f32 {
        self.detail
    }

    /// Vibrance, `-1..=1`.
    pub fn vibrance(&self) -> f32 {
        self.color.vibrance()
    }

    /// Saturation, `-1..=1`.
    pub fn saturation(&self) -> f32 {
        self.color.saturation()
    }

    /// The same settings at another radius (the dialog previews a
    /// downsampled proxy at a proportionally smaller one).
    pub fn with_radius(self, radius: f32) -> Self {
        Self {
            radius: if radius.is_finite() {
                radius.clamp(0.0, MAX_HDR_RADIUS)
            } else {
                self.radius
            },
            ..self
        }
    }

    /// Whether no setting changes a pixel: both contrast gains at zero,
    /// gamma 1, no exposure, no vibrance or saturation. The radius alone
    /// changes nothing.
    pub fn is_identity(&self) -> bool {
        self.strength == 0.0
            && self.detail == 0.0
            && self.gamma == 1.0
            && self.exposure == 0.0
            && self.color.is_identity()
    }

    /// `g(b)`: the base's global tone curve in log2 luminance.
    fn global(&self, base: f32) -> f32 {
        let p = pivot();
        p + (base - p) / self.gamma + self.exposure
    }

    /// Per pixel, with no neighbourhood: gamma, exposure, vibrance and
    /// saturation. See the [module docs](self).
    pub fn apply(&self, px: EncodedRgb, space: &ColorSpace) -> EncodedRgb {
        let lin = px.decode(space).get();
        let y = LinearRgb(lin).luminance();
        let l = log_luma(y);
        let out = retone(lin, y, self.global(l));
        self.color_pass(LinearRgb(out).encode(space))
    }

    fn color_pass(&self, enc: EncodedRgb) -> EncodedRgb {
        if self.color.is_identity() {
            enc
        } else {
            self.color.apply(enc)
        }
    }

    /// The whole operator over a `width × height` buffer of linear
    /// premultiplied RGBA, in place. Alpha is untouched and fully
    /// transparent pixels are skipped. It does **not** short-circuit on
    /// [`Self::is_identity`]: callers that want to skip an identity check it
    /// themselves, so the identity is something the maths is tested for.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::BufferShape`] when `pixels` is not `width × height`.
    pub fn apply_premultiplied_rgba_spatial(
        &self,
        pixels: &mut [[f32; 4]],
        width: usize,
        height: usize,
        space: &ColorSpace,
    ) -> Result<(), AdjustmentError> {
        if pixels.len() != width * height {
            return Err(AdjustmentError::BufferShape {
                pixels: pixels.len(),
                width,
                height,
            });
        }
        let n = pixels.len();
        let mut log = Vec::with_capacity(n);
        let mut weight = Vec::with_capacity(n);
        for px in pixels.iter() {
            if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                log.push(0.0);
                weight.push(0.0);
                continue;
            }
            let s = color::unpremultiply(*px);
            log.push(log_luma(LinearRgb([s[0], s[1], s[2]]).luminance()));
            weight.push(px[3]);
        }
        let base = guided_self(&log, &weight, width, height, self.radius, BASE_EPSILON);
        let fine_radius = (self.radius * FINE_FRACTION).max(1.0).min(self.radius);
        let fine = if fine_radius == self.radius {
            base.clone()
        } else {
            guided_self(&log, &weight, width, height, fine_radius, BASE_EPSILON)
        };
        for (i, px) in pixels.iter_mut().enumerate() {
            if weight[i] == 0.0 {
                continue;
            }
            let l_out = self.global(base[i])
                + (1.0 + self.strength) * (fine[i] - base[i])
                + (1.0 + self.detail) * (log[i] - fine[i]);
            let s = color::unpremultiply(*px);
            let lin = [s[0], s[1], s[2]];
            let y = LinearRgb(lin).luminance();
            let mut out = retone(lin, y, l_out);
            if !self.color.is_identity() {
                out = self
                    .color
                    .apply(LinearRgb(out).encode(space))
                    .decode(space)
                    .get();
            }
            *px = color::premultiply([out[0], out[1], out[2], px[3]]);
        }
        Ok(())
    }
}

/// `log2` of a luminance, floored so black is finite.
fn log_luma(y: f32) -> f32 {
    (y.max(0.0) + LOG_FLOOR).log2()
}

/// Rescale `rgb` (luminance `y`) to log2 luminance `l_out`, keeping its
/// chromaticity; a black pixel is lifted neutrally.
fn retone(rgb: [f32; 3], y: f32, l_out: f32) -> [f32; 3] {
    let y_out = (l_out.exp2() - LOG_FLOOR).max(0.0);
    let y0 = y.max(0.0);
    if y0 > 1e-6 {
        let k = y_out / y0;
        rgb.map(|c| c * k)
    } else {
        rgb.map(|c| c + (y_out - y0))
    }
}

/// Sums over a `(2r+1)²` window clamped to the plane, separably, with `f64`
/// running sums so a long row does not drift.
fn box_sum(src: &[f32], width: usize, height: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0.0f32; src.len()];
    for y in 0..height {
        let row = &src[y * width..(y + 1) * width];
        let out = &mut rows[y * width..(y + 1) * width];
        let mut acc = 0.0f64;
        for v in row.iter().take(r.min(width - 1) + 1) {
            acc += f64::from(*v);
        }
        for x in 0..width {
            out[x] = acc as f32;
            if x + r + 1 < width {
                acc += f64::from(row[x + r + 1]);
            }
            if x >= r {
                acc -= f64::from(row[x - r]);
            }
        }
    }
    let mut out = vec![0.0f32; src.len()];
    for x in 0..width {
        let mut acc = 0.0f64;
        for y in 0..=r.min(height - 1) {
            acc += f64::from(rows[y * width + x]);
        }
        for y in 0..height {
            out[y * width + x] = acc as f32;
            if y + r + 1 < height {
                acc += f64::from(rows[(y + r + 1) * width + x]);
            }
            if y >= r {
                acc -= f64::from(rows[(y - r) * width + x]);
            }
        }
    }
    out
}

/// The self-guided filter of `plane`, with every box mean weighted by
/// `weight` (alpha). A radius under half a pixel returns the plane itself.
fn guided_self(
    plane: &[f32],
    weight: &[f32],
    width: usize,
    height: usize,
    radius: f32,
    epsilon: f32,
) -> Vec<f32> {
    let r = radius.round() as usize;
    if r == 0 || plane.is_empty() {
        return plane.to_vec();
    }
    // Centre on the weighted mean so E[x²] − E[x]² does not cancel.
    let (sw, swx) = plane
        .iter()
        .zip(weight)
        .fold((0.0f64, 0.0f64), |(a, b), (x, w)| {
            (a + f64::from(*w), b + f64::from(*w) * f64::from(*x))
        });
    let centre = if sw > 0.0 { (swx / sw) as f32 } else { 0.0 };
    let c: Vec<f32> = plane.iter().map(|x| x - centre).collect();
    let den = box_sum(weight, width, height, r);
    let mean = |values: &[f32]| -> Vec<f32> {
        let weighted: Vec<f32> = values.iter().zip(weight).map(|(v, w)| v * w).collect();
        box_sum(&weighted, width, height, r)
            .iter()
            .zip(&den)
            .map(|(n, d)| if *d > 1e-6 { n / d } else { 0.0 })
            .collect()
    };
    let mean_i = mean(&c);
    let sq: Vec<f32> = c.iter().map(|v| v * v).collect();
    let mean_ii = mean(&sq);
    let mut a = Vec::with_capacity(c.len());
    let mut b = Vec::with_capacity(c.len());
    for (m, mm) in mean_i.iter().zip(&mean_ii) {
        let var = (mm - m * m).max(0.0);
        let ai = var / (var + epsilon);
        a.push(ai);
        b.push(m * (1.0 - ai));
    }
    let mean_a = mean(&a);
    let mean_b = mean(&b);
    c.iter()
        .zip(mean_a.iter().zip(&mean_b))
        .map(|(x, (ma, mb))| ma * x + mb + centre)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB: ColorSpace = ColorSpace::Srgb;

    /// A 64×64 linear test image: a left-to-right ramp from dark to light
    /// with a low-amplitude texture (±0.15 stop, period 8 px) on top.
    fn textured(w: usize, h: usize) -> Vec<[f32; 4]> {
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let ramp = -5.0 + 4.0 * x as f32 / (w - 1) as f32;
                let tex = 0.15 * ((x as f32 * 0.785).sin() * (y as f32 * 0.785).cos());
                let yv = (ramp + tex).exp2();
                out.push([yv * 1.1, yv, yv * 0.8, 1.0]);
            }
        }
        out
    }

    /// Two flat fields three stops apart (a step a tone mapper must keep)
    /// with the same low-amplitude texture on both.
    fn stepped(w: usize, h: usize) -> Vec<[f32; 4]> {
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let field = if x < w / 2 { -4.0 } else { -1.0 };
                let tex = 0.15 * ((x as f32 * 0.785).sin() * (y as f32 * 0.785).cos());
                let yv = (field + tex).exp2();
                out.push([yv * 1.1, yv, yv * 0.8, 1.0]);
            }
        }
        out
    }

    fn logs(px: &[[f32; 4]]) -> Vec<f32> {
        px.iter()
            .map(|p| log_luma(LinearRgb([p[0], p[1], p[2]]).luminance()))
            .collect()
    }

    /// Mean absolute deviation of log luminance from its own 9×9 box mean —
    /// the local contrast, measured independently of the filter under test.
    fn local_contrast(px: &[[f32; 4]], w: usize, h: usize) -> f32 {
        let l = logs(px);
        let r = 4i64;
        let mut total = 0.0;
        let mut n = 0.0;
        for y in r..h as i64 - r {
            for x in r..w as i64 - r {
                // Away from the step in `stepped`: the texture, not the edge.
                if (x - w as i64 / 2).abs() < 16 {
                    continue;
                }
                let mut s = 0.0;
                for dy in -r..=r {
                    for dx in -r..=r {
                        s += l[((y + dy) as usize) * w + (x + dx) as usize];
                    }
                }
                let m = s / ((2 * r + 1) * (2 * r + 1)) as f32;
                total += (l[y as usize * w + x as usize] - m).abs();
                n += 1.0;
            }
        }
        total / n
    }

    fn to8(v: f32) -> i32 {
        (color::from_linear(&SRGB, [v, v, v])[0].clamp(0.0, 1.0) * 255.0).round() as i32
    }

    #[test]
    fn zero_strength_and_detail_is_the_identity() {
        let (w, h) = (64, 64);
        let src = textured(w, h);
        for radius in [1.0, 6.0, 40.0] {
            let hdr = HdrToning::new(radius, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0).unwrap();
            assert!(hdr.is_identity());
            let mut out = src.clone();
            hdr.apply_premultiplied_rgba_spatial(&mut out, w, h, &SRGB)
                .unwrap();
            for (a, b) in src.iter().zip(&out) {
                for c in 0..3 {
                    assert!(
                        (to8(a[c]) - to8(b[c])).abs() <= 1,
                        "radius {radius}: {a:?} became {b:?}"
                    );
                    assert!((a[c] - b[c]).abs() <= a[c].abs() * 1e-3 + 1e-6);
                }
                assert_eq!(a[3], b[3]);
            }
        }
    }

    #[test]
    fn strength_increases_local_contrast() {
        let (w, h) = (96, 64);
        let src = stepped(w, h);
        let before = local_contrast(&src, w, h);
        let mut out = src.clone();
        HdrToning::new(8.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0)
            .unwrap()
            .apply_premultiplied_rgba_spatial(&mut out, w, h, &SRGB)
            .unwrap();
        let after = local_contrast(&out, w, h);
        assert!(
            after > before * 1.25,
            "local contrast {before} -> {after}: strength 1 did not raise it"
        );
        // And more strength, more contrast.
        let mut more = src.clone();
        HdrToning::new(8.0, 2.0, 1.0, 0.0, 0.0, 0.0, 0.0)
            .unwrap()
            .apply_premultiplied_rgba_spatial(&mut more, w, h, &SRGB)
            .unwrap();
        assert!(local_contrast(&more, w, h) > after);
    }

    #[test]
    fn detail_increases_fine_contrast_and_gamma_compresses_the_range() {
        let (w, h) = (96, 64);
        let src = stepped(w, h);
        let mut detailed = src.clone();
        HdrToning::new(30.0, 0.0, 1.0, 0.0, 2.0, 0.0, 0.0)
            .unwrap()
            .apply_premultiplied_rgba_spatial(&mut detailed, w, h, &SRGB)
            .unwrap();
        assert!(local_contrast(&detailed, w, h) > local_contrast(&src, w, h) * 1.2);

        let range = |px: &[[f32; 4]]| {
            let l = logs(px);
            l.iter().cloned().fold(f32::MIN, f32::max) - l.iter().cloned().fold(f32::MAX, f32::min)
        };
        let mut flat = src.clone();
        HdrToning::new(30.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0)
            .unwrap()
            .apply_premultiplied_rgba_spatial(&mut flat, w, h, &SRGB)
            .unwrap();
        assert!(range(&flat) < range(&src) * 0.8);
    }

    #[test]
    fn exposure_is_stops_and_transparent_pixels_are_skipped() {
        let mut px = vec![[0.1f32, 0.1, 0.1, 1.0], [0.0, 0.0, 0.0, 0.0]];
        HdrToning::new(5.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0)
            .unwrap()
            .apply_premultiplied_rgba_spatial(&mut px, 2, 1, &SRGB)
            .unwrap();
        assert!((px[0][0] - 0.2).abs() < 1e-3, "{:?}", px[0]);
        assert_eq!(px[1], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn bad_parameters_and_shapes_are_refused() {
        assert!(HdrToning::new(-1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0).is_err());
        assert!(HdrToning::new(1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0).is_err());
        assert!(HdrToning::new(1.0, f32::NAN, 1.0, 0.0, 0.0, 0.0, 0.0).is_err());
        let mut px = vec![[0.1f32; 4]; 3];
        assert!(matches!(
            HdrToning::IDENTITY.apply_premultiplied_rgba_spatial(&mut px, 2, 2, &SRGB),
            Err(AdjustmentError::BufferShape { .. })
        ));
    }
}
