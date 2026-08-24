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

use color::premultiply;
use editor_core::{Command, Selection};
use glam::{IVec2, Vec2};
use layer_model::{Layer, LayerKind, ShapeLayer};
use raster::PixelRect;
use vector::{
    fill::{fill, FillOptions},
    mask::PixelRect as VecRect,
    point, shapes,
    stroke::{stroke, StrokeStyle},
    to_svg, CornerRadii, Path,
};

use crate::error::ToolError;
use crate::gradient::constrain_45;
use crate::patch::{mask_coverage_of, ColorPatch, CoveragePatch};
use crate::tool::{PaintTarget, PointerEvent, Tool, ToolContext, ToolId};

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

/// Drag out a shape; release to commit it.
pub struct ShapeTool {
    pub kind: ShapeKind,
    pub mode: ShapeMode,
    /// Draw outward from the first point rather than corner-to-corner.
    pub from_center: bool,
    anchor: Option<Vec2>,
    current: Option<Vec2>,
}

impl ShapeTool {
    pub fn new(kind: ShapeKind, mode: ShapeMode) -> Self {
        Self {
            kind,
            mode,
            from_center: false,
            anchor: None,
            current: None,
        }
    }

    /// The path as it would commit right now, for the live overlay.
    pub fn preview(&self) -> Option<Path> {
        let (a, b) = self.corners(self.current?, false);
        path_for(&self.kind, a, b).ok()
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
        if self.anchor.is_none() {
            return Ok(());
        }
        let (a, b) = self.corners(event.pos, event.modifiers.shift);
        self.anchor = None;
        self.current = None;
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
                let layer =
                    Layer::with_kind(name, LayerKind::Shape(ShapeLayer::from_svg(to_svg(&path))));
                ctx.emit(Command::create_layer(layer));
            }
            ShapeMode::Rasterize => {
                let target = ctx.pixel_target()?;
                let key = ctx.pixel_key()?;
                let bounds = path.bounds();
                let rect = clip_bounds(&bounds, ctx.canvas).ok_or(ToolError::Degenerate)?;
                let color = ctx.foreground;
                let delta = match ctx.paint_target {
                    PaintTarget::Layer => {
                        let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
                        rasterize_path(&mut patch, &path, color, rect, &ctx.selection)?;
                        patch.commit(ctx.tiles, key)?
                    }
                    PaintTarget::Mask => {
                        let mut patch = CoveragePatch::load(ctx.tiles, key, rect)?;
                        rasterize_path_coverage(&mut patch, &path, color, rect, &ctx.selection)?;
                        patch.commit(ctx.tiles, key)?
                    }
                };
                if !delta.is_empty() {
                    ctx.emit(Command::PaintTiles { target, delta });
                }
            }
        }
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
}
