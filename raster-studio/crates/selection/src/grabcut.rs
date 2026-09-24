//! GrabCut: interactive foreground extraction by iterated graph cuts
//! (Rother, Kolmogorov & Blake, SIGGRAPH 2004), with no learned model.
//!
//! The pieces, all classical and all deterministic:
//!
//! * **Colour models.** Foreground and background are each a Gaussian mixture
//!   of [`GMM_COMPONENTS`] full-covariance components in RGB. The mixtures are
//!   seeded by Orchard–Bouman binary splitting (repeatedly split the cluster
//!   with the largest variance along its principal axis), not by a random
//!   k-means start, so the same pixels always give the same models.
//! * **The energy.** Each pixel pays `-ln p(z | model)` for the label it takes
//!   (the data term) and every 8-neighbour pair with different labels pays
//!   `gamma * exp(-beta * |z_m - z_n|^2) / dist` (the contrast-sensitive
//!   smoothness term), with `beta` set from the image's own mean neighbour
//!   contrast exactly as the paper does.
//! * **The minimisation.** A minimum s-t cut of the pixel graph, found by
//!   Dinic's max-flow ([`MaxFlow`]); the blocking-flow search is iterative, so
//!   a path as long as the graph cannot overflow the stack.
//! * **The loop.** Assign each pixel to its most likely component, re-learn
//!   both mixtures, re-cut; stop after the requested number of rounds or as
//!   soon as a round changes no label.
//!
//! What it is not: a semantic segmenter. GrabCut separates regions whose
//! *colour statistics* differ; a subject that shares its palette with the
//! background (a white cat on snow) will not separate cleanly, whatever the
//! seeds. [`crate::subject`] documents how the seeds are chosen and what that
//! means for Select Subject.

use crate::buf::{alloc_vec, try_push};
use crate::error::SelectionOpError;

/// Components per colour mixture (the paper's `K = 5`).
pub const GMM_COMPONENTS: usize = 5;

/// The smoothness weight (the paper's `gamma = 50`).
pub const GAMMA: f64 = 50.0;

/// Capacity standing in for "infinite" on a hard-constrained t-link. Larger
/// than any sum of n-links a pixel can have (8 * [`GAMMA`]) plus any data
/// term, so a hard label is never cut.
const HARD: f64 = 1.0e9;

/// What a pixel of the trimap is known to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// Definitely background: never becomes foreground.
    Background,
    /// Definitely foreground: never becomes background.
    Foreground,
    /// Starts as background, the cut decides.
    ProbablyBackground,
    /// Starts as foreground, the cut decides.
    ProbablyForeground,
}

impl Label {
    /// Whether the pixel currently counts as foreground.
    pub fn is_foreground(self) -> bool {
        matches!(self, Label::Foreground | Label::ProbablyForeground)
    }
}

// ------------------------------------------------------------------ GMM

#[derive(Debug, Clone, Copy)]
struct Gaussian {
    mean: [f64; 3],
    inv: [[f64; 3]; 3],
    /// `weight / sqrt((2 pi)^3 det)`, the component's normalisation.
    coef: f64,
}

/// A Gaussian mixture over RGB.
#[derive(Debug, Clone)]
pub struct Gmm {
    parts: Vec<Gaussian>,
}

/// Accumulated first and second moments of one cluster.
#[derive(Debug, Clone, Copy, Default)]
struct Moments {
    n: f64,
    sum: [f64; 3],
    prod: [[f64; 3]; 3],
}

impl Moments {
    fn add(&mut self, z: [f64; 3]) {
        self.n += 1.0;
        for i in 0..3 {
            self.sum[i] += z[i];
            for j in 0..3 {
                self.prod[i][j] += z[i] * z[j];
            }
        }
    }

    fn mean(&self) -> [f64; 3] {
        let n = self.n.max(1.0);
        [self.sum[0] / n, self.sum[1] / n, self.sum[2] / n]
    }

    fn cov(&self) -> [[f64; 3]; 3] {
        let n = self.n.max(1.0);
        let m = self.mean();
        let mut c = [[0.0; 3]; 3];
        for (i, row) in c.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = self.prod[i][j] / n - m[i] * m[j];
            }
        }
        // A ridge keeps a flat-coloured cluster invertible.
        for (i, row) in c.iter_mut().enumerate() {
            row[i] += 0.01;
        }
        c
    }
}

fn det3(m: &[[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn inv3(m: &[[f64; 3]; 3], det: f64) -> [[f64; 3]; 3] {
    let d = 1.0 / det;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ]
}

/// The principal eigenvector of a symmetric 3x3 matrix, by power iteration
/// from a fixed start (deterministic).
fn principal_axis(c: &[[f64; 3]; 3]) -> ([f64; 3], f64) {
    let mut v = [1.0, 0.8, 0.6];
    let mut lambda = 0.0;
    for _ in 0..32 {
        let w = [
            c[0][0] * v[0] + c[0][1] * v[1] + c[0][2] * v[2],
            c[1][0] * v[0] + c[1][1] * v[1] + c[1][2] * v[2],
            c[2][0] * v[0] + c[2][1] * v[1] + c[2][2] * v[2],
        ];
        let norm = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt();
        if norm <= f64::EPSILON {
            return (v, 0.0);
        }
        lambda = norm;
        v = [w[0] / norm, w[1] / norm, w[2] / norm];
    }
    (v, lambda)
}

impl Gmm {
    /// Learn a mixture from `samples`, each already assigned to a component in
    /// `0..GMM_COMPONENTS`. Empty components are dropped.
    fn learn(samples: &[[f64; 3]], assign: &[usize]) -> Gmm {
        let mut m = [Moments::default(); GMM_COMPONENTS];
        for (z, &k) in samples.iter().zip(assign) {
            m[k].add(*z);
        }
        let total: f64 = m.iter().map(|c| c.n).sum::<f64>().max(1.0);
        let mut parts = Vec::with_capacity(GMM_COMPONENTS);
        for c in m.iter().filter(|c| c.n > 0.0) {
            let cov = c.cov();
            let det = det3(&cov).max(1.0e-12);
            let weight = c.n / total;
            parts.push(Gaussian {
                mean: c.mean(),
                inv: inv3(&cov, det),
                coef: weight / ((2.0 * std::f64::consts::PI).powi(3) * det).sqrt(),
            });
        }
        Gmm { parts }
    }

    /// Orchard–Bouman initial assignment: start with one cluster and split
    /// the one with the largest principal variance until there are
    /// [`GMM_COMPONENTS`] (or no cluster can be split).
    fn initial_assignment(samples: &[[f64; 3]]) -> Result<Vec<usize>, SelectionOpError> {
        let mut assign = alloc_vec(samples.len(), 0usize)?;
        let mut clusters = 1;
        while clusters < GMM_COMPONENTS {
            let mut m = [Moments::default(); GMM_COMPONENTS];
            for (z, &k) in samples.iter().zip(&assign) {
                m[k].add(*z);
            }
            let mut best: Option<(usize, [f64; 3], f64)> = None;
            for (k, mk) in m.iter().enumerate().take(clusters) {
                if mk.n < 2.0 {
                    continue;
                }
                let (axis, lambda) = principal_axis(&mk.cov());
                if lambda > 0.02 && best.is_none_or(|(_, _, l)| lambda > l) {
                    best = Some((k, axis, lambda));
                }
            }
            let Some((k, axis, _)) = best else { break };
            let mean = m[k].mean();
            let split = axis[0] * mean[0] + axis[1] * mean[1] + axis[2] * mean[2];
            for (z, a) in samples.iter().zip(assign.iter_mut()) {
                if *a == k && axis[0] * z[0] + axis[1] * z[1] + axis[2] * z[2] > split {
                    *a = clusters;
                }
            }
            clusters += 1;
        }
        Ok(assign)
    }

    fn component_likelihood(g: &Gaussian, z: [f64; 3]) -> f64 {
        let d = [z[0] - g.mean[0], z[1] - g.mean[1], z[2] - g.mean[2]];
        let mut q = 0.0;
        for i in 0..3 {
            for j in 0..3 {
                q += d[i] * g.inv[i][j] * d[j];
            }
        }
        g.coef * (-0.5 * q).exp()
    }

    /// The most likely component for `z`.
    fn best_component(&self, z: [f64; 3]) -> usize {
        let mut best = (0, f64::NEG_INFINITY);
        for (k, g) in self.parts.iter().enumerate() {
            let p = Self::component_likelihood(g, z);
            if p > best.1 {
                best = (k, p);
            }
        }
        best.0
    }

    /// `-ln p(z)`, clamped so an impossible colour costs a lot, not infinity.
    pub fn cost(&self, z: [f64; 3]) -> f64 {
        let p: f64 = self
            .parts
            .iter()
            .map(|g| Self::component_likelihood(g, z))
            .sum();
        -(p.max(1.0e-30)).ln()
    }

    /// Fit a mixture to `samples` from scratch: an Orchard-Bouman split, then
    /// two rounds of assign / re-learn (each GrabCut round refits again).
    pub fn fit(samples: &[[f64; 3]]) -> Result<Gmm, SelectionOpError> {
        let mut assign = Self::initial_assignment(samples)?;
        let mut gmm = Self::learn(samples, &assign);
        for _ in 0..2 {
            for (z, a) in samples.iter().zip(assign.iter_mut()) {
                *a = gmm.best_component(*z);
            }
            gmm = Self::learn(samples, &assign);
        }
        Ok(gmm)
    }

    /// Whether the mixture has at least one component.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

// ------------------------------------------------------------- max-flow

/// A directed graph with residual capacities, cut by Dinic's algorithm.
///
/// Edges come in pairs `(e, e ^ 1)`; an undirected n-link is one pair whose
/// both arcs carry the weight, the standard trick that halves the arcs.
#[derive(Debug, Default)]
pub struct MaxFlow {
    head: Vec<u32>,
    next: Vec<u32>,
    to: Vec<u32>,
    cap: Vec<f64>,
}

const NONE: u32 = u32::MAX;
const EPS: f64 = 1.0e-9;

impl MaxFlow {
    /// A graph of `nodes` nodes and no edges.
    pub fn new(nodes: usize, edge_hint: usize) -> Result<Self, SelectionOpError> {
        let mut g = MaxFlow {
            head: alloc_vec(nodes, NONE)?,
            ..Default::default()
        };
        let arcs = edge_hint.saturating_mul(2);
        for v in [&mut g.next, &mut g.to] {
            v.try_reserve(arcs)
                .map_err(|_| SelectionOpError::OutOfMemory { bytes: arcs * 4 })?;
        }
        g.cap
            .try_reserve(arcs)
            .map_err(|_| SelectionOpError::OutOfMemory { bytes: arcs * 8 })?;
        Ok(g)
    }

    fn arc(&mut self, u: u32, v: u32, c: f64) -> Result<(), SelectionOpError> {
        try_push(&mut self.to, v)?;
        try_push(&mut self.cap, c)?;
        try_push(&mut self.next, self.head[u as usize])?;
        self.head[u as usize] = (self.to.len() - 1) as u32;
        Ok(())
    }

    /// Add `u -> v` with capacity `c` and `v -> u` with capacity `back`.
    pub fn edge(&mut self, u: u32, v: u32, c: f64, back: f64) -> Result<(), SelectionOpError> {
        self.arc(u, v, c.max(0.0))?;
        self.arc(v, u, back.max(0.0))
    }

    /// Push the maximum flow from `s` to `t`, then report which nodes stay
    /// reachable from `s` in the residual graph: the source side of a
    /// minimum cut.
    pub fn min_cut(&mut self, s: u32, t: u32) -> Result<Vec<bool>, SelectionOpError> {
        let n = self.head.len();
        let mut level = alloc_vec(n, -1i32)?;
        let mut iter = alloc_vec(n, NONE)?;
        let mut queue: Vec<u32> = alloc_vec(n, 0u32)?;
        let mut path: Vec<u32> = Vec::new();
        loop {
            // BFS levels.
            level.iter_mut().for_each(|l| *l = -1);
            level[s as usize] = 0;
            let (mut qh, mut qt) = (0usize, 0usize);
            queue[qt] = s;
            qt += 1;
            while qh < qt {
                let u = queue[qh];
                qh += 1;
                let mut e = self.head[u as usize];
                while e != NONE {
                    let v = self.to[e as usize];
                    if self.cap[e as usize] > EPS && level[v as usize] < 0 {
                        level[v as usize] = level[u as usize] + 1;
                        queue[qt] = v;
                        qt += 1;
                    }
                    e = self.next[e as usize];
                }
            }
            if level[t as usize] < 0 {
                break;
            }
            iter.copy_from_slice(&self.head);
            // Iterative blocking flow.
            path.clear();
            let mut u = s;
            loop {
                if u == t {
                    let mut f = f64::INFINITY;
                    for &e in &path {
                        f = f.min(self.cap[e as usize]);
                    }
                    let mut cut_at = path.len();
                    for (i, &e) in path.iter().enumerate() {
                        self.cap[e as usize] -= f;
                        self.cap[(e ^ 1) as usize] += f;
                        if cut_at == path.len() && self.cap[e as usize] <= EPS {
                            cut_at = i;
                        }
                    }
                    path.truncate(cut_at);
                    u = match path.last() {
                        Some(&e) => self.to[e as usize],
                        None => s,
                    };
                    continue;
                }
                let mut advanced = false;
                while iter[u as usize] != NONE {
                    let e = iter[u as usize];
                    let v = self.to[e as usize];
                    if self.cap[e as usize] > EPS && level[v as usize] == level[u as usize] + 1 {
                        try_push(&mut path, e)?;
                        u = v;
                        advanced = true;
                        break;
                    }
                    iter[u as usize] = self.next[e as usize];
                }
                if advanced {
                    continue;
                }
                if u == s {
                    break;
                }
                // Dead end: retire the node and back up one arc.
                level[u as usize] = -1;
                let e = path.pop().unwrap_or(NONE);
                if e == NONE {
                    break;
                }
                u = self.to[(e ^ 1) as usize];
                iter[u as usize] = self.next[iter[u as usize] as usize];
            }
        }
        // Residual reachability from the source.
        let mut seen = alloc_vec(n, false)?;
        seen[s as usize] = true;
        let mut stack = vec![s];
        while let Some(u) = stack.pop() {
            let mut e = self.head[u as usize];
            while e != NONE {
                let v = self.to[e as usize];
                if self.cap[e as usize] > EPS && !seen[v as usize] {
                    seen[v as usize] = true;
                    try_push(&mut stack, v)?;
                }
                e = self.next[e as usize];
            }
        }
        Ok(seen)
    }
}

// -------------------------------------------------------------- GrabCut

/// The 8-neighbourhood offsets GrabCut links, each undirected pair once.
const HALF_NEIGHBOURS: [(i32, i32); 4] = [(1, 0), (0, 1), (1, 1), (-1, 1)];

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// The paper's `beta = 1 / (2 <|z_m - z_n|^2>)` over every linked pair.
pub fn contrast_beta(pixels: &[[f64; 3]], w: usize, h: usize) -> f64 {
    let (mut sum, mut count) = (0.0, 0.0);
    for y in 0..h {
        for x in 0..w {
            for (dx, dy) in HALF_NEIGHBOURS {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                sum += dist2(pixels[y * w + x], pixels[ny as usize * w + nx as usize]);
                count += 1.0;
            }
        }
    }
    if sum <= f64::EPSILON {
        0.0
    } else {
        count / (2.0 * sum)
    }
}

/// The n-link weight between two pixels `step` apart (1 or sqrt 2).
pub fn n_link(a: [f64; 3], b: [f64; 3], beta: f64, diagonal: bool) -> f64 {
    let d = if diagonal {
        std::f64::consts::SQRT_2
    } else {
        1.0
    };
    GAMMA * (-beta * dist2(a, b)).exp() / d
}

/// The two colour models a run ended with, for refining at another
/// resolution.
#[derive(Debug, Clone)]
pub struct Models {
    pub foreground: Gmm,
    pub background: Gmm,
    pub beta: f64,
}

/// Run GrabCut over a `w * h` grid of RGB pixels (0..=255 per channel),
/// updating the probable labels of `trimap` in place.
///
/// `progress` hears a fraction in `0..=1` after each round. Returns the models
/// the last round used, or `None` when one side has no pixels to learn from
/// (every pixel is foreground, or every pixel is background) — there is then
/// nothing to cut.
pub fn grabcut(
    pixels: &[[f64; 3]],
    w: usize,
    h: usize,
    trimap: &mut [Label],
    rounds: u32,
    progress: &mut dyn FnMut(f32),
) -> Result<Option<Models>, SelectionOpError> {
    let n = w * h;
    if pixels.len() != n || trimap.len() != n || n == 0 {
        return Ok(None);
    }
    let beta = contrast_beta(pixels, w, h);
    let rounds = rounds.max(1);
    let mut models = None;
    for round in 0..rounds {
        let mut fg: Vec<[f64; 3]> = Vec::new();
        let mut bg: Vec<[f64; 3]> = Vec::new();
        for (z, l) in pixels.iter().zip(trimap.iter()) {
            if l.is_foreground() {
                try_push(&mut fg, *z)?;
            } else {
                try_push(&mut bg, *z)?;
            }
        }
        if fg.is_empty() || bg.is_empty() {
            return Ok(models);
        }
        let fgm = Gmm::fit(&fg)?;
        let bgm = Gmm::fit(&bg)?;
        drop((fg, bg));

        let (s, t) = (n as u32, n as u32 + 1);
        let mut g = MaxFlow::new(n + 2, n * 5)?;
        for i in 0..n {
            let (to_src, to_sink) = match trimap[i] {
                Label::Background => (0.0, HARD),
                Label::Foreground => (HARD, 0.0),
                _ => {
                    // Cutting s->p labels p background, so it costs the
                    // background data term; p->t costs the foreground one.
                    let dbg = bgm.cost(pixels[i]);
                    let dfg = fgm.cost(pixels[i]);
                    let m = dbg.min(dfg);
                    (dbg - m, dfg - m)
                }
            };
            if to_src > 0.0 {
                g.edge(s, i as u32, to_src, 0.0)?;
            }
            if to_sink > 0.0 {
                g.edge(i as u32, t, to_sink, 0.0)?;
            }
        }
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                for (dx, dy) in HALF_NEIGHBOURS {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    let wgt = n_link(pixels[i], pixels[j], beta, dx != 0 && dy != 0);
                    g.edge(i as u32, j as u32, wgt, wgt)?;
                }
            }
        }
        let source_side = g.min_cut(s, t)?;
        let mut changed = false;
        for (l, &fgside) in trimap.iter_mut().zip(&source_side) {
            let next = match *l {
                Label::ProbablyBackground | Label::ProbablyForeground => {
                    if fgside {
                        Label::ProbablyForeground
                    } else {
                        Label::ProbablyBackground
                    }
                }
                hard => hard,
            };
            changed |= next != *l;
            *l = next;
        }
        models = Some(Models {
            foreground: fgm,
            background: bgm,
            beta,
        });
        progress((round + 1) as f32 / rounds as f32);
        if !changed && round > 0 {
            break;
        }
    }
    progress(1.0);
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A four-node graph whose max flow is known by hand.
    #[test]
    fn dinic_finds_the_textbook_min_cut() {
        // s=0, a=1, b=2, t=3; s->a 3, s->b 2, a->b 1, a->t 2, b->t 3.
        let mut g = MaxFlow::new(4, 5).unwrap();
        g.edge(0, 1, 3.0, 0.0).unwrap();
        g.edge(0, 2, 2.0, 0.0).unwrap();
        g.edge(1, 2, 1.0, 0.0).unwrap();
        g.edge(1, 3, 2.0, 0.0).unwrap();
        g.edge(2, 3, 3.0, 0.0).unwrap();
        let side = g.min_cut(0, 3).unwrap();
        // Max flow 5 saturates s->a->t / s->a->b->t / s->b->t: only s stays.
        assert_eq!(side, vec![true, false, false, false]);
    }

    /// A two-colour image splits exactly along its colour edge.
    #[test]
    fn two_flat_halves_split_on_the_edge() {
        let (w, h) = (24usize, 16usize);
        let mut px = Vec::new();
        let mut tri = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                px.push(if x < 12 {
                    [200.0, 40.0, 40.0]
                } else {
                    [30.0, 60.0, 190.0]
                });
                tri.push(if x == 0 {
                    Label::Foreground
                } else if x == w - 1 {
                    Label::Background
                } else {
                    Label::ProbablyForeground
                });
            }
        }
        grabcut(&px, w, h, &mut tri, 4, &mut |_| {})
            .unwrap()
            .unwrap();
        for y in 0..h {
            for x in 0..w {
                assert_eq!(tri[y * w + x].is_foreground(), x < 12, "({x},{y})");
            }
        }
    }

    #[test]
    fn one_sided_trimap_has_nothing_to_cut() {
        let px = vec![[1.0, 2.0, 3.0]; 4];
        let mut tri = vec![Label::ProbablyForeground; 4];
        assert!(grabcut(&px, 2, 2, &mut tri, 3, &mut |_| {})
            .unwrap()
            .is_none());
    }
}
