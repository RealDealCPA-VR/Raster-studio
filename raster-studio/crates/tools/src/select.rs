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

use editor_core::{PixelKey, Selection, SelectionMask};
use glam::{IVec2, Vec2};
use selection::{
    feather,
    lasso::{lasso_freehand, lasso_magnetic, lasso_polygonal, MagneticOptions},
    marquee::{ellipse_subpixel, rectangle_subpixel, single_column, single_row},
    wand::{magic_wand, quick_select, QuickSelectOptions, WandOptions},
    BooleanOp, ImageView,
};

use crate::error::{finite, ToolError};
use crate::patch::read_rgba8;
use crate::tool::{Modifiers, PointerEvent, SelectionEdit, Tool, ToolContext, ToolId, ToolSetting};

/// How close to the first vertex a click has to land to close a polygon.
pub const POLYGON_CLOSE_PX: f32 = 6.0;

/// Distance between magnetic-lasso anchors.
const MAGNETIC_ANCHOR_SPACING: f32 = 12.0;

/// The largest feather the options bar offers (its `feather` spec's max),
/// well inside `selection`'s own radius cap.
pub const MAX_FEATHER_PX: f32 = 250.0;

/// The options bar's `mode` choices, in the registry's order: New, Add,
/// Subtract, Intersect. A choice index is looked up here, so the registry's
/// spec and this table must agree.
pub const SELECTION_MODES: &[BooleanOp] = &[
    BooleanOp::Replace,
    BooleanOp::Add,
    BooleanOp::Subtract,
    BooleanOp::Intersect,
];

fn mode_from_choice(key: &str, index: usize) -> Result<BooleanOp, ToolError> {
    SELECTION_MODES
        .get(index)
        .copied()
        .ok_or_else(|| ToolError::OptionKindMismatch {
            key: key.to_owned(),
        })
}

/// The boolean op a selection gesture commits with.
///
/// A held modifier wins — shift adds, alt subtracts, both intersect, the
/// convention every raster editor shares — and with no modifier held the
/// options bar's Mode decides. Captured at pointer-down, so releasing the key
/// mid-drag does not change the answer.
pub fn gesture_op(mode: BooleanOp, modifiers: Modifiers) -> BooleanOp {
    if modifiers.shift || modifiers.alt {
        modifiers.selection_op()
    } else {
        mode
    }
}

/// Snap every coverage sample to fully in or fully out — what "Anti-alias
/// off" means for a sub-pixel rasteriser.
fn harden(mask: &SelectionMask) -> Result<SelectionMask, ToolError> {
    let coverage = mask
        .coverage()
        .iter()
        .map(|&v| if v >= 128 { 255 } else { 0 })
        .collect();
    Ok(SelectionMask::new(
        mask.origin(),
        mask.width(),
        mask.height(),
        coverage,
    )?)
}

/// The three controls every selection gesture shares — the options bar's
/// Mode, Feather and Anti-alias — and how they reach the mask.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionOptions {
    /// How the gesture combines with the existing selection when no modifier
    /// is held.
    pub mode: BooleanOp,
    /// Feather radius in pixels; `0.0` leaves the edge as rasterised.
    pub feather: f32,
    /// Keep the rasteriser's fractional edge coverage; off snaps it hard.
    pub antialias: bool,
}

impl Default for SelectionOptions {
    fn default() -> Self {
        Self {
            mode: BooleanOp::Replace,
            feather: 0.0,
            antialias: true,
        }
    }
}

impl SelectionOptions {
    /// Adopt one options-bar value. `Ok(false)` means the key is not one of
    /// these three, so the caller can answer its own keys or refuse.
    fn set(&mut self, key: &str, setting: ToolSetting) -> Result<bool, ToolError> {
        match (key, setting) {
            ("mode", ToolSetting::Choice(i)) => {
                self.mode = mode_from_choice(key, i)?;
                Ok(true)
            }
            ("feather", ToolSetting::Float(v)) => {
                self.feather = finite("feather", v)?.clamp(0.0, MAX_FEATHER_PX);
                Ok(true)
            }
            ("antialias", ToolSetting::Bool(v)) => {
                self.antialias = v;
                Ok(true)
            }
            ("mode", _) | ("feather", _) | ("antialias", _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Ok(false),
        }
    }

    /// Run the rasterised shape through the two edge controls: anti-alias
    /// first (it is a property of the rasterised edge), then the feather (a
    /// blur of whatever edge that left).
    pub fn finish(&self, mask: SelectionMask) -> Result<SelectionMask, ToolError> {
        let mask = if self.antialias { mask } else { harden(&mask)? };
        if self.feather > 0.0 {
            Ok(feather(&mask, self.feather)?)
        } else {
            Ok(mask)
        }
    }
}

fn unknown(key: &str) -> ToolError {
    ToolError::UnknownOption {
        key: key.to_owned(),
    }
}

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
    /// The options bar's Mode / Feather / Anti-alias.
    pub options: SelectionOptions,
    anchor: Option<Vec2>,
    current: Option<Vec2>,
    op: BooleanOp,
}

impl MarqueeTool {
    pub fn new(shape: MarqueeShape) -> Self {
        Self {
            shape,
            from_center: false,
            options: SelectionOptions::default(),
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
        self.op = gesture_op(self.options.mode, event.modifiers);
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
        let mask = self.options.finish(mask)?;
        ctx.emit_selection(SelectionEdit::new(Selection::Mask(mask), self.op));
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor = None;
        self.current = None;
    }

    /// Exactly the registry's `SELECTION_OPTS` keys; anything else is refused.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        if self.options.set(key, setting)? {
            Ok(())
        } else {
            Err(unknown(key))
        }
    }

    fn set_choice(&mut self, key: &str, index: usize) {
        let _ = self.set_setting(key, ToolSetting::Choice(index));
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
    /// The magnetic lasso's snap: the options bar's Width (`search_radius`)
    /// and Contrast (`edge_weight`) land here and `lasso_magnetic` reads them.
    pub magnetic: MagneticOptions,
    /// The options bar's Mode / Feather / Anti-alias.
    pub options: SelectionOptions,
    points: Vec<Vec2>,
    dragging: bool,
    op: BooleanOp,
}

impl LassoTool {
    pub fn new(kind: LassoKind) -> Self {
        Self {
            kind,
            magnetic: MagneticOptions::default(),
            options: SelectionOptions::default(),
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
        let mask = self.options.finish(mask)?;
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
            self.op = gesture_op(self.options.mode, event.modifiers);
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

    /// The shared selection keys plus the magnetic lasso's two snap controls
    /// (the registry's `search_radius` "Width" and `edge_weight` "Contrast"),
    /// which `lasso_magnetic` reads out of [`LassoTool::magnetic`] at close.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        if self.options.set(key, setting)? {
            return Ok(());
        }
        match (key, setting) {
            ("search_radius", ToolSetting::Int(v)) => {
                self.magnetic.search_radius = v.clamp(1, 256) as u32;
                Ok(())
            }
            ("edge_weight", ToolSetting::Float(v)) => {
                self.magnetic.edge_weight = finite("edge weight", v)?.clamp(0.0, 4.0);
                Ok(())
            }
            ("search_radius", _) | ("edge_weight", _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(unknown(key)),
        }
    }

    fn set_choice(&mut self, key: &str, index: usize) {
        let _ = self.set_setting(key, ToolSetting::Choice(index));
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
    /// How the gesture combines with the existing selection when no modifier
    /// is held (the options bar's Mode).
    pub mode: BooleanOp,
    /// Judge tolerance against the flattened composite (the context's
    /// `sample_from`) rather than the active layer's own pixels.
    pub sample_merged: bool,
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
            mode: BooleanOp::Replace,
            sample_merged: false,
            stroke: Vec::new(),
            active: false,
            op: BooleanOp::Replace,
        }
    }

    /// Where the colour read comes from: the composite when Sample All Layers
    /// is on, the active layer otherwise.
    fn read_key(&self, ctx: &ToolContext<'_>) -> Result<PixelKey, ToolError> {
        if self.sample_merged {
            ctx.sample_key()
        } else {
            Ok(PixelKey::Layer(
                ctx.active_layer.ok_or(ToolError::NoActiveLayer)?,
            ))
        }
    }

    fn finish(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let stroke = std::mem::take(&mut self.stroke);
        self.active = false;
        let Some(first) = stroke.first().copied() else {
            return Ok(());
        };
        let key = self.read_key(ctx)?;
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
        self.op = gesture_op(self.mode, event.modifiers);
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

    /// The Magic Wand answers the registry's `WAND_OPTS` (mode, tolerance,
    /// contiguous, antialias, sample_merged); Quick Selection answers its
    /// own three (mode, radius, tolerance). Each key lands on the option
    /// struct the flood actually reads.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        let mismatch = || ToolError::OptionKindMismatch {
            key: key.to_owned(),
        };
        match (self.kind, key, setting) {
            (_, "mode", ToolSetting::Choice(i)) => {
                self.mode = mode_from_choice(key, i)?;
                Ok(())
            }
            (WandKind::Magic, "tolerance", ToolSetting::Float(v)) => {
                self.wand.tolerance = finite("tolerance", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            (WandKind::Magic, "contiguous", ToolSetting::Bool(v)) => {
                self.wand.contiguous = v;
                Ok(())
            }
            (WandKind::Magic, "antialias", ToolSetting::Bool(v)) => {
                // The same rim the paint bucket uses: coverage ramps out over
                // the last half of the tolerance, or not at all.
                self.wand.antialias = if v { 0.5 } else { 0.0 };
                Ok(())
            }
            (WandKind::Magic, "sample_merged", ToolSetting::Bool(v)) => {
                self.sample_merged = v;
                Ok(())
            }
            (WandKind::Quick, "radius", ToolSetting::Float(v)) => {
                self.quick.radius = finite("radius", v)?.clamp(1.0, 500.0);
                Ok(())
            }
            (WandKind::Quick, "tolerance", ToolSetting::Float(v)) => {
                self.quick.tolerance = finite("tolerance", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            (_, "mode", _)
            | (WandKind::Magic, "tolerance" | "contiguous" | "antialias" | "sample_merged", _)
            | (WandKind::Quick, "radius" | "tolerance", _) => Err(mismatch()),
            _ => Err(unknown(key)),
        }
    }

    fn set_choice(&mut self, key: &str, index: usize) {
        let _ = self.set_setting(key, ToolSetting::Choice(index));
    }

    fn is_active(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use layer_model::LayerId;
    use raster::PixelRect;

    const W: i64 = 64;
    const H: i64 = 64;

    /// A 64x64 opaque layer painted by `color(x)` per column, in one tile.
    fn paint(tiles: &mut MemoryTiles, key: PixelKey, color: impl Fn(i64) -> u8) {
        let ts = raster::TILE_SIZE as usize;
        let mut data = vec![0u8; ts * ts * 4];
        for y in 0..H as usize {
            for x in 0..W as usize {
                let g = color(x as i64);
                let i = (y * ts + x) * 4;
                data[i..i + 4].copy_from_slice(&[g, g, g, 255]);
            }
        }
        tiles.put(key, raster::TileCoord::new(0, 0, 0), data);
    }

    /// Three bands: dark (x < 16), light (16..32), dark again (32..).
    fn bands(x: i64) -> u8 {
        if (16..32).contains(&x) {
            220
        } else {
            40
        }
    }

    fn covered(edit: &SelectionEdit) -> usize {
        match &edit.incoming {
            Selection::Mask(m) => m.coverage().iter().filter(|&&v| v > 0).count(),
            other => panic!("expected a mask, got {other:?}"),
        }
    }

    fn partial(edit: &SelectionEdit) -> usize {
        match &edit.incoming {
            Selection::Mask(m) => m.coverage().iter().filter(|&&v| v > 0 && v < 255).count(),
            other => panic!("expected a mask, got {other:?}"),
        }
    }

    fn drag(
        tool: &mut dyn Tool,
        ctx: &mut ToolContext<'_>,
        a: (f32, f32),
        b: (f32, f32),
        m: Modifiers,
    ) -> SelectionEdit {
        tool.on_pointer_down(ctx, PointerEvent::at(a.0, a.1).with_modifiers(m))
            .unwrap();
        tool.on_pointer_move(ctx, PointerEvent::at(b.0, b.1).with_modifiers(m))
            .unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(b.0, b.1).with_modifiers(m))
            .unwrap();
        let mut edits = ctx.drain_selection();
        assert_eq!(edits.len(), 1, "one gesture, one edit");
        edits.pop().unwrap()
    }

    #[test]
    fn the_mode_choice_sets_the_op_and_a_held_modifier_still_overrides_it() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, W as u32, H as u32));
        let mut tool = MarqueeTool::new(MarqueeShape::Rect);
        // Default: New.
        assert_eq!(
            drag(
                &mut tool,
                &mut ctx,
                (2.0, 2.0),
                (20.0, 20.0),
                Modifiers::NONE
            )
            .op,
            BooleanOp::Replace
        );
        for (index, op) in [
            (1, BooleanOp::Add),
            (2, BooleanOp::Subtract),
            (3, BooleanOp::Intersect),
            (0, BooleanOp::Replace),
        ] {
            tool.set_setting("mode", ToolSetting::Choice(index))
                .unwrap();
            assert_eq!(
                drag(
                    &mut tool,
                    &mut ctx,
                    (2.0, 2.0),
                    (20.0, 20.0),
                    Modifiers::NONE
                )
                .op,
                op,
                "choice {index}"
            );
        }
        // Mode Add, alt held: the modifier wins (subtract).
        tool.set_setting("mode", ToolSetting::Choice(1)).unwrap();
        assert_eq!(
            drag(
                &mut tool,
                &mut ctx,
                (2.0, 2.0),
                (20.0, 20.0),
                Modifiers::alt()
            )
            .op,
            BooleanOp::Subtract
        );
        // An index past the table and a wrong kind are refused loudly.
        assert!(matches!(
            tool.set_setting("mode", ToolSetting::Choice(9)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
        assert!(matches!(
            tool.set_setting("mode", ToolSetting::Bool(true)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
        assert!(matches!(
            tool.set_setting("no_such", ToolSetting::Bool(true)),
            Err(ToolError::UnknownOption { .. })
        ));
        // The same table drives the lassos and the wands.
        let mut lasso = LassoTool::new(LassoKind::Freehand);
        lasso.set_setting("mode", ToolSetting::Choice(3)).unwrap();
        lasso
            .on_pointer_down(&mut ctx, PointerEvent::at(2.0, 2.0))
            .unwrap();
        lasso
            .on_pointer_move(&mut ctx, PointerEvent::at(30.0, 2.0))
            .unwrap();
        lasso
            .on_pointer_move(&mut ctx, PointerEvent::at(30.0, 30.0))
            .unwrap();
        lasso
            .on_pointer_up(&mut ctx, PointerEvent::at(2.0, 30.0))
            .unwrap();
        assert_eq!(ctx.drain_selection()[0].op, BooleanOp::Intersect);
    }

    #[test]
    fn antialias_off_hardens_the_marquee_edge_and_feather_softens_it() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, W as u32, H as u32));
        let mut tool = MarqueeTool::new(MarqueeShape::Rect);
        // A half-pixel rectangle: the default anti-aliased edge has fractional
        // coverage along all four sides.
        let soft = drag(
            &mut tool,
            &mut ctx,
            (10.5, 10.5),
            (30.5, 30.5),
            Modifiers::NONE,
        );
        assert!(
            partial(&soft) > 0,
            "the sub-pixel rasteriser gives a fractional rim"
        );
        assert_eq!(soft.incoming.coverage_at(IVec2::new(8, 20)), 0.0);

        tool.set_setting("antialias", ToolSetting::Bool(false))
            .unwrap();
        let hard = drag(
            &mut tool,
            &mut ctx,
            (10.5, 10.5),
            (30.5, 30.5),
            Modifiers::NONE,
        );
        assert_eq!(
            partial(&hard),
            0,
            "anti-alias off leaves no fractional coverage"
        );
        assert!(covered(&hard) > 0);

        tool.set_setting("feather", ToolSetting::Float(4.0))
            .unwrap();
        let feathered = drag(
            &mut tool,
            &mut ctx,
            (10.5, 10.5),
            (30.5, 30.5),
            Modifiers::NONE,
        );
        assert!(
            feathered.incoming.coverage_at(IVec2::new(8, 20)) > 0.0,
            "a 4px feather reaches 2px outside the box"
        );
        assert!(partial(&feathered) > 0, "the feather is a ramp");
        assert!(matches!(
            tool.set_setting("feather", ToolSetting::Float(f32::NAN)),
            Err(ToolError::NotFinite { .. })
        ));

        // The lassos share the same edge controls.
        let mut lasso = LassoTool::new(LassoKind::Polygonal);
        lasso
            .set_setting("feather", ToolSetting::Float(4.0))
            .unwrap();
        lasso
            .set_setting("antialias", ToolSetting::Bool(false))
            .unwrap();
        for p in [(10.0, 10.0), (40.0, 10.0), (40.0, 40.0), (10.0, 40.0)] {
            lasso
                .on_pointer_down(&mut ctx, PointerEvent::at(p.0, p.1))
                .unwrap();
            lasso
                .on_pointer_up(&mut ctx, PointerEvent::at(p.0, p.1))
                .unwrap();
        }
        // Clicking the first vertex closes the outline.
        lasso
            .on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        let edit = &ctx.drain_selection()[0];
        assert!(
            edit.incoming.coverage_at(IVec2::new(8, 25)) > 0.0,
            "the lasso feather reaches outside its outline"
        );
    }

    #[test]
    fn wand_tolerance_contiguous_and_sample_merged_reach_the_flood() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        let composite = LayerId::new();
        paint(&mut tiles, PixelKey::Layer(layer), bands);
        // The "composite" is flat: judged against it, everything matches.
        paint(&mut tiles, PixelKey::Layer(composite), |_| 40);
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, W as u32, H as u32))
            .with_layer(layer);
        ctx.sample_from = Some(PixelKey::Layer(composite));
        let mut wand = WandTool::new(WandKind::Magic);
        wand.set_setting("antialias", ToolSetting::Bool(false))
            .unwrap();
        let seed = (4.0, 8.0);

        // Tolerance 0: exactly the seed's band, and nothing across the gap.
        wand.set_setting("tolerance", ToolSetting::Float(0.0))
            .unwrap();
        let tight = drag(&mut wand, &mut ctx, seed, seed, Modifiers::NONE);
        assert_eq!(
            covered(&tight),
            16 * H as usize,
            "tolerance 0 selects the seed band only"
        );
        // Tolerance 1: every pixel.
        wand.set_setting("tolerance", ToolSetting::Float(1.0))
            .unwrap();
        let loose = drag(&mut wand, &mut ctx, seed, seed, Modifiers::NONE);
        assert_eq!(
            covered(&loose),
            (W * H) as usize,
            "tolerance 1 selects everything"
        );

        // Contiguous off at tolerance 0: both dark bands, not the light one.
        wand.set_setting("tolerance", ToolSetting::Float(0.0))
            .unwrap();
        wand.set_setting("contiguous", ToolSetting::Bool(false))
            .unwrap();
        let global = drag(&mut wand, &mut ctx, seed, seed, Modifiers::NONE);
        assert_eq!(
            covered(&global),
            48 * H as usize,
            "non-contiguous reaches the disconnected dark band"
        );
        assert_eq!(global.incoming.coverage_at(IVec2::new(24, 8)), 0.0);

        // Sample All Layers: judged against the flat composite, tolerance 0
        // selects the whole canvas even though the layer has three bands.
        wand.set_setting("contiguous", ToolSetting::Bool(true))
            .unwrap();
        wand.set_setting("sample_merged", ToolSetting::Bool(true))
            .unwrap();
        let merged = drag(&mut wand, &mut ctx, seed, seed, Modifiers::NONE);
        assert_eq!(
            covered(&merged),
            (W * H) as usize,
            "sample_merged reads the composite"
        );

        // Anti-alias is a wand option too: with a tolerance that straddles
        // the band difference, the rim ramps when on and snaps when off.
        wand.set_setting("sample_merged", ToolSetting::Bool(false))
            .unwrap();
        wand.set_setting("tolerance", ToolSetting::Float(0.75))
            .unwrap();
        wand.set_setting("antialias", ToolSetting::Bool(true))
            .unwrap();
        let aa = drag(&mut wand, &mut ctx, seed, seed, Modifiers::NONE);
        wand.set_setting("antialias", ToolSetting::Bool(false))
            .unwrap();
        let hard = drag(&mut wand, &mut ctx, seed, seed, Modifiers::NONE);
        assert!(
            partial(&aa) > 0,
            "anti-alias on ramps the light band coverage"
        );
        assert_eq!(partial(&hard), 0, "anti-alias off snaps it");

        // Quick Selection answers its own keys and they reach the grow.
        let mut quick = WandTool::new(WandKind::Quick);
        quick
            .set_setting("radius", ToolSetting::Float(2.0))
            .unwrap();
        assert_eq!(quick.quick.radius, 2.0);
        quick
            .set_setting("tolerance", ToolSetting::Float(0.0))
            .unwrap();
        let tight = drag(
            &mut quick,
            &mut ctx,
            (2.0, 8.0),
            (10.0, 8.0),
            Modifiers::NONE,
        );
        quick
            .set_setting("tolerance", ToolSetting::Float(1.0))
            .unwrap();
        let loose = drag(
            &mut quick,
            &mut ctx,
            (2.0, 8.0),
            (10.0, 8.0),
            Modifiers::NONE,
        );
        assert!(
            covered(&loose) > covered(&tight),
            "quick select tolerance widens the region: {} vs {}",
            covered(&loose),
            covered(&tight)
        );
        assert!(
            matches!(
                quick.set_setting("contiguous", ToolSetting::Bool(false)),
                Err(ToolError::UnknownOption { .. })
            ),
            "Quick Selection declares no contiguous option"
        );
        assert!(
            matches!(
                wand.set_setting("radius", ToolSetting::Float(3.0)),
                Err(ToolError::UnknownOption { .. })
            ),
            "the Magic Wand declares no radius option"
        );
    }

    #[test]
    fn the_magnetic_lassos_width_and_contrast_land_on_the_snap_options() {
        let mut mag = LassoTool::new(LassoKind::Magnetic);
        let before = mag.magnetic;
        mag.set_setting("search_radius", ToolSetting::Int(64))
            .unwrap();
        mag.set_setting("edge_weight", ToolSetting::Float(2.5))
            .unwrap();
        assert_eq!(mag.magnetic.search_radius, 64);
        assert_eq!(mag.magnetic.edge_weight, 2.5);
        assert_ne!(mag.magnetic, before);
        // Clamped into the registry range rather than refused.
        mag.set_setting("search_radius", ToolSetting::Int(0))
            .unwrap();
        assert_eq!(mag.magnetic.search_radius, 1);
        assert!(matches!(
            mag.set_setting("search_radius", ToolSetting::Float(3.0)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
    }
}
