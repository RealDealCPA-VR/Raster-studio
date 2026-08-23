//! Selection tools.
//!
//! Every one of these is a gesture recorder in front of an algorithm that
//! already exists in `selection`. The tools contribute three things the
//! algorithms deliberately do not have: **when** a gesture is finished, **what
//! the modifier keys meant** (replace / add / subtract / intersect, decided at
//! pointer-down so a shift released mid-drag does not change the answer), and
//! **which pixels the colour-driven ones read**.
//!
//! Everything they emit is a [`SelectionEdit`], never a [`editor_core::Command`]:
//! the selection is a field on the document rather than a command target, so
//! folding these in is the application's job and
//! [`SelectionEdit::apply`] is how it does it.

use editor_core::Selection;
use glam::{IVec2, Vec2};
use selection::{
    lasso::{lasso_freehand, lasso_magnetic, lasso_polygonal, MagneticOptions},
    marquee::{ellipse_subpixel, rectangle_subpixel, single_column, single_row},
    wand::{magic_wand, quick_select, QuickSelectOptions, WandOptions},
    BooleanOp, ImageView,
};

use crate::error::ToolError;
use crate::patch::read_rgba8;
use crate::tool::{PointerEvent, SelectionEdit, Tool, ToolContext, ToolId};

/// How close to the first vertex a click has to land to close a polygon.
pub const POLYGON_CLOSE_PX: f32 = 6.0;

/// Distance between magnetic-lasso anchors.
const MAGNETIC_ANCHOR_SPACING: f32 = 12.0;

/// Which marquee shape a [`MarqueeTool`] draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarqueeShape {
    Rect,
    Ellipse,
    SingleRow,
    SingleColumn,
}

impl MarqueeShape {
    fn tool_id(self) -> ToolId {
        match self {
            MarqueeShape::Rect => ToolId::RectMarquee,
            MarqueeShape::Ellipse => ToolId::EllipseMarquee,
            MarqueeShape::SingleRow => ToolId::SingleRowMarquee,
            MarqueeShape::SingleColumn => ToolId::SingleColumnMarquee,
        }
    }
}

/// Rectangular, elliptical and single-pixel-line marquees.
pub struct MarqueeTool {
    shape: MarqueeShape,
    /// Draw outward from the first corner rather than treating it as an edge.
    pub from_center: bool,
    anchor: Option<Vec2>,
    current: Option<Vec2>,
    op: BooleanOp,
}

impl MarqueeTool {
    pub fn new(shape: MarqueeShape) -> Self {
        Self {
            shape,
            from_center: false,
            anchor: None,
            current: None,
            op: BooleanOp::Replace,
        }
    }

    /// The rubber-band rectangle, for the overlay.
    pub fn preview(&self) -> Option<(Vec2, Vec2)> {
        Some(self.corners(self.anchor?, self.current?, false))
    }

    /// The corners after the modifier constraints are applied.
    ///
    /// The anchor is passed in rather than read from `self`, because
    /// [`Tool::on_pointer_up`] takes it out of the tool *before* it builds the
    /// mask — reading it back from `self` there would silently produce a
    /// zero-area box.
    fn corners(&self, a: Vec2, to: Vec2, shift: bool) -> (Vec2, Vec2) {
        let mut b = to;
        if shift {
            // Constrain to a square, keeping the drag's dominant extent.
            let d = b - a;
            let s = d.x.abs().max(d.y.abs());
            b = a + Vec2::new(s * d.x.signum(), s * d.y.signum());
        }
        if self.from_center {
            let d = b - a;
            (a - d, a + d)
        } else {
            (a, b)
        }
    }
}

impl Tool for MarqueeTool {
    fn id(&self) -> ToolId {
        self.shape.tool_id()
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("marquee anchor", event.pos)?;
        // Captured now: releasing shift halfway through a drag must not change
        // whether this gesture adds or replaces.
        self.op = event.modifiers.selection_op();
        self.anchor = Some(event.pos);
        self.current = Some(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.anchor.is_some() {
            self.current = Some(event.pos);
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(anchor) = self.anchor.take() else {
            return Ok(());
        };
        self.current = None;
        crate::error::finite_pt("marquee corner", event.pos)?;
        let mask = match self.shape {
            MarqueeShape::SingleRow => {
                let y = anchor.y.floor() as i32;
                single_row(y, ctx.canvas.x as i32, ctx.canvas.width)?
            }
            MarqueeShape::SingleColumn => {
                let x = anchor.x.floor() as i32;
                single_column(x, ctx.canvas.y as i32, ctx.canvas.height)?
            }
            MarqueeShape::Rect => {
                let (a, b) = self.corners(anchor, event.pos, event.modifiers.shift);
                rectangle_subpixel(a, b)?
            }
            MarqueeShape::Ellipse => {
                let (a, b) = self.corners(anchor, event.pos, event.modifiers.shift);
                ellipse_subpixel(a, b)?
            }
        };
        ctx.emit_selection(SelectionEdit::new(Selection::Mask(mask), self.op));
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor = None;
        self.current = None;
    }

    fn is_active(&self) -> bool {
        self.anchor.is_some()
    }
}

/// Which lasso a [`LassoTool`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LassoKind {
    /// Follows the pointer.
    Freehand,
    /// Click to place corners; click the first one again to close.
    Polygonal,
    /// Follows the pointer, then snaps the path onto image edges.
    Magnetic,
}

impl LassoKind {
    fn tool_id(self) -> ToolId {
        match self {
            LassoKind::Freehand => ToolId::Lasso,
            LassoKind::Polygonal => ToolId::PolygonalLasso,
            LassoKind::Magnetic => ToolId::MagneticLasso,
        }
    }
}

/// The three lassos.
pub struct LassoTool {
    kind: LassoKind,
    pub magnetic: MagneticOptions,
    points: Vec<Vec2>,
    dragging: bool,
    op: BooleanOp,
}

impl LassoTool {
    pub fn new(kind: LassoKind) -> Self {
        Self {
            kind,
            magnetic: MagneticOptions::default(),
            points: Vec::new(),
            dragging: false,
            op: BooleanOp::Replace,
        }
    }

    /// The path so far, for the overlay.
    pub fn path(&self) -> &[Vec2] {
        &self.points
    }

    /// Close the outline and emit it — the Enter key, and what clicking the
    /// first vertex of a polygonal lasso does.
    pub fn close(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let pts = std::mem::take(&mut self.points);
        self.dragging = false;
        if pts.len() < 3 {
            return Ok(());
        }
        let mask = match self.kind {
            LassoKind::Freehand => lasso_freehand(&pts)?,
            LassoKind::Polygonal => lasso_polygonal(&pts)?,
            LassoKind::Magnetic => {
                let key = ctx.sample_key()?;
                let pixels = read_rgba8(ctx.tiles, key, ctx.canvas)?;
                let view = ImageView::new(
                    IVec2::new(ctx.canvas.x as i32, ctx.canvas.y as i32),
                    ctx.canvas.width,
                    ctx.canvas.height,
                    &pixels,
                )?;
                lasso_magnetic(&view, &pts, &self.magnetic)?
            }
        };
        ctx.emit_selection(SelectionEdit::new(Selection::Mask(mask), self.op));
        Ok(())
    }
}

impl Tool for LassoTool {
    fn id(&self) -> ToolId {
        self.kind.tool_id()
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("lasso point", event.pos)?;
        if self.points.is_empty() {
            self.op = event.modifiers.selection_op();
        }
        match self.kind {
            LassoKind::Polygonal => {
                // Clicking back on the first vertex closes the outline.
                if self.points.len() >= 3
                    && (event.pos - self.points[0]).length() <= POLYGON_CLOSE_PX
                {
                    return self.close(ctx);
                }
                self.points.push(event.pos);
            }
            _ => {
                self.points.clear();
                self.points.push(event.pos);
                self.dragging = true;
            }
        }
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if !self.dragging {
            return Ok(());
        }
        crate::error::finite_pt("lasso point", event.pos)?;
        match self.kind {
            // A magnetic lasso wants sparse anchors: the snap runs between
            // them, and one anchor per pointer sample would pin the path to
            // every wobble of the hand.
            LassoKind::Magnetic => {
                if self
                    .points
                    .last()
                    .is_none_or(|p| (event.pos - *p).length() >= MAGNETIC_ANCHOR_SPACING)
                {
                    self.points.push(event.pos);
                }
            }
            _ => self.points.push(event.pos),
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        match self.kind {
            // A polygonal lasso's release is not the end of anything.
            LassoKind::Polygonal => Ok(()),
            _ => {
                if self.dragging {
                    self.points.push(event.pos);
                    self.close(ctx)
                } else {
                    Ok(())
                }
            }
        }
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.points.clear();
        self.dragging = false;
    }

    fn is_active(&self) -> bool {
        !self.points.is_empty()
    }
}

/// Which colour-driven selector a [`WandTool`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WandKind {
    /// Click a pixel, select everything within tolerance of it.
    Magic,
    /// Scrub over a region, select what the stroke's own colour spread covers.
    Quick,
}

/// The magic wand and quick select.
pub struct WandTool {
    kind: WandKind,
    pub wand: WandOptions,
    pub quick: QuickSelectOptions,
    stroke: Vec<Vec2>,
    active: bool,
    op: BooleanOp,
}

impl WandTool {
    pub fn new(kind: WandKind) -> Self {
        Self {
            kind,
            wand: WandOptions::default(),
            quick: QuickSelectOptions::default(),
            stroke: Vec::new(),
            active: false,
            op: BooleanOp::Replace,
        }
    }

    fn finish(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let stroke = std::mem::take(&mut self.stroke);
        self.active = false;
        let Some(first) = stroke.first().copied() else {
            return Ok(());
        };
        let key = ctx.sample_key()?;
        let canvas = ctx.canvas;
        let pixels = read_rgba8(ctx.tiles, key, canvas)?;
        let view = ImageView::new(
            IVec2::new(canvas.x as i32, canvas.y as i32),
            canvas.width,
            canvas.height,
            &pixels,
        )?;
        let mask = match self.kind {
            WandKind::Magic => {
                let seed = IVec2::new(first.x.floor() as i32, first.y.floor() as i32);
                if !view.contains(seed) {
                    return Err(ToolError::PointOutside {
                        x: seed.x,
                        y: seed.y,
                    });
                }
                magic_wand(&view, seed, &self.wand)?
            }
            WandKind::Quick => quick_select(&view, &stroke, &self.quick)?,
        };
        ctx.emit_selection(SelectionEdit::new(Selection::Mask(mask), self.op));
        Ok(())
    }
}

impl Tool for WandTool {
    fn id(&self) -> ToolId {
        match self.kind {
            WandKind::Magic => ToolId::MagicWand,
            WandKind::Quick => ToolId::QuickSelect,
        }
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("wand seed", event.pos)?;
        self.op = event.modifiers.selection_op();
        self.stroke.clear();
        self.stroke.push(event.pos);
        self.active = true;
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.active && self.kind == WandKind::Quick {
            crate::error::finite_pt("quick select point", event.pos)?;
            self.stroke.push(event.pos);
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if !self.active {
            return Ok(());
        }
        if self.kind == WandKind::Quick {
            self.stroke.push(event.pos);
        }
        self.finish(ctx)
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.stroke.clear();
        self.active = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }
}
