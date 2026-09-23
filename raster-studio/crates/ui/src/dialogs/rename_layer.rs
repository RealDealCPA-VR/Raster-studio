//! Layer ▸ Rename Layer… — one name field over the active layer.
//!
//! Confirms to a [`Command::SetLayerProperties`] carrying only the name, so
//! the rename is one undo step and travels the road every other layer
//! property edit does. A blank name and an unchanged name are both refused
//! with a reason: the first would erase the layer's label, the second would
//! record an undo step that changes nothing.

use editor_core::{Command, LayerPatch};
use egui::Context;
use layer_model::LayerId;

use super::action::DialogAction;
use super::chrome::{
    action_row, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::{ids, sizes};
use crate::strings::tr;

/// Layer ▸ Rename Layer….
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameLayerDialog {
    layer: LayerId,
    original: String,
    name: String,
}

impl RenameLayerDialog {
    /// Over `layer`, whose name is `current`.
    pub fn new(layer: LayerId, current: impl Into<String>) -> Self {
        let original = current.into();
        Self {
            layer,
            name: original.clone(),
            original,
        }
    }

    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The name as typed so far.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// The name a confirmation writes: trimmed of surrounding whitespace.
    fn trimmed(&self) -> &str {
        self.name.trim()
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "rename-layer",
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
                .id(ids::rename_layer_name())
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

impl Dialog for RenameLayerDialog {
    fn title(&self) -> &'static str {
        tr("ui.rename_layer.title")
    }

    fn confirm_label(&self) -> &'static str {
        "Rename"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let name = self.trimmed();
        (!name.is_empty() && name != self.original).then(|| {
            DialogAction::Command(Box::new(Command::SetLayerProperties {
                layer_id: self.layer,
                patch: LayerPatch {
                    name: Some(name.to_string()),
                    ..Default::default()
                },
            }))
        })
    }

    fn blocked_reason(&self) -> Option<String> {
        let name = self.trimmed();
        if name.is_empty() {
            Some(tr("ui.rename_layer.name.empty").to_string())
        } else if name == self.original {
            Some(tr("ui.rename_layer.name.unchanged").to_string())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirming_writes_the_trimmed_name_as_a_property_patch() {
        let id = LayerId::new();
        let mut dialog = RenameLayerDialog::new(id, "Layer 1");
        dialog.set_name("  Sky  ");
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerProperties { layer_id, patch } => {
                    assert_eq!(layer_id, id);
                    assert_eq!(patch.name.as_deref(), Some("Sky"));
                    assert_eq!(patch.visible, None, "only the name travels");
                }
                other => panic!("confirmed to {other:?}"),
            },
            other => panic!("confirmed to {other:?}"),
        }
    }

    #[test]
    fn a_blank_or_unchanged_name_is_blocked_with_a_reason() {
        let mut dialog = RenameLayerDialog::new(LayerId::new(), "Layer 1");
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("The name has not changed")
        );
        dialog.set_name("   ");
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("The name cannot be empty")
        );
    }

    #[test]
    fn resolve_confirms_on_enter_and_cancels_on_escape() {
        let mut dialog = RenameLayerDialog::new(LayerId::new(), "Layer 1");
        dialog.set_name("Renamed");
        assert!(matches!(
            resolve(&dialog, DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(DialogAction::Command(_))
        ));
        assert_eq!(
            resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = RenameLayerDialog::new(LayerId::new(), "Layer 1");
            assert!(dialog.show(ctx).is_open());
        });
    }
}
