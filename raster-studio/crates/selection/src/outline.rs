//! Marching ants: the polylines the UI draws around a selection.
//!
//! # Why pixel edges and not an isoline
//! The obvious implementation is marching squares over the coverage field,
//! interpolating each crossing. It produces a smooth contour — and it is the
//! wrong contour: the crossings land on *pixel centres*, so the outline of a
//! rectangle from `x = 2` to `x = 8` comes back as `1.5 .. 7.5`, half a pixel
//! inside the rectangle on every side, and the ants visibly float over the
//! selected pixels.
//!
//! What a user reads as the selection edge is the boundary between selected and
//! unselected *pixels*, so that is what this traces: the crack between them, on
//! integer coordinates, which for a rectangle is exactly the rectangle.
//!
//! # Cost
//! One `O(width x height)` pass to classify pixels and record boundary edges,
//! then work proportional to the boundary itself. No hashing and no allocation
//! per segment — the vertex table is a flat `(w+1) x (h+1)` array of direction
//! bits — because this runs on every frame the selection is visible.

use editor_core::{Selection, SelectionMask};
use glam::IVec2;
use serde::{Deserialize, Serialize};

use crate::buf::{alloc_bytes, try_push};
use crate::error::SelectionOpError;
use crate::rect::Rect;

/// A closed loop of the selection boundary, in document pixel-corner
/// coordinates.
///
/// Consecutive points are always axis-aligned and collinear runs are collapsed,
/// so a rectangle is four points rather than one per pixel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Polyline {
    pub points: Vec<IVec2>,
    /// Always `true` for outlines produced here: a selection boundary closes.
    pub closed: bool,
}

/// Direction deltas for the four edge orientations, in the order the turn
/// preference uses them: right, down, left, up (screen coordinates, y down).
const DELTA: [IVec2; 4] = [
    IVec2::new(1, 0),
    IVec2::new(0, 1),
    IVec2::new(-1, 0),
    IVec2::new(0, -1),
];

/// Trace the boundary of everything with coverage at or above `threshold`.
///
/// Each loop is oriented so the selected side is on the walker's right, which
/// makes outer boundaries clockwise on screen and holes counter-clockwise —
/// enough for a renderer to tell them apart without a point-in-polygon test.
///
/// Where four pixels meet diagonally (two selected on one diagonal, two not on
/// the other) the boundary genuinely touches itself. The walk takes the turn
/// that keeps the selected side on its right, which separates the two regions;
/// if such a pinch happens to be where a trace *starts*, the two loops through
/// it come back joined into one closed loop instead of two. It is still a
/// correct closed traversal of the same edges, and the ants look identical.
pub fn outline(mask: &SelectionMask, threshold: u8) -> Result<Vec<Polyline>, SelectionOpError> {
    let rect = Rect::of_mask(mask)?;
    if rect.is_empty() {
        return Ok(Vec::new());
    }
    let threshold = threshold.max(1);
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let cov = mask.coverage();
    let inside = |x: i64, y: i64| -> bool {
        x >= 0
            && y >= 0
            && x < w as i64
            && y < h as i64
            && cov[y as usize * w + x as usize] >= threshold
    };

    // Bit `d` of vertex v is set when an unused boundary edge leaves v in
    // direction `d`.
    let vw = w + 1;
    let mut avail = alloc_bytes(vw * (h + 1), 0)?;
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            if !inside(x, y) {
                continue;
            }
            let (xu, yu) = (x as usize, y as usize);
            if !inside(x, y - 1) {
                avail[yu * vw + xu] |= 1 << 0;
            }
            if !inside(x + 1, y) {
                avail[yu * vw + xu + 1] |= 1 << 1;
            }
            if !inside(x, y + 1) {
                avail[(yu + 1) * vw + xu + 1] |= 1 << 2;
            }
            if !inside(x - 1, y) {
                avail[(yu + 1) * vw + xu] |= 1 << 3;
            }
        }
    }

    let origin = rect.min();
    let mut out = Vec::new();
    let mut verts: Vec<IVec2> = Vec::new();
    let mut dirs: Vec<u8> = Vec::new();

    for start_idx in 0..avail.len() {
        while avail[start_idx] != 0 {
            let start = IVec2::new((start_idx % vw) as i32, (start_idx / vw) as i32);
            verts.clear();
            dirs.clear();
            let mut v = start;
            let mut vi = start_idx;
            let mut d = avail[vi].trailing_zeros() as u8;
            loop {
                avail[vi] &= !(1 << d);
                try_push(&mut verts, v)?;
                try_push(&mut dirs, d)?;
                v += DELTA[d as usize];
                vi = v.y as usize * vw + v.x as usize;
                if v == start {
                    break;
                }
                // Prefer the sharpest right turn, which keeps the selected side
                // on the right through a diagonal pinch.
                let bits = avail[vi];
                let Some(next) = [(d + 1) % 4, d, (d + 3) % 4, (d + 2) % 4]
                    .into_iter()
                    .find(|c| bits & (1 << c) != 0)
                else {
                    break;
                };
                d = next;
            }

            let n = verts.len();
            let mut points = Vec::new();
            for i in 0..n {
                if dirs[i] != dirs[(i + n - 1) % n] {
                    try_push(&mut points, verts[i] + origin)?;
                }
            }
            if points.is_empty() {
                try_push(&mut points, verts[0] + origin)?;
            }
            try_push(
                &mut out,
                Polyline {
                    points,
                    closed: true,
                },
            )?;
        }
    }
    Ok(out)
}

/// [`outline`] for a document selection; [`Selection::None`] outlines the
/// canvas, because that is the region it selects.
pub fn outline_selection(
    sel: &Selection,
    canvas: Rect,
    threshold: u8,
) -> Result<Vec<Polyline>, SelectionOpError> {
    let mask = crate::boolean::to_mask(sel, canvas)?;
    outline(&mask, threshold)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buf::CoverageBuf;
    use crate::marquee::{ellipse, rectangle};

    fn only(polys: &[Polyline]) -> &Polyline {
        assert_eq!(polys.len(), 1, "expected one loop, got {}", polys.len());
        &polys[0]
    }

    /// Rotate a closed loop so it starts at its lexicographically smallest
    /// point, so a comparison does not depend on where the trace began.
    fn canonical(p: &Polyline) -> Vec<IVec2> {
        let k = p
            .points
            .iter()
            .enumerate()
            .min_by_key(|(_, v)| (v.y, v.x))
            .map(|(i, _)| i)
            .unwrap();
        let mut v = p.points[k..].to_vec();
        v.extend_from_slice(&p.points[..k]);
        v
    }

    #[test]
    fn the_outline_of_a_rectangle_is_that_rectangle() {
        let m = rectangle(Rect::from_xywh(2, 3, 6, 4)).unwrap();
        let polys = outline(&m, 128).unwrap();
        let p = only(&polys);
        assert!(p.closed);
        assert_eq!(
            canonical(p),
            vec![
                IVec2::new(2, 3),
                IVec2::new(8, 3),
                IVec2::new(8, 7),
                IVec2::new(2, 7),
            ],
            "four corners on the pixel boundary, not half a pixel inside it"
        );
    }

    #[test]
    fn a_single_pixel_outlines_its_own_square() {
        let m = rectangle(Rect::from_xywh(-4, 9, 1, 1)).unwrap();
        let p = outline(&m, 128).unwrap();
        assert_eq!(
            canonical(only(&p)),
            vec![
                IVec2::new(-4, 9),
                IVec2::new(-3, 9),
                IVec2::new(-3, 10),
                IVec2::new(-4, 10),
            ]
        );
    }

    #[test]
    fn a_ring_yields_an_outer_loop_and_a_hole() {
        let outer = rectangle(Rect::from_xywh(0, 0, 10, 10)).unwrap();
        let inner = rectangle(Rect::from_xywh(3, 3, 4, 4)).unwrap();
        let ring =
            crate::boolean::combine(&outer, &inner, crate::boolean::BooleanOp::Subtract).unwrap();
        let polys = outline(&ring, 128).unwrap();
        assert_eq!(polys.len(), 2);
        let mut loops: Vec<Vec<IVec2>> = polys.iter().map(canonical).collect();
        loops.sort_by_key(|l| l[0].x);
        assert_eq!(
            loops[0],
            vec![
                IVec2::new(0, 0),
                IVec2::new(10, 0),
                IVec2::new(10, 10),
                IVec2::new(0, 10)
            ]
        );
        // The hole runs the other way round: interior stays on the walker's
        // right, so a hole is traced counter-clockwise on screen.
        assert_eq!(
            loops[1],
            vec![
                IVec2::new(3, 3),
                IVec2::new(3, 7),
                IVec2::new(7, 7),
                IVec2::new(7, 3)
            ]
        );
    }

    #[test]
    fn disjoint_regions_give_one_loop_each() {
        let a = rectangle(Rect::from_xywh(0, 0, 3, 3)).unwrap();
        let b = rectangle(Rect::from_xywh(10, 10, 2, 5)).unwrap();
        let both = crate::boolean::combine(&a, &b, crate::boolean::BooleanOp::Add).unwrap();
        assert_eq!(outline(&both, 128).unwrap().len(), 2);
    }

    #[test]
    fn the_threshold_decides_where_a_soft_edge_is_traced() {
        // A 3-pixel row: solid, 100, nothing.
        let mut buf = CoverageBuf::zeroed(Rect::from_xywh(0, 0, 3, 1)).unwrap();
        buf.set(IVec2::new(0, 0), 255);
        buf.set(IVec2::new(1, 0), 100);
        let m = buf.into_mask().unwrap();

        let tight = outline(&m, 128).unwrap();
        assert_eq!(
            canonical(only(&tight))[1].x,
            1,
            "at threshold 128 only the solid pixel is inside"
        );
        let loose = outline(&m, 50).unwrap();
        assert_eq!(
            canonical(only(&loose))[1].x,
            2,
            "at threshold 50 the partial pixel is inside too"
        );
        // Threshold 0 would make every pixel "inside" including the empty ones,
        // so it is clamped to 1: coverage 0 is never selected. The mask has to
        // carry a stored zero for that to be observable, hence the untrimmed
        // buffer — a trimmed mask has no zero left to misclassify.
        let mut padded = CoverageBuf::zeroed(Rect::from_xywh(0, 0, 3, 1)).unwrap();
        padded.set(IVec2::new(0, 0), 255);
        padded.set(IVec2::new(1, 0), 100);
        let stored_zero = padded.into_mask_untrimmed().unwrap();
        assert_eq!(stored_zero.width(), 3, "the trailing zero must be stored");
        assert_eq!(
            canonical(&outline(&stored_zero, 0).unwrap()[0])[1].x,
            2,
            "a stored zero is not selected, whatever threshold is asked for"
        );
    }

    #[test]
    fn an_empty_selection_has_no_outline() {
        let empty = SelectionMask::new(IVec2::new(4, 4), 0, 0, Vec::new()).unwrap();
        assert!(outline(&empty, 128).unwrap().is_empty());
        let blank = CoverageBuf::zeroed(Rect::from_xywh(0, 0, 8, 8))
            .unwrap()
            .into_mask_untrimmed()
            .unwrap();
        assert!(outline(&blank, 128).unwrap().is_empty());
    }

    #[test]
    fn a_curved_outline_closes_and_stays_on_the_boundary() {
        let disc = ellipse(Rect::from_xywh(0, 0, 21, 21)).unwrap();
        let polys = outline(&disc, 128).unwrap();
        let p = only(&polys);
        assert!(p.points.len() > 8, "a circle needs more than a few corners");
        // Every step is axis-aligned and the loop closes.
        let n = p.points.len();
        for i in 0..n {
            let a = p.points[i];
            let b = p.points[(i + 1) % n];
            assert!(
                (a.x == b.x) != (a.y == b.y),
                "segment {i} is neither horizontal nor vertical: {a:?} -> {b:?}"
            );
        }
        // The traced region is the thresholded one: a corner of the bounding
        // box is outside it.
        assert!(p.points.iter().all(|v| *v != IVec2::ZERO));
    }

    #[test]
    fn no_selection_outlines_the_canvas() {
        let canvas = Rect::from_xywh(0, 0, 5, 5);
        let polys = outline_selection(&Selection::None, canvas, 128).unwrap();
        assert_eq!(
            canonical(only(&polys)),
            vec![
                IVec2::new(0, 0),
                IVec2::new(5, 0),
                IVec2::new(5, 5),
                IVec2::new(0, 5)
            ]
        );
    }
}
