//! Workspace UI: panels, docks, tool options (egui).
//!
//! The UI is a *view* over `editor-core`. It reads the [`Document`] and emits
//! intent as [`Command`]s; it never mutates the document directly — all edits
//! go through commands + history so undo/redo is uniform. Kept thin in Phase 0;
//! high-value visual areas can later be replaced with custom rendering.

use editor_core::{Command, Document, History};
use layer_model::LayerId;

pub mod panels;

/// Aggregates the panels that make up the editor workspace and draws them for
/// one frame, given the current document + history. Editing intent is queued
/// onto `commands` and drained by the app via [`Workspace::drain_commands`].
pub struct Workspace {
    pub show_layers: bool,
    pub show_history: bool,
    pub show_tool_options: bool,
    /// The layer selected in the layers panel, if any.
    pub selected: Option<LayerId>,
    /// Commands panels emitted this frame; drained from [`Self::drain_commands`].
    commands: Vec<Command>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            show_layers: true,
            show_history: true,
            show_tool_options: true,
            selected: None,
            commands: Vec::new(),
        }
    }
}

impl Workspace {
    /// Draw all enabled panels. `ctx` is the egui context for the frame.
    pub fn ui(&mut self, ctx: &egui::Context, doc: &Document, history: &History) {
        if self.show_layers {
            panels::layers_panel(ctx, doc, &mut self.selected, &mut self.commands);
        }
        if self.show_history {
            panels::history_panel(ctx, history);
        }
        panels::status_bar(ctx, doc);
    }

    /// Take the commands panels emitted this frame. The app applies these
    /// through [`History`] so undo/redo stays uniform (the UI never mutates the
    /// document directly).
    pub fn drain_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }
}
