//! W10-H: Image ▸ Apply Image… — Photoshop's channel arithmetic into the
//! active layer: a source (an open document of the same size, one of its
//! layers or its merged image, one channel or the composite, optionally
//! inverted), a blending mode, an opacity, Preserve Transparency, and an
//! optional mask (another source channel).
//!
//! The dialog hands back an [`ApplyImageSpec`] and nothing else; the pixels
//! are the application's (the shell reads the source, blends it into the
//! active layer and writes the result as one undoable step). The source
//! block ([`source_block`]) is shared with Calculations.
//!
//! Documents are named by an opaque `key` the shell hands in with the list
//! ([`SourceDocument::key`]) and reads back from the spec, so the dialog
//! never needs the shell's document type.

use egui::Context;
use layer_model::{BlendMode, LayerId};

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, combo, numeric};
use crate::strings::tr;

/// Which channel of a source is read.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum SourceChannel {
    /// The composite colour (Apply Image only; Calculations reads one
    /// channel).
    #[default]
    Rgb,
    Red,
    Green,
    Blue,
    /// Rec.601 luma of the colour.
    Gray,
    /// The alpha channel.
    Transparency,
}

impl SourceChannel {
    /// Every channel, Apply Image's list.
    pub const ALL: [SourceChannel; 6] = [
        SourceChannel::Rgb,
        SourceChannel::Red,
        SourceChannel::Green,
        SourceChannel::Blue,
        SourceChannel::Gray,
        SourceChannel::Transparency,
    ];

    /// The single channels, Calculations' list.
    pub const SINGLE: [SourceChannel; 5] = [
        SourceChannel::Red,
        SourceChannel::Green,
        SourceChannel::Blue,
        SourceChannel::Gray,
        SourceChannel::Transparency,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SourceChannel::Rgb => tr("ui.apply_image.channel.rgb"),
            SourceChannel::Red => tr("ui.apply_image.channel.red"),
            SourceChannel::Green => tr("ui.apply_image.channel.green"),
            SourceChannel::Blue => tr("ui.apply_image.channel.blue"),
            SourceChannel::Gray => tr("ui.apply_image.channel.gray"),
            SourceChannel::Transparency => tr("ui.apply_image.channel.transparency"),
        }
    }
}

/// One open document the dialog offers as a source.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceDocument {
    /// The shell's handle for the document, handed back in the spec.
    pub key: u64,
    pub name: String,
    /// Its layers, top first, as (id, name).
    pub layers: Vec<(LayerId, String)>,
}

/// A source: which document, which layer (`None` is the merged image),
/// which channel, and whether it is inverted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImageSource {
    pub document: u64,
    pub layer: Option<LayerId>,
    pub channel: SourceChannel,
    pub invert: bool,
}

impl ImageSource {
    /// The merged image of `document`, `channel`, not inverted.
    pub fn merged(document: u64, channel: SourceChannel) -> Self {
        Self {
            document,
            layer: None,
            channel,
            invert: false,
        }
    }
}

/// The confirmed Apply Image.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ApplyImageSpec {
    pub source: ImageSource,
    pub blend: BlendMode,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub preserve_transparency: bool,
    /// A source channel whose value weights the blend per pixel.
    pub mask: Option<ImageSource>,
}

/// The blending modes Apply Image and Calculations offer: every layer mode
/// but Dissolve, which is a random pattern rather than arithmetic.
pub fn blend_choices() -> Vec<BlendMode> {
    BlendMode::ALL
        .iter()
        .copied()
        .filter(|m| *m != BlendMode::Dissolve)
        .collect()
}

/// A source's document, layer, channel and invert controls. `salt` keeps
/// two blocks in one dialog apart; `channels` is the channel list offered.
pub fn source_block(
    ui: &mut egui::Ui,
    salt: &str,
    documents: &[SourceDocument],
    source: &mut ImageSource,
    channels: &[SourceChannel],
) {
    if documents.is_empty() {
        return;
    }
    let mut doc_index = documents
        .iter()
        .position(|d| d.key == source.document)
        .unwrap_or(0);
    ui.label(tr("ui.apply_image.document"));
    let indices: Vec<usize> = (0..documents.len()).collect();
    if combo(
        ui,
        egui::Id::new(("dialogs", "apply-image-doc", salt)),
        &mut doc_index,
        &indices,
        |i| documents[i].name.clone(),
        |_| None,
    ) {
        source.layer = None;
    }
    let doc = &documents[doc_index];
    source.document = doc.key;
    if source
        .layer
        .is_some_and(|l| !doc.layers.iter().any(|(id, _)| *id == l))
    {
        source.layer = None;
    }
    ui.label(tr("ui.apply_image.layer"));
    let mut layers: Vec<Option<LayerId>> = vec![None];
    layers.extend(doc.layers.iter().map(|(id, _)| Some(*id)));
    combo(
        ui,
        egui::Id::new(("dialogs", "apply-image-layer", salt)),
        &mut source.layer,
        &layers,
        |l| match l {
            None => tr("ui.apply_image.merged").to_string(),
            Some(id) => doc
                .layers
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
        },
        |_| None,
    );
    if !channels.contains(&source.channel) {
        source.channel = channels[0];
    }
    ui.label(tr("ui.apply_image.channel"));
    combo(
        ui,
        egui::Id::new(("dialogs", "apply-image-channel", salt)),
        &mut source.channel,
        channels,
        |c| c.label().to_string(),
        |_| None,
    );
    checkbox_row(ui, tr("ui.apply_image.invert"), &mut source.invert);
}

/// The blending combo and the opacity field.
pub fn blend_block(ui: &mut egui::Ui, salt: &str, blend: &mut BlendMode, opacity: &mut f32) {
    ui.label(tr("ui.apply_image.blending"));
    combo(
        ui,
        egui::Id::new(("dialogs", "apply-image-blend", salt)),
        blend,
        &blend_choices(),
        |m| m.label().to_string(),
        |_| None,
    );
    ui.label(tr("ui.apply_image.opacity"));
    let mut percent = f64::from(*opacity * 100.0);
    numeric(ui, &mut percent, 0.0..=100.0, 0, "%");
    *opacity = (percent as f32 / 100.0).clamp(0.0, 1.0);
}

/// Image ▸ Apply Image….
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyImageDialog {
    documents: Vec<SourceDocument>,
    spec: ApplyImageSpec,
    /// The mask source the Mask box turns on, kept while it is off.
    mask: ImageSource,
    use_mask: bool,
}

impl ApplyImageDialog {
    /// Open over `documents` (the same-size open documents, the target
    /// first), sourcing the target's merged image in Multiply at 100%, as
    /// Photoshop opens.
    pub fn new(documents: Vec<SourceDocument>) -> Self {
        let key = documents.first().map_or(0, |d| d.key);
        let source = ImageSource::merged(key, SourceChannel::Rgb);
        Self {
            documents,
            spec: ApplyImageSpec {
                source,
                blend: BlendMode::Multiply,
                opacity: 1.0,
                preserve_transparency: false,
                mask: None,
            },
            mask: ImageSource::merged(key, SourceChannel::Gray),
            use_mask: false,
        }
    }

    pub fn spec(&self) -> ApplyImageSpec {
        self.spec
    }

    /// Set the spec directly — tests and presets.
    pub fn set_spec(&mut self, spec: ApplyImageSpec) {
        if let Some(mask) = spec.mask {
            self.mask = mask;
        }
        self.use_mask = spec.mask.is_some();
        self.spec = spec;
    }

    pub fn documents(&self) -> &[SourceDocument] {
        &self.documents
    }

    pub fn title(&self) -> &'static str {
        tr("ui.apply_image.title")
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        self.documents
            .is_empty()
            .then(|| tr("ui.apply_image.no.source").to_string())
    }

    /// The spec a confirmation hands over, or `None` when there is no
    /// source.
    pub fn confirm(&self) -> Option<ApplyImageSpec> {
        self.blocked_reason().is_none().then_some(self.spec)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<ApplyImageSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<ApplyImageSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "apply-image",
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
        caption(ui, tr("ui.apply_image.subtitle"));
        design::section_header(ui, tr("ui.apply_image.source"));
        source_block(
            ui,
            "source",
            &self.documents,
            &mut self.spec.source,
            &SourceChannel::ALL,
        );
        design::section_header(ui, tr("ui.apply_image.blending"));
        blend_block(ui, "apply", &mut self.spec.blend, &mut self.spec.opacity);
        checkbox_row(
            ui,
            tr("ui.apply_image.preserve"),
            &mut self.spec.preserve_transparency,
        );
        checkbox_row(ui, tr("ui.apply_image.use.mask"), &mut self.use_mask);
        if self.use_mask {
            design::section_header(ui, tr("ui.apply_image.mask"));
            source_block(
                ui,
                "mask",
                &self.documents,
                &mut self.mask,
                &SourceChannel::SINGLE,
            );
        }
        self.spec.mask = self.use_mask.then_some(self.mask);
        action_row(ui, "OK", self.blocked_reason().as_deref(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> Vec<SourceDocument> {
        vec![
            SourceDocument {
                key: 7,
                name: "Target".into(),
                layers: vec![(LayerId::new(), "Top".into())],
            },
            SourceDocument {
                key: 9,
                name: "Other".into(),
                layers: vec![],
            },
        ]
    }

    #[test]
    fn it_opens_on_the_targets_merged_image_in_multiply() {
        let dialog = ApplyImageDialog::new(docs());
        let spec = dialog.confirm().unwrap();
        assert_eq!(spec.source, ImageSource::merged(7, SourceChannel::Rgb));
        assert_eq!(spec.blend, BlendMode::Multiply);
        assert_eq!(spec.opacity, 1.0);
        assert_eq!(spec.mask, None);
    }

    #[test]
    fn with_no_source_document_it_cannot_confirm() {
        let dialog = ApplyImageDialog::new(Vec::new());
        assert!(dialog.blocked_reason().is_some());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn enter_confirms_the_spec_and_escape_wins() {
        let mut dialog = ApplyImageDialog::new(docs());
        let spec = ApplyImageSpec {
            source: ImageSource {
                document: 9,
                layer: None,
                channel: SourceChannel::Red,
                invert: true,
            },
            blend: BlendMode::Screen,
            opacity: 0.5,
            preserve_transparency: true,
            mask: Some(ImageSource::merged(7, SourceChannel::Gray)),
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
    fn dissolve_is_not_offered_and_every_channel_has_a_label() {
        assert!(!blend_choices().contains(&BlendMode::Dissolve));
        assert!(blend_choices().contains(&BlendMode::Multiply));
        for c in SourceChannel::ALL {
            assert!(!c.label().is_empty());
        }
        assert!(!SourceChannel::SINGLE.contains(&SourceChannel::Rgb));
    }

    #[test]
    fn it_draws_in_both_appearances_and_keeps_the_spec() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = ApplyImageDialog::new(docs());
            let spec = ApplyImageSpec {
                source: ImageSource::merged(9, SourceChannel::Blue),
                blend: BlendMode::Overlay,
                opacity: 0.25,
                preserve_transparency: false,
                mask: Some(ImageSource::merged(7, SourceChannel::Gray)),
            };
            dialog.set_spec(spec);
            assert!(dialog.show(ctx).is_open());
            assert_eq!(dialog.spec(), spec, "drawing a frame must not edit it");
        });
    }
}
