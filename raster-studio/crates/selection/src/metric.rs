//! How "close enough in colour" is measured, and how a tolerance turns into
//! fractional coverage.
//!
//! # Why colour selection is *not* done in linear light by default
//! The rest of this codebase composites and filters in linear light, because
//! light adds linearly. Colour *similarity* is the opposite problem: it asks
//! how close two colours look, and the eye's response is close to a power law,
//! so a fixed linear-light difference is enormous in the shadows and invisible
//! in the highlights. Selecting "everything about this dark grey" with a
//! linear-light tolerance would swallow the entire shadow range.
//!
//! So the default [`ColorMetric::Srgb`] is deliberately defined on the
//! *gamma-encoded* values — the explicit carve-out the codebase rule allows,
//! and also what a tolerance of "32" means in every other editor.
//! [`ColorMetric::Lab`] is the perceptually principled version, and
//! [`ColorMetric::LinearRgb`] exists for callers selecting on physical
//! intensity rather than appearance. Coverage, feathering and every filter in
//! this crate stay linear regardless — see [`crate::buf`].
//!
//! # Chebyshev, not Euclidean
//! The distance is the **largest per-channel difference**, which is what a
//! per-channel tolerance means and what makes [`crate::wand::similar`] exact:
//! a max-of-a-box is separable, so the acceptance table can be built with three
//! one-dimensional passes instead of a search.

use color::{rgb_to_lab, srgb8_to_linear};
use serde::{Deserialize, Serialize};

use crate::buf::to_byte;

/// Normalised colour coordinates: three colour axes plus alpha, each nominally
/// in `0.0..=1.0`, in whichever space the metric is defined on.
pub type ColorCoords = [f32; 4];

/// The space a colour tolerance is measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ColorMetric {
    /// Gamma-encoded sRGB, each channel `0..=1`. A tolerance of `t` accepts a
    /// per-channel difference of `t * 255` 8-bit codes — the familiar
    /// "tolerance 32" of a magic wand is `32.0 / 255.0`.
    #[default]
    Srgb,
    /// Linear light. Physically meaningful, perceptually lopsided; use it when
    /// selecting on intensity rather than on appearance.
    LinearRgb,
    /// CIELAB, normalised to `L*/100`, `(a* + 128)/256`, `(b* + 128)/256`, so a
    /// tolerance of `0.1` is `ΔL* = 10` or `Δa* = 25.6`.
    Lab,
}

impl ColorMetric {
    /// Map a straight-alpha, sRGB-encoded RGBA8 pixel into this metric's
    /// coordinates.
    pub fn coords(self, px: [u8; 4]) -> ColorCoords {
        let a = px[3] as f32 / 255.0;
        match self {
            ColorMetric::Srgb => [
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
                a,
            ],
            ColorMetric::LinearRgb => [
                srgb8_to_linear(px[0]),
                srgb8_to_linear(px[1]),
                srgb8_to_linear(px[2]),
                a,
            ],
            ColorMetric::Lab => {
                let lab = rgb_to_lab([
                    px[0] as f32 / 255.0,
                    px[1] as f32 / 255.0,
                    px[2] as f32 / 255.0,
                ]);
                [
                    (lab[0] / 100.0).clamp(0.0, 1.0),
                    ((lab[1] + 128.0) / 256.0).clamp(0.0, 1.0),
                    ((lab[2] + 128.0) / 256.0).clamp(0.0, 1.0),
                    a,
                ]
            }
        }
    }
}

/// Largest per-channel difference between two coordinate tuples.
///
/// `include_alpha` decides whether transparency counts as a colour difference;
/// with it off, a transparent pixel and an opaque one of the same hue are the
/// same colour.
pub fn distance(a: &ColorCoords, b: &ColorCoords, include_alpha: bool) -> f32 {
    let n = if include_alpha { 4 } else { 3 };
    a.iter()
        .zip(b.iter())
        .take(n)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max)
}

/// Slack on the tolerance comparison, in normalised metric units.
///
/// Three orders of magnitude below one 8-bit code (`1/255 ≈ 0.0039`), so it can
/// absorb the rounding of a division by 255 without ever letting a colour one
/// whole code past the tolerance through.
pub const TOLERANCE_EPSILON: f32 = 1e-6;

/// Turn a colour distance into fractional coverage.
///
/// * `d <= tolerance * (1 - antialias)` is fully selected;
/// * `d >= tolerance` is not selected;
/// * in between, coverage ramps down linearly.
///
/// `antialias = 0.0` is therefore a hard threshold that is **inclusive** at
/// exactly `tolerance` — the boundary a user tuning a wand expects to be able
/// to land on. `antialias = 1.0` ramps all the way from an exact match.
///
/// Inclusive means inclusive *up to representation error*: the comparison
/// carries [`TOLERANCE_EPSILON`] of slack, because a tolerance of `30.0/255.0`
/// and a measured difference of `130.0/255.0 - 100.0/255.0` are not the same
/// `f32`, and without the slack "tolerance 30" would reject a 30-code step
/// depending on which codes it was between.
pub fn tolerance_coverage(d: f32, tolerance: f32, antialias: f32) -> u8 {
    let t = tolerance.max(0.0);
    let inner = t * (1.0 - antialias.clamp(0.0, 1.0));
    if d <= inner + TOLERANCE_EPSILON {
        255
    } else if d >= t || t <= inner {
        0
    } else {
        to_byte(1.0 - (d - inner) / (t - inner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_tolerance_is_in_eight_bit_code_units() {
        let a = ColorMetric::Srgb.coords([100, 100, 100, 255]);
        let b = ColorMetric::Srgb.coords([130, 100, 100, 255]);
        let d = distance(&a, &b, false);
        assert!((d - 30.0 / 255.0).abs() < 1e-6, "{d}");
    }

    #[test]
    fn linear_light_would_mis_scale_a_shadow_tolerance() {
        // The reason Srgb is the default. The same 30-code step is a large
        // encoded distance near black and a tiny linear one, so a linear-light
        // tolerance tuned on a midtone swallows the whole shadow range.
        let dark = (
            ColorMetric::LinearRgb.coords([10, 10, 10, 255]),
            ColorMetric::LinearRgb.coords([40, 40, 40, 255]),
        );
        let bright = (
            ColorMetric::LinearRgb.coords([200, 200, 200, 255]),
            ColorMetric::LinearRgb.coords([230, 230, 230, 255]),
        );
        let d_dark = distance(&dark.0, &dark.1, false);
        let d_bright = distance(&bright.0, &bright.1, false);
        assert!(
            d_bright > d_dark * 5.0,
            "linear light compresses shadows: {d_dark} vs {d_bright}"
        );

        // In the encoded default the same step is the same distance everywhere.
        let e_dark = distance(
            &ColorMetric::Srgb.coords([10, 10, 10, 255]),
            &ColorMetric::Srgb.coords([40, 40, 40, 255]),
            false,
        );
        let e_bright = distance(
            &ColorMetric::Srgb.coords([200, 200, 200, 255]),
            &ColorMetric::Srgb.coords([230, 230, 230, 255]),
            false,
        );
        assert!((e_dark - e_bright).abs() < 1e-6);
    }

    #[test]
    fn lab_coordinates_are_normalised_into_the_unit_cube() {
        for px in [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 0, 0, 255],
            [0, 0, 255, 255],
        ] {
            let c = ColorMetric::Lab.coords(px);
            for (i, v) in c.iter().enumerate() {
                assert!((0.0..=1.0).contains(v), "{px:?} axis {i} = {v}");
            }
        }
        // Black to white is the full lightness axis.
        let black = ColorMetric::Lab.coords([0, 0, 0, 255]);
        let white = ColorMetric::Lab.coords([255, 255, 255, 255]);
        assert!((distance(&black, &white, false) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn alpha_only_counts_when_asked_for() {
        let opaque = ColorMetric::Srgb.coords([10, 20, 30, 255]);
        let ghost = ColorMetric::Srgb.coords([10, 20, 30, 0]);
        assert_eq!(distance(&opaque, &ghost, false), 0.0);
        assert_eq!(distance(&opaque, &ghost, true), 1.0);
    }

    #[test]
    fn a_hard_tolerance_is_inclusive_at_its_boundary() {
        assert_eq!(tolerance_coverage(0.1, 0.1, 0.0), 255);
        assert_eq!(tolerance_coverage(0.1001, 0.1, 0.0), 0);
        assert_eq!(tolerance_coverage(0.0, 0.0, 0.0), 255, "an exact match");
        assert_eq!(tolerance_coverage(0.001, 0.0, 0.0), 0);
    }

    #[test]
    fn the_boundary_slack_absorbs_rounding_but_not_a_whole_code() {
        // 130/255 - 100/255 is *not* the same f32 as 30/255; without the slack
        // a tolerance of exactly 30 codes would reject a 30-code step.
        let d = distance(
            &ColorMetric::Srgb.coords([130, 100, 100, 255]),
            &ColorMetric::Srgb.coords([100, 100, 100, 255]),
            false,
        );
        assert!(
            d > 30.0 / 255.0,
            "the fixture depends on d being the larger"
        );
        assert_eq!(tolerance_coverage(d, 30.0 / 255.0, 0.0), 255);
        // One further code is still firmly outside.
        let d31 = distance(
            &ColorMetric::Srgb.coords([131, 100, 100, 255]),
            &ColorMetric::Srgb.coords([100, 100, 100, 255]),
            false,
        );
        assert_eq!(tolerance_coverage(d31, 30.0 / 255.0, 0.0), 0);
        const _: () = assert!(TOLERANCE_EPSILON < 1.0 / 255.0 / 100.0);
    }

    #[test]
    fn antialiasing_ramps_between_the_inner_and_outer_tolerance() {
        // antialias = 0.5 with tolerance 0.2: solid to 0.1, gone at 0.2.
        assert_eq!(tolerance_coverage(0.1, 0.2, 0.5), 255);
        assert_eq!(tolerance_coverage(0.2, 0.2, 0.5), 0);
        let mid = tolerance_coverage(0.15, 0.2, 0.5);
        assert!(
            (120..=136).contains(&mid),
            "half way should be ~128, got {mid}"
        );
        // Monotone in between.
        let mut prev = 256u16;
        for i in 0..=20 {
            let d = 0.1 + 0.1 * i as f32 / 20.0;
            let c = tolerance_coverage(d, 0.2, 0.5) as u16;
            assert!(c <= prev, "coverage must fall as distance grows");
            prev = c;
        }
    }
}
