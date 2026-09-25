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
/// Subtract, Intersect, Exclude. A choice index is looked up here, so the
/// registry's spec ([`SELECTION_MODE_LABELS`]) and this table must agree.
///
/// W9-L: Exclude is the XOR — `selection::BooleanOp::Exclude`, which keeps
/// what exactly one of the old and the new selection covers. It has no
/// modifier chord (Shift adds, Alt subtracts, both intersect, as before), so
/// the Mode control is how it is reached.
pub const SELECTION_MODES: &[BooleanOp] = &[
    BooleanOp::Replace,
    BooleanOp::Add,
    BooleanOp::Subtract,
    BooleanOp::Intersect,
    BooleanOp::Exclude,
];

/// W9-L: the labels the registry's selection `mode` choice shows, index for
/// index with [`SELECTION_MODES`].
pub const SELECTION_MODE_LABELS: &[&str] = &["New", "Add", "Subtract", "Intersect", "Exclude"];

/// W9-L: the rectangular and elliptical marquees' **Style** choice, in the
/// registry's order.
pub const MARQUEE_STYLE_LABELS: &[&str] = &["Normal", "Fixed Ratio", "Fixed Size"];

/// The default Width / Height the marquee Style fields start at: a 1:1 ratio
/// under Fixed Ratio, a 64 x 64 px box under Fixed Size.
pub const MARQUEE_STYLE_DEFAULT_PX: f32 = 64.0;

/// The largest Width / Height the marquee Style fields accept.
pub const MARQUEE_STYLE_MAX_PX: f32 = 30_000.0;

/// W9-L: how a rectangular or elliptical marquee's drag is constrained — the
/// options bar's Style, Width and Height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarqueeStyle {
    /// The drag is the box (Shift squares it).
    Normal,
    /// The box keeps `width : height`, sized by the drag's larger extent.
    FixedRatio { width: f32, height: f32 },
    /// The box is exactly `width` x `height` pixels; the press places it and
    /// the drag only says which way from the press it opens.
    FixedSize { width: f32, height: f32 },
}

impl MarqueeStyle {
    /// The style a Style choice index and the two fields name. `None` for an
    /// index past [`MARQUEE_STYLE_LABELS`].
    pub fn from_choice(index: usize, width: f32, height: f32) -> Option<Self> {
        match index {
            0 => Some(MarqueeStyle::Normal),
            1 => Some(MarqueeStyle::FixedRatio { width, height }),
            2 => Some(MarqueeStyle::FixedSize { width, height }),
            _ => None,
        }
    }
}

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
    /// W9-L: the options bar's Style (`style`), with its Width
    /// (`style_width`) and Height (`style_height`). Read by the rectangle and
    /// the ellipse; the single-row/column marquees have no box to constrain.
    pub style: MarqueeStyle,
    style_index: usize,
    style_size: Vec2,
    anchor: Option<Vec2>,
    current: Option<Vec2>,
    /// W4-A: whether Shift was held on the last sample, so the published
    /// rubber band shows the square the release would make.
    shift: bool,
    /// W4-A: the canvas at pointer-down, for the single-row/column band.
    canvas: Option<raster::PixelRect>,
    op: BooleanOp,
}

impl MarqueeTool {
    pub fn new(shape: MarqueeShape) -> Self {
        Self {
            shape,
            from_center: false,
            options: SelectionOptions::default(),
            style: MarqueeStyle::Normal,
            style_index: 0,
            style_size: Vec2::splat(MARQUEE_STYLE_DEFAULT_PX),
            anchor: None,
            current: None,
            shift: false,
            canvas: None,
            op: BooleanOp::Replace,
        }
    }

    /// W4-A: the rubber band the release would select, `[min, max]` in
    /// document pixels: the constrained box for Rect/Ellipse, the one-pixel
    /// line across the canvas for the single-row/column marquees.
    fn band(&self) -> Option<[Vec2; 2]> {
        let (anchor, current) = (self.anchor?, self.current?);
        let rect = match self.shape {
            MarqueeShape::SingleRow => {
                let c = self.canvas?;
                let y = anchor.y.floor();
                [
                    Vec2::new(c.x as f32, y),
                    Vec2::new(c.right() as f32, y + 1.0),
                ]
            }
            MarqueeShape::SingleColumn => {
                let c = self.canvas?;
                let x = anchor.x.floor();
                [
                    Vec2::new(x, c.y as f32),
                    Vec2::new(x + 1.0, c.bottom() as f32),
                ]
            }
            MarqueeShape::Rect | MarqueeShape::Ellipse => {
                let (a, b) = self.corners(anchor, current, self.shift);
                [a.min(b), a.max(b)]
            }
        };
        (rect[0].is_finite() && rect[1].is_finite()).then_some(rect)
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
        // The side of the press the drag went to; a click with no drag
        // opens right and down.
        let side = |v: f32| if v < 0.0 { -1.0 } else { 1.0 };
        let raw = to - a;
        let d = match self.style {
            MarqueeStyle::Normal => {
                if shift {
                    // Constrain to a square, keeping the drag's dominant extent.
                    let s = raw.x.abs().max(raw.y.abs());
                    Vec2::new(s * raw.x.signum(), s * raw.y.signum())
                } else {
                    raw
                }
            }
            // W9-L: the larger extent (in ratio units) sizes the box, so the
            // box always reaches the pointer on one axis.
            MarqueeStyle::FixedRatio { width, height } => {
                let r = width / height;
                let w = raw.x.abs().max(raw.y.abs() * r);
                Vec2::new(w * side(raw.x), w / r * side(raw.y))
            }
            // W9-L: the size is the size; from the centre it is split about
            // the press rather than doubled.
            MarqueeStyle::FixedSize { width, height } => {
                let size = if self.from_center {
                    Vec2::new(width, height) * 0.5
                } else {
                    Vec2::new(width, height)
                };
                Vec2::new(size.x * side(raw.x), size.y * side(raw.y))
            }
        };
        if self.from_center {
            (a - d, a + d)
        } else {
            (a, a + d)
        }
    }

    /// W9-L: adopt one of the three Style keys. `Ok(false)` when `key` is
    /// not one of them.
    fn set_style(&mut self, key: &str, setting: ToolSetting) -> Result<bool, ToolError> {
        match (key, setting) {
            ("style", ToolSetting::Choice(i)) if i < MARQUEE_STYLE_LABELS.len() => {
                self.style_index = i;
            }
            ("style_width", ToolSetting::Float(v)) => {
                self.style_size.x = finite("style_width", v)?.clamp(0.01, MARQUEE_STYLE_MAX_PX);
            }
            ("style_height", ToolSetting::Float(v)) => {
                self.style_size.y = finite("style_height", v)?.clamp(0.01, MARQUEE_STYLE_MAX_PX);
            }
            ("style" | "style_width" | "style_height", _) => {
                return Err(ToolError::OptionKindMismatch {
                    key: key.to_owned(),
                })
            }
            _ => return Ok(false),
        }
        self.style =
            MarqueeStyle::from_choice(self.style_index, self.style_size.x, self.style_size.y)
                .unwrap_or(MarqueeStyle::Normal);
        Ok(true)
    }
}

impl Tool for MarqueeTool {
    fn id(&self) -> ToolId {
        self.shape.tool_id()
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("marquee anchor", event.pos)?;
        // Captured now: releasing shift halfway through a drag must not change
        // whether this gesture adds or replaces.
        self.op = gesture_op(self.options.mode, event.modifiers);
        self.anchor = Some(event.pos);
        self.current = Some(event.pos);
        self.shift = false;
        self.canvas = Some(ctx.canvas);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.anchor.is_some() {
            self.current = Some(event.pos);
            self.shift = event.modifiers.shift;
        }
        Ok(())
    }

    /// W4-A: the rubber band while the button is down; `None` once the
    /// release has emitted the selection or Escape dropped it.
    fn live_geometry(&self) -> Option<crate::tool::SessionGeometry> {
        Some(crate::tool::SessionGeometry::Marquee {
            shape: self.shape,
            rect: self.band()?,
        })
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

    /// Exactly the registry's `SELECTION_OPTS` keys, plus the W9-L Style
    /// keys the rectangular and elliptical marquees declare
    /// (`MARQUEE_OPTS`); anything else is refused.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        if self.options.set(key, setting)? || self.set_style(key, setting)? {
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

/// W16-A: how far, in screen pixels, a press may land from the previous press
/// and still close an open lasso outline as the second half of a double-click
/// (Photopea: "double-click to close"). The tool is handed no timestamps, so a
/// second press on the same spot is what it recognises; the view's zoom turns
/// the screen distance into document pixels.
pub const LASSO_DOUBLE_CLICK_PX: f32 = 4.0;

/// W16-A: the reserved setting the shell sends a lasso holding an open outline
/// when Backspace or Delete is pressed: the last point placed is removed, and
/// removing the only one ends the outline. It is not an options-bar key (no
/// registry spec names it); `Tool` offers no other object-safe door from a
/// `Box<dyn Tool>` to the lasso.
pub const LASSO_REMOVE_LAST_POINT: &str = "lasso_remove_last_point";

/// W16-A: Photoshop's default magnetic-lasso Frequency, which lays anchors the
/// [`MAGNETIC_ANCHOR_SPACING`] apart.
pub const MAGNETIC_FREQUENCY_DEFAULT: i32 = 57;

/// W16-A: the weakest edge (a normalised Sobel magnitude) a magnetic anchor
/// snaps to; on flatter ground the anchor stays where the pointer is.
const MAGNETIC_MIN_EDGE: f32 = 0.1;

/// W16-A: the anchor spacing a magnetic-lasso Frequency (`0..=100`) asks
/// for — a higher frequency lays anchors closer together. The default
/// ([`MAGNETIC_FREQUENCY_DEFAULT`]) is the [`MAGNETIC_ANCHOR_SPACING`] the
/// lasso has always used.
pub fn magnetic_spacing(frequency: i32) -> f32 {
    let f = frequency.clamp(0, 100) as f32;
    let per_step = (60.0 - MAGNETIC_ANCHOR_SPACING) / MAGNETIC_FREQUENCY_DEFAULT as f32;
    (60.0 - f * per_step).max(2.0)
}

/// The three lassos.
///
/// W16-A: the polygonal and magnetic lassos hold their outline open between
/// presses (Photopea): Enter ([`Tool::commit`]), a double-click, or a press
/// on the first point closes it; Backspace/Delete removes the last point
/// ([`LASSO_REMOVE_LAST_POINT`]); Escape drops it. The magnetic lasso lays
/// its anchors itself as the pointer moves — button down or not — each one
/// pulled onto the strongest edge within Width, a press adds one, and a drag
/// released back on its start still closes. Holding Alt while dragging the
/// freehand lasso draws a straight segment; released with Alt still held, the
/// outline stays open for straight segments, and letting Alt go closes it.
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
    /// W16-A: a freehand outline released with Alt held stays open, and the
    /// presses after it place straight segments.
    alt_poly: bool,
    /// W16-A: whether this freehand drag has seen Alt up. Alt held since the
    /// press means Subtract, not straight segments.
    alt_armed: bool,
    /// W16-A: the free end of the Alt-held straight segment, for the overlay.
    rubber: Option<Vec2>,
    /// W16-A: where the last press landed, with the double-click radius at
    /// its zoom — a second press there, with no travel in between, is the
    /// double-click that closes.
    last_press: Option<(Vec2, f32)>,
    /// W16-A: the magnetic lasso's anchor spacing (its Frequency).
    spacing: f32,
    /// W16-A: the pixels the magnetic lasso snaps its anchors onto, read once
    /// at the press that starts an outline.
    edge_image: Option<(raster::PixelRect, Vec<u8>)>,
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
            alt_poly: false,
            alt_armed: false,
            rubber: None,
            last_press: None,
            spacing: MAGNETIC_ANCHOR_SPACING,
            edge_image: None,
        }
    }

    /// The path so far, for the overlay.
    pub fn path(&self) -> &[Vec2] {
        &self.points
    }

    /// W16-A: the magnetic lasso's anchor spacing, in document pixels.
    pub fn anchor_spacing(&self) -> f32 {
        self.spacing
    }

    /// W16-A: an outline held open between presses — what Enter confirms and
    /// Backspace edits. A freehand outline is only open in its Alt mode.
    fn open_between_presses(&self) -> bool {
        !self.points.is_empty()
            && !self.dragging
            && (self.kind != LassoKind::Freehand || self.alt_poly)
    }

    fn reset(&mut self) {
        self.points.clear();
        self.dragging = false;
        self.alt_poly = false;
        self.alt_armed = false;
        self.rubber = None;
        self.last_press = None;
        self.edge_image = None;
    }

    /// W16-A: Backspace/Delete — drop the last point of an open outline.
    /// Removing the only point ends the outline. `false` when no outline is
    /// open between presses.
    pub fn remove_last_point(&mut self) -> bool {
        if !self.open_between_presses() {
            return false;
        }
        self.points.pop();
        self.last_press = None;
        if self.points.is_empty() {
            self.reset();
        }
        true
    }

    /// W16-A: pull a magnetic anchor onto the strongest edge within Width
    /// (`search_radius`, capped at 32 px) of `p`, weighted by Contrast
    /// (`edge_weight`) and a small pull back towards the pointer
    /// (`straight_weight`). With no pixels read, a zero Contrast, or no edge
    /// stronger than [`MAGNETIC_MIN_EDGE`] nearby, `p` is kept.
    fn snap_anchor(&self, p: Vec2) -> Vec2 {
        let Some((rect, px)) = &self.edge_image else {
            return p;
        };
        if self.magnetic.edge_weight <= 0.0 || rect.is_empty() {
            return p;
        }
        let (x0, y0) = (rect.x, rect.y);
        let (w, h) = (rect.width as i64, rect.height as i64);
        let lum = |x: i64, y: i64| -> f32 {
            let lx = (x - x0).clamp(0, w - 1);
            let ly = (y - y0).clamp(0, h - 1);
            let i = ((ly * w + lx) * 4) as usize;
            let Some(c) = px.get(i..i + 4) else {
                return 0.0;
            };
            let luma = 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
            luma / 255.0 * (c[3] as f32 / 255.0)
        };
        let r = self.magnetic.search_radius.clamp(1, 32) as i64;
        let (cx, cy) = (p.x.floor() as i64, p.y.floor() as i64);
        let mut best: Option<(f32, Vec2)> = None;
        for y in (cy - r).max(y0)..=(cy + r).min(y0 + h - 1) {
            for x in (cx - r).max(x0)..=(cx + r).min(x0 + w - 1) {
                let centre = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let d = (centre - p).length();
                if d > r as f32 {
                    continue;
                }
                let gx = (lum(x + 1, y - 1) + 2.0 * lum(x + 1, y) + lum(x + 1, y + 1))
                    - (lum(x - 1, y - 1) + 2.0 * lum(x - 1, y) + lum(x - 1, y + 1));
                let gy = (lum(x - 1, y + 1) + 2.0 * lum(x, y + 1) + lum(x + 1, y + 1))
                    - (lum(x - 1, y - 1) + 2.0 * lum(x, y - 1) + lum(x + 1, y - 1));
                let g = (gx * gx + gy * gy).sqrt() / 4.0;
                if g < MAGNETIC_MIN_EDGE {
                    continue;
                }
                let score = g * self.magnetic.edge_weight - self.magnetic.straight_weight * d;
                if best.is_none_or(|(s, _)| score > s) {
                    best = Some((score, centre));
                }
            }
        }
        best.map_or(p, |(_, q)| q)
    }

    /// W16-A: lay a magnetic anchor at `p` (snapped) when the pointer has
    /// travelled the Frequency's spacing from the last one.
    fn lay_anchor(&mut self, p: Vec2) {
        if self
            .points
            .last()
            .is_none_or(|last| (p - *last).length() >= self.spacing)
        {
            let anchor = self.snap_anchor(p);
            if self.points.last() != Some(&anchor) {
                self.points.push(anchor);
            }
        }
    }

    /// Close the outline and emit it — the Enter key, and what clicking the
    /// first vertex of a polygonal lasso does.
    pub fn close(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let pts = std::mem::take(&mut self.points);
        self.reset();
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

/// W16-A: the double-click distance in document pixels at `ctx`'s zoom.
fn double_click_doc_px(ctx: &ToolContext<'_>) -> f32 {
    let zoom = ctx.view.zoom;
    if zoom.is_finite() && zoom > 0.0 {
        LASSO_DOUBLE_CLICK_PX / zoom
    } else {
        LASSO_DOUBLE_CLICK_PX
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
        let pos = event.pos;
        if self.points.is_empty() {
            self.op = gesture_op(self.options.mode, event.modifiers);
        }
        // W16-A: an outline held open between presses. A press back on its
        // first point, or a second press on the last one (a double-click),
        // closes it; any other press adds to it.
        if self.open_between_presses() {
            let double = self
                .last_press
                .is_some_and(|(q, r)| (pos - q).length() <= r) && false /*RVMUT*/;
            let on_start =
                self.points.len() >= 3 && (pos - self.points[0]).length() <= POLYGON_CLOSE_PX;
            if double || on_start {
                return self.close(ctx);
            }
            self.last_press = Some((pos, double_click_doc_px(ctx)));
            match self.kind {
                LassoKind::Polygonal => self.points.push(pos),
                LassoKind::Magnetic => {
                    let anchor = self.snap_anchor(pos);
                    self.points.push(anchor);
                    self.dragging = true;
                }
                LassoKind::Freehand => {
                    self.points.push(pos);
                    self.dragging = true;
                    self.alt_armed = true;
                }
            }
            return Ok(());
        }
        // A fresh outline.
        self.reset();
        self.last_press = Some((pos, double_click_doc_px(ctx)));
        match self.kind {
            LassoKind::Polygonal => self.points.push(pos),
            LassoKind::Magnetic => {
                self.edge_image = ctx
                    .sample_key()
                    .ok()
                    .and_then(|key| read_rgba8(ctx.tiles, key, ctx.canvas).ok())
                    .map(|px| (ctx.canvas, px));
                let anchor = self.snap_anchor(pos);
                self.points.push(anchor);
                self.dragging = true;
            }
            LassoKind::Freehand => {
                self.points.push(pos);
                self.dragging = true;
                self.alt_armed = !event.modifiers.alt;
            }
        }
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let pos = event.pos;
        // W16-A: travel between two presses means they are not a double-click.
        if self.last_press.is_some_and(|(q, r)| (pos - q).length() > r) {
            self.last_press = None;
        }
        match self.kind {
            // The painter draws the rubber segment to the pointer itself.
            LassoKind::Polygonal => Ok(()),
            // A magnetic lasso wants sparse anchors: the snap runs between
            // them, and one anchor per pointer sample would pin the path to
            // every wobble of the hand. W16-A: with the button up too, once
            // an outline is open — Photopea's click, then move.
            LassoKind::Magnetic => {
                if self.dragging || self.open_between_presses() {
                    crate::error::finite_pt("lasso point", pos)?;
                    self.lay_anchor(pos);
                }
                Ok(())
            }
            LassoKind::Freehand => {
                if self.dragging {
                    crate::error::finite_pt("lasso point", pos)?;
                    // W16-A: Alt pressed during the drag draws a straight
                    // segment from the last point to the pointer.
                    if event.modifiers.alt && self.alt_armed {
                        self.rubber = Some(pos);
                    } else {
                        if !event.modifiers.alt {
                            self.alt_armed = true;
                        }
                        self.rubber = None;
                        self.points.push(pos);
                    }
                    Ok(())
                } else if self.alt_poly && !self.points.is_empty() {
                    crate::error::finite_pt("lasso point", pos)?;
                    // W16-A: letting Alt go closes the straight-segment
                    // outline (Photoshop's lasso), as Enter would.
                    if event.modifiers.alt {
                        self.rubber = Some(pos);
                        Ok(())
                    } else {
                        self.close(ctx)
                    }
                } else {
                    Ok(())
                }
            }
        }
    }

    /// W4-A: the outline so far, while there is one. A freehand lasso closes
    /// on release, so it is published closed; W16-A: a polygonal or
    /// magnetic outline — and a freehand one in its Alt mode — is open until
    /// Enter, a double-click or a press on its first point closes it.
    fn live_geometry(&self) -> Option<crate::tool::SessionGeometry> {
        if self.points.is_empty() {
            return None;
        }
        let mut points = self.points.clone();
        points.extend(self.rubber);
        Some(crate::tool::SessionGeometry::Lasso {
            points,
            closed: self.kind == LassoKind::Freehand && !self.alt_poly,
        })
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if !self.dragging {
            // A polygonal lasso's release is not the end of anything.
            return Ok(());
        }
        match self.kind {
            LassoKind::Polygonal => Ok(()),
            LassoKind::Magnetic => {
                self.dragging = false;
                // W16-A: a drag released back on its start closes; any other
                // release leaves the outline open for the pointer to extend.
                if self.points.len() >= 3
                    && (event.pos - self.points[0]).length() <= POLYGON_CLOSE_PX
                {
                    return self.close(ctx);
                }
                Ok(())
            }
            LassoKind::Freehand => {
                crate::error::finite_pt("lasso point", event.pos)?;
                self.points.push(event.pos);
                if event.modifiers.alt && self.alt_armed {
                    // W16-A: released with Alt held — the outline stays open
                    // and the next presses place straight segments.
                    self.dragging = false;
                    self.rubber = None;
                    self.alt_poly = true;
                    Ok(())
                } else {
                    self.close(ctx)
                }
            }
        }
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.reset();
    }

    /// W16-A: Enter closes an outline held open between presses.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        if false /*RVMUT*/ && self.open_between_presses() {
            self.close(ctx)
        } else {
            Ok(())
        }
    }

    fn has_pending_commit(&self) -> bool {
        self.open_between_presses()
    }

    /// The shared selection keys plus the magnetic lasso's two snap controls
    /// (the registry's `search_radius` "Width" and `edge_weight` "Contrast"),
    /// which `lasso_magnetic` reads out of [`LassoTool::magnetic`] at close.
    /// W16-A: plus the magnetic lasso's `frequency` and the shell's
    /// [`LASSO_REMOVE_LAST_POINT`].
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        if self.options.set(key, setting)? {
            return Ok(());
        }
        match (key, setting) {
            (LASSO_REMOVE_LAST_POINT, ToolSetting::Bool(true)) => {
                self.remove_last_point();
                Ok(())
            }
            ("search_radius", ToolSetting::Int(v)) => {
                self.magnetic.search_radius = v.clamp(1, 256) as u32;
                Ok(())
            }
            ("edge_weight", ToolSetting::Float(v)) => {
                self.magnetic.edge_weight = finite("edge weight", v)?.clamp(0.0, 4.0);
                Ok(())
            }
            ("frequency", ToolSetting::Int(v)) if self.kind == LassoKind::Magnetic => {
                self.spacing = magnetic_spacing(v);
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

/// W16-A: a press inside the selection with a selection tool, in New mode and
/// with no modifier held, drags the selection OUTLINE (Photopea) — the pixels
/// stay. This is the gesture's arithmetic; `app-shell` routes the pointer to
/// it and lands the moved outline as one `SetSelection` step on release.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutlineDrag {
    press: Vec2,
    offset: IVec2,
}

impl OutlineDrag {
    /// Whether a press at `pos` starts an outline drag: something is
    /// selected under the pointer, no modifier is held (Shift adds, Alt
    /// subtracts, Ctrl lends the Move tool) and the options bar's Mode is
    /// New.
    pub fn begins(selection: &Selection, pos: Vec2, modifiers: Modifiers, mode: BooleanOp) -> bool {
        mode == BooleanOp::Replace
            && modifiers == Modifiers::NONE
            && !selection.is_none()
            && pos.is_finite()
            && selection.coverage_at(IVec2::new(pos.x.floor() as i32, pos.y.floor() as i32)) > 0.0
    }

    pub fn new(press: Vec2) -> Self {
        Self {
            press,
            offset: IVec2::ZERO,
        }
    }

    /// Follow the pointer to `pos`: the offset is whole pixels, and Shift
    /// constrains it to a multiple of 45 degrees.
    pub fn drag_to(&mut self, pos: Vec2, shift: bool) -> IVec2 {
        if !pos.is_finite() {
            return self.offset;
        }
        let d = pos - self.press;
        let d = if shift { constrain_to_45(d) } else { d };
        self.offset = IVec2::new(d.x.round() as i32, d.y.round() as i32);
        self.offset
    }

    /// The whole-pixel offset so far.
    pub fn offset(&self) -> IVec2 {
        self.offset
    }

    /// Where the press landed.
    pub fn press(&self) -> Vec2 {
        self.press
    }
}

/// W16-A: `d` projected onto the nearest multiple of 45 degrees.
pub fn constrain_to_45(d: Vec2) -> Vec2 {
    if d == Vec2::ZERO || !d.is_finite() {
        return d;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let angle = (d.y.atan2(d.x) / step).round() * step;
    let dir = Vec2::new(angle.cos(), angle.sin());
    // Exact zeros on the axes, so a constrained drag never drifts a pixel.
    let dir = Vec2::new(
        if dir.x.abs() < 1e-4 { 0.0 } else { dir.x },
        if dir.y.abs() < 1e-4 { 0.0 } else { dir.y },
    );
    dir * d.dot(dir) / dir.length_squared()
}

/// W16-A: `selection` moved by whole pixels `by` — a rectangle stays a
/// rectangle; a mask is resampled nearest-neighbour, so no coverage changes.
pub fn translate_selection(
    selection: &Selection,
    canvas: raster::PixelRect,
    by: IVec2,
) -> Result<Selection, ToolError> {
    Ok(match selection {
        Selection::None => Selection::None,
        Selection::Rect { min, max } => Selection::Rect {
            min: *min + by,
            max: *max + by,
        },
        mask => {
            let canvas = selection::Rect::from_xywh(
                canvas.x.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                canvas.y.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                canvas.width,
                canvas.height,
            );
            selection::transform_selection(
                mask,
                canvas,
                glam::Affine2::from_translation(by.as_vec2()),
                selection::transform::ResampleFilter::Nearest,
            )?
        }
    })
}

/// W16-A: the options bar's Mode as a selection tool's settings carry it
/// (the registry's `mode` choice), `New` when absent.
pub fn mode_of_settings(settings: &[(String, ToolSetting)]) -> BooleanOp {
    settings
        .iter()
        .find_map(|(key, setting)| match (key.as_str(), setting) {
            ("mode", ToolSetting::Choice(i)) => SELECTION_MODES.get(*i).copied(),
            _ => None,
        })
        .unwrap_or(BooleanOp::Replace)
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

/// W9-L: the selection XOR and the marquee Style, driven the way the shell
/// drives them — the registry builds the tool, the options bar's values
/// arrive through `set_setting` under the registry's own keys, and a real
/// press / drag / release produces the edit that is folded into the
/// document's selection.
#[cfg(test)]
mod w9l_tests {
    use super::*;
    use crate::registry;
    use crate::tiles::MemoryTiles;
    use raster::PixelRect;

    const SIDE: u32 = 128;

    /// One gesture on a fresh registry-built tool holding `settings`.
    fn gesture(
        id: ToolId,
        settings: &[(&str, ToolSetting)],
        from: (f32, f32),
        to: (f32, f32),
    ) -> SelectionEdit {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, SIDE, SIDE));
        let mut tool = registry::make(id);
        for (key, value) in settings {
            tool.set_setting(key, *value)
                .unwrap_or_else(|e| panic!("{id:?} refused {key}: {e}"));
        }
        tool.on_pointer_down(&mut ctx, PointerEvent::at(from.0, from.1))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(to.0, to.1))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(to.0, to.1))
            .unwrap();
        let mut edits = ctx.drain_selection();
        assert_eq!(edits.len(), 1, "one gesture, one edit");
        edits.pop().unwrap()
    }

    fn bounds(edit: &SelectionEdit) -> (IVec2, IVec2) {
        edit.incoming.bounds().expect("a non-empty selection")
    }

    fn choice_of(id: ToolId, key: &str, label: &str) -> usize {
        let spec = registry::info(id)
            .and_then(|i| i.options.iter().find(|o| o.key == key))
            .unwrap_or_else(|| panic!("{id:?} declares no {key}"));
        match spec.kind {
            crate::registry::OptionKind::Choice { choices, .. } => choices
                .iter()
                .position(|c| *c == label)
                .unwrap_or_else(|| panic!("{key} has no {label:?}: {choices:?}")),
            other => panic!("{key} is not a choice: {other:?}"),
        }
    }

    #[test]
    fn the_exclude_mode_xors_a_marquee_with_the_selection_it_meets() {
        // Every selection tool's Mode offers it, at the index the tools
        // crate's own table maps to Exclude.
        for id in [
            ToolId::RectMarquee,
            ToolId::EllipseMarquee,
            ToolId::SingleRowMarquee,
            ToolId::SingleColumnMarquee,
            ToolId::Lasso,
            ToolId::PolygonalLasso,
            ToolId::MagneticLasso,
            ToolId::MagicWand,
            ToolId::QuickSelect,
        ] {
            let index = choice_of(id, "mode", "Exclude");
            assert_eq!(SELECTION_MODES[index], BooleanOp::Exclude, "{id:?}");
            assert!(
                registry::make(id)
                    .set_setting("mode", ToolSetting::Choice(index))
                    .is_ok(),
                "{id:?} refuses its own Exclude"
            );
        }
        let exclude = choice_of(ToolId::RectMarquee, "mode", "Exclude");
        let edit = gesture(
            ToolId::RectMarquee,
            &[("mode", ToolSetting::Choice(exclude))],
            (40.0, 40.0),
            (80.0, 80.0),
        );
        assert_eq!(edit.op, BooleanOp::Exclude);
        // Folded into an existing 20..60 box, the way the shell folds it.
        let base = Selection::Rect {
            min: IVec2::new(20, 20),
            max: IVec2::new(60, 60),
        };
        let canvas = selection::Rect::from_xywh(0, 0, SIDE, SIDE);
        let out = edit.apply(canvas, &base).unwrap();
        let at = |x: i32, y: i32| out.coverage_at(IVec2::new(x, y));
        assert_eq!(at(30, 30), 1.0, "only the old box: kept");
        assert_eq!(at(70, 70), 1.0, "only the new box: added");
        assert_eq!(at(50, 50), 0.0, "both: taken out (the XOR)");
        assert_eq!(at(10, 10), 0.0, "neither: empty");
    }

    #[test]
    fn fixed_ratio_keeps_w_to_h_whatever_the_drag_and_normal_is_the_drag() {
        let ratio = choice_of(ToolId::RectMarquee, "style", "Fixed Ratio");
        // Normal: the drag is the box.
        let normal = gesture(ToolId::RectMarquee, &[], (10.0, 10.0), (30.0, 50.0));
        assert_eq!(bounds(&normal), (IVec2::new(10, 10), IVec2::new(30, 50)));
        // 2 : 1 — a drag 20 wide and 40 tall is sized by its height in ratio
        // units: 80 x 40.
        let settings = [
            ("style", ToolSetting::Choice(ratio)),
            ("style_width", ToolSetting::Float(2.0)),
            ("style_height", ToolSetting::Float(1.0)),
        ];
        let edit = gesture(ToolId::RectMarquee, &settings, (10.0, 10.0), (30.0, 50.0));
        assert_eq!(bounds(&edit), (IVec2::new(10, 10), IVec2::new(90, 50)));
        // Dragged up and left, it opens up and left.
        let edit = gesture(ToolId::RectMarquee, &settings, (100.0, 100.0), (60.0, 90.0));
        assert_eq!(bounds(&edit), (IVec2::new(60, 80), IVec2::new(100, 100)));
        // The ellipse is constrained the same way.
        let edit = gesture(
            ToolId::EllipseMarquee,
            &settings,
            (10.0, 10.0),
            (30.0, 50.0),
        );
        let (min, max) = bounds(&edit);
        assert_eq!((max - min).x, 2 * (max - min).y, "{min:?}..{max:?}");
    }

    #[test]
    fn fixed_size_places_a_box_of_exactly_that_size_at_the_press() {
        let fixed = choice_of(ToolId::RectMarquee, "style", "Fixed Size");
        let settings = [
            ("style", ToolSetting::Choice(fixed)),
            ("style_width", ToolSetting::Float(30.0)),
            ("style_height", ToolSetting::Float(20.0)),
        ];
        // A click with no drag, and a long drag: the same 30 x 20.
        let click = gesture(ToolId::RectMarquee, &settings, (5.0, 5.0), (5.0, 5.0));
        assert_eq!(bounds(&click), (IVec2::new(5, 5), IVec2::new(35, 25)));
        let drag = gesture(ToolId::RectMarquee, &settings, (5.0, 5.0), (120.0, 90.0));
        assert_eq!(bounds(&drag), (IVec2::new(5, 5), IVec2::new(35, 25)));
        // Released up-left of the press, the box opens that way.
        let back = gesture(ToolId::RectMarquee, &settings, (60.0, 60.0), (10.0, 10.0));
        assert_eq!(bounds(&back), (IVec2::new(30, 40), IVec2::new(60, 60)));
        // A size of the wrong kind is refused, not dropped.
        assert!(matches!(
            registry::make(ToolId::RectMarquee).set_setting("style_width", ToolSetting::Bool(true)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
    }
}

/// W11-G: the Object Selection tool — drag a rectangle round an object.
///
/// The tool itself only records the rectangle: its release emits ONE
/// [`SelectionEdit`] whose `incoming` is the dragged [`Selection::Rect`] and
/// whose `op` is the gesture's mode (Shift adds, Alt subtracts, both
/// intersect, otherwise the options bar's Mode). The rectangle is not the
/// selection: the shell (`app_shell::tool_input`) recognises
/// [`ToolId::ObjectSelection`], hands the rectangle and the active layer's
/// pixels to `selection::select_object` (GrabCut initialised from the
/// rectangle) on a job worker, and folds the object's mask in with `op` as one
/// `SetSelection` step. A click, or a drag narrower than a pixel, emits
/// nothing.
#[derive(Debug, Default)]
pub struct ObjectSelectionTool {
    /// The options bar's Mode.
    pub mode: BooleanOp,
    anchor: Option<Vec2>,
    current: Option<Vec2>,
    op: BooleanOp,
}

impl ObjectSelectionTool {
    /// The rectangle a press at `a` and a release at `b` name, in whole
    /// document pixels, or `None` when it has no area.
    pub fn rect_of(a: Vec2, b: Vec2) -> Option<(IVec2, IVec2)> {
        let min = a.min(b).floor().as_ivec2();
        let max = a.max(b).ceil().as_ivec2();
        (max.x - min.x >= 1 && max.y - min.y >= 1 && a.is_finite() && b.is_finite())
            .then_some((min, max))
    }
}

impl Tool for ObjectSelectionTool {
    fn id(&self) -> ToolId {
        ToolId::ObjectSelection
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("object selection corner", event.pos)?;
        self.op = gesture_op(self.mode, event.modifiers);
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

    /// The rubber band while the button is down, drawn as the rectangular
    /// marquee's.
    fn live_geometry(&self) -> Option<crate::tool::SessionGeometry> {
        let (a, b) = (self.anchor?, self.current?);
        (a.is_finite() && b.is_finite()).then(|| crate::tool::SessionGeometry::Marquee {
            shape: MarqueeShape::Rect,
            rect: [a.min(b), a.max(b)],
        })
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
        crate::error::finite_pt("object selection corner", event.pos)?;
        if let Some((min, max)) = Self::rect_of(anchor, event.pos) {
            ctx.emit_selection(SelectionEdit::new(Selection::Rect { min, max }, self.op));
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor = None;
        self.current = None;
    }

    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("mode", ToolSetting::Choice(i)) => {
                self.mode = mode_from_choice(key, i)?;
                Ok(())
            }
            ("mode", _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(unknown(key)),
        }
    }

    fn set_choice(&mut self, key: &str, index: usize) {
        let _ = self.set_setting(key, ToolSetting::Choice(index));
    }

    fn is_active(&self) -> bool {
        self.anchor.is_some()
    }
}

#[cfg(test)]
mod object_selection_tests {
    use super::*;
    use crate::registry;
    use crate::tiles::MemoryTiles;
    use raster::PixelRect;

    fn drag(mods: Modifiers, from: (f32, f32), to: (f32, f32)) -> Vec<SelectionEdit> {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
        let mut tool = registry::make(ToolId::ObjectSelection);
        let at = |p: (f32, f32)| PointerEvent::at(p.0, p.1).with_modifiers(mods);
        tool.on_pointer_down(&mut ctx, at(from)).unwrap();
        tool.on_pointer_move(&mut ctx, at(to)).unwrap();
        assert!(tool.live_geometry().is_some(), "the rubber band shows");
        tool.on_pointer_up(&mut ctx, at(to)).unwrap();
        assert!(tool.live_geometry().is_none());
        ctx.drain_selection()
    }

    #[test]
    fn a_drag_emits_the_rectangle_with_the_gesture_mode_and_a_click_nothing() {
        let edits = drag(Modifiers::NONE, (40.5, 30.2), (10.2, 8.9));
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].incoming,
            Selection::Rect {
                min: IVec2::new(10, 8),
                max: IVec2::new(41, 31),
            }
        );
        assert_eq!(edits[0].op, BooleanOp::Replace);
        assert_eq!(
            drag(Modifiers::shift(), (1.0, 1.0), (9.0, 9.0))[0].op,
            BooleanOp::Add
        );
        assert_eq!(
            drag(Modifiers::alt(), (1.0, 1.0), (9.0, 9.0))[0].op,
            BooleanOp::Subtract
        );
        assert!(drag(Modifiers::NONE, (5.0, 5.0), (5.0, 5.0)).is_empty());
        assert!(registry::make(ToolId::ObjectSelection)
            .set_setting("tolerance", ToolSetting::Float(0.5))
            .is_err());
    }
}

/// W16-A: the lassos' open outline (Enter, double-click, Backspace, the
/// magnetic lasso's click-then-move anchors) and the outline drag's
/// arithmetic, on the tools themselves. The shell routes are proved in
/// the `select_w16_shell_tests` of `app-shell`.
#[cfg(test)]
mod w16a_tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use layer_model::LayerId;
    use raster::PixelRect;

    /// A 64x64 layer: dark, then light from x = 16 to 31, then dark — two
    /// vertical edges for the magnetic lasso.
    fn banded(tiles: &mut MemoryTiles, key: PixelKey) {
        let ts = raster::TILE_SIZE as usize;
        let mut data = vec![0u8; ts * ts * 4];
        for y in 0..64usize {
            for x in 0..64usize {
                let g = if (16..32).contains(&x) { 220 } else { 40 };
                let i = (y * ts + x) * 4;
                data[i..i + 4].copy_from_slice(&[g, g, g, 255]);
            }
        }
        tiles.put(key, raster::TileCoord::new(0, 0, 0), data);
    }

    fn at(x: f32, y: f32) -> PointerEvent {
        PointerEvent::at(x, y)
    }

    #[test]
    fn a_polygonal_outline_closes_on_enter_or_a_double_click_and_backspace_edits_it() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
        let mut poly = LassoTool::new(LassoKind::Polygonal);
        for (x, y) in [(10.0, 10.0), (50.0, 10.0), (50.0, 50.0), (60.0, 60.0)] {
            poly.on_pointer_down(&mut ctx, at(x, y)).unwrap();
            poly.on_pointer_up(&mut ctx, at(x, y)).unwrap();
        }
        assert!(poly.has_pending_commit(), "an open outline is Enter's");
        poly.set_setting(LASSO_REMOVE_LAST_POINT, ToolSetting::Bool(true))
            .unwrap();
        assert_eq!(
            poly.path().len(),
            3,
            "Backspace did not drop the last point"
        );
        poly.on_pointer_down(&mut ctx, at(10.0, 50.0)).unwrap();
        poly.on_pointer_up(&mut ctx, at(10.0, 50.0)).unwrap();
        assert!(ctx.selection_edits().is_empty());
        poly.commit(&mut ctx).unwrap();
        let edits = ctx.drain_selection();
        assert_eq!(edits.len(), 1, "Enter did not close the outline");
        assert!(edits[0].incoming.coverage_at(IVec2::new(30, 30)) > 0.5);
        assert_eq!(edits[0].incoming.coverage_at(IVec2::new(55, 40)), 0.0);
        assert!(!poly.has_pending_commit() && !poly.is_active());

        // A double-click: the second press on the first, with no travel.
        for (x, y) in [(10.0, 10.0), (50.0, 10.0), (50.0, 50.0), (10.0, 50.0)] {
            poly.on_pointer_down(&mut ctx, at(x, y)).unwrap();
            poly.on_pointer_up(&mut ctx, at(x, y)).unwrap();
        }
        poly.on_pointer_down(&mut ctx, at(10.5, 50.5)).unwrap();
        assert_eq!(
            ctx.drain_selection().len(),
            1,
            "a double-click did not close"
        );

        // Travel between the two presses: not a double-click.
        for (x, y) in [(10.0, 10.0), (50.0, 10.0), (50.0, 50.0)] {
            poly.on_pointer_down(&mut ctx, at(x, y)).unwrap();
            poly.on_pointer_up(&mut ctx, at(x, y)).unwrap();
        }
        poly.on_pointer_move(&mut ctx, at(30.0, 60.0)).unwrap();
        poly.on_pointer_move(&mut ctx, at(50.0, 50.0)).unwrap();
        poly.on_pointer_down(&mut ctx, at(50.0, 50.0)).unwrap();
        assert!(ctx.selection_edits().is_empty(), "a slow re-click closed");
        assert_eq!(poly.path().len(), 4);
    }

    #[test]
    fn the_magnetic_lasso_lays_edge_snapped_anchors_as_the_pointer_moves() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        banded(&mut tiles, PixelKey::Layer(layer));
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64)).with_layer(layer);
        let mut mag = LassoTool::new(LassoKind::Magnetic);
        // Click (press + release) three pixels left of the x = 16 edge.
        mag.on_pointer_down(&mut ctx, at(13.0, 4.0)).unwrap();
        mag.on_pointer_up(&mut ctx, at(13.0, 4.0)).unwrap();
        assert!(mag.has_pending_commit(), "the release closed the outline");
        // Move with the button up, down the column.
        for y in [10.0, 18.0, 26.0, 34.0, 42.0] {
            mag.on_pointer_move(&mut ctx, at(13.0, y)).unwrap();
        }
        let path = mag.path().to_vec();
        assert!(path.len() >= 3, "no anchors were laid: {path:?}");
        for p in &path {
            assert!(
                (p.x - 16.0).abs() <= 1.0,
                "anchor {p:?} was not pulled onto the x = 16 edge"
            );
        }
        // Contrast 0 turns the pull off: the anchor stays at the pointer.
        mag.set_setting("edge_weight", ToolSetting::Float(0.0))
            .unwrap();
        mag.on_pointer_move(&mut ctx, at(13.0, 60.0)).unwrap();
        assert_eq!(mag.path().last(), Some(&Vec2::new(13.0, 60.0)));
        // Frequency: 100 lays anchors two pixels apart, 0 sixty.
        assert_eq!(mag.anchor_spacing(), MAGNETIC_ANCHOR_SPACING);
        mag.set_setting("frequency", ToolSetting::Int(100)).unwrap();
        assert_eq!(mag.anchor_spacing(), 2.0);
        assert_eq!(magnetic_spacing(0), 60.0);
        assert!(
            (magnetic_spacing(MAGNETIC_FREQUENCY_DEFAULT) - MAGNETIC_ANCHOR_SPACING).abs() < 1e-3
        );
        assert!(LassoTool::new(LassoKind::Polygonal)
            .set_setting("frequency", ToolSetting::Int(50))
            .is_err());
        // Enter closes.
        mag.on_pointer_move(&mut ctx, at(30.0, 60.0)).unwrap();
        mag.commit(&mut ctx).unwrap();
        assert_eq!(ctx.drain_selection().len(), 1);
        assert!(!mag.is_active());
    }

    #[test]
    fn the_outline_drag_starts_only_inside_in_new_mode_and_shift_takes_45_degrees() {
        let sel = Selection::Rect {
            min: IVec2::new(10, 10),
            max: IVec2::new(30, 30),
        };
        let inside = Vec2::new(20.0, 20.0);
        let replace = BooleanOp::Replace;
        assert!(OutlineDrag::begins(&sel, inside, Modifiers::NONE, replace));
        let outside = Vec2::new(40.0, 40.0);
        assert!(!OutlineDrag::begins(
            &sel,
            outside,
            Modifiers::NONE,
            replace
        ));
        assert!(!OutlineDrag::begins(
            &sel,
            inside,
            Modifiers::shift(),
            replace
        ));
        assert!(!OutlineDrag::begins(
            &sel,
            inside,
            Modifiers::alt(),
            replace
        ));
        assert!(!OutlineDrag::begins(
            &sel,
            inside,
            Modifiers::NONE,
            BooleanOp::Add
        ));
        assert!(!OutlineDrag::begins(
            &Selection::None,
            inside,
            Modifiers::NONE,
            replace
        ));

        let mut drag = OutlineDrag::new(inside);
        assert_eq!(drag.drag_to(Vec2::new(27.4, 24.6), false), IVec2::new(7, 5));
        assert_eq!(drag.drag_to(Vec2::new(32.0, 23.0), true), IVec2::new(12, 0));
        assert_eq!(drag.drag_to(Vec2::new(28.0, 30.0), true), IVec2::new(9, 9));
        assert_eq!(drag.drag_to(Vec2::new(19.0, 6.0), true), IVec2::new(0, -14));

        let canvas = PixelRect::new(0, 0, 64, 64);
        assert_eq!(
            translate_selection(&sel, canvas, IVec2::new(3, -2)).unwrap(),
            Selection::Rect {
                min: IVec2::new(13, 8),
                max: IVec2::new(33, 28),
            }
        );
        let mask = Selection::Mask(
            ellipse_subpixel(Vec2::new(10.0, 10.0), Vec2::new(30.0, 30.0)).unwrap(),
        );
        let moved = translate_selection(&mask, canvas, IVec2::new(5, 5)).unwrap();
        assert_eq!(
            moved.coverage_at(IVec2::new(25, 25)),
            mask.coverage_at(IVec2::new(20, 20)),
            "a moved mask changed coverage"
        );
        assert_eq!(moved.coverage_at(IVec2::new(11, 20)), 0.0);
        assert_eq!(
            mode_of_settings(&[("mode".to_string(), ToolSetting::Choice(1))]),
            BooleanOp::Add
        );
        assert_eq!(mode_of_settings(&[]), BooleanOp::Replace);
    }
}
