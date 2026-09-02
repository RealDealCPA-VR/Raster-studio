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

    /// The tool-column footer's swap swatch control.
    pub fn color_swap() -> egui::Id {
        egui::Id::new("raster-color-swap")
    }

    /// The tool-column footer's reset control.
    pub fn color_reset() -> egui::Id {
        egui::Id::new("raster-color-reset")
    }

    /// The gradient ramp swatch that opens the stop editor.
    pub fn gradient_swatch(tool: tools::ToolId) -> egui::Id {
        egui::Id::new(("raster-gradient-swatch", tool))
    }

    /// "Add stop" inside the gradient stop editor.
    pub fn gradient_add_stop(tool: tools::ToolId) -> egui::Id {
        egui::Id::new(("raster-gradient-add-stop", tool))
    }

    /// A panel header's overflow disclosure, which reveals the move controls.
    pub fn panel_tab(panel: crate::dock::PanelId) -> egui::Id {
        egui::Id::new(("raster-panel-tab", panel))
    }

    pub fn panel_menu(panel: crate::dock::PanelId) -> egui::Id {
        egui::Id::new(("raster-panel-menu", panel))
    }

    /// "Move to <side>" inside a panel header's disclosure.
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

    /// The status bar's editable zoom field.
    pub fn status_zoom() -> egui::Id {
        egui::Id::new("raster-status-zoom")
    }

    /// One top-level title in the menu bar, by the title it prints.
    pub fn menu_title(title: &'static str) -> egui::Id {
        egui::Id::new(("raster-menu-title", title))
    }

    /// One row of a menu, by the action it would perform.
    ///
    /// The action alone names the row because one menu is open at a time and
    /// no menu lists an action twice — held by
    /// `crate::menu::tests::no_action_is_listed_twice_within_one_menu`. (A few
    /// actions, `Keyboard Shortcuts…` among them, are reachable from two
    /// different menus; those two rows are never drawn together.)
    pub fn menu_item(action: crate::menu::MenuAction) -> egui::Id {
        egui::Id::new(("raster-menu-item", action))
    }

    /// One submenu opener, by the label it prints.
    pub fn menu_submenu(label: &str) -> egui::Id {
        egui::Id::new(("raster-menu-submenu", label))
    }

    /// One control of the tool-options bar.
    pub fn tool_option(tool: tools::ToolId, key: &'static str) -> egui::Id {
        egui::Id::new(("raster-tool-option", tool, key))
    }

    /// One entry inside a tool option's drop-down.
    pub fn tool_option_choice(tool: tools::ToolId, key: &'static str, index: usize) -> egui::Id {
        egui::Id::new(("raster-tool-option-choice", tool, key, index))
    }

    /// The Layers panel's blend-mode combo.
    pub fn layer_blend() -> egui::Id {
        egui::Id::new("raster-layer-blend-combo")
    }

    /// One entry inside the Layers panel's blend-mode combo.
    pub fn layer_blend_option(mode: layer_model::BlendMode) -> egui::Id {
        egui::Id::new(("raster-layer-blend-option", mode))
    }

    /// The Layers panel's Opacity slider row.
    pub fn layer_opacity() -> egui::Id {
        egui::Id::new("raster-layer-opacity")
    }

    /// The Layers panel's Fill slider row.
    pub fn layer_fill() -> egui::Id {
        egui::Id::new("raster-layer-fill")
    }

    /// One of the Layers panel's four lock toggles.
    pub fn layer_lock(lock: super::LockToggle) -> egui::Id {
        egui::Id::new(("raster-layer-lock", lock))
    }

    /// The Layers panel footer's "new layer" button.
    pub fn new_layer() -> egui::Id {
        egui::Id::new("raster-new-layer")
    }

    /// The Layers panel footer's "new group" button.
    pub fn new_group() -> egui::Id {
        egui::Id::new("raster-new-group")
    }

    /// The Layers panel footer's remaining buttons, each with the stable id a
    /// click test needs: link, fx, mask, adjustment, delete.
    pub fn layer_link() -> egui::Id {
        egui::Id::new("raster-layer-link")
    }

    pub fn layer_fx() -> egui::Id {
        egui::Id::new("raster-layer-fx")
    }

    pub fn layer_mask() -> egui::Id {
        egui::Id::new("raster-layer-mask")
    }

    pub fn layer_adjustment() -> egui::Id {
        egui::Id::new("raster-layer-adjustment")
    }

    pub fn layer_delete() -> egui::Id {
        egui::Id::new("raster-layer-delete")
    }

    /// The layer-kind filter row: one button per class.
    pub fn layer_filter(class: crate::menu::LayerClass) -> egui::Id {
        egui::Id::new(("raster-layer-filter", class))
    }

    /// The filter row's "every kind" button.
    pub fn layer_filter_all() -> egui::Id {
        egui::Id::new("raster-layer-filter-all")
    }

    /// The thumbnail-size cycle button.
    pub fn layer_thumb_size() -> egui::Id {
        egui::Id::new("raster-layer-thumb-size")
    }

    /// A collapsed dock's panel icon.
    pub fn rail_icon(panel: crate::dock::PanelId) -> egui::Id {
        egui::Id::new(("raster-rail-icon", panel))
    }

    /// A collapsed dock's expand chevron.
    pub fn rail_expand(side: crate::dock::DockSide) -> egui::Id {
        egui::Id::new(("raster-rail-expand", side))
    }

    /// One tile of the Adjustments panel.
    pub fn adjustment_tile(adjustment: crate::menu::AdjustmentId) -> egui::Id {
        egui::Id::new(("raster-adjustment-tile", adjustment))
    }

    /// One saved snapshot in the History panel, by its position in the list.
    pub fn history_snapshot(index: usize) -> egui::Id {
        egui::Id::new(("raster-history-snapshot", index))
    }
}

/// The icon key for a layer class, drawn in the Layers panel's thumbnail well.
///
/// A key into [`crate::icons::ui_icon`], not a symbol: `"▦"`, `"◐"`, `"◈"` and
/// `"✦"` are all absent from the font egui loads, so every well in the panel
/// was a tofu box.
pub(crate) const fn kind_icon(class: crate::menu::LayerClass) -> &'static str {
    use crate::menu::LayerClass as C;
    match class {
        C::Raster => "layer-raster",
        C::Group => "layer-group",
        C::Adjustment => "layer-adjustment",
        C::Text => "layer-text",
        C::Shape => "layer-shape",
        C::SmartObject => "layer-smart-object",
        C::Generator => "layer-generator",
    }
}

/// Which of the Layers panel's four lock toggles a button is.
///
/// Named rather than indexed: the row used to decide what a click meant from
/// the toggle's position in an array, so re-ordering the buttons would have
/// silently re-wired them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LockToggle {
    Transparency,
    Pixels,
    Position,
    All,
}

impl LockToggle {
    /// Every toggle, in the order the row draws them.
    pub const ALL: [LockToggle; 4] = [
        LockToggle::Transparency,
        LockToggle::Pixels,
        LockToggle::Position,
        LockToggle::All,
    ];

    /// The icon key and the tooltip this toggle draws.
    ///
    /// A *key* into [`crate::icons::ui_icon`], never a symbol: the four symbols
    /// this row used to type (`"▨"`, `"✎"`, `"✥"`, `"🔒"`) are not in the font
    /// egui loads, so the row was four tofu boxes.
    pub(crate) fn icon_and_tooltip(self) -> (&'static str, &'static str) {
        match self {
            LockToggle::Transparency => (
                "lock-transparency",
                crate::strings::tr("ui.mod.lock.transparent.pixels"),
            ),
            LockToggle::Pixels => ("lock-pixels", crate::strings::tr("ui.mod.lock.pixels")),
            LockToggle::Position => ("lock-position", crate::strings::tr("ui.mod.lock.position")),
            LockToggle::All => ("lock-all", crate::strings::tr("ui.mod.lock.all")),
        }
    }

    /// Read this toggle out of a layer's lock state.
    pub(crate) const fn get(self, locks: layer_model::LockState) -> bool {
        match self {
            LockToggle::Transparency => locks.transparency,
            LockToggle::Pixels => locks.pixels,
            LockToggle::Position => locks.position,
            LockToggle::All => locks.all,
        }
    }

    /// Write this toggle into a layer's lock state.
    pub(crate) fn set(self, locks: &mut layer_model::LockState, on: bool) {
        match self {
            LockToggle::Transparency => locks.transparency = on,
            LockToggle::Pixels => locks.pixels = on,
            LockToggle::Position => locks.position = on,
            LockToggle::All => locks.all = on,
        }
    }
}

/// Register `id` over `rect` so a headless test can find a control egui named
/// by call order.
///
/// Some widgets — a `Slider`, a `ComboBox`, a menu row — build their own id
/// from the call sequence and offer no way to set one. Marking them keeps
/// [`ids`]'s promise ("a stable id lets a headless test click exactly it")
/// without reimplementing the widget. The marker senses *hover only*, so it
/// never takes the click away from the control it marks: egui's hit test picks
/// the top-most widget that senses click or drag, and this one senses neither.
pub(crate) fn mark(ui: &mut Ui, rect: egui::Rect, id: egui::Id) {
    ui.interact(rect, id, Sense::hover());
}

/// What one frame of a [`text_field`] produced.
pub(crate) struct FieldEdit {
    /// The value to commit — `Some` on exactly the frame the edit finished.
    pub committed: Option<String>,
    /// What the field is showing right now, committed or not.
    pub text: String,
    /// `true` while the user is part-way through an edit.
    pub editing: bool,
    /// The field itself, so a caller can hang a tooltip on it.
    pub response: Response,
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
/// the edit away.
///
/// Every text field in the chrome — the panels, the inspector and the status
/// bar's zoom — is built on it. The dialogs deliberately are not: each either
/// binds a `String` its dialog struct owns across frames, or writes the edit
/// back on `changed()`, which fires on the frame the keystroke lands. Either
/// way the re-seeding above cannot drop a keystroke, so they need no memory
/// buffer. The status bar's zoom used to hand-roll the same memory buffer and
/// got it wrong in a way this shape cannot be wrong in: it only *created* the
/// `TextEdit` on the frame after the click, so the field appeared with no
/// focus and the keystrokes that followed fell through to the tool shortcuts.
/// A widget that is always drawn takes focus from the click that lands on it.
pub(crate) fn text_field(ui: &mut Ui, id: egui::Id, value: &str) -> FieldEdit {
    let width = current_tokens(ui).metrics.inspector_label_width;
    text_field_sized(ui, id, value, width)
}

/// [`text_field`] at a width of the caller's choosing, for the places an
/// inspector column would be too wide — the status bar's zoom, for one.
pub(crate) fn text_field_sized(ui: &mut Ui, id: egui::Id, value: &str, width: f32) -> FieldEdit {
    let key = id.with("in-progress");
    let stored = ui.memory(|m| m.data.get_temp::<String>(key));
    let was_editing = stored.is_some();
    let mut buffer = stored.unwrap_or_else(|| value.to_string());

    let t = current_tokens(ui);
    let response = ui.add_sized(
        Vec2::new(width, t.metrics.control_height),
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
        response,
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

/// The checker size a request of `cell` is actually painted at.
///
/// A floor is needed at all because a cell of zero asks for an unbounded
/// number of squares, and it is a *design* number rather than a magic one: a
/// checker finer than the hairline the rest of the chrome is drawn with cannot
/// be read as a checker, so the hairline is the smallest one worth painting.
pub(crate) fn checker_cell(ctx: &egui::Context, cell: f32) -> f32 {
    cell.max(design::current_theme(ctx).tokens().borders.hairline)
}

/// The transparency checkerboard, in the palette's own two neutrals so it
/// belongs to the theme rather than being a pair of hard-coded greys.
pub(crate) fn checkerboard(painter: &egui::Painter, rect: egui::Rect, cell: f32) {
    let cell = checker_cell(painter.ctx(), cell);
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

/// A compact square toggle that draws an icon, used for the eye, the lock and
/// the twirl-down. Reads its states from the theme like every other control.
///
/// `key` names a drawing in [`crate::icons::ui_icon`]. It used to be a symbol
/// typed into `Painter::text`, which is how the panel headers, the lock row and
/// the Adjustments grid all came out as tofu boxes: egui's default font stack
/// has no `"▸"`, no `"✕"`, no `"⋯"`.
pub(crate) fn icon_toggle(ui: &mut Ui, key: &str, on: bool, tooltip: &str) -> Response {
    icon_toggle_id(ui, key, on, tooltip, None)
}

/// [`icon_toggle`] with an explicit id, for the toggles a headless test needs
/// to find. See [`ids`].
pub(crate) fn icon_toggle_id(
    ui: &mut Ui,
    key: &str,
    on: bool,
    tooltip: &str,
    id: Option<egui::Id>,
) -> Response {
    crate::icons::ui_icon_button_id(
        ui,
        key,
        tooltip,
        if on {
            TextRole::Primary
        } else {
            TextRole::Disabled
        },
        id,
    )
}

/// Draw `key` centred in `rect`, in the palette's colour for `role`.
///
/// The chrome's name for [`crate::icons::paint_ui_icon`], which is where the
/// drawings and the unknown-key rule live.
pub(crate) fn paint_icon(ui: &Ui, rect: egui::Rect, key: &str, role: TextRole) {
    crate::icons::paint_ui_icon(ui, rect, key, role);
}

/// A compact icon button with an explicit id and a painted disabled state — the
/// [`labelled_button`] shape for an affordance that is a picture, not a word.
pub(crate) fn icon_button_id(ui: &mut Ui, key: &str, enabled: bool, id: egui::Id) -> Response {
    let t = current_tokens(ui);
    let side = t.metrics.min_hit_target;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    if ui.is_rect_visible(rect) {
        if enabled && response.hovered() {
            let radius = Radius::Small.resolve(&t.radii, side);
            ui.painter().rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::ControlFillHovered)),
            );
        }
        paint_icon(
            ui,
            rect,
            key,
            if enabled {
                TextRole::Primary
            } else {
                TextRole::Disabled
            },
        );
    }
    response
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shipping `TextEdit::singleline` in the crate, as
    /// `(relative path, line number)`. Doc comments are skipped — this file's
    /// own comment quotes the broken shape — and so is everything from the
    /// first `#[cfg(test)]` on, because a test may stand up a scratch field.
    fn shipping_singleline_sites() -> Vec<(String, usize)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let entries =
                std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        assert!(
            files.len() >= 10,
            "the crate lost its source files: found {}",
            files.len()
        );
        files.sort();

        let mut found = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let shipping = match text.find("#[cfg(test)]") {
                Some(at) => &text[..at],
                None => &text[..],
            };
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            for (n, line) in shipping.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains("TextEdit::singleline") || code.contains("text_edit_singleline") {
                    found.push((rel.clone(), n + 1));
                }
            }
        }
        found
    }

    #[test]
    fn the_chrome_builds_every_text_field_on_the_shared_helper() {
        // `text_field`'s doc comment claims the chrome routes through it and
        // that only the dialogs hand-roll a field. A claim about the code that
        // nothing checks is how that comment went stale before.
        let stray: Vec<_> = shipping_singleline_sites()
            .into_iter()
            .filter(|(file, _)| !file.starts_with("dialogs/") && file != "view/mod.rs")
            .collect();
        assert!(
            stray.is_empty(),
            "chrome must use view::text_field, not a raw TextEdit::singleline: {stray:?}"
        );
    }

    #[test]
    fn the_helper_is_the_only_raw_singleline_outside_the_dialogs() {
        // The other half of the same claim: `view/mod.rs` is exempt above only
        // because it *is* the helper, so it must hold exactly one such call.
        // Were a second field hand-rolled here, the exemption would hide it.
        let here: Vec<_> = shipping_singleline_sites()
            .into_iter()
            .filter(|(file, _)| file == "view/mod.rs")
            .collect();
        assert_eq!(
            here.len(),
            1,
            "view/mod.rs should hold only text_field_sized's own TextEdit: {here:?}"
        );
    }

    #[test]
    fn the_dialog_exemption_is_not_vacuous() {
        // The gate above exempts `dialogs/` by string prefix, on a path this
        // helper normalises from the platform separator. If that normalisation
        // broke — or the walk stopped reaching the directory — the prefix would
        // match nothing, every dialog field would read as chrome, and the gate
        // would fail loudly rather than silently. Pin the other direction too:
        // the comment's claim that the dialogs hand-roll their fields is only
        // true while the scan can actually see them.
        let dialogs: Vec<_> = shipping_singleline_sites()
            .into_iter()
            .filter(|(file, _)| file.starts_with("dialogs/"))
            .collect();
        assert!(
            !dialogs.is_empty(),
            "the scan found no dialog text fields, so the exemption proves nothing"
        );
    }

    #[test]
    fn the_checkerboards_floor_is_a_design_token_and_not_a_magic_number() {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let hairline = design::current_theme(&ctx).tokens().borders.hairline;

        // A degenerate request is lifted to the token…
        assert_eq!(checker_cell(&ctx, 0.0), hairline);
        assert_eq!(checker_cell(&ctx, -1.0), hairline);
        // …to the token, and not to the pixel count this used to hard-code.
        assert_ne!(
            hairline, 2.0,
            "pick a floor the gate can tell apart from the old literal"
        );
        assert_ne!(checker_cell(&ctx, 0.0), 2.0);
        // …and it is a floor, so a legible cell is left alone.
        assert_eq!(checker_cell(&ctx, Space::XSmall.pt()), Space::XSmall.pt());
    }

    #[test]
    fn a_lock_toggle_reads_and_writes_only_its_own_flag() {
        let mut locks = layer_model::LockState::default();
        for toggle in LockToggle::ALL {
            assert!(!toggle.get(locks));
            toggle.set(&mut locks, true);
            assert!(toggle.get(locks));
            for other in LockToggle::ALL.iter().filter(|o| **o != toggle) {
                assert!(!other.get(locks), "{toggle:?} also set {other:?}");
            }
            toggle.set(&mut locks, false);
        }
        assert_eq!(locks, layer_model::LockState::default());
    }
}
