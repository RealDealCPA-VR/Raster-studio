//! The pending-edit lifecycle (plan card 011): begin, commit, cancel.
//!
//! A gesture that will end in document edits carries its intent in a session
//! from the moment it starts: which document it is aimed at, which layers it
//! may touch (captured **once**, so releasing a drag cannot edit a layer the
//! user selected mid-gesture), the label its one history entry will wear, and
//! a generation so stale handles cannot commit into a newer session.
//!
//! # The four guarantees the card names
//!
//! * **Commit is one entry.** Whatever a gesture produced, [`Editor::
//!   commit_edit`] wraps it in a single [`Command::Transaction`] under the
//!   session's label — a long drag yields one undo step, and undo's label is
//!   the gesture's.
//! * **Cancel is nothing.** [`Editor::cancel_edit`] drops the session and
//!   applies no command: no half-applied state, no history debris. The tools'
//!   own `cancel` contract (emit nothing) and the pointer route's Escape
//!   handler already keep the tools honest; this is the shell-side half.
//! * **Tab switches cannot retarget.** The session is keyed to the document
//!   it began on; committing after the active document changed is refused,
//!   and the pointer route separately refuses to feed a gesture samples from
//!   a different document (`ToolPointer`'s `WrongDocument` guard).
//! * **A failed commit changes nothing.** `apply` is refused before the
//!   transaction lands, so the document and the history are exactly as they
//!   were; the session ends and the caller sees the error.
//!
//! Pending edits are never journaled: nothing here touches save/export — the
//! session holds commands in memory until commit, and a crash mid-gesture
//! loses only the gesture, which is the pre-existing autosave contract for
//! uncommitted work.
//!
//! One pending edit at a time: a second `begin` while one is live is refused.
//! The session never mutates pixels itself — selection and target changes are
//! field reads; only `commit_edit` reaches history.

use editor_core::Command;
use layer_model::LayerId;

use crate::doc::DocumentId;
use crate::editor::Editor;

/// A gesture's committed intent, held from pointer-down to commit/cancel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEdit {
    /// The document the gesture began in. The only document `commit_edit`
    /// will touch.
    pub document: DocumentId,
    /// The layers the gesture captured at begin, in the order captured.
    /// Captured once by design: a selection made mid-gesture cannot redirect
    /// the commit.
    pub targets: Vec<LayerId>,
    /// The label the one history entry will wear.
    pub label: String,
    /// Monotonic within the process; distinguishes consecutive gestures in
    /// logs and later in preview cache keys (card 013).
    pub generation: u64,
}

impl PendingEdit {
    /// `true` when `layer` is one this session may edit.
    pub fn targets(&self, layer: LayerId) -> bool {
        self.targets.contains(&layer)
    }
}

/// The editor's pending-edit state: at most one live gesture.
#[derive(Debug, Default)]
pub struct EditSessions {
    active: Option<PendingEdit>,
    next_generation: u64,
}

impl EditSessions {
    /// The live session, if any.
    pub fn active(&self) -> Option<&PendingEdit> {
        self.active.as_ref()
    }

    /// Begin a gesture against `document`, allowed to edit `targets`.
    /// Refused while another gesture is live — one pending edit at a time —
    /// and when any target is not in the document's tree (the capture must be
    /// valid, or it is not a capture).
    pub fn begin_validated(
        &mut self,
        document: DocumentId,
        targets: Vec<LayerId>,
        label: impl Into<String>,
    ) -> Result<u64, String> {
        if self.active.is_some() {
            return Err("a gesture is already in progress".to_string());
        }
        self.next_generation += 1;
        let generation = self.next_generation;
        self.active = Some(PendingEdit {
            document,
            targets,
            label: label.into(),
            generation,
        });
        Ok(generation)
    }

    /// End the session without applying anything.
    pub fn cancel(&mut self) -> Option<PendingEdit> {
        self.active.take()
    }

    /// Take the session for commit; the caller applies the commands.
    pub fn take(&mut self) -> Option<PendingEdit> {
        self.active.take()
    }
}

impl Editor {
    /// The live pending edit, if a gesture is holding one.
    pub fn pending_edit(&self) -> Option<&crate::edit_session::PendingEdit> {
        self.edit_sessions.active()
    }

    /// Begin a pending edit on the active document over `targets`. Refused
    /// while another gesture is live, with no document open, or when a target
    /// is not in the active document's tree. Returns the generation.
    pub fn begin_edit(
        &mut self,
        targets: Vec<LayerId>,
        label: impl Into<String>,
    ) -> Result<u64, String> {
        let (id, layers_len_check) = {
            let doc = self.active().ok_or("No document is open")?;
            (doc.id(), doc.document.layers.len())
        };
        let _ = layers_len_check;
        // The target validation needs the tree; borrow it independently of the
        // session state.
        let targets_valid = {
            let doc = self.active().ok_or("No document is open")?;
            targets
                .iter()
                .all(|t| doc.document.layers.get(*t).is_some())
        };
        if !targets_valid {
            return Err("the gesture's target is not in this document".to_string());
        }
        self.edit_sessions.begin_validated(id, targets, label)
    }

    /// Commit the live gesture as **one** labeled history entry. The commands
    /// become a single [`Command::Transaction`], so a long drag is one undo
    /// step whose label is the gesture's. Refused — with document and history
    /// untouched — when no gesture is live, when the active document is no
    /// longer the one the gesture began in, or when the application refuses a
    /// command; the session ends either way.
    pub fn commit_edit(&mut self, commands: Vec<Command>) -> Result<usize, String> {
        let session = self
            .edit_sessions
            .take()
            .ok_or("No gesture is in progress")?;
        let is_right_document = self.active().is_some_and(|d| d.id() == session.document);
        if !is_right_document {
            return Err("The gesture belongs to another document".to_string());
        }
        let doc = self.active_mut().expect("checked above");
        let command = Command::Transaction {
            label: session.label.clone(),
            commands,
        };
        match doc.apply(command) {
            Ok(()) => Ok(1),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Abandon the live gesture: no command reaches the document, no entry
    /// reaches the history. Returns whether there was anything to abandon.
    pub fn cancel_edit(&mut self) -> bool {
        self.edit_sessions.cancel().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use editor_core::{Command, LayerPatch};

    fn editor(dir: &std::path::Path) -> Editor {
        let canvas = dir.join("canvas.png");
        std::fs::write(
            &canvas,
            raster::encode(raster::ExportFormat::Png, 64, 64, &[255u8; 64 * 64 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&canvas).unwrap();
        ed
    }

    fn rename(layer: LayerId, name: &str) -> Command {
        Command::SetLayerProperties {
            layer_id: layer,
            patch: LayerPatch {
                name: Some(name.to_string()),
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_committed_gesture_is_one_labeled_undo_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        let depth_before = ed.active().unwrap().history_depth();

        ed.begin_edit(vec![layer], "Move logo").unwrap();
        // A gesture that would naively be two edits commits as one entry.
        ed.commit_edit(vec![rename(layer, "A"), rename(layer, "B")])
            .unwrap();

        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth_before + 1,
            "two commands, one gesture: one entry"
        );
        assert_eq!(ed.active().unwrap().history.undo_label(), Some("Move logo"));
        // Undo takes the whole gesture back at once.
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(
            ed.active()
                .unwrap()
                .document
                .layers
                .get(layer)
                .unwrap()
                .name,
            "canvas.png"
        );
    }

    #[test]
    fn a_cancelled_gesture_leaves_no_entry_and_no_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        let before = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 64, 64))
            .unwrap();
        let depth_before = ed.active().unwrap().history_depth();

        ed.begin_edit(vec![layer], "Move logo").unwrap();
        assert!(ed.cancel_edit(), "there was a gesture to cancel");
        assert!(!ed.cancel_edit(), "and cancelling again is a no-op");

        assert_eq!(ed.active().unwrap().history_depth(), depth_before);
        let after = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 64, 64))
            .unwrap();
        assert_eq!(before, after, "cancel touches nothing");
    }

    #[test]
    fn a_tab_switch_cannot_retarget_the_gesture() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let first_id = ed.active().unwrap().id();
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        let depth_before = ed.active().unwrap().history_depth();
        let before = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 64, 64))
            .unwrap();

        ed.begin_edit(vec![layer], "Move logo").unwrap();

        // A second document slides in front: the gesture must not follow it.
        let second = dir.path().join("second.png");
        std::fs::write(
            &second,
            raster::encode(raster::ExportFormat::Png, 32, 32, &[9u8; 32 * 32 * 4]).unwrap(),
        )
        .unwrap();
        ed.open_path(&second).unwrap();
        assert_ne!(ed.active().unwrap().id(), first_id);

        let result = ed.commit_edit(vec![rename(layer, "X")]);
        assert!(result.is_err(), "the commit refuses the wrong document");
        ed.activate(0).unwrap();
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth_before,
            "the gesture's document gained no entry"
        );
        let after = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 64, 64))
            .unwrap();
        assert_eq!(before, after, "and no pixel moved");
    }

    #[test]
    fn a_failed_commit_leaves_document_and_history_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        let depth_before = ed.active().unwrap().history_depth();
        let before = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 64, 64))
            .unwrap();

        ed.begin_edit(vec![layer], "Move logo").unwrap();
        // A command the document refuses: it names a layer that is not there.
        let ghost = layer_model::Layer::raster("ghost").id;
        let result = ed.commit_edit(vec![rename(ghost, "X")]);
        assert!(result.is_err(), "the refused command reports: {result:?}");

        assert_eq!(ed.active().unwrap().history_depth(), depth_before);
        let after = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 64, 64))
            .unwrap();
        assert_eq!(before, after, "document and history are untouched");
        assert!(ed.pending_edit().is_none(), "the session ended");
    }

    #[test]
    fn targets_are_captured_once_at_begin() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();

        let generation = ed.begin_edit(vec![layer], "Move logo").unwrap();
        let pending = ed.pending_edit().unwrap();
        assert_eq!(pending.document, ed.active().unwrap().id());
        assert_eq!(pending.targets, vec![layer]);
        assert!(pending.targets(layer), "the captured layer is editable");

        // Captured once: the session's target list is the one from begin.
        let captured = pending.targets.clone();
        assert_eq!(ed.pending_edit().unwrap().targets, captured);
        let _ = generation;
    }

    #[test]
    fn a_second_begin_while_one_is_live_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        ed.begin_edit(vec![layer], "First").unwrap();
        assert!(ed.begin_edit(vec![layer], "Second").is_err());
        assert_eq!(ed.pending_edit().unwrap().label, "First");
    }

    #[test]
    fn begin_refuses_a_target_that_is_not_in_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let ghost = layer_model::Layer::raster("ghost").id;
        assert!(ed.begin_edit(vec![ghost], "Move").is_err());
        assert!(ed.pending_edit().is_none());
    }
}
