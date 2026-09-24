//! W10-H: Image ▸ Calculations… — Photoshop's two-source channel
//! arithmetic: one channel of Source 1 is blended onto one channel of
//! Source 2 (a blending mode and an opacity, optionally through a mask
//! channel), and the grey result becomes a new channel (a saved selection),
//! the live selection, or a new grayscale document.
//!
//! The dialog hands back a [`CalculationsSpec`]; the arithmetic is the
//! shell's. The source controls are Apply Image's
//! ([`super::apply_image::source_block`]).

use egui::Context;
use layer_model::BlendMode;

use super::apply_image::{blend_block, source_block, ImageSource, SourceChannel, SourceDocument};
use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, combo};
use crate::strings::tr;

/// Where the result goes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum CalculationResult {
    /// A new alpha channel: a saved selection named "Alpha N".
    #[default]
    NewChannel,
    /// The live selection, one undo step.
    Selection,
    /// A new grayscale document.
    NewDocument,
}

impl CalculationResult {
    pub const ALL: [CalculationResult; 3] = [
        CalculationResult::NewChannel,
        CalculationResult::Selection,
        CalculationResult::NewDocument,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CalculationResult::NewChannel => tr("ui.calculations.result.channel"),
            CalculationResult::Selection => tr("ui.calculations.result.selection"),
            CalculationResult::NewDocument => tr("ui.calculations.result.document"),
        }
    }
}

/// The confirmed Calculations.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CalculationsSpec {
    /// Blended onto `source2` (the base).
    pub source1: ImageSource,
    pub source2: ImageSource,
    pub blend: BlendMode,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub mask: Option<ImageSource>,
    pub result: CalculationResult,
}

/// Image ▸ Calculations….
#[derive(Debug, Clone, PartialEq)]
pub struct CalculationsDialog {
    documents: Vec<SourceDocument>,
    spec: CalculationsSpec,
    mask: ImageSource,
    use_mask: bool,
}

impl CalculationsDialog {
    /// Open over `documents` (the same-size open documents, the active one
    /// first): both sources the active document's merged Gray, Multiply at
    /// 100%, into a new channel.
    pub fn new(documents: Vec<SourceDocument>) -> Self {
        let key = documents.first().map_or(0, |d| d.key);
        let gray = ImageSource::merged(key, SourceChannel::Gray);
        Self {
            documents,
            spec: CalculationsSpec {
                source1: gray,
                source2: gray,
                blend: BlendMode::Multiply,
                opacity: 1.0,
                mask: None,
                result: CalculationResult::NewChannel,
            },
            mask: gray,
            use_mask: false,
        }
    }

    pub fn spec(&self) -> CalculationsSpec {
        self.spec
    }

    /// Set the spec directly — tests and presets.
    pub fn set_spec(&mut self, spec: CalculationsSpec) {
        if let Some(mask) = spec.mask {
            self.mask = mask;
        }
        self.use_mask = spec.mask.is_some();
        self.spec = spec;
    }

    pub fn title(&self) -> &'static str {
        tr("ui.calculations.title")
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        self.documents
            .is_empty()
            .then(|| tr("ui.apply_image.no.source").to_string())
    }

    /// The spec a confirmation hands over, or `None` when there is no
    /// source.
    pub fn confirm(&self) -> Option<CalculationsSpec> {
        self.blocked_reason().is_none().then_some(self.spec)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<CalculationsSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<CalculationsSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "calculations",
            self.title(),
            None,
            DialogWidth::Standard,
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
        caption(ui, tr("ui.calculations.subtitle"));
        design::section_header(ui, tr("ui.calculations.source1"));
        source_block(
            ui,
            "calc-1",
            &self.documents,
            &mut self.spec.source1,
            &SourceChannel::SINGLE,
        );
        design::section_header(ui, tr("ui.calculations.source2"));
        source_block(
            ui,
            "calc-2",
            &self.documents,
            &mut self.spec.source2,
            &SourceChannel::SINGLE,
        );
        design::section_header(ui, tr("ui.apply_image.blending"));
        blend_block(ui, "calc", &mut self.spec.blend, &mut self.spec.opacity);
        checkbox_row(ui, tr("ui.apply_image.use.mask"), &mut self.use_mask);
        if self.use_mask {
            design::section_header(ui, tr("ui.apply_image.mask"));
            source_block(
                ui,
                "calc-mask",
                &self.documents,
                &mut self.mask,
                &SourceChannel::SINGLE,
            );
        }
        self.spec.mask = self.use_mask.then_some(self.mask);
        design::section_header(ui, tr("ui.calculations.result"));
        combo(
            ui,
            egui::Id::new(("dialogs", "calculations-result")),
            &mut self.spec.result,
            &CalculationResult::ALL,
            |r| r.label().to_string(),
            |_| None,
        );
        action_row(ui, "OK", self.blocked_reason().as_deref(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> Vec<SourceDocument> {
        vec![SourceDocument {
            key: 3,
            name: "Doc".into(),
            layers: vec![],
        }]
    }

    #[test]
    fn it_opens_on_two_grays_multiplied_into_a_new_channel() {
        let spec = CalculationsDialog::new(docs()).confirm().unwrap();
        assert_eq!(spec.source1, ImageSource::merged(3, SourceChannel::Gray));
        assert_eq!(spec.source2, spec.source1);
        assert_eq!(spec.blend, BlendMode::Multiply);
        assert_eq!(spec.result, CalculationResult::NewChannel);
    }

    #[test]
    fn with_no_document_it_cannot_confirm() {
        let dialog = CalculationsDialog::new(Vec::new());
        assert!(dialog.confirm().is_none());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn enter_confirms_and_escape_wins() {
        let mut dialog = CalculationsDialog::new(docs());
        let mut spec = dialog.spec();
        spec.result = CalculationResult::Selection;
        spec.source1.invert = true;
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
    fn it_draws_in_both_appearances_and_keeps_the_spec() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = CalculationsDialog::new(docs());
            let mut spec = dialog.spec();
            spec.result = CalculationResult::NewDocument;
            spec.source2.channel = SourceChannel::Transparency;
            dialog.set_spec(spec);
            assert!(dialog.show(ctx).is_open());
            assert_eq!(dialog.spec(), spec);
            for r in CalculationResult::ALL {
                assert!(!r.label().is_empty());
            }
        });
    }
}
