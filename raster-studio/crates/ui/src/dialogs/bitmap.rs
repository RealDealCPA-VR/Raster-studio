//! W10-H: Image ▸ Mode ▸ Bitmap… — Photoshop's question: how the grayscale
//! image becomes pure black and white (50% threshold, pattern dither,
//! diffusion dither or a halftone screen with its cell, angle and shape).
//!
//! The dialog hands back a [`color::bitmap::BitmapMethod`] and nothing else;
//! the pixels are the application's (the shell flattens the document and
//! converts the plane as one undoable step). Like Indexed Color, there is no
//! [`super::chrome::Dialog`] impl: the shell parks the confirmed method for
//! the `SetColorMode(Bitmap)` menu arm.

use egui::Context;

use color::bitmap::{BitmapMethod, Halftone, HalftoneShape, MAX_CELL, MIN_CELL};

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{combo, numeric};
use crate::strings::tr;

/// The method's name in the dialog.
pub fn method_label(method: BitmapMethod) -> &'static str {
    match method {
        BitmapMethod::Threshold => tr("ui.bitmap.method.threshold"),
        BitmapMethod::Pattern => tr("ui.bitmap.method.pattern"),
        BitmapMethod::Diffusion => tr("ui.bitmap.method.diffusion"),
        BitmapMethod::Halftone(_) => tr("ui.bitmap.method.halftone"),
    }
}

/// The halftone dot shape's name in the dialog.
pub fn shape_label(shape: HalftoneShape) -> &'static str {
    match shape {
        HalftoneShape::Round => tr("ui.bitmap.shape.round"),
        HalftoneShape::Square => tr("ui.bitmap.shape.square"),
        HalftoneShape::Diamond => tr("ui.bitmap.shape.diamond"),
        HalftoneShape::Line => tr("ui.bitmap.shape.line"),
    }
}

/// Image ▸ Mode ▸ Bitmap….
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BitmapDialog {
    method: BitmapMethod,
    /// The screen the Halftone choice edits, kept while another method is
    /// picked so switching back does not lose it.
    screen: Halftone,
}

impl BitmapDialog {
    pub fn new(method: BitmapMethod) -> Self {
        let screen = match method {
            BitmapMethod::Halftone(h) => h,
            _ => Halftone::default(),
        };
        Self { method, screen }
    }

    /// The method as it stands.
    pub fn method(&self) -> BitmapMethod {
        self.method
    }

    /// Set the method directly — tests and presets.
    pub fn set_method(&mut self, method: BitmapMethod) {
        *self = Self::new(method);
    }

    pub fn title(&self) -> &'static str {
        tr("ui.bitmap.title")
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        (!self.method.is_valid()).then(|| tr("ui.bitmap.bad.cell").to_string())
    }

    /// The method a confirmation hands over, or `None` when it is invalid.
    pub fn confirm(&self) -> Option<BitmapMethod> {
        self.method.is_valid().then_some(self.method)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<BitmapMethod> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(method) = self.confirm() {
                return DialogOutcome::Confirmed(method);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<BitmapMethod> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "bitmap-mode",
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
        caption(ui, tr("ui.bitmap.subtitle"));
        design::section_header(ui, tr("ui.bitmap.method"));
        let choices = [
            BitmapMethod::Threshold,
            BitmapMethod::Pattern,
            BitmapMethod::Diffusion,
            BitmapMethod::Halftone(self.screen),
        ];
        let mut picked = choices
            .iter()
            .position(|m| m.same_kind(self.method))
            .unwrap_or(2);
        combo(
            ui,
            egui::Id::new(("dialogs", "bitmap-method")),
            &mut picked,
            &[0, 1, 2, 3],
            |i| method_label(choices[i]).to_string(),
            |_| None,
        );
        if let BitmapMethod::Halftone(_) = choices[picked] {
            design::section_header(ui, tr("ui.bitmap.cell"));
            let mut cell = f64::from(self.screen.cell);
            numeric(
                ui,
                &mut cell,
                f64::from(MIN_CELL)..=f64::from(MAX_CELL),
                1,
                "",
            );
            self.screen.cell = cell as f32;
            design::section_header(ui, tr("ui.bitmap.angle"));
            let mut angle = f64::from(self.screen.angle);
            numeric(ui, &mut angle, -180.0..=180.0, 1, "");
            self.screen.angle = angle as f32;
            design::section_header(ui, tr("ui.bitmap.shape"));
            combo(
                ui,
                egui::Id::new(("dialogs", "bitmap-shape")),
                &mut self.screen.shape,
                &HalftoneShape::ALL,
                |s| shape_label(s).to_string(),
                |_| None,
            );
            self.method = BitmapMethod::Halftone(self.screen);
        } else {
            self.method = choices[picked];
        }
        action_row(ui, "OK", self.blocked_reason().as_deref(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_diffusion_dither() {
        let dialog = BitmapDialog::default();
        assert_eq!(dialog.confirm(), Some(BitmapMethod::Diffusion));
    }

    #[test]
    fn a_halftone_cell_outside_the_range_is_blocked() {
        let mut dialog = BitmapDialog::default();
        dialog.set_method(BitmapMethod::Halftone(Halftone {
            cell: 1.0,
            ..Halftone::default()
        }));
        assert_eq!(dialog.confirm(), None);
        assert!(dialog.blocked_reason().is_some());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn enter_confirms_the_method_and_escape_wins() {
        let mut dialog = BitmapDialog::default();
        let screen = Halftone {
            cell: 12.0,
            angle: 15.0,
            shape: HalftoneShape::Diamond,
        };
        dialog.set_method(BitmapMethod::Halftone(screen));
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(BitmapMethod::Halftone(screen))
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
    fn every_choice_has_a_label() {
        for m in BitmapMethod::ALL {
            assert!(!method_label(m).is_empty());
        }
        for s in HalftoneShape::ALL {
            assert!(!shape_label(s).is_empty());
        }
    }

    #[test]
    fn it_draws_in_both_appearances_and_keeps_the_method() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = BitmapDialog::new(BitmapMethod::Halftone(Halftone::default()));
            assert!(dialog.show(ctx).is_open());
            assert_eq!(
                dialog.method(),
                BitmapMethod::Halftone(Halftone::default()),
                "drawing a frame must not change the choice"
            );
            let mut dialog = BitmapDialog::new(BitmapMethod::Pattern);
            assert!(dialog.show(ctx).is_open());
            assert_eq!(dialog.method(), BitmapMethod::Pattern);
        });
    }
}
