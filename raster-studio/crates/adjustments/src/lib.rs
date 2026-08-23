//! Parametric, non-destructive adjustments.
//!
//! Every adjustment here is a pure function of a colour and a set of validated
//! parameters. Nothing is baked into pixels, so an adjustment layer stays
//! editable for as long as the document exists — which is the roadmap's
//! "adjustments remain editable after save/reload" gate. These CPU
//! implementations are the *ground truth*; the GPU shaders in `render` must
//! match them within tolerance.
//!
//! # Using it
//!
//! ```
//! use adjustments::{Adjustment, ExposureParams, LinearRgb, PreparedAdjustment};
//! use color::ColorSpace;
//!
//! // Resolve the layer's parameters once...
//! let adj = Adjustment::Exposure(ExposureParams::stops_only(1.0).unwrap());
//! let prepared = PreparedAdjustment::new(&adj);
//!
//! // ...then walk a tile of linear, premultiplied RGBA.
//! let mut tile = vec![[0.2f32, 0.2, 0.2, 1.0]; 64 * 64];
//! prepared.apply_premultiplied_rgba(&mut tile, &ColorSpace::Srgb);
//! assert!((tile[0][0] - 0.4).abs() < 1e-6);
//! ```
//!
//! # Three things this crate is careful about
//!
//! **Nothing clamps that does not have to.** The working space is unbounded
//! linear `f32`, and an adjustment that pushes a highlight to `4.0` leaves it
//! there for a later adjustment to pull back. The previous `exposure` ended in
//! `.clamp(0.0, 1.0)`, so raising exposure and lowering it again returned a
//! flat gray patch where the highlights had been — the exact opposite of
//! non-destructive. Clamping happens at display and export.
//!
//! Where an adjustment does clamp, it is because the operation's *definition*
//! is bounded, and it is stated in that type's own documentation. The complete
//! list:
//!
//! * [`Posterize`] and [`Threshold`] — a quantiser and a binariser have a fixed
//!   output alphabet.
//! * [`GradientMap`] — the pixel only picks a *position* on a ramp; the colour
//!   comes from the ramp, which has two ends.
//! * [`Vibrance`], [`HueSaturation`], and [`BlackAndWhite`]'s optional tint —
//!   all three go through `color`'s HSL entry points, which are deliberately
//!   display-referred because "the hue of a 300% red" has no non-arbitrary
//!   answer. An *untinted* [`BlackAndWhite`] does not clamp.
//! * [`ColorBalance`] and [`SelectiveColor`] clamp only the value they
//!   *measure* — the band weights, and the range weights and ink separation
//!   respectively — never the value they return. Two highlights that differ
//!   going in still differ coming out, which
//!   `highlights_stay_distinct_through_color_balance`,
//!   `..._through_selective_color` and `..._through_black_and_white` pin
//!   against [`ChannelMixer`] as an unclamped control.
//!
//! Everything else — brightness/contrast, levels, curves, exposure, photo
//! filter, channel mixer, invert, the auto commands — passes scene-referred
//! values straight through. Curves is the one that has to work at it: outside
//! its control points' x range it continues along the end knot's tangent
//! rather than holding the endpoint y the way a Curves dialog would, because
//! holding would map every highlight above the last knot onto a single value
//! (`highlights_stay_distinct_through_curves`,
//! `highlights_stay_distinct_past_the_last_knot`).
//!
//! **Working space is a type, not a comment.** [`LinearRgb`] and
//! [`EncodedRgb`] are distinct, and the only ways across take the document's
//! [`ColorSpace`](color::ColorSpace). Every adjustment declares its side
//! through [`Adjustment::working_space`], and the declaration is not taken on
//! trust: internally a linear-space operation is handed no colour space at all,
//! so it *cannot* depend on one, and two tests
//! (`declared_working_space_matches_the_prepared_shape` and
//! `linear_space_adjustments_ignore_the_document_space`) check the declaration
//! against what the code does in both directions.
//!
//! **Bad parameters are reported, not absorbed.** Every constructor validates
//! and returns [`AdjustmentError`]. The three specific failures that used to be
//! silent are all covered by named tests: a `gamma <= 0` became a `1.0 / 1e-5`
//! exponent and flattened the image to black; a levels `white <= black` became
//! a 100000x gain step; and two curve control points at the same x divided by
//! `1e-5` and returned values far outside the output range.
//!
//! # Curves
//!
//! [`Curve`] is a monotone cubic (Fritsch–Carlson) interpolant, not the
//! piecewise-linear one it replaces. A tone curve made of straight segments has
//! a slope discontinuity at every control point, and a slope discontinuity in a
//! tone mapping shows up as a contour band across any smooth gradient. The
//! monotone family is the right one because an ordinary cubic spline overshoots
//! between control points, and an overshoot in a tone curve is a highlight that
//! gets *darker* as you raise it.
//!
//! Outside the control points the curve continues **linearly** along the end
//! knot's limited tangent. See [`Curve`]'s own documentation for why holding
//! the endpoint — what a Curves dialog does — is wrong for scene-referred
//! input.
//!
//! # Relationship to `layer_model::AdjustmentKind`
//!
//! [`Adjustment`] and its parameter types are deliberately **not**
//! `serde`-serializable: the persisted form is
//! [`layer_model::AdjustmentKind`], plain data with nothing for the project
//! format to interpret. Every one of the sixteen adjustments has a variant
//! there, so every one of them can be a saved adjustment *layer* —
//! `every_adjustment_round_trips_through_the_stored_vocabulary` walks all
//! sixteen out to JSON and back.
//!
//! [`Adjustment::to_layer_kind`] is **total by construction** — it returns an
//! `AdjustmentKind`, not an `Option<AdjustmentKind>` — so every adjustment this
//! crate can build has a stored form, including every *setting* of it. Five
//! adjustments have two stored spellings, because five of the stored shapes
//! predate this crate and are narrower than the parameters offered here. The
//! narrow shape is written whenever the settings fit in it, so an ordinary
//! document is byte-for-byte what it always was; a wider `*Full` variant
//! carries the rest:
//!
//! | setting | stored as |
//! |---|---|
//! | per-channel Levels, or a Levels output range | `LevelsFull` |
//! | per-channel Curves | `CurvesFull` |
//! | an exposure offset or gamma | `ExposureFull` |
//! | a colorizing Hue/Saturation | `HueSaturationFull` |
//! | a luminosity-preserving Colour Balance | `ColorBalanceFull` |
//!
//! `the_widened_settings_round_trip_through_the_full_spellings` walks each of
//! those out to JSON and back, and `to_layer_kind_prefers_the_narrow_spelling`
//! checks that nothing which fits the old shape is quietly migrated out of it.
//!
//! Conversions run both ways: lenient (`From<&AdjustmentKind>`, so a document
//! with an out-of-range slider still opens), strict
//! ([`Adjustment::try_from_layer_kind`], which reports what is wrong), and back
//! ([`Adjustment::to_layer_kind`]).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod auto;
pub mod color_ops;
pub mod curve;
pub mod error;
pub mod prepared;
pub mod space;
pub mod tone;

pub use auto::{AutoKind, AutoMode, Histogram, ImageStats, DEFAULT_CLIP, HISTOGRAM_BINS};
pub use color_ops::{
    BlackAndWhite, BwTint, ChannelMixer, ColorBalance, ColorRange, Colorize, GradientMap,
    GradientStop, HueSaturation, PhotoFilter, SelectiveColor, Vibrance, BW_DEFAULT_WEIGHTS,
};
pub use curve::Curve;
pub use error::AdjustmentError;
pub use prepared::{apply, apply_adjustment, Adjustment, PreparedAdjustment};
pub use space::{EncodedRgb, LinearRgb, WorkingSpace};
pub use tone::{
    invert, BrightnessContrast, Curves, ExposureParams, Levels, LevelsChannel, Posterize,
    Threshold, MAX_GAMMA, MIN_GAMMA, MIN_LEVELS_SPAN,
};

#[cfg(test)]
mod tests {
    use super::*;
    use color::ColorSpace;

    const SRGB: ColorSpace = ColorSpace::Srgb;

    /// A whole edit session's worth of adjustments, applied in order and then
    /// undone in reverse. Every step that is mathematically invertible must
    /// return the image it was given — that is what "non-destructive" means in
    /// practice, and it is what the old clamps made impossible.
    #[test]
    fn an_invertible_stack_round_trips_including_the_highlights() {
        let up = PreparedAdjustment::new(&Adjustment::Exposure(
            ExposureParams::stops_only(3.0).unwrap(),
        ));
        let down = PreparedAdjustment::new(&Adjustment::Exposure(
            ExposureParams::stops_only(-3.0).unwrap(),
        ));
        let inv = PreparedAdjustment::new(&Adjustment::Invert);

        for px in [
            LinearRgb([0.0, 0.0, 0.0]),
            LinearRgb([0.05, 0.4, 0.9]),
            LinearRgb([1.0, 1.0, 1.0]),
            LinearRgb([2.75, 0.1, 0.6]),
        ] {
            let lifted = up.apply(px, &SRGB);
            let back = down.apply(lifted, &SRGB);
            assert_eq!(back, px, "exposure round trip lost {px:?}");
            // Invert is an involution through the encode/decode pair too.
            let twice = inv.apply(inv.apply(px, &SRGB), &SRGB).get();
            for i in 0..3 {
                assert!(
                    (twice[i] - px.get()[i]).abs() < 1e-5,
                    "invert round trip: {twice:?} vs {px:?}"
                );
            }
        }
    }

    /// A tiled render and an untiled one must agree exactly, or every tile
    /// boundary in the document becomes a visible seam.
    #[test]
    fn splitting_a_buffer_into_tiles_changes_nothing() {
        let adjustments = vec![
            Adjustment::Levels(Levels::composite(
                LevelsChannel::new(0.02, 0.94, 1.15).unwrap(),
            )),
            Adjustment::Curves(Curves::composite(
                Curve::new(&[[0.0, 0.05], [0.4, 0.3], [1.0, 0.95]]).unwrap(),
            )),
            Adjustment::PhotoFilter(PhotoFilter::new(PhotoFilter::COOLING_80, 0.35).unwrap()),
            Adjustment::GradientMap(GradientMap::black_to_white()),
        ];
        let source: Vec<[f32; 4]> = (0..64)
            .map(|i| {
                let t = i as f32 / 63.0;
                color::premultiply([t, 1.0 - t, t * t, 0.25 + 0.75 * t])
            })
            .collect();

        for adj in adjustments {
            let prep = PreparedAdjustment::new(&adj);
            let mut whole = source.clone();
            prep.apply_premultiplied_rgba(&mut whole, &SRGB);

            let mut tiled = source.clone();
            for chunk in tiled.chunks_mut(7) {
                prep.apply_premultiplied_rgba(chunk, &SRGB);
            }
            assert_eq!(whole, tiled, "{adj:?} disagreed across a tile boundary");
        }
    }

    /// Every adjustment must leave alpha exactly as it found it. An adjustment
    /// layer re-colours the backdrop; it does not reshape it.
    #[test]
    fn no_adjustment_touches_alpha() {
        let alphas = [1.0f32, 0.75, 0.5, 0.01];
        let prepared = vec![
            PreparedAdjustment::new(&Adjustment::Invert),
            PreparedAdjustment::new(&Adjustment::Threshold(Threshold::new(0.5).unwrap())),
            PreparedAdjustment::new(&Adjustment::BlackAndWhite(BlackAndWhite::DEFAULT)),
            PreparedAdjustment::new(&Adjustment::Exposure(
                ExposureParams::stops_only(2.0).unwrap(),
            )),
            PreparedAdjustment::new(&Adjustment::GradientMap(GradientMap::black_to_white())),
        ];
        for prep in prepared {
            let mut px: Vec<[f32; 4]> = alphas
                .iter()
                .map(|a| color::premultiply([0.3, 0.6, 0.2, *a]))
                .collect();
            prep.apply_premultiplied_rgba(&mut px, &SRGB);
            for (i, a) in alphas.iter().enumerate() {
                assert_eq!(px[i][3], *a, "alpha {a} changed");
            }
        }
    }

    /// A finite pixel must never come out as `NaN`, whatever the adjustment.
    /// `signed_powf`'s mirroring is what makes this true for the gamma-bearing
    /// ones; a plain `powf` on a below-black value would poison the pixel.
    #[test]
    fn finite_pixels_never_become_nan() {
        let stats = ImageStats::from_encoded(&[
            EncodedRgb([0.05, 0.1, 0.15]),
            EncodedRgb([0.85, 0.9, 0.95]),
        ]);
        let adjustments = vec![
            Adjustment::Levels(Levels::composite(
                LevelsChannel::new(0.3, 0.7, 2.2).unwrap(),
            )),
            Adjustment::Exposure(ExposureParams::new(-2.0, -1.0, 2.2).unwrap()),
            Adjustment::Curves(Curves::composite(
                Curve::new(&[[0.1, 0.9], [0.9, 0.1]]).unwrap(),
            )),
            Adjustment::Vibrance(Vibrance::new(1.0, 1.0).unwrap()),
            Adjustment::HueSaturation(HueSaturation::new(180.0, -1.0, 1.0).unwrap()),
            Adjustment::ColorBalance(
                ColorBalance::new([1.0; 3], [-1.0; 3], [1.0; 3])
                    .unwrap()
                    .with_preserve_luminosity(true),
            ),
            Adjustment::BlackAndWhite(
                BlackAndWhite::new([-3.0, 3.0, -3.0, 3.0, -3.0, 3.0]).unwrap(),
            ),
            Adjustment::PhotoFilter(PhotoFilter::new([0.0, 0.0, 0.0], 1.0).unwrap()),
            Adjustment::SelectiveColor(SelectiveColor::new([[1.0, -1.0, 1.0, -1.0]; 9]).unwrap()),
            Adjustment::GradientMap(GradientMap::black_to_white()),
            Adjustment::Auto(AutoMode::COLOR),
        ];
        let pixels = [
            [0.0f32, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [-0.5, 0.5, 2.0],
            [1e6, -1e6, 0.0],
            [1e-30, 0.5, 100.0],
        ];
        for adj in adjustments {
            let prep = PreparedAdjustment::with_stats(&adj, &stats);
            for px in pixels {
                let out = prep.apply(LinearRgb(px), &SRGB).get();
                assert!(
                    out.iter().all(|c| !c.is_nan()),
                    "{adj:?} turned {px:?} into {out:?}"
                );
            }
        }
    }

    /// The full set is reachable through the public API and every member of it
    /// actually changes a pixel it should change. A "complete adjustment set"
    /// that cannot be enumerated is a claim, not a feature.
    #[test]
    fn every_named_adjustment_is_constructible_and_does_something() {
        let stats = ImageStats::from_encoded(&[
            EncodedRgb([0.2, 0.3, 0.25]),
            EncodedRgb([0.7, 0.6, 0.8]),
            EncodedRgb([0.45, 0.5, 0.5]),
        ]);
        let named: Vec<(&str, Adjustment)> = vec![
            (
                "Brightness/Contrast",
                Adjustment::BrightnessContrast(BrightnessContrast::new(0.1, 0.3).unwrap()),
            ),
            (
                "Levels (composite)",
                Adjustment::Levels(Levels::composite(
                    LevelsChannel::new(0.1, 0.9, 1.2).unwrap(),
                )),
            ),
            (
                "Levels (per channel)",
                Adjustment::Levels(Levels::per_channel(
                    LevelsChannel::new(0.1, 0.9, 1.0).unwrap(),
                    LevelsChannel::IDENTITY,
                    LevelsChannel::IDENTITY,
                )),
            ),
            (
                "Curves",
                Adjustment::Curves(Curves::composite(
                    Curve::new(&[[0.0, 0.0], [0.5, 0.65], [1.0, 1.0]]).unwrap(),
                )),
            ),
            (
                "Exposure",
                Adjustment::Exposure(ExposureParams::new(0.5, 0.02, 1.1).unwrap()),
            ),
            (
                "Vibrance",
                Adjustment::Vibrance(Vibrance::new(0.5, 0.0).unwrap()),
            ),
            (
                "Hue/Saturation",
                Adjustment::HueSaturation(HueSaturation::new(40.0, 0.2, 0.1).unwrap()),
            ),
            (
                "Color Balance",
                Adjustment::ColorBalance(
                    ColorBalance::new([0.2, 0.0, 0.0], [0.0, 0.1, 0.0], [0.0, 0.0, 0.2]).unwrap(),
                ),
            ),
            (
                "Black & White",
                Adjustment::BlackAndWhite(BlackAndWhite::DEFAULT),
            ),
            (
                "Photo Filter",
                Adjustment::PhotoFilter(PhotoFilter::new(PhotoFilter::WARMING_81, 0.5).unwrap()),
            ),
            (
                "Channel Mixer",
                Adjustment::ChannelMixer(
                    ChannelMixer::new([
                        [0.8, 0.2, 0.0, 0.0],
                        [0.0, 1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0, 0.0],
                    ])
                    .unwrap(),
                ),
            ),
            ("Invert", Adjustment::Invert),
            (
                "Posterize",
                Adjustment::Posterize(Posterize::new(4).unwrap()),
            ),
            (
                "Threshold",
                Adjustment::Threshold(Threshold::new(0.5).unwrap()),
            ),
            (
                "Gradient Map",
                Adjustment::GradientMap(GradientMap::black_to_white().reversed(true)),
            ),
            (
                "Selective Color",
                Adjustment::SelectiveColor(
                    SelectiveColor::IDENTITY
                        .with_range(ColorRange::Neutrals, [0.2, 0.0, 0.0, 0.1])
                        .unwrap()
                        .relative(false),
                ),
            ),
            ("Auto Tone", Adjustment::Auto(AutoMode::TONE)),
            ("Auto Contrast", Adjustment::Auto(AutoMode::CONTRAST)),
            ("Auto Color", Adjustment::Auto(AutoMode::COLOR)),
        ];
        assert_eq!(named.len(), 19, "the enumerated set changed size");

        let px = LinearRgb([0.18, 0.22, 0.14]);
        for (name, adj) in named {
            let prep = PreparedAdjustment::with_stats(&adj, &stats);
            assert!(!prep.is_identity(), "{name} prepared as an identity");
            assert_ne!(prep.apply(px, &SRGB), px, "{name} did not change the pixel");
        }
    }
}
