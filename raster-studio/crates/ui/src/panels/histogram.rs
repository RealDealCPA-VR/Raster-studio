//! The Histogram panel's numbers.
//!
//! The application owns the pixels: it composites the document and hands the
//! workspace a *bounded downsample* of the result (the same bytes the
//! Navigator thumbnail is drawn from). This module turns those bytes into 256
//! bins per channel plus a luminosity ladder, and remembers which composite
//! generation it counted so a frame that draws the panel does not recount an
//! image that has not changed.
//!
//! Nothing here touches egui: the counting is arithmetic on a byte slice and
//! is tested as such; `view::docks::histogram_body` draws what it computes.

use design::{ColorRole, Palette, Srgba};

/// How many bins a channel is counted into.
pub const BINS: usize = 256;

/// The most pixels one count will look at. A source larger than this is
/// sampled on a stride so a 6000×4000 composite costs the same as a thumbnail
/// — the shape of the distribution survives; the exact count does not matter.
pub const MAX_SAMPLES: usize = 65_536;

/// Which curves the panel draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HistogramMode {
    /// Red, green and blue overlaid.
    #[default]
    Rgb,
    /// One curve of perceived brightness.
    Luminosity,
}

impl HistogramMode {
    pub const ALL: &'static [HistogramMode] = &[HistogramMode::Rgb, HistogramMode::Luminosity];
}

/// The counted distribution of one composite.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HistogramBins {
    pub red: [u32; BINS],
    pub green: [u32; BINS],
    pub blue: [u32; BINS],
    pub luminosity: [u32; BINS],
    /// How many pixels were counted (transparent ones are skipped).
    pub samples: u32,
}

impl HistogramBins {
    /// Count `rgba` (straight-alpha 8-bit, four bytes per pixel, `width ×
    /// height` pixels). Pixels with zero alpha are not part of the image and
    /// are skipped, as Photoshop skips them; a short slice is counted as far
    /// as it goes.
    pub fn count(width: usize, height: usize, rgba: &[u8]) -> Self {
        let mut bins = Self {
            red: [0; BINS],
            green: [0; BINS],
            blue: [0; BINS],
            luminosity: [0; BINS],
            samples: 0,
        };
        let pixels = (width * height).min(rgba.len() / 4);
        if pixels == 0 {
            return bins;
        }
        // A stride keeps the work bounded; `ceil` so the stride never rounds
        // down to zero and a huge image still walks at most MAX_SAMPLES.
        let stride = pixels.div_ceil(MAX_SAMPLES).max(1);
        let mut i = 0;
        while i < pixels {
            let px = &rgba[i * 4..i * 4 + 4];
            if px[3] != 0 {
                bins.red[px[0] as usize] += 1;
                bins.green[px[1] as usize] += 1;
                bins.blue[px[2] as usize] += 1;
                bins.luminosity[luma(px[0], px[1], px[2]) as usize] += 1;
                bins.samples += 1;
            }
            i += stride;
        }
        bins
    }

    /// The tallest bin across the channels the mode draws, so every curve is
    /// scaled to the same ceiling.
    pub fn peak(&self, mode: HistogramMode) -> u32 {
        match mode {
            HistogramMode::Rgb => self
                .red
                .iter()
                .chain(self.green.iter())
                .chain(self.blue.iter())
                .copied()
                .max()
                .unwrap_or(0),
            HistogramMode::Luminosity => self.luminosity.iter().copied().max().unwrap_or(0),
        }
    }

    /// The mean luminosity, `0..=255`, or `None` with nothing counted.
    pub fn mean_luminosity(&self) -> Option<f32> {
        if self.samples == 0 {
            return None;
        }
        let sum: u64 = self
            .luminosity
            .iter()
            .enumerate()
            .map(|(v, n)| v as u64 * u64::from(*n))
            .sum();
        Some(sum as f32 / self.samples as f32)
    }

    /// `true` when at least one bin of the mode's channels is non-empty.
    pub fn has_bars(&self, mode: HistogramMode) -> bool {
        self.peak(mode) > 0
    }
}

/// The alpha the three RGB curves are drawn at, so where the channels agree
/// the overlap stacks towards white — Photoshop's "Colors" reading of the
/// histogram — instead of the last-drawn curve hiding the other two.
pub const CURVE_ALPHA: f32 = 0.6;

/// [`CURVE_ALPHA`] as the straight-alpha byte a token colour is drawn with.
pub const fn curve_alpha_byte() -> u8 {
    // 0.6 × 255, rounded; `const fn` cannot call `f32::round`, so the
    // arithmetic is spelled out and the unit test below checks the answer.
    ((CURVE_ALPHA * 255.0) + 0.5) as u8
}

/// The design role component channel `index` is drawn in: red for 0, green
/// for 1, blue for 2 (anything past that is clamped to blue).
///
/// # A data colour is still the theme's to choose
///
/// The red channel's curve is red because it *counts red* — but *which* red
/// is a design decision: the one that clears the panel body and the plot
/// well in each appearance, held to 3:1 by the design crate's gate. So the
/// panel names the role and never the number, and the Histogram's curves and
/// the Channels panel's per-channel thumbnail tints say the same red because
/// they read the same slot.
pub const fn channel_role(index: usize) -> ColorRole {
    match index {
        0 => ColorRole::ChannelRed,
        1 => ColorRole::ChannelGreen,
        _ => ColorRole::ChannelBlue,
    }
}

/// The colour the Histogram draws channel `index`'s curve in: its role's
/// colour in `palette`, at [`CURVE_ALPHA`].
pub fn curve_tint(palette: &Palette, index: usize) -> Srgba {
    palette
        .color(channel_role(index))
        .with_alpha(curve_alpha_byte())
}

/// Rec. 601 luma of an 8-bit pixel, rounded to the nearest level.
pub fn luma(r: u8, g: u8, b: u8) -> u8 {
    let y = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    y.round().clamp(0.0, 255.0) as u8
}

/// The panel's state: the bins of the composite it last counted, and the
/// generation they belong to.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct HistogramState {
    pub mode: HistogramMode,
    /// The composite generation [`HistogramState::bins`] was counted from.
    generation: Option<u64>,
    bins: Option<HistogramBins>,
}

impl HistogramState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count a new composite, unless `generation` is the one already counted.
    /// Returns `true` when a count happened.
    pub fn set_source(
        &mut self,
        generation: u64,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> bool {
        if self.generation == Some(generation) && self.bins.is_some() {
            return false;
        }
        self.bins = Some(HistogramBins::count(width, height, rgba));
        self.generation = Some(generation);
        true
    }

    /// Forget the count: the document closed, or there is none.
    pub fn clear(&mut self) {
        self.generation = None;
        self.bins = None;
    }

    pub fn bins(&self) -> Option<&HistogramBins> {
        self.bins.as_ref()
    }

    pub fn generation(&self) -> Option<u64> {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: usize, height: usize, px: [u8; 4]) -> Vec<u8> {
        std::iter::repeat_n(px, width * height).flatten().collect()
    }

    #[test]
    fn a_solid_image_lands_in_one_bin_per_channel() {
        let bins = HistogramBins::count(4, 4, &solid(4, 4, [200, 100, 50, 255]));
        assert_eq!(bins.samples, 16);
        assert_eq!(bins.red[200], 16);
        assert_eq!(bins.green[100], 16);
        assert_eq!(bins.blue[50], 16);
        assert_eq!(bins.red.iter().sum::<u32>(), 16);
        assert_eq!(bins.luminosity[luma(200, 100, 50) as usize], 16);
        assert!(bins.has_bars(HistogramMode::Rgb));
        assert!(bins.has_bars(HistogramMode::Luminosity));
        assert_eq!(bins.peak(HistogramMode::Rgb), 16);
    }

    #[test]
    fn transparent_pixels_are_not_part_of_the_image() {
        let mut rgba = solid(2, 2, [10, 10, 10, 255]);
        rgba[3] = 0;
        rgba[7] = 0;
        let bins = HistogramBins::count(2, 2, &rgba);
        assert_eq!(bins.samples, 2);
        assert_eq!(bins.red[10], 2);
        let empty = HistogramBins::count(2, 2, &solid(2, 2, [0, 0, 0, 0]));
        assert_eq!(empty.samples, 0);
        assert!(!empty.has_bars(HistogramMode::Rgb));
        assert_eq!(empty.mean_luminosity(), None);
    }

    #[test]
    fn a_large_source_is_sampled_on_a_stride_not_walked_whole() {
        let side = 600; // 360k pixels, well past MAX_SAMPLES
        let bins = HistogramBins::count(side, side, &solid(side, side, [1, 2, 3, 255]));
        assert!(bins.samples as usize <= MAX_SAMPLES, "{}", bins.samples);
        assert!(bins.samples as usize >= MAX_SAMPLES / 2, "{}", bins.samples);
        assert_eq!(bins.red[1], bins.samples);
    }

    #[test]
    fn a_short_slice_is_counted_as_far_as_it_goes() {
        let bins = HistogramBins::count(10, 10, &solid(3, 1, [9, 9, 9, 255]));
        assert_eq!(bins.samples, 3);
        assert_eq!(HistogramBins::count(10, 10, &[]).samples, 0);
    }

    #[test]
    fn luma_is_rec601_and_stays_in_range() {
        assert_eq!(luma(255, 255, 255), 255);
        assert_eq!(luma(0, 0, 0), 0);
        assert_eq!(luma(255, 0, 0), 76);
        assert_eq!(luma(0, 255, 0), 150);
        assert_eq!(luma(0, 0, 255), 29);
    }

    #[test]
    fn the_mean_is_the_weighted_average_of_the_luminosity_ladder() {
        let mut rgba = solid(2, 1, [0, 0, 0, 255]);
        rgba[4..8].copy_from_slice(&[255, 255, 255, 255]);
        let bins = HistogramBins::count(2, 1, &rgba);
        assert_eq!(bins.mean_luminosity(), Some(127.5));
    }

    #[test]
    fn the_state_counts_once_per_generation() {
        let mut state = HistogramState::new();
        assert!(state.bins().is_none());
        assert!(state.set_source(1, 2, 2, &solid(2, 2, [5, 5, 5, 255])));
        assert!(!state.set_source(1, 2, 2, &solid(2, 2, [200, 5, 5, 255])));
        assert_eq!(
            state.bins().unwrap().red[5],
            4,
            "the same generation is not recounted"
        );
        assert!(state.set_source(2, 2, 2, &solid(2, 2, [200, 5, 5, 255])));
        assert_eq!(state.bins().unwrap().red[200], 4);
        assert_eq!(state.generation(), Some(2));
        state.clear();
        assert!(state.bins().is_none());
        assert_eq!(state.generation(), None);
    }

    #[test]
    fn a_channels_curve_is_its_own_role_at_the_overlap_alpha() {
        assert_eq!(channel_role(0), ColorRole::ChannelRed);
        assert_eq!(channel_role(1), ColorRole::ChannelGreen);
        assert_eq!(channel_role(2), ColorRole::ChannelBlue);
        assert_eq!(channel_role(7), channel_role(2), "past blue clamps to blue");
        assert_eq!(curve_alpha_byte(), (CURVE_ALPHA * 255.0).round() as u8);
        for palette in [Palette::light(), Palette::dark()] {
            for i in 0..3 {
                let tint = curve_tint(&palette, i);
                let role = palette.color(channel_role(i));
                assert_eq!((tint.r, tint.g, tint.b), (role.r, role.g, role.b));
                assert_eq!(tint.a, curve_alpha_byte());
                assert!(
                    tint.a > 0 && tint.a < 255,
                    "translucent, or the curves hide each other: {tint:?}"
                );
            }
        }
    }
}
