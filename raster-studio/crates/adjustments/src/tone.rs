//! Tone adjustments: brightness/contrast, levels, curves, exposure, invert,
//! posterize and threshold.
//!
//! Working space is stated per type and is enforced by the argument types:
//! everything here takes an [`EncodedRgb`] except [`ExposureParams`], which
//! takes a [`LinearRgb`] because a photographic stop is a doubling of *light*,
//! and `2^stops` applied to a gamma-encoded value is not that.

use crate::curve::Curve;
use crate::error::{in_range, AdjustmentError};
use crate::space::{clamp01, signed_powf, EncodedRgb, LinearRgb};

/// The narrowest levels input range that is accepted.
///
/// The gain a levels adjustment applies is `1 / (white - black)`, so an empty
/// or near-empty range is a near-infinite gain: the pre-validation code used
/// `(white - black).max(1e-5)` and turned `white <= black` into a 100000x step
/// function that mapped almost every pixel to pure black or pure white. A
/// thousandth of the range is still an extreme 1000x gain, and anything
/// narrower is a data-entry error rather than an edit.
pub const MIN_LEVELS_SPAN: f32 = 1e-3;

/// Smallest accepted gamma. Values at or below zero used to invert or collapse
/// the transfer function entirely.
pub const MIN_GAMMA: f32 = 0.01;

/// Largest accepted gamma.
pub const MAX_GAMMA: f32 = 100.0;

// ---------------------------------------------------------------------------
// Brightness / contrast
// ---------------------------------------------------------------------------

/// Brightness and contrast, on **gamma-encoded** values.
///
/// Contrast pivots about mid-encoded-gray (`0.5`), which is where the control
/// is expected to pivot; brightness is a simple offset after it. Neither output
/// is clamped, so a highlight pushed above `1.0` survives for a later
/// adjustment to bring back.
///
/// The slope about the pivot is exactly
///
/// * `1 / (1 - 0.99 · contrast)` for `contrast >= 0`, and
/// * `1 + contrast` for `contrast < 0`,
///
/// which is what [`slope`](Self::slope) computes and what a GPU shader claiming
/// to match this must implement. The `0.99` is not decoration: without it
/// `contrast == 1.0` divides by zero, and with it the slope tops out at exactly
/// `100`, a hard curve rather than a step. At `contrast == 0.5` that is
/// `1.98020` rather than the `2.0` the un-damped form would give.
///
/// The alternative `tan((contrast + 1)·π/4)` was rejected because it evaluates
/// to `0.99999994` rather than `1.0` at zero contrast in `f32`, which would
/// make the neutral position of the slider move every pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrightnessContrast {
    brightness: f32,
    contrast: f32,
}

impl BrightnessContrast {
    /// Neutral: both controls at zero.
    pub const IDENTITY: Self = Self {
        brightness: 0.0,
        contrast: 0.0,
    };

    /// `brightness` and `contrast` both in `-1.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] or [`AdjustmentError::OutOfRange`].
    pub fn new(brightness: f32, contrast: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            brightness: in_range("brightness", brightness, -1.0, 1.0)?,
            contrast: in_range("contrast", contrast, -1.0, 1.0)?,
        })
    }

    /// The brightness offset.
    pub fn brightness(&self) -> f32 {
        self.brightness
    }

    /// The contrast amount.
    pub fn contrast(&self) -> f32 {
        self.contrast
    }

    /// Whether this instance cannot change any pixel.
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0 && self.contrast == 0.0
    }

    /// Slope applied about the `0.5` pivot:
    /// `1 / (1 - 0.99 · contrast)` above zero, `1 + contrast` below it.
    ///
    /// Public because the type-level documentation promises a specific curve
    /// and the GPU shader in `render` has to reproduce it; a number a shader
    /// author has to re-derive from prose is a number that will drift.
    pub fn slope(&self) -> f32 {
        if self.contrast >= 0.0 {
            1.0 / (1.0 - self.contrast * 0.99)
        } else {
            1.0 + self.contrast
        }
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        if self.is_identity() {
            return enc;
        }
        let s = self.slope();
        let b = self.brightness;
        enc.map(|v| (v - 0.5) * s + 0.5 + b)
    }
}

// ---------------------------------------------------------------------------
// Levels
// ---------------------------------------------------------------------------

/// One channel's levels mapping, on **gamma-encoded** values.
///
/// `input_black`/`input_white` select the part of the encoded ramp that is
/// stretched to `output_black`..`output_white`, with `gamma` bending the
/// midtones in between. The output pair may be inverted
/// (`output_white < output_black`); that is how a Levels dialog inverts a
/// channel, and it is allowed.
///
/// The normalised value is **not** clamped to `0..=1` before the gamma is
/// applied — `signed_powf` extends the power to negatives by mirroring — so
/// an encoded highlight above the white point stays above it instead of being
/// crushed. That is the difference between levels you can pull back and levels
/// you cannot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelsChannel {
    input_black: f32,
    input_white: f32,
    gamma: f32,
    output_black: f32,
    output_white: f32,
}

impl LevelsChannel {
    /// The mapping that returns its input unchanged.
    pub const IDENTITY: Self = Self {
        input_black: 0.0,
        input_white: 1.0,
        gamma: 1.0,
        output_black: 0.0,
        output_white: 1.0,
    };

    /// Input black/white points in `0.0..=1.0` and a gamma in
    /// [`MIN_GAMMA`]`..=`[`MAX_GAMMA`]. Output range defaults to the full
    /// `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// * [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`] for a
    ///   parameter outside its domain — in particular a `gamma <= 0`, which
    ///   used to be silently rewritten to a `1.0 / 1e-5 = 100000` exponent and
    ///   flattened the image to black.
    /// * [`AdjustmentError::DegenerateLevels`] when `input_white` does not
    ///   exceed `input_black` by at least [`MIN_LEVELS_SPAN`].
    pub fn new(input_black: f32, input_white: f32, gamma: f32) -> Result<Self, AdjustmentError> {
        let input_black = in_range("input_black", input_black, 0.0, 1.0)?;
        let input_white = in_range("input_white", input_white, 0.0, 1.0)?;
        let gamma = in_range("gamma", gamma, MIN_GAMMA, MAX_GAMMA)?;
        if input_white - input_black < MIN_LEVELS_SPAN {
            return Err(AdjustmentError::DegenerateLevels {
                black: input_black,
                white: input_white,
                min_span: MIN_LEVELS_SPAN,
            });
        }
        Ok(Self {
            input_black,
            input_white,
            gamma,
            output_black: 0.0,
            output_white: 1.0,
        })
    }

    /// Set the output range. Inverted pairs are permitted.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn with_output(
        mut self,
        output_black: f32,
        output_white: f32,
    ) -> Result<Self, AdjustmentError> {
        self.output_black = in_range("output_black", output_black, 0.0, 1.0)?;
        self.output_white = in_range("output_white", output_white, 0.0, 1.0)?;
        Ok(self)
    }

    /// The input black point.
    pub fn input_black(&self) -> f32 {
        self.input_black
    }

    /// The input white point.
    pub fn input_white(&self) -> f32 {
        self.input_white
    }

    /// The midtone gamma.
    pub fn gamma(&self) -> f32 {
        self.gamma
    }

    /// The output black point.
    pub fn output_black(&self) -> f32 {
        self.output_black
    }

    /// The output white point.
    pub fn output_white(&self) -> f32 {
        self.output_white
    }

    /// Whether this mapping cannot change any value.
    pub fn is_identity(&self) -> bool {
        self.input_black == 0.0
            && self.input_white == 1.0
            && self.gamma == 1.0
            && self.output_black == 0.0
            && self.output_white == 1.0
    }

    /// Apply to one encoded channel value.
    pub fn apply(&self, v: f32) -> f32 {
        if self.is_identity() {
            return v;
        }
        let t = (v - self.input_black) / (self.input_white - self.input_black);
        let g = signed_powf(t, 1.0 / self.gamma);
        self.output_black + g * (self.output_white - self.output_black)
    }
}

impl Default for LevelsChannel {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A full Levels adjustment: a composite mapping plus one per channel.
///
/// Per-channel mappings run first, then the composite, matching the order a
/// Levels dialog implies (the composite curve sits on top of whatever the
/// individual channels did).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Levels {
    /// Applied to all three channels after the per-channel mappings.
    pub composite: LevelsChannel,
    /// Red channel mapping.
    pub red: LevelsChannel,
    /// Green channel mapping.
    pub green: LevelsChannel,
    /// Blue channel mapping.
    pub blue: LevelsChannel,
}

impl Levels {
    /// A Levels that does nothing.
    pub const IDENTITY: Self = Self {
        composite: LevelsChannel::IDENTITY,
        red: LevelsChannel::IDENTITY,
        green: LevelsChannel::IDENTITY,
        blue: LevelsChannel::IDENTITY,
    };

    /// Composite-only Levels.
    pub const fn composite(channel: LevelsChannel) -> Self {
        Self {
            composite: channel,
            red: LevelsChannel::IDENTITY,
            green: LevelsChannel::IDENTITY,
            blue: LevelsChannel::IDENTITY,
        }
    }

    /// Per-channel Levels with no composite stage.
    pub const fn per_channel(
        red: LevelsChannel,
        green: LevelsChannel,
        blue: LevelsChannel,
    ) -> Self {
        Self {
            composite: LevelsChannel::IDENTITY,
            red,
            green,
            blue,
        }
    }

    /// Add a composite stage to per-channel Levels.
    pub const fn with_composite(self, composite: LevelsChannel) -> Self {
        Self {
            composite,
            red: self.red,
            green: self.green,
            blue: self.blue,
        }
    }

    /// Whether nothing here can change a pixel.
    pub fn is_identity(&self) -> bool {
        self.composite.is_identity()
            && self.red.is_identity()
            && self.green.is_identity()
            && self.blue.is_identity()
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        if self.is_identity() {
            return enc;
        }
        let v = enc.get();
        EncodedRgb([
            self.composite.apply(self.red.apply(v[0])),
            self.composite.apply(self.green.apply(v[1])),
            self.composite.apply(self.blue.apply(v[2])),
        ])
    }
}

// ---------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------

/// A full Curves adjustment: a composite curve plus one per channel, all
/// [monotone cubic](Curve) and all on **gamma-encoded** values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Curves {
    /// Applied to all three channels after the per-channel curves.
    pub composite: Curve,
    /// Red channel curve.
    pub red: Curve,
    /// Green channel curve.
    pub green: Curve,
    /// Blue channel curve.
    pub blue: Curve,
}

impl Curves {
    /// Curves that do nothing.
    pub fn identity() -> Self {
        Self::default()
    }

    /// Composite-only Curves.
    pub fn composite(curve: Curve) -> Self {
        Self {
            composite: curve,
            ..Self::default()
        }
    }

    /// Per-channel Curves with no composite stage.
    pub fn per_channel(red: Curve, green: Curve, blue: Curve) -> Self {
        Self {
            composite: Curve::identity(),
            red,
            green,
            blue,
        }
    }

    /// Whether nothing here can change a pixel.
    pub fn is_identity(&self) -> bool {
        self.composite.is_identity()
            && self.red.is_identity()
            && self.green.is_identity()
            && self.blue.is_identity()
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        if self.is_identity() {
            return enc;
        }
        let v = enc.get();
        EncodedRgb([
            self.composite.eval(self.red.eval(v[0])),
            self.composite.eval(self.green.eval(v[1])),
            self.composite.eval(self.blue.eval(v[2])),
        ])
    }
}

// ---------------------------------------------------------------------------
// Exposure
// ---------------------------------------------------------------------------

/// Exposure, offset and gamma correction, on **unbounded linear light**.
///
/// `out = ((v · 2^stops) + offset) ^ (1/gamma)`, with the power mirrored about
/// the origin for negative values.
///
/// Nothing is clamped. The previous `exposure` ended in `.clamp(0.0, 1.0)`,
/// which meant raising exposure and then lowering it again did not return the
/// original image: every value that went above `1.0` was flattened to exactly
/// `1.0` on the way up and came back as a uniform gray patch where the
/// highlights had been. `exposure_round_trips_through_the_highlights` is the
/// test that would have caught it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposureParams {
    stops: f32,
    offset: f32,
    gamma: f32,
}

impl ExposureParams {
    /// Neutral exposure.
    pub const IDENTITY: Self = Self {
        stops: 0.0,
        offset: 0.0,
        gamma: 1.0,
    };

    /// `stops` in `-32.0..=32.0`, `offset` in `-1.0..=1.0`, `gamma` in
    /// [`MIN_GAMMA`]`..=`[`MAX_GAMMA`].
    ///
    /// The stop bound is not arbitrary: `2^128` overflows `f32`, and beyond
    /// ±32 stops a single-precision working buffer has no usable precision
    /// left anyway.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(stops: f32, offset: f32, gamma: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            stops: in_range("stops", stops, -32.0, 32.0)?,
            offset: in_range("offset", offset, -1.0, 1.0)?,
            gamma: in_range("gamma", gamma, MIN_GAMMA, MAX_GAMMA)?,
        })
    }

    /// Exposure alone.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn stops_only(stops: f32) -> Result<Self, AdjustmentError> {
        Self::new(stops, 0.0, 1.0)
    }

    /// Exposure in stops.
    pub fn stops(&self) -> f32 {
        self.stops
    }

    /// Linear offset added after the gain.
    pub fn offset(&self) -> f32 {
        self.offset
    }

    /// Gamma correction exponent (applied as `1/gamma`).
    pub fn gamma(&self) -> f32 {
        self.gamma
    }

    /// The multiplier `2^stops`.
    pub fn gain(&self) -> f32 {
        2.0f32.powf(self.stops)
    }

    /// Whether this cannot change a pixel.
    pub fn is_identity(&self) -> bool {
        self.stops == 0.0 && self.offset == 0.0 && self.gamma == 1.0
    }

    /// Apply to one linear triple.
    pub fn apply(&self, linear: LinearRgb) -> LinearRgb {
        if self.is_identity() {
            return linear;
        }
        let gain = self.gain();
        let offset = self.offset;
        let inv_gamma = 1.0 / self.gamma;
        linear.map(|v| signed_powf(v * gain + offset, inv_gamma))
    }
}

impl Default for ExposureParams {
    fn default() -> Self {
        Self::IDENTITY
    }
}

// ---------------------------------------------------------------------------
// Invert / posterize / threshold
// ---------------------------------------------------------------------------

/// Invert, on **gamma-encoded** values: `1 - v`.
///
/// Encoded rather than linear on purpose. Inverting linear light gives the
/// photographic negative, which is a different (and much darker-looking)
/// picture from what an Invert command produces; the familiar result is the
/// complement of the *code values*.
pub fn invert(enc: EncodedRgb) -> EncodedRgb {
    enc.map(|v| 1.0 - v)
}

/// Posterize: quantise each **gamma-encoded** channel to `levels` evenly spaced
/// steps.
///
/// This is the one adjustment in the crate that clamps as part of its
/// definition: a quantiser has a fixed output alphabet, and "the nearest of
/// `levels` values in `0..=1`" is what the control means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posterize {
    levels: u32,
}

impl Posterize {
    /// The finest quantisation the control offers. An 8-bit output cannot tell
    /// it from the identity, which is what makes it the right thing for a
    /// corrupt stored level count to degrade to.
    pub const FINEST: Self = Self { levels: 256 };

    /// `levels` in `2..=256`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::PosterizeLevels`].
    pub fn new(levels: u32) -> Result<Self, AdjustmentError> {
        if !(2..=256).contains(&levels) {
            return Err(AdjustmentError::PosterizeLevels { got: levels });
        }
        Ok(Self { levels })
    }

    /// The number of output levels.
    pub fn levels(&self) -> u32 {
        self.levels
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        let n = (self.levels - 1) as f32;
        enc.map(|v| (clamp01(v) * n).round() / n)
    }
}

/// Threshold: every pixel becomes black or white depending on whether its
/// **gamma-encoded** Rec. 709 gray reaches `level`.
///
/// Encoded rather than linear because the control's `0..=1` scale is the
/// familiar 0..255 gray ramp, not a light ratio; thresholding at linear `0.5`
/// would sit at encoded `0.735` and look wrong against the histogram the user
/// is reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Threshold {
    level: f32,
}

impl Threshold {
    /// The control's default position, and the fallback for a corrupt stored
    /// level.
    pub const MIDDLE: Self = Self { level: 0.5 };

    /// `level` in `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(level: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            level: in_range("level", level, 0.0, 1.0)?,
        })
    }

    /// The threshold level.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        let on = if enc.luma() >= self.level { 1.0 } else { 0.0 };
        EncodedRgb([on; 3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- brightness / contrast -------------------------------------------

    #[test]
    fn brightness_contrast_identity_is_bit_exact() {
        let bc = BrightnessContrast::IDENTITY;
        let px = EncodedRgb([0.1234_5678, 0.5, 0.987]);
        assert_eq!(bc.apply(px), px);
        assert_eq!(BrightnessContrast::new(0.0, 0.0), Ok(bc));
    }

    #[test]
    fn brightness_is_an_offset_and_contrast_pivots_at_half() {
        let b = BrightnessContrast::new(0.25, 0.0).unwrap();
        assert!((b.apply(EncodedRgb([0.5; 3])).get()[0] - 0.75).abs() < 1e-6);
        let c = BrightnessContrast::new(0.0, 0.5).unwrap();
        // 0.5 is the pivot and must not move.
        assert!((c.apply(EncodedRgb([0.5; 3])).get()[0] - 0.5).abs() < 1e-6);
        // Slope 1/(1 - 0.495) = 1.980198; 0.6 -> 0.5 + 0.1*slope.
        let out = c.apply(EncodedRgb([0.6; 3])).get()[0];
        assert!((out - (0.5 + 0.1 * (1.0 / 0.505))).abs() < 1e-5, "{out}");
        // Negative contrast flattens toward the pivot.
        let flat = BrightnessContrast::new(0.0, -1.0).unwrap();
        assert!((flat.apply(EncodedRgb([0.9; 3])).get()[0] - 0.5).abs() < 1e-6);
    }

    /// The type-level documentation names an exact slope formula, and lib.rs
    /// promises these CPU implementations are what the GPU shaders must match.
    /// A shader author reads the formula, not the source, so the formula has to
    /// be the formula — this pins both branches, the cap, and the fact that the
    /// documented `1/(1 - contrast)` shorthand is *not* what runs.
    #[test]
    fn the_documented_slope_is_the_slope_that_runs() {
        for c in [0.0f32, 0.1, 0.25, 0.5, 0.75, 0.999, 1.0] {
            let bc = BrightnessContrast::new(0.0, c).unwrap();
            assert_eq!(bc.slope(), 1.0 / (1.0 - 0.99 * c), "contrast {c}");
        }
        for c in [-0.001f32, -0.25, -0.5, -1.0] {
            let bc = BrightnessContrast::new(0.0, c).unwrap();
            assert_eq!(bc.slope(), 1.0 + c, "contrast {c}");
        }
        // Full contrast is a slope of 100, not a division by zero.
        let hardest = BrightnessContrast::new(0.0, 1.0).unwrap().slope();
        assert!(
            hardest.is_finite() && (hardest - 100.0).abs() < 0.01,
            "{hardest}"
        );
        // And the un-damped `1/(1 - contrast)` really is a different number, so
        // the distinction the doc now draws is a real one.
        let half = BrightnessContrast::new(0.0, 0.5).unwrap().slope();
        assert!((half - 1.980_198).abs() < 1e-5, "{half}");
        assert!((half - 2.0).abs() > 1e-3, "{half}");
        // The slope really is the slope `apply` uses, measured off two points.
        let bc = BrightnessContrast::new(0.3, 0.4).unwrap();
        let lo = bc.apply(EncodedRgb([0.25; 3])).get()[0];
        let hi = bc.apply(EncodedRgb([0.75; 3])).get()[0];
        assert!(((hi - lo) / 0.5 - bc.slope()).abs() < 1e-5, "{lo} {hi}");
    }

    #[test]
    fn brightness_contrast_does_not_clamp() {
        let b = BrightnessContrast::new(1.0, 0.0).unwrap();
        assert!(b.apply(EncodedRgb([0.8; 3])).get()[0] > 1.0);
        let d = BrightnessContrast::new(-1.0, 0.0).unwrap();
        assert!(d.apply(EncodedRgb([0.2; 3])).get()[0] < 0.0);
    }

    #[test]
    fn brightness_contrast_rejects_out_of_range() {
        assert!(matches!(
            BrightnessContrast::new(2.0, 0.0),
            Err(AdjustmentError::OutOfRange {
                name: "brightness",
                ..
            })
        ));
        assert!(matches!(
            BrightnessContrast::new(0.0, f32::NAN),
            Err(AdjustmentError::NotFinite {
                name: "contrast",
                ..
            })
        ));
    }

    // --- levels -----------------------------------------------------------

    #[test]
    fn levels_identity_is_bit_exact() {
        let ch = LevelsChannel::new(0.0, 1.0, 1.0).unwrap();
        assert_eq!(ch, LevelsChannel::IDENTITY);
        for v in [0.0f32, 0.1234_5678, 0.5, 1.0, 2.5, -0.4] {
            assert_eq!(ch.apply(v), v);
        }
        let px = EncodedRgb([0.31, 0.62, 0.93]);
        assert_eq!(Levels::IDENTITY.apply(px), px);
    }

    #[test]
    fn levels_reference_values() {
        let ch = LevelsChannel::new(0.25, 0.75, 1.0).unwrap();
        assert!((ch.apply(0.25) - 0.0).abs() < 1e-6);
        assert!((ch.apply(0.75) - 1.0).abs() < 1e-6);
        assert!((ch.apply(0.5) - 0.5).abs() < 1e-6);
        // Gamma 2.0 lifts the midtone: 0.5^(1/2) = 1/sqrt(2).
        let g = LevelsChannel::new(0.0, 1.0, 2.0).unwrap();
        let expect = std::f32::consts::FRAC_1_SQRT_2;
        assert!((g.apply(0.5) - expect).abs() < 1e-6, "{}", g.apply(0.5));
        // Gamma 0.5 darkens it: 0.5^2 = 0.25.
        let d = LevelsChannel::new(0.0, 1.0, 0.5).unwrap();
        assert!((d.apply(0.5) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn levels_output_range_can_be_inverted() {
        let ch = LevelsChannel::new(0.0, 1.0, 1.0)
            .unwrap()
            .with_output(1.0, 0.0)
            .unwrap();
        assert!((ch.apply(0.0) - 1.0).abs() < 1e-6);
        assert!((ch.apply(1.0) - 0.0).abs() < 1e-6);
    }

    /// The regression this task names: `gamma <= 0` used to become a
    /// `1.0 / 1e-5` exponent and map every value below 1.0 to 0.
    #[test]
    fn levels_rejects_a_non_positive_gamma_instead_of_collapsing_to_black() {
        for bad in [0.0f32, -1.0, -0.0001] {
            assert!(
                matches!(
                    LevelsChannel::new(0.0, 1.0, bad),
                    Err(AdjustmentError::OutOfRange { name: "gamma", .. })
                ),
                "gamma {bad} was accepted"
            );
        }
        assert!(matches!(
            LevelsChannel::new(0.0, 1.0, f32::NAN),
            Err(AdjustmentError::NotFinite { name: "gamma", .. })
        ));
        // And the boundary is where the constant says it is.
        assert!(LevelsChannel::new(0.0, 1.0, MIN_GAMMA).is_ok());
        assert!(LevelsChannel::new(0.0, 1.0, MAX_GAMMA).is_ok());
        assert!(LevelsChannel::new(0.0, 1.0, MAX_GAMMA * 1.1).is_err());
    }

    /// The other named regression: `white <= black` used to become a 100000x
    /// gain step.
    #[test]
    fn levels_rejects_a_degenerate_input_range() {
        assert_eq!(
            LevelsChannel::new(0.8, 0.2, 1.0),
            Err(AdjustmentError::DegenerateLevels {
                black: 0.8,
                white: 0.2,
                min_span: MIN_LEVELS_SPAN,
            })
        );
        assert_eq!(
            LevelsChannel::new(0.5, 0.5, 1.0),
            Err(AdjustmentError::DegenerateLevels {
                black: 0.5,
                white: 0.5,
                min_span: MIN_LEVELS_SPAN,
            })
        );
        // Just under and just over the span limit.
        assert!(LevelsChannel::new(0.5, 0.5 + MIN_LEVELS_SPAN * 0.5, 1.0).is_err());
        assert!(LevelsChannel::new(0.5, 0.5 + MIN_LEVELS_SPAN * 2.0, 1.0).is_ok());
    }

    #[test]
    fn levels_preserves_out_of_range_ordering_rather_than_crushing_it() {
        let ch = LevelsChannel::new(0.0, 0.9, 1.0).unwrap();
        // Two distinct highlights above the white point stay distinct.
        let a = ch.apply(1.0);
        let b = ch.apply(1.5);
        assert!(a > 1.0 && b > a, "highlights were crushed: {a}, {b}");
        // A below-black value stays below black rather than becoming NaN.
        let g = LevelsChannel::new(0.1, 1.0, 2.2).unwrap();
        let below = g.apply(0.0);
        assert!(below < 0.0 && below.is_finite(), "{below}");
    }

    #[test]
    fn levels_runs_per_channel_then_composite() {
        let red = LevelsChannel::new(0.0, 0.5, 1.0).unwrap();
        let comp = LevelsChannel::new(0.0, 1.0, 1.0)
            .unwrap()
            .with_output(0.0, 0.5)
            .unwrap();
        let lv = Levels::per_channel(red, LevelsChannel::IDENTITY, LevelsChannel::IDENTITY)
            .with_composite(comp);
        let out = lv.apply(EncodedRgb([0.25, 0.4, 0.6])).get();
        // red: 0.25 -> 0.5, then composite halves it -> 0.25
        assert!((out[0] - 0.25).abs() < 1e-6, "{out:?}");
        // green/blue: only the composite halving.
        assert!((out[1] - 0.2).abs() < 1e-6, "{out:?}");
        assert!((out[2] - 0.3).abs() < 1e-6, "{out:?}");
    }

    // --- curves -----------------------------------------------------------

    #[test]
    fn curves_identity_is_bit_exact_and_per_channel_works() {
        let px = EncodedRgb([0.2, 0.4, 0.6]);
        assert_eq!(Curves::identity().apply(px), px);
        let lift = Curve::new(&[[0.0, 0.1], [1.0, 1.0]]).unwrap();
        let c = Curves::per_channel(lift, Curve::identity(), Curve::identity());
        let out = c.apply(px).get();
        assert!(out[0] > 0.2);
        assert_eq!(out[1], 0.4);
        assert_eq!(out[2], 0.6);
    }

    /// Curves must pass scene-referred values through like every other
    /// unbounded adjustment. Three encoded highlights 0.2 apart go in; three
    /// strictly ordered values, at least one still above `1.0`, must come out.
    /// Holding the end knot's y — the behaviour a Curves *dialog* has — would
    /// return exactly `1.0` for all three and no later exposure pull-back could
    /// separate them again.
    #[test]
    fn highlights_stay_distinct_through_curves() {
        let c = Curves::composite(Curve::new(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]).unwrap());
        let out: Vec<[f32; 3]> = [1.2f32, 1.4, 1.6]
            .iter()
            .map(|v| c.apply(EncodedRgb([*v; 3])).get())
            .collect();
        for ch in 0..3 {
            assert!(
                out[0][ch] < out[1][ch] && out[1][ch] < out[2][ch],
                "curves collapsed channel {ch}: {out:?}"
            );
        }
        assert!(
            out.iter().flatten().any(|v| *v > 1.0),
            "every highlight was pulled into the display range: {out:?}"
        );
    }

    // --- exposure ---------------------------------------------------------

    #[test]
    fn exposure_one_stop_doubles_the_light() {
        let e = ExposureParams::stops_only(1.0).unwrap();
        let out = e.apply(LinearRgb([0.25, 0.1, 0.0])).get();
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1] - 0.2).abs() < 1e-6);
        assert_eq!(out[2], 0.0);
    }

    #[test]
    fn exposure_identity_is_bit_exact() {
        let px = LinearRgb([0.1234_5678, 3.7, -0.2]);
        assert_eq!(ExposureParams::IDENTITY.apply(px), px);
        assert_eq!(
            ExposureParams::new(0.0, 0.0, 1.0),
            Ok(ExposureParams::IDENTITY)
        );
    }

    /// The headline regression: the old `exposure` clamped to `0..=1`, so this
    /// round trip lost every highlight.
    #[test]
    fn exposure_round_trips_through_the_highlights() {
        let up = ExposureParams::stops_only(2.0).unwrap();
        let down = ExposureParams::stops_only(-2.0).unwrap();
        for v in [0.05f32, 0.4, 0.9, 1.0, 3.75] {
            let px = LinearRgb([v; 3]);
            let round = down.apply(up.apply(px));
            // 2^2 and 2^-2 are exact in binary, so this is bit-exact.
            assert_eq!(round, px, "round trip lost {v}");
        }
        // And the intermediate really did leave the display range, which is
        // exactly what the old clamp destroyed.
        assert!(up.apply(LinearRgb([0.9; 3])).get()[0] > 1.0);
        // Two distinct highlights stay distinct after the lift.
        let a = up.apply(LinearRgb([1.0; 3])).get()[0];
        let b = up.apply(LinearRgb([2.0; 3])).get()[0];
        assert!(b > a, "highlights were flattened: {a} vs {b}");
    }

    #[test]
    fn exposure_offset_and_gamma() {
        let e = ExposureParams::new(0.0, 0.1, 1.0).unwrap();
        assert!((e.apply(LinearRgb([0.2; 3])).get()[0] - 0.3).abs() < 1e-6);
        let g = ExposureParams::new(0.0, 0.0, 2.0).unwrap();
        assert!((g.apply(LinearRgb([0.25; 3])).get()[0] - 0.5).abs() < 1e-6);
        // A negative offset drives the value below zero without producing NaN.
        let n = ExposureParams::new(0.0, -0.5, 2.2).unwrap();
        let out = n.apply(LinearRgb([0.1; 3])).get()[0];
        assert!(out < 0.0 && out.is_finite(), "{out}");
    }

    #[test]
    fn exposure_rejects_out_of_range() {
        assert!(matches!(
            ExposureParams::new(100.0, 0.0, 1.0),
            Err(AdjustmentError::OutOfRange { name: "stops", .. })
        ));
        assert!(matches!(
            ExposureParams::new(0.0, 0.0, 0.0),
            Err(AdjustmentError::OutOfRange { name: "gamma", .. })
        ));
        assert!(matches!(
            ExposureParams::new(0.0, 5.0, 1.0),
            Err(AdjustmentError::OutOfRange { name: "offset", .. })
        ));
    }

    // --- invert / posterize / threshold -----------------------------------

    #[test]
    fn invert_is_an_involution() {
        for v in [0.0f32, 0.25, 0.5, 1.0] {
            let px = EncodedRgb([v, 1.0 - v, 0.5]);
            assert_eq!(invert(invert(px)), px);
        }
        assert_eq!(invert(EncodedRgb([0.25, 0.5, 1.0])).get(), [0.75, 0.5, 0.0]);
    }

    #[test]
    fn posterize_reference_values() {
        let p = Posterize::new(2).unwrap();
        assert_eq!(p.apply(EncodedRgb([0.1, 0.6, 0.5])).get(), [0.0, 1.0, 1.0]);
        let q = Posterize::new(5).unwrap();
        // Steps at 0, 0.25, 0.5, 0.75, 1.
        assert_eq!(q.apply(EncodedRgb([0.3, 0.6, 0.9])).get(), [0.25, 0.5, 1.0]);
        // Out-of-range input is clamped into the alphabet.
        assert_eq!(q.apply(EncodedRgb([-2.0, 5.0, 0.0])).get(), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn posterize_with_256_levels_is_an_8_bit_quantiser() {
        let p = Posterize::new(256).unwrap();
        for code in [0u32, 1, 77, 128, 254, 255] {
            let v = code as f32 / 255.0;
            assert!((p.apply(EncodedRgb([v; 3])).get()[0] - v).abs() < 1e-6);
        }
    }

    #[test]
    fn posterize_rejects_degenerate_level_counts() {
        for bad in [0u32, 1, 257, 100_000] {
            assert_eq!(
                Posterize::new(bad),
                Err(AdjustmentError::PosterizeLevels { got: bad })
            );
        }
    }

    #[test]
    fn threshold_splits_at_the_encoded_gray() {
        let t = Threshold::new(0.5).unwrap();
        assert_eq!(t.apply(EncodedRgb([0.49; 3])).get(), [0.0; 3]);
        assert_eq!(t.apply(EncodedRgb([0.5; 3])).get(), [1.0; 3]);
        // Green dominates the Rec.709 weighting, so pure green passes and pure
        // blue does not.
        assert_eq!(t.apply(EncodedRgb([0.0, 1.0, 0.0])).get(), [1.0; 3]);
        assert_eq!(t.apply(EncodedRgb([0.0, 0.0, 1.0])).get(), [0.0; 3]);
    }

    #[test]
    fn threshold_rejects_out_of_range() {
        assert!(matches!(
            Threshold::new(1.5),
            Err(AdjustmentError::OutOfRange { name: "level", .. })
        ));
    }
}
