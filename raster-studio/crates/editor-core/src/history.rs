//! Undo/redo history built on the command/inverse pair.
//!
//! Each entry stores the command that was applied *and* the inverse captured at
//! apply time. Undo applies the inverse; redo re-applies the original. This is
//! also the basis for the on-disk command journal (`project-format` persists
//! the applied stream so a crash can be recovered by replay).

use crate::command::{Command, CommandError};
use crate::document::Document;

/// One committed edit: the forward command and its exact inverse.
#[derive(Debug, Clone)]
struct Entry {
    forward: Command,
    inverse: Command,
    label: String,
}

/// A linear undo/redo stack.
#[derive(Debug, Default)]
pub struct History {
    done: Vec<Entry>,
    undone: Vec<Entry>,
    /// Soft cap on retained entries; oldest are compacted away past this.
    limit: usize,
}

impl History {
    /// Create a history with a retention limit (0 = unbounded).
    pub fn with_limit(limit: usize) -> Self {
        Self {
            done: Vec::new(),
            undone: Vec::new(),
            limit,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Label of the next undo/redo step, for menu text.
    pub fn undo_label(&self) -> Option<&str> {
        self.done.last().map(|e| e.label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.undone.last().map(|e| e.label.as_str())
    }

    /// Apply a command to the document and record it for undo.
    /// Applying a new command clears the redo stack (standard linear history).
    pub fn apply(&mut self, doc: &mut Document, cmd: Command) -> Result<(), CommandError> {
        let label = cmd.label();
        let inverse = cmd.apply(doc)?;
        self.undone.clear();
        self.done.push(Entry {
            forward: cmd,
            inverse,
            label,
        });
        self.compact();
        Ok(())
    }

    /// Undo the most recent command.
    pub fn undo(&mut self, doc: &mut Document) -> Result<bool, CommandError> {
        let Some(entry) = self.done.pop() else {
            return Ok(false);
        };
        // Applying the inverse yields the forward command again (its inverse).
        entry.inverse.apply(doc)?;
        self.undone.push(entry);
        Ok(true)
    }

    /// Redo the most recently undone command.
    pub fn redo(&mut self, doc: &mut Document) -> Result<bool, CommandError> {
        let Some(entry) = self.undone.pop() else {
            return Ok(false);
        };
        entry.forward.apply(doc)?;
        self.done.push(entry);
        Ok(true)
    }

    /// The forward command stream, for journaling to disk.
    pub fn journal(&self) -> impl Iterator<Item = &Command> {
        self.done.iter().map(|e| &e.forward)
    }

    fn compact(&mut self) {
        if self.limit > 0 && self.done.len() > self.limit {
            let overflow = self.done.len() - self.limit;
            self.done.drain(0..overflow);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, LayerPatch};
    use layer_model::Layer;

    #[test]
    fn undo_redo_roundtrip() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::with_limit(0);

        let layer = Layer::raster("L1");
        let id = layer.id;
        hist.apply(&mut doc, Command::CreateLayer { layer })
            .unwrap();
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
        hist.apply(&mut doc, Command::CreateLayer { layer: l })
            .unwrap();
        hist.undo(&mut doc).unwrap();
        assert!(hist.can_redo());
        // Redo must be cleared by applying something new. Recreate a layer.
        hist.apply(
            &mut doc,
            Command::CreateLayer {
                layer: Layer::raster("L2"),
            },
        )
        .unwrap();
        assert!(!hist.can_redo());
        let _ = id;
    }

    #[test]
    fn limit_compacts_oldest() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::with_limit(2);
        for _ in 0..5 {
            hist.apply(
                &mut doc,
                Command::CreateLayer {
                    layer: Layer::raster("L"),
                },
            )
            .unwrap();
        }
        // Only the last 2 remain undoable.
        assert!(hist.undo(&mut doc).unwrap());
        assert!(hist.undo(&mut doc).unwrap());
        assert!(!hist.undo(&mut doc).unwrap());
    }

    #[test]
    fn journal_yields_forward_commands() {
        let mut doc = Document::new(100, 100, "t");
        let mut hist = History::with_limit(0);
        let l = Layer::raster("L1");
        let id = l.id;
        hist.apply(&mut doc, Command::CreateLayer { layer: l })
            .unwrap();
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
}
