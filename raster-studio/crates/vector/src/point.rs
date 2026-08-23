//! Points, vectors and floating-point bounds.
//!
//! # Why `f64` and not `glam::Vec2`
//! Path geometry is not pixel geometry. Bezier subdivision, arc-length
//! bisection and segment-segment intersection all compound rounding error over
//! many steps, and `f32` runs out of mantissa long before a 65,536-pixel canvas
//! runs out of coordinates: two points a thousandth of a pixel apart at
//! `x = 40000` are not distinguishable in `f32` at all. So the path model is
//! `f64` throughout, and converts to `glam`'s `f32` vectors only at the
//! boundary where results are handed to the rest of the editor.

use serde::{Deserialize, Serialize};
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

/// A point (or a vector — the type is deliberately the same) in path space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// Shorthand constructor.
pub const fn point(x: f64, y: f64) -> Point {
    Point { x, y }
}

impl Point {
    /// The origin.
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };

    /// A point at `(x, y)`.
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// `true` when both coordinates are finite.
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    /// Euclidean length, treating this as a vector.
    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    /// Squared length — no square root, for comparisons.
    pub fn length_squared(self) -> f64 {
        self.x * self.x + self.y * self.y
    }

    /// Distance to another point.
    pub fn distance(self, other: Point) -> f64 {
        (self - other).length()
    }

    /// Squared distance to another point.
    pub fn distance_squared(self, other: Point) -> f64 {
        (self - other).length_squared()
    }

    /// Dot product.
    pub fn dot(self, other: Point) -> f64 {
        self.x * other.x + self.y * other.y
    }

    /// 2-D cross product (the z of the 3-D one): positive when `other` is
    /// counter-clockwise from `self`.
    pub fn cross(self, other: Point) -> f64 {
        self.x * other.y - self.y * other.x
    }

    /// Unit vector in the same direction, or [`Point::ZERO`] when this vector
    /// has no direction to speak of.
    ///
    /// Returning zero rather than NaN is what keeps a zero-length segment from
    /// poisoning a whole stroke outline.
    pub fn normalize(self) -> Point {
        let len = self.length();
        if len <= 0.0 || !len.is_finite() {
            Point::ZERO
        } else {
            Point::new(self.x / len, self.y / len)
        }
    }

    /// This vector rotated a quarter turn counter-clockwise.
    pub fn perp(self) -> Point {
        Point::new(-self.y, self.x)
    }

    /// Linear interpolation; `t == 0` is `self`, `t == 1` is `other`.
    pub fn lerp(self, other: Point, t: f64) -> Point {
        Point::new(
            self.x + (other.x - self.x) * t,
            self.y + (other.y - self.y) * t,
        )
    }

    /// Angle of this vector in radians, in `-pi..=pi`.
    pub fn angle(self) -> f64 {
        self.y.atan2(self.x)
    }

    /// Lossy conversion to the editor's `f32` vector type.
    pub fn to_vec2(self) -> glam::Vec2 {
        glam::Vec2::new(self.x as f32, self.y as f32)
    }

    /// Widening conversion from the editor's `f32` vector type.
    pub fn from_vec2(v: glam::Vec2) -> Self {
        Self::new(v.x as f64, v.y as f64)
    }
}

impl From<glam::Vec2> for Point {
    fn from(v: glam::Vec2) -> Self {
        Point::from_vec2(v)
    }
}

impl From<Point> for glam::Vec2 {
    fn from(p: Point) -> Self {
        p.to_vec2()
    }
}

impl From<(f64, f64)> for Point {
    fn from((x, y): (f64, f64)) -> Self {
        Point::new(x, y)
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, rhs: Point) -> Point {
        Point::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl AddAssign for Point {
    fn add_assign(&mut self, rhs: Point) {
        *self = *self + rhs;
    }
}

impl Sub for Point {
    type Output = Point;
    fn sub(self, rhs: Point) -> Point {
        Point::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl SubAssign for Point {
    fn sub_assign(&mut self, rhs: Point) {
        *self = *self - rhs;
    }
}

impl Mul<f64> for Point {
    type Output = Point;
    fn mul(self, rhs: f64) -> Point {
        Point::new(self.x * rhs, self.y * rhs)
    }
}

impl Mul<Point> for f64 {
    type Output = Point;
    fn mul(self, rhs: Point) -> Point {
        rhs * self
    }
}

impl Div<f64> for Point {
    type Output = Point;
    fn div(self, rhs: f64) -> Point {
        Point::new(self.x / rhs, self.y / rhs)
    }
}

impl Neg for Point {
    type Output = Point;
    fn neg(self) -> Point {
        Point::new(-self.x, -self.y)
    }
}

/// An axis-aligned box in path space.
///
/// Empty is represented by an inverted box (`min` above `max`), which is what
/// makes [`Bounds::EMPTY`] the identity of [`Bounds::union`] without a special
/// case in the loop.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    /// Corner with the smaller coordinates.
    pub min: Point,
    /// Corner with the larger coordinates.
    pub max: Point,
}

impl Bounds {
    /// The empty box: inverted, so unioning anything with it gives that thing.
    pub const EMPTY: Bounds = Bounds {
        min: Point {
            x: f64::INFINITY,
            y: f64::INFINITY,
        },
        max: Point {
            x: f64::NEG_INFINITY,
            y: f64::NEG_INFINITY,
        },
    };

    /// A box from two opposite corners, in either order.
    pub fn new(a: Point, b: Point) -> Self {
        Self {
            min: Point::new(a.x.min(b.x), a.y.min(b.y)),
            max: Point::new(a.x.max(b.x), a.y.max(b.y)),
        }
    }

    /// A box from a corner and a size.
    pub fn from_xywh(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self::new(Point::new(x, y), Point::new(x + w, y + h))
    }

    /// The degenerate box containing exactly one point.
    pub fn from_point(p: Point) -> Self {
        Self { min: p, max: p }
    }

    /// The tight box of a set of points; [`Bounds::EMPTY`] when there are none.
    pub fn from_points(points: impl IntoIterator<Item = Point>) -> Self {
        points
            .into_iter()
            .fold(Self::EMPTY, |b, p| b.union_point(p))
    }

    /// `true` when the box contains no area *and* no point.
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    /// Width, or 0 when empty.
    pub fn width(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.max.x - self.min.x
        }
    }

    /// Height, or 0 when empty.
    pub fn height(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.max.y - self.min.y
        }
    }

    /// Centre point; meaningless on an empty box.
    pub fn center(&self) -> Point {
        Point::new(
            (self.min.x + self.max.x) * 0.5,
            (self.min.y + self.max.y) * 0.5,
        )
    }

    /// The smallest box containing both.
    pub fn union(self, other: Bounds) -> Bounds {
        Bounds {
            min: Point::new(self.min.x.min(other.min.x), self.min.y.min(other.min.y)),
            max: Point::new(self.max.x.max(other.max.x), self.max.y.max(other.max.y)),
        }
    }

    /// The smallest box containing this one and a point.
    pub fn union_point(self, p: Point) -> Bounds {
        Bounds {
            min: Point::new(self.min.x.min(p.x), self.min.y.min(p.y)),
            max: Point::new(self.max.x.max(p.x), self.max.y.max(p.y)),
        }
    }

    /// The overlap of two boxes, possibly empty.
    pub fn intersection(self, other: Bounds) -> Bounds {
        Bounds {
            min: Point::new(self.min.x.max(other.min.x), self.min.y.max(other.min.y)),
            max: Point::new(self.max.x.min(other.max.x), self.max.y.min(other.max.y)),
        }
    }

    /// `true` when the two boxes share any point.
    pub fn intersects(self, other: Bounds) -> bool {
        !self.intersection(other).is_empty()
    }

    /// `true` when `p` is inside or on the boundary.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    /// A copy grown by `d` on every side (or shrunk, for a negative `d`).
    pub fn inflate(self, d: f64) -> Bounds {
        if self.is_empty() {
            return self;
        }
        Bounds {
            min: Point::new(self.min.x - d, self.min.y - d),
            max: Point::new(self.max.x + d, self.max.y + d),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_vector_normalizes_to_zero_not_nan() {
        // The whole stroke path depends on this: a repeated point in a polyline
        // produces a zero-length direction, and a NaN normal there would turn
        // the entire outline into NaN and rasterise to nothing.
        assert_eq!(Point::ZERO.normalize(), Point::ZERO);
        assert!(Point::ZERO.normalize().is_finite());
        let tiny = Point::new(1e-320, 0.0).normalize();
        assert!(tiny.is_finite());
    }

    #[test]
    fn vector_algebra_is_the_usual_one() {
        let a = point(3.0, 4.0);
        assert_eq!(a.length(), 5.0);
        assert_eq!(a.normalize().length(), 1.0);
        assert_eq!(a.perp(), point(-4.0, 3.0));
        assert_eq!(a.dot(point(1.0, 0.0)), 3.0);
        assert_eq!(point(1.0, 0.0).cross(point(0.0, 1.0)), 1.0);
        assert_eq!(point(0.0, 1.0).cross(point(1.0, 0.0)), -1.0);
        assert_eq!(a + a - a, a);
        assert_eq!(a * 2.0, 2.0 * a);
        assert_eq!(-a, point(-3.0, -4.0));
        assert_eq!(a / 2.0, point(1.5, 2.0));
        assert_eq!(a.lerp(point(5.0, 4.0), 0.5), point(4.0, 4.0));
    }

    #[test]
    fn the_empty_box_is_the_identity_of_union() {
        assert!(Bounds::EMPTY.is_empty());
        assert_eq!(Bounds::EMPTY.width(), 0.0);
        let b = Bounds::from_xywh(1.0, 2.0, 3.0, 4.0);
        assert_eq!(Bounds::EMPTY.union(b), b);
        assert_eq!(b.union(Bounds::EMPTY), b);
        assert_eq!(Bounds::from_points([]), Bounds::EMPTY);
    }

    #[test]
    fn bounds_cover_the_points_they_are_built_from() {
        let b = Bounds::from_points([point(5.0, -1.0), point(-2.0, 3.0), point(0.0, 0.0)]);
        assert_eq!(b.min, point(-2.0, -1.0));
        assert_eq!(b.max, point(5.0, 3.0));
        assert_eq!(b.width(), 7.0);
        assert_eq!(b.height(), 4.0);
        assert!(b.contains(point(-2.0, -1.0)));
        assert!(!b.contains(point(-2.001, -1.0)));
        assert_eq!(b.center(), point(1.5, 1.0));
        assert!(b.intersects(Bounds::from_xywh(0.0, 0.0, 1.0, 1.0)));
        assert!(!b.intersects(Bounds::from_xywh(100.0, 0.0, 1.0, 1.0)));
        assert_eq!(b.inflate(1.0).min, point(-3.0, -2.0));
        assert_eq!(Bounds::EMPTY.inflate(5.0), Bounds::EMPTY);
    }

    #[test]
    fn glam_conversion_round_trips_through_f32() {
        let p = point(1.5, -2.25);
        assert_eq!(Point::from(p.to_vec2()), p);
        assert_eq!(glam::Vec2::from(p), glam::Vec2::new(1.5, -2.25));
    }
}
