//! W13-A: the Move tool's Alt+drag duplicates what it moves (Photopea,
//! learn/layer-manipulation).
//!
//! The tool cannot create layers: it sees tiles, not the tree. So a drag
//! begun with Alt held ([`tools::edit::MoveTool::is_copying`]) still ends the
//! way a plain drag does, naming the layer(s) it moved and by how much, and
//! the pointer route hands those moves here instead of applying them. Each
//! moved layer is duplicated in place (the same copy Layer > Duplicate Layer
//! makes, seated directly above its source) and the COPY takes the move; the
//! originals stay where they were. Duplicates and moves land as ONE
//! transaction, so a single Undo gives back the stack exactly as it was.
//!
//! The pixel-selection half of the gesture (a copy of the selected pixels
//! floats; the originals stay) is the tool's own: it needs no new layer.

use editor_core::Command;
use layer_model::LayerId;
use tools::ToolRequest;

use crate::editor::Editor;

/// The History row an Alt+drag copy lands as.
pub(crate) const LABEL: &str = "Duplicate and Move";

/// Take the layer moves out of a Move gesture's output (`commands` and
/// `requests`), perform them as one "duplicate, then move the copies" step,
/// and hand back whatever else the gesture produced for the ordinary route
/// to apply (a selection copy's transaction, a layer pick).
pub(crate) fn copy_and_move(
    editor: &mut Editor,
    commands: Vec<Command>,
    requests: Vec<ToolRequest>,
) -> (Vec<Command>, Vec<ToolRequest>) {
    let mut moves: Vec<(LayerId, [f32; 6])> = Vec::new();
    let mut other_commands = Vec::new();
    for command in commands {
        match command {
            Command::TransformLayer { layer_id, matrix } => moves.push((layer_id, matrix)),
            other => other_commands.push(other),
        }
    }
    let mut other_requests = Vec::new();
    for request in requests {
        match request {
            ToolRequest::TransformLayers { layers, delta } => {
                moves.extend(conjugated(editor, &layers, delta));
            }
            other => other_requests.push(other),
        }
    }
    if !moves.is_empty() {
        match copy_commands(editor, &moves) {
            Ok((command, copies, active)) => {
                let before = editor.active().map(|d| d.history_depth()).unwrap_or(0);
                editor.apply_command(command);
                let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
                if after > before {
                    editor.set_layer_selection(copies, active);
                }
            }
            Err(message) => editor.set_status(message),
        }
    }
    (other_commands, other_requests)
}

/// A multi-layer move's document-space `delta`, conjugated through each
/// layer's own parent chain, exactly as the plain set move applies it.
fn conjugated(editor: &Editor, layers: &[LayerId], delta: [f32; 6]) -> Vec<(LayerId, [f32; 6])> {
    let Some(doc) = editor.active() else {
        return Vec::new();
    };
    let delta = glam::Affine2::from_cols_array(&delta);
    layers
        .iter()
        .filter_map(|layer| {
            let total =
                crate::interaction_geometry::document_transform_of(&doc.document, *layer, 0)
                    .ok()?;
            let own = doc.document.layers.get(*layer)?.transform;
            let parent = total * own.inverse();
            Some((*layer, (parent.inverse() * delta * parent).to_cols_array()))
        })
        .collect()
}

/// The one transaction: a copy of every moved layer, seated above its
/// source, then the move applied to the copy. Answers the command, the
/// copies (the new layer selection) and the copy of the active layer.
fn copy_commands(
    editor: &Editor,
    moves: &[(LayerId, [f32; 6])],
) -> Result<(Command, Vec<LayerId>, Option<LayerId>), String> {
    let doc = editor.active().ok_or("No document is open")?;
    let tree = &doc.document.layers;
    for (layer, _) in moves {
        let node = tree
            .get(*layer)
            .ok_or("The moved layer is not in the tree")?;
        if node.is_group() {
            // Layer > Duplicate Layer copies one node; a group's children
            // would be left behind, so the copy is refused rather than made
            // as an empty group.
            return Err(format!(
                "Alt+drag copies layers; {} is a group, which it cannot copy yet",
                node.name
            ));
        }
    }
    // Each copy is seated at its source's index, pushing every sibling at or
    // below that index down one. Copying the BOTTOM-most first leaves the
    // indices still to be used untouched.
    let mut ordered: Vec<(LayerId, [f32; 6])> = moves.to_vec();
    ordered.sort_by_key(|(layer, _)| std::cmp::Reverse(tree.index_in_parent(*layer).unwrap_or(0)));
    let active = doc.document.active_layer();
    let mut commands = Vec::new();
    let mut copies = Vec::new();
    let mut active_copy = None;
    for (layer, matrix) in ordered {
        let (duplicate, copy, _, _) = crate::layer_ops::duplicate_commands(doc, layer, None)?;
        commands.extend(duplicate);
        commands.push(Command::TransformLayer {
            layer_id: copy,
            matrix,
        });
        if Some(layer) == active {
            active_copy = Some(copy);
        }
        copies.push(copy);
    }
    copies.reverse();
    let active_copy = active_copy.or_else(|| copies.first().copied());
    Ok((
        Command::Transaction {
            label: LABEL.to_string(),
            commands,
        },
        copies,
        active_copy,
    ))
}

#[cfg(test)]
mod tests {
    //! Driven through the real pointer route: [`super::super::ToolPointer::handle`]
    //! with the Alt modifier on the samples, exactly what `Shell::on_pointer`
    //! hands it (`modifiers_of` the held keys).
    use super::super::{tight_document_bounds, ToolPointer};
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use editor_core::{Command, Selection};
    use glam::{IVec2, Vec2};
    use raster::{PixelRect, TileCoord};
    use tools::{Modifiers, ToolId};
    use ui::canvas::{PointerInput, PointerPhase};

    const SIDE: u32 = 64;
    const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);
    const RED: [u8; 4] = [220, 20, 30, 255];

    /// Paint a red square `lo..hi` (both axes) into `id`'s first tile.
    fn ink_square(editor: &mut Editor, id: layer_model::LayerId, lo: usize, hi: usize) {
        let doc = editor.active_mut().unwrap();
        let mut bytes = vec![0u8; 256 * 256 * 4];
        for y in lo..hi {
            for x in lo..hi {
                let i = (y * 256 + x) * 4;
                bytes[i..i + 4].copy_from_slice(&RED);
            }
        }
        let hash = doc.tiles.insert_bytes(bytes);
        doc.apply(
            Command::paint_tiles(
                editor_core::PixelTarget::Layer(id),
                vec![editor_core::TileEdit::set(TileCoord::new(0, 0, 0), hash)],
            )
            .unwrap(),
        )
        .unwrap();
    }

    /// A white 64x64 image with a raster layer "Ink" on top holding a red
    /// 8..16 square, Ink active, the Move tool in hand, the view at 100%.
    fn editor_with_ink(dir: &std::path::Path) -> (Editor, layer_model::LayerId) {
        let png = dir.join("canvas.png");
        std::fs::write(
            &png,
            raster::encode(
                raster::ExportFormat::Png,
                SIDE,
                SIDE,
                &[255u8; (SIDE * SIDE * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        let layer = layer_model::Layer::raster("Ink");
        let ink = layer.id;
        {
            let doc = editor.active_mut().unwrap();
            doc.set_viewport(VIEWPORT);
            doc.camera.zoom = 1.0;
            doc.camera.center = Vec2::splat(SIDE as f32 / 2.0);
            doc.apply(Command::create_layer(layer)).unwrap();
        }
        ink_square(&mut editor, ink, 8, 16);
        editor.set_layer_selection(vec![ink], Some(ink));
        editor.set_tool(ToolId::Move);
        (editor, ink)
    }

    fn screen(doc: Vec2) -> Vec2 {
        VIEWPORT * 0.5 + (doc - Vec2::splat(SIDE as f32 / 2.0))
    }

    /// Press at `from`, drag and release at `to`, with `mods` held
    /// throughout. Answers the history steps the gesture reported.
    fn drag(editor: &mut Editor, from: Vec2, to: Vec2, mods: Modifiers) -> usize {
        let mut pointer = ToolPointer::new();
        let mut steps = 0;
        for (phase, at) in [
            (PointerPhase::Down, from),
            (PointerPhase::Move, (from + to) * 0.5),
            (PointerPhase::Move, to),
            (PointerPhase::Up, to),
        ] {
            let out = pointer.handle(
                editor,
                PointerInput::at(phase, screen(at)).with_modifiers(mods),
                false,
                &[],
            );
            assert_eq!(out.failed, None, "{phase:?} failed");
            steps += out.steps;
        }
        steps
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Modifiers::NONE
        }
    }

    fn layers(editor: &Editor) -> Vec<layer_model::LayerId> {
        editor.active().unwrap().document.layers.iter_depth_first()
    }

    fn bounds(editor: &Editor, id: layer_model::LayerId) -> PixelRect {
        let doc = editor.active().unwrap();
        tight_document_bounds(&doc.document, &doc.tiles, id).expect("ink bounds")
    }

    fn rgba_at(editor: &mut Editor, x: u32, y: u32) -> [u8; 4] {
        let c = editor
            .active_mut()
            .unwrap()
            .composite(PixelRect::new(0, 0, SIDE, SIDE))
            .unwrap();
        let i = ((y * SIDE + x) * 4) as usize;
        [c[i], c[i + 1], c[i + 2], c[i + 3]]
    }

    fn is_red(px: [u8; 4]) -> bool {
        px[0] > 200 && px[1] < 60 && px[2] < 60
    }

    fn is_white(px: [u8; 4]) -> bool {
        px[..3].iter().all(|c| *c > 240)
    }

    fn undo_label(editor: &Editor) -> Option<String> {
        editor
            .active()
            .unwrap()
            .history
            .undo_label()
            .map(str::to_string)
    }

    #[test]
    fn alt_drag_with_the_move_tool_moves_a_copy_of_the_layer_and_one_undo_takes_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let (mut editor, ink) = editor_with_ink(dir.path());
        let before = layers(&editor);
        let depth = editor.active().unwrap().history_depth();

        let steps = drag(
            &mut editor,
            Vec2::new(10.0, 10.0),
            Vec2::new(30.0, 14.0),
            alt(),
        );

        let after = layers(&editor);
        assert_eq!(after.len(), before.len() + 1, "no copy was made");
        assert_eq!(steps, 1, "the copy and its move are one step");
        assert_eq!(editor.active().unwrap().history_depth(), depth + 1);
        let copy = *after
            .iter()
            .find(|id| !before.contains(id))
            .expect("the new layer");
        // The copy sits directly above its source and took the move.
        let at = |id| after.iter().position(|l| *l == id).unwrap();
        assert_eq!(at(copy) + 1, at(ink), "the copy is not directly above Ink");
        assert_eq!(bounds(&editor, copy), PixelRect::new(28, 12, 8, 8));
        // The original did not move.
        assert_eq!(bounds(&editor, ink), PixelRect::new(8, 8, 8, 8));
        assert!(is_red(rgba_at(&mut editor, 10, 10)), "the original moved");
        assert!(is_red(rgba_at(&mut editor, 30, 14)), "no copy drawn");
        // The copy is what the panel now selects.
        let doc = &editor.active().unwrap().document;
        assert_eq!(doc.active_layer(), Some(copy));
        assert_eq!(
            doc.layers.get(copy).unwrap().name,
            ui::dialogs::DuplicateLayerDialog::suggested_name("Ink")
        );
        assert_eq!(undo_label(&editor).as_deref(), Some(super::LABEL));

        // ONE undo gives the stack back exactly.
        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(layers(&editor), before, "undo left the copy behind");
        assert_eq!(bounds(&editor, ink), PixelRect::new(8, 8, 8, 8));
        assert!(
            is_white(rgba_at(&mut editor, 30, 14)),
            "the copy's ink stayed"
        );
    }

    #[test]
    fn a_plain_move_drag_still_moves_the_layer_without_copying_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut editor, ink) = editor_with_ink(dir.path());
        let before = layers(&editor);

        let steps = drag(
            &mut editor,
            Vec2::new(10.0, 10.0),
            Vec2::new(30.0, 14.0),
            Modifiers::NONE,
        );

        assert_eq!(steps, 1);
        assert_eq!(layers(&editor), before, "a plain drag made a layer");
        assert_eq!(bounds(&editor, ink), PixelRect::new(28, 12, 8, 8));
        assert!(
            is_white(rgba_at(&mut editor, 10, 10)),
            "the layer did not move"
        );
    }

    #[test]
    fn alt_drag_on_a_multi_layer_selection_copies_every_layer_above_its_own_source() {
        let dir = tempfile::tempdir().unwrap();
        let (mut editor, ink) = editor_with_ink(dir.path());
        // A second inked layer, "Ink 2", above Ink, both selected.
        let second = layer_model::Layer::raster("Ink 2");
        let second_id = second.id;
        editor
            .active_mut()
            .unwrap()
            .apply(Command::create_layer(second))
            .unwrap();
        ink_square(&mut editor, second_id, 40, 44);
        editor.set_layer_selection(vec![ink, second_id], Some(ink));
        let before = layers(&editor);

        let steps = drag(
            &mut editor,
            Vec2::new(10.0, 10.0),
            Vec2::new(16.0, 16.0),
            alt(),
        );

        assert_eq!(steps, 1);
        let after = layers(&editor);
        assert_eq!(after.len(), before.len() + 2);
        let copies: Vec<_> = after.iter().filter(|id| !before.contains(id)).collect();
        for source in [ink, second_id] {
            let s = after.iter().position(|l| *l == source).unwrap();
            let above = after[s - 1];
            assert!(
                copies.contains(&&above),
                "no copy directly above {source:?}"
            );
            let b = bounds(&editor, source);
            assert_eq!(
                bounds(&editor, above),
                PixelRect::new(b.x + 6, b.y + 6, b.width, b.height)
            );
        }
        assert_eq!(bounds(&editor, ink), PixelRect::new(8, 8, 8, 8));
        assert_eq!(bounds(&editor, second_id), PixelRect::new(40, 40, 4, 4));
        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(layers(&editor), before);
    }

    #[test]
    fn alt_drag_with_a_selection_moves_a_copy_of_the_pixels_and_keeps_the_originals() {
        let dir = tempfile::tempdir().unwrap();
        let (mut editor, ink) = editor_with_ink(dir.path());
        let select = Selection::Rect {
            min: IVec2::new(8, 8),
            max: IVec2::new(16, 16),
        };
        editor.apply_command(Command::SetSelection {
            selection: select.clone(),
        });
        let before = layers(&editor);
        let depth = editor.active().unwrap().history_depth();

        let steps = drag(
            &mut editor,
            Vec2::new(10.0, 10.0),
            Vec2::new(30.0, 14.0),
            alt(),
        );

        assert_eq!(steps, 1, "the selection copy is one step");
        assert_eq!(layers(&editor), before, "a pixel copy made a layer");
        assert!(is_red(rgba_at(&mut editor, 10, 10)), "the originals left");
        assert!(is_red(rgba_at(&mut editor, 15, 15)), "the originals left");
        assert!(is_red(rgba_at(&mut editor, 30, 14)), "no copy at the drop");
        assert!(is_red(rgba_at(&mut editor, 35, 19)), "no copy at the drop");
        assert!(is_white(rgba_at(&mut editor, 36, 14)), "the copy spread");
        assert_eq!(bounds(&editor, ink), PixelRect::new(8, 8, 28, 12));
        // The ants travelled with the copy.
        assert_eq!(
            editor.active().unwrap().document.selection.bounds(),
            Some((IVec2::new(28, 12), IVec2::new(36, 20)))
        );
        assert_eq!(undo_label(&editor).as_deref(), Some("Duplicate Selection"));

        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(editor.active().unwrap().history_depth(), depth);
        assert!(is_white(rgba_at(&mut editor, 30, 14)), "undo kept the copy");
        assert!(is_red(rgba_at(&mut editor, 10, 10)));
        assert_eq!(editor.active().unwrap().document.selection, select);
    }

    #[test]
    fn a_plain_drag_with_a_selection_still_moves_the_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let (mut editor, _) = editor_with_ink(dir.path());
        editor.apply_command(Command::SetSelection {
            selection: Selection::Rect {
                min: IVec2::new(8, 8),
                max: IVec2::new(16, 16),
            },
        });
        drag(
            &mut editor,
            Vec2::new(10.0, 10.0),
            Vec2::new(30.0, 14.0),
            Modifiers::NONE,
        );
        assert!(
            is_white(rgba_at(&mut editor, 10, 10)),
            "a plain drag copied"
        );
        assert!(is_red(rgba_at(&mut editor, 30, 14)));
    }

    #[test]
    fn ctrl_alt_lends_the_move_tool_only_where_ctrl_already_lends_it() {
        use crate::shell::temporary_tool_for;
        // Regression guards (these held before W13-A too): with the Move
        // tool in hand neither Alt nor Ctrl+Alt lends another tool.
        assert_eq!(temporary_tool_for(ToolId::Move, false, alt()), None);
        let ctrl_alt = Modifiers {
            alt: true,
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(temporary_tool_for(ToolId::Move, false, ctrl_alt), None);
        // The tools Ctrl keeps (the Hand, the pens, type, crops, ...) keep
        // their own tool under Ctrl+Alt too: nothing is lent there.
        for kept in [ToolId::Hand, ToolId::Pen, ToolId::Type, ToolId::Crop] {
            assert_eq!(temporary_tool_for(kept, false, ctrl_alt), None, "{kept:?}");
        }
        // The load-bearing W13-A change: from a painting tool Ctrl lends
        // Move and Alt no longer blocks the lend.
        assert_eq!(
            temporary_tool_for(ToolId::Brush, false, ctrl_alt),
            Some(ToolId::Move)
        );
        // Alt alone on the Brush is still the Eyedropper.
        assert_eq!(
            temporary_tool_for(ToolId::Brush, false, alt()),
            Some(ToolId::Eyedropper)
        );
    }

    /// Ctrl+Alt+drag with the Brush in hand, through the same lend the shell
    /// applies on a modifier change (`temporary_tool_for` into
    /// `Editor::set_temporary_tool`) and then the pointer route: the lent
    /// Move tool copies the layer, the original stays, one step.
    #[test]
    fn ctrl_alt_drag_from_the_brush_copies_through_the_lent_move_tool() {
        use crate::shell::temporary_tool_for;
        let dir = tempfile::tempdir().unwrap();
        let (mut editor, ink) = editor_with_ink(dir.path());
        editor.set_tool(ToolId::Brush);
        let ctrl_alt = Modifiers {
            alt: true,
            ctrl: true,
            ..Modifiers::NONE
        };
        let lent = temporary_tool_for(editor.tool(), editor.temporary_hand(), ctrl_alt);
        editor.set_temporary_tool(lent);
        assert_eq!(editor.effective_tool(), ToolId::Move);
        let before = layers(&editor);

        let steps = drag(
            &mut editor,
            Vec2::new(10.0, 10.0),
            Vec2::new(30.0, 14.0),
            ctrl_alt,
        );

        let after = layers(&editor);
        assert_eq!(after.len(), before.len() + 1, "no copy was made");
        assert_eq!(steps, 1);
        assert_eq!(bounds(&editor, ink), PixelRect::new(8, 8, 8, 8));
        assert!(is_red(rgba_at(&mut editor, 10, 10)), "the original moved");
        assert!(is_red(rgba_at(&mut editor, 30, 14)), "no copy drawn");
    }
}
