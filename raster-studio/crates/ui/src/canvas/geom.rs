//! The small geometry vocabulary every canvas module shares.
//!
//! Three coordinate spaces meet on the canvas and mixing them up is the single
//! most common source of "the overlay is half a pixel off" bugs, so they are
//! named consistently everywhere in this module tree:
//!
//! | space | unit | type | who speaks it |
//! |---|---|---|---|
//! | **document** | image pixels, fractional | [`glam::Vec2`] | tools, layers, selections |
//! | **screen points** | egui logical points | [`glam::Vec2`] | egui input and painting |
//! | **screen pixels** | physical device pixels | [`glam::Vec2`] | the GPU surface |
//!
//! Screen points and screen pixels differ by the display scale
//! ([`crate::canvas::Viewport::pixels_per_point`]). Everything drawn with egui
//! is in points; everything handed to the renderer is in pixels; a "100% zoom"
//! means one document pixel per *physical* pixel, which is why
//! [`crate::canvas::CanvasCamera::zoom`] is defined in pixels and not points.

use glam::Vec2;

/// One of the two document axes.
///
/// `X` names the horizontal axis, so an `Axis::X` guide is a *vertical* line
/// standing at a constant document `x`. That is the convention every ruler,
/// guide and snap candidate in this module uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Axis {
    X,
    Y,
}

impl Axis {
    /// Both axes, horizontal first.
    pub const ALL: &'static [Axis] = &[Axis::X, Axis::Y];

    /// The component of `v` along this axis.
    pub fn of(self, v: Vec2) -> f32 {
        match self {
            Axis::X => v.x,
            Axis::Y => v.y,
        }
    }

    /// Overwrite the component of `v` along this axis.
    pub fn set(self, v: &mut Vec2, value: f32) {
        match self {
            Axis::X => v.x = value,
            Axis::Y => v.y = value,
        }
    }

    /// A vector with `value` on this axis and `other` on the other one.
    pub fn compose(self, value: f32, other: f32) -> Vec2 {
        match self {
            Axis::X => Vec2::new(value, other),
            Axis::Y => Vec2::new(other, value),
        }
    }

    /// This axis's extent of a rectangle, low end first.
    ///
    /// The one spelling of "is this coordinate inside that rectangle, on this
    /// axis" — which is what every per-axis bound in [`super::snapping`] asks.
    pub fn range_of(self, rect: DocRect) -> (f32, f32) {
        (self.of(rect.min), self.of(rect.max))
    }

    /// The perpendicular axis.
    pub fn other(self) -> Axis {
        match self {
            Axis::X => Axis::Y,
            Axis::Y => Axis::X,
        }
    }

    /// Short name, for tooltips and tests.
    pub const fn name(self) -> &'static str {
        match self {
            Axis::X => "x",
            Axis::Y => "y",
        }
    }
}

/// An axis-aligned rectangle in document space, with fractional edges.
///
/// Half-open in spirit — `max` is the far edge, not the last pixel — matching
/// [`raster::PixelRect`] and [`editor_core::Selection::bounds`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocRect {
    pub min: Vec2,
    pub max: Vec2,
}

impl DocRect {
    /// A rectangle from two already-ordered corners.
    pub const fn new(min: Vec2, max: Vec2) -> Self {
        Self { min, max }
    }

    /// A rectangle from two corners in any order.
    pub fn from_corners(a: Vec2, b: Vec2) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    /// A rectangle from its top-left corner and a (possibly negative) size.
    pub fn from_min_size(min: Vec2, size: Vec2) -> Self {
        Self::from_corners(min, min + size)
    }

    /// The zero-area rectangle at the origin.
    pub const ZERO: DocRect = DocRect {
        min: Vec2::ZERO,
        max: Vec2::ZERO,
    };

    /// The tight bounding box of a point set, or `None` when it is empty.
    pub fn of_points(points: &[Vec2]) -> Option<Self> {
        let mut it = points.iter().copied();
        let first = it.next()?;
        let mut r = Self::new(first, first);
        for p in it {
            r.min = r.min.min(p);
            r.max = r.max.max(p);
        }
        Some(r)
    }

    /// The document rectangle a canvas of `size` pixels covers.
    pub fn of_canvas(size: Vec2) -> Self {
        Self::new(Vec2::ZERO, size)
    }

    /// Adopt a [`raster::PixelRect`].
    pub fn of_pixel_rect(r: raster::PixelRect) -> Self {
        Self::new(
            Vec2::new(r.x as f32, r.y as f32),
            Vec2::new(r.right() as f32, r.bottom() as f32),
        )
    }

    pub fn width(&self) -> f32 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }

    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }

    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    /// `true` when the rectangle encloses no area.
    pub fn is_empty(&self) -> bool {
        !(self.max.x > self.min.x && self.max.y > self.min.y)
    }

    /// Half-open containment: the `min` edges are inside, the `max` edges out.
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.min.x && p.y >= self.min.y && p.x < self.max.x && p.y < self.max.y
    }

    /// The four corners, clockwise from the top-left — the same order
    /// [`tools::transform::TransformState::corners`] uses.
    pub fn corners(&self) -> [Vec2; 4] {
        [
            self.min,
            Vec2::new(self.max.x, self.min.y),
            self.max,
            Vec2::new(self.min.x, self.max.y),
        ]
    }

    /// Grow (or, with a negative `amount`, shrink) every edge.
    pub fn expanded(&self, amount: f32) -> Self {
        Self::from_corners(
            self.min - Vec2::splat(amount),
            self.max + Vec2::splat(amount),
        )
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, other: &Self) -> Self {
        Self::new(self.min.min(other.min), self.max.max(other.max))
    }

    /// The overlap, which may be empty.
    pub fn intersect(&self, other: &Self) -> Self {
        Self::new(self.min.max(other.min), self.max.min(other.max))
    }

    /// `p` moved to the nearest point inside the rectangle.
    pub fn clamp(&self, p: Vec2) -> Vec2 {
        p.clamp(self.min, self.max.max(self.min))
    }
}

/// Document point to egui position.
pub fn to_pos2(v: Vec2) -> egui::Pos2 {
    egui::pos2(v.x, v.y)
}

/// egui position to a [`glam::Vec2`].
pub fn from_pos2(p: egui::Pos2) -> Vec2 {
    Vec2::new(p.x, p.y)
}

/// [`glam::Vec2`] to an egui offset.
pub fn to_egui_vec2(v: Vec2) -> egui::Vec2 {
    egui::vec2(v.x, v.y)
}

/// egui offset to a [`glam::Vec2`].
pub fn from_egui_vec2(v: egui::Vec2) -> Vec2 {
    Vec2::new(v.x, v.y)
}

/// A screen-point rectangle as egui measures them.
pub fn to_egui_rect(min: Vec2, max: Vec2) -> egui::Rect {
    egui::Rect::from_min_max(to_pos2(min), to_pos2(max))
}

/// egui rectangle to a pair of [`glam::Vec2`] corners.
pub fn from_egui_rect(r: egui::Rect) -> (Vec2, Vec2) {
    (from_pos2(r.min), from_pos2(r.max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_components_round_trip() {
        let v = Vec2::new(3.0, 7.0);
        assert_eq!(Axis::X.of(v), 3.0);
        assert_eq!(Axis::Y.of(v), 7.0);
        assert_eq!(Axis::X.compose(3.0, 7.0), v);
        assert_eq!(Axis::Y.compose(7.0, 3.0), v);
        for a in Axis::ALL {
            assert_eq!(a.other().other(), *a);
            let mut w = Vec2::ZERO;
            a.set(&mut w, 5.0);
            assert_eq!(a.of(w), 5.0);
            assert_eq!(a.other().of(w), 0.0);
        }
    }

    #[test]
    fn a_rect_from_swapped_corners_is_normalised() {
        let r = DocRect::from_corners(Vec2::new(10.0, 20.0), Vec2::new(2.0, 4.0));
        assert_eq!(r.min, Vec2::new(2.0, 4.0));
        assert_eq!(r.max, Vec2::new(10.0, 20.0));
        assert_eq!(r.size(), Vec2::new(8.0, 16.0));
        assert_eq!(r.center(), Vec2::new(6.0, 12.0));
    }

    #[test]
    fn containment_is_half_open_so_adjacent_rects_do_not_both_claim_a_point() {
        let r = DocRect::new(Vec2::ZERO, Vec2::new(10.0, 10.0));
        assert!(r.contains(Vec2::ZERO));
        assert!(r.contains(Vec2::new(9.999, 9.999)));
        assert!(!r.contains(Vec2::new(10.0, 5.0)));
        assert!(!r.contains(Vec2::new(5.0, 10.0)));
        assert!(!r.contains(Vec2::new(-0.001, 5.0)));
    }

    #[test]
    fn corners_run_clockwise_from_the_top_left() {
        let r = DocRect::new(Vec2::new(1.0, 2.0), Vec2::new(5.0, 8.0));
        assert_eq!(
            r.corners(),
            [
                Vec2::new(1.0, 2.0),
                Vec2::new(5.0, 2.0),
                Vec2::new(5.0, 8.0),
                Vec2::new(1.0, 8.0),
            ]
        );
    }

    #[test]
    fn a_pixel_rect_becomes_the_same_area_in_document_space() {
        let r = DocRect::of_pixel_rect(raster::PixelRect::new(-4, 6, 20, 30));
        assert_eq!(r.min, Vec2::new(-4.0, 6.0));
        assert_eq!(r.max, Vec2::new(16.0, 36.0));
    }

    #[test]
    fn emptiness_covers_zero_and_inverted_extents() {
        assert!(DocRect::ZERO.is_empty());
        assert!(DocRect::new(Vec2::ZERO, Vec2::new(0.0, 5.0)).is_empty());
        assert!(DocRect::new(Vec2::new(5.0, 5.0), Vec2::ZERO).is_empty());
        assert!(!DocRect::new(Vec2::ZERO, Vec2::splat(1.0)).is_empty());
    }

    #[test]
    fn bounds_of_points_is_tight() {
        assert_eq!(DocRect::of_points(&[]), None);
        let r = DocRect::of_points(&[
            Vec2::new(3.0, -1.0),
            Vec2::new(-2.0, 8.0),
            Vec2::new(0.0, 0.0),
        ])
        .unwrap();
        assert_eq!(r.min, Vec2::new(-2.0, -1.0));
        assert_eq!(r.max, Vec2::new(3.0, 8.0));
    }

    #[test]
    fn union_and_intersection_behave() {
        let a = DocRect::new(Vec2::ZERO, Vec2::splat(10.0));
        let b = DocRect::new(Vec2::splat(5.0), Vec2::splat(20.0));
        assert_eq!(a.union(&b), DocRect::new(Vec2::ZERO, Vec2::splat(20.0)));
        assert_eq!(
            a.intersect(&b),
            DocRect::new(Vec2::splat(5.0), Vec2::splat(10.0))
        );
        let disjoint = DocRect::new(Vec2::splat(50.0), Vec2::splat(60.0));
        assert!(a.intersect(&disjoint).is_empty());
    }

    #[test]
    fn egui_conversions_round_trip() {
        let v = Vec2::new(1.25, -3.5);
        assert_eq!(from_pos2(to_pos2(v)), v);
        assert_eq!(from_egui_vec2(to_egui_vec2(v)), v);
        let r = to_egui_rect(Vec2::ZERO, Vec2::new(4.0, 6.0));
        assert_eq!(from_egui_rect(r), (Vec2::ZERO, Vec2::new(4.0, 6.0)));
    }
}
