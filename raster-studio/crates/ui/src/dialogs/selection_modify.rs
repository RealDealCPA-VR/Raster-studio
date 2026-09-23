//! Select ▸ Modify ▸ Border… / Smooth… / Expand… / Contract… / Feather… —
//! one small numeric dialog per operation, asking the amount in pixels.
//!
//! Photopea asks for every one of these; the menu rows carry the ellipsis
//! that promises the question. The dialog hands back a [`ModifySpec`] and
//! nothing else: the morphology is the `selection` crate's, run by the shell
//! over the live selection as one undoable `SetSelection` step. This module
//! owns only the question and its bounds.
//!
//! No [`super::chrome::Dialog`] impl, for the reason Trim has none: that
//! trait confirms to a [`super::action::DialogAction`], and an amount is not
//! one. The shell parks the confirmed spec for the `Modify` menu arm. `show`
//! still folds Escape, Enter and the action row into one [`DialogOutcome`],
//! so the keyboard contract is every other dialog's.

use egui::Context;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::numeric;
use super::ids;
use crate::menu::ModifySelection;
use crate::strings::tr;

/// The largest radius the selection morphology accepts
/// (`selection::modify::MAX_RADIUS`), restated as the dialog's ceiling so the
/// field cannot offer a number the operation would refuse.
pub const MAX_MODIFY_PX: f32 = 512.0;

/// The amount every Modify dialog opens at, in pixels — the fixed amount the
/// Modify commands ran at before they had a dialog (`MODIFY_RADIUS` in the
/// app shell, still used by a caller that bypasses the dialog), so confirming
/// the dialog unchanged gives the selection those commands always gave.
pub const DEFAULT_MODIFY_PX: f32 = 4.0;

/// A confirmed Select ▸ Modify: which operation, and how far.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ModifySpec {
    pub op: ModifySelection,
    /// Pixels. Whole pixels for the four morphology operations; Feather keeps
    /// one decimal (a Gaussian radius is not a pixel count).
    pub amount: f32,
}

impl ModifySpec {
    /// `op` at the opening amount.
    pub fn new(op: ModifySelection) -> Self {
        Self {
            op,
            amount: DEFAULT_MODIFY_PX,
        }
    }

    /// The range the field offers for `op`, in pixels: Photopea's bounds,
    /// capped at what the morphology accepts.
    pub fn range(op: ModifySelection) -> std::ops::RangeInclusive<f32> {
        match op {
            ModifySelection::Border => 1.0..=200.0,
            ModifySelection::Feather => 0.1..=MAX_MODIFY_PX,
            ModifySelection::Smooth | ModifySelection::Expand | ModifySelection::Contract => {
                1.0..=500.0
            }
        }
    }

    /// Decimals the field shows for `op`.
    pub fn decimals(op: ModifySelection) -> usize {
        match op {
            ModifySelection::Feather => 1,
            _ => 0,
        }
    }

    /// The amount as whole pixels, for the four morphology operations.
    pub fn whole_px(&self) -> u32 {
        self.amount.round().max(0.0) as u32
    }

    /// Whether the amount is one the operation can run.
    pub fn is_valid(&self) -> bool {
        self.amount.is_finite() && Self::range(self.op).contains(&self.amount)
    }
}

/// Select ▸ Modify ▸ <op>….
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionModifyDialog {
    spec: ModifySpec,
}

impl SelectionModifyDialog {
    /// The dialog for `op`, at the opening amount.
    pub fn new(op: ModifySelection) -> Self {
        Self {
            spec: ModifySpec::new(op),
        }
    }

    /// The spec as it stands.
    pub fn spec(&self) -> ModifySpec {
        self.spec
    }

    /// Set the amount — the field's route, and the tests'. Whole-pixel
    /// operations round, so what is confirmed is what the field shows.
    pub fn set_amount(&mut self, amount: f32) {
        self.spec.amount = if ModifySpec::decimals(self.spec.op) == 0 {
            amount.round()
        } else {
            (amount * 10.0).round() / 10.0
        };
    }

    pub fn title(&self) -> &'static str {
        match self.spec.op {
            ModifySelection::Border => tr("ui.selection_modify.border.title"),
            ModifySelection::Smooth => tr("ui.selection_modify.smooth.title"),
            ModifySelection::Expand => tr("ui.selection_modify.expand.title"),
            ModifySelection::Contract => tr("ui.selection_modify.contract.title"),
            ModifySelection::Feather => tr("ui.selection_modify.feather.title"),
        }
    }

    /// The field's label: what the amount measures for this operation.
    fn field_label(&self) -> &'static str {
        match self.spec.op {
            ModifySelection::Border => tr("ui.selection_modify.width"),
            ModifySelection::Smooth => tr("ui.selection_modify.sample.radius"),
            ModifySelection::Expand => tr("ui.selection_modify.expand.by"),
            ModifySelection::Contract => tr("ui.selection_modify.contract.by"),
            ModifySelection::Feather => tr("ui.selection_modify.feather.radius"),
        }
    }

    fn confirm_label(&self) -> &'static str {
        tr("ui.selection_modify.apply")
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        (!self.spec.is_valid()).then(|| {
            let range = ModifySpec::range(self.spec.op);
            format!(
                "{} {} {} {} {}",
                tr("ui.selection_modify.out.of.range"),
                range.start(),
                tr("ui.selection_modify.range.to"),
                range.end(),
                tr("ui.selection_modify.px")
            )
        })
    }

    /// The spec a confirmation hands over, or `None` when the amount is out
    /// of range.
    pub fn confirm(&self) -> Option<ModifySpec> {
        self.spec.is_valid().then_some(self.spec)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter, like
    /// [`super::chrome::resolve`].
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<ModifySpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<ModifySpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "selection-modify",
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
        let op = self.spec.op;
        let range = ModifySpec::range(op);
        let mut value = f64::from(self.spec.amount);
        design::inspector_field(ui, self.field_label(), |ui| {
            let response = ui
                .push_id(ids::selection_modify_amount(), |ui| {
                    numeric(
                        ui,
                        &mut value,
                        f64::from(*range.start())..=f64::from(*range.end()),
                        ModifySpec::decimals(op),
                        tr("ui.selection_modify.px"),
                    )
                })
                .inner;
            // The amount is the dialog's whole point: it takes focus on the
            // frame it appears, while nothing else holds it.
            if ui.memory(|m| m.focused()).is_none() {
                response.request_focus();
            }
        });
        if (value as f32 - self.spec.amount).abs() > f32::EPSILON {
            self.set_amount(value as f32);
        }
        if op == ModifySelection::Border {
            caption(ui, tr("ui.selection_modify.border.caption"));
        }
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
    fn every_operation_opens_at_the_default_amount_and_confirms_it() {
        for op in ModifySelection::ALL {
            let dialog = SelectionModifyDialog::new(*op);
            assert_eq!(
                dialog.confirm(),
                Some(ModifySpec {
                    op: *op,
                    amount: DEFAULT_MODIFY_PX
                })
            );
            assert!(!dialog.title().is_empty(), "{op:?} has no title");
            assert_eq!(dialog.blocked_reason(), None);
        }
    }

    #[test]
    fn an_amount_outside_the_operation_range_is_blocked_with_a_reason() {
        let mut dialog = SelectionModifyDialog::new(ModifySelection::Border);
        dialog.set_amount(201.0);
        assert_eq!(dialog.confirm(), None);
        let reason = dialog.blocked_reason().expect("a reason");
        assert!(reason.contains("200"), "{reason}");
        assert!(reason.is_ascii(), "a typed glyph in {reason:?}");
        assert!(reason.ends_with(" to 200 px"), "{reason}");
        dialog.set_amount(0.0);
        assert_eq!(dialog.confirm(), None);
        dialog.set_amount(12.0);
        assert_eq!(dialog.confirm().map(|s| s.whole_px()), Some(12));
    }

    #[test]
    fn whole_pixel_operations_round_and_feather_keeps_a_decimal() {
        let mut expand = SelectionModifyDialog::new(ModifySelection::Expand);
        expand.set_amount(7.6);
        assert_eq!(expand.spec().amount, 8.0);
        let mut feather = SelectionModifyDialog::new(ModifySelection::Feather);
        feather.set_amount(2.54);
        assert!((feather.spec().amount - 2.5).abs() < 1e-6);
        assert!(feather.confirm().is_some());
    }

    #[test]
    fn enter_confirms_and_escape_wins() {
        let dialog = SelectionModifyDialog::new(ModifySelection::Contract);
        assert!(matches!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(ModifySpec {
                op: ModifySelection::Contract,
                ..
            })
        ));
        let both = DialogKeys {
            confirm: true,
            cancel: true,
        };
        assert_eq!(dialog.resolve(both), DialogOutcome::Cancelled);
        assert!(dialog.resolve(DialogKeys::NONE).is_open());
    }

    #[test]
    fn it_draws_every_operation_in_both_appearances_with_its_amount_field() {
        for op in ModifySelection::ALL {
            super::super::chrome::test_support::frame_both_themes(|ctx| {
                let mut dialog = SelectionModifyDialog::new(*op);
                assert!(dialog.show(ctx).is_open());
            });
        }
    }
}
