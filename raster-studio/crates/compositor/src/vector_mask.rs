//! W9-G: vector-mask rasterisation.
//!
//! A [`layer_model::VectorMask`] is SVG path data in layer space. This module
//! turns it into anti-aliased coverage over a document-space rect at the
//! current mip level, through `vector`'s scan converter — the same one shape
//! layers use (see [`crate::shape`]). Feather, density and inversion are
//! applied by the caller (`Ctx::vector_mask_coverage`), which owns the blur
//! and the level's feather radii; this module only answers "how much of each
//! pixel does the path cover".
//!
//! Only the requested rect is scan-converted (`FillOptions::clip`), so a
//! canvas-sized path costs a tile's worth of work per tile, not a canvas's.

use glam::Affine2;
use raster::PixelRect;
use vector::{Affine, FillOptions, FillRule};

/// `glam`'s affine as `vector`'s six SVG-order coefficients.
fn to_vector_affine(t: Affine2) -> Affine {
    let [a, b, c, d, e, f] = t.to_cols_array();
    Affine::new([
        f64::from(a),
        f64::from(b),
        f64::from(c),
        f64::from(d),
        f64::from(e),
        f64::from(f),
    ])
}

/// Coverage in `0.0..=1.0` of `path_svg`, mapped through `pose` (layer space
/// in level-0 pixels → this level's document pixels), over every pixel of
/// `rect`, row-major.
///
/// An empty (all-whitespace) path covers nothing — every sample is `0.0`.
/// Path data that does not parse, or a pose that is not finite, answers
/// `None`: the caller then ignores the vector mask, which is wrong in the
/// direction that keeps the user's content on screen rather than hiding the
/// layer behind coverage that is zero only because it could not be computed.
pub(crate) fn path_coverage(path_svg: &str, pose: Affine2, rect: PixelRect) -> Option<Vec<f32>> {
    let len = rect.width as usize * rect.height as usize;
    let mut out = vec![0.0f32; len];
    if path_svg.trim().is_empty() || rect.is_empty() {
        return Some(out);
    }
    if !pose.to_cols_array().iter().all(|v| v.is_finite()) {
        return None;
    }
    let path = vector::parse_svg(path_svg).ok()?;
    let path = path.transform(&to_vector_affine(pose));
    // The rect is inside the compositor's allocation limits, which are far
    // inside `i32`; a rect that is not is refused rather than wrapped.
    let (x, y) = (i32::try_from(rect.x).ok()?, i32::try_from(rect.y).ok()?);
    let clip = vector::PixelRect::from_xywh(x, y, rect.width, rect.height);
    let opts = FillOptions::with_rule(FillRule::NonZero).clipped_to(clip);
    let Ok(mask) = vector::fill(&path, &opts) else {
        // A path the scan converter refuses (degenerate, or empty after the
        // transform) covers nothing inside the clip.
        return Some(out);
    };
    let origin = mask.origin();
    let stride = mask.width() as usize;
    let data = mask.coverage();
    for my in 0..mask.height() as i64 {
        let dy = i64::from(origin.y) + my - rect.y;
        if dy < 0 || dy >= i64::from(rect.height) {
            continue;
        }
        for mx in 0..mask.width() as i64 {
            let dx = i64::from(origin.x) + mx - rect.x;
            if dx < 0 || dx >= i64::from(rect.width) {
                continue;
            }
            let v = data[my as usize * stride + mx as usize];
            out[dy as usize * rect.width as usize + dx as usize] = f32::from(v) / 255.0;
        }
    }
    Some(out)
}

/// W9-G: the path's hard rendering as `0..=255` bytes over `rect`, row-major
/// — anti-aliased coverage through `pose` with no feather, density or
/// inversion. What a writer stores beside the path as its rendering (a
/// `.psd` mask record flagged "from render"), and what a thumbnail shows.
/// `None` when the path data does not parse or the pose is not finite.
pub fn rendering(path_svg: &str, pose: Affine2, rect: PixelRect) -> Option<Vec<u8>> {
    let cov = path_coverage(path_svg, pose, rect)?;
    Some(
        cov.into_iter()
            .map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Vector-mask path geometry for the application (W9-G)
// ---------------------------------------------------------------------------
//
// The application maps a document-space path into a mask's layer space
// (Layer ▸ Vector Mask ▸ Current Path) and turns a `.psd` vector mask's
// Bezier knots into SVG path data and back. It does so through these
// helpers, in the crate that already owns the path machinery, so the
// application needs no geometry of its own.

/// How a subpath combines with the ones before it (a `.psd` path record's
/// operation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combine {
    Xor,
    Union,
    Subtract,
    Intersect,
}

/// One cubic-Bezier subpath: each knot is `[before, anchor, after]` — the
/// incoming control point, the on-curve point, the outgoing control point.
#[derive(Debug, Clone, PartialEq)]
pub struct BezierSubpath {
    pub closed: bool,
    pub combine: Combine,
    pub knots: Vec<[[f64; 2]; 3]>,
}

/// `path` as SVG path data.
pub fn svg_of(path: &vector::Path) -> String {
    vector::to_svg(path)
}

/// `path_svg` mapped through `t`, as SVG path data; `None` when it does not
/// parse or `t` is not finite. An empty path stays empty.
pub fn svg_transformed(path_svg: &str, t: Affine2) -> Option<String> {
    if !t.to_cols_array().iter().all(|v| v.is_finite()) {
        return None;
    }
    if path_svg.trim().is_empty() {
        return Some(String::new());
    }
    let path = vector::parse_svg(path_svg).ok()?;
    Some(vector::to_svg(&path.transform(&to_vector_affine(t))))
}

fn bezier_path(sp: &BezierSubpath) -> vector::Path {
    let mut p = vector::Path::new();
    let Some(first) = sp.knots.first() else {
        return p;
    };
    let pt = |a: [f64; 2]| vector::point(a[0], a[1]);
    p.move_to(pt(first[1]));
    for pair in sp.knots.windows(2) {
        p.curve_to(pt(pair[0][2]), pt(pair[1][0]), pt(pair[1][1]));
    }
    if sp.closed {
        let last = sp.knots.last().expect("non-empty");
        p.curve_to(pt(last[2]), pt(first[0]), pt(first[1]));
        p.close();
    }
    p
}

/// The region `subpaths` cover, combined in order, as SVG path data.
///
/// Subpaths that all union (or a single one) are written as they stand,
/// curves exact, filled non-zero. Any other combination is resolved through
/// `vector`'s boolean operations — the first subpath combining with an empty
/// area — which flatten curves to polylines within
/// `vector::DEFAULT_TOLERANCE`; a combination the engine refuses keeps the
/// concatenated outlines rather than dropping the mask.
pub fn svg_from_subpaths(subpaths: &[BezierSubpath]) -> String {
    let parts: Vec<(Combine, vector::Path)> = subpaths
        .iter()
        .filter(|sp| !sp.knots.is_empty())
        .map(|sp| (sp.combine, bezier_path(sp)))
        .collect();
    let concat = || {
        let mut all = vector::Path::new();
        for (_, p) in &parts {
            all.extend(p);
        }
        all
    };
    if parts.iter().skip(1).all(|(c, _)| *c == Combine::Union) {
        return vector::to_svg(&concat());
    }
    let mut acc: Option<vector::Path> = None;
    for (combine, p) in &parts {
        acc = Some(match acc {
            None => match combine {
                Combine::Subtract | Combine::Intersect => vector::Path::new(),
                _ => p.clone(),
            },
            Some(a) => {
                let r = match combine {
                    Combine::Xor => vector::xor(&a, p),
                    Combine::Subtract => vector::difference(&a, p),
                    Combine::Intersect => vector::intersection(&a, p),
                    Combine::Union => vector::union(&a, p),
                };
                match r {
                    Ok(r) => r,
                    Err(_) => return vector::to_svg(&concat()),
                }
            }
        });
    }
    vector::to_svg(&acc.unwrap_or_default())
}

fn knots_of(sp: &vector::SubPath, combine: Combine) -> BezierSubpath {
    let xy = |p: vector::Point| [p.x, p.y];
    let mut segs: Vec<[[f64; 2]; 4]> = sp
        .segments
        .iter()
        .map(|s| match s.to_cubic() {
            vector::Segment::Cubic(a, b, c, d) => [xy(a), xy(b), xy(c), xy(d)],
            // `to_cubic` answers a cubic for every kind; these keep the
            // match total.
            vector::Segment::Line(a, d) | vector::Segment::Quad(a, _, d) => {
                [xy(a), xy(a), xy(d), xy(d)]
            }
        })
        .collect();
    // A closed `.psd` subpath closes itself; a zero-length closing line
    // would add a duplicate knot.
    if sp.closed && segs.len() > 1 && segs.last().is_some_and(|s| s[0] == s[3]) {
        segs.pop();
    }
    let n = segs.len();
    let mut knots = Vec::with_capacity(n + 1);
    for (i, s) in segs.iter().enumerate() {
        let before = match (i, sp.closed) {
            (0, true) => segs[n - 1][2],
            (0, false) => s[0],
            _ => segs[i - 1][2],
        };
        knots.push([before, s[0], s[1]]);
    }
    if !sp.closed {
        if let Some(last) = segs.last() {
            knots.push([last[2], last[3], last[3]]);
        }
    }
    BezierSubpath {
        closed: sp.closed,
        combine,
        knots,
    }
}

/// `path_svg` mapped through `pose` as Bezier subpaths; `None` when it does
/// not parse or the pose is not finite.
///
/// One subpath is returned exactly (combine: union). Several are first
/// normalised by a non-zero union into non-crossing rings and returned with
/// the xor combine, which reproduces the non-zero fill exactly — at the cost
/// of flattening curves.
pub fn subpaths_from_svg(path_svg: &str, pose: Affine2) -> Option<Vec<BezierSubpath>> {
    let svg = svg_transformed(path_svg, pose)?;
    if svg.is_empty() {
        return Some(Vec::new());
    }
    let path = vector::parse_svg(&svg).ok()?;
    let subs: Vec<vector::SubPath> = path
        .subpaths()
        .into_iter()
        .filter(|s| !s.segments.is_empty())
        .collect();
    if subs.len() <= 1 {
        return Some(subs.iter().map(|s| knots_of(s, Combine::Union)).collect());
    }
    Some(
        match vector::fold(
            std::slice::from_ref(&path),
            vector::BoolOp::Union,
            FillRule::NonZero,
        ) {
            Ok(norm) => norm
                .subpaths()
                .iter()
                .filter(|s| !s.segments.is_empty())
                .map(|s| knots_of(s, Combine::Xor))
                .collect(),
            Err(_) => subs.iter().map(|s| knots_of(s, Combine::Union)).collect(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_triangle_survives_svg_to_knots_and_back() {
        let subs = subpaths_from_svg("M0 0 L16 0 L0 16 Z", Affine2::IDENTITY).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].knots.len(), 3);
        let back = vector::parse_svg(&svg_from_subpaths(&subs)).unwrap();
        let b = back.bounds();
        assert_eq!((b.min.x, b.min.y, b.max.x, b.max.y), (0.0, 0.0, 16.0, 16.0));
    }

    #[test]
    fn a_subtracted_subpath_cuts_a_hole() {
        let square = |a: f64, b: f64, combine| BezierSubpath {
            closed: true,
            combine,
            knots: [[a, a], [b, a], [b, b], [a, b]]
                .iter()
                .map(|p| [*p, *p, *p])
                .collect(),
        };
        let svg = svg_from_subpaths(&[
            square(0.0, 10.0, Combine::Union),
            square(3.0, 7.0, Combine::Subtract),
        ]);
        let cov = path_coverage(&svg, Affine2::IDENTITY, PixelRect::new(0, 0, 10, 10)).unwrap();
        assert_eq!(cov[10 + 1], 1.0);
        assert_eq!(cov[5 * 10 + 5], 0.0, "the hole");
    }

    #[test]
    fn a_path_is_moved_into_another_space() {
        let t = Affine2::from_translation(glam::Vec2::new(-8.0, -8.0));
        let svg = svg_transformed("M8 8 L12 8 L12 12 Z", t).unwrap();
        let b = vector::parse_svg(&svg).unwrap().bounds();
        assert_eq!((b.min.x, b.min.y), (0.0, 0.0));
        assert!(svg_transformed("M0 0", Affine2::from_scale(glam::Vec2::NAN)).is_none());
    }

    #[test]
    fn a_square_covers_its_inside_and_nothing_outside() {
        let cov = path_coverage(
            "M2 2 L6 2 L6 6 L2 6 Z",
            Affine2::IDENTITY,
            PixelRect::new(0, 0, 8, 8),
        )
        .unwrap();
        assert_eq!(cov[3 * 8 + 3], 1.0, "inside");
        assert_eq!(cov[0], 0.0, "outside");
        assert_eq!(cov[7 * 8 + 7], 0.0, "outside");
    }

    #[test]
    fn the_pose_moves_the_path_and_the_rect_offsets_the_read() {
        let pose = Affine2::from_translation(glam::Vec2::new(100.0, 50.0));
        let cov =
            path_coverage("M0 0 L4 0 L4 4 L0 4 Z", pose, PixelRect::new(100, 50, 8, 8)).unwrap();
        assert_eq!(cov[8 + 1], 1.0);
        assert_eq!(cov[6 * 8 + 6], 0.0);
    }

    #[test]
    fn empty_path_covers_nothing_and_garbage_is_refused() {
        let cov = path_coverage("  ", Affine2::IDENTITY, PixelRect::new(0, 0, 4, 4)).unwrap();
        assert!(cov.iter().all(|&v| v == 0.0));
        assert!(path_coverage("M 1 Q", Affine2::IDENTITY, PixelRect::new(0, 0, 4, 4)).is_none());
    }
}
