//! Geometric distortions.
//!
//! Every filter here is **inverse mapped**: for each destination pixel it
//! computes where in the source that pixel came from and resamples. The
//! forward direction — walking source pixels and scattering them into the
//! destination — is the classic mistake, and it leaves holes wherever the
//! transform stretches, because two neighbouring source pixels can land three
//! pixels apart.
//!
//! Resampling and boundary behaviour are the caller's [`Sampling`]:
//! [`crate::Interpolation::Bilinear`] and [`crate::EdgeMode::Clamp`] by
//! default. Because these are weighted averages of premultiplied pixels, all
//! of them preserve a constant image.
//!
//! Where a distortion has a natural inverse — [`twirl`], [`shear`] along one
//! axis, [`polar_coordinates`] — applying it and then its inverse returns the
//! original coordinate exactly, which the tests check.

use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::{fill_tiles, Sampling};

use core::f32::consts::{PI, TAU};

/// Drive a destination-to-source coordinate map over the whole buffer.
///
/// `f` receives the **centre** of a destination pixel and returns the
/// continuous source coordinate to sample.
fn remap<F>(src: &FilterBuffer, sampling: Sampling, f: F) -> FilterBuffer
where
    F: Fn(f32, f32) -> (f32, f32) + Sync,
{
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (sx, sy) = f(x as f32 + 0.5, y as f32 + 0.5);
        src.sample(sx, sy, sampling)
    });
    out
}

/// The geometric centre of a buffer, the default origin for the radial
/// distortions.
pub fn center_of(width: u32, height: u32) -> (f32, f32) {
    (width as f32 * 0.5, height as f32 * 0.5)
}

/// Pinch (or bulge) the image inside a circle.
///
/// Radii are scaled by `1 - amount * (1 - d)^2`, where `d` is the distance
/// from the centre as a fraction of `radius`. The quadratic falloff means the
/// scale is exactly `1` at the rim, so the effect blends into the untouched
/// surroundings with no visible seam — a linear falloff leaves a crease.
///
/// `amount` is clamped to `-0.99 ..= 0.99`; positive pinches inward
/// (magnifying the centre), negative bulges outward. A zero amount or a
/// non-positive radius is the identity.
pub fn pinch(
    src: &FilterBuffer,
    center: (f32, f32),
    radius: f32,
    amount: f32,
    sampling: Sampling,
) -> FilterBuffer {
    if src.is_empty() || !finite_positive(radius) || !amount.is_finite() || amount == 0.0 {
        return src.clone();
    }
    let a = amount.clamp(-0.99, 0.99);
    remap(src, sampling, move |x, y| {
        let (dx, dy) = (x - center.0, y - center.1);
        let r = (dx * dx + dy * dy).sqrt();
        if r >= radius || r == 0.0 {
            return (x, y);
        }
        let d = r / radius;
        let t = 1.0 - d;
        let scale = (1.0 - a * t * t).max(0.01);
        (center.0 + dx * scale, center.1 + dy * scale)
    })
}

/// Spherize: wrap the image onto (or into) a sphere inside a circle.
///
/// The radial map is `d' = d - amount * (sin(d * pi/2) - d)`, which fixes both
/// endpoints — `d' = 0` at the centre and `d' = 1` at the rim — so the lens
/// blends seamlessly into the untouched surroundings.
///
/// `amount` is clamped to `-1 ..= 1`; positive bulges towards the viewer,
/// negative dents inward. Zero, or a non-positive radius, is the identity.
pub fn spherize(
    src: &FilterBuffer,
    center: (f32, f32),
    radius: f32,
    amount: f32,
    sampling: Sampling,
) -> FilterBuffer {
    if src.is_empty() || !finite_positive(radius) || !amount.is_finite() || amount == 0.0 {
        return src.clone();
    }
    let a = amount.clamp(-1.0, 1.0);
    remap(src, sampling, move |x, y| {
        let (dx, dy) = (x - center.0, y - center.1);
        let r = (dx * dx + dy * dy).sqrt();
        if r >= radius || r == 0.0 {
            return (x, y);
        }
        let d = r / radius;
        let d2 = (d - a * ((d * PI * 0.5).sin() - d)).max(0.0);
        let k = d2 / d;
        (center.0 + dx * k, center.1 + dy * k)
    })
}

/// Twirl: rotate the image about a point, by an angle that falls to zero at
/// `radius`.
///
/// The rotation angle at distance `d` (as a fraction of `radius`) is
/// `angle * (1 - d)^2`. Because a rotation preserves distance from the centre,
/// the angle applied at a point is the same going forwards and backwards:
/// `twirl(a)` followed by `twirl(-a)` restores the original coordinate
/// exactly, which `twirl_map_is_exactly_invertible` checks.
///
/// A zero angle or a non-positive radius is the identity.
pub fn twirl(
    src: &FilterBuffer,
    center: (f32, f32),
    radius: f32,
    angle_deg: f32,
    sampling: Sampling,
) -> FilterBuffer {
    if src.is_empty() || !finite_positive(radius) || !angle_deg.is_finite() || angle_deg == 0.0 {
        return src.clone();
    }
    let angle = angle_deg.to_radians();
    remap(src, sampling, move |x, y| {
        twirl_map(center, radius, angle, x, y)
    })
}

/// The destination-to-source map behind [`twirl`], in radians.
///
/// Exposed to the tests so the "twirl then untwirl is the identity" claim can
/// be checked on the *map*, exactly, separately from the resampling error the
/// image round trip necessarily carries.
pub(crate) fn twirl_map(center: (f32, f32), radius: f32, angle: f32, x: f32, y: f32) -> (f32, f32) {
    let (dx, dy) = (x - center.0, y - center.1);
    let r = (dx * dx + dy * dy).sqrt();
    if r >= radius {
        return (x, y);
    }
    let t = 1.0 - r / radius;
    // Negated: we are asking where this destination pixel came *from*.
    let (s, c) = (-angle * t * t).sin_cos();
    (center.0 + dx * c - dy * s, center.1 + dx * s + dy * c)
}

/// Ripple: a small, high-frequency sinusoidal displacement in both axes.
///
/// `amount` is the peak displacement in pixels and `wavelength` its period.
/// Each axis is displaced by a sine of the *other* coordinate, which is what
/// makes the pattern look like disturbed water rather than a shear.
///
/// A zero amount or a non-positive wavelength is the identity.
pub fn ripple(
    src: &FilterBuffer,
    amount: f32,
    wavelength: f32,
    sampling: Sampling,
) -> FilterBuffer {
    if src.is_empty() || !finite_positive(wavelength) || !amount.is_finite() || amount == 0.0 {
        return src.clone();
    }
    let k = TAU / wavelength;
    remap(src, sampling, move |x, y| {
        (x + amount * (y * k).sin(), y + amount * (x * k).sin())
    })
}

/// Shear about the buffer centre.
///
/// `shear_x` displaces columns in proportion to their distance from the
/// horizontal centre line, `shear_y` does the same for rows. Shearing on one
/// axis is exactly invertible by shearing by the negative amount; shearing on
/// *both* axes at once is not its own inverse (the two operations do not
/// commute), so `shear(a, b)` followed by `shear(-a, -b)` only approximately
/// restores the image.
///
/// Zero on both axes is the identity.
pub fn shear(src: &FilterBuffer, shear_x: f32, shear_y: f32, sampling: Sampling) -> FilterBuffer {
    if src.is_empty()
        || !shear_x.is_finite()
        || !shear_y.is_finite()
        || (shear_x == 0.0 && shear_y == 0.0)
    {
        return src.clone();
    }
    let (cx, cy) = center_of(src.width(), src.height());
    remap(src, sampling, move |x, y| {
        (x - shear_x * (y - cy), y - shear_y * (x - cx))
    })
}

/// Waveform used by [`wave`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WaveKind {
    #[default]
    Sine,
    /// Linear ramps between the peaks — sharper crests than a sine.
    Triangle,
    /// Hard alternation between `+amplitude` and `-amplitude`; produces
    /// offset bands rather than a smooth wave.
    Square,
}

/// Parameters for [`wave`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Wave {
    pub kind: WaveKind,
    /// Peak displacement in pixels.
    pub amplitude: f32,
    /// Period in pixels. Must be positive.
    pub wavelength: f32,
    /// Phase offset in degrees.
    pub phase_deg: f32,
}

impl Default for Wave {
    fn default() -> Self {
        Self {
            kind: WaveKind::Sine,
            amplitude: 8.0,
            wavelength: 40.0,
            phase_deg: 0.0,
        }
    }
}

/// Displace the image along a repeating waveform.
///
/// Like [`ripple`] but with a selectable waveform and an explicit phase; the
/// horizontal displacement follows the vertical coordinate and vice versa.
///
/// A zero amplitude or a non-positive wavelength is the identity.
pub fn wave(src: &FilterBuffer, params: &Wave, sampling: Sampling) -> FilterBuffer {
    if src.is_empty()
        || !finite_positive(params.wavelength)
        || !params.amplitude.is_finite()
        || params.amplitude == 0.0
    {
        return src.clone();
    }
    let k = TAU / params.wavelength;
    let phase = params.phase_deg.to_radians();
    let amp = params.amplitude;
    let kind = params.kind;
    remap(src, sampling, move |x, y| {
        (
            x + amp * waveform(kind, y * k + phase),
            y + amp * waveform(kind, x * k + phase),
        )
    })
}

#[inline]
fn waveform(kind: WaveKind, t: f32) -> f32 {
    let s = t.sin();
    match kind {
        WaveKind::Sine => s,
        // asin(sin(t)) is a triangle wave of amplitude pi/2; rescale to 1.
        WaveKind::Triangle => s.clamp(-1.0, 1.0).asin() * (2.0 / PI),
        WaveKind::Square => {
            if s >= 0.0 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

/// Shape of the [`zigzag`] disturbance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ZigZagKind {
    /// Radial ripples that fade out towards the rim — a stone in a pond.
    #[default]
    PondRipples,
    /// Radial displacement at full strength all the way to the rim.
    OutFromCenter,
    /// Tangential displacement: the rings twist rather than move outward.
    AroundCenter,
}

/// Parameters for [`zigzag`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ZigZag {
    pub kind: ZigZagKind,
    pub center: (f32, f32),
    /// Outside this distance the image is untouched.
    pub radius: f32,
    /// Peak displacement in pixels.
    pub amount: f32,
    /// Number of half-waves between the centre and the rim.
    pub ridges: f32,
}

/// Concentric ripples about a point.
///
/// A zero amount, a non-positive radius, or a non-positive ridge count is the
/// identity. All three variants leave the image outside `radius` untouched.
pub fn zigzag(src: &FilterBuffer, params: &ZigZag, sampling: Sampling) -> FilterBuffer {
    if src.is_empty()
        || !finite_positive(params.radius)
        || !finite_positive(params.ridges)
        || !params.amount.is_finite()
        || params.amount == 0.0
    {
        return src.clone();
    }
    let (cx, cy) = params.center;
    let (radius, amount, ridges, kind) = (params.radius, params.amount, params.ridges, params.kind);
    remap(src, sampling, move |x, y| {
        let (dx, dy) = (x - cx, y - cy);
        let r = (dx * dx + dy * dy).sqrt();
        if r >= radius || r == 0.0 {
            return (x, y);
        }
        let d = r / radius;
        let phase = (ridges * PI * d).sin();
        match kind {
            ZigZagKind::PondRipples => {
                let dr = amount * phase * (1.0 - d);
                let k = ((r + dr) / r).max(0.0);
                (cx + dx * k, cy + dy * k)
            }
            ZigZagKind::OutFromCenter => {
                let dr = amount * phase * (1.0 - d) * (1.0 - d);
                let k = ((r + dr) / r).max(0.0);
                (cx + dx * k, cy + dy * k)
            }
            ZigZagKind::AroundCenter => {
                // Tangential: convert a pixel displacement into an angle.
                let dtheta = amount * phase * (1.0 - d) / r;
                let (s, c) = dtheta.sin_cos();
                (cx + dx * c - dy * s, cy + dx * s + dy * c)
            }
        }
    })
}

/// Direction of the [`polar_coordinates`] mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolarMode {
    /// Wrap the source rectangle into a disc. The source's *x* axis becomes
    /// the angle — one full turn across the source width, starting at twelve
    /// o'clock and going clockwise — and the source's *y* axis becomes the
    /// radius, so the top row of the source lands at the centre and the
    /// bottom row at the corners.
    RectangularToPolar,
    /// Unroll a disc into a rectangle: the exact inverse of
    /// [`PolarMode::RectangularToPolar`].
    PolarToRectangular,
}

/// The destination-to-source coordinate map behind [`polar_coordinates`].
///
/// Exposed to the tests because "these two modes are inverses" is a property
/// of the *maps*, provable exactly, rather than of the resampled images, where
/// interpolation blurs the answer.
pub(crate) fn polar_map(mode: PolarMode, width: f32, height: f32, x: f32, y: f32) -> (f32, f32) {
    let (cx, cy) = (width * 0.5, height * 0.5);
    // Half the diagonal, so the radial axis reaches every corner.
    let rmax = 0.5 * (width * width + height * height).sqrt();
    match mode {
        PolarMode::RectangularToPolar => {
            let (dx, dy) = (x - cx, y - cy);
            let r = (dx * dx + dy * dy).sqrt();
            // atan2(dx, -dy): zero at twelve o'clock, increasing clockwise.
            let mut theta = dx.atan2(-dy);
            if theta < 0.0 {
                theta += TAU;
            }
            (theta / TAU * width, r / rmax * height)
        }
        PolarMode::PolarToRectangular => {
            let theta = x / width * TAU;
            let r = y / height * rmax;
            let (s, c) = theta.sin_cos();
            (cx + r * s, cy - r * c)
        }
    }
}

/// Convert between rectangular and polar layouts.
///
/// Boundary handling matters here more than anywhere else: with
/// [`crate::EdgeMode::Wrap`] on the angular axis the seam at twelve o'clock is
/// invisible, while [`crate::EdgeMode::Clamp`] leaves a visible join. The
/// caller chooses.
pub fn polar_coordinates(src: &FilterBuffer, mode: PolarMode, sampling: Sampling) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = (src.width() as f32, src.height() as f32);
    remap(src, sampling, move |x, y| polar_map(mode, w, h, x, y))
}

#[inline]
fn finite_positive(v: f32) -> bool {
    v.is_finite() && v > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{EdgeMode, Interpolation};

    const CONST_PX: [f32; 4] = [0.21, 0.34, 0.55, 0.8];

    fn constant(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, CONST_PX).unwrap()
    }

    /// A plane that is *linear* in x and y. Bilinear interpolation reproduces
    /// such a function exactly, so any error in a round-trip test is the
    /// coordinate maths, not the resampler.
    fn linear_field(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.push([
                    0.1 + x as f32 * 0.003,
                    0.2 + y as f32 * 0.004,
                    0.3 + (x + y) as f32 * 0.001,
                    1.0,
                ]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    fn bilinear() -> Sampling {
        Sampling::new(EdgeMode::Clamp, Interpolation::Bilinear)
    }

    fn assert_constant(buf: &FilterBuffer, what: &str) {
        for (i, px) in buf.pixels().iter().enumerate() {
            for c in 0..4 {
                assert!(
                    (px[c] - CONST_PX[c]).abs() < 1e-5,
                    "{what}: pixel {i} channel {c}: {px:?}"
                );
            }
        }
    }

    fn all_distorts(src: &FilterBuffer, s: Sampling) -> Vec<(&'static str, FilterBuffer)> {
        let c = center_of(src.width(), src.height());
        vec![
            ("pinch", pinch(src, c, 20.0, 0.6, s)),
            ("spherize", spherize(src, c, 20.0, 0.7, s)),
            ("twirl", twirl(src, c, 20.0, 75.0, s)),
            ("ripple", ripple(src, 3.0, 11.0, s)),
            ("shear", shear(src, 0.4, 0.2, s)),
            ("wave", wave(src, &Wave::default(), s)),
            (
                "zigzag",
                zigzag(
                    src,
                    &ZigZag {
                        kind: ZigZagKind::PondRipples,
                        center: c,
                        radius: 20.0,
                        amount: 4.0,
                        ridges: 5.0,
                    },
                    s,
                ),
            ),
            (
                "polar",
                polar_coordinates(src, PolarMode::RectangularToPolar, s),
            ),
        ]
    }

    #[test]
    fn every_distort_preserves_a_constant_image() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            for interp in [
                Interpolation::Nearest,
                Interpolation::Bilinear,
                Interpolation::Bicubic,
            ] {
                let s = Sampling::new(edge, interp);
                let src = constant(31, 23);
                for (name, out) in all_distorts(&src, s) {
                    assert_constant(&out, &format!("{name} {edge:?}/{interp:?}"));
                }
            }
        }
    }

    /// The twirl *map* is exactly invertible: rotation preserves distance from
    /// the centre, so the angle applied at a point is the same in both
    /// directions. Checked on the coordinates, where the only error is f32
    /// rounding.
    #[test]
    fn twirl_map_is_exactly_invertible() {
        let c = (20.5f32, 20.5);
        let angle = 65f32.to_radians();
        for iy in 0..41 {
            for ix in 0..41 {
                let (x, y) = (ix as f32 + 0.5, iy as f32 + 0.5);
                let (ux, uy) = twirl_map(c, 18.0, angle, x, y);
                let (bx, by) = twirl_map(c, 18.0, -angle, ux, uy);
                assert!((bx - x).abs() < 1e-4, "x at {x},{y}: {bx}");
                assert!((by - y).abs() < 1e-4, "y at {x},{y}: {by}");
            }
        }
    }

    /// The same round trip on the image. The residual here is resampling
    /// error, not map error: the intermediate buffer holds point samples of a
    /// rotated field, and bilinear reconstruction of a *curved* field is
    /// approximate. A sub-thousandth error on a field whose channels span 0.12
    /// is well under a quantisation step of 8-bit output.
    #[test]
    fn twirl_round_trips_through_resampling() {
        let src = linear_field(41, 41);
        let c = center_of(41, 41);
        let there = twirl(&src, c, 18.0, 65.0, bilinear());
        let back = twirl(&there, c, 18.0, -65.0, bilinear());
        assert_ne!(there, src, "the twirl must actually do something");
        // Only check away from the buffer edge, where clamping is in play.
        for y in 4..37u32 {
            for x in 4..37u32 {
                let a = back.get(x, y);
                let b = src.get(x, y);
                for ch in 0..4 {
                    assert!((a[ch] - b[ch]).abs() < 1e-3, "at {x},{y}: {a:?} vs {b:?}");
                }
            }
        }
    }

    /// Pins the half-pixel convention shared by `remap` and the resampler.
    ///
    /// `remap` evaluates the inverse map at the destination pixel's *centre*
    /// (`x + 0.5`), and `sample_bilinear` treats an integer-plus-a-half
    /// coordinate as landing exactly on a pixel. The two conventions have to
    /// agree, and this is the only test that can tell. A `shear_x` of 1.0 on a
    /// buffer of **odd** height puts the centre line at a half-integer, so
    /// `x + 0.5 - (y + 0.5 - 4.5)` is `x - y + 4.5` — another pixel centre.
    /// Every destination pixel therefore lands exactly on a source pixel and
    /// the output must be those pixels *copied*, bit for bit, with no
    /// interpolation whatsoever.
    ///
    /// (An even height would defeat the test: the two halves cancel and the
    /// mapping lands on a pixel corner even when the code is right.)
    ///
    /// Drop the `+ 0.5` from `remap` and the vertical coordinate lands on a
    /// row boundary instead, averaging two rows — visible here as soon as the
    /// source has sharp per-pixel detail. A round-trip test cannot catch this:
    /// a consistent half-pixel error cancels on the way back.
    #[test]
    fn shear_by_whole_pixels_copies_without_resampling() {
        // Every pixel distinct and adjacent pixels far apart, so any blending
        // with a neighbour shows up immediately.
        let (w, h) = (8u32, 9u32);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = ((x * 7 + y * 13) % 8) as f32 / 8.0 + 0.05;
                px.push([v, 1.0 - v, (x % 2) as f32 * 0.9, 1.0]);
            }
        }
        let src = FilterBuffer::from_pixels(w, h, px).unwrap();

        // cy = 4.5, so dest centre (x+0.5, y+0.5) maps to (x - y + 4.5, y+0.5):
        // a per-row shift by the whole number `4 - y`, sampled dead centre.
        let out = shear(&src, 1.0, 0.0, bilinear());
        for y in 0..h {
            for x in 0..w {
                let sx = (x as i64 - y as i64 + 4).clamp(0, w as i64 - 1) as u32;
                assert_eq!(
                    out.get(x, y),
                    src.get(sx, y),
                    "({x},{y}) should be an exact copy of source ({sx},{y})"
                );
            }
        }
        // And it really is a shear, not an accidental identity.
        assert_ne!(out, src);
    }

    /// A one-axis shear is exactly invertible by its negation.
    #[test]
    fn single_axis_shear_round_trips() {
        let src = linear_field(48, 32);
        let there = shear(&src, 0.35, 0.0, bilinear());
        let back = shear(&there, -0.35, 0.0, bilinear());
        for y in 2..30u32 {
            for x in 8..40u32 {
                let a = back.get(x, y);
                let b = src.get(x, y);
                for ch in 0..4 {
                    assert!((a[ch] - b[ch]).abs() < 1e-4, "at {x},{y}: {a:?} vs {b:?}");
                }
            }
        }
    }

    /// The two polar modes are exact inverses as coordinate maps. Proving it
    /// on the maps rather than on resampled images makes the tolerance tight.
    #[test]
    fn polar_modes_are_inverse_coordinate_maps() {
        let (w, h) = (64.0f32, 48.0f32);
        for iy in 0..48 {
            for ix in 0..64 {
                let (x, y) = (ix as f32 + 0.5, iy as f32 + 0.5);
                let (u, v) = polar_map(PolarMode::RectangularToPolar, w, h, x, y);
                let (bx, by) = polar_map(PolarMode::PolarToRectangular, w, h, u, v);
                assert!((bx - x).abs() < 2e-3, "x at {x},{y}: {bx}");
                assert!((by - y).abs() < 2e-3, "y at {x},{y}: {by}");
            }
        }
    }

    #[test]
    fn polar_map_puts_the_angular_origin_at_twelve_oclock() {
        let (w, h) = (100.0f32, 100.0f32);
        // Straight up from the centre must be angle 0, i.e. source x = 0.
        let (u, v) = polar_map(PolarMode::RectangularToPolar, w, h, 50.0, 20.0);
        assert!(u.abs() < 1e-3, "angle {u}");
        assert!(v > 0.0, "radius {v}");
        // A quarter turn clockwise (to the right) must be a quarter of the
        // width.
        let (u, _) = polar_map(PolarMode::RectangularToPolar, w, h, 80.0, 50.0);
        assert!((u - 25.0).abs() < 1e-3, "angle {u}");
    }

    /// Pinch with a positive amount magnifies the centre: the source radius it
    /// reads from is smaller than the destination radius.
    #[test]
    fn pinch_magnifies_the_centre_and_leaves_the_rim_alone() {
        let src = linear_field(64, 64);
        let c = center_of(64, 64);
        let out = pinch(&src, c, 24.0, 0.8, bilinear());
        // Outside the radius nothing moved.
        for x in 0..8u32 {
            assert_eq!(out.get(x, 2), src.get(x, 2), "rim moved at {x}");
        }
        // Inside it, the red channel (which grows with x) at a point right of
        // centre must read a value from closer to the centre, i.e. smaller.
        let probe_x = 44u32;
        assert!(
            out.get(probe_x, 32)[0] < src.get(probe_x, 32)[0] - 1e-4,
            "{} vs {}",
            out.get(probe_x, 32)[0],
            src.get(probe_x, 32)[0]
        );
        // A negative amount goes the other way.
        let bulge = pinch(&src, c, 24.0, -0.8, bilinear());
        assert!(bulge.get(probe_x, 32)[0] > src.get(probe_x, 32)[0] + 1e-4);
    }

    #[test]
    fn spherize_is_continuous_at_the_rim() {
        let src = linear_field(64, 64);
        let c = center_of(64, 64);
        let out = spherize(&src, c, 20.0, 1.0, bilinear());
        // Just inside and just outside the rim must agree closely; a map that
        // does not fix d = 1 leaves a visible ring here.
        let inside = out.get(51, 32)[0];
        let outside = out.get(53, 32)[0];
        assert!((inside - outside).abs() < 0.02, "{inside} vs {outside}");
    }

    #[test]
    fn zero_amounts_are_the_identity() {
        let src = linear_field(24, 18);
        let s = bilinear();
        let c = center_of(24, 18);
        assert_eq!(pinch(&src, c, 10.0, 0.0, s), src);
        assert_eq!(pinch(&src, c, 0.0, 0.5, s), src);
        assert_eq!(spherize(&src, c, 10.0, 0.0, s), src);
        assert_eq!(twirl(&src, c, 10.0, 0.0, s), src);
        assert_eq!(ripple(&src, 0.0, 10.0, s), src);
        assert_eq!(ripple(&src, 4.0, 0.0, s), src);
        assert_eq!(shear(&src, 0.0, 0.0, s), src);
        assert_eq!(
            wave(
                &src,
                &Wave {
                    amplitude: 0.0,
                    ..Wave::default()
                },
                s
            ),
            src
        );
        let z = ZigZag {
            kind: ZigZagKind::PondRipples,
            center: c,
            radius: 8.0,
            amount: 0.0,
            ridges: 4.0,
        };
        assert_eq!(zigzag(&src, &z, s), src);
        assert_eq!(
            zigzag(
                &src,
                &ZigZag {
                    amount: 3.0,
                    ridges: 0.0,
                    ..z
                },
                s
            ),
            src
        );
    }

    #[test]
    fn waveforms_have_the_right_shape() {
        // Sine: peaks at a quarter period.
        assert!((waveform(WaveKind::Sine, PI * 0.5) - 1.0).abs() < 1e-6);
        // Triangle: same peak, but linear in between — a quarter of the way
        // up the rising edge is a quarter of the amplitude.
        assert!((waveform(WaveKind::Triangle, PI * 0.5) - 1.0).abs() < 1e-6);
        assert!((waveform(WaveKind::Triangle, PI * 0.125) - 0.25).abs() < 1e-5);
        assert!((waveform(WaveKind::Sine, PI * 0.125) - 0.25).abs() > 0.1);
        // Square: only ever +-1.
        for i in 0..64 {
            let v = waveform(WaveKind::Square, i as f32 * 0.19);
            assert!(v == 1.0 || v == -1.0, "{v}");
        }
    }

    #[test]
    fn zigzag_variants_move_pixels_in_different_ways() {
        let src = linear_field(64, 64);
        let c = center_of(64, 64);
        let base = ZigZag {
            kind: ZigZagKind::PondRipples,
            center: c,
            radius: 28.0,
            amount: 5.0,
            ridges: 4.0,
        };
        let a = zigzag(&src, &base, bilinear());
        let b = zigzag(
            &src,
            &ZigZag {
                kind: ZigZagKind::OutFromCenter,
                ..base
            },
            bilinear(),
        );
        let d = zigzag(
            &src,
            &ZigZag {
                kind: ZigZagKind::AroundCenter,
                ..base
            },
            bilinear(),
        );
        assert_ne!(a, src);
        assert_ne!(a, b);
        assert_ne!(a, d);
        // All three leave the outside untouched.
        for out in [&a, &b, &d] {
            assert_eq!(out.get(1, 1), src.get(1, 1));
        }
    }

    #[test]
    fn distorts_survive_one_pixel_empty_and_absurd_inputs() {
        let s = bilinear();
        let one = constant(1, 1);
        let c = center_of(1, 1);
        assert_constant(&pinch(&one, c, 5.0, 0.9, s), "1x1 pinch");
        assert_constant(&spherize(&one, c, 5.0, 0.9, s), "1x1 spherize");
        assert_constant(&twirl(&one, c, 5.0, 180.0, s), "1x1 twirl");
        assert_constant(&ripple(&one, 9.0, 2.0, s), "1x1 ripple");
        assert_constant(&shear(&one, 3.0, 3.0, s), "1x1 shear");
        assert_constant(&wave(&one, &Wave::default(), s), "1x1 wave");
        assert_constant(
            &polar_coordinates(&one, PolarMode::PolarToRectangular, s),
            "1x1 polar",
        );

        let empty = FilterBuffer::transparent(6, 0).unwrap();
        assert!(pinch(&empty, c, 4.0, 0.5, s).is_empty());
        assert!(twirl(&empty, c, 4.0, 30.0, s).is_empty());
        assert!(polar_coordinates(&empty, PolarMode::RectangularToPolar, s).is_empty());

        // Non-finite parameters must be rejected, not propagated into NaN
        // coordinates.
        let src = linear_field(8, 8);
        for v in [f32::NAN, f32::INFINITY, -1.0] {
            assert_eq!(pinch(&src, c, v, 0.5, s), src, "radius {v}");
            assert_eq!(twirl(&src, c, v, 30.0, s), src, "radius {v}");
            assert_eq!(ripple(&src, 3.0, v, s), src, "wavelength {v}");
        }
        for v in [f32::NAN, f32::INFINITY] {
            assert_eq!(shear(&src, v, 0.0, s), src);
            assert_eq!(twirl(&src, c, 4.0, v, s), src);
        }
    }

    /// A destination pixel is always written, whatever the map does — the
    /// property forward mapping cannot offer.
    #[test]
    fn inverse_mapping_leaves_no_holes() {
        // A strong zoom-out: forward mapping here would scatter one source
        // pixel to every ninth destination pixel and leave the rest blank.
        let src = FilterBuffer::filled(33, 33, [0.5, 0.5, 0.5, 1.0]).unwrap();
        let out = remap(&src, bilinear(), |x, y| (x * 3.0 - 33.0, y * 3.0 - 33.0));
        for (i, px) in out.pixels().iter().enumerate() {
            assert!(px[3] > 0.0, "hole at pixel {i}");
        }
    }
}
