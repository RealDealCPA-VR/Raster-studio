//! Edit ▸ Auto-Align Layers (W10-G): how far one layer is from another.
//!
//! Two estimators, and the dialog names which one runs:
//!
//! # Similarity (translation + rotation + uniform scale): feature matching
//!
//! An ORB-style pipeline, written out here in full rather than pulled in:
//!
//! 1. **Working copy.** Both layers are reduced to gamma-encoded luminance
//!    (premultiplied, so transparent pixels read as black) and box-shrunk so
//!    the long side is at most [`WORKING_MAX_SIDE`]; the answer is scaled back
//!    at the end.
//! 2. **Pyramid.** [`PYRAMID_LEVELS`] levels, each [`PYRAMID_STEP`] smaller,
//!    so a feature is found at the scale it lives at and a scale change
//!    between the layers is matched level to level.
//! 3. **FAST-9 corners.** A pixel is a corner when nine contiguous pixels of
//!    the 16-pixel Bresenham circle of radius 3 are all brighter, or all
//!    darker, than it by the threshold. Its score is the summed excess over
//!    the threshold; a 3x3 non-maximum suppression keeps one pixel per
//!    corner, and the strongest [`MAX_FEATURES`] across the pyramid survive.
//!    A corner whose patch reaches a transparent pixel is dropped: the edge of
//!    a layer is not a feature of the scene.
//! 4. **Orientation.** The intensity-centroid angle of the patch (radius
//!    [`PATCH_RADIUS`]) — ORB's rule — so the descriptor can be steered.
//! 5. **Steered BRIEF.** 256 point pairs, drawn once from a fixed seed with a
//!    Gaussian spread inside the patch, rotated by the corner's angle and
//!    compared on a box-smoothed copy of the level: one bit per pair.
//! 6. **Brute-force matching.** Every descriptor of the moving layer against
//!    every descriptor of the reference by Hamming distance, kept when the
//!    match is mutual (cross-checked), under [`MAX_HAMMING`], and clearly
//!    better than the runner-up (ratio [`RATIO`]).
//! 7. **RANSAC similarity.** Two correspondences fix a similarity (as complex
//!    numbers, `w = a z + b`). [`RANSAC_ITERATIONS`] deterministic draws pick
//!    the model with the most inliers within [`INLIER_PX`] working pixels,
//!    and a least-squares refit over the inliers (repeated, as the inlier set
//!    settles) gives the answer. Fewer than [`MIN_INLIERS`] inliers is an
//!    [`AlignError::NotEnoughMatches`], never a guess.
//!
//! # Translation only: phase correlation
//!
//! Both working copies are Hann-windowed, zero-padded to a power of two and
//! Fourier-transformed (an iterative radix-2 FFT, [`fft`]); the normalised
//! cross-power spectrum's inverse transform peaks at the shift, and a
//! parabola through the peak and its neighbours gives the sub-pixel part.
//!
//! # The answer's direction
//!
//! [`Similarity`] maps a point of the **moving** layer to the point of the
//! **reference** layer showing the same thing, so drawing the moving layer
//! through that map lays it over the reference.

use crate::rng::Rng;
use crate::FilterBuffer;

/// The long side of the working copy, in pixels.
pub const WORKING_MAX_SIDE: usize = 640;
/// Pyramid levels searched for features.
pub const PYRAMID_LEVELS: usize = 4;
/// Each pyramid level is this much smaller than the one above.
pub const PYRAMID_STEP: f32 = 1.25;
/// Features kept across the pyramid.
pub const MAX_FEATURES: usize = 600;
/// The descriptor patch's radius, in level pixels.
pub const PATCH_RADIUS: i32 = 13;
/// A match further than this many differing bits is refused.
pub const MAX_HAMMING: u32 = 70;
/// A match must beat the runner-up by this ratio.
pub const RATIO: f32 = 0.85;
/// RANSAC draws.
pub const RANSAC_ITERATIONS: usize = 1500;
/// A correspondence within this many working pixels of the model is an inlier.
pub const INLIER_PX: f64 = 2.0;
/// The fewest inliers an answer may rest on.
pub const MIN_INLIERS: usize = 6;

/// How a layer is aligned.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum AlignMethod {
    /// Translation, rotation and uniform scale, by feature matching.
    #[default]
    Similarity,
    /// Translation only, by phase correlation.
    Translation,
}

impl AlignMethod {
    pub const ALL: [AlignMethod; 2] = [AlignMethod::Similarity, AlignMethod::Translation];
}

/// Why no alignment came back.
#[derive(Clone, PartialEq, Debug, thiserror::Error)]
pub enum AlignError {
    #[error("a layer is empty")]
    Empty,
    #[error("the layers have too few matching features ({0} found)")]
    NotEnoughMatches(usize),
    #[error("the layers share no recognisable content")]
    NoCorrelation,
}

/// `p -> scale * R(angle) * p + (tx, ty)`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Similarity {
    pub scale: f64,
    /// Radians, counter-clockwise in a y-down image (clockwise on screen).
    pub angle: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Similarity {
    pub const IDENTITY: Similarity = Similarity {
        scale: 1.0,
        angle: 0.0,
        tx: 0.0,
        ty: 0.0,
    };

    fn from_complex(a: C, b: C) -> Self {
        Self {
            scale: a.norm(),
            angle: a.im.atan2(a.re),
            tx: b.re,
            ty: b.im,
        }
    }

    /// Map a point.
    pub fn apply(&self, p: [f64; 2]) -> [f64; 2] {
        let (s, c) = self.angle.sin_cos();
        [
            self.scale * (c * p[0] - s * p[1]) + self.tx,
            self.scale * (s * p[0] + c * p[1]) + self.ty,
        ]
    }

    /// The map as the six numbers of a column-major 2x3 affine matrix
    /// (`[x_axis, y_axis, translation]`, glam's `Affine2::to_cols_array`).
    pub fn to_cols(&self) -> [f32; 6] {
        let (s, c) = self.angle.sin_cos();
        let k = self.scale;
        [
            (k * c) as f32,
            (k * s) as f32,
            (-k * s) as f32,
            (k * c) as f32,
            self.tx as f32,
            self.ty as f32,
        ]
    }
}

/// What an estimate rests on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AlignReport {
    pub transform: Similarity,
    /// Correspondences that agree with the answer (phase correlation: 1).
    pub inliers: usize,
    /// Correspondences found before RANSAC (phase correlation: 1).
    pub matches: usize,
}

/// Estimate the map from `moving` to `reference`. See the module docs.
pub fn estimate(
    reference: &FilterBuffer,
    moving: &FilterBuffer,
    method: AlignMethod,
) -> Result<AlignReport, AlignError> {
    if reference.is_empty() || moving.is_empty() {
        return Err(AlignError::Empty);
    }
    let long = reference
        .dimensions()
        .0
        .max(reference.dimensions().1)
        .max(moving.dimensions().0)
        .max(moving.dimensions().1) as usize;
    let factor = (long.div_ceil(WORKING_MAX_SIDE)).max(1);
    let r = Gray::from_buffer(reference, factor);
    let m = Gray::from_buffer(moving, factor);
    let k = factor as f64;
    let mut report = match method {
        AlignMethod::Similarity => similarity(&r, &m)?,
        AlignMethod::Translation => translation(&r, &m)?,
    };
    // A box shrink by k maps full-resolution p to p / k: the translation
    // scales back up, the linear part is unchanged.
    report.transform.tx *= k;
    report.transform.ty *= k;
    Ok(report)
}

// ---------------------------------------------------------------------------
// Grey working images
// ---------------------------------------------------------------------------

/// A single-channel image with an opacity plane.
#[derive(Clone, Debug)]
struct Gray {
    w: usize,
    h: usize,
    v: Vec<f32>,
    /// Coverage in `0..=1`.
    a: Vec<f32>,
}

impl Gray {
    /// Gamma-encoded premultiplied luminance, box-shrunk by `factor`.
    fn from_buffer(buf: &FilterBuffer, factor: usize) -> Gray {
        let (bw, bh) = buf.dimensions();
        let (bw, bh) = (bw as usize, bh as usize);
        let w = (bw / factor).max(1);
        let h = (bh / factor).max(1);
        let mut v = vec![0.0f32; w * h];
        let mut a = vec![0.0f32; w * h];
        let px = buf.pixels();
        for y in 0..h {
            for x in 0..w {
                let (mut sv, mut sa, mut n) = (0.0f32, 0.0f32, 0.0f32);
                for yy in y * factor..((y + 1) * factor).min(bh) {
                    for xx in x * factor..((x + 1) * factor).min(bw) {
                        let p = px[yy * bw + xx];
                        let lum = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                        sv += lum.max(0.0).powf(1.0 / 2.2);
                        sa += p[3].clamp(0.0, 1.0);
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    v[y * w + x] = sv / n;
                    a[y * w + x] = sa / n;
                }
            }
        }
        Gray { w, h, v, a }
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> f32 {
        let x = x.clamp(0, self.w as i32 - 1) as usize;
        let y = y.clamp(0, self.h as i32 - 1) as usize;
        self.v[y * self.w + x]
    }

    /// Bilinear resize to `w x h`.
    fn resized(&self, w: usize, h: usize) -> Gray {
        let (w, h) = (w.max(1), h.max(1));
        let sx = self.w as f32 / w as f32;
        let sy = self.h as f32 / h as f32;
        let mut v = vec![0.0; w * h];
        let mut a = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let fx = (x as f32 + 0.5) * sx - 0.5;
                let fy = (y as f32 + 0.5) * sy - 0.5;
                let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                let sample = |plane: &[f32], xx: i32, yy: i32| {
                    let xx = xx.clamp(0, self.w as i32 - 1) as usize;
                    let yy = yy.clamp(0, self.h as i32 - 1) as usize;
                    plane[yy * self.w + xx]
                };
                for (plane, out) in [(&self.v, &mut v), (&self.a, &mut a)] {
                    let p00 = sample(plane, x0, y0);
                    let p10 = sample(plane, x0 + 1, y0);
                    let p01 = sample(plane, x0, y0 + 1);
                    let p11 = sample(plane, x0 + 1, y0 + 1);
                    let top = p00 + (p10 - p00) * tx;
                    let bottom = p01 + (p11 - p01) * tx;
                    out[y * w + x] = top + (bottom - top) * ty;
                }
            }
        }
        Gray { w, h, v, a }
    }

    /// A `(2r+1)`-square box blur of the values (for the descriptor tests).
    fn box_blurred(&self, r: i32) -> Vec<f32> {
        let (w, h) = (self.w as i32, self.h as i32);
        let mut tmp = vec![0.0f32; self.v.len()];
        let n = (2 * r + 1) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0;
                for d in -r..=r {
                    s += self.get(x + d, y);
                }
                tmp[(y * w + x) as usize] = s / n;
            }
        }
        let mut out = vec![0.0f32; self.v.len()];
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0;
                for d in -r..=r {
                    let yy = (y + d).clamp(0, h - 1);
                    s += tmp[(yy * w + x) as usize];
                }
                out[(y * w + x) as usize] = s / n;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Features
// ---------------------------------------------------------------------------

/// The Bresenham circle of radius 3, clockwise from the top.
const CIRCLE: [(i32, i32); 16] = [
    (0, -3),
    (1, -3),
    (2, -2),
    (3, -1),
    (3, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (0, 3),
    (-1, 3),
    (-2, 2),
    (-3, 1),
    (-3, 0),
    (-3, -1),
    (-2, -2),
    (-1, -3),
];

/// One oriented feature, in level-0 working coordinates.
#[derive(Clone, Copy, Debug)]
struct Feature {
    x: f32,
    y: f32,
    score: f32,
    desc: [u64; 4],
}

/// The steered-BRIEF sampling pattern: 256 pairs inside the patch.
fn brief_pattern() -> Vec<[(f32, f32); 2]> {
    let mut rng = Rng::at(0x0b1e_f00d, 7, 11);
    let limit = PATCH_RADIUS as f32 * 0.7;
    let sigma = PATCH_RADIUS as f32 / 2.5;
    let draw = |rng: &mut Rng| -> (f32, f32) {
        loop {
            let x = rng.next_gaussian() * sigma;
            let y = rng.next_gaussian() * sigma;
            if x * x + y * y <= limit * limit {
                return (x, y);
            }
        }
    };
    (0..256).map(|_| [draw(&mut rng), draw(&mut rng)]).collect()
}

/// Summed-area table of "not opaque" pixels, for the patch-coverage test.
fn hole_table(g: &Gray) -> Vec<u32> {
    let (w, h) = (g.w, g.h);
    let mut t = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += u32::from(g.a[y * w + x] < 0.98);
            t[(y + 1) * (w + 1) + x + 1] = t[y * (w + 1) + x + 1] + row;
        }
    }
    t
}

fn holes_in(t: &[u32], w: usize, x0: i32, y0: i32, x1: i32, y1: i32) -> u32 {
    let (x0, y0, x1, y1) = (x0 as usize, y0 as usize, x1 as usize + 1, y1 as usize + 1);
    let s = w + 1;
    t[y1 * s + x1] + t[y0 * s + x0] - t[y0 * s + x1] - t[y1 * s + x0]
}

/// FAST-9 corners of one level with their scores, after non-maximum
/// suppression, inside `margin` of the border and on opaque patches only.
fn fast_corners(g: &Gray, threshold: f32, margin: i32) -> Vec<(i32, i32, f32)> {
    let (w, h) = (g.w as i32, g.h as i32);
    if w <= 2 * margin || h <= 2 * margin {
        return Vec::new();
    }
    let holes = hole_table(g);
    let mut score = vec![0.0f32; g.v.len()];
    for y in margin..h - margin {
        for x in margin..w - margin {
            let c = g.v[(y * w + x) as usize];
            let ring: [f32; 16] = CIRCLE.map(|(dx, dy)| g.v[((y + dy) * w + x + dx) as usize]);
            // Quick reject: of the four compass points, at least three must
            // pass for any 9-run to exist.
            let compass = [ring[0], ring[4], ring[8], ring[12]];
            let bright = compass.iter().filter(|&&p| p > c + threshold).count();
            let dark = compass.iter().filter(|&&p| p < c - threshold).count();
            if bright < 3 && dark < 3 {
                continue;
            }
            let run = |pass: &dyn Fn(f32) -> bool| -> bool {
                let mut best = 0;
                let mut cur = 0;
                for i in 0..32 {
                    if pass(ring[i % 16]) {
                        cur += 1;
                        best = best.max(cur);
                    } else {
                        cur = 0;
                    }
                }
                best >= 9
            };
            let s = if run(&|p| p > c + threshold) {
                ring.iter().map(|p| (p - c - threshold).max(0.0)).sum()
            } else if run(&|p| p < c - threshold) {
                ring.iter().map(|p| (c - threshold - p).max(0.0)).sum()
            } else {
                0.0
            };
            if s > 0.0
                && holes_in(&holes, g.w, x - margin, y - margin, x + margin, y + margin) == 0
            {
                score[(y * w + x) as usize] = s;
            }
        }
    }
    let mut out = Vec::new();
    for y in margin..h - margin {
        for x in margin..w - margin {
            let s = score[(y * w + x) as usize];
            if s <= 0.0 {
                continue;
            }
            let mut is_max = true;
            'n: for dy in -1..=1 {
                for dx in -1..=1 {
                    if (dx, dy) == (0, 0) {
                        continue;
                    }
                    let o = score[((y + dy) * w + x + dx) as usize];
                    // Ties go to the first in scan order.
                    if o > s || (o == s && (dy < 0 || (dy == 0 && dx < 0))) {
                        is_max = false;
                        break 'n;
                    }
                }
            }
            if is_max {
                out.push((x, y, s));
            }
        }
    }
    out
}

/// The intensity-centroid orientation at `(x, y)`.
fn orientation(g: &Gray, x: i32, y: i32) -> f32 {
    let (mut m10, mut m01) = (0.0f32, 0.0f32);
    let r = PATCH_RADIUS;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy > r * r {
                continue;
            }
            let v = g.get(x + dx, y + dy);
            m10 += dx as f32 * v;
            m01 += dy as f32 * v;
        }
    }
    m01.atan2(m10)
}

/// Every feature of `img`, strongest first, capped at [`MAX_FEATURES`].
fn features(img: &Gray, pattern: &[[(f32, f32); 2]]) -> Vec<Feature> {
    let margin = PATCH_RADIUS + 3;
    let mut all: Vec<Feature> = Vec::new();
    let mut level = img.clone();
    let mut level_scale = 1.0f32;
    for l in 0..PYRAMID_LEVELS {
        if l > 0 {
            level_scale *= PYRAMID_STEP;
            let w = (img.w as f32 / level_scale).round() as usize;
            let h = (img.h as f32 / level_scale).round() as usize;
            if w <= 2 * margin as usize + 4 || h <= 2 * margin as usize + 4 {
                break;
            }
            level = img.resized(w, h);
        }
        // An adaptive threshold: halve it until the level yields enough
        // corners (or it is too small to matter).
        let mut threshold = 0.08f32;
        let mut corners = fast_corners(&level, threshold, margin);
        while corners.len() < 150 && threshold > 0.01 {
            threshold *= 0.5;
            corners = fast_corners(&level, threshold, margin);
        }
        let smooth = level.box_blurred(2);
        let lw = level.w as i32;
        let lh = level.h as i32;
        let at = |x: f32, y: f32| -> f32 {
            let xi = (x.round() as i32).clamp(0, lw - 1);
            let yi = (y.round() as i32).clamp(0, lh - 1);
            smooth[(yi * lw + xi) as usize]
        };
        for (x, y, score) in corners {
            let theta = orientation(&level, x, y);
            let (s, c) = theta.sin_cos();
            let mut desc = [0u64; 4];
            for (i, [(ax, ay), (bx, by)]) in pattern.iter().enumerate() {
                let pa = (x as f32 + c * ax - s * ay, y as f32 + s * ax + c * ay);
                let pb = (x as f32 + c * bx - s * by, y as f32 + s * bx + c * by);
                if at(pa.0, pa.1) < at(pb.0, pb.1) {
                    desc[i / 64] |= 1u64 << (i % 64);
                }
            }
            all.push(Feature {
                // Level pixel centres back to level-0 coordinates.
                x: (x as f32 + 0.5) * level_scale - 0.5,
                y: (y as f32 + 0.5) * level_scale - 0.5,
                score,
                desc,
            });
        }
    }
    all.sort_by(|a, b| b.score.total_cmp(&a.score));
    all.truncate(MAX_FEATURES);
    all
}

fn hamming(a: &[u64; 4], b: &[u64; 4]) -> u32 {
    (0..4).map(|i| (a[i] ^ b[i]).count_ones()).sum()
}

/// Cross-checked, ratio-tested brute-force matches as `(moving, reference)`
/// point pairs.
fn match_features(moving: &[Feature], reference: &[Feature]) -> Vec<([f64; 2], [f64; 2])> {
    let best_of = |f: &Feature, pool: &[Feature]| -> Option<(usize, u32, u32)> {
        let mut best = (usize::MAX, u32::MAX);
        let mut second = u32::MAX;
        for (j, g) in pool.iter().enumerate() {
            let d = hamming(&f.desc, &g.desc);
            if d < best.1 {
                second = best.1;
                best = (j, d);
            } else if d < second {
                second = d;
            }
        }
        (best.0 != usize::MAX).then_some((best.0, best.1, second))
    };
    let back: Vec<Option<usize>> = reference
        .iter()
        .map(|f| best_of(f, moving).map(|(j, _, _)| j))
        .collect();
    let mut out = Vec::new();
    for (i, f) in moving.iter().enumerate() {
        let Some((j, d, second)) = best_of(f, reference) else {
            continue;
        };
        if d > MAX_HAMMING || back[j] != Some(i) {
            continue;
        }
        if second != u32::MAX && d as f32 > RATIO * second as f32 {
            continue;
        }
        let g = &reference[j];
        out.push((
            [f64::from(f.x), f64::from(f.y)],
            [f64::from(g.x), f64::from(g.y)],
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// RANSAC similarity
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn of(p: [f64; 2]) -> C {
        C { re: p[0], im: p[1] }
    }
    fn sub(self, o: C) -> C {
        C {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
    fn add(self, o: C) -> C {
        C {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn conj(self) -> C {
        C {
            re: self.re,
            im: -self.im,
        }
    }
    fn scale(self, k: f64) -> C {
        C {
            re: self.re * k,
            im: self.im * k,
        }
    }
    fn norm2(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
    fn norm(self) -> f64 {
        self.norm2().sqrt()
    }
}

/// Least-squares similarity `w = a z + b` over the pairs.
fn fit(pairs: &[([f64; 2], [f64; 2])]) -> Option<(C, C)> {
    let n = pairs.len() as f64;
    if pairs.len() < 2 {
        return None;
    }
    let mut zm = C { re: 0.0, im: 0.0 };
    let mut wm = C { re: 0.0, im: 0.0 };
    for (z, w) in pairs {
        zm = zm.add(C::of(*z));
        wm = wm.add(C::of(*w));
    }
    zm = zm.scale(1.0 / n);
    wm = wm.scale(1.0 / n);
    let mut num = C { re: 0.0, im: 0.0 };
    let mut den = 0.0;
    for (z, w) in pairs {
        let dz = C::of(*z).sub(zm);
        let dw = C::of(*w).sub(wm);
        num = num.add(dz.conj().mul(dw));
        den += dz.norm2();
    }
    if den < 1e-12 {
        return None;
    }
    let a = num.scale(1.0 / den);
    let b = wm.sub(a.mul(zm));
    Some((a, b))
}

fn inliers_of(a: C, b: C, pairs: &[([f64; 2], [f64; 2])]) -> Vec<usize> {
    pairs
        .iter()
        .enumerate()
        .filter(|(_, (z, w))| {
            a.mul(C::of(*z))
                .add(b)
                .sub(C::of(*w))
                .norm2()
                <= INLIER_PX * INLIER_PX
        })
        .map(|(i, _)| i)
        .collect()
}

fn similarity(reference: &Gray, moving: &Gray) -> Result<AlignReport, AlignError> {
    let pattern = brief_pattern();
    let fr = features(reference, &pattern);
    let fm = features(moving, &pattern);
    let pairs = match_features(&fm, &fr);
    if pairs.len() < MIN_INLIERS {
        return Err(AlignError::NotEnoughMatches(pairs.len()));
    }
    let mut best: Vec<usize> = Vec::new();
    for it in 0..RANSAC_ITERATIONS {
        let mut rng = Rng::at(0x5a4d_c0de, it as i64, pairs.len() as i64);
        let i = (rng.next_u64() % pairs.len() as u64) as usize;
        let j = (rng.next_u64() % pairs.len() as u64) as usize;
        if i == j {
            continue;
        }
        let (z1, w1) = (C::of(pairs[i].0), C::of(pairs[i].1));
        let (z2, w2) = (C::of(pairs[j].0), C::of(pairs[j].1));
        let dz = z2.sub(z1);
        if dz.norm2() < 4.0 {
            continue;
        }
        // a = (w2 - w1) / (z2 - z1)
        let a = w2.sub(w1).mul(dz.conj()).scale(1.0 / dz.norm2());
        // Refuse a wild scale outright: the layers of one scene.
        let s = a.norm();
        if !(0.25..=4.0).contains(&s) {
            continue;
        }
        let b = w1.sub(a.mul(z1));
        let inl = inliers_of(a, b, &pairs);
        if inl.len() > best.len() {
            best = inl;
        }
    }
    if best.len() < MIN_INLIERS {
        return Err(AlignError::NotEnoughMatches(best.len()));
    }
    let mut model = None;
    for _ in 0..4 {
        let chosen: Vec<_> = best.iter().map(|&i| pairs[i]).collect();
        let Some((a, b)) = fit(&chosen) else { break };
        model = Some((a, b));
        let next = inliers_of(a, b, &pairs);
        if next.len() < MIN_INLIERS || next == best {
            break;
        }
        best = next;
    }
    let (a, b) = model.ok_or(AlignError::NotEnoughMatches(best.len()))?;
    // The features sit at pixel indices (centre = index + 0.5); move the
    // answer into continuous coordinates, where the box shrink is a pure
    // scale about the origin.
    let half = C { re: 0.5, im: 0.5 };
    let b = b.add(half).sub(a.mul(half));
    Ok(AlignReport {
        transform: Similarity::from_complex(a, b),
        inliers: best.len(),
        matches: pairs.len(),
    })
}

// ---------------------------------------------------------------------------
// Phase correlation
// ---------------------------------------------------------------------------

/// An in-place iterative radix-2 FFT over `(re, im)`; `inverse` runs the
/// inverse transform (unnormalised). `re.len()` must be a power of two.
pub fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    if n <= 1 {
        return;
    }
    debug_assert!(n.is_power_of_two());
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * 2.0 * std::f64::consts::PI / len as f64;
        let (ws, wc) = ang.sin_cos();
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wc - ci * ws;
                ci = cr * ws + ci * wc;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

/// A 2-D FFT over an `n x n` row-major complex plane.
fn fft2(re: &mut [f64], im: &mut [f64], n: usize, inverse: bool) {
    let mut rr = vec![0.0; n];
    let mut ri = vec![0.0; n];
    for y in 0..n {
        fft(&mut re[y * n..(y + 1) * n], &mut im[y * n..(y + 1) * n], inverse);
    }
    for x in 0..n {
        for y in 0..n {
            rr[y] = re[y * n + x];
            ri[y] = im[y * n + x];
        }
        fft(&mut rr, &mut ri, inverse);
        for y in 0..n {
            re[y * n + x] = rr[y];
            im[y * n + x] = ri[y];
        }
    }
}

fn translation(reference: &Gray, moving: &Gray) -> Result<AlignReport, AlignError> {
    let n = reference
        .w
        .max(reference.h)
        .max(moving.w)
        .max(moving.h)
        .next_power_of_two();
    let plane = |g: &Gray| -> (Vec<f64>, Vec<f64>) {
        let mut re = vec![0.0; n * n];
        let mean = g.v.iter().map(|&v| f64::from(v)).sum::<f64>() / g.v.len().max(1) as f64;
        for y in 0..g.h {
            let wy = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * y as f64 / g.h as f64).cos();
            for x in 0..g.w {
                let wx = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * x as f64 / g.w as f64).cos();
                re[y * n + x] = (f64::from(g.v[y * g.w + x]) - mean) * wx * wy;
            }
        }
        (re, vec![0.0; n * n])
    };
    let (mut ar, mut ai) = plane(reference);
    let (mut br, mut bi) = plane(moving);
    fft2(&mut ar, &mut ai, n, false);
    fft2(&mut br, &mut bi, n, false);
    // R = B conj(A) / |B conj(A)|: with moving(p) = reference(p + t) the
    // inverse transform peaks at +t.
    let mut rr = vec![0.0; n * n];
    let mut ri = vec![0.0; n * n];
    for i in 0..n * n {
        let re = br[i] * ar[i] + bi[i] * ai[i];
        let im = bi[i] * ar[i] - br[i] * ai[i];
        let mag = (re * re + im * im).sqrt();
        if mag > 1e-12 {
            rr[i] = re / mag;
            ri[i] = im / mag;
        }
    }
    fft2(&mut rr, &mut ri, n, true);
    let (mut peak, mut at) = (f64::MIN, 0usize);
    for (i, &v) in rr.iter().enumerate() {
        if v > peak {
            peak = v;
            at = i;
        }
    }
    // A flat correlation surface means no shared structure.
    let mean = rr.iter().sum::<f64>() / (n * n) as f64;
    if !peak.is_finite() || peak <= mean + 1e-9 {
        return Err(AlignError::NoCorrelation);
    }
    let (px, py) = (at % n, at / n);
    let v = |x: usize, y: usize| rr[(y % n) * n + (x % n)];
    let sub = |m: f64, c: f64, p: f64| {
        let d = m - 2.0 * c + p;
        if d.abs() < 1e-12 {
            0.0
        } else {
            (0.5 * (m - p) / d).clamp(-0.5, 0.5)
        }
    };
    let dx = px as f64 + sub(v(px + n - 1, py), v(px, py), v(px + 1, py));
    let dy = py as f64 + sub(v(px, py + n - 1), v(px, py), v(px, py + 1));
    let wrap = |d: f64| if d > n as f64 / 2.0 { d - n as f64 } else { d };
    Ok(AlignReport {
        transform: Similarity {
            scale: 1.0,
            angle: 0.0,
            tx: wrap(dx),
            ty: wrap(dy),
        },
        inliers: 1,
        matches: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::EdgeMode;

    /// A scene with plenty of corners: rectangles and discs of random greys
    /// over a gentle gradient.
    fn scene(w: u32, h: u32) -> FilterBuffer {
        let mut b = FilterBuffer::transparent(w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let g = 0.15 + 0.3 * (x as f32 / w as f32) + 0.1 * (y as f32 / h as f32);
                b.set(x, y, [g, g, g, 1.0]);
            }
        }
        for i in 0..70 {
            let mut rng = Rng::at(42, i, 3);
            let cx = rng.next_f32() * w as f32;
            let cy = rng.next_f32() * h as f32;
            let rw = 4.0 + rng.next_f32() * 22.0;
            let rh = 4.0 + rng.next_f32() * 22.0;
            let v = rng.next_f32();
            let disc = i % 3 == 0;
            for y in 0..h {
                for x in 0..w {
                    let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                    let inside = if disc {
                        dx * dx + dy * dy < rw * rw * 0.5
                    } else {
                        dx.abs() < rw * 0.5 && dy.abs() < rh * 0.5
                    };
                    if inside {
                        b.set(x, y, [v, v * 0.8, v * 0.6, 1.0]);
                    }
                }
            }
        }
        b
    }

    /// `moving(p) = reference(s(p))`: the moving layer is the reference seen
    /// through the known map, so the estimate must return that map.
    fn through(reference: &FilterBuffer, s: Similarity) -> FilterBuffer {
        let (w, h) = reference.dimensions();
        let mut out = FilterBuffer::transparent(w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let p = s.apply([x as f64 + 0.5, y as f64 + 0.5]);
                out.set(
                    x,
                    y,
                    reference.sample_bilinear(p[0] as f32, p[1] as f32, EdgeMode::Clamp),
                );
            }
        }
        out
    }

    #[test]
    fn feature_matching_recovers_a_known_shift_and_rotation() {
        let reference = scene(256, 256);
        // Rotate about the centre by 4 degrees and shift by (9.4, -6.2).
        let angle = 4.0f64.to_radians();
        let (sn, cs) = angle.sin_cos();
        let (cx, cy) = (128.0, 128.0);
        let truth = Similarity {
            scale: 1.0,
            angle,
            tx: cx - (cs * cx - sn * cy) + 9.4,
            ty: cy - (sn * cx + cs * cy) - 6.2,
        };
        let moving = through(&reference, truth);
        let got = estimate(&reference, &moving, AlignMethod::Similarity).unwrap();
        let t = got.transform;
        let deg = (t.angle - truth.angle).to_degrees();
        assert!(deg.abs() <= 0.5, "rotation off by {deg} degrees: {t:?}");
        assert!((t.scale - 1.0).abs() < 0.01, "scale {}", t.scale);
        // Compare where the two maps send the image's own points.
        for p in [[128.0, 128.0], [40.0, 40.0], [216.0, 60.0], [60.0, 216.0]] {
            let a = t.apply(p);
            let b = truth.apply(p);
            let err = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
            assert!(err <= 1.0, "{p:?}: {a:?} vs {b:?} ({err} px), {got:?}");
        }
        assert!(got.inliers >= MIN_INLIERS);
    }

    #[test]
    fn feature_matching_recovers_a_scale_change() {
        let reference = scene(256, 256);
        let truth = Similarity {
            scale: 1.1,
            angle: 0.0,
            tx: -12.0,
            ty: -10.0,
        };
        let moving = through(&reference, truth);
        let t = estimate(&reference, &moving, AlignMethod::Similarity)
            .unwrap()
            .transform;
        assert!((t.scale - 1.1).abs() < 0.02, "{t:?}");
        let a = t.apply([128.0, 128.0]);
        let b = truth.apply([128.0, 128.0]);
        assert!((a[0] - b[0]).abs() <= 1.0 && (a[1] - b[1]).abs() <= 1.0);
    }

    #[test]
    fn phase_correlation_recovers_a_translation() {
        let reference = scene(200, 160);
        let truth = Similarity {
            scale: 1.0,
            angle: 0.0,
            tx: 13.0,
            ty: -7.0,
        };
        let moving = through(&reference, truth);
        let t = estimate(&reference, &moving, AlignMethod::Translation)
            .unwrap()
            .transform;
        assert!((t.tx - 13.0).abs() <= 1.0, "{t:?}");
        assert!((t.ty + 7.0).abs() <= 1.0, "{t:?}");
    }

    #[test]
    fn a_featureless_pair_is_refused_not_guessed() {
        let flat = FilterBuffer::filled(64, 64, [0.5, 0.5, 0.5, 1.0]).unwrap();
        assert!(matches!(
            estimate(&flat, &flat, AlignMethod::Similarity),
            Err(AlignError::NotEnoughMatches(_))
        ));
    }

    #[test]
    fn the_fft_round_trips() {
        let mut re: Vec<f64> = (0..16).map(|i| (i as f64 * 0.7).sin()).collect();
        let orig = re.clone();
        let mut im = vec![0.0; 16];
        fft(&mut re, &mut im, false);
        fft(&mut re, &mut im, true);
        for (a, b) in re.iter().zip(&orig) {
            assert!((a / 16.0 - b).abs() < 1e-9);
        }
    }

    #[test]
    fn to_cols_matches_apply() {
        let s = Similarity {
            scale: 1.3,
            angle: 0.4,
            tx: 5.0,
            ty: -2.0,
        };
        let m = s.to_cols();
        let p = [3.0f64, 7.0];
        let q = s.apply(p);
        let x = f64::from(m[0]) * p[0] + f64::from(m[2]) * p[1] + f64::from(m[4]);
        let y = f64::from(m[1]) * p[0] + f64::from(m[3]) * p[1] + f64::from(m[5]);
        assert!((x - q[0]).abs() < 1e-4 && (y - q[1]).abs() < 1e-4);
    }
}
