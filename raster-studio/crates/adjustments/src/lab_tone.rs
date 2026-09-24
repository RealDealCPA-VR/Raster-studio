//! W8-B: Levels and Curves on a Lab document's L, a and b channels.
//!
//! A Lab document keeps one RGB working buffer (Image > Mode > Lab only sets
//! the mode flag: 8-bit RGB round-trips through Lab), so Levels and Curves
//! reach its channels here, per pixel: the pixel pipeline's linear-light
//! sRGB colour is taken to CIELAB (D65), each channel is
//! put on the same `0..=1` scale the RGB channels use — `L / 100`, and
//! `(a + 128) / 255`, `(b + 128) / 255`, Photoshop's 8-bit Lab encoding —
//! the mapping runs there, and the result goes back to RGB.
//!
//! The parameters are the stored ones, `layer_model::AdjustmentKind`'s
//! Levels/`LevelsFull` and Curves/`CurvesFull`. Photoshop's Lab Levels and
//! Curves list Lightness, a and b with no composite row, so the three
//! per-channel fields are Lightness, a and b in that order, and the stored
//! composite (what a plain `Levels { black, white, gamma }` or a composite
//! curve is) acts on Lightness only, after the Lightness mapping. Run on a
//! and b it would move their neutral point (0.5 on this scale) and cast
//! every grey, so it never touches them.

use layer_model::AdjustmentKind;

use crate::auto::HISTOGRAM_BINS;
use crate::prepared::Adjustment;
use crate::tone::{Curves, Levels};

/// Levels or Curves, run on L, a and b.
#[derive(Debug, Clone, PartialEq)]
pub enum LabTone {
    /// Levels: Lightness (then the composite, on Lightness only), a, b.
    Levels(Levels),
    /// Curves: Lightness (then the composite, on Lightness only), a, b.
    Curves(Box<Curves>),
}

/// `[L, a, b]` on the `0..=1` scale a channel mapping reads.
fn to_unit(lab: [f32; 3]) -> [f32; 3] {
    [
        lab[0] / 100.0,
        (lab[1] + 128.0) / 255.0,
        (lab[2] + 128.0) / 255.0,
    ]
}

/// Inverse of [`to_unit`].
fn from_unit(v: [f32; 3]) -> [f32; 3] {
    [v[0] * 100.0, v[1] * 255.0 - 128.0, v[2] * 255.0 - 128.0]
}

impl LabTone {
    /// The Lab reading of a stored Levels or Curves; `None` for every other
    /// adjustment (they run in RGB on a Lab document too).
    pub fn from_kind(kind: &AdjustmentKind) -> Option<Self> {
        match Adjustment::from(kind) {
            Adjustment::Levels(levels) => Some(Self::Levels(levels)),
            Adjustment::Curves(curves) => Some(Self::Curves(Box::new(curves))),
            _ => None,
        }
    }

    /// Whether nothing here can change a pixel.
    pub fn is_identity(&self) -> bool {
        match self {
            Self::Levels(levels) => levels.is_identity(),
            Self::Curves(curves) => curves.is_identity(),
        }
    }

    /// Apply to one `[L, a, b]` colour (L in `0..=100`, a and b in
    /// `-128..=127`).
    pub fn apply_lab(&self, lab: [f32; 3]) -> [f32; 3] {
        let [l, a, b] = to_unit(lab).map(|c| c.clamp(0.0, 1.0));
        let mapped = match self {
            Self::Levels(levels) => [
                levels.composite.apply(levels.red.apply(l)),
                levels.green.apply(a),
                levels.blue.apply(b),
            ],
            Self::Curves(curves) => [
                curves.composite.eval(curves.red.eval(l)),
                curves.green.eval(a),
                curves.blue.eval(b),
            ],
        };
        from_unit(mapped.map(|c| c.clamp(0.0, 1.0)))
    }

    /// Apply to one straight (not premultiplied) linear-light sRGB colour.
    pub fn apply_linear(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.is_identity() {
            return rgb;
        }
        let lab = color::model::linear_srgb_to_lab(rgb);
        color::model::lab_to_linear_srgb(self.apply_lab(lab)).map(|c| {
            if c.is_finite() {
                c.clamp(0.0, 1.0)
            } else {
                0.0
            }
        })
    }

    /// Apply to linear premultiplied RGBA pixels, the shape the pixel
    /// pipeline (`filters::FilterBuffer`) edits. Transparent pixels are left
    /// alone.
    pub fn apply_premultiplied_rgba(&self, pixels: &mut [[f32; 4]]) {
        if self.is_identity() {
            return;
        }
        for px in pixels.iter_mut() {
            if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                continue;
            }
            let straight = color::unpremultiply(*px);
            let out = self.apply_linear([straight[0], straight[1], straight[2]]);
            *px = color::premultiply([out[0], out[1], out[2], px[3]]);
        }
    }
}

/// The L, a and b histograms of linear premultiplied RGBA `pixels`, each on the `0..=1` scale the channel mappings read (so the
/// Levels and Curves dialogs can draw them behind a Lab channel). Fully
/// transparent pixels are not counted.
pub fn lab_histograms(pixels: &[[f32; 4]]) -> [[u32; HISTOGRAM_BINS]; 3] {
    let mut out = [[0u32; HISTOGRAM_BINS]; 3];
    let top = (HISTOGRAM_BINS - 1) as f32;
    for px in pixels {
        if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
            continue;
        }
        let straight = color::unpremultiply(*px);
        let unit = to_unit(color::model::linear_srgb_to_lab([
            straight[0],
            straight[1],
            straight[2],
        ]));
        for (bins, v) in out.iter_mut().zip(unit) {
            if v.is_finite() {
                bins[(v.clamp(0.0, 1.0) * top).round() as usize] += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY5: [f32; 5] = [0.0, 1.0, 1.0, 0.0, 1.0];

    fn lab_of(rgb: [f32; 3]) -> [f32; 3] {
        color::model::linear_srgb_to_lab(rgb)
    }

    /// Levels on the Lightness channel moves L and leaves a and b where they
    /// were — which the same numbers on RGB's Red channel would not.
    #[test]
    fn levels_on_lightness_moves_l_and_keeps_the_colour() {
        let kind = AdjustmentKind::LevelsFull {
            composite: IDENTITY5,
            red: [0.0, 1.0, 1.0, 0.0, 0.5],
            green: IDENTITY5,
            blue: IDENTITY5,
        };
        let tone = LabTone::from_kind(&kind).expect("Levels reads in Lab");
        let rgb = [0.8, 0.4, 0.3];
        let before = lab_of(rgb);
        let after = lab_of(tone.apply_linear(rgb));
        assert!(
            (after[0] - before[0] * 0.5).abs() < 1.0,
            "L {before:?} -> {after:?}"
        );
        assert!(
            (after[1] - before[1]).abs() < 1.5,
            "a {before:?} -> {after:?}"
        );
        assert!(
            (after[2] - before[2]).abs() < 1.5,
            "b {before:?} -> {after:?}"
        );
    }

    /// A Curves raise of the a channel pushes a neutral grey towards magenta
    /// (a > 0) and does not touch its lightness.
    #[test]
    fn curves_on_a_shift_a_grey_along_a_only() {
        let kind = AdjustmentKind::CurvesFull {
            composite: vec![[0.0, 0.0], [1.0, 1.0]],
            red: vec![[0.0, 0.0], [1.0, 1.0]],
            green: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
            blue: vec![[0.0, 0.0], [1.0, 1.0]],
        };
        let tone = LabTone::from_kind(&kind).expect("Curves reads in Lab");
        let grey = [0.2, 0.2, 0.2];
        let out = tone.apply_linear(grey);
        let lab = lab_of(out);
        let before = lab_of(grey);
        assert!(lab[1] > 15.0, "a moved to {lab:?}");
        assert!(lab[2].abs() < 2.0, "b stayed at {lab:?}");
        assert!(
            (lab[0] - before[0]).abs() < 2.0,
            "L stayed: {before:?} -> {lab:?}"
        );
    }

    /// The composite (a plain Levels, a composite curve) acts on Lightness
    /// only: a mid grey stays neutral (a = b = 0) while its L moves. Run on
    /// a and b too, a black point of 0.2 takes this grey to about
    /// [42.8, -12.8, -26.8], a strong blue-cyan cast.
    #[test]
    fn a_composite_move_keeps_a_mid_grey_neutral() {
        let grey = [0.2, 0.2, 0.2];
        let before = lab_of(grey);
        let kinds = [
            AdjustmentKind::Levels {
                black: 0.2,
                white: 1.0,
                gamma: 1.0,
            },
            AdjustmentKind::LevelsFull {
                composite: [0.2, 1.0, 1.0, 0.0, 1.0],
                red: IDENTITY5,
                green: IDENTITY5,
                blue: IDENTITY5,
            },
            AdjustmentKind::CurvesFull {
                composite: vec![[0.0, 0.0], [0.5, 0.7], [1.0, 1.0]],
                red: vec![[0.0, 0.0], [1.0, 1.0]],
                green: vec![[0.0, 0.0], [1.0, 1.0]],
                blue: vec![[0.0, 0.0], [1.0, 1.0]],
            },
        ];
        for kind in kinds {
            let tone = LabTone::from_kind(&kind).expect("reads in Lab");
            let lab = lab_of(tone.apply_linear(grey));
            assert!(
                (lab[0] - before[0]).abs() > 3.0,
                "{kind:?}: L did not move: {before:?} -> {lab:?}"
            );
            assert!(
                lab[1].abs() < 0.5 && lab[2].abs() < 0.5,
                "{kind:?}: the grey took a cast: {before:?} -> {lab:?}"
            );
        }
    }

    #[test]
    fn identity_and_other_adjustments() {
        let identity = LabTone::from_kind(&AdjustmentKind::Levels {
            black: 0.0,
            white: 1.0,
            gamma: 1.0,
        })
        .unwrap();
        assert!(identity.is_identity());
        let mut px = [[0.2, 0.1, 0.05, 0.5]];
        identity.apply_premultiplied_rgba(&mut px);
        assert_eq!(px, [[0.2, 0.1, 0.05, 0.5]]);
        assert!(LabTone::from_kind(&AdjustmentKind::Invert).is_none());
    }

    /// A neutral grey lands every a and b count in the middle bin, and its L
    /// where L / 100 puts it.
    #[test]
    fn lab_histograms_bin_a_grey_at_neutral_a_and_b() {
        let [l, a, b] = lab_histograms(&[[0.5, 0.5, 0.5, 1.0], [0.0; 4]]);
        let mid = (((128.0 / 255.0) * (HISTOGRAM_BINS - 1) as f32).round()) as usize;
        assert_eq!(a[mid], 1);
        assert_eq!(b[mid], 1);
        let lightness = lab_of([0.5, 0.5, 0.5])[0] / 100.0;
        let bin = (lightness * (HISTOGRAM_BINS - 1) as f32).round() as usize;
        assert_eq!(l[bin], 1);
        assert_eq!(l.iter().sum::<u32>(), 1);
    }
}
