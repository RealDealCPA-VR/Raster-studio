//! W16-G: a shape layer's live-shape record — the parameters a Rectangle,
//! Ellipse, Polygon, Star or Line drag was drawn with, kept beside the path
//! so the Properties panel can edit them after drawing (Photopea's "Live
//! Shape: Shape, that can be reconstructed from parameters at any time").
//!
//! The record is **data only**. `layer-model` sits below `vector`, so the
//! path a record describes is built by `tools::shape::live_path`, and a
//! record counts as live only while that path is still exactly the layer's
//! `path_svg` (`tools::shape::live_shape_of`): editing the path by hand —
//! Direct Selection, Combine Shapes, a boolean — leaves a record that no
//! longer regenerates the path, and the shape is a plain path from then on,
//! as Photopea's `keyShapeInvalidated` makes it.
//!
//! Every geometric value is in the shape layer's own pixel space (the space
//! `path_svg` is in), never document space: the layer transform is applied
//! on top, as it is to the path.

use serde::{Deserialize, Serialize};

/// The live parameters of a drawn shape. Appended variants only (serde
/// append-only): a document written with a later variant fails to load that
/// one record, never an earlier one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LiveShape {
    /// An axis-aligned box `x, y, w, h` with a radius per corner, clockwise
    /// from the top left (Photoshop's `keyOriginRRectRadii` order is
    /// top-right, bottom-right, bottom-left, top-left; the PSD writer maps).
    /// All four zero is a sharp rectangle.
    Rectangle {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radii: [f64; 4],
    },
    /// The ellipse inscribed in the box `x, y, w, h`.
    Ellipse { x: f64, y: f64, w: f64, h: f64 },
    /// A regular polygon with `sides` sides inscribed in the box, first
    /// vertex up — what the Polygon tool draws.
    Polygon {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        sides: u32,
    },
    /// A star with `points` points inscribed in the box, its inner radius
    /// `inner_ratio` of the outer one.
    Star {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        points: u32,
        inner_ratio: f64,
    },
    /// A straight line from `(x1, y1)` to `(x2, y2)`, `weight` pixels thick
    /// with round ends, and its arrowheads when it was drawn with any (as
    /// Photopea keeps them in `keyOriginLineArr*`).
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        weight: f64,
        /// W16-G: the line's arrowheads; `None` (and not written) for a
        /// line without heads, so records that predate it load unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arrows: Option<LiveArrows>,
    },
}

/// W16-G: a live line's arrowheads — the Line tool's Arrowheads options
/// (`tools::shape::LineArrows`), their width and length a percentage of the
/// line weight and the concavity a percentage of the head length.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LiveArrows {
    pub start: bool,
    pub end: bool,
    pub width_pct: f64,
    pub length_pct: f64,
    pub concavity_pct: f64,
}

impl LiveArrows {
    /// Every number is finite.
    pub fn is_finite(&self) -> bool {
        [self.width_pct, self.length_pct, self.concavity_pct]
            .iter()
            .all(|v| v.is_finite())
    }
}

impl LiveShape {
    /// The box `[x, y, w, h]` the shape is drawn in; a line's is the box its
    /// two ends span.
    pub fn frame(&self) -> [f64; 4] {
        match *self {
            LiveShape::Rectangle { x, y, w, h, .. }
            | LiveShape::Ellipse { x, y, w, h }
            | LiveShape::Polygon { x, y, w, h, .. }
            | LiveShape::Star { x, y, w, h, .. } => [x, y, w, h],
            LiveShape::Line { x1, y1, x2, y2, .. } => {
                [x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs()]
            }
        }
    }

    /// The same shape moved and resized into `[x, y, w, h]`: a box shape
    /// takes the box, a line's two ends are mapped from its old box to the
    /// new one (a flat line keeps its flat axis). Radii and every other
    /// parameter are unchanged.
    pub fn with_frame(&self, frame: [f64; 4]) -> LiveShape {
        let [nx, ny, nw, nh] = frame;
        let mut next = self.clone();
        match &mut next {
            LiveShape::Rectangle { x, y, w, h, .. }
            | LiveShape::Ellipse { x, y, w, h }
            | LiveShape::Polygon { x, y, w, h, .. }
            | LiveShape::Star { x, y, w, h, .. } => {
                (*x, *y, *w, *h) = (nx, ny, nw, nh);
            }
            LiveShape::Line { x1, y1, x2, y2, .. } => {
                let [ox, oy, ow, oh] = self.frame();
                let map = |v: f64, o: f64, size: f64, n: f64, nsize: f64| {
                    if size > 0.0 {
                        n + (v - o) / size * nsize
                    } else {
                        n + (v - o)
                    }
                };
                *x1 = map(*x1, ox, ow, nx, nw);
                *x2 = map(*x2, ox, ow, nx, nw);
                *y1 = map(*y1, oy, oh, ny, nh);
                *y2 = map(*y2, oy, oh, ny, nh);
            }
        }
        next
    }

    /// Every number in the record is finite.
    pub fn is_finite(&self) -> bool {
        let [x, y, w, h] = self.frame();
        let extra = match *self {
            LiveShape::Rectangle { radii, .. } => radii.iter().all(|r| r.is_finite()),
            LiveShape::Star { inner_ratio, .. } => inner_ratio.is_finite(),
            LiveShape::Line { weight, arrows, .. } => {
                weight.is_finite() && arrows.is_none_or(|a| a.is_finite())
            }
            _ => true,
        };
        [x, y, w, h].iter().all(|v| v.is_finite()) && extra
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_record_round_trips_and_a_frame_moves_it() {
        let r = LiveShape::Rectangle {
            x: 1.0,
            y: 2.0,
            w: 30.0,
            h: 40.0,
            radii: [1.0, 2.0, 3.0, 4.0],
        };
        let back: LiveShape = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        assert_eq!(r.frame(), [1.0, 2.0, 30.0, 40.0]);
        let moved = r.with_frame([5.0, 6.0, 10.0, 20.0]);
        assert_eq!(
            moved,
            LiveShape::Rectangle {
                x: 5.0,
                y: 6.0,
                w: 10.0,
                h: 20.0,
                radii: [1.0, 2.0, 3.0, 4.0],
            }
        );
        let line = LiveShape::Line {
            x1: 10.0,
            y1: 10.0,
            x2: 30.0,
            y2: 20.0,
            weight: 2.0,
            arrows: None,
        };
        assert_eq!(line.frame(), [10.0, 10.0, 20.0, 10.0]);
        assert_eq!(
            line.with_frame([0.0, 0.0, 40.0, 20.0]),
            LiveShape::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 40.0,
                y2: 20.0,
                weight: 2.0,
                arrows: None,
            }
        );
        // W16-G: a line's arrowheads travel with it; a line without them
        // writes no `arrows` key, so an older record reads back the same.
        let arrowed = LiveShape::Line {
            x1: 0.0,
            y1: 0.0,
            x2: 40.0,
            y2: 0.0,
            weight: 3.0,
            arrows: Some(LiveArrows {
                start: false,
                end: true,
                width_pct: 500.0,
                length_pct: 1000.0,
                concavity_pct: 10.0,
            }),
        };
        let json = serde_json::to_string(&arrowed).unwrap();
        assert_eq!(serde_json::from_str::<LiveShape>(&json).unwrap(), arrowed);
        let old = r#"{"Line":{"x1":0.0,"y1":0.0,"x2":4.0,"y2":0.0,"weight":1.0}}"#;
        let plain: LiveShape = serde_json::from_str(old).unwrap();
        assert!(matches!(plain, LiveShape::Line { arrows: None, .. }));
        assert!(!serde_json::to_string(&plain).unwrap().contains("arrows"));
        assert!(!LiveShape::Ellipse {
            x: f64::NAN,
            y: 0.0,
            w: 1.0,
            h: 1.0
        }
        .is_finite());
    }
}
