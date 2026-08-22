//! Premultiplied-alpha conversion, and the single threshold that the renderer
//! and the compositor must both use.

/// Alpha at or below this is treated as fully transparent by [`unpremultiply`].
///
/// Exactly `1/65536`, which is marginally *below* one quantization step of a
/// 16-bit UNORM — the deepest integer alpha format the pipeline stores. That
/// step is `1/65535` (65536 codes, `0 -> 0.0` and `65535 -> 1.0`), about
/// `1.52590e-5`; this constant is about `1.52588e-5`. The conservative side is
/// chosen deliberately: `1/65536` is a power of two and therefore exact in
/// `f32`, `f64` and in any shader that recomputes it, so every stage compares
/// against bit-identical bytes. Rounding the other way would make an alpha of
/// exactly one storage step fall on the transparent side of the test.
///
/// Dividing by an alpha smaller than this multiplies the stored colour's
/// quantization error by more than 65536, which shows up as saturated speckle
/// along antialiased edges; below one storage step there is no colour
/// information left to recover anyway.
///
/// It is public and shared on purpose: the renderer, the compositor and any
/// exporter must agree exactly, or a pixel that one stage treats as opaque
/// enough to divide will be zeroed by the next and the edge will change colour.
/// It is deliberately *much* larger than [`f32::EPSILON`] (about `1.2e-7`),
/// which is a floating-point resolution, not a meaningful alpha threshold.
pub const UNPREMULTIPLY_ALPHA_EPSILON: f32 = 1.0 / 65_536.0;

/// Premultiply straight-alpha RGBA into premultiplied form.
///
/// Compositing is done in linear-premultiplied space; convert straight-alpha
/// source pixels with this before blending. Channels are not clamped, so
/// out-of-range working-space values pass through scaled rather than clipped.
#[inline]
pub fn premultiply(rgba: [f32; 4]) -> [f32; 4] {
    let a = rgba[3];
    [rgba[0] * a, rgba[1] * a, rgba[2] * a, a]
}

/// Undo [`premultiply`], returning straight-alpha RGBA.
///
/// Alpha at or below [`UNPREMULTIPLY_ALPHA_EPSILON`] yields a fully transparent
/// black pixel instead of a division; see that constant for why.
#[inline]
pub fn unpremultiply(rgba: [f32; 4]) -> [f32; 4] {
    let a = rgba[3];
    if a <= UNPREMULTIPLY_ALPHA_EPSILON {
        [0.0, 0.0, 0.0, 0.0]
    } else {
        [rgba[0] / a, rgba[1] / a, rgba[2] / a, a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_roundtrip() {
        let px = [0.6, 0.3, 0.9, 0.5];
        let round = unpremultiply(premultiply(px));
        for i in 0..4 {
            assert!((round[i] - px[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn unpremultiply_zero_alpha_is_transparent() {
        assert_eq!(unpremultiply([0.0, 0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 0.0]);
    }

    /// The constant's *stated definition* is the one thing that must not be off
    /// by one, because three subsystems are told to reproduce it from the doc.
    /// Asserting against `1.0 / (u16::MAX as f32 + 1.0)` would only re-encode
    /// whatever the constant already is; these assertions name the real 16-bit
    /// UNORM step independently and pin the relationship to it.
    #[test]
    fn epsilon_is_just_under_one_16_bit_unorm_step() {
        // A 16-bit UNORM has 65536 codes with 0 -> 0.0 and 65535 -> 1.0, so one
        // quantization step is 1/65535, not 1/65536.
        let unorm_step = 1.0 / u16::MAX as f32;
        assert_eq!(UNPREMULTIPLY_ALPHA_EPSILON, 1.0 / 65_536.0);
        assert!(
            UNPREMULTIPLY_ALPHA_EPSILON < unorm_step,
            "epsilon {UNPREMULTIPLY_ALPHA_EPSILON:e} must stay below the {unorm_step:e} step"
        );
        // They really are distinct f32 values, so "one step" would be wrong
        // rather than merely imprecise.
        assert_ne!(UNPREMULTIPLY_ALPHA_EPSILON, unorm_step);
        // Exact in f32 because it is a power of two: the mantissa is all zeros.
        // That is the reason for choosing the conservative side — every stage
        // and every shader recomputing `1.0 / 65536.0` gets the same bits.
        assert_eq!(UNPREMULTIPLY_ALPHA_EPSILON.to_bits() & 0x007f_ffff, 0);
        // The behavioural consequence: an alpha of exactly one storage step is
        // still reconstructed, not zeroed.
        let straight = unpremultiply([0.5 * unorm_step, 0.0, 0.0, unorm_step]);
        assert_eq!(straight[3], unorm_step, "one UNORM step was treated as transparent");
        assert!((straight[0] - 0.5).abs() < 1e-3, "{straight:?}");
        // Reconstructing colour from a sub-step alpha would amplify the
        // stored quantization error by more than 65536x.
        assert!(1.0 / UNPREMULTIPLY_ALPHA_EPSILON >= u16::MAX as f32);
    }

    #[test]
    fn sub_storage_step_alpha_does_not_amplify_noise() {
        // With an f32::EPSILON threshold this divides and returns ~1e5 per
        // channel: a single sub-quantum pixel becomes a blazing speckle.
        let px = [1e-6, 2e-6, 3e-6, 1e-5];
        assert_eq!(unpremultiply(px), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(unpremultiply([0.0, 0.0, 0.0, UNPREMULTIPLY_ALPHA_EPSILON]), [0.0; 4]);
    }

    #[test]
    fn alpha_above_the_threshold_still_divides() {
        let a = UNPREMULTIPLY_ALPHA_EPSILON * 2.0;
        let straight = unpremultiply([0.5 * a, 0.25 * a, 0.0, a]);
        assert!((straight[0] - 0.5).abs() < 1e-4, "{straight:?}");
        assert!((straight[1] - 0.25).abs() < 1e-4, "{straight:?}");
        assert_eq!(straight[3], a);
    }

    #[test]
    fn premultiply_preserves_out_of_range_channels() {
        let px = [2.0, -0.5, 0.0, 0.5];
        assert_eq!(premultiply(px), [1.0, -0.25, 0.0, 0.5]);
        let round = unpremultiply(premultiply(px));
        for i in 0..4 {
            assert!((round[i] - px[i]).abs() < 1e-6, "{round:?}");
        }
    }
}
