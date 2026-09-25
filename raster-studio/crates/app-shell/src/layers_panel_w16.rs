//! W16-D: the application's half of the Layers panel's Photopea behaviours.
//!
//! The panel ([`ui::panels::layers::w16`]) draws the effects list, routes
//! the double-clicks, solos on Alt-click, deletes on a drop onto the trash
//! and keeps the panel options; two things need the application:
//!
//! * the row menu's **Duplicate Layer**, which copies at once (Photopea's;
//!   the dialog is its **Duplicate Into…**) — the panel queues a request and
//!   [`after_harvest`] turns it into [`Action::DuplicateLayer`], the same
//!   dialog-free copy the Ctrl+Alt+J chord makes;
//! * **Thumbnails by Layer**, which crops each thumbnail to its layer's
//!   bounds — only the tile store can measure those, so [`after_harvest`]
//!   publishes each layer's bounds as a UV rectangle of the document-fitted
//!   thumbnail, re-measured when the document's content moves.
//!
//! The options persist through `Preferences::sync_layers_panel`, and the
//! copy-name option reaches every duplicate through `layer_ops::copy_name`.

use std::cell::RefCell;
use std::collections::HashMap;

use layer_model::LayerId;
use ui::panels::layers::w16::LayersRequest;

use crate::action::Action;
use crate::chrome::ChromeOutput;
use crate::doc::DocumentId;
use crate::editor::Editor;

thread_local! {
    /// The crops last published, and the document and content revision
    /// they measured, so an idle frame measures nothing.
    static MEASURED: RefCell<Option<(DocumentId, u64)>> = const { RefCell::new(None) };
}

/// Called by `Chrome::ui` once the frame's intents are routed: answer the
/// Layers panel's requests and keep its "Thumbnails by Layer" crops current.
pub(crate) fn after_harvest(w: &mut ui::Workspace, editor: &Editor, out: &mut ChromeOutput) {
    for request in w.layers.take_requests() {
        match request {
            LayersRequest::DuplicateLayer => out.actions.push(Action::DuplicateLayer),
        }
    }
    publish_thumb_crops(w, editor);
}

/// Measure every layer of the active document and hand the panel its crop,
/// while "Thumbnails by Layer" is on and the content has moved since the
/// last measure.
fn publish_thumb_crops(w: &mut ui::Workspace, editor: &Editor) {
    if !w.layers.thumbs_by_layer {
        MEASURED.with(|m| *m.borrow_mut() = None);
        return;
    }
    let Some(open) = editor.active() else {
        w.layers.set_thumb_crops(HashMap::new());
        MEASURED.with(|m| *m.borrow_mut() = None);
        return;
    };
    let key = (open.id(), editor.content_revision());
    if MEASURED.with(|m| *m.borrow() == Some(key)) {
        return;
    }
    let (dw, dh) = (
        open.document.width().max(1) as f32,
        open.document.height().max(1) as f32,
    );
    let mut crops = HashMap::new();
    for id in open.document.layers.iter_depth_first() {
        if let Some(uv) = layer_crop(open, id, dw, dh) {
            crops.insert(id, uv);
        }
    }
    w.layers.set_thumb_crops(crops);
    MEASURED.with(|m| *m.borrow_mut() = Some(key));
}

/// The UV rectangle (of the canvas-fitted thumbnail) `id`'s ink covers;
/// `None` for a layer with no measurable ink on the canvas.
fn layer_crop(
    open: &crate::doc::OpenDocument,
    id: LayerId,
    dw: f32,
    dh: f32,
) -> Option<egui::Rect> {
    let r = crate::tool_input::tight_document_bounds(&open.document, &open.tiles, id)?;
    if r.width == 0 || r.height == 0 {
        return None;
    }
    let x0 = (r.x as f32 / dw).clamp(0.0, 1.0);
    let y0 = (r.y as f32 / dh).clamp(0.0, 1.0);
    let x1 = ((r.x + r.width as i64) as f32 / dw).clamp(0.0, 1.0);
    let y1 = ((r.y + r.height as i64) as f32 / dh).clamp(0.0, 1.0);
    (x1 > x0 && y1 > y0).then(|| egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)))
}

#[cfg(test)]
#[path = "layers_panel_w16_tests.rs"]
mod tests;
