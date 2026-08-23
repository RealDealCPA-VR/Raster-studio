//! Colour adjustments: vibrance, hue/saturation, colour balance, black &
//! white, photo filter, channel mixer, gradient map and selective colour.
//!
//! Two of these are **linear-light** operations and take a [`LinearRgb`]:
//! [`PhotoFilter`], because a filter absorbs light multiplicatively and
//! multiplying gamma-encoded values is not that; and [`GradientMap`], because
//! the position along its ramp is a *luminance* and the result it produces is
//! light. The rest take an [`EncodedRgb`], because their controls are defined
//! against the display ramp.
//!
//! Those two take **no [`ColorSpace`] argument at all**. That is not an
//! oversight: their own colour parameters are documented as sRGB, so the result
//! cannot depend on the document's colour space, and the signature is what
//! proves it. The encoded operations do receive the space, because "preserve
//! luminosity" has to measure luminance in linear light and needs to know how
//! to get there.

use color::{from_linear, hsl_to_rgb, linear_srgb_luminance, rgb_to_hsl, to_linear, ColorSpace};

use crate::error::{in_range, triple_in_range, AdjustmentError};
use crate::space::{clamp01, toward, EncodedRgb, LinearRgb};

/// Below this luminance a "preserve luminosity" rescale is skipped: the
/// correction factor is `y_in / y_out`, and near black that ratio is noise
/// amplified without bound.
const LUMINOSITY_FLOOR: f32 = 1e-4;

/// Rescale `linear` so that its Rec. 709 luminance matches `target`.
fn match_luminance(linear: [f32; 3], target: f32) -> [f32; 3] {
    let y = linear_srgb_luminance(linear);
    if y.abs() < LUMINOSITY_FLOOR || target.abs() < LUMINOSITY_FLOOR {
        return linear;
    }
    let k = target / y;
    [linear[0] * k, linear[1] * k, linear[2] * k]
}

/// Preserve the luminance of `before` in `after`, both **encoded** in `space`.
fn preserve_luminosity_encoded(
    before: EncodedRgb,
    after: EncodedRgb,
    space: &ColorSpace,
) -> EncodedRgb {
    let target = linear_srgb_luminance(to_linear(space, before.get()));
    let out = match_luminance(to_linear(space, after.get()), target);
    EncodedRgb(from_linear(space, out))
}

// ---------------------------------------------------------------------------
// Vibrance
// ---------------------------------------------------------------------------

/// Vibrance and saturation, on **gamma-encoded** values via HSL.
///
/// Vibrance is a saturation boost weighted by `1 - s`, so colours that are
/// already saturated move least — that is the entire point of the control, and
/// the reason it exists alongside a plain saturation slider.
///
/// `color::rgb_to_hsl` clamps its input into `0..=1` by design (HSL is a
/// reparameterisation of *encoded* RGB and is not defined outside it), so an
/// encoded value carrying a scene-referred highlight loses the excess here.
/// This is documented rather than worked around: the alternative is inventing a
/// meaning for "hue of a 300% red", and every choice there is arbitrary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vibrance {
    vibrance: f32,
    saturation: f32,
}

impl Vibrance {
    /// Neutral.
    pub const IDENTITY: Self = Self {
        vibrance: 0.0,
        saturation: 0.0,
    };

    /// Both in `-1.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(vibrance: f32, saturation: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            vibrance: in_range("vibrance", vibrance, -1.0, 1.0)?,
            saturation: in_range("saturation", saturation, -1.0, 1.0)?,
        })
    }

    /// The vibrance amount.
    pub fn vibrance(&self) -> f32 {
        self.vibrance
    }

    /// The flat saturation amount.
    pub fn saturation(&self) -> f32 {
        self.saturation
    }

    /// Whether this cannot change a pixel.
    pub fn is_identity(&self) -> bool {
        self.vibrance == 0.0 && self.saturation == 0.0
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        if self.is_identity() {
            return enc;
        }
        let hsl = rgb_to_hsl(enc.get());
        let amount = (self.saturation + self.vibrance * (1.0 - hsl[1])).clamp(-1.0, 1.0);
        EncodedRgb(hsl_to_rgb([hsl[0], toward(hsl[1], amount), hsl[2]]))
    }
}

// ---------------------------------------------------------------------------
// Hue / saturation / lightness
// ---------------------------------------------------------------------------

/// The colorize mode of [`HueSaturation`]: replace hue and saturation outright
/// and keep only the lightness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colorize {
    hue: f32,
    saturation: f32,
    lightness: f32,
}

impl Colorize {
    /// `hue` in degrees (wrapped into `0..360`), `saturation` in `0.0..=1.0`,
    /// `lightness` in `-1.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(hue: f32, saturation: f32, lightness: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            hue: in_range("hue", hue, -3600.0, 3600.0)?.rem_euclid(360.0),
            saturation: in_range("saturation", saturation, 0.0, 1.0)?,
            lightness: in_range("lightness", lightness, -1.0, 1.0)?,
        })
    }

    /// The target hue in degrees.
    pub fn hue(&self) -> f32 {
        self.hue
    }

    /// The target saturation.
    pub fn saturation(&self) -> f32 {
        self.saturation
    }

    /// The lightness shift.
    pub fn lightness(&self) -> f32 {
        self.lightness
    }
}

/// Hue rotation, saturation and lightness, on **gamma-encoded** values via HSL.
///
/// Hue rotation and lightness are the two controls the previous crate had no
/// implementation of at all — `saturation()` was the whole of it. Hue is in
/// degrees and wraps; saturation and lightness are `-1..=1` amounts that move
/// the channel toward its endpoint, reaching it exactly at `±1`.
///
/// Same HSL domain caveat as [`Vibrance`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HueSaturation {
    hue_degrees: f32,
    saturation: f32,
    lightness: f32,
    colorize: Option<Colorize>,
}

impl HueSaturation {
    /// Neutral.
    pub const IDENTITY: Self = Self {
        hue_degrees: 0.0,
        saturation: 0.0,
        lightness: 0.0,
        colorize: None,
    };

    /// `hue_degrees` in `-3600.0..=3600.0` (wrapped into `0..360`),
    /// `saturation` and `lightness` in `-1.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(hue_degrees: f32, saturation: f32, lightness: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            hue_degrees: in_range("hue_degrees", hue_degrees, -3600.0, 3600.0)?,
            saturation: in_range("saturation", saturation, -1.0, 1.0)?,
            lightness: in_range("lightness", lightness, -1.0, 1.0)?,
            colorize: None,
        })
    }

    /// Switch to colorize mode.
    pub fn colorized(colorize: Colorize) -> Self {
        Self {
            hue_degrees: 0.0,
            saturation: 0.0,
            lightness: 0.0,
            colorize: Some(colorize),
        }
    }

    /// The hue rotation in degrees.
    pub fn hue_degrees(&self) -> f32 {
        self.hue_degrees
    }

    /// The saturation amount.
    pub fn saturation(&self) -> f32 {
        self.saturation
    }

    /// The lightness amount.
    pub fn lightness(&self) -> f32 {
        self.lightness
    }

    /// The colorize settings, if in colorize mode.
    pub fn colorize(&self) -> Option<Colorize> {
        self.colorize
    }

    /// Whether this cannot change a pixel.
    pub fn is_identity(&self) -> bool {
        self.colorize.is_none()
            && self.hue_degrees.rem_euclid(360.0) == 0.0
            && self.saturation == 0.0
            && self.lightness == 0.0
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        if self.is_identity() {
            return enc;
        }
        let hsl = rgb_to_hsl(enc.get());
        let out = match self.colorize {
            Some(c) => hsl_to_rgb([c.hue, c.saturation, toward(hsl[2], c.lightness)]),
            None => hsl_to_rgb([
                (hsl[0] + self.hue_degrees).rem_euclid(360.0),
                toward(hsl[1], self.saturation),
                toward(hsl[2], self.lightness),
            ]),
        };
        EncodedRgb(out)
    }
}

// ---------------------------------------------------------------------------
// Colour balance
// ---------------------------------------------------------------------------

/// Per-band colour shifts, on **gamma-encoded** values.
///
/// Each band holds a `[cyan..red, magenta..green, yellow..blue]` triple in
/// `-1.0..=1.0`, and each channel is weighted by where *that channel's* value
/// sits on the ramp. The three band weights are a partition of unity —
/// `shadows = max(0, 1 - 2v)`, `highlights = max(0, 2v - 1)`,
/// `midtones = 1 - |2v - 1|` — so a shift applied identically to all three
/// bands is a flat offset with no banding at the joins.
///
/// With `preserve_luminosity` the result is rescaled in **linear light** to the
/// input's Rec. 709 luminance. Doing that on encoded values, which is the
/// cheaper and more common shortcut, gets the correction wrong by the amount
/// the transfer curve bends — which is most of it.
///
/// **The pixel is not clamped.** Only the value the band weights are *read at*
/// is clamped, because the three weights are defined as a partition of the
/// `0..=1` ramp and there is nothing above the top of it: an encoded value over
/// `1.0` is entirely in the highlight band, which is the sensible reading.
/// The shift is then added to the original, unclamped value, so two highlights
/// that differ before this adjustment still differ after it and a later
/// exposure can pull both back. See
/// `highlights_stay_distinct_through_color_balance`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorBalance {
    shadows: [f32; 3],
    midtones: [f32; 3],
    highlights: [f32; 3],
    preserve_luminosity: bool,
}

impl ColorBalance {
    /// Neutral.
    pub const IDENTITY: Self = Self {
        shadows: [0.0; 3],
        midtones: [0.0; 3],
        highlights: [0.0; 3],
        preserve_luminosity: false,
    };

    /// All nine amounts in `-1.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
    ) -> Result<Self, AdjustmentError> {
        Ok(Self {
            shadows: triple_in_range("shadows", shadows, -1.0, 1.0)?,
            midtones: triple_in_range("midtones", midtones, -1.0, 1.0)?,
            highlights: triple_in_range("highlights", highlights, -1.0, 1.0)?,
            preserve_luminosity: false,
        })
    }

    /// Turn luminosity preservation on or off.
    pub fn with_preserve_luminosity(self, preserve: bool) -> Self {
        Self {
            preserve_luminosity: preserve,
            ..self
        }
    }

    /// The shadow band amounts.
    pub fn shadows(&self) -> [f32; 3] {
        self.shadows
    }

    /// The midtone band amounts.
    pub fn midtones(&self) -> [f32; 3] {
        self.midtones
    }

    /// The highlight band amounts.
    pub fn highlights(&self) -> [f32; 3] {
        self.highlights
    }

    /// Whether luminance is held constant.
    pub fn preserve_luminosity(&self) -> bool {
        self.preserve_luminosity
    }

    /// Whether this cannot change a pixel.
    pub fn is_identity(&self) -> bool {
        self.shadows == [0.0; 3] && self.midtones == [0.0; 3] && self.highlights == [0.0; 3]
    }

    /// The `(shadow, midtone, highlight)` weights at encoded value `v`. They
    /// sum to exactly one for every `v` in `0..=1`.
    pub fn band_weights(v: f32) -> (f32, f32, f32) {
        let v = clamp01(v);
        let shadow = (1.0 - 2.0 * v).max(0.0);
        let highlight = (2.0 * v - 1.0).max(0.0);
        (shadow, 1.0 - shadow - highlight, highlight)
    }

    /// Apply to one encoded triple.
    ///
    /// The value itself is **not** clamped; only [`band_weights`] clamps, and
    /// only because it is asked where on a bounded ramp a value sits.
    ///
    /// [`band_weights`]: ColorBalance::band_weights
    pub fn apply(&self, enc: EncodedRgb, space: &ColorSpace) -> EncodedRgb {
        if self.is_identity() {
            return enc;
        }
        let mut out = [0.0f32; 3];
        for (c, o) in out.iter_mut().enumerate() {
            let v = enc.get()[c];
            let (s, m, h) = Self::band_weights(v);
            *o = v + s * self.shadows[c] + m * self.midtones[c] + h * self.highlights[c];
        }
        let out = EncodedRgb(out);
        if self.preserve_luminosity {
            preserve_luminosity_encoded(enc, out, space)
        } else {
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Black & white
// ---------------------------------------------------------------------------

/// Photoshop's default Black & White weights, in
/// `[red, yellow, green, cyan, blue, magenta]` order.
pub const BW_DEFAULT_WEIGHTS: [f32; 6] = [0.4, 0.6, 0.4, 0.6, 0.2, 0.8];

/// An optional colour cast applied to the gray produced by [`BlackAndWhite`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BwTint {
    hue: f32,
    saturation: f32,
}

impl BwTint {
    /// `hue` in degrees, `saturation` in `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(hue: f32, saturation: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            hue: in_range("hue", hue, -3600.0, 3600.0)?.rem_euclid(360.0),
            saturation: in_range("saturation", saturation, 0.0, 1.0)?,
        })
    }

    /// The tint hue in degrees.
    pub fn hue(&self) -> f32 {
        self.hue
    }

    /// The tint saturation.
    pub fn saturation(&self) -> f32 {
        self.saturation
    }
}

/// Black & White: six per-colour weights and an optional tint, on
/// **gamma-encoded** values.
///
/// The pixel is decomposed the way the control implies: every RGB colour is
/// `min·white + (mid - min)·secondary + (max - mid)·primary`, where the primary
/// is whichever of R/G/B is largest and the secondary is the mix of the two
/// largest. Each part is then weighted by its slider. With all six weights at
/// `1.0` the result is `max`, i.e. the HSV value; with the
/// [defaults](BW_DEFAULT_WEIGHTS) it is a conventional panchromatic gray.
///
/// That decomposition is a weighted sum of differences, which is meaningful at
/// any magnitude, so [`gray`](Self::gray) does **not** clamp: a scene-referred
/// highlight converts to a scene-referred gray and survives for a later
/// adjustment to pull back (`highlights_stay_distinct_through_black_and_white`).
/// The one place this type does clamp is the optional [`BwTint`], because it
/// goes through HSL, whose lightness axis is `0..=1` by construction — an
/// untinted Black & White has no bound at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlackAndWhite {
    weights: [f32; 6],
    tint: Option<BwTint>,
}

impl BlackAndWhite {
    /// The default panchromatic weights, untinted.
    pub const DEFAULT: Self = Self {
        weights: BW_DEFAULT_WEIGHTS,
        tint: None,
    };

    /// Six weights in `[red, yellow, green, cyan, blue, magenta]` order, each
    /// in `-3.0..=3.0` (the control allows well over 100%, and negatives are
    /// how a colour is driven to black).
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(weights: [f32; 6]) -> Result<Self, AdjustmentError> {
        for w in weights {
            in_range("weight", w, -3.0, 3.0)?;
        }
        Ok(Self {
            weights,
            tint: None,
        })
    }

    /// Add or remove the tint.
    pub fn with_tint(self, tint: Option<BwTint>) -> Self {
        Self { tint, ..self }
    }

    /// The six weights.
    pub fn weights(&self) -> [f32; 6] {
        self.weights
    }

    /// The tint, if any.
    pub fn tint(&self) -> Option<BwTint> {
        self.tint
    }

    /// The gray this adjustment produces for one encoded triple, before any
    /// tint. Unbounded: an encoded value above `1.0` produces a gray above
    /// `1.0`.
    pub fn gray(&self, enc: EncodedRgb) -> f32 {
        let [r, g, b] = enc.get();
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        // `r + g + b - max - min` lands an ulp outside `[min, max]` on a
        // neutral pixel; clamping keeps both weighted parts non-negative.
        let mid = (r + g + b - max - min).clamp(min, max);
        // Index order: 0 red, 1 yellow, 2 green, 3 cyan, 4 blue, 5 magenta.
        let (primary, secondary) = if r >= g && g >= b {
            (0, 1)
        } else if g >= r && r >= b {
            (2, 1)
        } else if g >= b && b >= r {
            (2, 3)
        } else if b >= g && g >= r {
            (4, 3)
        } else if b >= r && r >= g {
            (4, 5)
        } else {
            (0, 5)
        };
        min + (mid - min) * self.weights[secondary] + (max - mid) * self.weights[primary]
    }

    /// Apply to one encoded triple.
    ///
    /// Untinted the result is unbounded. **Tinted it is clamped to `0..=1`**,
    /// because the tint is applied through HSL and HSL's lightness axis does
    /// not extend past white.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        let gray = self.gray(enc);
        match self.tint {
            None => EncodedRgb([gray; 3]),
            Some(t) => EncodedRgb(hsl_to_rgb([t.hue, t.saturation, clamp01(gray)])),
        }
    }
}

// ---------------------------------------------------------------------------
// Photo filter
// ---------------------------------------------------------------------------

/// A photographic filter over the lens, in **linear light**.
///
/// The filter colour is given as an **sRGB-encoded** triple — that is how
/// filters are named and picked — and is decoded once, so this adjustment's
/// result does not depend on the document's colour space. Density interpolates
/// between white (no filter) and the filter colour, and the whole thing is a
/// per-channel multiply of light, which is what a piece of coloured glass
/// actually does.
///
/// With `preserve_luminosity` the result is rescaled back to the input's
/// Rec. 709 luminance, so the filter changes colour without darkening the
/// image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhotoFilter {
    color_srgb: [f32; 3],
    density: f32,
    preserve_luminosity: bool,
}

impl PhotoFilter {
    /// Warming Filter (85), sRGB `#EC8A00`.
    pub const WARMING_85: [f32; 3] = [0.925, 0.541, 0.0];
    /// Warming Filter (81), sRGB `#EBB113`.
    pub const WARMING_81: [f32; 3] = [0.922, 0.694, 0.075];
    /// Cooling Filter (80), sRGB `#006DFF`.
    pub const COOLING_80: [f32; 3] = [0.0, 0.427, 1.0];
    /// Cooling Filter (82), sRGB `#00B5FF`.
    pub const COOLING_82: [f32; 3] = [0.0, 0.710, 1.0];
    /// Sepia, sRGB `#AC7A33`.
    pub const SEPIA: [f32; 3] = [0.675, 0.478, 0.200];

    /// No filter at all: a clear lens at zero density, and therefore an
    /// identity. This is what a corrupt stored filter degrades to.
    pub const NONE: Self = Self {
        color_srgb: [1.0; 3],
        density: 0.0,
        preserve_luminosity: true,
    };

    /// `color_srgb` channels in `0.0..=1.0`, `density` in `0.0..=1.0`.
    /// Luminosity preservation is on by default, matching the control.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(color_srgb: [f32; 3], density: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            color_srgb: triple_in_range("color_srgb", color_srgb, 0.0, 1.0)?,
            density: in_range("density", density, 0.0, 1.0)?,
            preserve_luminosity: true,
        })
    }

    /// Turn luminosity preservation on or off.
    pub fn with_preserve_luminosity(self, preserve: bool) -> Self {
        Self {
            preserve_luminosity: preserve,
            ..self
        }
    }

    /// The filter colour, sRGB-encoded.
    pub fn color_srgb(&self) -> [f32; 3] {
        self.color_srgb
    }

    /// The filter density.
    pub fn density(&self) -> f32 {
        self.density
    }

    /// Whether luminance is held constant.
    pub fn preserve_luminosity(&self) -> bool {
        self.preserve_luminosity
    }

    /// Whether this cannot change a pixel.
    pub fn is_identity(&self) -> bool {
        self.density == 0.0
    }

    /// The per-channel linear multiplier, computed once per layer rather than
    /// once per pixel.
    pub fn multiplier(&self) -> [f32; 3] {
        let filter = to_linear(&ColorSpace::Srgb, self.color_srgb);
        let d = self.density;
        [
            1.0 + (filter[0] - 1.0) * d,
            1.0 + (filter[1] - 1.0) * d,
            1.0 + (filter[2] - 1.0) * d,
        ]
    }

    /// Apply to one linear triple, using a precomputed [`multiplier`].
    ///
    /// [`multiplier`]: PhotoFilter::multiplier
    pub fn apply_with(&self, linear: LinearRgb, multiplier: [f32; 3]) -> LinearRgb {
        if self.is_identity() {
            return linear;
        }
        let v = linear.get();
        let out = [
            v[0] * multiplier[0],
            v[1] * multiplier[1],
            v[2] * multiplier[2],
        ];
        if self.preserve_luminosity {
            LinearRgb(match_luminance(out, linear_srgb_luminance(v)))
        } else {
            LinearRgb(out)
        }
    }

    /// Apply to one linear triple.
    pub fn apply(&self, linear: LinearRgb) -> LinearRgb {
        self.apply_with(linear, self.multiplier())
    }
}

// ---------------------------------------------------------------------------
// Channel mixer
// ---------------------------------------------------------------------------

/// Channel mixer, on **gamma-encoded** values.
///
/// `rows[out]` is `[red, green, blue, constant]`: the output channel is a
/// weighted sum of the three input channels plus an offset. In `monochrome`
/// mode `rows[0]` alone drives all three outputs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelMixer {
    rows: [[f32; 4]; 3],
    monochrome: bool,
}

impl ChannelMixer {
    /// Pass-through.
    pub const IDENTITY: Self = Self {
        rows: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
        monochrome: false,
    };

    /// Weights in `-2.0..=2.0` and constants in `-1.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(rows: [[f32; 4]; 3]) -> Result<Self, AdjustmentError> {
        for row in rows {
            for w in &row[..3] {
                in_range("mixer weight", *w, -2.0, 2.0)?;
            }
            in_range("mixer constant", row[3], -1.0, 1.0)?;
        }
        Ok(Self {
            rows,
            monochrome: false,
        })
    }

    /// Drive all three outputs from `rows[0]`.
    pub fn monochrome(self, monochrome: bool) -> Self {
        Self { monochrome, ..self }
    }

    /// The mixing rows.
    pub fn rows(&self) -> [[f32; 4]; 3] {
        self.rows
    }

    /// Whether all three outputs come from `rows[0]`.
    pub fn is_monochrome(&self) -> bool {
        self.monochrome
    }

    /// Whether this cannot change a pixel.
    pub fn is_identity(&self) -> bool {
        !self.monochrome && self.rows == Self::IDENTITY.rows
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        if self.is_identity() {
            return enc;
        }
        let v = enc.get();
        let mix = |row: &[f32; 4]| row[0] * v[0] + row[1] * v[1] + row[2] * v[2] + row[3];
        if self.monochrome {
            EncodedRgb([mix(&self.rows[0]); 3])
        } else {
            EncodedRgb([mix(&self.rows[0]), mix(&self.rows[1]), mix(&self.rows[2])])
        }
    }
}

// ---------------------------------------------------------------------------
// Gradient map
// ---------------------------------------------------------------------------

/// One stop of a [`GradientMap`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    position: f32,
    color_srgb: [f32; 3],
}

impl GradientStop {
    /// `position` in `0.0..=1.0`, `color_srgb` channels in `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(position: f32, color_srgb: [f32; 3]) -> Result<Self, AdjustmentError> {
        Ok(Self {
            position: in_range("stop position", position, 0.0, 1.0)?,
            color_srgb: triple_in_range("stop color", color_srgb, 0.0, 1.0)?,
        })
    }

    /// The stop's position along the ramp.
    pub fn position(&self) -> f32 {
        self.position
    }

    /// The stop's sRGB-encoded colour.
    pub fn color_srgb(&self) -> [f32; 3] {
        self.color_srgb
    }
}

/// Gradient map: replace every pixel with the gradient colour at its lightness.
///
/// Two deliberate choices:
///
/// * The position along the ramp is the pixel's Rec. 709 **luminance**,
///   re-encoded through the sRGB curve so that a mid-gray pixel lands at the
///   middle of the gradient rather than at 21% of it. Using raw linear
///   luminance would bunch almost every pixel into the dark end.
/// * The stop colours are interpolated in the **sRGB-encoded** domain, and only
///   the interpolated result is decoded to linear light. A gradient is authored
///   and displayed as an encoded ramp, so that is where "halfway between these
///   two stops" is defined; interpolating the same two stops as light gives a
///   visibly different, lighter ramp. The concrete test of it is
///   `black_to_white_gradient_map_is_a_luminance_preserving_desaturate`: with
///   an encoded ramp a black-to-white map returns each pixel's own luminance,
///   which is what the operation is supposed to mean, and with a linear ramp it
///   returns the *encoded* gray as if it were light and brightens the whole
///   image.
///
/// Both the position and the stop colours are defined against sRGB regardless
/// of the document space, so this adjustment takes no [`ColorSpace`] and its
/// output cannot depend on one.
#[derive(Debug, Clone, PartialEq)]
pub struct GradientMap {
    stops: Vec<GradientStop>,
    reverse: bool,
}

impl GradientMap {
    /// Build from stops. They are sorted by position, and stops sharing a
    /// position are merged by averaging their colours.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::TooFewGradientStops`] if fewer than two distinct
    /// positions remain.
    pub fn new(stops: &[GradientStop]) -> Result<Self, AdjustmentError> {
        let mut sorted = stops.to_vec();
        sorted.sort_by(|a, b| a.position.total_cmp(&b.position));

        let mut merged: Vec<GradientStop> = Vec::with_capacity(sorted.len());
        let mut i = 0;
        while i < sorted.len() {
            let p = sorted[i].position;
            let mut sum = [0.0f64; 3];
            let mut n = 0u32;
            while i < sorted.len() && sorted[i].position == p {
                for (s, c) in sum.iter_mut().zip(sorted[i].color_srgb) {
                    *s += f64::from(c);
                }
                n += 1;
                i += 1;
            }
            merged.push(GradientStop {
                position: p,
                color_srgb: sum.map(|s| (s / f64::from(n)) as f32),
            });
        }

        if merged.len() < 2 {
            return Err(AdjustmentError::TooFewGradientStops { got: merged.len() });
        }
        Ok(Self {
            stops: merged,
            reverse: false,
        })
    }

    /// A two-stop black-to-white ramp.
    pub fn black_to_white() -> Self {
        Self {
            stops: vec![
                GradientStop {
                    position: 0.0,
                    color_srgb: [0.0; 3],
                },
                GradientStop {
                    position: 1.0,
                    color_srgb: [1.0; 3],
                },
            ],
            reverse: false,
        }
    }

    /// Walk the ramp from the light end to the dark end.
    pub fn reversed(mut self, reverse: bool) -> Self {
        self.reverse = reverse;
        self
    }

    /// The merged, sorted stops.
    pub fn stops(&self) -> &[GradientStop] {
        &self.stops
    }

    /// Whether the ramp is walked backwards.
    pub fn is_reversed(&self) -> bool {
        self.reverse
    }

    /// The ramp as `(position, sRGB-encoded colour)` pairs, built once per
    /// layer instead of once per pixel.
    pub fn ramp(&self) -> Vec<(f32, [f32; 3])> {
        self.stops
            .iter()
            .map(|s| (s.position, s.color_srgb))
            .collect()
    }

    /// The sRGB-encoded gradient colour at `t`, without any pixel involved.
    fn sample(ramp: &[(f32, [f32; 3])], t: f32) -> [f32; 3] {
        if t <= ramp[0].0 {
            return ramp[0].1;
        }
        let last = ramp.len() - 1;
        if t >= ramp[last].0 {
            return ramp[last].1;
        }
        let hi = ramp.partition_point(|s| s.0 <= t);
        let (p0, c0) = ramp[hi - 1];
        let (p1, c1) = ramp[hi];
        let f = (t - p0) / (p1 - p0);
        [
            c0[0] + (c1[0] - c0[0]) * f,
            c0[1] + (c1[1] - c0[1]) * f,
            c0[2] + (c1[2] - c0[2]) * f,
        ]
    }

    /// Apply to one linear triple using a precomputed [`ramp`].
    ///
    /// [`ramp`]: GradientMap::ramp
    pub fn apply_with(&self, linear: LinearRgb, ramp: &[(f32, [f32; 3])]) -> LinearRgb {
        let mut t = clamp01(color::linear_to_srgb(clamp01(linear.luminance())));
        if self.reverse {
            t = 1.0 - t;
        }
        LinearRgb(to_linear(&ColorSpace::Srgb, Self::sample(ramp, t)))
    }

    /// Apply to one linear triple.
    pub fn apply(&self, linear: LinearRgb) -> LinearRgb {
        self.apply_with(linear, &self.ramp())
    }
}

// ---------------------------------------------------------------------------
// Selective colour
// ---------------------------------------------------------------------------

/// The nine ranges a [`SelectiveColor`] adjustment can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorRange {
    /// Red primaries.
    Reds,
    /// Yellow secondaries.
    Yellows,
    /// Green primaries.
    Greens,
    /// Cyan secondaries.
    Cyans,
    /// Blue primaries.
    Blues,
    /// Magenta secondaries.
    Magentas,
    /// Near-white pixels.
    Whites,
    /// Everything that is neither near-white nor near-black.
    Neutrals,
    /// Near-black pixels.
    Blacks,
}

impl ColorRange {
    /// All nine ranges, in index order.
    pub const ALL: [ColorRange; 9] = [
        ColorRange::Reds,
        ColorRange::Yellows,
        ColorRange::Greens,
        ColorRange::Cyans,
        ColorRange::Blues,
        ColorRange::Magentas,
        ColorRange::Whites,
        ColorRange::Neutrals,
        ColorRange::Blacks,
    ];

    /// Index into [`SelectiveColor`]'s table.
    pub fn index(self) -> usize {
        match self {
            ColorRange::Reds => 0,
            ColorRange::Yellows => 1,
            ColorRange::Greens => 2,
            ColorRange::Cyans => 3,
            ColorRange::Blues => 4,
            ColorRange::Magentas => 5,
            ColorRange::Whites => 6,
            ColorRange::Neutrals => 7,
            ColorRange::Blacks => 8,
        }
    }
}

/// Selective colour: per-range CMYK shifts, on **gamma-encoded** values.
///
/// Each range carries `[cyan, magenta, yellow, black]` deltas in `-1.0..=1.0`.
/// The pixel is converted to CMYK (`k = 1 - max`, the standard
/// maximum-black separation), the *weighted sum* of every range's deltas is
/// applied once, and it is converted back. Applying the sum once rather than
/// each range in turn is exact, not an approximation: both the relative and the
/// absolute rules are linear in the deltas.
///
/// Range membership is a genuine partition of unity, which is what keeps a
/// shift applied to every range from double-counting. The chromatic ranges take
/// the pixel's chroma, split between one primary (`max - mid`) and one
/// secondary (`mid - min`), exactly as [`BlackAndWhite`] decomposes it; the
/// achromatic ranges share what is left, `1 - chroma`, as
/// `whites = (1 - chroma)·max(0, 2·min - 1)`,
/// `blacks = (1 - chroma)·max(0, 1 - 2·max)` and `neutrals` the remainder.
///
/// Both the weights and the separation are read from the pixel clamped into
/// `0..=1`, because an ink coverage outside that range is not a coverage. The
/// *result* is not clamped: see [`apply`](Self::apply) for how a scene-referred
/// highlight is carried around the ink stage instead of being flattened onto
/// white.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectiveColor {
    ranges: [[f32; 4]; 9],
    relative: bool,
}

impl SelectiveColor {
    /// Neutral, in relative mode.
    pub const IDENTITY: Self = Self {
        ranges: [[0.0; 4]; 9],
        relative: true,
    };

    /// All 36 deltas in `-1.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(ranges: [[f32; 4]; 9]) -> Result<Self, AdjustmentError> {
        for range in ranges {
            for d in range {
                in_range("cmyk delta", d, -1.0, 1.0)?;
            }
        }
        Ok(Self {
            ranges,
            relative: true,
        })
    }

    /// Set one range's `[cyan, magenta, yellow, black]` deltas.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn with_range(
        mut self,
        range: ColorRange,
        cmyk: [f32; 4],
    ) -> Result<Self, AdjustmentError> {
        for d in cmyk {
            in_range("cmyk delta", d, -1.0, 1.0)?;
        }
        self.ranges[range.index()] = cmyk;
        Ok(self)
    }

    /// `true` for relative mode (deltas scale the amount of ink already
    /// present), `false` for absolute (deltas are added outright).
    pub fn relative(self, relative: bool) -> Self {
        Self { relative, ..self }
    }

    /// Whether the adjustment is in relative mode.
    pub fn is_relative(&self) -> bool {
        self.relative
    }

    /// One range's deltas.
    pub fn range(&self, range: ColorRange) -> [f32; 4] {
        self.ranges[range.index()]
    }

    /// All nine ranges' deltas, in [`ColorRange::ALL`] order.
    pub fn ranges(&self) -> [[f32; 4]; 9] {
        self.ranges
    }

    /// Whether this cannot change a pixel.
    pub fn is_identity(&self) -> bool {
        self.ranges.iter().all(|r| r.iter().all(|d| *d == 0.0))
    }

    /// The nine range weights for an encoded triple, in [`ColorRange::ALL`]
    /// order. They are non-negative and sum to exactly one.
    pub fn range_weights(enc: EncodedRgb) -> [f32; 9] {
        let [r, g, b] = enc.get().map(clamp01);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let mid = (r + g + b - max - min).clamp(min, max);
        let (primary, secondary) = if r >= g && g >= b {
            (0, 1)
        } else if g >= r && r >= b {
            (2, 1)
        } else if g >= b && b >= r {
            (2, 3)
        } else if b >= g && g >= r {
            (4, 3)
        } else if b >= r && r >= g {
            (4, 5)
        } else {
            (0, 5)
        };
        let mut w = [0.0f32; 9];
        // Split the chroma between the secondary and the primary. Deriving the
        // primary's share by subtraction rather than as `max - mid` keeps the
        // two exactly summing to the chroma: on a neutral pixel `mid` is
        // computed as `r + g + b - max - min` and lands an ulp either side of
        // `max`, which produced a weight of `-1.19e-7` and broke the partition.
        let chroma = max - min;
        let secondary_w = (mid - min).clamp(0.0, chroma);
        w[secondary] += secondary_w;
        w[primary] += chroma - secondary_w;
        // Whatever chroma does not claim is achromatic and is shared by the
        // whites / neutrals / blacks ranges.
        let achromatic = 1.0 - chroma;
        let whites = achromatic * (2.0 * min - 1.0).max(0.0);
        let blacks = achromatic * (1.0 - 2.0 * max).max(0.0);
        w[6] = whites;
        w[8] = blacks;
        w[7] = achromatic - whites - blacks;
        w
    }

    /// Apply to one encoded triple.
    ///
    /// The ink model is the only part of this that is bounded, and it is
    /// bounded by *definition*: an ink coverage outside `0..=1` is not a
    /// coverage, and `k = 1 - max` has no reading above white. So the
    /// separation runs on the display-range part of the pixel and whatever a
    /// scene-referred highlight carries **above** that range is added back
    /// afterwards rather than thrown away. A pixel already inside `0..=1` takes
    /// this path with an excess of exactly zero and is therefore bit-identical
    /// to a plain separation — `selective_color_round_trips_rgb_through_cmyk_on_the_slow_path`
    /// pins that, and `highlights_stay_distinct_through_selective_color` pins
    /// the other half.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        if self.is_identity() {
            return enc;
        }
        let w = Self::range_weights(enc);
        let mut delta = [0.0f32; 4];
        for (r, weight) in self.ranges.iter().zip(w) {
            for (d, dr) in delta.iter_mut().zip(r) {
                *d += weight * dr;
            }
        }

        let raw = enc.get();
        let base = raw.map(clamp01);
        let [r, g, b] = base;
        let k = 1.0 - r.max(g).max(b);
        let inv = 1.0 - k;
        let mut cmyk = if inv > 0.0 {
            [
                (1.0 - r - k) / inv,
                (1.0 - g - k) / inv,
                (1.0 - b - k) / inv,
                k,
            ]
        } else {
            [0.0, 0.0, 0.0, k]
        };

        for (v, d) in cmyk.iter_mut().zip(delta) {
            let shifted = if self.relative { *v + *v * d } else { *v + d };
            *v = clamp01(shifted);
        }

        let inv_k = 1.0 - cmyk[3];
        let inked = [
            (1.0 - cmyk[0]) * inv_k,
            (1.0 - cmyk[1]) * inv_k,
            (1.0 - cmyk[2]) * inv_k,
        ];
        // `excess == 0.0` for every in-gamut channel, and adding it would not
        // be bit-exact, so the in-gamut path returns `inked` untouched.
        EncodedRgb(std::array::from_fn(|c| {
            let excess = raw[c] - base[c];
            if excess == 0.0 {
                inked[c]
            } else {
                inked[c] + excess
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB: ColorSpace = ColorSpace::Srgb;

    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() <= eps)
    }

    // --- vibrance ---------------------------------------------------------

    #[test]
    fn vibrance_identity_is_bit_exact() {
        let px = EncodedRgb([0.2, 0.55, 0.8]);
        assert_eq!(Vibrance::IDENTITY.apply(px), px);
    }

    #[test]
    fn vibrance_boosts_the_dull_colour_more_than_the_vivid_one() {
        let v = Vibrance::new(0.5, 0.0).unwrap();
        // Same hue and lightness, different starting saturation.
        let dull = EncodedRgb(hsl_to_rgb([210.0, 0.2, 0.5]));
        let vivid = EncodedRgb(hsl_to_rgb([210.0, 0.9, 0.5]));
        let dull_gain = rgb_to_hsl(v.apply(dull).get())[1] - 0.2;
        let vivid_gain = rgb_to_hsl(v.apply(vivid).get())[1] - 0.9;
        assert!(dull_gain > 0.0 && vivid_gain > 0.0);
        assert!(
            dull_gain > vivid_gain * 3.0,
            "dull {dull_gain} vs vivid {vivid_gain}"
        );
    }

    #[test]
    fn vibrance_saturation_at_minus_one_is_gray() {
        let v = Vibrance::new(0.0, -1.0).unwrap();
        let out = v.apply(EncodedRgb([0.2, 0.5, 0.9])).get();
        assert!(close(out, [out[0]; 3], 1e-6), "{out:?}");
    }

    // --- hue / saturation -------------------------------------------------

    #[test]
    fn hue_saturation_identity_is_bit_exact() {
        let px = EncodedRgb([0.2, 0.55, 0.8]);
        assert_eq!(HueSaturation::IDENTITY.apply(px), px);
        // A full turn is also a no-op.
        assert_eq!(HueSaturation::new(360.0, 0.0, 0.0).unwrap().apply(px), px);
    }

    #[test]
    fn hue_rotation_moves_red_to_green_and_back() {
        let red = EncodedRgb([1.0, 0.0, 0.0]);
        let plus120 = HueSaturation::new(120.0, 0.0, 0.0).unwrap();
        let out = plus120.apply(red).get();
        assert!(close(out, [0.0, 1.0, 0.0], 1e-4), "{out:?}");
        // Three rotations of 120 degrees come back to red.
        let thrice = plus120.apply(plus120.apply(plus120.apply(red))).get();
        assert!(close(thrice, [1.0, 0.0, 0.0], 1e-4), "{thrice:?}");
        // Negative rotation is the inverse.
        let minus120 = HueSaturation::new(-120.0, 0.0, 0.0).unwrap();
        let back = minus120.apply(plus120.apply(red)).get();
        assert!(close(back, [1.0, 0.0, 0.0], 1e-4), "{back:?}");
    }

    #[test]
    fn lightness_reaches_white_and_black() {
        let px = EncodedRgb([0.2, 0.5, 0.9]);
        let up = HueSaturation::new(0.0, 0.0, 1.0).unwrap();
        assert!(close(up.apply(px).get(), [1.0; 3], 1e-5));
        let down = HueSaturation::new(0.0, 0.0, -1.0).unwrap();
        assert!(close(down.apply(px).get(), [0.0; 3], 1e-5));
    }

    #[test]
    fn colorize_replaces_hue_and_saturation() {
        let px = EncodedRgb([0.8, 0.2, 0.2]);
        let c = HueSaturation::colorized(Colorize::new(210.0, 0.5, 0.0).unwrap());
        let out = c.apply(px).get();
        let hsl = rgb_to_hsl(out);
        assert!((hsl[0] - 210.0).abs() < 0.5, "{hsl:?}");
        assert!((hsl[1] - 0.5).abs() < 1e-3, "{hsl:?}");
        // Lightness of the source is kept.
        assert!((hsl[2] - rgb_to_hsl(px.get())[2]).abs() < 1e-5, "{hsl:?}");
        assert!(!c.is_identity());
    }

    #[test]
    fn hue_saturation_rejects_out_of_range() {
        assert!(matches!(
            HueSaturation::new(0.0, 3.0, 0.0),
            Err(AdjustmentError::OutOfRange {
                name: "saturation",
                ..
            })
        ));
        assert!(matches!(
            Colorize::new(0.0, -0.5, 0.0),
            Err(AdjustmentError::OutOfRange {
                name: "saturation",
                ..
            })
        ));
    }

    // --- colour balance ---------------------------------------------------

    #[test]
    fn color_balance_identity_is_bit_exact() {
        let px = EncodedRgb([0.2, 0.55, 0.8]);
        assert_eq!(ColorBalance::IDENTITY.apply(px, &SRGB), px);
    }

    #[test]
    fn color_balance_band_weights_partition_unity() {
        for i in 0..=100 {
            let v = i as f32 / 100.0;
            let (s, m, h) = ColorBalance::band_weights(v);
            assert!(s >= 0.0 && m >= 0.0 && h >= 0.0, "{v}: {s} {m} {h}");
            assert!((s + m + h - 1.0).abs() < 1e-6, "{v}: {s} {m} {h}");
        }
        assert_eq!(ColorBalance::band_weights(0.0), (1.0, 0.0, 0.0));
        assert_eq!(ColorBalance::band_weights(0.5), (0.0, 1.0, 0.0));
        assert_eq!(ColorBalance::band_weights(1.0), (0.0, 0.0, 1.0));
    }

    #[test]
    fn color_balance_hits_the_band_it_targets() {
        // Highlight weight is 0.5 at v = 0.75 and 0 at v = 0.25.
        let hi = ColorBalance::new([0.0; 3], [0.0; 3], [0.4, 0.0, 0.0]).unwrap();
        let bright = hi.apply(EncodedRgb([0.75; 3]), &SRGB).get();
        assert!((bright[0] - 0.95).abs() < 1e-6, "{bright:?}");
        assert!((bright[1] - 0.75).abs() < 1e-6, "{bright:?}");
        let dark = hi.apply(EncodedRgb([0.25; 3]), &SRGB).get();
        assert!((dark[0] - 0.25).abs() < 1e-6, "{dark:?}");

        // Shadow weight is 0.5 at v = 0.25 and 0 at v = 0.75.
        let sh = ColorBalance::new([0.4, 0.0, 0.0], [0.0; 3], [0.0; 3]).unwrap();
        let dark = sh.apply(EncodedRgb([0.25; 3]), &SRGB).get();
        assert!((dark[0] - 0.45).abs() < 1e-6, "{dark:?}");
        let bright = sh.apply(EncodedRgb([0.75; 3]), &SRGB).get();
        assert!((bright[0] - 0.75).abs() < 1e-6, "{bright:?}");
    }

    #[test]
    fn color_balance_preserve_luminosity_holds_linear_luminance() {
        let cb = ColorBalance::new([0.0; 3], [0.3, 0.0, -0.2], [0.0; 3])
            .unwrap()
            .with_preserve_luminosity(true);
        let px = EncodedRgb([0.45, 0.5, 0.55]);
        let before = linear_srgb_luminance(px.decode(&SRGB).get());
        let after = linear_srgb_luminance(cb.apply(px, &SRGB).decode(&SRGB).get());
        assert!((before - after).abs() < 1e-4, "{before} vs {after}");
        // Without the flag the luminance really does move, so the test above
        // is measuring the flag and not a tautology.
        let plain = cb.with_preserve_luminosity(false);
        let moved = linear_srgb_luminance(plain.apply(px, &SRGB).decode(&SRGB).get());
        assert!((before - moved).abs() > 1e-3, "{before} vs {moved}");
    }

    // --- black & white ----------------------------------------------------

    #[test]
    fn black_and_white_produces_gray_and_honours_its_weights() {
        let bw = BlackAndWhite::DEFAULT;
        // Pure red: min 0, mid 0, max 1 -> weight[red] = 0.4.
        assert!((bw.gray(EncodedRgb([1.0, 0.0, 0.0])) - 0.4).abs() < 1e-6);
        // Pure yellow: min 0, mid 1, max 1 -> weight[yellow] = 0.6.
        assert!((bw.gray(EncodedRgb([1.0, 1.0, 0.0])) - 0.6).abs() < 1e-6);
        // Pure blue -> 0.2, pure cyan -> 0.6, pure green -> 0.4, magenta -> 0.8.
        assert!((bw.gray(EncodedRgb([0.0, 0.0, 1.0])) - 0.2).abs() < 1e-6);
        assert!((bw.gray(EncodedRgb([0.0, 1.0, 1.0])) - 0.6).abs() < 1e-6);
        assert!((bw.gray(EncodedRgb([0.0, 1.0, 0.0])) - 0.4).abs() < 1e-6);
        assert!((bw.gray(EncodedRgb([1.0, 0.0, 1.0])) - 0.8).abs() < 1e-6);
        // White stays white, black stays black, and the output is neutral.
        assert_eq!(bw.apply(EncodedRgb([1.0; 3])).get(), [1.0; 3]);
        assert_eq!(bw.apply(EncodedRgb([0.0; 3])).get(), [0.0; 3]);
        let out = bw.apply(EncodedRgb([0.3, 0.7, 0.1])).get();
        assert_eq!(out[0], out[1]);
        assert_eq!(out[1], out[2]);
    }

    #[test]
    fn black_and_white_with_all_weights_one_is_the_hsv_value() {
        let bw = BlackAndWhite::new([1.0; 6]).unwrap();
        for px in [[0.3f32, 0.7, 0.1], [0.9, 0.2, 0.5], [0.0, 0.0, 0.4]] {
            let max = px[0].max(px[1]).max(px[2]);
            assert!((bw.gray(EncodedRgb(px)) - max).abs() < 1e-6, "{px:?}");
        }
    }

    #[test]
    fn black_and_white_tint_colours_the_gray() {
        let bw = BlackAndWhite::DEFAULT.with_tint(Some(BwTint::new(30.0, 0.4).unwrap()));
        let out = bw.apply(EncodedRgb([0.3, 0.7, 0.1])).get();
        assert!(out[0] > out[2], "tint did not warm the gray: {out:?}");
        let hsl = rgb_to_hsl(out);
        assert!((hsl[0] - 30.0).abs() < 0.5, "{hsl:?}");
    }

    // --- photo filter -----------------------------------------------------

    #[test]
    fn photo_filter_zero_density_is_bit_exact() {
        let f = PhotoFilter::new(PhotoFilter::WARMING_85, 0.0).unwrap();
        let px = LinearRgb([0.2, 0.5, 3.0]);
        assert_eq!(f.apply(px), px);
        assert_eq!(f.multiplier(), [1.0; 3]);
    }

    #[test]
    fn warming_filter_warms_and_cooling_filter_cools() {
        let px = LinearRgb([0.25; 3]);
        let warm = PhotoFilter::new(PhotoFilter::WARMING_85, 0.5)
            .unwrap()
            .apply(px)
            .get();
        assert!(warm[0] > warm[2], "warming filter did not warm: {warm:?}");
        let cool = PhotoFilter::new(PhotoFilter::COOLING_80, 0.5)
            .unwrap()
            .apply(px)
            .get();
        assert!(cool[2] > cool[0], "cooling filter did not cool: {cool:?}");
    }

    #[test]
    fn photo_filter_preserve_luminosity_holds_luminance() {
        let px = LinearRgb([0.3, 0.25, 0.2]);
        let f = PhotoFilter::new(PhotoFilter::COOLING_80, 0.6).unwrap();
        let before = px.luminance();
        let after = f.apply(px).luminance();
        assert!((before - after).abs() < 1e-5, "{before} vs {after}");
        // And without it the filter really does darken the image.
        let plain = f.with_preserve_luminosity(false).apply(px).luminance();
        assert!(plain < before * 0.95, "{plain} vs {before}");
    }

    #[test]
    fn photo_filter_is_defined_on_light_so_it_scales_a_highlight() {
        let f = PhotoFilter::new(PhotoFilter::SEPIA, 1.0)
            .unwrap()
            .with_preserve_luminosity(false);
        let dim = f.apply(LinearRgb([0.5; 3])).get();
        let hot = f.apply(LinearRgb([5.0; 3])).get();
        for i in 0..3 {
            assert!((hot[i] / dim[i] - 10.0).abs() < 1e-3, "{hot:?} {dim:?}");
        }
    }

    // --- channel mixer ----------------------------------------------------

    #[test]
    fn channel_mixer_identity_is_bit_exact() {
        let px = EncodedRgb([0.2, 0.55, 0.8]);
        assert_eq!(ChannelMixer::IDENTITY.apply(px), px);
    }

    #[test]
    fn channel_mixer_swaps_and_offsets() {
        let swap = ChannelMixer::new([
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
        ])
        .unwrap();
        assert_eq!(
            swap.apply(EncodedRgb([0.1, 0.2, 0.3])).get(),
            [0.3, 0.2, 0.1]
        );
        let offset = ChannelMixer::new([
            [1.0, 0.0, 0.0, 0.25],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ])
        .unwrap();
        assert_eq!(
            offset.apply(EncodedRgb([0.1, 0.2, 0.3])).get(),
            [0.35, 0.2, 0.3]
        );
    }

    #[test]
    fn channel_mixer_monochrome_uses_the_first_row() {
        let m = ChannelMixer::new([
            [0.3, 0.6, 0.1, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ])
        .unwrap()
        .monochrome(true);
        let out = m.apply(EncodedRgb([1.0, 0.5, 0.0])).get();
        assert!(close(out, [0.6; 3], 1e-6), "{out:?}");
        assert!(!m.is_identity());
    }

    #[test]
    fn channel_mixer_rejects_out_of_range() {
        assert!(matches!(
            ChannelMixer::new([[9.0, 0.0, 0.0, 0.0], [0.0; 4], [0.0; 4]]),
            Err(AdjustmentError::OutOfRange {
                name: "mixer weight",
                ..
            })
        ));
        assert!(matches!(
            ChannelMixer::new([[1.0, 0.0, 0.0, 9.0], [0.0; 4], [0.0; 4]]),
            Err(AdjustmentError::OutOfRange {
                name: "mixer constant",
                ..
            })
        ));
    }

    // --- gradient map -----------------------------------------------------

    /// The property that pins the interpolation domain: a black-to-white
    /// gradient map must hand every pixel back its own luminance as a neutral.
    /// Interpolating the two stops as *light* instead would return the encoded
    /// gray as if it were light and brighten a mid-gray pixel from 21% to 50%.
    #[test]
    fn black_to_white_gradient_map_is_a_luminance_preserving_desaturate() {
        let gm = GradientMap::black_to_white();
        for px in [[0.2f32, 0.5, 0.8], [0.9, 0.1, 0.3], [0.5, 0.5, 0.5]] {
            let lin = EncodedRgb(px).decode(&SRGB);
            let out = gm.apply(lin).get();
            assert!(close(out, [out[0]; 3], 1e-6), "not neutral: {out:?}");
            assert!(
                (LinearRgb(out).luminance() - lin.luminance()).abs() < 2e-3,
                "{} vs {}",
                LinearRgb(out).luminance(),
                lin.luminance()
            );
        }
    }

    #[test]
    fn gradient_map_endpoints_and_midpoint() {
        let gm = GradientMap::new(&[
            GradientStop::new(0.0, [1.0, 0.0, 0.0]).unwrap(),
            GradientStop::new(1.0, [0.0, 0.0, 1.0]).unwrap(),
        ])
        .unwrap();
        let black = gm.apply(LinearRgb([0.0; 3])).get();
        assert!(close(black, to_linear(&SRGB, [1.0, 0.0, 0.0]), 1e-6));
        let white = gm.apply(LinearRgb([1.0; 3])).get();
        assert!(close(white, to_linear(&SRGB, [0.0, 0.0, 1.0]), 1e-6));
        // Mid-encoded-gray lands at the middle of the ramp, which is the
        // encoded colour [0.5, 0, 0.5] decoded to light.
        let mid = gm.apply(EncodedRgb([0.5; 3]).decode(&SRGB)).get();
        assert!(
            close(mid, to_linear(&SRGB, [0.5, 0.0, 0.5]), 5e-3),
            "{mid:?}"
        );
    }

    #[test]
    fn gradient_map_reverse_flips_the_ramp() {
        let gm = GradientMap::black_to_white().reversed(true);
        assert!(gm.apply(LinearRgb([0.0; 3])).get()[0] > 0.9);
        assert!(gm.apply(LinearRgb([1.0; 3])).get()[0] < 0.1);
    }

    #[test]
    fn gradient_map_merges_duplicate_positions_and_rejects_too_few() {
        let gm = GradientMap::new(&[
            GradientStop::new(0.5, [1.0, 0.0, 0.0]).unwrap(),
            GradientStop::new(0.5, [0.0, 0.0, 1.0]).unwrap(),
            GradientStop::new(0.0, [0.0; 3]).unwrap(),
        ])
        .unwrap();
        assert_eq!(gm.stops().len(), 2);
        assert_eq!(gm.stops()[1].color_srgb(), [0.5, 0.0, 0.5]);
        assert_eq!(
            GradientMap::new(&[
                GradientStop::new(0.5, [1.0, 0.0, 0.0]).unwrap(),
                GradientStop::new(0.5, [0.0, 0.0, 1.0]).unwrap(),
            ]),
            Err(AdjustmentError::TooFewGradientStops { got: 1 })
        );
    }

    // --- selective colour -------------------------------------------------

    #[test]
    fn selective_color_identity_is_bit_exact() {
        let px = EncodedRgb([0.2, 0.55, 0.8]);
        assert_eq!(SelectiveColor::IDENTITY.apply(px), px);
    }

    #[test]
    fn selective_color_range_weights_partition_unity() {
        for px in [
            [1.0f32, 0.0, 0.0],
            [0.5, 0.25, 0.75],
            [0.9, 0.9, 0.9],
            [0.05, 0.05, 0.05],
            [0.3, 0.6, 0.6],
        ] {
            let w = SelectiveColor::range_weights(EncodedRgb(px));
            let sum: f32 = w.iter().sum();
            assert!((sum - 1.0).abs() < 1e-6, "{px:?} weights {w:?} sum {sum}");
            assert!(w.iter().all(|v| *v >= -1e-7), "{px:?} -> {w:?}");
        }
        // Pure red is entirely in the Reds range.
        let w = SelectiveColor::range_weights(EncodedRgb([1.0, 0.0, 0.0]));
        assert!((w[ColorRange::Reds.index()] - 1.0).abs() < 1e-6, "{w:?}");
        // Pure yellow is entirely in the Yellows range.
        let w = SelectiveColor::range_weights(EncodedRgb([1.0, 1.0, 0.0]));
        assert!((w[ColorRange::Yellows.index()] - 1.0).abs() < 1e-6, "{w:?}");
        // Near-white is in Whites.
        let w = SelectiveColor::range_weights(EncodedRgb([1.0, 1.0, 1.0]));
        assert!((w[ColorRange::Whites.index()] - 1.0).abs() < 1e-6, "{w:?}");
        // Near-black is in Blacks.
        let w = SelectiveColor::range_weights(EncodedRgb([0.0; 3]));
        assert!((w[ColorRange::Blacks.index()] - 1.0).abs() < 1e-6, "{w:?}");
    }

    #[test]
    fn selective_color_targets_only_its_range() {
        // Add cyan ink to reds only.
        let sc = SelectiveColor::IDENTITY
            .with_range(ColorRange::Reds, [1.0, 0.0, 0.0, 0.0])
            .unwrap()
            .relative(false);
        let red = sc.apply(EncodedRgb([1.0, 0.0, 0.0])).get();
        assert!(red[0] < 0.01, "red was not shifted toward cyan: {red:?}");
        // A pure blue pixel is untouched: its weight in Reds is zero.
        let blue_in = EncodedRgb([0.0, 0.0, 1.0]);
        let blue = sc.apply(blue_in).get();
        assert!(close(blue, blue_in.get(), 1e-5), "{blue:?}");
    }

    #[test]
    fn selective_color_relative_scales_existing_ink_absolute_does_not() {
        // A pixel with no cyan ink at all: relative mode cannot add any.
        let rel = SelectiveColor::IDENTITY
            .with_range(ColorRange::Reds, [1.0, 0.0, 0.0, 0.0])
            .unwrap()
            .relative(true);
        let out = rel.apply(EncodedRgb([1.0, 0.0, 0.0])).get();
        assert!(close(out, [1.0, 0.0, 0.0], 1e-5), "{out:?}");
        // Absolute mode adds it regardless.
        let abs = rel.relative(false);
        assert!(abs.apply(EncodedRgb([1.0, 0.0, 0.0])).get()[0] < 0.01);
    }

    #[test]
    fn selective_color_black_ink_darkens_the_neutrals() {
        let sc = SelectiveColor::IDENTITY
            .with_range(ColorRange::Neutrals, [0.0, 0.0, 0.0, 0.5])
            .unwrap()
            .relative(false);
        let out = sc.apply(EncodedRgb([0.5; 3])).get();
        assert!(out[0] < 0.5 && close(out, [out[0]; 3], 1e-6), "{out:?}");
    }

    /// The `is_identity` fast path must not be hiding a lossy conversion: a
    /// pixel that carries a *non-zero* set of deltas whose weights all land on
    /// other ranges still goes through the full RGB → CMYK → RGB round trip,
    /// and must come back unchanged.
    #[test]
    fn selective_color_round_trips_rgb_through_cmyk_on_the_slow_path() {
        let sc = SelectiveColor::IDENTITY
            .with_range(ColorRange::Greens, [1.0, -1.0, 1.0, 1.0])
            .unwrap()
            .relative(false);
        assert!(!sc.is_identity(), "the slow path must actually be taken");
        for px in [
            [0.0f32, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.75, 0.1, 0.4],
            [0.9, 0.2, 0.2],
        ] {
            let enc = EncodedRgb(px);
            // None of these pixels has any weight in the Greens range.
            let w = SelectiveColor::range_weights(enc);
            assert!(w[ColorRange::Greens.index()] < 1e-6, "{px:?} -> {w:?}");
            let back = sc.apply(enc).get();
            assert!(close(back, px, 1e-6), "{px:?} -> {back:?}");
        }
    }

    // --- scene-referred highlights ----------------------------------------

    // The non-destructive claim, stated as the failure it prevents: three
    // encoded values above 1.0 go in, three *distinct* values must come out. A
    // clamp to 0..=1 collapses all three onto white and no later adjustment can
    // tell them apart again — the flat highlight patch task item 2 exists to
    // eliminate.
    //
    // `ChannelMixer` is the control: it is the encoded adjustment that was
    // never in doubt, so if it and the three under test agree, the property
    // belongs to the crate and not to one function.

    /// Three neutral highlights, each 0.2 apart.
    fn hot() -> [EncodedRgb; 3] {
        [
            EncodedRgb([1.2, 1.2, 1.2]),
            EncodedRgb([1.4, 1.4, 1.4]),
            EncodedRgb([1.6, 1.6, 1.6]),
        ]
    }

    /// The three results still order the way their inputs did, in every
    /// channel, and at least one of them is still outside the display range.
    fn assert_still_distinct(name: &str, out: [[f32; 3]; 3]) {
        for (c, ((lo, mid), hi)) in out[0].iter().zip(&out[1]).zip(&out[2]).enumerate() {
            assert!(
                lo < mid && mid < hi,
                "{name}: channel {c} collapsed: {out:?}"
            );
        }
        assert!(
            out.iter().flatten().any(|v| *v > 1.0),
            "{name}: every highlight was pulled back into the display range: {out:?}"
        );
    }

    #[test]
    fn highlights_stay_distinct_through_color_balance() {
        let cb = ColorBalance::new([0.3, -0.2, 0.1], [0.1, 0.2, -0.3], [-0.2, 0.1, 0.25]).unwrap();
        let out = hot().map(|px| cb.apply(px, &SRGB).get());
        assert_still_distinct("color balance", out);
        // The highlight band is the only one with any weight up there, so the
        // shift is exactly the highlight amount and the input spacing survives
        // untouched.
        for (hi, mid) in out[2].iter().zip(&out[1]) {
            assert!((hi - mid - 0.2).abs() < 1e-6, "{out:?}");
        }
    }

    #[test]
    fn highlights_stay_distinct_through_selective_color() {
        let sc = SelectiveColor::IDENTITY
            .with_range(ColorRange::Whites, [0.3, -0.2, 0.1, 0.2])
            .unwrap()
            .relative(false);
        let out = hot().map(|px| sc.apply(px).get());
        assert_still_distinct("selective colour", out);
        // The ink stage saw the same clamped white for all three, so the three
        // results differ by exactly what they differed by on the way in.
        for (hi, mid) in out[2].iter().zip(&out[1]) {
            assert!((hi - mid - 0.2).abs() < 1e-6, "{out:?}");
        }
    }

    #[test]
    fn highlights_stay_distinct_through_black_and_white() {
        let bw = BlackAndWhite::DEFAULT;
        let out = hot().map(|px| bw.apply(px).get());
        assert_still_distinct("black & white", out);
        // All six weights sum against a neutral pixel to `min`, so a neutral
        // highlight passes through at its own value.
        assert!((bw.gray(EncodedRgb([3.0; 3])) - 3.0).abs() < 1e-6);
        // A coloured highlight is a weighted sum, not a clamp: pure 2x red is
        // 2 * the red weight.
        assert!((bw.gray(EncodedRgb([2.0, 0.0, 0.0])) - 0.8).abs() < 1e-6);
    }

    /// The same property on a *non-neutral* highlight, which takes different
    /// branches: a chromatic pixel picks a primary/secondary pair in the Black &
    /// White decomposition and a chromatic range in selective colour's weights,
    /// where the neutral highlights above pick neither.
    #[test]
    fn a_chromatic_highlight_is_not_flattened_to_white() {
        let px = EncodedRgb([1.6, 1.4, 1.2]);

        let cb = ColorBalance::new([0.0; 3], [0.0; 3], [0.1, -0.1, 0.0]).unwrap();
        let out = cb.apply(px, &SRGB).get();
        assert_eq!(out, [1.7, 1.3, 1.2], "colour balance flattened {out:?}");

        let sc = SelectiveColor::IDENTITY
            .with_range(ColorRange::Whites, [0.0, 0.0, 0.2, 0.0])
            .unwrap()
            .relative(false);
        let out = sc.apply(px).get();
        // The ink stage sees white and lays 20% yellow on it, giving
        // `[1.0, 1.0, 0.8]`; the `[0.6, 0.4, 0.2]` the pixel carried above the
        // display range is added back on top. A clamping implementation returns
        // `[1.0, 1.0, 0.8]` and the three highlights are gone.
        assert_eq!(out, [1.6, 1.4, 1.0], "selective colour flattened {out:?}");

        // min + (mid - min)·yellow + (max - mid)·red
        //   = 1.2 + 0.2·0.6 + 0.2·0.4 = 1.4
        let bw = BlackAndWhite::DEFAULT.gray(px);
        assert!((bw - 1.4).abs() < 1e-6, "black & white flattened to {bw}");
        assert!((BlackAndWhite::new([1.0; 6]).unwrap().gray(px) - 1.6).abs() < 1e-6);
    }

    /// The reference the three above are measured against.
    #[test]
    fn highlights_stay_distinct_through_the_channel_mixer() {
        let mixer = ChannelMixer::new([
            [0.9, 0.1, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.1, 0.0, 0.9, 0.0],
        ])
        .unwrap();
        assert_still_distinct("channel mixer", hot().map(|px| mixer.apply(px).get()));
    }

    /// The end-to-end consequence: an adjustment stack that brackets one of
    /// these three between an exposure lift and a matching pull must return the
    /// highlights it was given, not a flat white.
    #[test]
    fn a_bracketed_color_balance_gives_the_highlights_back() {
        let cb = ColorBalance::new([0.0; 3], [0.0; 3], [0.1, 0.0, -0.1]).unwrap();
        let lifted = [
            EncodedRgb([1.1, 1.2, 1.3]),
            EncodedRgb([1.5, 1.6, 1.7]),
            EncodedRgb([2.0, 2.1, 2.2]),
        ];
        let out: Vec<[f32; 3]> = lifted.iter().map(|px| cb.apply(*px, &SRGB).get()).collect();
        // Subtracting the (constant, because all three are pure highlight) band
        // shift recovers the input exactly.
        for (px, got) in lifted.iter().zip(&out) {
            for ((g, v), shift) in got.iter().zip(px.get()).zip([0.1f32, 0.0, -0.1]) {
                assert!((g - shift - v).abs() < 1e-6, "{:?} -> {got:?}", px.get());
            }
        }
    }

    #[test]
    fn selective_color_rejects_out_of_range() {
        assert!(matches!(
            SelectiveColor::IDENTITY.with_range(ColorRange::Reds, [2.0, 0.0, 0.0, 0.0]),
            Err(AdjustmentError::OutOfRange {
                name: "cmyk delta",
                ..
            })
        ));
    }
}
