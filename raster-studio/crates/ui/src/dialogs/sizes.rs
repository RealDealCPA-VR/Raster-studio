//! The fixed extents the dialogs share.
//!
//! `design` owns the sizes the *whole app* has to agree on — control height,
//! label column, hit target. It does not own "how big is a colour swatch in an
//! inspector row", because nothing outside a dialog draws one. Those live here,
//! and every one of them is a whole number of grid units routed through
//! [`design::tokens::grid`], exactly the way [`super::chrome::DialogWidth`]
//! resolves a dialog's width.
//!
//! The point is that no dialog writes a dimension as a bare number. A size that
//! is a literal in a body function is a size nobody can re-scale, and
//! `tests/dialogs_style_gate.rs` refuses one: `vec2(`, `set_width(`,
//! `desired_width(` and friends may not be followed by a digit anywhere under
//! `src/dialogs`.

use design::tokens::grid;
use egui::{vec2, Vec2};

/// A colour swatch in an inspector row: wide enough to read a tint, short
/// enough to sit on one control line.
pub fn swatch() -> Vec2 {
    vec2(grid(12.0), grid(6.0))
}

/// A square swatch beside a menu, for a "Custom" entry's colour.
pub fn swatch_square() -> Vec2 {
    vec2(grid(6.0), grid(6.0))
}

/// One chip in the recent-colours strip. Square, and still a full hit target:
/// it is a control, not a legend.
pub fn swatch_recent() -> Vec2 {
    vec2(grid(6.0), grid(6.0))
}

/// The before/after pair at the foot of the colour picker.
pub fn swatch_compare() -> Vec2 {
    vec2(grid(10.0), grid(6.0))
}

/// A gradient preset chip.
pub fn preset_chip() -> Vec2 {
    vec2(grid(14.0), grid(6.0))
}

/// The stroke preview in the brush editor, in points.
///
/// The brush preview exists twice over: as a coverage buffer the real dab
/// engine renders into, which is measured in *texture pixels*, and as the
/// rectangle that buffer is drawn in, which is measured in *points* like every
/// other extent here. They are the same number today (one texel per point at
/// 100% scale), and `brush_editor` has a test pinning that, but they are not
/// the same kind of number — a UI scale changes one and not the other. This is
/// the point-space one.
pub fn brush_stroke_preview() -> Vec2 {
    vec2(grid(70.0), grid(24.0))
}

/// The narrowest a dropdown may be drawn.
///
/// `egui`'s own `combo_width` is a *starting* width, not a floor, so without
/// this a combo in a tight column collapses to the width of its longest word
/// and stops looking like a control. Wide enough for a colour-mode or a format
/// name plus the caret.
pub fn combo_min_width() -> f32 {
    grid(30.0)
}

/// The colour picker's saturation/value square.
pub fn saturation_value_field() -> Vec2 {
    vec2(grid(60.0), grid(50.0))
}

/// Width of the hue and alpha strips beside it.
pub fn color_strip_width() -> f32 {
    grid(5.0)
}

/// Height of the gradient editor's ramp bar.
pub fn gradient_bar_height() -> f32 {
    grid(9.0)
}

/// The layer style editor's schematic preview.
pub fn style_preview() -> Vec2 {
    vec2(grid(40.0), grid(40.0))
}

/// Width the filter preview image is fitted to.
pub fn filter_preview_width() -> f32 {
    grid(50.0)
}

/// Width the export preview image is fitted to.
pub fn export_preview_width() -> f32 {
    grid(65.0)
}

/// A list or section sidebar inside a dialog.
pub fn sidebar_width() -> f32 {
    grid(45.0)
}

/// The parameter column beside such a sidebar.
pub fn params_column_width() -> f32 {
    grid(60.0)
}

/// The export dialog's preview column.
pub fn preview_column_width() -> f32 {
    grid(70.0)
}

/// The preferences dialog's content pane.
pub fn pane_width() -> f32 {
    grid(105.0)
}

/// How tall a scrolling list inside a dialog may get before it scrolls.
pub fn list_max_height() -> f32 {
    grid(80.0)
}

/// The same, for the preferences pane.
pub fn pane_max_height() -> f32 {
    grid(90.0)
}

/// A wide single-line text field — a file name.
pub fn text_field_wide() -> f32 {
    grid(50.0)
}

/// A document or preset name field.
pub fn text_field_name() -> f32 {
    grid(45.0)
}

/// A short text field — a hex colour, a file-name suffix.
pub fn text_field_short() -> f32 {
    grid(22.0)
}

/// A path field.
pub fn text_field_path() -> f32 {
    grid(60.0)
}

/// Every extent, for the on-grid invariant check.
///
/// A two-dimensional extent contributes both of its sides: one axis drifting
/// off the grid is as wrong as both.
pub fn all() -> Vec<(&'static str, f32)> {
    let pairs = [
        ("swatch", swatch()),
        ("swatch_square", swatch_square()),
        ("swatch_recent", swatch_recent()),
        ("swatch_compare", swatch_compare()),
        ("preset_chip", preset_chip()),
        ("saturation_value_field", saturation_value_field()),
        ("style_preview", style_preview()),
        ("brush_stroke_preview", brush_stroke_preview()),
    ];
    let singles = [
        ("combo_min_width", combo_min_width()),
        ("color_strip_width", color_strip_width()),
        ("gradient_bar_height", gradient_bar_height()),
        ("filter_preview_width", filter_preview_width()),
        ("export_preview_width", export_preview_width()),
        ("sidebar_width", sidebar_width()),
        ("params_column_width", params_column_width()),
        ("preview_column_width", preview_column_width()),
        ("pane_width", pane_width()),
        ("list_max_height", list_max_height()),
        ("pane_max_height", pane_max_height()),
        ("text_field_wide", text_field_wide()),
        ("text_field_name", text_field_name()),
        ("text_field_short", text_field_short()),
        ("text_field_path", text_field_path()),
    ];
    let mut out: Vec<(&'static str, f32)> = Vec::new();
    for (name, size) in pairs {
        out.push((name, size.x));
        out.push((name, size.y));
    }
    out.extend(singles);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use design::tokens::UNIT_PT;

    #[test]
    fn every_dialog_extent_is_a_whole_number_of_grid_units() {
        for (name, value) in all() {
            assert!(value > 0.0, "{name} is {value}");
            assert_eq!(value % UNIT_PT, 0.0, "{name} is {value}pt, off the grid");
        }
    }

    #[test]
    fn a_combo_is_never_narrower_than_its_own_caret_and_a_word() {
        // The floor has to be wider than a hit target, or a "combo" is a
        // square nobody can read a format name out of.
        let metrics = design::tokens::Metrics::default();
        assert!(combo_min_width() > metrics.min_hit_target * 2.0);
    }

    #[test]
    fn a_swatch_is_at_least_a_hit_target_tall() {
        // A control the user is meant to click has to be clickable.
        let metrics = design::tokens::Metrics::default();
        for extent in [
            swatch(),
            swatch_square(),
            swatch_recent(),
            swatch_compare(),
            preset_chip(),
        ] {
            assert!(
                extent.y >= metrics.min_hit_target,
                "{extent:?} is shorter than the minimum hit target"
            );
        }
    }
}
