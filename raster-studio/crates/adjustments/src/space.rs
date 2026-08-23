//! The two colour representations an adjustment can be defined on, as distinct
//! types.
//!
//! Before this module the whole crate spoke in bare `f32`s: `levels(c, black,
//! white, gamma)` and `exposure(linear_c, stops)` were interchangeable at a
//! call site, and nothing stopped a caller from handing a linear working-space
//! pixel to a function whose maths is only meaningful on a gamma-encoded one.
//! That is not a hypothetical: `levels`' black/white sliders are positions on
//! the *encoded* ramp, so feeding it linear light silently moves every slider
//! to the wrong place.
//!
//! [`LinearRgb`] and [`EncodedRgb`] make the two non-interchangeable, and the
//! only ways across are [`LinearRgb::encode`] and [`EncodedRgb::decode`], both
//! of which demand the document's [`ColorSpace`]. An adjustment declares which
//! side it lives on through [`WorkingSpace`], and — more importantly than the
//! declaration — the internal operation types are *shaped* so that a
//! linear-space operation is handed no colour space at all and therefore
//! cannot depend on one. See [`crate::PreparedAdjustment::working_space`].

use color::ColorSpace;

/// Scene-referred, **unbounded** linear-light RGB: the document's working
/// space.
///
/// Values below `0.0` and above `1.0` are ordinary and are never clipped here.
/// That is the whole point of non-destructive editing: an exposure lift that
/// pushes a highlight to `4.0` must survive until a later adjustment pulls it
/// back down. Clamping belongs at display and export, not in an adjustment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearRgb(pub [f32; 3]);

/// Display-referred, gamma-encoded RGB in some [`ColorSpace`].
///
/// Nominally `0.0..=1.0`. Values outside that range are *representable* and are
/// passed through by every adjustment that can, because a scene-referred
/// highlight encodes to more than `1.0` and throwing it away would be the same
/// lossy round trip this crate exists to avoid.
///
/// The adjustments that clamp, and only because their definition is bounded,
/// are: posterize and threshold (fixed output alphabets), the gradient map (its
/// output is a ramp colour), the HSL-based vibrance and hue/saturation plus
/// black & white's optional tint, and — for the value they *measure*, never for
/// the value they return — colour balance's band weights and selective colour's
/// range weights and ink separation. Each says so in its own documentation, and
/// the full list is in the crate docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncodedRgb(pub [f32; 3]);

impl LinearRgb {
    /// Wrap a linear triple.
    pub const fn new(rgb: [f32; 3]) -> Self {
        Self(rgb)
    }

    /// The underlying triple.
    pub const fn get(self) -> [f32; 3] {
        self.0
    }

    /// Encode into `space` for a gamma-domain adjustment.
    pub fn encode(self, space: &ColorSpace) -> EncodedRgb {
        EncodedRgb(color::from_linear(space, self.0))
    }

    /// Rec. 709 relative luminance of the linear triple. Unclamped, and
    /// negative for a negative colour.
    pub fn luminance(self) -> f32 {
        color::linear_srgb_luminance(self.0)
    }

    /// Per-channel map, staying in the linear domain.
    pub(crate) fn map(self, f: impl Fn(f32) -> f32) -> Self {
        Self(self.0.map(f))
    }
}

impl EncodedRgb {
    /// Wrap an encoded triple.
    pub const fn new(rgb: [f32; 3]) -> Self {
        Self(rgb)
    }

    /// The underlying triple.
    pub const fn get(self) -> [f32; 3] {
        self.0
    }

    /// Decode out of `space` back into the linear working space.
    pub fn decode(self, space: &ColorSpace) -> LinearRgb {
        LinearRgb(color::to_linear(space, self.0))
    }

    /// Rec. 709 weighted sum of the *encoded* values.
    ///
    /// This is deliberately not a luminance: it is the gamma-domain "gray" that
    /// threshold and gradient-map style operations are conventionally defined
    /// on. For real luminance, [`decode`](Self::decode) first and use
    /// [`LinearRgb::luminance`].
    pub fn luma(self) -> f32 {
        color::REC709_LUMA[0] * self.0[0]
            + color::REC709_LUMA[1] * self.0[1]
            + color::REC709_LUMA[2] * self.0[2]
    }

    /// Per-channel map, staying in the encoded domain.
    pub(crate) fn map(self, f: impl Fn(f32) -> f32) -> Self {
        Self(self.0.map(f))
    }
}

/// Which representation an adjustment's maths is defined on.
///
/// Returned by [`crate::Adjustment::working_space`] and, structurally, by
/// [`crate::PreparedAdjustment::working_space`]. The two are checked against
/// each other by `declared_working_space_matches_the_prepared_shape`, and
/// `linear_space_adjustments_ignore_the_document_space` checks the claim that
/// actually matters: a [`WorkingSpace::Linear`] adjustment produces the same
/// output whatever colour space the document is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkingSpace {
    /// Operates on unbounded linear light. A stop really is a doubling.
    Linear,
    /// Operates on gamma-encoded values in the document's colour space.
    Encoded,
}

/// Clamp into the display range. Used only where an operation is *defined*
/// on `0..=1`.
pub(crate) fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// `v.powf(e)` extended to negatives by mirroring about the origin, and made
/// exactly the identity at `e == 1.0`.
///
/// Both properties matter. `powf` on a negative base with a fractional exponent
/// is `NaN`, so a gamma applied to a below-black value would poison the pixel;
/// mirroring keeps the function odd and monotone instead. And `powf(x, 1.0)` is
/// not guaranteed to return `x` bit-for-bit, which would make a gamma of `1.0`
/// a visible no-op that nonetheless changes every pixel.
pub(crate) fn signed_powf(v: f32, exponent: f32) -> f32 {
    if exponent == 1.0 {
        return v;
    }
    if v < 0.0 {
        -(-v).powf(exponent)
    } else {
        v.powf(exponent)
    }
}

/// Move `v` toward `1.0` for a positive `amount` and toward `0.0` for a
/// negative one, reaching the endpoint exactly at `±1.0` and returning `v`
/// bit-for-bit at `0.0`.
pub(crate) fn toward(v: f32, amount: f32) -> f32 {
    if amount == 0.0 {
        v
    } else if amount > 0.0 {
        v + (1.0 - v) * amount
    } else {
        v + v * amount
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trips_through_a_space() {
        let lin = LinearRgb([0.02, 0.4, 0.85]);
        for space in [
            ColorSpace::Srgb,
            ColorSpace::LinearSrgb,
            ColorSpace::DisplayP3,
        ] {
            let back = lin.encode(&space).decode(&space);
            for i in 0..3 {
                assert!(
                    (back.0[i] - lin.0[i]).abs() < 1e-5,
                    "{space:?} channel {i}: {back:?}"
                );
            }
        }
    }

    #[test]
    fn encoding_does_not_clip_a_highlight() {
        let hot = LinearRgb([4.0, 0.5, 0.25]);
        let enc = hot.encode(&ColorSpace::Srgb);
        assert!(enc.0[0] > 1.0, "highlight was clipped to {}", enc.0[0]);
        let back = hot.encode(&ColorSpace::Srgb).decode(&ColorSpace::Srgb);
        assert!((back.0[0] - 4.0).abs() < 1e-3, "{back:?}");
    }

    #[test]
    fn signed_powf_is_odd_monotone_and_exact_at_one() {
        // Exact identity at exponent 1 for a value `powf` would perturb.
        let awkward = 0.1234_5678_f32;
        assert_eq!(signed_powf(awkward, 1.0), awkward);
        // Odd: f(-x) == -f(x).
        for v in [0.25f32, 0.5, 1.0, 3.0] {
            assert_eq!(signed_powf(-v, 0.45), -signed_powf(v, 0.45));
        }
        // Never NaN on a negative base, which is what plain `powf` would do.
        assert!(!signed_powf(-0.5, 0.45).is_nan());
        assert!((-0.5f32).powf(0.45).is_nan(), "premise of the test changed");
        // Monotone increasing.
        let mut prev = f32::NEG_INFINITY;
        for i in -20..=20 {
            let v = i as f32 / 10.0;
            let out = signed_powf(v, 2.2);
            assert!(out > prev, "not monotone at {v}");
            prev = out;
        }
    }

    #[test]
    fn toward_hits_both_endpoints_and_is_exact_at_zero() {
        assert_eq!(toward(0.3, 0.0), 0.3);
        assert_eq!(toward(0.3, 1.0), 1.0);
        assert_eq!(toward(0.3, -1.0), 0.0);
        assert!((toward(0.4, 0.5) - 0.7).abs() < 1e-6);
        assert!((toward(0.4, -0.5) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn luma_and_luminance_are_different_quantities() {
        let enc = EncodedRgb([0.5, 0.5, 0.5]);
        let lin = enc.decode(&ColorSpace::Srgb);
        assert!((enc.luma() - 0.5).abs() < 1e-6);
        // Mid gray encodes to roughly 21% of the light, not 50%.
        assert!(
            (lin.luminance() - 0.2140).abs() < 1e-3,
            "{}",
            lin.luminance()
        );
    }
}
