//! W11-E: the last Layer / Edit gaps from the Photopea parity audit.
//!
//! * Edit ▸ Transform ▸ Again / Again with Copy ([`transform_again`]): the
//!   last committed whole-layer free transform's document-space affine
//!   ([`Editor::last_transform`]) applied again to the active layer, or to a
//!   duplicate of it.
//! * Layer ▸ Arrange ▸ Reverse ([`reverse_layers`]).
//! * Layer ▸ Select Linked Layers ([`select_linked_layers`]).
//! * Layer ▸ Smart Object ▸ Convert to Linked… / Embed Linked
//!   ([`convert_to_linked`], [`embed_linked`]).
//! * The Layers panel's colour labels ([`set_color_label`]).
//!
//! Every document edit here is one [`Command::Transaction`], so one Ctrl+Z
//! takes it back. Select Linked Layers changes the layer selection, which,
//! like Select ▸ All Layers, is not a history step in this build.

use editor_core::Command;
use layer_model::{AssetOrigin, ColorLabel, LayerId, LayerKind};

use crate::editor::Editor;

/// Apply `command` and answer whether it landed as a history step (a
/// refused transaction has already said why on the status line).
fn applied(editor: &mut Editor, command: Command) -> bool {
    let before = editor.active().map(|d| d.history_depth());
    editor.apply_command(command);
    editor.active().map(|d| d.history_depth()) != before
}

/// Edit ▸ Transform ▸ Again (`copy == false`) / Again with Copy (`true`).
///
/// The recorded affine is in document space (it maps where the layer was
/// drawn to where the transform left it), so it is conjugated through the
/// layer's parent chain onto its own transform, exactly as the Free
/// Transform commit does — a rotation about a point turns about that same
/// point again. With Copy, the duplicate (Duplicate Layer's own commands)
/// and its transform are one transaction.
pub(crate) fn transform_again(editor: &mut Editor, copy: bool) -> Result<String, String> {
    let delta = editor
        .last_transform()
        .ok_or(ui::menu::NO_TRANSFORM_TO_REPEAT)?;
    let (command, new_id, label) = {
        let doc = editor.active().ok_or("No document is open")?;
        let source = doc.document.active_layer().ok_or("Select a layer first")?;
        let layer = doc
            .document
            .layers
            .get(source)
            .ok_or("The active layer is not in the tree")?;
        if layer.locked.blocks_transform() {
            return Err("The layer's position is locked".to_string());
        }
        let total = crate::interaction_geometry::document_transform_of(&doc.document, source, 0)
            .map_err(|e| e.to_string())?;
        let parent = total * layer.transform.inverse();
        let matrix = (parent.inverse() * delta * parent).to_cols_array();
        if copy {
            let (mut commands, new_id, _, _) =
                crate::layer_ops::duplicate_commands(doc, source, None)?;
            commands.push(Command::TransformLayer {
                layer_id: new_id,
                matrix,
            });
            (
                Command::Transaction {
                    label: "Transform Again with Copy".to_string(),
                    commands,
                },
                Some(new_id),
                "Transform Again with Copy",
            )
        } else {
            (
                Command::Transaction {
                    label: "Transform Again".to_string(),
                    commands: vec![Command::TransformLayer {
                        layer_id: source,
                        matrix,
                    }],
                },
                None,
                "Transform Again",
            )
        }
    };
    if !applied(editor, command) {
        return Err(format!("{label} was refused"));
    }
    if let Some(id) = new_id {
        editor.set_layer_selection(vec![id], Some(id));
    }
    Ok(format!("{label}: repeated the last free transform"))
}

/// The selected layers, the active one included.
fn chosen(doc: &editor_core::Document) -> Vec<LayerId> {
    let mut set = doc.layer_selection();
    if let Some(active) = doc.active_layer() {
        if !set.contains(&active) {
            set.push(active);
        }
    }
    set
}

/// Layer ▸ Arrange ▸ Reverse: the selected layers swap places end for end —
/// the topmost takes the bottommost's slot and so on — while every other
/// layer keeps its own. The selection must share one parent (Photoshop's
/// rule). One undo step.
pub(crate) fn reverse_layers(editor: &mut Editor) -> Result<String, String> {
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        let set = chosen(&doc.document);
        if set.len() < 2 {
            return Err("Select two or more layers to reverse".to_string());
        }
        let layers = &doc.document.layers;
        let parent = layers.parent_of(set[0]);
        if set.iter().any(|id| layers.parent_of(*id) != parent) {
            return Err("Reverse works on layers in the same group".to_string());
        }
        let siblings: Vec<LayerId> = layers
            .siblings_of(set[0])
            .ok_or("The layer is not in the tree")?
            .to_vec();
        let slots: Vec<usize> = siblings
            .iter()
            .enumerate()
            .filter(|(_, id)| set.contains(id))
            .map(|(i, _)| i)
            .collect();
        let mut want = siblings.clone();
        for (k, slot) in slots.iter().enumerate() {
            want[*slot] = siblings[slots[slots.len() - 1 - k]];
        }
        // Seat each layer of the wanted order in turn; the ones before it are
        // already in place, so a move never disturbs them.
        let mut now = siblings;
        let mut commands = Vec::new();
        for (i, id) in want.iter().enumerate() {
            if now[i] != *id {
                let from = now.iter().position(|x| x == id).expect("a sibling");
                now.remove(from);
                now.insert(i, *id);
                commands.push(Command::MoveLayer {
                    layer_id: *id,
                    parent,
                    index: i,
                });
            }
        }
        if commands.is_empty() {
            return Err("The order is already its own reverse".to_string());
        }
        Command::Transaction {
            label: "Reverse Layers".to_string(),
            commands,
        }
    };
    let count = match &command {
        Command::Transaction { commands, .. } => commands.len(),
        _ => 1,
    };
    if !applied(editor, command) {
        return Err("Reverse was refused".to_string());
    }
    Ok(format!(
        "Reversed the selected layers' order ({count} moved)"
    ))
}

/// Layer ▸ Select Linked Layers: add every layer linked with the selected
/// ones to the selection (the link groups of
/// [`layer_model::LayerTree::link_partners`]). The active layer stays active.
pub(crate) fn select_linked_layers(editor: &mut Editor) -> Result<String, String> {
    let (all, active) = {
        let doc = editor.active().ok_or("No document is open")?;
        let set = chosen(&doc.document);
        let partners = doc.document.layers.link_partners(&set);
        if partners.iter().all(|id| set.contains(id)) {
            return Err("No other layer is linked to the selection".to_string());
        }
        let all: Vec<LayerId> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| set.contains(id) || partners.contains(id))
            .collect();
        (all, doc.document.active_layer())
    };
    let count = all.len();
    editor.set_layer_selection(all, active);
    Ok(format!("Selected {count} linked layers"))
}

/// A smart object's asset id, its origin and its recorded source size.
type SmartSource = (layer_model::AssetId, AssetOrigin, Option<(u32, u32)>);

/// The active smart object's asset id, origin and source size.
fn active_smart_object(editor: &Editor) -> Result<SmartSource, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let id = doc.document.active_layer().ok_or("Select a layer first")?;
    let asset = match doc.document.layers.get(id).map(|l| &l.kind) {
        Some(LayerKind::SmartObject(so)) => so.asset,
        _ => return Err("The active layer is not a smart object".to_string()),
    };
    let origin = doc
        .document
        .asset_origin(asset)
        .cloned()
        .ok_or("The smart object's asset is missing from the document")?;
    Ok((asset, origin, doc.document.asset_source_size(asset)))
}

/// The asset row's new origin plus every smart object sharing `asset`
/// flipped to `linked` — one transaction, one undo step. The pixels do not
/// change: the source bytes are the same either way.
fn relink_command(
    editor: &Editor,
    asset: layer_model::AssetId,
    origin: AssetOrigin,
    source_size: Option<(u32, u32)>,
    linked: bool,
    label: &str,
) -> Result<Command, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let mut commands = vec![Command::ReplaceAssetSource {
        asset,
        origin,
        source_size,
    }];
    for id in doc.document.layers.iter_depth_first() {
        if let Some(LayerKind::SmartObject(so)) = doc.document.layers.get(id).map(|l| &l.kind) {
            if so.asset == asset && so.linked != linked {
                let mut so = so.clone();
                so.linked = linked;
                commands.push(Command::SetLayerKind {
                    layer_id: id,
                    kind: Box::new(LayerKind::SmartObject(so)),
                });
            }
        }
    }
    Ok(Command::Transaction {
        label: label.to_string(),
        commands,
    })
}

/// Layer ▸ Smart Object ▸ Convert to Linked…: write the embedded source,
/// byte for byte, to a file the user picks, and point the object (and every
/// object sharing its source) at that file. One undo step; the file stays
/// on disk after an undo.
pub(crate) fn convert_to_linked(editor: &mut Editor) -> Result<String, String> {
    let (asset, origin, size) = active_smart_object(editor)?;
    let AssetOrigin::Embedded { name, bytes } = origin else {
        return Err("The smart object is already linked".to_string());
    };
    let stem = std::path::Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Smart Object".to_string());
    let folder = editor
        .active()
        .and_then(|d| d.project_path().or_else(|| d.source_path()))
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| editor.paths().root().to_path_buf());
    let suggested = folder.join(format!(
        "{stem}.{}",
        crate::editor::source_extension(&bytes)
    ));
    let target = editor
        .pick_save_path(&suggested)
        .ok_or("Convert to Linked was cancelled")?;
    std::fs::write(&target, &bytes).map_err(|e| {
        format!(
            "The smart object's source cannot be written to {}: {e}",
            target.display()
        )
    })?;
    let command = relink_command(
        editor,
        asset,
        AssetOrigin::Linked {
            path: target.clone(),
        },
        size,
        true,
        "Convert to Linked",
    )?;
    if !applied(editor, command) {
        return Err("Convert to Linked was refused".to_string());
    }
    Ok(format!(
        "The smart object is now linked to {}",
        target.display()
    ))
}

/// Layer ▸ Smart Object ▸ Embed Linked: read the linked file into the
/// document and embed it, so the object no longer depends on the file. One
/// undo step.
pub(crate) fn embed_linked(editor: &mut Editor) -> Result<String, String> {
    let (asset, origin, size) = active_smart_object(editor)?;
    let AssetOrigin::Linked { path } = origin else {
        return Err("The smart object is already embedded".to_string());
    };
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("The linked file {} cannot be read: {e}", path.display()))?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Smart Object".to_string());
    let command = relink_command(
        editor,
        asset,
        AssetOrigin::Embedded { name, bytes },
        size,
        false,
        "Embed Linked",
    )?;
    if !applied(editor, command) {
        return Err("Embed Linked was refused".to_string());
    }
    Ok(format!(
        "Embedded {} into the document",
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    ))
}

/// The Layers panel's colour label: give every selected layer `color`
/// (`NoColor` clears it). One `SetDocumentExtras` step.
pub(crate) fn set_color_label(editor: &mut Editor, color: ColorLabel) -> Result<String, String> {
    let (command, count) = {
        let doc = editor.active().ok_or("No document is open")?;
        let set = chosen(&doc.document);
        if set.is_empty() {
            return Err("Select a layer first".to_string());
        }
        let mut extras = doc.document.extras.clone();
        for id in &set {
            extras.set_color_label(*id, color);
        }
        if extras == doc.document.extras {
            return Err(if color == ColorLabel::NoColor {
                "The layer has no color label".to_string()
            } else {
                "The layer already wears this color label".to_string()
            });
        }
        (
            Command::Transaction {
                label: format!("Layer Color {}", color.label()),
                commands: vec![Command::SetDocumentExtras {
                    extras: Box::new(extras),
                }],
            },
            set.len(),
        )
    };
    if !applied(editor, command) {
        return Err("The color label was refused".to_string());
    }
    Ok(format!(
        "{} layer{} labelled {}",
        count,
        if count == 1 { "" } else { "s" },
        color.label()
    ))
}

#[cfg(test)]
#[path = "layer_ops_w11e_tests.rs"]
mod tests;

/// Merge Layers / Merge Down staging: make the groups that CONTAIN the merge
/// set (and are not in it) draw their children as if they were not there.
///
/// The merged layer lands back inside those groups, which keep their own
/// opacity, blend, mask, effects, clipping and transform; baking them into
/// the merged pixels too would apply them twice. And hiding them — the
/// staging hides everything outside the merge set — would drop their
/// children from the composite altogether.
pub(crate) fn neutralize_containing_groups(
    staged: &mut layer_model::LayerTree,
    doomed: &[LayerId],
) {
    let mut ancestors = Vec::new();
    for &id in doomed {
        let mut cur = staged.parent_of(id);
        while let Some(p) = cur {
            if !doomed.contains(&p) && !ancestors.contains(&p) {
                ancestors.push(p);
            }
            cur = staged.parent_of(p);
        }
    }
    for id in ancestors {
        if let Some(layer) = staged.get_mut(id) {
            layer.visible = true;
            layer.opacity = 1.0;
            layer.fill_opacity = 1.0;
            layer.blend_mode = layer_model::BlendMode::Normal;
            layer.mask = None;
            layer.clipping = layer_model::ClippingMode::None;
            layer.effects = layer_model::LayerEffects::default();
            layer.transform = glam::Affine2::IDENTITY;
            if let LayerKind::Group(g) = &mut layer.kind {
                g.blending = layer_model::GroupBlending::PassThrough;
            }
        }
    }
}
