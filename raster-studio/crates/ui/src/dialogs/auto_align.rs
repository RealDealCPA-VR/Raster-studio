//! Edit ▸ Auto-Align Layers and Edit ▸ Auto-Blend Layers (W10-G).
//!
//! Both act on the layers selected in the Layers panel, and both ask one
//! question before they run:
//!
//! * **Auto-Align** — the projection: *Auto* estimates translation, rotation
//!   and scale by feature matching; *Reposition* estimates translation only,
//!   by phase correlation ([`filters::align`] documents both). The first
//!   selected layer is the reference and stays put; every other one gets a
//!   layer transform that lays it over the reference.
//! * **Auto-Blend** — the method: *Panorama* stitches overlapping layers
//!   along seams where they agree; *Stack* keeps the sharpest layer at every
//!   pixel ([`filters::blend_layers`]). The result lands as one new layer
//!   above the selection.
//!
//! The confirmations are parked for their menu arms, the way Trim's is.

use egui::Context;
use filters::align::AlignMethod;
use filters::blend_layers::BlendMethod;

use super::chrome::{action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
use super::controls::combo;
use crate::strings::tr;

/// The projection's name, as the combo shows it.
pub fn align_method_label(method: AlignMethod) -> &'static str {
    match method {
        AlignMethod::Similarity => "Auto",
        AlignMethod::Translation => "Reposition",
    }
}

/// The method's name, as the combo shows it.
pub fn blend_method_label(method: BlendMethod) -> &'static str {
    match method {
        BlendMethod::Panorama => "Panorama",
        BlendMethod::StackImages => "Stack",
    }
}

/// A confirmed Auto-Align.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct AutoAlignSpec {
    pub method: AlignMethod,
}

/// A confirmed Auto-Blend.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct AutoBlendSpec {
    pub method: BlendMethod,
}

/// Edit ▸ Auto-Align Layers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoAlignDialog {
    spec: AutoAlignSpec,
}

impl AutoAlignDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spec(&self) -> AutoAlignSpec {
        self.spec
    }

    pub fn set_spec(&mut self, spec: AutoAlignSpec) {
        self.spec = spec;
    }

    pub fn title(&self) -> String {
        crate::menu::MenuAction::AutoAlignLayers.label()
    }

    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<AutoAlignSpec> {
        if keys.cancel {
            DialogOutcome::Cancelled
        } else if keys.confirm {
            DialogOutcome::Confirmed(self.spec)
        } else {
            DialogOutcome::Open
        }
    }

    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<AutoAlignSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(ctx, "auto-align", &title, None, DialogWidth::Narrow, |ui| {
            combo(
                ui,
                "auto-align-method",
                &mut self.spec.method,
                &AlignMethod::ALL,
                |m| align_method_label(m).to_string(),
                |_| None,
            );
            action_row(ui, tr("ui.adjustment.confirm"), None, &[])
        });
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => DialogOutcome::Confirmed(self.spec),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }
}

/// Edit ▸ Auto-Blend Layers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoBlendDialog {
    spec: AutoBlendSpec,
}

impl AutoBlendDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spec(&self) -> AutoBlendSpec {
        self.spec
    }

    pub fn set_spec(&mut self, spec: AutoBlendSpec) {
        self.spec = spec;
    }

    pub fn title(&self) -> String {
        crate::menu::MenuAction::AutoBlendLayers.label()
    }

    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<AutoBlendSpec> {
        if keys.cancel {
            DialogOutcome::Cancelled
        } else if keys.confirm {
            DialogOutcome::Confirmed(self.spec)
        } else {
            DialogOutcome::Open
        }
    }

    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<AutoBlendSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(ctx, "auto-blend", &title, None, DialogWidth::Narrow, |ui| {
            combo(
                ui,
                "auto-blend-method",
                &mut self.spec.method,
                &BlendMethod::ALL,
                |m| blend_method_label(m).to_string(),
                |_| None,
            );
            action_row(ui, tr("ui.adjustment.confirm"), None, &[])
        });
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => DialogOutcome::Confirmed(self.spec),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_confirms_the_chosen_method_and_escape_cancels() {
        let mut align = AutoAlignDialog::new();
        align.set_spec(AutoAlignSpec {
            method: AlignMethod::Translation,
        });
        assert_eq!(
            align.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(AutoAlignSpec {
                method: AlignMethod::Translation
            })
        );
        assert_eq!(align.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
        let blend = AutoBlendDialog::new();
        assert_eq!(
            blend.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(AutoBlendSpec::default())
        );
    }

    #[test]
    fn every_method_has_a_label() {
        for m in AlignMethod::ALL {
            assert!(!align_method_label(m).is_empty());
        }
        for m in BlendMethod::ALL {
            assert!(!blend_method_label(m).is_empty());
        }
    }

    #[test]
    fn both_draw_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            assert!(AutoAlignDialog::new().show(ctx).is_open());
            assert!(AutoBlendDialog::new().show(ctx).is_open());
        });
    }
}
