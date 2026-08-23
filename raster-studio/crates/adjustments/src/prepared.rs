//! The dispatcher: one [`Adjustment`] vocabulary, one [`PreparedAdjustment`]
//! that resolves a layer's parameters once, and batch entry points that walk a
//! whole tile.

use color::ColorSpace;
use layer_model::{AdjustmentKind, AutoAdjustment};

use crate::auto::{AutoKind, AutoMode, ImageStats, DEFAULT_CLIP};
use crate::color_ops::{
    BlackAndWhite, BwTint, ChannelMixer, ColorBalance, Colorize, GradientMap, GradientStop,
    HueSaturation, PhotoFilter, SelectiveColor, Vibrance,
};
use crate::curve::Curve;
use crate::error::{lenient, AdjustmentError};
use crate::space::{EncodedRgb, LinearRgb, WorkingSpace};
use crate::tone::{
    invert, BrightnessContrast, Curves, ExposureParams, Levels, LevelsChannel, Posterize,
    Threshold, MAX_GAMMA, MIN_GAMMA,
};

/// Every adjustment this crate can apply.
///
/// Every variant here has a [`layer_model::AdjustmentKind`] counterpart, so
/// every adjustment can be an adjustment *layer* in a saved document. The two
/// vocabularies are bridged three ways: a `From<&AdjustmentKind>` impl
/// (lenient — a stored document with an out-of-range slider still opens),
/// [`Adjustment::try_from_layer_kind`] (strict — it reports exactly what is
/// wrong), and [`Adjustment::to_layer_kind`] going back.
///
/// The mapping is onto but not one-to-one: five of the stored shapes predate
/// this crate and are narrower than the parameters here, so those five
/// adjustments have a second, wider stored spelling as well. Nothing is ever
/// dropped — [`Adjustment::to_layer_kind`] returns an `AdjustmentKind` rather
/// than an `Option<AdjustmentKind>`, which is the compiler checking that every
/// adjustment and every setting of it can be saved.
#[derive(Debug, Clone, PartialEq)]
pub enum Adjustment {
    /// Brightness and contrast, encoded.
    BrightnessContrast(BrightnessContrast),
    /// Levels, composite and per channel, encoded.
    Levels(Levels),
    /// Curves, composite and per channel, encoded.
    Curves(Curves),
    /// Exposure / offset / gamma, linear.
    Exposure(ExposureParams),
    /// Vibrance and saturation, encoded.
    Vibrance(Vibrance),
    /// Hue rotation, saturation and lightness, encoded.
    HueSaturation(HueSaturation),
    /// Shadow / midtone / highlight colour shifts, encoded.
    ColorBalance(ColorBalance),
    /// Black & White with per-colour weights and an optional tint, encoded.
    BlackAndWhite(BlackAndWhite),
    /// Photographic filter, linear.
    PhotoFilter(PhotoFilter),
    /// Channel mixer, encoded.
    ChannelMixer(ChannelMixer),
    /// Invert, encoded.
    Invert,
    /// Posterize, encoded.
    Posterize(Posterize),
    /// Threshold, encoded.
    Threshold(Threshold),
    /// Gradient map, linear.
    GradientMap(GradientMap),
    /// Selective colour, encoded.
    SelectiveColor(SelectiveColor),
    /// Auto Tone / Auto Contrast / Auto Color. Needs an [`ImageStats`] before
    /// it means anything — see [`PreparedAdjustment::with_stats`].
    Auto(AutoMode),
}

impl Adjustment {
    /// Which representation this adjustment's maths is defined on.
    ///
    /// This is a *declaration*; the enforcement is structural, in
    /// [`PreparedAdjustment`], whose linear operations are handed no
    /// [`ColorSpace`] and therefore cannot consult one. The two are checked
    /// against each other by a test.
    pub fn working_space(&self) -> WorkingSpace {
        match self {
            Adjustment::Exposure(_) | Adjustment::PhotoFilter(_) | Adjustment::GradientMap(_) => {
                WorkingSpace::Linear
            }
            _ => WorkingSpace::Encoded,
        }
    }

    /// Whether this adjustment provably cannot change any pixel.
    ///
    /// [`Adjustment::Auto`] answers `false` because the answer depends on the
    /// image; an *unresolved* auto adjustment is nonetheless an identity when
    /// prepared, see [`PreparedAdjustment::new`].
    pub fn is_identity(&self) -> bool {
        match self {
            Adjustment::BrightnessContrast(p) => p.is_identity(),
            Adjustment::Levels(p) => p.is_identity(),
            Adjustment::Curves(p) => p.is_identity(),
            Adjustment::Exposure(p) => p.is_identity(),
            Adjustment::Vibrance(p) => p.is_identity(),
            Adjustment::HueSaturation(p) => p.is_identity(),
            Adjustment::ColorBalance(p) => p.is_identity(),
            Adjustment::PhotoFilter(p) => p.is_identity(),
            Adjustment::ChannelMixer(p) => p.is_identity(),
            Adjustment::SelectiveColor(p) => p.is_identity(),
            Adjustment::BlackAndWhite(_)
            | Adjustment::Invert
            | Adjustment::Posterize(_)
            | Adjustment::Threshold(_)
            | Adjustment::GradientMap(_)
            | Adjustment::Auto(_) => false,
        }
    }

    /// The on-disk [`AdjustmentKind`] equivalent.
    ///
    /// **Total by construction**: the return type is `AdjustmentKind`, not
    /// `Option<AdjustmentKind>`, so "every adjustment can be a saved adjustment
    /// *layer*" is a fact the compiler checks rather than a claim a test has to
    /// keep up with. It used to answer `None` for five settings that had no
    /// stored spelling — per-channel Levels, a Levels output range, per-channel
    /// Curves, an exposure offset or gamma, a colorizing Hue/Saturation, a
    /// luminosity-preserving Colour Balance — and those now have one.
    ///
    /// Five adjustments therefore have two stored spellings, a narrow one that
    /// predates this crate and a `*Full` one. The narrow spelling is chosen
    /// whenever the settings fit in it, so an ordinary document keeps writing
    /// exactly the bytes it wrote before; the wide one appears only when they do
    /// not. Reading a needlessly wide document and writing it back
    /// canonicalises it to the narrow spelling, which is a change of spelling
    /// and not of the adjustment
    /// (`a_needlessly_wide_stored_form_canonicalises_to_the_narrow_one`).
    pub fn to_layer_kind(&self) -> AdjustmentKind {
        match self {
            Adjustment::Levels(l) => {
                let c = l.composite;
                if !l.red.is_identity()
                    || !l.green.is_identity()
                    || !l.blue.is_identity()
                    || c.output_black() != 0.0
                    || c.output_white() != 1.0
                {
                    return AdjustmentKind::LevelsFull {
                        composite: levels_channel_to_array(&l.composite),
                        red: levels_channel_to_array(&l.red),
                        green: levels_channel_to_array(&l.green),
                        blue: levels_channel_to_array(&l.blue),
                    };
                }
                AdjustmentKind::Levels {
                    black: c.input_black(),
                    white: c.input_white(),
                    gamma: c.gamma(),
                }
            }
            Adjustment::Curves(c) => {
                if !c.red.is_identity() || !c.green.is_identity() || !c.blue.is_identity() {
                    return AdjustmentKind::CurvesFull {
                        composite: c.composite.points(),
                        red: c.red.points(),
                        green: c.green.points(),
                        blue: c.blue.points(),
                    };
                }
                AdjustmentKind::Curves {
                    points: c.composite.points(),
                }
            }
            Adjustment::Exposure(e) => {
                if e.offset() != 0.0 || e.gamma() != 1.0 {
                    return AdjustmentKind::ExposureFull {
                        stops: e.stops(),
                        offset: e.offset(),
                        gamma: e.gamma(),
                    };
                }
                AdjustmentKind::Exposure { stops: e.stops() }
            }
            Adjustment::HueSaturation(h) => {
                if let Some(c) = h.colorize() {
                    return AdjustmentKind::HueSaturationFull {
                        hue: h.hue_degrees(),
                        saturation: h.saturation(),
                        lightness: h.lightness(),
                        colorize: Some([c.hue(), c.saturation(), c.lightness()]),
                    };
                }
                AdjustmentKind::HueSaturation {
                    hue: h.hue_degrees(),
                    saturation: h.saturation(),
                    lightness: h.lightness(),
                }
            }
            Adjustment::ColorBalance(b) => {
                if b.preserve_luminosity() {
                    return AdjustmentKind::ColorBalanceFull {
                        shadows: b.shadows(),
                        midtones: b.midtones(),
                        highlights: b.highlights(),
                        preserve_luminosity: true,
                    };
                }
                AdjustmentKind::ColorBalance {
                    shadows: b.shadows(),
                    midtones: b.midtones(),
                    highlights: b.highlights(),
                }
            }
            Adjustment::BrightnessContrast(p) => AdjustmentKind::BrightnessContrast {
                brightness: p.brightness(),
                contrast: p.contrast(),
            },
            Adjustment::Vibrance(p) => AdjustmentKind::Vibrance {
                vibrance: p.vibrance(),
                saturation: p.saturation(),
            },
            Adjustment::BlackAndWhite(p) => AdjustmentKind::BlackAndWhite {
                weights: p.weights(),
                tint: p.tint().map(|t| [t.hue(), t.saturation()]),
            },
            Adjustment::PhotoFilter(p) => AdjustmentKind::PhotoFilter {
                color_srgb: p.color_srgb(),
                density: p.density(),
                preserve_luminosity: p.preserve_luminosity(),
            },
            Adjustment::ChannelMixer(p) => AdjustmentKind::ChannelMixer {
                rows: p.rows(),
                monochrome: p.is_monochrome(),
            },
            Adjustment::Invert => AdjustmentKind::Invert,
            Adjustment::Posterize(p) => AdjustmentKind::Posterize { levels: p.levels() },
            Adjustment::Threshold(p) => AdjustmentKind::Threshold { level: p.level() },
            Adjustment::GradientMap(p) => AdjustmentKind::GradientMap {
                stops: p.ramp(),
                reverse: p.is_reversed(),
            },
            Adjustment::SelectiveColor(p) => AdjustmentKind::SelectiveColor {
                ranges: p.ranges(),
                relative: p.is_relative(),
            },
            Adjustment::Auto(m) => AdjustmentKind::Auto {
                mode: to_stored_auto(m.kind()),
                clip: m.clip(),
            },
        }
    }
}

/// The stored `[input_black, input_white, gamma, output_black, output_white]`
/// spelling of one levels channel.
fn levels_channel_to_array(c: &LevelsChannel) -> [f32; 5] {
    [
        c.input_black(),
        c.input_white(),
        c.gamma(),
        c.output_black(),
        c.output_white(),
    ]
}

/// Strict: every stored value must already be in range.
fn levels_channel_from_array(a: [f32; 5]) -> Result<LevelsChannel, AdjustmentError> {
    LevelsChannel::new(a[0], a[1], a[2])?.with_output(a[3], a[4])
}

/// Lenient: out-of-range values are pulled back, a non-positive gamma becomes
/// `1.0`, and a channel that still will not build degrades to the identity.
fn levels_channel_from_array_lenient(a: [f32; 5]) -> LevelsChannel {
    let gamma = if a[2].is_finite() && a[2] > 0.0 {
        a[2].clamp(MIN_GAMMA, MAX_GAMMA)
    } else {
        1.0
    };
    LevelsChannel::new(
        lenient(a[0], 0.0, 1.0, 0.0),
        lenient(a[1], 0.0, 1.0, 1.0),
        gamma,
    )
    .and_then(|c| c.with_output(lenient(a[3], 0.0, 1.0, 0.0), lenient(a[4], 0.0, 1.0, 1.0)))
    .unwrap_or(LevelsChannel::IDENTITY)
}

/// Lenient: non-finite control points are dropped, and a list that still will
/// not build becomes the identity curve.
fn curve_from_points_lenient(points: &[[f32; 2]]) -> Curve {
    let cleaned: Vec<[f32; 2]> = points
        .iter()
        .filter(|p| p[0].is_finite() && p[1].is_finite())
        .copied()
        .collect();
    Curve::new(&cleaned).unwrap_or_else(|_| Curve::identity())
}

/// The stored spelling of an [`AutoKind`].
fn to_stored_auto(kind: AutoKind) -> AutoAdjustment {
    match kind {
        AutoKind::Contrast => AutoAdjustment::Contrast,
        AutoKind::Tone => AutoAdjustment::Tone,
        AutoKind::Color => AutoAdjustment::Color,
    }
}

/// The in-memory spelling of an [`AutoAdjustment`].
fn from_stored_auto(mode: AutoAdjustment) -> AutoKind {
    match mode {
        AutoAdjustment::Contrast => AutoKind::Contrast,
        AutoAdjustment::Tone => AutoKind::Tone,
        AutoAdjustment::Color => AutoKind::Color,
    }
}

/// Lenient conversion from the stored vocabulary.
///
/// Out-of-range sliders are clamped and a non-positive or non-finite gamma
/// becomes `1.0`, because refusing to open a document is a worse failure than
/// moving a slider back into its range. Anything that still cannot be built —
/// a Levels whose white point is below its black point, a Curves with fewer
/// than two distinct control points — degrades to an identity, so a corrupt
/// document renders as an unadjusted image rather than as garbage.
///
/// Use [`Adjustment::try_from_layer_kind`] instead when the caller wants to
/// know.
impl From<&AdjustmentKind> for Adjustment {
    fn from(kind: &AdjustmentKind) -> Self {
        match kind {
            AdjustmentKind::Levels {
                black,
                white,
                gamma,
            } => {
                let g = if gamma.is_finite() && *gamma > 0.0 {
                    gamma.clamp(MIN_GAMMA, MAX_GAMMA)
                } else {
                    1.0
                };
                let ch = LevelsChannel::new(
                    lenient(*black, 0.0, 1.0, 0.0),
                    lenient(*white, 0.0, 1.0, 1.0),
                    g,
                )
                .unwrap_or(LevelsChannel::IDENTITY);
                Adjustment::Levels(Levels::composite(ch))
            }
            AdjustmentKind::LevelsFull {
                composite,
                red,
                green,
                blue,
            } => Adjustment::Levels(Levels {
                composite: levels_channel_from_array_lenient(*composite),
                red: levels_channel_from_array_lenient(*red),
                green: levels_channel_from_array_lenient(*green),
                blue: levels_channel_from_array_lenient(*blue),
            }),
            AdjustmentKind::Curves { points } => {
                Adjustment::Curves(Curves::composite(curve_from_points_lenient(points)))
            }
            AdjustmentKind::CurvesFull {
                composite,
                red,
                green,
                blue,
            } => Adjustment::Curves(Curves {
                composite: curve_from_points_lenient(composite),
                red: curve_from_points_lenient(red),
                green: curve_from_points_lenient(green),
                blue: curve_from_points_lenient(blue),
            }),
            AdjustmentKind::Exposure { stops } => Adjustment::Exposure(
                ExposureParams::new(lenient(*stops, -32.0, 32.0, 0.0), 0.0, 1.0)
                    .unwrap_or(ExposureParams::IDENTITY),
            ),
            AdjustmentKind::ExposureFull {
                stops,
                offset,
                gamma,
            } => Adjustment::Exposure(
                ExposureParams::new(
                    lenient(*stops, -32.0, 32.0, 0.0),
                    lenient(*offset, -1.0, 1.0, 0.0),
                    if gamma.is_finite() && *gamma > 0.0 {
                        gamma.clamp(MIN_GAMMA, MAX_GAMMA)
                    } else {
                        1.0
                    },
                )
                .unwrap_or(ExposureParams::IDENTITY),
            ),
            AdjustmentKind::HueSaturation {
                hue,
                saturation,
                lightness,
            } => Adjustment::HueSaturation(
                HueSaturation::new(
                    lenient(*hue, -3600.0, 3600.0, 0.0),
                    lenient(*saturation, -1.0, 1.0, 0.0),
                    lenient(*lightness, -1.0, 1.0, 0.0),
                )
                .unwrap_or(HueSaturation::IDENTITY),
            ),
            AdjustmentKind::HueSaturationFull {
                hue,
                saturation,
                lightness,
                colorize,
            } => Adjustment::HueSaturation(match colorize {
                Some(c) => match Colorize::new(
                    lenient(c[0], -3600.0, 3600.0, 0.0),
                    lenient(c[1], 0.0, 1.0, 0.0),
                    lenient(c[2], -1.0, 1.0, 0.0),
                ) {
                    Ok(c) => HueSaturation::colorized(c),
                    Err(_) => HueSaturation::IDENTITY,
                },
                None => HueSaturation::new(
                    lenient(*hue, -3600.0, 3600.0, 0.0),
                    lenient(*saturation, -1.0, 1.0, 0.0),
                    lenient(*lightness, -1.0, 1.0, 0.0),
                )
                .unwrap_or(HueSaturation::IDENTITY),
            }),
            AdjustmentKind::ColorBalance {
                shadows,
                midtones,
                highlights,
            } => Adjustment::ColorBalance(
                ColorBalance::new(
                    shadows.map(|v| lenient(v, -1.0, 1.0, 0.0)),
                    midtones.map(|v| lenient(v, -1.0, 1.0, 0.0)),
                    highlights.map(|v| lenient(v, -1.0, 1.0, 0.0)),
                )
                .unwrap_or(ColorBalance::IDENTITY),
            ),
            AdjustmentKind::ColorBalanceFull {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => Adjustment::ColorBalance(
                ColorBalance::new(
                    shadows.map(|v| lenient(v, -1.0, 1.0, 0.0)),
                    midtones.map(|v| lenient(v, -1.0, 1.0, 0.0)),
                    highlights.map(|v| lenient(v, -1.0, 1.0, 0.0)),
                )
                .unwrap_or(ColorBalance::IDENTITY)
                .with_preserve_luminosity(*preserve_luminosity),
            ),
            AdjustmentKind::BrightnessContrast {
                brightness,
                contrast,
            } => Adjustment::BrightnessContrast(
                BrightnessContrast::new(
                    lenient(*brightness, -1.0, 1.0, 0.0),
                    lenient(*contrast, -1.0, 1.0, 0.0),
                )
                .unwrap_or(BrightnessContrast::IDENTITY),
            ),
            AdjustmentKind::Vibrance {
                vibrance,
                saturation,
            } => Adjustment::Vibrance(
                Vibrance::new(
                    lenient(*vibrance, -1.0, 1.0, 0.0),
                    lenient(*saturation, -1.0, 1.0, 0.0),
                )
                .unwrap_or(Vibrance::IDENTITY),
            ),
            AdjustmentKind::BlackAndWhite { weights, tint } => {
                let bw = BlackAndWhite::new(weights.map(|w| lenient(w, -3.0, 3.0, 1.0)))
                    .unwrap_or(BlackAndWhite::DEFAULT);
                let tint = tint.and_then(|t| {
                    BwTint::new(
                        lenient(t[0], -3600.0, 3600.0, 0.0),
                        lenient(t[1], 0.0, 1.0, 0.0),
                    )
                    .ok()
                });
                Adjustment::BlackAndWhite(bw.with_tint(tint))
            }
            AdjustmentKind::PhotoFilter {
                color_srgb,
                density,
                preserve_luminosity,
            } => Adjustment::PhotoFilter(
                PhotoFilter::new(
                    color_srgb.map(|c| lenient(c, 0.0, 1.0, 1.0)),
                    lenient(*density, 0.0, 1.0, 0.0),
                )
                .unwrap_or(PhotoFilter::NONE)
                .with_preserve_luminosity(*preserve_luminosity),
            ),
            AdjustmentKind::ChannelMixer { rows, monochrome } => Adjustment::ChannelMixer(
                ChannelMixer::new(rows.map(|r| {
                    [
                        lenient(r[0], -2.0, 2.0, 0.0),
                        lenient(r[1], -2.0, 2.0, 0.0),
                        lenient(r[2], -2.0, 2.0, 0.0),
                        lenient(r[3], -1.0, 1.0, 0.0),
                    ]
                }))
                .unwrap_or(ChannelMixer::IDENTITY)
                .monochrome(*monochrome),
            ),
            AdjustmentKind::Invert => Adjustment::Invert,
            // No pre-clamp: clamping first made `unwrap_or` unreachable and
            // turned a corrupt `levels: 0` into a two-level posterise — the
            // most destructive output the control has — where both this
            // conversion and `Posterize::FINEST` promise a corrupt value
            // degrades to the near-identity instead.
            AdjustmentKind::Posterize { levels } => {
                Adjustment::Posterize(Posterize::new(*levels).unwrap_or(Posterize::FINEST))
            }
            AdjustmentKind::Threshold { level } => Adjustment::Threshold(
                Threshold::new(lenient(*level, 0.0, 1.0, 0.5)).unwrap_or(Threshold::MIDDLE),
            ),
            AdjustmentKind::GradientMap { stops, reverse } => {
                let cleaned: Vec<GradientStop> = stops
                    .iter()
                    .filter_map(|(p, c)| {
                        GradientStop::new(
                            lenient(*p, 0.0, 1.0, 0.0),
                            c.map(|v| lenient(v, 0.0, 1.0, 0.0)),
                        )
                        .ok()
                    })
                    .collect();
                Adjustment::GradientMap(
                    GradientMap::new(&cleaned)
                        .unwrap_or_else(|_| GradientMap::black_to_white())
                        .reversed(*reverse),
                )
            }
            AdjustmentKind::SelectiveColor { ranges, relative } => Adjustment::SelectiveColor(
                SelectiveColor::new(ranges.map(|r| r.map(|d| lenient(d, -1.0, 1.0, 0.0))))
                    .unwrap_or(SelectiveColor::IDENTITY)
                    .relative(*relative),
            ),
            AdjustmentKind::Auto { mode, clip } => Adjustment::Auto(
                AutoMode::new(
                    from_stored_auto(*mode),
                    lenient(*clip, 0.0, 0.1, DEFAULT_CLIP),
                )
                .unwrap_or(match mode {
                    AutoAdjustment::Contrast => AutoMode::CONTRAST,
                    AutoAdjustment::Tone => AutoMode::TONE,
                    AutoAdjustment::Color => AutoMode::COLOR,
                }),
            ),
        }
    }
}

impl Adjustment {
    /// Strict conversion from the stored vocabulary: every parameter must
    /// already be in range, and the first one that is not is reported.
    ///
    /// This is an inherent constructor rather than a `TryFrom` impl because the
    /// standard library's blanket `impl<T, U: Into<T>> TryFrom<U> for T`
    /// already supplies an infallible `TryFrom<&AdjustmentKind>` via the
    /// lenient [`From`] above, and the two cannot coexist.
    ///
    /// # Errors
    ///
    /// Whatever the corresponding constructor rejects — see
    /// [`AdjustmentError`].
    pub fn try_from_layer_kind(kind: &AdjustmentKind) -> Result<Self, AdjustmentError> {
        Ok(match kind {
            AdjustmentKind::Levels {
                black,
                white,
                gamma,
            } => Adjustment::Levels(Levels::composite(LevelsChannel::new(
                *black, *white, *gamma,
            )?)),
            AdjustmentKind::LevelsFull {
                composite,
                red,
                green,
                blue,
            } => Adjustment::Levels(Levels {
                composite: levels_channel_from_array(*composite)?,
                red: levels_channel_from_array(*red)?,
                green: levels_channel_from_array(*green)?,
                blue: levels_channel_from_array(*blue)?,
            }),
            AdjustmentKind::Curves { points } => {
                Adjustment::Curves(Curves::composite(Curve::new(points)?))
            }
            AdjustmentKind::CurvesFull {
                composite,
                red,
                green,
                blue,
            } => Adjustment::Curves(Curves {
                composite: Curve::new(composite)?,
                red: Curve::new(red)?,
                green: Curve::new(green)?,
                blue: Curve::new(blue)?,
            }),
            AdjustmentKind::Exposure { stops } => {
                Adjustment::Exposure(ExposureParams::new(*stops, 0.0, 1.0)?)
            }
            AdjustmentKind::ExposureFull {
                stops,
                offset,
                gamma,
            } => Adjustment::Exposure(ExposureParams::new(*stops, *offset, *gamma)?),
            AdjustmentKind::HueSaturation {
                hue,
                saturation,
                lightness,
            } => Adjustment::HueSaturation(HueSaturation::new(*hue, *saturation, *lightness)?),
            AdjustmentKind::HueSaturationFull {
                hue,
                saturation,
                lightness,
                colorize,
            } => Adjustment::HueSaturation(match colorize {
                Some(c) => HueSaturation::colorized(Colorize::new(c[0], c[1], c[2])?),
                None => HueSaturation::new(*hue, *saturation, *lightness)?,
            }),
            AdjustmentKind::ColorBalance {
                shadows,
                midtones,
                highlights,
            } => Adjustment::ColorBalance(ColorBalance::new(*shadows, *midtones, *highlights)?),
            AdjustmentKind::ColorBalanceFull {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => Adjustment::ColorBalance(
                ColorBalance::new(*shadows, *midtones, *highlights)?
                    .with_preserve_luminosity(*preserve_luminosity),
            ),
            AdjustmentKind::BrightnessContrast {
                brightness,
                contrast,
            } => Adjustment::BrightnessContrast(BrightnessContrast::new(*brightness, *contrast)?),
            AdjustmentKind::Vibrance {
                vibrance,
                saturation,
            } => Adjustment::Vibrance(Vibrance::new(*vibrance, *saturation)?),
            AdjustmentKind::BlackAndWhite { weights, tint } => {
                let tint = match tint {
                    Some(t) => Some(BwTint::new(t[0], t[1])?),
                    None => None,
                };
                Adjustment::BlackAndWhite(BlackAndWhite::new(*weights)?.with_tint(tint))
            }
            AdjustmentKind::PhotoFilter {
                color_srgb,
                density,
                preserve_luminosity,
            } => Adjustment::PhotoFilter(
                PhotoFilter::new(*color_srgb, *density)?
                    .with_preserve_luminosity(*preserve_luminosity),
            ),
            AdjustmentKind::ChannelMixer { rows, monochrome } => {
                Adjustment::ChannelMixer(ChannelMixer::new(*rows)?.monochrome(*monochrome))
            }
            AdjustmentKind::Invert => Adjustment::Invert,
            AdjustmentKind::Posterize { levels } => Adjustment::Posterize(Posterize::new(*levels)?),
            AdjustmentKind::Threshold { level } => Adjustment::Threshold(Threshold::new(*level)?),
            AdjustmentKind::GradientMap { stops, reverse } => {
                let mut built = Vec::with_capacity(stops.len());
                for (p, c) in stops {
                    built.push(GradientStop::new(*p, *c)?);
                }
                Adjustment::GradientMap(GradientMap::new(&built)?.reversed(*reverse))
            }
            AdjustmentKind::SelectiveColor { ranges, relative } => {
                Adjustment::SelectiveColor(SelectiveColor::new(*ranges)?.relative(*relative))
            }
            AdjustmentKind::Auto { mode, clip } => {
                Adjustment::Auto(AutoMode::new(from_stored_auto(*mode), *clip)?)
            }
        })
    }
}

/// An operation defined on **linear light**.
///
/// It is handed no [`ColorSpace`], and that is the enforcement behind
/// [`WorkingSpace::Linear`]: a variant added here *cannot* depend on the
/// document's encoding, because it is never told what it is.
#[derive(Debug, Clone, PartialEq)]
enum LinearOp {
    Exposure(ExposureParams),
    /// The filter with its linear multiplier, resolved once per layer.
    PhotoFilter(PhotoFilter, [f32; 3]),
    /// The map with its ramp, resolved once per layer.
    GradientMap(GradientMap, Vec<(f32, [f32; 3])>),
}

impl LinearOp {
    fn apply(&self, px: LinearRgb) -> LinearRgb {
        match self {
            LinearOp::Exposure(e) => e.apply(px),
            LinearOp::PhotoFilter(f, mul) => f.apply_with(px, *mul),
            LinearOp::GradientMap(g, ramp) => g.apply_with(px, ramp),
        }
    }
}

/// An operation defined on **gamma-encoded** values in the document's space.
///
/// It receives the space, because "preserve luminosity" has to weigh the result
/// in linear light and needs to know how to decode.
#[derive(Debug, Clone, PartialEq)]
enum EncodedOp {
    BrightnessContrast(BrightnessContrast),
    Levels(Levels),
    Curves(Curves),
    Vibrance(Vibrance),
    HueSaturation(HueSaturation),
    ColorBalance(ColorBalance),
    BlackAndWhite(BlackAndWhite),
    ChannelMixer(ChannelMixer),
    Invert,
    Posterize(Posterize),
    Threshold(Threshold),
    SelectiveColor(SelectiveColor),
}

impl EncodedOp {
    fn apply(&self, px: EncodedRgb, space: &ColorSpace) -> EncodedRgb {
        match self {
            EncodedOp::BrightnessContrast(p) => p.apply(px),
            EncodedOp::Levels(p) => p.apply(px),
            EncodedOp::Curves(p) => p.apply(px),
            EncodedOp::Vibrance(p) => p.apply(px),
            EncodedOp::HueSaturation(p) => p.apply(px),
            EncodedOp::ColorBalance(p) => p.apply(px, space),
            EncodedOp::BlackAndWhite(p) => p.apply(px),
            EncodedOp::ChannelMixer(p) => p.apply(px),
            EncodedOp::Invert => invert(px),
            EncodedOp::Posterize(p) => p.apply(px),
            EncodedOp::Threshold(p) => p.apply(px),
            EncodedOp::SelectiveColor(p) => p.apply(px),
        }
    }
}

/// `EncodedOp` is by far the larger of the two (a `Curves` carries four
/// splines), and this enum is stored once per prepared layer, so the box costs
/// one allocation per adjustment layer and saves carrying 320 bytes around in
/// every `PreparedAdjustment`.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    Linear(LinearOp),
    Encoded(Box<EncodedOp>),
}

/// An [`Adjustment`] with its per-layer setup done once instead of once per
/// pixel.
///
/// What that setup is depends on the adjustment: a photo filter's linear
/// multiplier, a gradient map's ramp, a curve's spline tangents, and — for
/// every adjustment — the identity test that lets the compositor skip the layer
/// entirely.
///
/// Nothing here approximates. There is deliberately no lookup table, so a tiled
/// render and an untiled one cannot disagree at a tile seam. Precisely:
///
/// * [`apply_rgb`] and [`apply_straight_rgba`] are **bit-identical** to
///   [`apply`] called per pixel — they call it, and
///   `the_batch_path_matches_the_scalar_path_exactly` checks every adjustment
///   against every sample pixel.
/// * [`apply_premultiplied_rgba`] and [`apply_premultiplied_rgba_masked`] add a
///   round trip through [`color::unpremultiply`] / [`color::premultiply`], which
///   is a divide and a multiply by alpha. At `alpha == 1.0` both are exact and
///   the result is still bit-identical
///   (`the_premultiplied_batch_is_bit_identical_at_alpha_one`); at any other
///   alpha it is correct to a rounding error, not bit-identical, which is why
///   `premultiplied_batch_unpremultiplies_first_and_keeps_alpha` compares with
///   a tolerance.
///
/// Seam-freedom does not depend on the exact half: a tile boundary splits a
/// buffer, it does not change any pixel's alpha, so the same pixel takes the
/// same path either way (`splitting_a_buffer_into_tiles_changes_nothing`).
///
/// [`apply`]: PreparedAdjustment::apply
/// [`apply_rgb`]: PreparedAdjustment::apply_rgb
/// [`apply_straight_rgba`]: PreparedAdjustment::apply_straight_rgba
/// [`apply_premultiplied_rgba`]: PreparedAdjustment::apply_premultiplied_rgba
/// [`apply_premultiplied_rgba_masked`]: PreparedAdjustment::apply_premultiplied_rgba_masked
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedAdjustment {
    /// `None` is the identity; it returns its input bit for bit.
    op: Option<Op>,
}

impl PreparedAdjustment {
    /// Resolve an adjustment's parameters.
    ///
    /// An [`Adjustment::Auto`] prepared this way is the **identity**: the three
    /// auto commands are decisions about an image, and no image has been seen.
    /// Use [`with_stats`](Self::with_stats) to resolve one.
    pub fn new(adjustment: &Adjustment) -> Self {
        Self::build(adjustment, None)
    }

    /// Resolve an adjustment against measured image statistics, which is what
    /// [`Adjustment::Auto`] needs. Every other adjustment ignores `stats`.
    pub fn with_stats(adjustment: &Adjustment, stats: &ImageStats) -> Self {
        Self::build(adjustment, Some(stats))
    }

    /// The identity: returns every pixel unchanged, bit for bit.
    pub fn identity() -> Self {
        Self { op: None }
    }

    fn build(adjustment: &Adjustment, stats: Option<&ImageStats>) -> Self {
        if adjustment.is_identity() {
            return Self::identity();
        }
        let op = match adjustment {
            Adjustment::Exposure(p) => Op::Linear(LinearOp::Exposure(*p)),
            Adjustment::PhotoFilter(p) => Op::Linear(LinearOp::PhotoFilter(*p, p.multiplier())),
            Adjustment::GradientMap(p) => Op::Linear(LinearOp::GradientMap(p.clone(), p.ramp())),
            Adjustment::BrightnessContrast(p) => {
                Op::Encoded(Box::new(EncodedOp::BrightnessContrast(*p)))
            }
            Adjustment::Levels(p) => Op::Encoded(Box::new(EncodedOp::Levels(*p))),
            Adjustment::Curves(p) => Op::Encoded(Box::new(EncodedOp::Curves(p.clone()))),
            Adjustment::Vibrance(p) => Op::Encoded(Box::new(EncodedOp::Vibrance(*p))),
            Adjustment::HueSaturation(p) => Op::Encoded(Box::new(EncodedOp::HueSaturation(*p))),
            Adjustment::ColorBalance(p) => Op::Encoded(Box::new(EncodedOp::ColorBalance(*p))),
            Adjustment::BlackAndWhite(p) => Op::Encoded(Box::new(EncodedOp::BlackAndWhite(*p))),
            Adjustment::ChannelMixer(p) => Op::Encoded(Box::new(EncodedOp::ChannelMixer(*p))),
            Adjustment::Invert => Op::Encoded(Box::new(EncodedOp::Invert)),
            Adjustment::Posterize(p) => Op::Encoded(Box::new(EncodedOp::Posterize(*p))),
            Adjustment::Threshold(p) => Op::Encoded(Box::new(EncodedOp::Threshold(*p))),
            Adjustment::SelectiveColor(p) => Op::Encoded(Box::new(EncodedOp::SelectiveColor(*p))),
            Adjustment::Auto(mode) => match stats {
                Some(s) => {
                    let levels = mode.resolve(s);
                    if levels.is_identity() {
                        return Self::identity();
                    }
                    Op::Encoded(Box::new(EncodedOp::Levels(levels)))
                }
                None => return Self::identity(),
            },
        };
        Self { op: Some(op) }
    }

    /// Whether this cannot change any pixel, in which case every entry point
    /// returns its input untouched.
    pub fn is_identity(&self) -> bool {
        self.op.is_none()
    }

    /// The representation this prepared operation actually consumes, or `None`
    /// for the identity.
    ///
    /// Derived from the operation's own shape rather than from a table, so it
    /// cannot drift away from what the code does.
    pub fn working_space(&self) -> Option<WorkingSpace> {
        match &self.op {
            None => None,
            Some(Op::Linear(_)) => Some(WorkingSpace::Linear),
            Some(Op::Encoded(_)) => Some(WorkingSpace::Encoded),
        }
    }

    /// Apply to one linear straight-alpha RGB sample.
    ///
    /// Alpha is neither read nor written: an adjustment layer re-colours the
    /// backdrop, it does not reshape it.
    pub fn apply(&self, px: LinearRgb, space: &ColorSpace) -> LinearRgb {
        match &self.op {
            None => px,
            Some(Op::Linear(op)) => op.apply(px),
            Some(Op::Encoded(op)) => op.apply(px.encode(space), space).decode(space),
        }
    }

    /// Apply over a slice of linear RGB triples.
    pub fn apply_rgb(&self, pixels: &mut [[f32; 3]], space: &ColorSpace) {
        if self.is_identity() {
            return;
        }
        for px in pixels.iter_mut() {
            *px = self.apply(LinearRgb(*px), space).get();
        }
    }

    /// Apply over a slice of linear **straight-alpha** RGBA pixels.
    pub fn apply_straight_rgba(&self, pixels: &mut [[f32; 4]], space: &ColorSpace) {
        if self.is_identity() {
            return;
        }
        for px in pixels.iter_mut() {
            let out = self.apply(LinearRgb([px[0], px[1], px[2]]), space).get();
            px[0] = out[0];
            px[1] = out[1];
            px[2] = out[2];
        }
    }

    /// Apply over a tile of linear **premultiplied** RGBA pixels — the form the
    /// compositor works in.
    ///
    /// Each pixel is un-premultiplied, adjusted, and re-premultiplied with its
    /// original alpha. Adjusting premultiplied values directly is the classic
    /// bug here: a 25%-alpha white pixel is stored as `0.25`, so an adjustment
    /// would treat it as a dark gray and an exposure lift would brighten
    /// translucent regions less than opaque ones.
    ///
    /// Fully transparent pixels are skipped. They carry no colour, and running
    /// e.g. a threshold or a gradient map over them would turn transparent
    /// black into premultiplied-white nonsense the moment alpha was later
    /// raised.
    pub fn apply_premultiplied_rgba(&self, pixels: &mut [[f32; 4]], space: &ColorSpace) {
        if self.is_identity() {
            return;
        }
        for px in pixels.iter_mut() {
            if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                continue;
            }
            *px = self.adjust_premultiplied(*px, space);
        }
    }

    /// The same, blended per pixel by a coverage mask in `0..=1` — an
    /// adjustment layer's mask multiplied by its opacity.
    ///
    /// The blend is done on premultiplied values, which is exact: both sides
    /// share an alpha, so interpolating premultiplied RGB is interpolating
    /// straight RGB scaled by the same constant.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::MaskLengthMismatch`] if `mask` is not exactly as long
    /// as `pixels`. Zipping the two instead would silently adjust a prefix of
    /// the tile.
    pub fn apply_premultiplied_rgba_masked(
        &self,
        pixels: &mut [[f32; 4]],
        mask: &[f32],
        space: &ColorSpace,
    ) -> Result<(), AdjustmentError> {
        if mask.len() != pixels.len() {
            return Err(AdjustmentError::MaskLengthMismatch {
                pixels: pixels.len(),
                mask: mask.len(),
            });
        }
        if self.is_identity() {
            return Ok(());
        }
        for (px, m) in pixels.iter_mut().zip(mask) {
            if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                continue;
            }
            // `f32::clamp` PROPAGATES NaN rather than replacing it, so a NaN
            // coverage entry would fall past both fast paths below and write
            // NaN into the tile. Treat a non-finite mask sample as no
            // coverage, which leaves the pixel bit-identical.
            if !m.is_finite() {
                continue;
            }
            let m = m.clamp(0.0, 1.0);
            if m == 0.0 {
                continue;
            }
            let adjusted = self.adjust_premultiplied(*px, space);
            if m == 1.0 {
                *px = adjusted;
            } else {
                px[0] += (adjusted[0] - px[0]) * m;
                px[1] += (adjusted[1] - px[1]) * m;
                px[2] += (adjusted[2] - px[2]) * m;
            }
        }
        Ok(())
    }

    fn adjust_premultiplied(&self, px: [f32; 4], space: &ColorSpace) -> [f32; 4] {
        let straight = color::unpremultiply(px);
        let out = self
            .apply(LinearRgb([straight[0], straight[1], straight[2]]), space)
            .get();
        color::premultiply([out[0], out[1], out[2], px[3]])
    }
}

/// Apply a stored [`AdjustmentKind`] to one linear straight RGB sample.
///
/// This is the convenience shape the compositor asked for. Parameters are
/// resolved on every call, so use [`PreparedAdjustment`] in a pixel loop.
pub fn apply(kind: &AdjustmentKind, px: LinearRgb, space: &ColorSpace) -> LinearRgb {
    PreparedAdjustment::new(&Adjustment::from(kind)).apply(px, space)
}

/// Apply an [`Adjustment`] to one linear straight RGB sample.
///
/// Parameters are resolved on every call, so use [`PreparedAdjustment`] in a
/// pixel loop.
pub fn apply_adjustment(adjustment: &Adjustment, px: LinearRgb, space: &ColorSpace) -> LinearRgb {
    PreparedAdjustment::new(adjustment).apply(px, space)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto::AutoKind;
    use crate::color_ops::GradientStop;

    const SRGB: ColorSpace = ColorSpace::Srgb;

    /// One instance of every variant, all non-identity, for the sweeps below.
    fn every_adjustment() -> Vec<Adjustment> {
        vec![
            Adjustment::BrightnessContrast(BrightnessContrast::new(0.1, 0.2).unwrap()),
            Adjustment::Levels(Levels::composite(
                LevelsChannel::new(0.05, 0.9, 1.3).unwrap(),
            )),
            Adjustment::Curves(Curves::composite(
                Curve::new(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]).unwrap(),
            )),
            Adjustment::Exposure(ExposureParams::new(0.7, 0.05, 1.2).unwrap()),
            Adjustment::Vibrance(Vibrance::new(0.4, -0.1).unwrap()),
            Adjustment::HueSaturation(HueSaturation::new(25.0, 0.2, -0.1).unwrap()),
            Adjustment::ColorBalance(
                ColorBalance::new([0.1, 0.0, -0.1], [0.0, 0.2, 0.0], [-0.1, 0.0, 0.1])
                    .unwrap()
                    .with_preserve_luminosity(true),
            ),
            Adjustment::BlackAndWhite(BlackAndWhite::DEFAULT),
            Adjustment::PhotoFilter(PhotoFilter::new(PhotoFilter::WARMING_85, 0.4).unwrap()),
            Adjustment::ChannelMixer(
                ChannelMixer::new([
                    [0.9, 0.1, 0.0, 0.02],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.1, 0.0, 0.9, 0.0],
                ])
                .unwrap(),
            ),
            Adjustment::Invert,
            Adjustment::Posterize(Posterize::new(6).unwrap()),
            Adjustment::Threshold(Threshold::new(0.4).unwrap()),
            Adjustment::GradientMap(
                GradientMap::new(&[
                    GradientStop::new(0.0, [0.1, 0.0, 0.3]).unwrap(),
                    GradientStop::new(1.0, [1.0, 0.9, 0.4]).unwrap(),
                ])
                .unwrap(),
            ),
            // Every range carries a delta, so the sweep's sample pixel is
            // guaranteed to be affected whichever range it falls in.
            Adjustment::SelectiveColor(
                SelectiveColor::new([[0.2, -0.1, 0.05, 0.05]; 9])
                    .unwrap()
                    .relative(false),
            ),
            Adjustment::Auto(AutoMode::TONE),
        ]
    }

    /// One instance of every variant, non-identity, restricted to settings the
    /// *narrow* stored [`AdjustmentKind`] spellings can carry. It differs from
    /// [`every_adjustment`] only in the five adjustments that have two stored
    /// spellings: no per-channel Levels or Curves, no exposure offset or gamma,
    /// no luminosity-preserving Colour Balance. The wide settings are covered
    /// by [`every_widened_adjustment`].
    fn every_storable_adjustment() -> Vec<Adjustment> {
        vec![
            Adjustment::BrightnessContrast(BrightnessContrast::new(0.1, -0.2).unwrap()),
            Adjustment::Levels(Levels::composite(
                LevelsChannel::new(0.05, 0.9, 1.3).unwrap(),
            )),
            Adjustment::Curves(Curves::composite(
                Curve::new(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]).unwrap(),
            )),
            Adjustment::Exposure(ExposureParams::stops_only(0.7).unwrap()),
            Adjustment::Vibrance(Vibrance::new(0.4, -0.1).unwrap()),
            Adjustment::HueSaturation(HueSaturation::new(25.0, 0.2, -0.1).unwrap()),
            Adjustment::ColorBalance(
                ColorBalance::new([0.1, 0.0, -0.1], [0.0, 0.2, 0.0], [-0.1, 0.0, 0.1]).unwrap(),
            ),
            Adjustment::BlackAndWhite(
                BlackAndWhite::new([0.35, 0.65, 0.45, 0.55, 0.25, 0.75])
                    .unwrap()
                    .with_tint(Some(BwTint::new(35.0, 0.4).unwrap())),
            ),
            Adjustment::PhotoFilter(
                PhotoFilter::new(PhotoFilter::WARMING_85, 0.4)
                    .unwrap()
                    .with_preserve_luminosity(false),
            ),
            Adjustment::ChannelMixer(
                ChannelMixer::new([
                    [0.9, 0.1, 0.0, 0.02],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.1, 0.0, 0.9, 0.0],
                ])
                .unwrap()
                .monochrome(true),
            ),
            Adjustment::Invert,
            Adjustment::Posterize(Posterize::new(6).unwrap()),
            Adjustment::Threshold(Threshold::new(0.4).unwrap()),
            Adjustment::GradientMap(
                GradientMap::new(&[
                    GradientStop::new(0.0, [0.1, 0.0, 0.3]).unwrap(),
                    GradientStop::new(0.5, [0.6, 0.2, 0.2]).unwrap(),
                    GradientStop::new(1.0, [1.0, 0.9, 0.4]).unwrap(),
                ])
                .unwrap()
                .reversed(true),
            ),
            Adjustment::SelectiveColor(
                SelectiveColor::new([[0.2, -0.1, 0.05, 0.05]; 9])
                    .unwrap()
                    .relative(false),
            ),
            Adjustment::Auto(AutoMode::new(AutoKind::Color, 0.005).unwrap()),
        ]
    }

    /// Every setting that does **not** fit a narrow stored spelling, one per
    /// `*Full` variant plus the two shapes of a wide Levels and Exposure.
    /// These are the settings that used to have no persisted form at all.
    fn every_widened_adjustment() -> Vec<(&'static str, Adjustment)> {
        vec![
            (
                "per-channel levels",
                Adjustment::Levels(
                    Levels::per_channel(
                        LevelsChannel::new(0.1, 0.9, 1.0).unwrap(),
                        LevelsChannel::new(0.0, 0.95, 1.2).unwrap(),
                        LevelsChannel::IDENTITY,
                    )
                    .with_composite(LevelsChannel::new(0.02, 0.98, 1.1).unwrap()),
                ),
            ),
            (
                "levels with an output range",
                Adjustment::Levels(Levels::composite(
                    LevelsChannel::new(0.0, 1.0, 1.2)
                        .unwrap()
                        .with_output(0.1, 0.9)
                        .unwrap(),
                )),
            ),
            (
                "levels with an inverted output range",
                Adjustment::Levels(Levels::composite(
                    LevelsChannel::IDENTITY.with_output(1.0, 0.0).unwrap(),
                )),
            ),
            (
                "per-channel curves",
                Adjustment::Curves(Curves::per_channel(
                    Curve::new(&[[0.0, 0.1], [0.5, 0.55], [1.0, 1.0]]).unwrap(),
                    Curve::identity(),
                    Curve::new(&[[0.0, 0.0], [1.0, 0.9]]).unwrap(),
                )),
            ),
            (
                "per-channel curves under a composite",
                Adjustment::Curves(Curves {
                    composite: Curve::new(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]).unwrap(),
                    red: Curve::new(&[[0.0, 0.05], [1.0, 1.0]]).unwrap(),
                    green: Curve::identity(),
                    blue: Curve::identity(),
                }),
            ),
            (
                "exposure with an offset",
                Adjustment::Exposure(ExposureParams::new(1.0, 0.2, 1.0).unwrap()),
            ),
            (
                "exposure with a gamma",
                Adjustment::Exposure(ExposureParams::new(-0.5, 0.0, 1.4).unwrap()),
            ),
            (
                "exposure with all three",
                Adjustment::Exposure(ExposureParams::new(1.5, -0.05, 2.2).unwrap()),
            ),
            (
                "colorizing hue/saturation",
                Adjustment::HueSaturation(HueSaturation::colorized(
                    Colorize::new(200.0, 0.4, -0.1).unwrap(),
                )),
            ),
            (
                "luminosity-preserving colour balance",
                Adjustment::ColorBalance(
                    ColorBalance::new([0.1, -0.2, 0.0], [0.0, 0.1, 0.0], [0.0, 0.0, 0.3])
                        .unwrap()
                        .with_preserve_luminosity(true),
                ),
            ),
        ]
    }

    /// One instance of every variant at its neutral setting, where such a
    /// setting exists.
    fn identity_adjustments() -> Vec<Adjustment> {
        vec![
            Adjustment::BrightnessContrast(BrightnessContrast::IDENTITY),
            Adjustment::Levels(Levels::IDENTITY),
            Adjustment::Curves(Curves::identity()),
            Adjustment::Exposure(ExposureParams::IDENTITY),
            Adjustment::Vibrance(Vibrance::IDENTITY),
            Adjustment::HueSaturation(HueSaturation::IDENTITY),
            Adjustment::ColorBalance(ColorBalance::IDENTITY),
            Adjustment::PhotoFilter(PhotoFilter::new(PhotoFilter::SEPIA, 0.0).unwrap()),
            Adjustment::ChannelMixer(ChannelMixer::IDENTITY),
            Adjustment::SelectiveColor(SelectiveColor::IDENTITY),
        ]
    }

    fn sample_pixels() -> Vec<LinearRgb> {
        vec![
            LinearRgb([0.0, 0.0, 0.0]),
            LinearRgb([1.0, 1.0, 1.0]),
            LinearRgb([0.2140, 0.2140, 0.2140]),
            LinearRgb([0.8, 0.05, 0.3]),
            LinearRgb([0.02, 0.44, 0.71]),
            LinearRgb([3.5, 0.5, 0.1]),
        ]
    }

    #[test]
    fn identity_parameters_are_a_true_no_op() {
        for adj in identity_adjustments() {
            assert!(adj.is_identity(), "{adj:?} should be an identity");
            let prep = PreparedAdjustment::new(&adj);
            assert!(prep.is_identity());
            assert_eq!(prep.working_space(), None);
            for px in sample_pixels() {
                assert_eq!(prep.apply(px, &SRGB), px, "{adj:?} moved {px:?}");
            }
        }
    }

    /// The declared working space and the prepared operation's shape must
    /// agree. The prepared shape is the one that binds — a `LinearOp` is handed
    /// no colour space — so this catches a variant wired to the wrong side.
    #[test]
    fn declared_working_space_matches_the_prepared_shape() {
        let stats = ImageStats::from_encoded(&[
            EncodedRgb([0.1, 0.2, 0.3]),
            EncodedRgb([0.8, 0.7, 0.9]),
            EncodedRgb([0.4, 0.5, 0.6]),
        ]);
        for adj in every_adjustment() {
            let prep = PreparedAdjustment::with_stats(&adj, &stats);
            assert_eq!(
                prep.working_space(),
                Some(adj.working_space()),
                "{adj:?} declares {:?} but prepares as {:?}",
                adj.working_space(),
                prep.working_space()
            );
        }
    }

    /// The claim that matters: an adjustment declared to work on linear light
    /// gives the same answer whatever the document's colour space is, and one
    /// declared to work on encoded values does not.
    #[test]
    fn linear_space_adjustments_ignore_the_document_space() {
        let stats = ImageStats::from_encoded(&[
            EncodedRgb([0.1, 0.2, 0.3]),
            EncodedRgb([0.8, 0.7, 0.9]),
            EncodedRgb([0.4, 0.5, 0.6]),
        ]);
        let px = LinearRgb([0.31, 0.12, 0.57]);
        for adj in every_adjustment() {
            let prep = PreparedAdjustment::with_stats(&adj, &stats);
            let srgb = prep.apply(px, &ColorSpace::Srgb);
            let linear = prep.apply(px, &ColorSpace::LinearSrgb);
            let p3 = prep.apply(px, &ColorSpace::DisplayP3);
            match adj.working_space() {
                WorkingSpace::Linear => {
                    assert_eq!(srgb, linear, "{adj:?} is not space-independent");
                    assert_eq!(srgb, p3, "{adj:?} is not space-independent");
                }
                WorkingSpace::Encoded => {
                    assert_ne!(srgb, linear, "{adj:?} did not use the document space");
                }
            }
        }
    }

    #[test]
    fn the_batch_path_matches_the_scalar_path_exactly() {
        let stats =
            ImageStats::from_encoded(&[EncodedRgb([0.1, 0.2, 0.3]), EncodedRgb([0.9, 0.8, 0.7])]);
        for adj in every_adjustment() {
            let prep = PreparedAdjustment::with_stats(&adj, &stats);
            let pixels = sample_pixels();

            let mut rgb: Vec<[f32; 3]> = pixels.iter().map(|p| p.get()).collect();
            prep.apply_rgb(&mut rgb, &SRGB);
            for (i, px) in pixels.iter().enumerate() {
                assert_eq!(rgb[i], prep.apply(*px, &SRGB).get(), "{adj:?} pixel {i}");
            }

            let mut straight: Vec<[f32; 4]> = pixels
                .iter()
                .map(|p| [p.get()[0], p.get()[1], p.get()[2], 0.5])
                .collect();
            prep.apply_straight_rgba(&mut straight, &SRGB);
            for (i, px) in pixels.iter().enumerate() {
                let expect = prep.apply(*px, &SRGB).get();
                assert_eq!(&straight[i][..3], &expect[..], "{adj:?} pixel {i}");
                assert_eq!(straight[i][3], 0.5, "alpha changed");
            }
        }
    }

    #[test]
    fn premultiplied_batch_unpremultiplies_first_and_keeps_alpha() {
        // Two pixels of the same colour at different alphas must receive the
        // same colour change, which is only true if alpha is divided out first.
        //
        // The adjustment has to be non-homogeneous for this to bind. An
        // exposure is a pure scale, so it commutes with premultiplication and
        // gives the right answer even from the wrong values; Invert does not.
        let prep = PreparedAdjustment::new(&Adjustment::Invert);
        let straight = [0.4f32, 0.15, 0.7];
        let expect = prep.apply(LinearRgb(straight), &SRGB).get();
        let mut px = [
            color::premultiply([straight[0], straight[1], straight[2], 1.0]),
            color::premultiply([straight[0], straight[1], straight[2], 0.5]),
            color::premultiply([straight[0], straight[1], straight[2], 0.05]),
        ];
        prep.apply_premultiplied_rgba(&mut px, &SRGB);
        for (i, alpha) in [1.0f32, 0.5, 0.05].into_iter().enumerate() {
            let got = color::unpremultiply(px[i]);
            for c in 0..3 {
                assert!(
                    (got[c] - expect[c]).abs() < 1e-5,
                    "alpha {alpha}: {got:?} vs {expect:?}"
                );
            }
            assert_eq!(px[i][3], alpha, "alpha changed");
        }
        // And the stored values really are premultiplied, not straight.
        assert!(
            (px[1][0] - expect[0] * 0.5).abs() < 1e-5,
            "the result was not re-premultiplied: {:?}",
            px[1]
        );
    }

    /// The narrow half of the bit-identity claim: at `alpha == 1.0` the
    /// un/re-premultiply round trip is a divide and a multiply by exactly one,
    /// so the premultiplied entry points match the scalar path bit for bit —
    /// including the masked one at full coverage. At any other alpha they do
    /// not, and the documentation says so rather than over-claiming.
    #[test]
    fn the_premultiplied_batch_is_bit_identical_at_alpha_one() {
        let stats =
            ImageStats::from_encoded(&[EncodedRgb([0.1, 0.2, 0.3]), EncodedRgb([0.9, 0.8, 0.7])]);
        for adj in every_adjustment() {
            let prep = PreparedAdjustment::with_stats(&adj, &stats);
            let pixels = sample_pixels();

            let mut premul: Vec<[f32; 4]> = pixels
                .iter()
                .map(|p| [p.get()[0], p.get()[1], p.get()[2], 1.0])
                .collect();
            let mut masked = premul.clone();
            prep.apply_premultiplied_rgba(&mut premul, &SRGB);
            let full = vec![1.0f32; pixels.len()];
            prep.apply_premultiplied_rgba_masked(&mut masked, &full, &SRGB)
                .unwrap();

            for (i, px) in pixels.iter().enumerate() {
                let expect = prep.apply(*px, &SRGB).get();
                assert_eq!(&premul[i][..3], &expect[..], "{adj:?} pixel {i}");
                assert_eq!(&masked[i][..3], &expect[..], "{adj:?} masked pixel {i}");
                assert_eq!(premul[i][3], 1.0);
            }
        }
    }

    #[test]
    fn premultiplied_batch_leaves_transparent_pixels_alone() {
        // Threshold would otherwise turn a transparent pixel into white.
        let prep = PreparedAdjustment::new(&Adjustment::Threshold(Threshold::new(0.0).unwrap()));
        let mut px = [[0.0, 0.0, 0.0, 0.0]];
        prep.apply_premultiplied_rgba(&mut px, &SRGB);
        assert_eq!(px, [[0.0, 0.0, 0.0, 0.0]]);
    }

    #[test]
    fn a_mask_blends_between_the_original_and_the_adjusted_pixel() {
        let prep = PreparedAdjustment::new(&Adjustment::Exposure(
            ExposureParams::stops_only(1.0).unwrap(),
        ));
        let base = color::premultiply([0.4, 0.4, 0.4, 1.0]);
        let mut px = [base, base, base];
        prep.apply_premultiplied_rgba_masked(&mut px, &[0.0, 0.5, 1.0], &SRGB)
            .unwrap();
        assert_eq!(px[0], base, "a zero mask must leave the pixel untouched");
        assert!((px[1][0] - 0.6).abs() < 1e-6, "{:?}", px[1]);
        assert!((px[2][0] - 0.8).abs() < 1e-6, "{:?}", px[2]);
    }

    #[test]
    fn a_mismatched_mask_is_an_error_not_a_silent_prefix() {
        let prep = PreparedAdjustment::new(&Adjustment::Exposure(
            ExposureParams::stops_only(1.0).unwrap(),
        ));
        let mut px = [[0.4, 0.4, 0.4, 1.0]; 3];
        assert_eq!(
            prep.apply_premultiplied_rgba_masked(&mut px, &[1.0, 1.0], &SRGB),
            Err(AdjustmentError::MaskLengthMismatch { pixels: 3, mask: 2 })
        );
        assert_eq!(px, [[0.4, 0.4, 0.4, 1.0]; 3], "the buffer was modified");
    }

    // --- bridge to the stored vocabulary ---------------------------------

    #[test]
    fn the_free_function_dispatches_a_stored_kind() {
        let kind = AdjustmentKind::Exposure { stops: 1.0 };
        let out = apply(&kind, LinearRgb([0.25, 0.5, 2.0]), &SRGB).get();
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[2] - 4.0).abs() < 1e-6, "the clamp is back: {out:?}");
    }

    /// The wide spellings are not just storable, they are *evaluated*: a
    /// document carrying one must render as the adjustment it describes, not as
    /// a no-op. A delegating arm that quietly returned the pixel would make the
    /// layer invisible rather than wrong, which is harder to notice.
    #[test]
    fn the_free_function_dispatches_the_wide_stored_kinds() {
        let px = LinearRgb([0.2, 0.35, 0.5]);
        for (name, adj) in every_widened_adjustment() {
            let kind = adj.to_layer_kind();
            let via_kind = apply(&kind, px, &SRGB);
            let direct = apply_adjustment(&adj, px, &SRGB);
            assert_eq!(via_kind, direct, "{name} took a different path");
            assert_ne!(via_kind, px, "{name} rendered as a no-op");
        }
        // And specifically the red-only per-channel Levels moves red alone.
        let red_only = AdjustmentKind::LevelsFull {
            composite: [0.0, 1.0, 1.0, 0.0, 1.0],
            red: [0.0, 1.0, 2.0, 0.0, 1.0],
            green: [0.0, 1.0, 1.0, 0.0, 1.0],
            blue: [0.0, 1.0, 1.0, 0.0, 1.0],
        };
        let out = apply(&red_only, px, &SRGB).get();
        // Red is lifted by a factor of two in gamma; green and blue only pick
        // up the encode/decode round trip's rounding error.
        assert!(out[0] > px.get()[0] + 0.2, "{out:?}");
        assert!((out[1] - px.get()[1]).abs() < 1e-6, "green moved: {out:?}");
        assert!((out[2] - px.get()[2]).abs() < 1e-6, "blue moved: {out:?}");
    }

    /// Every adjustment must survive a save/reload as an adjustment *layer*.
    /// An adjustment with no stored spelling is not an adjustment layer at all,
    /// only a transient computation, so this is the test that makes "the full
    /// adjustment set" true of a *document* rather than of a `Vec` in memory.
    ///
    /// The fixture uses non-default settings everywhere, so a conversion that
    /// dropped a field would come back as something else.
    #[test]
    fn every_adjustment_round_trips_through_the_stored_vocabulary() {
        let storable = every_storable_adjustment();
        // All sixteen variants, no repeats: a sweep that quietly skipped a
        // variant would prove nothing about it. `Vec::dedup` alone removes only
        // *consecutive* duplicates, so a fixture that repeated one variant out
        // of order while omitting another would still count sixteen; sorting
        // first is what makes the count a completeness proof.
        let mut seen: Vec<String> = storable
            .iter()
            .map(|a| format!("{:?}", std::mem::discriminant(a)))
            .collect();
        let total = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), total, "the fixture repeats a variant");
        assert_eq!(seen.len(), 16, "the fixture must cover every variant once");

        for adj in storable {
            let kind = adj.to_layer_kind();
            let back = Adjustment::try_from_layer_kind(&kind)
                .unwrap_or_else(|e| panic!("{kind:?} would not load back: {e}"));
            assert_eq!(back, adj, "{kind:?}");
            // The lenient path has to agree with the strict one on data that is
            // already valid, or opening a good document would change it.
            assert_eq!(Adjustment::from(&kind), adj, "lenient path: {kind:?}");
            // And back out again to exactly the same stored value.
            assert_eq!(back.to_layer_kind(), kind);
            // Through a real serialization, which is the point of storing it.
            let json = serde_json::to_string(&kind).unwrap();
            let decoded: AdjustmentKind = serde_json::from_str(&json).unwrap();
            assert_eq!(Adjustment::try_from_layer_kind(&decoded).unwrap(), adj);
        }
    }

    /// The identity settings round trip too — a neutral adjustment layer is a
    /// perfectly ordinary thing to save, and it is where a conversion that
    /// substituted a default would hide.
    #[test]
    fn identity_adjustments_round_trip_through_the_stored_vocabulary() {
        for adj in identity_adjustments() {
            let kind = adj.to_layer_kind();
            let back = Adjustment::try_from_layer_kind(&kind).unwrap();
            assert_eq!(back, adj, "{kind:?}");
            assert!(back.is_identity(), "{kind:?} stopped being an identity");
        }
    }

    /// The settings that used to have **no** stored form at all — per-channel
    /// Levels and Curves, a Levels output range, an exposure offset or gamma, a
    /// colorizing Hue/Saturation, a luminosity-preserving Colour Balance. Each
    /// now round trips through its `*Full` spelling, so "Levels (per-channel +
    /// composite)" and "Exposure (exposure/offset/gamma)" are adjustment
    /// *layers*, not transient computations.
    #[test]
    fn the_widened_settings_round_trip_through_the_full_spellings() {
        for (name, adj) in every_widened_adjustment() {
            let kind = adj.to_layer_kind();
            // It must be a wide spelling: the narrow one physically cannot hold
            // these settings, so a narrow answer here would mean one was
            // dropped.
            assert!(
                matches!(
                    kind,
                    AdjustmentKind::LevelsFull { .. }
                        | AdjustmentKind::CurvesFull { .. }
                        | AdjustmentKind::ExposureFull { .. }
                        | AdjustmentKind::HueSaturationFull { .. }
                        | AdjustmentKind::ColorBalanceFull { .. }
                ),
                "{name} was stored as the narrow {kind:?}"
            );
            let back = Adjustment::try_from_layer_kind(&kind)
                .unwrap_or_else(|e| panic!("{name} would not load back: {e}"));
            assert_eq!(back, adj, "{name}");
            assert_eq!(Adjustment::from(&kind), adj, "lenient path: {name}");
            assert_eq!(back.to_layer_kind(), kind, "{name}");
            // Through a real serialization, which is the point of storing it.
            let json = serde_json::to_string(&kind).unwrap();
            let decoded: AdjustmentKind = serde_json::from_str(&json).unwrap();
            assert_eq!(
                Adjustment::try_from_layer_kind(&decoded).unwrap(),
                adj,
                "{name}"
            );
        }
    }

    /// All five `*Full` spellings are exercised by the fixture above, and no
    /// two of its cases collapse onto the same stored value.
    #[test]
    fn every_full_spelling_is_covered_and_the_cases_are_distinct() {
        let kinds: Vec<AdjustmentKind> = every_widened_adjustment()
            .into_iter()
            .map(|(_, adj)| adj.to_layer_kind())
            .collect();
        let mut spellings: Vec<String> = kinds
            .iter()
            .map(|k| format!("{:?}", std::mem::discriminant(k)))
            .collect();
        spellings.sort();
        spellings.dedup();
        assert_eq!(spellings.len(), 5, "not every *Full spelling is exercised");
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(a, b, "two cases stored identically");
            }
        }
    }

    /// Totality is in the *type* — `to_layer_kind` returns an `AdjustmentKind`,
    /// not an `Option` — so what is left to check is that it is *canonical*:
    /// the narrow spelling wins whenever the settings fit, and an ordinary
    /// document keeps writing the bytes it always wrote instead of silently
    /// migrating to a wider variant no older reader understands.
    #[test]
    fn to_layer_kind_prefers_the_narrow_spelling() {
        for adj in every_storable_adjustment() {
            let kind = adj.to_layer_kind();
            assert!(
                !matches!(
                    kind,
                    AdjustmentKind::LevelsFull { .. }
                        | AdjustmentKind::CurvesFull { .. }
                        | AdjustmentKind::ExposureFull { .. }
                        | AdjustmentKind::HueSaturationFull { .. }
                        | AdjustmentKind::ColorBalanceFull { .. }
                ),
                "{adj:?} fits the narrow spelling but was widened to {kind:?}"
            );
        }
    }

    /// A wide spelling holding settings that would fit the narrow one is read
    /// back and then *re-written narrow*. That is a deliberate canonicalisation
    /// rather than a loss: the adjustment itself is unchanged.
    #[test]
    fn a_needlessly_wide_stored_form_canonicalises_to_the_narrow_one() {
        let wide = AdjustmentKind::ColorBalanceFull {
            shadows: [0.1, 0.0, -0.2],
            midtones: [0.0; 3],
            highlights: [0.0, 0.3, 0.0],
            preserve_luminosity: false,
        };
        let adj = Adjustment::try_from_layer_kind(&wide).unwrap();
        assert_eq!(
            adj.to_layer_kind(),
            AdjustmentKind::ColorBalance {
                shadows: [0.1, 0.0, -0.2],
                midtones: [0.0; 3],
                highlights: [0.0, 0.3, 0.0],
            }
        );
        // And the adjustment is the same one either way.
        assert_eq!(
            adj,
            Adjustment::try_from_layer_kind(&adj.to_layer_kind()).unwrap()
        );
    }

    /// The lenient path has to survive a corrupt *wide* document too, not just
    /// a corrupt narrow one.
    #[test]
    fn the_lenient_conversion_survives_a_corrupt_wide_document() {
        let bad = AdjustmentKind::LevelsFull {
            // Degenerate span, negative gamma, out-of-range output.
            composite: [0.9, 0.1, -4.0, -3.0, 9.0],
            red: [f32::NAN; 5],
            green: [0.0, 1.0, 1.0, 0.0, 1.0],
            blue: [0.0, 1.0, 1.0, 0.0, 1.0],
        };
        assert_eq!(
            Adjustment::from(&bad),
            Adjustment::Levels(Levels::IDENTITY),
            "a corrupt wide levels should degrade to neutral"
        );
        assert!(Adjustment::try_from_layer_kind(&bad).is_err());

        let bad_curves = AdjustmentKind::CurvesFull {
            composite: vec![[0.5, 0.1], [0.5, 0.9]],
            red: vec![[f32::NAN, 0.0], [0.0, 0.0], [1.0, 0.5]],
            green: vec![],
            blue: vec![[0.0, 0.0], [1.0, 1.0]],
        };
        match Adjustment::from(&bad_curves) {
            Adjustment::Curves(c) => {
                assert!(c.composite.is_identity(), "duplicate x was not repaired");
                assert!(c.green.is_identity(), "an empty list was not repaired");
                assert!(!c.red.is_identity(), "the NaN point took the curve with it");
            }
            other => panic!("{other:?}"),
        }

        let bad_exposure = AdjustmentKind::ExposureFull {
            stops: 99.0,
            offset: -9.0,
            gamma: 0.0,
        };
        match Adjustment::from(&bad_exposure) {
            Adjustment::Exposure(e) => {
                assert_eq!(e.stops(), 32.0);
                assert_eq!(e.offset(), -1.0);
                assert_eq!(e.gamma(), 1.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_stored_subset_round_trips() {
        let kinds = vec![
            AdjustmentKind::Levels {
                black: 0.1,
                white: 0.9,
                gamma: 1.4,
            },
            AdjustmentKind::Curves {
                points: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
            },
            AdjustmentKind::Exposure { stops: -0.75 },
            AdjustmentKind::HueSaturation {
                hue: 30.0,
                saturation: 0.25,
                lightness: -0.1,
            },
            AdjustmentKind::ColorBalance {
                shadows: [0.1, 0.0, -0.2],
                midtones: [0.0; 3],
                highlights: [0.0, 0.3, 0.0],
            },
        ];
        for kind in kinds {
            let adj = Adjustment::try_from_layer_kind(&kind).unwrap();
            assert_eq!(adj.to_layer_kind(), kind, "{kind:?}");
        }
    }

    /// Invert used to be the example of an adjustment with no stored form.
    /// It now has one, like every other adjustment and every other *setting* —
    /// see `the_widened_settings_round_trip_through_the_full_spellings`.
    #[test]
    fn the_parameterless_adjustments_have_a_stored_form() {
        assert_eq!(Adjustment::Invert.to_layer_kind(), AdjustmentKind::Invert);
        assert_eq!(
            Adjustment::try_from_layer_kind(&AdjustmentKind::Invert).unwrap(),
            Adjustment::Invert
        );
    }

    #[test]
    fn the_lenient_conversion_survives_a_corrupt_document() {
        // gamma <= 0 becomes 1.0 rather than a 100000 exponent.
        let bad = AdjustmentKind::Levels {
            black: 0.0,
            white: 1.0,
            gamma: -3.0,
        };
        assert_eq!(
            Adjustment::from(&bad),
            Adjustment::Levels(Levels::IDENTITY),
            "a non-positive gamma should degrade to neutral"
        );
        // white below black degrades to an identity instead of a 100000x step.
        let inverted = AdjustmentKind::Levels {
            black: 0.9,
            white: 0.1,
            gamma: 1.0,
        };
        assert_eq!(
            Adjustment::from(&inverted),
            Adjustment::Levels(Levels::IDENTITY)
        );
        // Duplicate curve points degrade to a merged curve, never to garbage.
        let dup = AdjustmentKind::Curves {
            points: vec![[0.5, 0.1], [0.5, 0.9]],
        };
        assert!(Adjustment::from(&dup).is_identity());
        // Out-of-range sliders are clamped, not rejected.
        let hot = AdjustmentKind::HueSaturation {
            hue: 45.0,
            saturation: 9.0,
            lightness: f32::NAN,
        };
        match Adjustment::from(&hot) {
            Adjustment::HueSaturation(h) => {
                assert_eq!(h.saturation(), 1.0);
                assert_eq!(h.lightness(), 0.0);
                assert_eq!(h.hue_degrees(), 45.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_strict_conversion_reports_what_is_wrong() {
        let bad = AdjustmentKind::Levels {
            black: 0.0,
            white: 1.0,
            gamma: 0.0,
        };
        assert!(matches!(
            Adjustment::try_from_layer_kind(&bad),
            Err(AdjustmentError::OutOfRange { name: "gamma", .. })
        ));
        let dup = AdjustmentKind::Curves {
            points: vec![[0.5, 0.1], [0.5, 0.9]],
        };
        assert_eq!(
            Adjustment::try_from_layer_kind(&dup),
            Err(AdjustmentError::TooFewCurvePoints { got: 1 })
        );
    }

    // --- auto -------------------------------------------------------------

    #[test]
    fn an_unresolved_auto_adjustment_is_the_identity() {
        for kind in [AutoKind::Tone, AutoKind::Contrast, AutoKind::Color] {
            let adj = Adjustment::Auto(AutoMode::new(kind, 0.001).unwrap());
            let prep = PreparedAdjustment::new(&adj);
            assert!(prep.is_identity(), "{kind:?} should need statistics");
            let px = LinearRgb([0.3, 0.4, 0.5]);
            assert_eq!(prep.apply(px, &SRGB), px);
        }
    }

    #[test]
    fn a_resolved_auto_adjustment_stretches_the_image() {
        // A low-contrast image occupying the encoded range 0.3..=0.6.
        let encoded: Vec<EncodedRgb> = (0..=60)
            .map(|i| EncodedRgb([0.3 + 0.005 * i as f32; 3]))
            .collect();
        let stats = ImageStats::from_encoded(&encoded);
        let prep = PreparedAdjustment::with_stats(&Adjustment::Auto(AutoMode::CONTRAST), &stats);
        assert!(!prep.is_identity());
        let dark = prep
            .apply(encoded[0].decode(&SRGB), &SRGB)
            .encode(&SRGB)
            .get();
        let light = prep
            .apply(encoded[60].decode(&SRGB), &SRGB)
            .encode(&SRGB)
            .get();
        assert!(dark[0] < 0.02, "{dark:?}");
        assert!(light[0] > 0.98, "{light:?}");
    }

    #[test]
    fn a_nan_mask_sample_leaves_the_pixel_untouched() {
        // `f32::clamp` propagates NaN instead of replacing it, so a NaN
        // coverage entry used to skip both fast paths and write NaN into the
        // tile — and one NaN pixel poisons every blend the compositor
        // subsequently performs on it.
        let prepared = PreparedAdjustment::new(&Adjustment::Invert);
        let before = [0.2f32, 0.5, 0.8, 1.0];
        let mut pixels = [before];
        prepared
            .apply_premultiplied_rgba_masked(&mut pixels, &[f32::NAN], &SRGB)
            .unwrap();
        assert_eq!(
            pixels[0], before,
            "a non-finite mask sample must mean no coverage, not NaN output"
        );
    }

    #[test]
    fn a_corrupt_posterize_level_degrades_to_the_near_identity() {
        // Both this conversion and `Posterize::FINEST` promise a corrupt stored
        // value degrades to something indistinguishable from the identity.
        // Clamping first turned `levels: 0` into a two-level posterise, the
        // most destructive output the control can produce.
        for bad in [0u32, 1, 100_000] {
            let a = Adjustment::from(&AdjustmentKind::Posterize { levels: bad });
            assert_eq!(
                a,
                Adjustment::Posterize(Posterize::FINEST),
                "levels: {bad} must degrade to FINEST"
            );
        }
        // A valid count is still honoured exactly.
        assert_eq!(
            Adjustment::from(&AdjustmentKind::Posterize { levels: 4 }),
            Adjustment::Posterize(Posterize::new(4).unwrap())
        );
    }
}
