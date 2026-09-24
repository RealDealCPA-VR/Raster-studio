//! W10-H: Image > Mode > Duotone — a grayscale image printed with one to
//! four inks, each through its own transfer curve.
//!
//! The ink model is the simple subtractive one Photopea previews with: a
//! grey value `g` (0 black, 1 white) is a tint `t = 1 - g`; each ink lays
//! `a = curve(t)` of its colour, and the inks multiply as filters on white
//! paper in the encoded (display) values —
//! `out = prod(1 - a_i * (1 - ink_i))` per channel. A monotone with a black
//! ink and the identity curve therefore reproduces the grey exactly. It is
//! not an ICC press simulation (no dot gain, no overprint table).

/// One ink: its colour on paper and its transfer curve.
#[derive(Debug, Clone, PartialEq)]
pub struct DuotoneInk {
    /// The ink's colour at 100%, encoded sRGB.
    pub color: [u8; 3],
    /// `[tint, ink]` pairs on `0..=1` (Photoshop's Duotone Curve: input tint
    /// to printed ink percentage), in any order; linear between points and
    /// flat outside them. Fewer than two points is the identity.
    pub curve: Vec<[f32; 2]>,
}

impl DuotoneInk {
    /// An ink with the identity curve.
    pub fn new(color: [u8; 3]) -> Self {
        Self {
            color,
            curve: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }

    /// The ink laid down at tint `t` (`0..=1`).
    pub fn amount(&self, t: f32) -> f32 {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mut pts: Vec<[f32; 2]> = self
            .curve
            .iter()
            .filter(|p| p[0].is_finite() && p[1].is_finite())
            .map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)])
            .collect();
        if pts.len() < 2 {
            return t;
        }
        pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
        if t <= pts[0][0] {
            return pts[0][1];
        }
        for w in pts.windows(2) {
            let ([x0, y0], [x1, y1]) = (w[0], w[1]);
            if t <= x1 {
                if x1 - x0 <= f32::EPSILON {
                    return y1;
                }
                return y0 + (y1 - y0) * (t - x0) / (x1 - x0);
            }
        }
        pts[pts.len() - 1][1]
    }
}

/// Monotone, duotone, tritone or quadtone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DuotoneType {
    Monotone,
    #[default]
    Duotone,
    Tritone,
    Quadtone,
}

impl DuotoneType {
    pub const ALL: [DuotoneType; 4] = [
        DuotoneType::Monotone,
        DuotoneType::Duotone,
        DuotoneType::Tritone,
        DuotoneType::Quadtone,
    ];

    /// How many inks the type prints with.
    pub const fn inks(self) -> usize {
        match self {
            DuotoneType::Monotone => 1,
            DuotoneType::Duotone => 2,
            DuotoneType::Tritone => 3,
            DuotoneType::Quadtone => 4,
        }
    }

    /// The type's name, as Photoshop's Duotone Options list it.
    pub const fn label(self) -> &'static str {
        match self {
            DuotoneType::Monotone => "Monotone",
            DuotoneType::Duotone => "Duotone",
            DuotoneType::Tritone => "Tritone",
            DuotoneType::Quadtone => "Quadtone",
        }
    }

    /// The type that prints with `n` inks (1-4, clamped).
    pub const fn of_inks(n: usize) -> Self {
        match n {
            0 | 1 => DuotoneType::Monotone,
            2 => DuotoneType::Duotone,
            3 => DuotoneType::Tritone,
            _ => DuotoneType::Quadtone,
        }
    }
}

/// The inks the Duotone dialog offers, in order: black first, then warm
/// brown, a slate blue and a muted gold.
pub const DEFAULT_INKS: [[u8; 3]; 4] = [[0, 0, 0], [170, 96, 48], [70, 96, 140], [196, 170, 90]];

/// The conversion: one to four inks.
#[derive(Debug, Clone, PartialEq)]
pub struct DuotoneSpec {
    pub inks: Vec<DuotoneInk>,
}

impl Default for DuotoneSpec {
    fn default() -> Self {
        Self::of_type(DuotoneType::Duotone)
    }
}

impl DuotoneSpec {
    /// The default inks for `kind`, identity curves.
    pub fn of_type(kind: DuotoneType) -> Self {
        Self {
            inks: DEFAULT_INKS[..kind.inks()]
                .iter()
                .map(|&c| DuotoneInk::new(c))
                .collect(),
        }
    }

    /// The spec's type, from its ink count.
    pub fn kind(&self) -> DuotoneType {
        DuotoneType::of_inks(self.inks.len())
    }

    /// Grow or shrink to `kind`'s ink count, keeping the inks it has.
    pub fn set_kind(&mut self, kind: DuotoneType) {
        let n = kind.inks();
        self.inks.truncate(n);
        while self.inks.len() < n {
            self.inks
                .push(DuotoneInk::new(DEFAULT_INKS[self.inks.len()]));
        }
    }

    /// One to four inks.
    pub fn is_valid(&self) -> bool {
        (1..=4).contains(&self.inks.len())
    }

    /// The printed colour of grey `g`.
    pub fn render(&self, g: u8) -> [u8; 3] {
        let t = 1.0 - f32::from(g) / 255.0;
        let mut out = [1.0f32; 3];
        for ink in &self.inks {
            let a = ink.amount(t);
            for (o, c) in out.iter_mut().zip(ink.color) {
                *o *= 1.0 - a * (1.0 - f32::from(c) / 255.0);
            }
        }
        out.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
    }

    /// [`DuotoneSpec::render`] for every grey.
    pub fn lut(&self) -> [[u8; 3]; 256] {
        let mut lut = [[0u8; 3]; 256];
        for (g, entry) in lut.iter_mut().enumerate() {
            *entry = self.render(g as u8);
        }
        lut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_black_monotone_with_the_identity_curve_is_the_grey_itself() {
        let spec = DuotoneSpec::of_type(DuotoneType::Monotone);
        for g in 0..=255u8 {
            assert_eq!(spec.render(g), [g, g, g]);
        }
    }

    #[test]
    fn a_duotone_keeps_paper_white_and_tints_the_midtones_with_its_second_ink() {
        let spec = DuotoneSpec::default();
        assert_eq!(spec.kind(), DuotoneType::Duotone);
        assert_eq!(spec.render(255), [255, 255, 255]);
        assert_eq!(spec.render(0), [0, 0, 0]);
        let mid = spec.render(160);
        assert!(mid[0] > mid[1] && mid[1] > mid[2], "a warm tint: {mid:?}");
    }

    #[test]
    fn an_inks_curve_changes_how_much_of_it_prints() {
        let mut spec = DuotoneSpec::of_type(DuotoneType::Monotone);
        spec.inks[0].color = [200, 0, 0];
        let plain = spec.render(128);
        spec.inks[0].curve = vec![[0.0, 0.0], [0.5, 0.2], [1.0, 1.0]];
        let light = spec.render(128);
        assert!(light[1] > plain[1], "{plain:?} -> {light:?}");
        spec.inks[0].curve = vec![[0.0, 0.0]];
        assert_eq!(spec.render(128), plain, "one point is the identity");
    }

    #[test]
    fn the_type_follows_the_ink_count() {
        let mut spec = DuotoneSpec::default();
        for kind in DuotoneType::ALL {
            spec.set_kind(kind);
            assert_eq!(spec.inks.len(), kind.inks());
            assert_eq!(spec.kind(), kind);
            assert!(spec.is_valid());
        }
        spec.set_kind(DuotoneType::Quadtone);
        spec.inks[0].color = [1, 2, 3];
        spec.set_kind(DuotoneType::Duotone);
        assert_eq!(spec.inks[0].color, [1, 2, 3], "kept the inks it had");
        assert!(!DuotoneSpec { inks: vec![] }.is_valid());
    }
}
