//! Colour-driven selection: magic wand, quick select, colour range, grow and
//! similar.
//!
//! All of these ask the same question — "is this pixel close enough to that
//! one?" — and answer it with [`crate::metric`], so the space the tolerance is
//! measured in is one setting rather than five hard-coded assumptions. All of
//! them produce **fractional** coverage when anti-aliasing is on, and all of
//! them trim their result to what is actually selected, so a wand click on a
//! small object in a huge image yields a small mask.

use editor_core::SelectionMask;
use glam::{IVec2, Vec2};
use serde::{Deserialize, Serialize};

use crate::buf::{alloc_vec, checked_samples, try_push, CoverageBuf};
use crate::error::SelectionOpError;
use crate::image::ImageView;
use crate::metric::{distance, tolerance_coverage, ColorCoords, ColorMetric};
use crate::rect::Rect;

const NEIGHBOURS: [IVec2; 4] = [
    IVec2::new(1, 0),
    IVec2::new(-1, 0),
    IVec2::new(0, 1),
    IVec2::new(0, -1),
];

/// Magic wand settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WandOptions {
    /// Largest accepted per-channel difference, in the metric's normalised
    /// units. The familiar 8-bit "tolerance 32" is `32.0 / 255.0`.
    pub tolerance: f32,
    /// Restrict the selection to the region connected to the seed.
    pub contiguous: bool,
    /// Fraction of the tolerance over which coverage ramps out, `0.0` for a
    /// hard edge and `1.0` for a ramp starting at an exact match.
    pub antialias: f32,
    /// Space the tolerance is measured in.
    pub metric: ColorMetric,
    /// Count a difference in alpha as a colour difference.
    pub sample_alpha: bool,
}

impl Default for WandOptions {
    fn default() -> Self {
        Self {
            tolerance: 32.0 / 255.0,
            contiguous: true,
            antialias: 0.0,
            metric: ColorMetric::default(),
            sample_alpha: false,
        }
    }
}

/// Select pixels whose colour is within `tolerance` of the seed's.
///
/// With `contiguous` set, only the region reachable from the seed through
/// accepted pixels is selected — the flood stops at the first pixel outside the
/// tolerance and does not resume on the far side of it.
pub fn magic_wand(
    img: &ImageView,
    seed: IVec2,
    opts: &WandOptions,
) -> Result<SelectionMask, SelectionOpError> {
    if !img.contains(seed) {
        return Err(SelectionOpError::SeedOutside {
            x: seed.x,
            y: seed.y,
        });
    }
    let target = img.coords_at(seed, opts.metric);
    let rect = img.rect();
    let mut out = CoverageBuf::zeroed(rect)?;

    if !opts.contiguous {
        for y in 0..rect.height() as usize {
            let dy = rect.min().y + y as i32;
            for x in 0..rect.width() as usize {
                let p = IVec2::new(rect.min().x + x as i32, dy);
                let d = distance(&img.coords_at(p, opts.metric), &target, opts.sample_alpha);
                out.row_mut(y)[x] = tolerance_coverage(d, opts.tolerance, opts.antialias);
            }
        }
        return out.into_mask();
    }

    let w = rect.width() as usize;
    let index =
        |p: IVec2| -> usize { (p.y - rect.min().y) as usize * w + (p.x - rect.min().x) as usize };
    let mut visited = alloc_vec(w * rect.height() as usize, false)?;
    let mut stack: Vec<IVec2> = Vec::new();
    try_push(&mut stack, seed)?;
    visited[index(seed)] = true;
    while let Some(p) = stack.pop() {
        let d = distance(&img.coords_at(p, opts.metric), &target, opts.sample_alpha);
        let c = tolerance_coverage(d, opts.tolerance, opts.antialias);
        if c == 0 {
            continue;
        }
        out.set(p, c);
        for step in NEIGHBOURS {
            let n = p + step;
            if rect.contains(n) && !visited[index(n)] {
                visited[index(n)] = true;
                try_push(&mut stack, n)?;
            }
        }
    }
    out.into_mask()
}

/// Quick-select settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct QuickSelectOptions {
    /// Brush radius, in pixels, around the stroke.
    pub radius: f32,
    /// Extra tolerance on top of the spread the stroke itself sampled.
    pub tolerance: f32,
    pub antialias: f32,
    pub metric: ColorMetric,
    pub sample_alpha: bool,
}

impl Default for QuickSelectOptions {
    fn default() -> Self {
        Self {
            radius: 8.0,
            tolerance: 16.0 / 255.0,
            antialias: 0.5,
            metric: ColorMetric::default(),
            sample_alpha: false,
        }
    }
}

fn distance_to_stroke(p: Vec2, stroke: &[Vec2]) -> f32 {
    if stroke.len() == 1 {
        return (p - stroke[0]).length();
    }
    stroke
        .windows(2)
        .map(|s| {
            let ab = s[1] - s[0];
            let len2 = ab.length_squared();
            if len2 <= f32::EPSILON {
                (p - s[0]).length()
            } else {
                let t = ((p - s[0]).dot(ab) / len2).clamp(0.0, 1.0);
                (p - (s[0] + ab * t)).length()
            }
        })
        .fold(f32::INFINITY, f32::min)
}

/// Grow a region out from a brush stroke.
///
/// The stroke is a sample, not a boundary: the pixels under it define both the
/// colour being selected and how varied that colour already is, and the region
/// then grows outward through everything within that spread. So a stroke laid
/// across a textured object selects the object rather than only the exact
/// shades the brush happened to touch.
pub fn quick_select(
    img: &ImageView,
    stroke: &[Vec2],
    opts: &QuickSelectOptions,
) -> Result<SelectionMask, SelectionOpError> {
    if stroke.is_empty() || img.rect().is_empty() {
        return Ok(SelectionMask::new(img.rect().min(), 0, 0, Vec::new())?);
    }
    for p in stroke {
        if !p.x.is_finite() || !p.y.is_finite() {
            return Err(SelectionOpError::NotFinite {
                what: "quick select stroke point",
                value: if p.x.is_finite() { p.y } else { p.x },
            });
        }
    }
    if !opts.radius.is_finite() {
        return Err(SelectionOpError::NotFinite {
            what: "quick select brush radius",
            value: opts.radius,
        });
    }
    let radius = opts.radius.clamp(0.0, crate::modify::MAX_RADIUS);
    let mut lo = stroke[0];
    let mut hi = stroke[0];
    for p in stroke {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    // The footprint the brush could possibly touch. `Rect::enclosing` is the
    // *corner* box of the stroke's extremes, so a tap — one point — and a
    // perfectly axis-aligned drag both give a box that is empty on at least one
    // axis when their coordinates are integers, and `Rect::inflate` leaves an
    // empty rect empty. Widening the box to the pixels the extremes fall in
    // before inflating is what keeps a click and a straight drag — the two most
    // common gestures there are — from selecting nothing at all. The sibling
    // band in `lasso::snap_segment` carries the same `+ IVec2::ONE`.
    let bbox = Rect::enclosing(lo, hi);
    let brush = Rect::new(bbox.min(), bbox.max() + IVec2::ONE)
        .inflate(radius.ceil() as i32 + 1)
        .intersection(img.rect());

    let rect = img.rect();
    let mut out = CoverageBuf::zeroed(rect)?;
    if brush.is_empty() {
        return out.into_mask();
    }

    // Which pixels of that footprint the stroke actually covers, as a bitmap
    // over the footprint rather than a list of points: an eighth of the memory,
    // it is the seed-membership test the flood needs anyway, and it goes
    // through the same guarded allocation as every other working buffer here.
    let bw = brush.width() as usize;
    let seed_index = |p: IVec2| -> usize {
        (p.y - brush.min().y) as usize * bw + (p.x - brush.min().x) as usize
    };
    let mut seeded = alloc_vec(checked_samples(brush)?, false)?;

    // Mean colour of the stroke, and how far the stroke itself already strays
    // from it. The second term is what lets one stroke cover a textured region.
    let mut mean: ColorCoords = [0.0; 4];
    let mut seed_count = 0u64;
    for y in brush.min().y..brush.max().y {
        for x in brush.min().x..brush.max().x {
            if distance_to_stroke(Vec2::new(x as f32 + 0.5, y as f32 + 0.5), stroke) > radius {
                continue;
            }
            let p = IVec2::new(x, y);
            seeded[seed_index(p)] = true;
            seed_count += 1;
            for (m, v) in mean.iter_mut().zip(img.coords_at(p, opts.metric)) {
                *m += v;
            }
        }
    }
    if seed_count == 0 {
        return out.into_mask();
    }
    for m in mean.iter_mut() {
        *m /= seed_count as f32;
    }
    let mut spread = 0.0f32;
    for y in brush.min().y..brush.max().y {
        for x in brush.min().x..brush.max().x {
            let p = IVec2::new(x, y);
            if seeded[seed_index(p)] {
                spread = spread.max(distance(
                    &img.coords_at(p, opts.metric),
                    &mean,
                    opts.sample_alpha,
                ));
            }
        }
    }
    let tol = opts.tolerance.max(0.0) + spread;

    let w = rect.width() as usize;
    let index =
        |p: IVec2| -> usize { (p.y - rect.min().y) as usize * w + (p.x - rect.min().x) as usize };
    let mut visited = alloc_vec(w * rect.height() as usize, false)?;
    let mut stack: Vec<IVec2> = Vec::new();
    for y in brush.min().y..brush.max().y {
        for x in brush.min().x..brush.max().x {
            let p = IVec2::new(x, y);
            if seeded[seed_index(p)] && !visited[index(p)] {
                visited[index(p)] = true;
                try_push(&mut stack, p)?;
            }
        }
    }

    while let Some(p) = stack.pop() {
        let d = distance(&img.coords_at(p, opts.metric), &mean, opts.sample_alpha);
        let c = if brush.contains(p) && seeded[seed_index(p)] {
            255
        } else {
            tolerance_coverage(d, tol, opts.antialias)
        };
        if c == 0 {
            continue;
        }
        out.set(p, c);
        for step in NEIGHBOURS {
            let n = p + step;
            if rect.contains(n) && !visited[index(n)] {
                visited[index(n)] = true;
                try_push(&mut stack, n)?;
            }
        }
    }
    out.into_mask()
}

/// Colour-range settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorRangeOptions {
    /// Distance at which coverage reaches zero. Coverage falls off linearly
    /// from a full match, so this is a soft selection by construction.
    pub fuzziness: f32,
    pub metric: ColorMetric,
    pub sample_alpha: bool,
}

impl Default for ColorRangeOptions {
    fn default() -> Self {
        Self {
            fuzziness: 40.0 / 255.0,
            metric: ColorMetric::default(),
            sample_alpha: false,
        }
    }
}

/// Select every pixel by similarity to one colour, with a soft falloff.
///
/// Unlike the magic wand this is never contiguous and never binary: an exact
/// match is fully selected and coverage ramps to zero at `fuzziness`, which is
/// what makes it usable for masking a colour cast rather than an object.
pub fn color_range(
    img: &ImageView,
    target: [u8; 4],
    opts: &ColorRangeOptions,
) -> Result<SelectionMask, SelectionOpError> {
    let target = opts.metric.coords(target);
    let rect = img.rect();
    let mut out = CoverageBuf::zeroed(rect)?;
    let fuzz = opts.fuzziness.max(0.0);
    for y in 0..rect.height() as usize {
        let dy = rect.min().y + y as i32;
        for x in 0..rect.width() as usize {
            let p = IVec2::new(rect.min().x + x as i32, dy);
            let d = distance(&img.coords_at(p, opts.metric), &target, opts.sample_alpha);
            // A full ramp from an exact match: antialias = 1.0.
            out.row_mut(y)[x] = tolerance_coverage(d, fuzz, 1.0);
        }
    }
    out.into_mask()
}

/// Select by luminance, with a soft shoulder outside the range.
///
/// Luminance is **linear** — `Y` from the Rec.709 primaries after undoing the
/// sRGB curve — because "how bright is this pixel" is a physical question, not
/// a per-channel appearance comparison like [`color_range`]. A midtone range of
/// `0.18..=0.5` therefore means what a light meter would say, not what the
/// 8-bit codes look like.
pub fn luminance_range(
    img: &ImageView,
    min: f32,
    max: f32,
    softness: f32,
) -> Result<SelectionMask, SelectionOpError> {
    for (what, v) in [
        ("luminance range minimum", min),
        ("luminance range maximum", max),
        ("luminance range softness", softness),
    ] {
        if !v.is_finite() {
            return Err(SelectionOpError::NotFinite { what, value: v });
        }
    }
    let (lo, hi) = (min.min(max), min.max(max));
    let soft = softness.max(0.0);
    let rect = img.rect();
    let mut out = CoverageBuf::zeroed(rect)?;
    for y in 0..rect.height() as usize {
        let dy = rect.min().y + y as i32;
        for x in 0..rect.width() as usize {
            let px = img.pixel(IVec2::new(rect.min().x + x as i32, dy));
            let l = color::srgb_luminance([
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ]);
            let d = if l < lo {
                lo - l
            } else if l > hi {
                l - hi
            } else {
                0.0
            };
            out.row_mut(y)[x] = tolerance_coverage(d, soft, 1.0);
        }
    }
    out.into_mask()
}

/// Extend the selection into neighbouring pixels of similar colour.
///
/// Photoshop's *Grow*: contiguous, and comparison is against the pixel the
/// growth came from rather than against a single seed, so a slow gradient is
/// followed while a hard boundary still stops it.
///
/// Growth only happens where there are pixels to compare, so it is confined to
/// `img`'s rectangle — but coverage that lies **outside** the view is carried
/// through untouched rather than deleted. An operation named *grow* must never
/// shrink a selection, and a mask that overhangs the image is ordinary: a
/// feather or an expand near the canvas edge produces one.
pub fn grow(
    img: &ImageView,
    mask: &SelectionMask,
    tolerance: f32,
    metric: ColorMetric,
    sample_alpha: bool,
) -> Result<SelectionMask, SelectionOpError> {
    let rect = img.rect();
    let mut out = CoverageBuf::zeroed(rect)?;
    if rect.is_empty() {
        return Ok(mask.clone());
    }
    let w = rect.width() as usize;
    let index =
        |p: IVec2| -> usize { (p.y - rect.min().y) as usize * w + (p.x - rect.min().x) as usize };
    let mut visited = alloc_vec(w * rect.height() as usize, false)?;
    let mut stack: Vec<IVec2> = Vec::new();
    for y in rect.min().y..rect.max().y {
        for x in rect.min().x..rect.max().x {
            let p = IVec2::new(x, y);
            let c = mask.coverage_at(p);
            if c > 0 {
                out.set(p, c);
                if !visited[index(p)] {
                    visited[index(p)] = true;
                    try_push(&mut stack, p)?;
                }
            }
        }
    }
    while let Some(p) = stack.pop() {
        let from = img.coords_at(p, metric);
        for step in NEIGHBOURS {
            let n = p + step;
            if !rect.contains(n) || visited[index(n)] {
                continue;
            }
            if distance(&img.coords_at(n, metric), &from, sample_alpha) <= tolerance.max(0.0) {
                visited[index(n)] = true;
                out.raise(n, 255);
                try_push(&mut stack, n)?;
            }
        }
    }
    // Union rather than replace: whatever of the selection hung off the image
    // was never a candidate for growth, and must survive it.
    crate::boolean::combine(&out.into_mask()?, mask, crate::boolean::BooleanOp::Add)
}

/// Buckets per colour axis in [`similar`]'s acceptance table.
const BUCKETS: usize = 32;

fn bucket(v: f32) -> usize {
    ((v.clamp(0.0, 1.0) * (BUCKETS - 1) as f32).round() as usize).min(BUCKETS - 1)
}

fn lut_index(r: usize, g: usize, b: usize) -> usize {
    (r * BUCKETS + g) * BUCKETS + b
}

/// Dilate a boolean colour cube by `k` buckets in every axis.
///
/// A Chebyshev ball is a box and a box max is separable, which is why the
/// metric is Chebyshev: three one-dimensional passes give an exact answer where
/// a Euclidean metric would need a search per pixel.
///
/// `scratch` is the caller's, so the three passes allocate nothing.
fn dilate_cube(cube: &mut [bool], scratch: &mut [bool], k: usize) {
    if k == 0 {
        return;
    }
    for axis in 0..3usize {
        scratch.copy_from_slice(cube);
        let src = &*scratch;
        for r in 0..BUCKETS {
            for g in 0..BUCKETS {
                for b in 0..BUCKETS {
                    let c = match axis {
                        0 => r,
                        1 => g,
                        _ => b,
                    };
                    let lo = c.saturating_sub(k);
                    let hi = (c + k).min(BUCKETS - 1);
                    cube[lut_index(r, g, b)] = (lo..=hi).any(|i| match axis {
                        0 => src[lut_index(i, g, b)],
                        1 => src[lut_index(r, i, b)],
                        _ => src[lut_index(r, g, i)],
                    });
                }
            }
        }
    }
}

/// Select every pixel in the image whose colour resembles one already selected.
///
/// Photoshop's *Similar*: global rather than contiguous. The selected colours
/// are collected into a `32³` cube of the metric space, the cube is widened by
/// the tolerance, and every pixel is then one lookup — so the cost is one pass
/// over the image regardless of how many distinct colours are selected.
///
/// The quantisation is the price: the effective tolerance is accurate to about
/// half a bucket, `±1/62` of the axis. Alpha is not part of the cube, so this
/// matches on colour alone.
///
/// Like [`grow`], only pixels the view actually holds can be matched, and
/// coverage outside the view is carried through untouched rather than dropped.
pub fn similar(
    img: &ImageView,
    mask: &SelectionMask,
    tolerance: f32,
    metric: ColorMetric,
) -> Result<SelectionMask, SelectionOpError> {
    let rect = img.rect();
    let mut out = CoverageBuf::zeroed(rect)?;
    if rect.is_empty() {
        return Ok(mask.clone());
    }
    let mut cube = alloc_vec(BUCKETS * BUCKETS * BUCKETS, false)?;
    let mut any = false;
    for y in rect.min().y..rect.max().y {
        for x in rect.min().x..rect.max().x {
            let p = IVec2::new(x, y);
            let c = mask.coverage_at(p);
            if c > 0 {
                out.set(p, c);
            }
            if c >= 128 {
                let k = img.coords_at(p, metric);
                cube[lut_index(bucket(k[0]), bucket(k[1]), bucket(k[2]))] = true;
                any = true;
            }
        }
    }
    if !any {
        return Ok(mask.clone());
    }
    let k = (tolerance.max(0.0) * (BUCKETS - 1) as f32).round() as usize;
    let mut scratch = alloc_vec(cube.len(), false)?;
    dilate_cube(&mut cube, &mut scratch, k.min(BUCKETS - 1));

    for y in rect.min().y..rect.max().y {
        for x in rect.min().x..rect.max().x {
            let p = IVec2::new(x, y);
            let c = img.coords_at(p, metric);
            if cube[lut_index(bucket(c[0]), bucket(c[1]), bucket(c[2]))] {
                out.raise(p, 255);
            }
        }
    }
    // Coverage outside the view had no chance to match; it is kept, not cut.
    crate::boolean::combine(&out.into_mask()?, mask, crate::boolean::BooleanOp::Add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageBuffer;
    use crate::marquee::rectangle;

    /// 16x16. Left half `a`, right half `b`, plus a 2x2 patch of `a` marooned
    /// in the right half at (12, 12).
    fn two_tone(a: [u8; 4], b: [u8; 4]) -> ImageBuffer {
        let (w, h) = (16u32, 16u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let c = if x < 8 { a } else { b };
                px[i..i + 4].copy_from_slice(&c);
            }
        }
        for y in 12..14u32 {
            for x in 12..14u32 {
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&a);
            }
        }
        ImageBuffer::from_rgba8(IVec2::ZERO, w, h, px).unwrap()
    }

    fn covered(m: &SelectionMask) -> usize {
        m.coverage().iter().filter(|&&v| v > 0).count()
    }

    #[test]
    fn a_contiguous_wand_stops_at_the_boundary_and_a_global_one_does_not() {
        let img = two_tone([200, 40, 40, 255], [40, 200, 40, 255]);
        let opts = WandOptions {
            tolerance: 0.05,
            ..Default::default()
        };
        let contig = magic_wand(&img.view(), IVec2::new(2, 2), &opts).unwrap();
        assert_eq!(contig.bounds(), Some((IVec2::ZERO, IVec2::new(8, 16))));
        assert_eq!(covered(&contig), 8 * 16);
        assert_eq!(
            contig.coverage_at(IVec2::new(12, 12)),
            0,
            "the island is not reachable"
        );

        let global = magic_wand(
            &img.view(),
            IVec2::new(2, 2),
            &WandOptions {
                contiguous: false,
                ..opts
            },
        )
        .unwrap();
        assert_eq!(covered(&global), 8 * 16 + 4);
        assert_eq!(global.coverage_at(IVec2::new(12, 12)), 255);
    }

    #[test]
    fn the_wand_tolerance_boundary_is_exactly_where_it_says() {
        // The two colours differ by exactly 30 codes on one channel.
        let img = two_tone([100, 100, 100, 255], [130, 100, 100, 255]);
        let inside = magic_wand(
            &img.view(),
            IVec2::new(2, 2),
            &WandOptions {
                tolerance: 30.0 / 255.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            covered(&inside),
            16 * 16,
            "30/255 must include a 30-code step"
        );

        let outside = magic_wand(
            &img.view(),
            IVec2::new(2, 2),
            &WandOptions {
                tolerance: 29.0 / 255.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(covered(&outside), 8 * 16, "29/255 must exclude it");
    }

    #[test]
    fn wand_antialiasing_produces_fractional_coverage_near_the_boundary() {
        let img = two_tone([100, 100, 100, 255], [115, 100, 100, 255]);
        let m = magic_wand(
            &img.view(),
            IVec2::new(2, 2),
            &WandOptions {
                tolerance: 30.0 / 255.0,
                antialias: 1.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            m.coverage_at(IVec2::new(2, 2)),
            255,
            "an exact match is solid"
        );
        let edge = m.coverage_at(IVec2::new(10, 2));
        assert!(
            edge > 0 && edge < 255,
            "half a tolerance away should be partial, got {edge}"
        );
        // Same image with anti-aliasing off is strictly binary.
        let hard = magic_wand(
            &img.view(),
            IVec2::new(2, 2),
            &WandOptions {
                tolerance: 30.0 / 255.0,
                antialias: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(hard.coverage().iter().all(|&v| v == 0 || v == 255));
    }

    #[test]
    fn alpha_only_separates_regions_when_sampling_alpha_is_on() {
        let img = two_tone([80, 80, 80, 255], [80, 80, 80, 0]);
        let ignoring = magic_wand(&img.view(), IVec2::new(2, 2), &WandOptions::default()).unwrap();
        assert_eq!(covered(&ignoring), 16 * 16);
        let respecting = magic_wand(
            &img.view(),
            IVec2::new(2, 2),
            &WandOptions {
                sample_alpha: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(covered(&respecting), 8 * 16);
    }

    #[test]
    fn a_seed_outside_the_image_is_an_error_not_a_panic() {
        let img = two_tone([1, 1, 1, 255], [2, 2, 2, 255]);
        assert!(matches!(
            magic_wand(&img.view(), IVec2::new(-1, 0), &WandOptions::default()),
            Err(SelectionOpError::SeedOutside { x: -1, y: 0 })
        ));
        assert!(matches!(
            magic_wand(&img.view(), IVec2::new(0, 999), &WandOptions::default()),
            Err(SelectionOpError::SeedOutside { .. })
        ));
    }

    #[test]
    fn quick_select_grows_a_textured_region_from_one_stroke() {
        // Left half is noisy grey (values 100..108), right half is flat white.
        let (w, h) = (24u32, 24u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let c = if x < 12 {
                    let v = 100 + ((x * 3 + y * 5) % 9) as u8;
                    [v, v, v, 255]
                } else {
                    [250, 250, 250, 255]
                };
                px[i..i + 4].copy_from_slice(&c);
            }
        }
        let img = ImageBuffer::from_rgba8(IVec2::ZERO, w, h, px).unwrap();
        let m = quick_select(
            &img.view(),
            &[Vec2::new(3.5, 4.5), Vec2::new(3.5, 18.5)],
            &QuickSelectOptions {
                radius: 2.0,
                tolerance: 4.0 / 255.0,
                antialias: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        // The whole textured half, and none of the white half.
        assert_eq!(m.bounds(), Some((IVec2::ZERO, IVec2::new(12, 24))));
        assert_eq!(covered(&m), 12 * 24);

        // With no stroke there is nothing to grow from.
        assert!(
            quick_select(&img.view(), &[], &QuickSelectOptions::default())
                .unwrap()
                .is_empty()
        );
    }

    /// A flat 24x24, for gestures whose *shape* is what is under test.
    fn flat(v: u8) -> ImageBuffer {
        ImageBuffer::from_rgba8(IVec2::ZERO, 24, 24, vec![v; 24 * 24 * 4]).unwrap()
    }

    fn quick(img: &ImageBuffer, stroke: &[Vec2], radius: f32) -> SelectionMask {
        quick_select(
            &img.view(),
            stroke,
            &QuickSelectOptions {
                radius,
                tolerance: 4.0 / 255.0,
                antialias: 0.0,
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// A tap and a straight drag are the two commonest quick-select gestures,
    /// and at integer coordinates both used to select **nothing at all**: the
    /// corner box of a single point, or of a perfectly vertical or horizontal
    /// stroke, is empty on one axis, and inflating an empty rect leaves it
    /// empty, so the brush footprint never existed and there were no seeds.
    ///
    /// Half-pixel coordinates happened to dodge it, which is why this asserts
    /// the two agree: a fixture must not be able to hide the bug again by
    /// picking lucky coordinates.
    #[test]
    fn a_tap_and_an_axis_aligned_drag_select_the_same_at_integer_and_half_pixel_coordinates() {
        let img = flat(90);
        let all = 24 * 24;

        // A tap.
        assert_eq!(covered(&quick(&img, &[Vec2::new(12.0, 12.0)], 4.0)), all);
        assert_eq!(covered(&quick(&img, &[Vec2::new(12.5, 12.5)], 4.0)), all);

        // A perfectly vertical drag, and a perfectly horizontal one.
        for (int_stroke, half_stroke) in [
            (
                [Vec2::new(6.0, 4.0), Vec2::new(6.0, 18.0)],
                [Vec2::new(6.5, 4.0), Vec2::new(6.5, 18.0)],
            ),
            (
                [Vec2::new(4.0, 6.0), Vec2::new(18.0, 6.0)],
                [Vec2::new(4.0, 6.5), Vec2::new(18.0, 6.5)],
            ),
        ] {
            assert_eq!(
                covered(&quick(&img, &int_stroke, 4.0)),
                all,
                "{int_stroke:?} selected nothing"
            );
            assert_eq!(covered(&quick(&img, &half_stroke, 4.0)), all);
        }
    }

    /// ...and the footprint is a real disk of the brush radius, not merely
    /// non-empty: the same tap with a radius that reaches across a colour
    /// boundary samples the far side and the one that does not, does not.
    #[test]
    fn the_quick_select_brush_radius_decides_what_the_stroke_samples() {
        // 16x16, left half 100, right half 250, split at x = 8.
        let img = two_tone([100, 100, 100, 255], [250, 250, 250, 255]);
        let tap = [Vec2::new(5.0, 8.0)];

        // Radius 1: the disk is the four pixels at x in 4..=5, entirely in the
        // left tone, so the stroke measures no spread and the flood stops dead
        // at the boundary.
        let near = quick(&img, &tap, 1.0);
        assert_eq!(
            covered(&near),
            8 * 16,
            "a radius-1 tap must sample only the tone it landed on"
        );
        assert_eq!(near.coverage_at(IVec2::new(7, 8)), 255);
        assert_eq!(near.coverage_at(IVec2::new(8, 8)), 0);

        // Radius 4: the disk reaches x = 8, so the stroke samples both tones,
        // the spread it measures spans them, and the region grows across.
        let far = quick(&img, &tap, 4.0);
        assert_eq!(
            covered(&far),
            16 * 16,
            "a radius-4 tap straddles the boundary and takes both tones"
        );
    }

    #[test]
    fn colour_range_falls_off_smoothly_and_ignores_connectivity() {
        let img = two_tone([200, 40, 40, 255], [40, 200, 40, 255]);
        let m = color_range(
            &img.view(),
            [200, 40, 40, 255],
            &ColorRangeOptions {
                fuzziness: 0.9,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(m.coverage_at(IVec2::new(2, 2)), 255, "an exact match");
        let other = m.coverage_at(IVec2::new(14, 2));
        assert!(
            other > 0 && other < 255,
            "a far colour inside the fuzziness is partial, got {other}"
        );
        assert_eq!(
            m.coverage_at(IVec2::new(12, 12)),
            255,
            "the marooned patch is selected: colour range is not contiguous"
        );
    }

    #[test]
    fn luminance_range_selects_by_linear_brightness() {
        let (w, h) = (4u32, 1u32);
        let mut px = Vec::new();
        for v in [0u8, 90, 180, 255] {
            px.extend_from_slice(&[v, v, v, 255]);
        }
        let img = ImageBuffer::from_rgba8(IVec2::ZERO, w, h, px).unwrap();
        // Y for sRGB 90 is ~0.099, for 180 ~0.446, for 255 = 1.0.
        let m = luminance_range(&img.view(), 0.05, 0.5, 0.0).unwrap();
        assert_eq!(m.coverage_at(IVec2::new(0, 0)), 0);
        assert_eq!(m.coverage_at(IVec2::new(1, 0)), 255);
        assert_eq!(m.coverage_at(IVec2::new(2, 0)), 255);
        assert_eq!(m.coverage_at(IVec2::new(3, 0)), 0);

        // Softness bleeds into the neighbours instead of clipping.
        let soft = luminance_range(&img.view(), 0.05, 0.5, 0.6).unwrap();
        assert!(soft.coverage_at(IVec2::new(3, 0)) > 0);
        assert!(soft.coverage_at(IVec2::new(3, 0)) < 255);
    }

    #[test]
    fn grow_follows_a_gradient_but_stops_at_a_hard_edge() {
        // A horizontal ramp for 12 columns, then a jump to black.
        let (w, h) = (16u32, 4u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = if x < 12 { 100 + x as u8 * 4 } else { 0 };
                px[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let img = ImageBuffer::from_rgba8(IVec2::ZERO, w, h, px).unwrap();
        let seed = rectangle(Rect::from_xywh(0, 0, 1, 4)).unwrap();
        let g = grow(&img.view(), &seed, 5.0 / 255.0, ColorMetric::Srgb, false).unwrap();
        assert_eq!(
            g.bounds(),
            Some((IVec2::ZERO, IVec2::new(12, 4))),
            "a 4-code step per column is inside a 5-code tolerance; the jump to black is not"
        );
        assert_eq!(g.coverage_at(IVec2::new(12, 0)), 0);
    }

    /// A selection that hangs off the image is ordinary — `feather` and
    /// `expand` produce one at the canvas edge — and neither *grow* nor
    /// *similar* may quietly delete the overhang while claiming to extend the
    /// selection.
    #[test]
    fn grow_and_similar_keep_the_part_of_the_selection_outside_the_image() {
        let img = two_tone([200, 40, 40, 255], [40, 200, 40, 255]);
        // (-4,-4)..(2,2): four pixels of it lie on the image, twenty do not.
        let straddling = rectangle(Rect::new(IVec2::new(-4, -4), IVec2::new(2, 2))).unwrap();
        assert_eq!(straddling.coverage_at(IVec2::new(-4, -4)), 255);

        let g = grow(&img.view(), &straddling, 0.0, ColorMetric::Srgb, false).unwrap();
        assert_eq!(
            g.coverage_at(IVec2::new(-4, -4)),
            255,
            "grow deleted the off-image part of the selection"
        );
        assert_eq!(g.bounds().map(|(min, _)| min), Some(IVec2::new(-4, -4)));
        // ...and it still grew, into the matching left half.
        assert_eq!(g.coverage_at(IVec2::new(7, 15)), 255);
        assert_eq!(g.coverage_at(IVec2::new(8, 15)), 0);

        let s = similar(&img.view(), &straddling, 0.02, ColorMetric::Srgb).unwrap();
        assert_eq!(
            s.coverage_at(IVec2::new(-4, -4)),
            255,
            "similar deleted the off-image part of the selection"
        );
        assert_eq!(s.coverage_at(IVec2::new(12, 12)), 255, "and still matched");

        // An image the selection does not touch at all leaves it alone.
        let elsewhere = ImageBuffer::from_rgba8(IVec2::new(100, 100), 2, 2, vec![7; 16]).unwrap();
        assert_eq!(
            grow(
                &elsewhere.view(),
                &straddling,
                0.0,
                ColorMetric::Srgb,
                false
            )
            .unwrap(),
            straddling
        );
        assert_eq!(
            similar(&elsewhere.view(), &straddling, 0.0, ColorMetric::Srgb).unwrap(),
            straddling
        );
    }

    #[test]
    fn similar_finds_disconnected_pixels_of_the_same_colour() {
        let img = two_tone([200, 40, 40, 255], [40, 200, 40, 255]);
        // Seed with a single pixel of the left colour.
        let seed = rectangle(Rect::from_xywh(2, 2, 1, 1)).unwrap();
        let s = similar(&img.view(), &seed, 0.02, ColorMetric::Srgb).unwrap();
        assert_eq!(
            covered(&s),
            8 * 16 + 4,
            "every pixel of that colour, connected or not"
        );
        assert_eq!(s.coverage_at(IVec2::new(12, 12)), 255);
        assert_eq!(
            s.coverage_at(IVec2::new(10, 2)),
            0,
            "the other colour is untouched"
        );

        // A tolerance wide enough to bridge the two colours takes everything.
        let wide = similar(&img.view(), &seed, 1.0, ColorMetric::Srgb).unwrap();
        assert_eq!(covered(&wide), 16 * 16);
    }

    #[test]
    fn similar_keeps_the_original_coverage_it_was_given() {
        let img = two_tone([200, 40, 40, 255], [40, 200, 40, 255]);
        let mut b = CoverageBuf::zeroed(Rect::from_xywh(10, 2, 2, 1)).unwrap();
        b.set(IVec2::new(10, 2), 200); // a mostly-covered sample of the right half
        let seed = b.into_mask().unwrap();
        let s = similar(&img.view(), &seed, 0.02, ColorMetric::Srgb).unwrap();
        assert_eq!(
            s.coverage_at(IVec2::new(10, 2)),
            255,
            "its own colour matches, so it is promoted to solid"
        );
        assert_eq!(
            covered(&s),
            8 * 16 - 4,
            "and so is the rest of the right half"
        );
        // A coverage below the 128 sampling threshold contributes no colour to
        // the cube, so nothing grows and the faint sample survives untouched.
        let mut b2 = CoverageBuf::zeroed(Rect::from_xywh(2, 2, 1, 1)).unwrap();
        b2.set(IVec2::new(2, 2), 40);
        let faint = b2.into_mask().unwrap();
        let s2 = similar(&img.view(), &faint, 0.0, ColorMetric::Srgb).unwrap();
        assert_eq!(
            s2.coverage_at(IVec2::new(2, 2)),
            40,
            "nothing was sampled, nothing grew"
        );
    }
}
