//! Edit ▸ Fade (W10-G): how much of the last filter, adjustment, fill or
//! stroke to keep, and how to lay it over the pixels it replaced.
//!
//! The dialog asks two things — opacity and blend mode — and hands back a
//! [`FadeSpec`]. The pixels are the shell's: it reads the layer before and
//! after the last step, blends the one over the other and swaps the last
//! history entry for the faded one (`app_shell::fade`). This module owns only
//! the question.
//!
//! No [`super::chrome::Dialog`] impl, for the reason Trim gives: the
//! confirmation is not a [`super::action::DialogAction`], it is parked for the
//! `Fade` menu arm. `show` still folds Escape, Enter and the action row into
//! one [`DialogOutcome`].

use egui::Context;
use layer_model::BlendMode;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::controls::{combo, numeric};
use crate::strings::tr;

/// A confirmed Fade.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FadeSpec {
    /// How much of the step's result to keep, `0..=1`.
    pub opacity: f32,
    /// How the step's result is laid over the pixels it replaced.
    pub mode: BlendMode,
}

impl Default for FadeSpec {
    /// Photoshop's opening state: all of it, Normal.
    fn default() -> Self {
        Self {
            opacity: 1.0,
            mode: BlendMode::Normal,
        }
    }
}

impl FadeSpec {
    /// Whether this fade changes nothing (all of the step, laid on Normally).
    pub fn is_identity(&self) -> bool {
        self.opacity >= 1.0 && self.mode == BlendMode::Normal
    }

    /// Fade one straight-alpha pixel: `after` is the step's result, `before`
    /// the pixel it replaced, both in `0..=1`. The result is the step blended
    /// over the original by the mode, then mixed back toward the original by
    /// `1 - opacity`, in premultiplied terms so a transparent side contributes
    /// nothing.
    pub fn fade(&self, before: [f32; 4], after: [f32; 4]) -> [f32; 4] {
        let k = self.opacity.clamp(0.0, 1.0);
        let blended_rgb = if before[3] <= 0.0 {
            [after[0], after[1], after[2]]
        } else if self.mode.is_separable() {
            std::array::from_fn(|c| self.mode.blend_channel(before[c], after[c]))
        } else {
            self.mode.blend_rgb(
                [before[0], before[1], before[2]],
                [after[0], after[1], after[2]],
            )
        };
        // Premultiply, mix, unpremultiply.
        let alpha = before[3] + (after[3] - before[3]) * k;
        if alpha <= 0.0 {
            return [0.0; 4];
        }
        let mut out = [0.0f32; 4];
        for c in 0..3 {
            let b = before[c] * before[3];
            let a = blended_rgb[c] * after[3];
            out[c] = ((b + (a - b) * k) / alpha).clamp(0.0, 1.0);
        }
        out[3] = alpha.clamp(0.0, 1.0);
        out
    }

    /// [`Self::fade`] over two equal-length RGBA8 buffers; a pixel the step
    /// did not change is kept exactly.
    pub fn fade_rgba8(&self, before: &[u8], after: &[u8]) -> Vec<u8> {
        let unit = |p: &[u8; 4]| -> [f32; 4] { std::array::from_fn(|c| f32::from(p[c]) / 255.0) };
        let mut out = Vec::with_capacity(after.len());
        for (b, a) in before
            .as_chunks::<4>()
            .0
            .iter()
            .zip(after.as_chunks::<4>().0)
        {
            // A pixel the step left alone (outside its selection) stays as
            // it is: a blend mode must not re-blend the original with itself.
            if a == b {
                out.extend_from_slice(b);
                continue;
            }
            let p = self.fade(unit(b), unit(a));
            out.extend(p.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8));
        }
        out
    }

    /// [`Self::fade`] over two equal-length RGBA16 buffers.
    pub fn fade_rgba16(&self, before: &[u16], after: &[u16]) -> Vec<u16> {
        let unit =
            |p: &[u16; 4]| -> [f32; 4] { std::array::from_fn(|c| f32::from(p[c]) / 65535.0) };
        let mut out = Vec::with_capacity(after.len());
        for (b, a) in before
            .as_chunks::<4>()
            .0
            .iter()
            .zip(after.as_chunks::<4>().0)
        {
            if a == b {
                out.extend_from_slice(b);
                continue;
            }
            let p = self.fade(unit(b), unit(a));
            out.extend(p.map(|v| (v * 65535.0).round().clamp(0.0, 65535.0) as u16));
        }
        out
    }
}

/// Edit ▸ Fade.
#[derive(Debug, Clone, PartialEq)]
pub struct FadeDialog {
    /// The history label of the step being faded ("Invert", "Fill").
    step: String,
    /// Percent, `0..=100`, as the field edits it.
    opacity_percent: f64,
    mode: BlendMode,
}

impl FadeDialog {
    /// Over the step named `step`.
    pub fn new(step: impl Into<String>) -> Self {
        Self {
            step: step.into(),
            opacity_percent: 100.0,
            mode: BlendMode::Normal,
        }
    }

    /// The step being faded.
    pub fn step(&self) -> &str {
        &self.step
    }

    pub fn spec(&self) -> FadeSpec {
        FadeSpec {
            opacity: (self.opacity_percent / 100.0).clamp(0.0, 1.0) as f32,
            mode: self.mode,
        }
    }

    /// Set the answer directly — tests and presets.
    pub fn set_spec(&mut self, spec: FadeSpec) {
        self.opacity_percent = f64::from(spec.opacity.clamp(0.0, 1.0)) * 100.0;
        self.mode = spec.mode;
    }

    /// "Fade Invert": the menu row's own word and the step it fades.
    pub fn title(&self) -> String {
        let fade = crate::menu::MenuAction::Fade.label();
        format!("{} {}", fade.trim_end_matches('…'), self.step)
    }

    /// Why the primary action is unavailable: a Fade that keeps all of the
    /// step on Normal changes nothing.
    pub fn blocked_reason(&self) -> Option<String> {
        self.spec()
            .is_identity()
            .then(|| tr("ui.adjustment.blocked.identity").to_string())
    }

    pub fn confirm(&self) -> Option<FadeSpec> {
        (!self.spec().is_identity()).then(|| self.spec())
    }

    /// Escape and Enter, without drawing — Escape wins.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<FadeSpec> {
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

    /// Draw one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<FadeSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(ctx, "fade", &title, None, DialogWidth::Narrow, |ui| {
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
        ui.horizontal(|ui| {
            ui.label(tr("ui.layer_style.blending.opacity"));
            numeric(ui, &mut self.opacity_percent, 0.0..=100.0, 0, "%");
        });
        ui.horizontal(|ui| {
            ui.label(tr("ui.layer_style.blending.mode"));
            combo(
                ui,
                "fade-mode",
                &mut self.mode,
                &BlendMode::ALL,
                |m| m.label().to_string(),
                |_| None,
            );
        });
        let blocked = self.blocked_reason();
        action_row(ui, tr("ui.adjustment.confirm"), blocked.as_deref(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifty_percent_normal_is_the_midpoint() {
        let spec = FadeSpec {
            opacity: 0.5,
            mode: BlendMode::Normal,
        };
        let p = spec.fade([0.2, 0.4, 1.0, 1.0], [0.8, 0.6, 0.0, 1.0]);
        for (got, want) in p.iter().zip([0.5, 0.5, 0.5, 1.0]) {
            assert!((got - want).abs() < 1e-6, "{p:?}");
        }
    }

    #[test]
    fn full_opacity_normal_keeps_the_step_and_zero_keeps_the_original() {
        let (b, a) = ([0.1, 0.2, 0.3, 1.0], [0.9, 0.8, 0.7, 1.0]);
        let full = FadeSpec::default().fade(b, a);
        let none = FadeSpec {
            opacity: 0.0,
            mode: BlendMode::Normal,
        }
        .fade(b, a);
        for c in 0..4 {
            assert!((full[c] - a[c]).abs() < 1e-6);
            assert!((none[c] - b[c]).abs() < 1e-6);
        }
    }

    #[test]
    fn a_blend_mode_lays_the_step_over_the_original() {
        let spec = FadeSpec {
            opacity: 1.0,
            mode: BlendMode::Multiply,
        };
        let p = spec.fade([0.5, 0.5, 0.5, 1.0], [0.5, 1.0, 0.0, 1.0]);
        assert!((p[0] - 0.25).abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6 && p[2].abs() < 1e-6);
    }

    /// A pixel the step left alone (outside its selection) is kept exactly,
    /// even in a mode that would change a pixel blended with itself
    /// (Multiply darkens `x` to `x * x`, Screen lightens it).
    #[test]
    fn a_pixel_the_step_did_not_change_is_kept_exactly() {
        for mode in [BlendMode::Multiply, BlendMode::Screen] {
            let spec = FadeSpec { opacity: 1.0, mode };
            // Pixel 0 changed (inverted); pixel 1 did not.
            let before8 = [200u8, 100, 50, 255, 128, 64, 32, 255];
            let after8 = [55u8, 155, 205, 255, 128, 64, 32, 255];
            let out8 = spec.fade_rgba8(&before8, &after8);
            assert_eq!(&out8[4..], &before8[4..], "{mode:?}: 8-bit untouched pixel");
            assert_ne!(
                &out8[..4],
                &after8[..4],
                "{mode:?}: the changed pixel is blended"
            );
            let before16 = [51400u16, 25700, 12850, 65535, 32896, 16448, 8224, 65535];
            let after16 = [14135u16, 39835, 52685, 65535, 32896, 16448, 8224, 65535];
            let out16 = spec.fade_rgba16(&before16, &after16);
            assert_eq!(
                &out16[4..],
                &before16[4..],
                "{mode:?}: 16-bit untouched pixel"
            );
            assert_ne!(
                &out16[..4],
                &after16[..4],
                "{mode:?}: the changed pixel is blended"
            );
        }
    }

    #[test]
    fn the_identity_is_blocked_and_a_change_confirms() {
        let mut dialog = FadeDialog::new("Invert");
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().is_some());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
        let spec = FadeSpec {
            opacity: 0.5,
            mode: BlendMode::Normal,
        };
        dialog.set_spec(spec);
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(spec)
        );
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
        assert!(dialog.title().ends_with("Invert"), "{}", dialog.title());
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = FadeDialog::new("Invert");
            assert!(dialog.show(ctx).is_open());
        });
    }
}
