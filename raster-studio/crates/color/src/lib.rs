//! Color management for Raster Studio.
//!
//! The pipeline shape is fixed so no sRGB assumption is baked into layer or
//! shader APIs:
//!
//! ```text
//! source decode
//!   → source space to linear working RGB   (to_linear)
//!   → linear-premultiplied compositing
//!   → working RGB to display space         (from_linear)
//!   → presentation
//! ```
//!
//! Every source and document carries a [`ColorSpace`], and both ends of the
//! pipeline go through [`to_linear`] / [`from_linear`] rather than calling a
//! transfer function directly, so adding a space is a change here and nowhere
//! else.
//!
//! Two invariants the rest of the codebase depends on:
//!
//! * **The working space is unclamped `f32` linear sRGB.** Nothing here panics.
//!   The transfer functions, the space dispatch, CIELAB, luminance and the
//!   alpha helpers are scene-referred: they mirror negatives rather than
//!   clipping them and pass highlights above `1.0` straight through. The
//!   HSL/HSV entry points are the exception — they are display-referred
//!   reparameterisations of encoded RGB, are defined only on `[0, 1]`, and
//!   clamp their input into that range (see [`model`]), so they must not be fed
//!   raw working-space pixels. `clamping_claim_matches_the_code` machine-checks
//!   that split in both directions. `NaN` comes out only where `NaN` went in,
//!   apart from a closed set of numeric-overflow exceptions listed below.
//!
//!   Two independent overflows create them, and both need a channel magnitude
//!   far beyond any pixel value. An *encoded* channel above about `1.19e16`
//!   overflows the sRGB curve to infinity (see [`transfer`]). A *linear*
//!   channel large enough to overflow a coefficient — above about `1.05e38`
//!   (`f32::MAX / 3.241`, the largest [`XYZ_D65_TO_LINEAR_SRGB`] coefficient),
//!   or an XYZ component past about `±4.4e37`, where the CIELAB `f` function's
//!   near-black branch divides by `3(6/29)² = 0.1284` — does the same. Once one
//!   term is infinite, a later sum of two same-signed infinities with opposite
//!   coefficient signs, or of oppositely-signed infinities, evaluates
//!   `inf - inf`. The entry points that can therefore return `NaN` from a
//!   finite input are exactly these five:
//!
//!   | entry point | overflowing step |
//!   |---|---|
//!   | [`to_linear`] for [`ColorSpace::DisplayP3`] (and [`try_to_linear`], which delegates to it) | sRGB curve, then the mixed-sign P3 matrix row |
//!   | [`rgb_to_lab`] | sRGB curve, then the XYZ matrix and CIELAB `f` |
//!   | [`srgb_luminance`] | sRGB curve, then the [`REC709_LUMA`] dot product |
//!   | [`linear_srgb_to_lab`] | CIELAB `f`'s near-black branch |
//!   | [`xyz_to_linear_srgb`] | its own `3.241` coefficient |
//!
//!   Every other entry point is `NaN`-free for every finite input: all of
//!   [`from_linear`] and [`try_from_linear`], [`to_linear`] for the other
//!   spaces, [`linear_srgb_to_xyz`], the transfer functions and the 8-bit LUT,
//!   HSL/HSV in both directions, [`linear_srgb_luminance`], the `lab_to_*`
//!   direction (capped, see `model`), and [`premultiply`] / [`unpremultiply`].
//!   [`mat3_mul_vec3`] is excluded from the claim in both directions: it takes a
//!   caller-supplied matrix and inherits whatever that matrix does.
//!
//!   `nan_sources_are_exactly_the_documented_set` machine-checks this: it sweeps
//!   every entry point above over a wide range of finite magnitudes and fails if
//!   a listed one stops overflowing or an unlisted one starts, so neither half
//!   of the claim can drift into prose.
//! * **Unsupported means unsupported.** [`ColorSpace::IccProfile`] carries only
//!   an asset hash, not profile bytes, so inside `ColorSpace` it still takes a
//!   documented identity path and [`try_to_linear`] reports it as an error.
//!   The real matrix-shaper engine lives in [`crate::icc`]: a zero-I/O
//!   [`crate::icc::MatrixShaper::parse`] that reads `rXYZ/gXYZ/bXYZ` and the
//!   `rTRC/gTRC/bTRC` tone curves and transforms encoded RGB to/from linear
//!   sRGB with Bradford D50–D65 adaptation. Threading the asset-store bytes
//!   into [`ColorSpace::IccProfile`] and the compositor/export path is the
//!   remaining step; until then no transform tries to approximate a profile.
//!
//! ```
//! use color::{ColorSpace, to_linear, from_linear};
//!
//! // Display P3 white is D65 white, same as sRGB white.
//! let linear = to_linear(&ColorSpace::DisplayP3, [1.0, 1.0, 1.0]);
//! assert!((linear[0] - 1.0).abs() < 1e-5);
//!
//! let back = from_linear(&ColorSpace::DisplayP3, linear);
//! assert!((back[0] - 1.0).abs() < 1e-5);
//! ```

#![forbid(unsafe_code)]

pub mod alpha;
pub mod icc;
pub mod model;
pub mod space;
pub mod transfer;

pub use alpha::{premultiply, unpremultiply, UNPREMULTIPLY_ALPHA_EPSILON};
pub use icc::{Curve as IccCurve, IccError, MatrixShaper as IccMatrixShaper};
pub use model::{
    hsl_to_rgb, hsv_to_rgb, lab_to_linear_srgb, lab_to_rgb, linear_srgb_luminance,
    linear_srgb_to_lab, rgb_to_hsl, rgb_to_hsv, rgb_to_lab, srgb_luminance, REC709_LUMA,
};
pub use space::{
    from_linear, linear_srgb_to_xyz, mat3_mul_vec3, to_linear, try_from_linear, try_to_linear,
    xyz_to_linear_srgb, ColorSpace, Mat3, UnsupportedColorSpace, D65_WHITE_XYZ,
    DISPLAY_P3_TO_LINEAR_SRGB, LINEAR_SRGB_TO_DISPLAY_P3, LINEAR_SRGB_TO_XYZ_D65,
    XYZ_D65_TO_LINEAR_SRGB,
};
pub use transfer::{
    linear_to_srgb, linear_to_srgb3, srgb8_to_linear, srgb_to_linear, srgb_to_linear3,
    SRGB8_TO_LINEAR,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the crate: a source pixel decoded from one space and
    /// presented in another survives the working space unchanged in appearance.
    #[test]
    fn end_to_end_srgb_source_to_p3_display() {
        let source = [0.2f32, 0.4, 0.6];
        let working = to_linear(&ColorSpace::Srgb, source);
        let displayed = from_linear(&ColorSpace::DisplayP3, working);
        // Re-decoding the P3 encoding must return the same working colour.
        let round = to_linear(&ColorSpace::DisplayP3, displayed);
        for i in 0..3 {
            assert!(
                (round[i] - working[i]).abs() < 1e-5,
                "channel {i}: {} vs {}",
                round[i],
                working[i]
            );
        }
        // P3 encodes the same colour with less saturation headroom used.
        assert!(displayed != source);
    }

    /// The 8-bit decode path used by image loaders lands in the same working
    /// space as the float path.
    #[test]
    fn eight_bit_decode_agrees_with_the_dispatch_path() {
        for v in [0u8, 1, 63, 128, 200, 255] {
            let via_lut = srgb8_to_linear(v);
            let via_dispatch = to_linear(&ColorSpace::Srgb, [v as f32 / 255.0; 3])[0];
            assert!(
                (via_lut - via_dispatch).abs() < 1e-6,
                "code {v}: LUT {via_lut} vs dispatch {via_dispatch}"
            );
        }
    }

    /// Compositing operates on premultiplied linear values; check the two
    /// halves of the crate compose without a clamp sneaking in.
    #[test]
    fn decode_premultiply_unpremultiply_encode_is_lossless() {
        let source = [0.8f32, 0.35, 0.1];
        let alpha = 0.25;
        let lin = to_linear(&ColorSpace::Srgb, source);
        let pm = premultiply([lin[0], lin[1], lin[2], alpha]);
        let straight = unpremultiply(pm);
        let out = from_linear(&ColorSpace::Srgb, [straight[0], straight[1], straight[2]]);
        for i in 0..3 {
            assert!((out[i] - source[i]).abs() < 1e-5, "channel {i}: {out:?}");
        }
        assert_eq!(straight[3], alpha);
    }

    /// One public entry point, reduced to "triple in, triple out" so the whole
    /// surface can be swept uniformly.
    struct EntryPoint {
        /// Name as it appears in the crate-level exception table.
        name: &'static str,
        eval: fn([f32; 3]) -> [f32; 3],
        /// Whether the crate docs list this entry point as able to return `NaN`
        /// from a finite input.
        documented_nan: bool,
    }

    /// Every entry point the crate-level `NaN` claim covers, with the claim's
    /// verdict for each. `mat3_mul_vec3` is deliberately absent: it takes a
    /// caller-supplied matrix, so the crate makes no claim about it.
    fn entry_points() -> Vec<EntryPoint> {
        fn scalar(y: f32) -> [f32; 3] {
            [y, y, y]
        }
        fn rgba(v: [f32; 3]) -> [f32; 4] {
            [v[0], v[1], v[2], v[2]]
        }
        fn drop_alpha(v: [f32; 4]) -> [f32; 3] {
            [v[0], v[1], v[2]]
        }
        fn icc() -> ColorSpace {
            ColorSpace::IccProfile {
                asset_hash: "probe".to_string(),
            }
        }
        vec![
            EntryPoint {
                name: "to_linear(sRGB)",
                eval: |v| to_linear(&ColorSpace::Srgb, v),
                documented_nan: false,
            },
            EntryPoint {
                name: "to_linear(Linear sRGB)",
                eval: |v| to_linear(&ColorSpace::LinearSrgb, v),
                documented_nan: false,
            },
            EntryPoint {
                name: "to_linear(Display P3)",
                eval: |v| to_linear(&ColorSpace::DisplayP3, v),
                documented_nan: true,
            },
            EntryPoint {
                name: "to_linear(ICC identity)",
                eval: |v| to_linear(&icc(), v),
                documented_nan: false,
            },
            EntryPoint {
                name: "try_to_linear(Display P3)",
                eval: |v| try_to_linear(&ColorSpace::DisplayP3, v).unwrap(),
                documented_nan: true,
            },
            EntryPoint {
                name: "from_linear(sRGB)",
                eval: |v| from_linear(&ColorSpace::Srgb, v),
                documented_nan: false,
            },
            EntryPoint {
                name: "from_linear(Linear sRGB)",
                eval: |v| from_linear(&ColorSpace::LinearSrgb, v),
                documented_nan: false,
            },
            EntryPoint {
                name: "from_linear(Display P3)",
                eval: |v| from_linear(&ColorSpace::DisplayP3, v),
                documented_nan: false,
            },
            EntryPoint {
                name: "try_from_linear(Display P3)",
                eval: |v| try_from_linear(&ColorSpace::DisplayP3, v).unwrap(),
                documented_nan: false,
            },
            EntryPoint {
                name: "srgb_to_linear3",
                eval: srgb_to_linear3,
                documented_nan: false,
            },
            EntryPoint {
                name: "linear_to_srgb3",
                eval: linear_to_srgb3,
                documented_nan: false,
            },
            EntryPoint {
                name: "linear_srgb_to_xyz",
                eval: linear_srgb_to_xyz,
                documented_nan: false,
            },
            EntryPoint {
                name: "xyz_to_linear_srgb",
                eval: xyz_to_linear_srgb,
                documented_nan: true,
            },
            EntryPoint {
                name: "rgb_to_hsl",
                eval: rgb_to_hsl,
                documented_nan: false,
            },
            EntryPoint {
                name: "hsl_to_rgb",
                eval: hsl_to_rgb,
                documented_nan: false,
            },
            EntryPoint {
                name: "rgb_to_hsv",
                eval: rgb_to_hsv,
                documented_nan: false,
            },
            EntryPoint {
                name: "hsv_to_rgb",
                eval: hsv_to_rgb,
                documented_nan: false,
            },
            EntryPoint {
                name: "rgb_to_lab",
                eval: rgb_to_lab,
                documented_nan: true,
            },
            EntryPoint {
                name: "linear_srgb_to_lab",
                eval: linear_srgb_to_lab,
                documented_nan: true,
            },
            EntryPoint {
                name: "lab_to_rgb",
                eval: lab_to_rgb,
                documented_nan: false,
            },
            EntryPoint {
                name: "lab_to_linear_srgb",
                eval: lab_to_linear_srgb,
                documented_nan: false,
            },
            EntryPoint {
                name: "srgb_luminance",
                eval: |v| scalar(srgb_luminance(v)),
                documented_nan: true,
            },
            EntryPoint {
                name: "linear_srgb_luminance",
                eval: |v| scalar(linear_srgb_luminance(v)),
                documented_nan: false,
            },
            EntryPoint {
                name: "premultiply",
                eval: |v| drop_alpha(premultiply(rgba(v))),
                documented_nan: false,
            },
            EntryPoint {
                name: "unpremultiply",
                eval: |v| drop_alpha(unpremultiply(rgba(v))),
                documented_nan: false,
            },
        ]
    }

    /// Finite magnitudes spanning pixel scale to `f32::MAX`, straddling both
    /// documented overflow points (the `~1.19e16` encoded-domain sRGB curve
    /// overflow and the `~1e38` linear-domain coefficient overflow).
    fn sweep_values() -> [f32; 30] {
        [
            0.0,
            -0.0,
            1e-30,
            -1e-30,
            0.5,
            -0.5,
            1.0,
            -1.0,
            3.0,
            -3.0,
            100.0,
            -100.0,
            1e6,
            -1e6,
            1e10,
            -1e10,
            1e16,
            -1e16,
            1.3e16,
            -1.3e16,
            1e17,
            -1e17,
            1e30,
            -1e30,
            4e37,
            -4e37,
            1.1e38,
            -1.1e38,
            f32::MAX,
            f32::MIN,
        ]
    }

    /// Machine-checks the crate-level `NaN` claim in **both** directions, which
    /// is the only way the exception table stays honest: prose enumerating the
    /// exceptions is exactly the kind of statement that rots when a function is
    /// added or a coefficient changes.
    ///
    /// For every entry point the claim covers, the sweep asserts:
    ///
    /// * marked `documented_nan: false` — no finite input in the sweep produces
    ///   `NaN` (the exhaustiveness half: an unlisted overflow fails here);
    /// * marked `documented_nan: true` — some finite input in the sweep does
    ///   (the no-stale-entries half: an exception that no longer applies, or was
    ///   never real, fails here).
    ///
    /// A three-value sweep over 30 magnitudes is 27,000 triples per entry point;
    /// it is what caught `srgb_luminance`, `linear_srgb_to_lab` and
    /// `xyz_to_linear_srgb`, none of which the hand-written table named.
    #[test]
    fn nan_sources_are_exactly_the_documented_set() {
        let values = sweep_values();
        for entry in entry_points() {
            let mut nan_witness: Option<[f32; 3]> = None;
            for &r in &values {
                for &g in &values {
                    for &b in &values {
                        let v = [r, g, b];
                        let out = (entry.eval)(v);
                        if out.iter().any(|c| c.is_nan()) {
                            assert!(
                                entry.documented_nan,
                                "{} is documented as NaN-free but {v:?} produced {out:?}; \
                                 add it to the crate-level exception table",
                                entry.name
                            );
                            nan_witness = Some(v);
                        }
                    }
                }
            }
            assert_eq!(
                nan_witness.is_some(),
                entry.documented_nan,
                "{} is listed as a NaN exception but never produced one; \
                 remove it from the crate-level exception table",
                entry.name
            );
        }
        // The 8-bit LUT is not part of the triple sweep (its domain is `u8`).
        for v in 0u8..=255 {
            assert!(!srgb8_to_linear(v).is_nan(), "LUT[{v}] is NaN");
        }
    }

    /// Pins each documented exception at a concrete input, so a reader can see
    /// where the boundary is and a silent shift of it fails loudly. The sweep
    /// above proves the *set* is right; this proves the *threshold* has not
    /// moved down into magnitudes an image could reach.
    #[test]
    fn documented_nan_boundaries_sit_where_the_docs_say() {
        // Encoded-domain overflow (~1.19e16): the sRGB curve saturates to
        // infinity, then a channel-combining step evaluates `inf - inf`.
        // P3 needs same-signed infinities because its matrix row mixes
        // coefficient signs (+1.2249, -0.2249); the all-positive XYZ and
        // Rec.709 rows instead need oppositely-signed channels.
        assert!(
            to_linear(&ColorSpace::DisplayP3, [1e17, 1e17, 0.0])[0].is_nan(),
            "the documented DisplayP3 overflow boundary moved"
        );
        assert!(
            rgb_to_lab([1e17, -1e17, 0.0])[0].is_nan(),
            "the documented rgb_to_lab overflow boundary moved"
        );
        assert!(
            srgb_luminance([1e17, -1e17, 0.0]).is_nan(),
            "the documented srgb_luminance overflow boundary moved"
        );
        // Linear-domain overflow (~1e38), reachable only within a small factor
        // of `f32::MAX`.
        assert!(
            xyz_to_linear_srgb([f32::MAX, f32::MAX, 0.0])[0].is_nan(),
            "the documented xyz_to_linear_srgb overflow boundary moved"
        );
        assert!(
            linear_srgb_to_lab([0.0, -1e38, -1e38])[1].is_nan(),
            "the documented linear_srgb_to_lab overflow boundary moved"
        );

        // Below the thresholds every one of them is clean. 1e10 is already ten
        // orders past any pixel value, and 1e16 sits just under the encoded
        // curve's overflow point; a threshold that crept downwards would show
        // up here rather than in a user's histogram.
        for v in [[1e10f32, -1e10, 5.0], [1e16, -1e16, 0.0], [-3.0, 9.5, 0.0]] {
            for c in to_linear(&ColorSpace::DisplayP3, v) {
                assert!(!c.is_nan(), "to_linear(P3, {v:?}) produced NaN");
            }
            for c in rgb_to_lab(v) {
                assert!(!c.is_nan(), "rgb_to_lab({v:?}) produced NaN");
            }
            assert!(
                !srgb_luminance(v).is_nan(),
                "srgb_luminance({v:?}) produced NaN"
            );
            for c in xyz_to_linear_srgb(v) {
                assert!(!c.is_nan(), "xyz_to_linear_srgb({v:?}) produced NaN");
            }
            for c in linear_srgb_to_lab(v) {
                assert!(!c.is_nan(), "linear_srgb_to_lab({v:?}) produced NaN");
            }
        }
        // Lab is NaN-free for every finite Lab coordinate (the cube is capped).
        for c in lab_to_rgb([1e20, 1e20, -1e20]) {
            assert!(!c.is_nan(), "lab_to_rgb of absurd L* produced NaN");
        }
        // NaN in, NaN out is deliberate, not an accident of the above.
        assert!(to_linear(&ColorSpace::Srgb, [f32::NAN; 3])[0].is_nan());
    }

    /// A scene-referred entry point named as the crate doc names it, paired
    /// with a "triple in, triple out" reduction of it.
    type UnclampedEntryPoint = (&'static str, fn([f32; 3]) -> [f32; 3]);

    /// Entry points the crate-level doc lists as scene-referred. Each must pass
    /// its input through unclamped, so both a highlight above `1.0` and a
    /// negative have to change the output rather than collapsing onto whatever
    /// the clamped triple gives.
    fn unclamped_triple_entry_points() -> Vec<UnclampedEntryPoint> {
        vec![
            ("to_linear(sRGB)", |v| to_linear(&ColorSpace::Srgb, v)),
            ("to_linear(Linear sRGB)", |v| {
                to_linear(&ColorSpace::LinearSrgb, v)
            }),
            ("to_linear(Display P3)", |v| {
                to_linear(&ColorSpace::DisplayP3, v)
            }),
            ("to_linear(ICC identity)", |v| {
                to_linear(
                    &ColorSpace::IccProfile {
                        asset_hash: "probe".to_string(),
                    },
                    v,
                )
            }),
            ("from_linear(sRGB)", |v| from_linear(&ColorSpace::Srgb, v)),
            ("from_linear(Linear sRGB)", |v| {
                from_linear(&ColorSpace::LinearSrgb, v)
            }),
            ("from_linear(Display P3)", |v| {
                from_linear(&ColorSpace::DisplayP3, v)
            }),
            ("srgb_to_linear3", srgb_to_linear3),
            ("linear_to_srgb3", linear_to_srgb3),
            ("linear_srgb_to_xyz", linear_srgb_to_xyz),
            ("xyz_to_linear_srgb", xyz_to_linear_srgb),
            ("rgb_to_lab", rgb_to_lab),
            ("linear_srgb_to_lab", linear_srgb_to_lab),
        ]
    }

    /// Machine-checks the crate-level range claim in **both** directions.
    ///
    /// The doc used to state as a blanket invariant that "negatives are
    /// mirrored rather than clipped, and highlights above `1.0` pass through".
    /// Four entry points have never satisfied it: `rgb_to_hsl`, `rgb_to_hsv`,
    /// `hsl_to_rgb` and `hsv_to_rgb` push every non-hue channel through
    /// `clamp01`, so `rgb_to_hsl([2.0, 0.5, 0.25])` returns exactly what
    /// `rgb_to_hsl([1.0, 0.5, 0.25])` returns and the highlight is destroyed.
    /// `model`'s own per-function docs said so correctly; only the crate root
    /// was wrong, which is the worst place for it — a caller who reads just the
    /// root concludes HSL is HDR-safe and feeds it working-space pixels.
    ///
    /// The root now names the exception. This test is what keeps the two lists
    /// from rotting back into prose: adding a clamp to a scene-referred path
    /// fails the second half, and removing one from an HSL/HSV path fails the
    /// first.
    #[test]
    fn clamping_claim_matches_the_code() {
        // --- Display-referred: the input IS clamped into [0, 1]. ---
        // Analysis direction: all three channels are colour channels.
        assert_eq!(
            rgb_to_hsl([2.0, 0.5, 0.25]),
            rgb_to_hsl([1.0, 0.5, 0.25]),
            "rgb_to_hsl is documented as clamping its input"
        );
        assert_eq!(
            rgb_to_hsl([-1.0, 0.5, 0.25]),
            rgb_to_hsl([0.0, 0.5, 0.25]),
            "rgb_to_hsl is documented as clamping its input"
        );
        assert_eq!(
            rgb_to_hsv([2.0, 0.5, 0.25]),
            rgb_to_hsv([1.0, 0.5, 0.25]),
            "rgb_to_hsv is documented as clamping its input"
        );
        assert_eq!(
            rgb_to_hsv([-1.0, 0.5, 0.25]),
            rgb_to_hsv([0.0, 0.5, 0.25]),
            "rgb_to_hsv is documented as clamping its input"
        );
        // Synthesis direction: component 0 is hue, which wraps rather than
        // clamping, so only saturation and lightness/value are probed. The
        // colour chosen is chromatic and mid-lightness, so an unclamped
        // saturation really would change the answer.
        assert_eq!(
            hsl_to_rgb([30.0, 2.0, 0.5]),
            hsl_to_rgb([30.0, 1.0, 0.5]),
            "hsl_to_rgb is documented as clamping saturation"
        );
        assert_eq!(
            hsl_to_rgb([30.0, 0.5, -1.0]),
            hsl_to_rgb([30.0, 0.5, 0.0]),
            "hsl_to_rgb is documented as clamping lightness"
        );
        assert_eq!(
            hsv_to_rgb([30.0, 2.0, 0.5]),
            hsv_to_rgb([30.0, 1.0, 0.5]),
            "hsv_to_rgb is documented as clamping saturation"
        );
        assert_eq!(
            hsv_to_rgb([30.0, 0.5, 2.0]),
            hsv_to_rgb([30.0, 0.5, 1.0]),
            "hsv_to_rgb is documented as clamping value"
        );
        // And the clamp is doing real work at those inputs, not comparing two
        // degenerate blacks: the clamped answers are distinguishable colours.
        assert_ne!(hsl_to_rgb([30.0, 1.0, 0.5]), hsl_to_rgb([30.0, 0.5, 0.5]));
        assert_ne!(hsv_to_rgb([30.0, 1.0, 0.5]), hsv_to_rgb([30.0, 0.5, 0.5]));

        // --- Scene-referred: the input is NOT clamped. ---
        for (name, eval) in unclamped_triple_entry_points() {
            assert_ne!(
                eval([2.0, 0.25, 0.25]),
                eval([1.0, 0.25, 0.25]),
                "{name} clamped a highlight away; the crate doc promises it passes through"
            );
            assert_ne!(
                eval([-0.5, 0.25, 0.25]),
                eval([0.0, 0.25, 0.25]),
                "{name} clipped a negative to zero; the crate doc promises it is mirrored"
            );
        }
        // Alpha helpers, which take a quadruple rather than a triple.
        assert_eq!(
            premultiply([2.0, -0.5, 0.25, 0.5]),
            [1.0, -0.25, 0.125, 0.5]
        );
        assert_eq!(
            unpremultiply([1.0, -0.25, 0.125, 0.5]),
            [2.0, -0.5, 0.25, 0.5]
        );
        // Luminance is a weighted sum with no clamp at either end.
        assert!(srgb_luminance([2.0, 0.25, 0.25]) > srgb_luminance([1.0, 0.25, 0.25]));
        assert!(linear_srgb_luminance([-1.0, 0.0, 0.0]) < 0.0);
        // The `lab_to_*` direction is unclamped on its *output*: an
        // out-of-gamut Lab colour leaves `[0, 1]` instead of being clipped.
        let vivid = lab_to_rgb([50.0, 120.0, -80.0]);
        assert!(
            vivid.iter().any(|c| !(0.0..=1.0).contains(c)),
            "lab_to_rgb clipped an out-of-gamut colour into gamut: {vivid:?}"
        );
        let vivid = lab_to_linear_srgb([50.0, 120.0, -80.0]);
        assert!(
            vivid.iter().any(|c| !(0.0..=1.0).contains(c)),
            "lab_to_linear_srgb clipped an out-of-gamut colour into gamut: {vivid:?}"
        );
    }

    #[test]
    fn luminance_is_invariant_across_encodings_of_the_same_colour() {
        let working = [0.3f32, 0.5, 0.2];
        let via_srgb = srgb_luminance(from_linear(&ColorSpace::Srgb, working));
        let direct = linear_srgb_luminance(working);
        assert!((via_srgb - direct).abs() < 1e-5, "{via_srgb} vs {direct}");
    }
}
