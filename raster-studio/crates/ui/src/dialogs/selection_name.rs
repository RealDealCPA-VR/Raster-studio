//! Select ▸ Save Selection… and Select ▸ Load Selection… — the naming and
//! choosing questions Photopea asks around its alpha channels.
//!
//! Save asks for a name, opening at the first free "Alpha N" the way a new
//! alpha channel is named; Load lists the saved selections by name and asks
//! how the chosen one meets the live selection (new, add, subtract,
//! intersect), with Invert beside it. The saved selections live on the
//! document (`editor_core::Document::saved_selections`, name + selection) —
//! a public list, which the Channels panel's body
//! (`view::docks::saved_selection_rows`) lists as alpha rows under the
//! channels; a click on one opens Load Selection.
//!
//! Neither dialog implements [`super::chrome::Dialog`], for the reason Trim
//! does not: the confirmed value is a name or a choice, not a
//! [`super::action::DialogAction`]. The shell parks it for the
//! `SaveSelection` / `LoadSelection` menu arm, which performs the edit.

use egui::Context;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, combo};
use super::{ids, sizes};
use crate::strings::tr;

/// The first "Alpha N" (N ≥ 1) that `existing` does not already use.
pub fn next_alpha_name(existing: &[String]) -> String {
    let stem = tr("ui.selection_name.alpha");
    (1..)
        .map(|n| format!("{stem} {n}"))
        .find(|candidate| !existing.iter().any(|e| e == candidate))
        .unwrap_or_else(|| stem.to_string())
}

/// A confirmed Save Selection: the name the new entry takes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SaveSelectionSpec {
    pub name: String,
}

/// Select ▸ Save Selection….
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSelectionDialog {
    name: String,
    existing: Vec<String>,
}

impl SaveSelectionDialog {
    /// Over the document's saved-selection names, opening at the first free
    /// "Alpha N".
    pub fn new(existing: Vec<String>) -> Self {
        Self {
            name: next_alpha_name(&existing),
            existing,
        }
    }

    /// The name as typed so far.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    pub fn title(&self) -> &'static str {
        tr("ui.selection_name.save.title")
    }

    fn trimmed(&self) -> &str {
        self.name.trim()
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        let name = self.trimmed();
        if name.is_empty() {
            Some(tr("ui.selection_name.empty").to_string())
        } else if self.existing.iter().any(|e| e == name) {
            Some(tr("ui.selection_name.taken").to_string())
        } else {
            None
        }
    }

    /// The spec a confirmation hands over: the trimmed name, when it is
    /// neither blank nor already taken.
    pub fn confirm(&self) -> Option<SaveSelectionSpec> {
        self.blocked_reason().is_none().then(|| SaveSelectionSpec {
            name: self.trimmed().to_string(),
        })
    }

    /// Escape and Enter, without drawing.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<SaveSelectionSpec> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(spec) = self.confirm() {
                return DialogOutcome::Confirmed(spec);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<SaveSelectionSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "save-selection",
            self.title(),
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        design::inspector_field(ui, tr("ui.selection_name.name"), |ui| {
            let field = egui::TextEdit::singleline(&mut self.name)
                .id(ids::save_selection_name())
                .desired_width(sizes::text_field_name());
            let response = ui.add(field);
            if ui.memory(|m| m.focused()).is_none() {
                response.request_focus();
            }
        });
        caption(ui, tr("ui.selection_name.save.caption"));
        action_row(
            ui,
            tr("ui.selection_name.save"),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

/// How a loaded selection meets the live one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum LoadOperation {
    /// Replace the live selection.
    #[default]
    New,
    /// Union with the live selection.
    Add,
    /// Take the loaded coverage away from the live selection.
    Subtract,
    /// Keep only what both cover.
    Intersect,
}

impl LoadOperation {
    pub const ALL: [LoadOperation; 4] = [
        LoadOperation::New,
        LoadOperation::Add,
        LoadOperation::Subtract,
        LoadOperation::Intersect,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LoadOperation::New => tr("ui.selection_name.op.new"),
            LoadOperation::Add => tr("ui.selection_name.op.add"),
            LoadOperation::Subtract => tr("ui.selection_name.op.subtract"),
            LoadOperation::Intersect => tr("ui.selection_name.op.intersect"),
        }
    }
}

/// A confirmed Load Selection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LoadSelectionSpec {
    /// Index into the document's saved selections, and the name it had when
    /// the dialog opened — the shell refuses the load if the two no longer
    /// agree, rather than loading whatever moved into that slot.
    pub index: usize,
    pub name: String,
    pub op: LoadOperation,
    pub invert: bool,
}

/// Select ▸ Load Selection….
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadSelectionDialog {
    names: Vec<String>,
    index: usize,
    op: LoadOperation,
    invert: bool,
    /// Whether the document has a live selection: without one, only New is
    /// meaningful (there is nothing to add to or take from).
    has_selection: bool,
}

impl LoadSelectionDialog {
    /// Over the document's saved-selection names, the most recent chosen.
    pub fn new(names: Vec<String>, has_selection: bool) -> Self {
        let index = names.len().saturating_sub(1);
        Self {
            names,
            index,
            op: LoadOperation::New,
            invert: false,
            has_selection,
        }
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Over the document's saved-selection names, `index` chosen when it
    /// names one (W3-X: the Channels row that was clicked), the most recent
    /// otherwise.
    pub fn new_at(names: Vec<String>, has_selection: bool, index: usize) -> Self {
        let mut dialog = Self::new(names, has_selection);
        dialog.select(index);
        dialog
    }

    /// The index of the entry the dialog would load.
    pub fn selected(&self) -> usize {
        self.index
    }

    pub fn select(&mut self, index: usize) {
        if index < self.names.len() {
            self.index = index;
        }
    }

    pub fn set_operation(&mut self, op: LoadOperation) {
        self.op = op;
    }

    pub fn set_invert(&mut self, invert: bool) {
        self.invert = invert;
    }

    pub fn title(&self) -> &'static str {
        tr("ui.selection_name.load.title")
    }

    /// Why `op` cannot be chosen, or `None` when it can.
    pub fn operation_unavailable(&self, op: LoadOperation) -> Option<&'static str> {
        (!self.has_selection && op != LoadOperation::New)
            .then(|| tr("ui.selection_name.op.needs.selection"))
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        if self.names.is_empty() {
            return Some(tr("ui.selection_name.none.saved").to_string());
        }
        self.operation_unavailable(self.op).map(str::to_string)
    }

    /// The spec a confirmation hands over.
    pub fn confirm(&self) -> Option<LoadSelectionSpec> {
        if self.blocked_reason().is_some() {
            return None;
        }
        Some(LoadSelectionSpec {
            index: self.index,
            name: self.names.get(self.index)?.clone(),
            op: self.op,
            invert: self.invert,
        })
    }

    /// Escape and Enter, without drawing.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<LoadSelectionSpec> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(spec) = self.confirm() {
                return DialogOutcome::Confirmed(spec);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<LoadSelectionSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "load-selection",
            self.title(),
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        let indices: Vec<usize> = (0..self.names.len()).collect();
        let names = self.names.clone();
        design::inspector_field(ui, tr("ui.selection_name.channel"), |ui| {
            combo(
                ui,
                ids::load_selection_source(),
                &mut self.index,
                &indices,
                |i| names.get(i).cloned().unwrap_or_default(),
                |_| None,
            );
        });
        let has_selection = self.has_selection;
        design::inspector_field(ui, tr("ui.selection_name.operation"), |ui| {
            combo(
                ui,
                ids::load_selection_operation(),
                &mut self.op,
                &LoadOperation::ALL,
                |op| op.label().to_string(),
                |op| {
                    (!has_selection && op != LoadOperation::New)
                        .then(|| tr("ui.selection_name.op.needs.selection"))
                },
            );
        });
        checkbox_row(ui, tr("ui.selection_name.invert"), &mut self.invert);
        action_row(
            ui,
            tr("ui.selection_name.load"),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_opens_at_the_first_free_alpha_name() {
        assert_eq!(SaveSelectionDialog::new(Vec::new()).name(), "Alpha 1");
        let taken = vec!["Alpha 1".to_string(), "Sky".to_string()];
        assert_eq!(SaveSelectionDialog::new(taken).name(), "Alpha 2");
        let gap = vec!["Alpha 2".to_string()];
        assert_eq!(SaveSelectionDialog::new(gap).name(), "Alpha 1");
    }

    #[test]
    fn save_confirms_the_trimmed_name_and_refuses_blank_or_taken_ones() {
        let mut dialog = SaveSelectionDialog::new(vec!["Sky".to_string()]);
        assert_eq!(
            dialog.confirm(),
            Some(SaveSelectionSpec {
                name: "Alpha 1".to_string()
            })
        );
        dialog.set_name("  Sky ");
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some(tr("ui.selection_name.taken"))
        );
        dialog.set_name("   ");
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some(tr("ui.selection_name.empty"))
        );
        dialog.set_name(" Horizon ");
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(SaveSelectionSpec {
                name: "Horizon".to_string()
            })
        );
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn load_lists_by_name_and_confirms_the_chosen_entry_and_operation() {
        let names = vec!["Alpha 1".to_string(), "Sky".to_string()];
        let mut dialog = LoadSelectionDialog::new(names, true);
        // The most recent entry is chosen on open.
        assert_eq!(dialog.confirm().map(|s| s.name), Some("Sky".to_string()));
        dialog.select(0);
        dialog.set_operation(LoadOperation::Subtract);
        dialog.set_invert(true);
        assert_eq!(
            dialog.confirm(),
            Some(LoadSelectionSpec {
                index: 0,
                name: "Alpha 1".to_string(),
                op: LoadOperation::Subtract,
                invert: true,
            })
        );
        // Out of range is ignored rather than stored.
        dialog.select(9);
        assert_eq!(dialog.confirm().map(|s| s.index), Some(0));
    }

    /// W3-X: a Channels saved-selection row opens the dialog on the row that
    /// was clicked (`new_at`), not on the newest entry; an index that no
    /// longer names an entry falls back to the newest rather than panicking.
    #[test]
    fn the_dialog_opens_on_the_channels_row_that_was_clicked() {
        let names = vec!["Alpha 1".to_string(), "Alpha 2".to_string()];
        let dialog = LoadSelectionDialog::new_at(names.clone(), false, 0);
        assert_eq!(dialog.selected(), 0);
        assert_eq!(
            dialog.confirm().map(|s| (s.index, s.name)),
            Some((0, "Alpha 1".to_string()))
        );
        let stale = LoadSelectionDialog::new_at(names, false, 7);
        assert_eq!(stale.selected(), 1);
        let hint = tr("ui.docks.channels.saved.hint").to_lowercase();
        assert!(
            hint.contains("load selection"),
            "the hint must name the dialog the click opens: {hint}"
        );
    }

    #[test]
    fn without_a_live_selection_only_new_is_offered() {
        let mut dialog = LoadSelectionDialog::new(vec!["Alpha 1".to_string()], false);
        assert!(dialog.confirm().is_some());
        for op in [
            LoadOperation::Add,
            LoadOperation::Subtract,
            LoadOperation::Intersect,
        ] {
            dialog.set_operation(op);
            assert_eq!(dialog.confirm(), None, "{op:?}");
            assert!(dialog.blocked_reason().is_some());
        }
        let empty = LoadSelectionDialog::new(Vec::new(), true);
        assert_eq!(empty.confirm(), None);
        assert!(empty.blocked_reason().is_some());
    }

    #[test]
    fn both_draw_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut save = SaveSelectionDialog::new(Vec::new());
            assert!(save.show(ctx).is_open());
            let mut load = LoadSelectionDialog::new(vec!["Alpha 1".to_string()], true);
            assert!(load.show(ctx).is_open());
        });
    }
}
