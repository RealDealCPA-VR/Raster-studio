//! PatchMatch hole filling — the engine behind content-aware fill.
//!
//! Content-aware fill is not a learned model. It is PatchMatch (Barnes,
//! Shechtman, Finkelstein and Goldman, 2009) driving the multi-scale
//! expectation–maximisation synthesis of Wexler, Shechtman and Irani (2007):
//!
//! 1. **A pyramid.** The image and its hole are halved until the hole is
//!    about one patch across. A coarse pixel is part of the hole when any of
//!    its four children is, so the hole only ever grows on the way down, and
//!    the known pixels are averaged over their known children only.
//! 2. **An initial guess** at the coarsest level: the hole is peeled from its
//!    rim inward, each rim pixel taking the mean of its known neighbours.
//! 3. **Nearest-neighbour field.** Every [`PATCH_SIZE`]-square patch that
//!    touches the hole (a *target*) is matched to a patch lying wholly in the
//!    known region (a *source*) by PatchMatch: a guess, then alternating
//!    scan-order passes of *propagation* (a neighbour's match, shifted by one,
//!    is tried) and *random search* (candidates at exponentially shrinking
//!    radii around the current best).
//! 4. **Voting.** Every hole pixel becomes the weighted mean of the pixels
//!    the overlapping targets' matches put there, each vote weighted by
//!    `exp(-d / 2σ²)` of its patch distance, with `σ²` the 75th percentile of
//!    the distances (Wexler's choice). Steps 3–4 alternate a few times per
//!    level.
//! 5. **Up a level.** The filled coarse hole seeds the finer one, the coarse
//!    field is doubled to seed the finer field, and the loop repeats until
//!    the full-resolution level is synthesised.
//!
//! Known pixels are never written, at any level: the output equals the input
//! everywhere outside the hole, bit for bit.
//!
//! # Determinism
//!
//! The whole synthesis is sequential and draws from one [`Rng`] stream seeded
//! by [`InpaintParams::seed`], so the same image, hole and seed give the same
//! bytes on every run and every machine thread count.
//!
//! # Cost
//!
//! `O(targets × patch² × iterations)` per level. It is bounded, not cheap:
//! callers crop to the hole's neighbourhood first
//! ([`crate::content_aware::content_aware_fill`] does).

use crate::rng::Rng;

/// The side of a PatchMatch patch, in pixels. Seven is the size Barnes et al.
/// and most later work settle on: big enough to carry a stripe's phase or a
/// texture's grain, small enough to find matches in a modest context.
pub const PATCH_SIZE: usize = 7;

/// Half a patch: offsets run `-R..=R`.
const R: i64 = (PATCH_SIZE as i64) / 2;

/// The deepest pyramid this module builds.
const MAX_LEVELS: usize = 8;

/// How the synthesis runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InpaintParams {
    /// Seeds the one random stream the synthesis draws from.
    pub seed: u64,
    /// Expectation–maximisation rounds (match, then vote) at each level.
    pub em_iterations: u32,
    /// PatchMatch passes (propagation + random search) per round.
    pub search_iterations: u32,
}

impl Default for InpaintParams {
    fn default() -> Self {
        Self {
            seed: 0x5EED_CAFE,
            em_iterations: 4,
            search_iterations: 2,
        }
    }
}

/// Why a hole could not be filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InpaintError {
    /// `pixels` or `hole` does not hold `width × height` entries.
    BadLength,
    /// Nothing is marked as hole.
    NoHole,
    /// No patch lies wholly in the known region: there is nothing to copy
    /// from.
    NoSource,
}

/// One pyramid level: an image and its hole.
#[derive(Clone)]
struct Level {
    w: usize,
    h: usize,
    img: Vec<[f32; 4]>,
    hole: Vec<bool>,
}

impl Level {
    /// Half the size: a pixel is hole when any child is, and a known pixel is
    /// the mean of its known children.
    fn downsample(&self) -> Level {
        let (w2, h2) = (self.w.div_ceil(2), self.h.div_ceil(2));
        let mut img = vec![[0.0; 4]; w2 * h2];
        let mut hole = vec![false; w2 * h2];
        for y in 0..h2 {
            for x in 0..w2 {
                let mut acc = [0.0f32; 4];
                let mut n = 0.0f32;
                let mut any_hole = false;
                for (cx, cy) in [
                    (2 * x, 2 * y),
                    (2 * x + 1, 2 * y),
                    (2 * x, 2 * y + 1),
                    (2 * x + 1, 2 * y + 1),
                ] {
                    if cx >= self.w || cy >= self.h {
                        continue;
                    }
                    let i = cy * self.w + cx;
                    if self.hole[i] {
                        any_hole = true;
                    } else {
                        let p = self.img[i];
                        for c in 0..4 {
                            acc[c] += p[c];
                        }
                        n += 1.0;
                    }
                }
                let o = y * w2 + x;
                hole[o] = any_hole;
                if n > 0.0 {
                    img[o] = [acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n];
                }
            }
        }
        Level {
            w: w2,
            h: h2,
            img,
            hole,
        }
    }

    /// The hole's bounding box extent, `max(width, height)`, or 0.
    fn hole_extent(&self) -> usize {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.hole[y * self.w + x] {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        if x0 == usize::MAX {
            0
        } else {
            (x1 - x0 + 1).max(y1 - y0 + 1)
        }
    }

    /// Summed-area table of hole pixels, `(w + 1) × (h + 1)`.
    fn hole_sat(&self) -> Vec<u32> {
        let sw = self.w + 1;
        let mut sat = vec![0u32; sw * (self.h + 1)];
        for y in 0..self.h {
            let mut row = 0u32;
            for x in 0..self.w {
                row += u32::from(self.hole[y * self.w + x]);
                sat[(y + 1) * sw + x + 1] = sat[y * sw + x + 1] + row;
            }
        }
        sat
    }

    /// Hole pixels inside the patch centred at `(x, y)`, clipped to the image.
    fn holes_in_patch(&self, sat: &[u32], x: usize, y: usize) -> u32 {
        let sw = self.w + 1;
        let x0 = x.saturating_sub(R as usize);
        let y0 = y.saturating_sub(R as usize);
        let x1 = (x + R as usize + 1).min(self.w);
        let y1 = (y + R as usize + 1).min(self.h);
        sat[y1 * sw + x1] + sat[y0 * sw + x0] - sat[y0 * sw + x1] - sat[y1 * sw + x0]
    }

    /// Whether the patch centred at each pixel is a usable source: wholly
    /// inside the image and wholly known.
    fn sources(&self) -> Vec<bool> {
        let sat = self.hole_sat();
        let mut ok = vec![false; self.w * self.h];
        let r = R as usize;
        if self.w < PATCH_SIZE || self.h < PATCH_SIZE {
            return ok;
        }
        for y in r..self.h - r {
            for x in r..self.w - r {
                ok[y * self.w + x] = self.holes_in_patch(&sat, x, y) == 0;
            }
        }
        ok
    }

    /// Fill the hole by peeling it from the rim inward: each rim pixel takes
    /// the mean of its already-known 8-neighbours.
    fn onion_fill(&mut self) {
        let mut known: Vec<bool> = self.hole.iter().map(|h| !h).collect();
        loop {
            let mut rim = Vec::new();
            for y in 0..self.h {
                for x in 0..self.w {
                    let i = y * self.w + x;
                    if known[i] {
                        continue;
                    }
                    let mut acc = [0.0f32; 4];
                    let mut n = 0.0f32;
                    for dy in -1i64..=1 {
                        for dx in -1i64..=1 {
                            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                            if nx < 0 || ny < 0 || nx >= self.w as i64 || ny >= self.h as i64 {
                                continue;
                            }
                            let j = ny as usize * self.w + nx as usize;
                            if known[j] {
                                let p = self.img[j];
                                for c in 0..4 {
                                    acc[c] += p[c];
                                }
                                n += 1.0;
                            }
                        }
                    }
                    if n > 0.0 {
                        rim.push((i, [acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n]));
                    }
                }
            }
            if rim.is_empty() {
                break;
            }
            for (i, p) in rim {
                self.img[i] = p;
                known[i] = true;
            }
        }
    }
}

/// The nearest-neighbour field of one level and the machinery to improve it.
struct Field<'a> {
    level: &'a Level,
    /// Target centres, in scan order.
    targets: Vec<usize>,
    /// `true` at a target centre.
    is_target: Vec<bool>,
    /// `true` at a usable source centre.
    source_ok: &'a [bool],
    /// Per pixel: the source centre a target matches (meaningful at targets).
    nnf: Vec<usize>,
    /// Per pixel: the sum-of-squares distance of that match.
    cost: Vec<f32>,
}

impl Field<'_> {
    /// Sum of squared differences between the patch at target `q` and source
    /// `s`, over the target's in-image offsets; gives up past `limit`.
    fn distance(&self, q: usize, s: usize, limit: f32) -> f32 {
        let l = self.level;
        let (qx, qy) = ((q % l.w) as i64, (q / l.w) as i64);
        let (sx, sy) = ((s % l.w) as i64, (s / l.w) as i64);
        let mut d = 0.0f32;
        for dy in -R..=R {
            let ty = qy + dy;
            if ty < 0 || ty >= l.h as i64 {
                continue;
            }
            let trow = ty as usize * l.w;
            let srow = (sy + dy) as usize * l.w;
            for dx in -R..=R {
                let tx = qx + dx;
                if tx < 0 || tx >= l.w as i64 {
                    continue;
                }
                let a = l.img[trow + tx as usize];
                let b = l.img[srow + (sx + dx) as usize];
                let e = [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]];
                d += e[0] * e[0] + e[1] * e[1] + e[2] * e[2] + e[3] * e[3];
            }
            if d > limit {
                return d;
            }
        }
        d
    }

    /// Recompute every target's cost against the level's current image.
    fn rescore(&mut self) {
        for k in 0..self.targets.len() {
            let q = self.targets[k];
            self.cost[q] = self.distance(q, self.nnf[q], f32::INFINITY);
        }
    }

    /// Try `s` as target `q`'s match; keep it when it is better.
    fn try_candidate(&mut self, q: usize, s: usize) {
        if !self.source_ok[s] || s == self.nnf[q] {
            return;
        }
        let d = self.distance(q, s, self.cost[q]);
        if d < self.cost[q] {
            self.cost[q] = d;
            self.nnf[q] = s;
        }
    }

    /// One PatchMatch pass: propagation from the already-visited neighbours
    /// in scan order (reversed on odd passes), then random search.
    fn pass(&mut self, rng: &mut Rng, reverse: bool) {
        let (w, h) = (self.level.w as i64, self.level.h as i64);
        let n = self.targets.len();
        for k in 0..n {
            let q = if reverse {
                self.targets[n - 1 - k]
            } else {
                self.targets[k]
            };
            let (qx, qy) = ((q as i64) % w, (q as i64) / w);
            let step: i64 = if reverse { 1 } else { -1 };
            // Propagation: the neighbour one step back along x, then along y,
            // shifted forward by that same step.
            for (nx, ny) in [(qx + step, qy), (qx, qy + step)] {
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let nb = (ny * w + nx) as usize;
                if !self.is_target[nb] {
                    continue;
                }
                let m = self.nnf[nb] as i64;
                let (mx, my) = (m % w - (nx - qx), m / w - (ny - qy));
                if mx < 0 || my < 0 || mx >= w || my >= h {
                    continue;
                }
                self.try_candidate(q, (my * w + mx) as usize);
            }
            // Random search at halving radii around the current best.
            let mut radius = w.max(h) as f32;
            while radius >= 1.0 {
                let b = self.nnf[q] as i64;
                let (bx, by) = (b % w, b / w);
                let cx = (bx as f32 + rng.next_signed() * radius).round() as i64;
                let cy = (by as f32 + rng.next_signed() * radius).round() as i64;
                let cx = cx.clamp(R, w - 1 - R);
                let cy = cy.clamp(R, h - 1 - R);
                self.try_candidate(q, (cy * w + cx) as usize);
                radius *= 0.5;
            }
        }
    }

    /// The patch-distance count behind a target's cost, for averaging.
    fn compared(&self, q: usize) -> f32 {
        let l = self.level;
        let (qx, qy) = ((q % l.w) as i64, (q / l.w) as i64);
        let span = |c: i64, n: usize| ((c + R).min(n as i64 - 1) - (c - R).max(0) + 1) as f32;
        span(qx, l.w) * span(qy, l.h)
    }

    /// The voting step: every hole pixel becomes the similarity-weighted mean
    /// of what the overlapping targets' matches put there.
    fn vote(&self) -> Vec<[f32; 4]> {
        let l = self.level;
        let mut means: Vec<f32> = self
            .targets
            .iter()
            .map(|&q| self.cost[q] / self.compared(q).max(1.0))
            .collect();
        let sigma2 = if means.is_empty() {
            1.0
        } else {
            let k = (means.len() * 3 / 4).min(means.len() - 1);
            let (_, v, _) = means.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
            v.max(1e-6)
        };
        let mut acc = vec![[0.0f32; 4]; l.w * l.h];
        let mut wsum = vec![0.0f32; l.w * l.h];
        for &q in &self.targets {
            let mean = self.cost[q] / self.compared(q).max(1.0);
            let wgt = (-mean / (2.0 * sigma2)).exp().max(1e-12);
            let s = self.nnf[q];
            let (qx, qy) = ((q % l.w) as i64, (q / l.w) as i64);
            let (sx, sy) = ((s % l.w) as i64, (s / l.w) as i64);
            for dy in -R..=R {
                let ty = qy + dy;
                if ty < 0 || ty >= l.h as i64 {
                    continue;
                }
                for dx in -R..=R {
                    let tx = qx + dx;
                    if tx < 0 || tx >= l.w as i64 {
                        continue;
                    }
                    let t = ty as usize * l.w + tx as usize;
                    if !l.hole[t] {
                        continue;
                    }
                    let p = l.img[(sy + dy) as usize * l.w + (sx + dx) as usize];
                    for c in 0..4 {
                        acc[t][c] += wgt * p[c];
                    }
                    wsum[t] += wgt;
                }
            }
        }
        let mut out = l.img.clone();
        for i in 0..out.len() {
            if l.hole[i] && wsum[i] > 0.0 {
                let k = 1.0 / wsum[i];
                out[i] = [acc[i][0] * k, acc[i][1] * k, acc[i][2] * k, acc[i][3] * k];
            }
        }
        out
    }
}

/// Fill the pixels `hole` marks in a `width × height` image, by multi-scale
/// PatchMatch synthesis from the rest of the image. See the module docs.
///
/// Pixels outside the hole are returned unchanged. The channels are treated
/// as four independent numbers — pass premultiplied values so a transparent
/// region is copied as transparency, not as its hidden colour.
pub fn inpaint(
    width: usize,
    height: usize,
    pixels: &[[f32; 4]],
    hole: &[bool],
    params: InpaintParams,
) -> Result<Vec<[f32; 4]>, InpaintError> {
    let n = width.checked_mul(height).ok_or(InpaintError::BadLength)?;
    if pixels.len() != n || hole.len() != n {
        return Err(InpaintError::BadLength);
    }
    if !hole.iter().any(|&h| h) {
        return Err(InpaintError::NoHole);
    }
    let base = Level {
        w: width,
        h: height,
        img: pixels.to_vec(),
        hole: hole.to_vec(),
    };
    let base_sources = base.sources();
    if !base_sources.iter().any(|&s| s) {
        return Err(InpaintError::NoSource);
    }

    // The pyramid, finest first. A level is only added while the hole is
    // still wider than a patch and the coarser level still has sources.
    let mut levels = vec![base];
    let mut sources = vec![base_sources];
    while levels.len() < MAX_LEVELS {
        let top = levels.last().expect("the pyramid starts non-empty");
        if top.hole_extent() <= PATCH_SIZE {
            break;
        }
        let next = top.downsample();
        if next.w < PATCH_SIZE * 2 || next.h < PATCH_SIZE * 2 {
            break;
        }
        let ok = next.sources();
        if !ok.iter().any(|&s| s) {
            break;
        }
        levels.push(next);
        sources.push(ok);
    }

    let mut rng = Rng::new(params.seed);
    let coarsest = levels.len() - 1;
    levels[coarsest].onion_fill();
    let mut prev_nnf: Option<(usize, Vec<usize>)> = None;

    for li in (0..levels.len()).rev() {
        if let Some(coarse) = li.checked_add(1).filter(|&c| c < levels.len()) {
            // Seed this level's hole from the synthesised coarser one.
            let (cw, ch) = (levels[coarse].w, levels[coarse].h);
            let coarse_img = levels[coarse].img.clone();
            let lv = &mut levels[li];
            for y in 0..lv.h {
                for x in 0..lv.w {
                    let i = y * lv.w + x;
                    if lv.hole[i] {
                        lv.img[i] = coarse_img[(y / 2).min(ch - 1) * cw + (x / 2).min(cw - 1)];
                    }
                }
            }
        }
        let level = &levels[li];
        let source_ok = &sources[li];
        let source_list: Vec<usize> = (0..source_ok.len()).filter(|&i| source_ok[i]).collect();
        let sat = level.hole_sat();
        let mut is_target = vec![false; level.w * level.h];
        let mut targets = Vec::new();
        for y in 0..level.h {
            for x in 0..level.w {
                if level.holes_in_patch(&sat, x, y) > 0 {
                    is_target[y * level.w + x] = true;
                    targets.push(y * level.w + x);
                }
            }
        }
        // The initial field: the coarser field doubled where it lands on a
        // source, a random source everywhere else.
        let mut nnf = vec![0usize; level.w * level.h];
        for &q in &targets {
            let (qx, qy) = (q % level.w, q / level.w);
            let mut pick = None;
            if let Some((cw, cnnf)) = &prev_nnf {
                let cq = (qy / 2) * cw + (qx / 2);
                if let Some(&cs) = cnnf.get(cq) {
                    let (sx, sy) = (2 * (cs % cw) + qx % 2, 2 * (cs / cw) + qy % 2);
                    if sx < level.w && sy < level.h && source_ok[sy * level.w + sx] {
                        pick = Some(sy * level.w + sx);
                    }
                }
            }
            nnf[q] = pick.unwrap_or_else(|| {
                source_list[(rng.next_u64() % source_list.len() as u64) as usize]
            });
        }
        let em = if li == coarsest {
            params.em_iterations.max(1) * 2
        } else {
            params.em_iterations.max(1)
        };
        let mut img = level.img.clone();
        let mut final_nnf = nnf;
        for _ in 0..em {
            let lv = Level {
                w: level.w,
                h: level.h,
                img: img.clone(),
                hole: level.hole.clone(),
            };
            let mut field = Field {
                level: &lv,
                targets: targets.clone(),
                is_target: is_target.clone(),
                source_ok,
                nnf: final_nnf,
                cost: vec![f32::INFINITY; level.w * level.h],
            };
            field.rescore();
            for pass in 0..params.search_iterations.max(1) {
                field.pass(&mut rng, pass % 2 == 1);
            }
            img = field.vote();
            final_nnf = field.nnf;
        }
        levels[li].img = img;
        prev_nnf = Some((levels[li].w, final_nnf));
    }

    let mut out = levels.swap_remove(0).img;
    // Belt and braces: known pixels are the input's, exactly.
    for i in 0..n {
        if !hole[i] {
            out[i] = pixels[i];
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hole_with_no_source_patch_is_refused() {
        let px = vec![[0.5; 4]; 5 * 5];
        let hole = vec![true; 25];
        assert_eq!(
            inpaint(5, 5, &px, &hole, InpaintParams::default()),
            Err(InpaintError::NoSource)
        );
    }

    #[test]
    fn an_empty_hole_is_refused() {
        let px = vec![[0.5; 4]; 16 * 16];
        let hole = vec![false; 256];
        assert_eq!(
            inpaint(16, 16, &px, &hole, InpaintParams::default()),
            Err(InpaintError::NoHole)
        );
    }

    #[test]
    fn mismatched_lengths_are_refused() {
        let px = vec![[0.5; 4]; 10];
        let hole = vec![true; 11];
        assert_eq!(
            inpaint(2, 5, &px, &hole, InpaintParams::default()),
            Err(InpaintError::BadLength)
        );
    }
}
