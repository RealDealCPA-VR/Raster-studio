//! Image ▸ Rotation ▸ Arbitrary… — one angle, any angle.
//!
//! The dialog is deliberately one field: the angle in degrees, positive
//! clockwise. Everything interesting happens after confirm — orthogonal
//! angles take the same exact index-copy path the fixed rotations use, and
//! everything else resamples — so the dialog's whole job is to hand over one
//! finite number without lying about what it will do to the pixels.

use egui::Context;

use super::action::DialogAction;
use super::chrome::{action_row, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome};
use design::tokens::Space;

/// Image ▸ Rotation ▸ Arbitrary….
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArbitraryRotationDialog {
    degrees: f64,
}

impl ArbitraryRotationDialog {
    /// The angle the dialog will commit, in degrees.
    pub fn degrees(&self) -> f64 {
        self.degrees
    }

    /// Set the angle directly — presets and tests.
    pub fn set_degrees(&mut self, degrees: f64) {
        self.degrees = degrees;
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let confirm_label = self.confirm_label();
        let blocked = self.blocked_reason();
        let drawn = modal(
            ctx,
            "arbitrary-rotation",
            self.title(),
            Some(crate::strings::tr("ui.canvas_rotation.rotates.everything")),
            super::chrome::DialogWidth::Narrow,
            |ui| {
                ui.horizontal(|ui| {
                    ui.label("Angle");
                    ui.add(
                        egui::DragValue::new(&mut self.degrees)
                            .speed(1.0)
                            .suffix("°"),
                    );
                });
                ui.add_space(Space::Small.pt());
                ui.label(
                    egui::RichText::new(crate::strings::tr(
                        "ui.canvas_rotation.the.canvas.grows.to.fit.the",
                    ))
                    .text_style(egui::TextStyle::Small),
                );
                action_row(ui, confirm_label, blocked.as_deref(), &[])
            },
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
}

impl Dialog for ArbitraryRotationDialog {
    fn title(&self) -> &'static str {
        crate::strings::tr("ui.canvas_rotation.rotate.canvas")
    }

    fn confirm_label(&self) -> &'static str {
        "Rotate"
    }

    fn confirm(&self) -> Option<DialogAction> {
        self.degrees
            .is_finite()
            .then_some(DialogAction::RotateCanvas(self.degrees))
    }

    fn blocked_reason(&self) -> Option<String> {
        if self.degrees.is_finite() {
            None
        } else {
            Some(crate::strings::tr("ui.canvas_rotation.the.angle.must.be.a.finite").to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::DialogKeys;

    #[test]
    fn confirming_hands_over_the_angle_that_was_set() {
        let mut dialog = ArbitraryRotationDialog::default();
        dialog.set_degrees(37.5);
        assert_eq!(
            dialog.confirm(),
            Some(DialogAction::RotateCanvas(37.5)),
            "the angle survives the trip"
        );
        assert_eq!(dialog.blocked_reason(), None);
    }

    #[test]
    fn zero_degrees_confirms_and_is_the_callers_no_op() {
        let dialog = ArbitraryRotationDialog::default();
        assert_eq!(dialog.confirm(), Some(DialogAction::RotateCanvas(0.0)));
    }

    #[test]
    fn resolve_confirms_on_enter_and_cancels_on_escape() {
        let mut dialog = ArbitraryRotationDialog::default();
        dialog.set_degrees(-12.0);
        assert!(matches!(
            resolve(&dialog, DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(DialogAction::RotateCanvas(-12.0))
        ));
        assert!(matches!(
            resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        ));
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = ArbitraryRotationDialog::default();
            assert!(dialog.show(ctx).is_open());
        });
    }
}
