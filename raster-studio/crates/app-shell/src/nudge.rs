//! W13X-1: the arrow keys nudge (Photopea, learn/layer-manipulation).
//!
//! * With the **Move** tool an arrow moves what a Move drag of the same
//!   distance would move: the active layer (and the rest of the panel's
//!   layer selection, its link groups, minus locked layers), or, with a
//!   pixel selection, the selected pixels of the active layer, lifted and
//!   laid down with the marching ants travelling along. One pixel, ten with
//!   Shift. With Alt the move lands on a COPY: the layers are duplicated in
//!   place and the copies move (`move_duplicate`, W13-A's Alt+drag path), or
//!   the selected pixels float a copy and the originals stay.
//! * With a **selection tool** (the marquees, the lassos, the Magic Wand,
//!   Quick and Object Selection) an arrow moves the selection OUTLINE only;
//!   no pixel changes. Shift makes it ten pixels.
//! * Any other tool: an arrow does nothing, as in Photopea.
//!
//! The Move half is not a second implementation of the move: a fresh
//! [`tools::edit::MoveTool`] (Auto-Select off, so the pick is the active
//! layer, as Photopea's nudge is) is driven through a press and a release
//! `step` pixels apart over the same off-pointer context a commit uses, with
//! the snap candidates emptied (a nudge never snaps). Whatever the tool
//! emits takes the pointer route's own apply path.
//!
//! History: every press is ONE step (Photopea records each nudge as its own
//! Move entry); consecutive nudges are not coalesced. A held key's repeats
//! each nudge again; with Alt only the first press copies — the repeats move
//! the copy it made.

use editor_core::{Command, Selection};
use glam::{IVec2, Vec2};
use tools::{Modifiers, PointerEvent, Tool, ToolId, ToolRequest};

use super::super::ToolPointer;
use crate::action::NudgeDirection;
use crate::editor::Editor;

/// How far one nudge goes: 1 px, or 10 px with Shift.
pub(crate) fn step_of(direction: NudgeDirection, big: bool) -> IVec2 {
    let (x, y) = direction.unit();
    let n = if big { 10 } else { 1 };
    IVec2::new(x * n, y * n)
}

/// The tools on which an arrow moves the selection outline.
pub(crate) fn moves_the_outline(tool: ToolId) -> bool {
    matches!(
        tool,
        ToolId::RectMarquee
            | ToolId::EllipseMarquee
            | ToolId::SingleRowMarquee
            | ToolId::SingleColumnMarquee
            | ToolId::Lasso
            | ToolId::PolygonalLasso
            | ToolId::MagneticLasso
            | ToolId::MagicWand
            | ToolId::QuickSelect
            | ToolId::ObjectSelection
    )
}

impl ToolPointer {
    /// One arrow press: nudge by one pixel that way (ten with `big`), a copy
    /// with `copy` (Move tool only). Answers the history steps it landed —
    /// 0 when the current tool has nothing to nudge — or why it failed.
    ///
    /// Refused (0 steps, nothing touched) while a text run is open or a
    /// pointer gesture is running: the arrows are the caret's then, or the
    /// drag's.
    pub(crate) fn nudge(
        &mut self,
        editor: &mut Editor,
        direction: NudgeDirection,
        big: bool,
        copy: bool,
    ) -> Result<usize, String> {
        if editor.active().is_none()
            || self.is_text_editing()
            || self.is_gesture_active()
            || self.is_tool_active()
        {
            return Ok(0);
        }
        let step = step_of(direction, big);
        let tool = editor.effective_tool();
        if tool == ToolId::Move {
            self.nudge_with_move(editor, step, copy)
        } else if moves_the_outline(tool) {
            Ok(nudge_outline(editor, step))
        } else {
            Ok(0)
        }
    }

    /// The Move tool's half: a press/release `step` apart through a fresh
    /// Move tool, applied as the pointer route applies a Move release.
    fn nudge_with_move(
        &mut self,
        editor: &mut Editor,
        step: IVec2,
        copy: bool,
    ) -> Result<usize, String> {
        let saved = self.current.take();
        self.current = Some((ToolId::Move, Box::new(tools::edit::MoveTool::default())));
        let modifiers = Modifiers {
            alt: copy,
            ..Modifiers::NONE
        };
        let from = Vec2::ZERO;
        let to = step.as_vec2();
        let (result, commands, requests) = self.off_pointer(editor, |tool: &mut dyn Tool, ctx| {
            // A nudge goes exactly `step`: nothing may snap it.
            ctx.snap_candidates.clear();
            tool.on_pointer_down(
                ctx,
                PointerEvent {
                    pos: from,
                    pressure: 1.0,
                    modifiers,
                },
            )?;
            tool.on_pointer_up(
                ctx,
                PointerEvent {
                    pos: to,
                    pressure: 1.0,
                    modifiers,
                },
            )
        });
        self.current = saved;
        // A Show Transform Controls box framed the ink before it moved.
        self.move_display_seeded = None;
        if let Err(e) = result {
            return Err(e.to_string());
        }
        let before = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        // W13-A's path: layer moves land on duplicates, as one step.
        let (commands, requests) = if copy {
            super::copy_and_move(editor, commands, requests)
        } else {
            (commands, requests)
        };
        for command in commands {
            editor.apply_command(command);
        }
        for request in requests {
            match request {
                ToolRequest::TransformLayers { layers, delta } => {
                    Self::perform_transform_layers(editor, &layers, delta);
                }
                ToolRequest::SelectLayer(id) => editor.set_layer_selection(vec![id], Some(id)),
                _ => {}
            }
        }
        let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        Ok(after.saturating_sub(before))
    }
}

/// A selection tool's half: the outline moves `step`, the pixels stay. One
/// `SetSelection` history step (its inverse is the outline as it was).
fn nudge_outline(editor: &mut Editor, step: IVec2) -> usize {
    let Some(doc) = editor.active() else {
        return 0;
    };
    let next = match &doc.document.selection {
        Selection::None => return 0,
        Selection::Rect { min, max } => Selection::Rect {
            min: *min + step,
            max: *max + step,
        },
        mask @ Selection::Mask(_) => {
            let canvas = doc.canvas_rect();
            let canvas = selection::rect::Rect::from_xywh(
                canvas.x as i32,
                canvas.y as i32,
                canvas.width,
                canvas.height,
            );
            match selection::transform_selection(
                mask,
                canvas,
                glam::Affine2::from_translation(step.as_vec2()),
                selection::transform::ResampleFilter::Nearest,
            ) {
                Ok(next) => next,
                Err(e) => {
                    editor.set_status(e.to_string());
                    return 0;
                }
            }
        }
    };
    let before = doc.history_depth();
    editor.apply_command(Command::SetSelection { selection: next });
    let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
    after.saturating_sub(before)
}
