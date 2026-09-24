//! Select ▸ Subject and the Object Selection rectangle, without a neural
//! model.
//!
//! Photopea's (and Photoshop's) Select Subject runs a trained segmentation
//! network. This build ships no model, so it uses a classical pipeline that
//! is honest about what it can see: **regions whose colour differs from the
//! image border**. The steps:
//!
//! 1. **Working grid.** The image is box-averaged down to at most
//!    [`SubjectOptions::working_side`] pixels on its long side (transparent
//!    pixels composited over mid grey first) and converted to CIELAB.
//! 2. **Saliency.** Two classical cues on the Lab image blurred at about a
//!    fortieth of its size (enough to wash out fine texture):
//!    * *frequency-tuned* contrast (Achanta et al. 2009): each pixel's
//!      distance from the image's mean colour;
//!    * a *border-connectivity background prior*: the geodesic distance from
//!      the image border, where a step costs the colour change it crosses
//!      minus the image's mean neighbour change (so a textured background is
//!      cheap to walk across and a colour edge is expensive).
//!
//!    Saliency is `geodesic * (0.5 + 0.5 * frequency_tuned)`, both normalised,
//!    times a gentle centre prior. The geodesic term dominates on purpose: a
//!    large subject that fills half the frame shifts the mean colour but is
//!    still walled off from the border.
//! 3. **Seeds.** Otsu's threshold splits the saliency map. The connected
//!    region above it with the largest total saliency is the subject seed; its
//!    most salient pixels are hard foreground. Low-saliency pixels connected
//!    to the border are hard background. Everything else is left to the cut.
//! 4. **GrabCut** ([`crate::grabcut`]) — Gaussian-mixture colour models and a
//!    max-flow/min-cut on the pixel grid — at the working resolution, keeping
//!    only the foreground connected to the hard seed.
//! 5. **Full-resolution refinement.** When the image was reduced, the cut is
//!    repeated at full resolution in a band one working cell either side of
//!    the boundary, with the learned colour models and the pixels outside the
//!    band fixed, so the edge follows the real pixels rather than the
//!    working grid.
//!
//! # Limits, stated plainly
//!
//! * It finds *one* region: the most salient one. Two separate people in a
//!   frame come back as whichever stands out more.
//! * It is colour-driven. A subject that shares its palette with the border
//!   (a white cat on snow), or that touches the border over most of its
//!   outline, separates poorly or not at all.
//! * It has no idea what a "subject" is: on a landscape it selects whatever
//!   region differs most from the edges of the frame.
//! * The mask is hard-edged (coverage 0 or 255); Refine Edge softens it.
//! * An image with no region that stands out (a flat fill, a uniform texture)
//!   selects nothing and says so ([`SubjectOutcome::NothingFound`]).
//!
//! Everything is deterministic: no random initialisation, no threads, so the
//! same pixels always give the same selection.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use editor_core::SelectionMask;
use glam::IVec2;

use crate::buf::{alloc_vec, try_heap_push, try_push, CoverageBuf};
use crate::error::SelectionOpError;
use crate::grabcut::{self, grabcut, Label, Models};
use crate::image::ImageView;
use crate::metric::ColorMetric;
use crate::rect::Rect;

/// The knobs of [`select_subject`] and [`select_object`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubjectOptions {
    /// Longest side of the working grid the saliency and first cut run on.
    pub working_side: u32,
    /// GrabCut rounds (each re-learns the colour models and re-cuts).
    pub rounds: u32,
}

impl Default for SubjectOptions {
    fn default() -> Self {
        Self {
            working_side: 384,
            rounds: 5,
        }
    }
}

/// What Select Subject found.
#[derive(Debug, Clone, PartialEq)]
pub enum SubjectOutcome {
    /// The subject's mask, trimmed to its own box.
    Selected(SelectionMask),
    /// Nothing stands out; the reason is a sentence for the status line.
    NothingFound(&'static str),
}

/// The sentence for an image with no region that differs from its border.
pub const NOTHING_STANDS_OUT: &str =
    "No subject found: no region of the image stands out from its surroundings";

/// Below this (CIELAB delta E) of contrast, nothing is considered to stand out.
const MIN_CONTRAST: f32 = 6.0;

/// A full-resolution refinement band larger than this many pixels is skipped
/// (the working-grid cut is upsampled instead), bounding memory to a few
/// hundred megabytes on any image.
const MAX_REFINE_NODES: usize = 3_000_000;

/// A working grid: the image reduced by an integer factor.
struct Grid {
    w: usize,
    h: usize,
    factor: usize,
    rgb: Vec<[f64; 3]>,
    lab: Vec<[f32; 3]>,
}

/// A straight-alpha pixel composited over mid grey, as RGB `0..=255`.
fn over_grey(px: [u8; 4]) -> [f64; 3] {
    let a = f64::from(px[3]) / 255.0;
    let g = 128.0 * (1.0 - a);
    [
        f64::from(px[0]) * a + g,
        f64::from(px[1]) * a + g,
        f64::from(px[2]) * a + g,
    ]
}

fn to_lab(rgb: [f64; 3]) -> [f32; 3] {
    let c = ColorMetric::Lab.coords([
        rgb[0].round().clamp(0.0, 255.0) as u8,
        rgb[1].round().clamp(0.0, 255.0) as u8,
        rgb[2].round().clamp(0.0, 255.0) as u8,
        255,
    ]);
    [c[0] * 100.0, c[1] * 256.0 - 128.0, c[2] * 256.0 - 128.0]
}

fn build_grid(img: &ImageView, side: u32) -> Result<Grid, SelectionOpError> {
    let (iw, ih) = (img.width(), img.height());
    let side = side.max(16) as usize;
    let factor = iw.max(ih).div_ceil(side).max(1);
    let (w, h) = (iw.div_ceil(factor), ih.div_ceil(factor));
    let mut rgb = alloc_vec(w * h, [0.0f64; 3])?;
    let mut lab = alloc_vec(w * h, [0.0f32; 3])?;
    let origin = img.rect().min();
    for cy in 0..h {
        for cx in 0..w {
            let mut acc = [0.0f64; 3];
            let mut n = 0.0;
            for y in cy * factor..((cy + 1) * factor).min(ih) {
                for x in cx * factor..((cx + 1) * factor).min(iw) {
                    let z = over_grey(img.pixel(origin + IVec2::new(x as i32, y as i32)));
                    acc[0] += z[0];
                    acc[1] += z[1];
                    acc[2] += z[2];
                    n += 1.0;
                }
            }
            let z = [acc[0] / n, acc[1] / n, acc[2] / n];
            rgb[cy * w + cx] = z;
            lab[cy * w + cx] = to_lab(z);
        }
    }
    Ok(Grid {
        w,
        h,
        factor,
        rgb,
        lab,
    })
}

/// Three passes of a clamped box blur of `radius` over a Lab grid.
fn blur(
    src: &[[f32; 3]],
    w: usize,
    h: usize,
    radius: usize,
) -> Result<Vec<[f32; 3]>, SelectionOpError> {
    let mut a = alloc_vec(w * h, [0.0f32; 3])?;
    a.copy_from_slice(src);
    let mut b = alloc_vec(w * h, [0.0f32; 3])?;
    let r = radius as i64;
    let norm = 1.0 / (2 * r + 1) as f32;
    for _ in 0..3 {
        for y in 0..h {
            for x in 0..w {
                let mut s = [0.0f32; 3];
                for k in -r..=r {
                    let xx = (x as i64 + k).clamp(0, w as i64 - 1) as usize;
                    let p = a[y * w + xx];
                    s[0] += p[0];
                    s[1] += p[1];
                    s[2] += p[2];
                }
                b[y * w + x] = [s[0] * norm, s[1] * norm, s[2] * norm];
            }
        }
        for y in 0..h {
            for x in 0..w {
                let mut s = [0.0f32; 3];
                for k in -r..=r {
                    let yy = (y as i64 + k).clamp(0, h as i64 - 1) as usize;
                    let p = b[yy * w + x];
                    s[0] += p[0];
                    s[1] += p[1];
                    s[2] += p[2];
                }
                a[y * w + x] = [s[0] * norm, s[1] * norm, s[2] * norm];
            }
        }
    }
    Ok(a)
}

fn lab_dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The saliency map in `0..=1`, or `None` when nothing has enough contrast to
/// stand out.
fn saliency(grid: &Grid) -> Result<Option<Vec<f32>>, SelectionOpError> {
    let (w, h) = (grid.w, grid.h);
    let n = w * h;
    let radius = (w.max(h) / 40).max(1);
    let lab = blur(&grid.lab, w, h, radius)?;

    // Frequency-tuned: distance from the mean colour.
    let mut mean = [0.0f64; 3];
    for p in &lab {
        for c in 0..3 {
            mean[c] += f64::from(p[c]);
        }
    }
    let mean = [
        (mean[0] / n as f64) as f32,
        (mean[1] / n as f64) as f32,
        (mean[2] / n as f64) as f32,
    ];
    let mut ft = alloc_vec(n, 0.0f32)?;
    for (f, p) in ft.iter_mut().zip(&lab) {
        *f = lab_dist(*p, mean);
    }

    // Border-connectivity: geodesic distance from the border.
    let mut steps = 0.0f64;
    let mut count = 0.0f64;
    for y in 0..h {
        for x in 0..w {
            if x + 1 < w {
                steps += f64::from(lab_dist(lab[y * w + x], lab[y * w + x + 1]));
                count += 1.0;
            }
            if y + 1 < h {
                steps += f64::from(lab_dist(lab[y * w + x], lab[(y + 1) * w + x]));
                count += 1.0;
            }
        }
    }
    let tau = if count > 0.0 {
        (steps / count) as f32
    } else {
        0.0
    };
    let mut geo = alloc_vec(n, f32::INFINITY)?;
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    for y in 0..h {
        for x in 0..w {
            if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                let i = y * w + x;
                geo[i] = 0.0;
                try_heap_push(&mut heap, Reverse((0, i as u32)))?;
            }
        }
    }
    while let Some(Reverse((bits, i))) = heap.pop() {
        let d = f32::from_bits(bits);
        let i = i as usize;
        if d > geo[i] {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let mut relax = |j: usize, heap: &mut BinaryHeap<Reverse<(u32, u32)>>| {
            let nd = d + (lab_dist(lab[i], lab[j]) - tau).max(0.0);
            if nd < geo[j] {
                geo[j] = nd;
                try_heap_push(heap, Reverse((nd.to_bits(), j as u32)))
            } else {
                Ok(())
            }
        };
        if x > 0 {
            relax(i - 1, &mut heap)?;
        }
        if x + 1 < w {
            relax(i + 1, &mut heap)?;
        }
        if y > 0 {
            relax(i - w, &mut heap)?;
        }
        if y + 1 < h {
            relax(i + w, &mut heap)?;
        }
    }

    let ft_max = ft.iter().copied().fold(0.0f32, f32::max);
    let geo_max = geo.iter().copied().fold(0.0f32, f32::max);
    if geo_max < MIN_CONTRAST || ft_max < MIN_CONTRAST {
        return Ok(None);
    }
    let mut s = alloc_vec(n, 0.0f32)?;
    let mut s_max = 0.0f32;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let nx = (x as f32 + 0.5) / w as f32 * 2.0 - 1.0;
            let ny = (y as f32 + 0.5) / h as f32 * 2.0 - 1.0;
            let centre = (-(nx * nx + ny * ny) / (2.0 * 0.6 * 0.6)).exp();
            let v = (geo[i] / geo_max) * (0.5 + 0.5 * ft[i] / ft_max) * (0.5 + 0.5 * centre);
            s[i] = v;
            s_max = s_max.max(v);
        }
    }
    if s_max <= 0.0 {
        return Ok(None);
    }
    for v in &mut s {
        *v /= s_max;
    }
    Ok(Some(s))
}

/// Otsu's threshold of values in `0..=1`.
fn otsu(values: &[f32]) -> f32 {
    let mut hist = [0u64; 256];
    for &v in values {
        hist[(v.clamp(0.0, 1.0) * 255.0) as usize] += 1;
    }
    let total = values.len() as f64;
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();
    let (mut w_b, mut sum_b, mut best, mut best_t) = (0.0f64, 0.0f64, -1.0f64, 128usize);
    for (t, &c) in hist.iter().enumerate() {
        w_b += c as f64;
        if w_b == 0.0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0.0 {
            break;
        }
        sum_b += t as f64 * c as f64;
        let m_b = sum_b / w_b;
        let m_f = (sum_all - sum_b) / w_f;
        let between = w_b * w_f * (m_b - m_f).powi(2);
        if between > best {
            best = between;
            best_t = t;
        }
    }
    (best_t as f32 + 0.5) / 255.0
}

/// 4-connected components of `mask`; returns the component id per pixel
/// (`u32::MAX` outside) and the number of components.
fn components(mask: &[bool], w: usize, h: usize) -> Result<(Vec<u32>, u32), SelectionOpError> {
    let mut id = alloc_vec(w * h, u32::MAX)?;
    let mut next = 0u32;
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if !mask[start] || id[start] != u32::MAX {
            continue;
        }
        id[start] = next;
        try_push(&mut stack, start)?;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize, stack: &mut Vec<usize>| {
                if mask[j] && id[j] == u32::MAX {
                    id[j] = next;
                    try_push(stack, j)
                } else {
                    Ok(())
                }
            };
            if x > 0 {
                visit(i - 1, &mut stack)?;
            }
            if x + 1 < w {
                visit(i + 1, &mut stack)?;
            }
            if y > 0 {
                visit(i - w, &mut stack)?;
            }
            if y + 1 < h {
                visit(i + w, &mut stack)?;
            }
        }
        next += 1;
    }
    Ok((id, next))
}

/// The Select Subject trimap from a saliency map, or `None` when no seed
/// region is plausible.
fn subject_trimap(s: &[f32], w: usize, h: usize) -> Result<Option<Vec<Label>>, SelectionOpError> {
    let n = w * h;
    let t = otsu(s).clamp(0.15, 0.85);
    let above: Vec<bool> = s.iter().map(|&v| v > t).collect();
    let (id, count) = components(&above, w, h)?;
    if count == 0 {
        return Ok(None);
    }
    let mut mass = alloc_vec(count as usize, 0.0f64)?;
    for (i, &c) in id.iter().enumerate() {
        if c != u32::MAX {
            mass[c as usize] += f64::from(s[i]);
        }
    }
    let seed = mass
        .iter()
        .enumerate()
        .fold(
            (0usize, -1.0f64),
            |b, (k, &m)| if m > b.1 { (k, m) } else { b },
        )
        .0 as u32;
    let area = id.iter().filter(|&&c| c == seed).count();
    if area * 1000 < n || area * 100 > n * 95 {
        return Ok(None);
    }
    let core = t + 0.6 * (1.0 - t);
    let mut tri = alloc_vec(n, Label::ProbablyBackground)?;
    let mut hard_fg = 0usize;
    for i in 0..n {
        if id[i] == seed {
            if s[i] >= core {
                tri[i] = Label::Foreground;
                hard_fg += 1;
            } else {
                tri[i] = Label::ProbablyForeground;
            }
        }
    }
    if hard_fg == 0 {
        // A flat-topped region: its single most salient pixel anchors it.
        let best = (0..n)
            .filter(|&i| id[i] == seed)
            .fold(None::<usize>, |b, i| match b {
                Some(j) if s[j] >= s[i] => Some(j),
                _ => Some(i),
            });
        if let Some(i) = best {
            tri[i] = Label::Foreground;
        }
    }
    // Hard background: low saliency connected to the border.
    let low = t * 0.5;
    let mut stack: Vec<usize> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if (x == 0 || y == 0 || x + 1 == w || y + 1 == h)
                && s[i] < low
                && tri[i] == Label::ProbablyBackground
            {
                tri[i] = Label::Background;
                try_push(&mut stack, i)?;
            }
        }
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for (ok, j) in [
            (x > 0, i.wrapping_sub(1)),
            (x + 1 < w, i + 1),
            (y > 0, i.wrapping_sub(w)),
            (y + 1 < h, i + w),
        ] {
            if ok && s[j] < low && tri[j] == Label::ProbablyBackground {
                tri[j] = Label::Background;
                try_push(&mut stack, j)?;
            }
        }
    }
    Ok(Some(tri))
}

/// Keep only the foreground 4-connected to a hard-foreground pixel.
fn keep_anchored(tri: &[Label], w: usize, h: usize) -> Result<Vec<bool>, SelectionOpError> {
    let fg: Vec<bool> = tri.iter().map(|l| l.is_foreground()).collect();
    let (id, count) = components(&fg, w, h)?;
    let mut anchored = alloc_vec(count as usize, false)?;
    for (i, l) in tri.iter().enumerate() {
        if *l == Label::Foreground && id[i] != u32::MAX {
            anchored[id[i] as usize] = true;
        }
    }
    Ok(id
        .iter()
        .map(|&c| c != u32::MAX && anchored[c as usize])
        .collect())
}

/// Scale the working-grid labels up to the image and, when the grid was
/// reduced, re-cut a band around the boundary at full resolution.
fn to_full_resolution(
    img: &ImageView,
    grid: &Grid,
    fg: &[bool],
    models: &Models,
    progress: &mut dyn FnMut(f32),
) -> Result<SelectionMask, SelectionOpError> {
    let rect = img.rect();
    let (iw, ih) = (img.width(), img.height());
    let (gw, gh, f) = (grid.w, grid.h, grid.factor);
    let mut out = CoverageBuf::zeroed(rect)?;
    for y in 0..ih {
        let row = out.row_mut(y);
        for (x, v) in row.iter_mut().enumerate() {
            if fg[(y / f) * gw + x / f] {
                *v = 255;
            }
        }
    }
    if f == 1 {
        progress(1.0);
        return out.into_mask();
    }
    // Band cells: a cell with a differently-labelled 8-neighbour.
    let mut base = alloc_vec(gw * gh, u32::MAX)?;
    let mut nodes = 0usize;
    let cell_nodes = f * f;
    for cy in 0..gh {
        for cx in 0..gw {
            let me = fg[cy * gw + cx];
            let mut edge = false;
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (cx as i64 + dx, cy as i64 + dy);
                    if nx >= 0 && ny >= 0 && (nx as usize) < gw && (ny as usize) < gh {
                        edge |= fg[ny as usize * gw + nx as usize] != me;
                    }
                }
            }
            if edge {
                base[cy * gw + cx] = nodes as u32;
                nodes += cell_nodes;
            }
        }
    }
    if nodes == 0 || nodes > MAX_REFINE_NODES {
        progress(1.0);
        return out.into_mask();
    }
    let origin = rect.min();
    let colour = |x: usize, y: usize| over_grey(img.pixel(origin + IVec2::new(x as i32, y as i32)));
    let node_of = |x: usize, y: usize| -> Option<u32> {
        let b = base[(y / f) * gw + x / f];
        (b != u32::MAX).then(|| b + ((y % f) * f + (x % f)) as u32)
    };
    // Beta over the band's own pixel pairs.
    let (mut sum, mut count) = (0.0f64, 0.0f64);
    for y in 0..ih {
        for x in 0..iw {
            if node_of(x, y).is_none() {
                continue;
            }
            let z = colour(x, y);
            if x + 1 < iw {
                let q = colour(x + 1, y);
                sum += (z[0] - q[0]).powi(2) + (z[1] - q[1]).powi(2) + (z[2] - q[2]).powi(2);
                count += 1.0;
            }
        }
    }
    let beta = if sum > 0.0 {
        count / (2.0 * sum)
    } else {
        models.beta
    };
    let (s, t) = (nodes as u32, nodes as u32 + 1);
    let mut g = grabcut::MaxFlow::new(nodes + 2, nodes * 5)?;
    let neighbours: [(i64, i64); 8] = [
        (1, 0),
        (-1, 0),
        (0, 1),
        (0, -1),
        (1, 1),
        (-1, -1),
        (1, -1),
        (-1, 1),
    ];
    for y in 0..ih {
        for x in 0..iw {
            let Some(p) = node_of(x, y) else { continue };
            let z = colour(x, y);
            let dbg = models.background.cost(z);
            let dfg = models.foreground.cost(z);
            let m = dbg.min(dfg);
            let (mut to_src, mut to_sink) = (dbg - m, dfg - m);
            for (dx, dy) in neighbours {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx as usize >= iw || ny as usize >= ih {
                    continue;
                }
                let (nx, ny) = (nx as usize, ny as usize);
                let wgt = grabcut::n_link(z, colour(nx, ny), beta, dx != 0 && dy != 0);
                match node_of(nx, ny) {
                    // Each band pair once (the positive half of the offsets).
                    Some(q) => {
                        if (dy, dx) > (0, 0) {
                            g.edge(p, q, wgt, wgt)?;
                        }
                    }
                    None => {
                        if fg[(ny / f) * gw + nx / f] {
                            to_src += wgt;
                        } else {
                            to_sink += wgt;
                        }
                    }
                }
            }
            if to_src > 0.0 {
                g.edge(s, p, to_src, 0.0)?;
            }
            if to_sink > 0.0 {
                g.edge(p, t, to_sink, 0.0)?;
            }
        }
    }
    progress(0.5);
    let side = g.min_cut(s, t)?;
    for y in 0..ih {
        let row = out.row_mut(y);
        for (x, v) in row.iter_mut().enumerate() {
            if let Some(p) = node_of(x, y) {
                *v = if side[p as usize] { 255 } else { 0 };
            }
        }
    }
    progress(1.0);
    out.into_mask()
}

/// Run GrabCut on `trimap`, keep the anchored foreground, and bring it to full
/// resolution.
fn cut(
    img: &ImageView,
    grid: &Grid,
    mut trimap: Vec<Label>,
    opts: &SubjectOptions,
    progress: &mut dyn FnMut(f32),
) -> Result<SubjectOutcome, SelectionOpError> {
    let n = grid.w * grid.h;
    let models = grabcut(
        &grid.rgb,
        grid.w,
        grid.h,
        &mut trimap,
        opts.rounds,
        &mut |p| progress(0.15 + 0.65 * p),
    )?;
    let Some(models) = models else {
        return Ok(SubjectOutcome::NothingFound(NOTHING_STANDS_OUT));
    };
    let fg = keep_anchored(&trimap, grid.w, grid.h)?;
    let area = fg.iter().filter(|&&v| v).count();
    if area == 0 || area * 100 > n * 97 {
        return Ok(SubjectOutcome::NothingFound(NOTHING_STANDS_OUT));
    }
    let mask = to_full_resolution(img, grid, &fg, &models, &mut |p| progress(0.8 + 0.2 * p))?;
    Ok(SubjectOutcome::Selected(mask))
}

/// Select ▸ Subject: select the region of `img` that stands out most from
/// its border, as described in the [module documentation](self).
///
/// `progress` hears fractions in `0..=1` as the stages finish.
pub fn select_subject(
    img: &ImageView,
    opts: &SubjectOptions,
    progress: &mut dyn FnMut(f32),
) -> Result<SubjectOutcome, SelectionOpError> {
    if img.width() < 3 || img.height() < 3 {
        return Ok(SubjectOutcome::NothingFound(NOTHING_STANDS_OUT));
    }
    let grid = build_grid(img, opts.working_side)?;
    progress(0.05);
    let Some(s) = saliency(&grid)? else {
        progress(1.0);
        return Ok(SubjectOutcome::NothingFound(NOTHING_STANDS_OUT));
    };
    progress(0.12);
    let Some(trimap) = subject_trimap(&s, grid.w, grid.h)? else {
        progress(1.0);
        return Ok(SubjectOutcome::NothingFound(NOTHING_STANDS_OUT));
    };
    cut(img, &grid, trimap, opts, progress)
}

/// Object Selection: GrabCut initialised from a rectangle. Everything
/// outside `bounds` is background; inside, the cut decides. A rectangle that
/// covers the whole image leaves nothing to learn the background from, so it
/// runs [`select_subject`] instead.
pub fn select_object(
    img: &ImageView,
    bounds: Rect,
    opts: &SubjectOptions,
    progress: &mut dyn FnMut(f32),
) -> Result<SubjectOutcome, SelectionOpError> {
    let inside = bounds.intersection(img.rect());
    if inside.is_empty() || inside.width() < 3 || inside.height() < 3 {
        return Ok(SubjectOutcome::NothingFound(
            "No object found: the rectangle is too small",
        ));
    }
    if inside == img.rect() {
        return select_subject(img, opts, progress);
    }
    let grid = build_grid(img, opts.working_side)?;
    let f = grid.factor as i32;
    let origin = img.rect().min();
    let mut trimap = alloc_vec(grid.w * grid.h, Label::Background)?;
    let mut any_inside = false;
    for cy in 0..grid.h {
        for cx in 0..grid.w {
            let centre = origin + IVec2::new(cx as i32 * f + f / 2, cy as i32 * f + f / 2);
            if inside.contains(centre) {
                trimap[cy * grid.w + cx] = Label::ProbablyForeground;
                any_inside = true;
            }
        }
    }
    if !any_inside {
        return Ok(SubjectOutcome::NothingFound(
            "No object found: the rectangle is too small",
        ));
    }
    progress(0.12);
    let models = grabcut(
        &grid.rgb,
        grid.w,
        grid.h,
        &mut trimap,
        opts.rounds,
        &mut |p| progress(0.15 + 0.65 * p),
    )?;
    let Some(models) = models else {
        return Ok(SubjectOutcome::NothingFound(NOTHING_STANDS_OUT));
    };
    let fg: Vec<bool> = trimap.iter().map(|l| l.is_foreground()).collect();
    if !fg.iter().any(|&v| v) {
        return Ok(SubjectOutcome::NothingFound(
            "No object found: nothing in the rectangle differs from its surroundings",
        ));
    }
    let mask = to_full_resolution(img, &grid, &fg, &models, &mut |p| progress(0.8 + 0.2 * p))?;
    Ok(SubjectOutcome::Selected(mask))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageBuffer;

    /// A cheap deterministic hash for texture.
    fn noise(x: u32, y: u32, salt: u32) -> u32 {
        let mut v = x
            .wrapping_mul(374_761_393)
            .wrapping_add(y.wrapping_mul(668_265_263))
            .wrapping_add(salt.wrapping_mul(2_246_822_519));
        v = (v ^ (v >> 13)).wrapping_mul(1_274_126_177);
        v ^ (v >> 16)
    }

    /// A textured orange disc on a textured blue-green background.
    fn disc_image(w: u32, h: u32, cx: f32, cy: f32, r: f32) -> (ImageBuffer, Vec<bool>) {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        let mut truth = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let inside = d < r;
                let n = (noise(x, y, 7) % 60) as i32 - 30;
                let stripe = if (x / 3 + y / 5) % 2 == 0 { 18 } else { -18 };
                let c = if inside {
                    [215 + n / 2 + stripe / 2, 120 + n + stripe, 40 + n / 2]
                } else {
                    [40 + n / 2, 110 + n + stripe, 150 + n / 2 - stripe]
                };
                for v in c {
                    px.push(v.clamp(0, 255) as u8);
                }
                px.push(255);
                truth.push(inside);
            }
        }
        (
            ImageBuffer::from_rgba8(IVec2::ZERO, w, h, px).unwrap(),
            truth,
        )
    }

    fn iou(mask: &SelectionMask, truth: &[bool], w: u32, h: u32) -> f64 {
        let sel = editor_core::Selection::Mask(mask.clone());
        let (mut inter, mut uni) = (0u64, 0u64);
        for y in 0..h {
            for x in 0..w {
                let a = sel.coverage_at(IVec2::new(x as i32, y as i32)) >= 0.5;
                let b = truth[(y * w + x) as usize];
                inter += u64::from(a && b);
                uni += u64::from(a || b);
            }
        }
        inter as f64 / uni.max(1) as f64
    }

    fn selected(o: SubjectOutcome) -> SelectionMask {
        match o {
            SubjectOutcome::Selected(m) => m,
            SubjectOutcome::NothingFound(why) => panic!("found nothing: {why}"),
        }
    }

    #[test]
    fn a_textured_disc_is_selected_with_iou_over_0_9() {
        let (img, truth) = disc_image(160, 120, 84.0, 58.0, 34.0);
        let m =
            selected(select_subject(&img.view(), &SubjectOptions::default(), &mut |_| {}).unwrap());
        let score = iou(&m, &truth, 160, 120);
        assert!(score >= 0.9, "IoU {score}");
    }

    /// The working grid is smaller than the image, so the full-resolution
    /// boundary refinement runs, and the result still hugs the disc.
    #[test]
    fn a_reduced_working_grid_is_refined_at_full_resolution() {
        let (img, truth) = disc_image(240, 200, 110.0, 104.0, 56.0);
        let opts = SubjectOptions {
            working_side: 80,
            ..SubjectOptions::default()
        };
        let m = selected(select_subject(&img.view(), &opts, &mut |_| {}).unwrap());
        let score = iou(&m, &truth, 240, 200);
        assert!(score >= 0.9, "IoU {score}");
    }

    /// The boundary is re-cut per PIXEL, not per working cell: with a coarse
    /// grid (8 px cells) an upsampled cut is uniform inside every cell and
    /// misses the disc's rim by up to half a cell all the way round. The
    /// refined mask must split cells along the rim and sit within a pixel or
    /// so of the true edge.
    #[test]
    fn the_boundary_is_recut_at_full_resolution_not_upsampled() {
        let (w, h) = (240u32, 200u32);
        let (img, truth) = disc_image(w, h, 118.0, 98.0, 60.0);
        let opts = SubjectOptions {
            working_side: 30,
            ..SubjectOptions::default()
        };
        let grid = build_grid(&img.view(), opts.working_side).unwrap();
        let f = grid.factor as u32;
        assert_eq!(f, 8, "an 8 px working cell");
        let m = selected(select_subject(&img.view(), &opts, &mut |_| {}).unwrap());
        let sel = editor_core::Selection::Mask(m);
        let on = |x: u32, y: u32| sel.coverage_at(IVec2::new(x as i32, y as i32)) >= 0.5;
        // Cells the mask splits: impossible for an upsampled cut.
        let mut split = 0u32;
        for cy in 0..h.div_ceil(f) {
            for cx in 0..w.div_ceil(f) {
                let first = on(cx * f, cy * f);
                let mixed = (cy * f..((cy + 1) * f).min(h))
                    .any(|y| (cx * f..((cx + 1) * f).min(w)).any(|x| on(x, y) != first));
                split += u32::from(mixed);
            }
        }
        let wrong = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| on(x, y) != truth[(y * w + x) as usize])
            .count();
        let perimeter = (2.0 * std::f64::consts::PI * 60.0) as usize;
        assert!(
            split >= 20,
            "only {split} working cells were re-cut per pixel"
        );
        assert!(
            wrong <= perimeter,
            "{wrong} pixels are on the wrong side of a rim {perimeter} px long"
        );
    }

    /// The seeds alone are NOT the answer: they leave the disc's rim
    /// undecided (seed IoU under 0.9), and it is the GrabCut graph cut that
    /// labels that band. Every undecided pixel must end on the right side, so
    /// a cut that labelled the whole band foreground (or background) fails.
    #[test]
    fn the_graph_cut_decides_the_band_the_seeds_leave_open() {
        let (w, h) = (160u32, 120u32);
        let (img, truth) = disc_image(w, h, 84.0, 58.0, 34.0);
        let view = img.view();
        let grid = build_grid(&view, SubjectOptions::default().working_side).unwrap();
        assert_eq!(grid.factor, 1, "the whole image is the working grid here");
        let s = saliency(&grid).unwrap().expect("the disc stands out");
        let tri = subject_trimap(&s, grid.w, grid.h).unwrap().expect("seeds");
        let (mut inter, mut uni, mut open) = (0u32, 0u32, 0u32);
        for (l, &t) in tri.iter().zip(&truth) {
            inter += u32::from(l.is_foreground() && t);
            uni += u32::from(l.is_foreground() || t);
            open += u32::from(matches!(
                l,
                Label::ProbablyBackground | Label::ProbablyForeground
            ));
        }
        let seed_iou = f64::from(inter) / f64::from(uni);
        assert!(seed_iou < 0.9, "the seeds alone already score {seed_iou}");
        assert!(open > 500, "only {open} pixels were left to the cut");
        let m = selected(select_subject(&view, &SubjectOptions::default(), &mut |_| {}).unwrap());
        let sel = editor_core::Selection::Mask(m);
        let mut wrong = 0u32;
        for (i, (l, &t)) in tri.iter().zip(&truth).enumerate() {
            let p = IVec2::new((i as u32 % w) as i32, (i as u32 / w) as i32);
            let a = sel.coverage_at(p) >= 0.5;
            if matches!(l, Label::ProbablyBackground | Label::ProbablyForeground) && a != t {
                wrong += 1;
            }
        }
        assert!(
            wrong * 100 <= open,
            "{wrong} of the {open} undecided pixels ended on the wrong side"
        );
    }

    #[test]
    fn a_flat_image_selects_nothing_and_says_so() {
        let img = ImageBuffer::from_rgba8(IVec2::ZERO, 64, 48, [90, 140, 200, 255].repeat(64 * 48))
            .unwrap();
        let out = select_subject(&img.view(), &SubjectOptions::default(), &mut |_| {}).unwrap();
        assert_eq!(out, SubjectOutcome::NothingFound(NOTHING_STANDS_OUT));
    }

    #[test]
    fn the_same_pixels_give_the_same_selection() {
        let (img, _) = disc_image(120, 100, 60.0, 50.0, 26.0);
        let a = select_subject(&img.view(), &SubjectOptions::default(), &mut |_| {}).unwrap();
        let b = select_subject(&img.view(), &SubjectOptions::default(), &mut |_| {}).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn progress_is_reported_and_ends_at_one() {
        let (img, _) = disc_image(100, 80, 50.0, 40.0, 20.0);
        let mut seen = Vec::new();
        select_subject(&img.view(), &SubjectOptions::default(), &mut |p| {
            seen.push(p)
        })
        .unwrap();
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        assert_eq!(seen.last().copied(), Some(1.0));
    }

    #[test]
    fn object_selection_from_a_rectangle_finds_the_disc() {
        // The rectangle hugs the disc; everything outside it is background.
        let (img, truth) = disc_image(200, 100, 150.0, 50.0, 30.0);
        let m = selected(
            select_object(
                &img.view(),
                Rect::from_xywh(110, 10, 80, 80),
                &SubjectOptions::default(),
                &mut |_| {},
            )
            .unwrap(),
        );
        assert!(iou(&m, &truth, 200, 100) >= 0.9);
    }
}
