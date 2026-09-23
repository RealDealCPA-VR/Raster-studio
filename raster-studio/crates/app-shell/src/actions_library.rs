//! W4-I: the Actions panel's named library, and the per-frame syncs that
//! connect the Actions, Swatches and Brushes panels to the editor.
//!
//! A child module of [`crate::editor`] (declared there with `#[path]`), so it
//! extends [`Editor`] with the library without widening the editor's fields.
//!
//! # The library
//!
//! Every recording that captured something is kept when it stops, named
//! "Action N". The panel lists them, expands one to its steps (each step's
//! [`editor_core::Command::label`]), plays the selected one on the active
//! document through [`Editor::replay`], deletes one, and saves or loads the
//! whole library to `actions.json` beside the preferences. The file carries
//! the tile bytes each step's pixels need, so a loaded action replays in a
//! later session exactly as it did in the one that recorded it.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::panels::actions::{ActionSummary, ActionsRequest, ActionsView};

use super::{Editor, RecordedEdit};

/// One named action: a stopped recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedAction {
    pub name: String,
    pub edits: Vec<RecordedEdit>,
}

/// The actions file's shape.
#[derive(Serialize, Deserialize)]
struct ActionsFile {
    version: u32,
    actions: Vec<NamedAction>,
}

const ACTIONS_FILE_VERSION: u32 = 1;

impl Editor {
    /// The Actions library, oldest first.
    pub fn actions(&self) -> &[NamedAction] {
        &self.actions
    }

    /// Keep a stopped recording in the library, unless it captured nothing.
    pub(super) fn keep_recording(&mut self, edits: &[RecordedEdit]) {
        if edits.is_empty() {
            return;
        }
        let mut n = self.actions.len() + 1;
        while self.actions.iter().any(|a| a.name == format!("Action {n}")) {
            n += 1;
        }
        self.actions.push(NamedAction {
            name: format!("Action {n}"),
            edits: edits.to_vec(),
        });
    }

    /// Replay the action at `index` on the active document: the number of
    /// steps that applied, or `None` when there is no such action.
    pub fn play_action(&mut self, index: usize) -> Option<usize> {
        let edits = self.actions.get(index)?.edits.clone();
        Some(self.replay(&edits))
    }

    /// Remove the action at `index` from the library.
    pub fn delete_action(&mut self, index: usize) -> Option<NamedAction> {
        (index < self.actions.len()).then(|| self.actions.remove(index))
    }

    /// Where Save / Load keep the library.
    pub fn actions_file(&self) -> PathBuf {
        self.paths.root().join("actions.json")
    }

    /// Write the whole library to [`Self::actions_file`].
    pub fn save_actions(&self) -> io::Result<()> {
        self.paths.ensure()?;
        let file = ActionsFile {
            version: ACTIONS_FILE_VERSION,
            actions: self.actions.clone(),
        };
        let json = serde_json::to_string(&file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(self.actions_file(), json)
    }

    /// Read [`Self::actions_file`] and add every action whose name the
    /// library does not already hold. Answers how many were added.
    pub fn load_actions(&mut self) -> io::Result<usize> {
        let text = std::fs::read_to_string(self.actions_file())?;
        let file: ActionsFile = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut added = 0;
        for action in file.actions {
            if self.actions.iter().any(|a| a.name == action.name) {
                continue;
            }
            self.actions.push(action);
            added += 1;
        }
        Ok(added)
    }

    /// The library as the panel draws it.
    pub fn actions_view(&self) -> ActionsView {
        ActionsView {
            recording: self.is_recording(),
            actions: self
                .actions
                .iter()
                .map(|a| ActionSummary {
                    name: a.name.clone(),
                    steps: a.edits.iter().map(|e| e.command.label()).collect(),
                })
                .collect(),
        }
    }

    /// Perform the Actions panel's queued clicks, then publish the library
    /// for the panel to draw. Called once a frame by the chrome.
    pub fn sync_actions_panel(&mut self, ctx: &egui::Context) {
        for request in ui::panels::actions::take_requests(ctx) {
            match request {
                ActionsRequest::Play(i) => match self.play_action(i) {
                    Some(applied) => self.set_status(format!("Played {applied} step(s)")),
                    None => self.set_status("That action is no longer in the list"),
                },
                ActionsRequest::Delete(i) => {
                    if let Some(gone) = self.delete_action(i) {
                        self.set_status(format!("Deleted {}", gone.name));
                    }
                }
                ActionsRequest::Save => match self.save_actions() {
                    Ok(()) => self.set_status(format!(
                        "Saved {} action(s) to {}",
                        self.actions.len(),
                        self.actions_file().display()
                    )),
                    Err(e) => self.set_status(format!("Could not save actions: {e}")),
                },
                ActionsRequest::Load => match self.load_actions() {
                    Ok(n) => self.set_status(format!("Loaded {n} action(s)")),
                    Err(e) => self.set_status(format!("Could not load actions: {e}")),
                },
            }
        }
        self.actions_view().publish(ctx);
    }

    /// W4-I: keep the Swatches and Brushes panels and the preferences in
    /// step (see [`crate::prefs::Preferences::sync_panel_presets`]); a change
    /// the user made is written to the preferences file at once.
    pub fn sync_panel_presets(&mut self, w: &mut ui::Workspace) {
        if self.prefs.sync_panel_presets(w) {
            if let Err(e) = self.prefs.save(&self.paths.preferences_file()) {
                tracing::warn!("could not write the panel presets: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use editor_core::{Command, Selection};
    use glam::IVec2;

    fn png(dir: &std::path::Path, name: &str) -> PathBuf {
        let rgba = vec![200u8; 16 * 16 * 4];
        let bytes = raster::encode(raster::ExportFormat::Png, 16, 16, &rgba).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn editor(dir: &std::path::Path) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        )
    }

    fn rect() -> Selection {
        Selection::Rect {
            min: IVec2::new(2, 2),
            max: IVec2::new(9, 9),
        }
    }

    /// Record two steps, see them in the published view under a name, and
    /// play them through the panel's request queue onto another document.
    #[test]
    fn a_recording_of_two_steps_is_listed_with_its_steps_and_plays_from_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        ed.open_path(&png(dir.path(), "b.png")).unwrap();
        ed.activate(0).unwrap();

        ed.start_recording();
        ed.apply_command(Command::create_layer(layer_model::Layer::raster(
            "Recorded",
        )));
        ed.apply_command(Command::SetSelection { selection: rect() });
        let captured = ed.stop_recording().unwrap();
        assert_eq!(captured.len(), 2);

        let ctx = egui::Context::default();
        ed.sync_actions_panel(&ctx);
        let view = ActionsView::published(&ctx);
        assert!(!view.recording);
        assert_eq!(view.actions.len(), 1, "{view:?}");
        assert_eq!(view.actions[0].name, "Action 1");
        assert_eq!(
            view.actions[0].steps,
            vec![
                Command::create_layer(layer_model::Layer::raster("x")).label(),
                Command::SetSelection { selection: rect() }.label(),
            ]
        );

        ed.activate(1).unwrap();
        let before = ed.active().unwrap().document.layers.len();
        assert_eq!(ed.active().unwrap().document.selection, Selection::None);
        ui::panels::actions::request(&ctx, ActionsRequest::Play(0));
        ed.sync_actions_panel(&ctx);
        let doc = &ed.active().unwrap().document;
        assert_eq!(doc.layers.len(), before + 1, "the layer step replayed");
        assert_eq!(doc.selection, rect(), "the selection step replayed");
    }

    #[test]
    fn the_library_saves_and_loads_through_its_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        ed.start_recording();
        ed.apply_command(Command::SetSelection { selection: rect() });
        ed.stop_recording();
        // An empty recording is not kept.
        ed.start_recording();
        ed.stop_recording();
        assert_eq!(ed.actions().len(), 1);

        let ctx = egui::Context::default();
        ui::panels::actions::request(&ctx, ActionsRequest::Save);
        ed.sync_actions_panel(&ctx);
        assert!(ed.actions_file().exists());

        let mut other = editor(dir.path());
        assert_eq!(other.load_actions().unwrap(), 1);
        assert_eq!(other.actions()[0].name, "Action 1");
        assert_eq!(other.load_actions().unwrap(), 0, "a name is loaded once");

        ui::panels::actions::request(&ctx, ActionsRequest::Delete(0));
        other.sync_actions_panel(&ctx);
        assert!(other.actions().is_empty());
    }
}
