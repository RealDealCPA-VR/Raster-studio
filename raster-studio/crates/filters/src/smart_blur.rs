//! Smart Blur: a hard-thresholded neighbourhood average with an edge overlay.
//!
//! [`crate::blur::surface_blur`] weights neighbours by how similar they are;
//! Smart Blur is the older, blunter tool: a neighbour is either inside the
//! threshold and averaged, or outside it and ignored. That gives the flat,
//! posterised look the filter is known for, and its two edge modes draw the
//! boundaries it found.
//!
//! The similarity test is on **straight** (unpremultiplied) values, like the
//! other edge-aware filters in this crate, so coverage does not scale the
//! decision; the average itself is over premultiplied pixels.

use serde::{Deserialize, Serialize};

use crate::blur::straight_plane;
use crate::buffer::FilterBuffer;
use crate::support::{accumulate, fill_tiles, max_abs_diff, scale, EdgeMode};

/// Largest Smart Blur radius. The filter is `O(r^2)` per pixel.
pub const MAX_SMART_BLUR_RADIUS: u32 = 64;

/// What [`smart_blur`] draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SmartBlurMode {
    /// The thresholded average.
    #[default]
    Normal,
    /// Only the edges the threshold found: white on black.
    EdgeOnly,
    /// The thresholded average with the found edges drawn white over it.
    OverlayEdge,
}

/// Smart Blur.
///
/// * `radius` — half the window side, in pixels; zero is the identity in
///   `Normal` mode.
/// * `threshold` — the largest straight-value difference (any channel) a
///   neighbour may have from the centre and still be averaged in. An edge is a
///   4-neighbour difference of at least this much.
/// * `mode` — see [`SmartBlurMode`].
///
/// The centre pixel is always inside its own threshold, so the average never
/// divides by zero and a constant image is preserved.
pub fn smart_blur(
    src: &FilterBuffer,
    radius: u32,
    threshold: f32,
    mode: SmartBlurMode,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let t = if threshold.is_finite() {
        threshold.max(0.0)
    } else {
        0.0
    };
    if mode == SmartBlurMode::Normal && radius == 0 {
        return src.clone();
    }
    let r = radius.min(MAX_SMART_BLUR_RADIUS) as i64;
    let (w, h) = src.dimensions();
    let straight = straight_plane(src);
    let sw = w as usize;
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let center = straight[y as usize * sw + x as usize];
        let alpha = src.get(x, y)[3];
        let is_edge = || {
            [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .any(
                    |(ox, oy)| match (edge.map(x as i64 + ox, w), edge.map(y as i64 + oy, h)) {
                        (Some(sx), Some(sy)) => max_abs_diff(straight[sy * sw + sx], center) >= t,
                        _ => false,
                    },
                )
        };
        if mode == SmartBlurMode::EdgeOnly {
            let e = if is_edge() { alpha } else { 0.0 };
            return [e, e, e, alpha];
        }
        if mode == SmartBlurMode::OverlayEdge && is_edge() {
            return [alpha, alpha, alpha, alpha];
        }
        let mut acc = [0.0f32; 4];
        let mut count = 0.0f32;
        for oy in -r..=r {
            for ox in -r..=r {
                let (Some(sx), Some(sy)) = (edge.map(x as i64 + ox, w), edge.map(y as i64 + oy, h))
                else {
                    continue;
                };
                if max_abs_diff(straight[sy * sw + sx], center) > t {
                    continue;
                }
                accumulate(&mut acc, src.get(sx as u32, sy as u32), 1.0);
                count += 1.0;
            }
        }
        if count > 0.0 {
            scale(acc, 1.0 / count)
        } else {
            src.get(x, y)
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hard step with grain on both plateaux.
    fn grainy_step(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let base = if x < w / 2 { 0.2f32 } else { 0.7 };
                let n = (crate::rng::hash_unit(3, x as i64, y as i64) - 0.5) * 0.04;
                let v = base + n;
                px.push([v, v, v, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn normal_mode_smooths_the_grain_and_keeps_the_step() {
        let src = grainy_step(32, 8);
        let out = smart_blur(&src, 3, 0.1, SmartBlurMode::Normal, EdgeMode::Clamp);
        // Grain on the left plateau is gone: every pixel within 1e-2 of 0.2.
        let left_spread = (0..8u32)
            .flat_map(|y| (0..12u32).map(move |x| (x, y)))
            .map(|(x, y)| (out.get(x, y)[0] - 0.2).abs())
            .fold(0.0f32, f32::max);
        let src_spread = (0..8u32)
            .flat_map(|y| (0..12u32).map(move |x| (x, y)))
            .map(|(x, y)| (src.get(x, y)[0] - 0.2).abs())
            .fold(0.0f32, f32::max);
        assert!(
            left_spread < src_spread * 0.5,
            "{left_spread} vs {src_spread}"
        );
        // The step is still a step: the two sides did not bleed into each
        // other, because the threshold is below the 0.5 jump.
        assert!(out.get(15, 4)[0] < 0.3);
        assert!(out.get(16, 4)[0] > 0.6);
    }

    #[test]
    fn edge_only_draws_the_step_and_nothing_else() {
        let src = grainy_step(32, 8);
        let out = smart_blur(&src, 3, 0.1, SmartBlurMode::EdgeOnly, EdgeMode::Clamp);
        for y in 0..8u32 {
            for x in 0..32u32 {
                let p = out.get(x, y);
                assert_eq!(p[0], p[1]);
                assert_eq!(p[1], p[2]);
                let expect_edge = x == 15 || x == 16;
                assert_eq!(p[0] > 0.5, expect_edge, "({x},{y}) = {p:?}");
            }
        }
    }

    #[test]
    fn overlay_edge_is_normal_plus_white_at_the_step() {
        let src = grainy_step(32, 8);
        let normal = smart_blur(&src, 3, 0.1, SmartBlurMode::Normal, EdgeMode::Clamp);
        let overlay = smart_blur(&src, 3, 0.1, SmartBlurMode::OverlayEdge, EdgeMode::Clamp);
        assert_eq!(overlay.get(15, 2), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(overlay.get(4, 2), normal.get(4, 2));
    }

    #[test]
    fn a_constant_image_survives_and_a_zero_radius_is_the_identity() {
        let flat = FilterBuffer::filled(9, 9, [0.21, 0.34, 0.55, 0.8]).unwrap();
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let out = smart_blur(&flat, 4, 0.05, SmartBlurMode::Normal, edge);
            for p in out.pixels() {
                for (v, want) in p.iter().zip(flat.pixels()[0].iter()) {
                    assert!((v - want).abs() < 1e-5);
                }
            }
        }
        let src = grainy_step(10, 4);
        assert_eq!(
            smart_blur(&src, 0, 0.1, SmartBlurMode::Normal, EdgeMode::Clamp).pixels(),
            src.pixels()
        );
        let _ = smart_blur(
            &FilterBuffer::transparent(0, 0).unwrap(),
            3,
            f32::NAN,
            SmartBlurMode::OverlayEdge,
            EdgeMode::Wrap,
        );
        let one = FilterBuffer::filled(1, 1, [0.5; 4]).unwrap();
        assert_eq!(
            smart_blur(&one, 500, 0.1, SmartBlurMode::EdgeOnly, EdgeMode::Mirror).dimensions(),
            (1, 1)
        );
    }
}
