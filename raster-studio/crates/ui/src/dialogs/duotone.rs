//! W10-H: Image ▸ Mode ▸ Duotone… — Photoshop's Duotone Options: the type
//! (mono, duo, tri or quadtone), each ink's colour and each ink's curve (how
//! much of it prints at a given tint), over a black-to-white preview drawn
//! through the inks.
//!
//! The dialog hands back a [`color::duotone::DuotoneSpec`] and nothing
//! else; the shell renders every grayscale pixel through the inks as one
//! undoable step. Like Indexed Color, there is no
//! [`super::chrome::Dialog`] impl: the shell parks the confirmed spec for
//! the `SetColorMode(Duotone)` menu arm.

use egui::Context;

use color::duotone::{DuotoneSpec, DuotoneType};

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{combo, integer};
use super::sizes;
use crate::strings::tr;

/// The tints the curve fields sit at, as fractions of full ink.
pub const CURVE_TINTS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

/// How many cells the preview ramp is drawn in.
const PREVIEW_STEPS: usize = 32;

/// The type's name in the dialog.
pub fn type_label(kind: DuotoneType) -> &'static str {
    match kind {
        DuotoneType::Monotone => tr("ui.duotone.type.mono"),
        DuotoneType::Duotone => tr("ui.duotone.type.duo"),
        DuotoneType::Tritone => tr("ui.duotone.type.tri"),
        DuotoneType::Quadtone => tr("ui.duotone.type.quad"),
    }
}

/// Image ▸ Mode ▸ Duotone….
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DuotoneDialog {
    spec: DuotoneSpec,
}

impl DuotoneDialog {
    pub fn new(spec: DuotoneSpec) -> Self {
        Self { spec }
    }

    /// The spec as it stands.
    pub fn spec(&self) -> &DuotoneSpec {
        &self.spec
    }

    /// Set the spec directly — tests and presets.
    pub fn set_spec(&mut self, spec: DuotoneSpec) {
        self.spec = spec;
    }

    pub fn title(&self) -> &'static str {
        tr("ui.duotone.title")
    }

    /// The spec a confirmation hands over, or `None` when it is invalid.
    pub fn confirm(&self) -> Option<DuotoneSpec> {
        self.spec.is_valid().then(|| self.spec.clone())
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<DuotoneSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DuotoneSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "duotone-mode",
            self.title(),
            None,
            DialogWidth::Medium,
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
        caption(ui, tr("ui.duotone.subtitle"));
        design::section_header(ui, tr("ui.duotone.type"));
        let mut kind = self.spec.kind();
        if combo(
            ui,
            egui::Id::new(("dialogs", "duotone-type")),
            &mut kind,
            &DuotoneType::ALL,
            |k| type_label(k).to_string(),
            |_| None,
        ) {
            self.spec.set_kind(kind);
        }
        caption(ui, tr("ui.duotone.curve"));
        for (index, ink) in self.spec.inks.iter_mut().enumerate() {
            design::section_header(ui, &format!("{} {}", tr("ui.duotone.ink"), index + 1));
            ui.horizontal(|ui| {
                egui::color_picker::color_edit_button_srgb(ui, &mut ink.color);
                for tint in CURVE_TINTS {
                    let mut percent = (ink.amount(tint) * 100.0).round() as i64;
                    let before = percent;
                    integer(ui, &mut percent, 0..=100);
                    if percent != before {
                        set_curve_point(&mut ink.curve, tint, percent as f32 / 100.0);
                    }
                }
            });
        }
        design::section_header(ui, tr("ui.duotone.preview"));
        self.preview(ui);
        action_row(ui, "OK", None, &[])
    }

    /// The black-to-white ramp printed through the inks.
    fn preview(&self, ui: &mut egui::Ui) {
        let size = egui::Vec2::new(sizes::color_strip_width(), sizes::gradient_bar_height());
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        for (step, color) in preview_ramp(&self.spec).iter().enumerate() {
            let x0 = rect.left() + rect.width() * step as f32 / PREVIEW_STEPS as f32;
            let x1 = rect.left() + rect.width() * (step + 1) as f32 / PREVIEW_STEPS as f32;
            let cell =
                egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()));
            ui.painter().rect_filled(
                cell,
                egui::Rounding::ZERO,
                egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], u8::MAX),
            );
        }
    }
}

/// The preview ramp's colours: [`PREVIEW_STEPS`] greys from black to white,
/// each printed through `spec`.
pub fn preview_ramp(spec: &DuotoneSpec) -> Vec<[u8; 3]> {
    (0..PREVIEW_STEPS)
        .map(|i| spec.render((i * 255 / (PREVIEW_STEPS - 1)) as u8))
        .collect()
}

/// Set the ink printed at `tint` on a curve: the point at that tint is
/// replaced (or added), so the five fields each own one point.
pub fn set_curve_point(curve: &mut Vec<[f32; 2]>, tint: f32, ink: f32) {
    // A curve that is not already on the five tints is resampled onto them
    // first, so editing one field never bends the others.
    let on_grid = curve.len() == CURVE_TINTS.len()
        && curve
            .iter()
            .zip(CURVE_TINTS)
            .all(|(p, t)| (p[0] - t).abs() < 1e-6);
    if !on_grid {
        let probe = color::duotone::DuotoneInk {
            color: [0, 0, 0],
            curve: curve.clone(),
        };
        *curve = CURVE_TINTS.iter().map(|&t| [t, probe.amount(t)]).collect();
    }
    if let Some(point) = curve.iter_mut().find(|p| (p[0] - tint).abs() < 1e-6) {
        point[1] = ink.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_a_two_ink_duotone() {
        let dialog = DuotoneDialog::default();
        let spec = dialog.confirm().unwrap();
        assert_eq!(spec.kind(), DuotoneType::Duotone);
        assert_eq!(spec.inks.len(), 2);
    }

    #[test]
    fn an_empty_ink_list_cannot_be_confirmed() {
        let mut dialog = DuotoneDialog::default();
        dialog.set_spec(DuotoneSpec { inks: vec![] });
        assert_eq!(dialog.confirm(), None);
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn enter_confirms_the_spec_and_escape_wins() {
        let mut dialog = DuotoneDialog::default();
        let spec = DuotoneSpec::of_type(DuotoneType::Tritone);
        dialog.set_spec(spec.clone());
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
    fn a_curve_field_moves_only_its_own_point() {
        let mut curve = vec![[0.0, 0.0], [1.0, 1.0]];
        set_curve_point(&mut curve, 0.5, 0.2);
        assert_eq!(curve.len(), 5);
        let ink = color::duotone::DuotoneInk {
            color: [0, 0, 0],
            curve: curve.clone(),
        };
        assert!((ink.amount(0.5) - 0.2).abs() < 1e-6);
        assert!((ink.amount(0.25) - 0.25).abs() < 1e-6, "{curve:?}");
        assert!((ink.amount(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_preview_ramp_runs_through_the_inks() {
        let spec = DuotoneSpec::default();
        let ramp = preview_ramp(&spec);
        assert_eq!(ramp.len(), PREVIEW_STEPS);
        assert_eq!(ramp[0], spec.render(0));
        assert_eq!(ramp[PREVIEW_STEPS - 1], [255, 255, 255]);
    }

    #[test]
    fn every_type_has_a_label() {
        for k in DuotoneType::ALL {
            assert!(!type_label(k).is_empty());
        }
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = DuotoneDialog::new(DuotoneSpec::of_type(DuotoneType::Quadtone));
            assert!(dialog.show(ctx).is_open());
            assert_eq!(dialog.spec().inks.len(), 4, "drawing must not drop an ink");
        });
    }
}
