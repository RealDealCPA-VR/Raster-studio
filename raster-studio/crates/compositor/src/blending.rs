//! The one place a source sample meets a backdrop sample.
//!
//! [`blend_over`] is the W3C compositing-and-blending model:
//!
//! ```text
//! Cs' = (1 - ab) * Cs + ab * B(Cb, Cs)      // blended source colour
//! ao  = as + ab * (1 - as)                  // Porter-Duff "over"
//! Co  = as * Cs' + Cb_premul * (1 - as)     // premultiplied result
//! ```
//!
//! Two consequences are load-bearing and both are pinned by tests here:
//!
//! * Over a **fully transparent backdrop** `ab` is 0, so `Cs' == Cs` and the
//!   source appears unchanged no matter which of the 27 modes is selected. A
//!   compositor that fed `Cs` straight into `B(Cb, Cs)` would make `Multiply`
//!   over nothing evaluate to black, and the top layer of a document would
//!   vanish.
//! * The maths is done on **straight** colour and returns a **premultiplied**
//!   result, because that is what the accumulator holds. `Cb` is recovered with
//!   [`color::unpremultiply`], which shares its threshold with the renderer.

use color::{from_linear, to_linear, unpremultiply, ColorSpace};
use layer_model::BlendMode;

/// Which encoding the *blend function* `B(Cb, Cs)` sees.
///
/// Alpha compositing itself is always linear — that part is physics, not
/// taste — but the blend functions are a different question. Photoshop's are
/// defined on gamma-encoded values, so `Multiply` there is not the same
/// operation as `Multiply` on linear light.
///
/// [`BlendSpace::Linear`] is the default because this compositor's working
/// space is linear light and that is what makes filtering, resampling and
/// blending mutually consistent. [`BlendSpace::Encoded`] exists for the paths
/// that must reproduce a file's original look rather than a physically correct
/// one (PSD import fidelity, golden-image comparison against another editor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlendSpace {
    /// `B(Cb, Cs)` is evaluated on linear-light values. The default.
    #[default]
    Linear,
    /// `B(Cb, Cs)` is evaluated on values encoded in the document's colour
    /// space, and the result is decoded back to linear before compositing.
    Encoded,
}

/// Everything [`blend_over`] needs beyond the two samples.
#[derive(Debug, Clone, Copy)]
pub struct BlendContext<'a> {
    /// The document's colour space, used only when `blend_space` is
    /// [`BlendSpace::Encoded`].
    pub space: &'a ColorSpace,
    pub blend_space: BlendSpace,
}

impl<'a> BlendContext<'a> {
    /// A context that blends in linear light — the compositor's default.
    pub fn linear(space: &'a ColorSpace) -> Self {
        Self {
            space,
            blend_space: BlendSpace::Linear,
        }
    }
}

/// Blend one straight-alpha source sample over one premultiplied backdrop
/// sample, returning the premultiplied result.
///
/// * `backdrop` is linear **premultiplied** RGBA.
/// * `src_rgb` is linear **straight** RGB; `src_alpha` is its already-resolved
///   alpha (layer alpha × mask × opacity × clip).
///
/// A `src_alpha` of zero returns the backdrop untouched, bit for bit, which is
/// what lets the compositor skip work without changing the answer.
pub fn blend_over(
    backdrop: [f32; 4],
    src_rgb: [f32; 3],
    src_alpha: f32,
    mode: BlendMode,
    ctx: &BlendContext<'_>,
) -> [f32; 4] {
    let sa = layer_model::blend::unit(src_alpha);
    if sa <= 0.0 {
        return backdrop;
    }
    let ab = layer_model::blend::unit(backdrop[3]);
    let straight = unpremultiply(backdrop);
    let cb = [straight[0], straight[1], straight[2]];

    let blended = match ctx.blend_space {
        BlendSpace::Linear => mode.blend_rgb(cb, src_rgb),
        BlendSpace::Encoded => {
            let b = mode.blend_rgb(from_linear(ctx.space, cb), from_linear(ctx.space, src_rgb));
            to_linear(ctx.space, b)
        }
    };

    let mut out = [0.0f32; 4];
    for i in 0..3 {
        // Cs' — the source colour after the backdrop has had its say.
        let cs = (1.0 - ab) * src_rgb[i] + ab * blended[i];
        out[i] = sa * cs + backdrop[i] * (1.0 - sa);
    }
    out[3] = sa + ab * (1.0 - sa);
    out
}

/// Blend a source sample **atop** a premultiplied backdrop: same colour maths
/// as [`blend_over`], but the result keeps the backdrop's alpha.
///
/// This is Porter-Duff `atop`, and it is how a clipping group works. A clipped
/// layer may recolour the base it is clipped to but must never extend it: with
/// plain `over`, a fully opaque clipped layer over a base of alpha 0.5 would
/// composite to alpha 0.75 and the clipping group would visibly grow. Here the
/// alpha out is the alpha in, always.
pub fn blend_atop(
    backdrop: [f32; 4],
    src_rgb: [f32; 3],
    src_alpha: f32,
    mode: BlendMode,
    ctx: &BlendContext<'_>,
) -> [f32; 4] {
    let sa = layer_model::blend::unit(src_alpha);
    let ab = layer_model::blend::unit(backdrop[3]);
    if sa <= 0.0 || ab <= 0.0 {
        return backdrop;
    }
    let straight = unpremultiply(backdrop);
    let cb = [straight[0], straight[1], straight[2]];

    let blended = match ctx.blend_space {
        BlendSpace::Linear => mode.blend_rgb(cb, src_rgb),
        BlendSpace::Encoded => {
            let b = mode.blend_rgb(from_linear(ctx.space, cb), from_linear(ctx.space, src_rgb));
            to_linear(ctx.space, b)
        }
    };

    let mut out = [0.0f32; 4];
    for i in 0..3 {
        let cs = (1.0 - ab) * src_rgb[i] + ab * blended[i];
        out[i] = sa * ab * cs + backdrop[i] * (1.0 - sa);
    }
    out[3] = ab;
    out
}

/// Deterministic uniform noise in `0.0..1.0` for one image-space pixel.
///
/// `Dissolve` needs a per-pixel random draw that is *stable*: the same pixel
/// must get the same number on every frame, in every region, at every tile
/// boundary, or the layer would sparkle as the viewport moves and the
/// region-independence property would be false. Hashing the absolute image
/// coordinate (not a tile-local one) is what buys that.
pub fn dissolve_noise(x: i64, y: i64, level: u8, seed: u64) -> f32 {
    // SplitMix64 finaliser over a mixed coordinate — cheap, no state, and
    // well-distributed in its high bits.
    let mut z = (x as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
        .wrapping_add((level as u64).wrapping_mul(0x1656_67B1_9E37_79F9))
        .wrapping_add(seed);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Top 24 bits over 2^24 — exactly representable in f32 and in [0, 1).
    (z >> 40) as f32 / 16_777_216.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB: ColorSpace = ColorSpace::Srgb;

    fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
        (0..4).all(|i| (a[i] - b[i]).abs() <= tol)
    }

    #[test]
    fn a_zero_alpha_source_returns_the_backdrop_untouched() {
        let ctx = BlendContext::linear(&SRGB);
        let bd = [0.1, 0.2, 0.3, 0.4];
        for mode in BlendMode::ALL {
            assert_eq!(blend_over(bd, [1.0, 1.0, 1.0], 0.0, mode, &ctx), bd);
        }
    }

    #[test]
    fn every_mode_shows_the_source_over_a_fully_transparent_backdrop() {
        // The bug this pins: `B(Cb, Cs)` with `Cb = 0` is black for Multiply,
        // Darken, ColorBurn and friends. The `(1 - ab)` term is what stops the
        // top layer of a document from vanishing.
        let ctx = BlendContext::linear(&SRGB);
        let src = [0.8, 0.5, 0.2];
        for mode in BlendMode::ALL {
            let out = blend_over([0.0; 4], src, 1.0, mode, &ctx);
            assert!(
                close(out, [src[0], src[1], src[2], 1.0], 1e-6),
                "{:?} over nothing gave {out:?}",
                mode
            );
        }
    }

    #[test]
    fn half_alpha_source_over_transparent_keeps_premultiplication() {
        let ctx = BlendContext::linear(&SRGB);
        let out = blend_over([0.0; 4], [1.0, 0.0, 0.0], 0.5, BlendMode::Normal, &ctx);
        assert!(close(out, [0.5, 0.0, 0.0, 0.5], 1e-6), "{out:?}");
    }

    #[test]
    fn normal_over_an_opaque_backdrop_is_a_plain_lerp() {
        let ctx = BlendContext::linear(&SRGB);
        let out = blend_over(
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            0.5,
            BlendMode::Normal,
            &ctx,
        );
        assert!(close(out, [0.5, 0.0, 0.5, 1.0], 1e-6), "{out:?}");
    }

    #[test]
    fn multiply_over_an_opaque_backdrop_multiplies() {
        let ctx = BlendContext::linear(&SRGB);
        let out = blend_over(
            [0.5, 0.5, 0.5, 1.0],
            [0.4, 0.4, 0.4],
            1.0,
            BlendMode::Multiply,
            &ctx,
        );
        assert!(close(out, [0.2, 0.2, 0.2, 1.0], 1e-6), "{out:?}");
    }

    #[test]
    fn a_semi_transparent_backdrop_dilutes_the_blend_function() {
        // ab = 0.5, Cb = 1.0 (premultiplied 0.5), Cs = 0.0, Multiply.
        // B = 0. Cs' = (1 - 0.5)*0 + 0.5*0 = 0. ao = 1. Co = 1*0 + 0.5*0 = 0.
        let ctx = BlendContext::linear(&SRGB);
        let out = blend_over(
            [0.5, 0.5, 0.5, 0.5],
            [0.0, 0.0, 0.0],
            1.0,
            BlendMode::Multiply,
            &ctx,
        );
        assert!(close(out, [0.0, 0.0, 0.0, 1.0], 1e-6), "{out:?}");

        // The same with a white source: B = Cb = 1, Cs' = 0.5*1 + 0.5*1 = 1.
        let out = blend_over(
            [0.5, 0.5, 0.5, 0.5],
            [1.0, 1.0, 1.0],
            1.0,
            BlendMode::Multiply,
            &ctx,
        );
        assert!(close(out, [1.0, 1.0, 1.0, 1.0], 1e-6), "{out:?}");
    }

    #[test]
    fn alpha_composites_as_porter_duff_over() {
        let ctx = BlendContext::linear(&SRGB);
        let out = blend_over(
            [0.0, 0.0, 0.0, 0.5],
            [1.0, 1.0, 1.0],
            0.5,
            BlendMode::Normal,
            &ctx,
        );
        // 0.5 + 0.5*0.5 = 0.75
        assert!((out[3] - 0.75).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn the_encoded_blend_space_differs_from_the_linear_one() {
        // The whole reason `BlendSpace` exists: Multiply on linear light is not
        // Multiply on sRGB-encoded values.
        let linear = BlendContext::linear(&SRGB);
        let encoded = BlendContext {
            space: &SRGB,
            blend_space: BlendSpace::Encoded,
        };
        let bd = [0.5, 0.5, 0.5, 1.0];
        let a = blend_over(bd, [0.5, 0.5, 0.5], 1.0, BlendMode::Multiply, &linear);
        let b = blend_over(bd, [0.5, 0.5, 0.5], 1.0, BlendMode::Multiply, &encoded);
        assert!((a[0] - 0.25).abs() < 1e-6, "linear multiply: {a:?}");
        assert!(
            (a[0] - b[0]).abs() > 1e-3,
            "encoded multiply must differ: {a:?} vs {b:?}"
        );
        // Encoded: srgb(0.5) ~= 0.7354; squared ~= 0.5408; back to linear.
        let want = color::srgb_to_linear(color::linear_to_srgb(0.5).powi(2));
        assert!((b[0] - want).abs() < 1e-5, "{b:?} want {want}");
    }

    #[test]
    fn normal_is_identical_in_both_blend_spaces() {
        // Normal returns the source untouched, so the encode/decode round trip
        // must not shift it — that would show up as a colour drift on every
        // ordinary layer the moment someone switched the option.
        let linear = BlendContext::linear(&SRGB);
        let encoded = BlendContext {
            space: &SRGB,
            blend_space: BlendSpace::Encoded,
        };
        let bd = [0.2, 0.4, 0.1, 0.7];
        let a = blend_over(bd, [0.3, 0.6, 0.9], 0.4, BlendMode::Normal, &linear);
        let b = blend_over(bd, [0.3, 0.6, 0.9], 0.4, BlendMode::Normal, &encoded);
        assert!(close(a, b, 1e-5), "{a:?} vs {b:?}");
    }

    #[test]
    fn atop_never_changes_the_backdrop_alpha() {
        let ctx = BlendContext::linear(&SRGB);
        for ab in [0.0f32, 0.25, 0.5, 1.0] {
            for sa in [0.0f32, 0.5, 1.0] {
                for mode in BlendMode::ALL {
                    let bd = [0.1 * ab, 0.2 * ab, 0.3 * ab, ab];
                    let out = blend_atop(bd, [0.9, 0.8, 0.7], sa, mode, &ctx);
                    assert_eq!(out[3], ab, "{mode:?} ab={ab} sa={sa}");
                }
            }
        }
    }

    #[test]
    fn atop_replaces_colour_inside_the_backdrop_shape() {
        // The bug this pins: with plain `over`, an opaque clipped layer on a
        // half-transparent base composites to alpha 0.75 and the clipping
        // group grows past its base.
        let ctx = BlendContext::linear(&SRGB);
        let base = [0.5, 0.0, 0.0, 0.5]; // straight red at alpha 0.5
        let out = blend_atop(base, [0.0, 1.0, 0.0], 1.0, BlendMode::Normal, &ctx);
        assert!(close(out, [0.0, 0.5, 0.0, 0.5], 1e-6), "{out:?}");

        let over = blend_over(base, [0.0, 1.0, 0.0], 1.0, BlendMode::Normal, &ctx);
        assert_eq!(over[3], 1.0, "premise: `over` would have grown the shape");
    }

    #[test]
    fn atop_a_transparent_backdrop_draws_nothing() {
        let ctx = BlendContext::linear(&SRGB);
        for mode in BlendMode::ALL {
            assert_eq!(
                blend_atop([0.0; 4], [1.0, 1.0, 1.0], 1.0, mode, &ctx),
                [0.0; 4],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn atop_at_half_source_alpha_is_a_lerp_within_the_shape() {
        let ctx = BlendContext::linear(&SRGB);
        // Opaque black backdrop, white source at 0.5.
        let out = blend_atop([0.0, 0.0, 0.0, 1.0], [1.0; 3], 0.5, BlendMode::Normal, &ctx);
        assert!(close(out, [0.5, 0.5, 0.5, 1.0], 1e-6), "{out:?}");
    }

    #[test]
    fn dissolve_noise_is_stable_uniform_and_coordinate_dependent() {
        assert_eq!(dissolve_noise(3, 4, 0, 0), dissolve_noise(3, 4, 0, 0));
        assert_ne!(dissolve_noise(3, 4, 0, 0), dissolve_noise(4, 3, 0, 0));
        assert_ne!(dissolve_noise(3, 4, 0, 0), dissolve_noise(3, 4, 1, 0));
        assert_ne!(dissolve_noise(3, 4, 0, 0), dissolve_noise(3, 4, 0, 1));

        let mut sum = 0.0f64;
        let mut n = 0u32;
        for y in -50..50i64 {
            for x in -50..50i64 {
                let v = dissolve_noise(x, y, 0, 7);
                assert!((0.0..1.0).contains(&v), "({x},{y}) -> {v}");
                sum += v as f64;
                n += 1;
            }
        }
        let mean = sum / n as f64;
        assert!((mean - 0.5).abs() < 0.02, "mean {mean} is not uniform");
    }
}
