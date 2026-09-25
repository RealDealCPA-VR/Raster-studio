//! W16-G: live shapes and Photopea's Parametric Shape tool.
//!
//! # Live shapes
//!
//! A Rectangle, Rounded Rectangle, Ellipse, Polygon, Star or (arrowless)
//! Line drawn as a shape layer keeps the parameters it was drawn with in
//! [`ShapeLayer::live`] ([`layer_model::LiveShape`]). [`live_path`] rebuilds
//! the path from them, and the commit writes exactly that path, so a fresh
//! shape's record regenerates its `path_svg` byte for byte. [`live_shape_of`]
//! is the one reader: it answers the record only while that is still true,
//! so an outline edited by hand (Direct Selection, Path Select's Combine,
//! Layer ▸ Combine Shapes) stops being live without any of those tools
//! knowing about records, as Photopea's `keyShapeInvalidated` does.
//! [`apply_live`] is the one writer the Properties panel uses: new
//! parameters, the path they describe, one `SetLayerKind` step.
//!
//! # Parametric Shape
//!
//! Photopea's Parametric Shape tool (the Polygon tool's slot, Photoshop's
//! `polygonTool`) draws one of [`PARAMETRIC_SHAPE_CHOICES`] — Polygon, Star,
//! Arrow, Grid, Spiral, its `pshape` list in that order — each with its own
//! parameters ([`ParametricOptions`]). The tool keeps every parameter
//! whatever shape is picked, so the options bar can set any key in any order
//! and switching the shape back finds the values where they were left.
//!
//! As in Photopea (`X.hn.LC`), a Polygon or Star is drawn from its centre:
//! the press is the centre, the release is a vertex (Shift snaps the angle to
//! 15 degrees), and Corner Radius rounds every corner. The Arrow runs press
//! to release and the Grid fills the drag box. Photopea keeps what this tool
//! draws as a plain path (a `customShape` origination), so it commits no
//! live record; the Rectangle, Ellipse and Line tools do.

use glam::Vec2;
use layer_model::{LiveShape, ShapeLayer};
use vector::{point, shapes, stroke::stroke, stroke::StrokeStyle, to_svg, CornerRadii, Path};

use super::{ShapeKind, ShapeTool};
use crate::error::ToolError;
use crate::tool::ToolSetting;

/// Photopea's Parametric Shape list (`pshape`), in its order.
pub const PARAMETRIC_SHAPE_CHOICES: &[&str] = &["Polygon", "Star", "Arrow", "Grid", "Spiral"];

/// The option keys [`ParametricOptions::set`] answers, besides `pshape`.
pub const PARAMETRIC_KEYS: &[&str] = &[
    "pshape",
    "sides",
    "inner_ratio",
    "weight",
    "head_start",
    "head_end",
    "head_width",
    "head_length",
    "concavity",
    "rows",
    "cols",
    "border",
    "length",
    "corner_radius",
];

/// Every parameter of the Parametric Shape tool, Photopea's defaults: five
/// sides (the registry's Polygon default is six, kept), a 40% star indent, a
/// 5 px arrow with an end head 50 px wide and 100 px long, a 3 x 4 grid with
/// an 8 px border, a spiral of length 4.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParametricOptions {
    /// Index into [`PARAMETRIC_SHAPE_CHOICES`].
    pub shape: usize,
    pub sides: u32,
    /// Star inner radius as a fraction of the outer one.
    pub inner_ratio: f64,
    /// Arrow shaft width, pixels (Photopea's "Width").
    pub weight: f64,
    pub head_start: bool,
    pub head_end: bool,
    /// Arrowhead width and length, pixels.
    pub head_width: f64,
    pub head_length: f64,
    /// `-50..=50`, percent.
    pub concavity: f64,
    pub rows: u32,
    pub cols: u32,
    /// Grid frame and gutter width, pixels.
    pub border: f64,
    /// Spiral length, Photopea's `4..=40`: quarter turns.
    pub length: u32,
    /// Photopea's Corner Radius (`crad`, pixels) on a Polygon or Star: every
    /// corner rounded by an arc of this radius ([`vector::shapes::rounded_polygon`]).
    pub corner_radius: f64,
}

impl Default for ParametricOptions {
    fn default() -> Self {
        Self {
            shape: 0,
            sides: 6,
            inner_ratio: 0.4,
            weight: 5.0,
            head_start: false,
            head_end: true,
            head_width: 50.0,
            head_length: 100.0,
            concavity: 0.0,
            rows: 3,
            cols: 4,
            border: 8.0,
            length: 4,
            corner_radius: 0.0,
        }
    }
}

impl ParametricOptions {
    /// The shape kind the options describe.
    pub fn kind(&self) -> ShapeKind {
        match self.shape {
            0 => ShapeKind::Polygon { sides: self.sides },
            1 => ShapeKind::Star {
                points: self.sides,
                inner_ratio: self.inner_ratio,
            },
            2 => ShapeKind::Arrow {
                weight: self.weight,
                head_start: self.head_start,
                head_end: self.head_end,
                head_width: self.head_width,
                head_length: self.head_length,
                concavity: self.concavity,
            },
            3 => ShapeKind::Grid {
                rows: self.rows,
                cols: self.cols,
                border: self.border,
            },
            _ => ShapeKind::Spiral {
                turns: f64::from(self.length) / 4.0,
                inner_ratio: 0.1,
                clockwise: true,
            },
        }
    }

    /// Set one parametric key: `Some(answer)` when the key is one of
    /// [`PARAMETRIC_KEYS`], `None` otherwise.
    pub fn set(&mut self, key: &str, setting: ToolSetting) -> Option<Result<(), ToolError>> {
        if !PARAMETRIC_KEYS.contains(&key) {
            return None;
        }
        let mismatch = || {
            Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            })
        };
        let count = |v: i32, lo: i32, hi: i32| u32::try_from(v.clamp(lo, hi)).unwrap_or(1);
        let float = |what: &'static str, v: f32, lo: f32, hi: f32| {
            crate::error::finite(what, v).map(|v| f64::from(v.clamp(lo, hi)))
        };
        Some(match (key, setting) {
            ("pshape", ToolSetting::Choice(i)) => {
                self.shape = i.min(PARAMETRIC_SHAPE_CHOICES.len() - 1);
                Ok(())
            }
            ("sides", ToolSetting::Int(v)) => {
                self.sides = count(v, 3, 100);
                Ok(())
            }
            ("inner_ratio", ToolSetting::Float(v)) => {
                float("star indent", v, 0.01, 1.0).map(|v| self.inner_ratio = v)
            }
            ("weight", ToolSetting::Float(v)) => {
                float("arrow width", v, 0.0, 1000.0).map(|v| self.weight = v)
            }
            ("head_start", ToolSetting::Bool(v)) => {
                self.head_start = v;
                Ok(())
            }
            ("head_end", ToolSetting::Bool(v)) => {
                self.head_end = v;
                Ok(())
            }
            ("head_width", ToolSetting::Float(v)) => {
                float("arrowhead width", v, 0.0, 5000.0).map(|v| self.head_width = v)
            }
            ("head_length", ToolSetting::Float(v)) => {
                float("arrowhead length", v, 0.0, 5000.0).map(|v| self.head_length = v)
            }
            ("concavity", ToolSetting::Float(v)) => {
                float("arrowhead concavity", v, -50.0, 50.0).map(|v| self.concavity = v)
            }
            ("rows", ToolSetting::Int(v)) => {
                self.rows = count(v, 1, 100);
                Ok(())
            }
            ("cols", ToolSetting::Int(v)) => {
                self.cols = count(v, 1, 100);
                Ok(())
            }
            ("border", ToolSetting::Float(v)) => {
                float("grid border", v, 0.0, 1000.0).map(|v| self.border = v)
            }
            ("length", ToolSetting::Int(v)) => {
                self.length = count(v, 1, 200);
                Ok(())
            }
            ("corner_radius", ToolSetting::Float(v)) => {
                float("corner radius", v, 0.0, 1000.0).map(|v| self.corner_radius = v)
            }
            _ => mismatch(),
        })
    }
}

impl ParametricOptions {
    /// Photopea draws a parametric Polygon or Star from its centre: the press
    /// is the centre, the release a vertex (`X.hn.LC`: radius the drag's
    /// length, the first vertex on the drag's direction).
    pub fn is_centred(&self) -> bool {
        self.shape <= 1
    }

    /// The outline of a centred Polygon or Star dragged from `centre` to
    /// `tip`, its corners rounded by [`ParametricOptions::corner_radius`];
    /// `None` for the shapes drawn in a box or along the drag.
    pub fn centred_path(&self, centre: Vec2, tip: Vec2) -> Option<Result<Path, ToolError>> {
        if !self.is_centred() {
            return None;
        }
        let c = point(f64::from(centre.x), f64::from(centre.y));
        let d = point(f64::from(tip.x), f64::from(tip.y)) - c;
        let radius = d.length();
        if !c.is_finite() || !radius.is_finite() || radius < 0.5 {
            return Some(Err(ToolError::Degenerate));
        }
        let start = d.y.atan2(d.x);
        let (count, inner) = match self.shape {
            0 => (self.sides.max(3) as usize, 1.0),
            _ => (
                self.sides.max(3) as usize * 2,
                self.inner_ratio.clamp(0.01, 1.0),
            ),
        };
        let step = std::f64::consts::TAU / count as f64;
        let verts: Vec<vector::Point> = (0..count)
            .map(|i| {
                let r = if i % 2 == 1 && self.shape == 1 {
                    radius * inner
                } else {
                    radius
                };
                let a = start + step * i as f64;
                c + point(a.cos(), a.sin()) * r
            })
            .collect();
        let path = shapes::rounded_polygon(&verts, self.corner_radius.max(0.0));
        Some(if path.is_empty() || !path.is_finite() {
            Err(ToolError::Degenerate)
        } else {
            Ok(path)
        })
    }
}

/// `to` turned about `from` onto the nearest multiple of 15 degrees, at the
/// same distance: Photopea's Shift on a centred parametric shape.
pub fn snap_15(from: Vec2, to: Vec2) -> Vec2 {
    let d = to - from;
    let len = d.length();
    if len.is_nan() || len <= 0.0 {
        return to;
    }
    let step = 15f32.to_radians();
    let a = (d.y.atan2(d.x) / step).round() * step;
    from + Vec2::new(a.cos(), a.sin()) * len
}

impl ShapeTool {
    /// W16-G: Photopea's Parametric Shape tool — the Polygon tool's slot,
    /// drawing whichever of [`PARAMETRIC_SHAPE_CHOICES`] its `pshape` option
    /// picks. It answers to [`crate::tool::ToolId::Polygon`] whatever shape
    /// it is drawing.
    pub fn parametric_tool() -> Self {
        let options = ParametricOptions::default();
        let mut tool = ShapeTool::new(options.kind(), super::ShapeMode::VectorLayer);
        tool.parametric = Some(options);
        tool
    }

    /// W16-G: the Parametric Shape options, `None` on every other shape tool.
    pub fn parametric_options(&self) -> Option<&ParametricOptions> {
        self.parametric.as_ref()
    }

    /// W16-G: the centred outline of the Parametric Shape tool's Polygon or
    /// Star; `None` on every other tool and shape.
    pub(super) fn parametric_outline(&self, a: Vec2, b: Vec2) -> Option<Result<Path, ToolError>> {
        self.parametric.as_ref()?.centred_path(a, b)
    }

    /// W16-G: whether the drag is a centred parametric shape (no box
    /// constraint, no from-centre: Shift snaps the angle to 15 degrees).
    pub(super) fn parametric_centred(&self) -> bool {
        self.parametric.as_ref().is_some_and(|p| p.is_centred())
    }

    /// W16-G: route a parametric key to the options and rebuild the kind.
    pub(super) fn set_parametric(
        &mut self,
        key: &str,
        setting: ToolSetting,
    ) -> Option<Result<(), ToolError>> {
        let options = self.parametric.as_mut()?;
        let answer = options.set(key, setting)?;
        if answer.is_ok() {
            self.kind = options.kind();
        }
        Some(answer)
    }
}

// ------------------------------------------------------------ live shapes ----

/// The live record a drag from `a` to `b` with `kind` commits, `None` for a
/// kind that is not live (custom shapes, spirals, arrows, grids — Photopea
/// keeps those as plain paths too). The Parametric Shape tool never commits
/// one (see the module docs). `arrows` says whether a line carries
/// heads, which makes it a plain path.
pub fn live_for(kind: &ShapeKind, a: Vec2, b: Vec2, arrows: bool) -> Option<LiveShape> {
    let min = a.min(b);
    let max = a.max(b);
    let (x, y) = (f64::from(min.x), f64::from(min.y));
    let (w, h) = (f64::from(max.x - min.x), f64::from(max.y - min.y));
    let live = match kind {
        ShapeKind::Rectangle => LiveShape::Rectangle {
            x,
            y,
            w,
            h,
            radii: [0.0; 4],
        },
        ShapeKind::RoundedRectangle { radius } => {
            let r = radius.max(0.0).min(w.min(h) * 0.5);
            LiveShape::Rectangle {
                x,
                y,
                w,
                h,
                radii: [r; 4],
            }
        }
        ShapeKind::Ellipse => LiveShape::Ellipse { x, y, w, h },
        ShapeKind::Polygon { sides } => LiveShape::Polygon {
            x,
            y,
            w,
            h,
            sides: (*sides).max(3),
        },
        ShapeKind::Star {
            points,
            inner_ratio,
        } => LiveShape::Star {
            x,
            y,
            w,
            h,
            points: (*points).max(3),
            inner_ratio: inner_ratio.clamp(0.01, 1.0),
        },
        ShapeKind::Line { width } if !arrows => LiveShape::Line {
            x1: f64::from(a.x),
            y1: f64::from(a.y),
            x2: f64::from(b.x),
            y2: f64::from(b.y),
            weight: width.max(0.1),
        },
        _ => return None,
    };
    live.is_finite().then_some(live)
}

/// The path a live record describes, in the shape layer's own pixels — the
/// same outline the shape tool draws for those parameters.
pub fn live_path(live: &LiveShape) -> Result<Path, ToolError> {
    if !live.is_finite() {
        return Err(ToolError::Degenerate);
    }
    let [x, y, w, h] = live.frame();
    let boxed = || -> Result<(vector::Bounds, vector::Point, f64, f64), ToolError> {
        if w <= 0.0 || h <= 0.0 {
            return Err(ToolError::Degenerate);
        }
        let bounds = vector::Bounds::new(point(x, y), point(x + w, y + h));
        let center = point(x + w * 0.5, y + h * 0.5);
        Ok((bounds, center, w * 0.5, h * 0.5))
    };
    let path = match *live {
        LiveShape::Rectangle { radii, .. } => {
            let (bounds, ..) = boxed()?;
            shapes::rounded_rect(
                bounds,
                CornerRadii::new(radii[0], radii[1], radii[2], radii[3]),
            )
        }
        LiveShape::Ellipse { .. } => {
            let (_, center, rx, ry) = boxed()?;
            shapes::ellipse(center, point(rx, ry))
        }
        LiveShape::Polygon { sides, .. } => {
            let (_, center, rx, ry) = boxed()?;
            shapes::regular_polygon(
                center,
                rx.min(ry),
                sides.max(3),
                -std::f64::consts::FRAC_PI_2,
            )
        }
        LiveShape::Star {
            points,
            inner_ratio,
            ..
        } => {
            let (_, center, rx, ry) = boxed()?;
            let radius = rx.min(ry);
            shapes::star(
                center,
                radius,
                radius * inner_ratio.clamp(0.01, 1.0),
                points.max(3),
                -std::f64::consts::FRAC_PI_2,
            )
        }
        LiveShape::Line {
            x1,
            y1,
            x2,
            y2,
            weight,
        } => stroke(
            &shapes::line(point(x1, y1), point(x2, y2)),
            &StrokeStyle {
                width: weight.max(0.1),
                cap: vector::Cap::Round,
                ..Default::default()
            },
        )?,
    };
    if path.is_empty() || !path.is_finite() {
        return Err(ToolError::Degenerate);
    }
    Ok(path)
}

/// The shape's live record while it still describes the shape's path
/// exactly; `None` for a plain path or once the outline was edited by hand.
pub fn live_shape_of(shape: &ShapeLayer) -> Option<&LiveShape> {
    let live = shape.live.as_ref()?;
    let path = live_path(live).ok()?;
    (to_svg(&path) == shape.path_svg).then_some(live)
}

/// `shape` with new live parameters: the record and the path they describe,
/// everything else (paint, fill rule) untouched.
pub fn apply_live(shape: &ShapeLayer, live: LiveShape) -> Result<ShapeLayer, ToolError> {
    let path = live_path(&live)?;
    Ok(ShapeLayer {
        path_svg: to_svg(&path),
        live: Some(live),
        ..shape.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::ShapeMode;
    use crate::tiles::MemoryTiles;
    use crate::tool::{PointerEvent, Tool, ToolContext, ToolId};
    use editor_core::Command;
    use layer_model::LayerKind;
    use raster::PixelRect;

    fn drawn(tool: &mut ShapeTool, a: (f32, f32), b: (f32, f32)) -> ShapeLayer {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 200, 200));
        tool.on_pointer_down(&mut ctx, PointerEvent::at(a.0, a.1))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(b.0, b.1))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(b.0, b.1))
            .unwrap();
        let commands = ctx.drain();
        let Some(Command::CreateLayer { layer }) = commands.first() else {
            panic!("no layer: {commands:?}");
        };
        match &layer.kind {
            LayerKind::Shape(s) => s.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_drawn_rectangle_is_live_and_a_new_radius_rewrites_its_path() {
        let mut tool = ShapeTool::new(ShapeKind::Rectangle, ShapeMode::VectorLayer);
        let shape = drawn(&mut tool, (10.0, 20.0), (90.0, 60.0));
        let live = live_shape_of(&shape).expect("a fresh rectangle is live");
        assert_eq!(
            *live,
            LiveShape::Rectangle {
                x: 10.0,
                y: 20.0,
                w: 80.0,
                h: 40.0,
                radii: [0.0; 4],
            }
        );
        // One corner rounded: the path changes and is still live.
        let next = apply_live(
            &shape,
            LiveShape::Rectangle {
                x: 10.0,
                y: 20.0,
                w: 80.0,
                h: 40.0,
                radii: [12.0, 0.0, 0.0, 0.0],
            },
        )
        .unwrap();
        assert_ne!(next.path_svg, shape.path_svg);
        assert!(live_shape_of(&next).is_some());
        let path = vector::parse_svg(&next.path_svg).unwrap();
        let inside = |x, y| vector::contains(&path, point(x, y), vector::FillRule::NonZero);
        assert!(!inside(11.0, 21.0), "the top-left corner is rounded off");
        assert!(inside(89.0, 21.0), "the top-right corner is still sharp");
        // A path edited by hand is not live any more.
        let mut edited = next.clone();
        edited.path_svg = "M0 0 L10 0 L10 10 Z".into();
        assert!(live_shape_of(&edited).is_none());
    }

    #[test]
    fn every_live_kind_commits_a_record_that_regenerates_its_path() {
        let kinds = [
            ShapeKind::Rectangle,
            ShapeKind::RoundedRectangle { radius: 6.0 },
            ShapeKind::Ellipse,
            ShapeKind::Polygon { sides: 5 },
            ShapeKind::Star {
                points: 6,
                inner_ratio: 0.5,
            },
            ShapeKind::Line { width: 4.0 },
        ];
        for kind in kinds {
            let mut tool = ShapeTool::new(kind.clone(), ShapeMode::VectorLayer);
            let shape = drawn(&mut tool, (12.5, 30.0), (77.0, 91.25));
            assert!(live_shape_of(&shape).is_some(), "{kind:?} is not live");
        }
        // A line with heads and a custom shape are plain paths.
        let mut tool = ShapeTool::new(ShapeKind::Line { width: 4.0 }, ShapeMode::VectorLayer);
        tool.set_setting("arrow_end", ToolSetting::Bool(true))
            .unwrap();
        assert!(drawn(&mut tool, (10.0, 10.0), (90.0, 10.0)).live.is_none());
    }

    #[test]
    fn each_parametric_shape_draws_through_the_polygon_slot() {
        let mut tool = ShapeTool::parametric_tool();
        assert_eq!(tool.id(), ToolId::Polygon);
        for (index, name) in PARAMETRIC_SHAPE_CHOICES.iter().enumerate() {
            tool.set_setting("pshape", ToolSetting::Choice(index))
                .unwrap();
            assert_eq!(tool.id(), ToolId::Polygon, "{name} keeps the slot's id");
            let shape = drawn(&mut tool, (20.0, 20.0), (180.0, 140.0));
            let path = vector::parse_svg(&shape.path_svg).unwrap();
            assert!(!path.is_empty() && path.is_finite(), "{name} drew nothing");
            let b = path.bounds();
            assert!(b.width() > 10.0 && b.height() > 10.0, "{name}: {b:?}");
        }
        // The Arrow runs press to release with its head at the end: 50 px
        // across by default, pointing right.
        tool.set_setting("pshape", ToolSetting::Choice(2)).unwrap();
        let arrow = drawn(&mut tool, (20.0, 100.0), (180.0, 100.0));
        let path = vector::parse_svg(&arrow.path_svg).unwrap();
        let inside = |x, y| vector::contains(&path, point(x, y), vector::FillRule::NonZero);
        // 90 px from the tip the 100 px head is 45 px across.
        assert!(inside(90.0, 110.0), "the head is wide");
        assert!(!inside(30.0, 110.0), "the shaft is 5 px");
        // The Grid's cells are holes: 3 rows x 4 columns, 8 px border.
        tool.set_setting("pshape", ToolSetting::Choice(3)).unwrap();
        tool.set_setting("rows", ToolSetting::Int(2)).unwrap();
        tool.set_setting("cols", ToolSetting::Int(2)).unwrap();
        let grid = drawn(&mut tool, (0.0, 0.0), (100.0, 100.0));
        let path = vector::parse_svg(&grid.path_svg).unwrap();
        let inside = |x, y| vector::contains(&path, point(x, y), vector::FillRule::NonZero);
        assert!(
            inside(4.0, 50.0) && inside(50.0, 50.0),
            "frame and gutter fill"
        );
        assert!(!inside(25.0, 25.0), "a cell is a hole");
        // A key for another shape is kept, not refused.
        tool.set_setting("inner_ratio", ToolSetting::Float(0.7))
            .unwrap();
        assert_eq!(
            tool.parametric_options().unwrap().inner_ratio,
            0.7_f32 as f64
        );
        assert!(tool.set_setting("sides", ToolSetting::Bool(true)).is_err());
    }

    #[test]
    fn a_parametric_polygon_is_centred_on_the_press_with_rounded_corners() {
        let mut tool = ShapeTool::parametric_tool();
        tool.set_setting("sides", ToolSetting::Int(4)).unwrap();
        // Press at the centre, release at a vertex 50 px to the right.
        let square = drawn(&mut tool, (100.0, 100.0), (150.0, 100.0));
        assert!(square.live.is_none(), "Photopea keeps it a plain path");
        let path = vector::parse_svg(&square.path_svg).unwrap();
        let b = path.bounds();
        for (got, want) in [
            (b.min.x, 50.0),
            (b.max.x, 150.0),
            (b.min.y, 50.0),
            (b.max.y, 150.0),
        ] {
            assert!((got - want).abs() < 1e-3, "{b:?}");
        }
        let inside =
            |p: &vector::Path, x, y| vector::contains(p, point(x, y), vector::FillRule::NonZero);
        assert!(inside(&path, 148.0, 100.0), "the vertex is at the release");
        // Corner Radius rounds the tip off.
        tool.set_setting("corner_radius", ToolSetting::Float(20.0))
            .unwrap();
        let round = drawn(&mut tool, (100.0, 100.0), (150.0, 100.0));
        let rpath = vector::parse_svg(&round.path_svg).unwrap();
        assert!(!inside(&rpath, 148.0, 100.0), "the tip is rounded off");
        assert!(inside(&rpath, 120.0, 100.0));
        // A star's first point is at the release too, its indent inside.
        tool.set_setting("pshape", ToolSetting::Choice(1)).unwrap();
        tool.set_setting("corner_radius", ToolSetting::Float(0.0))
            .unwrap();
        let star = drawn(&mut tool, (100.0, 100.0), (100.0, 40.0));
        let spath = vector::parse_svg(&star.path_svg).unwrap();
        assert!(
            inside(&spath, 100.0, 42.0),
            "the point is up at the release"
        );
        // Shift snaps the drag to 15 degrees about the centre.
        let snapped = snap_15(Vec2::new(0.0, 0.0), Vec2::new(10.0, 1.0));
        assert!(snapped.y.abs() < 1e-4 && (snapped.x - 101f32.sqrt()).abs() < 1e-4);
    }
}
