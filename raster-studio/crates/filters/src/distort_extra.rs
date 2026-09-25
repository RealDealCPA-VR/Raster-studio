//! W13-J: Filter ▸ Distort ▸ Kaleidoscope and Dents, with Photopea's
//! controls.
//!
//! Both are inverse-mapped like [`crate::distort`]: every destination pixel
//! asks where in the source it came from and resamples there, so neither
//! leaves holes, and both preserve a constant image exactly.
//!
//! The controls are Photopea's (its `Kale` and `Dnts` descriptors and
//! dialogs): Kaleidoscope has *Mirrors* (2 to 20) and *Angle* (0 to 360
//! degrees) and folds about the image centre; Dents has *Scale*, *Refraction*
//! and *Turbulence*, plus the *Detail* and *Random Seed* its descriptor
//! carries but its dialog does not show. Kaleidoscope's mapping is Photopea's
//! formula; Dents' structure is Photopea's (a constant-length displacement
//! whose direction is a noise field times the turbulence) but its noise is
//! this crate's [`Perlin`], not Photopea's generator, so the same seed does not
//! give Photopea's exact dents.

use crate::buffer::FilterBuffer;
use crate::rng::Perlin;
use crate::support::{fill_tiles, Sampling};

use core::f32::consts::{FRAC_PI_2, TAU};

/// Fewest mirrors Photopea's Kaleidoscope dialog accepts.
pub const MIN_KALEIDOSCOPE_MIRRORS: u32 = 2;
/// Most mirrors Photopea's Kaleidoscope dialog accepts.
pub const MAX_KALEIDOSCOPE_MIRRORS: u32 = 20;

/// Kaleidoscope: fold the image into `mirrors` mirrored pairs of wedges about
/// its centre, the pattern turned by `angle_deg`.
///
/// Photopea's mapping: with `r` the rotation (`angle + 90` degrees) and
/// `f = 360 / mirrors` degrees, a pixel at polar angle `t` reads the source at
/// the same distance from the centre and at angle `u - r`, where `u` is
/// `(t + r) mod f`, reflected to `f - u` when it is past `f / 2`. So each
/// period of `f` degrees is one source wedge of `f / 2` and its mirror, and at
/// angle 0 the source wedge starts straight up and runs clockwise. `mirrors`
/// is clamped to `2 ..= 20`; a non-finite angle is the identity.
pub fn kaleidoscope(
    src: &FilterBuffer,
    mirrors: u32,
    angle_deg: f32,
    sampling: Sampling,
) -> FilterBuffer {
    if src.is_empty() || !angle_deg.is_finite() {
        return src.clone();
    }
    let n = mirrors.clamp(MIN_KALEIDOSCOPE_MIRRORS, MAX_KALEIDOSCOPE_MIRRORS);
    let period = TAU / n as f32;
    let half = 0.5 * period;
    let rot = angle_deg.to_radians() + FRAC_PI_2;
    let (w, h) = src.dimensions();
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
        let r = dx.hypot(dy);
        let mut u = (dy.atan2(dx) + rot).rem_euclid(period);
        if u > half {
            u = period - u;
        }
        let a = u - rot;
        src.sample(cx + r * a.cos(), cy + r * a.sin(), sampling)
    });
    out
}

/// Photopea's Dents descriptor defaults: Scale 25, Refraction 50, Detail 10,
/// Turbulence 10, Random Seed 8438429.
pub const DENTS_DEFAULT_DETAIL: f32 = 10.0;
/// See [`DENTS_DEFAULT_DETAIL`].
pub const DENTS_DEFAULT_SEED: u64 = 8_438_429;

/// Dents: push every pixel a fixed distance in a direction a smooth, seeded
/// noise field chooses.
///
/// Photopea's structure, in pixels (its grid is three pixels a cell, which the
/// constants below fold in):
///
/// * the noise is sampled every `step = 800 / (min(w, h) * scale)` noise units
///   per pixel, so `scale` (0 to 200) sizes the dents relative to the image;
/// * it is a fractal sum of `1 + detail / 10` octaves (fewer when the finest
///   would be finer than a grid cell) at persistence `detail / 100`;
/// * every pixel moves `3 * refraction / 100 / (3 * step)` pixels — the same
///   distance everywhere — in the direction `2 pi * turbulence / 10 * noise`.
///
/// A zero or non-finite scale or refraction is the identity; `turbulence` is
/// clamped to `[0, 100]`. The same seed always gives the same dents.
pub fn dents(
    src: &FilterBuffer,
    scale: f32,
    refraction: f32,
    detail: f32,
    turbulence: f32,
    seed: u64,
    sampling: Sampling,
) -> FilterBuffer {
    let finite_positive = |v: f32| v.is_finite() && v > 0.0;
    if src.is_empty() || !finite_positive(scale) || !finite_positive(refraction) {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    // Photopea: i7 = min(grid w, grid h) / 2, ep = 400 / i7 / scale, in noise
    // units per grid cell of 3 pixels.
    let half_grid = ((w / 3).min(h / 3) as f32 * 0.5).max(0.5);
    let ep = 400.0 / half_grid / scale.min(200.0);
    let per_pixel = ep / 3.0;
    let detail = if detail.is_finite() {
        detail.clamp(0.0, 100.0)
    } else {
        DENTS_DEFAULT_DETAIL
    };
    let wanted = 1.0 + detail / 10.0;
    let finest = (ep.ln() / 0.5f32.ln()).floor();
    let octaves = if wanted > finest && finest >= 1.0 {
        finest
    } else {
        wanted
    };
    let octaves = octaves.round().clamp(1.0, 12.0) as u32;
    let persistence = (detail / 100.0).max(0.01);
    // Grid units -> pixels: the displacement is refraction / 100 / ep cells.
    let distance = 3.0 * refraction.min(200.0) / 100.0 / ep;
    let spin =
        TAU * if turbulence.is_finite() {
            turbulence.clamp(0.0, 100.0)
        } else {
            0.0
        } / 10.0;
    let field = Perlin::new(seed);
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let n = field.fbm(px * per_pixel, py * per_pixel, octaves, persistence);
        let a = spin * n;
        src.sample(
            px + distance * (-a).sin(),
            py + distance * a.cos(),
            sampling,
        )
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{EdgeMode, Interpolation};

    fn nearest() -> Sampling {
        Sampling::new(EdgeMode::Clamp, Interpolation::Nearest)
    }

    fn ramp(w: u32, h: u32) -> FilterBuffer {
        let px = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                [x as f32 / w as f32, y as f32 / h as f32, 0.5, 1.0]
            })
            .collect();
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn kaleidoscope_mirrors_the_source_wedge_exactly() {
        // Two mirrors at angle 0 about the centre of a 16x16 image: the period
        // is 180 degrees and the source wedge is the upper-right quadrant
        // (straight up, clockwise to the +x axis). The upper-left quadrant
        // mirrors it in the vertical axis, the lower-right in the horizontal.
        let src = ramp(16, 16);
        let out = kaleidoscope(&src, 2, 0.0, nearest());
        for y in 0..8u32 {
            for x in 8..16u32 {
                assert_eq!(out.get(x, y), src.get(x, y), "({x}, {y})");
                assert_eq!(out.get(15 - x, y), src.get(x, y), "left of ({x}, {y})");
                assert_eq!(out.get(x, 15 - y), src.get(x, y), "below ({x}, {y})");
            }
        }
        // Rotating by 90 degrees turns the source wedge a quarter turn
        // anticlockwise: the upper-left quadrant is now the source.
        let turned = kaleidoscope(&src, 2, 90.0, nearest());
        assert_ne!(turned, out);
        for y in 0..8u32 {
            for x in 0..8u32 {
                assert_eq!(turned.get(x, y), src.get(x, y), "({x}, {y})");
            }
        }
        // Mirrors are clamped to Photopea's 2 ..= 20.
        assert_eq!(kaleidoscope(&src, 0, 0.0, nearest()), out);
        assert_eq!(
            kaleidoscope(&src, 99, 0.0, nearest()),
            kaleidoscope(&src, 20, 0.0, nearest())
        );
        // Deterministic, and a flat image stays flat.
        assert_eq!(out, kaleidoscope(&src, 2, 0.0, nearest()));
        let flat = FilterBuffer::filled(9, 7, [0.3, 0.2, 0.1, 1.0]).unwrap();
        assert_eq!(kaleidoscope(&flat, 7, 33.0, Sampling::clamped()), flat);
    }

    #[test]
    fn dents_move_every_pixel_the_same_distance_and_are_seeded() {
        let src = ramp(60, 60);
        // min(60/3, 60/3) / 2 = 10 cells; scale 25 -> ep = 1.6 noise units a
        // cell; refraction 50 -> 3 * 0.5 / 1.6 = 0.9375 px, every pixel.
        let expect = 3.0 * 0.5 / 1.6;
        // Turbulence 0: the direction is 0 everywhere, so the whole image
        // moves `expect` pixels straight down the source (reads from below).
        let still = dents(&src, 25.0, 50.0, 10.0, 0.0, 1, Sampling::clamped());
        for y in 0..58u32 {
            for x in 0..60u32 {
                let p = still.get(x, y);
                let want = (y as f32 + expect) / 60.0;
                assert!((p[1] - want).abs() < 1e-4, "({x}, {y}): {p:?}");
                assert!((p[0] - x as f32 / 60.0).abs() < 1e-4);
            }
        }
        let a = dents(&src, 25.0, 50.0, 10.0, 10.0, 3, Sampling::clamped());
        assert_eq!(
            a,
            dents(&src, 25.0, 50.0, 10.0, 10.0, 3, Sampling::clamped()),
            "deterministic"
        );
        assert_ne!(a, still, "turbulence turns the dents");
        assert_ne!(
            a,
            dents(&src, 25.0, 50.0, 10.0, 10.0, 4, Sampling::clamped()),
            "the seed matters"
        );
        // Away from the clamped border, every pixel reads from exactly
        // `expect` pixels away (bilinear on a linear ramp is exact).
        for y in 2..58u32 {
            for x in 2..58u32 {
                let p = a.get(x, y);
                let (sx, sy) = (p[0] * 60.0 - x as f32, p[1] * 60.0 - y as f32);
                assert!((sx.hypot(sy) - expect).abs() < 2e-3, "({x}, {y})");
            }
        }
        assert_eq!(dents(&src, 0.0, 50.0, 10.0, 10.0, 3, nearest()), src);
        assert_eq!(dents(&src, 25.0, 0.0, 10.0, 10.0, 3, nearest()), src);
    }
}
