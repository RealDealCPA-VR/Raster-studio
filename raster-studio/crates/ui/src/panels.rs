//! Individual egui panels.
//!
//! The UI is a *view*: panels read the [`Document`]/[`History`] and never mutate
//! them. Editing intent is emitted as [`Command`]s into an out-collection that
//! the app drains and runs through history, so undo/redo stays uniform.

use editor_core::{Command, Document, History, LayerPatch};
use layer_model::{Layer, LayerId};

/// Left dock: the layer stack, top-most first. Enables selection and emits
/// layer commands (add / delete / toggle visibility).
pub fn layers_panel(
    ctx: &egui::Context,
    doc: &Document,
    selected: &mut Option<LayerId>,
    out: &mut Vec<Command>,
) {
    // Drop a stale selection (e.g. the layer was deleted or undone away).
    if let Some(id) = *selected {
        if doc.layers.get(id).is_none() {
            *selected = None;
        }
    }

    egui::SidePanel::left("layers").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Layers");
            if ui.button("+").on_hover_text("Add raster layer").clicked() {
                out.push(Command::CreateLayer {
                    layer: Layer::raster("New Layer"),
                });
            }
            let can_delete = selected.is_some();
            if ui
                .add_enabled(can_delete, egui::Button::new("−").on_hover_text("Delete selected"))
                .clicked()
            {
                if let Some(id) = *selected {
                    out.push(Command::DeleteLayer { layer_id: id });
                    *selected = None;
                }
            }
        });
        ui.separator();

        for &id in doc.layers.root() {
            let Some(layer) = doc.layers.get(id) else { continue };
            let is_selected = *selected == Some(id);
            let label = format!(
                "{}{} — {:.0}%",
                if layer.visible { "👁" } else { "—" },
                layer.name,
                layer.opacity * 100.0
            );
            if ui.selectable_label(is_selected, label).clicked() {
                *selected = Some(id);
            }
            // Visibility toggle is also an edit: emit a command for it.
            if ui
                .small_button(if layer.visible { "hide" } else { "show" })
                .clicked()
            {
                out.push(Command::SetLayerProperties {
                    layer_id: id,
                    patch: LayerPatch {
                        visible: Some(!layer.visible),
                        ..Default::default()
                    },
                });
            }
        }

        if doc.layers.is_empty() {
            ui.weak("No layers yet. Add one with +");
        }
    });
}

/// Right dock: undo/redo state.
pub fn history_panel(ctx: &egui::Context, history: &History) {
    egui::SidePanel::right("history").show(ctx, |ui| {
        ui.heading("History");
        ui.separator();
        ui.label(match history.undo_label() {
            Some(l) => format!("Undo: {l}"),
            None => "Nothing to undo".to_string(),
        });
        ui.label(match history.redo_label() {
            Some(l) => format!("Redo: {l}"),
            None => "Nothing to redo".to_string(),
        });
    });
}

/// Bottom bar: document size / title.
pub fn status_bar(ctx: &egui::Context, doc: &Document) {
    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(&doc.meta.title);
            ui.separator();
            ui.label(format!("{} × {}", doc.width(), doc.height()));
            ui.separator();
            ui.label(format!("{} layers", doc.layers.len()));
        });
    });
}
