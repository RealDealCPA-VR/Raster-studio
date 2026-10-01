//! W18-I: the distances Photopea's smart guides label while a layer moves.
//!
//! For the moving box (the session's destination corners), on each of its
//! four sides, the nearest other visible layer that faces it — its bounds
//! overlap the moving box across the gap — and the gap between them, in
//! document pixels. Each gap is drawn as a smart-guide line across the
//! middle of the overlap with its length in a label
//! (`ui::canvas::paint::smart_guide_distances`).

use glam::Vec2;
use layer_model::LayerId;
use ui::canvas::paint::GuideDistance;

use crate::doc::OpenDocument;

/// The bounds of every visible layer that does not move with `moving`, as
/// `[x0, y0, x1, y1]`.
fn other_boxes(doc: &OpenDocument, moving: &[LayerId]) -> Vec<[f32; 4]> {
    let layers = &doc.document.layers;
    let moves = |id: LayerId| {
        let mut at = Some(id);
        while let Some(i) = at {
            if moving.contains(&i) {
                return true;
            }
            at = layers.parent_of(i);
        }
        false
    };
    layers
        .iter_depth_first()
        .into_iter()
        .filter(|id| !moves(*id))
        .filter(|id| {
            layers
                .get(*id)
                .is_some_and(|l| l.visible && !matches!(l.kind, layer_model::LayerKind::Group(_)))
        })
        .take(crate::tool_input::SNAP_CANDIDATE_LAYER_CAP)
        .filter_map(|id| crate::tool_input::tight_document_bounds(&doc.document, &doc.tiles, id))
        .map(|b| {
            [
                b.x as f32,
                b.y as f32,
                (b.x + i64::from(b.width)) as f32,
                (b.y + i64::from(b.height)) as f32,
            ]
        })
        .collect()
}

/// The labelled gaps from the box `corners` spans to its nearest
/// neighbour on each side (left, right, top, bottom), among `others`.
pub(crate) fn gaps(corners: &[Vec2; 4], others: &[[f32; 4]]) -> Vec<GuideDistance> {
    let lo = |f: fn(&Vec2) -> f32| corners.iter().map(f).fold(f32::INFINITY, f32::min);
    let hi = |f: fn(&Vec2) -> f32| corners.iter().map(f).fold(f32::NEG_INFINITY, f32::max);
    let (x0, x1, y0, y1) = (lo(|c| c.x), hi(|c| c.x), lo(|c| c.y), hi(|c| c.y));
    if !(x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite()) {
        return Vec::new();
    }
    // [left, right, top, bottom]: (gap, the line's across coordinate).
    let mut best: [Option<(f32, f32)>; 4] = [None; 4];
    let mut keep = |side: usize, gap: f32, across: f32| {
        if gap > 0.0 && best[side].is_none_or(|(g, _)| gap < g) {
            best[side] = Some((gap, across));
        }
    };
    for &[bx0, by0, bx1, by1] in others {
        if by0 < y1 && by1 > y0 {
            let across = (by0.max(y0) + by1.min(y1)) * 0.5;
            keep(0, x0 - bx1, across);
            keep(1, bx0 - x1, across);
        }
        if bx0 < x1 && bx1 > x0 {
            let across = (bx0.max(x0) + bx1.min(x1)) * 0.5;
            keep(2, y0 - by1, across);
            keep(3, by0 - y1, across);
        }
    }
    best.iter()
        .enumerate()
        .filter_map(|(side, b)| {
            let (gap, across) = (*b)?;
            let (a, b) = match side {
                0 => (Vec2::new(x0 - gap, across), Vec2::new(x0, across)),
                1 => (Vec2::new(x1, across), Vec2::new(x1 + gap, across)),
                2 => (Vec2::new(across, y0 - gap), Vec2::new(across, y0)),
                _ => (Vec2::new(across, y1), Vec2::new(across, y1 + gap)),
            };
            Some(GuideDistance { a, b, px: gap })
        })
        .collect()
}

/// The labelled gaps around the moving box `corners` in `doc`; the moving
/// layers are the panel selection and the active layer.
pub(crate) fn distances(doc: &OpenDocument, corners: &[Vec2; 4]) -> Vec<GuideDistance> {
    let mut moving = doc.document.layer_selection().to_vec();
    if let Some(active) = doc.document.active_layer() {
        if !moving.contains(&active) {
            moving.push(active);
        }
    }
    gaps(corners, &other_boxes(doc, &moving))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> [Vec2; 4] {
        [
            Vec2::new(x0, y0),
            Vec2::new(x1, y0),
            Vec2::new(x1, y1),
            Vec2::new(x0, y1),
        ]
    }

    #[test]
    fn each_side_measures_to_its_nearest_facing_neighbour() {
        let moving = rect(10.0, 10.0, 20.0, 20.0);
        let others = [
            [0.0, 12.0, 4.0, 18.0],   // left, 6 px away
            [2.0, 0.0, 8.0, 30.0],    // left, nearer: 2 px
            [26.0, 15.0, 30.0, 40.0], // right, 6 px
            [40.0, 40.0, 50.0, 50.0], // faces nothing
            [12.0, 25.0, 18.0, 28.0], // below, 5 px
        ];
        let d = gaps(&moving, &others);
        let px: Vec<f32> = d.iter().map(|g| g.px).collect();
        assert_eq!(px, [2.0, 6.0, 5.0], "{d:?}");
        assert_eq!(d[0].a, Vec2::new(8.0, 15.0));
        assert_eq!(d[0].b, Vec2::new(10.0, 15.0));
        assert_eq!(d[1].a, Vec2::new(20.0, 17.5), "across the overlap's middle");
        // Touching or overlapping boxes have no gap to label.
        assert!(gaps(&moving, &[[20.0, 10.0, 30.0, 20.0]]).is_empty());
    }
}
