//! The spacing grid and the derived layout metrics.
//!
//! Everything is a multiple of [`UNIT_PT`] except [`Space::Hair`], which is the
//! single sanctioned half-unit and exists only for gaps between adjacent
//! segments of one control.

/// The grid unit, in points.
pub const UNIT_PT: f32 = 4.0;

/// `steps` grid units in points. `steps` may be fractional for half-units.
pub fn grid(steps: f32) -> f32 {
    steps * UNIT_PT
}

/// Named rungs of the spacing scale, smallest first.
///
/// The declaration order **is** the ordering asserted by the monotonicity test.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Space {
    /// Half a unit. Only for seams inside a single control.
    Hair,
    XSmall,
    Small,
    Medium,
    Large,
    XLarge,
    XXLarge,
}

impl Space {
    /// Every rung, ascending.
    pub const ALL: &'static [Space] = &[
        Self::Hair,
        Self::XSmall,
        Self::Small,
        Self::Medium,
        Self::Large,
        Self::XLarge,
        Self::XXLarge,
    ];

    /// Grid multiples, in units.
    pub const fn units(self) -> f32 {
        match self {
            Self::Hair => 0.5,
            Self::XSmall => 1.0,
            Self::Small => 2.0,
            Self::Medium => 3.0,
            Self::Large => 4.0,
            Self::XLarge => 6.0,
            Self::XXLarge => 8.0,
        }
    }

    /// Size in points.
    pub fn pt(self) -> f32 {
        self.units() * UNIT_PT
    }
}

/// Fixed sizes that the whole app must agree on so controls line up across
/// panels. All are grid multiples.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Metrics {
    /// Height of a button, combo box, or text field.
    pub control_height: f32,
    /// Edge length of a square toolbar button.
    pub toolbar_button: f32,
    /// Height of the toolbar strip.
    pub toolbar_height: f32,
    /// Height of one row in a layers/assets list.
    pub list_row_height: f32,
    /// Padding inside a panel, from its edge to its content.
    pub panel_padding: f32,
    /// Width of the label column in an inspector, so fields align.
    pub inspector_label_width: f32,
    /// Width of the numeric field beside a slider.
    pub numeric_field_width: f32,
    /// Smallest side of any pointer target.
    pub min_hit_target: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            control_height: grid(6.0),         // 24
            toolbar_button: grid(7.0),         // 28
            toolbar_height: grid(11.0),        // 44
            list_row_height: grid(7.0),        // 28
            panel_padding: grid(3.0),          // 12
            inspector_label_width: grid(23.0), // 92
            numeric_field_width: grid(14.0),   // 56
            min_hit_target: grid(6.0),         // 24
        }
    }
}

impl Metrics {
    /// Every metric, for invariant checks.
    pub fn all(&self) -> [(&'static str, f32); 8] {
        [
            ("control_height", self.control_height),
            ("toolbar_button", self.toolbar_button),
            ("toolbar_height", self.toolbar_height),
            ("list_row_height", self.list_row_height),
            ("panel_padding", self.panel_padding),
            ("inspector_label_width", self.inspector_label_width),
            ("numeric_field_width", self.numeric_field_width),
            ("min_hit_target", self.min_hit_target),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rung_but_hair_is_a_whole_unit() {
        for space in Space::ALL {
            let units = space.units();
            if *space == Space::Hair {
                assert_eq!(units, 0.5);
            } else {
                assert_eq!(units, units.round(), "{space:?} is off-grid: {units}");
            }
        }
    }

    #[test]
    fn points_are_units_times_the_grid() {
        assert_eq!(Space::Small.pt(), 8.0);
        assert_eq!(Space::XXLarge.pt(), 32.0);
        assert_eq!(grid(2.5), 10.0);
    }

    #[test]
    fn metrics_sit_on_the_grid() {
        let m = Metrics::default();
        for (name, value) in m.all() {
            assert_eq!(value % UNIT_PT, 0.0, "{name} = {value} is off-grid");
            assert!(value > 0.0, "{name} = {value}");
        }
    }

    #[test]
    fn controls_are_at_least_the_minimum_hit_target() {
        let m = Metrics::default();
        assert!(m.control_height >= m.min_hit_target);
        assert!(m.toolbar_button >= m.min_hit_target);
        assert!(m.list_row_height >= m.min_hit_target);
    }
}
