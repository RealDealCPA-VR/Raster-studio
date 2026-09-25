//! W13-H: paint symmetry — every dab of a Brush, Pencil or Eraser stroke
//! mirrored about an axis through the canvas centre.
//!
//! Photopea's options bar offers the symmetry as one drop-down; this is its
//! model. The stroke engine ([`crate::stroke::StrokeTool`]) expands the
//! emitter's dabs through [`Symmetry::expand`] wherever it renders them — the
//! release and the live preview alike — so a mirrored copy is painted, and
//! previewed, exactly like the dab it mirrors. The expansion keeps the copies
//! of each dab next to each other, so the expanded list only ever grows at its
//! end as the stroke grows: the live preview's "dabs already laid in" index
//! stays valid over it.
//!
//! The axis goes through the centre of the canvas (in the paint target's
//! space, taken at the press). Moving the axis is not offered: Photopea's
//! draggable axis widget has no counterpart here yet.

use std::borrow::Cow;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

use glam::Vec2;

use crate::brush::Dab;

/// The options-bar key of the symmetry drop-down (a Choice indexing
/// [`SymmetryMode::CHOICES`]).
pub const SYMMETRY_KEY: &str = "symmetry";
/// The options-bar key of the Radial / Mandala segment count (an Int).
pub const SYMMETRY_SEGMENTS_KEY: &str = "symmetry_segments";
/// The fewest segments a radial or mandala symmetry may have.
pub const MIN_SEGMENTS: i32 = 2;
/// The most segments a radial or mandala symmetry may have.
pub const MAX_SEGMENTS: i32 = 32;
/// The segment count a fresh tool starts with.
pub const DEFAULT_SEGMENTS: i32 = 6;

/// How a stroke is mirrored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SymmetryMode {
    /// No symmetry: the stroke paints only where it is drawn.
    #[default]
    Off,
    /// Mirrored across the vertical axis through the centre (left / right).
    Vertical,
    /// Mirrored across the horizontal axis through the centre (top / bottom).
    Horizontal,
    /// Both axes: four copies.
    DualAxis,
    /// Mirrored across the diagonal through the centre that runs from the
    /// top-left towards the bottom-right.
    Diagonal,
    /// `segments` copies rotated evenly about the centre.
    Radial,
    /// Radial, with every rotated copy also mirrored: `2 × segments` copies,
    /// a kaleidoscope.
    Mandala,
}

impl SymmetryMode {
    /// The drop-down's entries, index for index with [`SymmetryMode::ALL`].
    pub const CHOICES: &'static [&'static str] = &[
        "Off",
        "Vertical",
        "Horizontal",
        "Dual Axis",
        "Diagonal",
        "Radial",
        "Mandala",
    ];

    /// Every mode, in drop-down order.
    pub const ALL: [SymmetryMode; 7] = [
        SymmetryMode::Off,
        SymmetryMode::Vertical,
        SymmetryMode::Horizontal,
        SymmetryMode::DualAxis,
        SymmetryMode::Diagonal,
        SymmetryMode::Radial,
        SymmetryMode::Mandala,
    ];

    /// The mode a drop-down index names; past the end clamps to the last.
    pub fn from_choice(index: usize) -> Self {
        Self::ALL[index.min(Self::ALL.len() - 1)]
    }
}

/// A stroke's symmetry: the mode and, for Radial / Mandala, how many
/// segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Symmetry {
    pub mode: SymmetryMode,
    pub segments: u32,
}

impl Default for Symmetry {
    fn default() -> Self {
        Self {
            mode: SymmetryMode::Off,
            segments: DEFAULT_SEGMENTS as u32,
        }
    }
}

/// One copy of a dab: where it goes and how its tip turns.
#[derive(Debug, Clone, Copy)]
struct Image {
    /// `true` for a reflection, `false` for a rotation.
    reflect: bool,
    /// Rotation angle, or the angle of the reflection line.
    angle: f32,
}

impl Image {
    fn apply(self, d: Vec2) -> Vec2 {
        if self.reflect {
            // Reflection across the line at `angle`: rotate by twice it and
            // flip.
            let (s, c) = (2.0 * self.angle).sin_cos();
            Vec2::new(c * d.x + s * d.y, s * d.x - c * d.y)
        } else {
            let (s, c) = self.angle.sin_cos();
            Vec2::new(c * d.x - s * d.y, s * d.x + c * d.y)
        }
    }

    /// The dab's own tip angle under this image: a rotation adds, a
    /// reflection across the line at `a` sends `θ` to `2a − θ`.
    fn tip_angle(self, theta: f32) -> f32 {
        let a = if self.reflect {
            2.0 * self.angle - theta
        } else {
            theta + self.angle
        };
        // Keep it inside the options bar's own −π..π range.
        (a + PI).rem_euclid(TAU) - PI
    }
}

impl Symmetry {
    /// `true` when the stroke paints only its own dabs.
    pub fn is_off(&self) -> bool {
        self.mode == SymmetryMode::Off
    }

    /// The images of the plane this symmetry paints through, the identity
    /// first. (In image space y grows downwards; a "vertical" axis is
    /// the line x = centre, whose reflection flips x.)
    fn images(&self) -> Vec<Image> {
        let id = Image {
            reflect: false,
            angle: 0.0,
        };
        let refl = |angle: f32| Image {
            reflect: true,
            angle,
        };
        let rot = |angle: f32| Image {
            reflect: false,
            angle,
        };
        let n = self
            .segments
            .clamp(MIN_SEGMENTS as u32, MAX_SEGMENTS as u32);
        match self.mode {
            SymmetryMode::Off => vec![id],
            SymmetryMode::Vertical => vec![id, refl(FRAC_PI_2)],
            SymmetryMode::Horizontal => vec![id, refl(0.0)],
            SymmetryMode::DualAxis => vec![id, refl(FRAC_PI_2), refl(0.0), rot(PI)],
            SymmetryMode::Diagonal => vec![id, refl(PI / 4.0)],
            SymmetryMode::Radial => (0..n).map(|k| rot(TAU * k as f32 / n as f32)).collect(),
            SymmetryMode::Mandala => (0..n)
                .flat_map(|k| {
                    let a = TAU * k as f32 / n as f32;
                    // The mirrored copy of the segment at `a` reflects across
                    // the vertical through the centre, then turns by `a`: a
                    // reflection across the line at `π/2 + a/2`.
                    [rot(a), refl(FRAC_PI_2 + a / 2.0)]
                })
                .collect(),
        }
    }

    /// Every point `p` paints under this symmetry about `center`, `p` first.
    pub fn points(&self, p: Vec2, center: Vec2) -> Vec<Vec2> {
        self.images()
            .into_iter()
            .map(|im| center + im.apply(p - center))
            .collect()
    }

    /// The dabs a stroke actually stamps: each of `dabs` followed by its
    /// mirrored copies about `center`. Borrowed untouched when the symmetry
    /// is off. The copies of dab `i` sit together at
    /// `i * n .. (i + 1) * n` for `n` images, so the list grows only at its
    /// end as the stroke does.
    pub fn expand<'a>(&self, dabs: &'a [Dab], center: Vec2) -> Cow<'a, [Dab]> {
        if self.is_off() {
            return Cow::Borrowed(dabs);
        }
        let images = self.images();
        let mut out = Vec::with_capacity(dabs.len() * images.len());
        for d in dabs {
            for im in &images {
                let mut copy = *d;
                copy.center = center + im.apply(d.center - center);
                copy.angle = im.tip_angle(d.angle);
                out.push(copy);
            }
        }
        Cow::Owned(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    fn has(points: &[Vec2], want: Vec2) -> bool {
        points.iter().any(|p| close(*p, want))
    }

    fn sym(mode: SymmetryMode, segments: u32) -> Symmetry {
        Symmetry { mode, segments }
    }

    #[test]
    fn each_mode_maps_a_point_to_its_mirror_images() {
        let c = Vec2::new(32.0, 32.0);
        let p = Vec2::new(12.0, 20.0);
        let off = sym(SymmetryMode::Off, 6).points(p, c);
        assert_eq!(off.len(), 1);
        let v = sym(SymmetryMode::Vertical, 6).points(p, c);
        assert_eq!(v.len(), 2);
        assert!(has(&v, Vec2::new(52.0, 20.0)), "{v:?}");
        let h = sym(SymmetryMode::Horizontal, 6).points(p, c);
        assert!(has(&h, Vec2::new(12.0, 44.0)), "{h:?}");
        let dual = sym(SymmetryMode::DualAxis, 6).points(p, c);
        assert_eq!(dual.len(), 4);
        for want in [(12.0, 20.0), (52.0, 20.0), (12.0, 44.0), (52.0, 44.0)] {
            assert!(has(&dual, Vec2::new(want.0, want.1)), "{dual:?}");
        }
        let diag = sym(SymmetryMode::Diagonal, 6).points(p, c);
        // (dx, dy) = (-20, -12) mirrors to (-12, -20).
        assert!(has(&diag, Vec2::new(20.0, 12.0)), "{diag:?}");
        let radial = sym(SymmetryMode::Radial, 4).points(p, c);
        assert_eq!(radial.len(), 4);
        // A quarter turn (y down): (dx, dy) -> (-dy, dx) = (12, -20).
        assert!(has(&radial, Vec2::new(44.0, 12.0)), "{radial:?}");
        assert!(has(&radial, Vec2::new(52.0, 44.0)), "{radial:?}");
        let mandala = sym(SymmetryMode::Mandala, 4).points(p, c);
        assert_eq!(mandala.len(), 8);
        // The mandala holds the radial images AND the vertical mirror.
        for q in &radial {
            assert!(has(&mandala, *q), "{mandala:?}");
        }
        assert!(has(&mandala, Vec2::new(52.0, 20.0)), "{mandala:?}");
    }

    #[test]
    fn the_expansion_keeps_each_dabs_copies_together_so_it_only_grows_at_the_end() {
        let d = |x: f32| Dab {
            center: Vec2::new(x, 10.0),
            radius: 2.0,
            hardness: 1.0,
            angle: 0.3,
            roundness: 0.5,
            flow: 1.0,
            aliased: false,
            tip: Default::default(),
        };
        let s = sym(SymmetryMode::Radial, 3);
        let c = Vec2::new(32.0, 32.0);
        let short = s.expand(&[d(1.0)], c).into_owned();
        let long = s.expand(&[d(1.0), d(5.0)], c).into_owned();
        assert_eq!(short.len(), 3);
        assert_eq!(long.len(), 6);
        assert_eq!(&long[..3], &short[..]);
        // A rotated copy's tip turns with it.
        assert!((long[1].angle - (0.3 + TAU / 3.0)).abs() < 1e-4);
        // Off borrows the dabs untouched.
        assert!(matches!(
            Symmetry::default().expand(&[d(1.0)], c),
            Cow::Borrowed(_)
        ));
    }
}
