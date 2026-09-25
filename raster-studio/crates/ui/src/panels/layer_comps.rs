//! W10-B: the Layer Comps panel (photopea.com/learn/layer-comps).
//!
//! A layer comp is a named snapshot of every layer's visibility, position
//! and appearance (opacity, fill, blend mode, layer style), stored in the
//! document ([`layer_model::DocumentExtras::layer_comps`]) so it travels with
//! the file. The panel lists the comps; a click on a row applies it, and the
//! footer records a new comp, updates the one last applied from the layers as
//! they stand, deletes it, and steps to the previous or next comp.
//!
//! W16-E: as in Photopea, each row carries three flag toggles —
//! **Visibility**, **Position** and **Appearance** — and applying a comp puts
//! back only the aspects its flags name ([`apply_comp`]). At the top sits the
//! **Last Document State**: the layers as they stood before a comp was
//! applied from it (captured then, into
//! [`layer_model::DocumentExtras::last_document_state`]); a click on it puts
//! them back. Each row's marker shows which state is applied.
//!
//! Every button emits a finished [`editor_core::Command`], so each gesture
//! is one undo step and the panel never touches the document itself.
//!
//! # Strings
//!
//! Through the strings catalogue ([`crate::strings::tr`]): each constant
//! below is a catalogue key. The one English literal left is the default
//! name a new comp gets (`Layer Comp {n}`), which is stored in the document.

use design::{current_tokens, Space, TextRole};
use editor_core::extras;
use editor_core::{Command, Document, LayerPatch};
use egui::{Align, Layout, Ui};
use glam::Affine2;
use layer_model::doc_extras::CompFlags;
use layer_model::LayerComp;

use crate::intent::Intent;
use crate::strings::tr;
use crate::view::{
    body, empty_state, hairline, hint, icon_action_id, icon_toggle_id, list_row_layout,
    paint_panel_icon, ActionState,
};
use crate::Workspace;

const NO_DOCUMENT: &str = "ui.layer_comps.no_document";
const NO_COMPS: &str = "ui.layer_comps.none";
const NEW: &str = "ui.layer_comps.new";
const UPDATE: &str = "ui.layer_comps.update";
const DELETE: &str = "ui.layer_comps.delete";
const PREVIOUS: &str = "ui.layer_comps.previous";
const NEXT: &str = "ui.layer_comps.next";
const APPLIED: &str = "ui.layer_comps.applied";
const LAST_STATE: &str = "ui.w16.comps.last.state";
const LAST_STATE_NONE: &str = "ui.w16.comps.last.state.none";
const FLAG_VISIBILITY: &str = "ui.w16.comps.flag.visibility";
const FLAG_POSITION: &str = "ui.w16.comps.flag.position";
const FLAG_APPEARANCE: &str = "ui.w16.comps.flag.appearance";

/// Stable ids, so a headless test can click exactly the control it means.
pub mod ids {
    pub fn new() -> egui::Id {
        egui::Id::new("raster-layer-comps-new")
    }
    pub fn update() -> egui::Id {
        egui::Id::new("raster-layer-comps-update")
    }
    pub fn delete() -> egui::Id {
        egui::Id::new("raster-layer-comps-delete")
    }
    pub fn previous() -> egui::Id {
        egui::Id::new("raster-layer-comps-previous")
    }
    pub fn next() -> egui::Id {
        egui::Id::new("raster-layer-comps-next")
    }
    /// The row of comp `index`; a click applies the comp.
    pub fn row(index: usize) -> egui::Id {
        egui::Id::new(("raster-layer-comps-row", index))
    }
    /// W16-E: comp `index`'s flag toggle: 0 Visibility, 1 Position,
    /// 2 Appearance.
    pub fn flag(index: usize, flag: usize) -> egui::Id {
        crate::panels::panel_menus_w16::ids::comp_flag(index, flag)
    }
    /// W16-E: the Last Document State row.
    pub fn last_state() -> egui::Id {
        crate::panels::panel_menus_w16::ids::last_state_row()
    }
}

/// `Layer Comp <n>` for the lowest `n` from one past the comp count that no
/// comp is already called.
pub fn next_comp_name(doc: &Document) -> String {
    let comps = &doc.extras.layer_comps;
    let mut n = comps.len() + 1;
    loop {
        let name = format!("Layer Comp {n}");
        if !comps.iter().any(|c| c.name == name) {
            return name;
        }
        n += 1;
    }
}

/// W16-E: the layer edits that put back what `comp` recorded, limited to the
/// aspects `flags` name, for every layer that still exists. Only properties
/// that differ are patched; a fully locked layer keeps its state and a
/// position-locked layer keeps its position (the locks every edit obeys).
pub fn comp_patches(doc: &Document, comp: &LayerComp, flags: CompFlags) -> Vec<Command> {
    let mut commands = Vec::new();
    for state in &comp.layers {
        let Some(layer) = doc.layers.get(state.layer) else {
            continue;
        };
        if layer.locked.all {
            continue;
        }
        let mut patch = LayerPatch::default();
        let mut any = false;
        if flags.visibility && layer.visible != state.visible {
            patch.visible = Some(state.visible);
            any = true;
        }
        let transform_ok = state.transform.iter().all(|v| v.is_finite());
        if flags.position
            && transform_ok
            && layer.transform != Affine2::from_cols_array(&state.transform)
            && !layer.locked.blocks_transform()
        {
            patch.transform = Some(state.transform);
            any = true;
        }
        if flags.appearance {
            let opacity = state.opacity.clamp(0.0, 1.0);
            if state.opacity.is_finite() && layer.opacity != opacity {
                patch.opacity = Some(opacity);
                any = true;
            }
            let fill = state.fill_opacity.clamp(0.0, 1.0);
            if state.fill_opacity.is_finite() && layer.fill_opacity != fill {
                patch.fill_opacity = Some(fill);
                any = true;
            }
            if layer.blend_mode != state.blend_mode {
                patch.blend_mode = Some(state.blend_mode);
                any = true;
            }
            if layer.effects != state.effects {
                patch.effects = Some(Box::new(state.effects.clone()));
                any = true;
            }
        }
        if any {
            commands.push(Command::SetLayerProperties {
                layer_id: state.layer,
                patch,
            });
        }
    }
    commands
}

/// W16-E: Layer Comps > Apply comp `index`, honouring its flags, as one
/// undoable step. Applied from the Last Document State (no comp applied),
/// the layers as they stand are kept first as the new Last Document State,
/// so its row can put them back.
pub fn apply_comp(doc: &Document, index: usize) -> Option<Command> {
    let comp = doc.extras.layer_comps.get(index)?;
    let mut commands = comp_patches(doc, comp, comp.flags);
    let from_last_state = doc.extras.last_comp.is_none();
    let last = from_last_state.then(|| LayerComp::capture("Last Document State", &doc.layers));
    commands.push(extras::edit_extras(doc, |x| {
        x.last_comp = Some(index);
        if let Some(last) = last {
            x.last_document_state = Some(last);
        }
    }));
    Some(Command::Transaction {
        label: format!("Apply Layer Comp {}", comp.name),
        commands,
    })
}

/// W16-E: the Last Document State row: put every layer back as it stood
/// before a comp was applied (every aspect), and mark no comp applied.
/// `None` when it is already the state shown, or none was kept.
pub fn restore_last_state(doc: &Document) -> Option<Command> {
    doc.extras.last_comp?;
    let last = doc.extras.last_document_state.as_ref()?;
    let mut commands = comp_patches(doc, last, CompFlags::default());
    commands.push(extras::edit_extras(doc, |x| x.last_comp = None));
    Some(Command::Transaction {
        label: "Apply Last Document State".to_string(),
        commands,
    })
}

/// W16-E: Previous / Next through [`apply_comp`], wrapping round; with none
/// applied yet, Next starts at the first comp and Previous at the last.
pub fn step_comp(doc: &Document, forward: bool) -> Option<Command> {
    let count = doc.extras.layer_comps.len();
    if count == 0 {
        return None;
    }
    let index = match (doc.extras.last_comp.filter(|i| *i < count), forward) {
        (Some(i), true) => (i + 1) % count,
        (Some(i), false) => (i + count - 1) % count,
        (None, true) => 0,
        (None, false) => count - 1,
    };
    apply_comp(doc, index)
}

/// Layer Comps > Update: re-record comp `index` from the layers as they
/// stand. W16-E: the comp keeps its name, comment and flags (Photopea's
/// Update changes what is recorded, not which aspects apply).
pub fn update_comp(doc: &Document, index: usize) -> Option<Command> {
    let old = doc.extras.layer_comps.get(index)?;
    let mut comp = LayerComp::capture(old.name.clone(), &doc.layers);
    comp.comment = old.comment.clone();
    comp.flags = old.flags;
    Some(extras::edit_extras(doc, |x| {
        x.layer_comps[index] = comp;
        x.last_comp = Some(index);
    }))
}

/// W16-E: flip flag `flag` (0 Visibility, 1 Position, 2 Appearance) of comp
/// `index`, as one undo step.
pub fn toggle_flag(doc: &Document, index: usize, flag: usize) -> Option<Command> {
    doc.extras.layer_comps.get(index)?;
    if flag > 2 {
        return None;
    }
    Some(extras::edit_extras(doc, |x| {
        let flags = &mut x.layer_comps[index].flags;
        match flag {
            0 => flags.visibility = !flags.visibility,
            1 => flags.position = !flags.position,
            _ => flags.appearance = !flags.appearance,
        }
    }))
}

/// The applied marker: a check on the applied row, a dash elsewhere
/// (Photopea's button on the left of each row).
fn marker(ui: &mut Ui, applied: bool) {
    let t = current_tokens(ui);
    let side = t.metrics.min_hit_target;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    paint_panel_icon(
        ui,
        rect,
        if applied { "check" } else { "minus" },
        if applied {
            TextRole::Primary
        } else {
            TextRole::Tertiary
        },
    );
}

/// Draw the panel.
pub(crate) fn layer_comps_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let comps = &doc.extras.layer_comps;
    let applied = doc.extras.last_comp.filter(|i| *i < comps.len());
    // W16-E: the Last Document State, first.
    let last = list_row_layout(ui, ids::last_state(), applied.is_none(), |ui| {
        marker(ui, applied.is_none());
        ui.label(body(ui, tr(LAST_STATE)));
    });
    let last = if doc.extras.last_document_state.is_none() {
        last.response.on_hover_text(tr(LAST_STATE_NONE))
    } else {
        last.response
    };
    if last.clicked() {
        if let Some(command) = restore_last_state(doc) {
            w.emit(Intent::Document(command));
        }
    }
    if comps.is_empty() {
        ui.label(hint(ui, tr(NO_COMPS)));
    }
    for (index, comp) in comps.iter().enumerate() {
        let mut flip: Option<usize> = None;
        let row = list_row_layout(ui, ids::row(index), applied == Some(index), |ui| {
            marker(ui, applied == Some(index));
            ui.label(body(ui, comp.name.clone()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(Space::XSmall.pt());
                // Right to left: Appearance, Position, Visibility.
                for (flag, key, on, tip) in [
                    (2, "adjustment", comp.flags.appearance, FLAG_APPEARANCE),
                    (1, "target", comp.flags.position, FLAG_POSITION),
                    (0, "eye", comp.flags.visibility, FLAG_VISIBILITY),
                ] {
                    if icon_toggle_id(ui, key, on, tr(tip), Some(ids::flag(index, flag))).clicked()
                    {
                        flip = Some(flag);
                    }
                }
                if applied == Some(index) {
                    ui.label(crate::view::text(
                        ui,
                        tr(APPLIED),
                        TextRole::Secondary,
                        design::TypeRole::Caption,
                    ));
                }
            });
        });
        if let Some(flag) = flip {
            if let Some(command) = toggle_flag(doc, index, flag) {
                w.emit(Intent::Document(command));
            }
            continue;
        }
        let response = row.response.on_hover_text(if comp.comment.is_empty() {
            comp.name.clone()
        } else {
            comp.comment.clone()
        });
        if response.clicked() {
            if let Some(command) = apply_comp(doc, index) {
                w.emit(Intent::Document(command));
            }
        }
    }

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    let t = current_tokens(ui);
    let has_comps = !comps.is_empty();
    let on_applied = if applied.is_some() {
        ActionState::Idle
    } else {
        ActionState::Disabled
    };
    let stepping = if has_comps {
        ActionState::Idle
    } else {
        ActionState::Disabled
    };
    ui.allocate_ui_with_layout(
        egui::Vec2::new(ui.available_width(), t.metrics.control_height),
        Layout::left_to_right(Align::Center),
        |ui| {
            if icon_action_id(
                ui,
                "chevron-left",
                tr(PREVIOUS),
                stepping,
                Some(ids::previous()),
            )
            .clicked()
            {
                if let Some(command) = step_comp(doc, false) {
                    w.emit(Intent::Document(command));
                }
            }
            if icon_action_id(ui, "chevron-right", tr(NEXT), stepping, Some(ids::next())).clicked()
            {
                if let Some(command) = step_comp(doc, true) {
                    w.emit(Intent::Document(command));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_action_id(ui, "trash", tr(DELETE), on_applied, Some(ids::delete()))
                    .clicked()
                {
                    if let Some(command) =
                        applied.and_then(|index| extras::delete_layer_comp(doc, index))
                    {
                        w.emit(Intent::Document(command));
                    }
                }
                if icon_action_id(ui, "plus", tr(NEW), ActionState::Idle, Some(ids::new()))
                    .clicked()
                {
                    let name = next_comp_name(doc);
                    w.emit(Intent::Document(extras::new_layer_comp(doc, name)));
                }
                if icon_action_id(ui, "check", tr(UPDATE), on_applied, Some(ids::update()))
                    .clicked()
                {
                    if let Some(command) = applied.and_then(|index| update_comp(doc, index)) {
                        w.emit(Intent::Document(command));
                    }
                }
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W10-B: every string the panel shows is a catalogue key that resolves.
    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [
            NO_DOCUMENT,
            NO_COMPS,
            NEW,
            UPDATE,
            DELETE,
            PREVIOUS,
            NEXT,
            APPLIED,
            LAST_STATE,
            LAST_STATE_NONE,
            FLAG_VISIBILITY,
            FLAG_POSITION,
            FLAG_APPEARANCE,
        ] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }
    use layer_model::Layer;

    #[test]
    fn new_comp_names_skip_the_ones_in_use() {
        let mut doc = Document::new(8, 8, "t");
        assert_eq!(next_comp_name(&doc), "Layer Comp 1");
        editor_core::Command::create_layer(Layer::raster("A"))
            .apply(&mut doc)
            .unwrap();
        let comp = extras::new_layer_comp(&doc, "Layer Comp 2");
        comp.apply(&mut doc).unwrap();
        // One comp exists, called "Layer Comp 2": the next free name is 3.
        assert_eq!(next_comp_name(&doc), "Layer Comp 3");
    }
}
