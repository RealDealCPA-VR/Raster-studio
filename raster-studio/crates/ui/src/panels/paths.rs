//! W4-I: the Paths panel's footer operations, as pure functions over the
//! document so a test can assert on exactly what a click produces.
//!
//! Photoshop's footer carries six actions: fill the path with the foreground,
//! stroke it with the brush, load it as a selection, make a work path from
//! the selection, new path, delete path. Every one here resolves to a real
//! [`editor_core::Command`] (or, for the Work Path, to the panel's own state):
//!
//! * **Load as selection** rasterises the path through [`vector::fill`] into a
//!   [`SelectionMask`] and emits [`Command::SetSelection`] — one undo step,
//!   like every other selection edit.
//! * **Work path from selection** traces the selection's boundary with
//!   [`selection::outline::outline_selection`] and keeps it as the Work Path.
//! * **Fill** and **Stroke** emit a new shape layer carrying the path painted
//!   with the foreground (the stroke at the brush's size). The panel cannot
//!   write pixels itself — pixel edits need the application's tile store — so
//!   the painted path lands on its own layer, which the tooltips say.
//! * **New** saves the Work Path (or an empty path) as a path layer: a shape
//!   layer with neither fill nor stroke, which is how the pen's Path mode
//!   publishes one.
//! * **Delete** removes the Work Path, or the selected path's layer.

use editor_core::{Command, Document, Selection, SelectionMask};
use glam::{Affine2, IVec2};
use layer_model::{Layer, LayerKind, ShapeLayer, ShapeStroke};
use vector::{Path, Point};

/// A layer transform as the vector crate's affine (SVG `matrix()` order).
pub fn affine_of(t: Affine2) -> vector::Affine {
    let m = t.matrix2;
    vector::Affine::new([
        m.x_axis.x as f64,
        m.x_axis.y as f64,
        m.y_axis.x as f64,
        m.y_axis.y as f64,
        t.translation.x as f64,
        t.translation.y as f64,
    ])
}

/// Rasterise `path` into a selection over the document's canvas.
///
/// `None` when the path encloses no pixel of the canvas: loading an empty
/// selection would silently deselect, which is not what the button says.
pub fn path_to_selection(path: &Path, width: u32, height: u32) -> Option<Selection> {
    if path.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let clip = vector::PixelRect::from_xywh(0, 0, width, height);
    let mask = vector::fill(path, &vector::FillOptions::default().clipped_to(clip)).ok()?;
    if mask.is_empty() {
        return None;
    }
    let (origin, w, h, coverage) = mask.into_parts();
    let mask = SelectionMask::new(origin, w, h, coverage).ok()?;
    if mask.is_empty() {
        return None;
    }
    Some(Selection::Mask(mask))
}

/// The [`Command`] "load path as selection" emits, or `None` when there is
/// nothing to load.
pub fn load_as_selection(doc: &Document, path: &Path) -> Option<Command> {
    let selection = path_to_selection(path, doc.width(), doc.height())?;
    Some(Command::SetSelection { selection })
}

/// Trace the document's selection into a closed path, or `None` when there
/// is no selection (or it selects nothing).
pub fn selection_to_path(doc: &Document) -> Option<Path> {
    if doc.selection.is_none() || doc.selection.is_empty() {
        return None;
    }
    let canvas = selection::rect::Rect::new(
        IVec2::ZERO,
        IVec2::new(doc.width() as i32, doc.height() as i32),
    );
    let loops = selection::outline::outline_selection(&doc.selection, canvas, 128).ok()?;
    let mut els = Vec::new();
    for poly in loops {
        let points = without_collinear(&poly.points);
        if points.len() < 3 {
            continue;
        }
        let pts: Vec<Point> = points
            .iter()
            .map(|p| Point::new(p.x as f64, p.y as f64))
            .collect();
        els.extend_from_slice(Path::from_polyline(&pts, true).elements());
    }
    (!els.is_empty()).then(|| Path::from_elements(els))
}

/// Drop the vertices a traced outline has in the middle of a straight run —
/// the tracer walks pixel edges, so a 100px side arrives as 100 points.
fn without_collinear(points: &[IVec2]) -> Vec<IVec2> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    (0..n)
        .filter(|&i| {
            let a = points[(i + n - 1) % n];
            let b = points[i];
            let c = points[(i + 1) % n];
            let (d1, d2) = (b - a, c - b);
            d1.x * d2.y - d1.y * d2.x != 0
        })
        .map(|i| points[i])
        .collect()
}

/// A shape layer carrying `path`, filled with `foreground`.
pub fn fill_layer(path: &Path, foreground: [f32; 4]) -> Command {
    let shape = ShapeLayer {
        path_svg: vector::to_svg(path),
        fill: Some(foreground),
        stroke: None,
        ..ShapeLayer::default()
    };
    Command::create_layer(Layer::with_kind("Path Fill", LayerKind::Shape(shape)))
}

/// A shape layer carrying `path`, stroked with `foreground` at `width` px.
pub fn stroke_layer(path: &Path, foreground: [f32; 4], width: f32) -> Command {
    let shape = ShapeLayer {
        path_svg: vector::to_svg(path),
        fill: None,
        stroke: Some(ShapeStroke {
            color: foreground,
            width_px: width.max(1.0),
            ..ShapeStroke::default()
        }),
        ..ShapeLayer::default()
    };
    Command::create_layer(Layer::with_kind("Path Stroke", LayerKind::Shape(shape)))
}

/// A path layer (no paint) holding `path`, or an empty path when `None`.
pub fn new_path_layer(path: Option<&Path>, name: &str) -> Command {
    let shape = ShapeLayer {
        path_svg: path.map(vector::to_svg).unwrap_or_default(),
        fill: None,
        stroke: None,
        ..ShapeLayer::default()
    };
    Command::create_layer(Layer::with_kind(name, LayerKind::Shape(shape)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Path {
        Path::from_polyline(
            &[
                Point::new(x, y),
                Point::new(x + s, y),
                Point::new(x + s, y + s),
                Point::new(x, y + s),
            ],
            true,
        )
    }

    #[test]
    fn a_square_path_loads_as_a_selection_of_its_pixels() {
        let sel = path_to_selection(&square(4.0, 4.0, 8.0), 32, 32).unwrap();
        assert_eq!(sel.coverage_at(IVec2::new(6, 6)), 1.0);
        assert_eq!(sel.coverage_at(IVec2::new(20, 20)), 0.0);
        assert_eq!(sel.bounds(), Some((IVec2::new(4, 4), IVec2::new(12, 12))));
    }

    #[test]
    fn a_path_off_the_canvas_loads_nothing() {
        assert!(path_to_selection(&square(100.0, 100.0, 8.0), 32, 32).is_none());
        assert!(path_to_selection(&Path::new(), 32, 32).is_none());
    }

    #[test]
    fn a_rect_selection_traces_to_a_four_corner_path_and_back() {
        let mut doc = Document::new(32, 32, "p");
        doc.selection = Selection::Rect {
            min: IVec2::new(2, 3),
            max: IVec2::new(10, 12),
        };
        let path = selection_to_path(&doc).expect("a path");
        let back = path_to_selection(&path, 32, 32).unwrap();
        assert_eq!(back.bounds(), Some((IVec2::new(2, 3), IVec2::new(10, 12))));
        doc.selection = Selection::None;
        assert!(selection_to_path(&doc).is_none());
    }
}
