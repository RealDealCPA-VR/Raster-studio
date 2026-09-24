//! Filter ▸ Blur Gallery: Field, Iris, Tilt-Shift, Path and Spin blur (W9-O).
//!
//! Three of the five are **spatially varying Gaussian blurs**: a per-pixel
//! blur amount (a Gaussian sigma, in image pixels) is computed from the
//! blur's geometry, and the result at each pixel is interpolated between a
//! small stack of uniformly blurred copies of the source whose sigmas bracket
//! that pixel's amount ([`variable_blur`]).
//!
//! * [`FieldBlur`]: pins, each with its own amount; between pins the amount
//!   is inverse-distance-squared (Shepard) interpolated.
//! * [`IrisBlur`]: an ellipse; sharp inside `focus` (a fraction of the
//!   radii), ramping smoothly to the full amount at the ellipse's rim, full
//!   blur outside.
//! * [`TiltShiftBlur`]: a sharp band through a centre at an angle, `focus`
//!   pixels either side of its centre line, then a `transition` ramp to the
//!   full amount. `distortion` adds a directional streak to the blurred
//!   region: along the band normal when positive, along the band when
//!   negative, below the band only unless `symmetric`.
//!
//! The other two are line integrals:
//!
//! * [`PathBlur`]: a motion blur whose direction at each pixel follows a
//!   drawn polyline — the inverse-distance-squared weighted mean of the
//!   segments' directions — over `speed` pixels, centred on the pixel.
//! * [`SpinBlur`]: a rotational blur around a centre, sweeping `angle_deg`
//!   (centred on the pixel) inside an ellipse, fading to nothing over the
//!   outer `feather` fraction of the radii.
//!
//! Every blur is a weighted average with weights summing to one, so a flat
//! image stays flat. A blur of zero (no pins, zero amounts, a path of fewer
//! than two points, a zero angle) returns the source bit for bit.
//!
//! Coordinates are image pixels with `(0.5, 0.5)` the centre of the top-left
//! pixel, as in [`FilterBuffer::sample_bilinear`]; angles are degrees,
//! clockwise on screen (y grows downwards).

use rayon::prelude::*;

use crate::blur::{box_blur, gaussian_blur};
use crate::buffer::FilterBuffer;
use crate::support::{accumulate, fill_tiles, EdgeMode, Sampling};

/// The largest blur amount (Gaussian sigma, image pixels) any gallery blur
/// takes; larger values are clamped.
pub const MAX_GALLERY_BLUR: f32 = 256.0;
/// The largest Path Blur streak, in image pixels.
pub const MAX_PATH_SPEED: f32 = 1000.0;
/// The largest Spin Blur sweep, in degrees.
pub const MAX_SPIN_ANGLE: f32 = 360.0;
/// The most points a Path Blur path keeps.
pub const MAX_PATH_POINTS: usize = 64;
/// The most pins a Field Blur keeps.
pub const MAX_FIELD_PINS: usize = 32;

/// Sigmas of the blur stack [`variable_blur`] interpolates across. The first
/// is the untouched source.
const LEVELS: [f32; 10] = [0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0];
/// Above this sigma a level is three box passes rather than a true Gaussian.
const BOX_ABOVE: f32 = 8.0;
/// The most taps a line-integral blur (Path, Spin, distortion) averages.
const MAX_TAPS: u32 = 256;

/// Which of the five gallery blurs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum BlurGalleryKind {
    Field,
    Iris,
    TiltShift,
    Path,
    Spin,
}

impl BlurGalleryKind {
    /// Every kind, in Photopea's menu order.
    pub const ALL: [BlurGalleryKind; 5] = [
        BlurGalleryKind::Field,
        BlurGalleryKind::Iris,
        BlurGalleryKind::TiltShift,
        BlurGalleryKind::Path,
        BlurGalleryKind::Spin,
    ];

    /// The menu row's label.
    pub const fn label(self) -> &'static str {
        match self {
            BlurGalleryKind::Field => "Field Blur…",
            BlurGalleryKind::Iris => "Iris Blur…",
            BlurGalleryKind::TiltShift => "Tilt-Shift…",
            BlurGalleryKind::Path => "Path Blur…",
            BlurGalleryKind::Spin => "Spin Blur…",
        }
    }
}

/// One Field Blur pin.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FieldPin {
    pub pos: [f32; 2],
    /// Gaussian sigma at the pin, image pixels.
    pub blur: f32,
}

/// Field Blur: pins with amounts, interpolated between.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct FieldBlur {
    pub pins: Vec<FieldPin>,
}

/// Iris Blur: sharp inside an ellipse, blurred outside.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct IrisBlur {
    pub center: [f32; 2],
    /// The ellipse's half-axes, image pixels.
    pub radii: [f32; 2],
    pub rotation_deg: f32,
    /// Where the sharp region ends, as a fraction of the radii (`0..1`).
    pub focus: f32,
    /// Gaussian sigma outside the ellipse, image pixels.
    pub blur: f32,
}

/// Tilt-Shift: a sharp band, blurred either side.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TiltShiftBlur {
    pub center: [f32; 2],
    /// The band's direction, degrees clockwise from +x.
    pub angle_deg: f32,
    /// Half-width of the sharp band, image pixels.
    pub focus: f32,
    /// Width of the ramp from sharp to full blur, image pixels.
    pub transition: f32,
    /// Gaussian sigma past the ramp, image pixels.
    pub blur: f32,
    /// `-1..1`: a streak along the band normal (positive) or along the band
    /// (negative), proportional to the local blur.
    pub distortion: f32,
    /// Distort both sides of the band, not only the side below it.
    pub symmetric: bool,
}

/// Path Blur: motion along a drawn path.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PathBlur {
    pub points: Vec<[f32; 2]>,
    /// Streak length, image pixels.
    pub speed: f32,
}

/// Spin Blur: rotation around a centre inside an ellipse.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SpinBlur {
    pub center: [f32; 2],
    pub radii: [f32; 2],
    pub rotation_deg: f32,
    /// The outer fraction of the radii over which the spin fades out (`0..1`).
    pub feather: f32,
    /// Total sweep, degrees.
    pub angle_deg: f32,
}

/// One configured gallery blur.
#[derive(Clone, PartialEq, Debug)]
pub enum BlurGallery {
    Field(FieldBlur),
    Iris(IrisBlur),
    TiltShift(TiltShiftBlur),
    Path(PathBlur),
    Spin(SpinBlur),
}

fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

fn clamp_amount(v: f32) -> f32 {
    finite_or(v, 0.0).clamp(0.0, MAX_GALLERY_BLUR)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The normalised elliptical distance of `p`: 1 on the rim.
fn ellipse_distance(p: [f32; 2], center: [f32; 2], radii: [f32; 2], rotation_deg: f32) -> f32 {
    let (s, c) = rotation_deg.to_radians().sin_cos();
    let dx = p[0] - center[0];
    let dy = p[1] - center[1];
    let u = dx * c + dy * s;
    let v = -dx * s + dy * c;
    let rx = radii[0].abs().max(1e-3);
    let ry = radii[1].abs().max(1e-3);
    ((u / rx).powi(2) + (v / ry).powi(2)).sqrt()
}

impl BlurGallery {
    /// The kind's default, laid out on a `width x height` image.
    pub fn default_for(kind: BlurGalleryKind, width: u32, height: u32) -> Self {
        let (w, h) = (width as f32, height as f32);
        let center = [w * 0.5, h * 0.5];
        let short = w.min(h).max(1.0);
        match kind {
            BlurGalleryKind::Field => BlurGallery::Field(FieldBlur {
                pins: vec![FieldPin {
                    pos: center,
                    blur: 15.0,
                }],
            }),
            BlurGalleryKind::Iris => BlurGallery::Iris(IrisBlur {
                center,
                radii: [short * 0.3, short * 0.3],
                rotation_deg: 0.0,
                focus: 0.5,
                blur: 15.0,
            }),
            BlurGalleryKind::TiltShift => BlurGallery::TiltShift(TiltShiftBlur {
                center,
                angle_deg: 0.0,
                focus: h * 0.1,
                transition: h * 0.15,
                blur: 15.0,
                distortion: 0.0,
                symmetric: false,
            }),
            BlurGalleryKind::Path => BlurGallery::Path(PathBlur {
                points: vec![[w * 0.25, h * 0.5], [w * 0.75, h * 0.5]],
                speed: 50.0,
            }),
            BlurGalleryKind::Spin => BlurGallery::Spin(SpinBlur {
                center,
                radii: [short * 0.3, short * 0.3],
                rotation_deg: 0.0,
                feather: 0.2,
                angle_deg: 15.0,
            }),
        }
    }

    pub fn kind(&self) -> BlurGalleryKind {
        match self {
            BlurGallery::Field(_) => BlurGalleryKind::Field,
            BlurGallery::Iris(_) => BlurGalleryKind::Iris,
            BlurGallery::TiltShift(_) => BlurGalleryKind::TiltShift,
            BlurGallery::Path(_) => BlurGalleryKind::Path,
            BlurGallery::Spin(_) => BlurGalleryKind::Spin,
        }
    }

    /// Whether [`BlurGallery::apply`] returns its source untouched.
    pub fn is_identity(&self) -> bool {
        match self {
            BlurGallery::Field(f) => f.pins.iter().all(|p| clamp_amount(p.blur) <= 0.0),
            BlurGallery::Iris(i) => clamp_amount(i.blur) <= 0.0,
            BlurGallery::TiltShift(t) => clamp_amount(t.blur) <= 0.0,
            BlurGallery::Path(p) => {
                p.points.len() < 2 || finite_or(p.speed, 0.0) <= 0.0 || !has_extent(&p.points)
            }
            BlurGallery::Spin(s) => finite_or(s.angle_deg, 0.0) == 0.0,
        }
    }

    /// The same blur for an image `k` times the size: positions, lengths and
    /// amounts scale, angles and fractions do not. How a bounded preview
    /// shows what the full-resolution apply will do.
    pub fn scaled(&self, k: f32) -> Self {
        let k = if k.is_finite() && k > 0.0 { k } else { 1.0 };
        let p = |q: [f32; 2]| [q[0] * k, q[1] * k];
        match self {
            BlurGallery::Field(f) => BlurGallery::Field(FieldBlur {
                pins: f
                    .pins
                    .iter()
                    .map(|pin| FieldPin {
                        pos: p(pin.pos),
                        blur: pin.blur * k,
                    })
                    .collect(),
            }),
            BlurGallery::Iris(i) => BlurGallery::Iris(IrisBlur {
                center: p(i.center),
                radii: p(i.radii),
                blur: i.blur * k,
                ..*i
            }),
            BlurGallery::TiltShift(t) => BlurGallery::TiltShift(TiltShiftBlur {
                center: p(t.center),
                focus: t.focus * k,
                transition: t.transition * k,
                blur: t.blur * k,
                ..*t
            }),
            BlurGallery::Path(pb) => BlurGallery::Path(PathBlur {
                points: pb.points.iter().map(|q| p(*q)).collect(),
                speed: pb.speed * k,
            }),
            BlurGallery::Spin(s) => BlurGallery::Spin(SpinBlur {
                center: p(s.center),
                radii: p(s.radii),
                ..*s
            }),
        }
    }

    /// The blur applied to `src`.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        if src.is_empty() || self.is_identity() {
            return src.clone();
        }
        match self {
            BlurGallery::Field(f) => field_blur(src, f),
            BlurGallery::Iris(i) => iris_blur(src, i),
            BlurGallery::TiltShift(t) => tilt_shift_blur(src, t),
            BlurGallery::Path(p) => path_blur(src, p),
            BlurGallery::Spin(s) => spin_blur(src, s),
        }
    }
}

fn has_extent(points: &[[f32; 2]]) -> bool {
    points
        .windows(2)
        .any(|w| (w[1][0] - w[0][0]).abs() + (w[1][1] - w[0][1]).abs() > 1e-4)
}

/// One uniformly blurred copy of `src` at `sigma`: a true Gaussian up to
/// [`BOX_ABOVE`], three box passes of matching variance past it.
fn blur_level(src: &FilterBuffer, sigma: f32) -> FilterBuffer {
    if sigma <= BOX_ABOVE {
        return gaussian_blur(src, sigma, EdgeMode::Clamp);
    }
    // Three boxes of radius r have variance r (r + 1).
    let r = (((1.0 + 4.0 * sigma * sigma).sqrt() - 1.0) * 0.5)
        .round()
        .max(1.0) as u32;
    let once = box_blur(src, r, EdgeMode::Clamp);
    let twice = box_blur(&once, r, EdgeMode::Clamp);
    box_blur(&twice, r, EdgeMode::Clamp)
}

/// A spatially varying Gaussian: pixel `i` of the result is `src` blurred by
/// `amounts[i]` (a sigma, clamped to `0..=MAX_GALLERY_BLUR`), interpolated
/// linearly between the two stack levels that bracket it. A pixel whose
/// amount is zero is copied through exactly. Holds at most two blurred
/// copies at a time, whatever the image size.
///
/// `amounts` must have one entry per pixel; a mismatched map is the identity.
pub fn variable_blur(src: &FilterBuffer, amounts: &[f32]) -> FilterBuffer {
    let mut out = src.clone();
    if src.is_empty() || amounts.len() != src.len() {
        return out;
    }
    let max = amounts
        .iter()
        .map(|a| clamp_amount(*a))
        .fold(0.0f32, f32::max);
    if max <= 0.0 {
        return out;
    }
    let mut prev = src.clone();
    let mut prev_s = LEVELS[0];
    for &s in &LEVELS[1..] {
        if prev_s >= max {
            break;
        }
        let cur = blur_level(src, s);
        out.pixels_mut()
            .par_iter_mut()
            .zip(amounts.par_iter())
            .enumerate()
            .for_each(|(i, (px, a))| {
                let a = clamp_amount(*a);
                if a > prev_s && a <= s {
                    let t = (a - prev_s) / (s - prev_s);
                    let lo = prev.pixels()[i];
                    let hi = cur.pixels()[i];
                    for c in 0..4 {
                        px[c] = lo[c] + (hi[c] - lo[c]) * t;
                    }
                }
            });
        prev = cur;
        prev_s = s;
    }
    out
}

/// The per-pixel amount map of `f`, evaluated at pixel centres.
fn amount_map(src: &FilterBuffer, f: impl Fn(f32, f32) -> f32 + Sync) -> Vec<f32> {
    let (w, _) = src.dimensions();
    let w = w.max(1) as usize;
    (0..src.len())
        .into_par_iter()
        .map(|i| f((i % w) as f32 + 0.5, (i / w) as f32 + 0.5))
        .collect()
}

/// The Field Blur amount at `(x, y)`.
pub fn field_amount(f: &FieldBlur, x: f32, y: f32) -> f32 {
    let pins = &f.pins[..f.pins.len().min(MAX_FIELD_PINS)];
    let (mut num, mut den) = (0.0f32, 0.0f32);
    for pin in pins {
        let d2 = (x - pin.pos[0]).powi(2) + (y - pin.pos[1]).powi(2);
        if d2 < 1e-6 {
            return clamp_amount(pin.blur);
        }
        let w = 1.0 / d2;
        num += w * clamp_amount(pin.blur);
        den += w;
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

/// The Iris Blur amount at `(x, y)`.
pub fn iris_amount(i: &IrisBlur, x: f32, y: f32) -> f32 {
    let r = ellipse_distance([x, y], i.center, i.radii, finite_or(i.rotation_deg, 0.0));
    let focus = finite_or(i.focus, 0.5).clamp(0.0, 1.0);
    clamp_amount(i.blur) * smoothstep(focus, 1.0, r)
}

fn tilt_axes(t: &TiltShiftBlur) -> ([f32; 2], [f32; 2]) {
    let (s, c) = finite_or(t.angle_deg, 0.0).to_radians().sin_cos();
    ([c, s], [-s, c])
}

/// The Tilt-Shift amount at `(x, y)`.
pub fn tilt_shift_amount(t: &TiltShiftBlur, x: f32, y: f32) -> f32 {
    let (_, n) = tilt_axes(t);
    let d = ((x - t.center[0]) * n[0] + (y - t.center[1]) * n[1]).abs();
    let focus = finite_or(t.focus, 0.0).max(0.0);
    let transition = finite_or(t.transition, 0.0).max(0.0);
    clamp_amount(t.blur) * smoothstep(focus, focus + transition, d)
}

fn field_blur(src: &FilterBuffer, f: &FieldBlur) -> FilterBuffer {
    variable_blur(src, &amount_map(src, |x, y| field_amount(f, x, y)))
}

fn iris_blur(src: &FilterBuffer, i: &IrisBlur) -> FilterBuffer {
    variable_blur(src, &amount_map(src, |x, y| iris_amount(i, x, y)))
}

fn tilt_shift_blur(src: &FilterBuffer, t: &TiltShiftBlur) -> FilterBuffer {
    let amounts = amount_map(src, |x, y| tilt_shift_amount(t, x, y));
    let blurred = variable_blur(src, &amounts);
    let distortion = finite_or(t.distortion, 0.0).clamp(-1.0, 1.0);
    if distortion == 0.0 {
        return blurred;
    }
    let (dir, n) = tilt_axes(t);
    let streak = if distortion > 0.0 { n } else { dir };
    let (w, h) = src.dimensions();
    let mut out = blurred.clone();
    let sampling = Sampling::clamped();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let i = (y * w + x) as usize;
        let here = blurred.pixels()[i];
        let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
        let side = (cx - t.center[0]) * n[0] + (cy - t.center[1]) * n[1];
        if !t.symmetric && side <= 0.0 {
            return here;
        }
        let length = distortion.abs() * 3.0 * amounts[i];
        let taps = (length.ceil() as u32).clamp(1, MAX_TAPS);
        if taps < 2 {
            return here;
        }
        let inv = 1.0 / taps as f32;
        let mut acc = [0.0f32; 4];
        for k in 0..taps {
            let s = ((k as f32 + 0.5) * inv - 0.5) * length;
            accumulate(
                &mut acc,
                blurred.sample(cx + streak[0] * s, cy + streak[1] * s, sampling),
                inv,
            );
        }
        acc
    });
    out
}

/// The unit direction a Path Blur streaks in at `(x, y)`, if the path gives
/// one there.
pub fn path_direction(points: &[[f32; 2]], x: f32, y: f32) -> Option<[f32; 2]> {
    let points = &points[..points.len().min(MAX_PATH_POINTS)];
    let (mut vx, mut vy) = (0.0f32, 0.0f32);
    for seg in points.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let len2 = ex * ex + ey * ey;
        if len2 < 1e-8 {
            continue;
        }
        let t = (((x - a[0]) * ex + (y - a[1]) * ey) / len2).clamp(0.0, 1.0);
        let (px, py) = (a[0] + ex * t, a[1] + ey * t);
        let d2 = (x - px).powi(2) + (y - py).powi(2);
        let w = 1.0 / (d2 + 1.0);
        let len = len2.sqrt();
        vx += w * ex / len;
        vy += w * ey / len;
    }
    let norm = (vx * vx + vy * vy).sqrt();
    (norm > 1e-6).then(|| [vx / norm, vy / norm])
}

fn path_blur(src: &FilterBuffer, p: &PathBlur) -> FilterBuffer {
    let speed = finite_or(p.speed, 0.0).clamp(0.0, MAX_PATH_SPEED);
    let taps = (speed.ceil() as u32).clamp(1, MAX_TAPS);
    let (w, h) = src.dimensions();
    let inv = 1.0 / taps as f32;
    let sampling = Sampling::clamped();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
        let Some(d) = path_direction(&p.points, cx, cy) else {
            return src.get(x, y);
        };
        let mut acc = [0.0f32; 4];
        for k in 0..taps {
            let s = ((k as f32 + 0.5) * inv - 0.5) * speed;
            accumulate(
                &mut acc,
                src.sample(cx + d[0] * s, cy + d[1] * s, sampling),
                inv,
            );
        }
        acc
    });
    out
}

/// The Spin Blur's weight at `(x, y)`: 1 inside, fading to 0 at the rim.
pub fn spin_weight(s: &SpinBlur, x: f32, y: f32) -> f32 {
    let r = ellipse_distance([x, y], s.center, s.radii, finite_or(s.rotation_deg, 0.0));
    let feather = finite_or(s.feather, 0.0).clamp(0.0, 1.0);
    1.0 - smoothstep(1.0 - feather, 1.0, r)
}

fn spin_blur(src: &FilterBuffer, s: &SpinBlur) -> FilterBuffer {
    let angle = finite_or(s.angle_deg, 0.0)
        .clamp(-MAX_SPIN_ANGLE, MAX_SPIN_ANGLE)
        .to_radians();
    let (w, h) = src.dimensions();
    let sampling = Sampling::clamped();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
        let weight = spin_weight(s, cx, cy);
        let sweep = angle * weight;
        let (px, py) = (cx - s.center[0], cy - s.center[1]);
        let radius = (px * px + py * py).sqrt();
        let taps = ((radius * sweep.abs()).ceil() as u32 + 1).clamp(1, MAX_TAPS);
        if taps < 2 {
            return src.get(x, y);
        }
        let inv = 1.0 / taps as f32;
        let mut acc = [0.0f32; 4];
        for k in 0..taps {
            let t = (k as f32 / (taps - 1) as f32 - 0.5) * sweep;
            let (sn, cs) = t.sin_cos();
            accumulate(
                &mut acc,
                src.sample(
                    s.center[0] + px * cs - py * sn,
                    s.center[1] + px * sn + py * cs,
                    sampling,
                ),
                inv,
            );
        }
        acc
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic high-frequency pattern: every pixel differs from its
    /// neighbours, so any blur lowers local variance.
    fn checker(w: u32, h: u32) -> FilterBuffer {
        let mut buf = FilterBuffer::filled(w, h, [0.0, 0.0, 0.0, 1.0]).unwrap();
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
                buf.set(x, y, [v, v * 0.5, 1.0 - v, 1.0]);
            }
        }
        buf
    }

    /// Variance of the red channel over a `size`-square window at `(x0, y0)`.
    fn variance(buf: &FilterBuffer, x0: u32, y0: u32, size: u32) -> f32 {
        let mut vals = Vec::new();
        for y in y0..y0 + size {
            for x in x0..x0 + size {
                vals.push(buf.get(x, y)[0]);
            }
        }
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32
    }

    #[test]
    fn zero_blur_is_the_identity_for_every_kind() {
        let src = checker(40, 30);
        let zero = [
            BlurGallery::Field(FieldBlur {
                pins: vec![FieldPin {
                    pos: [10.0, 10.0],
                    blur: 0.0,
                }],
            }),
            BlurGallery::Field(FieldBlur::default()),
            BlurGallery::Iris(IrisBlur {
                blur: 0.0,
                ..match BlurGallery::default_for(BlurGalleryKind::Iris, 40, 30) {
                    BlurGallery::Iris(i) => i,
                    _ => unreachable!(),
                }
            }),
            BlurGallery::TiltShift(TiltShiftBlur {
                blur: 0.0,
                distortion: 0.8,
                ..match BlurGallery::default_for(BlurGalleryKind::TiltShift, 40, 30) {
                    BlurGallery::TiltShift(t) => t,
                    _ => unreachable!(),
                }
            }),
            BlurGallery::Path(PathBlur {
                points: vec![[5.0, 5.0], [30.0, 20.0]],
                speed: 0.0,
            }),
            BlurGallery::Path(PathBlur {
                points: vec![[5.0, 5.0]],
                speed: 40.0,
            }),
            BlurGallery::Spin(SpinBlur {
                angle_deg: 0.0,
                ..match BlurGallery::default_for(BlurGalleryKind::Spin, 40, 30) {
                    BlurGallery::Spin(s) => s,
                    _ => unreachable!(),
                }
            }),
        ];
        for g in &zero {
            assert!(g.is_identity(), "{g:?}");
            assert_eq!(g.apply(&src), src, "{g:?} changed the image");
        }
        // The engine path, not only the early return: a zero amount map.
        assert_eq!(variable_blur(&src, &vec![0.0; src.len()]), src);
        // And every default does change it.
        for kind in BlurGalleryKind::ALL {
            let g = BlurGallery::default_for(kind, 40, 30);
            assert!(!g.is_identity(), "{kind:?}");
            assert_ne!(g.apply(&src), src, "{kind:?} default changed nothing");
        }
    }

    #[test]
    fn a_flat_image_stays_flat_under_every_default() {
        let src = FilterBuffer::filled(33, 21, [0.2, 0.3, 0.4, 0.9]).unwrap();
        for kind in BlurGalleryKind::ALL {
            let out = BlurGallery::default_for(kind, 33, 21).apply(&src);
            for px in out.pixels() {
                for (c, want) in [0.2, 0.3, 0.4, 0.9].into_iter().enumerate() {
                    assert!((px[c] - want).abs() < 1e-4, "{kind:?}: {px:?}");
                }
            }
        }
    }

    #[test]
    fn iris_leaves_the_centre_sharp_and_blurs_the_corners() {
        let src = checker(96, 96);
        let iris = BlurGallery::Iris(IrisBlur {
            center: [48.0, 48.0],
            radii: [30.0, 30.0],
            rotation_deg: 0.0,
            focus: 0.5,
            blur: 6.0,
        });
        let out = iris.apply(&src);
        let centre_before = variance(&src, 40, 40, 16);
        let centre_after = variance(&out, 40, 40, 16);
        let corner_before = variance(&src, 0, 0, 12);
        let corner_after = variance(&out, 0, 0, 12);
        assert!(centre_before > 0.2 && corner_before > 0.2);
        assert!(
            (centre_after - centre_before).abs() < 1e-6,
            "the centre stayed sharp: {centre_before} -> {centre_after}"
        );
        // Inside the focus the pixels are the source's exactly.
        for y in 40..56 {
            for x in 40..56 {
                assert_eq!(out.get(x, y), src.get(x, y));
            }
        }
        assert!(
            corner_after < corner_before * 0.05,
            "the corners blurred: {corner_before} -> {corner_after}"
        );
    }

    #[test]
    fn tilt_shift_keeps_its_band_sharp_and_blurs_beyond_it() {
        let src = checker(80, 80);
        let tilt = BlurGallery::TiltShift(TiltShiftBlur {
            center: [40.0, 40.0],
            angle_deg: 0.0,
            focus: 8.0,
            transition: 8.0,
            blur: 5.0,
            distortion: 0.0,
            symmetric: false,
        });
        let out = tilt.apply(&src);
        // Rows 33..47 are within 8 px of the centre line: untouched.
        for y in 33..47 {
            for x in 0..80 {
                assert_eq!(out.get(x, y), src.get(x, y), "({x},{y})");
            }
        }
        assert!(variance(&out, 10, 0, 12) < variance(&src, 10, 0, 12) * 0.05);
        assert!(variance(&out, 10, 68, 12) < variance(&src, 10, 68, 12) * 0.05);
        // Distortion changes only the side below the band unless symmetric.
        let distorted = BlurGallery::TiltShift(TiltShiftBlur {
            center: [40.0, 40.0],
            angle_deg: 0.0,
            focus: 8.0,
            transition: 8.0,
            blur: 5.0,
            distortion: 1.0,
            symmetric: false,
        })
        .apply(&checker(80, 80));
        for y in 0..40 {
            for x in 0..80 {
                assert_eq!(distorted.get(x, y), out.get(x, y));
            }
        }
    }

    /// Concentric rings plus an angular pattern: `ring(r) + spoke(theta)`.
    fn rings_and_spokes(n: u32, spokes: bool) -> FilterBuffer {
        let mut buf = FilterBuffer::filled(n, n, [0.0, 0.0, 0.0, 1.0]).unwrap();
        let c = n as f32 * 0.5;
        for y in 0..n {
            for x in 0..n {
                let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
                let r = (dx * dx + dy * dy).sqrt();
                let ring = 0.5 + 0.5 * (r * 0.5).sin();
                let spoke = if spokes {
                    0.5 + 0.5 * (dy.atan2(dx) * 12.0).sin()
                } else {
                    0.0
                };
                buf.set(x, y, [ring, spoke, 0.0, 1.0]);
            }
        }
        buf
    }

    #[test]
    fn spin_blurs_tangentially_and_preserves_the_radial_profile() {
        let n = 96;
        let src = rings_and_spokes(n, true);
        let spin = BlurGallery::Spin(SpinBlur {
            center: [48.0, 48.0],
            radii: [200.0, 200.0],
            rotation_deg: 0.0,
            feather: 0.0,
            angle_deg: 60.0,
        });
        let out = spin.apply(&src);
        // Red is radial only: a rotation about the centre maps it onto itself,
        // so the radial profile survives (up to resampling).
        let mut worst_red = 0.0f32;
        // Green is angular only: 12 spokes over a 60 degree sweep (two whole
        // periods) average out.
        let (mut spoke_before, mut spoke_after) = (0.0f32, 0.0f32);
        for y in 8..88 {
            for x in 8..88 {
                let (dx, dy) = (x as f32 + 0.5 - 48.0, y as f32 + 0.5 - 48.0);
                let r = (dx * dx + dy * dy).sqrt();
                if !(12.0..=40.0).contains(&r) {
                    continue;
                }
                worst_red = worst_red.max((out.get(x, y)[0] - src.get(x, y)[0]).abs());
                spoke_before += (src.get(x, y)[1] - 0.5).abs();
                spoke_after += (out.get(x, y)[1] - 0.5).abs();
            }
        }
        assert!(worst_red < 0.08, "the radial profile moved by {worst_red}");
        assert!(
            spoke_after < spoke_before * 0.2,
            "the tangential pattern blurred: {spoke_before} -> {spoke_after}"
        );
        // Outside the spin ellipse nothing moves.
        let small = BlurGallery::Spin(SpinBlur {
            center: [48.0, 48.0],
            radii: [20.0, 20.0],
            rotation_deg: 0.0,
            feather: 0.0,
            angle_deg: 40.0,
        })
        .apply(&src);
        assert_eq!(small.get(2, 2), src.get(2, 2));
    }

    #[test]
    fn path_blur_streaks_along_the_path_not_across_it() {
        // Vertical stripes: a horizontal path smears them, a vertical path
        // runs along them and leaves them be.
        let mut src = FilterBuffer::filled(64, 32, [0.0, 0.0, 0.0, 1.0]).unwrap();
        for y in 0..32 {
            for x in (0..64).step_by(4) {
                src.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let across = BlurGallery::Path(PathBlur {
            points: vec![[4.0, 16.0], [60.0, 16.0]],
            speed: 16.0,
        })
        .apply(&src);
        let along = BlurGallery::Path(PathBlur {
            points: vec![[32.0, 2.0], [32.0, 30.0]],
            speed: 16.0,
        })
        .apply(&src);
        assert!(variance(&across, 20, 10, 12) < variance(&src, 20, 10, 12) * 0.1);
        for x in 20..44 {
            assert!((along.get(x, 16)[0] - src.get(x, 16)[0]).abs() < 1e-4);
        }
    }

    #[test]
    fn field_pins_interpolate_their_amounts() {
        let f = FieldBlur {
            pins: vec![
                FieldPin {
                    pos: [0.0, 0.0],
                    blur: 0.0,
                },
                FieldPin {
                    pos: [100.0, 0.0],
                    blur: 20.0,
                },
            ],
        };
        assert_eq!(field_amount(&f, 0.0, 0.0), 0.0);
        assert_eq!(field_amount(&f, 100.0, 0.0), 20.0);
        assert!((field_amount(&f, 50.0, 0.0) - 10.0).abs() < 1e-4);
        assert!(field_amount(&f, 25.0, 0.0) < field_amount(&f, 75.0, 0.0));
    }

    #[test]
    fn scaling_matches_the_full_resolution_geometry() {
        let g = BlurGallery::default_for(BlurGalleryKind::Iris, 200, 100);
        let half = g.scaled(0.5);
        let (BlurGallery::Iris(a), BlurGallery::Iris(b)) = (&g, &half) else {
            unreachable!()
        };
        assert_eq!(b.center, [a.center[0] * 0.5, a.center[1] * 0.5]);
        assert_eq!(b.blur, a.blur * 0.5);
        assert_eq!(b.focus, a.focus);
        assert!((iris_amount(a, 10.0, 10.0) * 0.5 - iris_amount(b, 5.0, 5.0)).abs() < 1e-4);
    }

    #[test]
    fn hostile_parameters_do_not_panic() {
        let src = checker(3, 1);
        let g = BlurGallery::Iris(IrisBlur {
            center: [f32::NAN, 0.0],
            radii: [0.0, f32::INFINITY],
            rotation_deg: f32::NAN,
            focus: f32::NAN,
            blur: 1e9,
        });
        let _ = g.apply(&src);
        let _ = BlurGallery::Spin(SpinBlur {
            center: [0.0, 0.0],
            radii: [0.0, 0.0],
            rotation_deg: 0.0,
            feather: f32::NAN,
            angle_deg: 1e9,
        })
        .apply(&src);
        let empty = FilterBuffer::transparent(0, 0).unwrap();
        for kind in BlurGalleryKind::ALL {
            assert!(BlurGallery::default_for(kind, 1, 1)
                .apply(&empty)
                .is_empty());
            let _ = BlurGallery::default_for(kind, 1, 1).apply(&checker(1, 1));
        }
    }
}
