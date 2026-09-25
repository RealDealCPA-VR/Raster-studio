//! W16-D: the Layers panel behaviours Photopea's panel has and this one
//! lacked — the effects list under a styled layer (each effect with its own
//! eye), Alt-click solo on a layer's eye, drops onto the trash, and the
//! panel options (thumbnail size down to none, thumbnails by layer or by
//! document bounds, "Add "copy" to copied layers").
//!
//! Everything here is a pure function of the document and the panel state,
//! so the rules are pinned without a window; `view::docks` draws them and the
//! application's `layers_panel_w16` module drives them through a real frame.
//!
//! # Hidden effects
//!
//! A layer style slot is present (`Some`) or absent; the model has no
//! per-effect "off" that keeps the parameters. Photopea's eye does keep them,
//! so the panel does: switching an effect's eye off removes the slot from the
//! style (one undo step) and keeps its parameters in [`LayersState`], and the
//! eye switched back on puts exactly those parameters back (one undo step).
//! That stash is session state of the panel: it is not written into the
//! document, so an effect hidden in the panel and then saved is saved absent.

use std::collections::HashSet;

use editor_core::{Command, Document, LayerPatch};
use layer_model::{LayerEffects, LayerId};

use super::{LayersModel, LayersState, ThumbScale};
use crate::dialogs::layer_style::EffectKind;
use crate::menu::EffectSlot;

/// The order the panel lists a style's effects under its layer — the Layer
/// Style dialog's list order, which is Photopea's and Photoshop's.
pub const PANEL_ORDER: [EffectSlot; 10] = [
    EffectSlot::BevelEmboss,
    EffectSlot::Stroke,
    EffectSlot::InnerShadow,
    EffectSlot::InnerGlow,
    EffectSlot::Satin,
    EffectSlot::ColorOverlay,
    EffectSlot::GradientOverlay,
    EffectSlot::PatternOverlay,
    EffectSlot::OuterGlow,
    EffectSlot::DropShadow,
];

/// The Layer Style dialog page a menu slot names.
pub const fn effect_kind(slot: EffectSlot) -> EffectKind {
    match slot {
        EffectSlot::DropShadow => EffectKind::DropShadow,
        EffectSlot::InnerShadow => EffectKind::InnerShadow,
        EffectSlot::OuterGlow => EffectKind::OuterGlow,
        EffectSlot::InnerGlow => EffectKind::InnerGlow,
        EffectSlot::BevelEmboss => EffectKind::BevelEmboss,
        EffectSlot::Satin => EffectKind::Satin,
        EffectSlot::ColorOverlay => EffectKind::ColorOverlay,
        EffectSlot::GradientOverlay => EffectKind::GradientOverlay,
        EffectSlot::PatternOverlay => EffectKind::PatternOverlay,
        EffectSlot::Stroke => EffectKind::Stroke,
    }
}

/// One row of a layer's effects list: the effect and whether its eye is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EffectRow {
    pub slot: EffectSlot,
    pub on: bool,
}

/// What the panel asks of the application that the panel cannot do itself
/// (the pixels live there). Drained once a frame by the application.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayersRequest {
    /// Photopea's row-menu "Duplicate Layer": copy the active layer at once,
    /// no dialog (the dialog is "Duplicate Into…").
    DuplicateLayer,
}

/// One row of the Layers panel's options menu, in Photopea's order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum OptionsItem {
    /// Add "copy" to copied layers (a check).
    AddCopy,
    /// − Thumbnail Size (the last step is no thumbnails).
    Smaller,
    /// + Thumbnail Size.
    Larger,
    /// Thumbnails by Layer (a radio).
    ByLayer,
    /// Thumbnails by Document (a radio).
    ByDocument,
}

impl OptionsItem {
    pub const ALL: [OptionsItem; 5] = [
        OptionsItem::AddCopy,
        OptionsItem::Smaller,
        OptionsItem::Larger,
        OptionsItem::ByLayer,
        OptionsItem::ByDocument,
    ];

    /// Whether the row shows a check mark in `state`.
    pub fn checked(self, state: &LayersState) -> bool {
        match self {
            OptionsItem::AddCopy => !state.plain_copy_names,
            OptionsItem::ByLayer => state.thumbs_by_layer,
            OptionsItem::ByDocument => !state.thumbs_by_layer,
            OptionsItem::Smaller | OptionsItem::Larger => false,
        }
    }

    /// Whether the row does anything in `state`.
    pub fn enabled(self, state: &LayersState) -> bool {
        match self {
            OptionsItem::Smaller => state.thumb_scale != ThumbScale::None,
            OptionsItem::Larger => state.thumb_scale != ThumbScale::Large,
            _ => true,
        }
    }

    /// Apply the row to `state`. Answers whether an option changed.
    pub fn apply(self, state: &mut LayersState) -> bool {
        let before = state.panel_options();
        match self {
            OptionsItem::AddCopy => state.plain_copy_names = !state.plain_copy_names,
            OptionsItem::Smaller => state.thumb_scale = state.thumb_scale.smaller(),
            OptionsItem::Larger => state.thumb_scale = state.thumb_scale.larger(),
            OptionsItem::ByLayer => state.thumbs_by_layer = true,
            OptionsItem::ByDocument => state.thumbs_by_layer = false,
        }
        let changed = state.panel_options() != before;
        if changed {
            state.note_option_edit();
        }
        changed
    }
}

/// The layers-panel options the preferences file keeps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PanelOptions {
    pub thumb_scale: ThumbScale,
    pub thumbs_by_layer: bool,
    pub add_copy: bool,
}

impl ThumbScale {
    /// Every size, smallest first ("−" walks left, "+" walks right).
    pub const STEPS: [ThumbScale; 4] = [
        ThumbScale::None,
        ThumbScale::Small,
        ThumbScale::Regular,
        ThumbScale::Large,
    ];

    /// Photopea's "− Thumbnail Size": one step smaller, down to none.
    pub fn smaller(self) -> Self {
        let i = Self::STEPS.iter().position(|s| *s == self).unwrap_or(2);
        Self::STEPS[i.saturating_sub(1)]
    }

    /// Photopea's "+ Thumbnail Size": one step larger.
    pub fn larger(self) -> Self {
        let i = Self::STEPS.iter().position(|s| *s == self).unwrap_or(2);
        Self::STEPS[(i + 1).min(Self::STEPS.len() - 1)]
    }

    /// Whether the rows draw thumbnail wells at all.
    pub fn shows_thumbnails(self) -> bool {
        self != ThumbScale::None
    }

    /// The name the preferences file stores.
    pub const fn key(self) -> &'static str {
        match self {
            ThumbScale::None => "none",
            ThumbScale::Small => "small",
            ThumbScale::Regular => "regular",
            ThumbScale::Large => "large",
        }
    }

    /// The size a stored name names; `Regular` for anything unknown.
    pub fn from_key(key: &str) -> Self {
        Self::STEPS
            .iter()
            .copied()
            .find(|s| s.key() == key)
            .unwrap_or_default()
    }
}

/// Where a "Thumbnails by Layer" crop is drawn inside the thumbnail well
/// `well`: the crop `uv` of a `texture`-sized document thumbnail, scaled to
/// fit the well whole and centred, so a tall layer stays tall.
pub fn fit_crop(uv: egui::Rect, texture: (f32, f32), well: egui::Rect) -> egui::Rect {
    let w = (uv.width() * texture.0).max(f32::EPSILON);
    let h = (uv.height() * texture.1).max(f32::EPSILON);
    let scale = (well.width() / w).min(well.height() / h);
    egui::Rect::from_center_size(well.center(), egui::vec2(w * scale, h * scale))
}

/// `effects` with every field of `slot` — the primary and, for a
/// repeatable effect, the extra instances — moved into a fresh block.
fn take_slot(effects: &mut LayerEffects, slot: EffectSlot) -> LayerEffects {
    let mut out = LayerEffects::default();
    let x = &mut effects.extras;
    let o = &mut out.extras;
    match slot {
        EffectSlot::DropShadow => {
            out.drop_shadow = effects.drop_shadow.take();
            o.drop_shadows = std::mem::take(&mut x.drop_shadows);
        }
        EffectSlot::InnerShadow => {
            out.inner_shadow = effects.inner_shadow.take();
            o.inner_shadows = std::mem::take(&mut x.inner_shadows);
        }
        EffectSlot::OuterGlow => out.outer_glow = effects.outer_glow.take(),
        EffectSlot::InnerGlow => out.inner_glow = effects.inner_glow.take(),
        EffectSlot::BevelEmboss => out.bevel_emboss = effects.bevel_emboss.take(),
        EffectSlot::Satin => out.satin = effects.satin.take(),
        EffectSlot::ColorOverlay => {
            out.color_overlay = effects.color_overlay.take();
            o.color_overlays = std::mem::take(&mut x.color_overlays);
        }
        EffectSlot::GradientOverlay => {
            out.gradient_overlay = effects.gradient_overlay.take();
            o.gradient_overlays = std::mem::take(&mut x.gradient_overlays);
        }
        EffectSlot::PatternOverlay => out.pattern_overlay = effects.pattern_overlay.take(),
        EffectSlot::Stroke => {
            out.stroke = effects.stroke.take();
            o.strokes = std::mem::take(&mut x.strokes);
        }
    }
    out
}

/// Put the fields of `slot` that [`take_slot`] moved out back into `effects`.
fn put_slot(effects: &mut LayerEffects, mut from: LayerEffects, slot: EffectSlot) {
    let x = &mut effects.extras;
    let o = &mut from.extras;
    match slot {
        EffectSlot::DropShadow => {
            effects.drop_shadow = from.drop_shadow;
            x.drop_shadows = std::mem::take(&mut o.drop_shadows);
        }
        EffectSlot::InnerShadow => {
            effects.inner_shadow = from.inner_shadow;
            x.inner_shadows = std::mem::take(&mut o.inner_shadows);
        }
        EffectSlot::OuterGlow => effects.outer_glow = from.outer_glow,
        EffectSlot::InnerGlow => effects.inner_glow = from.inner_glow,
        EffectSlot::BevelEmboss => effects.bevel_emboss = from.bevel_emboss,
        EffectSlot::Satin => effects.satin = from.satin,
        EffectSlot::ColorOverlay => {
            effects.color_overlay = from.color_overlay;
            x.color_overlays = std::mem::take(&mut o.color_overlays);
        }
        EffectSlot::GradientOverlay => {
            effects.gradient_overlay = from.gradient_overlay;
            x.gradient_overlays = std::mem::take(&mut o.gradient_overlays);
        }
        EffectSlot::PatternOverlay => effects.pattern_overlay = from.pattern_overlay,
        EffectSlot::Stroke => {
            effects.stroke = from.stroke;
            x.strokes = std::mem::take(&mut o.strokes);
        }
    }
}

fn effects_patch(id: LayerId, effects: LayerEffects) -> Command {
    Command::SetLayerProperties {
        layer_id: id,
        patch: LayerPatch {
            effects: Some(Box::new(effects)),
            ..LayerPatch::default()
        },
    }
}

impl LayersState {
    /// The rows of `id`'s effects list, in [`PANEL_ORDER`]: every effect the
    /// style carries (eye on) and every effect whose eye was switched off
    /// in the panel (eye off). Empty for an unstyled layer.
    pub fn effect_rows(&self, doc: &Document, id: LayerId) -> Vec<EffectRow> {
        let Some(layer) = doc.layers.get(id) else {
            return Vec::new();
        };
        let hidden = self.hidden_effects.get(&id);
        PANEL_ORDER
            .iter()
            .copied()
            .filter_map(|slot| {
                if slot.is_set(&layer.effects) {
                    Some(EffectRow { slot, on: true })
                } else if hidden.is_some_and(|h| h.iter().any(|(s, _)| *s == slot)) {
                    Some(EffectRow { slot, on: false })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Whether `id`'s effects list is unfolded (the default, as Photopea).
    pub fn effects_open(&self, id: LayerId) -> bool {
        !self.effects_folded.contains(&id)
    }

    /// The row's fx toggle: fold or unfold its effects list.
    pub fn toggle_effects_open(&mut self, id: LayerId) {
        if !self.effects_folded.remove(&id) {
            self.effects_folded.insert(id);
        }
    }

    /// One effect row's eye. Off: the slot leaves the style and its
    /// parameters are kept here; on: those parameters go back. One command
    /// (one undo step) either way; `None` when nothing would change.
    pub fn set_effect_visible(
        &mut self,
        doc: &Document,
        id: LayerId,
        slot: EffectSlot,
        on: bool,
    ) -> Option<Command> {
        let layer = doc.layers.get(id)?;
        let mut effects = layer.effects.clone();
        if on {
            if slot.is_set(&effects) {
                return None;
            }
            let stash = self.hidden_effects.get_mut(&id)?;
            let at = stash.iter().position(|(s, _)| *s == slot)?;
            let (_, kept) = stash.remove(at);
            if stash.is_empty() {
                self.hidden_effects.remove(&id);
            }
            put_slot(&mut effects, kept, slot);
        } else {
            if !slot.is_set(&effects) {
                return None;
            }
            let kept = take_slot(&mut effects, slot);
            let stash = self.hidden_effects.entry(id).or_default();
            stash.retain(|(s, _)| *s != slot);
            stash.push((slot, kept));
        }
        Some(effects_patch(id, effects))
    }

    /// The "Effects" row dropped on the trash: the whole style cleared
    /// (Photopea's Layer Style ▸ Clear), one undo step. Hidden effects go
    /// with it.
    pub fn clear_effects(&mut self, doc: &Document, id: LayerId) -> Option<Command> {
        let layer = doc.layers.get(id)?;
        self.hidden_effects.remove(&id);
        if layer.effects == LayerEffects::default() {
            return None;
        }
        Some(effects_patch(id, LayerEffects::default()))
    }

    /// One effect row dropped on the trash: that effect deleted (its
    /// parameters too), one undo step when the style changes.
    pub fn delete_effect(
        &mut self,
        doc: &Document,
        id: LayerId,
        slot: EffectSlot,
    ) -> Option<Command> {
        let layer = doc.layers.get(id)?;
        if let Some(stash) = self.hidden_effects.get_mut(&id) {
            stash.retain(|(s, _)| *s != slot);
            if stash.is_empty() {
                self.hidden_effects.remove(&id);
            }
        }
        let mut effects = layer.effects.clone();
        if !slot.is_set(&effects) {
            return None;
        }
        let _ = take_slot(&mut effects, slot);
        Some(effects_patch(id, effects))
    }

    /// An effects row started dragging: `slot` is `None` for the "Effects"
    /// row itself (the whole style).
    pub fn begin_effect_drag(&mut self, id: LayerId, slot: Option<EffectSlot>) {
        self.effect_drag = Some((id, slot));
    }

    /// The effects row being dragged, if any.
    pub fn effect_drag(&self) -> Option<(LayerId, Option<EffectSlot>)> {
        self.effect_drag
    }

    /// The release: the drag is over, whatever it was over.
    pub fn end_effect_drag(&mut self) -> Option<(LayerId, Option<EffectSlot>)> {
        self.effect_drag.take()
    }

    /// Alt-click on `id`'s eye (Photoshop's and Photopea's solo): every
    /// layer beside it — its siblings, and its ancestors' siblings — is
    /// hidden, and it (and the groups holding it) shown. Alt-clicking the
    /// same eye again while those layers are still hidden shows them again.
    /// One command, one undo step; `None` when nothing would change.
    pub fn solo(&mut self, doc: &Document, id: LayerId) -> Option<Command> {
        doc.layers.get(id)?;
        if let Some((layer, hidden)) = &self.solo {
            let still_hidden = !hidden.is_empty()
                && hidden
                    .iter()
                    .all(|h| doc.layers.get(*h).is_some_and(|l| !l.visible));
            if *layer == id && still_hidden {
                let commands = hidden
                    .iter()
                    .map(|h| LayersModel::set_visible(*h, true))
                    .collect();
                self.solo = None;
                return Some(Command::Transaction {
                    label: "Show Other Layers".to_string(),
                    commands,
                });
            }
        }
        let mut path = vec![id];
        let mut at = id;
        while let Some(parent) = doc.layers.parent_of(at) {
            path.push(parent);
            at = parent;
        }
        let on_path: HashSet<LayerId> = path.iter().copied().collect();
        let mut commands = Vec::new();
        let mut hidden = Vec::new();
        for node in &path {
            if doc.layers.get(*node).is_some_and(|l| !l.visible) {
                commands.push(LayersModel::set_visible(*node, true));
            }
            let siblings = match doc.layers.parent_of(*node) {
                Some(_) => doc.layers.siblings_of(*node).unwrap_or(&[]),
                None => doc.layers.root(),
            };
            for other in siblings {
                if on_path.contains(other) {
                    continue;
                }
                if doc.layers.get(*other).is_some_and(|l| l.visible) {
                    commands.push(LayersModel::set_visible(*other, false));
                    hidden.push(*other);
                }
            }
        }
        if commands.is_empty() {
            return None;
        }
        self.solo = Some((id, hidden));
        Some(Command::Transaction {
            label: "Show Only This Layer".to_string(),
            commands,
        })
    }

    /// Ask the application for something only it can do.
    pub fn request(&mut self, request: LayersRequest) {
        self.requests.push(request);
    }

    /// The requests since the last drain, oldest first.
    pub fn take_requests(&mut self) -> Vec<LayersRequest> {
        std::mem::take(&mut self.requests)
    }

    /// The options the preferences file keeps, as the panel holds them.
    pub fn panel_options(&self) -> PanelOptions {
        PanelOptions {
            thumb_scale: self.thumb_scale,
            thumbs_by_layer: self.thumbs_by_layer,
            add_copy: !self.plain_copy_names,
        }
    }

    /// Take stored options (the load on start).
    pub fn restore_panel_options(&mut self, options: PanelOptions) {
        self.thumb_scale = options.thumb_scale;
        self.thumbs_by_layer = options.thumbs_by_layer;
        self.plain_copy_names = !options.add_copy;
    }

    /// How many times the user changed a panel option this session: `0`
    /// means the stored options win (the load on start), anything else
    /// means the panel's own values are the ones to store.
    pub fn option_edits(&self) -> u32 {
        self.option_edits
    }

    /// A panel option changed by the user.
    pub fn note_option_edit(&mut self) {
        self.option_edits = self.option_edits.saturating_add(1);
    }

    /// The UV rectangle `id`'s thumbnail shows under "Thumbnails by Layer",
    /// published by the application; `None` shows the whole document.
    pub fn thumb_crop(&self, id: LayerId) -> Option<egui::Rect> {
        self.thumbs_by_layer
            .then(|| self.thumb_crops.get(&id).copied())
            .flatten()
    }

    /// The application's per-layer thumbnail crops (UV of the document
    /// thumbnail), replacing the previous set.
    pub fn set_thumb_crops(&mut self, crops: std::collections::HashMap<LayerId, egui::Rect>) {
        self.thumb_crops = crops;
    }

    /// Drop W16-D state about layers that left the document.
    pub fn prune_w16(&mut self, doc: &Document) {
        self.effects_folded.retain(|id| doc.layers.contains(*id));
        self.hidden_effects.retain(|id, _| doc.layers.contains(*id));
        if self
            .effect_drag
            .is_some_and(|(id, _)| !doc.layers.contains(id))
        {
            self.effect_drag = None;
        }
    }
}

/// Stable ids for the W16-D controls, so a headless test can find them.
pub mod ids {
    use layer_model::LayerId;

    use crate::menu::EffectSlot;

    /// A layer row's fx toggle: folds and unfolds its effects list.
    pub fn fx_toggle(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-layer-fx-toggle", layer))
    }

    /// The "Effects" row under a styled layer.
    pub fn effects_row(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-layer-effects-row", layer))
    }

    /// The "Effects" row's eye (the whole style on or off).
    pub fn effects_eye(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-layer-effects-eye", layer))
    }

    /// One effect's row under its layer.
    pub fn effect_row(layer: LayerId, slot: EffectSlot) -> egui::Id {
        egui::Id::new(("raster-layer-effect-row", layer, slot))
    }

    /// One effect's eye.
    pub fn effect_eye(layer: LayerId, slot: EffectSlot) -> egui::Id {
        egui::Id::new(("raster-layer-effect-eye", layer, slot))
    }

    /// The panel-options button (Photopea's Layers panel menu).
    pub fn options_button() -> egui::Id {
        egui::Id::new("raster-layers-options")
    }

    /// One row of the panel-options menu.
    pub fn options_item(item: super::OptionsItem) -> egui::Id {
        egui::Id::new(("raster-layers-options-item", item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::{Layer, ShadowEffect, StrokeEffect};

    fn styled() -> (Document, LayerId, LayerId) {
        let mut doc = Document::new(16, 16, "t");
        let mut layer = Layer::raster("Styled");
        layer.effects.drop_shadow = Some(ShadowEffect::default());
        layer.effects.stroke = Some(StrokeEffect {
            size_px: 7.0,
            ..StrokeEffect::default()
        });
        let id = doc.layers.push_root(layer).unwrap();
        let other = doc.layers.push_root(Layer::raster("Other")).unwrap();
        (doc, id, other)
    }

    fn apply(doc: &mut Document, command: Command) {
        let mut history = editor_core::History::new();
        history.apply(doc, command).unwrap();
    }

    #[test]
    fn the_effects_list_follows_the_dialog_order_and_keeps_a_hidden_effect() {
        let (mut doc, id, _) = styled();
        let mut state = LayersState::new();
        assert_eq!(
            state.effect_rows(&doc, id),
            vec![
                EffectRow {
                    slot: EffectSlot::Stroke,
                    on: true
                },
                EffectRow {
                    slot: EffectSlot::DropShadow,
                    on: true
                },
            ]
        );
        let off = state
            .set_effect_visible(&doc, id, EffectSlot::Stroke, false)
            .unwrap();
        apply(&mut doc, off);
        assert!(doc.layers.get(id).unwrap().effects.stroke.is_none());
        assert_eq!(
            state.effect_rows(&doc, id)[0],
            EffectRow {
                slot: EffectSlot::Stroke,
                on: false
            },
            "the row stays, eye off"
        );
        let on = state
            .set_effect_visible(&doc, id, EffectSlot::Stroke, true)
            .unwrap();
        apply(&mut doc, on);
        assert_eq!(
            doc.layers
                .get(id)
                .unwrap()
                .effects
                .stroke
                .as_ref()
                .unwrap()
                .size_px,
            7.0,
            "the eye brings back the parameters it hid"
        );
    }

    #[test]
    fn thumbnail_sizes_step_down_to_none_and_back() {
        assert_eq!(ThumbScale::Small.smaller(), ThumbScale::None);
        assert_eq!(ThumbScale::None.smaller(), ThumbScale::None);
        assert_eq!(ThumbScale::None.larger(), ThumbScale::Small);
        assert_eq!(ThumbScale::Large.larger(), ThumbScale::Large);
        for s in ThumbScale::STEPS {
            assert_eq!(ThumbScale::from_key(s.key()), s);
        }
    }
}
