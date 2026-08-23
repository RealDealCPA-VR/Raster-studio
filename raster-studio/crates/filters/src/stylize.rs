//! Stylize: filters that reinterpret the image rather than clean it up.
//!
//! [`emboss`], [`find_edges`], [`oil_paint`] and [`diffuse`] work in linear
//! light on premultiplied pixels, like the rest of the crate. [`solarize`] is
//! the exception and says so: tone reversal is defined on the **gamma-encoded**
//! value, and applying it to linear light puts the fold at a completely
//! different tone than every reference implementation.
//!
//! Edge handling is the caller's [`EdgeMode`] throughout.

use color::{linear_srgb_luminance, linear_to_srgb, premultiply, srgb_to_linear, unpremultiply};
use serde::{Deserialize, Serialize};

use crate::buffer::{clamp_premultiplied, FilterBuffer};
use crate::rng::hash_unit;
use crate::support::{fill_rows, fill_tiles, lerp_px, scale, EdgeMode, Sampling};

/// Emboss: replace the image with a directional derivative over a neutral
/// grey.
///
/// `out.rgb = 0.5 * alpha + amount * (upwind - downwind)`, sampled `height`
/// pixels either side of the destination along `angle_deg` (counter-clockwise
/// from +x, y downwards). Alpha is passed through untouched — embossing alpha
/// would eat the layer's silhouette.
///
/// The neutral is `0.5 * alpha` rather than `0.5` because the buffer is
/// premultiplied: mid grey at 40% coverage is `0.2`, not `0.5`.
///
/// **The result is clamped** into `[0, alpha]` per channel — this is one of
/// the filters the [`FilterBuffer`] invariant means when it says "only filters
/// that can overshoot re-clamp, and they say so". A relief is a *difference*,
/// and across a hard black-to-white edge `amount * (upwind - downwind)` swings
/// a full unit either side of the neutral. Overshoot above `1.0` would be a
/// tolerable scene-referred highlight, but the undershoot would not: a
/// negative premultiplied channel *subtracts* light from the layers beneath
/// when the compositor blends this one, which is not what a dark relief looks
/// like.
///
/// A constant image has a zero derivative and comes out flat grey. A zero
/// height or a zero amount is the identity.
pub fn emboss(
    src: &FilterBuffer,
    angle_deg: f32,
    height: f32,
    amount: f32,
    sampling: Sampling,
) -> FilterBuffer {
    if src.is_empty()
        || !height.is_finite()
        || height <= 0.0
        || !amount.is_finite()
        || amount == 0.0
        || !angle_deg.is_finite()
    {
        return src.clone();
    }
    let a = angle_deg.to_radians();
    let (dx, dy) = (a.cos() * height, -a.sin() * height);
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let cx = x as f32 + 0.5;
        let cy = y as f32 + 0.5;
        let up = src.sample(cx - dx, cy - dy, sampling);
        let down = src.sample(cx + dx, cy + dy, sampling);
        let alpha = src.get(x, y)[3];
        let grey = 0.5 * alpha;
        clamp_premultiplied([
            grey + amount * (up[0] - down[0]),
            grey + amount * (up[1] - down[1]),
            grey + amount * (up[2] - down[2]),
            alpha,
        ])
    });
    out
}

/// Find edges: a Sobel gradient magnitude, drawn dark on white.
///
/// The magnitude is computed per colour channel on premultiplied values and
/// subtracted from the pixel's own alpha, so an opaque flat area comes out
/// white and an edge comes out dark — the familiar "pencil on paper" look. The
/// result is clamped into `[0, alpha]` so it stays a valid premultiplied
/// pixel. Alpha itself is passed through.
pub fn find_edges(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    const GX: [[f32; 3]; 3] = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
    const GY: [[f32; 3]; 3] = [[-1.0, -2.0, -1.0], [0.0, 0.0, 0.0], [1.0, 2.0, 1.0]];
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let mut gx = [0.0f32; 3];
        let mut gy = [0.0f32; 3];
        for (j, (rx, ry)) in GX.iter().zip(GY.iter()).enumerate() {
            for (i, (kx, ky)) in rx.iter().zip(ry.iter()).enumerate() {
                let p = src.at(x as i64 + i as i64 - 1, y as i64 + j as i64 - 1, edge);
                for c in 0..3 {
                    gx[c] += p[c] * kx;
                    gy[c] += p[c] * ky;
                }
            }
        }
        let alpha = src.get(x, y)[3];
        let mut px = [0.0f32, 0.0, 0.0, alpha];
        for c in 0..3 {
            let mag = (gx[c] * gx[c] + gy[c] * gy[c]).sqrt();
            px[c] = (alpha - mag).clamp(0.0, alpha);
        }
        px
    });
    out
}

/// Largest oil-paint / stylize window radius accepted.
pub const MAX_STYLIZE_RADIUS: u32 = 64;

/// Oil paint: replace each pixel with the average colour of the most common
/// brightness band in its neighbourhood.
///
/// Brightness is quantised into `levels` bands; whichever band has the most
/// members in the window wins, and the output is the mean of *its* pixels
/// only. Averaging the whole window would just be a box blur — restricting the
/// average to the dominant band is what produces flat, brush-like patches with
/// hard boundaries.
///
/// A radius of zero, or fewer than two levels, is the identity.
pub fn oil_paint(src: &FilterBuffer, radius: u32, levels: u32, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() || radius == 0 || levels < 2 {
        return src.clone();
    }
    let r = radius.min(MAX_STYLIZE_RADIUS) as i64;
    let n = levels.min(256) as usize;
    let (w, h) = src.dimensions();
    // Luminance of the straight colour: a partly transparent pixel should band
    // by its colour, not by its coverage.
    let luma: Vec<f32> = src
        .pixels()
        .iter()
        .map(|p| {
            let s = unpremultiply(*p);
            linear_srgb_luminance([s[0], s[1], s[2]]).clamp(0.0, 1.0)
        })
        .collect();
    let sw = w as usize;
    let mut out = src.same_size_blank();
    // The band tallies are allocated once per scanline, not once per pixel: at
    // 256 levels a per-pixel allocation dominates the whole filter.
    fill_rows(w, h, out.pixels_mut(), |y, row| {
        let mut counts = vec![0u32; n];
        let mut sums = vec![[0.0f32; 4]; n];
        for (x, slot) in row.iter_mut().enumerate() {
            counts.iter_mut().for_each(|c| *c = 0);
            sums.iter_mut().for_each(|s| *s = [0.0; 4]);
            for oy in -r..=r {
                let Some(sy) = edge.map(y as i64 + oy, h) else {
                    continue;
                };
                for ox in -r..=r {
                    let Some(sx) = edge.map(x as i64 + ox, w) else {
                        continue;
                    };
                    let idx = sy * sw + sx;
                    let band = ((luma[idx] * n as f32) as usize).min(n - 1);
                    counts[band] += 1;
                    let p = src.pixels()[idx];
                    for (acc, v) in sums[band].iter_mut().zip(p.iter()) {
                        *acc += v;
                    }
                }
            }
            let best = counts
                .iter()
                .enumerate()
                .max_by_key(|(_, c)| **c)
                .map(|(i, _)| i)
                .unwrap_or(0);
            *slot = if counts[best] == 0 {
                src.get(x as u32, y)
            } else {
                scale(sums[best], 1.0 / counts[best] as f32)
            };
        }
    });
    out
}

/// Solarize: fold the tone curve back on itself above mid grey.
///
/// **Defined on gamma-encoded values.** Each straight-alpha colour channel is
/// encoded to sRGB, mapped by `v < 0.5 ? v : 1 - v`, and decoded again. Doing
/// this in linear light would put the fold at 18% encoded brightness instead
/// of 50%, which looks nothing like the darkroom effect the filter is named
/// after — this is the one place in the crate where leaving linear space is
/// the correct answer.
///
/// Alpha is untouched. Values outside `[0, 1]` are clamped before folding,
/// since the transform is only defined on that range.
pub fn solarize(src: &FilterBuffer) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let mut s = unpremultiply(src.get(x, y));
        for c in s.iter_mut().take(3) {
            let e = linear_to_srgb(c.clamp(0.0, 1.0));
            let folded = if e < 0.5 { e } else { 1.0 - e };
            *c = srgb_to_linear(folded);
        }
        premultiply(s)
    });
    out
}

/// Which way [`wind`] blows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WindDirection {
    /// Streaks trail to the right of an edge.
    #[default]
    FromLeft,
    /// Streaks trail to the left of an edge.
    FromRight,
}

/// Wind: horizontal streaks trailing away from vertical edges.
///
/// A destination pixel blends in pixels from upwind, but only where those
/// pixels sit on an edge — the contribution is gated by the local horizontal
/// luminance gradient and by a per-source-pixel random draw, so the streaks
/// are ragged rather than a uniform smear. Flat areas have no gradient and are
/// left exactly alone, which is why a constant image comes back unchanged.
///
/// `strength` in `0 ..= 1` sets both the streak length (up to 64 pixels) and
/// how readily a pixel streaks. Deterministic in `seed`.
pub fn wind(
    src: &FilterBuffer,
    direction: WindDirection,
    strength: f32,
    seed: u64,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty() || !strength.is_finite() || strength <= 0.0 {
        return src.clone();
    }
    let s = strength.clamp(0.0, 1.0);
    let max_len = ((s * 64.0).ceil() as i64).clamp(1, 64);
    // Upwind is the direction the streak comes *from*.
    let upwind: i64 = match direction {
        WindDirection::FromLeft => -1,
        WindDirection::FromRight => 1,
    };
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let mut acc = src.get(x, y);
        for k in 1..=max_len {
            let sx = x as i64 + upwind * k;
            let here = src.at(sx, y as i64, edge);
            let next = src.at(sx - upwind, y as i64, edge);
            let gradient = (linear_srgb_luminance([here[0], here[1], here[2]])
                - linear_srgb_luminance([next[0], next[1], next[2]]))
            .abs();
            let draw = hash_unit(seed, sx, y as i64);
            if draw >= gradient * s * 4.0 {
                continue;
            }
            let falloff = 0.6 * (1.0 - (k - 1) as f32 / max_len as f32);
            acc = lerp_px(acc, here, falloff);
        }
        acc
    });
    out
}

/// How [`diffuse`] chooses between a pixel and its random neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiffuseMode {
    /// Always take the neighbour.
    #[default]
    Normal,
    /// Take the neighbour only when it is darker.
    DarkenOnly,
    /// Take the neighbour only when it is brighter.
    LightenOnly,
}

/// Diffuse: shuffle each pixel with a random neighbour inside `radius`.
///
/// The neighbour offset is a pure function of `(seed, x, y)`, so the result is
/// reproducible and independent of thread scheduling. A constant image is
/// unchanged whatever the mode, since every candidate is the same colour.
///
/// A zero radius is the identity.
pub fn diffuse(
    src: &FilterBuffer,
    radius: u32,
    mode: DiffuseMode,
    seed: u64,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty() || radius == 0 {
        return src.clone();
    }
    let r = radius.min(MAX_STYLIZE_RADIUS) as i64;
    let span = (2 * r + 1) as f32;
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (xi, yi) = (x as i64, y as i64);
        let ox = (hash_unit(seed, xi, yi) * span) as i64 - r;
        let oy = (hash_unit(seed ^ 0x5851_F42D_4C95_7F2D, xi, yi) * span) as i64 - r;
        let here = src.get(x, y);
        let cand = src.at(xi + ox, yi + oy, edge);
        match mode {
            DiffuseMode::Normal => cand,
            DiffuseMode::DarkenOnly => {
                if luma(cand) < luma(here) {
                    cand
                } else {
                    here
                }
            }
            DiffuseMode::LightenOnly => {
                if luma(cand) > luma(here) {
                    cand
                } else {
                    here
                }
            }
        }
    });
    out
}

#[inline]
fn luma(px: [f32; 4]) -> f32 {
    linear_srgb_luminance([px[0], px[1], px[2]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Interpolation;

    fn opaque_constant(v: f32, w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, [v, v * 0.8, v * 0.5, 1.0]).unwrap()
    }

    fn checker(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 3 + y / 3) % 2 == 0 { 0.85 } else { 0.15 };
                px.push([v, v * 0.6, v * 0.3, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    fn bilinear() -> Sampling {
        Sampling::new(EdgeMode::Clamp, Interpolation::Bilinear)
    }

    /// A constant image has no derivative anywhere, so emboss must flatten it
    /// to exactly the neutral — not to something a little off, which is what a
    /// mishandled boundary produces at the border.
    #[test]
    fn emboss_of_a_constant_image_is_flat_neutral() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let src = opaque_constant(0.7, 21, 17);
            let out = emboss(
                &src,
                135.0,
                2.0,
                1.0,
                Sampling::new(edge, Interpolation::Bilinear),
            );
            for (i, px) in out.pixels().iter().enumerate() {
                for c in 0..3 {
                    assert!((px[c] - 0.5).abs() < 1e-5, "{edge:?} pixel {i}: {px:?}");
                }
                assert_eq!(px[3], 1.0);
            }
        }
    }

    #[test]
    fn emboss_neutral_scales_with_alpha() {
        // A 40%-covered flat layer must emboss to 0.2, the premultiplied form
        // of mid grey at 40% coverage — 0.5 would be an impossible pixel.
        let src = FilterBuffer::filled(8, 8, [0.2, 0.2, 0.2, 0.4]).unwrap();
        let out = emboss(&src, 45.0, 1.0, 1.0, bilinear());
        for px in out.pixels() {
            assert!((px[0] - 0.2).abs() < 1e-6, "{px:?}");
            assert!(
                px[0] <= px[3] + 1e-6,
                "impossible premultiplied pixel {px:?}"
            );
        }
    }

    /// A hard edge is where the relief overshoots hardest. Every output pixel
    /// must still be a valid premultiplied pixel: colour in `[0, alpha]`.
    /// Without the clamp this returns colour down to `-0.5` at alpha `1.0`,
    /// which subtracts light in the compositor instead of rendering dark.
    #[test]
    fn emboss_never_emits_an_invalid_premultiplied_pixel() {
        for alpha in [1.0f32, 0.5, 0.25] {
            let (w, h) = (16u32, 3u32);
            let mut px = Vec::new();
            for _ in 0..h {
                for x in 0..w {
                    // Straight black on the left, straight white on the right,
                    // at a uniform coverage.
                    let v = if x < w / 2 { 0.0f32 } else { 1.0 };
                    px.push([v * alpha, v * alpha, v * alpha, alpha]);
                }
            }
            let src = FilterBuffer::from_pixels(w, h, px).unwrap();
            for angle in [0.0f32, 45.0, 135.0, 270.0] {
                let out = emboss(&src, angle, 2.0, 1.0, Sampling::clamped());
                for (i, p) in out.pixels().iter().enumerate() {
                    assert!((p[3] - alpha).abs() < 1e-6, "alpha moved: {p:?}");
                    for c in 0..3 {
                        assert!(
                            p[c] >= 0.0 && p[c] <= p[3],
                            "alpha {alpha} angle {angle} pixel {i} channel {c}: {p:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn emboss_reverses_with_the_angle() {
        let src = checker(18, 18);
        // The relief is antisymmetric in the light direction only where it
        // does not hit the clamp, so the amount is chosen to keep the largest
        // channel swing (0.7 here) inside `[0, alpha]` about the 0.5 neutral.
        let a = emboss(&src, 0.0, 1.0, 0.5, bilinear());
        let b = emboss(&src, 180.0, 1.0, 0.5, bilinear());
        // Opposite light directions mirror the relief about the neutral.
        for i in 0..src.len() {
            let (pa, pb) = (a.pixels()[i], b.pixels()[i]);
            let grey = src.pixels()[i][3] * 0.5;
            assert!(
                ((pa[0] - grey) + (pb[0] - grey)).abs() < 1e-5,
                "pixel {i}: {pa:?} {pb:?}"
            );
        }
    }

    #[test]
    fn find_edges_leaves_a_flat_area_white_and_darkens_an_edge() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let flat = opaque_constant(0.6, 15, 15);
            let out = find_edges(&flat, edge);
            for (i, px) in out.pixels().iter().enumerate() {
                for c in 0..3 {
                    assert!((px[c] - 1.0).abs() < 1e-5, "{edge:?} pixel {i}: {px:?}");
                }
            }
        }
        let src = checker(18, 18);
        let out = find_edges(&src, EdgeMode::Clamp);
        // Pixel 3,3 sits on a block boundary in this checker.
        assert!(out.get(3, 3)[0] < 0.5, "{:?}", out.get(3, 3));
        // The middle of a block does not.
        assert!(out.get(4, 4)[0] > 0.9, "{:?}", out.get(4, 4));
    }

    #[test]
    fn oil_paint_preserves_a_constant_image() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let src = opaque_constant(0.42, 23, 19);
            let out = oil_paint(&src, 3, 12, edge);
            for (i, px) in out.pixels().iter().enumerate() {
                for c in 0..4 {
                    assert!(
                        (px[c] - src.pixels()[i][c]).abs() < 1e-5,
                        "{edge:?} pixel {i}: {px:?}"
                    );
                }
            }
        }
    }

    /// The distinguishing behaviour: on a two-tone image oil paint returns one
    /// of the two tones, never a blend, because it averages within one band.
    #[test]
    fn oil_paint_does_not_blend_across_bands() {
        let src = checker(24, 24);
        let out = oil_paint(&src, 2, 8, EdgeMode::Clamp);
        for px in out.pixels() {
            let near_light = (px[0] - 0.85).abs() < 1e-4;
            let near_dark = (px[0] - 0.15).abs() < 1e-4;
            assert!(near_light || near_dark, "blended pixel {px:?}");
        }
    }

    /// The *dominant* band wins, and the output is the mean of that band's
    /// pixels alone. This is the one property that separates oil paint from a
    /// box blur, and it is only observable where the winning band is **not**
    /// the centre pixel's own band — otherwise a source-passthrough fallback
    /// or a minority band would satisfy the assertion by accident.
    ///
    /// A 7x7 image with radius 3 puts the whole image in the window of the
    /// centre pixel. Rows 0..4 are dark and rows 4..7 are bright, so dark wins
    /// 27 to 22 once the centre pixel itself is flipped bright. The correct
    /// answer at the centre is the dark mean, 0.1:
    ///   * a minority-band pick (`min_by_key`) finds an empty band and falls
    ///     back to the source pixel, 0.9;
    ///   * averaging the whole window (a box blur) gives ~0.459;
    ///   * returning the centre pixel unchanged gives 0.9.
    #[test]
    fn oil_paint_returns_the_dominant_band_not_the_centre_pixel() {
        const DARK: f32 = 0.1;
        const BRIGHT: f32 = 0.9;
        let mut src = FilterBuffer::filled(7, 7, [DARK, DARK, DARK, 1.0]).unwrap();
        for y in 4..7u32 {
            for x in 0..7u32 {
                src.set(x, y, [BRIGHT, BRIGHT, BRIGHT, 1.0]);
            }
        }
        // The centre pixel belongs to the *losing* band.
        src.set(3, 3, [BRIGHT, BRIGHT, BRIGHT, 1.0]);

        // Sanity: 27 dark against 22 bright inside the 7x7 window.
        let dark_count = src.pixels().iter().filter(|p| p[0] < 0.5).count();
        assert_eq!(dark_count, 27, "test fixture drifted");

        let out = oil_paint(&src, 3, 8, EdgeMode::Clamp);
        let px = out.get(3, 3);
        assert!(
            (px[0] - DARK).abs() < 1e-5,
            "centre must be the dominant (dark) band mean {DARK}, got {px:?}"
        );
        // Explicitly rule out the three wrong answers a mutation would give.
        assert!(
            (px[0] - BRIGHT).abs() > 0.5,
            "centre-pixel passthrough: {px:?}"
        );
        assert!((px[0] - 0.459).abs() > 0.3, "whole-window box blur: {px:?}");
        assert!(px[3] > 0.99, "alpha must survive: {px:?}");
    }

    /// Solarize is defined on encoded values: an encoded 0.75 must fold to
    /// 0.25, whatever that is in linear light.
    #[test]
    fn solarize_folds_the_encoded_curve_at_a_half() {
        let bright = srgb_to_linear(0.75);
        let dark = srgb_to_linear(0.25);
        let src = FilterBuffer::filled(4, 4, [bright, dark, 0.0, 1.0]).unwrap();
        let out = solarize(&src);
        let px = out.get(1, 1);
        assert!(
            (linear_to_srgb(px[0]) - 0.25).abs() < 1e-4,
            "0.75 encoded should fold to 0.25, got {}",
            linear_to_srgb(px[0])
        );
        assert!(
            (linear_to_srgb(px[1]) - 0.25).abs() < 1e-4,
            "0.25 encoded is below the fold and must pass through, got {}",
            linear_to_srgb(px[1])
        );
        assert_eq!(px[3], 1.0);
    }

    /// If solarize folded in linear light instead, mid encoded grey (linear
    /// 0.216) would be nowhere near the fold. This pins that it is not.
    #[test]
    fn solarize_folds_at_encoded_mid_grey_not_linear_mid_grey() {
        let encoded_mid = srgb_to_linear(0.5);
        let src = FilterBuffer::filled(2, 2, [encoded_mid, 0.5, 0.0, 1.0]).unwrap();
        let out = solarize(&src);
        let px = out.get(0, 0);
        // Encoded 0.5 is exactly at the fold: 1 - 0.5 = 0.5, unchanged.
        assert!((linear_to_srgb(px[0]) - 0.5).abs() < 1e-4);
        // Linear 0.5 encodes to about 0.735, which is above the fold and must
        // therefore change.
        assert!(
            (px[1] - 0.5).abs() > 0.1,
            "linear mid grey should have folded: {}",
            px[1]
        );
    }

    #[test]
    fn solarize_preserves_alpha_and_stays_valid() {
        let src = FilterBuffer::filled(4, 4, [0.3, 0.15, 0.05, 0.5]).unwrap();
        let out = solarize(&src);
        for px in out.pixels() {
            assert!((px[3] - 0.5).abs() < 1e-6);
            for c in 0..3 {
                assert!(px[c] >= -1e-6 && px[c] <= px[3] + 1e-6, "{px:?}");
            }
        }
    }

    #[test]
    fn wind_leaves_a_flat_image_alone_but_streaks_an_edge() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let flat = opaque_constant(0.5, 30, 6);
            assert_eq!(
                wind(&flat, WindDirection::FromLeft, 0.8, 4, edge),
                flat,
                "{edge:?}"
            );
        }
        // A vertical edge in the middle must smear to the right, not the left.
        let (w, h) = (40u32, 8u32);
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.9f32 } else { 0.1 };
                px.push([v, v, v, 1.0]);
            }
        }
        let src = FilterBuffer::from_pixels(w, h, px).unwrap();
        let out = wind(&src, WindDirection::FromLeft, 1.0, 11, EdgeMode::Clamp);
        let mut right_changed = 0;
        let mut left_changed = 0;
        for y in 0..h {
            for x in 0..w {
                if (out.get(x, y)[0] - src.get(x, y)[0]).abs() > 1e-4 {
                    if x >= w / 2 {
                        right_changed += 1;
                    } else {
                        left_changed += 1;
                    }
                }
            }
        }
        assert!(right_changed > 0, "nothing streaked downwind");
        assert_eq!(left_changed, 0, "pixels upwind of the edge must not move");
    }

    #[test]
    fn wind_is_deterministic() {
        let src = checker(32, 8);
        // A modest strength keeps the per-pixel draw in play; at full
        // strength every edge pixel streaks regardless of the seed and the
        // test would prove nothing about determinism.
        let a = wind(&src, WindDirection::FromRight, 0.2, 5, EdgeMode::Clamp);
        let b = wind(&src, WindDirection::FromRight, 0.2, 5, EdgeMode::Clamp);
        let c = wind(&src, WindDirection::FromRight, 0.2, 6, EdgeMode::Clamp);
        assert_eq!(a.pixels(), b.pixels(), "same seed must be reproducible");
        let differing = (0..src.len())
            .filter(|&i| a.pixels()[i] != c.pixels()[i])
            .count();
        assert!(differing > 0, "a different seed must change the streaks");
    }

    #[test]
    fn diffuse_preserves_a_constant_image_in_every_mode() {
        for mode in [
            DiffuseMode::Normal,
            DiffuseMode::DarkenOnly,
            DiffuseMode::LightenOnly,
        ] {
            for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
                let src = opaque_constant(0.33, 20, 20);
                assert_eq!(diffuse(&src, 4, mode, 9, edge), src, "{mode:?} {edge:?}");
            }
        }
    }

    #[test]
    fn diffuse_modes_respect_their_direction() {
        let src = checker(24, 24);
        let darken = diffuse(&src, 3, DiffuseMode::DarkenOnly, 2, EdgeMode::Clamp);
        let lighten = diffuse(&src, 3, DiffuseMode::LightenOnly, 2, EdgeMode::Clamp);
        for i in 0..src.len() {
            assert!(
                luma(darken.pixels()[i]) <= luma(src.pixels()[i]) + 1e-6,
                "darken brightened pixel {i}"
            );
            assert!(
                luma(lighten.pixels()[i]) >= luma(src.pixels()[i]) - 1e-6,
                "lighten darkened pixel {i}"
            );
        }
        assert_ne!(darken, src);
        assert_ne!(lighten, src);
    }

    #[test]
    fn diffuse_is_deterministic_and_stays_in_the_radius() {
        let src = checker(16, 16);
        assert_eq!(
            diffuse(&src, 2, DiffuseMode::Normal, 77, EdgeMode::Clamp),
            diffuse(&src, 2, DiffuseMode::Normal, 77, EdgeMode::Clamp)
        );
        // Every output pixel must be *some* source pixel, unmodified.
        let out = diffuse(&src, 2, DiffuseMode::Normal, 77, EdgeMode::Clamp);
        for px in out.pixels() {
            assert!(
                src.pixels().contains(px),
                "diffuse invented a colour: {px:?}"
            );
        }
    }

    #[test]
    fn identity_parameters_and_degenerate_sizes_do_not_panic() {
        let src = checker(9, 7);
        assert_eq!(emboss(&src, 45.0, 0.0, 1.0, bilinear()), src);
        assert_eq!(emboss(&src, 45.0, 2.0, 0.0, bilinear()), src);
        assert_eq!(emboss(&src, f32::NAN, 2.0, 1.0, bilinear()), src);
        assert_eq!(oil_paint(&src, 0, 8, EdgeMode::Clamp), src);
        assert_eq!(oil_paint(&src, 3, 1, EdgeMode::Clamp), src);
        assert_eq!(
            wind(&src, WindDirection::FromLeft, 0.0, 1, EdgeMode::Clamp),
            src
        );
        assert_eq!(
            diffuse(&src, 0, DiffuseMode::Normal, 1, EdgeMode::Clamp),
            src
        );

        let one = opaque_constant(0.5, 1, 1);
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let s = Sampling::new(edge, Interpolation::Bicubic);
            assert!(!emboss(&one, 30.0, 5.0, 2.0, s).is_empty());
            assert!(!find_edges(&one, edge).is_empty());
            assert!(!oil_paint(&one, 9, 20, edge).is_empty());
            assert!(!solarize(&one).is_empty());
            assert!(!wind(&one, WindDirection::FromRight, 1.0, 1, edge).is_empty());
            assert!(!diffuse(&one, 9, DiffuseMode::Normal, 1, edge).is_empty());
        }

        let empty = FilterBuffer::transparent(0, 9).unwrap();
        assert!(emboss(&empty, 1.0, 1.0, 1.0, bilinear()).is_empty());
        assert!(find_edges(&empty, EdgeMode::Clamp).is_empty());
        assert!(oil_paint(&empty, 3, 8, EdgeMode::Clamp).is_empty());
        assert!(solarize(&empty).is_empty());
        assert!(wind(&empty, WindDirection::FromLeft, 1.0, 1, EdgeMode::Clamp).is_empty());
        assert!(diffuse(&empty, 3, DiffuseMode::Normal, 1, EdgeMode::Clamp).is_empty());
    }

    #[test]
    fn absurd_radii_are_clamped_not_fatal() {
        let src = checker(6, 6);
        assert!(!oil_paint(&src, u32::MAX, u32::MAX, EdgeMode::Wrap).is_empty());
        assert!(!diffuse(&src, u32::MAX, DiffuseMode::Normal, 1, EdgeMode::Mirror).is_empty());
        assert!(!wind(&src, WindDirection::FromLeft, 1e30, 1, EdgeMode::Clamp).is_empty());
    }
}
