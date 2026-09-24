//! W10-B: the Layer Comps panel (photopea.com/learn/layer-comps).
//!
//! A layer comp is a named snapshot of every layer's visibility, position
//! and appearance (opacity, fill, blend mode, layer style), stored in the
//! document ([`layer_model::DocumentExtras::layer_comps`]) so it travels with
//! the file. The panel lists the comps; a click on a row applies it, and the
//! footer records a new comp, updates the one last applied from the layers as
//! they stand, deletes it, and steps to the previous or next comp.
//!
//! Every button emits a finished [`editor_core::Command`] built by
//! [`editor_core::extras`], so each gesture is one undo step and the panel
//! never touches the document itself.
//!
//! # Strings
//!
//! Through the strings catalogue ([`crate::strings::tr`]): each constant
//! below is a catalogue key. The one English literal left is the default
//! name a new comp gets (`Layer Comp {n}`), which is stored in the document.

use design::{current_tokens, Space, TextRole};
use editor_core::extras;
use editor_core::Document;
use egui::{Align, Layout, Ui};

use crate::intent::Intent;
use crate::strings::tr;
use crate::view::{
    body, empty_state, hairline, hint, icon_action_id, list_row_layout, ActionState,
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

/// Draw the panel.
pub(crate) fn layer_comps_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let comps = &doc.extras.layer_comps;
    let applied = doc.extras.last_comp.filter(|i| *i < comps.len());
    if comps.is_empty() {
        ui.label(hint(ui, tr(NO_COMPS)));
    }
    for (index, comp) in comps.iter().enumerate() {
        let row = list_row_layout(ui, ids::row(index), applied == Some(index), |ui| {
            ui.add_space(Space::XSmall.pt());
            ui.label(body(ui, comp.name.clone()));
            if applied == Some(index) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(Space::XSmall.pt());
                    ui.label(crate::view::text(
                        ui,
                        tr(APPLIED),
                        TextRole::Secondary,
                        design::TypeRole::Caption,
                    ));
                });
            }
        });
        let response = row.response.on_hover_text(if comp.comment.is_empty() {
            comp.name.clone()
        } else {
            comp.comment.clone()
        });
        if response.clicked() {
            if let Some(command) = extras::apply_layer_comp(doc, index) {
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
                if let Some(command) = extras::step_layer_comp(doc, false) {
                    w.emit(Intent::Document(command));
                }
            }
            if icon_action_id(ui, "chevron-right", tr(NEXT), stepping, Some(ids::next())).clicked()
            {
                if let Some(command) = extras::step_layer_comp(doc, true) {
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
                    if let Some(command) =
                        applied.and_then(|index| extras::update_layer_comp(doc, index))
                    {
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
