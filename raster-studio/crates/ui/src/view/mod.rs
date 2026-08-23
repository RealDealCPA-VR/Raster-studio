//! Drawing.
//!
//! Everything below reads a model and paints it. No decision about *what* a
//! click means is made here — that lives in [`crate::menu`], [`crate::palette`]
//! and the panel models, where it can be tested without a window. This layer's
//! only job is to put those decisions on screen and post the resulting
//! [`crate::Intent`]s.
//!
//! # No literal colours, sizes or fonts
//!
//! Every value comes from `design`: colours through [`design::current_tokens`]
//! and [`design::color32`], gaps through [`design::Space`], type through
//! [`design::egui_theme::font_id`], radii through [`design::Radius`]. The gate
//! is `crates/ui/tests/no_hardcoded_style.rs`, which reads this module tree's
//! own source and fails on a literal `Color32::from_*`, a bare `FontId::new`,
//! or a raw pixel gap.

use design::{
    color32, current_tokens,
    egui_theme::{font_id, rounding, shadow},
    ColorRole, Elevation, Radius, Space, TextRole, TypeRole,
};
use egui::{Align, Layout, Response, RichText, Sense, Stroke, Ui, Vec2};

mod docks;
mod menu_bar;
mod status;
mod toolbar;

pub use docks::docks;
pub use menu_bar::menu_bar;
pub use status::status_bar;
pub use toolbar::{tool_options, tool_palette};

/// Widget ids the chrome pins down.
///
/// An affordance painted by hand gets an id of its own rather than the one
/// egui derives from call order, for one reason: a stable id lets a headless
/// test find the control with `egui::Context::read_response` and click exactly
/// it. That is what turns "this row emits that command" from a claim about the
/// model into a claim about the thing on screen.
pub mod ids {
    /// One row of the layers panel.
    pub fn layer_row(layer: layer_model::LayerId) -> egui::Id {
        egui::Id::new(("raster-layer-row", layer))
    }

    /// One slot of the tool palette.
    pub fn tool_slot(slot: usize) -> egui::Id {
        egui::Id::new(("raster-tool-slot", slot))
    }

    /// One row of a slot's fly-out.
    ///
    /// Keyed by slot *and* tool, because the slot button and the fly-out row
    /// are two controls for the same tool and must not share an id.
    pub fn flyout_tool(slot: usize, tool: tools::ToolId) -> egui::Id {
        egui::Id::new(("raster-tool-flyout-row", slot, tool))
    }

    /// The visibility toggle of one layer row.
    pub fn layer_eye(layer: layer_model::LayerId) -> egui::Id {
        egui::Id::new(("raster-layer-eye", layer))
    }

    /// One row of the history panel, by its index in the flattened stack.
    pub fn history_row(index: usize) -> egui::Id {
        egui::Id::new(("raster-history-row", index))
    }

    /// The options bar's Reset button, for the active tool.
    pub fn tool_options_reset(tool: tools::ToolId) -> egui::Id {
        egui::Id::new(("raster-tool-reset", tool))
    }

    /// The gradient ramp swatch that opens the stop editor.
    pub fn gradient_swatch(tool: tools::ToolId) -> egui::Id {
        egui::Id::new(("raster-gradient-swatch", tool))
    }

    /// "Add stop" inside the gradient stop editor.
    pub fn gradient_add_stop(tool: tools::ToolId) -> egui::Id {
        egui::Id::new(("raster-gradient-add-stop", tool))
    }

    /// A panel header's "⋯" disclosure, which reveals the move controls.
    pub fn panel_menu(panel: crate::dock::PanelId) -> egui::Id {
        egui::Id::new(("raster-panel-menu", panel))
    }

    /// "Move to ▸ <side>" inside a panel header's disclosure.
    pub fn panel_dock(panel: crate::dock::PanelId, side: crate::dock::DockSide) -> egui::Id {
        egui::Id::new(("raster-panel-dock", panel, side))
    }

    /// The reorder arrows inside a panel header's disclosure.
    pub fn panel_reorder(panel: crate::dock::PanelId, up: bool) -> egui::Id {
        egui::Id::new(("raster-panel-reorder", panel, up))
    }

    /// One row of the Channels panel's eye toggle.
    pub fn channel_eye(index: usize) -> egui::Id {
        egui::Id::new(("raster-channel-eye", index))
    }

    /// The Navigator's proxy rectangle.
    pub fn navigator_proxy() -> egui::Id {
        egui::Id::new("raster-navigator-proxy")
    }

    /// The Navigator's "Fit" button.
    pub fn navigator_fit() -> egui::Id {
        egui::Id::new("raster-navigator-fit")
    }

    /// The Properties panel's "Open editor…" for an adjustment layer.
    pub fn adjustment_editor() -> egui::Id {
        egui::Id::new("raster-adjustment-editor")
    }

    /// The Properties panel's layer Name field.
    pub fn layer_name(layer: layer_model::LayerId) -> egui::Id {
        egui::Id::new(("raster-layer-name", layer))
    }

    /// The Character panel's font Family field.
    pub fn character_family(layer: layer_model::LayerId) -> egui::Id {
        egui::Id::new(("raster-character-family", layer))
    }

    /// The Colour panel's Hex field.
    pub fn color_hex() -> egui::Id {
        egui::Id::new("raster-color-hex")
    }
}

/// What one frame of a [`text_field`] produced.
pub(crate) struct FieldEdit {
    /// The value to commit — `Some` on exactly the frame the edit finished.
    pub committed: Option<String>,
    /// What the field is showing right now, committed or not.
    pub text: String,
    /// `true` while the user is part-way through an edit.
    pub editing: bool,
}

/// A text field whose in-progress edit survives between frames.
///
/// The shape this replaces cannot work, and three fields in this crate were
/// written that way:
///
/// ```ignore
/// let mut name = layer.name.clone();
/// if ui.text_edit_singleline(&mut name).lost_focus() && name != layer.name { … }
/// ```
///
/// The buffer is re-seeded from the document on every frame, so the keystroke
/// is dropped when the local goes out of scope; and the two halves of that
/// condition are mutually exclusive anyway, because `lost_focus` is only true
/// on a frame that consumed no keystroke. The edit is stashed in `ui.memory`
/// instead — keyed off `id`, seeded from `value` only when no edit is in
/// progress — and handed back once, on Enter or on losing focus. Escape throws
/// the edit away. This is the same pattern [`super::status`]'s zoom field
/// already used.
pub(crate) fn text_field(ui: &mut Ui, id: egui::Id, value: &str) -> FieldEdit {
    let key = id.with("in-progress");
    let stored = ui.memory(|m| m.data.get_temp::<String>(key));
    let was_editing = stored.is_some();
    let mut buffer = stored.unwrap_or_else(|| value.to_string());

    let t = current_tokens(ui);
    let response = ui.add_sized(
        Vec2::new(t.metrics.inspector_label_width, t.metrics.control_height),
        egui::TextEdit::singleline(&mut buffer).id(id),
    );

    let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let finished = response.lost_focus();
    let editing = was_editing || response.has_focus() || response.changed();

    if finished || cancelled {
        ui.memory_mut(|m| m.data.remove::<String>(key));
    } else if editing {
        ui.memory_mut(|m| m.data.insert_temp(key, buffer.clone()));
    }

    FieldEdit {
        committed: (finished && !cancelled).then(|| buffer.clone()),
        text: buffer,
        editing,
    }
}

/// A compact text button with an explicit id, painted from the theme.
///
/// `egui::Button` derives its id from call order, which a headless test cannot
/// name. Every control a test drives gets one of these instead — see [`ids`].
/// The disabled state is *painted* rather than merely sensed, so a control that
/// is off looks off.
pub(crate) fn labelled_button(ui: &mut Ui, label: &str, enabled: bool, id: egui::Id) -> Response {
    let t = current_tokens(ui);
    let role = if enabled {
        TextRole::Primary
    } else {
        TextRole::Disabled
    };
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        font_id(t, TypeRole::Body),
        color32(t.palette.text(role)),
    );
    let size = Vec2::new(
        galley.size().x + Space::Medium.pt(),
        t.metrics.control_height,
    );
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    if ui.is_rect_visible(rect) {
        if enabled && response.hovered() {
            let radius = Radius::Small.resolve(&t.radii, rect.height());
            ui.painter().rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::ControlFillHovered)),
            );
        }
        let at = egui::pos2(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        );
        ui.painter()
            .galley(at, galley, color32(t.palette.text(role)));
    }
    response
}

/// A themed label at a given text role and type rung.
pub(crate) fn text(ui: &Ui, s: impl Into<String>, role: TextRole, size: TypeRole) -> RichText {
    let t = current_tokens(ui);
    RichText::new(s.into())
        .color(color32(t.palette.text(role)))
        .font(font_id(t, size))
}

/// Body text in the primary colour — the default for anything readable.
pub(crate) fn body(ui: &Ui, s: impl Into<String>) -> RichText {
    text(ui, s, TextRole::Primary, TypeRole::Body)
}

/// De-emphasised footnote text, for units, counts and hints.
pub(crate) fn hint(ui: &Ui, s: impl Into<String>) -> RichText {
    text(ui, s, TextRole::Tertiary, TypeRole::Footnote)
}

/// A full-width hairline rule. Separators, never boxes.
pub(crate) fn hairline(ui: &mut Ui) {
    let t = current_tokens(ui);
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), t.borders.hairline),
        Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        ui.painter().hline(
            rect.x_range(),
            rect.center().y,
            Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::SeparatorHairline)),
            ),
        );
    }
}

/// The frame a docked panel's body is drawn in.
pub(crate) fn panel_frame(ui: &Ui) -> egui::Frame {
    let t = current_tokens(ui);
    egui::Frame::none()
        .fill(color32(t.palette.color(ColorRole::SurfacePanel)))
        .inner_margin(egui::Margin::same(t.metrics.panel_padding))
}

/// The frame a floating surface — a fly-out, a popover — is drawn in.
pub(crate) fn overlay_frame(ui: &Ui) -> egui::Frame {
    let t = current_tokens(ui);
    let radius = Radius::Large.resolve(&t.radii, t.metrics.control_height * 2.0);
    egui::Frame::none()
        .fill(color32(t.palette.color(ColorRole::SurfaceOverlay)))
        .rounding(rounding(radius))
        .inner_margin(egui::Margin::same(Space::Small.pt()))
        .shadow(shadow(&t.palette, Elevation::Overlay))
}

/// A small square swatch of a straight-alpha sRGB colour, over a checkerboard
/// so partial alpha reads as partial alpha rather than as a darker colour.
pub(crate) fn swatch(ui: &mut Ui, rgba: [f32; 4], side: f32, sense: Sense) -> Response {
    let t = current_tokens(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), sense);
    if ui.is_rect_visible(rect) {
        let radius = Radius::Small.resolve(&t.radii, side);
        let painter = ui.painter();
        if rgba[3] < 1.0 {
            checkerboard(painter, rect, side * 0.25);
        }
        painter.rect_filled(rect, rounding(radius), rgba_to_color32(rgba));
        painter.rect_stroke(
            rect,
            rounding(radius),
            Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::ControlStroke)),
            ),
        );
    }
    response
}

/// The transparency checkerboard, in the palette's own two neutrals so it
/// belongs to the theme rather than being a pair of hard-coded greys.
pub(crate) fn checkerboard(painter: &egui::Painter, rect: egui::Rect, cell: f32) {
    let cell = cell.max(2.0);
    let light = painter
        .ctx()
        .style()
        .visuals
        .widgets
        .noninteractive
        .weak_bg_fill;
    let dark = painter.ctx().style().visuals.extreme_bg_color;
    painter.rect_filled(rect, egui::Rounding::ZERO, light);
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for row in 0..rows {
        for col in 0..cols {
            if (row + col) % 2 == 0 {
                continue;
            }
            let min = rect.min + Vec2::new(col as f32 * cell, row as f32 * cell);
            let square = egui::Rect::from_min_size(min, Vec2::splat(cell)).intersect(rect);
            painter.rect_filled(square, egui::Rounding::ZERO, dark);
        }
    }
}

/// Straight-alpha sRGB in `0.0..=1.0` as an egui colour.
///
/// The user's own foreground and background are the one class of colour the
/// design system does not choose, which is why this conversion exists at all.
pub(crate) fn rgba_to_color32(rgba: [f32; 4]) -> egui::Color32 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(c(rgba[0]), c(rgba[1]), c(rgba[2]), c(rgba[3]))
}

/// A row of an inventory list: a leading glyph area, a label, and trailing
/// content, all on one baseline and the full width of the panel.
pub(crate) fn row_layout<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let t = current_tokens(ui);
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), t.metrics.list_row_height),
        Layout::left_to_right(Align::Center),
        add_contents,
    )
}

/// A compact square toggle that paints a glyph, used for the eye, the lock and
/// the twirl-down. Reads its states from the theme like every other control.
pub(crate) fn glyph_toggle(ui: &mut Ui, glyph: &str, on: bool, tooltip: &str) -> Response {
    glyph_toggle_id(ui, glyph, on, tooltip, None)
}

/// [`glyph_toggle`] with an explicit id, for the toggles a headless test needs
/// to find. See [`ids`].
pub(crate) fn glyph_toggle_id(
    ui: &mut Ui,
    glyph: &str,
    on: bool,
    tooltip: &str,
    id: Option<egui::Id>,
) -> Response {
    let t = current_tokens(ui);
    let side = t.metrics.min_hit_target;
    let (rect, auto) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let response = match id {
        Some(id) => ui.interact(rect, id, Sense::click()),
        None => auto,
    };
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            let radius = Radius::Small.resolve(&t.radii, side);
            painter.rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::ControlFillHovered)),
            );
        }
        let role = if on {
            TextRole::Primary
        } else {
            TextRole::Disabled
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            font_id(t, TypeRole::Footnote),
            color32(t.palette.text(role)),
        );
    }
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// A small pill badge — the mask, effects and clipping indicators.
pub(crate) fn badge(ui: &mut Ui, label: &str, accent: bool) {
    let t = current_tokens(ui);
    let height = Space::Large.pt();
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        font_id(t, TypeRole::Caption),
        color32(t.palette.text(TextRole::Secondary)),
    );
    let width = galley.size().x + Space::Small.pt();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    if ui.is_rect_visible(rect) {
        let radius = Radius::Continuous.resolve(&t.radii, height);
        let fill = if accent {
            ColorRole::AccentSubtle
        } else {
            ColorRole::ControlFill
        };
        ui.painter()
            .rect_filled(rect, rounding(radius), color32(t.palette.color(fill)));
        ui.painter().galley(
            egui::pos2(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            color32(t.palette.text(TextRole::Secondary)),
        );
    }
}

/// The message a panel shows instead of an empty box.
pub(crate) fn empty_state(ui: &mut Ui, message: &str) {
    ui.add_space(Space::Medium.pt());
    ui.vertical_centered(|ui| {
        ui.label(hint(ui, message));
    });
    ui.add_space(Space::Medium.pt());
}
