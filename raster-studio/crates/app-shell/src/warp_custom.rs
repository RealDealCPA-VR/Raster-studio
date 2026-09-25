//! W13X-5: Warp Text's Custom style, edited on the canvas.
//!
//! A type layer is **in warp mode** when it is the active layer, it is not
//! locked, its warp style is Custom (Layer > Text > Warp Text... > Custom)
//! and the Move tool is the tool in hand. The canvas then draws the warp's
//! 4x4 Bezier control mesh over the text (the grid through the sixteen
//! control points, and a square handle on each one) through the text
//! overlay route ([`crate::tool_input::ToolPointer::text_overlay_geometry`]),
//! and a Move press on a handle drags that control point instead of moving
//! the layer:
//!
//! * the press picks the nearest handle within [`HANDLE_PX`] screen pixels
//!   (a press anywhere else is the Move tool's, as always);
//! * each drag sample moves the point and re-renders the text as a live
//!   draft outside history ([`crate::doc::OpenDocument::apply_text_draft`]);
//! * the release puts the layer back as it was and applies ONE
//!   [`editor_core::Command::SetLayerKind`] with the new mesh, so the drag is
//!   one undo step, and the mesh is saved with the layer
//!   ([`layer_model::text::TextWarp::mesh`]).
//!
//! The mesh is stored as fractions of the text's line-box bounds, so it
//! follows the text when it is re-edited or re-laid out.

use std::cell::RefCell;

use glam::Vec2;
use layer_model::text::{TextLayer, TextWarp, WarpStyle};
use layer_model::{LayerId, LayerKind};
use tools::ToolId;
use ui::canvas::PointerPhase;

use crate::doc::DocumentId;
use crate::editor::Editor;
use crate::tool_input::{TextOverlayKind, TextOverlaySegment};

/// How near a press must land to a handle, in screen pixels (and half the
/// drawn handle square's side).
pub(crate) const HANDLE_PX: f32 = 6.0;

/// One handle drag in progress.
#[derive(Clone)]
struct Drag {
    doc: DocumentId,
    layer: LayerId,
    index: usize,
    /// The layer as it was at the press: what the release restores before
    /// the one history step.
    original: TextLayer,
    bounds: text_engine::Rect,
}

thread_local! {
    static DRAG: RefCell<Option<Drag>> = const { RefCell::new(None) };
}

/// The active text layer in warp mode for `tool`: its id and payload.
pub(crate) fn target(editor: &Editor, tool: ToolId) -> Option<(LayerId, TextLayer)> {
    if tool != ToolId::Move {
        return None;
    }
    let doc = editor.active()?;
    let id = doc.document.active_layer()?;
    let layer = doc.document.layers.get(id)?;
    if layer.locked.all {
        return None;
    }
    match &layer.kind {
        LayerKind::Text(text) if text.warp.style == WarpStyle::Custom && text.path.is_none() => {
            Some((id, text.clone()))
        }
        _ => None,
    }
}

/// The text's line-box bounds (layer space), the box the mesh is fitted to.
fn bounds_of(text: &TextLayer) -> Option<text_engine::Rect> {
    let b = compositor::text::text_bounds(&text_engine::TextRun::from(text));
    (b.width > 0.0 && b.height > 0.0).then_some(b)
}

/// The sixteen handles in document space, over `bounds`.
fn handles_in_document(
    editor: &Editor,
    layer: LayerId,
    warp: &TextWarp,
    bounds: text_engine::Rect,
) -> Option<[Vec2; 16]> {
    let doc = editor.active()?;
    let local = text_engine::warp::mesh_handles(warp, bounds);
    let mut out = [Vec2::ZERO; 16];
    for (slot, p) in out.iter_mut().zip(local) {
        *slot = crate::interaction_geometry::layer_to_document(
            &doc.document,
            layer,
            0,
            Vec2::new(p[0], p[1]),
        )
        .ok()?;
    }
    Some(out)
}

/// Document pixels per screen pixel at the active document's zoom.
fn doc_per_screen_px(editor: &Editor) -> f32 {
    editor
        .active()
        .map(|d| d.camera.zoom)
        .filter(|z| z.is_finite() && *z > 0.0)
        .map_or(1.0, |z| 1.0 / z)
}

/// Whether a handle drag is under way.
pub(crate) fn is_dragging() -> bool {
    DRAG.with(|d| d.borrow().is_some())
}

/// Route one pointer sample of a `tool` gesture at `pos` (document pixels).
/// `None` when the sample is not a mesh-handle press, drag or release (the
/// tool gets it as usual); else `Some(steps)`, the undo steps it added (one
/// at a release that moved the handle), and the tool never sees it.
pub(crate) fn route(
    editor: &mut Editor,
    tool: ToolId,
    phase: PointerPhase,
    pos: Vec2,
) -> Option<usize> {
    match phase {
        PointerPhase::Down => press(editor, tool, pos).then_some(0),
        PointerPhase::Move => drag(editor, pos).then_some(0),
        PointerPhase::Up => release(editor, pos),
    }
}

fn press(editor: &mut Editor, tool: ToolId, pos: Vec2) -> bool {
    DRAG.with(|d| d.borrow_mut().take());
    let Some((layer, text)) = target(editor, tool) else {
        return false;
    };
    let Some(bounds) = bounds_of(&text) else {
        return false;
    };
    let Some(handles) = handles_in_document(editor, layer, &text.warp, bounds) else {
        return false;
    };
    let reach = HANDLE_PX * doc_per_screen_px(editor);
    let nearest = handles
        .iter()
        .enumerate()
        .map(|(i, h)| (i, h.distance(pos)))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((index, _)) = nearest else {
        return false;
    };
    let Some(doc) = editor.active().map(|d| d.id()) else {
        return false;
    };
    DRAG.with(|d| {
        *d.borrow_mut() = Some(Drag {
            doc,
            layer,
            index,
            original: text,
            bounds,
        });
    });
    true
}

/// The layer with the dragged handle at `pos`, or `None` when no drag is
/// under way on the active document.
fn dragged(editor: &Editor, pos: Vec2) -> Option<(Drag, TextLayer)> {
    let drag = DRAG.with(|d| d.borrow().clone())?;
    let doc = editor.active()?;
    if doc.id() != drag.doc {
        return None;
    }
    let local =
        crate::interaction_geometry::document_to_layer(&doc.document, drag.layer, 0, pos).ok()?;
    let mut text = drag.original.clone();
    text.warp = text_engine::warp::with_mesh_handle(
        &drag.original.warp,
        drag.bounds,
        drag.index,
        [local.x, local.y],
    );
    Some((drag, text))
}

fn drag(editor: &mut Editor, pos: Vec2) -> bool {
    if !is_dragging() {
        return false;
    }
    if let Some((drag, text)) = dragged(editor, pos) {
        if let Some(doc) = editor.active_mut() {
            let _ = doc.apply_text_draft(drag.layer, LayerKind::Text(text));
        }
    }
    true
}

fn release(editor: &mut Editor, pos: Vec2) -> Option<usize> {
    if !is_dragging() {
        return None;
    }
    let moved = dragged(editor, pos);
    let drag = DRAG.with(|d| d.borrow_mut().take())?;
    // Back to the pressed state outside history, then the one step.
    if let Some(doc) = editor.active_mut().filter(|d| d.id() == drag.doc) {
        let _ = doc.apply_text_draft(drag.layer, LayerKind::Text(drag.original.clone()));
    }
    if let Some((_, text)) = moved {
        if text.warp != drag.original.warp {
            editor.apply_command(editor_core::Command::SetLayerKind {
                layer_id: drag.layer,
                kind: Box::new(LayerKind::Text(text)),
            });
            return Some(1);
        }
    }
    Some(0)
}

/// Abandon a drag (Escape, a tab switch): the layer goes back to the press.
pub(crate) fn cancel(editor: &mut Editor) {
    if let Some(drag) = DRAG.with(|d| d.borrow_mut().take()) {
        if let Some(doc) = editor.active_mut().filter(|d| d.id() == drag.doc) {
            let _ = doc.apply_text_draft(drag.layer, LayerKind::Text(drag.original));
        }
    }
}

/// The mesh overlay for the layer in warp mode, in document space: the grid
/// through the control points (drawn as frame lines) and a square handle on
/// each point (drawn as selection lines). Empty when no layer is in warp
/// mode.
pub(crate) fn overlay(editor: &Editor, tool: ToolId) -> Vec<TextOverlaySegment> {
    let Some((layer, text)) = target(editor, tool) else {
        return Vec::new();
    };
    let Some(bounds) = bounds_of(&text) else {
        return Vec::new();
    };
    let Some(h) = handles_in_document(editor, layer, &text.warp, bounds) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let line = |out: &mut Vec<TextOverlaySegment>, a: Vec2, b: Vec2, kind| {
        out.push(TextOverlaySegment { a, b, kind });
    };
    for row in 0..4 {
        for col in 0..3 {
            let (i, j) = (row * 4 + col, col * 4 + row);
            line(&mut out, h[i], h[i + 1], TextOverlayKind::BoxFrame);
            line(&mut out, h[j], h[j + 4], TextOverlayKind::BoxFrame);
        }
    }
    let r = HANDLE_PX * doc_per_screen_px(editor);
    for p in h {
        let c = [
            p + Vec2::new(-r, -r),
            p + Vec2::new(r, -r),
            p + Vec2::new(r, r),
            p + Vec2::new(-r, r),
        ];
        for k in 0..4 {
            line(&mut out, c[k], c[(k + 1) % 4], TextOverlayKind::Selection);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! W13X-5: the mesh is drawn and dragged through the real pointer route
    //! (`ToolPointer::handle` and `text_overlay_geometry`, what the shell
    //! calls), and the composited canvas follows the dragged handle.
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use crate::tool_input::ToolPointer;
    use editor_core::Command;
    use layer_model::Layer;
    use raster::PixelRect;
    use ui::canvas::PointerInput;
    use ui::dialogs::{Dialog, DialogAction};

    const W: u32 = 400;
    const H: u32 = 200;
    const VIEWPORT: Vec2 = Vec2::new(800.0, 400.0);
    const AT: (f32, f32) = (40.0, 50.0);

    /// A white 400x200 document at 100 %, centred, with a black three-bar
    /// text layer, warped Custom through the Warp Text dialog.
    fn setup(dir: &std::path::Path) -> (Editor, LayerId) {
        let bytes = dejavu::sans::regular().to_vec();
        compositor::load_font(bytes.clone());
        text_engine::register_session_font(bytes);
        let png = dir.join("canvas.png");
        let white = vec![255u8; (W * H * 4) as usize];
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, W, H, &white).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&png).unwrap();
        let doc = ed.active_mut().unwrap();
        doc.set_viewport(VIEWPORT);
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
        let mut layer = Layer::with_kind(
            "Type",
            LayerKind::Text(TextLayer::legacy("I       I       I", "DejaVu Sans", 48.0)),
        );
        layer.transform = glam::Affine2::from_translation(glam::vec2(AT.0, AT.1));
        let id = layer.id;
        ed.apply_command(Command::create_layer(layer));
        ed.set_active_layer(id);
        // Layer > Text > Warp Text... > Custom, confirmed.
        let mut dialog = ui::dialogs::WarpTextDialog::new(id, text_of(&ed, id));
        dialog.set_style(WarpStyle::Custom);
        match dialog.confirm() {
            Some(DialogAction::Command(c)) => ed.apply_command(*c),
            other => panic!("the Custom warp was not confirmed: {other:?}"),
        }
        assert_eq!(text_of(&ed, id).warp.style, WarpStyle::Custom);
        (ed, id)
    }

    fn text_of(ed: &Editor, id: LayerId) -> TextLayer {
        match &ed.active().unwrap().document.layers.get(id).unwrap().kind {
            LayerKind::Text(t) => t.clone(),
            other => panic!("not text: {other:?}"),
        }
    }

    fn screen(doc: Vec2) -> Vec2 {
        VIEWPORT * 0.5 + (doc - Vec2::new(W as f32 / 2.0, H as f32 / 2.0))
    }

    /// Dark-ink centroid of the composite in document columns `x0..x1`.
    fn ink(ed: &mut Editor, x0: u32, x1: u32) -> Option<Vec2> {
        let rgba = ed
            .active_mut()
            .unwrap()
            .composite(PixelRect::new(0, 0, W, H))
            .unwrap();
        let (mut sx, mut sy, mut s) = (0.0f64, 0.0f64, 0.0f64);
        for y in 0..H {
            for x in x0..x1.min(W) {
                let i = ((y * W + x) * 4) as usize;
                let dark = 255.0 - f64::from(rgba[i]);
                if dark > 32.0 {
                    sx += dark * f64::from(x);
                    sy += dark * f64::from(y);
                    s += dark;
                }
            }
        }
        (s > 0.0).then(|| Vec2::new((sx / s) as f32, (sy / s) as f32))
    }

    #[test]
    fn w13x5_the_mesh_is_drawn_and_a_dragged_handle_bends_the_text_in_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, id) = setup(dir.path());
        let mut pointer = ToolPointer::new();
        let bounds = bounds_of(&text_of(&ed, id)).expect("shaped text");
        let local = text_engine::warp::mesh_handles(&text_of(&ed, id).warp, bounds);
        let corner = Vec2::new(local[15][0] + AT.0, local[15][1] + AT.1);

        // No mesh under another tool.
        ed.set_tool(ToolId::Brush);
        assert!(pointer.text_overlay_geometry(&ed).is_empty());
        // The Move tool puts the layer in warp mode: 24 grid lines and a
        // square on each of the 16 handles, in document space.
        ed.set_tool(ToolId::Move);
        let overlay = pointer.text_overlay_geometry(&ed);
        let grid = overlay
            .iter()
            .filter(|s| s.kind == TextOverlayKind::BoxFrame)
            .count();
        let squares = overlay
            .iter()
            .filter(|s| s.kind == TextOverlayKind::Selection)
            .count();
        assert_eq!((grid, squares), (24, 64), "{overlay:?}");
        assert!(
            overlay.iter().any(|s| s.kind == TextOverlayKind::Selection
                && ((s.a + s.b) * 0.5 - corner).length() < HANDLE_PX + 0.01
                && (s.a - corner).length() > HANDLE_PX),
            "a handle square is drawn round the bottom-right control point {corner}"
        );

        // The bars, flat.
        let third = bounds.width / 3.0;
        let left = (AT.0 as u32, (AT.0 + third) as u32);
        let right = (
            (AT.0 + 2.0 * third) as u32,
            (AT.0 + bounds.width + 2.0) as u32,
        );
        let flat_left = ink(&mut ed, left.0, left.1).expect("left bar");
        let flat_right = ink(&mut ed, right.0, right.1).expect("right bar");
        let depth = ed.active().unwrap().history.undo_depth();

        // Drag the bottom-right AND top-right handles 50 px down, through
        // the pointer route the shell drives.
        let top_right = Vec2::new(local[3][0] + AT.0, local[3][1] + AT.1);
        for handle in [corner, top_right] {
            let down = pointer.handle(
                &mut ed,
                PointerInput::at(PointerPhase::Down, screen(handle)),
                false,
                &[],
            );
            assert!(down.reached_tool, "{down:?}");
            assert!(is_dragging(), "the press grabbed the handle");
            let mid = pointer.handle(
                &mut ed,
                PointerInput::at(PointerPhase::Move, screen(handle + Vec2::new(0.0, 25.0))),
                false,
                &[],
            );
            assert!(mid.needs_repaint());
            let up = pointer.handle(
                &mut ed,
                PointerInput::at(PointerPhase::Up, screen(handle + Vec2::new(0.0, 50.0))),
                false,
                &[],
            );
            assert_eq!(up.steps, 1, "{up:?}");
            assert!(!is_dragging());
        }
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth + 2,
            "each handle drag is one undo step"
        );
        // Stored with the layer, as a fraction of the bounds.
        let mesh = text_of(&ed, id).warp.mesh.expect("the mesh is stored");
        assert!(
            (mesh[15][1] - (1.0 + 50.0 / bounds.height)).abs() < 1e-3,
            "{mesh:?}"
        );
        // The layer did not move: the Move tool never saw the drags.
        let pose = ed
            .active()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .transform;
        assert_eq!(pose.translation, glam::vec2(AT.0, AT.1));

        // The canvas: the right bar followed the handles down; the left bar,
        // far from them, stayed.
        let bent_right = ink(&mut ed, right.0 - 4, right.1 + 4).expect("right bar");
        let bent_left = ink(&mut ed, left.0, left.1).expect("left bar");
        assert!(
            bent_right.y - flat_right.y > 20.0,
            "the right bar moved down: {flat_right} -> {bent_right}"
        );
        assert!(
            (bent_left - flat_left).length() < 3.0,
            "the left bar stayed: {flat_left} -> {bent_left}"
        );

        // Undo takes the drags back.
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert!(ed.active_mut().unwrap().undo().unwrap());
        let back = ink(&mut ed, right.0, right.1).expect("right bar");
        assert!((back - flat_right).length() < 1.0, "{flat_right} vs {back}");
    }

    /// A Move press away from every handle is the Move tool's, as always.
    #[test]
    fn w13x5_a_press_off_the_handles_is_not_taken() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, _) = setup(dir.path());
        assert_eq!(
            route(
                &mut ed,
                ToolId::Move,
                PointerPhase::Down,
                Vec2::new(390.0, 190.0)
            ),
            None
        );
        assert!(!is_dragging());
    }
}
