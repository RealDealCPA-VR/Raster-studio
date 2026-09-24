//! W10-B: a saved selection opened as an editable alpha channel.
//!
//! Photopea's Channels panel lists every saved selection as an alpha
//! channel; turning one on shows it alone, in grayscale, and the brushes
//! paint into it. This build keeps a saved selection as a document-level
//! [`editor_core::Selection`], not as pixels, so opening one for editing
//! gives it pixels for as long as it is open:
//!
//! * **Open** ([`open_alpha_channel`]) creates a *hidden* scratch raster
//!   layer whose mask coverage is the saved selection, makes it the active
//!   layer, aims the edit target at its mask (white reveals, black
//!   conceals — the mask-editing colour pair), and records the pair in
//!   [`layer_model::DocumentExtras::alpha_edit`]. The Channels panel sets the
//!   canvas to show the active layer's mask alone in grayscale (the
//!   presenter's card-059 mask view), so the canvas shows the channel and
//!   every brush, fill and filter edits it. The layer is hidden, so the
//!   composite and every export are unchanged. One undo step.
//! * **Close** ([`close_alpha_channel`]) reads the scratch mask back into a
//!   selection mask, stores it in the saved selection it came from
//!   ([`editor_core::Command::SetSavedSelection`]), and deletes the scratch
//!   layer — all in one transaction, so one undo takes the store back along
//!   with the close, and the undos after it take back the painting and the
//!   open. With no channel open there is nothing to close: the menu row is
//!   greyed out ([`ui::MenuContext::alpha_editing`]) and a direct call
//!   refuses.

use editor_core::{Command, PixelTarget, TileEdit};
use layer_model::{Layer, LayerMask, MaskId};

use crate::edit_target::EditTargetKind;
use crate::editor::Editor;

/// The scratch layer's name for the channel called `name`.
pub fn scratch_layer_name(name: &str) -> String {
    format!("{name} (alpha channel)")
}

/// Open saved selection `index` as an editable alpha channel.
pub fn open_alpha_channel(editor: &mut Editor, index: usize) -> Result<String, String> {
    if editor
        .active()
        .ok_or("No document is open")?
        .document
        .extras
        .alpha_edit
        .is_some()
    {
        close_alpha_channel(editor)?;
    }
    let (w, h) = super::canvas_of(editor)?;
    let doc = editor.active_mut().ok_or("No document is open")?;
    let (name, saved) = doc
        .document
        .saved_selections
        .get(index)
        .cloned()
        .ok_or_else(|| "That alpha channel is no longer saved".to_string())?;
    let coverage =
        selection::to_mask(&saved, super::canvas_rect(w, h)).map_err(|e| e.to_string())?;
    let tiles = selection::selection_to_mask_tiles(&coverage).map_err(|e| e.to_string())?;

    let mut layer = Layer::raster(scratch_layer_name(&name));
    layer.visible = false;
    layer.set_mask(LayerMask::new(MaskId::new()));
    let layer_id = layer.id;
    let edits: Vec<TileEdit> = tiles
        .into_iter()
        .map(|t| TileEdit::set(t.coord, doc.tiles.insert_bytes(t.coverage)))
        .collect();
    let mut commands = vec![Command::create_layer(layer)];
    if !edits.is_empty() {
        commands.push(
            Command::paint_tiles(PixelTarget::Mask(layer_id), edits).map_err(|e| e.to_string())?,
        );
    }
    commands.push(editor_core::extras::edit_extras(&doc.document, |x| {
        x.alpha_edit = Some(layer_model::AlphaEdit {
            index,
            layer: layer_id,
        });
    }));
    editor.apply_command(Command::Transaction {
        label: format!("Edit Alpha Channel {name}"),
        commands,
    });
    let opened = editor
        .active()
        .and_then(|d| d.document.extras.alpha_edit)
        .is_some_and(|a| a.layer == layer_id);
    if !opened {
        return Err(format!("Could not open \"{name}\" for editing"));
    }
    editor.set_active_layer(layer_id);
    editor.set_edit_target_kind(EditTargetKind::Mask);
    Ok(format!("Editing the alpha channel \"{name}\""))
}

/// The scratch layer's mask coverage over the `w` x `h` canvas.
fn read_coverage(doc: &crate::doc::OpenDocument, layer: layer_model::LayerId) -> Vec<u8> {
    let (w, h) = (doc.document.width(), doc.document.height());
    let mut coverage = vec![0u8; (w as usize) * (h as usize)];
    let ts = raster::TILE_SIZE as usize;
    let Some(mask_id) = doc.document.layers.get(layer).and_then(|l| l.mask_id()) else {
        return coverage;
    };
    let Some(map) = doc
        .document
        .pixels
        .tiles(editor_core::PixelKey::Mask(mask_id))
    else {
        return coverage;
    };
    for (coord, hash) in map.iter() {
        if coord.level != 0 {
            continue;
        }
        let Some(bytes) = compositor::TileSource::tile(&doc.tiles, hash) else {
            continue;
        };
        let (ox, oy) = coord.pixel_origin();
        for row in 0..ts {
            let y = oy + row as i64;
            if y < 0 || y >= h as i64 {
                continue;
            }
            for col in 0..ts {
                let x = ox + col as i64;
                if x < 0 || x >= w as i64 {
                    continue;
                }
                coverage[y as usize * w as usize + x as usize] = bytes[row * ts + col];
            }
        }
    }
    coverage
}

/// Close the alpha channel being edited, keeping what was painted.
pub fn close_alpha_channel(editor: &mut Editor) -> Result<String, String> {
    let doc = editor.active_mut().ok_or("No document is open")?;
    let Some(edit) = doc.document.extras.alpha_edit else {
        return Err("No alpha channel is being edited".to_string());
    };
    let (w, h) = (doc.document.width(), doc.document.height());
    let mut name = String::from("the alpha channel");
    let mut commands = Vec::new();
    if doc.document.layers.get(edit.layer).is_some() {
        let coverage = read_coverage(doc, edit.layer);
        let selection = editor_core::SelectionMask::new(glam::IVec2::ZERO, w, h, coverage)
            .map(editor_core::Selection::Mask)
            .map_err(|e| e.to_string())?;
        if let Some(slot) = doc.document.saved_selections.get(edit.index) {
            name = slot.0.clone();
            // Through history, not a field write: undoing the close takes
            // the stored coverage back too.
            commands.push(Command::SetSavedSelection {
                index: edit.index,
                name: name.clone(),
                selection,
            });
        }
    }
    if doc.document.layers.get(edit.layer).is_some() {
        commands.push(Command::DeleteLayer {
            layer_id: edit.layer,
        });
    }
    commands.push(editor_core::extras::edit_extras(&doc.document, |x| {
        x.alpha_edit = None;
    }));
    editor.apply_command(Command::Transaction {
        label: format!("Close Alpha Channel {name}"),
        commands,
    });
    editor.set_edit_target_kind(EditTargetKind::Content);
    Ok(format!("Stored the alpha channel \"{name}\""))
}
