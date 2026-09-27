//! W18-F: the shell's half of the options bar's facts and requests
//! ([`tools::registry::bar_w18`]).
//!
//! * [`publish_pending`]: every frame, whether the live tool holds an edit
//!   Enter would confirm - a Type run, a Perspective Crop quad, a held Show
//!   Transform Controls drag - so the bar draws its Cancel cross and Commit
//!   check.
//! * [`drain`], from [`ToolPointer::begin_pending_session`] (every frame, and
//!   every pointer sample): the Cancel cross (Escape's route: a Type run is
//!   cancelled through its own text route, anything else through
//!   [`ToolPointer::cancel`]), a Crop by row (the Crop tool is given that box
//!   and waits for the commit, as Photopea's does), a Paint Bucket pattern
//!   pick, and the defined patterns the picker lists.
//! * [`adjust_input`], at the head of [`ToolPointer::handle`]: the Zoom bar's
//!   Zoom Out flips which way a click steps (Alt flips it back), and a Clone
//!   Stamp / Healing Brush press with the bar's Alt toggle in or K held picks
//!   the source exactly as an Alt press does.
//! * [`follow_all_documents`]: the Zoom and Hand bars' All Documents - a zoom
//!   or pan of the active document is applied to every open one.

use glam::Vec2;
use raster::PixelRect;
use tools::registry::bar_w18::{self as bar, CropBy};
use tools::{Tool, ToolId};
use ui::canvas::{PointerInput, PointerPhase, Route};

use super::{tight_document_bounds, ToolPointer};
use crate::editor::Editor;

/// The tools whose held edit only the shell can see: the Type family (a run
/// is not a canvas session the bar reads), Perspective Crop (its quad is not
/// either) and the Move tool (its transform session is the Show Transform
/// Controls box, held or not).
fn publishes(tool: ToolId) -> bool {
    matches!(
        tool,
        ToolId::Type
            | ToolId::VerticalType
            | ToolId::HorizontalTypeMask
            | ToolId::VerticalTypeMask
            | ToolId::PerspectiveCrop
            | ToolId::Move
    )
}

/// Publish whether the live tool holds an edit Enter would confirm.
pub(super) fn publish_pending(pointer: &ToolPointer) {
    let state = pointer.current.as_ref().map(|(id, tool)| {
        let held = publishes(*id) && (tool.has_pending_commit() || tool.is_text_editing());
        (*id, held)
    });
    bar::publish_pending(state);
}

/// Perform what the bar posted. Reports whether the published geometry may
/// have changed.
pub(super) fn drain(
    pointer: &mut ToolPointer,
    editor: &mut Editor,
    settings: &[(String, tools::ToolSetting)],
) -> bool {
    let mut changed = false;
    if bar::take_cancel() {
        if pointer.is_text_editing() {
            let _ = pointer.text_edit(editor, tools::TextEdit::Cancel);
        } else if pointer.cancel(editor) {
            pointer.settle_preview(editor);
        }
        changed = true;
    }
    if let Some(name) = bar::take_pattern_pick() {
        match editor.set_active_pattern(&name) {
            Ok(()) => editor.set_status(format!("Pattern: {name}")),
            Err(e) => editor.set_status(e),
        }
    }
    let names = editor.presets().pattern_names();
    let active = editor.active_pattern().map(|p| p.name.clone());
    if bar::patterns() != (names.clone(), active.clone()) {
        bar::publish_patterns(names, active);
    }
    if let Some(by) = bar::take_crop_by() {
        changed |= crop_by(pointer, editor, by, settings);
    }
    publish_pending(pointer);
    changed
}

/// The box a Crop by row sets, in document pixels: the bounds of every
/// layer, of the active layer, of the non-transparent composite (Trim's
/// basis) or of the selection. `None` when there is nothing to box.
pub fn crop_by_rect(editor: &mut Editor, by: CropBy) -> Option<PixelRect> {
    let doc = editor.active_mut()?;
    match by {
        CropBy::AllLayers => {
            let ids = doc.document.layers.iter_depth_first();
            ids.into_iter()
                .filter_map(|id| tight_document_bounds(&doc.document, &doc.tiles, id))
                .reduce(union)
        }
        CropBy::CurrentLayer => {
            let id = doc.document.active_layer()?;
            tight_document_bounds(&doc.document, &doc.tiles, id)
        }
        CropBy::Trim => {
            let canvas = doc.canvas_rect();
            let rgba = doc.composite(canvas).ok()?;
            let (x, y, w, h) = crate::layer_ops::trim_rect(
                &rgba,
                canvas.width,
                canvas.height,
                ui::dialogs::TrimSpec::default(),
            )?;
            Some(PixelRect::new(
                canvas.x + i64::from(x),
                canvas.y + i64::from(y),
                w,
                h,
            ))
        }
        CropBy::Selection => {
            let (min, max) = doc.document.selection.bounds()?;
            let (w, h) = ((max.x - min.x).max(0), (max.y - min.y).max(0));
            (w > 0 && h > 0)
                .then(|| PixelRect::new(i64::from(min.x), i64::from(min.y), w as u32, h as u32))
        }
    }
}

fn union(a: PixelRect, b: PixelRect) -> PixelRect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
}

/// Give the Crop tool the box `by` describes, waiting for the commit.
fn crop_by(
    pointer: &mut ToolPointer,
    editor: &mut Editor,
    by: CropBy,
    settings: &[(String, tools::ToolSetting)],
) -> bool {
    if editor.effective_tool() != ToolId::Crop {
        return false;
    }
    let Some(doc_id) = editor.active().map(|d| d.id()) else {
        return false;
    };
    let Some(rect) = crop_by_rect(editor, by) else {
        editor.set_status("Nothing to crop to".to_string());
        return false;
    };
    let mut crop = tools::edit::CropTool::default();
    for (key, setting) in settings {
        let _ = crop.set_setting(key, *setting);
    }
    crop.box_rect = Some(rect);
    pointer.current = Some((ToolId::Crop, Box::new(crop)));
    pointer.session_doc = Some(doc_id);
    true
}

/// The pointer sample as the bar's toggles read it: see the module docs.
pub(super) fn adjust_input(tool: ToolId, mut input: PointerInput) -> PointerInput {
    if tool == ToolId::Zoom && bar::nav().zoom_out {
        input.modifiers.alt = !input.modifiers.alt;
    }
    if matches!(tool, ToolId::CloneStamp | ToolId::HealingBrush)
        && input.phase == PointerPhase::Down
    {
        // Photopea: "Select clone source by holding Alt (or K) and clicking
        // on the image"; the Alt toggle pops out once it has picked.
        let armed = bar::take_select_source();
        if armed || bar::source_key_held() {
            input.modifiers.alt = true;
        }
    }
    input
}

/// The Zoom and Hand bars' All Documents: the active document went from
/// `before` (zoom, centre) to its camera now through `route`; every other
/// open document takes the same zoom step about its own centre, or the same
/// on-screen pan.
pub(super) fn follow_all_documents(editor: &mut Editor, route: Route, before: (f32, Vec2)) {
    let prefs = bar::nav();
    let Some(active) = editor.active().map(|d| d.id()) else {
        return;
    };
    let Some((zoom, center)) = editor.active().map(|d| (d.camera.zoom, d.camera.center)) else {
        return;
    };
    match route {
        Route::Zoom if prefs.zoom_all_documents => {
            let factor = zoom / before.0;
            if !factor.is_finite() || factor == 1.0 {
                return;
            }
            for doc in editor.documents_mut() {
                if doc.id() != active {
                    doc.camera.zoom =
                        (doc.camera.zoom * factor).clamp(render::MIN_ZOOM, render::MAX_ZOOM);
                }
            }
        }
        Route::Pan if prefs.hand_all_documents => {
            let screen = (center - before.1) * zoom;
            if screen == Vec2::ZERO || !screen.is_finite() {
                return;
            }
            for doc in editor.documents_mut() {
                if doc.id() != active {
                    doc.camera.center += screen / doc.camera.zoom;
                }
            }
        }
        _ => {}
    }
}
