//! The Pen tool: click for a corner, drag for a curve.
//!
//! # Why this exists
//!
//! `P` was the one letter of the brief the registry could not answer, and the
//! whole of `crates/vector` — paths, hit testing, stroking, booleans, SVG —
//! was reachable only through the seven shape tools, each of which draws one
//! *fixed* outline from a drag. There was no way for a user to author an
//! arbitrary path at all.
//!
//! # The shape of a pen gesture
//!
//! Nothing is emitted while the path is being drawn. The first press starts a
//! subpath and each later one adds an anchor; the path becomes a layer (or
//! pixels) at exactly two moments: releasing a press on the first anchor
//! (which closes it) or confirming with [`Tool::commit`] (Enter), which
//! leaves it open. That is the same "hold the gesture, publish on commit"
//! shape [`crate::edit::CropTool`] keeps, and for the same reason — emitting
//! a `CreateLayer` per anchor would cost one undo step and one layer row each.
//!
//! # Corners and curves
//!
//! A click makes a **corner** anchor: no handles, straight segments either
//! side. Dragging out of the press makes a **smooth** anchor: the drag is the
//! outgoing handle and the incoming handle mirrors it, so the curve passes
//! through the anchor with one tangent. Holding **Alt** while dragging breaks
//! the pair — the outgoing handle follows the pointer and the incoming one
//! keeps whatever it had — which is how a cusp is drawn. A drag shorter than
//! [`DRAG_THRESHOLD_PX`] is a click.
//!
//! Two anchors are joined by a straight line when neither has a handle on
//! that side and by a cubic Bezier otherwise ([`Anchor`] holds the handles as
//! offsets, so a zero offset *is* "no handle"). Pressing on the first anchor
//! closes the path; dragging out of that press shapes the closing segment.
//!
//! Anchors of a finished path are added, deleted and converted through the
//! Direct Selection tool's modifier clicks (`crate::path_select`), which edit
//! the layer the path lives on.
//!
//! # Modes, paint and combine
//!
//! * **Path** publishes a bare shape layer — the path with neither fill nor
//!   stroke, which the compositor draws as nothing and Path Select / Direct
//!   Selection can grab. **Shape** publishes a painted shape layer. **Pixels**
//!   paints the path into the active layer's pixels on commit.
//! * Paint is a [`ShapePaint`], shared with the shape tools. An open path
//!   encloses nothing, so it is never filled: it is stroked with the stroke
//!   paint, or — when no stroke is set — in the fill's colour, so an open
//!   path with the default options is visible rather than an invisible layer.
//! * **Combine** applies to a *closed* path in Path or Shape mode when the
//!   active layer is a shape layer: Add, Subtract and Intersect rewrite that
//!   layer's path as the boolean of the two (one `SetLayerKind` step) instead
//!   of creating a layer. New, an open path, or no active shape layer all
//!   create a layer. `vector::boolean` supports all three ops, so none of the
//!   four choices is a dead control.

use glam::Vec2;
use layer_model::{Layer, LayerKind, ShapeStroke};
use vector::{svg, BoolOp, FillRule, Path, Point};

use editor_core::Command;

use crate::error::ToolError;
use crate::shape::{rasterize_painted, PaintSource, ShapePaint};
use crate::tool::{PointerEvent, Tool, ToolContext, ToolId, ToolSetting};

/// How near the first anchor a press has to land, in document pixels, to
/// close the path rather than add another anchor.
///
/// A fixed document distance rather than a screen one: the tool is handed
/// document coordinates and has no camera, so a zoom-aware radius would be a
/// number it cannot compute. At any usable zoom six pixels is a comfortable
/// target and is far below the distance between two anchors a user meant to
/// place apart.
pub const CLOSE_RADIUS_PX: f32 = 6.0;

/// A drag shorter than this, in document pixels, is a click: the anchor stays
/// a corner rather than growing a handle the size of a hand tremor.
pub const DRAG_THRESHOLD_PX: f32 = 2.0;

/// The fewest anchors a path needs before it is worth a layer: two for an open
/// path (a line), three to close (a triangle).
pub const MIN_OPEN_ANCHORS: usize = 2;
pub const MIN_CLOSED_ANCHORS: usize = 3;

/// The most anchors one path may hold, so a stuck auto-clicker cannot grow an
/// unbounded document.
pub const MAX_ANCHORS: usize = 10_000;

/// One anchor of the path being drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// The point the path passes through, in document pixels.
    pub pos: Vec2,
    /// The incoming control handle as an offset from `pos`; zero means the
    /// segment arriving here is straight on this side.
    pub handle_in: Vec2,
    /// The outgoing control handle as an offset from `pos`; zero means the
    /// segment leaving here is straight on this side.
    pub handle_out: Vec2,
}

impl Anchor {
    /// A corner: no handles.
    pub fn corner(pos: Vec2) -> Self {
        Self {
            pos,
            handle_in: Vec2::ZERO,
            handle_out: Vec2::ZERO,
        }
    }

    /// `true` when at least one handle is non-zero.
    pub fn is_smooth(&self) -> bool {
        self.handle_in != Vec2::ZERO || self.handle_out != Vec2::ZERO
    }
}

/// What a finished pen path becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PenMode {
    /// A shape layer with no paint: the path alone, for later use.
    Path,
    /// A painted shape layer.
    #[default]
    Shape,
    /// Paint into the active layer's pixels.
    Pixels,
}

impl PenMode {
    /// The `Choice` labels, in [`Self::from_choice`]'s order.
    pub const CHOICES: &'static [&'static str] = &["Path", "Shape", "Pixels"];

    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => PenMode::Path,
            1 => PenMode::Shape,
            _ => PenMode::Pixels,
        }
    }
}

/// How a closed pen path combines with the active shape layer's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Combine {
    /// A new layer.
    #[default]
    New,
    Add,
    Subtract,
    Intersect,
}

impl Combine {
    /// The `Choice` labels, in [`Self::from_choice`]'s order.
    pub const CHOICES: &'static [&'static str] = &["New", "Add", "Subtract", "Intersect"];

    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => Combine::New,
            1 => Combine::Add,
            2 => Combine::Subtract,
            _ => Combine::Intersect,
        }
    }

    fn op(self) -> Option<BoolOp> {
        match self {
            Combine::New => None,
            Combine::Add => Some(BoolOp::Union),
            Combine::Subtract => Some(BoolOp::Difference),
            Combine::Intersect => Some(BoolOp::Intersection),
        }
    }
}

/// Which anchor a live drag is shaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dragging {
    /// The anchor just placed (the last one).
    Last,
    /// The first anchor, pressed to close the path.
    Closing,
}

/// Pen: author a path one anchor at a time.
#[derive(Default)]
pub struct PenTool {
    anchors: Vec<Anchor>,
    closed: bool,
    dragging: Option<Dragging>,
    pub mode: PenMode,
    pub paint: ShapePaint,
    pub combine: Combine,
}

impl PenTool {
    /// The anchors placed so far, in document pixels.
    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    /// `true` once the path has been closed by pressing its first anchor.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The path as it stands. Empty until the first press.
    pub fn path(&self) -> Path {
        anchors_to_path(&self.anchors, self.closed)
    }

    /// Whether the path has enough anchors to become a layer.
    fn publishable(&self) -> bool {
        let needed = if self.closed {
            MIN_CLOSED_ANCHORS
        } else {
            MIN_OPEN_ANCHORS
        };
        self.anchors.len() >= needed
    }

    fn reset(&mut self) {
        self.anchors.clear();
        self.closed = false;
        self.dragging = None;
    }

    /// The layer paint for a finished path: a closed path takes the paint
    /// as set; an open path encloses nothing, so it is unfilled and stroked —
    /// with the stroke paint, or in the fill's colour when no stroke is set.
    fn layer_for(
        &self,
        path: &Path,
        closed: bool,
        foreground: [f32; 4],
    ) -> layer_model::ShapeLayer {
        let mut shape = self.paint.layer(path, foreground);
        if self.mode == PenMode::Path {
            shape.fill = None;
            shape.stroke = None;
            return shape;
        }
        if !closed {
            if shape.stroke.is_none() {
                shape.stroke = shape.fill.map(|color| ShapeStroke {
                    color,
                    width_px: self.paint.stroke_width.max(1.0),
                    ..ShapeStroke::default()
                });
            }
            shape.fill = None;
        }
        shape
    }

    /// Emit what this path describes and start a fresh path.
    ///
    /// Reports whether anything was published: a path of one anchor is a
    /// press the user has not finished, not an edit.
    fn publish(&mut self, ctx: &mut ToolContext<'_>) -> Result<bool, ToolError> {
        if !self.publishable() {
            self.reset();
            return Ok(false);
        }
        let path = self.path();
        let closed = self.closed;
        self.reset();

        if self.mode == PenMode::Pixels {
            let mut paint = self.paint;
            if !closed {
                // An open path is its stroke; with no stroke set it is drawn
                // in the fill's colour, as the layer form is.
                if paint.stroke == PaintSource::None || paint.stroke_width <= 0.0 {
                    paint.stroke = paint.fill;
                    paint.stroke_color = paint.fill_color;
                    paint.stroke_width = paint.stroke_width.max(1.0);
                }
                paint.fill = PaintSource::None;
            }
            rasterize_painted(ctx, &path, &paint)?;
            return Ok(true);
        }

        if closed {
            if let Some(op) = self.combine.op() {
                if let Some((layer_id, base)) = active_shape(ctx) {
                    let Ok(existing) = svg::parse(&base.path_svg) else {
                        return Err(ToolError::Degenerate);
                    };
                    let combined = vector::boolean::boolean(
                        &existing,
                        &path,
                        op,
                        FillRule::NonZero,
                        vector::DEFAULT_TOLERANCE,
                    )?;
                    if combined.is_empty() {
                        // Nothing survives (Intersect of disjoint shapes, or
                        // Subtract of a cover): refused rather than leaving an
                        // empty path behind on the layer.
                        return Err(ToolError::Degenerate);
                    }
                    let mut shape = base;
                    shape.path_svg = svg::to_svg(&combined);
                    ctx.emit(Command::SetLayerKind {
                        layer_id,
                        kind: Box::new(LayerKind::Shape(shape)),
                    });
                    return Ok(true);
                }
            }
        }

        let shape = self.layer_for(&path, closed, ctx.foreground);
        ctx.emit(Command::create_layer(Layer::with_kind(
            "Path",
            LayerKind::Shape(shape),
        )));
        Ok(true)
    }
}

/// The active layer when it is a shape layer, with its shape definition.
fn active_shape(ctx: &ToolContext<'_>) -> Option<(layer_model::LayerId, layer_model::ShapeLayer)> {
    let active = ctx.active_layer?;
    ctx.shape_paths
        .iter()
        .find(|(id, _)| *id == active)
        .map(|(id, shape)| (*id, shape.clone()))
}

fn pt(v: Vec2) -> Point {
    Point::new(f64::from(v.x), f64::from(v.y))
}

/// The segment from `a` to `b`: a line when neither side has a handle, a
/// cubic otherwise.
fn join(path: &mut Path, a: &Anchor, b: &Anchor) {
    if a.handle_out == Vec2::ZERO && b.handle_in == Vec2::ZERO {
        path.line_to(pt(b.pos));
    } else {
        path.curve_to(pt(a.pos + a.handle_out), pt(b.pos + b.handle_in), pt(b.pos));
    }
}

/// The path a run of anchors describes.
pub fn anchors_to_path(anchors: &[Anchor], closed: bool) -> Path {
    let mut path = Path::new();
    let Some(first) = anchors.first() else {
        return path;
    };
    path.move_to(pt(first.pos));
    for pair in anchors.windows(2) {
        join(&mut path, &pair[0], &pair[1]);
    }
    if closed {
        if let Some(last) = anchors.last() {
            if anchors.len() > 1 && (last.handle_out != Vec2::ZERO || first.handle_in != Vec2::ZERO)
            {
                // A curved closing segment has to be spelled out; a straight
                // one is what `Z` draws.
                join(&mut path, last, first);
            }
        }
        path.close();
    }
    path
}

impl Tool for PenTool {
    fn id(&self) -> ToolId {
        ToolId::Pen
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("pen anchor", event.pos)?;
        if let Some(first) = self.anchors.first().map(|a| a.pos) {
            if self.anchors.len() >= MIN_CLOSED_ANCHORS
                && (event.pos - first).length() <= CLOSE_RADIUS_PX
            {
                // Pressing the first anchor closes the path. Publishing waits
                // for the release so a drag out of this press can shape the
                // closing segment.
                self.dragging = Some(Dragging::Closing);
                return Ok(());
            }
            // A repeated press on the anchor just placed adds nothing: a
            // zero-length segment is not a corner, and `vector` would carry it
            // into every later flatten and hit test.
            if let Some(last) = self.anchors.last() {
                if (event.pos - last.pos).length() <= f32::EPSILON {
                    self.dragging = Some(Dragging::Last);
                    return Ok(());
                }
            }
        }
        if self.anchors.len() >= MAX_ANCHORS {
            return Err(ToolError::RegionTooLarge {
                tiles: self.anchors.len() as u64,
                max: MAX_ANCHORS as u64,
            });
        }
        self.anchors.push(Anchor::corner(event.pos));
        self.dragging = Some(Dragging::Last);
        Ok(())
    }

    /// Dragging out of a press pulls the anchor's handles: the outgoing
    /// handle follows the pointer and the incoming one mirrors it, unless Alt
    /// is held, which leaves the incoming handle where it was.
    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(which) = self.dragging else {
            return Ok(());
        };
        if !event.pos.is_finite() {
            return Ok(());
        }
        let Some(anchor) = (match which {
            Dragging::Last => self.anchors.last_mut(),
            Dragging::Closing => self.anchors.first_mut(),
        }) else {
            return Ok(());
        };
        let d = event.pos - anchor.pos;
        if d.length() < DRAG_THRESHOLD_PX {
            return Ok(());
        }
        match which {
            Dragging::Last => {
                anchor.handle_out = d;
                if !event.modifiers.alt {
                    anchor.handle_in = -d;
                }
            }
            Dragging::Closing => {
                // The closing segment arrives at the first anchor, so the
                // dragged handle is its incoming one, mirrored from the drag.
                anchor.handle_in = -d;
                if !event.modifiers.alt {
                    anchor.handle_out = d;
                }
            }
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(which) = self.dragging.take() else {
            return Ok(());
        };
        if which == Dragging::Closing {
            self.closed = true;
            self.publish(ctx)?;
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.reset();
    }

    /// Enter finishes an open path and publishes it.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        if !self.publishable() {
            // Fewer anchors than a path needs. Refused rather than silently
            // dropped, so Enter on a half-started path says why.
            self.reset();
            return Err(ToolError::Degenerate);
        }
        self.publish(ctx)?;
        Ok(())
    }

    fn has_pending_commit(&self) -> bool {
        !self.anchors.is_empty()
    }

    fn is_active(&self) -> bool {
        !self.anchors.is_empty()
    }

    /// Every option the registry declares for the pen reaches it: `mode`,
    /// `combine`, and the five paint keys ([`crate::shape::PAINT_KEYS`]).
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        if let Some(answer) = self.paint.set(key, setting) {
            return answer;
        }
        let mismatch = || {
            Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            })
        };
        match (key, setting) {
            ("mode", ToolSetting::Choice(i)) => {
                self.mode = PenMode::from_choice(i);
                Ok(())
            }
            ("combine", ToolSetting::Choice(i)) => {
                self.combine = Combine::from_choice(i);
                Ok(())
            }
            ("mode" | "combine", _) => mismatch(),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use crate::tool::Modifiers;
    use layer_model::ShapeLayer;
    use raster::PixelRect;
    use vector::PathEl;

    fn ctx(tiles: &mut MemoryTiles) -> ToolContext<'_> {
        ToolContext::new(tiles, PixelRect::new(0, 0, 256, 256))
    }

    fn click(tool: &mut PenTool, ctx: &mut ToolContext<'_>, x: f32, y: f32) {
        tool.on_pointer_down(ctx, PointerEvent::at(x, y)).unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(x, y)).unwrap();
    }

    fn drag(
        tool: &mut PenTool,
        ctx: &mut ToolContext<'_>,
        from: (f32, f32),
        to: (f32, f32),
        modifiers: Modifiers,
    ) {
        tool.on_pointer_down(ctx, PointerEvent::at(from.0, from.1))
            .unwrap();
        tool.on_pointer_move(ctx, PointerEvent::at(to.0, to.1).with_modifiers(modifiers))
            .unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(to.0, to.1).with_modifiers(modifiers))
            .unwrap();
    }

    const ALT: Modifiers = Modifiers {
        shift: false,
        alt: true,
        ctrl: false,
    };

    fn shape_of(commands: &[Command]) -> ShapeLayer {
        let Some(Command::CreateLayer { layer }) = commands.first() else {
            panic!("no layer was created: {commands:?}");
        };
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("the pen created a {:?} layer", layer.kind);
        };
        shape.clone()
    }

    fn shape_svg(commands: &[Command]) -> String {
        shape_of(commands).path_svg
    }

    /// The largest distance of a flattened path's points from the chord
    /// between its first and last anchor.
    fn bulge(path: &Path) -> f64 {
        let polys = path.flatten(0.05);
        let pts = &polys[0].points;
        let (a, b) = (pts[0], *pts.last().unwrap());
        let ab = b - a;
        let len = ab.length();
        pts.iter()
            .map(|p| ((*p - a).cross(ab)).abs() / len)
            .fold(0.0, f64::max)
    }

    #[test]
    fn a_click_sequence_builds_corners_and_a_drag_builds_a_curve() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();

        click(&mut tool, &mut ctx, 10.0, 10.0);
        assert_eq!(tool.anchors(), &[Anchor::corner(Vec2::new(10.0, 10.0))]);
        assert!(tool.is_active());
        click(&mut tool, &mut ctx, 60.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 50.0);
        assert!(tool.anchors().iter().all(|a| !a.is_smooth()));
        let path = tool.path();
        assert_eq!(
            path.elements(),
            &[
                PathEl::MoveTo(Point::new(10.0, 10.0)),
                PathEl::LineTo(Point::new(60.0, 10.0)),
                PathEl::LineTo(Point::new(60.0, 50.0)),
            ],
            "clicks authored something other than corners"
        );
        // Nothing is emitted while the path is being drawn.
        assert!(ctx.commands().is_empty(), "{:?}", ctx.commands());

        // Now a press-and-drag: the anchor grows symmetric handles...
        drag(
            &mut tool,
            &mut ctx,
            (110.0, 50.0),
            (110.0, 90.0),
            Modifiers::NONE,
        );
        let smooth = tool.anchors()[3];
        assert_eq!(smooth.pos, Vec2::new(110.0, 50.0));
        assert_eq!(smooth.handle_out, Vec2::new(0.0, 40.0));
        assert_eq!(smooth.handle_in, Vec2::new(0.0, -40.0));
        assert!(smooth.is_smooth());
        // ...the path carries a cubic into it...
        let path = tool.path();
        assert!(
            matches!(path.elements()[3], PathEl::CurveTo(..)),
            "{:?}",
            path.elements()
        );
        // ...and the flattened segment really curves: its midpoint leaves
        // the chord between (60,50) and (110,50).
        let mut last = Path::new();
        last.move_to(Point::new(60.0, 50.0));
        last.push(path.elements()[3]);
        assert!(
            bulge(&last) > 5.0,
            "the dragged segment did not curve: bulge {}",
            bulge(&last)
        );
        assert!(ctx.commands().is_empty());
    }

    #[test]
    fn a_short_drag_is_a_click() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        drag(
            &mut tool,
            &mut ctx,
            (10.0, 10.0),
            (11.0, 10.5),
            Modifiers::NONE,
        );
        assert!(!tool.anchors()[0].is_smooth());
    }

    #[test]
    fn alt_drag_breaks_the_handles_so_they_are_asymmetric() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        // Drag without Alt first: symmetric.
        drag(
            &mut tool,
            &mut ctx,
            (60.0, 10.0),
            (80.0, 30.0),
            Modifiers::NONE,
        );
        let before = tool.anchors()[1];
        assert_eq!(before.handle_in, -before.handle_out);
        // Keep dragging the same anchor with Alt: the outgoing handle moves,
        // the incoming one stays.
        tool.on_pointer_down(&mut ctx, PointerEvent::at(60.0, 10.0))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(60.0, 50.0).with_modifiers(ALT))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(60.0, 50.0).with_modifiers(ALT))
            .unwrap();
        let after = tool.anchors()[1];
        assert_eq!(after.handle_out, Vec2::new(0.0, 40.0));
        assert_eq!(
            after.handle_in, before.handle_in,
            "Alt moved the incoming handle"
        );
        assert_ne!(
            after.handle_in, -after.handle_out,
            "the handles are still a mirror pair"
        );
        assert_eq!(tool.anchors().len(), 2, "the Alt press added an anchor");

        // A fresh Alt-drag on a new anchor grows only the outgoing handle.
        drag(&mut tool, &mut ctx, (120.0, 50.0), (150.0, 50.0), ALT);
        let cusp = tool.anchors()[2];
        assert_eq!(cusp.handle_out, Vec2::new(30.0, 0.0));
        assert_eq!(cusp.handle_in, Vec2::ZERO);
    }

    #[test]
    fn clicking_the_first_anchor_closes_the_path_and_makes_the_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        ctx.foreground = [1.0, 0.0, 0.0, 1.0];
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 50.0);
        // Within the close radius of the first anchor, not exactly on it.
        click(&mut tool, &mut ctx, 12.0, 11.0);

        let commands = ctx.drain();
        assert_eq!(commands.len(), 1, "{commands:?}");
        let shape = shape_of(&commands);
        assert!(shape.path_svg.starts_with('M'), "{}", shape.path_svg);
        assert!(
            shape.path_svg.trim_end().ends_with('Z'),
            "the closed path did not close: {}",
            shape.path_svg
        );
        // A closed path is filled with the foreground by default, unstroked.
        crate::shape::assert_rgba_near(shape.fill.expect("filled"), [1.0, 0.0, 0.0, 1.0]);
        assert!(shape.stroke.is_none());
        // ...and the tool is ready for the next path rather than stuck holding
        // the finished one.
        assert!(!tool.is_active());
        assert!(tool.anchors().is_empty());
    }

    #[test]
    fn dragging_out_of_the_closing_press_curves_the_closing_segment() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 50.0);
        drag(
            &mut tool,
            &mut ctx,
            (10.0, 10.0),
            (10.0, 40.0),
            Modifiers::NONE,
        );
        let svg = shape_svg(&ctx.drain());
        let path = svg::parse(&svg).unwrap();
        assert!(
            path.elements()
                .iter()
                .any(|e| matches!(e, PathEl::CurveTo(..))),
            "the closing drag drew no curve: {svg}"
        );
        assert!(svg.trim_end().ends_with('Z'), "{svg}");
        assert!(!tool.is_active());
    }

    #[test]
    fn enter_finishes_an_open_path_stroked_not_filled() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        ctx.foreground = [0.0, 1.0, 0.0, 1.0];
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 5.0, 5.0);
        click(&mut tool, &mut ctx, 40.0, 80.0);
        assert!(tool.has_pending_commit());
        Tool::commit(&mut tool, &mut ctx).unwrap();

        let commands = ctx.drain();
        let shape = shape_of(&commands);
        assert!(
            !shape.path_svg.trim_end().ends_with('Z'),
            "committing closed a path the user left open: {}",
            shape.path_svg
        );
        assert_eq!(shape.fill, None, "an open path was filled");
        let stroke = shape.stroke.expect("an open path is stroked");
        crate::shape::assert_rgba_near(stroke.color, [0.0, 1.0, 0.0, 1.0]);
        assert!(!tool.is_active());
    }

    #[test]
    fn the_paint_options_reach_the_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        ctx.foreground = [1.0, 0.0, 0.0, 1.0];
        let mut tool = PenTool::default();
        tool.set_setting("fill", ToolSetting::Choice(2)).unwrap();
        tool.set_setting("fill_color", ToolSetting::Color([0.0, 0.0, 1.0, 1.0]))
            .unwrap();
        tool.set_setting("stroke", ToolSetting::Choice(1)).unwrap();
        tool.set_setting("stroke_width", ToolSetting::Float(4.0))
            .unwrap();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 50.0);
        click(&mut tool, &mut ctx, 10.0, 10.0);
        let shape = shape_of(&ctx.drain());
        assert_eq!(shape.fill, Some([0.0, 0.0, 1.0, 1.0]));
        let stroke = shape.stroke.unwrap();
        crate::shape::assert_rgba_near(stroke.color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(stroke.width_px, 4.0);
    }

    #[test]
    fn path_mode_publishes_the_path_with_no_paint() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        tool.set_setting("mode", ToolSetting::Choice(0)).unwrap();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 50.0);
        click(&mut tool, &mut ctx, 10.0, 10.0);
        let shape = shape_of(&ctx.drain());
        assert!(shape.fill.is_none() && shape.stroke.is_none());
        assert!(!shape.is_drawable());
        assert!(svg::parse(&shape.path_svg).is_ok());
    }

    fn triangle_anchors(tool: &mut PenTool, ctx: &mut ToolContext<'_>) {
        click(tool, ctx, 10.0, 10.0);
        click(tool, ctx, 60.0, 10.0);
        click(tool, ctx, 60.0, 50.0);
        click(tool, ctx, 10.0, 10.0);
    }

    fn square_layer() -> (layer_model::LayerId, ShapeLayer) {
        let square = vector::shapes::rect(vector::Bounds::from_xywh(0.0, 0.0, 40.0, 40.0));
        let mut shape = ShapeLayer::from_svg(svg::to_svg(&square));
        shape.fill = Some([0.0, 0.0, 1.0, 1.0]);
        (layer_model::LayerId::new(), shape)
    }

    #[test]
    fn add_subtract_and_intersect_rewrite_the_active_shape_layer() {
        for (choice, area_lo, area_hi) in [
            // Union of a 40x40 square and a triangle: more than the square.
            (1usize, 1601.0, 3000.0),
            // The square minus the triangle: less than the square.
            (2, 800.0, 1599.0),
            // Their overlap: less than the triangle (1000).
            (3, 100.0, 999.0),
        ] {
            let mut tiles = MemoryTiles::new();
            let mut ctx = ctx(&mut tiles);
            let (id, shape) = square_layer();
            ctx.active_layer = Some(id);
            ctx.shape_paths = vec![(id, shape)];
            let mut tool = PenTool::default();
            tool.set_setting("combine", ToolSetting::Choice(choice))
                .unwrap();
            triangle_anchors(&mut tool, &mut ctx);
            let commands = ctx.drain();
            assert_eq!(commands.len(), 1, "{commands:?}");
            let Command::SetLayerKind { layer_id, kind } = &commands[0] else {
                panic!("combine {choice} created a layer instead: {commands:?}");
            };
            assert_eq!(*layer_id, id);
            let LayerKind::Shape(shape) = kind.as_ref() else {
                panic!("{kind:?}");
            };
            assert_eq!(shape.fill, Some([0.0, 0.0, 1.0, 1.0]), "the paint was lost");
            let path = svg::parse(&shape.path_svg).unwrap();
            let area = vector::fill(&path, &vector::FillOptions::default())
                .unwrap()
                .area();
            assert!(
                area > area_lo && area < area_hi,
                "combine {choice}: area {area} outside {area_lo}..{area_hi}"
            );
        }
    }

    #[test]
    fn combine_new_or_no_active_shape_layer_creates_a_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let (id, shape) = square_layer();
        ctx.active_layer = Some(id);
        ctx.shape_paths = vec![(id, shape)];
        let mut tool = PenTool::default();
        triangle_anchors(&mut tool, &mut ctx);
        assert!(matches!(ctx.drain()[..], [Command::CreateLayer { .. }]));

        // Add, but the active layer is not a shape layer.
        ctx.active_layer = Some(layer_model::LayerId::new());
        tool.set_setting("combine", ToolSetting::Choice(1)).unwrap();
        triangle_anchors(&mut tool, &mut ctx);
        assert!(matches!(ctx.drain()[..], [Command::CreateLayer { .. }]));

        // Add, but the path is open: a boolean needs a region.
        ctx.active_layer = Some(id);
        click(&mut tool, &mut ctx, 100.0, 100.0);
        click(&mut tool, &mut ctx, 150.0, 100.0);
        Tool::commit(&mut tool, &mut ctx).unwrap();
        assert!(matches!(ctx.drain()[..], [Command::CreateLayer { .. }]));
    }

    #[test]
    fn a_path_with_too_few_anchors_makes_no_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 5.0, 5.0);
        assert!(Tool::commit(&mut tool, &mut ctx).is_err());
        assert!(ctx.commands().is_empty());
        assert!(!tool.is_active(), "the refused path was not cleared");

        // ...and two anchors on the same point are one anchor.
        click(&mut tool, &mut ctx, 20.0, 20.0);
        click(&mut tool, &mut ctx, 20.0, 20.0);
        assert_eq!(tool.anchors().len(), 1);
    }

    #[test]
    fn a_click_near_the_first_anchor_before_the_third_is_a_corner_not_a_close() {
        // Two anchors cannot enclose anything, so clicking back on the start
        // has to keep drawing rather than publish a degenerate loop.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 40.0, 10.0);
        click(&mut tool, &mut ctx, 11.0, 10.0);
        assert_eq!(tool.anchors().len(), 3);
        assert!(!tool.is_closed());
        assert!(ctx.commands().is_empty());
    }

    #[test]
    fn escape_throws_the_path_away_and_emits_nothing() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 10.0);
        Tool::cancel(&mut tool, &mut ctx);
        assert!(!tool.is_active());
        assert!(tool.anchors().is_empty());
        assert!(ctx.commands().is_empty());
        assert!(tool.path().is_empty());
    }

    #[test]
    fn a_non_finite_click_is_refused() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        assert!(tool
            .on_pointer_down(&mut ctx, PointerEvent::at(0.0, f32::INFINITY))
            .is_err());
        assert!(tool.anchors().is_empty());
    }

    #[test]
    fn the_anchor_count_is_bounded() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        // Every anchor further from the first than `CLOSE_RADIUS_PX`, so the
        // walk never closes the path and publishes it out from under the count.
        for i in 0..MAX_ANCHORS {
            click(&mut tool, &mut ctx, 100.0 + i as f32 * 10.0, 100.0);
        }
        assert_eq!(tool.anchors().len(), MAX_ANCHORS);
        assert!(tool
            .on_pointer_down(&mut ctx, PointerEvent::at(999_999.0, 100.0))
            .is_err());
    }

    #[test]
    fn options_refuse_the_wrong_kind_and_unknown_keys() {
        let mut tool = PenTool::default();
        assert!(matches!(
            tool.set_setting("mode", ToolSetting::Float(1.0)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
        assert!(matches!(
            tool.set_setting("combine", ToolSetting::Bool(true)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
        assert!(matches!(
            tool.set_setting("radius", ToolSetting::Float(1.0)),
            Err(ToolError::UnknownOption { .. })
        ));
        tool.set_setting("mode", ToolSetting::Choice(2)).unwrap();
        assert_eq!(tool.mode, PenMode::Pixels);
        tool.set_setting("combine", ToolSetting::Choice(3)).unwrap();
        assert_eq!(tool.combine, Combine::Intersect);
    }

    /// Pixels mode paints the path into the active layer on commit, with the
    /// same paint rule as the shape tools: stroke only leaves the inside
    /// clear, and the default fill paints it.
    #[test]
    fn pixels_mode_paints_the_active_layer_with_the_paint_options() {
        let run = |configure: &dyn Fn(&mut PenTool)| {
            let mut tiles = MemoryTiles::new();
            let layer = layer_model::LayerId::new();
            let key = editor_core::PixelKey::Layer(layer);
            let commands = {
                let mut ctx = ctx(&mut tiles)
                    .with_layer(layer)
                    .with_foreground([0.0, 0.0, 1.0, 1.0]);
                let mut tool = PenTool::default();
                tool.set_setting("mode", ToolSetting::Choice(2)).unwrap();
                configure(&mut tool);
                click(&mut tool, &mut ctx, 20.0, 20.0);
                click(&mut tool, &mut ctx, 120.0, 20.0);
                click(&mut tool, &mut ctx, 120.0, 120.0);
                click(&mut tool, &mut ctx, 20.0, 120.0);
                click(&mut tool, &mut ctx, 20.0, 20.0);
                ctx.drain()
            };
            assert!(
                commands
                    .iter()
                    .all(|c| matches!(c, Command::PaintTiles { .. })),
                "Pixels mode made something other than a paint step: {commands:?}"
            );
            for command in &commands {
                if let Command::PaintTiles { delta, .. } = command {
                    tiles.apply_delta(key, delta);
                }
            }
            (tiles, key)
        };
        let (tiles, key) = run(&|tool| {
            tool.set_setting("fill", ToolSetting::Choice(0)).unwrap();
            tool.set_setting("stroke", ToolSetting::Choice(1)).unwrap();
            tool.set_setting("stroke_width", ToolSetting::Float(4.0))
                .unwrap();
        });
        assert_eq!(
            tiles.pixel(key, 70, 70),
            [0, 0, 0, 0],
            "fill off painted the inside"
        );
        assert!(tiles.pixel(key, 70, 20)[3] > 0, "the stroke is missing");
        let (tiles, key) = run(&|_| {});
        assert_eq!(tiles.pixel(key, 70, 70), [0, 0, 255, 255]);
    }
}
