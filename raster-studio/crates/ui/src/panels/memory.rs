//! W18-I: the Memory panel (Window ▸ Memory).
//!
//! What the active document holds in memory and the three purges that give
//! it back — Photopea's advice for a heavy session is "Clears the history
//! of editing of a document. Should lead to releasing some RAM memory", and
//! this panel puts the numbers beside the buttons that act on them:
//!
//! * **Image data**: the distinct pixel tiles the document uses, at its bit
//!   depth, uncompressed — the same figure as Document Info
//!   ([`crate::panels::doc_info::DocInfo`]), not the process's resident size;
//! * **History**: the undo and redo steps the active document keeps;
//! * **Clipboard**: whether the application's own clipboard holds pixels.
//!
//! **Purge Clipboard / Purge Histories / Purge All** raise Edit ▸ Purge's
//! own [`MenuAction::Purge`] rows (the application confirms a history purge,
//! which cannot be undone), greyed with the menu's own reasons.

use design::{Space, TextRole, TypeRole};
use editor_core::{Document, History};
use egui::Ui;

use crate::menu::{MenuAction, PurgeTarget, Resolution};
use crate::panels::doc_info::{megabytes, DocInfo};
use crate::strings::tr;
use crate::view::{empty_state, hairline, hint, labelled_button, text};
use crate::Workspace;

const NO_DOCUMENT: &str = "ui.w18.memory.no_document";
const IMAGE: &str = "ui.w18.memory.image";
const IMAGE_VALUE: &str = "ui.w18.memory.image.value";
const HISTORY: &str = "ui.w18.memory.history";
const HISTORY_VALUE: &str = "ui.w18.memory.history.value";
const CLIPBOARD: &str = "ui.w18.memory.clipboard";
const CLIPBOARD_PIXELS: &str = "ui.w18.memory.clipboard.pixels";
const CLIPBOARD_EMPTY: &str = "ui.w18.memory.clipboard.empty";
const PURGE_CLIPBOARD: &str = "ui.w18.memory.purge.clipboard";
const PURGE_HISTORIES: &str = "ui.w18.memory.purge.histories";
const PURGE_ALL: &str = "ui.w18.memory.purge.all";

/// Stable ids for a headless test.
pub mod ids {
    /// The value cell of row `index` in [`super::MemoryInfo::rows`] order.
    pub fn row(index: usize) -> egui::Id {
        egui::Id::new(("raster-memory-row", index))
    }
    /// The purge button for `target`.
    pub fn purge(target: crate::menu::PurgeTarget) -> egui::Id {
        egui::Id::new(("raster-memory-purge", target))
    }
}

/// Every figure the panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryInfo {
    /// Distinct pixel tiles in use, uncompressed, in bytes.
    pub image_bytes: u64,
    pub tiles: usize,
    pub undo_steps: usize,
    pub redo_steps: usize,
    pub clipboard_pixels: bool,
}

impl MemoryInfo {
    /// The figures for `doc` and its `history`.
    pub fn of(doc: &Document, history: &History, clipboard_pixels: bool) -> Self {
        let info = DocInfo::of(doc, 72.0);
        Self {
            image_bytes: info.memory_bytes,
            tiles: info.tiles,
            undo_steps: history.undo_depth(),
            redo_steps: history.redo_depth(),
            clipboard_pixels,
        }
    }

    /// The panel's rows, label key then value, top to bottom.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                IMAGE,
                tr(IMAGE_VALUE)
                    .replace("{size}", &megabytes(self.image_bytes))
                    .replace("{tiles}", &self.tiles.to_string()),
            ),
            (
                HISTORY,
                tr(HISTORY_VALUE)
                    .replace("{undo}", &self.undo_steps.to_string())
                    .replace("{redo}", &self.redo_steps.to_string()),
            ),
            (
                CLIPBOARD,
                tr(if self.clipboard_pixels {
                    CLIPBOARD_PIXELS
                } else {
                    CLIPBOARD_EMPTY
                })
                .to_string(),
            ),
        ]
    }
}

/// Draw the panel.
pub(crate) fn memory_body(w: &mut Workspace, ui: &mut Ui, doc: &Document, history: &History) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let context = w.menu_context(doc, history);
    let info = MemoryInfo::of(doc, history, context.clipboard.has_internal_pixels());
    egui::Grid::new("raster-memory-grid")
        .num_columns(2)
        .spacing(egui::vec2(Space::Small.pt(), Space::XSmall.pt()))
        .show(ui, |ui| {
            for (index, (key, value)) in info.rows().into_iter().enumerate() {
                ui.label(hint(ui, tr(key)));
                let cell = ui.label(text(ui, value, TextRole::Primary, TypeRole::Footnote));
                let _ = ui.interact(cell.rect, ids::row(index), egui::Sense::hover());
                ui.end_row();
            }
        });
    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    let mut fire = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
        for (target, label) in [
            (PurgeTarget::Clipboard, PURGE_CLIPBOARD),
            (PurgeTarget::Histories, PURGE_HISTORIES),
            (PurgeTarget::All, PURGE_ALL),
        ] {
            let resolution = MenuAction::Purge(target).resolve(&context);
            let response =
                labelled_button(ui, tr(label), resolution.is_enabled(), ids::purge(target));
            match resolution {
                Resolution::Enabled(intent) if response.clicked() => fire = Some(intent),
                Resolution::Disabled(reason) => {
                    response.on_hover_text(reason);
                }
                Resolution::Enabled(_) => {}
            }
        }
    });
    if let Some(intent) = fire {
        w.emit(intent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [
            NO_DOCUMENT,
            IMAGE,
            IMAGE_VALUE,
            HISTORY,
            HISTORY_VALUE,
            CLIPBOARD,
            CLIPBOARD_PIXELS,
            CLIPBOARD_EMPTY,
            PURGE_CLIPBOARD,
            PURGE_HISTORIES,
            PURGE_ALL,
        ] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }
}

/// W18-I: the Channels, Layer Comps, Timeline and Memory panels driven
/// through real frames of the workspace.
#[cfg(test)]
#[path = "w18_panel_tests.rs"]
mod w18_panel_tests;
