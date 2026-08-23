//! Boolean combination of selections, defined on **fractional** coverage.
//!
//! Binary set algebra has one answer per operation; fuzzy coverage has a family
//! of them, and the choice is visible on every anti-aliased edge. The rule used
//! here is: each operation must be *exact* on binary inputs, and must give the
//! obvious answer when a shape is combined with itself.
//!
//! | op | formula | `f(a, a)` |
//! |---|---|---|
//! | [`BooleanOp::Add`] | `max(a, b)` | `a` |
//! | [`BooleanOp::Intersect`] | `min(a, b)` | `a` |
//! | [`BooleanOp::Subtract`] | `max(0, a - b)` | `0` |
//! | [`BooleanOp::Exclude`] | `abs(a - b)` | `0` |
//!
//! `max`/`min` for union and intersection because they are idempotent —
//! adding a selection to itself, or to an overlapping copy, must not brighten
//! the overlap. Clamped difference for subtract and exclude because `min(a,
//! 1 - b)` (the other common choice) leaves 0.5 behind when a half-covered
//! edge is subtracted from itself, which shows up as a grey ghost of the shape
//! the user just removed.
//!
//! These are not De Morgan duals on partial coverage — `subtract(a, b)` is not
//! `intersect(a, invert(b))` when both are fractional — and that is a
//! deliberate trade, documented rather than hidden.
//!
//! # Allocation
//! The result rectangle is derived from the operands' *tight* bounds, never
//! from a canvas: subtracting a full-canvas selection from a 4×4 one produces a
//! buffer no larger than 4×4.

use editor_core::{Selection, SelectionMask};
use glam::IVec2;
use serde::{Deserialize, Serialize};

use crate::buf::CoverageBuf;
use crate::error::SelectionOpError;
use crate::rect::Rect;

/// How a new selection combines with the existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BooleanOp {
    /// Discard the old selection.
    #[default]
    Replace,
    /// Union.
    Add,
    /// Remove the incoming shape from the existing one.
    Subtract,
    /// Keep only the overlap.
    Intersect,
    /// Symmetric difference.
    Exclude,
}

impl BooleanOp {
    /// The per-sample rule, on 8-bit coverage.
    pub fn apply(self, a: u8, b: u8) -> u8 {
        match self {
            BooleanOp::Replace => b,
            BooleanOp::Add => a.max(b),
            BooleanOp::Intersect => a.min(b),
            BooleanOp::Subtract => a.saturating_sub(b),
            BooleanOp::Exclude => a.abs_diff(b),
        }
    }

    /// The smallest rectangle that can hold the result.
    fn result_rect(self, a: Rect, b: Rect) -> Rect {
        match self {
            BooleanOp::Replace => b,
            BooleanOp::Add | BooleanOp::Exclude => a.union(b),
            BooleanOp::Intersect => a.intersection(b),
            BooleanOp::Subtract => a,
        }
    }
}

fn content_rect(mask: &SelectionMask) -> Rect {
    match mask.bounds() {
        Some((min, max)) => Rect::new(min, max),
        None => Rect::EMPTY,
    }
}

/// Combine two coverage masks.
pub fn combine(
    a: &SelectionMask,
    b: &SelectionMask,
    op: BooleanOp,
) -> Result<SelectionMask, SelectionOpError> {
    // Validate both operands' geometry even when one is ignored, so a corrupt
    // mask cannot slip through one branch and fail on the next call.
    let ra = content_rect(a).intersection(Rect::of_mask(a)?);
    let rb = content_rect(b).intersection(Rect::of_mask(b)?);
    let rect = op.result_rect(ra, rb);
    if rect.is_empty() {
        return Ok(SelectionMask::new(rect.min(), 0, 0, Vec::new())?);
    }
    let mut out = CoverageBuf::zeroed(rect)?;
    let w = rect.width() as usize;
    for y in 0..rect.height() as usize {
        let dy = rect.min().y + y as i32;
        for x in 0..w {
            let p = IVec2::new(rect.min().x + x as i32, dy);
            let v = op.apply(a.coverage_at(p), b.coverage_at(p));
            out.row_mut(y)[x] = v;
        }
    }
    out.into_mask()
}

/// Materialise a [`Selection`] as a coverage mask over `canvas`.
///
/// [`Selection::None`] means *everything*, which no finite mask can express, so
/// it becomes a filled canvas — the caller supplies the canvas precisely
/// because "everything" is only meaningful relative to one.
pub fn to_mask(sel: &Selection, canvas: Rect) -> Result<SelectionMask, SelectionOpError> {
    match sel {
        Selection::None => crate::marquee::rectangle(canvas),
        Selection::Rect { min, max } => crate::marquee::rectangle(Rect::new(*min, *max)),
        Selection::Mask(m) => Ok(m.clone()),
    }
}

/// Combine two document selections.
///
/// [`BooleanOp::Replace`] hands the incoming selection straight back, so
/// replacing with [`Selection::None`] restores "no selection" rather than
/// baking the canvas into a mask. Every other operation has to look at actual
/// coverage, so it materialises both operands over `canvas` first.
pub fn combine_selection(
    canvas: Rect,
    base: &Selection,
    incoming: &Selection,
    op: BooleanOp,
) -> Result<Selection, SelectionOpError> {
    if op == BooleanOp::Replace {
        return Ok(incoming.clone());
    }
    let a = to_mask(base, canvas)?;
    let b = to_mask(incoming, canvas)?;
    Ok(Selection::Mask(combine(&a, &b, op)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marquee::rectangle;

    fn cov(m: &SelectionMask, x: i32, y: i32) -> u8 {
        m.coverage_at(IVec2::new(x, y))
    }

    fn half(rect: Rect) -> SelectionMask {
        let mut b = CoverageBuf::filled_with(rect, 128).unwrap();
        b.set(rect.min(), 128);
        b.into_mask().unwrap()
    }

    #[test]
    fn overlapping_rectangles_give_exact_coverage_for_every_op() {
        // a = [0,6) x [0,4);  b = [4,10) x [0,4);  overlap = [4,6).
        let a = rectangle(Rect::from_xywh(0, 0, 6, 4)).unwrap();
        let b = rectangle(Rect::from_xywh(4, 0, 6, 4)).unwrap();

        let add = combine(&a, &b, BooleanOp::Add).unwrap();
        assert_eq!(add.bounds(), Some((IVec2::ZERO, IVec2::new(10, 4))));
        assert!(add.coverage().iter().all(|&v| v == 255));

        let inter = combine(&a, &b, BooleanOp::Intersect).unwrap();
        assert_eq!(inter.bounds(), Some((IVec2::new(4, 0), IVec2::new(6, 4))));
        assert!(inter.coverage().iter().all(|&v| v == 255));

        let sub = combine(&a, &b, BooleanOp::Subtract).unwrap();
        assert_eq!(sub.bounds(), Some((IVec2::ZERO, IVec2::new(4, 4))));
        assert_eq!(cov(&sub, 3, 1), 255);
        assert_eq!(cov(&sub, 4, 1), 0);

        let xor = combine(&a, &b, BooleanOp::Exclude).unwrap();
        assert_eq!(xor.bounds(), Some((IVec2::ZERO, IVec2::new(10, 4))));
        assert_eq!(cov(&xor, 0, 0), 255);
        assert_eq!(cov(&xor, 5, 0), 0, "the overlap cancels");
        assert_eq!(cov(&xor, 9, 3), 255);

        let rep = combine(&a, &b, BooleanOp::Replace).unwrap();
        assert_eq!(rep.bounds(), b.bounds());
    }

    #[test]
    fn partial_coverage_follows_the_documented_formulas() {
        let a = half(Rect::from_xywh(0, 0, 2, 1)); // 128 everywhere
        let mut bb = CoverageBuf::zeroed(Rect::from_xywh(0, 0, 2, 1)).unwrap();
        bb.set(IVec2::new(0, 0), 64);
        bb.set(IVec2::new(1, 0), 200);
        let b = bb.into_mask().unwrap();

        assert_eq!(cov(&combine(&a, &b, BooleanOp::Add).unwrap(), 0, 0), 128);
        assert_eq!(cov(&combine(&a, &b, BooleanOp::Add).unwrap(), 1, 0), 200);
        assert_eq!(
            cov(&combine(&a, &b, BooleanOp::Intersect).unwrap(), 0, 0),
            64
        );
        assert_eq!(
            cov(&combine(&a, &b, BooleanOp::Intersect).unwrap(), 1, 0),
            128
        );
        assert_eq!(
            cov(&combine(&a, &b, BooleanOp::Subtract).unwrap(), 0, 0),
            64
        );
        assert_eq!(cov(&combine(&a, &b, BooleanOp::Subtract).unwrap(), 1, 0), 0);
        assert_eq!(cov(&combine(&a, &b, BooleanOp::Exclude).unwrap(), 0, 0), 64);
        assert_eq!(cov(&combine(&a, &b, BooleanOp::Exclude).unwrap(), 1, 0), 72);
    }

    #[test]
    fn combining_a_shape_with_itself_gives_the_obvious_answer() {
        // The property that rules out `a + b - ab` for union and `min(a, 1-b)`
        // for subtract: a half-covered antialiased edge must survive a union
        // with itself unchanged, and vanish completely on a self-subtract.
        let a = crate::marquee::ellipse(Rect::from_xywh(0, 0, 21, 21)).unwrap();
        assert!(
            a.coverage().iter().any(|&v| v > 0 && v < 255),
            "the fixture needs partial coverage for this to mean anything"
        );

        let union = combine(&a, &a, BooleanOp::Add).unwrap();
        assert_eq!(union, a, "union with itself must be the identity");
        let inter = combine(&a, &a, BooleanOp::Intersect).unwrap();
        assert_eq!(inter, a);
        assert!(combine(&a, &a, BooleanOp::Subtract).unwrap().is_empty());
        assert!(combine(&a, &a, BooleanOp::Exclude).unwrap().is_empty());
    }

    #[test]
    fn a_result_is_trimmed_to_what_is_actually_selected() {
        let a = rectangle(Rect::from_xywh(0, 0, 100, 100)).unwrap();
        let b = rectangle(Rect::from_xywh(10, 10, 4, 4)).unwrap();
        let inter = combine(&a, &b, BooleanOp::Intersect).unwrap();
        assert_eq!(
            inter.coverage().len(),
            16,
            "no canvas-sized buffer survives"
        );

        // Subtracting a big shape from a small one is bounded by the small one.
        let sub = combine(&b, &a, BooleanOp::Subtract).unwrap();
        assert!(sub.is_empty());
        assert_eq!(sub.coverage().len(), 0);
    }

    #[test]
    fn disjoint_shapes_intersect_to_nothing() {
        let a = rectangle(Rect::from_xywh(0, 0, 4, 4)).unwrap();
        let b = rectangle(Rect::from_xywh(50, 50, 4, 4)).unwrap();
        let inter = combine(&a, &b, BooleanOp::Intersect).unwrap();
        assert!(inter.is_empty());
        let add = combine(&a, &b, BooleanOp::Add).unwrap();
        assert_eq!(add.bounds(), Some((IVec2::ZERO, IVec2::new(54, 54))));
    }

    #[test]
    fn replace_with_no_selection_restores_no_selection() {
        // `Selection::None` means every pixel; collapsing it into a filled mask
        // on Replace would turn "no selection" into "a selection that happens
        // to cover the canvas", which serializes and undoes differently.
        let canvas = Rect::from_xywh(0, 0, 16, 16);
        let base = Selection::Rect {
            min: IVec2::ZERO,
            max: IVec2::new(4, 4),
        };
        assert_eq!(
            combine_selection(canvas, &base, &Selection::None, BooleanOp::Replace).unwrap(),
            Selection::None
        );
    }

    #[test]
    fn no_selection_behaves_like_the_whole_canvas_in_a_combination() {
        let canvas = Rect::from_xywh(0, 0, 16, 16);
        let small = Selection::Rect {
            min: IVec2::new(2, 2),
            max: IVec2::new(6, 6),
        };
        let sub = combine_selection(canvas, &Selection::None, &small, BooleanOp::Subtract).unwrap();
        assert_eq!(sub.coverage_at(IVec2::new(0, 0)), 1.0);
        assert_eq!(sub.coverage_at(IVec2::new(3, 3)), 0.0);
        assert_eq!(sub.coverage_at(IVec2::new(15, 15)), 1.0);

        let inter =
            combine_selection(canvas, &Selection::None, &small, BooleanOp::Intersect).unwrap();
        assert_eq!(
            Rect::of_selection_bounds(&inter),
            Rect::from_xywh(2, 2, 4, 4)
        );
    }
}
