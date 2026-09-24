//! The shell-owned edit target: *what* the user is editing.
//!
//! Layer selection already lives on [`editor_core::Document`]
//! (`active_layer` + `layer_selection`, validated against the tree, reused by
//! the layers panel — no competing list). What the shell adds here is the
//! **target dimension**: within the active layer, is the user editing the
//! layer's *content* or its *mask's coverage*? The Properties panel's
//! Layer/Mask control and (from card 055) the mask thumbnail both speak this
//! language; today nothing shell-owned does, which is exactly gap E08.
//!
//! # The reconciliation rule
//!
//! The stored kind is a **preference**; validity is computed at read:
//!
//! * the target layer is the active document's active layer, resolved through
//!   [`editor_core::Document::active_layer`], which already filters an id
//!   whose layer left the tree (undo, delete, tab switch);
//! * a `Mask` target over a layer with no attached mask answers `Content` —
//!   that covers a mask removed while selected (undo, Delete Mask), a tab
//!   switch to a document whose active layer has no mask, and a fresh
//!   document.
//!
//! So undo, redo, deleting layers and switching tabs cannot leave a stale
//! target behind: there is nothing to go stale, because the snapshot names
//! only what the document can prove. Selecting never mutates pixels — it is
//! field reads and this sticky kind, nothing else.
//!
//! One kind per document (`DocumentId` key): switching documents restores
//! *that* document's target, which is what the card's check asks for.

use editor_core::Document;
use layer_model::LayerId;

use crate::doc::{DocumentId, OpenDocument};
#[cfg(test)]
use crate::editor::Editor;

/// Which half of the active layer the user is editing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum EditTargetKind {
    /// The layer's pixels (the default — nothing has aimed at a mask).
    #[default]
    Content,
    /// The active layer's raster mask coverage.
    Mask,
    /// W10-I: the active smart object's shared smart-filter mask
    /// ([`layer_model::SmartObjectLayer::filter_mask`]).
    FilterMask,
}

impl EditTargetKind {
    /// The kind a Properties-panel focus names. The panel's own enum is the
    /// UI's; this is the shell's, kept nameable here so `ui` stays the only
    /// crate that knows about panels.
    pub fn from_focus(mask_focused: bool) -> Self {
        if mask_focused {
            EditTargetKind::Mask
        } else {
            EditTargetKind::Content
        }
    }
}

/// A validated snapshot: the layer the tools will act on, and which half of
/// it. Built by [`Editor::edit_target`]; a tool never constructs one itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EditTarget {
    pub layer: LayerId,
    pub kind: EditTargetKind,
}

impl EditTarget {
    /// Validate against a document: the layer must be in the tree, and a
    /// `Mask` target must name a layer that still has a mask. The failure
    /// mode is a fallback, never an error — a target that outlived its mask
    /// silently becomes a content target, which is the card's check.
    pub fn resolve(document: &Document, layer: LayerId, kind: EditTargetKind) -> Option<Self> {
        let layer_ref = document.layers.get(layer)?;
        let kind = match kind {
            EditTargetKind::Mask if layer_ref.mask.is_none() => EditTargetKind::Content,
            EditTargetKind::FilterMask if filter_mask_of(layer_ref).is_none() => {
                EditTargetKind::Content
            }
            other => other,
        };
        Some(Self { layer, kind })
    }
}

/// W10-I: the filter mask a layer carries — a smart object's shared
/// smart-filter mask — or `None`.
pub fn filter_mask_of(layer: &layer_model::Layer) -> Option<&layer_model::LayerMask> {
    match &layer.kind {
        layer_model::LayerKind::SmartObject(so) => so.filter_mask.as_ref(),
        _ => None,
    }
}

/// The per-document target kinds. Missing entry = [`EditTargetKind::Content`].
#[derive(Clone, Debug, Default)]
pub struct EditTargets {
    kinds: std::collections::HashMap<DocumentId, EditTargetKind>,
}

impl EditTargets {
    /// The sticky kind stored for one document.
    pub fn kind_of(&self, id: DocumentId) -> EditTargetKind {
        self.kinds.get(&id).copied().unwrap_or_default()
    }

    /// Set the kind for one document. Storing a `Mask` target for a document
    /// whose active layer has no mask is allowed and harmless: reads fall
    /// back to content, and the choice becomes live again the moment that
    /// document regains a masked active layer (undo of the mask removal, say).
    pub fn set_kind(&mut self, id: DocumentId, kind: EditTargetKind) {
        self.kinds.insert(id, kind);
    }
}

/// Read the validated target for one open document against its sticky kind.
pub fn resolve_active(doc: &OpenDocument, kind: EditTargetKind) -> Option<EditTarget> {
    let layer = doc.document.active_layer()?;
    EditTarget::resolve(&doc.document, layer, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use editor_core::{Command, LayerPatch, Patch};
    use layer_model::{LayerMask, MaskId};

    fn editor(dir: &std::path::Path) -> Editor {
        let canvas = dir.join("canvas.png");
        std::fs::write(
            &canvas,
            raster::encode(raster::ExportFormat::Png, 64, 64, &[255u8; 64 * 64 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&canvas).unwrap();
        ed
    }

    fn attach_mask(ed: &mut Editor, layer: LayerId) -> MaskId {
        let mask = LayerMask::new(MaskId::new());
        let id = mask.id;
        ed.active_mut()
            .unwrap()
            .apply(Command::SetLayerProperties {
                layer_id: layer,
                patch: LayerPatch {
                    mask: Patch::Set(mask),
                    ..Default::default()
                },
            })
            .unwrap();
        id
    }

    #[test]
    fn a_mask_target_over_a_masked_layer_resolves_to_the_mask() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        attach_mask(&mut ed, layer);
        ed.set_edit_target_kind(EditTargetKind::Mask);
        let target = ed.edit_target().expect("a target");
        assert_eq!(target.layer, layer);
        assert_eq!(target.kind, EditTargetKind::Mask);
    }

    #[test]
    fn a_mask_removed_while_selected_returns_to_content() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        attach_mask(&mut ed, layer);
        ed.set_edit_target_kind(EditTargetKind::Mask);
        assert_eq!(ed.edit_target().unwrap().kind, EditTargetKind::Mask);

        // The mask goes away while the target points at it — delete, here via
        // the same property patch the Layer ▸ Layer Mask ▸ Delete item emits.
        ed.active_mut()
            .unwrap()
            .apply(Command::SetLayerProperties {
                layer_id: layer,
                patch: LayerPatch {
                    mask: Patch::Clear,
                    ..Default::default()
                },
            })
            .unwrap();

        let target = ed.edit_target().expect("the layer still targets");
        assert_eq!(
            target.kind,
            EditTargetKind::Content,
            "a mask target over a mask-less layer falls back to content"
        );
        assert_eq!(target.layer, layer);
    }

    #[test]
    fn the_target_is_per_document_and_survives_tab_switches() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let first = ed.active().unwrap().id();
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        attach_mask(&mut ed, layer);
        ed.set_edit_target_kind(EditTargetKind::Mask);

        // A second document whose active layer has no mask. Tabs are indices:
        // the first document is 0, the newly opened one 1.
        let second_png = dir.path().join("second.png");
        std::fs::write(
            &second_png,
            raster::encode(raster::ExportFormat::Png, 32, 32, &[9u8; 32 * 32 * 4]).unwrap(),
        )
        .unwrap();
        ed.open_path(&second_png).unwrap();
        assert_eq!(
            ed.edit_target().map(|t| t.kind),
            Some(EditTargetKind::Content)
        );

        // Back to the first tab: its Mask target comes with it.
        ed.activate(0).unwrap();
        assert_eq!(ed.edit_target().map(|t| t.kind), Some(EditTargetKind::Mask));
        ed.activate(1).unwrap();
        assert_eq!(
            ed.edit_target().map(|t| t.kind),
            Some(EditTargetKind::Content)
        );

        // And the id-keyed store answers for the first document directly.
        assert_eq!(ed.edit_target_kind(first), EditTargetKind::Mask);
    }

    #[test]
    fn selecting_never_mutates_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let layer = ed.active().and_then(|d| d.document.active_layer()).unwrap();
        attach_mask(&mut ed, layer);
        let before = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 64, 64))
            .unwrap();
        let history_before = ed.active().unwrap().history_depth();

        ed.set_edit_target_kind(EditTargetKind::Mask);
        ed.set_layer_selection(vec![layer], Some(layer));
        let _ = ed.edit_target();

        let after = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 64, 64))
            .unwrap();
        assert_eq!(before, after, "selecting a target changes no pixels");
        assert_eq!(
            ed.active().unwrap().history_depth(),
            history_before,
            "and writes no history entries"
        );
    }

    #[test]
    fn a_mask_target_falls_back_when_the_active_layer_has_no_mask() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_edit_target_kind(EditTargetKind::Mask);
        // The opened layer has no mask yet: the read answers content.
        let target = ed.edit_target().unwrap();
        assert_eq!(target.kind, EditTargetKind::Content);
    }
}
