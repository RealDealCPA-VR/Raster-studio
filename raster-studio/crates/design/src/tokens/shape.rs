//! Corner radii and border widths.

/// Concrete corner radii in points.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Radii {
    /// Checkboxes, swatches, tags.
    pub small: f32,
    /// Buttons, fields, list rows.
    pub medium: f32,
    /// Panels, cards, popovers.
    pub large: f32,
}

impl Default for Radii {
    fn default() -> Self {
        Self {
            small: 4.0,
            medium: 7.0,
            large: 12.0,
        }
    }
}

/// A radius token. [`Radius::Continuous`] cannot be a number until the shape it
/// applies to is known, which is why radii are requested through this enum.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Radius {
    None,
    Small,
    Medium,
    Large,
    /// Capsule: the largest radius whose curvature stays continuous across the
    /// whole side, i.e. half the shorter side of the shape.
    Continuous,
}

impl Radius {
    /// Every token, ascending in the radius each yields for a large shape.
    pub const ALL: &'static [Radius] = &[
        Self::None,
        Self::Small,
        Self::Medium,
        Self::Large,
        Self::Continuous,
    ];

    /// Resolve to points.
    ///
    /// `shorter_side_pt` is the shorter side of the rectangle being rounded; it
    /// clamps every token, because a radius above half the shorter side would
    /// make opposite corners overlap.
    pub fn resolve(self, radii: &Radii, shorter_side_pt: f32) -> f32 {
        let max = (shorter_side_pt * 0.5).max(0.0);
        let nominal = match self {
            Self::None => 0.0,
            Self::Small => radii.small,
            Self::Medium => radii.medium,
            Self::Large => radii.large,
            Self::Continuous => max,
        };
        nominal.clamp(0.0, max)
    }
}

/// Stroke widths in points.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BorderWidths {
    /// Separators and control outlines. Kept at 1pt so it lands on a whole
    /// physical pixel at 1x and stays crisp at 2x.
    pub hairline: f32,
    /// Emphasised outline, e.g. the active tool.
    pub thick: f32,
    /// Keyboard focus ring.
    pub focus_ring: f32,
}

impl Default for BorderWidths {
    fn default() -> Self {
        Self {
            hairline: 1.0,
            thick: 2.0,
            focus_ring: 2.5,
        }
    }
}

impl BorderWidths {
    /// The hairline expressed so it lands on exactly one physical pixel at the
    /// given `pixels_per_point`.
    pub fn hairline_for_scale(&self, pixels_per_point: f32) -> f32 {
        if pixels_per_point <= 0.0 {
            self.hairline
        } else {
            1.0 / pixels_per_point
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radii_are_ordered() {
        let r = Radii::default();
        assert!(r.small < r.medium);
        assert!(r.medium < r.large);
    }

    #[test]
    fn resolution_is_monotonic_on_a_shape_big_enough_to_show_it() {
        let r = Radii::default();
        let side = 200.0;
        for pair in Radius::ALL.windows(2) {
            let lo = pair[0].resolve(&r, side);
            let hi = pair[1].resolve(&r, side);
            assert!(hi > lo, "{:?} ({lo}) !< {:?} ({hi})", pair[0], pair[1]);
        }
    }

    #[test]
    fn no_token_can_overlap_opposite_corners() {
        let r = Radii::default();
        for side in [0.0, 1.0, 6.0, 14.0, 24.0] {
            for token in Radius::ALL {
                let v = token.resolve(&r, side);
                assert!(v <= side * 0.5 + 1e-6, "{token:?} on {side}pt gave {v}");
                assert!(v >= 0.0);
            }
        }
    }

    #[test]
    fn continuous_is_a_capsule() {
        let r = Radii::default();
        assert_eq!(Radius::Continuous.resolve(&r, 24.0), 12.0);
    }

    #[test]
    fn hairline_tracks_the_display_scale() {
        let b = BorderWidths::default();
        assert_eq!(b.hairline_for_scale(2.0), 0.5);
        assert_eq!(b.hairline_for_scale(1.0), 1.0);
        assert_eq!(b.hairline_for_scale(0.0), b.hairline);
    }
}
