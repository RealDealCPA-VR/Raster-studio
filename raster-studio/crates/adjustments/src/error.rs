//! Parameter validation.
//!
//! Every adjustment in this crate is built through a constructor that returns
//! [`AdjustmentError`] rather than silently accepting nonsense. The reason is
//! concrete: the pre-validation `levels` collapsed the whole image to black for
//! `gamma <= 0` (a `1.0 / 1e-5` exponent) and turned `white <= black` into a
//! 100000x gain step, and the pre-validation `curve` divided by `1e-5` on
//! duplicate control points and returned values far outside the output range.
//! Neither failure was reported anywhere; both looked like a rendering bug.

use thiserror::Error;

/// Everything that can be wrong with an adjustment's parameters.
///
/// `PartialEq` is derived so tests can assert on the exact rejection rather
/// than merely that *something* was rejected.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum AdjustmentError {
    /// A parameter was `NaN` or infinite. Non-finite parameters propagate into
    /// every pixel, so they are refused at the door.
    #[error("`{name}` must be finite, got {value}")]
    NotFinite {
        /// Parameter name as it appears in the constructor.
        name: &'static str,
        /// The offending value.
        value: f32,
    },

    /// A parameter was finite but outside the range the adjustment is defined
    /// on.
    #[error("`{name}` must be within {min}..={max}, got {value}")]
    OutOfRange {
        /// Parameter name as it appears in the constructor.
        name: &'static str,
        /// Inclusive lower bound.
        min: f32,
        /// Inclusive upper bound.
        max: f32,
        /// The offending value.
        value: f32,
    },

    /// A levels input range that is empty or so narrow the gain is absurd.
    /// See [`MIN_LEVELS_SPAN`](crate::MIN_LEVELS_SPAN).
    #[error(
        "levels input white ({white}) must exceed input black ({black}) \
         by at least {min_span}"
    )]
    DegenerateLevels {
        /// The requested input black point.
        black: f32,
        /// The requested input white point.
        white: f32,
        /// The minimum accepted `white - black`.
        min_span: f32,
    },

    /// A curve needs at least two control points with *distinct* x after
    /// sorting and merging duplicates.
    #[error("a curve needs at least 2 control points with distinct x, got {got}")]
    TooFewCurvePoints {
        /// How many distinct-x points survived merging.
        got: usize,
    },

    /// Posterize is a quantiser; fewer than two output levels is not a
    /// quantisation, and more than 256 cannot be distinguished in 8-bit output.
    #[error("posterize needs 2..=256 levels, got {got}")]
    PosterizeLevels {
        /// The requested level count.
        got: u32,
    },

    /// A gradient map needs at least two stops with distinct positions.
    #[error("a gradient map needs at least 2 stops with distinct positions, got {got}")]
    TooFewGradientStops {
        /// How many distinct-position stops survived merging.
        got: usize,
    },

    /// A masked batch call was handed a mask whose length does not match the
    /// pixel buffer. Silently zipping the two would apply the adjustment to a
    /// prefix and leave the rest untouched.
    #[error("mask has {mask} entries but the pixel buffer has {pixels}")]
    MaskLengthMismatch {
        /// Number of pixels in the buffer.
        pixels: usize,
        /// Number of entries in the mask.
        mask: usize,
    },
}

/// Reject `NaN` and infinities.
pub(crate) fn finite(name: &'static str, value: f32) -> Result<f32, AdjustmentError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(AdjustmentError::NotFinite { name, value })
    }
}

/// Reject non-finite values and values outside `min..=max`.
pub(crate) fn in_range(
    name: &'static str,
    value: f32,
    min: f32,
    max: f32,
) -> Result<f32, AdjustmentError> {
    let value = finite(name, value)?;
    if value < min || value > max {
        return Err(AdjustmentError::OutOfRange {
            name,
            min,
            max,
            value,
        });
    }
    Ok(value)
}

/// Reject a triple, naming the same parameter for each channel.
pub(crate) fn triple_in_range(
    name: &'static str,
    value: [f32; 3],
    min: f32,
    max: f32,
) -> Result<[f32; 3], AdjustmentError> {
    for v in value {
        in_range(name, v, min, max)?;
    }
    Ok(value)
}

/// Coerce a value into `min..=max`, substituting `fallback` for non-finite
/// input. Used only by the *lenient* conversions from a stored document, where
/// refusing to open the file would be worse than clamping a slider.
pub(crate) fn lenient(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `NaN != NaN`, so the derived `PartialEq` cannot be used to assert on a
    /// `NotFinite { value: NaN }`; match on the shape instead.
    #[test]
    fn finite_rejects_nan_and_infinities() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            match finite("x", bad) {
                Err(AdjustmentError::NotFinite { name, value }) => {
                    assert_eq!(name, "x");
                    assert_eq!(value.is_nan(), bad.is_nan());
                    assert!(!value.is_finite());
                }
                other => panic!("{bad} should have been rejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn finite_accepts_extremes() {
        assert_eq!(finite("x", f32::MAX), Ok(f32::MAX));
        assert_eq!(finite("x", -0.0), Ok(-0.0));
    }

    #[test]
    fn in_range_reports_the_bounds_it_enforced() {
        assert_eq!(
            in_range("gamma", 12.0, 0.01, 10.0),
            Err(AdjustmentError::OutOfRange {
                name: "gamma",
                min: 0.01,
                max: 10.0,
                value: 12.0,
            })
        );
        assert_eq!(in_range("gamma", 10.0, 0.01, 10.0), Ok(10.0));
    }

    #[test]
    fn lenient_clamps_and_substitutes() {
        assert_eq!(lenient(5.0, -1.0, 1.0, 0.0), 1.0);
        assert_eq!(lenient(-5.0, -1.0, 1.0, 0.0), -1.0);
        assert_eq!(lenient(f32::NAN, -1.0, 1.0, 0.25), 0.25);
        assert_eq!(lenient(0.5, -1.0, 1.0, 0.0), 0.5);
    }

    #[test]
    fn error_messages_name_the_parameter() {
        let e = AdjustmentError::OutOfRange {
            name: "density",
            min: 0.0,
            max: 1.0,
            value: 3.0,
        };
        assert!(e.to_string().contains("density"), "{e}");
    }
}
