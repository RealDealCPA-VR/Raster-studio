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
//!
//! # Sets and `.atn` (W13-E)
//!
//! Every action belongs to a *set* ([`NamedAction::set`]; recordings join
//! the set chosen with [`ActionSetRequest::RecordInto`] or made last with
//! [`ActionSetRequest::NewSet`], else [`DEFAULT_SET`]). File > Open of an
//! `.atn` (or [`ActionSetRequest::ImportAtn`]) adds its set: each step is
//! kept parametric ([`NamedAction::imported`]) and played through
//! `atn_play`'s routes; a step this application has no equivalent for stays
//! listed, marked skipped with the reason, and Play reports it by name. A
//! step can be unchecked ([`NamedAction::off`]) and an action played from a
//! step. [`Editor::export_action_set`] writes a set back as `.atn`.

use std::io;
use std::path::PathBuf;

use asset_store::resources::atn::{self, AtnAction, AtnSet, AtnStep, StepOp};
use serde::{Deserialize, Serialize};
use ui::panels::actions::{
    ActionSetRequest, ActionSetSummary, ActionSetsView, ActionSummary, ActionsRequest, ActionsView,
    SetActionSummary, StepSummary,
};

use super::{Action, Editor, RecordedEdit};

#[path = "atn_play.rs"]
mod atn_play;

/// W13-E: the set a recording joins when none was chosen, and the set an
/// action saved before sets existed belongs to.
pub const DEFAULT_SET: &str = "Default Actions";

fn default_set() -> String {
    DEFAULT_SET.to_string()
}

/// One named action: a stopped recording, or (W13-E) an action an `.atn`
/// file brought in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedAction {
    pub name: String,
    pub edits: Vec<RecordedEdit>,
    /// W13-E: the set this action is listed under.
    #[serde(default = "default_set")]
    pub set: String,
    /// W13-E: parametric `.atn` steps, played after [`Self::edits`].
    #[serde(default)]
    pub imported: Vec<AtnStep>,
    /// W13-E: unchecked steps, as indices into [`Self::edits`] followed by
    /// [`Self::imported`]; Play passes over them.
    #[serde(default)]
    pub off: Vec<usize>,
}

impl NamedAction {
    /// How many steps the action lists: its recorded edits, then its
    /// imported steps.
    pub fn step_count(&self) -> usize {
        self.edits.len() + self.imported.len()
    }

    /// Whether step `i` is checked.
    pub fn is_on(&self, i: usize) -> bool {
        !self.off.contains(&i)
    }

    /// Each step as the panel lists it.
    pub fn step_summaries(&self) -> Vec<StepSummary> {
        let recorded = self.edits.iter().map(|e| (e.command.label(), None));
        let imported = self
            .imported
            .iter()
            .map(|s| (s.name.clone(), atn::interpret(s).err()));
        recorded
            .chain(imported)
            .enumerate()
            .map(|(i, (label, skipped))| StepSummary {
                label,
                enabled: self.is_on(i),
                skipped,
            })
            .collect()
    }
}

/// W13-E: what one Play did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayReport {
    /// Steps that ran (a recorded edit counts when it changed the document).
    pub applied: usize,
    /// Unchecked steps passed over.
    pub off: usize,
    /// `step name: why` for each step with no equivalent here.
    pub skipped: Vec<String>,
    /// `step name: why` for each step that ran and was refused.
    pub failed: Vec<String>,
}

impl PlayReport {
    /// The status line: what played and, by name, what did not.
    pub fn message(&self) -> String {
        let mut out = format!("Played {} step(s)", self.applied);
        if self.off > 0 {
            out.push_str(&format!(", {} unchecked", self.off));
        }
        if !self.skipped.is_empty() {
            out.push_str(&format!(
                "; skipped {} with no equivalent here: {}",
                self.skipped.len(),
                self.skipped.join("; ")
            ));
        }
        if !self.failed.is_empty() {
            out.push_str(&format!(
                "; {} failed: {}",
                self.failed.len(),
                self.failed.join("; ")
            ));
        }
        out
    }
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

/// W5-B: a loaded step's tile must be one this application stores — an RGBA8,
/// RGBA16 or (W10-H) RGBA f32 layer tile, or an 8-bit mask tile — filed under its own hash.
/// A file that says otherwise is refused whole: replaying it would put bytes
/// in the store no compositor path reads correctly.
fn check_tiles(action: &NamedAction) -> io::Result<()> {
    let sizes = [
        raster::Tile::byte_len(raster::PixelFormat::Rgba8),
        raster::Tile::byte_len(raster::PixelFormat::Rgba16),
        // W10-H: a 32 Bits/Channel document's `f32` layer tile.
        raster::Tile::byte_len(raster::PixelFormat::RgbaF32),
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
        let set = self
            .presets
            .recording_set()
            .map_or_else(default_set, str::to_string);
        self.actions.push(NamedAction {
            name: format!("Action {n}"),
            edits: edits.to_vec(),
            set,
            imported: Vec::new(),
            off: Vec::new(),
        });
    }

    /// Replay the action at `index` on the active document: the number of
    /// steps that applied, or `None` when there is no such action.
    pub fn play_action(&mut self, index: usize) -> Option<usize> {
        self.play_action_from(index, 0).map(|r| r.applied)
    }

    /// W13-E: play the action at `index` from step `from` on: recorded
    /// edits replay, imported steps run through their routes, unchecked
    /// steps are passed over, and a step with no equivalent here is
    /// reported by name, not dropped. `None` when there is no such action.
    pub fn play_action_from(&mut self, index: usize, from: usize) -> Option<PlayReport> {
        let action = self.actions.get(index)?.clone();
        let mut report = PlayReport::default();
        let recorded = action.edits.len();
        for i in from..action.step_count() {
            if !action.is_on(i) {
                report.off += 1;
                continue;
            }
            if i < recorded {
                report.applied += self.replay(std::slice::from_ref(&action.edits[i]));
                continue;
            }
            let step = &action.imported[i - recorded];
            match atn::interpret(step) {
                Err(why) => report.skipped.push(format!("{}: {why}", step.name)),
                Ok(op) => match self.perform_step_op(&op) {
                    Ok(_) => report.applied += 1,
                    Err(why) => report.failed.push(format!("{}: {why}", step.name)),
                },
            }
        }
        self.touch();
        Some(report)
    }

    /// W13-E: write the presets file, which keeps the set names.
    fn save_set_names(&self) {
        if let Err(e) = self.presets.save(&self.paths.presets_file()) {
            tracing::warn!("could not write the action set names: {e}");
        }
    }

    /// W13-E: the set names in panel order: the ones the presets list (a
    /// New Set with no action yet included), then any an action names that
    /// the list does not.
    pub fn action_sets(&self) -> Vec<String> {
        let mut sets: Vec<String> = self.presets.action_sets().to_vec();
        for a in &self.actions {
            if !sets.contains(&a.set) {
                sets.push(a.set.clone());
            }
        }
        sets
    }

    /// W13-E: a set name not yet in use, `base` or `base 2`, `base 3`...
    fn free_set_name(&self, base: &str) -> String {
        let sets = self.action_sets();
        let base = if base.trim().is_empty() { "Set" } else { base };
        let mut name = base.to_string();
        let mut n = 1;
        while sets.contains(&name) {
            n += 1;
            name = format!("{base} {n}");
        }
        name
    }

    /// W13-E: make an empty set (stopped recordings join it). Answers the
    /// name it got: `name`, or `name 2` when that is taken.
    pub fn new_action_set(&mut self, name: &str) -> String {
        let name = self.free_set_name(name);
        self.presets.define_action_set(&name);
        self.presets.set_recording_set(Some(name.clone()));
        self.save_set_names();
        name
    }

    /// W13-E: rename the set at `set`.
    pub fn rename_action_set(&mut self, set: usize, to: &str) -> Result<(), String> {
        let sets = self.action_sets();
        let from = sets
            .get(set)
            .ok_or("That set is no longer in the list")?
            .clone();
        if to.trim().is_empty() {
            return Err("A set needs a name".to_string());
        }
        if sets.iter().any(|s| s == to) {
            return Err(format!("There is already a set named \u{201c}{to}\u{201d}"));
        }
        self.presets.define_action_set(&from);
        self.presets.rename_action_set(&from, to);
        for a in self.actions.iter_mut().filter(|a| a.set == from) {
            a.set = to.to_string();
        }
        self.save_set_names();
        Ok(())
    }

    /// W13-E: remove the set at `set` and every action in it; answers its
    /// name and how many actions went with it.
    pub fn delete_action_set(&mut self, set: usize) -> Option<(String, usize)> {
        let name = self.action_sets().get(set)?.clone();
        let before = self.actions.len();
        self.actions.retain(|a| a.set != name);
        self.presets.remove_action_set(&name);
        self.save_set_names();
        Some((name, before - self.actions.len()))
    }

    /// W13-E: check or uncheck step `step` of the action at `action`;
    /// answers whether it is now checked.
    pub fn toggle_action_step(&mut self, action: usize, step: usize) -> Option<bool> {
        let a = self.actions.get_mut(action)?;
        if step >= a.step_count() {
            return None;
        }
        Some(match a.off.iter().position(|i| *i == step) {
            Some(at) => {
                a.off.remove(at);
                true
            }
            None => {
                a.off.push(step);
                false
            }
        })
    }

    /// W13-E: add an `.atn` set to the library under a free set name.
    /// Answers the status line: the set, its actions and steps, and how many
    /// steps will be skipped on play because nothing here performs them.
    pub fn import_action_set(&mut self, set: AtnSet, file: &str) -> String {
        let name = self.free_set_name(&set.name);
        self.presets.define_action_set(&name);
        self.save_set_names();
        let (mut steps, mut skipped) = (0, 0);
        let count = set.actions.len();
        for action in set.actions {
            steps += action.steps.len();
            skipped += action
                .steps
                .iter()
                .filter(|s| atn::interpret(s).is_err())
                .count();
            let off = action
                .steps
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.enabled)
                .map(|(i, _)| i)
                .collect();
            self.actions.push(NamedAction {
                name: action.name,
                edits: Vec::new(),
                set: name.clone(),
                imported: action.steps,
                off,
            });
        }
        let mut message = format!(
            "Loaded the action set \u{201c}{name}\u{201d} from {file}: {count} action(s), {steps} step(s)"
        );
        if skipped > 0 {
            message.push_str(&format!(
                "; {skipped} step(s) have no equivalent here and are skipped on play"
            ));
        }
        message
    }

    /// W13-E: the set at `set` as an `.atn` set. A recorded edit that is a
    /// new layer or a rectangle / empty selection becomes the Photoshop step
    /// for it; any other recorded edit has none and is left out, and the
    /// second value counts those.
    pub fn action_set_as_atn(&self, set: usize) -> Option<(AtnSet, usize)> {
        let name = self.action_sets().get(set)?.clone();
        let mut left_out = 0;
        let actions = self
            .actions
            .iter()
            .filter(|a| a.set == name)
            .map(|a| {
                let mut steps = Vec::new();
                for (i, edit) in a.edits.iter().enumerate() {
                    let op = match &edit.command {
                        editor_core::Command::CreateLayer { .. } => Some(StepOp::MakeLayer),
                        editor_core::Command::SetSelection {
                            selection: editor_core::Selection::None,
                        } => Some(StepOp::Deselect),
                        editor_core::Command::SetSelection {
                            selection: editor_core::Selection::Rect { min, max },
                        } => Some(StepOp::SelectRect {
                            left: f64::from(min.x),
                            top: f64::from(min.y),
                            right: f64::from(max.x),
                            bottom: f64::from(max.y),
                        }),
                        _ => None,
                    };
                    match op {
                        Some(op) => {
                            let mut step = op.to_step();
                            step.enabled = a.is_on(i);
                            steps.push(step);
                        }
                        None => left_out += 1,
                    }
                }
                for (j, step) in a.imported.iter().enumerate() {
                    let mut step = step.clone();
                    step.enabled = a.is_on(a.edits.len() + j);
                    steps.push(step);
                }
                AtnAction {
                    name: a.name.clone(),
                    steps,
                    ..AtnAction::default()
                }
            })
            .collect();
        Some((
            AtnSet {
                name,
                expanded: true,
                actions,
            },
            left_out,
        ))
    }

    /// W13-E: write the set at `set` to an `.atn` file the user picks.
    pub fn export_action_set(&mut self, set: usize) -> Result<String, String> {
        let (atn_set, left_out) = self
            .action_set_as_atn(set)
            .ok_or("That set is no longer in the list")?;
        let suggested = PathBuf::from(format!("{}.atn", atn_set.name));
        let Some(mut path) = self.dialogs.pick_export_path(&suggested) else {
            return Err("Export cancelled".to_string());
        };
        if path.extension().is_none() {
            path.set_extension("atn");
        }
        let bytes = atn::write(&atn_set).map_err(|e| e.to_string())?;
        crate::doc::write_atomically(&path, &bytes).map_err(|e| e.to_string())?;
        let mut message = format!(
            "Exported \u{201c}{}\u{201d} to {}",
            atn_set.name,
            path.display()
        );
        if left_out > 0 {
            message.push_str(&format!(
                " ({left_out} recorded step(s) have no Photoshop equivalent and were left out)"
            ));
        }
        Ok(message)
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
            if self
                .actions
                .iter()
                .any(|a| a.name == action.name && a.set == action.set)
            {
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
                    // W13-E: an action outside the default set is listed
                    // under its set's name.
                    name: if a.set == DEFAULT_SET {
                        a.name.clone()
                    } else {
                        format!("{} / {}", a.set, a.name)
                    },
                    steps: a
                        .step_summaries()
                        .into_iter()
                        .map(|s| {
                            let mut label = s.label;
                            if let Some(why) = s.skipped {
                                label = format!("{label} (skipped on play: {why})");
                            }
                            if !s.enabled {
                                label = format!("{label} (unchecked)");
                            }
                            label
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// W13-E: the library as Set -> Action -> Steps.
    pub fn action_sets_view(&self) -> ActionSetsView {
        let recording = self.presets.recording_set();
        ActionSetsView {
            sets: self
                .action_sets()
                .into_iter()
                .map(|set| ActionSetSummary {
                    recording_target: recording.map_or(set == DEFAULT_SET, |r| r == set),
                    actions: self
                        .actions
                        .iter()
                        .enumerate()
                        .filter(|(_, a)| a.set == set)
                        .map(|(index, a)| SetActionSummary {
                            index,
                            name: a.name.clone(),
                            steps: a.step_summaries(),
                        })
                        .collect(),
                    name: set,
                })
                .collect(),
        }
    }

    /// W13-E: perform one set-level request from the panel.
    fn perform_set_request(&mut self, request: ActionSetRequest) {
        match request {
            ActionSetRequest::NewSet(name) => {
                let name = self.new_action_set(&name);
                self.set_status(format!(
                    "Made the action set \u{201c}{name}\u{201d}; recordings join it"
                ));
            }
            ActionSetRequest::RenameSet { set, name } => match self.rename_action_set(set, &name) {
                Ok(()) => self.set_status(format!("Renamed the set to \u{201c}{name}\u{201d}")),
                Err(e) => self.set_status(e),
            },
            ActionSetRequest::DeleteSet(set) => match self.delete_action_set(set) {
                Some((name, n)) => self.set_status(format!(
                    "Deleted the set \u{201c}{name}\u{201d} and its {n} action(s)"
                )),
                None => self.set_status("That set is no longer in the list"),
            },
            ActionSetRequest::RecordInto(set) => match self.action_sets().get(set).cloned() {
                Some(name) => {
                    self.presets.define_action_set(&name);
                    self.presets.set_recording_set(Some(name.clone()));
                    self.save_set_names();
                    self.set_status(format!("Recordings join \u{201c}{name}\u{201d}"));
                }
                None => self.set_status("That set is no longer in the list"),
            },
            ActionSetRequest::ToggleStep { action, step } => {
                match self.toggle_action_step(action, step) {
                    Some(true) => self.set_status("Step checked: it plays"),
                    Some(false) => self.set_status("Step unchecked: Play passes over it"),
                    None => self.set_status("That step is no longer in the list"),
                }
            }
            ActionSetRequest::PlayFrom { action, step } => {
                match self.play_action_from(action, step) {
                    Some(report) => self.set_status(report.message()),
                    None => self.set_status("That action is no longer in the list"),
                }
            }
            ActionSetRequest::ExportSet(set) => match self.export_action_set(set) {
                Ok(message) | Err(message) => self.set_status(message),
            },
            ActionSetRequest::ImportAtn => {
                let Some(path) = self.dialogs.pick_open_file() else {
                    return;
                };
                if let Err(e) = self.open_resource(&path) {
                    self.set_status(e.to_string());
                }
            }
        }
    }

    /// Perform the Actions panel's queued clicks, then publish the library
    /// for the panel to draw. Called once a frame by the chrome.
    pub fn sync_actions_panel(&mut self, ctx: &egui::Context) {
        for request in ui::panels::actions::take_requests(ctx) {
            match request {
                ActionsRequest::Play(i) => match self.play_action_from(i, 0) {
                    Some(report) => self.set_status(report.message()),
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
        for request in ui::panels::actions::take_set_requests(ctx) {
            self.perform_set_request(request);
        }
        self.actions_view().publish(ctx);
        self.action_sets_view().publish(ctx);
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
        // W9-N: swatches (and a gradient) File > Open brought in from a
        // resource file join the panels after the saved list was restored;
        // the next frame's sync writes them to the preferences file.
        self.drain_panel_imports(w);
        // W10-G: the Preset Manager's swatches and tool presets, which live
        // on these panels (see `crate::edit_gaps::sync_workspace_presets`).
        crate::edit_gaps::sync_workspace_presets(w);
    }
}

/// W5-B: the save, journal-hold and restart routes that lose data when they
/// go wrong, driven through the editor.
#[cfg(test)]
#[path = "journal_hold_tests.rs"]
mod journal_hold_tests;

/// W13-E: `.atn` through File > Open, the chrome frame's Play, set editing
/// and Export.
#[cfg(test)]
#[path = "atn_route_tests.rs"]
mod atn_route_tests;

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
                set: DEFAULT_SET.to_string(),
                imported: Vec::new(),
                off: Vec::new(),
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
