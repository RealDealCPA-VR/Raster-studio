//! Match Color: a Reinhard-style statistics transfer in CIELAB.
//!
//! # The algorithm
//!
//! Reinhard, Ashikhmin, Gooch & Shirley, "Color Transfer between Images"
//! (2001), in CIELAB rather than their lαβ: both spaces decorrelate lightness
//! from the two opponent axes, which is what lets each channel be matched on
//! its own. For every channel `k` of `[L*, a*, b*]`:
//!
//! ```text
//! m_k = (x_k − μt_k) · σs_k / σt_k + μs_k
//! ```
//!
//! `μt`, `σt` are the target's mean and standard deviation (the pixels being
//! changed), `μs`, `σs` the source's. A channel whose target deviation is
//! below [`MIN_STD`] is shifted but not scaled — a flat channel has no spread
//! to stretch. Then:
//!
//! * **Luminance** scales the matched `L*`, **Color Intensity** the matched
//!   `a*`/`b*` (so `0` is grey); `1` leaves each as matched.
//! * **Neutralize** matches toward a source whose `a*`/`b*` means are zero —
//!   the source's cast is not imported, and the target's own is removed.
//! * **Fade** blends back: `out = m + (x − m)·fade`, so `fade = 1` is the
//!   original, exactly (and [`MatchColor::is_identity`] says so).
//!
//! Lab is taken from the linear working values as linear sRGB primaries. The
//! transfer is a round trip through an invertible map, so the choice of
//! primaries changes what "the same statistics" means slightly on a
//! wide-gamut document, not whether identity settings are the identity.
//!
//! Statistics are alpha-weighted, and [`LabStats::measure`] takes an optional
//! coverage so the shell can match on the selection only, as Photopea does.

use crate::error::{finite, in_range, AdjustmentError};
use crate::space::LinearRgb;

/// Below this a target channel is shifted, not scaled.
pub const MIN_STD: f32 = 1e-3;

/// Largest luminance / colour-intensity factor.
pub const MAX_MATCH_GAIN: f32 = 2.0;

/// Mean and standard deviation of an image in CIELAB, `[L*, a*, b*]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabStats {
    /// Per-channel mean.
    pub mean: [f32; 3],
    /// Per-channel (population) standard deviation.
    pub std: [f32; 3],
}

impl LabStats {
    /// A mid-grey, unit-spread placeholder: what a Match Color with no
    /// source picked compares equal to.
    pub const NEUTRAL: Self = Self {
        mean: [50.0, 0.0, 0.0],
        std: [1.0, 1.0, 1.0],
    };

    /// Measure linear premultiplied RGBA, each pixel weighted by its alpha
    /// times `coverage(index)` when one is given. `None` when nothing has any
    /// weight (an empty or fully transparent image, or an empty selection).
    pub fn measure(pixels: &[[f32; 4]], coverage: Option<&dyn Fn(usize) -> f32>) -> Option<Self> {
        let mut sw = 0.0f64;
        let mut s = [0.0f64; 3];
        let mut ss = [0.0f64; 3];
        for (i, px) in pixels.iter().enumerate() {
            if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                continue;
            }
            let w = f64::from(px[3] * coverage.map_or(1.0, |c| c(i).clamp(0.0, 1.0)));
            if w <= 0.0 {
                continue;
            }
            let u = color::unpremultiply(*px);
            let lab = color::linear_srgb_to_lab([u[0], u[1], u[2]]);
            if lab.iter().any(|v| !v.is_finite()) {
                continue;
            }
            sw += w;
            for k in 0..3 {
                let v = f64::from(lab[k]);
                s[k] += w * v;
                ss[k] += w * v * v;
            }
        }
        if sw <= 0.0 {
            return None;
        }
        let mean = s.map(|v| v / sw);
        let mut std = [0.0f32; 3];
        for k in 0..3 {
            std[k] = (ss[k] / sw - mean[k] * mean[k]).max(0.0).sqrt() as f32;
        }
        Some(Self {
            mean: mean.map(|v| v as f32),
            std,
        })
    }
}

/// Match Color's validated parameters. See the [module docs](self).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchColor {
    source: LabStats,
    target: LabStats,
    luminance: f32,
    color_intensity: f32,
    fade: f32,
    neutralize: bool,
}

impl MatchColor {
    /// No source picked: source and target are the same statistics.
    pub const IDENTITY: Self = Self {
        source: LabStats::NEUTRAL,
        target: LabStats::NEUTRAL,
        luminance: 1.0,
        color_intensity: 1.0,
        fade: 0.0,
        neutralize: false,
    };

    /// Build one.
    ///
    /// # Errors
    ///
    /// A non-finite mean, a negative or non-finite deviation, or a gain or
    /// fade outside its range.
    pub fn new(
        source: LabStats,
        target: LabStats,
        luminance: f32,
        color_intensity: f32,
        fade: f32,
        neutralize: bool,
    ) -> Result<Self, AdjustmentError> {
        for stats in [source, target] {
            for k in 0..3 {
                finite("mean", stats.mean[k])?;
                in_range("std", stats.std[k], 0.0, 1000.0)?;
            }
        }
        Ok(Self {
            source,
            target,
            luminance: in_range("luminance", luminance, 0.0, MAX_MATCH_GAIN)?,
            color_intensity: in_range("color_intensity", color_intensity, 0.0, MAX_MATCH_GAIN)?,
            fade: in_range("fade", fade, 0.0, 1.0)?,
            neutralize,
        })
    }

    /// The source statistics.
    pub fn source(&self) -> LabStats {
        self.source
    }

    /// The target statistics.
    pub fn target(&self) -> LabStats {
        self.target
    }

    /// Luminance factor.
    pub fn luminance(&self) -> f32 {
        self.luminance
    }

    /// Colour-intensity factor.
    pub fn color_intensity(&self) -> f32 {
        self.color_intensity
    }

    /// Fade, `0..=1`.
    pub fn fade(&self) -> f32 {
        self.fade
    }

    /// Whether the source's cast is neutralised.
    pub fn neutralize(&self) -> bool {
        self.neutralize
    }

    /// The same settings against freshly measured target statistics.
    pub fn with_target(self, target: LabStats) -> Self {
        Self { target, ..self }
    }

    /// Whether no pixel can change: a full fade, or matching an image to its
    /// own statistics with every gain at 1 and no neutralising.
    pub fn is_identity(&self) -> bool {
        self.fade >= 1.0
            || (self.source == self.target
                && self.luminance == 1.0
                && self.color_intensity == 1.0
                && !self.neutralize)
    }

    /// The transfer, one linear pixel.
    pub fn apply(&self, px: LinearRgb) -> LinearRgb {
        let lab = color::linear_srgb_to_lab(px.get());
        let mut source_mean = self.source.mean;
        if self.neutralize {
            source_mean[1] = 0.0;
            source_mean[2] = 0.0;
        }
        let mut m = [0.0f32; 3];
        for k in 0..3 {
            let scale = if self.target.std[k] > MIN_STD {
                self.source.std[k] / self.target.std[k]
            } else {
                1.0
            };
            m[k] = (lab[k] - self.target.mean[k]) * scale + source_mean[k];
        }
        m[0] *= self.luminance;
        m[1] *= self.color_intensity;
        m[2] *= self.color_intensity;
        let out: [f32; 3] = std::array::from_fn(|k| m[k] + (lab[k] - m[k]) * self.fade);
        LinearRgb(color::lab_to_linear_srgb(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bluish, low-contrast image and a warm, contrasty one.
    fn image(tint: [f32; 3], spread: f32) -> Vec<[f32; 4]> {
        (0..256)
            .map(|i| {
                let t = (i as f32 / 255.0 - 0.5) * spread + 0.3;
                let a = if i % 7 == 0 { 0.5 } else { 1.0 };
                color::premultiply([t * tint[0], t * tint[1], t * tint[2], a])
            })
            .collect()
    }

    fn apply_all(mc: &MatchColor, px: &mut [[f32; 4]]) {
        for p in px.iter_mut() {
            let u = color::unpremultiply(*p);
            let o = mc.apply(LinearRgb([u[0], u[1], u[2]])).get();
            *p = color::premultiply([o[0], o[1], o[2], p[3]]);
        }
    }

    #[test]
    fn matching_moves_the_target_means_to_the_source() {
        let source_px = image([1.2, 0.9, 0.6], 0.5);
        let mut target_px = image([0.6, 0.8, 1.3], 0.2);
        let source = LabStats::measure(&source_px, None).unwrap();
        let target = LabStats::measure(&target_px, None).unwrap();
        assert!(
            (source.mean[2] - target.mean[2]).abs() > 10.0,
            "fixture too similar"
        );
        let mc = MatchColor::new(source, target, 1.0, 1.0, 0.0, false).unwrap();
        assert!(!mc.is_identity());
        apply_all(&mc, &mut target_px);
        let after = LabStats::measure(&target_px, None).unwrap();
        for k in 0..3 {
            assert!(
                (after.mean[k] - source.mean[k]).abs() < 0.5,
                "channel {k}: mean {} vs source {}",
                after.mean[k],
                source.mean[k]
            );
            assert!(
                (after.std[k] - source.std[k]).abs() < 0.5,
                "channel {k}: std {} vs source {}",
                after.std[k],
                source.std[k]
            );
        }
    }

    #[test]
    fn full_fade_is_the_identity() {
        let source = LabStats::measure(&image([1.2, 0.9, 0.6], 0.5), None).unwrap();
        let target_px = image([0.6, 0.8, 1.3], 0.2);
        let target = LabStats::measure(&target_px, None).unwrap();
        let mc = MatchColor::new(source, target, 1.3, 0.4, 1.0, true).unwrap();
        assert!(mc.is_identity());
        // The per-pixel maths agrees, not just the shortcut.
        let mut out = target_px.clone();
        apply_all(&mc, &mut out);
        for (a, b) in target_px.iter().zip(&out) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 1e-4, "{a:?} became {b:?}");
            }
        }
    }

    #[test]
    fn neutralize_removes_the_cast_and_intensity_zero_is_grey() {
        let target_px = image([0.6, 0.8, 1.3], 0.2);
        let target = LabStats::measure(&target_px, None).unwrap();
        let mut px = target_px.clone();
        let mc = MatchColor::new(target, target, 1.0, 1.0, 0.0, true).unwrap();
        apply_all(&mc, &mut px);
        let after = LabStats::measure(&px, None).unwrap();
        assert!(
            after.mean[1].abs() < 0.5 && after.mean[2].abs() < 0.5,
            "{after:?}"
        );

        let mut grey = target_px.clone();
        apply_all(
            &MatchColor::new(target, target, 1.0, 0.0, 0.0, false).unwrap(),
            &mut grey,
        );
        let g = LabStats::measure(&grey, None).unwrap();
        assert!(g.std[1] < 0.5 && g.std[2] < 0.5 && g.mean[1].abs() < 0.5);
    }

    #[test]
    fn coverage_limits_what_is_measured() {
        let px = vec![
            [0.8f32, 0.1, 0.1, 1.0],
            [0.1, 0.1, 0.8, 1.0],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let all = LabStats::measure(&px, None).unwrap();
        let first = LabStats::measure(&px, Some(&|i| if i == 0 { 1.0 } else { 0.0 })).unwrap();
        assert_ne!(all, first);
        assert_eq!(first.std, [0.0, 0.0, 0.0]);
        assert!(LabStats::measure(&px[2..], None).is_none());
    }

    #[test]
    fn bad_parameters_are_refused() {
        let n = LabStats::NEUTRAL;
        assert!(MatchColor::new(n, n, 2.5, 1.0, 0.0, false).is_err());
        assert!(MatchColor::new(n, n, 1.0, 1.0, 1.5, false).is_err());
        let bad = LabStats {
            mean: [f32::NAN, 0.0, 0.0],
            std: [1.0; 3],
        };
        assert!(MatchColor::new(bad, n, 1.0, 1.0, 0.0, false).is_err());
        assert!(MatchColor::IDENTITY.is_identity());
    }
}
