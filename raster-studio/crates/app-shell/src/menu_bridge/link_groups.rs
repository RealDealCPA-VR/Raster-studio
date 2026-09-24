//! W10-A: Layer ▸ Link Layers with independent link groups.
//!
//! Each click links the selected layers into a group of their own
//! ([`layer_model::Layer::link_group`], a fresh id from
//! [`layer_model::LayerTree::next_link_group`]), so linking A+B and then C+D
//! leaves two groups, and a move of A takes B along but not C or D (the
//! pointer route reads the groups through
//! [`layer_model::LayerTree::link_partners`]). When every selected layer is
//! already in one and the same group, the click unlinks them instead, as
//! Photopea's toggle does. The `linked` flag is kept in step (it is what the
//! Layers panel's link badge and button read), and an old document's single
//! chain reads as one group ([`layer_model::Layer::link_key`]).
//!
//! One click is one [`Command::Transaction`], so one undo step.
//!
//! The Layers panel's own link button (`ui::view::docks`, the footer's
//! first button) speaks only the `linked` flag: one
//! `SetLayerProperties { linked }` per selected layer, all posted in the
//! same frame. [`panel_link`] is the bridge's arm for exactly that patch. It
//! gives a link the group id a Link Layers click would
//! ([`layer_model::LayerTree::next_link_group`]) — the chrome picks every
//! intent of one frame against the same document, so the layers of one
//! click share the id and the next click gets the next one — and an unlink
//! clears the group with the flag. So the panel makes an independent group
//! per click too, instead of joining every layer it links into the legacy
//! chain.

use editor_core::{Command, LayerPatch, Patch};

use crate::editor::Editor;

/// Layer ▸ Link Layers: see the module docs.
pub(crate) fn link_layers(editor: &mut Editor) -> Result<String, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let layers: Vec<layer_model::LayerId> = doc
        .document
        .layer_selection()
        .into_iter()
        .filter(|id| doc.document.layers.get(*id).is_some())
        .collect();
    if layers.len() < 2 {
        return Err("Select two or more layers to link".to_string());
    }
    let keys: Vec<Option<u64>> = layers
        .iter()
        .map(|id| doc.document.layers.get(*id).and_then(|l| l.link_key()))
        .collect();
    // Unlink when the selection is exactly one group's members, all of them.
    let one_group = keys[0].is_some() && keys.iter().all(|k| *k == keys[0]);
    let (commands, label, message): (Vec<Command>, &str, String) = if one_group {
        let commands = layers
            .iter()
            .map(|id| Command::SetLayerProperties {
                layer_id: *id,
                patch: LayerPatch {
                    linked: Some(false),
                    link_group: Patch::Clear,
                    ..LayerPatch::default()
                },
            })
            .collect();
        (
            commands,
            "Unlink Layers",
            format!("Unlinked {} layers", layers.len()),
        )
    } else {
        let group = doc.document.layers.next_link_group();
        let commands = layers
            .iter()
            .map(|id| Command::SetLayerProperties {
                layer_id: *id,
                patch: LayerPatch {
                    linked: Some(true),
                    link_group: Patch::Set(group),
                    ..LayerPatch::default()
                },
            })
            .collect();
        (
            commands,
            "Link Layers",
            format!("Linked {} layers into their own group", layers.len()),
        )
    };
    let before = editor.active().map(|d| d.history_depth());
    editor.apply_command(Command::Transaction {
        label: label.to_string(),
        commands,
    });
    if editor.active().map(|d| d.history_depth()) == before {
        // A refused transaction has already said why in the status bar.
        return Err(format!("{label} was refused"));
    }
    Ok(message)
}

/// The bridge's arm for the Layers panel's link button: see the module docs.
///
/// Answers the command to perform in place of `command` when `command` is
/// exactly the panel's patch (the `linked` flag and nothing else), and
/// `None` for everything else, which then routes unchanged.
pub(crate) fn panel_link(command: &Command, editor: &Editor) -> Option<Command> {
    let Command::SetLayerProperties { layer_id, patch } = command else {
        return None;
    };
    let linked = patch.linked?;
    let bare = LayerPatch {
        linked: Some(linked),
        ..LayerPatch::default()
    };
    if *patch != bare {
        return None;
    }
    let doc = editor.active()?;
    let link_group = if linked {
        let layer = doc.document.layers.get(*layer_id)?;
        if layer.linked {
            // Already linked: the click changes nothing about its group.
            return None;
        }
        Patch::Set(doc.document.layers.next_link_group())
    } else {
        Patch::Clear
    };
    Some(Command::SetLayerProperties {
        layer_id: *layer_id,
        patch: LayerPatch {
            linked: Some(linked),
            link_group,
            ..LayerPatch::default()
        },
    })
}
