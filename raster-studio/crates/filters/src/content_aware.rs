//! Content-aware fill and content-aware scale.
//!
//! Neither needs a learned model. [`content_aware_fill`] is PatchMatch
//! synthesis ([`crate::patchmatch`]) over the hole's neighbourhood;
//! [`content_aware_scale`] is seam carving (Avidan and Shamir, 2007): the
//! image is narrowed (or widened) one connected, minimum-energy seam at a
//! time, so the rows and columns that go are the ones crossing the least
//! detail, and a high-contrast object keeps its size while the flat ground
//! around it gives way.
//!
//! Both work on the crate's [`FilterBuffer`] (linear, premultiplied), so a
//! transparent region is synthesised or carved as transparency.

use crate::patchmatch::{self, InpaintError, InpaintParams, PATCH_SIZE};
use crate::{FilterBuffer, FilterError};

/// The most pixels the fill's context window may hold. The synthesis is
/// sequential and `O(pixels × patch²)` per round, so the ceiling is what keeps
/// one fill a few seconds rather than minutes; past it the fill refuses
/// rather than stalling.
pub const MAX_CONTEXT_PIXELS: usize = 2_000_000;

/// The context margin's bounds, in pixels, around the hole's bounding box.
pub const MIN_CONTEXT_MARGIN: u32 = 24;
pub const MAX_CONTEXT_MARGIN: u32 = 256;

/// The largest factor [`content_aware_scale`] will widen or heighten by.
pub const MAX_SCALE_UP: u32 = 2;

/// The most pixel-visits one [`content_aware_scale`] may cost: every seam
/// recomputes the energy and the cumulative map over the whole image, so the
/// work is `pixels × seams`. Past this the scale refuses rather than
/// stalling the editor for minutes.
pub const MAX_SCALE_WORK: u64 = 4_000_000_000;

/// Why a content-aware operation refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ContentAwareError {
    #[error("the hole mask has {got} entries for {expected} pixels")]
    BadMask { expected: usize, got: usize },
    #[error("nothing is selected to fill")]
    NoHole,
    #[error("there is no area outside the selection to sample from")]
    NoSource,
    #[error(
        "the area around the selection is too large for content-aware fill ({pixels} pixels; the limit is {limit})"
    )]
    TooLarge { pixels: usize, limit: usize },
    #[error("content-aware scale needs a target between 1 px and {max}x the source")]
    BadScale { max: u32 },
    #[error(
        "this image is too large to content-aware scale by that much ({work} pixel-seams; the limit is {limit})"
    )]
    ScaleTooLarge { work: u64, limit: u64 },
    #[error(transparent)]
    Filter(#[from] FilterError),
}

/// How a fill runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillOptions {
    /// Seeds the synthesis; the same seed gives the same bytes.
    pub seed: u64,
    /// The context kept around the hole's bounding box, or `None` for the
    /// default: the hole's own larger side, clamped to
    /// [`MIN_CONTEXT_MARGIN`]..=[`MAX_CONTEXT_MARGIN`].
    pub margin: Option<u32>,
}

impl Default for FillOptions {
    fn default() -> Self {
        Self {
            seed: InpaintParams::default().seed,
            margin: None,
        }
    }
}

/// Fill the pixels `hole` marks (row-major, one per pixel) from the rest of
/// the image, by multi-scale PatchMatch synthesis.
///
/// Only the hole's bounding box grown by the context margin is read and
/// synthesised; every pixel outside the hole comes back unchanged.
pub fn content_aware_fill(
    src: &FilterBuffer,
    hole: &[bool],
    options: FillOptions,
) -> Result<FilterBuffer, ContentAwareError> {
    let (w, h) = (src.width() as usize, src.height() as usize);
    if hole.len() != w * h {
        return Err(ContentAwareError::BadMask {
            expected: w * h,
            got: hole.len(),
        });
    }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if hole[y * w + x] {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 == usize::MAX {
        return Err(ContentAwareError::NoHole);
    }
    let extent = (x1 - x0 + 1).max(y1 - y0 + 1) as u32;
    let margin = options
        .margin
        .unwrap_or_else(|| extent.clamp(MIN_CONTEXT_MARGIN, MAX_CONTEXT_MARGIN))
        .max(PATCH_SIZE as u32) as usize;
    let cx0 = x0.saturating_sub(margin);
    let cy0 = y0.saturating_sub(margin);
    let cx1 = (x1 + margin + 1).min(w);
    let cy1 = (y1 + margin + 1).min(h);
    let (cw, ch) = (cx1 - cx0, cy1 - cy0);
    if cw * ch > MAX_CONTEXT_PIXELS {
        return Err(ContentAwareError::TooLarge {
            pixels: cw * ch,
            limit: MAX_CONTEXT_PIXELS,
        });
    }
    let all = src.pixels();
    let mut crop = Vec::with_capacity(cw * ch);
    let mut crop_hole = Vec::with_capacity(cw * ch);
    for y in cy0..cy1 {
        crop.extend_from_slice(&all[y * w + cx0..y * w + cx1]);
        crop_hole.extend_from_slice(&hole[y * w + cx0..y * w + cx1]);
    }
    let params = InpaintParams {
        seed: options.seed,
        ..InpaintParams::default()
    };
    let filled = patchmatch::inpaint(cw, ch, &crop, &crop_hole, params).map_err(|e| match e {
        InpaintError::BadLength => ContentAwareError::BadMask {
            expected: cw * ch,
            got: crop_hole.len(),
        },
        InpaintError::NoHole => ContentAwareError::NoHole,
        InpaintError::NoSource => ContentAwareError::NoSource,
    })?;
    let mut out = src.clone();
    let px = out.pixels_mut();
    for y in cy0..cy1 {
        for x in cx0..cx1 {
            if hole[y * w + x] {
                px[y * w + x] = filled[(y - cy0) * cw + (x - cx0)];
            }
        }
    }
    Ok(out)
}

/// Retarget `src` to `new_width × new_height` by seam carving.
///
/// The width changes first, then the height (by the same carving on the
/// transposed image). Narrowing removes the lowest-energy 8-connected seams
/// one at a time, recomputing the energy after each; widening finds the `k`
/// seams the same removal would take and duplicates each (as the mean of the
/// seam pixel and its right-hand neighbour), at most half the current width
/// per round so the same seam is not stretched over and over.
///
/// The energy is the gradient magnitude — `|∂x| + |∂y|` summed over the four
/// premultiplied channels — so a seam crossing an edge is expensive and a
/// high-contrast object is carved last. There is no protect-skin mask or
/// user-painted protection channel.
pub fn content_aware_scale(
    src: &FilterBuffer,
    new_width: u32,
    new_height: u32,
) -> Result<FilterBuffer, ContentAwareError> {
    let (w, h) = (src.width(), src.height());
    let bad = || ContentAwareError::BadScale { max: MAX_SCALE_UP };
    if new_width == 0 || new_height == 0 || w == 0 || h == 0 {
        return Err(bad());
    }
    if new_width > w.saturating_mul(MAX_SCALE_UP) || new_height > h.saturating_mul(MAX_SCALE_UP) {
        return Err(bad());
    }
    let seams = u64::from(w.abs_diff(new_width)) + u64::from(h.abs_diff(new_height));
    let work = u64::from(w.max(new_width)) * u64::from(h.max(new_height)) * seams;
    if work > MAX_SCALE_WORK {
        return Err(ContentAwareError::ScaleTooLarge {
            work,
            limit: MAX_SCALE_WORK,
        });
    }
    let mut img = Img {
        w: w as usize,
        h: h as usize,
        px: src.pixels().to_vec(),
    };
    img = retarget_width(img, new_width as usize);
    if new_height != h {
        img = retarget_width(img.transposed(), new_height as usize).transposed();
    }
    Ok(FilterBuffer::from_pixels(
        img.w as u32,
        img.h as u32,
        img.px,
    )?)
}

/// A plain row-major working image.
#[derive(Clone)]
struct Img {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Img {
    fn transposed(&self) -> Img {
        let mut px = Vec::with_capacity(self.px.len());
        for x in 0..self.w {
            for y in 0..self.h {
                px.push(self.px[y * self.w + x]);
            }
        }
        Img {
            w: self.h,
            h: self.w,
            px,
        }
    }

    /// Gradient-magnitude energy, clamped at the borders.
    fn energy(&self) -> Vec<f32> {
        let (w, h) = (self.w, self.h);
        let at = |x: usize, y: usize| self.px[y * w + x];
        let diff = |a: [f32; 4], b: [f32; 4]| {
            (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() + (a[3] - b[3]).abs()
        };
        let mut e = vec![0.0f32; w * h];
        for y in 0..h {
            let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for x in 0..w {
                let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
                e[y * w + x] = diff(at(xr, y), at(xl, y)) + diff(at(x, yd), at(x, yu));
            }
        }
        e
    }

    /// The minimum-energy vertical seam: one x per row, each within one
    /// column of the last. Ties go to the lowest x, so the answer is
    /// deterministic.
    fn seam(&self) -> Vec<usize> {
        let (w, h) = (self.w, self.h);
        let e = self.energy();
        let mut m = e.clone();
        for y in 1..h {
            for x in 0..w {
                let up = (y - 1) * w;
                let mut best = m[up + x];
                if x > 0 {
                    best = best.min(m[up + x - 1]);
                }
                if x + 1 < w {
                    best = best.min(m[up + x + 1]);
                }
                m[y * w + x] += best;
            }
        }
        let last = (h - 1) * w;
        let mut x = 0;
        for c in 1..w {
            if m[last + c] < m[last + x] {
                x = c;
            }
        }
        let mut seam = vec![0usize; h];
        seam[h - 1] = x;
        for y in (0..h - 1).rev() {
            let row = y * w;
            let lo = x.saturating_sub(1);
            let hi = (x + 1).min(w - 1);
            let mut best = lo;
            for c in lo..=hi {
                if m[row + c] < m[row + best] {
                    best = c;
                }
            }
            x = best;
            seam[y] = x;
        }
        seam
    }

    /// Remove one pixel per row at `seam`.
    fn remove(&mut self, seam: &[usize]) {
        let (w, h) = (self.w, self.h);
        let mut px = Vec::with_capacity((w - 1) * h);
        for (y, &sx) in seam.iter().enumerate() {
            let row = &self.px[y * w..(y + 1) * w];
            px.extend_from_slice(&row[..sx]);
            px.extend_from_slice(&row[sx + 1..]);
        }
        self.w -= 1;
        self.px = px;
    }
}

/// Narrow or widen `img` to `target` columns by seam carving.
fn retarget_width(mut img: Img, target: usize) -> Img {
    while img.w > target && img.w > 1 {
        let seam = img.seam();
        img.remove(&seam);
    }
    while img.w < target {
        let k = (target - img.w).min((img.w / 2).max(1));
        img = insert_seams(&img, k);
    }
    img
}

/// Widen `img` by `k` columns: find the `k` seams successive removals would
/// take (tracking each removed pixel's original column) and duplicate each.
fn insert_seams(img: &Img, k: usize) -> Img {
    let (w, h) = (img.w, img.h);
    let mut work = img.clone();
    // Original column of every pixel in the shrinking work image.
    let mut origin: Vec<Vec<usize>> = (0..h).map(|_| (0..w).collect()).collect();
    let mut chosen: Vec<Vec<usize>> = vec![Vec::with_capacity(k); h];
    for _ in 0..k.min(w.saturating_sub(1)).max(1) {
        if work.w <= 1 {
            break;
        }
        let seam = work.seam();
        for (y, &sx) in seam.iter().enumerate() {
            chosen[y].push(origin[y].remove(sx));
        }
        work.remove(&seam);
    }
    let added = chosen[0].len();
    let mut px = Vec::with_capacity((w + added) * h);
    for (y, cols) in chosen.iter_mut().enumerate() {
        cols.sort_unstable();
        let row = &img.px[y * w..(y + 1) * w];
        let mut next = 0;
        for x in 0..w {
            px.push(row[x]);
            while next < cols.len() && cols[next] == x {
                let b = row[(x + 1).min(w - 1)];
                let a = row[x];
                px.push([
                    (a[0] + b[0]) * 0.5,
                    (a[1] + b[1]) * 0.5,
                    (a[2] + b[2]) * 0.5,
                    (a[3] + b[3]) * 0.5,
                ]);
                next += 1;
            }
        }
    }
    Img {
        w: w + added,
        h,
        px,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

    /// A vertical-stripe image: `period`-pixel cycles, the first half black.
    fn stripes(w: u32, h: u32, period: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                px.push(if (x % period) < period / 2 {
                    BLACK
                } else {
                    WHITE
                });
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    /// A rectangular hole.
    fn rect_hole(w: u32, h: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<bool> {
        let mut hole = vec![false; (w * h) as usize];
        for y in y0..y1 {
            for x in x0..x1 {
                hole[(y * w + x) as usize] = true;
            }
        }
        hole
    }

    /// Filling a hole in vertical stripes reproduces the stripes: every
    /// filled column carries the value the stripe pattern puts there.
    #[test]
    fn filling_a_hole_in_vertical_stripes_reproduces_the_stripes() {
        let (w, h, period) = (72u32, 72u32, 8u32);
        let src = stripes(w, h, period);
        // Punch the hole with a colour the stripes never use, so a fill that
        // left it (or smeared it) cannot pass.
        let hole = rect_hole(w, h, 26, 26, 46, 46);
        let mut damaged = src.clone();
        for (i, px) in damaged.pixels_mut().iter_mut().enumerate() {
            if hole[i] {
                *px = [1.0, 0.0, 0.0, 1.0];
            }
        }
        let out = content_aware_fill(&damaged, &hole, FillOptions::default()).unwrap();
        let mut good = 0usize;
        let mut total = 0usize;
        for y in 26..46u32 {
            for x in 26..46u32 {
                total += 1;
                let want = src.get(x, y);
                let got = out.get(x, y);
                if (0..3).all(|c| (want[c] - got[c]).abs() < 0.25) {
                    good += 1;
                }
            }
        }
        assert!(
            good * 100 >= total * 95,
            "only {good}/{total} filled pixels follow the stripe pattern"
        );
        // Every filled column is uniform down its length, like a stripe.
        for x in 26..46u32 {
            let col: Vec<f32> = (26..46u32).map(|y| out.get(x, y)[0]).collect();
            let mean = col.iter().sum::<f32>() / col.len() as f32;
            let want = src.get(x, 0)[0];
            assert!(
                (mean - want).abs() < 0.2,
                "column {x}: mean {mean}, stripe value {want}"
            );
        }
        // Outside the hole nothing moved.
        for (i, (a, b)) in damaged.pixels().iter().zip(out.pixels()).enumerate() {
            if !hole[i] {
                assert_eq!(a, b, "pixel {i} outside the hole changed");
            }
        }
    }

    /// Filling inside a flat colour area yields that colour.
    #[test]
    fn filling_a_flat_area_yields_its_colour() {
        let c = [0.2, 0.4, 0.6, 1.0];
        let mut src = FilterBuffer::filled(48, 48, c).unwrap();
        let hole = rect_hole(48, 48, 16, 20, 30, 34);
        for (i, px) in src.pixels_mut().iter_mut().enumerate() {
            if hole[i] {
                *px = [1.0, 0.0, 0.0, 1.0];
            }
        }
        let out = content_aware_fill(&src, &hole, FillOptions::default()).unwrap();
        for (i, px) in out.pixels().iter().enumerate() {
            for ch in 0..4 {
                assert!(
                    (px[ch] - c[ch]).abs() < 1e-4,
                    "pixel {i} channel {ch}: {} vs {}",
                    px[ch],
                    c[ch]
                );
            }
        }
    }

    /// A textured source so matches are not all ties.
    fn texture(w: u32, h: u32) -> FilterBuffer {
        let mut rng = crate::rng::Rng::new(7);
        let px = (0..w * h)
            .map(|_| {
                let v = rng.next_f32();
                [v, v * 0.5, 1.0 - v, 1.0]
            })
            .collect();
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    /// A fixed seed gives the same bytes; another seed is free to differ.
    #[test]
    fn a_fixed_seed_is_deterministic() {
        let src = texture(56, 56);
        let hole = rect_hole(56, 56, 20, 20, 34, 34);
        let opts = FillOptions {
            seed: 42,
            margin: None,
        };
        let a = content_aware_fill(&src, &hole, opts).unwrap();
        let b = content_aware_fill(&src, &hole, opts).unwrap();
        assert_eq!(a, b, "the same seed gave different fills");
        let c = content_aware_fill(
            &src,
            &hole,
            FillOptions {
                seed: 43,
                margin: None,
            },
        )
        .unwrap();
        assert_ne!(a, c, "the seed does not reach the synthesis");
    }

    #[test]
    fn a_fill_refuses_without_a_hole_or_a_source() {
        let src = FilterBuffer::filled(20, 20, WHITE).unwrap();
        assert_eq!(
            content_aware_fill(&src, &[false; 400], FillOptions::default()),
            Err(ContentAwareError::NoHole)
        );
        assert_eq!(
            content_aware_fill(&src, &[true; 400], FillOptions::default()),
            Err(ContentAwareError::NoSource)
        );
        assert!(matches!(
            content_aware_fill(&src, &[true; 3], FillOptions::default()),
            Err(ContentAwareError::BadMask { .. })
        ));
    }

    /// The width of the dark run through row `y`.
    fn dark_width(img: &FilterBuffer, y: u32) -> u32 {
        (0..img.width()).filter(|&x| img.get(x, y)[0] < 0.5).count() as u32
    }

    /// A plain bilinear resample to `nw` columns — what Image Size does.
    fn plain_resample(src: &FilterBuffer, nw: u32) -> FilterBuffer {
        let (w, h) = (src.width(), src.height());
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..nw {
                let sx = (x as f32 + 0.5) * w as f32 / nw as f32 - 0.5;
                px.push(src.sample_bilinear(sx, y as f32, crate::EdgeMode::Clamp));
            }
        }
        FilterBuffer::from_pixels(nw, h, px).unwrap()
    }

    /// Content-aware scale to 80% width keeps a high-contrast object's width
    /// where a plain resample shrinks it by the same 80%.
    #[test]
    fn content_aware_scale_preserves_a_high_contrast_object_better_than_a_resample() {
        let (w, h) = (100u32, 60u32);
        let mut px = Vec::new();
        let mut rng = crate::rng::Rng::new(3);
        for y in 0..h {
            for x in 0..w {
                // A black 24-px square on a faintly noisy light ground, near
                // the left edge so that carving columns in scan order (what a
                // flat energy map degenerates to) would eat it.
                if (4..28).contains(&x) && (18..42).contains(&y) {
                    px.push(BLACK);
                } else {
                    let v = 0.9 + rng.next_f32() * 0.02;
                    px.push([v, v, v, 1.0]);
                }
            }
        }
        let src = FilterBuffer::from_pixels(w, h, px).unwrap();
        let carved = content_aware_scale(&src, 80, h).unwrap();
        assert_eq!((carved.width(), carved.height()), (80, 60));
        let resampled = plain_resample(&src, 80);
        let (orig, ca, plain) = (
            dark_width(&src, 30),
            dark_width(&carved, 30),
            dark_width(&resampled, 30),
        );
        assert_eq!(orig, 24);
        let ca_err = orig.abs_diff(ca);
        let plain_err = orig.abs_diff(plain);
        assert!(
            ca_err < plain_err,
            "content-aware width {ca} (error {ca_err}) vs resample {plain} (error {plain_err})"
        );
        assert!(ca_err <= 1, "the object lost {ca_err} columns to carving");
    }

    #[test]
    fn content_aware_scale_widens_and_heightens() {
        let src = texture(30, 20);
        let out = content_aware_scale(&src, 40, 25).unwrap();
        assert_eq!((out.width(), out.height()), (40, 25));
        let same = content_aware_scale(&src, 30, 20).unwrap();
        assert_eq!(same, src, "a no-op scale changed pixels");
        assert!(content_aware_scale(&src, 61, 20).is_err());
        assert!(content_aware_scale(&src, 0, 20).is_err());
    }
}
