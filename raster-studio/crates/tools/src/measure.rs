//! The two measuring tools in the Eyedropper slot: the Ruler and the Colour
//! Sampler.
//!
//! Neither edits pixels. The Ruler holds one measurement — a line from the
//! press to the release — and its one edit is **Straighten Layer**
//! ([`Tool::commit`], Enter): the active layer is rotated about the line's
//! midpoint by the angle that lays the line on the nearest axis, as ONE
//! [`Command::TransformLayer`], so one Ctrl+Z takes it back. The Colour
//! Sampler holds up to [`MAX_SAMPLERS`] persistent sample points.
//!
//! The Ruler keeps its line on the tool instance, which the shell keeps alive
//! for as long as the tool stays selected — the measurement survives its own
//! pointer-up, exactly as Photoshop's ruler line does — and publishes it
//! through [`Tool::live_geometry`] ([`SessionGeometry::Measure`]): the shell
//! draws the line over the canvas and fills the Info panel's Distance and
//! Angle rows from the same value. The options bar's **Straighten Layer**
//! button and Enter both reach [`Tool::commit`].
//!
//! Sample points are the *document's*, as in Photoshop: they outlive the tool
//! (switch to the Brush and back and they are still there, still read in the
//! Info panel and still marked on the canvas). The shell lends them to the
//! tool as [`ToolContext::samplers`]; the tool edits them in place and keeps
//! only which one is being dragged.

use editor_core::Command;
use glam::{Affine2, Vec2};

use crate::error::ToolError;
use crate::tool::{LiveReadout, PointerEvent, SessionGeometry, Tool, ToolContext, ToolId};

/// How many sample points the Colour Sampler keeps — Photoshop's limit.
pub const MAX_SAMPLERS: usize = 4;

/// A press closer than this to an existing sampler (document pixels) grabs it
/// rather than placing a new one.
pub const SAMPLER_GRAB_RADIUS: f32 = 6.0;

/// A drag shorter than this (document pixels) is a click, and a click with the
/// Ruler clears the measurement.
const MIN_RULER_LENGTH: f32 = 0.5;

/// One ruler measurement: a line in document pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    pub start: Vec2,
    pub end: Vec2,
}

impl Measurement {
    /// The line as a vector, start to end.
    pub fn delta(&self) -> Vec2 {
        self.end - self.start
    }

    /// Length of the line, in document pixels.
    pub fn distance(&self) -> f32 {
        self.delta().length()
    }

    /// The Info panel's angle: degrees counter-clockwise from the positive x
    /// axis as the user sees it (the document is y-down, so a line drawn up
    /// and to the right reads positive), in `(-180, 180]`.
    pub fn angle_degrees(&self) -> f32 {
        let d = self.delta();
        if d.length_squared() == 0.0 {
            return 0.0;
        }
        let a = (-d.y).atan2(d.x).to_degrees();
        if a <= -180.0 {
            a + 360.0
        } else {
            a
        }
    }

    /// The rotation (radians, glam's convention: positive turns +x toward +y,
    /// clockwise on a y-down screen) that lays this line on the NEAREST axis —
    /// horizontal for a shallow line, vertical for a steep one. Always within
    /// `[-π/4, π/4]`, so straightening never turns an image on its side.
    pub fn straighten_radians(&self) -> f32 {
        let d = self.delta();
        if d.length_squared() == 0.0 {
            return 0.0;
        }
        let quarter = std::f32::consts::FRAC_PI_2;
        let screen = d.y.atan2(d.x);
        let off_axis = screen - (screen / quarter).round() * quarter;
        -off_axis
    }

    /// The document-space affine that performs [`Self::straighten_radians`]
    /// about the line's midpoint.
    pub fn straighten_affine(&self) -> Affine2 {
        let pivot = (self.start + self.end) * 0.5;
        Affine2::from_translation(pivot)
            * Affine2::from_angle(self.straighten_radians())
            * Affine2::from_translation(-pivot)
    }
}

/// Constrain `end` so the line from `start` sits on a multiple of 45° —
/// Shift-drag, as in every editor.
fn snap_to_45(start: Vec2, end: Vec2) -> Vec2 {
    let d = end - start;
    let len = d.length();
    if len == 0.0 {
        return end;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let a = (d.y.atan2(d.x) / step).round() * step;
    start + Vec2::new(a.cos(), a.sin()) * len
}

/// The Ruler: drag to measure; Enter straightens the active layer along the
/// line; a click clears it.
#[derive(Default)]
pub struct RulerTool {
    measurement: Option<Measurement>,
    dragging: bool,
}

impl RulerTool {
    /// The current measurement, while there is one — held after release until
    /// the next drag, a click, a straighten or Escape.
    pub fn measurement(&self) -> Option<Measurement> {
        self.measurement
    }

    fn track(&mut self, event: PointerEvent) {
        if !event.pos.is_finite() {
            return;
        }
        if let Some(m) = &mut self.measurement {
            m.end = if event.modifiers.shift {
                snap_to_45(m.start, event.pos)
            } else {
                event.pos
            };
        }
    }
}

impl Tool for RulerTool {
    fn id(&self) -> ToolId {
        ToolId::Ruler
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if !event.pos.is_finite() {
            return Err(ToolError::NotFinite {
                what: "ruler point",
                value: if event.pos.x.is_finite() {
                    event.pos.y
                } else {
                    event.pos.x
                },
            });
        }
        self.measurement = Some(Measurement {
            start: event.pos,
            end: event.pos,
        });
        self.dragging = true;
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.dragging {
            self.track(event);
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if !self.dragging {
            return Ok(());
        }
        self.track(event);
        self.dragging = false;
        if self
            .measurement
            .is_some_and(|m| m.distance() < MIN_RULER_LENGTH)
        {
            // A click: Photoshop's "Clear".
            self.measurement = None;
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.dragging = false;
        self.measurement = None;
    }

    /// Straighten Layer: rotate the active layer about the line's midpoint so
    /// the line lands on the nearest axis — ONE [`Command::TransformLayer`].
    /// The delta is computed in document space and conjugated through the
    /// layer's parent chain (card 035's rule), since the command pre-multiplies
    /// in the layer's parent space. The measurement is consumed either way: the
    /// line no longer describes the rotated image.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let Some(m) = self.measurement.take() else {
            return Ok(());
        };
        self.dragging = false;
        let layer = ctx.active_layer.ok_or(ToolError::NoActiveLayer)?;
        if m.straighten_radians().abs() < 1e-6 {
            return Ok(());
        }
        let parent = ctx
            .active_layer_parent_transform
            .unwrap_or(Affine2::IDENTITY);
        let delta = parent.inverse() * m.straighten_affine() * parent;
        ctx.emit(Command::TransformLayer {
            layer_id: layer,
            matrix: delta.to_cols_array(),
        });
        Ok(())
    }

    fn has_pending_commit(&self) -> bool {
        !self.dragging && self.measurement.is_some()
    }

    fn is_active(&self) -> bool {
        self.dragging
    }

    /// The measurement, for as long as it is held: the canvas line and the
    /// Info panel's Distance and Angle rows.
    fn live_geometry(&self) -> Option<SessionGeometry> {
        let m = self.measurement?;
        Some(SessionGeometry::Measure {
            start: m.start,
            end: m.end,
        })
    }

    /// The line's horizontal and vertical extent beside the pointer while the
    /// drag runs — the shell's existing W/H label route.
    fn live_readout(&self) -> Option<LiveReadout> {
        if !self.dragging {
            return None;
        }
        let m = self.measurement?;
        let d = m.delta();
        Some(LiveReadout {
            width_px: d.x.abs(),
            height_px: d.y.abs(),
            anchor: m.end,
        })
    }
}

/// The Colour Sampler: click to place up to [`MAX_SAMPLERS`] sample points on
/// the document, drag one to move it, Alt-click one to delete it. The points
/// live in [`ToolContext::samplers`]; see the module docs.
#[derive(Default)]
pub struct ColorSamplerTool {
    /// The sampler being dragged, by index.
    grabbed: Option<usize>,
}

/// The sampler within [`SAMPLER_GRAB_RADIUS`] of `p`, nearest first.
fn nearest(samples: &[Vec2], p: Vec2) -> Option<usize> {
    samples
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.distance(p)))
        .filter(|(_, d)| *d <= SAMPLER_GRAB_RADIUS)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// A sample point sits on the pixel it names: the centre of that pixel.
fn pixel_centre(p: Vec2) -> Vec2 {
    p.floor() + Vec2::splat(0.5)
}

impl Tool for ColorSamplerTool {
    fn id(&self) -> ToolId {
        ToolId::ColorSampler
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if !event.pos.is_finite() {
            return Ok(());
        }
        let c = ctx.canvas;
        // No document to keep them on: nothing to place.
        let Some(samples) = ctx.samplers.as_deref_mut() else {
            return Err(ToolError::NotStarted);
        };
        if let Some(i) = nearest(samples, event.pos) {
            if event.modifiers.alt {
                samples.remove(i);
            } else {
                self.grabbed = Some(i);
            }
            return Ok(());
        }
        if event.modifiers.alt {
            return Ok(());
        }
        let p = pixel_centre(event.pos);
        let inside = p.x >= c.x as f32
            && p.y >= c.y as f32
            && p.x < c.right() as f32
            && p.y < c.bottom() as f32;
        if !inside || samples.len() >= MAX_SAMPLERS {
            return Ok(());
        }
        samples.push(p);
        self.grabbed = Some(samples.len() - 1);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let (Some(i), Some(samples)) = (self.grabbed, ctx.samplers.as_deref_mut()) {
            if let (Some(slot), true) = (samples.get_mut(i), event.pos.is_finite()) {
                *slot = pixel_centre(event.pos);
            }
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.on_pointer_move(ctx, event)?;
        self.grabbed = None;
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.grabbed = None;
    }

    fn is_active(&self) -> bool {
        self.grabbed.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use crate::tool::Modifiers;
    use layer_model::LayerId;
    use raster::PixelRect;

    fn drag(tool: &mut dyn Tool, ctx: &mut ToolContext<'_>, a: Vec2, b: Vec2) {
        tool.on_pointer_down(ctx, PointerEvent::at(a.x, a.y))
            .unwrap();
        tool.on_pointer_move(ctx, PointerEvent::at(b.x, b.y))
            .unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(b.x, b.y)).unwrap();
    }

    #[test]
    fn the_ruler_measures_distance_and_angle_and_keeps_them_after_release() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 200, 200));
        let mut ruler = RulerTool::default();
        drag(
            &mut ruler,
            &mut ctx,
            Vec2::new(10.0, 50.0),
            Vec2::new(40.0, 10.0),
        );
        let m = ruler.measurement().expect("held after release");
        assert!((m.distance() - 50.0).abs() < 1e-4, "{}", m.distance());
        // Up and to the right reads positive on a y-down document.
        let expected = (40.0f32).atan2(30.0).to_degrees();
        assert!((m.angle_degrees() - expected).abs() < 1e-3);
        assert!(ruler.has_pending_commit());
        assert!(!ruler.is_active());
        assert!(ctx.commands().is_empty(), "measuring edits nothing");
        assert_eq!(ruler.live_readout(), None, "the label dies with the drag");
    }

    #[test]
    fn the_ruler_publishes_its_extent_while_dragging() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 200, 200));
        let mut ruler = RulerTool::default();
        ruler
            .on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        ruler
            .on_pointer_move(&mut ctx, PointerEvent::at(40.0, 50.0))
            .unwrap();
        let r = ruler.live_readout().expect("dragging");
        assert_eq!((r.width_px, r.height_px), (30.0, 40.0));
        assert_eq!(r.anchor, Vec2::new(40.0, 50.0));
    }

    #[test]
    fn a_ruler_click_clears_the_measurement() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 200, 200));
        let mut ruler = RulerTool::default();
        drag(
            &mut ruler,
            &mut ctx,
            Vec2::new(10.0, 10.0),
            Vec2::new(90.0, 20.0),
        );
        assert!(ruler.measurement().is_some());
        drag(
            &mut ruler,
            &mut ctx,
            Vec2::new(5.0, 5.0),
            Vec2::new(5.0, 5.0),
        );
        assert_eq!(ruler.measurement(), None);
        assert!(!ruler.has_pending_commit());
    }

    #[test]
    fn shift_constrains_the_ruler_to_45_degrees() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 200, 200));
        let mut ruler = RulerTool::default();
        ruler
            .on_pointer_down(&mut ctx, PointerEvent::at(0.0, 0.0))
            .unwrap();
        let e = PointerEvent::at(100.0, 8.0).with_modifiers(Modifiers::shift());
        ruler.on_pointer_up(&mut ctx, e).unwrap();
        let m = ruler.measurement().unwrap();
        assert!(m.angle_degrees().abs() < 1e-3, "{}", m.angle_degrees());
    }

    #[test]
    fn straightening_lays_the_line_on_the_nearest_axis_as_one_command() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        let mut ctx =
            ToolContext::new(&mut tiles, PixelRect::new(0, 0, 200, 200)).with_layer(layer);
        let mut ruler = RulerTool::default();
        let (a, b) = (Vec2::new(20.0, 100.0), Vec2::new(120.0, 80.0));
        drag(&mut ruler, &mut ctx, a, b);
        ruler.commit(&mut ctx).unwrap();
        let cmds = ctx.drain();
        let [Command::TransformLayer { layer_id, matrix }] = &cmds[..] else {
            panic!("expected one TransformLayer: {cmds:?}");
        };
        assert_eq!(*layer_id, layer);
        let t = Affine2::from_cols_array(matrix);
        let (a2, b2) = (t.transform_point2(a), t.transform_point2(b));
        assert!((a2.y - b2.y).abs() < 1e-3, "not level: {a2:?} {b2:?}");
        assert!(
            ((b2 - a2).length() - (b - a).length()).abs() < 1e-3,
            "rigid"
        );
        assert!(!ruler.has_pending_commit(), "the line is consumed");

        // A steep line straightens to vertical, not onto its side.
        let steep = Measurement {
            start: Vec2::new(50.0, 10.0),
            end: Vec2::new(60.0, 110.0),
        };
        let t = steep.straighten_affine();
        let (s, e) = (
            t.transform_point2(steep.start),
            t.transform_point2(steep.end),
        );
        assert!((s.x - e.x).abs() < 1e-3, "{s:?} {e:?}");
        assert!(steep.straighten_radians().abs() <= std::f32::consts::FRAC_PI_4 + 1e-6);
    }

    #[test]
    fn straightening_with_no_layer_refuses_and_emits_nothing() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 200, 200));
        let mut ruler = RulerTool::default();
        drag(
            &mut ruler,
            &mut ctx,
            Vec2::new(0.0, 0.0),
            Vec2::new(50.0, 10.0),
        );
        assert!(matches!(
            ruler.commit(&mut ctx),
            Err(ToolError::NoActiveLayer)
        ));
        assert!(ctx.commands().is_empty());
    }

    fn held(ctx: &ToolContext<'_>) -> Vec<Vec2> {
        ctx.samplers.as_deref().cloned().unwrap_or_default()
    }

    #[test]
    fn the_sampler_keeps_up_to_four_points_and_alt_click_removes_one() {
        let mut tiles = MemoryTiles::new();
        let mut points = Vec::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 100, 100));
        ctx.samplers = Some(&mut points);
        let mut s = ColorSamplerTool::default();
        for i in 0..6 {
            let p = Vec2::new(10.0 + 15.0 * i as f32, 20.3);
            drag(&mut s, &mut ctx, p, p);
        }
        assert_eq!(held(&ctx).len(), MAX_SAMPLERS);
        assert_eq!(held(&ctx)[0], Vec2::new(10.5, 20.5), "pixel centres");
        // Drag the second one somewhere else.
        drag(
            &mut s,
            &mut ctx,
            Vec2::new(25.5, 20.5),
            Vec2::new(70.2, 80.9),
        );
        assert_eq!(held(&ctx)[1], Vec2::new(70.5, 80.5));
        // Alt-click the first one away.
        let at = PointerEvent::at(10.5, 20.5).with_modifiers(Modifiers::alt());
        s.on_pointer_down(&mut ctx, at).unwrap();
        s.on_pointer_up(&mut ctx, at).unwrap();
        assert_eq!(held(&ctx).len(), MAX_SAMPLERS - 1);
        assert!(ctx.commands().is_empty(), "sampling edits nothing");
        // Off the canvas places nothing.
        drag(&mut s, &mut ctx, Vec2::new(-5.0, 5.0), Vec2::new(-5.0, 5.0));
        assert_eq!(held(&ctx).len(), MAX_SAMPLERS - 1);
        drop(ctx);
        assert_eq!(
            points.len(),
            MAX_SAMPLERS - 1,
            "the points are the lender's"
        );
    }

    /// The points belong to whoever lent them, not to the tool instance: a
    /// fresh tool (what a tool switch builds) sees and edits the same ones.
    #[test]
    fn sample_points_outlive_the_tool_instance() {
        let mut tiles = MemoryTiles::new();
        let mut points = Vec::new();
        {
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 100, 100));
            ctx.samplers = Some(&mut points);
            let p = Vec2::new(30.0, 30.0);
            drag(&mut ColorSamplerTool::default(), &mut ctx, p, p);
        }
        assert_eq!(points, vec![Vec2::new(30.5, 30.5)]);
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 100, 100));
        ctx.samplers = Some(&mut points);
        let mut again = ColorSamplerTool::default();
        drag(
            &mut again,
            &mut ctx,
            Vec2::new(30.5, 30.5),
            Vec2::new(60.0, 60.0),
        );
        drop(ctx);
        assert_eq!(
            points,
            vec![Vec2::new(60.5, 60.5)],
            "grabbed, not re-placed"
        );
    }

    #[test]
    fn with_nowhere_to_keep_points_the_sampler_refuses() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 100, 100));
        let mut s = ColorSamplerTool::default();
        assert!(s
            .on_pointer_down(&mut ctx, PointerEvent::at(5.0, 5.0))
            .is_err());
        assert!(!s.is_active());
    }
}
