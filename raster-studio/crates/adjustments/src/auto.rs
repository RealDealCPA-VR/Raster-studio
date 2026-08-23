//! Auto Tone, Auto Contrast and Auto Color.
//!
//! These three are not pixel functions. They are *analyses*: they read a
//! histogram of the image and emit a concrete [`Levels`], which is then applied
//! like any other adjustment. Keeping the two halves separate is what makes
//! them non-destructive — the emitted Levels is an ordinary parametric
//! adjustment the user can inspect and edit afterwards, rather than a baked
//! pixel operation — and it is also what makes them testable, because the
//! analysis can be checked against a known histogram without rendering
//! anything.
//!
//! The histogram is built from **gamma-encoded** values, which is the space the
//! controls' black/white points live in.

use color::ColorSpace;

use crate::error::{in_range, AdjustmentError};
use crate::space::{clamp01, EncodedRgb};
use crate::tone::{Levels, LevelsChannel, MAX_GAMMA, MIN_GAMMA, MIN_LEVELS_SPAN};

/// Bin count of the per-channel histograms: one bin per 8-bit code.
pub const HISTOGRAM_BINS: usize = 256;

/// The fraction of pixels clipped at each end by default: 0.1%, the figure
/// Photoshop's auto commands use.
pub const DEFAULT_CLIP: f32 = 0.001;

/// A 256-bin histogram of encoded values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Histogram {
    bins: [u32; HISTOGRAM_BINS],
    total: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            bins: [0; HISTOGRAM_BINS],
            total: 0,
        }
    }
}

impl Histogram {
    /// An empty histogram.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one encoded value. Values outside `0..=1` are counted in the
    /// nearest end bin; they are real pixels and dropping them would bias the
    /// percentiles.
    pub fn add(&mut self, v: f32) {
        if v.is_nan() {
            return;
        }
        let bin = (clamp01(v) * (HISTOGRAM_BINS - 1) as f32).round() as usize;
        self.bins[bin] += 1;
        self.total += 1;
    }

    /// How many values were recorded.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The raw bins.
    pub fn bins(&self) -> &[u32; HISTOGRAM_BINS] {
        &self.bins
    }

    /// The lowest bin index at which the running count from the dark end has
    /// passed `skip`, as a value in `0..=1`.
    fn low_bound(&self, skip: u64) -> f32 {
        let mut seen = 0u64;
        for (i, count) in self.bins.iter().enumerate() {
            seen += u64::from(*count);
            if seen > skip {
                return i as f32 / (HISTOGRAM_BINS - 1) as f32;
            }
        }
        0.0
    }

    /// The same from the light end.
    fn high_bound(&self, skip: u64) -> f32 {
        let mut seen = 0u64;
        for (i, count) in self.bins.iter().enumerate().rev() {
            seen += u64::from(*count);
            if seen > skip {
                return i as f32 / (HISTOGRAM_BINS - 1) as f32;
            }
        }
        1.0
    }

    /// The value below which half the samples fall, in `0..=1`.
    pub fn median(&self) -> f32 {
        self.low_bound(self.total / 2)
    }

    /// The `(black, white)` points that clip `clip` of the samples off each
    /// end, or `None` if the histogram is empty or the surviving range is
    /// narrower than [`MIN_LEVELS_SPAN`].
    pub fn clipped_bounds(&self, clip: f32) -> Option<(f32, f32)> {
        if self.total == 0 {
            return None;
        }
        let skip = (f64::from(clip) * self.total as f64) as u64;
        let black = self.low_bound(skip);
        let white = self.high_bound(skip);
        if white - black < MIN_LEVELS_SPAN {
            return None;
        }
        Some((black, white))
    }
}

/// Per-channel and composite histograms of an image region.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageStats {
    /// Red, green and blue histograms of the encoded values.
    pub channels: [Histogram; 3],
    /// Histogram of the encoded Rec. 709 gray.
    pub luma: Histogram,
}

impl ImageStats {
    /// An empty set of statistics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one encoded pixel.
    pub fn add(&mut self, enc: EncodedRgb) {
        let v = enc.get();
        for (h, c) in self.channels.iter_mut().zip(v) {
            h.add(c);
        }
        self.luma.add(enc.luma());
    }

    /// Build from encoded pixels.
    pub fn from_encoded(pixels: &[EncodedRgb]) -> Self {
        let mut s = Self::new();
        for px in pixels {
            s.add(*px);
        }
        s
    }

    /// Build from a linear **premultiplied** RGBA buffer, the form the
    /// compositor works in.
    ///
    /// Fully transparent pixels are skipped: their colour is `[0, 0, 0]` by
    /// construction and counting them would drag every black point to zero and
    /// make the auto commands do nothing on any image with transparency.
    /// Partially transparent pixels are un-premultiplied first, so a 25%-alpha
    /// white pixel counts as white and not as a dark gray.
    pub fn from_premultiplied_rgba(pixels: &[[f32; 4]], space: &ColorSpace) -> Self {
        let mut s = Self::new();
        for px in pixels {
            if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                continue;
            }
            let straight = color::unpremultiply(*px);
            s.add(EncodedRgb(color::from_linear(
                space,
                [straight[0], straight[1], straight[2]],
            )));
        }
        s
    }

    /// How many pixels were recorded.
    pub fn count(&self) -> u64 {
        self.luma.total()
    }
}

/// Which of the three auto commands to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoKind {
    /// Auto Contrast: one black/white point from the composite gray, applied to
    /// all three channels. Stretches contrast **without** touching colour
    /// balance, because every channel gets the same mapping.
    Contrast,
    /// Auto Tone: an independent black/white point per channel. Stretches
    /// contrast *and* removes a colour cast at the ends of the range, because
    /// each channel is stretched to fill the range on its own.
    Tone,
    /// Auto Color: per-channel black/white points **and** a per-channel gamma
    /// chosen so each channel's median lands on mid-gray. Neutralises a cast
    /// through the midtones as well as the ends.
    Color,
}

/// An auto command: a [kind](AutoKind) and how much of each tail to clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoMode {
    kind: AutoKind,
    clip: f32,
}

impl AutoMode {
    /// Auto Contrast at the [default clip](DEFAULT_CLIP).
    pub const CONTRAST: Self = Self {
        kind: AutoKind::Contrast,
        clip: DEFAULT_CLIP,
    };
    /// Auto Tone at the [default clip](DEFAULT_CLIP).
    pub const TONE: Self = Self {
        kind: AutoKind::Tone,
        clip: DEFAULT_CLIP,
    };
    /// Auto Color at the [default clip](DEFAULT_CLIP).
    pub const COLOR: Self = Self {
        kind: AutoKind::Color,
        clip: DEFAULT_CLIP,
    };

    /// `clip` in `0.0..=0.1` — the fraction of pixels discarded at *each* end
    /// before the black and white points are read off.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::NotFinite`] / [`AdjustmentError::OutOfRange`].
    pub fn new(kind: AutoKind, clip: f32) -> Result<Self, AdjustmentError> {
        Ok(Self {
            kind,
            clip: in_range("clip", clip, 0.0, 0.1)?,
        })
    }

    /// Which command this is.
    pub fn kind(&self) -> AutoKind {
        self.kind
    }

    /// The tail fraction clipped at each end.
    pub fn clip(&self) -> f32 {
        self.clip
    }

    /// Turn the command into the concrete [`Levels`] it stands for.
    ///
    /// Returns [`Levels::IDENTITY`] when the statistics cannot support a
    /// decision — an empty region, or one whose whole range is narrower than
    /// [`MIN_LEVELS_SPAN`]. Doing nothing is the right answer there; the
    /// alternative is inventing a 1000x gain from three pixels.
    pub fn resolve(&self, stats: &ImageStats) -> Levels {
        match self.kind {
            AutoKind::Contrast => match stats.luma.clipped_bounds(self.clip) {
                Some((b, w)) => match LevelsChannel::new(b, w, 1.0) {
                    Ok(ch) => Levels::composite(ch),
                    Err(_) => Levels::IDENTITY,
                },
                None => Levels::IDENTITY,
            },
            AutoKind::Tone => Levels::per_channel(
                stretch(&stats.channels[0], self.clip, false),
                stretch(&stats.channels[1], self.clip, false),
                stretch(&stats.channels[2], self.clip, false),
            ),
            AutoKind::Color => Levels::per_channel(
                stretch(&stats.channels[0], self.clip, true),
                stretch(&stats.channels[1], self.clip, true),
                stretch(&stats.channels[2], self.clip, true),
            ),
        }
    }
}

/// One channel's auto mapping: clip the tails, and — when `neutralise` — pick
/// the gamma that puts the channel's median on mid-gray.
fn stretch(h: &Histogram, clip: f32, neutralise: bool) -> LevelsChannel {
    let Some((black, white)) = h.clipped_bounds(clip) else {
        return LevelsChannel::IDENTITY;
    };
    let gamma = if neutralise {
        // Where the median sits after the black/white stretch.
        let t = (h.median() - black) / (white - black);
        // `t^(1/gamma) == 0.5` has the solution `gamma = ln(t) / ln(0.5)`.
        if t > 1e-4 && t < 1.0 - 1e-4 {
            (t.ln() / 0.5f32.ln()).clamp(MIN_GAMMA, MAX_GAMMA)
        } else {
            1.0
        }
    } else {
        1.0
    };
    LevelsChannel::new(black, white, gamma).unwrap_or(LevelsChannel::IDENTITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB: ColorSpace = ColorSpace::Srgb;

    /// An image whose channels each occupy a known, different sub-range.
    fn cast_image() -> Vec<EncodedRgb> {
        let mut px = Vec::new();
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            px.push(EncodedRgb([
                0.20 + 0.40 * t, // red   in 0.20..0.60
                0.10 + 0.60 * t, // green in 0.10..0.70
                0.30 + 0.30 * t, // blue  in 0.30..0.60
            ]));
        }
        px
    }

    #[test]
    fn histogram_bounds_and_median() {
        let mut h = Histogram::new();
        for i in 0..=100 {
            h.add(i as f32 / 100.0);
        }
        assert_eq!(h.total(), 101);
        let (b, w) = h.clipped_bounds(0.0).unwrap();
        assert!(b < 0.01, "{b}");
        assert!(w > 0.99, "{w}");
        assert!((h.median() - 0.5).abs() < 0.02, "{}", h.median());
    }

    #[test]
    fn histogram_clip_discards_the_tails() {
        let mut h = Histogram::new();
        // 1000 mid-gray pixels plus one pure black and one pure white outlier.
        for _ in 0..1000 {
            h.add(0.5);
        }
        h.add(0.0);
        h.add(1.0);
        let (b0, w0) = h.clipped_bounds(0.0).unwrap();
        assert_eq!((b0, w0), (0.0, 1.0));
        // Clipping 1% of 1002 samples is 10 per end, which swallows the
        // outliers and leaves only the mid-gray spike.
        assert_eq!(h.clipped_bounds(0.01), None);
    }

    #[test]
    fn empty_statistics_resolve_to_the_identity() {
        let empty = ImageStats::new();
        for mode in [AutoMode::CONTRAST, AutoMode::TONE, AutoMode::COLOR] {
            assert_eq!(mode.resolve(&empty), Levels::IDENTITY);
        }
    }

    #[test]
    fn a_flat_image_resolves_to_the_identity() {
        let flat = ImageStats::from_encoded(&vec![EncodedRgb([0.42; 3]); 500]);
        for mode in [AutoMode::CONTRAST, AutoMode::TONE, AutoMode::COLOR] {
            assert_eq!(mode.resolve(&flat), Levels::IDENTITY);
        }
    }

    #[test]
    fn auto_contrast_stretches_without_changing_the_colour_balance() {
        let stats = ImageStats::from_encoded(&cast_image());
        let lv = AutoMode::CONTRAST.resolve(&stats);
        // One composite mapping, no per-channel mappings.
        assert_eq!(lv.red, LevelsChannel::IDENTITY);
        assert_eq!(lv.green, LevelsChannel::IDENTITY);
        assert_eq!(lv.blue, LevelsChannel::IDENTITY);
        assert!(!lv.composite.is_identity());

        // The darkest and lightest pixels reach the ends of the range.
        let first = lv.apply(cast_image()[0]).get();
        let last = lv.apply(*cast_image().last().unwrap()).get();
        let dark = first[0].min(first[1]).min(first[2]);
        let light = last[0].max(last[1]).max(last[2]);
        assert!(dark < 0.05, "{first:?}");
        assert!(light > 0.95, "{last:?}");

        // The cast survives, because every channel got the same mapping: the
        // blue channel still does not fill the range.
        let out: Vec<_> = cast_image().iter().map(|p| lv.apply(*p).get()).collect();
        let blue_lo = out.iter().fold(f32::INFINITY, |a, p| a.min(p[2]));
        let blue_hi = out.iter().fold(f32::NEG_INFINITY, |a, p| a.max(p[2]));
        assert!(
            blue_lo > 0.1 && blue_hi < 0.95,
            "auto contrast neutralised the cast: {blue_lo}..{blue_hi}"
        );
    }

    #[test]
    fn auto_tone_stretches_every_channel_to_the_full_range() {
        let stats = ImageStats::from_encoded(&cast_image());
        let lv = AutoMode::TONE.resolve(&stats);
        assert_eq!(lv.composite, LevelsChannel::IDENTITY);
        let out: Vec<_> = cast_image().iter().map(|p| lv.apply(*p).get()).collect();
        for c in 0..3 {
            let lo = out.iter().fold(f32::INFINITY, |a, p| a.min(p[c]));
            let hi = out.iter().fold(f32::NEG_INFINITY, |a, p| a.max(p[c]));
            assert!(lo < 0.02, "channel {c} floor {lo}");
            assert!(hi > 0.98, "channel {c} ceiling {hi}");
        }
        // Gamma is untouched by Auto Tone.
        assert_eq!(lv.red.gamma(), 1.0);
        assert_eq!(lv.green.gamma(), 1.0);
        assert_eq!(lv.blue.gamma(), 1.0);
    }

    #[test]
    fn auto_color_puts_every_channel_median_on_mid_gray() {
        // A cast that is not symmetric, so a black/white stretch alone leaves
        // the midtones tinted: the green channel is bunched toward its top.
        let mut px = Vec::new();
        for i in 0..=200 {
            let t = i as f32 / 200.0;
            px.push(EncodedRgb([t, t.powf(0.4), t.powf(1.8)]));
        }
        let stats = ImageStats::from_encoded(&px);
        let lv = AutoMode::COLOR.resolve(&stats);
        assert_ne!(lv.green.gamma(), 1.0, "no midtone correction was made");

        let out: Vec<_> = px.iter().map(|p| lv.apply(*p).get()).collect();
        for c in 0..3 {
            let mut vals: Vec<f32> = out.iter().map(|p| p[c]).collect();
            vals.sort_by(f32::total_cmp);
            let median = vals[vals.len() / 2];
            assert!(
                (median - 0.5).abs() < 0.03,
                "channel {c} median {median} is not mid-gray"
            );
        }
    }

    #[test]
    fn premultiplied_statistics_skip_transparent_and_unpremultiply_the_rest() {
        // Three opaque mid-gray pixels, one 25%-alpha white, and a thousand
        // fully transparent ones that must not count.
        let white_lin = color::to_linear(&SRGB, [1.0; 3]);
        let mut px = vec![[0.0, 0.0, 0.0, 0.0]; 1000];
        let gray_lin = color::to_linear(&SRGB, [0.5; 3]);
        for _ in 0..3 {
            px.push(color::premultiply([
                gray_lin[0],
                gray_lin[1],
                gray_lin[2],
                1.0,
            ]));
        }
        px.push(color::premultiply([
            white_lin[0],
            white_lin[1],
            white_lin[2],
            0.25,
        ]));
        let stats = ImageStats::from_premultiplied_rgba(&px, &SRGB);
        assert_eq!(stats.count(), 4, "transparent pixels were counted");
        // The white pixel is recorded as white, not as a 25% gray.
        let (b, w) = stats.channels[0].clipped_bounds(0.0).unwrap();
        assert!((b - 0.5).abs() < 0.01, "{b}");
        assert!(w > 0.99, "{w}");
    }

    #[test]
    fn auto_mode_rejects_an_out_of_range_clip() {
        assert!(matches!(
            AutoMode::new(AutoKind::Tone, 0.5),
            Err(AdjustmentError::OutOfRange { name: "clip", .. })
        ));
        assert!(AutoMode::new(AutoKind::Tone, 0.1).is_ok());
        assert_eq!(AutoMode::CONTRAST.clip(), DEFAULT_CLIP);
        assert_eq!(AutoMode::TONE.kind(), AutoKind::Tone);
    }
}
