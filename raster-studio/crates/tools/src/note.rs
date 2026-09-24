//! W10-B: the Note tool (Photoshop's and Photopea's, in the Eyedropper slot).
//!
//! A click on the canvas pins a note there. The note itself is a document
//! record, not a layer ([`layer_model::DocumentExtras::notes`]), and a tool
//! does not see the document's records, so the tool publishes where the
//! click landed as a [`ToolRequest::PlaceNote`] at pointer-up and the shell
//! adds the note (one undo step). The Notes panel lists the notes and edits
//! their text; the canvas draws a pin at each one.
//!
//! A press and its release must both land on the canvas: a click in the
//! pasteboard around the image pins nothing. A drag is still a click — the
//! note pins where the button came up, which is where the user let go.

use glam::Vec2;

use crate::error::ToolError;
use crate::tool::{PointerEvent, Tool, ToolContext, ToolId, ToolRequest};

/// See the module docs.
#[derive(Debug, Default)]
pub struct NoteTool {
    /// The button is down on the canvas.
    pressed: bool,
}

/// `p` is on the canvas `ctx.canvas`.
fn on_canvas(ctx: &ToolContext<'_>, p: Vec2) -> bool {
    let c = ctx.canvas;
    p.is_finite()
        && p.x >= c.x as f32
        && p.y >= c.y as f32
        && p.x <= c.right() as f32
        && p.y <= c.bottom() as f32
}

impl Tool for NoteTool {
    fn id(&self) -> ToolId {
        ToolId::Note
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.pressed = on_canvas(ctx, event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let pressed = std::mem::take(&mut self.pressed);
        if pressed && on_canvas(ctx, event.pos) {
            ctx.emit_request(ToolRequest::PlaceNote { at: event.pos });
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.pressed = false;
    }

    fn is_active(&self) -> bool {
        self.pressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use raster::PixelRect;

    #[test]
    fn a_click_on_the_canvas_asks_for_a_note_there() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 32));
        let mut tool = NoteTool::default();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 12.0))
            .unwrap();
        assert!(tool.is_active());
        tool.on_pointer_up(&mut ctx, PointerEvent::at(11.0, 12.5))
            .unwrap();
        assert!(!tool.is_active());
        assert_eq!(
            ctx.drain_requests(),
            vec![ToolRequest::PlaceNote {
                at: Vec2::new(11.0, 12.5)
            }]
        );
        assert!(ctx.commands().is_empty(), "the shell makes the note");
    }

    #[test]
    fn a_click_off_the_canvas_or_a_cancelled_one_pins_nothing() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 32));
        let mut tool = NoteTool::default();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(-5.0, 12.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(-5.0, 12.0))
            .unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(5.0, 40.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(5.0, 40.0))
            .unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(5.0, 5.0))
            .unwrap();
        tool.cancel(&mut ctx);
        tool.on_pointer_up(&mut ctx, PointerEvent::at(5.0, 5.0))
            .unwrap();
        assert!(ctx.drain_requests().is_empty());
    }
}
