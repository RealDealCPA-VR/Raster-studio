//! Blurs.
//!
//! Every blur here is a **weighted average with weights summing to one**, so
//! blurring a constant image returns that constant exactly — the single test
//! that catches both a mis-normalised kernel and a mishandled boundary. All of
//! them work on premultiplied linear pixels, where a weighted average *is* the
//! correct composite of the covered pixels.
//!
//! Boundary behaviour is the caller's [`EdgeMode`] throughout, defaulting to
//! [`EdgeMode::Clamp`].

use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::{accumulate, fill_rows, fill_tiles, max_abs_diff, scale, EdgeMode, Sampling};

/// Largest 1D kernel radius a blur will build, so an absurd sigma or radius
/// costs bounded time and memory instead of exhausting the machine.
pub const MAX_BLUR_RADIUS: u32 = 1024;

/// Largest number of taps [`motion_blur`] will average per pixel.
pub const MAX_MOTION_TAPS: u32 = 4096;

/// A normalised 1D Gaussian kernel for `sigma`, of odd length.
///
/// The kernel is truncated at three sigma (which holds 99.7% of the mass),
/// divided by its sum, and then the residual `1 - sum` is added to the centre
/// tap so the weights sum to one to within a single ulp of that tap. Skipping
/// that last step is how repeated blurs slowly darken or brighten an image.
///
/// A non-positive or non-finite sigma yields the identity kernel `[1.0]`.
pub fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    if !sigma.is_finite() || sigma <= 0.0 {
        return vec![1.0];
    }
    let radius = ((sigma * 3.0).ceil() as u32).clamp(1, MAX_BLUR_RADIUS) as usize;
    let inv = -0.5 / (sigma * sigma);
    let mut k: Vec<f32> = (0..=2 * radius)
        .map(|i| {
            let d = i as f32 - radius as f32;
            (d * d * inv).exp()
        })
        .collect();
    let sum: f32 = k.iter().sum();
    if sum > 0.0 {
        for v in &mut k {
            *v /= sum;
        }
    }
    let residual = 1.0 - k.iter().sum::<f32>();
    k[radius] += residual;
    k
}

/// Convolve every row with a 1D kernel. The kernel's centre tap is
/// `kernel[kernel.len() / 2]`.
pub fn convolve_horizontal(src: &FilterBuffer, kernel: &[f32], edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() || kernel.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let r = (kernel.len() / 2) as i64;
    let mut out = src.same_size_blank();
    fill_rows(w, h, out.pixels_mut(), |y, row| {
        for (x, slot) in row.iter_mut().enumerate() {
            let mut acc = [0.0f32; 4];
            for (k, &wk) in kernel.iter().enumerate() {
                let sx = x as i64 + k as i64 - r;
                accumulate(&mut acc, src.at(sx, y as i64, edge), wk);
            }
            *slot = acc;
        }
    });
    out
}

/// Convolve every column with a 1D kernel.
///
/// Implemented as transpose → [`convolve_horizontal`] → transpose so there is
/// exactly one 1D convolution in the crate. A separately written column loop
/// is the usual home of an edge bug that shows on only one axis.
pub fn convolve_vertical(src: &FilterBuffer, kernel: &[f32], edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() || kernel.is_empty() {
        return src.clone();
    }
    convolve_horizontal(&src.transposed(), kernel, edge).transposed()
}

/// Separable Gaussian blur: one horizontal pass then one vertical pass.
///
/// Cost is `O(sigma)` per pixel, not `O(sigma^2)`. The two passes are
/// mathematically identical to convolving with the outer product of the 1D
/// kernel with itself, which `separable_matches_the_2d_reference` checks
/// against a brute-force 2D convolution.
pub fn gaussian_blur(src: &FilterBuffer, sigma: f32, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() || !sigma.is_finite() || sigma <= 0.0 {
        return src.clone();
    }
    let k = gaussian_kernel(sigma);
    convolve_vertical(&convolve_horizontal(src, &k, edge), &k, edge)
}

/// Box blur over a `(2 * radius + 1)` square, separable and `O(1)` per pixel.
///
/// The running sums are accumulated in `f64`, so the constant-image identity
/// survives a wide box: an `f32` prefix sum over a few thousand samples loses
/// enough low bits to show as banding.
///
/// A radius of zero is the identity.
pub fn box_blur(src: &FilterBuffer, radius: u32, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() || radius == 0 {
        return src.clone();
    }
    let r = radius.min(MAX_BLUR_RADIUS);
    box_blur_horizontal(&box_blur_horizontal(src, r, edge).transposed(), r, edge).transposed()
}

fn box_blur_horizontal(src: &FilterBuffer, radius: u32, edge: EdgeMode) -> FilterBuffer {
    let (w, h) = src.dimensions();
    let r = radius as i64;
    let window = (2 * radius + 1) as f64;
    let extended = w as usize + 2 * radius as usize;
    let mut out = src.same_size_blank();
    fill_rows(w, h, out.pixels_mut(), |y, row| {
        let mut prefix = vec![[0.0f64; 4]; extended + 1];
        for i in 0..extended {
            let p = src.at(i as i64 - r, y as i64, edge);
            let prev = prefix[i];
            prefix[i + 1] = [
                prev[0] + p[0] as f64,
                prev[1] + p[1] as f64,
                prev[2] + p[2] as f64,
                prev[3] + p[3] as f64,
            ];
        }
        for (x, slot) in row.iter_mut().enumerate() {
            let hi = prefix[x + 2 * radius as usize + 1];
            let lo = prefix[x];
            for (c, s) in slot.iter_mut().enumerate() {
                *s = ((hi[c] - lo[c]) / window) as f32;
            }
        }
    });
    out
}

/// Directional (motion) blur.
///
/// Averages `ceil(distance)` evenly spaced taps along a line of `distance`
/// pixels centred on the destination pixel, at `angle_deg` measured
/// counter-clockwise from the +x axis in screen space (y grows downwards).
/// Taps are resampled with `sampling`, so the streak is smooth rather than
/// stair-stepped.
///
/// The tap count is clamped to [`MAX_MOTION_TAPS`]. Past that the streak still
/// spans the requested `distance`, but the taps are spaced more than a pixel
/// apart and the trail can read as a row of discrete ghosts rather than a
/// continuous smear.
///
/// A non-positive distance is the identity.
pub fn motion_blur(
    src: &FilterBuffer,
    angle_deg: f32,
    distance: f32,
    sampling: Sampling,
) -> FilterBuffer {
    if src.is_empty() || !distance.is_finite() || distance <= 0.0 {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let taps = (distance.ceil() as u32).clamp(1, MAX_MOTION_TAPS);
    let a = angle_deg.to_radians();
    let (dx, dy) = (a.cos(), -a.sin());
    let inv = 1.0 / taps as f32;
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let cx = x as f32 + 0.5;
        let cy = y as f32 + 0.5;
        let mut acc = [0.0f32; 4];
        for i in 0..taps {
            let t = ((i as f32 + 0.5) * inv - 0.5) * distance;
            accumulate(
                &mut acc,
                src.sample(cx + dx * t, cy + dy * t, sampling),
                inv,
            );
        }
        acc
    });
    out
}

/// Which way [`radial_blur`] smears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RadialBlurKind {
    /// Rotate about the centre: an arc smear.
    Spin,
    /// Scale about the centre: a streak towards or away from it.
    Zoom,
}

/// Parameters for [`radial_blur`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RadialBlur {
    pub kind: RadialBlurKind,
    /// Centre in pixel coordinates, where `(0.5, 0.5)` is the first pixel's
    /// centre.
    pub center: (f32, f32),
    /// Total sweep in **degrees** for [`RadialBlurKind::Spin`], or the total
    /// relative scale change for [`RadialBlurKind::Zoom`] (`0.1` means the
    /// taps span 0.95x to 1.05x).
    pub amount: f32,
    /// Number of taps. Clamped to `1..=256`.
    pub samples: u32,
    pub sampling: Sampling,
}

impl RadialBlur {
    /// A spin blur centred on a buffer of the given size.
    pub fn spin(width: u32, height: u32, degrees: f32) -> Self {
        Self {
            kind: RadialBlurKind::Spin,
            center: (width as f32 * 0.5, height as f32 * 0.5),
            amount: degrees,
            samples: 16,
            sampling: Sampling::clamped(),
        }
    }

    /// A zoom blur centred on a buffer of the given size.
    pub fn zoom(width: u32, height: u32, amount: f32) -> Self {
        Self {
            kind: RadialBlurKind::Zoom,
            center: (width as f32 * 0.5, height as f32 * 0.5),
            amount,
            samples: 16,
            sampling: Sampling::clamped(),
        }
    }
}

/// Spin or zoom blur about a centre point.
///
/// Taps are evenly spaced over the sweep and equally weighted, so a constant
/// image is preserved. A zero amount, or a single sample, is the identity.
pub fn radial_blur(src: &FilterBuffer, params: &RadialBlur) -> FilterBuffer {
    if src.is_empty() || !params.amount.is_finite() || params.amount == 0.0 {
        return src.clone();
    }
    let n = params.samples.clamp(1, 256);
    if n == 1 {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let (cx, cy) = params.center;
    let inv = 1.0 / n as f32;
    let span = match params.kind {
        RadialBlurKind::Spin => params.amount.to_radians(),
        RadialBlurKind::Zoom => params.amount,
    };
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let px = x as f32 + 0.5 - cx;
        let py = y as f32 + 0.5 - cy;
        let mut acc = [0.0f32; 4];
        for i in 0..n {
            // Symmetric about zero, so the sweep is centred on the
            // destination pixel. With an odd `samples` the middle tap *is*
            // the untouched pixel; with an even one the two central taps
            // straddle it.
            let t = (i as f32 / (n - 1) as f32 - 0.5) * span;
            let (sx, sy) = match params.kind {
                RadialBlurKind::Spin => {
                    let (s, c) = t.sin_cos();
                    (px * c - py * s, px * s + py * c)
                }
                RadialBlurKind::Zoom => {
                    let k = 1.0 + t;
                    (px * k, py * k)
                }
            };
            accumulate(&mut acc, src.sample(cx + sx, cy + sy, params.sampling), inv);
        }
        acc
    });
    out
}

/// Lens (bokeh) blur: an average over a disc, or over a regular polygon when
/// `blades >= 3`, mimicking an iris.
///
/// Unlike a Gaussian this has a hard-edged kernel, which is what turns a
/// highlight into a disc or a hexagon rather than a soft blob. Cost is
/// `O(radius^2)` — inherent, the shape is not separable.
///
/// `rotation_deg` turns the polygon; it has no effect on a disc. A radius
/// below half a pixel is the identity.
pub fn lens_blur(
    src: &FilterBuffer,
    radius: f32,
    blades: u32,
    rotation_deg: f32,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty() || !radius.is_finite() || radius < 0.5 {
        return src.clone();
    }
    let r = radius.min(MAX_BLUR_RADIUS as f32);
    let ri = r.ceil() as i64;
    let offsets = iris_offsets(r, ri, blades, rotation_deg);
    if offsets.is_empty() {
        return src.clone();
    }
    let weight = 1.0 / offsets.len() as f32;
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let mut acc = [0.0f32; 4];
        for &(ox, oy) in &offsets {
            accumulate(&mut acc, src.at(x as i64 + ox, y as i64 + oy, edge), weight);
        }
        acc
    });
    out
}

/// Integer offsets inside the iris shape.
fn iris_offsets(r: f32, ri: i64, blades: u32, rotation_deg: f32) -> Vec<(i64, i64)> {
    let mut offsets = Vec::new();
    let poly = if blades >= 3 {
        let n = blades.min(24);
        let rot = rotation_deg.to_radians();
        let apothem = r * (core::f32::consts::PI / n as f32).cos();
        let normals: Vec<(f32, f32)> = (0..n)
            .map(|k| {
                let a = rot + core::f32::consts::TAU * k as f32 / n as f32;
                (a.cos(), a.sin())
            })
            .collect();
        Some((normals, apothem))
    } else {
        None
    };
    for oy in -ri..=ri {
        for ox in -ri..=ri {
            let (fx, fy) = (ox as f32, oy as f32);
            let inside = match &poly {
                Some((normals, apothem)) => normals
                    .iter()
                    .all(|(nx, ny)| fx * nx + fy * ny <= *apothem + 1e-4),
                None => fx * fx + fy * fy <= r * r,
            };
            if inside {
                offsets.push((ox, oy));
            }
        }
    }
    offsets
}

/// Edge-preserving (surface) blur.
///
/// A box window whose weights fall off with how different a neighbour is from
/// the centre pixel: `w = max(0, 1 - d / threshold)`, where `d` is the largest
/// per-channel difference in **straight** (unpremultiplied) linear values.
/// Neighbours across an edge contribute nothing, so flat areas smooth and
/// edges stay put.
///
/// The centre pixel always has weight one, so the weight sum is never zero and
/// a constant image is preserved exactly. A zero radius or a non-positive
/// threshold is the identity.
pub fn surface_blur(
    src: &FilterBuffer,
    radius: u32,
    threshold: f32,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty() || radius == 0 || !threshold.is_finite() || threshold <= 0.0 {
        return src.clone();
    }
    let r = radius.min(MAX_BLUR_RADIUS) as i64;
    let (w, h) = src.dimensions();
    let straight = straight_plane(src);
    let sw = w as usize;
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let center = straight[y as usize * sw + x as usize];
        let mut acc = [0.0f32; 4];
        let mut total = 0.0f32;
        for oy in -r..=r {
            for ox in -r..=r {
                let (Some(sx), Some(sy)) = (edge.map(x as i64 + ox, w), edge.map(y as i64 + oy, h))
                else {
                    continue;
                };
                let d = max_abs_diff(straight[sy * sw + sx], center);
                let weight = 1.0 - d / threshold;
                if weight <= 0.0 {
                    continue;
                }
                accumulate(&mut acc, src.get(sx as u32, sy as u32), weight);
                total += weight;
            }
        }
        if total > 0.0 {
            scale(acc, 1.0 / total)
        } else {
            src.get(x, y)
        }
    });
    out
}

/// Unpremultiplied copy of the buffer, used by the edge-aware filters so the
/// "are these pixels different?" test is not confounded by alpha.
///
/// Concretely: on premultiplied values every colour difference is scaled by
/// coverage, so the same edge measures four times smaller on a 25%-alpha layer
/// than on an opaque one and slips under the threshold that was protecting it.
/// Comparing straight values makes the decision depend on the colour alone —
/// which is what [`surface_blur`] and [`crate::noise::reduce_noise`] both
/// promise. Coverage is still part of the comparison (the alpha channel is one
/// of the four), so a genuine coverage step is still an edge.
pub(crate) fn straight_plane(src: &FilterBuffer) -> Vec<[f32; 4]> {
    src.pixels()
        .iter()
        .map(|p| color::unpremultiply(*p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Interpolation;

    pub(crate) const CONST_PX: [f32; 4] = [0.21, 0.34, 0.55, 0.8];

    fn constant(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, CONST_PX).unwrap()
    }

    fn impulse(w: u32, h: u32) -> FilterBuffer {
        let mut b = FilterBuffer::transparent(w, h).unwrap();
        b.set(w / 2, h / 2, [1.0, 1.0, 1.0, 1.0]);
        b
    }

    fn checker(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 0.9 } else { 0.1 };
                px.push([v * 0.5, v, v * 0.25, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    fn assert_constant(buf: &FilterBuffer, what: &str) {
        for (i, px) in buf.pixels().iter().enumerate() {
            for c in 0..4 {
                assert!(
                    (px[c] - CONST_PX[c]).abs() < 1e-5,
                    "{what}: pixel {i} channel {c} drifted: {px:?}"
                );
            }
        }
    }

    /// The kernel must sum to one *in the precision the convolution actually
    /// uses*, which is f32 — checking the f64 widening would pass on a kernel
    /// that drifts every time it is applied.
    ///
    /// This is what pins `gaussian_kernel`'s residual-to-centre-tap step.
    /// Dividing by the f32 sum alone leaves 3e-7 at sigma 7 and 7e-7 at sigma
    /// 100, because the division rounds 241 (or 601) taps independently;
    /// folding `1 - sum` back into the centre tap brings every kernel here to
    /// within one ulp of 1.0.
    #[test]
    fn gaussian_kernel_sums_to_one() {
        for sigma in [0.3, 0.5, 1.0, 2.5, 7.0, 40.0, 100.0] {
            let k = gaussian_kernel(sigma);
            assert_eq!(k.len() % 2, 1, "kernel must be odd-length");
            // Two ulp of 1.0. The uncorrected kernel misses this at every
            // sigma above about 5.
            let sum: f32 = k.iter().sum();
            assert!(
                (sum - 1.0).abs() <= 1.2e-7,
                "sigma {sigma}: {} taps sum to {sum:e}, error {:e}",
                k.len(),
                (sum - 1.0).abs()
            );
        }
    }

    #[test]
    fn gaussian_kernel_is_symmetric_and_peaked_at_the_centre() {
        let k = gaussian_kernel(2.0);
        let r = k.len() / 2;
        for i in 0..r {
            assert!(
                (k[i] - k[k.len() - 1 - i]).abs() < 1e-7,
                "asymmetric at {i}"
            );
            assert!(k[i] < k[r], "centre must be the largest tap");
        }
    }

    #[test]
    fn non_positive_sigma_gives_the_identity_kernel() {
        for s in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(gaussian_kernel(s), vec![1.0], "sigma {s}");
        }
    }

    /// The headline invariant: every blur of a constant image is that
    /// constant, under every edge mode. Catches normalisation and boundary
    /// bugs in one shot.
    #[test]
    fn every_blur_preserves_a_constant_image() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let src = constant(37, 23);
            assert_constant(&gaussian_blur(&src, 3.5, edge), "gaussian");
            assert_constant(&box_blur(&src, 4, edge), "box");
            assert_constant(&lens_blur(&src, 5.0, 0, 0.0, edge), "lens disc");
            assert_constant(&lens_blur(&src, 5.0, 6, 15.0, edge), "lens hexagon");
            assert_constant(&surface_blur(&src, 3, 0.2, edge), "surface");
            let sampling = Sampling::new(edge, Interpolation::Bilinear);
            assert_constant(&motion_blur(&src, 27.0, 9.0, sampling), "motion");
            let mut spin = RadialBlur::spin(37, 23, 30.0);
            spin.sampling = sampling;
            assert_constant(&radial_blur(&src, &spin), "radial spin");
            let mut zoom = RadialBlur::zoom(37, 23, 0.3);
            zoom.sampling = sampling;
            assert_constant(&radial_blur(&src, &zoom), "radial zoom");
        }
    }

    /// A blur applied over and over must not drift. This is what an
    /// unnormalised kernel actually costs.
    #[test]
    fn repeated_gaussian_does_not_drift() {
        let mut b = constant(24, 24);
        for _ in 0..40 {
            b = gaussian_blur(&b, 2.0, EdgeMode::Clamp);
        }
        assert_constant(&b, "40 stacked gaussians");
    }

    /// Known impulse response: blurring a single white pixel must reproduce
    /// the separable Gaussian kernel's outer product, and conserve total
    /// energy.
    #[test]
    fn gaussian_impulse_response_is_the_kernel_outer_product() {
        let sigma = 1.5f32;
        let k = gaussian_kernel(sigma);
        let r = k.len() / 2;
        let n = 2 * r as u32 + 9;
        let src = impulse(n, n);
        let out = gaussian_blur(&src, sigma, EdgeMode::Clamp);
        let (cx, cy) = (n / 2, n / 2);
        for j in 0..k.len() {
            for i in 0..k.len() {
                let x = cx as i64 + i as i64 - r as i64;
                let y = cy as i64 + j as i64 - r as i64;
                let expect = k[i] * k[j];
                let got = out.get(x as u32, y as u32);
                assert!(
                    (got[3] - expect).abs() < 1e-6,
                    "tap {i},{j}: expected {expect}, got {}",
                    got[3]
                );
            }
        }
        // Energy is conserved: the impulse spreads, it does not evaporate.
        let total: f64 = out.pixels().iter().map(|p| p[3] as f64).sum();
        assert!((total - 1.0).abs() < 1e-4, "energy {total}");
    }

    /// Two 1D passes must equal one brute-force 2D convolution with the outer
    /// product of the kernel. If they differ, the passes are not separable —
    /// usually because the second pass reads the source instead of the
    /// intermediate, or handles edges differently.
    #[test]
    fn separable_matches_the_2d_reference() {
        let k = gaussian_kernel(1.2);
        let r = (k.len() / 2) as i64;
        let src = checker(17, 13);
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let separable = gaussian_blur(&src, 1.2, edge);
            let (w, h) = src.dimensions();
            for y in 0..h {
                for x in 0..w {
                    let mut acc = [0.0f32; 4];
                    for (j, &kj) in k.iter().enumerate() {
                        for (i, &ki) in k.iter().enumerate() {
                            let p = src.at(x as i64 + i as i64 - r, y as i64 + j as i64 - r, edge);
                            accumulate(&mut acc, p, ki * kj);
                        }
                    }
                    let got = separable.get(x, y);
                    for c in 0..4 {
                        assert!(
                            (got[c] - acc[c]).abs() < 1e-5,
                            "{edge:?} at {x},{y} ch{c}: {} vs {}",
                            got[c],
                            acc[c]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn box_blur_matches_a_brute_force_average() {
        let src = checker(11, 9);
        let r = 2i64;
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let out = box_blur(&src, r as u32, edge);
            for y in 0..9i64 {
                for x in 0..11i64 {
                    let mut acc = [0.0f64; 4];
                    let mut n = 0.0f64;
                    for oy in -r..=r {
                        for ox in -r..=r {
                            let p = src.at(x + ox, y + oy, edge);
                            for c in 0..4 {
                                acc[c] += p[c] as f64;
                            }
                            n += 1.0;
                        }
                    }
                    let got = out.get(x as u32, y as u32);
                    for c in 0..4 {
                        assert!(
                            (got[c] as f64 - acc[c] / n).abs() < 1e-5,
                            "{edge:?} at {x},{y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn convolve_vertical_is_the_transpose_of_convolve_horizontal() {
        let src = checker(9, 6);
        let k = gaussian_kernel(1.0);
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let v = convolve_vertical(&src, &k, edge);
            let h = convolve_horizontal(&src.transposed(), &k, edge).transposed();
            assert_eq!(v, h, "{edge:?}");
        }
    }

    #[test]
    fn zero_amount_blurs_are_the_identity() {
        let src = checker(8, 8);
        assert_eq!(gaussian_blur(&src, 0.0, EdgeMode::Clamp), src);
        assert_eq!(box_blur(&src, 0, EdgeMode::Clamp), src);
        assert_eq!(lens_blur(&src, 0.0, 6, 0.0, EdgeMode::Clamp), src);
        assert_eq!(surface_blur(&src, 0, 1.0, EdgeMode::Clamp), src);
        assert_eq!(surface_blur(&src, 3, 0.0, EdgeMode::Clamp), src);
        assert_eq!(
            motion_blur(&src, 45.0, 0.0, Sampling::clamped()),
            src,
            "zero distance"
        );
        let mut p = RadialBlur::spin(8, 8, 0.0);
        p.samples = 32;
        assert_eq!(radial_blur(&src, &p), src);
    }

    #[test]
    fn blurs_survive_a_one_pixel_image() {
        let src = FilterBuffer::filled(1, 1, CONST_PX).unwrap();
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            assert_constant(&gaussian_blur(&src, 6.0, edge), "1x1 gaussian");
            assert_constant(&box_blur(&src, 9, edge), "1x1 box");
            assert_constant(&lens_blur(&src, 8.0, 5, 0.0, edge), "1x1 lens");
            assert_constant(&surface_blur(&src, 4, 0.5, edge), "1x1 surface");
            let s = Sampling::new(edge, Interpolation::Bicubic);
            assert_constant(&motion_blur(&src, 12.0, 20.0, s), "1x1 motion");
            let mut p = RadialBlur::zoom(1, 1, 0.9);
            p.sampling = s;
            assert_constant(&radial_blur(&src, &p), "1x1 radial");
        }
    }

    #[test]
    fn blurs_survive_an_empty_image() {
        let src = FilterBuffer::transparent(0, 12).unwrap();
        assert!(gaussian_blur(&src, 4.0, EdgeMode::Clamp).is_empty());
        assert!(box_blur(&src, 4, EdgeMode::Clamp).is_empty());
        assert!(lens_blur(&src, 4.0, 6, 0.0, EdgeMode::Clamp).is_empty());
        assert!(surface_blur(&src, 4, 0.3, EdgeMode::Clamp).is_empty());
        assert!(motion_blur(&src, 4.0, 4.0, Sampling::clamped()).is_empty());
        assert!(radial_blur(&src, &RadialBlur::spin(0, 12, 20.0)).is_empty());
    }

    /// Surface blur exists to *not* smooth across an edge. A step image must
    /// keep its two plateaux exactly when the step is larger than the
    /// threshold, while a Gaussian of the same radius smears it.
    #[test]
    fn surface_blur_preserves_a_step_a_gaussian_destroys() {
        let (w, h) = (16u32, 4u32);
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.1 } else { 0.9 };
                px.push([v, v, v, 1.0]);
            }
        }
        let src = FilterBuffer::from_pixels(w, h, px).unwrap();
        let surface = surface_blur(&src, 3, 0.2, EdgeMode::Clamp);
        let gauss = gaussian_blur(&src, 3.0, EdgeMode::Clamp);
        for y in 0..h {
            for x in 0..w {
                assert!(
                    (surface.get(x, y)[0] - src.get(x, y)[0]).abs() < 1e-5,
                    "surface blur crossed the edge at {x},{y}"
                );
            }
        }
        let mid = gauss.get(w / 2, h / 2)[0];
        assert!(
            (0.2..0.8).contains(&mid),
            "gaussian should have smeared the step, got {mid}"
        );
    }

    /// The similarity metric runs on **straight** colour, and this is the case
    /// that proves it: the *same* colour step at two different coverages.
    ///
    /// On premultiplied values every colour difference is scaled by alpha, so
    /// a 0.7 step at 25% coverage measures 0.175 and slips under a threshold
    /// that the identical step at full coverage clears — the faint layer's
    /// edges smear while the opaque one's survive. Measuring straight values
    /// makes the decision, and therefore the output *straight* colour,
    /// identical at both coverages. Every other surface-blur test uses alpha
    /// 1.0, where straight and premultiplied values coincide and the bug is
    /// invisible.
    #[test]
    fn surface_blur_is_not_confounded_by_alpha() {
        let (w, h) = (16u32, 5u32);
        let opaque = colour_step(w, h, 1.0);
        let faint = colour_step(w, h, 0.25);
        let a = surface_blur(&opaque, 3, 0.35, EdgeMode::Clamp);
        let b = surface_blur(&faint, 3, 0.35, EdgeMode::Clamp);
        for y in 0..h {
            for x in 0..w {
                let sa = color::unpremultiply(a.get(x, y));
                let sb = color::unpremultiply(b.get(x, y));
                for c in 0..3 {
                    assert!(
                        (sa[c] - sb[c]).abs() < 1e-5,
                        "at {x},{y} ch{c}: 25% coverage gave {} where full coverage gave {}",
                        sb[c],
                        sa[c]
                    );
                }
                assert!((b.get(x, y)[3] - 0.25).abs() < 1e-6, "coverage moved");
            }
        }
        // The shared decision is the edge-preserving one: a 0.7 straight step
        // is past the 0.35 threshold, so the step survives intact.
        for y in 0..h {
            for x in 0..w {
                assert!(
                    (a.get(x, y)[0] - opaque.get(x, y)[0]).abs() < 1e-5,
                    "surface blur crossed the edge at {x},{y}"
                );
            }
        }
        // ... and that is a statement about the threshold, not about the
        // filter declining to do anything: raise it past the step and the
        // faint buffer smooths.
        let smoothed = surface_blur(&faint, 3, 1.0, EdgeMode::Clamp);
        assert!(
            (smoothed.get(7, 2)[0] - faint.get(7, 2)[0]).abs() > 0.01,
            "a threshold above the step should have smoothed it"
        );
    }

    /// A step in straight colour at a uniform coverage. The premultiplied
    /// values scale with `alpha`; the straight ones do not.
    fn colour_step(w: u32, h: u32, alpha: f32) -> FilterBuffer {
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.1f32 } else { 0.8 };
                px.push([v * alpha, v * alpha, v * alpha, alpha]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn lens_blur_iris_is_a_polygon_not_a_disc() {
        let disc = iris_offsets(6.0, 6, 0, 0.0);
        let hexagon = iris_offsets(6.0, 6, 6, 0.0);
        assert!(!disc.is_empty() && !hexagon.is_empty());
        // A hexagon inscribed in the same circle covers less area.
        assert!(
            hexagon.len() < disc.len(),
            "hexagon {} vs disc {}",
            hexagon.len(),
            disc.len()
        );
        // ... but it is not empty or degenerate: area ratio is near
        // 3*sqrt(3)/(2*pi) ~ 0.827.
        let ratio = hexagon.len() as f32 / disc.len() as f32;
        assert!((0.7..0.95).contains(&ratio), "ratio {ratio}");
    }

    #[test]
    fn absurd_radii_are_clamped_not_fatal() {
        let src = checker(4, 4);
        let k = gaussian_kernel(1e30);
        assert!(k.len() <= 2 * MAX_BLUR_RADIUS as usize + 1);
        assert!(!box_blur(&src, u32::MAX, EdgeMode::Wrap).is_empty());
        assert!(!motion_blur(&src, 0.0, 1e30, Sampling::clamped()).is_empty());
    }

    /// Motion blur along the x axis with an even tap count must equal a
    /// horizontal box average of the same width — a second, independent
    /// definition of the same operation.
    #[test]
    fn motion_blur_along_x_matches_a_horizontal_average() {
        let src = checker(12, 3);
        let out = motion_blur(
            &src,
            0.0,
            4.0,
            Sampling::new(EdgeMode::Clamp, Interpolation::Bilinear),
        );
        for y in 0..3i64 {
            for x in 0..12i64 {
                // Taps at -1.5, -0.5, +0.5, +1.5 pixels from centre; each
                // lands on a pixel boundary, so bilinear averages two pixels.
                let mut acc = [0.0f32; 4];
                for t in [-1.5f32, -0.5, 0.5, 1.5] {
                    accumulate(
                        &mut acc,
                        src.sample_bilinear(x as f32 + 0.5 + t, y as f32 + 0.5, EdgeMode::Clamp),
                        0.25,
                    );
                }
                let got = out.get(x as u32, y as u32);
                for c in 0..4 {
                    assert!((got[c] - acc[c]).abs() < 1e-6, "at {x},{y}");
                }
            }
        }
    }
}
