//! W16-A: the selection tools' pointer plumbing (Photopea).
//!
//! * **Drag the outline.** A press inside the selection with any selection
//!   tool (the marquees, the three lassos, the Magic Wand, Quick Selection,
//!   Object Selection), in New mode and with no modifier held, moves the
//!   selection OUTLINE once the pointer travels [`OUTLINE_DRAG_START_PX`]
//!   screen pixels: the ants follow the pointer, the pixels stay, and the
//!   release lands ONE `SetSelection` step (Escape puts the outline back).
//!   Shift during the drag constrains it to 45 degrees. A press that does not
//!   travel is the tool's own click (a wand click still selects).
//! * **Hovers reach an open lasso.** While a polygonal or magnetic lasso (or
//!   a freehand one in its Alt mode) holds an outline between presses, the
//!   pointer's hovers are handed to it, so the magnetic lasso lays its
//!   anchors as the pointer moves and letting Alt go closes the freehand one.
//! * **Enter lands the selection** a lasso closes from `Tool::commit`
//!   ([`ToolPointer::commit`] folds what the off-pointer route drained).
//! * **Backspace/Delete** remove an open lasso's last point
//!   ([`ToolPointer::remove_last_lasso_point`], called by the shell's key
//!   route).

use editor_core::{Command, Selection};
use tools::select::{mode_of_settings, translate_selection, OutlineDrag, LASSO_REMOVE_LAST_POINT};
use tools::{PointerEvent, SelectionEdit, Tool, ToolContext, ToolId, ToolSetting};
use ui::canvas::{PointerPhase, Route};

use super::{DocumentTiles, PointerOutcome, ToolPointer};
use crate::doc::DocumentId;
use crate::editor::Editor;

/// How far, in screen pixels, the pointer must travel from a press inside the
/// selection before the outline starts to move — a click's jitter is still a
/// click.
pub(crate) const OUTLINE_DRAG_START_PX: f32 = 3.0;

/// A press inside the selection that may become an outline drag.
pub(crate) struct OutlineDragState {
    /// The document the press landed on.
    doc: DocumentId,
    /// The selection as it was at the press — what Escape puts back and what
    /// the release's step undoes to.
    base: Selection,
    drag: OutlineDrag,
    /// [`OUTLINE_DRAG_START_PX`] in document pixels at the press's zoom.
    threshold_doc: f32,
    /// The pointer has travelled past the threshold: the outline is moving
    /// and the tool's own gesture was called off.
    moving: bool,
}

/// The three lassos — the tools that hold an outline open between presses.
pub(crate) fn is_lasso(id: ToolId) -> bool {
    matches!(
        id,
        ToolId::Lasso | ToolId::PolygonalLasso | ToolId::MagneticLasso
    )
}

/// Show `selection` on the document `doc` without touching its history: the
/// live half of an outline drag (the ants are traced from the field).
fn show(editor: &mut Editor, doc: DocumentId, selection: Selection) {
    if let Some(open) = editor.documents_mut().iter_mut().find(|d| d.id() == doc) {
        open.document.selection = selection;
    }
}

/// Fold `edits` into the active document's selection as ONE history step —
/// the same fold the pointer route performs (card 056). Answers the steps
/// landed and why nothing landed, if it failed.
fn land_selection_edits(editor: &mut Editor, edits: &[SelectionEdit]) -> (usize, Option<String>) {
    if edits.is_empty() {
        return (0, None);
    }
    let Some(doc) = editor.active() else {
        return (0, None);
    };
    let c = doc.canvas_rect();
    let canvas = selection::Rect::from_xywh(
        c.x.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        c.y.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        c.width,
        c.height,
    );
    let mut current = doc.document.selection.clone();
    let mut commands = Vec::new();
    for edit in edits {
        match edit.apply(canvas, &current) {
            Ok(next) if next != current => {
                commands.push(Command::SetSelection {
                    selection: next.clone(),
                });
                current = next;
            }
            Ok(_) => {}
            Err(e) => return (0, Some(e.to_string())),
        }
    }
    let command = match commands.len() {
        0 => return (0, None),
        1 => commands.pop().expect("one command"),
        _ => Command::Transaction {
            label: "Select".to_string(),
            commands,
        },
    };
    let before = doc.history_depth();
    editor.apply_command(command);
    let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
    (after.saturating_sub(before), None)
}

impl ToolPointer {
    /// W16-A: the outline-drag route, consulted for every sample of a tool
    /// gesture. `Some(steps)` means the sample was the outline's and the tool
    /// must not see it; `None` hands it to the tool as usual. A Down is never
    /// consumed: the tool keeps its click until the pointer travels.
    pub(crate) fn route_outline_drag(
        &mut self,
        editor: &mut Editor,
        id: ToolId,
        phase: PointerPhase,
        event: PointerEvent,
        settings: &[(String, ToolSetting)],
    ) -> Option<usize> {
        match phase {
            PointerPhase::Down => {
                self.cancel_outline_drag(editor);
                if !super::move_duplicate::nudge::moves_the_outline(id) {
                    return None;
                }
                // A press that extends an open lasso outline is the lasso's.
                if self
                    .current
                    .as_ref()
                    .is_some_and(|(cur, tool)| *cur == id && tool.has_pending_commit())
                {
                    return None;
                }
                let doc = editor.active()?;
                if !OutlineDrag::begins(
                    &doc.document.selection,
                    event.pos,
                    event.modifiers,
                    mode_of_settings(settings),
                ) {
                    return None;
                }
                let zoom = doc.camera.zoom;
                let threshold_doc = if zoom.is_finite() && zoom > 0.0 {
                    OUTLINE_DRAG_START_PX / zoom
                } else {
                    OUTLINE_DRAG_START_PX
                };
                self.outline_drag = Some(OutlineDragState {
                    doc: doc.id(),
                    base: doc.document.selection.clone(),
                    drag: OutlineDrag::new(event.pos),
                    threshold_doc,
                    moving: false,
                });
                None
            }
            PointerPhase::Move => {
                let state = self.outline_drag.as_mut()?;
                if !state.moving {
                    if (event.pos - state.drag.press()).length() < state.threshold_doc {
                        return None;
                    }
                    state.moving = true;
                    // The tool had the press; its gesture is called off.
                    self.cancel_live_tool(editor);
                }
                let state = self.outline_drag.as_mut()?;
                let offset = state.drag.drag_to(event.pos, event.modifiers.shift);
                let (doc, base) = (state.doc, state.base.clone());
                let canvas = editor.active().map(|d| d.canvas_rect())?;
                match translate_selection(&base, canvas, offset) {
                    Ok(moved) => show(editor, doc, moved),
                    Err(e) => editor.set_status(e.to_string()),
                }
                Some(0)
            }
            PointerPhase::Up => {
                if !self.outline_drag.as_ref()?.moving {
                    self.outline_drag = None;
                    return None;
                }
                let mut state = self.outline_drag.take()?;
                let offset = state.drag.drag_to(event.pos, event.modifiers.shift);
                // The live outline goes back first, so the step's inverse is
                // the selection as it was at the press.
                show(editor, state.doc, state.base.clone());
                if offset == glam::IVec2::ZERO || editor.active().map(|d| d.id()) != Some(state.doc)
                {
                    return Some(0);
                }
                let canvas = editor.active().map(|d| d.canvas_rect())?;
                let moved = match translate_selection(&state.base, canvas, offset) {
                    Ok(moved) => moved,
                    Err(e) => {
                        editor.set_status(e.to_string());
                        return Some(0);
                    }
                };
                let before = editor.active().map(|d| d.history_depth()).unwrap_or(0);
                editor.apply_command(Command::SetSelection { selection: moved });
                let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
                Some(after.saturating_sub(before))
            }
        }
    }

    /// W16-A: abandon an outline drag (Escape, a lost document): the outline
    /// goes back where the press found it, and no step is taken.
    pub(crate) fn cancel_outline_drag(&mut self, editor: &mut Editor) -> bool {
        match self.outline_drag.take() {
            Some(state) if state.moving => {
                show(editor, state.doc, state.base);
                true
            }
            _ => false,
        }
    }

    /// W16-A: `true` while an outline drag is moving the ants.
    pub fn is_dragging_outline(&self) -> bool {
        self.outline_drag.as_ref().is_some_and(|s| s.moving)
    }

    /// Call off the live tool's gesture (the outline drag took the pointer).
    fn cancel_live_tool(&mut self, editor: &mut Editor) {
        let Some((_, tool)) = &mut self.current else {
            return;
        };
        let Some(doc) = editor.active_mut() else {
            return;
        };
        let canvas = doc.canvas_rect();
        let mut access = DocumentTiles::new(&doc.document.pixels, &mut doc.tiles)
            .at_document_depth(doc.document.meta.bit_depth);
        let tool: &mut dyn Tool = tool.as_mut();
        tool.cancel(&mut ToolContext::new(&mut access, canvas));
    }

    /// W16-A: a hover while a lasso holds an outline open between presses is
    /// handed to it — the magnetic lasso lays anchors as the pointer moves,
    /// and a freehand one in its Alt mode closes when Alt is let go.
    pub(crate) fn route_lasso_hover(
        &mut self,
        editor: &mut Editor,
        route: Route,
        event: PointerEvent,
        out: &mut PointerOutcome,
    ) {
        let Route::Tool(id) = route else {
            return;
        };
        let before = match &self.current {
            Some((cur, tool)) if *cur == id && is_lasso(id) && tool.has_pending_commit() => {
                tool.live_geometry()
            }
            _ => return,
        };
        let (result, _commands, _requests) =
            self.off_pointer(editor, |tool, ctx| tool.on_pointer_move(ctx, event));
        out.reached_tool = true;
        if let Err(e) = result {
            out.failed = Some(e.to_string());
            editor.set_status(e.to_string());
        }
        let (steps, failed) = self.land_off_pointer_selection(editor);
        out.steps += steps;
        out.selection_changed |= steps > 0;
        if let Some(reason) = failed {
            editor.set_status(reason.clone());
            out.failed = Some(reason);
        }
        let after = self.current.as_ref().and_then(|(_, t)| t.live_geometry());
        if after != before {
            // The outline changed: ask for a frame, as a live preview does.
            out.preview_tiles = out.preview_tiles.max(1);
        }
    }

    /// W16-A: fold what the last off-pointer call drained from the tool's
    /// selection outbox — a lasso closed by Enter or by a hover — as one
    /// history step.
    pub(crate) fn land_off_pointer_selection(
        &mut self,
        editor: &mut Editor,
    ) -> (usize, Option<String>) {
        let edits = std::mem::take(&mut self.off_pointer_selection);
        land_selection_edits(editor, &edits)
    }

    /// W16-A: Backspace / Delete while a lasso holds an outline open between
    /// presses: its last point goes (the only one going ends the outline).
    /// `false` when no lasso outline is open, so the key keeps its keymap
    /// meaning (Delete clears the selected pixels).
    pub fn remove_last_lasso_point(&mut self, editor: &Editor) -> bool {
        if editor.active().is_none() || self.router.is_gesture_active() {
            return false;
        }
        match &mut self.current {
            Some((id, tool)) if is_lasso(*id) && tool.has_pending_commit() => tool
                .set_setting(LASSO_REMOVE_LAST_POINT, ToolSetting::Bool(true))
                .is_ok(),
            _ => false,
        }
    }
}
