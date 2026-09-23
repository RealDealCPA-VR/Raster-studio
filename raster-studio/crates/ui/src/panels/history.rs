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
//! * Photoshop paints a rendered thumbnail per row. Nothing stores a
//!   composited snapshot per history state, so W4-I keeps one as the states
//!   go by: each time the application rebuilds its composite preview it hands
//!   the same small image to [`HistoryThumbs::capture`] under the row the
//!   document is at, and moves the pictures already stored to the rows their
//!   states are at now (a rewrite past the cursor drops them; compaction at
//!   the history limit shifts them down). A row with no picture — a redo row
//!   never visited, or every row when the shift could not be told — shows
//!   only its [`StepKind`] glyph, which says what the step *was*.
//! * The History Brush (`tools::history_brush`) paints from the state its
//!   `source` option names — a row of this panel, `0` (the default) being
//!   the document as opened. The panel's source column marks that row (for
//!   `0`, [`HistoryThumbs::opened_row`]), and W4-G made a click in the
//!   column set it: the cell writes the option through the same
//!   `Intent::SetToolOption` the options bar sends, and the shell rebuilds
//!   that state (on a copy of the document) at the press of each stroke.

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
            // A crop is a canvas resize plus one translation per layer, and
            // the resize is the part the user asked for — so it reads as a
            // geometry step rather than as a layer edit.
            Command::SetSelection { .. }
            | Command::SetMetaColorMode { .. }
            | Command::SetMetaBitDepth { .. }
            | Command::TransformLayer { .. }
            | Command::SetCanvasSize { .. } => StepKind::Transformed,
            // Image ▸ Image Size resamples every layer — a geometry step on
            // the document, not a paint on any one layer.
            Command::ResampleImage { .. } => StepKind::Transformed,
            Command::SetGuides { .. } => StepKind::Transformed,
            // Asset-table bookkeeping: it rides inside a refresh Transaction
            // (shown as one Batch row) and never stands alone in the panel.
            Command::SetAssetSourceSize { .. } => StepKind::Transformed,
            // Card 069: a replace rides inside a Transaction like the refresh
            // (tiles + transforms + the asset row), so it reads as a geometry
            // step rather than as a plain paint.
            Command::ReplaceAssetSource { .. } => StepKind::Transformed,
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

/// W4-I: one small composite picture per history row the document has been
/// at, kept in egui's frame data (the textures belong to the context).
///
/// A row index is not a state's identity, for two reasons, and each
/// [`HistoryThumbs::capture`] handles both before it stores the current row:
///
/// * **Rewrites.** Undo, then a new edit, makes the row past the cursor a
///   different state. The rows past the stack's end are dropped, and the
///   rewritten row is the current one, which is captured again.
/// * **Compaction.** Once the done stack is at the history limit, every new
///   edit drops the *oldest* entry ([`editor_core::History::limit`]), so each
///   surviving state moves down one row. Every stored picture remembers a
///   fingerprint of the command that led into its row; the capture finds the
///   one shift under which every stored fingerprint still matches the
///   journal and moves the pictures with it. When no shift — or more than
///   one, as with a run of identical commands — explains the journal, the
///   pictures are dropped rather than risk showing one state under another
///   state's row.
#[derive(Clone, Default)]
pub struct HistoryThumbs {
    document: u64,
    rows: Vec<Thumb>,
    /// The opened document may no longer be row 0: entries have been
    /// compacted off the bottom of the stack (or may have been, when the
    /// shift could not be told).
    compacted: bool,
}

/// One stored picture.
#[derive(Clone)]
struct Thumb {
    index: usize,
    /// Fingerprint of the command that led into this row; `None` for row 0.
    lead: Option<u64>,
    tex: egui::TextureHandle,
}

/// A stable fingerprint of one command: its full `Debug` form, hashed as it
/// is written (tile edits carry content hashes, so two different strokes do
/// not collide), with no string built.
fn fingerprint(command: &Command) -> u64 {
    use std::hash::Hasher as _;
    struct Sink(std::collections::hash_map::DefaultHasher);
    impl std::fmt::Write for Sink {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0.write(s.as_bytes());
            Ok(())
        }
    }
    let mut sink = Sink(std::collections::hash_map::DefaultHasher::new());
    let _ = std::fmt::write(&mut sink, format_args!("{command:?}"));
    sink.0.finish()
}

/// The journal's fingerprints, computed only for the rows a check asks for.
struct Leads<'a> {
    journal: Vec<&'a Command>,
    memo: Vec<Option<u64>>,
}

impl<'a> Leads<'a> {
    fn new(history: &'a History) -> Self {
        let journal: Vec<&Command> = history.journal().collect();
        let memo = vec![None; journal.len()];
        Self { journal, memo }
    }

    /// The fingerprint of the command that led into row `row` (`row >= 1`).
    fn of_row(&mut self, row: usize) -> Option<u64> {
        let at = row.checked_sub(1)?;
        let command = self.journal.get(at)?;
        Some(*self.memo[at].get_or_insert_with(|| fingerprint(command)))
    }
}

/// Nearest-neighbour shrink of an RGBA image to at most `edge` texels on its
/// longer side — a history row's picture is a few dozen points wide, and a
/// deep stack of full-size previews would hold megabytes of textures.
fn downsample(size: [usize; 2], rgba: &[u8], edge: usize) -> egui::ColorImage {
    let [w, h] = size;
    let scale = (edge as f32 / w.max(h) as f32).min(1.0);
    let (dw, dh) = (
        ((w as f32 * scale).round() as usize).max(1),
        ((h as f32 * scale).round() as usize).max(1),
    );
    let mut out = Vec::with_capacity(dw * dh * 4);
    for y in 0..dh {
        let sy = (y * h / dh).min(h - 1);
        for x in 0..dw {
            let sx = (x * w / dw).min(w - 1);
            let at = (sy * w + sx) * 4;
            out.extend_from_slice(&rgba[at..at + 4]);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([dw, dh], &out)
}

impl HistoryThumbs {
    /// The longest edge of a stored picture, in texels.
    pub const EDGE: usize = 64;

    fn slot() -> egui::Id {
        egui::Id::new("raster-history-thumbs")
    }

    /// Store `rgba` (unmultiplied, `size[0] x size[1]`) as the picture of the
    /// row `history` is at now, for document `document`, after moving the
    /// stored pictures to the rows their states are at (see the type's note).
    pub fn capture(
        ctx: &egui::Context,
        document: u64,
        history: &History,
        size: [usize; 2],
        rgba: &[u8],
    ) {
        if size[0] == 0 || size[1] == 0 || rgba.len() < size[0] * size[1] * 4 {
            return;
        }
        let index = history.undo_depth();
        let last = index + history.redo_depth();
        let image = downsample(size, rgba, HistoryThumbs::EDGE);
        let mut thumbs = ctx
            .data(|d| d.get_temp::<Self>(Self::slot()))
            .unwrap_or_default();
        if thumbs.document != document {
            thumbs = Self {
                document,
                ..Self::default()
            };
        }
        let mut leads = Leads::new(history);
        thumbs.realign(&mut leads, index, history.limit());
        thumbs.rows.retain(|t| t.index <= last);
        let lead = leads.of_row(index);
        match thumbs.rows.iter_mut().find(|t| t.index == index) {
            Some(t) => {
                t.tex.set(image, egui::TextureOptions::LINEAR);
                t.lead = lead;
            }
            None => {
                let tex = ctx.load_texture(
                    format!("raster-history-thumb-{document}-{index}"),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                thumbs.rows.push(Thumb { index, lead, tex });
            }
        }
        ctx.data_mut(|d| d.insert_temp(Self::slot(), thumbs));
    }

    /// How many stored pictures' fingerprints match the journal once each
    /// row moves down by `shift`, or `None` when one of them does not. The
    /// row the document is at now is not checked (it is captured again),
    /// nor are redo rows (their commands are not in the journal); a row that
    /// falls to 0 or below has no command to check.
    fn matches(&self, leads: &mut Leads<'_>, shift: usize, current: usize) -> Option<usize> {
        let mut matched = 0;
        for t in &self.rows {
            let Some(row) = t.index.checked_sub(shift) else {
                continue;
            };
            if row == current || row == 0 || row > leads.journal.len() {
                continue;
            }
            if t.lead.is_none() || leads.of_row(row) != t.lead {
                return None;
            }
            matched += 1;
        }
        Some(matched)
    }

    /// Move the stored pictures down by the one shift that explains the
    /// journal, or drop them when none or several do.
    ///
    /// No shift is the default whenever nothing contradicts it. A shift of
    /// `n > 0` is only a candidate with the done stack at its limit (the
    /// only time compaction runs) and with at least one picture positively
    /// matching after the move — a shift that pushes every checkable picture
    /// off the bottom explains nothing.
    fn realign(&mut self, leads: &mut Leads<'_>, current: usize, limit: usize) {
        if self.rows.is_empty() {
            return;
        }
        let deepest = self.rows.iter().map(|t| t.index).max().unwrap_or(0);
        let mut shifts = Vec::new();
        for n in 0..=deepest {
            let candidate = match self.matches(leads, n, current) {
                Some(_) if n == 0 => true,
                Some(matched) => current >= limit && matched > 0,
                None => false,
            };
            if candidate {
                shifts.push(n);
                if shifts.len() > 1 {
                    break;
                }
            }
        }
        match shifts.as_slice() {
            [0] => {}
            [n] => {
                let n = *n;
                self.rows.retain(|t| t.index >= n);
                for t in &mut self.rows {
                    t.index -= n;
                    if t.index == 0 {
                        t.lead = None;
                    }
                }
                self.compacted = true;
            }
            _ => {
                self.rows.clear();
                // Compaction only happens with the stack at its limit; below
                // it, row 0 is still the opened document.
                if current >= limit {
                    self.compacted = true;
                }
            }
        }
    }

    /// W4-I: the row that is the document as opened — what the History
    /// Brush paints from (the shell hands the tool the opened state as
    /// `tools::ToolContext::history_source`) — or `None` once that state has
    /// been compacted off the bottom of the stack.
    pub fn opened_row(ctx: &egui::Context) -> Option<usize> {
        let compacted = ctx
            .data(|d| d.get_temp::<Self>(Self::slot()))
            .is_some_and(|t| t.compacted);
        (!compacted).then_some(0)
    }

    /// Forget every picture (no document open).
    pub fn clear(ctx: &egui::Context) {
        ctx.data_mut(|d| d.remove::<Self>(Self::slot()));
    }

    /// The picture of row `index`, when one was captured.
    pub fn texture(ctx: &egui::Context, index: usize) -> Option<egui::TextureHandle> {
        ctx.data(|d| d.get_temp::<Self>(Self::slot()))?
            .rows
            .into_iter()
            .find(|t| t.index == index)
            .map(|t| t.tex)
    }
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

    /// W4-I: when a run of identical commands makes the compaction shift
    /// impossible to tell, the pictures are dropped — never shown under a
    /// row whose state they are not — and the opened row is no longer
    /// claimed; a distinct run is realigned instead.
    #[test]
    fn thumbnails_are_realigned_or_dropped_when_the_limit_compacts() {
        let px = [8usize, 8];
        let image = |v: u8| vec![v; 8 * 8 * 4];
        let select = |x: i32| Command::SetSelection {
            selection: editor_core::Selection::Rect {
                min: glam::IVec2::new(x, 0),
                max: glam::IVec2::new(x + 2, 2),
            },
        };
        let run = |commands: Vec<Command>| {
            let ctx = egui::Context::default();
            let mut doc = Document::new(16, 16, "Test");
            let mut history = History::with_limit(2);
            HistoryThumbs::capture(&ctx, 1, &history, px, &image(0));
            let mut ids = vec![HistoryThumbs::texture(&ctx, 0).unwrap().id()];
            for (i, command) in commands.into_iter().enumerate() {
                history.apply(&mut doc, command).unwrap();
                HistoryThumbs::capture(&ctx, 1, &history, px, &image(i as u8 + 1));
                let row = history.undo_depth();
                ids.push(HistoryThumbs::texture(&ctx, row).unwrap().id());
            }
            (ctx, ids)
        };

        // Distinct commands: after the third edit row 0 is the state after
        // the first, row 1 after the second.
        let (ctx, ids) = run(vec![select(0), select(3), select(6)]);
        assert_eq!(
            HistoryThumbs::texture(&ctx, 0).map(|t| t.id()),
            Some(ids[1])
        );
        assert_eq!(
            HistoryThumbs::texture(&ctx, 1).map(|t| t.id()),
            Some(ids[2])
        );
        assert_eq!(HistoryThumbs::opened_row(&ctx), None);

        // Identical commands: the shift cannot be told, so no older picture
        // survives; only the current row has one.
        let (ctx, _) = run(vec![select(0), select(0), select(0)]);
        assert!(HistoryThumbs::texture(&ctx, 0).is_none());
        assert!(HistoryThumbs::texture(&ctx, 1).is_none());
        assert!(HistoryThumbs::texture(&ctx, 2).is_some());
        assert_eq!(HistoryThumbs::opened_row(&ctx), None);
    }

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
