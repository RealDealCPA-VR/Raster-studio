//! W10-A: the Slice Select tool (Photoshop's, in the Crop slot).
//!
//! It edits the slice set the Slice tool committed — the one File ▸ Export ▸
//! Slices writes. The shell builds the tool over the active document's set at
//! every press ([`SliceSelectTool::with_rects`]), so what it edits is always
//! what the document holds, and every finished edit publishes the whole
//! edited set as one [`ToolRequest::Slices`] at pointer-up, which the shell
//! keeps in place of the old set:
//!
//! * a drag that starts inside a slice moves it;
//! * a drag that starts within [`HANDLE_REACH`] pixels of a slice's edge or
//!   corner resizes it from that edge or corner (a corner moves both edges);
//! * an Alt+click inside a slice deletes it.
//!
//! The top-most slice (the last drawn) wins where slices overlap. A slice is
//! kept inside the canvas and at least one pixel wide and tall. A slice set is
//! not document pixels, so none of this is a history entry — the same rule as
//! the Slice tool's.

use glam::Vec2;
use raster::PixelRect;

use crate::error::ToolError;
use crate::tool::{PointerEvent, Slice, Tool, ToolContext, ToolId, ToolRequest};

/// How close, in document pixels, a press must land to a slice's edge to
/// grab that edge rather than the slice's body.
pub const HANDLE_REACH: f32 = 4.0;

/// Which part of a slice a press grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grab {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Grab {
    /// The whole slice: every edge moves together.
    pub const BODY: Grab = Grab {
        left: true,
        right: true,
        top: true,
        bottom: true,
    };

    fn is_body(self) -> bool {
        self == Grab::BODY
    }
}

/// What a press started.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Gesture {
    /// Moving or resizing slice `index`, from `from`.
    Edit {
        index: usize,
        grab: Grab,
        from: Vec2,
        current: Vec2,
    },
    /// Deleting slice `index` when the button comes up.
    Delete { index: usize },
}

/// See the module docs.
#[derive(Debug, Default)]
pub struct SliceSelectTool {
    slices: Vec<Slice>,
    gesture: Option<Gesture>,
    /// The slice the last press picked, for the overlay.
    selected: Option<usize>,
}

impl SliceSelectTool {
    /// The tool over a document's committed slice set, in slice order.
    pub fn with_rects(rects: impl IntoIterator<Item = PixelRect>) -> Self {
        Self {
            slices: rects
                .into_iter()
                .enumerate()
                .map(|(i, rect)| Slice {
                    rect,
                    name: format!("slice_{:02}", i + 1),
                })
                .collect(),
            gesture: None,
            selected: None,
        }
    }

    /// The slice set as it stands, the gesture in flight not applied.
    pub fn slices(&self) -> &[Slice] {
        &self.slices
    }

    /// The slice the last press picked.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// What a press at `p` grabs, top-most slice first: an edge or corner
    /// within reach, else the body of a slice containing it.
    pub fn hit(&self, p: Vec2) -> Option<(usize, Grab)> {
        for (index, s) in self.slices.iter().enumerate().rev() {
            let (x0, y0) = (s.rect.x as f32, s.rect.y as f32);
            let (x1, y1) = (s.rect.right() as f32, s.rect.bottom() as f32);
            let inside_x = p.x >= x0 - HANDLE_REACH && p.x <= x1 + HANDLE_REACH;
            let inside_y = p.y >= y0 - HANDLE_REACH && p.y <= y1 + HANDLE_REACH;
            if !(inside_x && inside_y) {
                continue;
            }
            let grab = Grab {
                left: (p.x - x0).abs() <= HANDLE_REACH,
                right: (p.x - x1).abs() <= HANDLE_REACH,
                top: (p.y - y0).abs() <= HANDLE_REACH,
                bottom: (p.y - y1).abs() <= HANDLE_REACH,
            };
            // A slice narrower than two reaches is grabbed by its nearer edge.
            let grab = Grab {
                left: grab.left && (!grab.right || (p.x - x0).abs() <= (p.x - x1).abs()),
                right: grab.right && (!grab.left || (p.x - x1).abs() < (p.x - x0).abs()),
                top: grab.top && (!grab.bottom || (p.y - y0).abs() <= (p.y - y1).abs()),
                bottom: grab.bottom && (!grab.top || (p.y - y1).abs() < (p.y - y0).abs()),
            };
            if grab.left || grab.right || grab.top || grab.bottom {
                return Some((index, grab));
            }
            if p.x >= x0 && p.x < x1 && p.y >= y0 && p.y < y1 {
                return Some((index, Grab::BODY));
            }
        }
        None
    }

    /// `rect` with the grabbed edges moved by `delta` (whole pixels), kept
    /// inside `canvas` and at least one pixel on each side.
    pub fn edited(rect: PixelRect, grab: Grab, delta: Vec2, canvas: PixelRect) -> PixelRect {
        let (dx, dy) = (delta.x.round() as i64, delta.y.round() as i64);
        let (mut x0, mut y0, mut x1, mut y1) = (rect.x, rect.y, rect.right(), rect.bottom());
        if grab.is_body() {
            // A move keeps the size and stops at the canvas edge.
            let w = x1 - x0;
            let h = y1 - y0;
            let nx = (x0 + dx).clamp(canvas.x, (canvas.right() - w).max(canvas.x));
            let ny = (y0 + dy).clamp(canvas.y, (canvas.bottom() - h).max(canvas.y));
            return PixelRect::new(nx, ny, w as u32, h as u32);
        }
        if grab.left {
            x0 = (x0 + dx).clamp(canvas.x, x1 - 1);
        }
        if grab.right {
            x1 = (x1 + dx).clamp(x0 + 1, canvas.right());
        }
        if grab.top {
            y0 = (y0 + dy).clamp(canvas.y, y1 - 1);
        }
        if grab.bottom {
            y1 = (y1 + dy).clamp(y0 + 1, canvas.bottom());
        }
        PixelRect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32)
    }

    fn publish(&self, ctx: &mut ToolContext<'_>) {
        ctx.emit_request(ToolRequest::Slices(self.slices.clone()));
    }
}

impl Tool for SliceSelectTool {
    fn id(&self) -> ToolId {
        ToolId::SliceSelect
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("slice select point", event.pos)?;
        self.gesture = None;
        let Some((index, grab)) = self.hit(event.pos) else {
            self.selected = None;
            return Ok(());
        };
        self.selected = Some(index);
        self.gesture = Some(if event.modifiers.alt && grab.is_body() {
            Gesture::Delete { index }
        } else {
            Gesture::Edit {
                index,
                grab,
                from: event.pos,
                current: event.pos,
            }
        });
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let Some(Gesture::Edit { current, .. }) = &mut self.gesture {
            if event.pos.is_finite() {
                *current = event.pos;
            }
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        match self.gesture.take() {
            None => Ok(()),
            Some(Gesture::Delete { index }) => {
                if index < self.slices.len() {
                    self.slices.remove(index);
                    for (i, s) in self.slices.iter_mut().enumerate() {
                        s.name = format!("slice_{:02}", i + 1);
                    }
                    self.selected = None;
                    self.publish(ctx);
                }
                Ok(())
            }
            Some(Gesture::Edit {
                index, grab, from, ..
            }) => {
                crate::error::finite_pt("slice select point", event.pos)?;
                let Some(slice) = self.slices.get(index) else {
                    return Ok(());
                };
                let rect = Self::edited(slice.rect, grab, event.pos - from, ctx.canvas);
                if rect != slice.rect {
                    self.slices[index].rect = rect;
                    self.publish(ctx);
                }
                Ok(())
            }
        }
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.gesture = None;
    }

    /// Every slice of the set, the one being dragged where the pointer has
    /// it, numbered in slice order.
    fn live_geometry(&self) -> Option<crate::tool::SessionGeometry> {
        if self.slices.is_empty() {
            return None;
        }
        let rects: Vec<[Vec2; 2]> = self
            .slices
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let r = match self.gesture {
                    Some(Gesture::Edit {
                        index,
                        grab,
                        from,
                        current,
                    }) if index == i => {
                        // No canvas here; the release clamps. The preview
                        // only needs to follow the pointer.
                        let open = PixelRect::new(
                            -i64::from(i32::MAX),
                            -i64::from(i32::MAX),
                            u32::MAX,
                            u32::MAX,
                        );
                        Self::edited(s.rect, grab, current - from, open)
                    }
                    Some(Gesture::Delete { index }) if index == i => return None,
                    _ => s.rect,
                };
                Some([
                    Vec2::new(r.x as f32, r.y as f32),
                    Vec2::new(r.right() as f32, r.bottom() as f32),
                ])
            })
            .collect();
        Some(crate::tool::SessionGeometry::Slices { rects })
    }

    fn is_active(&self) -> bool {
        self.gesture.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;

    fn canvas() -> PixelRect {
        PixelRect::new(0, 0, 100, 100)
    }

    fn gesture(tool: &mut SliceSelectTool, from: Vec2, to: Vec2, alt: bool) -> Vec<ToolRequest> {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, canvas());
        let m = if alt {
            crate::tool::Modifiers::alt()
        } else {
            crate::tool::Modifiers::NONE
        };
        tool.on_pointer_down(&mut ctx, PointerEvent::at(from.x, from.y).with_modifiers(m))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(to.x, to.y)).unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(to.x, to.y)).unwrap();
        ctx.drain_requests()
    }

    fn rects(reqs: &[ToolRequest]) -> Vec<PixelRect> {
        match reqs {
            [ToolRequest::Slices(s)] => s.iter().map(|s| s.rect).collect(),
            other => panic!("expected one slice set: {other:?}"),
        }
    }

    fn two() -> SliceSelectTool {
        SliceSelectTool::with_rects([PixelRect::new(10, 10, 20, 20), PixelRect::new(50, 50, 20, 20)])
    }

    #[test]
    fn a_body_drag_moves_the_slice_and_publishes_the_whole_set() {
        let mut t = two();
        let out = gesture(&mut t, Vec2::new(60.0, 60.0), Vec2::new(70.0, 55.0), false);
        assert_eq!(
            rects(&out),
            vec![PixelRect::new(10, 10, 20, 20), PixelRect::new(60, 45, 20, 20)]
        );
        assert_eq!(t.selected(), Some(1));
    }

    #[test]
    fn an_edge_drag_resizes_and_a_corner_drag_moves_two_edges() {
        let mut t = two();
        // The right edge of slice 1 is x = 30.
        let out = gesture(&mut t, Vec2::new(30.0, 20.0), Vec2::new(40.0, 20.0), false);
        assert_eq!(rects(&out)[0], PixelRect::new(10, 10, 30, 20));
        // Its top-left corner.
        let out = gesture(&mut t, Vec2::new(10.0, 10.0), Vec2::new(5.0, 2.0), false);
        assert_eq!(rects(&out)[0], PixelRect::new(5, 2, 35, 28));
    }

    #[test]
    fn a_move_stops_at_the_canvas_and_a_resize_keeps_a_pixel() {
        let mut t = two();
        let out = gesture(&mut t, Vec2::new(60.0, 60.0), Vec2::new(200.0, 60.0), false);
        assert_eq!(rects(&out)[1], PixelRect::new(80, 50, 20, 20));
        let mut t = two();
        let out = gesture(&mut t, Vec2::new(30.0, 20.0), Vec2::new(-50.0, 20.0), false);
        assert_eq!(rects(&out)[0], PixelRect::new(10, 10, 1, 20));
    }

    #[test]
    fn alt_click_deletes_and_a_miss_publishes_nothing() {
        let mut t = two();
        let out = gesture(&mut t, Vec2::new(20.0, 20.0), Vec2::new(20.0, 20.0), true);
        assert_eq!(rects(&out), vec![PixelRect::new(50, 50, 20, 20)]);
        assert_eq!(t.slices()[0].name, "slice_01");
        let out = gesture(&mut t, Vec2::new(5.0, 95.0), Vec2::new(30.0, 95.0), false);
        assert!(out.is_empty(), "{out:?}");
        // A click that does not move publishes nothing either.
        let out = gesture(&mut t, Vec2::new(60.0, 60.0), Vec2::new(60.0, 60.0), false);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn the_overlay_follows_the_drag_and_hides_a_slice_being_deleted() {
        let mut t = two();
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, canvas());
        t.on_pointer_down(&mut ctx, PointerEvent::at(60.0, 60.0)).unwrap();
        t.on_pointer_move(&mut ctx, PointerEvent::at(65.0, 60.0)).unwrap();
        let Some(crate::tool::SessionGeometry::Slices { rects }) = t.live_geometry() else {
            panic!("no overlay");
        };
        assert_eq!(rects[1], [Vec2::new(55.0, 50.0), Vec2::new(75.0, 70.0)]);
        t.cancel(&mut ctx);
        t.on_pointer_down(
            &mut ctx,
            PointerEvent::at(20.0, 20.0).with_modifiers(crate::tool::Modifiers::alt()),
        )
        .unwrap();
        let Some(crate::tool::SessionGeometry::Slices { rects }) = t.live_geometry() else {
            panic!("no overlay");
        };
        assert_eq!(rects.len(), 1);
    }
}
