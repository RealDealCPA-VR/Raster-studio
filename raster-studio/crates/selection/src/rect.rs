//! The half-open integer rectangle every algorithm here works in.
//!
//! Same convention as [`editor_core::Selection::bounds`]: `min` is the first
//! covered pixel, `max` is one past the last, so `max - min` is the size and
//! `min == max` is empty.
//!
//! # Why coordinates are clamped
//! [`COORD_LIMIT`] caps every coordinate at `±2^30`. Selection algorithms
//! *grow* rectangles — feather pads by the kernel radius, expand pads by the
//! structuring element, a transform bounds four rotated corners — and those
//! additions have to be total. Clamping once, at the constructor, is what lets
//! the rest of the crate add and subtract `i32` coordinates without an overflow
//! check at every step: two clamped coordinates differ by at most `2^31`, which
//! fits.

use glam::IVec2;
use serde::{Deserialize, Serialize};

use crate::error::SelectionOpError;

/// The largest magnitude a selection coordinate may have.
///
/// Deliberately far below `i32::MAX`: the headroom is what makes
/// `max - min`, `min - radius` and `max + radius` total inside this crate.
pub const COORD_LIMIT: i32 = 1 << 30;

/// Clamp an `i64` coordinate into the working grid.
pub(crate) fn clamp_coord(v: i64) -> i32 {
    v.clamp(-(COORD_LIMIT as i64), COORD_LIMIT as i64) as i32
}

fn clamp_point(p: IVec2) -> IVec2 {
    IVec2::new(clamp_coord(p.x as i64), clamp_coord(p.y as i64))
}

/// A half-open, never inside-out rectangle in document pixel space.
///
/// Every constructor normalises: a `max` below `min` on either axis collapses
/// to `min` on that axis, producing an empty rect rather than a negative size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    min: IVec2,
    max: IVec2,
}

impl Rect {
    /// An empty rect at the origin.
    pub const EMPTY: Rect = Rect {
        min: IVec2::ZERO,
        max: IVec2::ZERO,
    };

    /// Clamp both corners into the working grid and normalise.
    pub fn new(min: IVec2, max: IVec2) -> Self {
        let min = clamp_point(min);
        let max = clamp_point(max);
        Self {
            min,
            max: IVec2::new(max.x.max(min.x), max.y.max(min.y)),
        }
    }

    /// Position-and-size form.
    pub fn from_xywh(x: i32, y: i32, width: u32, height: u32) -> Self {
        let min = clamp_point(IVec2::new(x, y));
        Self::new(
            min,
            IVec2::new(
                clamp_coord(min.x as i64 + width as i64),
                clamp_coord(min.y as i64 + height as i64),
            ),
        )
    }

    /// The rect two floating-point corners cover, rounded outwards to whole
    /// pixels — the pixel bounding box of a sub-pixel shape.
    pub fn enclosing(a: glam::Vec2, b: glam::Vec2) -> Self {
        let lo = a.min(b);
        let hi = a.max(b);
        Self::new(
            IVec2::new(
                clamp_coord(lo.x.floor() as i64),
                clamp_coord(lo.y.floor() as i64),
            ),
            IVec2::new(
                clamp_coord(hi.x.ceil() as i64),
                clamp_coord(hi.y.ceil() as i64),
            ),
        )
    }

    /// The storage rectangle of a mask, refusing one that reaches past the
    /// working grid.
    pub fn of_mask(mask: &editor_core::SelectionMask) -> Result<Self, SelectionOpError> {
        let o = mask.origin();
        let far_x = o.x as i64 + mask.width() as i64;
        let far_y = o.y as i64 + mask.height() as i64;
        let limit = COORD_LIMIT as i64;
        if o.x as i64 > limit
            || o.y as i64 > limit
            || (o.x as i64) < -limit
            || (o.y as i64) < -limit
            || far_x > limit
            || far_y > limit
        {
            return Err(SelectionOpError::CoordOutOfRange {
                x: o.x,
                y: o.y,
                width: mask.width(),
                height: mask.height(),
                limit: COORD_LIMIT,
            });
        }
        Ok(Self::from_xywh(o.x, o.y, mask.width(), mask.height()))
    }

    /// The tight box of a selection's non-zero coverage, or [`Rect::EMPTY`]
    /// when nothing is selected. [`editor_core::Selection::None`] has no
    /// region, so it is empty here too — callers that need "everything"
    /// substitute the canvas themselves.
    pub fn of_selection_bounds(sel: &editor_core::Selection) -> Self {
        match sel.bounds() {
            Some((min, max)) => Rect::new(min, max),
            None => Rect::EMPTY,
        }
    }

    pub const fn min(&self) -> IVec2 {
        self.min
    }

    pub const fn max(&self) -> IVec2 {
        self.max
    }

    /// Never negative: the constructor normalised `max >= min`, and both
    /// corners are inside [`COORD_LIMIT`], so the difference fits in `u32`.
    pub fn width(&self) -> u32 {
        (self.max.x as i64 - self.min.x as i64) as u32
    }

    pub fn height(&self) -> u32 {
        (self.max.y as i64 - self.min.y as i64) as u32
    }

    pub fn is_empty(&self) -> bool {
        self.max.x <= self.min.x || self.max.y <= self.min.y
    }

    /// Pixel count, exact — a `u32 * u32` product always fits a `u64`.
    pub fn area(&self) -> u64 {
        self.width() as u64 * self.height() as u64
    }

    pub fn contains(&self, p: IVec2) -> bool {
        p.x >= self.min.x && p.y >= self.min.y && p.x < self.max.x && p.y < self.max.y
    }

    /// Smallest rect containing both. An empty operand contributes nothing.
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            other
        } else if other.is_empty() {
            self
        } else {
            Rect {
                min: self.min.min(other.min),
                max: self.max.max(other.max),
            }
        }
    }

    /// Overlap, empty when they do not meet.
    pub fn intersection(self, other: Rect) -> Rect {
        if self.is_empty() || other.is_empty() {
            return Rect::EMPTY;
        }
        Rect::new(self.min.max(other.min), self.max.min(other.max))
    }

    /// Grow (or, with a negative radius, shrink) on every side.
    pub fn inflate(self, r: i32) -> Rect {
        if self.is_empty() {
            return self;
        }
        Rect::new(
            IVec2::new(
                clamp_coord(self.min.x as i64 - r as i64),
                clamp_coord(self.min.y as i64 - r as i64),
            ),
            IVec2::new(
                clamp_coord(self.max.x as i64 + r as i64),
                clamp_coord(self.max.y as i64 + r as i64),
            ),
        )
    }

    pub fn translate(self, d: IVec2) -> Rect {
        Rect::new(
            IVec2::new(
                clamp_coord(self.min.x as i64 + d.x as i64),
                clamp_coord(self.min.y as i64 + d.y as i64),
            ),
            IVec2::new(
                clamp_coord(self.max.x as i64 + d.x as i64),
                clamp_coord(self.max.y as i64 + d.y as i64),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rect_is_never_inside_out() {
        let r = Rect::new(IVec2::new(10, 10), IVec2::new(2, 30));
        assert!(
            r.is_empty(),
            "a max below min collapses instead of wrapping"
        );
        assert_eq!(r.width(), 0);
        assert_eq!(r.height(), 20);
        assert_eq!(r.area(), 0);
    }

    #[test]
    fn growing_a_rect_at_the_edge_of_the_grid_clamps_instead_of_overflowing() {
        // The whole reason COORD_LIMIT exists: feather/expand inflate rects, and
        // `i32::MAX + radius` would wrap to a negative coordinate.
        let r = Rect::new(
            IVec2::new(COORD_LIMIT - 1, COORD_LIMIT - 1),
            IVec2::new(COORD_LIMIT, COORD_LIMIT),
        );
        let big = r.inflate(1_000_000);
        assert_eq!(big.max(), IVec2::new(COORD_LIMIT, COORD_LIMIT));
        assert!(big.width() > 0 && big.height() > 0);

        // And a caller-supplied corner past the limit is clamped on the way in.
        let clamped = Rect::new(IVec2::new(i32::MIN, 0), IVec2::new(i32::MAX, 4));
        assert_eq!(clamped.min(), IVec2::new(-COORD_LIMIT, 0));
        assert_eq!(clamped.max(), IVec2::new(COORD_LIMIT, 4));
    }

    #[test]
    fn union_and_intersection_treat_empty_as_nothing() {
        let a = Rect::from_xywh(0, 0, 4, 4);
        let empty = Rect::EMPTY;
        assert_eq!(a.union(empty), a);
        assert_eq!(empty.union(a), a);
        assert!(a.intersection(empty).is_empty());
        assert_eq!(
            a.intersection(Rect::from_xywh(2, 2, 10, 10)),
            Rect::from_xywh(2, 2, 2, 2)
        );
        assert!(a.intersection(Rect::from_xywh(100, 100, 1, 1)).is_empty());
    }

    #[test]
    fn enclosing_rounds_outwards_to_whole_pixels() {
        let r = Rect::enclosing(glam::Vec2::new(1.2, -0.3), glam::Vec2::new(4.7, 2.0));
        assert_eq!(r.min(), IVec2::new(1, -1));
        assert_eq!(r.max(), IVec2::new(5, 2));
    }
}
