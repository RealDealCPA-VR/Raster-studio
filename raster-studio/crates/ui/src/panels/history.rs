//! The History panel: the real stack, not a pair of labels.
//!
//! # The model
//!
//! [`HistoryModel`] flattens [`editor_core::History`] into a list of rows the
//! panel draws and a cursor saying which row the document is currently at. Row
//! `0` is always the document as opened; row `k` is the state after `k` edits.
//! Clicking a row is therefore a *distance*, and [`HistoryModel::jump_to`]
//! turns it into a [`HistoryJump`] of whole undo or redo steps — which is the
//! only vocabulary `History` has, and the only one that keeps the document and
//! the stack in step.
//!
//! # Two honest limitations
//!
//! * `History` publishes its *done* stack ([`editor_core::History::journal`])
//!   but only the top of its undone stack ([`editor_core::History::redo_label`]).
//!   Rows past the first redoable step are therefore shown as numbered steps
//!   rather than by name. Naming them needs an accessor this crate cannot add.
//! * Photoshop paints a rendered thumbnail per row. That needs a composited
//!   snapshot per history state, and nothing stores one. Each row instead
//!   carries a [`StepKind`] — derived from the command — which the panel paints
//!   as a glyph. It says what the step *was*, which is what the row is for.

use editor_core::{Command, History};

/// A move of the history cursor, in whole steps. Exactly one field is non-zero.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct HistoryJump {
    /// Steps to undo.
    pub undo: usize,
    /// Steps to redo.
    pub redo: usize,
}

impl HistoryJump {
    pub const fn undo(steps: usize) -> Self {
        Self {
            undo: steps,
            redo: 0,
        }
    }

    pub const fn redo(steps: usize) -> Self {
        Self {
            undo: 0,
            redo: steps,
        }
    }

    /// Total number of steps this jump moves.
    pub const fn steps(self) -> usize {
        self.undo + self.redo
    }
}

/// What kind of edit a history row records — the panel's stand-in for a
/// rendered thumbnail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepKind {
    /// The document as it was opened. Only ever row 0.
    Open,
    LayerAdded,
    LayerRemoved,
    LayerMoved,
    LayerChanged,
    Transformed,
    Painted,
    Filled,
    Cleared,
    Batch,
    /// A step whose command this build cannot see (a redo row past the top).
    Unknown,
}

impl StepKind {
    /// Every kind, so a new one cannot ship without a drawing — see the gate in
    /// [`crate::icons`].
    pub const ALL: [StepKind; 11] = [
        StepKind::Open,
        StepKind::LayerAdded,
        StepKind::LayerRemoved,
        StepKind::LayerMoved,
        StepKind::LayerChanged,
        StepKind::Transformed,
        StepKind::Painted,
        StepKind::Filled,
        StepKind::Cleared,
        StepKind::Batch,
        StepKind::Unknown,
    ];

    /// The icon key the panel draws in the row's marker.
    ///
    /// A *key* into [`crate::icons::ui_icon`], never a symbol: `"◻"`, `"⤡"`,
    /// `"✎"`, `"□"` and `"≡"` are all absent from the font egui loads, so most
    /// of the column was tofu boxes.
    pub const fn icon(self) -> &'static str {
        match self {
            StepKind::Open => "step-open",
            StepKind::LayerAdded => "step-layer-added",
            StepKind::LayerRemoved => "step-layer-removed",
            StepKind::LayerMoved => "step-layer-moved",
            StepKind::LayerChanged => "step-layer-changed",
            StepKind::Transformed => "step-transformed",
            StepKind::Painted => "step-painted",
            StepKind::Filled => "step-filled",
            StepKind::Cleared => "step-cleared",
            StepKind::Batch => "step-batch",
            StepKind::Unknown => "step-unknown",
        }
    }

    fn of(command: &Command) -> Self {
        match command {
            Command::CreateLayer { .. } | Command::RestoreLayers { .. } => StepKind::LayerAdded,
            Command::DeleteLayer { .. } => StepKind::LayerRemoved,
            Command::MoveLayer { .. } => StepKind::LayerMoved,
            // A kind edit changes the layer and nothing else about the
            // document, exactly as a property patch does, so it wears the same
            // icon rather than an eighth one nobody would learn.
            Command::SetLayerProperties { .. } | Command::SetLayerKind { .. } => {
                StepKind::LayerChanged
            }
            Command::TransformLayer { .. } => StepKind::Transformed,
            Command::PaintTiles { .. } => StepKind::Painted,
            Command::FillRegion { .. } => StepKind::Filled,
            Command::ClearRegion { .. } => StepKind::Cleared,
            Command::Transaction { .. } => StepKind::Batch,
        }
    }
}

/// One row of the panel.
#[derive(Clone, PartialEq, Debug)]
pub struct HistoryStep {
    /// Number of edits applied at this row. Row `0` is the opened document.
    pub index: usize,
    pub label: String,
    pub kind: StepKind,
    /// `true` once the cursor has moved above this row: the step is still
    /// redoable but is not currently applied, and the panel dims it.
    pub undone: bool,
}

/// A user-named marker in the stack.
#[derive(Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub name: String,
    /// The row this snapshot was taken at.
    pub index: usize,
}

/// The history, flattened for drawing.
#[derive(Clone, PartialEq, Debug)]
pub struct HistoryModel {
    steps: Vec<HistoryStep>,
    /// Index of the row the document is currently at.
    current: usize,
}

impl HistoryModel {
    /// Flatten a live history.
    pub fn new(history: &History) -> Self {
        let mut steps = vec![HistoryStep {
            index: 0,
            label: "Open".to_string(),
            kind: StepKind::Open,
            undone: false,
        }];
        for (i, command) in history.journal().enumerate() {
            steps.push(HistoryStep {
                index: i + 1,
                label: command.label(),
                kind: StepKind::of(command),
                undone: false,
            });
        }
        let current = history.undo_depth();
        // The undone stack is only readable one deep, so name what can be named
        // and number the rest. See the module note.
        let redo_top = history.redo_label().map(str::to_owned);
        for i in 0..history.redo_depth() {
            let index = current + 1 + i;
            steps.push(HistoryStep {
                index,
                label: match (i, &redo_top) {
                    (0, Some(label)) => label.clone(),
                    _ => format!("Step {index}"),
                },
                kind: StepKind::Unknown,
                undone: true,
            });
        }
        Self { steps, current }
    }

    /// Every row, oldest first.
    pub fn steps(&self) -> &[HistoryStep] {
        &self.steps
    }

    /// The row the document is at.
    pub fn current(&self) -> usize {
        self.current
    }

    /// The last row, i.e. the most-redone state.
    pub fn last(&self) -> usize {
        self.steps.len().saturating_sub(1)
    }

    /// The jump that lands the cursor on `index`, or `None` when the click was
    /// on the row the document is already at, or on a row that does not exist.
    ///
    /// This is the whole of the "click to jump" behaviour, and it is a pure
    /// function of two integers — which is why it is tested rather than
    /// eyeballed.
    pub fn jump_to(&self, index: usize) -> Option<HistoryJump> {
        if index >= self.steps.len() || index == self.current {
            return None;
        }
        Some(if index < self.current {
            HistoryJump::undo(self.current - index)
        } else {
            HistoryJump::redo(index - self.current)
        })
    }

    /// The jump that returns to a snapshot, or `None` if the snapshot points
    /// past the end of the stack — which happens when the steps it named were
    /// discarded by the history limit.
    pub fn jump_to_snapshot(&self, snapshot: &Snapshot) -> Option<HistoryJump> {
        self.jump_to(snapshot.index)
    }

    /// `true` when a snapshot no longer names a reachable row.
    pub fn snapshot_is_stale(&self, snapshot: &Snapshot) -> bool {
        snapshot.index >= self.steps.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{Command, Document, History};
    use layer_model::Layer;

    fn document_with(edits: usize) -> (Document, History) {
        let mut doc = Document::new(32, 32, "Test");
        let mut history = History::new();
        for i in 0..edits {
            history
                .apply(
                    &mut doc,
                    Command::create_layer(Layer::raster(format!("L{i}"))),
                )
                .expect("apply");
        }
        (doc, history)
    }

    #[test]
    fn an_empty_history_is_one_row_and_nothing_to_jump_to() {
        let history = History::new();
        let m = HistoryModel::new(&history);
        assert_eq!(m.steps().len(), 1);
        assert_eq!(m.steps()[0].label, "Open");
        assert_eq!(m.steps()[0].kind, StepKind::Open);
        assert_eq!(m.current(), 0);
        assert_eq!(m.jump_to(0), None);
        assert_eq!(m.jump_to(1), None);
    }

    #[test]
    fn each_applied_command_becomes_a_named_row() {
        let (_doc, history) = document_with(3);
        let m = HistoryModel::new(&history);
        assert_eq!(m.steps().len(), 4);
        assert_eq!(m.current(), 3);
        let labels: Vec<&str> = m.steps().iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Open", "Create Layer", "Create Layer", "Create Layer"]
        );
        assert!(m.steps()[1..]
            .iter()
            .all(|s| s.kind == StepKind::LayerAdded));
    }

    #[test]
    fn clicking_an_earlier_row_undoes_exactly_that_many_steps() {
        let (_doc, history) = document_with(4);
        let m = HistoryModel::new(&history);
        assert_eq!(m.current(), 4);
        assert_eq!(m.jump_to(0), Some(HistoryJump::undo(4)));
        assert_eq!(m.jump_to(1), Some(HistoryJump::undo(3)));
        assert_eq!(m.jump_to(3), Some(HistoryJump::undo(1)));
        assert_eq!(m.jump_to(4), None, "clicking the current row does nothing");
    }

    #[test]
    fn clicking_an_undone_row_redoes_exactly_that_many_steps() {
        let (mut doc, mut history) = document_with(4);
        history.undo(&mut doc).unwrap();
        history.undo(&mut doc).unwrap();
        let m = HistoryModel::new(&history);
        assert_eq!(m.current(), 2);
        assert_eq!(m.steps().len(), 5, "undone steps stay in the list");
        assert_eq!(m.jump_to(3), Some(HistoryJump::redo(1)));
        assert_eq!(m.jump_to(4), Some(HistoryJump::redo(2)));
        assert_eq!(m.jump_to(1), Some(HistoryJump::undo(1)));
        assert_eq!(m.jump_to(5), None, "past the end is not a jump");
    }

    #[test]
    fn undone_rows_are_marked_and_applied_rows_are_not() {
        let (mut doc, mut history) = document_with(3);
        history.undo(&mut doc).unwrap();
        let m = HistoryModel::new(&history);
        let undone: Vec<bool> = m.steps().iter().map(|s| s.undone).collect();
        assert_eq!(undone, vec![false, false, false, true]);
    }

    #[test]
    fn the_next_redoable_row_is_named_and_the_rest_are_numbered() {
        let (mut doc, mut history) = document_with(3);
        history.undo(&mut doc).unwrap();
        history.undo(&mut doc).unwrap();
        let m = HistoryModel::new(&history);
        assert_eq!(m.current(), 1);
        assert_eq!(m.steps()[2].label, "Create Layer");
        // The second redo row cannot be named; it is numbered rather than blank.
        assert_eq!(m.steps()[3].label, "Step 3");
        assert!(!m.steps()[3].label.is_empty());
    }

    #[test]
    fn a_jump_actually_lands_where_the_row_says() {
        // The arithmetic is the whole feature, so run it against a real
        // history rather than trusting the integers.
        let (mut doc, mut history) = document_with(5);
        let m = HistoryModel::new(&history);
        let jump = m.jump_to(2).expect("row 2 is reachable");
        for _ in 0..jump.undo {
            assert!(history.undo(&mut doc).unwrap());
        }
        assert_eq!(history.undo_depth(), 2);
        assert_eq!(HistoryModel::new(&history).current(), 2);

        let m = HistoryModel::new(&history);
        let jump = m.jump_to(5).expect("row 5 is reachable again");
        for _ in 0..jump.redo {
            assert!(history.redo(&mut doc).unwrap());
        }
        assert_eq!(HistoryModel::new(&history).current(), 5);
    }

    #[test]
    fn a_jump_moves_at_most_in_one_direction() {
        let (mut doc, mut history) = document_with(4);
        history.undo(&mut doc).unwrap();
        let m = HistoryModel::new(&history);
        for index in 0..=m.last() {
            if let Some(j) = m.jump_to(index) {
                assert!(j.undo == 0 || j.redo == 0, "row {index} jumps both ways");
                assert!(j.steps() > 0);
            }
        }
    }

    #[test]
    fn a_snapshot_jumps_to_its_row_and_goes_stale_when_the_row_is_gone() {
        let (_doc, history) = document_with(3);
        let m = HistoryModel::new(&history);
        let snap = Snapshot {
            name: "Before retouch".into(),
            index: 1,
        };
        assert!(!m.snapshot_is_stale(&snap));
        assert_eq!(m.jump_to_snapshot(&snap), Some(HistoryJump::undo(2)));

        let stale = Snapshot {
            name: "Long ago".into(),
            index: 99,
        };
        assert!(m.snapshot_is_stale(&stale));
        assert_eq!(m.jump_to_snapshot(&stale), None);
    }

    #[test]
    fn a_new_edit_after_an_undo_drops_the_redo_rows() {
        let (mut doc, mut history) = document_with(3);
        history.undo(&mut doc).unwrap();
        assert_eq!(HistoryModel::new(&history).steps().len(), 4);
        history
            .apply(&mut doc, Command::create_layer(Layer::raster("New")))
            .unwrap();
        let m = HistoryModel::new(&history);
        assert_eq!(m.steps().len(), 4);
        assert_eq!(m.current(), 3);
        assert!(m.steps().iter().all(|s| !s.undone));
    }

    #[test]
    fn every_command_kind_maps_to_a_distinct_icon_key() {
        assert_eq!(StepKind::ALL.len(), 11);
        let mut keys: Vec<&str> = StepKind::ALL.iter().map(|k| k.icon()).collect();
        assert!(keys.iter().all(|k| !k.is_empty()));
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "two step kinds share an icon key");
    }

    #[test]
    fn a_transaction_row_takes_its_own_label() {
        let mut doc = Document::new(16, 16, "Test");
        let mut history = History::new();
        history
            .apply(
                &mut doc,
                Command::Transaction {
                    label: "Place Image".into(),
                    commands: vec![Command::create_layer(Layer::raster("Placed"))],
                },
            )
            .unwrap();
        let m = HistoryModel::new(&history);
        assert_eq!(m.steps()[1].label, "Place Image");
        assert_eq!(m.steps()[1].kind, StepKind::Batch);
    }
}
