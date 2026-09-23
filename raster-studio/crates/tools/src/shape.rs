//! Shape tools, driven by `vector`.
//!
//! One tool, seven shapes, two commit modes.
//!
//! The commit modes matter more than the shapes. **Vector mode** creates a
//! shape layer holding the path, so the shape stays editable forever — that is
//! the product's central invariant, and a shape tool that could only rasterise
//! would break it. **Rasterise mode** fills the path into the active layer's
//! pixels, which is what you want when the shape is a mask, a texture element,
//! or something you are about to paint over. The same [`vector::Path`] feeds
//! both, so the two modes cannot drift apart.
//!
//! # Paint
//!
//! What a shape is painted *with* is a [`ShapePaint`]: a fill and a stroke,
//! each taken from nowhere, from the foreground colour, or from a colour of
//! its own, plus a stroke width in document pixels. The same struct answers
//! the pen's options, so a pen path and a rectangle are painted by one rule
//! and the two tools' options bars read the same. Every colour a layer stores
//! is straight-alpha sRGB (the document's space, which the compositor
//! decodes); the foreground reaches a tool in linear space and is encoded on
//! the way in, and a custom colour is decoded on the way to a pixel.

use color::{linear_to_srgb3, premultiply, srgb_to_linear3};
use editor_core::{Command, Selection};
use glam::{IVec2, Vec2};
use layer_model::{Layer, LayerKind, ShapeLayer, ShapeStroke};
use raster::PixelRect;
use vector::{
    fill::{fill, FillOptions},
    mask::PixelRect as VecRect,
    point, shapes,
    stroke::{stroke, StrokeStyle},
    to_svg, CornerRadii, CustomShape, Path,
};

use crate::error::ToolError;
use crate::gradient::constrain_45;
use crate::patch::{mask_coverage_of, ColorPatch, CoveragePatch};
use crate::tool::{PaintTarget, PointerEvent, Tool, ToolContext, ToolId, ToolSetting};

/// The shapes the tool can draw.
#[derive(Debug, Clone, PartialEq)]
pub enum ShapeKind {
    Rectangle,
    RoundedRectangle {
        radius: f64,
    },
    Ellipse,
    Polygon {
        sides: u32,
    },
    Star {
        points: u32,
        inner_ratio: f64,
    },
    Line {
        width: f64,
    },
    /// Any path, scaled into the drag box — the custom-shape library.
    Custom {
        path: Path,
        name: String,
    },
}

impl ShapeKind {
    pub fn tool_id(&self) -> ToolId {
        match self {
            ShapeKind::Rectangle => ToolId::Rectangle,
            ShapeKind::RoundedRectangle { .. } => ToolId::RoundedRectangle,
            ShapeKind::Ellipse => ToolId::Ellipse,
            ShapeKind::Polygon { .. } => ToolId::Polygon,
            ShapeKind::Star { .. } => ToolId::Star,
            ShapeKind::Line { .. } => ToolId::Line,
            ShapeKind::Custom { .. } => ToolId::CustomShape,
        }
    }

    /// The library entry the Custom Shape tool starts on.
    pub fn custom(shape: CustomShape) -> Self {
        ShapeKind::Custom {
            path: shape.path(),
            name: shape.name().to_owned(),
        }
    }
}

/// What a finished shape becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShapeMode {
    /// A new shape layer holding the path — still editable afterwards.
    #[default]
    VectorLayer,
    /// Coverage filled into the active layer's pixels.
    Rasterize,
}

impl ShapeMode {
    /// The mode a registry `mode` choice index names: `0` is "Shape Layer",
    /// anything else is "Rasterize" — the registry's two-entry list, in its
    /// order.
    pub fn from_choice(index: usize) -> Self {
        if index == 0 {
            ShapeMode::VectorLayer
        } else {
            ShapeMode::Rasterize
        }
    }
}

/// Where a fill or a stroke takes its colour from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaintSource {
    /// Not painted at all.
    None,
    /// The foreground colour at the moment the shape commits.
    #[default]
    Foreground,
    /// The options bar's own swatch for this paint.
    Custom,
}

impl PaintSource {
    /// The `Choice` labels, in the order [`Self::from_choice`] reads them.
    pub const CHOICES: &'static [&'static str] = &["None", "Foreground", "Custom"];

    /// The source a registry choice index names; out of range clamps to the
    /// last entry the way the options bar's own conform step does.
    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => PaintSource::None,
            1 => PaintSource::Foreground,
            _ => PaintSource::Custom,
        }
    }
}

/// The stroke width a shape or pen path starts with, in document pixels.
pub const DEFAULT_STROKE_WIDTH: f32 = 2.0;

/// The option keys [`ShapePaint::set`] answers — declared by every shape tool
/// and by the pen.
pub const PAINT_KEYS: &[&str] = &[
    "fill",
    "fill_color",
    "stroke",
    "stroke_color",
    "stroke_width",
];

/// How a shape is painted: a fill, a stroke, and where each takes its colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapePaint {
    pub fill: PaintSource,
    /// The custom fill colour, straight-alpha sRGB.
    pub fill_color: [f32; 4],
    pub stroke: PaintSource,
    /// The custom stroke colour, straight-alpha sRGB.
    pub stroke_color: [f32; 4],
    /// Total stroke width in document pixels.
    pub stroke_width: f32,
}

impl Default for ShapePaint {
    fn default() -> Self {
        Self {
            fill: PaintSource::Foreground,
            fill_color: [0.0, 0.0, 0.0, 1.0],
            stroke: PaintSource::None,
            stroke_color: [0.0, 0.0, 0.0, 1.0],
            stroke_width: DEFAULT_STROKE_WIDTH,
        }
    }
}

/// A linear straight-alpha colour as the layer stores it: sRGB, straight alpha.
///
/// Each channel is snapped to a 16-bit grid, far finer than any display or
/// the 8-bit tiles: the transfer curve's float error otherwise stores a pure
/// red foreground as `0.99999994`, which reads back as "not the colour I
/// picked" to anything comparing it (a swatch, a preset, a test).
fn encode(linear: [f32; 4]) -> [f32; 4] {
    let s = linear_to_srgb3([linear[0], linear[1], linear[2]]);
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() / 65535.0;
    [q(s[0]), q(s[1]), q(s[2]), linear[3]]
}

/// A stored sRGB straight-alpha colour as a pixel tool paints it: linear.
fn decode(srgb: [f32; 4]) -> [f32; 4] {
    let l = srgb_to_linear3([srgb[0], srgb[1], srgb[2]]);
    [l[0], l[1], l[2], srgb[3]]
}

impl ShapePaint {
    /// Answer one option key. `None` for a key that is not a paint key, so a
    /// tool can fall through to its own keys; `Some(Err(..))` for a paint key
    /// given a value of the wrong kind or a non-finite number.
    pub fn set(&mut self, key: &str, setting: ToolSetting) -> Option<Result<(), ToolError>> {
        let mismatch = || {
            Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            })
        };
        Some(match (key, setting) {
            ("fill", ToolSetting::Choice(i)) => {
                self.fill = PaintSource::from_choice(i);
                Ok(())
            }
            ("stroke", ToolSetting::Choice(i)) => {
                self.stroke = PaintSource::from_choice(i);
                Ok(())
            }
            ("fill_color", ToolSetting::Color(c)) => finite_color(key, c).map(|c| {
                self.fill_color = c;
            }),
            ("stroke_color", ToolSetting::Color(c)) => finite_color(key, c).map(|c| {
                self.stroke_color = c;
            }),
            ("stroke_width", ToolSetting::Float(w)) => {
                crate::error::finite("stroke width", w).map(|w| {
                    self.stroke_width = w.max(0.0);
                })
            }
            ("fill" | "stroke" | "fill_color" | "stroke_color" | "stroke_width", _) => mismatch(),
            _ => return None,
        })
    }

    /// The fill colour a layer stores (straight sRGB), given the linear
    /// foreground; `None` when the shape is unfilled.
    pub fn fill_rgba(&self, foreground_linear: [f32; 4]) -> Option<[f32; 4]> {
        match self.fill {
            PaintSource::None => None,
            PaintSource::Foreground => Some(encode(foreground_linear)),
            PaintSource::Custom => Some(self.fill_color),
        }
    }

    /// The stroke colour a layer stores (straight sRGB); `None` when unstroked
    /// or when the width paints nothing.
    pub fn stroke_rgba(&self, foreground_linear: [f32; 4]) -> Option<[f32; 4]> {
        if self.stroke_width <= 0.0 {
            return None;
        }
        match self.stroke {
            PaintSource::None => None,
            PaintSource::Foreground => Some(encode(foreground_linear)),
            PaintSource::Custom => Some(self.stroke_color),
        }
    }

    /// The layer stroke, or `None` when the shape is unstroked.
    pub fn layer_stroke(&self, foreground_linear: [f32; 4]) -> Option<ShapeStroke> {
        self.stroke_rgba(foreground_linear)
            .map(|color| ShapeStroke {
                color,
                width_px: self.stroke_width,
                ..ShapeStroke::default()
            })
    }

    /// The fill colour a pixel tool paints with (linear).
    pub fn fill_linear(&self, foreground_linear: [f32; 4]) -> Option<[f32; 4]> {
        match self.fill {
            PaintSource::None => None,
            PaintSource::Foreground => Some(foreground_linear),
            PaintSource::Custom => Some(decode(self.fill_color)),
        }
    }

    /// The stroke colour a pixel tool paints with (linear).
    pub fn stroke_linear(&self, foreground_linear: [f32; 4]) -> Option<[f32; 4]> {
        if self.stroke_width <= 0.0 {
            return None;
        }
        match self.stroke {
            PaintSource::None => None,
            PaintSource::Foreground => Some(foreground_linear),
            PaintSource::Custom => Some(decode(self.stroke_color)),
        }
    }

    /// `true` when neither the fill nor the stroke paints anything.
    pub fn paints_nothing(&self) -> bool {
        self.fill == PaintSource::None
            && (self.stroke == PaintSource::None || self.stroke_width <= 0.0)
    }

    /// A shape layer holding `path`, painted by this paint.
    pub fn layer(&self, path: &Path, foreground_linear: [f32; 4]) -> ShapeLayer {
        ShapeLayer {
            path_svg: to_svg(path),
            fill: self.fill_rgba(foreground_linear),
            stroke: self.layer_stroke(foreground_linear),
            ..ShapeLayer::default()
        }
    }

    /// The stroke's outline as a fillable path — what rasterising the stroke
    /// fills. Empty when the width paints nothing.
    pub fn outline(&self, path: &Path) -> Result<Path, ToolError> {
        Ok(stroke(
            path,
            &StrokeStyle::new(f64::from(self.stroke_width)),
        )?)
    }
}

fn finite_color(key: &str, c: [f32; 4]) -> Result<[f32; 4], ToolError> {
    for v in c {
        if !v.is_finite() {
            return Err(ToolError::NotFinite {
                what: if key == "fill_color" {
                    "fill colour"
                } else {
                    "stroke colour"
                },
                value: v,
            });
        }
    }
    Ok(c.map(|v| v.clamp(0.0, 1.0)))
}

/// Build the path a drag from `a` to `b` describes.
///
/// Every shape is defined by the drag box, so switching shape mid-gesture
/// (which the UI allows) cannot produce a geometry the box does not explain.
pub fn path_for(kind: &ShapeKind, a: Vec2, b: Vec2) -> Result<Path, ToolError> {
    crate::error::finite_pt("shape corner", a)?;
    crate::error::finite_pt("shape corner", b)?;
    let min = a.min(b);
    let max = a.max(b);
    let bounds = vector::Bounds::new(
        point(min.x as f64, min.y as f64),
        point(max.x as f64, max.y as f64),
    );
    let center = point((min.x + max.x) as f64 * 0.5, (min.y + max.y) as f64 * 0.5);
    let rx = (max.x - min.x) as f64 * 0.5;
    let ry = (max.y - min.y) as f64 * 0.5;
    let radius = rx.min(ry);

    let path = match kind {
        ShapeKind::Line { width } => {
            let line = shapes::line(point(a.x as f64, a.y as f64), point(b.x as f64, b.y as f64));
            stroke(
                &line,
                &StrokeStyle {
                    width: width.max(0.1),
                    cap: vector::Cap::Round,
                    ..Default::default()
                },
            )?
        }
        _ if rx <= 0.0 || ry <= 0.0 => return Err(ToolError::Degenerate),
        ShapeKind::Rectangle => shapes::rect(bounds),
        ShapeKind::RoundedRectangle { radius: r } => {
            shapes::rounded_rect(bounds, CornerRadii::uniform(r.max(0.0).min(rx.min(ry))))
        }
        ShapeKind::Ellipse => shapes::ellipse(center, point(rx, ry)),
        ShapeKind::Polygon { sides } => shapes::regular_polygon(
            center,
            radius,
            (*sides).max(3),
            -std::f64::consts::FRAC_PI_2,
        ),
        ShapeKind::Star {
            points,
            inner_ratio,
        } => shapes::star(
            center,
            radius,
            radius * inner_ratio.clamp(0.01, 1.0),
            (*points).max(3),
            -std::f64::consts::FRAC_PI_2,
        ),
        ShapeKind::Custom { path, .. } => {
            // Fit the stored path into the drag box.
            let src = path.bounds();
            let (sw, sh) = (src.max.x - src.min.x, src.max.y - src.min.y);
            if sw <= 0.0 || sh <= 0.0 {
                return Err(ToolError::Degenerate);
            }
            let t = vector::Affine::translate(-src.min.x, -src.min.y)
                .then(vector::Affine::scale(rx * 2.0 / sw, ry * 2.0 / sh))
                .then(vector::Affine::translate(min.x as f64, min.y as f64));
            path.transform(&t)
        }
    };
    if path.is_empty() || !path.is_finite() {
        return Err(ToolError::Degenerate);
    }
    Ok(path)
}

/// The anti-aliased coverage of a path, clipped to `clip`.
fn path_coverage(path: &Path, clip: PixelRect) -> Result<vector::CoverageMask, ToolError> {
    let opts = FillOptions::default().clipped_to(VecRect::from_xywh(
        clip.x as i32,
        clip.y as i32,
        clip.width,
        clip.height,
    ));
    Ok(fill(path, &opts)?)
}

/// Fill a path's coverage into a patch with one colour.
pub fn rasterize_path(
    patch: &mut ColorPatch,
    path: &Path,
    color: [f32; 4],
    clip: PixelRect,
    selection: &Selection,
) -> Result<(), ToolError> {
    let mask = path_coverage(path, clip)?;
    let origin = mask.origin();
    for y in 0..mask.height() as i32 {
        for x in 0..mask.width() as i32 {
            let p = IVec2::new(origin.x + x, origin.y + y);
            let cov = mask.coverage_f32(p);
            if cov <= 0.0 || patch.index_of(p).is_none() {
                continue;
            }
            let a = (color[3] * cov * selection.coverage_at(p)).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let src = premultiply([color[0], color[1], color[2], a]);
            let dst = patch.get(p);
            patch.set(
                p,
                [
                    src[0] + dst[0] * (1.0 - a),
                    src[1] + dst[1] * (1.0 - a),
                    src[2] + dst[2] * (1.0 - a),
                    a + dst[3] * (1.0 - a),
                ],
            );
        }
    }
    Ok(())
}

/// Fill a path's coverage into a mask's coverage plane.
///
/// Stencilling a shape into a layer mask — an ellipse to open a soft vignette,
/// a rectangle to hide a band — is the ordinary reason to rasterise at all, so
/// rasterise mode targets the mask through [`CoveragePatch`] rather than
/// refusing. As everywhere else on a mask the colour contributes its luminance
/// and its alpha decides how much of it lands.
pub fn rasterize_path_coverage(
    patch: &mut CoveragePatch,
    path: &Path,
    color: [f32; 4],
    clip: PixelRect,
    selection: &Selection,
) -> Result<(), ToolError> {
    let mask = path_coverage(path, clip)?;
    let origin = mask.origin();
    let value = mask_coverage_of(color);
    for y in 0..mask.height() as i32 {
        for x in 0..mask.width() as i32 {
            let p = IVec2::new(origin.x + x, origin.y + y);
            let cov = mask.coverage_f32(p);
            if cov <= 0.0 {
                continue;
            }
            patch.blend(p, value, color[3] * cov * selection.coverage_at(p));
        }
    }
    Ok(())
}

/// Paint `path` into the active layer's pixels (or its mask) with `paint`:
/// the fill first, then the stroke's outline over it, as one tile delta.
///
/// Shared by the shape tools' Rasterize mode and the pen's Pixels mode, so a
/// rasterised rectangle and a rasterised pen path are painted by one rule.
/// A paint that paints nothing is refused as [`ToolError::Degenerate`] rather
/// than emitting an empty step.
pub fn rasterize_painted(
    ctx: &mut ToolContext<'_>,
    path: &Path,
    paint: &ShapePaint,
) -> Result<(), ToolError> {
    let fill_color = paint.fill_linear(ctx.foreground);
    let stroke_color = paint.stroke_linear(ctx.foreground);
    if fill_color.is_none() && stroke_color.is_none() {
        return Err(ToolError::Degenerate);
    }
    let outline = if stroke_color.is_some() {
        Some(paint.outline(path)?)
    } else {
        None
    };
    let target = ctx.pixel_target()?;
    let key = ctx.pixel_key()?;
    let mut bounds = path.bounds();
    if let Some(outline) = &outline {
        bounds = bounds.union(outline.bounds());
    }
    let rect = clip_bounds(&bounds, ctx.canvas).ok_or(ToolError::Degenerate)?;
    let delta = match ctx.paint_target {
        PaintTarget::Layer => {
            let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
            if let Some(color) = fill_color {
                rasterize_path(&mut patch, path, color, rect, &ctx.selection)?;
            }
            if let (Some(color), Some(outline)) = (stroke_color, &outline) {
                rasterize_path(&mut patch, outline, color, rect, &ctx.selection)?;
            }
            patch.commit(ctx.tiles, key)?
        }
        PaintTarget::Mask => {
            let mut patch = CoveragePatch::load(ctx.tiles, key, rect)?;
            if let Some(color) = fill_color {
                rasterize_path_coverage(&mut patch, path, color, rect, &ctx.selection)?;
            }
            if let (Some(color), Some(outline)) = (stroke_color, &outline) {
                rasterize_path_coverage(&mut patch, outline, color, rect, &ctx.selection)?;
            }
            patch.commit(ctx.tiles, key)?
        }
    };
    if !delta.is_empty() {
        ctx.emit(Command::PaintTiles { target, delta });
    }
    Ok(())
}

/// Drag out a shape; release to commit it.
pub struct ShapeTool {
    pub kind: ShapeKind,
    pub mode: ShapeMode,
    /// Draw outward from the first point rather than corner-to-corner.
    pub from_center: bool,
    /// What the shape is painted with.
    pub paint: ShapePaint,
    anchor: Option<Vec2>,
    current: Option<Vec2>,
    /// Shift as the last pointer sample carried it, so the live preview and
    /// the W/H readout show the constrained box the release will commit.
    shift: bool,
}

impl ShapeTool {
    pub fn new(kind: ShapeKind, mode: ShapeMode) -> Self {
        Self {
            kind,
            mode,
            from_center: false,
            paint: ShapePaint::default(),
            anchor: None,
            current: None,
            shift: false,
        }
    }

    /// The path as it would commit right now, for the live overlay.
    pub fn preview(&self) -> Option<Path> {
        let (a, b) = self.corners(self.current?, self.shift);
        path_for(&self.kind, a, b).ok()
    }

    /// The width and height of the box being dragged, in document pixels.
    /// `None` between gestures. The value behind [`Tool::live_readout`],
    /// which the app shell publishes to its chrome after each pointer sample
    /// so the W/H label is drawn by the pointer.
    pub fn drag_size(&self) -> Option<Vec2> {
        let (a, b) = self.corners(self.current?, self.shift);
        Some((b - a).abs())
    }

    fn corners(&self, to: Vec2, shift: bool) -> (Vec2, Vec2) {
        let a = self.anchor.unwrap_or(to);
        let mut b = to;
        if shift {
            b = match self.kind {
                // A shift-constrained line snaps to 45°; every other shape
                // constrains to a square box.
                ShapeKind::Line { .. } => constrain_45(a, to),
                _ => {
                    let d = to - a;
                    let s = d.x.abs().max(d.y.abs());
                    a + Vec2::new(s * d.x.signum(), s * d.y.signum())
                }
            };
        }
        if self.from_center && !matches!(self.kind, ShapeKind::Line { .. }) {
            let d = b - a;
            (a - d, a + d)
        } else {
            (a, b)
        }
    }
}

impl Default for ShapeTool {
    fn default() -> Self {
        Self::new(ShapeKind::Rectangle, ShapeMode::VectorLayer)
    }
}

impl Tool for ShapeTool {
    fn id(&self) -> ToolId {
        self.kind.tool_id()
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("shape anchor", event.pos)?;
        self.anchor = Some(event.pos);
        self.current = Some(event.pos);
        self.shift = event.modifiers.shift;
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

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.anchor.is_none() {
            return Ok(());
        }
        let (a, b) = self.corners(event.pos, event.modifiers.shift);
        self.anchor = None;
        self.current = None;
        self.shift = false;
        let path = path_for(&self.kind, a, b)?;

        match self.mode {
            ShapeMode::VectorLayer => {
                let name = match &self.kind {
                    ShapeKind::Custom { name, .. } => name.clone(),
                    other => format!("{other:?}")
                        .split_whitespace()
                        .next()
                        .unwrap_or("Shape")
                        .to_string(),
                };
                let layer = Layer::with_kind(
                    name,
                    LayerKind::Shape(self.paint.layer(&path, ctx.foreground)),
                );
                ctx.emit(Command::create_layer(layer));
            }
            ShapeMode::Rasterize => rasterize_painted(ctx, &path, &self.paint)?,
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor = None;
        self.current = None;
        self.shift = false;
    }

    fn is_active(&self) -> bool {
        self.anchor.is_some()
    }

    /// The W/H of the box being dragged, anchored at the pointer — what the
    /// chrome labels beside the cursor. `None` between gestures.
    fn live_readout(&self) -> Option<crate::tool::LiveReadout> {
        let size = self.drag_size()?;
        Some(crate::tool::LiveReadout {
            width_px: size.x,
            height_px: size.y,
            anchor: self.current?,
        })
    }

    /// Every option the registry declares for a shape tool reaches the tool:
    /// `mode` (Shape Layer / Rasterize), `from_center` and the five paint
    /// keys ([`PAINT_KEYS`]) for all seven, and the geometry each kind owns —
    /// the rounded rectangle's `radius`, the polygon's `sides`, the star's
    /// `points` and `inner_ratio`, the line's `width`, the custom shape's
    /// `preset`. A geometry key on the wrong kind is unknown (the registry
    /// never offers it there); a known key with the wrong kind of value is a
    /// mismatch. Nothing here is a silent no-op.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        if let Some(answer) = self.paint.set(key, setting) {
            return answer;
        }
        let mismatch = || {
            Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            })
        };
        match (key, setting, &mut self.kind) {
            ("mode", ToolSetting::Choice(index), _) => {
                self.mode = ShapeMode::from_choice(index);
                Ok(())
            }
            ("mode", _, _) => mismatch(),
            ("from_center", ToolSetting::Bool(v), _) => {
                self.from_center = v;
                Ok(())
            }
            ("from_center", _, _) => mismatch(),
            ("radius", ToolSetting::Float(v), ShapeKind::RoundedRectangle { radius }) => {
                crate::error::finite("corner radius", v)?;
                *radius = f64::from(v.max(0.0));
                Ok(())
            }
            ("radius", _, ShapeKind::RoundedRectangle { .. }) => mismatch(),
            ("sides", ToolSetting::Int(v), ShapeKind::Polygon { sides }) => {
                *sides = u32::try_from(v.max(3)).unwrap_or(3);
                Ok(())
            }
            ("sides", _, ShapeKind::Polygon { .. }) => mismatch(),
            ("points", ToolSetting::Int(v), ShapeKind::Star { points, .. }) => {
                *points = u32::try_from(v.max(3)).unwrap_or(3);
                Ok(())
            }
            ("inner_ratio", ToolSetting::Float(v), ShapeKind::Star { inner_ratio, .. }) => {
                crate::error::finite("star indent", v)?;
                *inner_ratio = f64::from(v.clamp(0.01, 1.0));
                Ok(())
            }
            ("points" | "inner_ratio", _, ShapeKind::Star { .. }) => mismatch(),
            ("width", ToolSetting::Float(v), ShapeKind::Line { width }) => {
                crate::error::finite("line weight", v)?;
                *width = f64::from(v.max(0.1));
                Ok(())
            }
            ("width", _, ShapeKind::Line { .. }) => mismatch(),
            ("preset", ToolSetting::Choice(index), ShapeKind::Custom { path, name }) => {
                let last = CustomShape::ALL[CustomShape::ALL.len() - 1];
                let shape = CustomShape::from_index(index).unwrap_or(last);
                *path = shape.path();
                *name = shape.name().to_owned();
                Ok(())
            }
            ("preset", _, ShapeKind::Custom { .. }) => mismatch(),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }
}

/// Two stored colours equal to within the sRGB round trip's float error.
#[cfg(test)]
pub(crate) fn assert_rgba_near(got: [f32; 4], want: [f32; 4]) {
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-5, "{got:?} is not {want:?}");
    }
}

/// A path's bounds as an integer pixel rect, clipped to the canvas.
pub(crate) fn clip_bounds(b: &vector::Bounds, canvas: PixelRect) -> Option<PixelRect> {
    if !b.min.is_finite() || !b.max.is_finite() {
        return None;
    }
    let x0 = (b.min.x.floor() as i64).max(canvas.x);
    let y0 = (b.min.y.floor() as i64).max(canvas.y);
    let x1 = (b.max.x.ceil() as i64 + 1).min(canvas.right());
    let y1 = (b.max.y.ceil() as i64 + 1).min(canvas.bottom());
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;

    #[test]
    fn every_shape_kind_builds_a_finite_non_empty_path() {
        let a = Vec2::new(10.0, 10.0);
        let b = Vec2::new(90.0, 60.0);
        for kind in [
            ShapeKind::Rectangle,
            ShapeKind::RoundedRectangle { radius: 8.0 },
            ShapeKind::Ellipse,
            ShapeKind::Polygon { sides: 6 },
            ShapeKind::Star {
                points: 5,
                inner_ratio: 0.4,
            },
            ShapeKind::Line { width: 3.0 },
            ShapeKind::Custom {
                path: shapes::rect(vector::Bounds::new(point(0.0, 0.0), point(2.0, 1.0))),
                name: "Custom".into(),
            },
        ] {
            let p = path_for(&kind, a, b).unwrap();
            assert!(!p.is_empty() && p.is_finite(), "{kind:?} produced nothing");
            let bb = p.bounds();
            assert!(bb.max.x > bb.min.x, "{kind:?} has no width");
        }
    }

    #[test]
    fn a_zero_area_drag_is_refused_rather_than_producing_an_empty_shape() {
        let p = Vec2::new(5.0, 5.0);
        assert!(matches!(
            path_for(&ShapeKind::Rectangle, p, p),
            Err(ToolError::Degenerate)
        ));
        // A line is the exception: it has length, not area.
        assert!(path_for(&ShapeKind::Line { width: 2.0 }, p, Vec2::new(50.0, 5.0)).is_ok());
        assert!(path_for(&ShapeKind::Rectangle, p, Vec2::new(f32::NAN, 1.0)).is_err());
    }

    #[test]
    fn a_custom_shape_is_fitted_into_the_drag_box() {
        // A 2x1 source path dragged into a 80x50 box comes out 80x50.
        let src = shapes::rect(vector::Bounds::new(point(0.0, 0.0), point(2.0, 1.0)));
        let p = path_for(
            &ShapeKind::Custom {
                path: src,
                name: "c".into(),
            },
            Vec2::new(10.0, 10.0),
            Vec2::new(90.0, 60.0),
        )
        .unwrap();
        let b = p.bounds();
        assert!((b.min.x - 10.0).abs() < 1e-6 && (b.min.y - 10.0).abs() < 1e-6);
        assert!((b.max.x - 90.0).abs() < 1e-6 && (b.max.y - 60.0).abs() < 1e-6);
    }

    /// Every library entry, picked through the `preset` option and dragged
    /// out, renders ink inside the box and nowhere else — the whole route
    /// from the options bar's choice index to coverage.
    #[test]
    fn every_custom_preset_renders_non_empty_inside_its_box() {
        for (index, shape) in CustomShape::ALL.iter().enumerate() {
            let mut tool = ShapeTool::new(
                ShapeKind::custom(CustomShape::Heart),
                ShapeMode::VectorLayer,
            );
            tool.set_setting("preset", ToolSetting::Choice(index))
                .unwrap();
            assert!(
                matches!(&tool.kind, ShapeKind::Custom { name, .. } if name == shape.name()),
                "{shape:?} was not picked by index {index}"
            );
            let p = path_for(&tool.kind, Vec2::new(20.0, 30.0), Vec2::new(120.0, 130.0)).unwrap();
            let m = fill(&p, &FillOptions::default()).unwrap();
            assert!(m.area() > 1000.0, "{shape:?} rendered {} px", m.area());
            let (lo, hi) = m.bounds().unwrap();
            assert!(
                lo.x >= 20 && lo.y >= 30 && hi.x <= 120 && hi.y <= 130,
                "{shape:?}: {lo:?}..{hi:?}"
            );
        }
        // A distinct preset is a distinct shape, not the same rectangle
        // under another name.
        let heart = ShapeKind::custom(CustomShape::Heart);
        let star = ShapeKind::custom(CustomShape::Star);
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(100.0, 100.0);
        let ha = fill(&path_for(&heart, a, b).unwrap(), &FillOptions::default()).unwrap();
        let sa = fill(&path_for(&star, a, b).unwrap(), &FillOptions::default()).unwrap();
        assert!((ha.area() - sa.area()).abs() > 100.0);
    }

    fn drag(tool: &mut ShapeTool, ctx: &mut ToolContext<'_>, a: (f32, f32), b: (f32, f32)) {
        tool.on_pointer_down(ctx, PointerEvent::at(a.0, a.1))
            .unwrap();
        tool.on_pointer_move(ctx, PointerEvent::at(b.0, b.1))
            .unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(b.0, b.1)).unwrap();
    }

    fn created_shape(ctx: &mut ToolContext<'_>) -> ShapeLayer {
        let cmds = ctx.drain();
        let Some(Command::CreateLayer { layer }) = cmds.first() else {
            panic!("no layer: {cmds:?}");
        };
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("not a shape layer: {:?}", layer.kind);
        };
        shape.clone()
    }

    #[test]
    fn the_default_paint_fills_with_the_foreground_and_does_not_stroke() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
        ctx.foreground = [1.0, 0.0, 0.0, 1.0];
        let mut tool = ShapeTool::default();
        drag(&mut tool, &mut ctx, (10.0, 10.0), (60.0, 40.0));
        let shape = created_shape(&mut ctx);
        assert_rgba_near(shape.fill.expect("filled"), [1.0, 0.0, 0.0, 1.0]);
        assert!(shape.stroke.is_none());
    }

    #[test]
    fn fill_off_and_a_custom_stroke_reach_the_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
        ctx.foreground = [1.0, 0.0, 0.0, 1.0];
        let mut tool = ShapeTool::default();
        tool.set_setting("fill", ToolSetting::Choice(0)).unwrap();
        tool.set_setting("stroke", ToolSetting::Choice(2)).unwrap();
        tool.set_setting("stroke_color", ToolSetting::Color([0.0, 0.0, 1.0, 1.0]))
            .unwrap();
        tool.set_setting("stroke_width", ToolSetting::Float(4.0))
            .unwrap();
        drag(&mut tool, &mut ctx, (10.0, 10.0), (60.0, 40.0));
        let shape = created_shape(&mut ctx);
        assert_eq!(shape.fill, None, "fill off still filled");
        let stroke = shape.stroke.expect("stroke on");
        assert_eq!(stroke.color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(stroke.width_px, 4.0);
    }

    #[test]
    fn a_custom_fill_colour_is_stored_as_given_and_the_foreground_is_encoded() {
        let mut paint = ShapePaint::default();
        paint.set("fill", ToolSetting::Choice(2)).unwrap().unwrap();
        paint
            .set("fill_color", ToolSetting::Color([0.2, 0.4, 0.6, 0.5]))
            .unwrap()
            .unwrap();
        assert_eq!(paint.fill_rgba([0.0; 4]), Some([0.2, 0.4, 0.6, 0.5]));
        // The foreground arrives linear and the layer stores sRGB: a linear
        // mid-grey is a light sRGB grey.
        paint.fill = PaintSource::Foreground;
        let stored = paint.fill_rgba([0.5, 0.5, 0.5, 1.0]).unwrap();
        assert!(stored[0] > 0.7 && stored[0] < 0.76, "{stored:?}");
        assert_eq!(stored[3], 1.0);
        // ...and the custom colour is decoded on the way to a pixel.
        paint.fill = PaintSource::Custom;
        paint.fill_color = [0.5, 0.5, 0.5, 1.0];
        let painted = paint.fill_linear([0.0; 4]).unwrap();
        assert!(painted[0] > 0.2 && painted[0] < 0.22, "{painted:?}");
    }

    #[test]
    fn paint_keys_refuse_the_wrong_kind_and_non_finite_values() {
        let mut paint = ShapePaint::default();
        assert!(matches!(
            paint.set("fill", ToolSetting::Float(1.0)),
            Some(Err(ToolError::OptionKindMismatch { .. }))
        ));
        assert!(matches!(
            paint.set("stroke_width", ToolSetting::Float(f32::NAN)),
            Some(Err(ToolError::NotFinite { .. }))
        ));
        assert!(matches!(
            paint.set(
                "fill_color",
                ToolSetting::Color([f32::INFINITY, 0.0, 0.0, 1.0])
            ),
            Some(Err(ToolError::NotFinite { .. }))
        ));
        assert!(paint.set("sides", ToolSetting::Int(3)).is_none());
        for key in PAINT_KEYS {
            assert!(paint.set(key, ToolSetting::Int(3)).is_some(), "{key}");
        }
    }

    #[test]
    fn the_drag_size_readout_is_the_box_being_dragged() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
        let mut tool = ShapeTool::default();
        assert_eq!(tool.drag_size(), None);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 20.0))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(70.0, 50.0))
            .unwrap();
        assert_eq!(tool.drag_size(), Some(Vec2::new(60.0, 30.0)));
        tool.from_center = true;
        assert_eq!(tool.drag_size(), Some(Vec2::new(120.0, 60.0)));
        Tool::cancel(&mut tool, &mut ctx);
        assert_eq!(tool.drag_size(), None);
    }

    /// XB: the readout a shell reads through `dyn Tool` is the dragged box,
    /// anchored at the pointer, shift-constrained exactly as the release
    /// will commit it — and gone once the gesture ends.
    #[test]
    fn a_shape_drag_publishes_a_live_readout_until_release() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
        let mut boxed: Box<dyn Tool> = Box::new(ShapeTool::default());
        let tool = boxed.as_mut();
        assert_eq!(tool.live_readout(), None);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 20.0))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(133.0, 77.0))
            .unwrap();
        assert_eq!(
            tool.live_readout(),
            Some(crate::tool::LiveReadout {
                width_px: 123.0,
                height_px: 57.0,
                anchor: Vec2::new(133.0, 77.0),
            })
        );
        // Shift squares the box on release, so the readout squares it too.
        let shifted = PointerEvent::at(133.0, 77.0).with_modifiers(crate::tool::Modifiers::shift());
        tool.on_pointer_move(&mut ctx, shifted).unwrap();
        let readout = tool.live_readout().expect("still dragging");
        assert_eq!((readout.width_px, readout.height_px), (123.0, 123.0));
        tool.on_pointer_up(&mut ctx, shifted).unwrap();
        assert_eq!(tool.live_readout(), None);
        // What was committed is the box the readout showed.
        let committed = created_shape(&mut ctx);
        let square = path_for(
            &ShapeKind::Rectangle,
            Vec2::new(10.0, 20.0),
            Vec2::new(133.0, 143.0),
        )
        .unwrap();
        assert_eq!(
            committed.path_svg,
            ShapePaint::default()
                .layer(&square, ctx.foreground)
                .path_svg
        );
    }

    /// Rasterise a 20..100 x 20..100 rectangle with `configure`'s paint into
    /// a fresh layer, and return the tiles and the layer key.
    fn rasterised(configure: impl FnOnce(&mut ShapeTool)) -> (MemoryTiles, editor_core::PixelKey) {
        rasterise_kind(ShapeKind::Rectangle, configure)
    }

    /// Drag `kind` out in Rasterize mode and apply the `PaintTiles` it emits
    /// to the store, the way the editor does.
    fn rasterise_kind(
        kind: ShapeKind,
        configure: impl FnOnce(&mut ShapeTool),
    ) -> (MemoryTiles, editor_core::PixelKey) {
        let mut tiles = MemoryTiles::new();
        let layer = layer_model::LayerId::new();
        let key = editor_core::PixelKey::Layer(layer);
        let commands = {
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 128))
                .with_layer(layer)
                .with_foreground([1.0, 0.0, 0.0, 1.0]);
            let mut tool = ShapeTool::new(kind, ShapeMode::Rasterize);
            configure(&mut tool);
            drag(&mut tool, &mut ctx, (20.0, 20.0), (100.0, 100.0));
            ctx.drain()
        };
        for command in &commands {
            if let Command::PaintTiles { delta, .. } = command {
                tiles.apply_delta(key, delta);
            }
        }
        (tiles, key)
    }

    fn inked(tiles: &MemoryTiles, key: editor_core::PixelKey) -> usize {
        (0..128)
            .flat_map(|y| (0..128).map(move |x| (x, y)))
            .filter(|&(x, y)| tiles.pixel(key, x, y)[3] > 0)
            .count()
    }

    fn stroke_only(width: f32) -> impl FnOnce(&mut ShapeTool) {
        move |tool| {
            tool.set_setting("fill", ToolSetting::Choice(0)).unwrap();
            tool.set_setting("stroke", ToolSetting::Choice(1)).unwrap();
            tool.set_setting("stroke_width", ToolSetting::Float(width))
                .unwrap();
        }
    }

    #[test]
    fn a_wider_stroke_rasterises_a_wider_outline() {
        let (thin_tiles, key1) = rasterised(stroke_only(1.0));
        let (thick_tiles, key4) = rasterised(stroke_only(4.0));
        let thin = inked(&thin_tiles, key1);
        let thick = inked(&thick_tiles, key4);
        assert!(thin > 0, "a 1 px stroke painted nothing");
        // An 80x80 outline: ~320 px of perimeter per pixel of width. A 1 px
        // stroke on the pixel grid straddles two rows at partial coverage, so
        // it inks ~640 px; a 4 px one inks ~1280.
        assert!(
            thick * 2 > thin * 3,
            "a 4 px stroke inked {thick} px, a 1 px stroke {thin} px"
        );
        // A pixel 2 px outside the edge is inked by the 4 px stroke only.
        assert_eq!(thin_tiles.pixel(key1, 60, 18)[3], 0);
        assert!(thick_tiles.pixel(key4, 60, 18)[3] > 0);
    }

    #[test]
    fn fill_off_leaves_the_interior_transparent_and_fill_on_paints_it() {
        let (tiles, key) = rasterised(stroke_only(2.0));
        assert_eq!(
            tiles.pixel(key, 60, 60),
            [0, 0, 0, 0],
            "fill off painted the interior"
        );
        assert!(tiles.pixel(key, 60, 20)[3] > 0, "the outline is missing");

        let (tiles, key) = rasterised(|_| {});
        assert_eq!(
            tiles.pixel(key, 60, 60),
            [255, 0, 0, 255],
            "the default fill is the foreground"
        );

        // Nothing to paint at all is refused, not an empty step.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 128))
            .with_layer(layer_model::LayerId::new());
        let mut tool = ShapeTool::new(ShapeKind::Rectangle, ShapeMode::Rasterize);
        tool.set_setting("fill", ToolSetting::Choice(0)).unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(20.0, 20.0))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(100.0, 100.0))
            .unwrap();
        assert!(tool
            .on_pointer_up(&mut ctx, PointerEvent::at(100.0, 100.0))
            .is_err());
        assert!(ctx.commands().is_empty());
    }

    #[test]
    fn the_rounded_rectangle_radius_rounds_the_rasterised_corner() {
        let corner_alpha = |radius: f32| {
            let (tiles, key) =
                rasterise_kind(ShapeKind::RoundedRectangle { radius: 0.0 }, |tool| {
                    tool.set_setting("radius", ToolSetting::Float(radius))
                        .unwrap();
                });
            tiles.pixel(key, 21, 21)[3]
        };
        assert_eq!(corner_alpha(0.0), 255);
        assert_eq!(corner_alpha(20.0), 0);
    }
}
