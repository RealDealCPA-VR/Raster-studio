//! Image ▸ Trim… — Photopea's options: what counts as "empty", and which
//! sides to trim away.
//!
//! The dialog hands back a [`TrimSpec`] and nothing else; the pixels are the
//! application's, so the bounds are computed there (the shell composites the
//! canvas, judges every pixel against the basis, and resizes as one undoable
//! step). This module owns only the question.
//!
//! No [`super::chrome::Dialog`] impl: that trait confirms to a
//! [`super::action::DialogAction`], and a trim spec is not one of those —
//! the shell parks the confirmed spec for the `Trim` menu arm the way it
//! parks an adjustment's parameters. `show` still folds Escape, Enter and
//! the action row into one [`DialogOutcome`], so the keyboard contract is the
//! same as every other dialog's.

use egui::Context;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, combo};
use super::ids;
use crate::strings::tr;

/// What a "trimmable" pixel is judged against.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum TrimBasis {
    /// Pixels with zero alpha.
    #[default]
    Transparent,
    /// Pixels equal to the top-left corner's colour.
    TopLeftColor,
    /// Pixels equal to the bottom-right corner's colour.
    BottomRightColor,
}

impl TrimBasis {
    pub const ALL: [TrimBasis; 3] = [
        TrimBasis::Transparent,
        TrimBasis::TopLeftColor,
        TrimBasis::BottomRightColor,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TrimBasis::Transparent => tr("ui.trim.transparent.pixels"),
            TrimBasis::TopLeftColor => tr("ui.trim.top.left.color"),
            TrimBasis::BottomRightColor => tr("ui.trim.bottom.right.color"),
        }
    }
}

/// The confirmed trim: a basis and the sides to take away.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TrimSpec {
    pub basis: TrimBasis,
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl Default for TrimSpec {
    /// Photopea's opening state: transparent pixels, every side.
    fn default() -> Self {
        Self {
            basis: TrimBasis::Transparent,
            top: true,
            bottom: true,
            left: true,
            right: true,
        }
    }
}

impl TrimSpec {
    /// Whether at least one side is chosen — a trim of no side does nothing.
    pub fn is_valid(&self) -> bool {
        self.top || self.bottom || self.left || self.right
    }
}

/// Image ▸ Trim….
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrimDialog {
    spec: TrimSpec,
}

impl TrimDialog {
    pub fn new(spec: TrimSpec) -> Self {
        Self { spec }
    }

    /// The spec as it stands.
    pub fn spec(&self) -> TrimSpec {
        self.spec
    }

    /// Set the spec directly — tests and presets.
    pub fn set_spec(&mut self, spec: TrimSpec) {
        self.spec = spec;
    }

    pub fn title(&self) -> &'static str {
        "Trim"
    }

    fn confirm_label(&self) -> &'static str {
        "Trim"
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        (!self.spec.is_valid()).then(|| tr("ui.trim.choose.a.side").to_string())
    }

    /// The spec a confirmation hands over, or `None` when no side is chosen.
    pub fn confirm(&self) -> Option<TrimSpec> {
        self.spec.is_valid().then_some(self.spec)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter, like
    /// [`super::chrome::resolve`].
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<TrimSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<TrimSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(ctx, "trim", self.title(), None, DialogWidth::Narrow, |ui| {
            self.body(ui)
        });
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
        caption(ui, tr("ui.trim.subtitle"));
        design::section_header(ui, tr("ui.trim.based.on"));
        combo(
            ui,
            ids::trim_basis(),
            &mut self.spec.basis,
            &TrimBasis::ALL,
            |b| b.label().to_string(),
            |_| None,
        );
        design::section_header(ui, tr("ui.trim.trim.away"));
        ui.horizontal(|ui| {
            checkbox_row(ui, "Top", &mut self.spec.top);
            checkbox_row(ui, "Left", &mut self.spec.left);
        });
        ui.horizontal(|ui| {
            checkbox_row(ui, "Bottom", &mut self.spec.bottom);
            checkbox_row(ui, "Right", &mut self.spec.right);
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
    fn the_default_trims_transparent_pixels_from_every_side() {
        let dialog = TrimDialog::default();
        assert_eq!(dialog.confirm(), Some(TrimSpec::default()));
        assert_eq!(dialog.spec().basis, TrimBasis::Transparent);
        assert!(
            dialog.spec().top && dialog.spec().bottom && dialog.spec().left && dialog.spec().right
        );
        assert_eq!(dialog.blocked_reason(), None);
    }

    #[test]
    fn a_trim_of_no_side_is_blocked_with_a_reason() {
        let mut dialog = TrimDialog::default();
        dialog.set_spec(TrimSpec {
            top: false,
            bottom: false,
            left: false,
            right: false,
            ..TrimSpec::default()
        });
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("Choose at least one side to trim")
        );
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn enter_confirms_the_spec_that_was_set_and_escape_wins() {
        let mut dialog = TrimDialog::default();
        let spec = TrimSpec {
            basis: TrimBasis::TopLeftColor,
            right: false,
            ..TrimSpec::default()
        };
        dialog.set_spec(spec);
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(spec)
        );
        assert_eq!(
            dialog.resolve(DialogKeys {
                confirm: true,
                cancel: true,
            }),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn every_basis_has_a_label() {
        for basis in TrimBasis::ALL {
            assert!(!basis.label().is_empty(), "{basis:?}");
        }
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = TrimDialog::default();
            assert!(dialog.show(ctx).is_open());
        });
    }
}
