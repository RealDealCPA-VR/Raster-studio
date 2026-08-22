//! sRGB transfer functions (IEC 61966-2-1) and the 8-bit linearization LUT.
//!
//! The working space is `f32` linear sRGB and is **not** clamped to `[0, 1]`:
//! adjustment layers, blur kernels with negative lobes, and HDR sources all
//! produce out-of-range values that must survive a round trip through the
//! transfer functions. Both functions here are therefore *total* over the whole
//! `f32` domain:
//!
//! * Negative inputs are mirrored through the origin (`f(-x) == -f(x)`), which
//!   keeps the curve odd, continuous and monotone. A naive
//!   `if c <= 0.04045 { c / 12.92 }` instead routes every negative down the
//!   near-black linear segment, which is wrong by a factor of five at
//!   `c = -0.5` — and wrong identically in both directions, so a round-trip
//!   test does not notice.
//! * Inputs above `1.0` are passed through the same analytic branch rather than
//!   being clamped, so highlights are preserved.
//! * `NaN` in yields `NaN` out; no finite input yields `NaN`; no input panics.
//!   [`linear_to_srgb`] is finite for every finite input, including `f32::MAX`.
//!   [`srgb_to_linear`] is finite for magnitudes below about `1.19e16` and has
//!   already saturated to infinity by `1.2e16`, where
//!   `((m + 0.055) / 1.055).powf(2.4)` genuinely exceeds `f32::MAX`. The exact
//!   crossing sits inside that bracket and depends on the platform's `powf`
//!   rounding, so the bracket is what is promised and what
//!   `overflow_threshold_is_where_the_docs_say_it_is` pins — note the bound is
//!   exclusive: `1.2e16` itself is already infinite. Either way it is ~16
//!   orders of magnitude above any pixel value, so callers need no guard.

/// Encoded-domain breakpoint of the sRGB curve's linear segment (IEC 61966-2-1).
pub const SRGB_ENCODED_KNEE: f32 = 0.040_45;

/// Linear-domain breakpoint of the sRGB curve's linear segment.
pub const SRGB_LINEAR_KNEE: f32 = 0.003_130_8;

/// Slope of the sRGB curve's linear (near-black) segment.
pub const SRGB_LINEAR_SLOPE: f32 = 12.92;

/// Offset of the sRGB curve's power segment.
pub const SRGB_ALPHA: f32 = 0.055;

/// sRGB electro-optical transfer function (gamma-encoded -> linear), per channel.
///
/// Total over all of `f32`: odd-symmetric for negatives, unclamped above `1.0`.
/// `srgb_to_linear(0.5) == 0.2140` to four decimal places.
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    let m = c.abs();
    let l = if m <= SRGB_ENCODED_KNEE {
        m / SRGB_LINEAR_SLOPE
    } else {
        ((m + SRGB_ALPHA) / (1.0 + SRGB_ALPHA)).powf(2.4)
    };
    if c < 0.0 {
        -l
    } else {
        l
    }
}

/// Inverse of [`srgb_to_linear`] (linear -> gamma-encoded).
///
/// Total over all of `f32` with the same mirroring and highlight pass-through
/// rules. `linear_to_srgb(0.2140) == 0.5` to four decimal places.
#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    let m = c.abs();
    let s = if m <= SRGB_LINEAR_KNEE {
        m * SRGB_LINEAR_SLOPE
    } else {
        (1.0 + SRGB_ALPHA) * m.powf(1.0 / 2.4) - SRGB_ALPHA
    };
    if c < 0.0 {
        -s
    } else {
        s
    }
}

/// Applies [`srgb_to_linear`] to each of the three colour channels.
#[inline]
pub fn srgb_to_linear3(rgb: [f32; 3]) -> [f32; 3] {
    [
        srgb_to_linear(rgb[0]),
        srgb_to_linear(rgb[1]),
        srgb_to_linear(rgb[2]),
    ]
}

/// Applies [`linear_to_srgb`] to each of the three colour channels.
#[inline]
pub fn linear_to_srgb3(rgb: [f32; 3]) -> [f32; 3] {
    [
        linear_to_srgb(rgb[0]),
        linear_to_srgb(rgb[1]),
        linear_to_srgb(rgb[2]),
    ]
}

/// Exact linearization of every 8-bit sRGB code value, index = code value.
///
/// Entries were evaluated in `f64` from the IEC 61966-2-1 curve and rounded to
/// `f32`, so `SRGB8_TO_LINEAR[i]` is the correctly rounded linearization of
/// `i / 255` — never further than a few ULP from [`srgb_to_linear`] of the same
/// input, and cheaper than `powf` in decode inner loops.
pub const SRGB8_TO_LINEAR: [f32; 256] = [
    0.0,
    0.000303527,
    0.000607054,
    0.000910581,
    0.001214108,
    0.001517635,
    0.001821162,
    0.0021246888,
    0.002428216,
    0.0027317428,
    0.00303527,
    0.0033465358,
    0.0036765074,
    0.004024717,
    0.004391442,
    0.0047769533,
    0.0051815165,
    0.0056053917,
    0.006048833,
    0.0065120906,
    0.00699541,
    0.007499032,
    0.008023193,
    0.008568126,
    0.009134059,
    0.009721218,
    0.010329823,
    0.010960094,
    0.011612245,
    0.012286488,
    0.0129830325,
    0.013702083,
    0.014443844,
    0.015208514,
    0.015996294,
    0.016807375,
    0.017641954,
    0.01850022,
    0.019382361,
    0.020288562,
    0.02121901,
    0.022173885,
    0.023153367,
    0.024157632,
    0.02518686,
    0.026241222,
    0.027320892,
    0.02842604,
    0.029556835,
    0.030713445,
    0.031896032,
    0.033104766,
    0.034339808,
    0.035601314,
    0.03688945,
    0.038204372,
    0.039546236,
    0.0409152,
    0.04231141,
    0.04373503,
    0.045186203,
    0.046665087,
    0.048171826,
    0.049706567,
    0.051269457,
    0.052860647,
    0.054480277,
    0.05612849,
    0.05780543,
    0.059511237,
    0.061246052,
    0.063010015,
    0.064803265,
    0.06662594,
    0.06847817,
    0.070360094,
    0.07227185,
    0.07421357,
    0.07618538,
    0.07818742,
    0.08021982,
    0.08228271,
    0.08437621,
    0.08650046,
    0.08865558,
    0.09084171,
    0.093058966,
    0.09530747,
    0.09758735,
    0.099898726,
    0.10224173,
    0.104616486,
    0.107023105,
    0.10946171,
    0.11193243,
    0.114435375,
    0.116970666,
    0.11953843,
    0.122138776,
    0.12477182,
    0.12743768,
    0.13013647,
    0.13286832,
    0.13563333,
    0.13843161,
    0.14126329,
    0.14412847,
    0.14702727,
    0.14995979,
    0.15292615,
    0.15592647,
    0.15896083,
    0.16202937,
    0.1651322,
    0.1682694,
    0.17144111,
    0.1746474,
    0.17788842,
    0.18116425,
    0.18447499,
    0.18782078,
    0.19120169,
    0.19461784,
    0.19806932,
    0.20155625,
    0.20507874,
    0.20863687,
    0.21223076,
    0.2158605,
    0.2195262,
    0.22322796,
    0.22696587,
    0.23074006,
    0.23455058,
    0.23839757,
    0.24228112,
    0.24620132,
    0.25015828,
    0.2541521,
    0.25818285,
    0.26225066,
    0.2663556,
    0.2704978,
    0.2746773,
    0.27889428,
    0.28314874,
    0.28744084,
    0.29177064,
    0.29613826,
    0.30054379,
    0.3049873,
    0.30946892,
    0.31398872,
    0.31854677,
    0.3231432,
    0.3277781,
    0.33245152,
    0.33716363,
    0.34191442,
    0.34670407,
    0.3515326,
    0.35640013,
    0.3613068,
    0.3662526,
    0.3712377,
    0.37626213,
    0.38132602,
    0.38642943,
    0.39157248,
    0.39675522,
    0.40197778,
    0.4072402,
    0.4125426,
    0.41788507,
    0.42326766,
    0.4286905,
    0.43415365,
    0.43965718,
    0.4452012,
    0.4507858,
    0.45641103,
    0.462077,
    0.4677838,
    0.47353148,
    0.47932017,
    0.48514995,
    0.49102086,
    0.49693298,
    0.5028865,
    0.50888133,
    0.5149177,
    0.52099556,
    0.5271151,
    0.5332764,
    0.5394795,
    0.54572445,
    0.55201143,
    0.5583404,
    0.5647115,
    0.57112485,
    0.57758045,
    0.58407843,
    0.59061885,
    0.59720176,
    0.60382736,
    0.61049557,
    0.6172066,
    0.6239604,
    0.63075715,
    0.63759685,
    0.6444797,
    0.65140563,
    0.65837485,
    0.6653873,
    0.67244315,
    0.6795425,
    0.6866853,
    0.69387174,
    0.7011019,
    0.70837575,
    0.7156935,
    0.7230551,
    0.73046076,
    0.7379104,
    0.7454042,
    0.7529422,
    0.7605245,
    0.76815116,
    0.7758222,
    0.7835378,
    0.7912979,
    0.7991027,
    0.80695224,
    0.8148466,
    0.82278574,
    0.8307699,
    0.838799,
    0.8468732,
    0.8549926,
    0.8631572,
    0.8713671,
    0.8796224,
    0.8879231,
    0.8962694,
    0.9046612,
    0.91309863,
    0.92158186,
    0.9301109,
    0.9386857,
    0.9473065,
    0.9559733,
    0.9646863,
    0.9734453,
    0.9822506,
    0.9911021,
    1.0,
];

/// LUT fast path: linearize an 8-bit sRGB code value without a `powf`.
///
/// Equivalent to `srgb_to_linear(v as f32 / 255.0)` to within a few ULP; see
/// [`SRGB8_TO_LINEAR`]. Cannot panic — every `u8` is a valid index.
#[inline]
pub fn srgb8_to_linear(v: u8) -> f32 {
    SRGB8_TO_LINEAR[v as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference values from the IEC 61966-2-1 curve. A round-trip test alone
    /// passes when both directions are wrong in mirrored ways, so the absolute
    /// anchors below are the ones that actually pin the curve down.
    #[test]
    fn srgb_to_linear_matches_reference_values() {
        let cases = [
            (0.0f32, 0.0f32),
            (0.04045, 0.003_130_8),
            (0.25, 0.050_876_2),
            (0.5, 0.214_041_1),
            (0.75, 0.522_522_1),
            (1.0, 1.0),
        ];
        for (encoded, expected) in cases {
            let got = srgb_to_linear(encoded);
            assert!(
                (got - expected).abs() < 1e-5,
                "srgb_to_linear({encoded}) = {got}, expected {expected}"
            );
        }
    }

    #[test]
    fn linear_to_srgb_matches_reference_values() {
        let cases = [
            (0.0f32, 0.0f32),
            (0.003_130_8, 0.040_45),
            (0.050_876_2, 0.25),
            (0.214_041_1, 0.5),
            (0.522_522_1, 0.75),
            (1.0, 1.0),
        ];
        for (linear, expected) in cases {
            let got = linear_to_srgb(linear);
            assert!(
                (got - expected).abs() < 1e-5,
                "linear_to_srgb({linear}) = {got}, expected {expected}"
            );
        }
    }

    /// The bug this replaces: a bare `if c <= 0.04045 { c / 12.92 }` sends
    /// *every* negative down the near-black linear segment, so
    /// `srgb_to_linear(-0.5)` was `-0.0387` instead of `-0.2140`. Both
    /// directions were wrong the same way, so a round-trip test stayed green.
    #[test]
    fn negatives_use_the_mirrored_curve_not_the_linear_segment() {
        let got = srgb_to_linear(-0.5);
        assert!(
            (got + 0.214_041_1).abs() < 1e-5,
            "srgb_to_linear(-0.5) = {got}"
        );
        assert!(
            (got + 0.5 / 12.92).abs() > 0.1,
            "still on the linear segment: {got}"
        );
        let got = linear_to_srgb(-0.214_041_1);
        assert!((got + 0.5).abs() < 1e-5, "linear_to_srgb(-0.2140) = {got}");
    }

    #[test]
    fn every_finite_input_produces_a_finite_output() {
        for &c in &[
            -1e5f32, -100.0, -1.0, -0.5, -0.056, -0.04, 0.0, 0.04, 1.0, 1e5,
        ] {
            assert!(
                srgb_to_linear(c).is_finite(),
                "srgb_to_linear({c}) not finite"
            );
            assert!(
                linear_to_srgb(c).is_finite(),
                "linear_to_srgb({c}) not finite"
            );
        }
    }

    /// Pins the documented overflow bound, **including the two numbers the doc
    /// actually quotes**. The module doc used to claim `1e12` overflows; it
    /// does not, and quoting a bound 4 orders of magnitude too low invites
    /// callers to add a guard they do not need. It then claimed finiteness "up
    /// to a magnitude of roughly `1.2e16`" while only bracketing `1e16` and
    /// `1e17` here — and read inclusively that claim was false, because
    /// `1.2e16` is already infinite. The bracket below is therefore the doc's
    /// own pair of figures, so the two cannot drift apart.
    #[test]
    fn overflow_threshold_is_where_the_docs_say_it_is() {
        for &c in &[1e6f32, 1e12, 1e15, 1e16, -1e16] {
            assert!(
                srgb_to_linear(c).is_finite(),
                "srgb_to_linear({c}) = {} should still be finite",
                srgb_to_linear(c)
            );
        }
        // 1e12 specifically: the value the old doc named.
        assert!((srgb_to_linear(1e12) - 5.548_752e28).abs() < 1e24);
        // The documented bracket. Below `1.19e16` the curve is still finite...
        for &c in &[1.19e16f32, -1.19e16] {
            assert!(
                srgb_to_linear(c).is_finite(),
                "srgb_to_linear({c}) = {} but the docs promise finite below 1.19e16",
                srgb_to_linear(c)
            );
        }
        // ...and by `1.2e16` it has already gone, which is why the documented
        // bound is exclusive. An inclusive reading of the old "roughly 1.2e16"
        // was simply wrong here.
        assert_eq!(
            srgb_to_linear(1.2e16),
            f32::INFINITY,
            "the docs promise 1.2e16 is already infinite"
        );
        assert_eq!(srgb_to_linear(-1.2e16), f32::NEG_INFINITY);
        // Well above the bracket, unchanged.
        assert!(srgb_to_linear(1e17).is_infinite());
        assert_eq!(srgb_to_linear(-1e17), f32::NEG_INFINITY);
        // The inverse direction never overflows: m^(1/2.4) shrinks.
        assert!(linear_to_srgb(f32::MAX).is_finite());
        assert!(linear_to_srgb(f32::MIN).is_finite());
    }

    #[test]
    fn transfer_functions_are_odd_symmetric() {
        for &c in &[0.001f32, 0.04, 0.2, 0.5, 0.9, 1.0, 3.0] {
            assert_eq!(srgb_to_linear(-c), -srgb_to_linear(c));
            assert_eq!(linear_to_srgb(-c), -linear_to_srgb(c));
        }
        // The mirrored branch must reproduce the positive reference value.
        assert!((srgb_to_linear(-0.5) + 0.214_041_1).abs() < 1e-5);
    }

    #[test]
    fn highlights_pass_through_unclamped_and_round_trip() {
        for &c in &[1.0f32, 1.5, 2.0, 8.0, 100.0] {
            let lin = srgb_to_linear(c);
            assert!(lin >= c, "highlight {c} was clamped to {lin}");
            let round = linear_to_srgb(lin);
            assert!(
                (round - c).abs() < 1e-3 * c.max(1.0),
                "highlight round trip failed for {c}: {round}"
            );
        }
        // 2.0 encoded sits well above 1.0 in linear; the exact analytic value.
        assert!(
            (srgb_to_linear(2.0) - 4.953_846).abs() < 1e-4,
            "srgb_to_linear(2.0) = {}",
            srgb_to_linear(2.0)
        );
    }

    #[test]
    fn transfer_functions_round_trip_over_the_extended_domain() {
        for step in -40i32..=140 {
            let c = step as f32 / 100.0;
            let round = linear_to_srgb(srgb_to_linear(c));
            assert!(
                (round - c).abs() < 1e-5,
                "round trip failed for {c}: got {round}"
            );
        }
    }

    #[test]
    fn nan_in_nan_out_and_no_panics() {
        assert!(srgb_to_linear(f32::NAN).is_nan());
        assert!(linear_to_srgb(f32::NAN).is_nan());
        assert_eq!(srgb_to_linear(f32::INFINITY), f32::INFINITY);
        assert_eq!(srgb_to_linear(f32::NEG_INFINITY), f32::NEG_INFINITY);
    }

    #[test]
    fn lut_matches_the_powf_path_for_every_code_value() {
        for v in 0u8..=255 {
            let exact = srgb_to_linear(v as f32 / 255.0);
            let lut = srgb8_to_linear(v);
            assert!(
                (lut - exact).abs() < 1e-6,
                "LUT[{v}] = {lut}, powf path = {exact}"
            );
        }
    }

    #[test]
    fn lut_endpoints_and_monotonicity() {
        assert_eq!(srgb8_to_linear(0), 0.0);
        assert_eq!(srgb8_to_linear(255), 1.0);
        // Mid grey 128/255 is the classic 0.2159 (not 0.2140, which is 0.5).
        assert!((srgb8_to_linear(128) - 0.215_861).abs() < 1e-5);
        for v in 1u8..=255 {
            assert!(
                srgb8_to_linear(v) > srgb8_to_linear(v - 1),
                "LUT not strictly increasing at {v}"
            );
        }
    }

    #[test]
    fn channel_wise_helpers_apply_per_channel() {
        let rgb = [0.0, 0.5, 1.0];
        assert_eq!(
            srgb_to_linear3(rgb),
            [
                srgb_to_linear(0.0),
                srgb_to_linear(0.5),
                srgb_to_linear(1.0)
            ]
        );
        assert_eq!(
            linear_to_srgb3(rgb),
            [
                linear_to_srgb(0.0),
                linear_to_srgb(0.5),
                linear_to_srgb(1.0)
            ]
        );
        // Channel order must not be permuted.
        assert_ne!(
            srgb_to_linear3([0.1, 0.2, 0.3]),
            srgb_to_linear3([0.3, 0.2, 0.1])
        );
    }
}
