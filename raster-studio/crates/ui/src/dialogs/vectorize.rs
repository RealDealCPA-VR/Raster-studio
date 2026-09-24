//! W10-E: Image ▸ Vectorize Bitmap….
//!
//! Photopea's Vectorize Bitmap turns the active layer's pixels into shape
//! layers, one per colour. The tracing is `vector::trace` (posterize by
//! median cut, stack the colour regions, follow each region's pixel cracks,
//! fit cubic Béziers by Schneider's method — its module header documents the
//! algorithm); this dialog asks its three questions and hands back a
//! [`VectorizeSpec`].

use egui::Context;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, integer, numeric};
use crate::strings::tr;

/// The most colours the dialog offers (the tracer's own ceiling).
pub const MAX_COLORS: u32 = 64;

/// A confirmed Vectorize Bitmap.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VectorizeSpec {
    /// Posterize to at most this many colours, `1..=`[`MAX_COLORS`]; one
    /// shape layer per colour.
    pub colors: u32,
    /// How far, in pixels, a fitted curve may stray from the traced outline.
    pub tolerance: f64,
    /// Edges at least this many pixels long on both sides of a vertex keep
    /// it a sharp corner.
    pub corner: f64,
    /// Hide the source layer once the shapes are made.
    pub hide_source: bool,
}

impl Default for VectorizeSpec {
    fn default() -> Self {
        Self {
            colors: 8,
            tolerance: 1.0,
            corner: 3.0,
            hide_source: true,
        }
    }
}

impl VectorizeSpec {
    /// Whether every field is in range.
    pub fn is_valid(&self) -> bool {
        (1..=MAX_COLORS).contains(&self.colors)
            && self.tolerance.is_finite()
            && self.tolerance > 0.0
            && self.corner.is_finite()
            && self.corner >= 1.0
    }
}

/// Image ▸ Vectorize Bitmap….
#[derive(Clone, Debug, Default)]
pub struct VectorizeDialog {
    spec: VectorizeSpec,
}

impl VectorizeDialog {
    pub fn new(spec: VectorizeSpec) -> Self {
        Self { spec }
    }

    pub fn spec(&self) -> VectorizeSpec {
        self.spec
    }

    pub fn set_spec(&mut self, spec: VectorizeSpec) {
        self.spec = spec;
    }

    pub fn blocked_reason(&self) -> Option<&'static str> {
        (!self.spec.is_valid()).then(|| tr("ui.vectorize.invalid"))
    }

    pub fn confirm(&self) -> Option<VectorizeSpec> {
        self.spec.is_valid().then_some(self.spec)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<VectorizeSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<VectorizeSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "w10e-vectorize",
            tr("ui.vectorize.title"),
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
        caption(ui, tr("ui.vectorize.subtitle"));
        design::inspector_field(ui, tr("ui.vectorize.colors"), |ui| {
            let mut n = i64::from(self.spec.colors);
            if integer(ui, &mut n, 1..=i64::from(MAX_COLORS)).changed() {
                self.spec.colors = n.clamp(1, i64::from(MAX_COLORS)) as u32;
            }
        });
        design::inspector_field(ui, tr("ui.vectorize.tolerance"), |ui| {
            numeric(ui, &mut self.spec.tolerance, 0.1..=10.0, 1, "px");
        });
        design::inspector_field(ui, tr("ui.vectorize.corner"), |ui| {
            numeric(ui, &mut self.spec.corner, 1.0..=20.0, 1, "px");
        });
        checkbox_row(
            ui,
            tr("ui.vectorize.hide.source"),
            &mut self.spec.hide_source,
        );
        action_row(ui, tr("ui.vectorize.run"), self.blocked_reason(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_confirm_and_a_bad_field_blocks() {
        let mut dialog = VectorizeDialog::default();
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(VectorizeSpec::default())
        );
        dialog.set_spec(VectorizeSpec {
            colors: 0,
            ..VectorizeSpec::default()
        });
        assert!(dialog.blocked_reason().is_some());
        dialog.set_spec(VectorizeSpec {
            tolerance: f64::NAN,
            ..VectorizeSpec::default()
        });
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = VectorizeDialog::default();
            assert!(dialog.show(ctx).is_open());
        });
    }
}
