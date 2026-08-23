//! Sharpening.
//!
//! Both filters here are unsharp masking: add back a scaled copy of what a
//! blur removed. Because the buffer is linear light, the added detail is
//! *light*, not gamma-encoded code values — sharpening gamma-encoded pixels is
//! what produces the characteristic dark halo on the shadow side of an edge
//! and a weak one on the highlight side.
//!
//! Both can overshoot, so both re-clamp the result back into a valid
//! premultiplied pixel: alpha into `[0, 1]` and each colour channel into
//! `[0, alpha]`. Edge handling is the caller's [`EdgeMode`], inherited by the
//! internal blur.

use crate::blur::gaussian_blur;
use crate::buffer::{clamp_premultiplied, FilterBuffer};
use crate::support::{fill_tiles, smoothstep, sub, EdgeMode};

/// Unsharp mask.
///
/// `out = src + amount * (src - blur(src, radius))`, applied only where the
/// local contrast exceeds `threshold`.
///
/// * `amount` — strength; `1.0` doubles the detail. Negative values soften.
/// * `radius` — the blur sigma in pixels; larger radii sharpen coarser
///   structure.
/// * `threshold` — minimum linear-light difference (largest of the four
///   channels) before a pixel is touched at all. Raising it leaves flat areas
///   — and therefore noise and skin — alone. The gate is smooth over
///   `threshold .. 2 * threshold` so it does not carve visible contours into a
///   gradient.
///
/// A zero amount or a non-positive radius is the identity.
pub fn unsharp_mask(
    src: &FilterBuffer,
    amount: f32,
    radius: f32,
    threshold: f32,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty()
        || !amount.is_finite()
        || amount == 0.0
        || !radius.is_finite()
        || radius <= 0.0
    {
        return src.clone();
    }
    let blurred = gaussian_blur(src, radius, edge);
    let (w, h) = src.dimensions();
    let t = if threshold.is_finite() {
        threshold.max(0.0)
    } else {
        0.0
    };
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = src.get(x, y);
        let d = sub(s, blurred.get(x, y));
        let mag = d.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let gate = if t <= 0.0 {
            1.0
        } else {
            smoothstep(t, t * 2.0, mag)
        };
        let k = amount * gate;
        clamp_premultiplied([
            s[0] + d[0] * k,
            s[1] + d[1] * k,
            s[2] + d[2] * k,
            s[3] + d[3] * k,
        ])
    });
    out
}

/// Smart sharpen: unsharp masking whose strength follows local detail.
///
/// A plain unsharp mask amplifies sensor noise exactly as eagerly as it
/// amplifies edges. Here the amount at each pixel is scaled by
/// `sd / (sd + noise_floor)`, where `sd` is the standard deviation of linear
/// luminance over the 3x3 neighbourhood. Flat, noisy areas have a small `sd`
/// and are left nearly untouched; real edges have a large one and sharpen at
/// close to the full amount.
///
/// `noise_floor` is in linear-luminance units — the standard deviation you
/// consider "just noise". Zero disables the gate and makes this a plain
/// [`unsharp_mask`] with no threshold.
///
/// A constant image has `sd == 0` everywhere and is returned unchanged.
pub fn smart_sharpen(
    src: &FilterBuffer,
    amount: f32,
    radius: f32,
    noise_floor: f32,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty()
        || !amount.is_finite()
        || amount == 0.0
        || !radius.is_finite()
        || radius <= 0.0
    {
        return src.clone();
    }
    let blurred = gaussian_blur(src, radius, edge);
    let (w, h) = src.dimensions();
    let floor = if noise_floor.is_finite() {
        noise_floor.max(0.0)
    } else {
        0.0
    };
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = src.get(x, y);
        let d = sub(s, blurred.get(x, y));
        let gate = if floor <= 0.0 {
            1.0
        } else {
            let sd = local_luma_sd(src, x, y, edge);
            sd / (sd + floor)
        };
        let k = amount * gate;
        clamp_premultiplied([
            s[0] + d[0] * k,
            s[1] + d[1] * k,
            s[2] + d[2] * k,
            s[3] + d[3] * k,
        ])
    });
    out
}

/// Standard deviation of linear luminance over the 3x3 neighbourhood.
///
/// Luminance is taken on the premultiplied values: a partly transparent pixel
/// genuinely carries less light, and sharpening it as though it were opaque
/// would pull colour out of the transparent side of an antialiased edge.
fn local_luma_sd(src: &FilterBuffer, x: u32, y: u32, edge: EdgeMode) -> f32 {
    let mut sum = 0.0f64;
    let mut sum2 = 0.0f64;
    for oy in -1i64..=1 {
        for ox in -1i64..=1 {
            let p = src.at(x as i64 + ox, y as i64 + oy, edge);
            let l = color::linear_srgb_luminance([p[0], p[1], p[2]]) as f64;
            sum += l;
            sum2 += l * l;
        }
    }
    let n = 9.0;
    let mean = sum / n;
    ((sum2 / n - mean * mean).max(0.0)).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONST_PX: [f32; 4] = [0.21, 0.34, 0.55, 0.8];

    fn constant(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, CONST_PX).unwrap()
    }

    /// An image with a single hard edge and flat plateaux either side.
    fn step(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.2f32 } else { 0.6 };
                px.push([v, v, v, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    fn noisy_flat(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let n = if (x * 7 + y * 13) % 3 == 0 {
                    0.01
                } else {
                    -0.01
                };
                let v = 0.4 + n;
                px.push([v, v, v, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn sharpening_a_constant_image_changes_nothing() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let src = constant(19, 11);
            let a = unsharp_mask(&src, 2.0, 2.0, 0.0, edge);
            let b = smart_sharpen(&src, 2.0, 2.0, 0.01, edge);
            for i in 0..src.len() {
                for (c, expected) in CONST_PX.iter().enumerate() {
                    assert!((a.pixels()[i][c] - expected).abs() < 1e-5, "unsharp");
                    assert!((b.pixels()[i][c] - expected).abs() < 1e-5, "smart");
                }
            }
        }
    }

    /// The behaviour that makes it a sharpen at all: contrast across the edge
    /// must increase.
    #[test]
    fn unsharp_mask_increases_edge_contrast() {
        let src = step(20, 4);
        let out = unsharp_mask(&src, 1.5, 2.0, 0.0, EdgeMode::Clamp);
        let dark = out.get(9, 2)[0];
        let light = out.get(10, 2)[0];
        assert!(dark < src.get(9, 2)[0], "dark side did not darken: {dark}");
        assert!(
            light > src.get(10, 2)[0],
            "light side did not lighten: {light}"
        );
        assert!(light - dark > 0.4, "contrast only {}", light - dark);
    }

    /// The threshold is the whole point of the parameter: below it, nothing
    /// happens.
    #[test]
    fn threshold_leaves_low_contrast_detail_alone() {
        let src = noisy_flat(16, 16);
        let sharpened = unsharp_mask(&src, 3.0, 1.5, 0.0, EdgeMode::Clamp);
        let gated = unsharp_mask(&src, 3.0, 1.5, 0.2, EdgeMode::Clamp);
        assert_ne!(sharpened, src, "no threshold should amplify the noise");
        assert_eq!(gated, src, "a high threshold must leave it untouched");
    }

    /// Smart sharpen's reason to exist: strong on an edge, weak on noise, with
    /// the same parameters.
    #[test]
    fn smart_sharpen_amplifies_edges_more_than_noise() {
        let floor = 0.02;
        let edge_src = step(20, 6);
        let noise_src = noisy_flat(20, 20);

        let edge_out = smart_sharpen(&edge_src, 2.0, 1.5, floor, EdgeMode::Clamp);
        let noise_out = smart_sharpen(&noise_src, 2.0, 1.5, floor, EdgeMode::Clamp);

        let edge_gain = (edge_out.get(10, 3)[0] - edge_src.get(10, 3)[0]).abs();
        let noise_gain: f32 = (0..noise_src.len())
            .map(|i| (noise_out.pixels()[i][0] - noise_src.pixels()[i][0]).abs())
            .fold(0.0, f32::max);
        assert!(
            edge_gain > noise_gain * 4.0,
            "edge {edge_gain} vs noise {noise_gain}"
        );
    }

    #[test]
    fn a_zero_noise_floor_is_a_plain_unsharp_mask() {
        let src = step(12, 3);
        assert_eq!(
            smart_sharpen(&src, 1.0, 1.5, 0.0, EdgeMode::Clamp),
            unsharp_mask(&src, 1.0, 1.5, 0.0, EdgeMode::Clamp)
        );
    }

    #[test]
    fn results_stay_valid_premultiplied_pixels() {
        // 50% alpha layer: sharpening must not push colour above alpha, which
        // would be an impossible premultiplied pixel.
        let mut px = Vec::new();
        for x in 0..16u32 {
            let v = if x < 8 { 0.0f32 } else { 0.5 };
            px.push([v, v * 0.5, 0.0, 0.5]);
        }
        let src = FilterBuffer::from_pixels(16, 1, px).unwrap();
        let out = unsharp_mask(&src, 4.0, 2.0, 0.0, EdgeMode::Clamp);
        for p in out.pixels() {
            assert!((0.0..=1.0).contains(&p[3]), "alpha {p:?}");
            for c in 0..3 {
                assert!(p[c] >= 0.0 && p[c] <= p[3] + 1e-6, "channel {c} of {p:?}");
            }
        }
    }

    #[test]
    fn identity_parameters_and_degenerate_sizes_do_not_panic() {
        let src = step(9, 5);
        assert_eq!(unsharp_mask(&src, 0.0, 2.0, 0.0, EdgeMode::Clamp), src);
        assert_eq!(unsharp_mask(&src, 1.0, 0.0, 0.0, EdgeMode::Clamp), src);
        assert_eq!(smart_sharpen(&src, 0.0, 2.0, 0.1, EdgeMode::Clamp), src);
        assert_eq!(smart_sharpen(&src, 1.0, -1.0, 0.1, EdgeMode::Clamp), src);

        let one = constant(1, 1);
        assert!(!unsharp_mask(&one, 2.0, 3.0, 0.0, EdgeMode::Wrap).is_empty());
        assert!(!smart_sharpen(&one, 2.0, 3.0, 0.1, EdgeMode::Mirror).is_empty());

        let empty = FilterBuffer::transparent(7, 0).unwrap();
        assert!(unsharp_mask(&empty, 2.0, 2.0, 0.0, EdgeMode::Clamp).is_empty());
        assert!(smart_sharpen(&empty, 2.0, 2.0, 0.1, EdgeMode::Clamp).is_empty());

        for v in [f32::NAN, f32::INFINITY] {
            assert_eq!(unsharp_mask(&src, v, 2.0, 0.0, EdgeMode::Clamp), src);
            assert_eq!(unsharp_mask(&src, 1.0, v, 0.0, EdgeMode::Clamp), src);
            assert!(!unsharp_mask(&src, 1.0, 2.0, v, EdgeMode::Clamp).is_empty());
            assert!(!smart_sharpen(&src, 1.0, 2.0, v, EdgeMode::Clamp).is_empty());
        }
    }
}
