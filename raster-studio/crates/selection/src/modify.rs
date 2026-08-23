//! Modifying an existing selection: feather, expand, contract, smooth, border,
//! invert.
//!
//! Every one of these is defined on **partial** coverage, because that is the
//! normal case — a wand with anti-aliasing on, an ellipse, anything already
//! feathered. Expand and contract are grayscale morphology (a running max and a
//! running min under a disk-shaped structuring element), which is the
//! definition that degenerates to binary dilate/erode when the input happens to
//! be binary and still does the right thing when it does not. The element is an
//! octagon rather than the exact discrete disk, because that is what makes the
//! cost independent of the radius — see [`morph`] for why that matters and what
//! the shape gives up.
//!
//! Coverage is a linear fraction of a pixel, so the Gaussian in [`feather`]
//! averages the coverage bytes directly. There is no transfer curve to
//! linearise here, and applying one would bend the ramp — see [`crate::buf`].

use std::collections::VecDeque;

use editor_core::{Selection, SelectionMask};
use glam::IVec2;
use rayon::prelude::*;

use crate::buf::{alloc_bytes, alloc_f32, to_byte, CoverageBuf};
use crate::error::SelectionOpError;
use crate::rect::Rect;

/// Largest feather / expand / contract / smooth / border radius.
///
/// # What this actually bounds
/// A cap only makes a caller-supplied radius safe if the work inside it is
/// affordable, so here is the real cost of each operation at radius `r` on a
/// `w * h` mask:
///
/// * [`expand`] / [`contract`] — `O(w' * h')` over the **result**, independent
///   of the radius: the structuring element decomposes into at most five passes
///   whose per-sample cost does not depend on the window (see [`morph`]).
///   Contract works in place, so `w' * h' = w * h`; expand pads, so
///   `w' * h' = (w + 2r) * (h + 2r)` — quadratic in the radius because the
///   *answer* is, not because the kernel is. Pinned by
///   `morphology_costs_the_same_at_the_largest_radius_as_at_the_smallest`.
/// * [`feather`] — `O((w + 2r) * (h + 2r) * r)`: a true Gaussian, two separable
///   passes of `2r + 1` taps. This is the expensive one.
/// * [`smooth`] — `O((w + 2r) * (h + 2r) * r)`: the disk sum is carried along
///   the row, so a sample costs one add and one subtract per disk row.
///
/// 512 is chosen against the worst of those rather than against the cheapest,
/// and matches where the mainstream editors land (Photoshop caps expand and
/// contract at 500 px). A caller wiring a UI field straight to these should
/// still expect a large feather to take real time — the cap keeps that bounded,
/// it does not make it free.
pub const MAX_RADIUS: f32 = 512.0;

fn check_radius(what: &'static str, radius: f32) -> Result<(), SelectionOpError> {
    if !radius.is_finite() {
        return Err(SelectionOpError::NotFinite {
            what,
            value: radius,
        });
    }
    if radius > MAX_RADIUS {
        return Err(SelectionOpError::RadiusTooLarge {
            what,
            value: radius,
            max: MAX_RADIUS,
        });
    }
    Ok(())
}

/// Half-widths of a discrete disk of radius `r`, indexed by `dy + r`.
fn disk_half_widths(r: u32) -> Vec<usize> {
    let rr = (r as i64) * (r as i64);
    (-(r as i64)..=(r as i64))
        .map(|dy| ((rr - dy * dy) as f64).sqrt().floor() as usize)
        .collect()
}

/// One-dimensional running max (`dilate`) or min along a line of samples, over
/// a window of half-width `r`.
///
/// A monotonic deque, so the pass is `O(len)` however wide the window is —
/// which is the whole reason [`morph`] costs the same at radius 512 as at
/// radius 2. Everything past the ends of the line reads 0: outside the buffer
/// nothing is selected, so an erosion whose window hangs off the line is 0
/// there, which is what lets `contract` eat into the edge of a mask.
fn line_pass(src: &[u8], r: usize, dilate: bool, dst: &mut [u8], dq: &mut VecDeque<usize>) {
    let n = src.len();
    if n == 0 {
        return;
    }
    dq.clear();
    let mut next = 0usize;
    for (x, out) in dst[..n].iter_mut().enumerate() {
        let hi = (x + r).min(n - 1);
        while next <= hi {
            let v = src[next];
            while let Some(&b) = dq.back() {
                if (dilate && src[b] <= v) || (!dilate && src[b] >= v) {
                    dq.pop_back();
                } else {
                    break;
                }
            }
            dq.push_back(next);
            next += 1;
        }
        let lo = x.saturating_sub(r);
        while let Some(&f) = dq.front() {
            if f < lo {
                dq.pop_front();
            } else {
                break;
            }
        }
        let front = *dq.front().expect("the window always holds x itself");
        *out = if !dilate && (x < r || x + r >= n) {
            0
        } else {
            src[front]
        };
    }
}

/// Scratch for one line at a time, so the passes allocate nothing per line.
struct Lines {
    input: Vec<u8>,
    output: Vec<u8>,
    dq: VecDeque<usize>,
}

/// One pass: the shape of the working buffer, and the window applied to it.
#[derive(Clone, Copy)]
struct Pass {
    w: usize,
    h: usize,
    r: usize,
    dilate: bool,
}

fn pass_rows(data: &mut [u8], p: Pass, l: &mut Lines) {
    if p.r == 0 || p.w == 0 {
        return;
    }
    for y in 0..p.h {
        let row = &mut data[y * p.w..y * p.w + p.w];
        l.input[..p.w].copy_from_slice(row);
        line_pass(
            &l.input[..p.w],
            p.r,
            p.dilate,
            &mut l.output[..p.w],
            &mut l.dq,
        );
        row.copy_from_slice(&l.output[..p.w]);
    }
}

fn pass_cols(data: &mut [u8], p: Pass, l: &mut Lines) {
    if p.r == 0 || p.h == 0 {
        return;
    }
    for x in 0..p.w {
        for y in 0..p.h {
            l.input[y] = data[y * p.w + x];
        }
        line_pass(
            &l.input[..p.h],
            p.r,
            p.dilate,
            &mut l.output[..p.h],
            &mut l.dq,
        );
        for y in 0..p.h {
            data[y * p.w + x] = l.output[y];
        }
    }
}

/// One diagonal line, gathered from `(sx, sy)` in direction `(1, ±1)`.
fn pass_one_diagonal(data: &mut [u8], p: Pass, start: (usize, usize), down: bool, l: &mut Lines) {
    let step = if down { 1i64 } else { -1i64 };
    let mut n = 0usize;
    let (mut x, mut y) = (start.0 as i64, start.1 as i64);
    while x < p.w as i64 && y >= 0 && y < p.h as i64 {
        l.input[n] = data[y as usize * p.w + x as usize];
        n += 1;
        x += 1;
        y += step;
    }
    line_pass(&l.input[..n], p.r, p.dilate, &mut l.output[..n], &mut l.dq);
    let (mut x, mut y) = (start.0 as i64, start.1 as i64);
    for i in 0..n {
        data[y as usize * p.w + x as usize] = l.output[i];
        x += 1;
        y += step;
    }
}

fn pass_diagonals(data: &mut [u8], p: Pass, down: bool, l: &mut Lines) {
    if p.r == 0 || p.w == 0 || p.h == 0 {
        return;
    }
    // Every diagonal starts on the left edge or on the edge it runs away from.
    let first_row = if down { 0 } else { p.h - 1 };
    for x in 0..p.w {
        pass_one_diagonal(data, p, (x, first_row), down, l);
    }
    for y in 0..p.h {
        if y != first_row {
            pass_one_diagonal(data, p, (0, y), down, l);
        }
    }
}

/// Morphology under the four-neighbour plus, the one piece of the octagon that
/// is not a sum of two line segments.
fn pass_plus(data: &mut [u8], p: Pass, tmp: &mut [u8]) {
    let (w, h) = (p.w, p.h);
    tmp[..w * h].copy_from_slice(&data[..w * h]);
    for y in 0..h {
        for x in 0..w {
            let mut v = tmp[y * w + x];
            let mut off_edge = false;
            for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    off_edge = true;
                    continue;
                }
                let n = tmp[ny as usize * w + nx as usize];
                v = if p.dilate { v.max(n) } else { v.min(n) };
            }
            data[y * w + x] = if !p.dilate && off_edge { 0 } else { v };
        }
    }
}

/// The octagon that stands in for a disk of radius `r`, given as the two shapes
/// it is the Minkowski sum of: a square of half-width `a` and a diamond of
/// half-width `b`.
///
/// `SQ(a) + DIA(b)` is exactly `{ |dx| <= a + b, |dy| <= a + b,
/// |dx| + |dy| <= 2a + b }`, so `a = m - r` and `b = 2r - m` reproduce
/// `{ |dx| <= r, |dy| <= r, |dx| + |dy| <= m }`. Taking `m = floor(r * sqrt 2)`
/// makes that the tightest octagon around the discrete disk: it reaches exactly
/// `r` on the axes, like the disk, and stops short of the square's corners.
fn octagon_parts(r: u32) -> (u32, u32) {
    if r == 0 {
        return (0, 0);
    }
    let m = (((r as f64) * std::f64::consts::SQRT_2).floor() as u32).clamp(r, 2 * r);
    (m - r, 2 * r - m)
}

/// Grayscale morphology under an octagonal structuring element.
///
/// # Why an octagon and not the disk itself
/// A disk is not separable, so taking the running max under one costs a pass
/// per disk row — `O(radius)` per sample, on a working rectangle that is itself
/// `O(radius)` on both axes. That is cubic in the radius: measured at ~7x per
/// doubling, an expand at a four-figure radius is minutes of unresponsive
/// compute, which is not a bound, it is a hang.
///
/// The octagon `SQ(a) + DIA(b)` is the tightest approximation to a disk that
/// decomposes into line segments — a horizontal, a vertical and two diagonal
/// ones, plus at most one four-neighbour pass when `b` is odd — and morphology
/// under a Minkowski sum is morphology under each part in turn. Each part is a
/// monotonic-deque pass whose cost does not depend on its window, so the whole
/// operation is a fixed handful of passes over the working rectangle at every
/// radius. The shape still reaches exactly the radius on the axes and still
/// rounds the corners: `the_structuring_element_is_a_disk_not_a_square` pins
/// that, and `morphology_costs_the_same_at_the_largest_radius_as_at_the_smallest`
/// pins the cost.
///
/// Composition is exact at the buffer edge in both directions. For erosion,
/// every point outside the buffer erodes to 0 and stays 0, so treating
/// off-the-line reads as 0 in each pass is the same as eroding a plane that is
/// 0 outside the mask. For dilation the rectangle is padded by the full radius
/// first, and no intermediate reaches further than that.
fn morph(src: &CoverageBuf, radius: u32, dilate: bool) -> Result<CoverageBuf, SelectionOpError> {
    if radius == 0 || src.rect().is_empty() {
        return Ok(src.clone());
    }
    let rect = if dilate {
        src.rect().inflate(radius as i32)
    } else {
        src.rect()
    };
    let mut buf = src.resized(rect)?;
    let (w, h) = (buf.width(), buf.height());
    let (a, b) = octagon_parts(radius);
    let line = w.max(h);
    let mut l = Lines {
        input: alloc_bytes(line, 0)?,
        output: alloc_bytes(line, 0)?,
        dq: VecDeque::new(),
    };
    let data = buf.data_mut();
    if a > 0 {
        let p = Pass {
            w,
            h,
            r: a as usize,
            dilate,
        };
        pass_rows(data, p, &mut l);
        pass_cols(data, p, &mut l);
    }
    if b >= 2 {
        // DIA(2q) is the sum of the two diagonal segments of half-length q.
        let p = Pass {
            w,
            h,
            r: (b / 2) as usize,
            dilate,
        };
        pass_diagonals(data, p, true, &mut l);
        pass_diagonals(data, p, false, &mut l);
    }
    if b % 2 == 1 {
        // ...and an odd diamond is that, plus one more unit of it.
        let mut tmp = alloc_bytes(w * h, 0)?;
        pass_plus(data, Pass { w, h, r: 1, dilate }, &mut tmp);
    }
    Ok(buf)
}

/// Grow the selection by `radius` pixels (morphological dilation by a disk).
///
/// The structuring element is the octagon of [`morph`]: it reaches exactly
/// `radius` along the axes, like a disk, and rounds the corners rather than
/// squaring them.
///
/// On partial coverage this is a running maximum, so a feathered edge is pushed
/// outward with its ramp intact rather than being hardened.
pub fn expand(mask: &SelectionMask, radius: u32) -> Result<SelectionMask, SelectionOpError> {
    check_radius("expand radius", radius as f32)?;
    morph(&CoverageBuf::from_mask(mask)?, radius, true)?.into_mask()
}

/// Shrink the selection by `radius` pixels (morphological erosion by the same
/// disk-shaped element [`expand`] grows by, so the two undo each other).
pub fn contract(mask: &SelectionMask, radius: u32) -> Result<SelectionMask, SelectionOpError> {
    check_radius("contract radius", radius as f32)?;
    morph(&CoverageBuf::from_mask(mask)?, radius, false)?.into_mask()
}

/// Gaussian-blur the coverage.
///
/// `radius` is the visible reach of the feather: σ is `radius / 3` and the
/// kernel is truncated at 3σ, so a hard edge becomes a monotone ramp extending
/// exactly `radius` pixels to each side of it, and coverage `radius + 1` pixels
/// inside the old edge is still solid. That relationship is what makes
/// "feather 4 px" mean the same thing as the dialog says it does.
pub fn feather(mask: &SelectionMask, radius: f32) -> Result<SelectionMask, SelectionOpError> {
    check_radius("feather radius", radius)?;
    let src = CoverageBuf::from_mask(mask)?;
    if radius <= 0.0 || src.rect().is_empty() {
        return src.into_mask();
    }
    let kr = radius.ceil() as i32;
    let sigma = (radius / 3.0).max(1e-4);
    let taps = (2 * kr + 1) as usize;
    let mut kernel = alloc_f32(taps)?;
    let mut sum = 0.0f32;
    for (i, k) in kernel.iter_mut().enumerate() {
        let d = i as f32 - kr as f32;
        *k = (-(d * d) / (2.0 * sigma * sigma)).exp();
        sum += *k;
    }
    for k in kernel.iter_mut() {
        *k /= sum;
    }

    let rect = src.rect().inflate(kr);
    let padded = src.resized(rect)?;
    let (w, h) = (padded.width(), padded.height());
    let mut a = alloc_f32(w * h)?;
    for (dst, &s) in a.iter_mut().zip(padded.data()) {
        *dst = s as f32 / 255.0;
    }

    // Horizontal pass; rows are independent, so they parallelise exactly.
    let mut b = alloc_f32(w * h)?;
    b.par_chunks_mut(w).enumerate().for_each(|(y, out_row)| {
        let src_row = &a[y * w..y * w + w];
        for (x, o) in out_row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (t, &k) in kernel.iter().enumerate() {
                let sx = x as i64 + t as i64 - kr as i64;
                if sx >= 0 && sx < w as i64 {
                    acc += k * src_row[sx as usize];
                }
            }
            *o = acc;
        }
    });

    // Vertical pass.
    a.par_chunks_mut(w).enumerate().for_each(|(y, out_row)| {
        for (x, o) in out_row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (t, &k) in kernel.iter().enumerate() {
                let sy = y as i64 + t as i64 - kr as i64;
                if sy >= 0 && sy < h as i64 {
                    acc += k * b[sy as usize * w + x];
                }
            }
            *o = acc;
        }
    });

    let mut out = CoverageBuf::zeroed(rect)?;
    for (d, &v) in out.data_mut().iter_mut().zip(a.iter()) {
        *d = to_byte(v);
    }
    out.into_mask()
}

/// Round off corners and drop speckles without softening straight edges.
///
/// Each sample becomes the mean coverage under a disk, pushed back through a
/// contrast curve steep enough that a straight edge lands back exactly where it
/// was (the curve's slope is derived from the disk's own geometry, so it holds
/// at every radius). An isolated pixel, whose disk mean is near zero,
/// disappears; a one-pixel notch in an edge fills in.
///
/// This is the one modifier here that can turn a binary selection into a
/// fractional one, and deliberately: the corners it rounds come out
/// anti-aliased rather than stair-stepped. The classical majority-vote
/// implementation would keep the input binary at the cost of a hard, jagged
/// arc. Pinned by `smoothing_a_binary_selection_antialiases_the_corners_it_rounds`.
pub fn smooth(mask: &SelectionMask, radius: u32) -> Result<SelectionMask, SelectionOpError> {
    check_radius("smooth radius", radius as f32)?;
    let src = CoverageBuf::from_mask(mask)?;
    if radius == 0 || src.rect().is_empty() {
        return src.into_mask();
    }
    let r = radius as i32;
    let rect = src.rect().inflate(r);
    let padded = src.resized(rect)?;
    let (w, h) = (padded.width(), padded.height());

    let widths = disk_half_widths(radius);
    let area: u64 = widths.iter().map(|&d| (2 * d + 1) as u64).sum();
    let central = (2 * radius as u64 + 1) as f32;
    // Slope of the contrast curve: enough that half a central column of
    // difference saturates it, which is exactly the margin a straight edge has.
    let slope = 2.5 * area as f32 / central;

    // The disk sum is carried along the row incrementally: stepping x by one
    // adds the sample entering each disk row and drops the one leaving it, so a
    // sample costs O(radius) with **no** auxiliary buffer at all.
    //
    // A prefix-sum table would cost the same time and 4 bytes per sample, and
    // the accumulator would have to hold a whole row: a row of a
    // 20-million-pixel-wide single-row marquee — which `single_row` really does
    // produce, and which `Rect` really does accept — sums past `u32`, wrapping
    // silently in release and aborting the process in debug. The running sum
    // here is `u64` and never holds more than one disk (255 * pi * 4096^2 at
    // the largest legal radius), so there is nothing to overflow and nothing to
    // allocate.
    let data = padded.data();
    let mut out = CoverageBuf::zeroed(rect)?;
    for y in 0..h {
        let mut total = 0u64;
        for (i, &dx) in widths.iter().enumerate() {
            let sy = y as i64 + i as i64 - r as i64;
            if sy < 0 || sy >= h as i64 {
                continue;
            }
            let base = sy as usize * w;
            // The window at x = 0 is [0 - dx, 0 + dx] clipped to the row.
            for &v in &data[base..base + (dx + 1).min(w)] {
                total += v as u64;
            }
        }
        let orow = out.row_mut(y);
        for (x, o) in orow.iter_mut().enumerate() {
            let mean = total as f32 / (255.0 * area as f32);
            let s = ((mean - 0.5) * slope + 0.5).clamp(0.0, 1.0);
            *o = to_byte(s * s * (3.0 - 2.0 * s));
            if x + 1 == w {
                break;
            }
            for (i, &dx) in widths.iter().enumerate() {
                let sy = y as i64 + i as i64 - r as i64;
                if sy < 0 || sy >= h as i64 {
                    continue;
                }
                let base = sy as usize * w;
                let entering = x + 1 + dx;
                if entering < w {
                    total += data[base + entering] as u64;
                }
                let leaving = x as i64 - dx as i64;
                if leaving >= 0 {
                    total -= data[base + leaving as usize] as u64;
                }
            }
        }
    }
    out.into_mask()
}

/// A band of the given total width straddling the selection's edge.
///
/// The band reaches `width / 2` pixels outside the edge and the remainder
/// inside, so a width of 1 is the innermost rim of the selection and a width of
/// 4 is two pixels either side.
pub fn border(mask: &SelectionMask, width: u32) -> Result<SelectionMask, SelectionOpError> {
    check_radius("border width", width as f32)?;
    if width == 0 {
        return Ok(SelectionMask::new(mask.origin(), 0, 0, Vec::new())?);
    }
    let outer_r = width / 2;
    let inner_r = width - outer_r;
    let outer = expand(mask, outer_r)?;
    let inner = contract(mask, inner_r)?;
    crate::boolean::combine(&outer, &inner, crate::boolean::BooleanOp::Subtract)
}

/// Invert the selection inside `canvas`.
///
/// A selection has no meaning outside a canvas — the complement of a finite
/// region in an infinite plane is infinite — so the canvas is an argument
/// rather than an assumption. Partial coverage inverts to `255 - coverage`,
/// which keeps an anti-aliased edge anti-aliased.
pub fn invert(mask: &SelectionMask, canvas: Rect) -> Result<SelectionMask, SelectionOpError> {
    if canvas.is_empty() {
        return Ok(SelectionMask::new(canvas.min(), 0, 0, Vec::new())?);
    }
    let mut out = CoverageBuf::zeroed(canvas)?;
    let w = canvas.width() as usize;
    for y in 0..canvas.height() as usize {
        let dy = canvas.min().y + y as i32;
        let row = out.row_mut(y);
        for (x, o) in row.iter_mut().enumerate() {
            *o = 255 - mask.coverage_at(IVec2::new(canvas.min().x + x as i32, dy));
        }
        debug_assert_eq!(row.len(), w);
    }
    out.into_mask()
}

/// [`invert`] for a document selection: inverting "no selection" yields an
/// empty selection, and inverting an empty one yields the whole canvas.
pub fn invert_selection(sel: &Selection, canvas: Rect) -> Result<Selection, SelectionOpError> {
    let mask = crate::boolean::to_mask(sel, canvas)?;
    Ok(Selection::Mask(invert(&mask, canvas)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marquee::{ellipse, rectangle};

    fn cov(m: &SelectionMask, x: i32, y: i32) -> u8 {
        m.coverage_at(IVec2::new(x, y))
    }

    #[test]
    fn feathering_a_hard_edge_gives_a_monotone_ramp_of_the_stated_width() {
        // A tall band so the vertical pass sees a constant column in the middle
        // and the profile is purely the horizontal one.
        let radius = 4.0f32;
        let kr = radius as i32;
        let src = rectangle(Rect::from_xywh(0, 0, 20, 60)).unwrap();
        let f = feather(&src, radius).unwrap();
        let y = 30;

        // Solid up to `radius + 1` inside the old edge (at x = 20).
        assert_eq!(cov(&f, 20 - kr - 1, y), 255);
        // Fractional across exactly 2 * radius pixels...
        assert!(cov(&f, 20 - kr, y) < 255);
        assert!(cov(&f, 20 + kr - 1, y) > 0);
        // ...and nothing at all `radius` past the edge.
        assert_eq!(cov(&f, 20 + kr, y), 0);

        // Monotone non-increasing all the way across the ramp.
        let mut prev = 255u8;
        for x in (20 - kr - 1)..=(20 + kr) {
            let v = cov(&f, x, y);
            assert!(v <= prev, "coverage rose at x={x}: {prev} -> {v}");
            prev = v;
        }
        // Symmetric about the edge: the two samples either side of it sum to a
        // whole pixel, which is what "the edge did not move" means.
        let sum = cov(&f, 19, y) as u16 + cov(&f, 20, y) as u16;
        assert!((253..=257).contains(&sum), "edge drifted: {sum}");
        assert!(cov(&f, 19, y) > 128 && cov(&f, 20, y) < 128);

        // And the ramp really is a Gaussian of sigma = radius/3, not merely
        // *some* falloff of the right reach. At 3 sigma a normalised Gaussian
        // has about 0.4% of its mass left, so the outermost sample of the ramp
        // is barely above zero and the innermost barely below solid. A wider
        // sigma truncated at the same radius would leave a visible step at both
        // ends instead.
        assert!(
            cov(&f, 20 + kr - 1, y) <= 3,
            "the outermost ramp sample is {}, so the kernel is far wider than 3 sigma \
             and the feather ends in a step",
            cov(&f, 20 + kr - 1, y)
        );
        assert!(
            cov(&f, 20 - kr, y) >= 252,
            "the innermost ramp sample is {}, so the kernel is far wider than 3 sigma",
            cov(&f, 20 - kr, y)
        );
        // The profile matches the analytic Gaussian CDF across the whole ramp.
        let sigma = radius / 3.0;
        for x in (20 - kr)..(20 + kr) {
            // Coverage of pixel x is the mass of the kernel that still falls on
            // the selected side, i.e. Phi((edge - centre) / sigma).
            let z = (20.0 - (x as f32 + 0.5)) / sigma;
            let phi = 0.5 * (1.0 + erf(z / std::f32::consts::SQRT_2));
            let got = cov(&f, x, y) as f32 / 255.0;
            assert!(
                (got - phi).abs() < 0.02,
                "at x={x} the profile is {got:.4}, a sigma={sigma} Gaussian gives {phi:.4}"
            );
        }
    }

    /// Abramowitz & Stegun 7.1.26, plenty for a 2% profile comparison.
    fn erf(x: f32) -> f32 {
        let sign = if x < 0.0 { -1.0f64 } else { 1.0 };
        let x = x.abs() as f64;
        let t = 1.0 / (1.0 + 0.327_591_1 * x);
        let y = 1.0
            - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736)
                * t
                + 0.254_829_592)
                * t
                * (-x * x).exp();
        (sign * y) as f32
    }

    #[test]
    fn feathering_preserves_total_coverage() {
        // A normalised Gaussian conserves mass, and the padding is wide enough
        // that none of it falls off the buffer.
        let src = ellipse(Rect::from_xywh(0, 0, 40, 40)).unwrap();
        let before: f64 = src.coverage().iter().map(|&v| v as f64).sum();
        let after: f64 = feather(&src, 5.0)
            .unwrap()
            .coverage()
            .iter()
            .map(|&v| v as f64)
            .sum();
        assert!(
            (after - before).abs() / before < 0.01,
            "{before} -> {after}"
        );
    }

    #[test]
    fn a_zero_or_negative_feather_changes_nothing() {
        let src = ellipse(Rect::from_xywh(0, 0, 9, 9)).unwrap();
        assert_eq!(feather(&src, 0.0).unwrap(), src);
        assert_eq!(feather(&src, -3.0).unwrap(), src);
        assert!(matches!(
            feather(&src, f32::NAN),
            Err(SelectionOpError::NotFinite { .. })
        ));
        assert!(matches!(
            feather(&src, 1e9),
            Err(SelectionOpError::RadiusTooLarge { .. })
        ));
    }

    #[test]
    fn expand_and_contract_move_the_edge_by_the_radius() {
        let src = rectangle(Rect::from_xywh(10, 10, 20, 20)).unwrap();
        let e = expand(&src, 3).unwrap();
        assert_eq!(cov(&e, 7, 20), 255, "3 pixels left of the old edge");
        assert_eq!(cov(&e, 6, 20), 0);
        // The structuring element is a disk, so the corner is rounded: a point
        // 3 pixels out diagonally is outside it, one 2 out diagonally is in.
        assert_eq!(cov(&e, 8, 8), 255);
        assert_eq!(cov(&e, 7, 7), 0, "the corner is rounded, not squared");

        let c = contract(&src, 3).unwrap();
        assert_eq!(c.bounds(), Some((IVec2::new(13, 13), IVec2::new(27, 27))));
        assert_eq!(cov(&c, 13, 13), 255);
        assert_eq!(cov(&c, 12, 20), 0);
    }

    #[test]
    fn expand_then_contract_by_the_same_amount_restores_the_original() {
        // Morphological closing. A rectangle is exactly closed under a disk, so
        // this is an equality, not an approximation.
        let src = rectangle(Rect::from_xywh(5, 5, 20, 14)).unwrap();
        for r in [1u32, 2, 3, 5] {
            let round = contract(&expand(&src, r).unwrap(), r).unwrap();
            assert_eq!(round, src, "closing by {r} moved a rectangle");
        }

        // On a curved, anti-aliased shape it is approximate, and the tolerance
        // is what "approximately" is allowed to mean.
        let disc = ellipse(Rect::from_xywh(0, 0, 41, 41)).unwrap();
        let round = contract(&expand(&disc, 3).unwrap(), 3).unwrap();
        let before: f64 = disc.coverage().iter().map(|&v| v as f64).sum();
        let after: f64 = round.coverage().iter().map(|&v| v as f64).sum();
        assert!(
            (after - before).abs() / before < 0.05,
            "closing a disc changed its area by more than 5%: {before} -> {after}"
        );
        assert_eq!(round.bounds(), disc.bounds());
    }

    /// The structuring element is still disk-shaped after being decomposed
    /// into line passes: it reaches exactly the radius along the axes, stops
    /// there, reaches the disk's distance along the diagonal, and does **not**
    /// reach the square's corner. A separable square — the naive way to make
    /// morphology radius-independent — fails the last of these at every radius.
    #[test]
    fn the_structuring_element_is_a_disk_not_a_square() {
        let dot = rectangle(Rect::from_xywh(0, 0, 1, 1)).unwrap();
        for r in [1u32, 2, 3, 4, 5, 8, 16, 37] {
            let e = expand(&dot, r).unwrap();
            let ri = r as i32;
            assert_eq!(cov(&e, ri, 0), 255, "r={r}: does not reach the radius");
            assert_eq!(cov(&e, 0, ri), 255, "r={r}");
            assert_eq!(cov(&e, ri + 1, 0), 0, "r={r}: reaches past the radius");
            assert_eq!(cov(&e, 0, ri + 1), 0, "r={r}");
            // r/sqrt(2) diagonally is inside the disk; (r, r) is not.
            let d = (r as f64 / std::f64::consts::SQRT_2).floor() as i32;
            assert_eq!(cov(&e, d, d), 255, "r={r}: the diagonal is clipped");
            assert_eq!(cov(&e, -d, d), 255, "r={r}");
            assert_eq!(cov(&e, ri, ri), 0, "r={r}: the element is a square");
        }
    }

    /// `MAX_RADIUS` is only an honest bound if the work inside it is bounded
    /// too. `contract` erodes in place — its working rectangle is the mask, the
    /// same at every radius — so its cost must be flat in the radius. It is,
    /// because the octagon decomposes into a fixed number of monotonic-deque
    /// passes whose per-sample cost does not depend on the window.
    ///
    /// The per-disk-row implementation this replaced was `O(w * h * r)` here,
    /// which on this fixture is a factor of ~340 between the two radii below.
    ///
    /// A ratio against a baseline measured on the same machine in the same run,
    /// never a wall-clock threshold: this has to measure the algorithm and not
    /// the machine it happens to be running on.
    #[test]
    fn morphology_costs_the_same_at_the_largest_radius_as_at_the_smallest() {
        let src = rectangle(Rect::from_xywh(0, 0, 256, 256)).unwrap();
        let best = |r: u32| {
            let mut best = std::time::Duration::MAX;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                let out = contract(&src, r).unwrap();
                let d = t.elapsed();
                std::hint::black_box(out);
                best = best.min(d);
            }
            best.max(std::time::Duration::from_nanos(1))
        };
        let small = best(2);
        let large = best(MAX_RADIUS as u32);
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        assert!(
            ratio < 15.0,
            "eroding a 256x256 mask took {ratio:.1}x as long at radius {} as at radius 2 \
             ({small:?} -> {large:?}), so the cost still scales with the radius",
            MAX_RADIUS as u32
        );
    }

    #[test]
    fn morphology_is_defined_on_partial_coverage() {
        // A dilation must carry a fractional value outward, not round it to
        // fully selected: that is the difference between grayscale morphology
        // and thresholding first.
        let mut b = CoverageBuf::zeroed(Rect::from_xywh(0, 0, 5, 5)).unwrap();
        b.set(IVec2::new(2, 2), 100);
        let m = b.into_mask().unwrap();
        let e = expand(&m, 1).unwrap();
        assert_eq!(cov(&e, 2, 2), 100);
        assert_eq!(cov(&e, 1, 2), 100, "the value spreads, unchanged");
        assert_eq!(
            cov(&e, 1, 1),
            0,
            "a disk of radius 1 is a plus, not a square"
        );

        // And erosion takes the minimum, so a partial pixel eats its
        // neighbours rather than being erased by them.
        let mut b2 = CoverageBuf::filled_with(Rect::from_xywh(0, 0, 7, 7), 255).unwrap();
        b2.set(IVec2::new(3, 3), 40);
        let m2 = b2.into_mask().unwrap();
        let c = contract(&m2, 1).unwrap();
        assert_eq!(cov(&c, 3, 3), 40);
        assert_eq!(cov(&c, 2, 3), 40);
        assert_eq!(cov(&c, 1, 3), 255);
    }

    #[test]
    fn contract_eats_a_selection_smaller_than_the_radius() {
        let src = rectangle(Rect::from_xywh(0, 0, 4, 4)).unwrap();
        assert!(contract(&src, 5).unwrap().is_empty());
        assert_eq!(expand(&src, 0).unwrap(), src);
        assert_eq!(contract(&src, 0).unwrap(), src);
    }

    #[test]
    fn smooth_drops_speckles_and_fills_notches_without_moving_a_straight_edge() {
        let mut b = CoverageBuf::filled_with(Rect::from_xywh(0, 0, 40, 40), 0).unwrap();
        for y in 8..32 {
            for x in 8..32 {
                b.set(IVec2::new(x, y), 255);
            }
        }
        // A lone speckle far from the block, and a one-pixel bite out of an edge.
        b.set(IVec2::new(2, 2), 255);
        b.set(IVec2::new(31, 20), 0);
        let m = b.into_mask().unwrap();

        let mut previous_corner = 256u16;
        for r in [1u32, 2, 4] {
            let s = smooth(&m, r).unwrap();
            assert_eq!(cov(&s, 2, 2), 0, "r={r}: the speckle survived");
            assert!(
                cov(&s, 31, 20) >= 250,
                "r={r}: the notch was not filled, got {}",
                cov(&s, 31, 20)
            );
            // Straight edges stay exactly where they were, at every radius.
            assert_eq!(cov(&s, 31, 12), 255, "r={r}: the edge softened inward");
            assert_eq!(cov(&s, 32, 12), 0, "r={r}: the edge crept outward");
            assert_eq!(cov(&s, 20, 20), 255, "r={r}: the interior changed");
            // Corners round off, and round off further as the radius grows.
            let corner = cov(&s, 8, 8) as u16;
            assert!(corner < 255, "r={r}: the corner stayed square");
            assert!(
                corner < previous_corner,
                "r={r}: a larger radius must round more, {previous_corner} -> {corner}"
            );
            previous_corner = corner;
        }
        assert_eq!(
            previous_corner, 0,
            "a radius-4 disk cuts the corner off entirely"
        );
        assert_eq!(smooth(&m, 0).unwrap(), m);
    }

    /// A row of coverage bytes sums past `u32` once the mask is wider than
    /// `u32::MAX / 255` = 16 843 009 columns, and `single_row` across a wide
    /// canvas produces exactly that mask for ~17 MB. Accumulating a whole row
    /// wrapped there: an abort in debug, silent garbage coverage in release.
    #[test]
    fn smoothing_a_mask_wider_than_a_u32_row_sum_keeps_its_coverage() {
        let w = 16_900_000u32;
        assert!(
            w as u64 * 255 > u32::MAX as u64,
            "the fixture has to be wide enough to overflow a u32 row sum"
        );
        // The same bar, narrow enough that nothing could overflow, is the
        // reference profile: a wide one must smooth to identical values.
        let reference = smooth(&crate::marquee::single_row(0, 0, 64).unwrap(), 1).unwrap();
        let expected = cov(&reference, 32, 0);
        assert!(
            expected > 200,
            "the reference interior should stay nearly solid, got {expected}"
        );

        let wide = smooth(&crate::marquee::single_row(0, 0, w).unwrap(), 1).unwrap();
        for x in [1i32, 1_000, 8_000_000, 16_000_000, w as i32 - 2] {
            assert_eq!(
                cov(&wide, x, 0),
                expected,
                "interior coverage is wrong at x={x} on a {w}-wide mask"
            );
        }
    }

    #[test]
    fn smoothing_a_binary_selection_antialiases_the_corners_it_rounds() {
        let src = rectangle(Rect::from_xywh(0, 0, 24, 24)).unwrap();
        let s = smooth(&src, 4).unwrap();
        assert!(
            s.coverage().iter().any(|&v| v > 0 && v < 255),
            "a rounded corner must be anti-aliased, not stair-stepped"
        );
        // Only the corners: interior and straight edges stay exactly binary.
        for y in 6..18 {
            for x in 0..24 {
                let v = cov(&s, x, y);
                assert_eq!(v, 255, "straight-edge band changed at {x},{y}");
            }
        }
    }

    #[test]
    fn a_border_is_a_band_of_the_requested_width_around_the_edge() {
        let src = rectangle(Rect::from_xywh(10, 10, 20, 20)).unwrap();
        let b = border(&src, 4).unwrap();
        // Two pixels out, two pixels in.
        assert_eq!(b.bounds(), Some((IVec2::new(8, 8), IVec2::new(32, 32))));
        assert_eq!(cov(&b, 8, 20), 255);
        assert_eq!(cov(&b, 7, 20), 0);
        assert_eq!(cov(&b, 11, 20), 255);
        assert_eq!(cov(&b, 12, 20), 0, "the interior is hollow");
        assert_eq!(cov(&b, 20, 20), 0);

        // Width 1 is the innermost rim.
        let thin = border(&src, 1).unwrap();
        assert_eq!(thin.bounds(), src.bounds());
        assert_eq!(cov(&thin, 10, 20), 255);
        assert_eq!(cov(&thin, 11, 20), 0);
        assert!(border(&src, 0).unwrap().is_empty());
    }

    #[test]
    fn inverting_keeps_partial_coverage_partial() {
        let canvas = Rect::from_xywh(0, 0, 32, 32);
        let disc = ellipse(Rect::from_xywh(4, 4, 24, 24)).unwrap();
        let inv = invert(&disc, canvas).unwrap();
        assert_eq!(cov(&inv, 16, 16), 0);
        assert_eq!(cov(&inv, 0, 0), 255);
        for y in 0..32 {
            for x in 0..32 {
                assert_eq!(
                    cov(&inv, x, y) as u16 + cov(&disc, x, y) as u16,
                    255,
                    "at {x},{y}"
                );
            }
        }
        // Inverting twice is the identity inside the canvas.
        let back = invert(&inv, canvas).unwrap();
        for y in 0..32 {
            for x in 0..32 {
                assert_eq!(cov(&back, x, y), cov(&disc, x, y), "at {x},{y}");
            }
        }
    }

    #[test]
    fn inverting_no_selection_selects_nothing() {
        let canvas = Rect::from_xywh(0, 0, 8, 8);
        let inv = invert_selection(&Selection::None, canvas).unwrap();
        assert!(inv.is_empty(), "everything inverts to nothing");
        assert!(
            !inv.is_none(),
            "and it is an empty selection, not the absence of one"
        );

        let empty = Selection::Rect {
            min: IVec2::ZERO,
            max: IVec2::ZERO,
        };
        let all = invert_selection(&empty, canvas).unwrap();
        assert_eq!(
            Rect::of_selection_bounds(&all),
            canvas,
            "nothing inverts to the whole canvas"
        );
    }
}
