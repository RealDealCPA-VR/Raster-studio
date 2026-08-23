//! Small themed controls the dialogs share.
//!
//! Everything here is a thin wrapper that reads `design` tokens and hands back
//! a plain `bool`/`Response`, so a dialog body stays a list of fields rather
//! than a pile of egui plumbing. Nothing in this file names a colour, a radius
//! or a gap.

use design::{
    color32, current_tokens,
    egui_theme::rounding,
    tokens::palette::ColorRole,
    tokens::{Radius, Space, TextRole, TypeRole},
};
use egui::{vec2, Response, Sense, Ui};

/// A dropdown over a fixed set of options.
///
/// `disabled` returns the reason an option cannot be chosen; such an option is
/// drawn greyed out and explains itself on hover instead of silently doing
/// nothing when clicked. Returns `true` when the selection changed.
pub fn combo<T: Copy + PartialEq>(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash,
    current: &mut T,
    options: &[T],
    label: impl Fn(T) -> String,
    disabled: impl Fn(T) -> Option<&'static str>,
) -> bool {
    let mut changed = false;
    let width = ui.available_width().min(
        ui.spacing()
            .combo_width
            .max(super::sizes::combo_min_width()),
    );
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(label(*current))
        .width(width)
        .show_ui(ui, |ui| {
            for option in options {
                let reason = disabled(*option);
                let response = ui
                    .add_enabled_ui(reason.is_none(), |ui| {
                        ui.selectable_label(*option == *current, label(*option))
                    })
                    .inner;
                match reason {
                    Some(reason) => {
                        response.on_disabled_hover_text(reason);
                    }
                    None => {
                        if response.clicked() && *option != *current {
                            *current = *option;
                            changed = true;
                        }
                    }
                }
            }
        });
    changed
}

/// A numeric field with a unit suffix.
pub fn numeric(
    ui: &mut Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    decimals: usize,
    suffix: &str,
) -> Response {
    let t = current_tokens(ui);
    let width = t.metrics.numeric_field_width;
    ui.add_sized(
        vec2(width, t.metrics.control_height),
        egui::DragValue::new(value)
            .range(range)
            .max_decimals(decimals)
            .suffix(if suffix.is_empty() {
                String::new()
            } else {
                format!(" {suffix}")
            }),
    )
}

/// An integer field, for pixel counts.
pub fn integer(ui: &mut Ui, value: &mut i64, range: std::ops::RangeInclusive<i64>) -> Response {
    let t = current_tokens(ui);
    ui.add_sized(
        vec2(t.metrics.numeric_field_width, t.metrics.control_height),
        egui::DragValue::new(value).range(range),
    )
}

/// A checkbox with its label to the right, on the control grid.
pub fn checkbox_row(ui: &mut Ui, label: &str, value: &mut bool) -> Response {
    let t = current_tokens(ui);
    let color = color32(t.palette.text(TextRole::Primary));
    ui.checkbox(value, egui::RichText::new(label).color(color))
}

/// A colour swatch over a checkerboard, so alpha is visible.
///
/// Senses clicks: it is how every dialog opens the colour picker. The caller
/// passes a stable id from [`super::ids`] rather than letting `egui` allocate
/// one, so a test can find the swatch on screen and click the real rectangle —
/// which is the only way to tell a wired swatch from a painted one.
///
/// The returned `Response` is the whole point of the control. A call site that
/// drops it draws something that looks live and does nothing; that is a bug,
/// not a style, and [`super::ids`] exists so the test that catches it can be
/// written.
#[must_use = "a swatch that ignores its Response is a control wired to nothing"]
pub fn swatch(ui: &mut Ui, id: egui::Id, rgba: [f32; 4], size: egui::Vec2) -> Response {
    swatch_with(ui, id, rgba, size, Sense::click())
}

/// A swatch that only *shows* a colour.
///
/// Senses hover, not clicks, so it does not invite a press that would do
/// nothing — the colour picker's "after" chip is a readout of the colour the
/// dialog will hand back, not a second way to choose one.
pub fn swatch_readonly(ui: &mut Ui, id: egui::Id, rgba: [f32; 4], size: egui::Vec2) {
    let _ = swatch_with(ui, id, rgba, size, Sense::hover());
}

fn swatch_with(
    ui: &mut Ui,
    id: egui::Id,
    rgba: [f32; 4],
    size: egui::Vec2,
    sense: Sense,
) -> Response {
    let (_, rect) = ui.allocate_space(size);
    let response = ui.interact(rect, id, sense);
    if ui.is_rect_visible(rect) {
        let t = current_tokens(ui);
        let radius = Radius::Small.resolve(&t.radii, size.x.min(size.y));
        checkerboard(ui, rect, radius);
        ui.painter().rect_filled(
            rect,
            rounding(radius),
            egui::Color32::from_rgba_unmultiplied(
                to_byte(rgba[0]),
                to_byte(rgba[1]),
                to_byte(rgba[2]),
                to_byte(rgba[3]),
            ),
        );
        ui.painter().rect_stroke(
            rect,
            rounding(radius),
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::ControlStroke)),
            ),
        );
    }
    response
}

/// The transparency checkerboard, clipped to a rounded rect.
pub fn checkerboard(ui: &Ui, rect: egui::Rect, radius: f32) {
    let t = current_tokens(ui);
    let light = color32(t.palette.color(ColorRole::SurfaceElevated));
    let dark = color32(t.palette.color(ColorRole::SurfaceSunken));
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, rounding(radius), light);
    let cell = Space::Small.pt();
    let mut y = rect.top();
    let mut row = 0;
    while y < rect.bottom() {
        let mut x = rect.left() + if row % 2 == 0 { 0.0 } else { cell };
        while x < rect.right() {
            let square =
                egui::Rect::from_min_size(egui::pos2(x, y), vec2(cell, cell)).intersect(rect);
            painter.rect_filled(square, egui::Rounding::ZERO, dark);
            x += cell * 2.0;
        }
        y += cell;
        row += 1;
    }
}

/// A tab strip for a dialog with sections (Preferences, Layer Style).
///
/// Returns `true` when the selection changed. Unlike a segmented control this
/// is a vertical list, because a sidebar of eight sections in a capsule is
/// unreadable.
pub fn sidebar_list(ui: &mut Ui, selected: &mut usize, items: &[&str]) -> bool {
    let mut changed = false;
    for (index, item) in items.iter().enumerate() {
        if design::list_row(ui, item, index == *selected).clicked() && index != *selected {
            *selected = index;
            changed = true;
        }
    }
    changed
}

/// A small monospace-ish readout for a computed number, right-aligned under a
/// field. Quiet by design: it is a consequence, not an input.
pub fn readout(ui: &mut Ui, text: impl Into<String>) -> Response {
    let t = current_tokens(ui);
    ui.label(
        egui::RichText::new(text.into())
            .color(color32(t.palette.text(TextRole::Tertiary)))
            .font(design::egui_theme::font_id(t, TypeRole::Footnote)),
    )
}

/// The tint that leaves a texture's own pixels alone.
///
/// `egui` multiplies every texel by the tint, so anything but opaque white
/// would recolour the image a preview exists to show honestly. It is therefore
/// not a design decision and not a palette entry — it is the identity element
/// of a multiply, named once here so the dialogs' style gate has a single
/// sanctioned site rather than a bare literal at every preview.
pub const UNTINTED: egui::Color32 = egui::Color32::WHITE; // design-exempt: identity tint, not a colour choice

/// 0..=1 float to a colour byte, saturating.
pub fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A colour byte back to 0..=1.
pub fn from_byte(value: u8) -> f32 {
    f32::from(value) / 255.0
}

/// A straight-alpha [`egui::Color32`] from a 0..=1 RGBA quad.
pub fn color_of(rgba: [f32; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        to_byte(rgba[0]),
        to_byte(rgba[1]),
        to_byte(rgba[2]),
        to_byte(rgba[3]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::frame_both_themes;

    #[test]
    fn byte_conversion_round_trips_every_code() {
        for byte in 0..=255u8 {
            assert_eq!(to_byte(from_byte(byte)), byte);
        }
    }

    #[test]
    fn out_of_range_floats_saturate_rather_than_wrapping() {
        assert_eq!(to_byte(-4.0), 0);
        assert_eq!(to_byte(9.0), 255);
        assert_eq!(to_byte(f32::NAN), 0);
    }

    #[test]
    fn the_controls_draw_in_both_appearances() {
        frame_both_themes(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut choice = 1usize;
                combo(
                    ui,
                    "combo",
                    &mut choice,
                    &[0, 1, 2],
                    |v| format!("option {v}"),
                    |v| (v == 2).then_some("not available yet"),
                );
                let mut value = 3.5;
                numeric(ui, &mut value, 0.0..=10.0, 2, "in");
                let mut count = 7i64;
                integer(ui, &mut count, 1..=100);
                let mut flag = true;
                checkbox_row(ui, "Constrain proportions", &mut flag);
                let _ = swatch(
                    ui,
                    egui::Id::new("controls-test-swatch"),
                    [1.0, 0.5, 0.0, 0.5],
                    crate::dialogs::sizes::swatch(),
                );
                let mut section = 0usize;
                sidebar_list(ui, &mut section, &["General", "Interface"]);
                readout(ui, "1200 x 800 px");
            });
        });
    }
}
