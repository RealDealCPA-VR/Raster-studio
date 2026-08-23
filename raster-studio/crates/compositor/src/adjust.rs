//! Evaluation of [`AdjustmentKind`] — what an adjustment layer does to the
//! backdrop beneath it.
//!
//! # Which values each adjustment sees
//!
//! The compositor's working space is linear light, but not every adjustment is
//! defined there, and applying one in the wrong domain is the difference
//! between a familiar-looking Curves and an unusable one:
//!
//! | adjustment | domain | why |
//! |---|---|---|
//! | [`AdjustmentKind::Exposure`] | **linear** | a stop is a doubling of light; `2^stops` is only that in linear |
//! | [`AdjustmentKind::Levels`] | encoded | black/white/gamma sliders are positions on the encoded ramp |
//! | [`AdjustmentKind::Curves`] | encoded | the curve widget's axes are encoded values |
//! | [`AdjustmentKind::HueSaturation`] | encoded | HSL is a reparameterisation of *encoded* RGB — `color::rgb_to_hsl` says so explicitly and clamps to `[0, 1]` |
//! | [`AdjustmentKind::ColorBalance`] | encoded | its shadow/midtone/highlight bands are defined on the encoded ramp |
//!
//! Those five are the adjustments that shipped with the layer model, and this
//! module owns their evaluation. **Every other [`AdjustmentKind`] is delegated
//! to the [`adjustments`] crate**, which owns the parametric adjustment set and
//! declares each one's working space itself; there is deliberately no second
//! implementation of them here to drift out of step.
//!
//! Encoding happens through [`color::from_linear`] / [`color::to_linear`] with
//! the *document's* colour space, so a P3 document's adjustments act on P3
//! values rather than on sRGB ones.
//!
//! # Ranges
//!
//! [`AdjustmentKind`]'s fields are public, undocumented `f32`s, so this module
//! fixes their meaning and is total over every value including NaN:
//!
//! * `Levels { black, white }` — positions on the encoded `0.0..=1.0` ramp.
//!   `gamma` is Photoshop's midtone slider: output is `t^(1/gamma)`, so above
//!   1.0 brightens. A non-positive or non-finite gamma is treated as 1.0.
//! * `Curves { points }` — `[x, y]` pairs on the encoded ramp, in any order.
//!   Fewer than two usable points is the identity. Interpolation is piecewise
//!   linear between sorted points and clamped flat outside them.
//! * `Exposure { stops }` — photographic stops, clamped to `-32..=32`.
//! * `HueSaturation { hue, saturation, lightness }` — `hue` in **degrees**,
//!   the other two in `-1.0..=1.0` as relative moves: negative scales toward
//!   zero, positive interpolates toward the maximum.
//! * `ColorBalance { shadows, midtones, highlights }` — per-channel shifts in
//!   `-1.0..=1.0` applied with weights that sum to exactly 1 at every input
//!   value, so a uniform shift across all three bands is a plain offset.

use color::{from_linear, hsl_to_rgb, rgb_to_hsl, to_linear, ColorSpace};
use layer_model::blend::unit;
use layer_model::AdjustmentKind;

/// An [`AdjustmentKind`] with its per-layer setup done once instead of per
/// pixel.
///
/// The only kind that needs it is `Curves`, whose points arrive unsorted and
/// possibly duplicated; sorting them inside the pixel loop would be quadratic
/// nonsense on every tile.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedAdjustment {
    kind: Prepared,
}

#[derive(Debug, Clone, PartialEq)]
enum Prepared {
    Levels {
        black: f32,
        white: f32,
        inv_gamma: f32,
    },
    /// Sorted, de-duplicated, clamped control points. Empty means identity.
    Curves(Vec<[f32; 2]>),
    Exposure {
        gain: f32,
    },
    HueSaturation {
        hue: f32,
        sat: f32,
        light: f32,
    },
    ColorBalance {
        bands: [[f32; 3]; 3],
    },
    /// Everything else, evaluated by the `adjustments` crate.
    ///
    /// The five arms above predate that crate and are kept because their
    /// behaviour is pinned by this module's own tests; every kind added to
    /// [`AdjustmentKind`] afterwards — including the five `*Full` spellings —
    /// has exactly one implementation, there, and duplicating it here is how
    /// the two would drift apart. Boxed
    /// because a prepared `Curves` carries four splines and this enum is stored
    /// once per adjustment layer.
    Delegated(Box<adjustments::PreparedAdjustment>),
}

impl PreparedAdjustment {
    /// Resolve an adjustment's parameters once, before the pixel loop.
    pub fn new(kind: &AdjustmentKind) -> Self {
        let kind = match kind {
            AdjustmentKind::Levels {
                black,
                white,
                gamma,
            } => {
                let g = if gamma.is_finite() && *gamma > 0.0 {
                    *gamma
                } else {
                    1.0
                };
                Prepared::Levels {
                    black: unit(*black),
                    white: unit(*white),
                    inv_gamma: 1.0 / g,
                }
            }
            AdjustmentKind::Curves { points } => {
                let mut pts: Vec<[f32; 2]> = points
                    .iter()
                    .filter(|p| p[0].is_finite() && p[1].is_finite())
                    .map(|p| [unit(p[0]), unit(p[1])])
                    .collect();
                pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
                pts.dedup_by(|a, b| a[0] == b[0]);
                if pts.len() < 2 {
                    pts.clear();
                }
                Prepared::Curves(pts)
            }
            AdjustmentKind::Exposure { stops } => {
                let s = if stops.is_finite() {
                    stops.clamp(-32.0, 32.0)
                } else {
                    0.0
                };
                Prepared::Exposure {
                    gain: 2.0f32.powf(s),
                }
            }
            AdjustmentKind::HueSaturation {
                hue,
                saturation,
                lightness,
            } => Prepared::HueSaturation {
                hue: if hue.is_finite() { *hue } else { 0.0 },
                sat: signed_unit(*saturation),
                light: signed_unit(*lightness),
            },
            AdjustmentKind::ColorBalance {
                shadows,
                midtones,
                highlights,
            } => Prepared::ColorBalance {
                bands: [
                    shadows.map(signed_unit),
                    midtones.map(signed_unit),
                    highlights.map(signed_unit),
                ],
            },
            other => Prepared::Delegated(Box::new(adjustments::PreparedAdjustment::new(
                &adjustments::Adjustment::from(other),
            ))),
        };
        Self { kind }
    }

    /// `true` when this adjustment cannot change any pixel, so the compositor
    /// can skip the whole layer.
    pub fn is_identity(&self) -> bool {
        match &self.kind {
            Prepared::Levels {
                black,
                white,
                inv_gamma,
            } => *black == 0.0 && *white == 1.0 && *inv_gamma == 1.0,
            Prepared::Curves(p) => p.is_empty(),
            Prepared::Exposure { gain } => *gain == 1.0,
            Prepared::HueSaturation { hue, sat, light } => {
                *hue % 360.0 == 0.0 && *sat == 0.0 && *light == 0.0
            }
            Prepared::ColorBalance { bands } => bands.iter().all(|b| b.iter().all(|v| *v == 0.0)),
            Prepared::Delegated(p) => p.is_identity(),
        }
    }

    /// Apply to one **linear** straight RGB sample, returning linear RGB.
    ///
    /// Alpha is never passed in and never changes: an adjustment layer
    /// re-colours the backdrop, it does not reshape it.
    ///
    /// An [identity](PreparedAdjustment::is_identity) adjustment returns its
    /// input **bit for bit**. That is not just an optimisation: the encoded
    /// adjustments round-trip through [`from_linear`]/[`to_linear`], and that
    /// round trip is not exact, so a do-nothing Curves would otherwise shift
    /// every pixel of the document by an ulp or two.
    ///
    /// The result is *scene-referred*: `Exposure` can and does return values
    /// above 1.0, matching the `color` crate's unclamped working space.
    /// Clamping happens at the display end, in [`crate::Canvas::to_rgba8`].
    pub fn apply(&self, linear: [f32; 3], space: &ColorSpace) -> [f32; 3] {
        if self.is_identity() {
            return linear;
        }
        match &self.kind {
            // The one adjustment that is defined on light rather than on code
            // values.
            Prepared::Exposure { gain } => [linear[0] * gain, linear[1] * gain, linear[2] * gain],
            // The `adjustments` crate does its own encode/decode, because which
            // domain an adjustment is defined on is part of the adjustment.
            Prepared::Delegated(p) => p.apply(adjustments::LinearRgb(linear), space).get(),
            other => {
                let enc = from_linear(space, linear);
                let out = match other {
                    Prepared::Levels {
                        black,
                        white,
                        inv_gamma,
                    } => enc.map(|v| levels_channel(v, *black, *white, *inv_gamma)),
                    Prepared::Curves(points) => enc.map(|v| curve(points, v)),
                    Prepared::HueSaturation { hue, sat, light } => {
                        let hsl = rgb_to_hsl(enc);
                        hsl_to_rgb([
                            (hsl[0] + hue).rem_euclid(360.0),
                            toward(hsl[1], *sat),
                            toward(hsl[2], *light),
                        ])
                    }
                    Prepared::ColorBalance { bands } => {
                        let mut out = [0.0f32; 3];
                        for (c, o) in out.iter_mut().enumerate() {
                            let v = unit(enc[c]);
                            let (s, m, h) = band_weights(v);
                            *o = unit(v + s * bands[0][c] + m * bands[1][c] + h * bands[2][c]);
                        }
                        out
                    }
                    // Both handled above; unreachable without adding a variant,
                    // and a new variant should be an explicit arm rather than a
                    // silent identity.
                    Prepared::Exposure { .. } | Prepared::Delegated(_) => enc,
                };
                to_linear(space, out)
            }
        }
    }
}

/// Convenience wrapper: prepare and apply in one call. Use
/// [`PreparedAdjustment`] directly in a pixel loop.
pub fn apply_adjustment(kind: &AdjustmentKind, linear: [f32; 3], space: &ColorSpace) -> [f32; 3] {
    PreparedAdjustment::new(kind).apply(linear, space)
}

/// Map any `f32` into `-1.0..=1.0`; non-finite becomes `0.0` (no change).
fn signed_unit(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Move `v` (in `0..=1`) toward 1 for positive `amount` and toward 0 for
/// negative, reaching the endpoint exactly at ±1.
fn toward(v: f32, amount: f32) -> f32 {
    if amount >= 0.0 {
        v + (1.0 - v) * amount
    } else {
        v * (1.0 + amount)
    }
}

fn levels_channel(v: f32, black: f32, white: f32, inv_gamma: f32) -> f32 {
    let v = unit(v);
    let t = if white - black <= 1e-6 {
        // A collapsed (or inverted) range is a hard threshold rather than a
        // division by ~zero.
        if v >= white {
            1.0
        } else {
            0.0
        }
    } else {
        ((v - black) / (white - black)).clamp(0.0, 1.0)
    };
    if inv_gamma == 1.0 {
        t
    } else {
        unit(t.powf(inv_gamma))
    }
}

/// Piecewise-linear evaluation of a sorted, de-duplicated control-point list.
fn curve(points: &[[f32; 2]], v: f32) -> f32 {
    if points.len() < 2 {
        return unit(v);
    }
    let v = unit(v);
    if v <= points[0][0] {
        return points[0][1];
    }
    let last = points[points.len() - 1];
    if v >= last[0] {
        return last[1];
    }
    // `partition_point` gives the first index whose x exceeds v; both
    // neighbours therefore exist.
    let i = points.partition_point(|p| p[0] <= v);
    let (a, b) = (points[i - 1], points[i]);
    let span = b[0] - a[0];
    if span <= 0.0 {
        return b[1];
    }
    let t = (v - a[0]) / span;
    unit(a[1] + (b[1] - a[1]) * t)
}

/// Shadow / midtone / highlight weights for an encoded value.
///
/// Triangular and normalised: they sum to exactly 1.0 for every `v` in
/// `0..=1`, which is what makes an equal shift in all three bands a plain
/// offset rather than a triple-counted one.
fn band_weights(v: f32) -> (f32, f32, f32) {
    let shadow = (1.0 - 2.0 * v).max(0.0);
    let highlight = (2.0 * v - 1.0).max(0.0);
    (shadow, 1.0 - shadow - highlight, highlight)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB: ColorSpace = ColorSpace::Srgb;

    fn enc(v: [f32; 3]) -> [f32; 3] {
        to_linear(&SRGB, v)
    }

    fn dec(v: [f32; 3]) -> [f32; 3] {
        from_linear(&SRGB, v)
    }

    fn close3(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() <= tol)
    }

    #[test]
    fn exposure_doubles_linear_light_per_stop() {
        let a = PreparedAdjustment::new(&AdjustmentKind::Exposure { stops: 1.0 });
        let out = a.apply([0.1, 0.2, 0.3], &SRGB);
        assert!(close3(out, [0.2, 0.4, 0.6], 1e-6), "{out:?}");

        // And it is *not* a doubling of the encoded value — that is the whole
        // point of applying it in linear.
        let encoded_out = dec(out);
        let naive = dec([0.1, 0.2, 0.3]).map(|v| v * 2.0);
        assert!(
            (encoded_out[0] - naive[0]).abs() > 0.05,
            "{encoded_out:?} vs {naive:?}"
        );

        let down = PreparedAdjustment::new(&AdjustmentKind::Exposure { stops: -1.0 });
        assert!(close3(
            down.apply([0.4, 0.4, 0.4], &SRGB),
            [0.2, 0.2, 0.2],
            1e-6
        ));
    }

    #[test]
    fn zero_stops_is_the_identity() {
        let a = PreparedAdjustment::new(&AdjustmentKind::Exposure { stops: 0.0 });
        assert!(a.is_identity());
        assert_eq!(a.apply([0.3, 0.4, 0.5], &SRGB), [0.3, 0.4, 0.5]);
    }

    #[test]
    fn levels_stretches_the_encoded_ramp() {
        // black 0.25, white 0.75: encoded 0.5 sits exactly in the middle.
        let a = PreparedAdjustment::new(&AdjustmentKind::Levels {
            black: 0.25,
            white: 0.75,
            gamma: 1.0,
        });
        let out = dec(a.apply(enc([0.5, 0.5, 0.5]), &SRGB));
        assert!((out[0] - 0.5).abs() < 1e-4, "{out:?}");

        // Encoded 0.25 clamps to black, 0.75 to white.
        let lo = dec(a.apply(enc([0.25, 0.25, 0.25]), &SRGB));
        let hi = dec(a.apply(enc([0.75, 0.75, 0.75]), &SRGB));
        assert!(lo[0] < 1e-4, "{lo:?}");
        assert!(hi[0] > 1.0 - 1e-4, "{hi:?}");
    }

    #[test]
    fn levels_gamma_above_one_brightens() {
        let a = PreparedAdjustment::new(&AdjustmentKind::Levels {
            black: 0.0,
            white: 1.0,
            gamma: 2.0,
        });
        let out = dec(a.apply(enc([0.25, 0.25, 0.25]), &SRGB));
        // 0.25 ^ (1/2) = 0.5
        assert!((out[0] - 0.5).abs() < 1e-4, "{out:?}");
    }

    #[test]
    fn a_degenerate_levels_range_thresholds_instead_of_dividing_by_zero() {
        let a = PreparedAdjustment::new(&AdjustmentKind::Levels {
            black: 0.5,
            white: 0.5,
            gamma: 1.0,
        });
        let dark = dec(a.apply(enc([0.4, 0.4, 0.4]), &SRGB));
        let light = dec(a.apply(enc([0.6, 0.6, 0.6]), &SRGB));
        assert!(
            dark[0] < 1e-4 && light[0] > 1.0 - 1e-4,
            "{dark:?} {light:?}"
        );
        assert!(dark.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_non_positive_gamma_is_treated_as_one_rather_than_producing_infinity() {
        for bad in [0.0f32, -2.0, f32::NAN, f32::INFINITY] {
            let a = PreparedAdjustment::new(&AdjustmentKind::Levels {
                black: 0.0,
                white: 1.0,
                gamma: bad,
            });
            let out = a.apply(enc([0.5, 0.5, 0.5]), &SRGB);
            assert!(out.iter().all(|v| v.is_finite()), "gamma {bad} -> {out:?}");
            assert!(a.is_identity(), "gamma {bad} must fall back to identity");
        }
    }

    #[test]
    fn curves_interpolate_linearly_between_sorted_points() {
        // Deliberately unsorted input.
        let a = PreparedAdjustment::new(&AdjustmentKind::Curves {
            points: vec![[1.0, 1.0], [0.0, 0.0], [0.5, 0.25]],
        });
        assert!(!a.is_identity());
        let at = |v: f32| dec(a.apply(enc([v, v, v]), &SRGB))[0];
        assert!((at(0.5) - 0.25).abs() < 1e-4, "{}", at(0.5));
        assert!((at(0.25) - 0.125).abs() < 1e-4, "{}", at(0.25));
        assert!((at(0.75) - 0.625).abs() < 1e-4, "{}", at(0.75));
        assert!((at(0.0) - 0.0).abs() < 1e-4);
        assert!((at(1.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn curves_clamp_flat_outside_the_control_points() {
        let a = PreparedAdjustment::new(&AdjustmentKind::Curves {
            points: vec![[0.25, 0.4], [0.75, 0.6]],
        });
        let at = |v: f32| dec(a.apply(enc([v, v, v]), &SRGB))[0];
        assert!((at(0.0) - 0.4).abs() < 1e-4, "{}", at(0.0));
        assert!((at(1.0) - 0.6).abs() < 1e-4, "{}", at(1.0));
    }

    #[test]
    fn fewer_than_two_usable_points_is_the_identity() {
        for points in [
            vec![],
            vec![[0.5, 0.9]],
            // Two points at the same x collapse to one.
            vec![[0.5, 0.9], [0.5, 0.1]],
            // Non-finite points are dropped before the count.
            vec![[0.0, 0.0], [f32::NAN, 0.5]],
        ] {
            let a = PreparedAdjustment::new(&AdjustmentKind::Curves { points });
            assert!(a.is_identity());
            assert_eq!(a.apply([0.3, 0.4, 0.5], &SRGB), [0.3, 0.4, 0.5]);
        }
    }

    #[test]
    fn hue_rotation_moves_hue_and_leaves_lightness_alone() {
        let a = PreparedAdjustment::new(&AdjustmentKind::HueSaturation {
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0,
        });
        // Pure encoded red -> pure encoded green.
        let out = dec(a.apply(enc([1.0, 0.0, 0.0]), &SRGB));
        assert!(close3(out, [0.0, 1.0, 0.0], 1e-4), "{out:?}");
    }

    #[test]
    fn full_desaturation_produces_a_neutral_of_the_same_hsl_lightness() {
        let a = PreparedAdjustment::new(&AdjustmentKind::HueSaturation {
            hue: 0.0,
            saturation: -1.0,
            lightness: 0.0,
        });
        let out = dec(a.apply(enc([1.0, 0.0, 0.0]), &SRGB));
        // HSL lightness of pure red is 0.5.
        assert!(close3(out, [0.5, 0.5, 0.5], 1e-4), "{out:?}");
    }

    #[test]
    fn a_hue_saturation_of_all_zeros_is_the_identity() {
        let a = PreparedAdjustment::new(&AdjustmentKind::HueSaturation {
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
        });
        assert!(a.is_identity());
    }

    #[test]
    fn colour_balance_bands_sum_to_one_at_every_value() {
        for i in 0..=100 {
            let v = i as f32 / 100.0;
            let (s, m, h) = band_weights(v);
            assert!((s + m + h - 1.0).abs() < 1e-6, "v={v}: {s} {m} {h}");
            assert!(s >= 0.0 && m >= 0.0 && h >= 0.0, "v={v}");
        }
        assert_eq!(band_weights(0.0).0, 1.0);
        assert_eq!(band_weights(1.0).2, 1.0);
        assert_eq!(band_weights(0.5).1, 1.0);
    }

    #[test]
    fn colour_balance_shifts_only_the_band_it_names() {
        let a = PreparedAdjustment::new(&AdjustmentKind::ColorBalance {
            shadows: [0.5, 0.0, 0.0],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
        });
        // Black is entirely in the shadow band: the full +0.5 lands.
        let dark = dec(a.apply(enc([0.0, 0.0, 0.0]), &SRGB));
        assert!((dark[0] - 0.5).abs() < 1e-4, "{dark:?}");
        // Mid grey is entirely midtone: no shadow contribution at all.
        let mid = dec(a.apply(enc([0.5, 0.5, 0.5]), &SRGB));
        assert!((mid[0] - 0.5).abs() < 1e-4, "{mid:?}");
        assert!(mid[1] < 0.5 + 1e-4 && mid[1] > 0.5 - 1e-4);
    }

    #[test]
    fn an_all_zero_colour_balance_is_the_identity() {
        let a = PreparedAdjustment::new(&AdjustmentKind::ColorBalance {
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
        });
        assert!(a.is_identity());
    }

    #[test]
    fn every_adjustment_is_total_over_hostile_parameters() {
        let kinds = [
            AdjustmentKind::Levels {
                black: f32::NAN,
                white: f32::NEG_INFINITY,
                gamma: f32::NAN,
            },
            AdjustmentKind::Curves {
                points: vec![[f32::NAN, f32::NAN], [2.0, -5.0], [-1.0, 9.0]],
            },
            AdjustmentKind::Exposure { stops: f32::NAN },
            AdjustmentKind::HueSaturation {
                hue: f32::INFINITY,
                saturation: 99.0,
                lightness: f32::NAN,
            },
            AdjustmentKind::ColorBalance {
                shadows: [f32::NAN; 3],
                midtones: [1e30; 3],
                highlights: [-1e30; 3],
            },
        ];
        for kind in &kinds {
            let a = PreparedAdjustment::new(kind);
            for sample in [[0.0; 3], [0.5, 0.25, 1.0], [1.0; 3]] {
                let out = a.apply(sample, &SRGB);
                assert!(
                    out.iter().all(|v| v.is_finite()),
                    "{kind:?} on {sample:?} -> {out:?}"
                );
            }
        }
    }

    /// The eleven adjustment kinds this module does not evaluate itself must
    /// still be evaluated — a delegating arm that quietly returned the pixel
    /// would make an adjustment layer invisible instead of wrong, which is
    /// harder to notice. Every one of them changes the sample, and every one
    /// agrees with the `adjustments` crate exactly, because that is where the
    /// single implementation lives.
    #[test]
    fn the_delegated_adjustment_kinds_are_actually_evaluated() {
        let kinds = vec![
            AdjustmentKind::BrightnessContrast {
                brightness: 0.1,
                contrast: 0.3,
            },
            AdjustmentKind::Vibrance {
                vibrance: 0.5,
                saturation: 0.0,
            },
            AdjustmentKind::BlackAndWhite {
                weights: [0.4, 0.6, 0.4, 0.6, 0.2, 0.8],
                tint: None,
            },
            AdjustmentKind::PhotoFilter {
                color_srgb: [0.92, 0.69, 0.07],
                density: 0.5,
                preserve_luminosity: false,
            },
            AdjustmentKind::ChannelMixer {
                rows: [
                    [0.8, 0.2, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                ],
                monochrome: false,
            },
            AdjustmentKind::Invert,
            AdjustmentKind::Posterize { levels: 4 },
            AdjustmentKind::Threshold { level: 0.5 },
            AdjustmentKind::GradientMap {
                stops: vec![(0.0, [0.1, 0.0, 0.3]), (1.0, [1.0, 0.9, 0.4])],
                reverse: false,
            },
            AdjustmentKind::SelectiveColor {
                ranges: [[0.2, -0.1, 0.05, 0.05]; 9],
                relative: false,
            },
            // Auto needs image statistics it is never given here, so it is the
            // one delegated kind that is legitimately an identity.
            AdjustmentKind::Auto {
                mode: layer_model::AutoAdjustment::Tone,
                clip: 0.001,
            },
        ];
        assert_eq!(kinds.len(), 11);
        let sample = [0.18, 0.22, 0.14];
        for kind in &kinds {
            let mine = PreparedAdjustment::new(kind).apply(sample, &SRGB);
            let theirs = adjustments::PreparedAdjustment::new(&adjustments::Adjustment::from(kind))
                .apply(adjustments::LinearRgb(sample), &SRGB)
                .get();
            assert_eq!(mine, theirs, "{kind:?} disagreed with `adjustments`");
            let is_auto = matches!(kind, AdjustmentKind::Auto { .. });
            assert_eq!(
                mine == sample,
                is_auto,
                "{kind:?} left the sample alone: {mine:?}"
            );
            assert_eq!(PreparedAdjustment::new(kind).is_identity(), is_auto);
        }
    }

    #[test]
    fn the_convenience_wrapper_matches_the_prepared_form() {
        let kind = AdjustmentKind::Exposure { stops: 0.5 };
        assert_eq!(
            apply_adjustment(&kind, [0.2, 0.3, 0.4], &SRGB),
            PreparedAdjustment::new(&kind).apply([0.2, 0.3, 0.4], &SRGB)
        );
    }

    #[test]
    fn a_p3_document_adjusts_in_p3_not_in_srgb() {
        // The encode/decode round trip goes through the document's space, so
        // the same Levels on the same linear sample lands somewhere else.
        let a = PreparedAdjustment::new(&AdjustmentKind::Levels {
            black: 0.2,
            white: 0.9,
            gamma: 1.0,
        });
        let linear = [0.6, 0.2, 0.1];
        let in_srgb = a.apply(linear, &ColorSpace::Srgb);
        let in_p3 = a.apply(linear, &ColorSpace::DisplayP3);
        assert!(!close3(in_srgb, in_p3, 1e-3), "{in_srgb:?} vs {in_p3:?}");
    }
}
