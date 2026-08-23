//! Navigation: hand, zoom, rotate view.
//!
//! These are the only tools that emit nothing. Pan, zoom and canvas rotation
//! are properties of the *viewport*, not of the document — they change no
//! pixel, survive no save, and must never appear in undo history, because a
//! ctrl+Z that scrolls the canvas instead of undoing an edit is the most
//! confusing thing an editor can do. They mutate [`ToolContext::view`] in place
//! and stop there.

use glam::Vec2;

use crate::error::ToolError;
use crate::tool::{PointerEvent, Tool, ToolContext, ToolId, ViewState};

/// Which navigation gesture a [`ViewTool`] performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewGesture {
    /// Drag the canvas.
    Pan,
    /// Drag right to zoom in, left to zoom out; click to step.
    Zoom,
    /// Drag to spin the canvas under the cursor.
    Rotate,
}

/// How many screen pixels of drag double the zoom.
const ZOOM_DRAG_PX: f32 = 200.0;

/// Hand, zoom and rotate-view.
pub struct ViewTool {
    gesture: ViewGesture,
    /// Where the drag started, in *document* space, so the anchor stays put
    /// while the view moves under it.
    anchor_doc: Option<Vec2>,
    /// The cursor's last **screen** angle about the viewport centre.
    ///
    /// Screen, not document: the view rotation is what the gesture is
    /// changing, so measuring the angle in document space would feed the
    /// rotation back into its own input and the canvas would chase the cursor
    /// at half speed.
    last_screen_angle: f32,
    start_zoom: f32,
    dragged: bool,
}

impl ViewTool {
    pub fn new(gesture: ViewGesture) -> Self {
        Self {
            gesture,
            anchor_doc: None,
            last_screen_angle: 0.0,
            start_zoom: 1.0,
            dragged: false,
        }
    }

    fn screen_angle(ctx: &ToolContext<'_>, doc: Vec2) -> f32 {
        let d = ctx.view.screen_at(doc) - ctx.view.viewport * 0.5;
        d.y.atan2(d.x)
    }
}

impl Tool for ViewTool {
    fn id(&self) -> ToolId {
        match self.gesture {
            ViewGesture::Pan => ToolId::Hand,
            ViewGesture::Zoom => ToolId::Zoom,
            ViewGesture::Rotate => ToolId::RotateView,
        }
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("view anchor", event.pos)?;
        self.anchor_doc = Some(event.pos);
        self.last_screen_angle = Self::screen_angle(ctx, event.pos);
        self.start_zoom = ctx.view.zoom;
        self.dragged = false;
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(anchor) = self.anchor_doc else {
            return Ok(());
        };
        if !event.pos.x.is_finite() || !event.pos.y.is_finite() {
            return Ok(());
        }
        match self.gesture {
            ViewGesture::Pan => {
                // The event arrives in document space, derived from the screen
                // position through the *current* view. So the offset between
                // the grabbed point and the point now under the cursor is
                // exactly the correction that puts the grabbed point back.
                let delta = event.pos - anchor;
                if delta.length() > 0.0 {
                    self.dragged = true;
                }
                ctx.view.center -= delta;
            }
            ViewGesture::Zoom => {
                let dx = (ctx.view.screen_at(event.pos) - ctx.view.screen_at(anchor)).x;
                if dx.abs() > 0.5 {
                    self.dragged = true;
                }
                let target = (self.start_zoom * 2f32.powf(dx / ZOOM_DRAG_PX))
                    .clamp(ViewState::MIN_ZOOM, ViewState::MAX_ZOOM);
                let factor = target / ctx.view.zoom;
                ctx.view.zoom_about(anchor, factor);
            }
            ViewGesture::Rotate => {
                let now = Self::screen_angle(ctx, event.pos);
                let d = now - self.last_screen_angle;
                if d.is_finite() && d != 0.0 {
                    ctx.view.rotation += d;
                    self.dragged = true;
                }
                self.last_screen_angle = now;
            }
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let anchor = self.anchor_doc.take();
        // A zoom *click* (no drag) steps by a factor of two, in or out
        // depending on alt — the behaviour every editor's magnifier has.
        if self.gesture == ViewGesture::Zoom && !self.dragged {
            if let Some(a) = anchor {
                let factor = if event.modifiers.alt { 0.5 } else { 2.0 };
                ctx.view.zoom_about(a, factor);
            }
        }
        // Alt-clicking the rotate-view tool snaps the canvas back to upright.
        if self.gesture == ViewGesture::Rotate && !self.dragged && event.modifiers.alt {
            ctx.view.rotation = 0.0;
        }
        self.dragged = false;
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor_doc = None;
        self.dragged = false;
    }

    fn is_active(&self) -> bool {
        self.anchor_doc.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use raster::PixelRect;

    fn ctx(tiles: &mut MemoryTiles) -> ToolContext<'_> {
        ToolContext::new(tiles, PixelRect::new(0, 0, 512, 512))
    }

    #[test]
    fn navigation_tools_never_emit_a_command() {
        let mut tiles = MemoryTiles::new();
        let mut c = ctx(&mut tiles);
        for g in [ViewGesture::Pan, ViewGesture::Zoom, ViewGesture::Rotate] {
            let mut t = ViewTool::new(g);
            t.on_pointer_down(&mut c, PointerEvent::at(10.0, 10.0))
                .unwrap();
            t.on_pointer_move(&mut c, PointerEvent::at(60.0, 40.0))
                .unwrap();
            t.on_pointer_up(&mut c, PointerEvent::at(60.0, 40.0))
                .unwrap();
        }
        assert!(c.commands().is_empty());
        assert!(c.selection_edits().is_empty());
        assert!(c.requests().is_empty());
    }

    #[test]
    fn a_zoom_click_steps_in_and_alt_click_steps_out() {
        let mut tiles = MemoryTiles::new();
        let mut c = ctx(&mut tiles);
        let mut t = ViewTool::new(ViewGesture::Zoom);
        let before = c.view.zoom;
        t.on_pointer_down(&mut c, PointerEvent::at(100.0, 100.0))
            .unwrap();
        t.on_pointer_up(&mut c, PointerEvent::at(100.0, 100.0))
            .unwrap();
        assert!((c.view.zoom - before * 2.0).abs() < 1e-5);

        t.on_pointer_down(&mut c, PointerEvent::at(100.0, 100.0))
            .unwrap();
        t.on_pointer_up(
            &mut c,
            PointerEvent::at(100.0, 100.0).with_modifiers(crate::tool::Modifiers::alt()),
        )
        .unwrap();
        assert!((c.view.zoom - before).abs() < 1e-5);
    }

    #[test]
    fn panning_moves_the_view_centre_against_the_drag() {
        let mut tiles = MemoryTiles::new();
        let mut c = ctx(&mut tiles);
        c.view.center = glam::Vec2::new(100.0, 100.0);
        let mut t = ViewTool::new(ViewGesture::Pan);
        t.on_pointer_down(&mut c, PointerEvent::at(100.0, 100.0))
            .unwrap();
        t.on_pointer_move(&mut c, PointerEvent::at(130.0, 100.0))
            .unwrap();
        assert!(c.view.center.x < 100.0, "centre went the wrong way");
    }

    #[test]
    fn rotate_view_spins_and_alt_click_resets() {
        let mut tiles = MemoryTiles::new();
        let mut c = ctx(&mut tiles);
        c.view.center = glam::Vec2::ZERO;
        let mut t = ViewTool::new(ViewGesture::Rotate);
        t.on_pointer_down(&mut c, PointerEvent::at(100.0, 0.0))
            .unwrap();
        t.on_pointer_move(&mut c, PointerEvent::at(0.0, 100.0))
            .unwrap();
        assert!((c.view.rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
        t.on_pointer_up(&mut c, PointerEvent::at(0.0, 100.0))
            .unwrap();

        t.on_pointer_down(&mut c, PointerEvent::at(5.0, 5.0))
            .unwrap();
        t.on_pointer_up(
            &mut c,
            PointerEvent::at(5.0, 5.0).with_modifiers(crate::tool::Modifiers::alt()),
        )
        .unwrap();
        assert_eq!(c.view.rotation, 0.0);
    }

    #[test]
    fn cancelling_a_navigation_gesture_leaves_the_tool_reusable() {
        let mut tiles = MemoryTiles::new();
        let mut c = ctx(&mut tiles);
        let mut t = ViewTool::new(ViewGesture::Pan);
        t.on_pointer_down(&mut c, PointerEvent::at(1.0, 1.0))
            .unwrap();
        assert!(t.is_active());
        t.cancel(&mut c);
        assert!(!t.is_active());
        t.on_pointer_move(&mut c, PointerEvent::at(50.0, 50.0))
            .unwrap();
        assert!(c.commands().is_empty());
    }
}
