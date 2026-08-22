//! Undo/redo history built on the command/inverse pair.
//!
//! Each entry stores the command that was applied *and* the inverse captured at
//! apply time. Undo applies the inverse; redo re-applies the original. This is
//! also the basis for the on-disk command journal (`project-format` persists
//! the applied stream so a crash can be recovered by replay).
//!
//! # Failure behaviour
//! Every [`Command`] is atomic (see [`crate::command`]), so a failed apply,
//! undo, or redo leaves the document untouched. This module keeps its stacks in
//! step with that: an entry is recorded only on a successful apply, and an
//! entry whose undo or redo failed is put back where it came from. History and
//! document therefore never disagree about what has happened.

use crate::command::{Command, CommandError};
use crate::document::Document;

/// Entries retained when a caller does not choose a limit.
///
/// Retention is bounded because an entry is not cheap: a delete's inverse holds
/// the whole detached subtree — every removed [`layer_model::Layer`], effect
/// block and all — and a paint's inverse holds one hash per touched tile. An
/// unbounded stack is a slow leak in a program users leave open for days.
pub const DEFAULT_HISTORY_LIMIT: usize = 200;

/// One committed edit: the forward command and its exact inverse.
#[derive(Debug, Clone)]
struct Entry {
    forward: Command,
    inverse: Command,
    label: String,
}

/// A linear undo/redo stack.
#[derive(Debug)]
pub struct History {
    done: Vec<Entry>,
    undone: Vec<Entry>,
    /// Hard cap on retained entries, per stack. Always `>= 1`.
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// A history retaining [`DEFAULT_HISTORY_LIMIT`] entries.
    pub fn new() -> Self {
        Self::with_limit(DEFAULT_HISTORY_LIMIT)
    }

    /// Create a history retaining at most `limit` entries per stack.
    ///
    /// `0` selects [`DEFAULT_HISTORY_LIMIT`]. There is deliberately no
    /// unbounded mode — see the note on that constant — and no zero mode
    /// either, since a history that discards the edit it was just handed is
    /// worse than no history at all.
    pub fn with_limit(limit: usize) -> Self {
        Self {
            done: Vec::new(),
            undone: Vec::new(),
            limit: if limit == 0 {
                DEFAULT_HISTORY_LIMIT
            } else {
                limit
            },
        }
    }

    /// Entries retained per stack.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Change the ceiling, dropping the oldest entries of *both* stacks
    /// immediately if the new one is lower.
    ///
    /// This is the path a preferences change takes ("keep 20 undo steps"), and
    /// it is the only one that can leave the redo stack over the ceiling:
    /// during normal editing `undone` only ever receives entries popped from
    /// `done`, which is already capped. `0` means
    /// [`DEFAULT_HISTORY_LIMIT`], as in [`History::with_limit`].
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = if limit == 0 {
            DEFAULT_HISTORY_LIMIT
        } else {
            limit
        };
        self.compact();
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// How many steps can still be undone / redone.
    pub fn undo_depth(&self) -> usize {
        self.done.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.undone.len()
    }

    /// Label of the next undo/redo step, for menu text.
    pub fn undo_label(&self) -> Option<&str> {
        self.done.last().map(|e| e.label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.undone.last().map(|e| e.label.as_str())
    }

    /// Forget every recorded edit. The document is left exactly as it is; only
    /// the ability to undo is dropped.
    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }

    /// Apply a command to the document and record it for undo.
    /// Applying a new command clears the redo stack (standard linear history).
    ///
    /// On failure nothing is recorded — which is only sound because a failed
    /// command changed nothing.
    pub fn apply(&mut self, doc: &mut Document, cmd: Command) -> Result<(), CommandError> {
        let label = cmd.label();
        let inverse = cmd.apply(doc)?;
        self.undone.clear();
        self.done.push(Entry {
            forward: cmd,
            inverse,
            label,
        });
        doc.mark_dirty();
        self.compact();
        Ok(())
    }

    /// Undo the most recent command.
    pub fn undo(&mut self, doc: &mut Document) -> Result<bool, CommandError> {
        let Some(entry) = self.done.pop() else {
            return Ok(false);
        };
        if let Err(e) = entry.inverse.apply(doc) {
            // The document is unchanged, so the entry is still undoable.
            self.done.push(entry);
            return Err(e);
        }
        self.undone.push(entry);
        doc.mark_dirty();
        self.compact();
        Ok(true)
    }

    /// Redo the most recently undone command.
    ///
    /// The inverse captured here replaces the one recorded at the original
    /// apply: it describes the state this redo actually overwrote, which is the
    /// state the *next* undo has to restore. Keeping the original capture would
    /// make that undo restore a state the document had at some earlier point.
    pub fn redo(&mut self, doc: &mut Document) -> Result<bool, CommandError> {
        let Some(mut entry) = self.undone.pop() else {
            return Ok(false);
        };
        match entry.forward.apply(doc) {
            Ok(inverse) => {
                entry.inverse = inverse;
                self.done.push(entry);
                doc.mark_dirty();
                self.compact();
                Ok(true)
            }
            Err(e) => {
                self.undone.push(entry);
                Err(e)
            }
        }
    }

    /// The forward command stream, for journaling to disk.
    pub fn journal(&self) -> impl Iterator<Item = &Command> {
        self.done.iter().map(|e| &e.forward)
    }

    /// Drop the oldest entries of either stack past the limit.
    ///
    /// Both stacks, not just `done`: [`History::set_limit`] can lower the
    /// ceiling under a full redo stack, and an entry there is exactly as
    /// expensive as one in `done`.
    fn compact(&mut self) {
        let limit = self.limit;
        for stack in [&mut self.done, &mut self.undone] {
            if stack.len() > limit {
                let overflow = stack.len() - limit;
                stack.drain(0..overflow);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, LayerPatch};
    use crate::pixels::{PixelKey, PixelTarget, TileEdit};
    use layer_model::{Layer, LayerId};
    use raster::{TileCoord, TileHash};

    fn coord(x: i32, y: i32) -> TileCoord {
        TileCoord::new(x, y, 0)
    }

    fn hash(seed: u8) -> TileHash {
        TileHash([seed; 32])
    }

    fn doc_with_layer() -> (Document, LayerId) {
        let mut doc = Document::new(1024, 1024, "t");
        let l = Layer::raster("L1");
        let id = l.id;
        doc.layers.push_root(l).unwrap();
        (doc, id)
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::new();

        let layer = Layer::raster("L1");
        let id = layer.id;
        hist.apply(&mut doc, Command::create_layer(layer)).unwrap();
        assert!(doc.layers.get(id).is_some());
        assert!(hist.can_undo() && !hist.can_redo());

        hist.undo(&mut doc).unwrap();
        assert!(doc.layers.get(id).is_none());
        assert!(hist.can_redo());

        hist.redo(&mut doc).unwrap();
        assert!(doc.layers.get(id).is_some());
    }

    #[test]
    fn new_command_clears_redo() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::with_limit(0);
        let l = Layer::raster("L1");
        let id = l.id;
        hist.apply(&mut doc, Command::create_layer(l)).unwrap();
        hist.undo(&mut doc).unwrap();
        assert!(hist.can_redo());
        // Redo must be cleared by applying something new. Recreate a layer.
        hist.apply(&mut doc, Command::create_layer(Layer::raster("L2")))
            .unwrap();
        assert!(!hist.can_redo());
        let _ = id;
    }

    #[test]
    fn limit_compacts_oldest() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::with_limit(2);
        for _ in 0..5 {
            hist.apply(&mut doc, Command::create_layer(Layer::raster("L")))
                .unwrap();
        }
        // Only the last 2 remain undoable.
        assert!(hist.undo(&mut doc).unwrap());
        assert!(hist.undo(&mut doc).unwrap());
        assert!(!hist.undo(&mut doc).unwrap());
    }

    #[test]
    fn history_is_bounded_even_when_no_limit_is_asked_for() {
        // `with_limit(0)` used to mean "unbounded", and every entry can retain
        // a whole layer, so the stack was a leak with no ceiling.
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::with_limit(0);
        assert_eq!(hist.limit(), DEFAULT_HISTORY_LIMIT);

        let n = DEFAULT_HISTORY_LIMIT + 50;
        for _ in 0..n {
            hist.apply(&mut doc, Command::create_layer(Layer::raster("L")))
                .unwrap();
        }
        assert_eq!(hist.undo_depth(), DEFAULT_HISTORY_LIMIT);

        // And the redo stack cannot grow past the same ceiling: it only ever
        // receives entries popped off the already-capped undo stack.
        while hist.undo(&mut doc).unwrap() {}
        assert_eq!(hist.redo_depth(), DEFAULT_HISTORY_LIMIT);
        assert!(hist.redo_depth() <= hist.limit());
    }

    #[test]
    fn lowering_the_limit_compacts_the_redo_stack_too() {
        // The only way a stack can be found over the ceiling, and therefore the
        // case that makes `compact`'s redo half load-bearing: a redo stack full
        // of entries when the ceiling drops under it.
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::with_limit(6);
        for _ in 0..6 {
            hist.apply(&mut doc, Command::create_layer(Layer::raster("L")))
                .unwrap();
        }
        while hist.undo(&mut doc).unwrap() {}
        assert_eq!((hist.undo_depth(), hist.redo_depth()), (0, 6));

        hist.set_limit(2);
        assert_eq!(hist.limit(), 2);
        assert_eq!(
            hist.redo_depth(),
            2,
            "the redo stack retains whole layers per entry; it has to obey the ceiling"
        );

        // What survived is the top of the stack — the next steps to be redone —
        // so redo still walks forward from where the user is, and only the far
        // end of the redo chain is lost.
        assert!(hist.redo(&mut doc).unwrap());
        assert!(hist.redo(&mut doc).unwrap());
        assert!(!hist.redo(&mut doc).unwrap());
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(hist.undo_depth(), 2);

        // And a zero limit still means the default, not "discard everything".
        hist.set_limit(0);
        assert_eq!(hist.limit(), DEFAULT_HISTORY_LIMIT);
        assert_eq!(hist.undo_depth(), 2);
    }

    #[test]
    fn a_default_history_is_bounded_too() {
        assert_eq!(History::default().limit(), DEFAULT_HISTORY_LIMIT);
        assert_eq!(History::new().limit(), DEFAULT_HISTORY_LIMIT);
    }

    #[test]
    fn journal_yields_forward_commands() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::with_limit(0);
        let l = Layer::raster("L1");
        let id = l.id;
        hist.apply(&mut doc, Command::create_layer(l)).unwrap();
        hist.apply(
            &mut doc,
            Command::SetLayerProperties {
                layer_id: id,
                patch: LayerPatch {
                    opacity: Some(0.5),
                    ..Default::default()
                },
            },
        )
        .unwrap();
        assert_eq!(hist.journal().count(), 2);
    }

    #[test]
    fn redo_captures_the_state_it_actually_replaced() {
        // Redo used to throw away the inverse it computed and keep the one from
        // the original apply, so the next undo restored a state the document no
        // longer had.
        let (mut doc, id) = doc_with_layer();
        let mut hist = History::new();

        hist.apply(
            &mut doc,
            Command::SetLayerProperties {
                layer_id: id,
                patch: LayerPatch {
                    opacity: Some(0.25),
                    ..Default::default()
                },
            },
        )
        .unwrap();
        assert_eq!(doc.layers.get(id).unwrap().opacity, 0.25);

        hist.undo(&mut doc).unwrap();
        assert_eq!(doc.layers.get(id).unwrap().opacity, 1.0);

        // An edit made outside this history — a second view, a script, a
        // command applied directly. `Command::apply` is public API.
        Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                opacity: Some(0.8),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();

        hist.redo(&mut doc).unwrap();
        assert_eq!(doc.layers.get(id).unwrap().opacity, 0.25);

        hist.undo(&mut doc).unwrap();
        assert_eq!(
            doc.layers.get(id).unwrap().opacity,
            0.8,
            "undo must restore what redo overwrote, not an older state"
        );
    }

    #[test]
    fn a_failed_apply_records_nothing_and_changes_nothing() {
        let (mut doc, id) = doc_with_layer();
        let mut hist = History::new();
        let before = doc.clone();

        let err = hist
            .apply(
                &mut doc,
                Command::Transaction {
                    label: "Import".into(),
                    commands: vec![
                        Command::create_layer(Layer::raster("A")),
                        Command::DeleteLayer {
                            layer_id: LayerId::new(),
                        },
                    ],
                },
            )
            .unwrap_err();
        assert!(matches!(err, CommandError::Tree(_)));
        assert_eq!(doc, before);
        assert!(!hist.can_undo());
        assert!(!doc.is_dirty(), "a refused command is not a change");
        let _ = id;
    }

    #[test]
    fn a_failed_undo_keeps_the_entry_undoable() {
        let (mut doc, id) = doc_with_layer();
        let mut hist = History::new();
        hist.apply(
            &mut doc,
            Command::paint_tiles(PixelTarget::Layer(id), [TileEdit::set(coord(0, 0), hash(1))])
                .unwrap(),
        )
        .unwrap();

        // Lock the layer behind the history's back: the recorded inverse is now
        // un-appliable.
        doc.layers.get_mut(id).unwrap().locked.pixels = true;
        let err = hist.undo(&mut doc).unwrap_err();
        assert!(matches!(err, CommandError::LayerLocked(_)));
        assert_eq!(hist.undo_depth(), 1, "the entry must not be consumed");
        assert!(!hist.can_redo());

        // Unlock and the undo works, proving the entry survived intact.
        doc.layers.get_mut(id).unwrap().locked.pixels = false;
        assert!(hist.undo(&mut doc).unwrap());
        assert!(doc.pixels.tiles(PixelKey::Layer(id)).is_none());
    }

    #[test]
    fn a_multi_tile_stroke_is_one_undo_step() {
        let (mut doc, id) = doc_with_layer();
        let mut hist = History::new();
        let before = doc.clone();

        let stroke = Command::paint_tiles(
            PixelTarget::Layer(id),
            (0..12).map(|i| TileEdit::set(coord(i, i), hash(i as u8))),
        )
        .unwrap();
        hist.apply(&mut doc, stroke).unwrap();
        assert_eq!(doc.pixels.tile_count(), 12);
        assert_eq!(
            hist.undo_depth(),
            1,
            "a stroke is one history entry however many tiles it crossed"
        );

        assert!(hist.undo(&mut doc).unwrap());
        assert_eq!(doc, before, "one undo puts every tile back");
        assert!(hist.redo(&mut doc).unwrap());
        assert_eq!(doc.pixels.tile_count(), 12);
    }

    #[test]
    fn editing_marks_the_document_dirty_and_saving_clears_it() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::new();
        assert!(!doc.is_dirty());

        hist.apply(&mut doc, Command::create_layer(Layer::raster("L")))
            .unwrap();
        assert!(doc.is_dirty());

        doc.mark_saved();
        hist.undo(&mut doc).unwrap();
        assert!(doc.is_dirty(), "undoing past the saved state is a change");

        doc.mark_saved();
        hist.redo(&mut doc).unwrap();
        assert!(doc.is_dirty());
    }

    #[test]
    fn clear_drops_the_stacks_without_touching_the_document() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::new();
        hist.apply(&mut doc, Command::create_layer(Layer::raster("L")))
            .unwrap();
        let before = doc.clone();
        hist.clear();
        assert!(!hist.can_undo() && !hist.can_redo());
        assert_eq!(doc, before);
        assert_eq!(hist.journal().count(), 0);
    }
}
