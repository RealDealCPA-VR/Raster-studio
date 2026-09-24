//! The W2-F menu commands that need the live document: Layer ▸ Align,
//! Distribute and Stamp Visible, Image ▸ Trim… with its options, Select ▸
//! Refine Edge…, and File ▸ Save as PSD….
//!
//! Every one is the other half of a [`crate::menu_bridge::Pick::Menu`]: the
//! shared menu model decided the row was live, and
//! [`crate::menu_bridge::perform`] hands the click here with `&mut Editor`.
//! Each edit lands as **one** [`editor_core::Command`] through
//! [`Editor::apply_command`] — a [`Command::Transaction`] where more than one
//! command is involved — so one Ctrl+Z puts it all back. A request that would
//! change nothing is refused with a sentence rather than recorded as an empty
//! undo step.

use editor_core::{Command, Selection, SelectionMask};
use glam::IVec2;
use layer_model::LayerId;
use ui::dialogs::refine_mask::RefineMaskSpec;
use ui::dialogs::{TrimBasis, TrimSpec};
use ui::menu::{AlignEdge, DistributeAxis};

use crate::action::Action;
use crate::doc::OpenDocument;
use crate::editor::Editor;

/// A layer's tight ink rectangle in document space, as `(x0, y0, x1, y1)`.
///
/// `None` for a layer with no ink at all (an empty raster layer, a group
/// with nothing visible), which Align and Distribute skip rather than move.
fn ink_bounds(doc: &OpenDocument, id: LayerId) -> Option<(f32, f32, f32, f32)> {
    let rect = crate::tool_input::tight_document_bounds(&doc.document, &doc.tiles, id)?;
    if rect.width == 0 || rect.height == 0 {
        return None;
    }
    Some((
        rect.x as f32,
        rect.y as f32,
        (rect.x + rect.width as i64) as f32,
        (rect.y + rect.height as i64) as f32,
    ))
}

/// The layers an Align or Distribute acts on: the document's layer
/// selection (Photopea's multi-selection), or the active layer alone.
fn subjects(doc: &OpenDocument) -> Vec<LayerId> {
    let mut set = doc.document.layer_selection();
    if let Some(active) = doc.document.active_layer() {
        if !set.contains(&active) {
            set.push(active);
        }
    }
    set
}

/// The rectangle Align measures against, Photopea's (and Photoshop's) rule:
/// the pixel selection when there is one; otherwise, with two or more
/// layers carrying ink, the union of those layers' bounds (so they align
/// to *each other* — "their upper edge to the same height"); otherwise,
/// with a single layer, the canvas.
fn align_target(doc: &OpenDocument, subjects: &[LayerId]) -> (f32, f32, f32, f32) {
    if let Some((min, max)) = doc.document.selection.bounds() {
        return (min.x as f32, min.y as f32, max.x as f32, max.y as f32);
    }
    let inked: Vec<_> = subjects
        .iter()
        .filter_map(|id| ink_bounds(doc, *id))
        .collect();
    if inked.len() >= 2 {
        return inked.into_iter().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(ax0, ay0, ax1, ay1), (x0, y0, x1, y1)| {
                (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1))
            },
        );
    }
    (
        0.0,
        0.0,
        doc.document.width() as f32,
        doc.document.height() as f32,
    )
}

/// The whole-pixel translation that brings `bounds`' named edge onto
/// `target`'s.
fn align_delta(
    edge: AlignEdge,
    bounds: (f32, f32, f32, f32),
    target: (f32, f32, f32, f32),
) -> (f32, f32) {
    let (bx0, by0, bx1, by1) = bounds;
    let (tx0, ty0, tx1, ty1) = target;
    let d = match edge {
        AlignEdge::Left => tx0 - bx0,
        AlignEdge::HorizontalCenter => (tx0 + tx1) * 0.5 - (bx0 + bx1) * 0.5,
        AlignEdge::Right => tx1 - bx1,
        AlignEdge::Top => ty0 - by0,
        AlignEdge::VerticalCenter => (ty0 + ty1) * 0.5 - (by0 + by1) * 0.5,
        AlignEdge::Bottom => ty1 - by1,
    }
    .round();
    if edge.is_horizontal() {
        (d, 0.0)
    } else {
        (0.0, d)
    }
}

/// A document-space translation of a layer, as the pre-multiplied delta
/// [`Command::TransformLayer`] takes.
fn translate(layer_id: LayerId, dx: f32, dy: f32) -> Command {
    Command::TransformLayer {
        layer_id,
        matrix: glam::Affine2::from_translation(glam::vec2(dx, dy)).to_cols_array(),
    }
}

/// Layer ▸ Align ▸ `edge`: move every selected layer so its named edge meets
/// the selection's (when there is one), the selected layers' combined
/// bounds' (two or more layers), or the canvas's (one layer). One undoable
/// step.
pub fn align(editor: &mut Editor, edge: AlignEdge) -> Result<String, String> {
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        let subjects = subjects(doc);
        let target = align_target(doc, &subjects);
        let mut commands = Vec::new();
        let mut skipped_locked = 0usize;
        for id in subjects {
            let Some(layer) = doc.document.layers.get(id) else {
                continue;
            };
            if layer.locked.blocks_transform() {
                skipped_locked += 1;
                continue;
            }
            let Some(bounds) = ink_bounds(doc, id) else {
                continue;
            };
            let (dx, dy) = align_delta(edge, bounds, target);
            if dx != 0.0 || dy != 0.0 {
                commands.push(translate(id, dx, dy));
            }
        }
        if commands.is_empty() {
            return Err(if skipped_locked > 0 {
                "The layer's position is locked".to_string()
            } else {
                format!("Already aligned: {}", edge.label().to_lowercase())
            });
        }
        Command::Transaction {
            label: format!("Align {}", edge.label()),
            commands,
        }
    };
    let moved = match &command {
        Command::Transaction { commands, .. } => commands.len(),
        _ => 1,
    };
    editor.apply_command(command);
    Ok(format!(
        "Aligned {moved} layer{} to {}",
        if moved == 1 { "" } else { "s" },
        edge.label().to_lowercase()
    ))
}

/// Layer ▸ Distribute ▸ `axis`: space three or more selected layers so their
/// centres are evenly apart between the two outermost. One undoable step.
pub fn distribute(editor: &mut Editor, axis: DistributeAxis) -> Result<String, String> {
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        let mut boxes: Vec<(LayerId, f32)> = subjects(doc)
            .into_iter()
            .filter(|id| {
                doc.document
                    .layers
                    .get(*id)
                    .is_some_and(|l| !l.locked.blocks_transform())
            })
            .filter_map(|id| {
                let (x0, y0, x1, y1) = ink_bounds(doc, id)?;
                let center = match axis {
                    DistributeAxis::Horizontal => (x0 + x1) * 0.5,
                    DistributeAxis::Vertical => (y0 + y1) * 0.5,
                };
                Some((id, center))
            })
            .collect();
        if boxes.len() < 3 {
            return Err("Select three or more layers".to_string());
        }
        boxes.sort_by(|a, b| a.1.total_cmp(&b.1));
        let first = boxes[0].1;
        let last = boxes[boxes.len() - 1].1;
        let step = (last - first) / (boxes.len() - 1) as f32;
        let mut commands = Vec::new();
        for (i, (id, center)) in boxes.iter().enumerate() {
            let want = first + step * i as f32;
            let d = (want - center).round();
            if d != 0.0 {
                commands.push(match axis {
                    DistributeAxis::Horizontal => translate(*id, d, 0.0),
                    DistributeAxis::Vertical => translate(*id, 0.0, d),
                });
            }
        }
        if commands.is_empty() {
            return Err("The layers are already evenly spaced".to_string());
        }
        Command::Transaction {
            label: format!("Distribute {}", axis.label()),
            commands,
        }
    };
    editor.apply_command(command);
    Ok(format!(
        "Distributed layers {}",
        axis.label().to_lowercase()
    ))
}

/// Layer ▸ Stamp Visible: composite every visible layer into a new raster
/// layer directly above the active one (at the top when no layer is
/// active), as one undoable step, and make it the active layer.
pub fn stamp_visible(editor: &mut Editor) -> Result<String, String> {
    let (command, new_id) = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        if doc.document.layers.is_empty() {
            return Err("The document has no layers".to_string());
        }
        let (w, h) = (doc.document.width(), doc.document.height());
        let rect = doc.canvas_rect();
        let rgba = doc.composite(rect).map_err(|e| e.to_string())?;
        let placement = doc.document.active_layer().map(|active| {
            (
                doc.document.layers.parent_of(active),
                doc.document.layers.index_in_parent(active).unwrap_or(0),
            )
        });
        let layer = layer_model::Layer::raster(format!("Layer {}", doc.document.layers.len() + 1));
        let new_id = layer.id;
        let mut commands = vec![Command::create_layer(layer)];
        let grid = raster::TileGrid::from_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
        let mut edits = Vec::new();
        for (coord, tile) in grid.iter() {
            let hash = doc.tiles.insert_bytes(tile.data().to_vec());
            edits.push(editor_core::pixels::TileEdit::set(coord, hash));
        }
        commands.push(
            Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                .map_err(|e| e.to_string())?,
        );
        // `create_layer` lands at the top of the root; the move puts the
        // stamp directly above the active layer, in the active layer's own
        // group. The index is the active layer's *original* one: lifting the
        // stamp back out restores that numbering before the insert.
        if let Some((parent, index)) = placement {
            commands.push(Command::MoveLayer {
                layer_id: new_id,
                parent,
                index,
            });
        }
        (
            Command::Transaction {
                label: "Stamp Visible".to_string(),
                commands,
            },
            new_id,
        )
    };
    editor.apply_command(command);
    editor.set_layer_selection(vec![new_id], Some(new_id));
    Ok("Stamped every visible layer onto a new layer".to_string())
}

/// Layer ▸ Duplicate Layer…: copy the active layer directly above itself
/// under `name` — the dialog's, or Photoshop's "<name> copy" when nothing
/// was asked (a chord). One undoable step: the layer, its pixels, its mask
/// and the move that seats it above its source travel in one transaction.
///
/// Pixels are content-addressed, so copying them is copying hashes; the
/// bytes are shared and the duplicate costs nothing on disk.
pub fn duplicate_layer(editor: &mut Editor, name: Option<String>) -> Result<String, String> {
    let (command, new_id, status) = {
        let doc = editor.active().ok_or("No document is open")?;
        let source_id = doc.document.active_layer().ok_or("No layer is active")?;
        let (commands, new_id, status, label) = duplicate_commands(doc, source_id, name)?;
        (Command::Transaction { label, commands }, new_id, status)
    };
    editor.apply_command(command);
    editor.set_layer_selection(vec![new_id], Some(new_id));
    Ok(status)
}

/// The commands Duplicate Layer applies (W11-E: shared with Edit ▸
/// Transform ▸ Again with Copy, which appends its transform to them): the
/// copy of `source_id` named `name` (or "<name> copy"), its pixels, masks and
/// colour label, seated directly above the source. Answers the commands, the
/// copy's id, the status sentence and the transaction label.
pub(crate) fn duplicate_commands(
    doc: &OpenDocument,
    source_id: LayerId,
    name: Option<String>,
) -> Result<(Vec<Command>, LayerId, String, String), String> {
    {
        let source = doc
            .document
            .layers
            .get(source_id)
            .ok_or("The active layer is not in the tree")?;
        let mut copy = source.clone();
        copy.id = LayerId::new();
        copy.name =
            name.unwrap_or_else(|| ui::dialogs::DuplicateLayerDialog::suggested_name(&source.name));
        // A duplicated mask needs its own identity, or both layers would
        // edit one set of coverage tiles.
        let old_mask = copy.mask.as_mut().map(|m| {
            let old = m.id;
            m.id = layer_model::MaskId::new();
            old
        });
        // W10-I: and so does a smart object's filter mask.
        let filter_mask =
            crate::menu_bridge::layer_extras::rekey_filter_mask(&mut copy, &doc.document);
        let new_id = copy.id;
        let status = format!("Duplicated {} as {}", source.name, copy.name);
        let mut commands = vec![Command::create_layer(copy)];
        if let Some(edits) = filter_mask.filter(|e| !e.is_empty()) {
            commands.push(
                Command::paint_tiles(editor_core::pixels::PixelTarget::FilterMask(new_id), edits)
                    .map_err(|e| e.to_string())?,
            );
        }
        if let Some(map) = doc.document.layer_tiles(source_id) {
            let edits: Vec<_> = map
                .iter()
                .map(|(coord, hash)| editor_core::pixels::TileEdit::set(coord, hash))
                .collect();
            if !edits.is_empty() {
                commands.push(
                    Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                        .map_err(|e| e.to_string())?,
                );
            }
        }
        if let Some(old_mask) = old_mask {
            if let Some(map) = doc
                .document
                .pixels
                .tiles(editor_core::PixelKey::Mask(old_mask))
            {
                let edits: Vec<_> = map
                    .iter()
                    .map(|(coord, hash)| editor_core::pixels::TileEdit::set(coord, hash))
                    .collect();
                if !edits.is_empty() {
                    commands.push(
                        Command::paint_tiles(editor_core::pixels::PixelTarget::Mask(new_id), edits)
                            .map_err(|e| e.to_string())?,
                    );
                }
            }
        }
        // `create_layer` lands at the top of the root; the move seats the
        // copy directly above its source, in the source's own group.
        commands.push(Command::MoveLayer {
            layer_id: new_id,
            parent: doc.document.layers.parent_of(source_id),
            index: doc.document.layers.index_in_parent(source_id).unwrap_or(0),
        });
        // W11-E: the copy wears its source's colour label, as in Photoshop.
        let color = doc.document.extras.color_label(source_id);
        if color != layer_model::ColorLabel::NoColor {
            let mut extras = doc.document.extras.clone();
            extras.set_color_label(new_id, color);
            commands.push(Command::SetDocumentExtras {
                extras: Box::new(extras),
            });
        }
        Ok((
            commands,
            new_id,
            status,
            format!("Duplicate {}", source.name),
        ))
    }
}

/// The rectangle Image ▸ Trim… keeps, as `(x, y, width, height)`, judged
/// over the composite `rgba` (`w * h * 4` bytes) by `spec`.
///
/// `None` when every pixel is "empty" under the basis — there is nothing to
/// trim to. A side the spec does not name keeps the canvas edge.
pub fn trim_rect(rgba: &[u8], w: u32, h: u32, spec: TrimSpec) -> Option<(u32, u32, u32, u32)> {
    if w == 0 || h == 0 || rgba.len() < (w as usize) * (h as usize) * 4 {
        return None;
    }
    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * w + x) as usize) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    };
    let empty: Box<dyn Fn([u8; 4]) -> bool> = match spec.basis {
        TrimBasis::Transparent => Box::new(|p: [u8; 4]| p[3] == 0),
        TrimBasis::TopLeftColor => {
            let reference = px(0, 0);
            Box::new(move |p: [u8; 4]| p == reference)
        }
        TrimBasis::BottomRightColor => {
            let reference = px(w - 1, h - 1);
            Box::new(move |p: [u8; 4]| p == reference)
        }
    };
    let (mut minx, mut miny, mut maxx, mut maxy) = (w, h, 0u32, 0u32);
    let mut any = false;
    for y in 0..h {
        for x in 0..w {
            if !empty(px(x, y)) {
                any = true;
                minx = minx.min(x);
                maxx = maxx.max(x);
                miny = miny.min(y);
                maxy = maxy.max(y);
            }
        }
    }
    if !any {
        return None;
    }
    let x0 = if spec.left { minx } else { 0 };
    let x1 = if spec.right { maxx + 1 } else { w };
    let y0 = if spec.top { miny } else { 0 };
    let y1 = if spec.bottom { maxy + 1 } else { h };
    Some((x0, y0, x1 - x0, y1 - y0))
}

/// Image ▸ Trim… with the dialog's options: crop the canvas to the content
/// judged by `spec`, as one undoable step.
pub fn trim_with(editor: &mut Editor, spec: TrimSpec) -> Result<String, String> {
    if !spec.is_valid() {
        return Err("Choose at least one side to trim".to_string());
    }
    let (w, h, rgba) = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let rect = doc.canvas_rect();
        let rgba = doc.composite(rect).map_err(|e| e.to_string())?;
        (doc.document.width(), doc.document.height(), rgba)
    };
    let (x, y, new_w, new_h) =
        trim_rect(&rgba, w, h, spec).ok_or("The document has no content to trim to")?;
    if (x, y, new_w, new_h) == (0, 0, w, h) {
        return Err("The content already fills the canvas".to_string());
    }
    editor.resize_canvas(new_w, new_h, IVec2::new(x as i32, y as i32))?;
    Ok(format!("Trimmed the canvas to {new_w}×{new_h}"))
}

/// The selection's coverage, one byte per canvas pixel — the baseline the
/// Refine Edge dialog previews and refines.
fn selection_coverage(selection: &Selection, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize)];
    for y in 0..h {
        for x in 0..w {
            let c = selection.coverage_at(IVec2::new(x as i32, y as i32));
            out[(y * w + x) as usize] = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

/// Select ▸ Refine Edge…: the dialog over the document's composite and the
/// selection's coverage. `None` without a document or a selection — the
/// menu gates the same way; this is the second line of defence.
pub fn refine_edge_dialog(editor: &Editor) -> Option<ui::dialogs::refine_mask::RefineMaskDialog> {
    let doc = editor.active()?;
    doc.document.selection.bounds()?;
    let (w, h) = (doc.document.width(), doc.document.height());
    // The free compositor: the host holds only `&Editor` when a dialog
    // opens, and a one-off full-canvas composite is what the preview shows.
    let canvas = compositor::composite_region(
        &doc.document,
        &doc.tiles,
        doc.canvas_rect(),
        0,
        compositor::CompositeOptions::default(),
    )
    .ok()?;
    let content = canvas.to_rgba8(&doc.document.meta.color_space);
    let baseline = selection_coverage(&doc.document.selection, w, h);
    Some(ui::dialogs::refine_mask::RefineMaskDialog::new(
        content, baseline, w, h,
    ))
}

/// Select ▸ Refine Edge… confirmed: selection → temporary coverage → the
/// Refine Mask pipeline → back to the selection, as one undoable
/// [`Command::SetSelection`].
pub fn refine_edge_with(editor: &mut Editor, spec: &RefineMaskSpec) -> Result<String, String> {
    if spec.is_identity() {
        return Err("Every Refine Edge parameter is at its neutral value".to_string());
    }
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        doc.document
            .selection
            .bounds()
            .ok_or("There is no selection")?;
        let (w, h) = (doc.document.width(), doc.document.height());
        let baseline = selection_coverage(&doc.document.selection, w, h);
        let mask =
            SelectionMask::new(IVec2::ZERO, w, h, baseline.clone()).map_err(|e| e.to_string())?;
        let refined = selection::refine_mask(&mask, &spec.params()).map_err(|e| e.to_string())?;
        let after = selection_coverage(&Selection::Mask(refined.clone()), w, h);
        if after == baseline {
            return Err("The refinement changed no pixel of the selection".to_string());
        }
        Command::Transaction {
            label: "Refine Edge".to_string(),
            commands: vec![Command::SetSelection {
                selection: Selection::Mask(refined),
            }],
        }
    };
    editor.apply_command(command);
    Ok(format!(
        "Refined the selection edge: feather {} px, shift {} px, smooth {} px, contrast {:.0}%",
        spec.feather_px,
        spec.shift_px,
        spec.smooth_px,
        spec.contrast * 100.0
    ))
}

/// File ▸ Save as PSD…: the layered PSD writer behind a `.psd` picker.
///
/// The editor's `Export` action owns the only road to the export picker and
/// to [`OpenDocument::export_to`], which writes a layered `.psd` by
/// extension; this arms that one picker as a PSD save (see
/// [`crate::dialogs::arm_psd_save`]) and dispatches it.
pub fn save_as_psd(editor: &mut Editor) -> Result<String, String> {
    if editor.active().is_none() {
        return Err("No document is open".to_string());
    }
    // W11-H: a canvas past 30 000 px is offered as a `.psb`.
    let size = editor
        .active()
        .map(|d| (d.document.width(), d.document.height()))
        .unwrap_or_default();
    crate::dialogs::arm_psd_save_for(size);
    let outcome = editor.dispatch(Action::Export);
    // A dispatch that never reached the picker leaves the arming behind;
    // clear it so the next plain Export is a plain Export.
    let _ = crate::dialogs::take_psd_save();
    match outcome {
        Ok(_) => Ok(editor
            .status()
            .map(|s| s.replacen("Exported", "Saved a layered PSD at", 1))
            .unwrap_or_else(|| "Saved a layered PSD".to_string())),
        Err(e) => Err(format!("Save as PSD: {e}")),
    }
}

#[cfg(test)]
#[path = "w11i_align_tests.rs"]
mod w11i_align_tests;

// W11-E: Transform Again, Arrange > Reverse, Select Linked Layers, Smart
// Object Convert to Linked / Embed Linked and the colour labels.
#[path = "layer_ops_w11e.rs"]
pub(crate) mod w11e;

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut out = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                out[i..i + 4].copy_from_slice(&f(x, y));
            }
        }
        out
    }

    #[test]
    fn trim_rect_finds_the_ink_and_honours_the_sides() {
        // Ink in a 3x2 block at (2,1) on a 10x8 transparent canvas.
        let rgba = canvas(10, 8, |x, y| {
            if (2..5).contains(&x) && (1..3).contains(&y) {
                [9, 9, 9, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        assert_eq!(
            trim_rect(&rgba, 10, 8, TrimSpec::default()),
            Some((2, 1, 3, 2))
        );
        // Only the left and top sides: the right and bottom keep the canvas.
        assert_eq!(
            trim_rect(
                &rgba,
                10,
                8,
                TrimSpec {
                    right: false,
                    bottom: false,
                    ..TrimSpec::default()
                }
            ),
            Some((2, 1, 8, 7))
        );
        // Nothing but transparency: nothing to trim to.
        let blank = canvas(4, 4, |_, _| [0, 0, 0, 0]);
        assert_eq!(trim_rect(&blank, 4, 4, TrimSpec::default()), None);
    }

    #[test]
    fn trim_rect_judges_by_a_corner_colour_when_asked() {
        // An opaque white canvas with a red square: transparent-basis finds
        // everything (the canvas is opaque), the top-left basis finds the
        // square.
        let rgba = canvas(12, 12, |x, y| {
            if (4..8).contains(&x) && (5..9).contains(&y) {
                [255, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            }
        });
        assert_eq!(
            trim_rect(&rgba, 12, 12, TrimSpec::default()),
            Some((0, 0, 12, 12))
        );
        assert_eq!(
            trim_rect(
                &rgba,
                12,
                12,
                TrimSpec {
                    basis: TrimBasis::TopLeftColor,
                    ..TrimSpec::default()
                }
            ),
            Some((4, 5, 4, 4))
        );
        // The bottom-right basis judges against the other corner: on this
        // canvas the two corners match, so it agrees.
        assert_eq!(
            trim_rect(
                &rgba,
                12,
                12,
                TrimSpec {
                    basis: TrimBasis::BottomRightColor,
                    ..TrimSpec::default()
                }
            ),
            Some((4, 5, 4, 4))
        );
    }

    #[test]
    fn align_deltas_land_the_named_edge_on_the_target() {
        let bounds = (10.0, 20.0, 30.0, 40.0);
        let target = (0.0, 0.0, 100.0, 60.0);
        assert_eq!(align_delta(AlignEdge::Left, bounds, target), (-10.0, 0.0));
        assert_eq!(align_delta(AlignEdge::Right, bounds, target), (70.0, 0.0));
        assert_eq!(
            align_delta(AlignEdge::HorizontalCenter, bounds, target),
            (30.0, 0.0)
        );
        assert_eq!(align_delta(AlignEdge::Top, bounds, target), (0.0, -20.0));
        assert_eq!(align_delta(AlignEdge::Bottom, bounds, target), (0.0, 20.0));
        assert_eq!(
            align_delta(AlignEdge::VerticalCenter, bounds, target),
            (0.0, 0.0)
        );
    }
}
