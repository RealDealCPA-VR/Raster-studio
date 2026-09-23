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

/// Largest actions file [`Editor::load_actions`] reads: 256 MiB, checked from
/// the metadata before a byte is read. The file carries every tile a recorded
/// step painted, as JSON numbers, so a library of paint steps is large — but
/// not larger than this without being a file nobody meant to load.
pub const MAX_ACTIONS_FILE_BYTES: u64 = 256 << 20;

/// W5-B: a loaded step's tile must be one this application stores — an RGBA8
/// or RGBA16 layer tile, or an 8-bit mask tile — filed under its own hash.
/// A file that says otherwise is refused whole: replaying it would put bytes
/// in the store no compositor path reads correctly.
fn check_tiles(action: &NamedAction) -> io::Result<()> {
    let sizes = [
        raster::Tile::byte_len(raster::PixelFormat::Rgba8),
        raster::Tile::byte_len(raster::PixelFormat::Rgba16),
        editor_core::MASK_TILE_BYTES,
    ];
    for edit in &action.edits {
        for (hash, bytes) in &edit.tiles {
            let problem = if !sizes.contains(&bytes.len()) {
                format!("a tile of {} bytes is not a tile size", bytes.len())
            } else if raster::TileHash::of(bytes) != *hash {
                "a tile is filed under the wrong hash".to_string()
            } else {
                continue;
            };
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {problem}", action.name),
            ));
        }
    }
    Ok(())
}

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
        // W5-B: never truncated in place — a save that fails half way (a full
        // disk, a crash) must leave the previous library whole.
        crate::doc::write_atomically(&self.actions_file(), json.as_bytes())
    }

    /// Read [`Self::actions_file`] and add every action whose name the
    /// library does not already hold. Answers how many were added.
    ///
    /// W5-B: a file over [`MAX_ACTIONS_FILE_BYTES`] is refused before it is
    /// read, and one whose tiles are not tiles is refused whole
    /// ([`check_tiles`]).
    pub fn load_actions(&mut self) -> io::Result<usize> {
        use std::io::Read as _;
        let path = self.actions_file();
        let too_large = |size: u64| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the actions file is {size} bytes; at most {MAX_ACTIONS_FILE_BYTES} are read"
                ),
            )
        };
        let size = std::fs::metadata(&path)?.len();
        if size > MAX_ACTIONS_FILE_BYTES {
            return Err(too_large(size));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)?
            .take(MAX_ACTIONS_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_ACTIONS_FILE_BYTES {
            return Err(too_large(bytes.len() as u64));
        }
        let file: ActionsFile = serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        for action in &file.actions {
            check_tiles(action)?;
        }
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

/// W5-B: the save, journal-hold and restart routes that lose data when they
/// go wrong, driven through the editor.
#[cfg(test)]
#[path = "journal_hold_tests.rs"]
mod journal_hold_tests;

/// W5-B: Color Lookup's Load route, driven through the real chrome and
/// dialog host with the file dialog answered by a test.
#[cfg(test)]
#[path = "cube_load_route_tests.rs"]
mod cube_load_route_tests;

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

    fn library_with(tiles: Vec<(raster::TileHash, Vec<u8>)>) -> String {
        serde_json::to_string(&ActionsFile {
            version: ACTIONS_FILE_VERSION,
            actions: vec![NamedAction {
                name: "Loaded".to_string(),
                edits: vec![RecordedEdit {
                    command: Command::SetSelection { selection: rect() },
                    layer: None,
                    tiles,
                }],
            }],
        })
        .unwrap()
    }

    /// W5-B: Save replaced the file by truncating it in place, so a save
    /// that failed half way destroyed the library it was replacing. It now
    /// writes a sibling and renames it over: the old file's *contents* are
    /// never touched — which a second hard link to it observes.
    #[test]
    fn saving_the_library_replaces_the_file_rather_than_rewriting_it_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        ed.start_recording();
        ed.apply_command(Command::SetSelection { selection: rect() });
        ed.stop_recording();
        ed.paths.ensure().unwrap();
        let file = ed.actions_file();
        std::fs::write(&file, "the previous library").unwrap();
        let link = dir.path().join("link.json");
        std::fs::hard_link(&file, &link).unwrap();

        ed.save_actions().unwrap();
        assert_eq!(
            std::fs::read_to_string(&link).unwrap(),
            "the previous library",
            "the old file was rewritten in place"
        );
        let mut other = editor(dir.path());
        assert_eq!(other.load_actions().unwrap(), 1);
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// W5-B: the file is size-checked from its metadata before it is read.
    #[test]
    fn an_actions_file_over_the_cap_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.paths.ensure().unwrap();
        // Sparse, and twice the cap: only the metadata knows this size — the
        // bounded read stops at the cap and would name that instead.
        let size = MAX_ACTIONS_FILE_BYTES * 2;
        let f = std::fs::File::create(ed.actions_file()).unwrap();
        f.set_len(size).unwrap();
        drop(f);
        let err = ed.load_actions().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string()
                .contains(&format!("the actions file is {size} bytes")),
            "not refused from the metadata: {err}"
        );
    }

    /// W5-B: a tile that is not an RGBA8/RGBA16 layer tile or a mask tile,
    /// or that is filed under another hash, refuses the whole file; real
    /// tiles of every stored size load.
    #[test]
    fn a_loaded_action_whose_tiles_are_not_tiles_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.paths.ensure().unwrap();

        let odd = vec![1u8, 2, 3, 4, 5];
        std::fs::write(
            ed.actions_file(),
            library_with(vec![(raster::TileHash::of(&odd), odd)]),
        )
        .unwrap();
        let err = ed.load_actions().unwrap_err();
        assert!(err.to_string().contains("not a tile size"), "{err}");
        assert!(ed.actions().is_empty());

        let t8 = vec![7u8; raster::Tile::byte_len(raster::PixelFormat::Rgba8)];
        std::fs::write(
            ed.actions_file(),
            library_with(vec![(raster::TileHash([0; 32]), t8.clone())]),
        )
        .unwrap();
        let err = ed.load_actions().unwrap_err();
        assert!(err.to_string().contains("wrong hash"), "{err}");

        let t16 = raster::widen_rgba8_tile(&t8).unwrap();
        let mask = vec![9u8; editor_core::MASK_TILE_BYTES];
        let good = [t8, t16, mask]
            .into_iter()
            .map(|b| (raster::TileHash::of(&b), b))
            .collect();
        std::fs::write(ed.actions_file(), library_with(good)).unwrap();
        assert_eq!(ed.load_actions().unwrap(), 1);
    }
}
