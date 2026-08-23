//! The filters that do not belong to a family: high pass, offset,
//! morphological minimum and maximum, and arbitrary NxN convolution.
//!
//! All of them work on premultiplied linear pixels. Boundary behaviour is the
//! caller's [`EdgeMode`]; [`offset`] is the one filter whose *natural* default
//! is [`EdgeMode::Wrap`], since offsetting a tile is how a seamless texture is
//! checked.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::blur::gaussian_blur;
use crate::buffer::{clamp_premultiplied, FilterBuffer, FilterError};
use crate::support::{accumulate, fill_rows, fill_tiles, EdgeMode};

/// Largest morphology radius accepted.
pub const MAX_MORPHOLOGY_RADIUS: u32 = 512;

/// High pass: keep what a blur would remove, over a neutral grey.
///
/// `out.rgb = src.rgb - blur(src).rgb + 0.5 * alpha`. Small `radius` keeps
/// only fine detail; large `radius` keeps progressively more. Alpha is passed
/// through unchanged.
///
/// The neutral is `0.5 * alpha`, not `0.5`, because the buffer is
/// premultiplied — mid grey at 40% coverage is `0.2`. A constant image
/// therefore flattens to exactly that neutral, whatever the radius or edge
/// mode.
///
/// **The result is clamped** into `[0, alpha]` per channel — one of the
/// filters the [`FilterBuffer`] invariant means when it says "only filters
/// that can overshoot re-clamp, and they say so". `src - blur(src)` reaches
/// `±alpha` at an isolated impulse, so an opaque white speck in a black field
/// leaves the neutral at `1.5` and its surround below zero. A negative
/// premultiplied channel is not a tolerable out-of-gamut value the way a
/// scene-referred highlight is: it subtracts light from whatever is composited
/// underneath.
///
/// A non-positive radius is the identity.
pub fn high_pass(src: &FilterBuffer, radius: f32, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() || !radius.is_finite() || radius <= 0.0 {
        return src.clone();
    }
    let blurred = gaussian_blur(src, radius, edge);
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = src.get(x, y);
        let b = blurred.get(x, y);
        let grey = 0.5 * s[3];
        clamp_premultiplied([
            s[0] - b[0] + grey,
            s[1] - b[1] + grey,
            s[2] - b[2] + grey,
            s[3],
        ])
    });
    out
}

/// Shift the image by `(dx, dy)` pixels, resolving what moves in through
/// `edge`.
///
/// With [`EdgeMode::Wrap`] this is a torus rotation: nothing is lost, and
/// offsetting by half the buffer in both axes brings the four corners together
/// in the middle, which is how a seamless tile is checked. With
/// [`EdgeMode::Clamp`] the vacated strip is filled by smearing the edge, and
/// with [`EdgeMode::Mirror`] by reflecting it.
///
/// A zero offset is the identity. Offsets of any magnitude are legal; huge
/// ones are resolved by the edge rule rather than overflowing.
pub fn offset(src: &FilterBuffer, dx: i64, dy: i64, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() || (dx == 0 && dy == 0) {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        src.at(
            (x as i64).wrapping_sub(dx),
            (y as i64).wrapping_sub(dy),
            edge,
        )
    });
    out
}

/// Morphological erosion over a `(2 * radius + 1)` square: each channel takes
/// the minimum of its neighbourhood.
///
/// Separable — a square structuring element erodes the same way whether you do
/// both axes at once or one after the other — and `O(1)` per pixel via a
/// monotonic deque, so the cost does not grow with the radius.
///
/// Applied per channel to the premultiplied values, which is what makes it
/// shrink a layer's silhouette as well as darken it: the alpha channel is
/// eroded along with the colour. A zero radius is the identity.
pub fn minimum(src: &FilterBuffer, radius: u32, edge: EdgeMode) -> FilterBuffer {
    morphology(src, radius, edge, true)
}

/// Morphological dilation: the maximum over a `(2 * radius + 1)` square. The
/// exact counterpart of [`minimum`]; see it for the details.
pub fn maximum(src: &FilterBuffer, radius: u32, edge: EdgeMode) -> FilterBuffer {
    morphology(src, radius, edge, false)
}

fn morphology(src: &FilterBuffer, radius: u32, edge: EdgeMode, take_min: bool) -> FilterBuffer {
    if src.is_empty() || radius == 0 {
        return src.clone();
    }
    let r = radius.min(MAX_MORPHOLOGY_RADIUS);
    let pass = morphology_horizontal(src, r, edge, take_min);
    morphology_horizontal(&pass.transposed(), r, edge, take_min).transposed()
}

fn morphology_horizontal(
    src: &FilterBuffer,
    radius: u32,
    edge: EdgeMode,
    take_min: bool,
) -> FilterBuffer {
    let (w, h) = src.dimensions();
    let r = radius as i64;
    let k = (2 * radius + 1) as usize;
    let n = w as usize + 2 * radius as usize;
    let mut out = src.same_size_blank();
    fill_rows(w, h, out.pixels_mut(), |y, row| {
        let mut ext = vec![0.0f32; n];
        let mut dq: VecDeque<usize> = VecDeque::with_capacity(k);
        // Iterating an explicit list rather than a range: each channel is
        // ranked independently and the index is a channel *name*, not a
        // position in some slice.
        for channel in [0usize, 1, 2, 3] {
            gather_channel(src, y, channel, r, edge, &mut ext);
            dq.clear();
            for i in 0..n {
                // Drop every candidate the newcomer dominates: it is at least
                // as extreme and stays in the window at least as long.
                while let Some(&b) = dq.back() {
                    let dominated = if take_min {
                        ext[b] >= ext[i]
                    } else {
                        ext[b] <= ext[i]
                    };
                    if !dominated {
                        break;
                    }
                    dq.pop_back();
                }
                dq.push_back(i);
                if i + 1 >= k {
                    let start = i + 1 - k;
                    while let Some(&f) = dq.front() {
                        if f >= start {
                            break;
                        }
                        dq.pop_front();
                    }
                    if let Some(&f) = dq.front() {
                        row[start][channel] = ext[f];
                    }
                }
            }
        }
    });
    out
}

/// Copy one channel of scanline `y` into `ext`, extended on both sides by the
/// halo the window needs. Every out-of-range sample goes through `edge`, which
/// is what keeps the morphology from inventing a black or white border.
fn gather_channel(
    src: &FilterBuffer,
    y: u32,
    channel: usize,
    radius: i64,
    edge: EdgeMode,
    ext: &mut [f32],
) {
    for (i, slot) in ext.iter_mut().enumerate() {
        *slot = src.at(i as i64 - radius, y as i64, edge)[channel];
    }
}

/// An arbitrary square convolution kernel.
///
/// The kernel must be square and odd-sized so it has a well-defined centre
/// tap; an even kernel would shift the image by half a pixel, which is a bug
/// rather than a feature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kernel {
    size: u32,
    weights: Vec<f32>,
    divisor: f32,
    bias: f32,
}

impl Kernel {
    /// Build a kernel from row-major weights.
    ///
    /// The divisor defaults to the sum of the weights, so a kernel written as
    /// small integers — the way convolution kernels are conventionally
    /// tabulated — is automatically normalised and preserves image brightness.
    /// A zero sum (an edge-detection kernel, say) falls back to a divisor of
    /// one rather than dividing by zero.
    pub fn new(size: u32, weights: Vec<f32>) -> Result<Self, FilterError> {
        let expected = (size as usize).saturating_mul(size as usize);
        if size == 0 || size % 2 == 0 || weights.len() != expected {
            return Err(FilterError::BadKernel {
                size,
                len: weights.len(),
            });
        }
        let sum: f32 = weights.iter().sum();
        let divisor = if sum.is_finite() && sum != 0.0 {
            sum
        } else {
            1.0
        };
        Ok(Self {
            size,
            weights,
            divisor,
            bias: 0.0,
        })
    }

    /// Override the divisor. A zero or non-finite divisor is refused, and the
    /// kernel is returned unchanged.
    pub fn with_divisor(mut self, divisor: f32) -> Self {
        if divisor.is_finite() && divisor != 0.0 {
            self.divisor = divisor;
        }
        self
    }

    /// Set the constant added to each colour channel after division.
    ///
    /// The bias is scaled by the source pixel's alpha, so a bias of `0.5`
    /// means "mid grey" — which at 40% coverage is `0.2`, not `0.5`. That
    /// keeps the *bias term* in the premultiplied convention; it does not make
    /// the result a valid premultiplied pixel, because the convolution sum
    /// itself is never clamped. A standard `5 / -1` sharpen kernel on a
    /// 50%-alpha edge already reaches `[1.0, 1.0, 1.0, 0.5]` with no bias at
    /// all. Clamp the output yourself — [`crate::sharpen::unsharp_mask`] is
    /// the sharpening path that does.
    pub fn with_bias(mut self, bias: f32) -> Self {
        if bias.is_finite() {
            self.bias = bias;
        }
        self
    }

    /// Edge length of the kernel.
    pub const fn size(&self) -> u32 {
        self.size
    }

    pub fn weights(&self) -> &[f32] {
        &self.weights
    }

    pub const fn divisor(&self) -> f32 {
        self.divisor
    }

    pub const fn bias(&self) -> f32 {
        self.bias
    }

    /// The kernel that leaves an image alone.
    pub fn identity(size: u32) -> Result<Self, FilterError> {
        let expected = (size as usize).saturating_mul(size as usize);
        if size == 0 || size % 2 == 0 {
            return Err(FilterError::BadKernel { size, len: 0 });
        }
        let mut weights = vec![0.0f32; expected];
        weights[expected / 2] = 1.0;
        Self::new(size, weights)
    }
}

/// Apply an arbitrary convolution kernel.
///
/// Colour channels get `sum / divisor + bias * alpha`; the alpha channel gets
/// `sum / divisor` with no bias, because biasing alpha would change the
/// layer's coverage rather than its appearance.
///
/// The result is **not** clamped: an arbitrary kernel is exactly the place a
/// caller may want the raw signed response, and a kernel with negative weights
/// can legitimately leave the premultiplied range. Pass the output through
/// your own clamp if you need a compositable pixel.
pub fn convolve(src: &FilterBuffer, kernel: &Kernel, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let size = kernel.size as i64;
    let r = size / 2;
    let inv = 1.0 / kernel.divisor;
    let bias = kernel.bias;
    let weights = &kernel.weights;
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let mut acc = [0.0f32; 4];
        for (i, &weight) in weights.iter().enumerate() {
            if weight == 0.0 {
                continue;
            }
            let kx = (i as i64) % size - r;
            let ky = (i as i64) / size - r;
            accumulate(&mut acc, src.at(x as i64 + kx, y as i64 + ky, edge), weight);
        }
        let alpha_bias = bias * src.get(x, y)[3];
        [
            acc[0] * inv + alpha_bias,
            acc[1] * inv + alpha_bias,
            acc[2] * inv + alpha_bias,
            acc[3] * inv,
        ]
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blur::box_blur;

    const CONST_PX: [f32; 4] = [0.21, 0.34, 0.55, 0.8];

    fn constant(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, CONST_PX).unwrap()
    }

    fn checker(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 2 + y / 3) % 2 == 0 { 0.8 } else { 0.2 };
                px.push([v, v * 0.5, v * 0.25, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn high_pass_flattens_a_constant_image_to_neutral() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let src = constant(23, 19);
            let out = high_pass(&src, 4.0, edge);
            for (i, px) in out.pixels().iter().enumerate() {
                for c in 0..3 {
                    assert!(
                        (px[c] - 0.5 * CONST_PX[3]).abs() < 1e-5,
                        "{edge:?} pixel {i}: {px:?}"
                    );
                }
                assert!((px[3] - CONST_PX[3]).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn high_pass_keeps_detail_and_drops_the_base_level() {
        // Two flat plateaux with a fine ripple on each. High pass must keep
        // the ripple and forget which plateau it sat on.
        let (w, h) = (32u32, 4u32);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let base = if x < w / 2 { 0.2f32 } else { 0.8 };
                let ripple = if (x + y) % 2 == 0 { 0.05 } else { -0.05 };
                let v = base + ripple;
                px.push([v, v, v, 1.0]);
            }
        }
        let src = FilterBuffer::from_pixels(w, h, px).unwrap();
        let out = high_pass(&src, 3.0, EdgeMode::Clamp);
        // Well inside each plateau the two sides now sit at the same level.
        // Average over a whole ripple period so the comparison is of the base
        // level, not of which phase of the ripple the probe landed on.
        let mean = |x0: u32| -> f32 { (x0..x0 + 6).map(|x| out.get(x, 1)[0]).sum::<f32>() / 6.0 };
        let (left, right) = (mean(3), mean(23));
        assert!((left - right).abs() < 0.02, "{left} vs {right}");
        assert!((left - 0.5).abs() < 0.02, "base level survived: {left}");
        // ... and the ripple survives.
        assert!((out.get(4, 1)[0] - out.get(5, 1)[0]).abs() > 0.05);
        // The source, by contrast, had a 0.6 gap between the plateaux.
        assert!((src.get(4, 1)[0] - src.get(27, 1)[0]).abs() > 0.5);
    }

    /// An impulse is the worst case for `src - blur(src)`: the speck itself
    /// overshoots past `alpha` and its surround undershoots past zero. Every
    /// output pixel must still satisfy `0 <= colour <= alpha`. Without the
    /// clamp this returns `1.4956` at alpha `1.0`, and negative colour around
    /// the speck.
    #[test]
    fn high_pass_never_emits_an_invalid_premultiplied_pixel() {
        for alpha in [1.0f32, 0.5] {
            // An opaque-white (at this coverage) speck in a black field.
            let mut impulse = FilterBuffer::filled(33, 33, [0.0, 0.0, 0.0, alpha]).unwrap();
            impulse.set(16, 16, [alpha, alpha, alpha, alpha]);
            // ... and a hard edge, the other overshoot case.
            let mut edge_px = Vec::new();
            for _ in 0..8 {
                for x in 0..32u32 {
                    let v = if x < 16 { 0.0f32 } else { 1.0 };
                    edge_px.push([v * alpha, v * alpha, v * alpha, alpha]);
                }
            }
            let hard_edge = FilterBuffer::from_pixels(32, 8, edge_px).unwrap();
            for src in [&impulse, &hard_edge] {
                for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
                    for radius in [1.0f32, 6.0] {
                        let out = high_pass(src, radius, edge);
                        for (i, p) in out.pixels().iter().enumerate() {
                            assert!((p[3] - alpha).abs() < 1e-6, "alpha moved: {p:?}");
                            for c in 0..3 {
                                assert!(
                                    p[c] >= 0.0 && p[c] <= p[3],
                                    "alpha {alpha} r{radius} {edge:?} pixel {i} ch{c}: {p:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn offset_wraps_losslessly() {
        let src = checker(16, 12);
        let moved = offset(&src, 5, -3, EdgeMode::Wrap);
        for y in 0..12i64 {
            for x in 0..16i64 {
                assert_eq!(
                    moved.get(x as u32, y as u32),
                    src.at(x - 5, y + 3, EdgeMode::Wrap),
                    "at {x},{y}"
                );
            }
        }
        // Wrapping is a rotation: undo it and the image is bit-identical.
        assert_eq!(offset(&moved, -5, 3, EdgeMode::Wrap), src);
        // A full-width offset is the identity.
        assert_eq!(offset(&src, 16, 12, EdgeMode::Wrap), src);
    }

    #[test]
    fn offset_respects_the_other_edge_modes() {
        let src = checker(8, 8);
        let clamped = offset(&src, 3, 0, EdgeMode::Clamp);
        // The vacated strip is a smear of the first column.
        for x in 0..3u32 {
            assert_eq!(clamped.get(x, 4), src.get(0, 4), "column {x}");
        }
        let mirrored = offset(&src, 3, 0, EdgeMode::Mirror);
        assert_eq!(mirrored.get(2, 4), src.get(0, 4));
        assert_eq!(mirrored.get(1, 4), src.get(1, 4));
        assert_ne!(mirrored, clamped);
    }

    #[test]
    fn offset_survives_extreme_shifts_and_degenerate_inputs() {
        let src = checker(5, 5);
        assert_eq!(offset(&src, 0, 0, EdgeMode::Wrap), src);
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            assert!(!offset(&src, i64::MAX, i64::MIN, edge).is_empty());
            assert!(!offset(&src, i64::MIN, i64::MAX, edge).is_empty());
        }
        let empty = FilterBuffer::transparent(4, 0).unwrap();
        assert!(offset(&empty, 3, 3, EdgeMode::Wrap).is_empty());
    }

    #[test]
    fn morphology_preserves_a_constant_image() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            for out in [
                minimum(&constant(21, 17), 3, edge),
                maximum(&constant(21, 17), 3, edge),
            ] {
                for (i, px) in out.pixels().iter().enumerate() {
                    for c in 0..4 {
                        assert!((px[c] - CONST_PX[c]).abs() < 1e-6, "{edge:?} pixel {i}");
                    }
                }
            }
        }
    }

    /// The sliding-window implementation must agree with the definition.
    #[test]
    fn morphology_matches_a_brute_force_reference() {
        let src = checker(19, 13);
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            for radius in [1u32, 2, 5] {
                let r = radius as i64;
                let lo = minimum(&src, radius, edge);
                let hi = maximum(&src, radius, edge);
                for y in 0..13i64 {
                    for x in 0..19i64 {
                        let mut expect_lo = [f32::INFINITY; 4];
                        let mut expect_hi = [f32::NEG_INFINITY; 4];
                        for oy in -r..=r {
                            for ox in -r..=r {
                                let p = src.at(x + ox, y + oy, edge);
                                for c in 0..4 {
                                    expect_lo[c] = expect_lo[c].min(p[c]);
                                    expect_hi[c] = expect_hi[c].max(p[c]);
                                }
                            }
                        }
                        assert_eq!(
                            lo.get(x as u32, y as u32),
                            expect_lo,
                            "min {edge:?} r{radius} at {x},{y}"
                        );
                        assert_eq!(
                            hi.get(x as u32, y as u32),
                            expect_hi,
                            "max {edge:?} r{radius} at {x},{y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn minimum_shrinks_and_maximum_grows_a_blob() {
        let mut src = FilterBuffer::transparent(21, 21).unwrap();
        for y in 8..13u32 {
            for x in 8..13u32 {
                src.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let eroded = minimum(&src, 1, EdgeMode::Clamp);
        let dilated = maximum(&src, 1, EdgeMode::Clamp);
        let area = |b: &FilterBuffer| b.pixels().iter().filter(|p| p[3] > 0.5).count();
        assert_eq!(area(&src), 25);
        assert_eq!(area(&eroded), 9, "erosion should shrink the blob");
        assert_eq!(area(&dilated), 49, "dilation should grow it");
    }

    #[test]
    fn morphology_zero_radius_and_degenerate_sizes() {
        let src = checker(7, 7);
        assert_eq!(minimum(&src, 0, EdgeMode::Clamp), src);
        assert_eq!(maximum(&src, 0, EdgeMode::Clamp), src);
        let one = constant(1, 1);
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            assert_eq!(minimum(&one, 9, edge).get(0, 0), CONST_PX);
            assert_eq!(maximum(&one, 9, edge).get(0, 0), CONST_PX);
        }
        let empty = FilterBuffer::transparent(0, 6).unwrap();
        assert!(minimum(&empty, 3, EdgeMode::Clamp).is_empty());
        assert!(maximum(&empty, 3, EdgeMode::Clamp).is_empty());
        assert!(!minimum(&src, u32::MAX, EdgeMode::Wrap).is_empty());
    }

    #[test]
    fn kernel_validates_its_shape() {
        assert!(Kernel::new(3, vec![0.0; 9]).is_ok());
        assert_eq!(
            Kernel::new(2, vec![0.0; 4]).unwrap_err(),
            FilterError::BadKernel { size: 2, len: 4 },
            "even kernels have no centre tap"
        );
        assert!(matches!(
            Kernel::new(3, vec![0.0; 8]),
            Err(FilterError::BadKernel { .. })
        ));
        assert!(matches!(
            Kernel::new(0, vec![]),
            Err(FilterError::BadKernel { .. })
        ));
        assert!(matches!(
            Kernel::identity(4),
            Err(FilterError::BadKernel { .. })
        ));
    }

    #[test]
    fn kernel_divisor_defaults_to_the_weight_sum() {
        let k = Kernel::new(3, vec![1.0; 9]).unwrap();
        assert_eq!(k.divisor(), 9.0);
        // A zero-sum kernel must not divide by zero.
        let edge =
            Kernel::new(3, vec![-1.0, -1.0, -1.0, -1.0, 8.0, -1.0, -1.0, -1.0, 0.0]).unwrap();
        assert_eq!(edge.divisor(), 1.0);
        assert_eq!(k.clone().with_divisor(0.0).divisor(), 9.0, "refused");
        assert_eq!(k.clone().with_divisor(f32::NAN).divisor(), 9.0, "refused");
        assert_eq!(k.clone().with_divisor(2.0).divisor(), 2.0);
        assert_eq!(k.clone().with_bias(f32::NAN).bias(), 0.0, "refused");
        assert_eq!(k.with_bias(0.25).bias(), 0.25);
    }

    #[test]
    fn the_identity_kernel_is_the_identity() {
        let src = checker(11, 9);
        for size in [1u32, 3, 5, 7] {
            let k = Kernel::identity(size).unwrap();
            for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
                let out = convolve(&src, &k, edge);
                for i in 0..src.len() {
                    for c in 0..4 {
                        assert!(
                            (out.pixels()[i][c] - src.pixels()[i][c]).abs() < 1e-6,
                            "size {size} {edge:?} pixel {i}"
                        );
                    }
                }
            }
        }
    }

    /// A uniform kernel is a box blur; checking it against the dedicated
    /// implementation cross-validates both.
    #[test]
    fn a_uniform_kernel_reproduces_the_box_blur() {
        let src = checker(17, 11);
        let k = Kernel::new(5, vec![1.0; 25]).unwrap();
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let a = convolve(&src, &k, edge);
            let b = box_blur(&src, 2, edge);
            for i in 0..src.len() {
                for c in 0..4 {
                    assert!(
                        (a.pixels()[i][c] - b.pixels()[i][c]).abs() < 1e-5,
                        "{edge:?} pixel {i} channel {c}: {} vs {}",
                        a.pixels()[i][c],
                        b.pixels()[i][c]
                    );
                }
            }
        }
    }

    #[test]
    fn convolution_bias_scales_with_alpha_and_skips_it() {
        // A zero kernel plus a 0.5 bias: colour becomes mid grey at the
        // pixel's own coverage, and alpha is untouched by the bias.
        let src = FilterBuffer::filled(6, 6, [0.2, 0.2, 0.2, 0.4]).unwrap();
        let k = Kernel::new(3, vec![0.0; 9])
            .unwrap()
            .with_divisor(1.0)
            .with_bias(0.5);
        let out = convolve(&src, &k, EdgeMode::Clamp);
        for px in out.pixels() {
            assert!((px[0] - 0.2).abs() < 1e-6, "{px:?}");
            assert!(
                px[3].abs() < 1e-6,
                "the zero kernel must zero alpha: {px:?}"
            );
        }
    }

    #[test]
    fn convolution_handles_degenerate_sizes() {
        let k = Kernel::new(3, vec![1.0; 9]).unwrap();
        let one = constant(1, 1);
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let out = convolve(&one, &k, edge);
            for (c, expected) in CONST_PX.iter().enumerate() {
                assert!((out.get(0, 0)[c] - expected).abs() < 1e-6, "{edge:?}");
            }
        }
        let empty = FilterBuffer::transparent(3, 0).unwrap();
        assert!(convolve(&empty, &k, EdgeMode::Clamp).is_empty());
        assert!(high_pass(&empty, 3.0, EdgeMode::Clamp).is_empty());
        let src = checker(4, 4);
        assert_eq!(high_pass(&src, 0.0, EdgeMode::Clamp), src);
        assert_eq!(high_pass(&src, f32::NAN, EdgeMode::Clamp), src);
    }
}
