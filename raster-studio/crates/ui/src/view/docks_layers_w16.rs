//! W16-D: the Layers panel's Photopea behaviours, drawn — the effects list
//! under a styled layer's row, the double-click routes (row → Layer Style,
//! thumbnail → the layer's own editor), Alt-click solo, drops on the trash
//! and the panel-options menu. The rules live in
//! [`crate::panels::layers::w16`]; this is where they meet the pointer.

use super::*;
use crate::menu::{EffectSlot, LayerClass, MenuAction};
use crate::panels::layers::w16::{self, ids, OptionsItem};

/// Whether `response` is a double-click whose first click landed on the
/// same control, with no Shift or Ctrl held. egui reports a double-click
/// for any second click inside its window, wherever the first one was, so a
/// quick click on one row and then another (or a Shift-click range) would
/// otherwise open Layer Style. Call it once per control per frame: it also
/// records a click on this control as the first of a possible pair.
pub(super) fn double_click_on(ui: &Ui, response: &egui::Response) -> bool {
    let key = egui::Id::new("raster-layers-last-click");
    let previous: Option<egui::Id> = ui.data(|d| d.get_temp(key));
    let modifiers = ui.input(|i| i.modifiers);
    let double = response.double_clicked()
        && previous == Some(response.id)
        && !modifiers.shift
        && !modifiers.command;
    if response.clicked() {
        ui.data_mut(|d| d.insert_temp(key, response.id));
    }
    double
}

/// Select `layer` alone, the way a click on its row does, so the intent that
/// follows acts on it.
fn select(w: &mut Workspace, layer: LayerId) {
    w.layers.select_only(layer);
    w.emit(Intent::SelectLayers {
        layers: vec![layer],
        active: Some(layer),
    });
}

/// A double-click on a layer row away from its name: Photopea opens Layer
/// Style (its Blending Options page) for every kind of layer but text,
/// which it enters for editing instead.
pub(super) fn row_double_click(w: &mut Workspace, row: &LayerRow) {
    if row.class == LayerClass::Text {
        w.emit(Intent::EnterTextLayer { layer: row.id });
        return;
    }
    select(w, row.id);
    w.emit(Intent::Action(MenuAction::BlendingOptions));
}

/// A double-click on a layer's content thumbnail: a smart object opens its
/// contents, a fill or adjustment layer its own dialog (Properties for an
/// adjustment the host has no dialog for), a text layer enters editing, and
/// anything else opens Layer Style as the row does.
pub(super) fn thumbnail_double_click(w: &mut Workspace, row: &LayerRow) {
    match row.class {
        LayerClass::SmartObject => {
            select(w, row.id);
            w.emit(Intent::Action(MenuAction::EditSmartObjectContents));
        }
        LayerClass::Adjustment => {
            select(w, row.id);
            w.emit(Intent::Action(MenuAction::EditAdjustmentLayer));
        }
        _ => row_double_click(w, row),
    }
}

/// The eye of a layer row was clicked: Alt solos the layer (Photopea and
/// Photoshop), a plain click flips the layer's own visibility.
pub(super) fn eye_click(w: &mut Workspace, ui: &Ui, doc: &Document, row: &LayerRow) {
    if ui.input(|i| i.modifiers.alt) {
        if let Some(command) = w.layers.solo(doc, row.id) {
            w.emit(Intent::Document(command));
        }
    } else {
        w.emit(Intent::Document(LayersModel::set_visible(
            row.id,
            !row.visible,
        )));
    }
}

/// The row's fx badge, drawn as a toggle that folds and unfolds the effects
/// list under the row (Photopea's arrow at the right of a styled layer).
pub(super) fn fx_toggle(w: &mut Workspace, ui: &mut Ui, doc: &Document, row: &LayerRow) {
    if w.layers.effect_rows(doc, row.id).is_empty() {
        return;
    }
    let open = w.layers.effects_open(row.id);
    if icon_toggle_id(
        ui,
        if open {
            "chevron-down"
        } else {
            "chevron-right"
        },
        true,
        crate::strings::tr("ui.docks.layers.fx.toggle"),
        Some(ids::fx_toggle(row.id)),
    )
    .clicked()
    {
        w.layers.toggle_effects_open(row.id);
    }
    badge(ui, "fx", true);
}

/// Where the pointer is over the Layers footer's trash button, if it is.
pub(super) fn pointer_over_trash(ui: &Ui) -> bool {
    let Some(trash) = ui.ctx().read_response(crate::view::ids::layer_delete()) else {
        return false;
    };
    ui.input(|i| i.pointer.interact_pos())
        .is_some_and(|p| trash.rect.contains(p))
}

/// A layer row released over the trash: delete it — with the rest of the
/// selection when it is part of it, as Photopea deletes every dragged row.
pub(super) fn drop_layer_on_trash(w: &mut Workspace, doc: &Document, dragged: LayerId) {
    let targets = if w.layers.is_selected(dragged) {
        w.layers.selection().to_vec()
    } else {
        vec![dragged]
    };
    if let Some(command) = LayersModel::delete_selection(doc, &targets) {
        w.emit(Intent::Document(command));
        w.layers.clear_selection();
    }
}

/// An effects row released over the trash: the "Effects" row clears the
/// style, one effect's row deletes that effect.
pub(super) fn drop_effect_on_trash(
    w: &mut Workspace,
    doc: &Document,
    (layer, slot): (LayerId, Option<EffectSlot>),
) {
    let command = match slot {
        None => w.layers.clear_effects(doc, layer),
        Some(slot) => w.layers.delete_effect(doc, layer, slot),
    };
    if let Some(command) = command {
        w.emit(Intent::Document(command));
    }
}

/// While a layer or effects row is dragged over the trash, the trash says it
/// will take it.
pub(super) fn trash_cue(w: &Workspace, ui: &Ui, trash: egui::Rect) {
    if w.layers.dragging().is_none() && w.layers.effect_drag().is_none() {
        return;
    }
    let over = ui
        .input(|i| i.pointer.interact_pos())
        .is_some_and(|p| trash.contains(p));
    if over {
        let t = current_tokens(ui);
        ui.painter().rect_stroke(
            trash,
            rounding(Radius::Small.resolve(&t.radii, trash.height())),
            egui::Stroke::new(
                t.borders.thick,
                color32(t.palette.color(ColorRole::SelectionStroke)),
            ),
        );
    }
}

/// One effects-list row: its rectangle, an eye, and a label.
struct EffectsLine<'a> {
    id: egui::Id,
    eye_id: egui::Id,
    eye_on: bool,
    eye_tip: &'a str,
    label: String,
    dim: bool,
}

/// Draw one line of the effects list; answers the row's response and
/// whether its eye was clicked.
fn effects_line(ui: &mut Ui, indent: f32, line: EffectsLine<'_>) -> (egui::Response, bool) {
    let t = current_tokens(ui);
    let height = t.metrics.list_row_height;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let response = ui.interact(rect, line.id, Sense::click_and_drag());
    if response.hovered() && ui.is_rect_visible(rect) {
        ui.painter().rect_filled(
            rect,
            rounding(Radius::Medium.resolve(&t.radii, height)),
            color32(t.palette.color(ColorRole::ControlFillHovered)),
        );
    }
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(Space::XSmall.pt(), 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    content.add_space(indent);
    let eye = icon_toggle_id(
        &mut content,
        "eye",
        line.eye_on,
        line.eye_tip,
        Some(line.eye_id),
    )
    .clicked();
    let role = if line.dim {
        TextRole::Tertiary
    } else {
        TextRole::Secondary
    };
    let name = line.label.clone();
    content.label(text(&content, line.label, role, TypeRole::Footnote));
    // The line paints its own text, so the accessibility tree is told it.
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    (response, eye)
}

/// W16-D: the effects list under a styled layer's row, when unfolded:
/// the "Effects" line (its eye is the whole style) and one line per effect
/// with its own eye. A double-click opens Layer Style — on the effect's own
/// page for an effect line — and a drag to the trash deletes (the whole
/// style from the "Effects" line).
pub(super) fn effect_rows(w: &mut Workspace, ui: &mut Ui, doc: &Document, row: &LayerRow) {
    let rows = w.layers.effect_rows(doc, row.id);
    if rows.is_empty() || !w.layers.effects_open(row.id) {
        return;
    }
    let style_on = doc.layers.get(row.id).is_some_and(|l| l.effects.enabled);
    let t = current_tokens(ui);
    let indent = (row.depth as f32 + 1.0) * Space::Medium.pt() + t.metrics.min_hit_target;
    let (header, eye) = effects_line(
        ui,
        indent,
        EffectsLine {
            id: ids::effects_row(row.id),
            eye_id: ids::effects_eye(row.id),
            eye_on: style_on,
            eye_tip: crate::strings::tr("ui.docks.layers.effects.eye"),
            label: crate::strings::tr("ui.docks.layers.effects").to_string(),
            dim: !style_on,
        },
    );
    if eye {
        if let Some(command) = LayersModel::set_effects_enabled(doc, row.id, !style_on) {
            w.emit(Intent::Document(command));
        }
    }
    if header.drag_started() {
        w.layers.begin_effect_drag(row.id, None);
    }
    if double_click_on(ui, &header) {
        select(w, row.id);
        w.emit(Intent::Action(MenuAction::BlendingOptions));
    } else if header.clicked() {
        select(w, row.id);
    }
    header.on_hover_text(crate::strings::tr("ui.docks.layers.effects.tip"));
    let indent = indent + Space::Medium.pt();
    for line in rows {
        let (response, eye) = effects_line(
            ui,
            indent,
            EffectsLine {
                id: ids::effect_row(row.id, line.slot),
                eye_id: ids::effect_eye(row.id, line.slot),
                eye_on: line.on,
                eye_tip: crate::strings::tr("ui.docks.layers.effect.eye"),
                label: w16::effect_kind(line.slot).label().to_string(),
                dim: !line.on || !style_on,
            },
        );
        if eye {
            if let Some(command) = w
                .layers
                .set_effect_visible(doc, row.id, line.slot, !line.on)
            {
                w.emit(Intent::Document(command));
            }
        }
        if response.drag_started() {
            w.layers.begin_effect_drag(row.id, Some(line.slot));
        }
        if double_click_on(ui, &response) {
            select(w, row.id);
            w.emit(Intent::Action(MenuAction::LayerStyle(line.slot)));
        } else if response.clicked() {
            select(w, row.id);
        }
        response.on_hover_text(crate::strings::tr("ui.docks.layers.effects.tip"));
    }
}

/// The label a panel-options row shows.
fn options_label(item: OptionsItem) -> &'static str {
    match item {
        OptionsItem::AddCopy => crate::strings::tr("ui.docks.layers.options.add.copy"),
        OptionsItem::Smaller | OptionsItem::Larger => {
            crate::strings::tr("ui.docks.layers.options.thumb.size")
        }
        OptionsItem::ByLayer => crate::strings::tr("ui.docks.layers.options.by.layer"),
        OptionsItem::ByDocument => crate::strings::tr("ui.docks.layers.options.by.document"),
    }
}

/// The panel-options button, in the filter row: opens Photopea's Layers
/// panel menu.
pub(super) fn options_button(w: &mut Workspace, ui: &mut Ui) {
    let open = w.layers.options_open;
    if icon_action_id(
        ui,
        "chevron-down",
        crate::strings::tr("ui.docks.layers.options"),
        ActionState::selected_if(open),
        Some(ids::options_button()),
    )
    .clicked()
    {
        w.layers.options_open = !open;
        w.layers.options_fresh = !open;
    }
}

/// Photopea's Layers panel menu, floating under its button while open:
/// Add "copy" to copied layers, − / + Thumbnail Size (the last "−" step
/// is no thumbnails), Thumbnails by Layer, Thumbnails by Document. A click
/// applies the row and closes the menu, as Photopea's does; Escape or a
/// click elsewhere closes it.
pub(super) fn options_menu(w: &mut Workspace, ui: &mut Ui) {
    if !w.layers.options_open {
        return;
    }
    let Some(button) = ui.ctx().read_response(ids::options_button()) else {
        return;
    };
    let fresh = std::mem::take(&mut w.layers.options_fresh);
    let t = current_tokens(ui);
    let row_h = t.metrics.control_height;
    let side = t.metrics.min_hit_target * 0.75;
    let mut picked = None;
    let area = egui::Area::new(egui::Id::new("raster-layers-options-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(button.rect.left_bottom())
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for item in OptionsItem::ALL {
                    let enabled = item.enabled(&w.layers);
                    let width = ui
                        .available_width()
                        .max(t.metrics.inspector_label_width * 2.0);
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::hover());
                    let sense = if enabled {
                        Sense::click()
                    } else {
                        Sense::hover()
                    };
                    let response = ui.interact(rect, ids::options_item(item), sense);
                    if enabled && response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            egui::Rounding::ZERO,
                            color32(t.palette.color(ColorRole::ControlFillHovered)),
                        );
                    }
                    let role = if enabled {
                        TextRole::Primary
                    } else {
                        TextRole::Disabled
                    };
                    // The mark column: a check for the on/off rows, the
                    // minus / plus of the two size rows.
                    let mark = egui::Rect::from_min_size(
                        egui::pos2(
                            rect.left() + Space::XSmall.pt(),
                            rect.center().y - side * 0.5,
                        ),
                        Vec2::splat(side),
                    );
                    match item {
                        OptionsItem::Smaller => super::super::paint_icon(ui, mark, "minus", role),
                        OptionsItem::Larger => super::super::paint_icon(ui, mark, "plus", role),
                        _ if item.checked(&w.layers) => {
                            super::super::paint_icon(ui, mark, "check", role)
                        }
                        _ => {}
                    }
                    let font = design::egui_theme::text_style(TypeRole::Body).resolve(ui.style());
                    ui.painter().text(
                        egui::pos2(mark.right() + Space::Small.pt(), rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        options_label(item),
                        font,
                        color32(t.palette.text(role)),
                    );
                    if enabled && response.clicked() {
                        picked = Some(item);
                    }
                }
            });
        });
    if let Some(item) = picked {
        item.apply(&mut w.layers);
        w.layers.options_open = false;
        return;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        w.layers.options_open = false;
    } else if !fresh && ui.input(|i| i.pointer.any_click()) {
        let pointer = ui.input(|i| i.pointer.interact_pos()).unwrap_or_default();
        if !area.response.rect.contains(pointer) && !button.rect.contains(pointer) {
            w.layers.options_open = false;
        }
    }
}

/// The content thumbnail's texture, drawn cropped to the layer's own bounds
/// under "Thumbnails by Layer" (the crop keeps its aspect inside the well),
/// or the whole document otherwise.
pub(super) fn paint_thumb(
    ui: &Ui,
    w: &Workspace,
    row: &LayerRow,
    tex: &egui::TextureHandle,
    rect: egui::Rect,
) {
    match w.layers.thumb_crop(row.id) {
        Some(uv) => {
            let [tw, th] = tex.size();
            let fitted = w16::fit_crop(uv, (tw as f32, th as f32), rect);
            ui.painter()
                .image(tex.id(), fitted, uv, crate::dialogs::controls::UNTINTED);
        }
        None => {
            ui.painter().image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                crate::dialogs::controls::UNTINTED,
            );
        }
    }
}
