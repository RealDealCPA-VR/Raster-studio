//! The Properties panel and the Adjustments panel.
//!
//! # Context-sensitive means *derived*, not remembered
//!
//! Properties shows whatever the user last put the focus on, but it does not
//! keep a copy of it. [`PropertiesSubject::resolve`] re-derives the subject
//! from the document plus a small [`PropertyFocus`] hint every frame, so a
//! subject cannot outlive the thing it describes: delete the masked layer and
//! the panel falls back on its own, with no stale id to guard against at every
//! read.
//!
//! The fallbacks are the interesting part and they are all tested — asking for
//! mask properties on a layer with no mask shows the *layer*, not an empty
//! panel, because an empty panel reads as a bug.

use editor_core::{Command, Document, LayerPatch, Patch};
use layer_model::{AdjustmentKind, LayerId, LayerKind, LayerMask, MaskError};

use crate::intent::Intent;
use crate::menu::{AdjustmentId, LayerClass};

/// What the user last clicked, which decides what Properties talks about.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PropertyFocus {
    /// The layer row itself.
    #[default]
    Layer,
    /// The layer's mask thumbnail.
    Mask,
}

/// What the Properties panel is showing.
#[derive(Clone, PartialEq, Debug)]
pub enum PropertiesSubject {
    /// No layer is active. The panel says so rather than showing an empty box.
    Nothing,
    /// A plain layer: name, opacity, blend, locks.
    Layer(LayerId),
    /// A layer mask: density, feather, invert, link.
    Mask(LayerId),
    /// An adjustment layer's parameters, live-editable.
    Adjustment {
        layer: LayerId,
        id: Option<AdjustmentId>,
    },
    /// A text layer: hands off to Character and Paragraph.
    Text(LayerId),
    /// A shape layer: fill, stroke, path.
    Shape(LayerId),
}

impl PropertiesSubject {
    /// Decide what to show.
    pub fn resolve(doc: &Document, active: Option<LayerId>, focus: PropertyFocus) -> Self {
        let Some(id) = active else {
            return PropertiesSubject::Nothing;
        };
        let Some(layer) = doc.layers.get(id) else {
            return PropertiesSubject::Nothing;
        };
        if focus == PropertyFocus::Mask && layer.mask.is_some() {
            return PropertiesSubject::Mask(id);
        }
        match &layer.kind {
            LayerKind::Adjustment(a) => PropertiesSubject::Adjustment {
                layer: id,
                id: adjustment_id_of(&a.kind),
            },
            LayerKind::Text(_) => PropertiesSubject::Text(id),
            LayerKind::Shape(_) => PropertiesSubject::Shape(id),
            _ => PropertiesSubject::Layer(id),
        }
    }

    /// The layer this subject describes, if any.
    pub const fn layer(&self) -> Option<LayerId> {
        match self {
            PropertiesSubject::Nothing => None,
            PropertiesSubject::Layer(id)
            | PropertiesSubject::Mask(id)
            | PropertiesSubject::Adjustment { layer: id, .. }
            | PropertiesSubject::Text(id)
            | PropertiesSubject::Shape(id) => Some(*id),
        }
    }

    /// The panel's heading.
    pub fn title(&self) -> &'static str {
        match self {
            PropertiesSubject::Nothing => "Properties",
            PropertiesSubject::Layer(_) => "Layer Properties",
            PropertiesSubject::Mask(_) => "Mask Properties",
            PropertiesSubject::Adjustment { .. } => "Adjustment",
            PropertiesSubject::Text(_) => "Text Properties",
            PropertiesSubject::Shape(_) => "Shape Properties",
        }
    }
}

/// Which [`AdjustmentId`] a stored kind corresponds to.
///
/// `None` for `Auto`, which has no panel entry of its own — it is the three
/// Auto commands in the Image menu, not a one-click adjustment layer.
pub fn adjustment_id_of(kind: &AdjustmentKind) -> Option<AdjustmentId> {
    Some(match kind {
        AdjustmentKind::Levels { .. } | AdjustmentKind::LevelsFull { .. } => AdjustmentId::Levels,
        AdjustmentKind::Curves { .. } | AdjustmentKind::CurvesFull { .. } => AdjustmentId::Curves,
        AdjustmentKind::Exposure { .. } | AdjustmentKind::ExposureFull { .. } => {
            AdjustmentId::Exposure
        }
        AdjustmentKind::HueSaturation { .. } | AdjustmentKind::HueSaturationFull { .. } => {
            AdjustmentId::HueSaturation
        }
        AdjustmentKind::ColorBalance { .. } | AdjustmentKind::ColorBalanceFull { .. } => {
            AdjustmentId::ColorBalance
        }
        AdjustmentKind::BrightnessContrast { .. } => AdjustmentId::BrightnessContrast,
        AdjustmentKind::Vibrance { .. } => AdjustmentId::Vibrance,
        AdjustmentKind::BlackAndWhite { .. } => AdjustmentId::BlackAndWhite,
        AdjustmentKind::PhotoFilter { .. } => AdjustmentId::PhotoFilter,
        AdjustmentKind::ChannelMixer { .. } => AdjustmentId::ChannelMixer,
        AdjustmentKind::Invert => AdjustmentId::Invert,
        AdjustmentKind::Posterize { .. } => AdjustmentId::Posterize,
        AdjustmentKind::Threshold { .. } => AdjustmentId::Threshold,
        AdjustmentKind::GradientMap { .. } => AdjustmentId::GradientMap,
        AdjustmentKind::SelectiveColor { .. } => AdjustmentId::SelectiveColor,
        AdjustmentKind::Auto { .. } => return None,
    })
}

/// Editing a mask's own numbers.
///
/// `LayerMask` validates through setters, so an out-of-range value returns
/// [`MaskError`] rather than being clamped silently — and the panel refuses to
/// emit rather than sending the document a value it would reject.
pub struct MaskProperties;

impl MaskProperties {
    /// The mask on a layer, if it has one.
    pub fn of(doc: &Document, layer: LayerId) -> Option<&LayerMask> {
        doc.layers.get(layer)?.mask.as_ref()
    }

    fn edit(
        doc: &Document,
        layer: LayerId,
        f: impl FnOnce(&mut LayerMask) -> Result<(), MaskError>,
    ) -> Option<Command> {
        let before = doc.layers.get(layer)?.mask.clone()?;
        let mut mask = before.clone();
        f(&mut mask).ok()?;
        (mask != before).then_some(Command::SetLayerProperties {
            layer_id: layer,
            patch: LayerPatch {
                mask: Patch::Set(mask),
                ..Default::default()
            },
        })
    }

    pub fn set_density(doc: &Document, layer: LayerId, density: f32) -> Option<Command> {
        Self::edit(doc, layer, |m| m.set_density(density))
    }

    pub fn set_feather(doc: &Document, layer: LayerId, feather_px: f32) -> Option<Command> {
        Self::edit(doc, layer, |m| m.set_feather_px(feather_px))
    }

    pub fn set_inverted(doc: &Document, layer: LayerId, inverted: bool) -> Option<Command> {
        Self::edit(doc, layer, |m| {
            m.inverted = inverted;
            Ok(())
        })
    }

    pub fn set_enabled(doc: &Document, layer: LayerId, enabled: bool) -> Option<Command> {
        Self::edit(doc, layer, |m| {
            m.enabled = enabled;
            Ok(())
        })
    }

    pub fn set_linked(doc: &Document, layer: LayerId, linked: bool) -> Option<Command> {
        Self::edit(doc, layer, |m| {
            m.linked = linked;
            Ok(())
        })
    }
}

/// Live editing of an adjustment layer's parameters.
///
/// Produces an [`Intent::EditLayerKind`], not a `Command` — see that variant's
/// documentation for why, and what is missing from `editor-core`.
pub fn edit_adjustment(doc: &Document, layer: LayerId, kind: AdjustmentKind) -> Option<Intent> {
    let current = match &doc.layers.get(layer)?.kind {
        LayerKind::Adjustment(a) => a,
        _ => return None,
    };
    (current.kind != kind).then(|| Intent::EditLayerKind {
        layer,
        kind: Box::new(LayerKind::Adjustment(layer_model::AdjustmentLayer { kind })),
    })
}

/// The Adjustments panel: one button per adjustment, each creating a layer.
///
/// The whole panel is a function of [`AdjustmentId::ALL`], so an adjustment
/// added to the vocabulary appears here with no edit.
pub struct AdjustmentsPanel;

impl AdjustmentsPanel {
    /// Every button, in panel order.
    pub fn entries() -> &'static [AdjustmentId] {
        AdjustmentId::ALL
    }

    /// The command a button emits.
    pub fn create(id: AdjustmentId) -> Command {
        id.create_command()
    }

    /// The icon key for a button, so the grid reads at a glance.
    ///
    /// A *key* into [`crate::icons::ui_icon`], never a symbol. All fifteen of
    /// these were symbols once — `"◐"`, `"⊿"`, `"∿"`, `"⋔"` and the rest — and
    /// egui's default font stack has none of them, so all fourteen visible
    /// buttons in the panel were tofu boxes.
    pub const fn icon(id: AdjustmentId) -> &'static str {
        match id {
            AdjustmentId::BrightnessContrast => "adj-brightness-contrast",
            AdjustmentId::Levels => "adj-levels",
            AdjustmentId::Curves => "adj-curves",
            AdjustmentId::Exposure => "adj-exposure",
            AdjustmentId::Vibrance => "adj-vibrance",
            AdjustmentId::HueSaturation => "adj-hue-saturation",
            AdjustmentId::ColorBalance => "adj-color-balance",
            AdjustmentId::BlackAndWhite => "adj-black-and-white",
            AdjustmentId::PhotoFilter => "adj-photo-filter",
            AdjustmentId::ChannelMixer => "adj-channel-mixer",
            AdjustmentId::Invert => "adj-invert",
            AdjustmentId::Posterize => "adj-posterize",
            AdjustmentId::Threshold => "adj-threshold",
            AdjustmentId::GradientMap => "adj-gradient-map",
            AdjustmentId::SelectiveColor => "adj-selective-color",
        }
    }
}

/// Whether a layer class has anything for the Properties panel beyond the
/// common block. Used to decide whether to draw a second section.
pub const fn has_kind_properties(class: LayerClass) -> bool {
    matches!(
        class,
        LayerClass::Adjustment | LayerClass::Text | LayerClass::Shape | LayerClass::Group
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::History;
    use layer_model::{AdjustmentLayer, Layer, MaskId, ShapeLayer, TextLayer};

    fn doc_with(kind: LayerKind) -> (Document, LayerId) {
        let mut doc = Document::new(32, 32, "Test");
        let id = doc.layers.push_root(Layer::with_kind("L", kind)).unwrap();
        doc.set_active_layer(Some(id)).unwrap();
        (doc, id)
    }

    #[test]
    fn with_no_active_layer_the_panel_says_nothing_rather_than_showing_a_box() {
        let doc = Document::new(8, 8, "Empty");
        let s = PropertiesSubject::resolve(&doc, None, PropertyFocus::Layer);
        assert_eq!(s, PropertiesSubject::Nothing);
        assert_eq!(s.layer(), None);
        assert_eq!(s.title(), "Properties");
    }

    #[test]
    fn an_active_layer_that_left_the_document_falls_back_to_nothing() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        doc.layers.remove(id).unwrap();
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Nothing
        );
    }

    #[test]
    fn the_subject_follows_the_layer_kind() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Layer(id)
        );

        let (doc, id) = doc_with(LayerKind::Text(TextLayer::default()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Text(id)
        );

        let (doc, id) = doc_with(LayerKind::Shape(ShapeLayer::default()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Shape(id)
        );

        let (doc, id) = doc_with(LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Invert,
        }));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Adjustment {
                layer: id,
                id: Some(AdjustmentId::Invert),
            }
        );
    }

    #[test]
    fn focusing_a_mask_shows_the_mask_and_only_when_there_is_one() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        // No mask yet: mask focus falls back on the layer, not an empty panel.
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Mask),
            PropertiesSubject::Layer(id)
        );
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Mask),
            PropertiesSubject::Mask(id)
        );
        // ...and layer focus still shows the layer.
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Layer),
            PropertiesSubject::Layer(id)
        );
    }

    #[test]
    fn a_mask_on_an_adjustment_layer_still_shows_the_mask_when_focused() {
        let (mut doc, id) = doc_with(LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Invert,
        }));
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        assert_eq!(
            PropertiesSubject::resolve(&doc, Some(id), PropertyFocus::Mask),
            PropertiesSubject::Mask(id)
        );
    }

    #[test]
    fn every_subject_has_a_title() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        for s in [
            PropertiesSubject::Nothing,
            PropertiesSubject::Layer(id),
            PropertiesSubject::Mask(id),
            PropertiesSubject::Adjustment {
                layer: id,
                id: Some(AdjustmentId::Curves),
            },
            PropertiesSubject::Text(id),
            PropertiesSubject::Shape(id),
        ] {
            assert!(!s.title().is_empty(), "{s:?}");
        }
        drop(doc);
    }

    // ---- mask properties --------------------------------------------------

    #[test]
    fn mask_density_and_feather_emit_patches_that_apply() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        let mut history = History::new();

        let command = MaskProperties::set_density(&doc, id, 0.4).expect("in range");
        history.apply(&mut doc, command).expect("apply");
        assert_eq!(MaskProperties::of(&doc, id).unwrap().density(), 0.4);

        let command = MaskProperties::set_feather(&doc, id, 6.0).expect("in range");
        history.apply(&mut doc, command).expect("apply");
        assert_eq!(MaskProperties::of(&doc, id).unwrap().feather_px(), 6.0);

        let command = MaskProperties::set_inverted(&doc, id, true).expect("changed");
        history.apply(&mut doc, command).expect("apply");
        assert!(MaskProperties::of(&doc, id).unwrap().inverted);
    }

    #[test]
    fn an_out_of_range_mask_value_emits_nothing_rather_than_a_doomed_command() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        assert!(MaskProperties::set_density(&doc, id, 5.0).is_none());
        assert!(MaskProperties::set_density(&doc, id, f32::NAN).is_none());
        assert!(MaskProperties::set_feather(&doc, id, -1.0).is_none());
        // The mask is untouched.
        let mask = MaskProperties::of(&doc, id).unwrap();
        assert_eq!(mask.density(), 1.0);
        assert_eq!(mask.feather_px(), 0.0);
    }

    #[test]
    fn writing_a_mask_value_it_already_holds_emits_nothing() {
        let (mut doc, id) = doc_with(LayerKind::Raster(Default::default()));
        doc.layers.get_mut(id).unwrap().mask = Some(LayerMask::new(MaskId::new()));
        assert!(MaskProperties::set_density(&doc, id, 1.0).is_none());
        assert!(MaskProperties::set_inverted(&doc, id, false).is_none());
        assert!(MaskProperties::set_enabled(&doc, id, true).is_none());
    }

    #[test]
    fn a_layer_with_no_mask_emits_no_mask_commands() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        assert!(MaskProperties::of(&doc, id).is_none());
        assert!(MaskProperties::set_density(&doc, id, 0.5).is_none());
        assert!(MaskProperties::set_linked(&doc, id, false).is_none());
    }

    // ---- adjustments ------------------------------------------------------

    #[test]
    fn editing_an_adjustment_emits_the_new_parameters() {
        let (doc, id) = doc_with(LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Posterize { levels: 256 },
        }));
        let intent = edit_adjustment(&doc, id, AdjustmentKind::Posterize { levels: 8 })
            .expect("a real change");
        let Intent::EditLayerKind { layer, kind } = intent else {
            panic!("unexpected intent");
        };
        assert_eq!(layer, id);
        assert_eq!(
            *kind,
            LayerKind::Adjustment(AdjustmentLayer {
                kind: AdjustmentKind::Posterize { levels: 8 }
            })
        );
    }

    #[test]
    fn editing_an_adjustment_to_what_it_already_is_emits_nothing() {
        let (doc, id) = doc_with(LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Invert,
        }));
        assert!(edit_adjustment(&doc, id, AdjustmentKind::Invert).is_none());
    }

    #[test]
    fn a_non_adjustment_layer_refuses_adjustment_edits() {
        let (doc, id) = doc_with(LayerKind::Raster(Default::default()));
        assert!(edit_adjustment(&doc, id, AdjustmentKind::Invert).is_none());
    }

    #[test]
    fn every_stored_adjustment_kind_maps_back_to_a_panel_entry() {
        for id in AdjustmentId::ALL {
            assert_eq!(
                adjustment_id_of(&id.identity_kind()),
                Some(*id),
                "{id:?} did not round trip"
            );
        }
        // The wide spellings map back to the same entry as the narrow ones.
        assert_eq!(
            adjustment_id_of(&AdjustmentKind::LevelsFull {
                composite: [0.0, 1.0, 1.0, 0.0, 1.0],
                red: [0.0, 1.0, 1.0, 0.0, 1.0],
                green: [0.0, 1.0, 1.0, 0.0, 1.0],
                blue: [0.0, 1.0, 1.0, 0.0, 1.0],
            }),
            Some(AdjustmentId::Levels)
        );
        // Auto has no panel entry, and says so rather than guessing.
        assert_eq!(
            adjustment_id_of(&AdjustmentKind::Auto {
                mode: layer_model::AutoAdjustment::Tone,
                clip: 0.001,
            }),
            None
        );
    }

    #[test]
    fn the_adjustments_panel_offers_every_adjustment_with_an_icon_key() {
        assert_eq!(AdjustmentsPanel::entries().len(), AdjustmentId::ALL.len());
        let mut keys: Vec<&str> = AdjustmentId::ALL
            .iter()
            .map(|id| AdjustmentsPanel::icon(*id))
            .collect();
        assert!(keys.iter().all(|k| !k.is_empty()));
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "two adjustments share an icon key");
    }

    #[test]
    fn an_adjustments_panel_button_creates_that_adjustment() {
        let command = AdjustmentsPanel::create(AdjustmentId::Threshold);
        let Command::CreateLayer { layer } = command else {
            panic!("expected a create");
        };
        assert_eq!(
            adjustment_id_of(match &layer.kind {
                LayerKind::Adjustment(a) => &a.kind,
                _ => panic!("not an adjustment layer"),
            }),
            Some(AdjustmentId::Threshold)
        );
    }

    #[test]
    fn only_the_kinds_with_extra_controls_get_a_second_section() {
        assert!(has_kind_properties(LayerClass::Adjustment));
        assert!(has_kind_properties(LayerClass::Text));
        assert!(has_kind_properties(LayerClass::Shape));
        assert!(has_kind_properties(LayerClass::Group));
        assert!(!has_kind_properties(LayerClass::Raster));
        assert!(!has_kind_properties(LayerClass::SmartObject));
    }
}
