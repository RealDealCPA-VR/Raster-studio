//! Colour models used by adjustments, filters and the colour picker.
//!
//! Two different conventions live here and mixing them up is the classic
//! source of wrong-looking edits, so each function states which one it takes:
//!
//! * **HSL / HSV** are display-referred reparameterisations of the *encoded*
//!   (gamma) RGB a picker shows. They are only defined on `[0, 1]`, so inputs
//!   are clamped into that range; they are not HDR-capable and must not be fed
//!   raw working-space values.
//! * **CIELAB and luminance** are scene-referred: they are defined on *linear*
//!   light. The `rgb_*` spellings take sRGB-encoded input and linearise
//!   internally; the `linear_srgb_*` spellings skip that step.

use crate::space::{linear_srgb_to_xyz, xyz_to_linear_srgb, D65_WHITE_XYZ};
use crate::transfer::{linear_to_srgb3, srgb_to_linear3};

/// Rec.709 / sRGB luminance weights for **linear** RGB.
///
/// Equal to the middle row of [`crate::space::LINEAR_SRGB_TO_XYZ_D65`] (the `Y` row) to
/// within 4e-5; the rounded Rec.709 values are used because they are the ones
/// quoted in the standard and they sum to exactly 1.
pub const REC709_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Rec.709 luminance of a **linear** sRGB triple.
///
/// Unclamped and defined for negative input, so it stays usable on
/// working-space pixels that left `[0, 1]`.
#[inline]
pub fn linear_srgb_luminance(rgb: [f32; 3]) -> f32 {
    REC709_LUMA[0] * rgb[0] + REC709_LUMA[1] * rgb[1] + REC709_LUMA[2] * rgb[2]
}

/// Rec.709 luminance of an **sRGB-encoded** triple.
///
/// Linearises first; this is the value to use for contrast ratios and for
/// desaturation that should not darken. Averaging the encoded channels instead
/// is the common bug this exists to avoid.
#[inline]
pub fn srgb_luminance(rgb: [f32; 3]) -> f32 {
    linear_srgb_luminance(srgb_to_linear3(rgb))
}

#[inline]
fn clamp01(v: f32) -> f32 {
    // `f32::clamp` panics on NaN operands only if min > max; NaN input
    // propagates through `clamp` unchanged, so map it to 0 explicitly.
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// Returns `(hue_degrees, chroma, max, min)` for an already-clamped RGB triple.
///
/// `hue_degrees` is in the half-open interval `[0, 360)`; `chroma` is
/// `max - min`; `max` and `min` are the largest and smallest of the three
/// channels. Hue is `0` when `chroma == 0`.
fn hue_chroma(rgb: [f32; 3]) -> (f32, f32, f32, f32) {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    let hue = if chroma <= 0.0 {
        0.0
    } else if max == r {
        60.0 * (g - b) / chroma
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    };
    let hue = if hue < 0.0 { hue + 360.0 } else { hue };
    // `hue + 360.0` rounds *up* to exactly 360.0f32 whenever the red-sector
    // hue is a tiny negative: f32 spacing at 360 is ~3.05e-5, so any hue in
    // (-1.5e-5, 0) lands on the excluded endpoint. Re-wrapping keeps the
    // documented half-open range true for every input, which callers rely on
    // when they compute `(hue / 60.0) as usize` to index a six-entry sextant
    // table or `hue / 360.0` for a slider position.
    let hue = if hue >= 360.0 { 0.0 } else { hue };
    (hue, chroma, max, min)
}

/// Reconstructs RGB from hue (degrees), chroma and a per-channel offset.
fn from_hue_chroma(hue: f32, chroma: f32, m: f32) -> [f32; 3] {
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = chroma * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// Encoded RGB to HSL: hue in `[0, 360)` degrees, saturation and lightness in
/// `[0, 1]`. Input channels are clamped to `[0, 1]` first; hue is `0` for
/// achromatic input.
pub fn rgb_to_hsl(rgb: [f32; 3]) -> [f32; 3] {
    let rgb = [clamp01(rgb[0]), clamp01(rgb[1]), clamp01(rgb[2])];
    let (hue, chroma, max, min) = hue_chroma(rgb);
    let lightness = (max + min) * 0.5;
    let denom = 1.0 - (2.0 * lightness - 1.0).abs();
    let saturation = if chroma <= 0.0 || denom <= 0.0 {
        0.0
    } else {
        (chroma / denom).min(1.0)
    };
    [hue, saturation, lightness]
}

/// Inverse of [`rgb_to_hsl`]. Hue wraps; saturation and lightness are clamped.
pub fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let (h, s, l) = (hsl[0], clamp01(hsl[1]), clamp01(hsl[2]));
    let chroma = (1.0 - (2.0 * l - 1.0).abs()) * s;
    from_hue_chroma(h, chroma, l - chroma * 0.5)
}

/// Encoded RGB to HSV: hue in `[0, 360)` degrees, saturation and value in
/// `[0, 1]`. Input channels are clamped to `[0, 1]` first; hue is `0` for
/// achromatic input.
pub fn rgb_to_hsv(rgb: [f32; 3]) -> [f32; 3] {
    let rgb = [clamp01(rgb[0]), clamp01(rgb[1]), clamp01(rgb[2])];
    let (hue, chroma, max, _min) = hue_chroma(rgb);
    let saturation = if max <= 0.0 { 0.0 } else { chroma / max };
    [hue, saturation, max]
}

/// Inverse of [`rgb_to_hsv`]. Hue wraps; saturation and value are clamped.
pub fn hsv_to_rgb(hsv: [f32; 3]) -> [f32; 3] {
    let (h, s, v) = (hsv[0], clamp01(hsv[1]), clamp01(hsv[2]));
    let chroma = v * s;
    from_hue_chroma(h, chroma, v - chroma)
}

/// CIELAB `f` function, total over negative input (out-of-gamut XYZ).
#[inline]
fn lab_f(t: f32) -> f32 {
    const DELTA: f32 = 6.0 / 29.0;
    if t > DELTA * DELTA * DELTA {
        t.cbrt()
    } else {
        t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
    }
}

/// Largest magnitude [`lab_f_inv`] is allowed to return.
///
/// Without a cap, `t * t * t` overflows to `+inf` for all three of fx/fy/fz and
/// the [`xyz_to_linear_srgb`] row then evaluates `inf - inf - inf == NaN`,
/// breaking the crate's "no `NaN` from a finite input" invariant.
///
/// The bound is deliberately conservative: the output is scaled by
/// [`D65_WHITE_XYZ`] (largest component `1.0891`) and combined by a matrix
/// whose largest row sums to `5.2769` in absolute value, so any cap below
/// `f32::MAX / (1.0891 * 5.2769)` keeps every partial sum finite. Reachable
/// inputs cancel more than that worst case — `absurd_but_finite_lab_values_stay_finite`
/// still passes at `f32::MAX / 4` and fails at `f32::MAX / 3` — but the
/// sufficient bound is the one worth encoding, since it needs no argument
/// about which `L*a*b*` triples an editor can produce.
const LAB_F_INV_MAX: f32 = f32::MAX / 16.0;

/// Inverse of [`lab_f`], saturating at [`LAB_F_INV_MAX`] so no finite `L*a*b*`
/// can drive [`lab_to_linear_srgb`] into `inf - inf`. `NaN` still propagates.
#[inline]
fn lab_f_inv(t: f32) -> f32 {
    const DELTA: f32 = 6.0 / 29.0;
    let v = if t > DELTA {
        t * t * t
    } else {
        3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
    };
    // `f32::clamp` propagates NaN rather than replacing it, which is what we
    // want: NaN in, NaN out; only finite inputs are promised NaN-free output.
    v.clamp(-LAB_F_INV_MAX, LAB_F_INV_MAX)
}

/// Linear sRGB to CIELAB (D65), `[L*, a*, b*]` with `L*` in `[0, 100]` for
/// in-range input.
///
/// The reference white is [`D65_WHITE_XYZ`], the white point the crate's
/// matrices actually realise, so neutral input gives exactly `a* = b* = 0`.
pub fn linear_srgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    let xyz = linear_srgb_to_xyz(rgb);
    let fx = lab_f(xyz[0] / D65_WHITE_XYZ[0]);
    let fy = lab_f(xyz[1] / D65_WHITE_XYZ[1]);
    let fz = lab_f(xyz[2] / D65_WHITE_XYZ[2]);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Inverse of [`linear_srgb_to_lab`]. Unclamped: Lab values outside the sRGB
/// gamut yield linear components outside `[0, 1]` rather than being clipped.
pub fn lab_to_linear_srgb(lab: [f32; 3]) -> [f32; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let xyz = [
        lab_f_inv(fx) * D65_WHITE_XYZ[0],
        lab_f_inv(fy) * D65_WHITE_XYZ[1],
        lab_f_inv(fz) * D65_WHITE_XYZ[2],
    ];
    xyz_to_linear_srgb(xyz)
}

/// sRGB-encoded RGB to CIELAB (D65). Linearises first.
pub fn rgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    linear_srgb_to_lab(srgb_to_linear3(rgb))
}

/// Inverse of [`rgb_to_lab`], returning sRGB-encoded RGB. Unclamped.
pub fn lab_to_rgb(lab: [f32; 3]) -> [f32; 3] {
    linear_to_srgb3(lab_to_linear_srgb(lab))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::LINEAR_SRGB_TO_XYZ_D65;

    fn assert_close(got: [f32; 3], want: [f32; 3], tol: f32, what: &str) {
        for i in 0..3 {
            assert!(
                (got[i] - want[i]).abs() < tol,
                "{what}: component {i} = {}, expected {} (got {got:?})",
                got[i],
                want[i]
            );
        }
    }

    /// A grid dense enough to catch sextant-boundary mistakes.
    fn grid() -> Vec<[f32; 3]> {
        let mut out = Vec::new();
        for r in 0..6 {
            for g in 0..6 {
                for b in 0..6 {
                    out.push([r as f32 / 5.0, g as f32 / 5.0, b as f32 / 5.0]);
                }
            }
        }
        out
    }

    #[test]
    fn rec709_weights_sum_to_one_and_match_the_xyz_y_row() {
        let sum: f32 = REC709_LUMA.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6, "weights sum to {sum}");
        for i in 0..3 {
            let exact = LINEAR_SRGB_TO_XYZ_D65[1][i];
            assert!(
                (REC709_LUMA[i] - exact).abs() < 5e-5,
                "weight {i}: {} vs XYZ Y row {exact}",
                REC709_LUMA[i]
            );
        }
    }

    #[test]
    fn luminance_reference_values() {
        assert!((linear_srgb_luminance([1.0, 1.0, 1.0]) - 1.0).abs() < 1e-6);
        assert!((linear_srgb_luminance([0.0, 0.0, 0.0])).abs() < 1e-9);
        assert!((linear_srgb_luminance([1.0, 0.0, 0.0]) - 0.2126).abs() < 1e-6);
        assert!((linear_srgb_luminance([0.0, 1.0, 0.0]) - 0.7152).abs() < 1e-6);
        assert!((linear_srgb_luminance([0.0, 0.0, 1.0]) - 0.0722).abs() < 1e-6);
        // Encoded mid grey is much darker than 0.5 in light.
        assert!((srgb_luminance([0.5, 0.5, 0.5]) - 0.214_041_1).abs() < 1e-5);
        assert!((srgb_luminance([1.0, 0.0, 0.0]) - 0.2126).abs() < 1e-5);
    }

    #[test]
    fn luminance_survives_out_of_range_working_values() {
        let y = linear_srgb_luminance([-0.5, 3.0, 0.25]);
        assert!(y.is_finite());
        assert!((y - (-0.5 * 0.2126 + 3.0 * 0.7152 + 0.25 * 0.0722)).abs() < 1e-6);
    }

    #[test]
    fn hsl_reference_values() {
        let cases = [
            ([1.0f32, 0.0, 0.0], [0.0f32, 1.0, 0.5]),
            ([1.0, 1.0, 0.0], [60.0, 1.0, 0.5]),
            ([0.0, 1.0, 0.0], [120.0, 1.0, 0.5]),
            ([0.0, 1.0, 1.0], [180.0, 1.0, 0.5]),
            ([0.0, 0.0, 1.0], [240.0, 1.0, 0.5]),
            ([1.0, 0.0, 1.0], [300.0, 1.0, 0.5]),
            ([1.0, 0.5, 0.0], [30.0, 1.0, 0.5]),
            ([0.5, 0.5, 0.5], [0.0, 0.0, 0.5]),
            ([1.0, 1.0, 1.0], [0.0, 0.0, 1.0]),
            ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            ([0.2, 0.6, 0.4], [150.0, 0.5, 0.4]),
        ];
        for (rgb, want) in cases {
            assert_close(rgb_to_hsl(rgb), want, 1e-4, "rgb_to_hsl");
        }
    }

    #[test]
    fn hsv_reference_values() {
        let cases = [
            ([1.0f32, 0.0, 0.0], [0.0f32, 1.0, 1.0]),
            ([0.0, 1.0, 0.0], [120.0, 1.0, 1.0]),
            ([0.0, 0.0, 1.0], [240.0, 1.0, 1.0]),
            ([0.5, 0.5, 0.5], [0.0, 0.0, 0.5]),
            ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            ([0.2, 0.6, 0.4], [150.0, 2.0 / 3.0, 0.6]),
        ];
        for (rgb, want) in cases {
            assert_close(rgb_to_hsv(rgb), want, 1e-4, "rgb_to_hsv");
        }
    }

    #[test]
    fn hsl_and_hsv_round_trip_over_a_grid() {
        for rgb in grid() {
            assert_close(hsl_to_rgb(rgb_to_hsl(rgb)), rgb, 1e-5, "hsl round trip");
            assert_close(hsv_to_rgb(rgb_to_hsv(rgb)), rgb, 1e-5, "hsv round trip");
        }
    }

    #[test]
    fn hue_wraps_and_out_of_range_inputs_are_clamped() {
        // 360 + 30 is the same colour as 30.
        assert_close(hsl_to_rgb([390.0, 1.0, 0.5]), [1.0, 0.5, 0.0], 1e-5, "wrap");
        assert_close(
            hsl_to_rgb([-330.0, 1.0, 0.5]),
            [1.0, 0.5, 0.0],
            1e-5,
            "wrap",
        );
        // HDR / negative input is clamped, not turned into garbage or NaN.
        assert_eq!(rgb_to_hsl([2.0, -1.0, 0.5]), rgb_to_hsl([1.0, 0.0, 0.5]));
        assert_eq!(rgb_to_hsv([2.0, -1.0, 0.5]), rgb_to_hsv([1.0, 0.0, 0.5]));
        assert_eq!(hsl_to_rgb([0.0, 5.0, -1.0]), hsl_to_rgb([0.0, 1.0, 0.0]));
        for c in rgb_to_hsl([f32::NAN, 0.5, 0.5]) {
            assert!(c.is_finite());
        }
    }

    #[test]
    fn hsl_output_ranges_are_respected() {
        for rgb in grid() {
            let [h, s, l] = rgb_to_hsl(rgb);
            assert!(
                (0.0..360.0).contains(&h),
                "hue {h} out of range for {rgb:?}"
            );
            assert!((0.0..=1.0).contains(&s), "sat {s} out of range");
            assert!((0.0..=1.0).contains(&l), "lightness {l} out of range");
            let [h, s, v] = rgb_to_hsv(rgb);
            assert!((0.0..360.0).contains(&h), "hue {h} out of range");
            assert!((0.0..=1.0).contains(&s));
            assert!((0.0..=1.0).contains(&v));
        }
    }

    /// The coarse grid above cannot reach this: when `max == r` and green is a
    /// single ULP below blue, `60 * (g - b) / chroma` is a tiny negative and
    /// `hue + 360.0` rounds to exactly `360.0f32` (f32 spacing at 360 is
    /// ~3.05e-5). A picker doing `(hue / 60.0) as usize` would index a
    /// six-entry sextant table at 6.
    #[test]
    fn hue_never_reaches_the_excluded_360_endpoint() {
        let b = f32::from_bits(0.5f32.to_bits() + 1);
        let h = rgb_to_hsl([1.0, 0.5, b])[0];
        assert!(h < 360.0, "rgb_to_hsl hue = {h}");
        assert!(h >= 0.0, "rgb_to_hsl hue = {h}");
        let h = rgb_to_hsv([1.0, 0.5, b])[0];
        assert!(h < 360.0, "rgb_to_hsv hue = {h}");
        assert!(h >= 0.0, "rgb_to_hsv hue = {h}");

        // The same shape, swept across the red sector, plus the reverse
        // ordering that produces a tiny *positive* hue near 0.
        for k in 1u32..=64 {
            for &max in &[1.0f32, 0.5, 0.25, 0.125] {
                let g = max * 0.5;
                let b = f32::from_bits(g.to_bits() + k);
                for h in [
                    rgb_to_hsl([max, g, b])[0],
                    rgb_to_hsv([max, g, b])[0],
                    rgb_to_hsl([max, b, g])[0],
                    rgb_to_hsv([max, b, g])[0],
                ] {
                    assert!(
                        (0.0..360.0).contains(&h),
                        "hue {h} out of [0,360) for max {max}, k {k}"
                    );
                }
            }
        }
    }

    /// The crate-level docs promise no `NaN` from a finite input. Large finite
    /// `L*` used to cube to `+inf` in all three of fx/fy/fz, after which the
    /// XYZ->RGB row evaluated `inf - inf - inf == NaN`.
    ///
    /// Asserts *finite*, not merely non-`NaN`, which is what makes the exact
    /// value of `LAB_F_INV_MAX` load-bearing: a looser cap such as
    /// `f32::MAX / 4` still avoids `NaN` (no single matrix term can reach
    /// infinity on its own) but lets a partial sum saturate to `inf`.
    #[test]
    fn absurd_but_finite_lab_values_stay_finite() {
        for lab in [
            [1e20f32, 0.0, 0.0],
            [f32::MAX, 0.0, 0.0],
            [1e20, 1e20, -1e20],
            [-1e20, 0.0, 0.0],
            [0.0, f32::MAX, f32::MIN],
            [f32::MAX, f32::MIN, f32::MAX],
            [f32::MIN, f32::MAX, f32::MIN],
            // Worst case for the cap: `a*` drives fx past the cube overflow
            // while `b*` keeps fz on the negative linear branch, so the
            // XYZ->RGB row adds three same-signed terms with no cancellation.
            [-1000.0, f32::MAX, f32::MAX],
            [-1000.0, f32::MIN, f32::MIN],
            [1e15, 5.0, -5.0],
        ] {
            let linear = lab_to_linear_srgb(lab);
            for c in linear {
                assert!(c.is_finite(), "lab_to_linear_srgb({lab:?}) = {linear:?}");
            }
            let encoded = lab_to_rgb(lab);
            for c in encoded {
                assert!(c.is_finite(), "lab_to_rgb({lab:?}) = {encoded:?}");
            }
        }
        // NaN must still propagate, not be clamped into a real number.
        assert!(lab_to_linear_srgb([f32::NAN, 0.0, 0.0])[0].is_nan());
        // The saturation must not disturb values an image can actually reach.
        assert_close(
            lab_to_rgb(rgb_to_lab([0.3, 0.6, 0.9])),
            [0.3, 0.6, 0.9],
            1e-4,
            "unaffected",
        );
    }

    #[test]
    fn lab_reference_values() {
        // Published CIELAB D65 coordinates of the sRGB primaries.
        let cases = [
            ([1.0f32, 0.0, 0.0], [53.2408f32, 80.0925, 67.2032]),
            ([0.0, 1.0, 0.0], [87.7347, -86.1827, 83.1793]),
            ([0.0, 0.0, 1.0], [32.2970, 79.1875, -107.8602]),
        ];
        for (rgb, want) in cases {
            assert_close(rgb_to_lab(rgb), want, 0.02, "rgb_to_lab");
        }
        // Neutrals must be exactly achromatic, or greys pick up a cast.
        assert_close(
            rgb_to_lab([1.0, 1.0, 1.0]),
            [100.0, 0.0, 0.0],
            1e-3,
            "white",
        );
        assert_close(rgb_to_lab([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0], 1e-6, "black");
        assert_close(
            rgb_to_lab([0.5, 0.5, 0.5]),
            [53.389, 0.0, 0.0],
            1e-2,
            "grey",
        );
    }

    #[test]
    fn lab_round_trips_over_a_grid() {
        for rgb in grid() {
            assert_close(lab_to_rgb(rgb_to_lab(rgb)), rgb, 1e-4, "lab round trip");
        }
    }

    #[test]
    fn lab_lightness_is_monotone_in_luminance() {
        let mut prev = f32::NEG_INFINITY;
        for step in 0..=20 {
            let v = step as f32 / 20.0;
            let l = rgb_to_lab([v, v, v])[0];
            assert!(l > prev - 1e-6, "L* not monotone at {v}: {l} after {prev}");
            prev = l;
        }
    }

    #[test]
    fn lab_handles_out_of_gamut_values_without_nan() {
        // Display P3 red expressed in linear sRGB is outside the sRGB gamut.
        let out_of_gamut = [1.224_940_2, -0.042_056_955, -0.019_637_555];
        let lab = linear_srgb_to_lab(out_of_gamut);
        for c in lab {
            assert!(c.is_finite(), "out-of-gamut Lab produced {lab:?}");
        }
        let back = lab_to_linear_srgb(lab);
        assert_close(back, out_of_gamut, 1e-3, "out-of-gamut Lab round trip");
    }

    #[test]
    fn linear_and_encoded_lab_entry_points_agree() {
        let encoded = [0.3, 0.6, 0.9];
        assert_close(
            rgb_to_lab(encoded),
            linear_srgb_to_lab(srgb_to_linear3(encoded)),
            1e-5,
            "lab entry points",
        );
    }
}
