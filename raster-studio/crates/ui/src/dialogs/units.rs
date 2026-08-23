//! Measurement units shared by the size dialogs.
//!
//! A dialog that offers "inches" has to be able to say what an inch *is* in
//! pixels, and that answer depends on the document's resolution. Keeping the
//! conversion here — as plain arithmetic on `f64`, with no widget anywhere near
//! it — is what lets New Document, Image Size and Canvas Size agree on the
//! number and be tested without a window.
//!
//! `f64` rather than `f32` on purpose: a 60000 px document at 2.54 cm/inch
//! round-trips through `f32` with visible error, and a size field that changes
//! the value merely by being redisplayed is a bug the user can see.

/// A unit a length can be typed in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Unit {
    #[default]
    Pixels,
    /// Relative to a reference length supplied by the caller.
    Percent,
    Inches,
    Centimeters,
    Millimeters,
    /// Typographic points: 72 to the inch.
    Points,
    /// Picas: 6 to the inch.
    Picas,
}

impl Unit {
    /// Every unit, in menu order.
    pub const ALL: &'static [Unit] = &[
        Self::Pixels,
        Self::Percent,
        Self::Inches,
        Self::Centimeters,
        Self::Millimeters,
        Self::Points,
        Self::Picas,
    ];

    /// The units that describe a *printed* length, i.e. the ones whose pixel
    /// value depends on resolution.
    pub const PHYSICAL: &'static [Unit] = &[
        Self::Inches,
        Self::Centimeters,
        Self::Millimeters,
        Self::Points,
        Self::Picas,
    ];

    /// Short suffix for a numeric field.
    pub const fn short(self) -> &'static str {
        match self {
            Self::Pixels => "px",
            Self::Percent => "%",
            Self::Inches => "in",
            Self::Centimeters => "cm",
            Self::Millimeters => "mm",
            Self::Points => "pt",
            Self::Picas => "pc",
        }
    }

    /// Full name, for a unit menu.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pixels => "Pixels",
            Self::Percent => "Percent",
            Self::Inches => "Inches",
            Self::Centimeters => "Centimeters",
            Self::Millimeters => "Millimeters",
            Self::Points => "Points",
            Self::Picas => "Picas",
        }
    }

    /// How many inches one of this unit is, or `None` when the unit is not a
    /// physical length ([`Unit::Pixels`], [`Unit::Percent`]).
    pub const fn inches_per(self) -> Option<f64> {
        match self {
            Self::Pixels | Self::Percent => None,
            Self::Inches => Some(1.0),
            Self::Centimeters => Some(1.0 / 2.54),
            Self::Millimeters => Some(1.0 / 25.4),
            Self::Points => Some(1.0 / 72.0),
            Self::Picas => Some(1.0 / 6.0),
        }
    }

    /// Whether this unit's pixel value depends on `ppi`.
    pub const fn is_physical(self) -> bool {
        self.inches_per().is_some()
    }

    /// `value` of this unit expressed in pixels.
    ///
    /// `ppi` is the document resolution in pixels per inch and `reference_px`
    /// is what 100% means; both are ignored by the units that do not use them.
    /// A non-positive or non-finite `ppi` falls back to [`DEFAULT_PPI`] rather
    /// than producing an infinity, because a resolution field the user has just
    /// cleared must not blank out the pixel field beside it.
    pub fn to_pixels(self, value: f64, ppi: f64, reference_px: f64) -> f64 {
        match self {
            Self::Pixels => value,
            Self::Percent => value / 100.0 * reference_px,
            _ => {
                let inches = self.inches_per().unwrap_or(1.0) * value;
                inches * sane_ppi(ppi)
            }
        }
    }

    /// Inverse of [`Unit::to_pixels`].
    pub fn from_pixels(self, px: f64, ppi: f64, reference_px: f64) -> f64 {
        match self {
            Self::Pixels => px,
            Self::Percent => {
                if reference_px.abs() < f64::EPSILON {
                    0.0
                } else {
                    px / reference_px * 100.0
                }
            }
            _ => px / sane_ppi(ppi) / self.inches_per().unwrap_or(1.0),
        }
    }

    /// How many decimals a field in this unit should show. Pixels are whole;
    /// a printed length needs three or a 1 px nudge is invisible.
    pub const fn decimals(self) -> usize {
        match self {
            Self::Pixels => 0,
            Self::Percent => 2,
            Self::Points | Self::Picas => 2,
            _ => 3,
        }
    }
}

/// The resolution assumed when a document or a field does not state one.
pub const DEFAULT_PPI: f64 = 72.0;

/// Largest resolution a dialog will accept, in pixels per inch.
pub const MAX_PPI: f64 = 10_000.0;

fn sane_ppi(ppi: f64) -> f64 {
    if ppi.is_finite() && ppi > 0.0 {
        ppi.min(MAX_PPI)
    } else {
        DEFAULT_PPI
    }
}

/// Whether a resolution is typed per inch or per centimeter.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ResolutionUnit {
    #[default]
    PerInch,
    PerCentimeter,
}

impl ResolutionUnit {
    /// Both, in menu order.
    pub const ALL: &'static [ResolutionUnit] = &[Self::PerInch, Self::PerCentimeter];

    /// Label for a unit menu.
    pub const fn label(self) -> &'static str {
        match self {
            Self::PerInch => "Pixels/Inch",
            Self::PerCentimeter => "Pixels/Centimeter",
        }
    }

    /// A value typed in this unit, converted to pixels per inch.
    pub fn to_ppi(self, value: f64) -> f64 {
        match self {
            Self::PerInch => value,
            Self::PerCentimeter => value * 2.54,
        }
    }

    /// Inverse of [`ResolutionUnit::to_ppi`].
    pub fn from_ppi(self, ppi: f64) -> f64 {
        match self {
            Self::PerInch => ppi,
            Self::PerCentimeter => ppi / 2.54,
        }
    }
}

/// A byte count rendered the way a file manager renders it.
///
/// Binary multiples, because that is what an image editor's "Image Size" panel
/// has always meant by MB and what the user compares against.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GB", 1 << 30),
        ("MB", 1 << 20),
        ("KB", 1 << 10),
        ("bytes", 1),
    ];
    for (suffix, scale) in UNITS {
        if bytes >= scale {
            if scale == 1 {
                return format!("{bytes} {suffix}");
            }
            let value = bytes as f64 / scale as f64;
            let decimals = if value >= 100.0 { 0 } else { 1 };
            return format!("{value:.decimals$} {suffix}");
        }
    }
    "0 bytes".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_units_round_trip_through_pixels() {
        for unit in Unit::ALL {
            for value in [0.0, 0.5, 1.0, 7.25, 1234.5] {
                let px = unit.to_pixels(value, 300.0, 800.0);
                let back = unit.from_pixels(px, 300.0, 800.0);
                assert!(
                    (back - value).abs() < 1e-9,
                    "{unit:?}: {value} -> {px}px -> {back}"
                );
            }
        }
    }

    #[test]
    fn one_inch_is_the_resolution_in_pixels() {
        assert_eq!(Unit::Inches.to_pixels(1.0, 300.0, 0.0), 300.0);
        assert_eq!(Unit::Inches.to_pixels(2.0, 72.0, 0.0), 144.0);
    }

    #[test]
    fn the_physical_units_agree_on_what_an_inch_is() {
        let ppi = 254.0;
        let inch = Unit::Inches.to_pixels(1.0, ppi, 0.0);
        for (unit, count) in [
            (Unit::Centimeters, 2.54),
            (Unit::Millimeters, 25.4),
            (Unit::Points, 72.0),
            (Unit::Picas, 6.0),
        ] {
            let got = unit.to_pixels(count, ppi, 0.0);
            assert!(
                (got - inch).abs() < 1e-6,
                "{count} {unit:?} should be an inch ({inch}px), got {got}px"
            );
        }
    }

    #[test]
    fn percent_is_relative_to_the_reference_and_ignores_resolution() {
        assert_eq!(Unit::Percent.to_pixels(50.0, 300.0, 800.0), 400.0);
        assert_eq!(Unit::Percent.to_pixels(50.0, 72.0, 800.0), 400.0);
        assert_eq!(Unit::Percent.from_pixels(400.0, 72.0, 800.0), 50.0);
    }

    #[test]
    fn a_zero_reference_does_not_divide_by_zero() {
        assert_eq!(Unit::Percent.from_pixels(400.0, 72.0, 0.0), 0.0);
    }

    #[test]
    fn a_broken_resolution_falls_back_instead_of_producing_infinity() {
        for bad in [0.0, -10.0, f64::NAN, f64::INFINITY] {
            let px = Unit::Inches.to_pixels(1.0, bad, 0.0);
            assert!(px.is_finite(), "ppi {bad} produced {px}");
            assert_eq!(px, DEFAULT_PPI);
        }
    }

    #[test]
    fn resolution_units_round_trip() {
        assert_eq!(ResolutionUnit::PerInch.to_ppi(300.0), 300.0);
        let per_cm = ResolutionUnit::PerCentimeter;
        assert!((per_cm.from_ppi(per_cm.to_ppi(118.11)) - 118.11).abs() < 1e-9);
        assert!((per_cm.to_ppi(1.0) - 2.54).abs() < 1e-12);
    }

    #[test]
    fn only_physical_units_depend_on_resolution() {
        for unit in Unit::ALL {
            let at_72 = unit.to_pixels(3.0, 72.0, 100.0);
            let at_300 = unit.to_pixels(3.0, 300.0, 100.0);
            assert_eq!(
                unit.is_physical(),
                at_72 != at_300,
                "{unit:?} is_physical={} but {at_72} vs {at_300}",
                unit.is_physical()
            );
            assert_eq!(unit.is_physical(), Unit::PHYSICAL.contains(unit));
        }
    }

    #[test]
    fn byte_counts_read_the_way_a_file_manager_writes_them() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(3 << 20), "3.0 MB");
        assert_eq!(format_bytes(200 << 20), "200 MB");
        assert_eq!(format_bytes(5 << 30), "5.0 GB");
    }
}
