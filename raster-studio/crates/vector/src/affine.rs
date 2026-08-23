//! The 2-D affine transform paths are moved, scaled and rotated by.
//!
//! Stored as the six meaningful coefficients of the 3x3 matrix, in the same
//! order SVG's `matrix(a b c d e f)` uses, so a transform can be round-tripped
//! through a document or an SVG attribute without a reinterpretation step:
//!
//! ```text
//! | a  c  e |   | x |     | a*x + c*y + e |
//! | b  d  f | * | y |  =  | b*x + d*y + f |
//! | 0  0  1 |   | 1 |     |       1       |
//! ```

use serde::{Deserialize, Serialize};

use crate::point::{Bounds, Point};

/// An affine transform of the plane.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    /// `[a, b, c, d, e, f]`, matching SVG's `matrix()`.
    pub m: [f64; 6],
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    /// The transform that changes nothing.
    pub const IDENTITY: Affine = Affine {
        m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };

    /// Build from the six coefficients, in SVG order.
    pub const fn new(m: [f64; 6]) -> Self {
        Self { m }
    }

    /// A pure translation.
    pub const fn translate(dx: f64, dy: f64) -> Self {
        Self {
            m: [1.0, 0.0, 0.0, 1.0, dx, dy],
        }
    }

    /// A scale about the origin.
    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self {
            m: [sx, 0.0, 0.0, sy, 0.0, 0.0],
        }
    }

    /// A rotation about the origin, counter-clockwise in a y-up frame.
    pub fn rotate(radians: f64) -> Self {
        let (s, c) = radians.sin_cos();
        Self {
            m: [c, s, -s, c, 0.0, 0.0],
        }
    }

    /// A rotation about an arbitrary centre.
    pub fn rotate_about(radians: f64, center: Point) -> Self {
        // `then` applies the receiver first, so the centre is moved to the
        // origin first and put back last.
        Self::translate(-center.x, -center.y)
            .then(Self::rotate(radians))
            .then(Self::translate(center.x, center.y))
    }

    /// A shear, with each axis' angle given in radians.
    pub fn skew(ax: f64, ay: f64) -> Self {
        Self {
            m: [1.0, ay.tan(), ax.tan(), 1.0, 0.0, 0.0],
        }
    }

    /// This transform followed by `next`.
    ///
    /// Reading order, not matrix order: `a.then(b)` applies `a` first. The
    /// matrix product is `b * a`, which is exactly the source of the classic
    /// transform-order bug, so the API is named for what happens rather than
    /// for how it multiplies.
    pub fn then(self, next: Affine) -> Affine {
        let (a, b) = (self.m, next.m);
        Affine {
            m: [
                b[0] * a[0] + b[2] * a[1],
                b[1] * a[0] + b[3] * a[1],
                b[0] * a[2] + b[2] * a[3],
                b[1] * a[2] + b[3] * a[3],
                b[0] * a[4] + b[2] * a[5] + b[4],
                b[1] * a[4] + b[3] * a[5] + b[5],
            ],
        }
    }

    /// Determinant of the linear part: the factor by which area is scaled.
    pub fn determinant(&self) -> f64 {
        self.m[0] * self.m[3] - self.m[1] * self.m[2]
    }

    /// The inverse, or `None` when this transform collapses the plane.
    pub fn inverse(&self) -> Option<Affine> {
        let det = self.determinant();
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let inv = 1.0 / det;
        let [a, b, c, d, e, f] = self.m;
        Some(Affine {
            m: [
                d * inv,
                -b * inv,
                -c * inv,
                a * inv,
                (c * f - d * e) * inv,
                (b * e - a * f) * inv,
            ],
        })
    }

    /// Apply to a point (translation included).
    pub fn apply(&self, p: Point) -> Point {
        let [a, b, c, d, e, f] = self.m;
        Point::new(a * p.x + c * p.y + e, b * p.x + d * p.y + f)
    }

    /// Apply to a direction vector (translation excluded).
    pub fn apply_vector(&self, v: Point) -> Point {
        let [a, b, c, d, _, _] = self.m;
        Point::new(a * v.x + c * v.y, b * v.x + d * v.y)
    }

    /// The tight box of a transformed box.
    ///
    /// Transforming the box corners and re-bounding them, which is exact for an
    /// affine map because the image of a box is a parallelogram whose extreme
    /// points are its corners.
    pub fn apply_bounds(&self, b: Bounds) -> Bounds {
        if b.is_empty() {
            return b;
        }
        Bounds::from_points([
            self.apply(b.min),
            self.apply(Point::new(b.max.x, b.min.y)),
            self.apply(b.max),
            self.apply(Point::new(b.min.x, b.max.y)),
        ])
    }

    /// `true` when every coefficient is finite.
    pub fn is_finite(&self) -> bool {
        self.m.iter().all(|v| v.is_finite())
    }

    /// The largest factor by which this transform can stretch a unit vector.
    ///
    /// Used to convert a tolerance expressed in device pixels into one in path
    /// space: flattening a path that will be scaled 10x needs a tolerance 10x
    /// tighter, or the curve visibly becomes a polygon.
    pub fn max_scale(&self) -> f64 {
        // Largest singular value of the 2x2 linear part, computed from the
        // eigenvalues of M^T M without forming the matrix.
        let [a, b, c, d] = [self.m[0], self.m[1], self.m[2], self.m[3]];
        let e = (a * a + b * b + c * c + d * d) * 0.5;
        let f = ((a * a + b * b - c * c - d * d) * 0.5).powi(2) + (a * c + b * d).powi(2);
        (e + f.max(0.0).sqrt()).max(0.0).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::point;

    fn close(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn then_applies_the_receiver_first() {
        // The bug this API exists to prevent: scale-then-translate and
        // translate-then-scale are different, and matrix order reads backwards.
        let scale = Affine::scale(2.0, 2.0);
        let shift = Affine::translate(10.0, 0.0);
        assert!(close(
            scale.then(shift).apply(point(1.0, 0.0)),
            point(12.0, 0.0)
        ));
        assert!(close(
            shift.then(scale).apply(point(1.0, 0.0)),
            point(22.0, 0.0)
        ));
    }

    #[test]
    fn identity_and_inverse_round_trip() {
        let t = Affine::translate(3.0, -4.0)
            .then(Affine::rotate(0.7))
            .then(Affine::scale(2.0, 3.0))
            .then(Affine::skew(0.2, -0.1));
        let inv = t.inverse().unwrap();
        for p in [point(0.0, 0.0), point(1.0, 2.0), point(-30.0, 17.5)] {
            assert!(close(inv.apply(t.apply(p)), p), "{p:?}");
        }
        assert!(close(
            Affine::IDENTITY.apply(point(1.0, 2.0)),
            point(1.0, 2.0)
        ));
        assert_eq!(Affine::default(), Affine::IDENTITY);
    }

    #[test]
    fn a_degenerate_transform_has_no_inverse_instead_of_nan() {
        assert!(Affine::scale(0.0, 1.0).inverse().is_none());
        assert!(Affine::new([1.0, 1.0, 2.0, 2.0, 0.0, 0.0])
            .inverse()
            .is_none());
        assert!(Affine::scale(f64::INFINITY, 1.0).inverse().is_none());
    }

    #[test]
    fn rotation_about_a_centre_leaves_that_centre_alone() {
        let c = point(5.0, 7.0);
        let t = Affine::rotate_about(std::f64::consts::FRAC_PI_3, c);
        assert!(close(t.apply(c), c));
        // and it is a rigid motion: distances survive
        let p = point(9.0, 7.0);
        assert!((t.apply(p).distance(c) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_vector_ignores_translation_but_a_point_does_not() {
        let t = Affine::translate(100.0, 100.0);
        assert!(close(t.apply_vector(point(1.0, 0.0)), point(1.0, 0.0)));
        assert!(close(t.apply(point(1.0, 0.0)), point(101.0, 100.0)));
    }

    #[test]
    fn max_scale_reports_the_largest_stretch() {
        assert!((Affine::IDENTITY.max_scale() - 1.0).abs() < 1e-12);
        assert!((Affine::scale(3.0, 1.0).max_scale() - 3.0).abs() < 1e-12);
        assert!((Affine::scale(1.0, -4.0).max_scale() - 4.0).abs() < 1e-12);
        assert!((Affine::rotate(1.1).max_scale() - 1.0).abs() < 1e-12);
        assert!(
            (Affine::rotate(1.1)
                .then(Affine::scale(2.0, 2.0))
                .max_scale()
                - 2.0)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn transformed_bounds_are_tight_for_a_rotation() {
        let b = Bounds::from_xywh(-1.0, -1.0, 2.0, 2.0);
        let r = Affine::rotate(std::f64::consts::FRAC_PI_4).apply_bounds(b);
        let s = 2f64.sqrt();
        assert!((r.width() - 2.0 * s).abs() < 1e-9);
        assert_eq!(
            Affine::rotate(1.0).apply_bounds(Bounds::EMPTY),
            Bounds::EMPTY
        );
    }
}
