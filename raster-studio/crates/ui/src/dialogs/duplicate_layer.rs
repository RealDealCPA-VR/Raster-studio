//! Layer ▸ Duplicate Layer… — the name the copy is given.
//!
//! Photoshop's row asks before it copies (the field opens at "<name> copy"),
//! which is what its ellipsis promises; this dialog is that question and
//! nothing else. The copy itself is the application's — the layer tree and
//! the tiles live there — so the shell parks the confirmed name for the
//! `DuplicateLayer` menu arm the way it parks Trim's options, and that arm
//! makes the copy as one undoable step.
//!
//! No [`super::chrome::Dialog`] impl: that trait confirms to a
//! [`super::action::DialogAction`], and a layer name is not one of those.
//! `show` still folds Escape, Enter and the action row into one
//! [`DialogOutcome`], so the keyboard contract is every other dialog's.

use egui::Context;
use layer_model::LayerId;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::{ids, sizes};
use crate::strings::tr;

/// Layer ▸ Duplicate Layer….
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateLayerDialog {
    source: LayerId,
    name: String,
}

impl DuplicateLayerDialog {
    /// Over `source`, whose name is `current`; the field opens at
    /// "`current` copy", Photoshop's suggestion.
    pub fn new(source: LayerId, current: &str) -> Self {
        Self {
            source,
            name: Self::suggested_name(current),
        }
    }

    /// The name the dialog opens with for a layer called `current`.
    pub fn suggested_name(current: &str) -> String {
        format!("{current} copy")
    }

    /// The layer being copied.
    pub fn source(&self) -> LayerId {
        self.source
    }

    /// The name as typed so far.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    pub fn title(&self) -> &'static str {
        tr("ui.duplicate_layer.title")
    }

    fn confirm_label(&self) -> &'static str {
        "Duplicate"
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        self.name
            .trim()
            .is_empty()
            .then(|| tr("ui.duplicate_layer.name.empty").to_string())
    }

    /// The name a confirmation hands over, trimmed; `None` when it is blank.
    pub fn confirm(&self) -> Option<String> {
        let name = self.name.trim();
        (!name.is_empty()).then(|| name.to_string())
    }

    /// Escape and Enter, without drawing — Escape wins over Enter, like
    /// [`super::chrome::resolve`].
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<String> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(name) = self.confirm() {
                return DialogOutcome::Confirmed(name);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<String> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "duplicate-layer",
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
        design::inspector_field(ui, "Name", |ui| {
            let field = egui::TextEdit::singleline(&mut self.name)
                .id(ids::duplicate_layer_name())
                .desired_width(sizes::text_field_name());
            let response = ui.add(field);
            // The field is the dialog's whole point, so it takes focus on
            // the frame it appears, while nothing else holds it.
            if ui.memory(|m| m.focused()).is_none() {
                response.request_focus();
            }
        });
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_opens_at_the_name_copy_and_confirms_the_trimmed_name() {
        let id = LayerId::new();
        let mut dialog = DuplicateLayerDialog::new(id, "Sky");
        assert_eq!(dialog.source(), id);
        assert_eq!(dialog.name(), "Sky copy");
        assert_eq!(dialog.confirm(), Some("Sky copy".to_string()));
        dialog.set_name("  Clouds  ");
        assert_eq!(dialog.confirm(), Some("Clouds".to_string()));
        assert_eq!(dialog.blocked_reason(), None);
    }

    #[test]
    fn a_blank_name_is_blocked_with_a_reason() {
        let mut dialog = DuplicateLayerDialog::new(LayerId::new(), "Sky");
        dialog.set_name("   ");
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("The name cannot be empty")
        );
        assert_eq!(dialog.resolve(DialogKeys::CONFIRM), DialogOutcome::Open);
    }

    #[test]
    fn resolve_confirms_on_enter_and_cancels_on_escape() {
        let dialog = DuplicateLayerDialog::new(LayerId::new(), "Sky");
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed("Sky copy".to_string())
        );
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = DuplicateLayerDialog::new(LayerId::new(), "Sky");
            assert!(dialog.show(ctx).is_open());
        });
    }
}
