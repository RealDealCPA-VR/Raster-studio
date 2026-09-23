//! Puppet Warp: pins on a triangle mesh laid over a layer's ink.
//!
//! [`PuppetMesh::from_alpha`] covers every cell of a regular grid that holds
//! ink (alpha above zero), dilated by one cell so the outline has room to
//! bend, and splits each covered cell into two triangles. The grid spacing is
//! the [`MeshDensity`]: the long side of the image is cut into a bounded
//! number of cells, so the mesh size does not grow with the image.
//!
//! Pins hold mesh vertices. [`PuppetMesh::deform`] moves the free vertices so
//! the mesh follows the pins:
//!
//! * [`PuppetMode::Rigid`] is **as-rigid-as-possible** (Sorkine and Alexa's
//!   local/global iteration, with uniform edge weights): each vertex's
//!   one-ring is fitted with its best rotation, then the vertex positions are
//!   solved so every edge is as close as possible to its rotated rest edge,
//!   with the pinned vertices held.
//! * [`PuppetMode::Linear`] is the simpler **linear blend**: each free vertex
//!   moves by the average of the pins' moves, weighted by inverse squared rest
//!   distance, so a vertex follows the pins near it.
//!
//! [`PuppetMesh::warp`] then maps every deformed triangle back onto its rest
//! triangle and resamples the source bilinearly. Pixels no deformed triangle
//! covers are transparent: the ink moved away from them. When no pin moved,
//! [`PuppetMesh::deform`] returns the rest positions exactly and
//! [`PuppetMesh::warp`] returns the source unchanged, bit for bit.
//!
//! Coordinates are image pixels, continuous, with `(0.5, 0.5)` the centre of
//! the top-left pixel.

use crate::buffer::FilterBuffer;
use crate::support::EdgeMode;

/// How finely the mesh follows the outline.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum MeshDensity {
    /// Large triangles: stiffer, faster.
    Fewer,
    #[default]
    Normal,
    /// Small triangles: follows the outline and the pins more closely.
    More,
}

impl MeshDensity {
    pub const ALL: [MeshDensity; 3] = [MeshDensity::Fewer, MeshDensity::Normal, MeshDensity::More];

    /// Cells along the image's long side.
    pub fn cells(self) -> u32 {
        match self {
            MeshDensity::Fewer => 16,
            MeshDensity::Normal => 28,
            MeshDensity::More => 44,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MeshDensity::Fewer => "Fewer",
            MeshDensity::Normal => "Normal",
            MeshDensity::More => "More",
        }
    }
}

/// How the free vertices follow the pins.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PuppetMode {
    /// As-rigid-as-possible.
    #[default]
    Rigid,
    /// Inverse-distance linear blend of the pins' moves.
    Linear,
}

impl PuppetMode {
    pub const ALL: [PuppetMode; 2] = [PuppetMode::Rigid, PuppetMode::Linear];

    pub fn label(self) -> &'static str {
        match self {
            PuppetMode::Rigid => "Rigid",
            PuppetMode::Linear => "Linear",
        }
    }
}

/// A pin: the mesh vertex it holds and where that vertex is now.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Pin {
    pub vertex: u32,
    pub at: [f32; 2],
}

/// ARAP outer (local/global) iterations.
const ARAP_ITERATIONS: usize = 10;
/// Gauss-Seidel sweeps per global step.
const ARAP_SWEEPS: usize = 12;

/// The triangulated mesh over a layer's ink, in its rest pose.
#[derive(Clone, PartialEq, Debug)]
pub struct PuppetMesh {
    width: u32,
    height: u32,
    /// Grid spacing, in image pixels.
    cell: u32,
    rest: Vec<[f32; 2]>,
    triangles: Vec<[u32; 3]>,
    neighbours: Vec<Vec<u32>>,
}

impl PuppetMesh {
    /// The mesh over every pixel of `alpha` (one byte per pixel, row-major,
    /// `width * height` long) that holds ink. `None` when there is no ink or
    /// the plane does not match the size.
    pub fn from_alpha(alpha: &[u8], width: u32, height: u32, density: MeshDensity) -> Option<Self> {
        let n = (width as usize).checked_mul(height as usize)?;
        if n == 0 || alpha.len() != n {
            return None;
        }
        let long = width.max(height);
        let cell = long.div_ceil(density.cells()).max(1);
        let cols = width.div_ceil(cell) as usize;
        let rows = height.div_ceil(cell) as usize;
        let mut inked = vec![false; cols * rows];
        for y in 0..height as usize {
            let row = &alpha[y * width as usize..(y + 1) * width as usize];
            let cy = y / cell as usize;
            for (x, a) in row.iter().enumerate() {
                if *a > 0 {
                    inked[cy * cols + x / cell as usize] = true;
                }
            }
        }
        if !inked.iter().any(|c| *c) {
            return None;
        }
        // Dilate by one cell so the outline has room to bend.
        let mut covered = inked.clone();
        for cy in 0..rows {
            for cx in 0..cols {
                if !inked[cy * cols + cx] {
                    continue;
                }
                for ny in cy.saturating_sub(1)..(cy + 2).min(rows) {
                    for nx in cx.saturating_sub(1)..(cx + 2).min(cols) {
                        covered[ny * cols + nx] = true;
                    }
                }
            }
        }
        let corner_cols = cols + 1;
        let mut index = vec![u32::MAX; corner_cols * (rows + 1)];
        let mut rest = Vec::new();
        let mut vertex = |vx: usize, vy: usize, rest: &mut Vec<[f32; 2]>| -> u32 {
            let slot = &mut index[vy * corner_cols + vx];
            if *slot == u32::MAX {
                *slot = rest.len() as u32;
                rest.push([(vx as u32 * cell) as f32, (vy as u32 * cell) as f32]);
            }
            *slot
        };
        let mut triangles = Vec::new();
        for cy in 0..rows {
            for cx in 0..cols {
                if !covered[cy * cols + cx] {
                    continue;
                }
                let tl = vertex(cx, cy, &mut rest);
                let tr = vertex(cx + 1, cy, &mut rest);
                let br = vertex(cx + 1, cy + 1, &mut rest);
                let bl = vertex(cx, cy + 1, &mut rest);
                triangles.push([tl, tr, br]);
                triangles.push([tl, br, bl]);
            }
        }
        let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); rest.len()];
        for t in &triangles {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if !neighbours[a as usize].contains(&b) {
                    neighbours[a as usize].push(b);
                }
                if !neighbours[b as usize].contains(&a) {
                    neighbours[b as usize].push(a);
                }
            }
        }
        Some(Self {
            width,
            height,
            cell,
            rest,
            triangles,
            neighbours,
        })
    }

    /// The image size the mesh was built for.
    pub fn image_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The distance between neighbouring vertices at rest, in image pixels.
    pub fn spacing(&self) -> f32 {
        self.cell as f32
    }

    /// The rest positions of the vertices.
    pub fn rest(&self) -> &[[f32; 2]] {
        &self.rest
    }

    /// The triangles, as vertex indices.
    pub fn triangles(&self) -> &[[u32; 3]] {
        &self.triangles
    }

    /// Whether `p` lies on the mesh in its rest pose — on the layer's ink or
    /// the band around it, where a pin may be placed.
    pub fn contains(&self, p: [f32; 2]) -> bool {
        self.triangles.iter().any(|t| {
            barycentric(
                p,
                self.rest[t[0] as usize],
                self.rest[t[1] as usize],
                self.rest[t[2] as usize],
            )
            .is_some_and(inside)
        })
    }

    /// The vertex nearest `p` in `positions` (the rest pose or a deformed
    /// one), and its distance.
    pub fn nearest_vertex(positions: &[[f32; 2]], p: [f32; 2]) -> Option<(u32, f32)> {
        positions
            .iter()
            .enumerate()
            .map(|(i, v)| (i as u32, dist(*v, p)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// The vertex positions the pins pull the mesh into.
    ///
    /// Pins naming a vertex the mesh does not have are ignored. With no pin
    /// moved off its rest position the rest positions are returned exactly.
    pub fn deform(&self, pins: &[Pin], mode: PuppetMode) -> Vec<[f32; 2]> {
        let pins: Vec<Pin> = pins
            .iter()
            .copied()
            .filter(|p| (p.vertex as usize) < self.rest.len() && p.at.iter().all(|v| v.is_finite()))
            .collect();
        if pins.iter().all(|p| p.at == self.rest[p.vertex as usize]) {
            return self.rest.clone();
        }
        let linear = self.linear_blend(&pins);
        match mode {
            PuppetMode::Linear => linear,
            // One pin can only translate; the blend already does exactly that.
            PuppetMode::Rigid if pins.len() < 2 => linear,
            PuppetMode::Rigid => self.arap(&pins, linear),
        }
    }

    fn linear_blend(&self, pins: &[Pin]) -> Vec<[f32; 2]> {
        let moves: Vec<([f32; 2], [f32; 2])> = pins
            .iter()
            .map(|p| {
                let r = self.rest[p.vertex as usize];
                (r, [p.at[0] - r[0], p.at[1] - r[1]])
            })
            .collect();
        let mut out: Vec<[f32; 2]> = self
            .rest
            .iter()
            .map(|v| {
                let (mut sx, mut sy, mut sw) = (0.0f32, 0.0f32, 0.0f32);
                for (r, m) in &moves {
                    let d2 = (v[0] - r[0]).powi(2) + (v[1] - r[1]).powi(2);
                    let w = 1.0 / (d2 + 1e-3);
                    sx += w * m[0];
                    sy += w * m[1];
                    sw += w;
                }
                [v[0] + sx / sw, v[1] + sy / sw]
            })
            .collect();
        for p in pins {
            out[p.vertex as usize] = p.at;
        }
        out
    }

    /// As-rigid-as-possible, starting from `current`.
    fn arap(&self, pins: &[Pin], mut current: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
        let mut pinned = vec![false; self.rest.len()];
        for p in pins {
            pinned[p.vertex as usize] = true;
            current[p.vertex as usize] = p.at;
        }
        let mut rotations = vec![[1.0f32, 0.0]; self.rest.len()];
        for _ in 0..ARAP_ITERATIONS {
            // Local step: the best rotation of each one-ring.
            for (i, ring) in self.neighbours.iter().enumerate() {
                let (mut a, mut b) = (0.0f32, 0.0f32);
                for &j in ring {
                    let e = sub(self.rest[i], self.rest[j as usize]);
                    let f = sub(current[i], current[j as usize]);
                    a += e[0] * f[0] + e[1] * f[1];
                    b += e[0] * f[1] - e[1] * f[0];
                }
                let len = (a * a + b * b).sqrt();
                rotations[i] = if len > 1e-12 {
                    [a / len, b / len]
                } else {
                    [1.0, 0.0]
                };
            }
            // Global step: Gauss-Seidel on the uniform-weight Laplacian.
            for _ in 0..ARAP_SWEEPS {
                for (i, ring) in self.neighbours.iter().enumerate() {
                    if pinned[i] || ring.is_empty() {
                        continue;
                    }
                    let mut acc = [0.0f32; 2];
                    for &j in ring {
                        let j = j as usize;
                        let e = sub(self.rest[i], self.rest[j]);
                        let ri = rotate(rotations[i], e);
                        let rj = rotate(rotations[j], e);
                        acc[0] += current[j][0] + 0.5 * (ri[0] + rj[0]);
                        acc[1] += current[j][1] + 0.5 * (ri[1] + rj[1]);
                    }
                    let n = ring.len() as f32;
                    current[i] = [acc[0] / n, acc[1] / n];
                }
            }
        }
        current
    }

    /// Warp `src` by moving the mesh from its rest pose to `deformed`.
    ///
    /// `src` may be any size; the mesh is laid over it by the ratio of the
    /// two. A `deformed` equal to the rest pose returns `src` unchanged; a
    /// `deformed` of the wrong length is treated as the rest pose.
    pub fn warp(&self, src: &FilterBuffer, deformed: &[[f32; 2]]) -> FilterBuffer {
        if src.is_empty()
            || self.width == 0
            || self.height == 0
            || deformed.len() != self.rest.len()
            || deformed == self.rest.as_slice()
        {
            return src.clone();
        }
        let (sw, sh) = src.dimensions();
        let kx = sw as f32 / self.width as f32;
        let ky = sh as f32 / self.height as f32;
        let scale = |p: [f32; 2]| [p[0] * kx, p[1] * ky];
        let mut out = src.same_size_blank();
        for t in &self.triangles {
            let [a, b, c] = t.map(|i| scale(deformed[i as usize]));
            let [ra, rb, rc] = t.map(|i| scale(self.rest[i as usize]));
            if !a.iter().chain(&b).chain(&c).all(|v| v.is_finite()) {
                continue;
            }
            let min_x = a[0].min(b[0]).min(c[0]).floor().max(0.0) as u32;
            let min_y = a[1].min(b[1]).min(c[1]).floor().max(0.0) as u32;
            let max_x = (a[0].max(b[0]).max(c[0]).ceil().max(0.0) as u32).min(sw);
            let max_y = (a[1].max(b[1]).max(c[1]).ceil().max(0.0) as u32).min(sh);
            for y in min_y..max_y {
                for x in min_x..max_x {
                    let p = [x as f32 + 0.5, y as f32 + 0.5];
                    let Some(l) = barycentric(p, a, b, c) else {
                        continue;
                    };
                    if !inside(l) {
                        continue;
                    }
                    let s = [
                        l[0] * ra[0] + l[1] * rb[0] + l[2] * rc[0],
                        l[0] * ra[1] + l[1] * rb[1] + l[2] * rc[1],
                    ];
                    out.set(x, y, src.sample_bilinear(s[0], s[1], EdgeMode::Clamp));
                }
            }
        }
        out
    }
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Rotate `v` by the unit complex number `r = [cos, sin]`.
fn rotate(r: [f32; 2], v: [f32; 2]) -> [f32; 2] {
    [r[0] * v[0] - r[1] * v[1], r[1] * v[0] + r[0] * v[1]]
}

/// Barycentric coordinates of `p` in triangle `abc`, or `None` when the
/// triangle is degenerate.
fn barycentric(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> Option<[f32; 3]> {
    let v0 = sub(b, a);
    let v1 = sub(c, a);
    let v2 = sub(p, a);
    let den = v0[0] * v1[1] - v1[0] * v0[1];
    if den.abs() < 1e-9 {
        return None;
    }
    let l1 = (v2[0] * v1[1] - v1[0] * v2[1]) / den;
    let l2 = (v0[0] * v2[1] - v2[0] * v0[1]) / den;
    Some([1.0 - l1 - l2, l1, l2])
}

/// Inside or on the edge, with a little slack so shared edges leave no gap.
fn inside(l: [f32; 3]) -> bool {
    l.iter().all(|v| *v >= -1e-4)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64x64 image with an opaque 40x16 horizontal bar of a gradient at
    /// y = 24..40, x = 12..52, transparent elsewhere.
    fn bar() -> (FilterBuffer, Vec<u8>) {
        let mut buf = FilterBuffer::transparent(64, 64).unwrap();
        let mut alpha = vec![0u8; 64 * 64];
        for y in 24..40 {
            for x in 12..52 {
                let v = x as f32 / 64.0;
                buf.set(x, y, [v, 0.5, 1.0 - v, 1.0]);
                alpha[(y * 64 + x) as usize] = 255;
            }
        }
        (buf, alpha)
    }

    fn pin_at(mesh: &PuppetMesh, p: [f32; 2]) -> Pin {
        let (vertex, _) = PuppetMesh::nearest_vertex(mesh.rest(), p).unwrap();
        Pin {
            vertex,
            at: mesh.rest()[vertex as usize],
        }
    }

    #[test]
    fn the_mesh_covers_the_ink_and_nothing_far_from_it() {
        let (_, alpha) = bar();
        let mesh = PuppetMesh::from_alpha(&alpha, 64, 64, MeshDensity::Normal).unwrap();
        assert!(!mesh.triangles().is_empty());
        assert!(mesh.contains([30.0, 30.0]));
        assert!(!mesh.contains([2.0, 2.0]), "the corner is far from the ink");
        assert!(PuppetMesh::from_alpha(&[0; 16], 4, 4, MeshDensity::Normal).is_none());
        let fewer = PuppetMesh::from_alpha(&alpha, 64, 64, MeshDensity::Fewer).unwrap();
        let more = PuppetMesh::from_alpha(&alpha, 64, 64, MeshDensity::More).unwrap();
        assert!(fewer.triangles().len() < more.triangles().len());
    }

    #[test]
    fn puppet_warp_with_pins_not_moved_is_identity() {
        let (src, alpha) = bar();
        let mesh = PuppetMesh::from_alpha(&alpha, 64, 64, MeshDensity::Normal).unwrap();
        let pins = [pin_at(&mesh, [14.0, 32.0]), pin_at(&mesh, [50.0, 32.0])];
        for mode in PuppetMode::ALL {
            let deformed = mesh.deform(&pins, mode);
            assert_eq!(deformed, mesh.rest());
            assert_eq!(mesh.warp(&src, &deformed), src, "{mode:?}");
        }
        assert_eq!(mesh.warp(&src, &mesh.deform(&[], PuppetMode::Rigid)), src);
    }

    #[test]
    fn moving_one_pin_moves_nearby_pixels_more_than_far_ones() {
        let (src, alpha) = bar();
        let mesh = PuppetMesh::from_alpha(&alpha, 64, 64, MeshDensity::Normal).unwrap();
        let left = pin_at(&mesh, [14.0, 32.0]);
        let mut right = pin_at(&mesh, [50.0, 32.0]);
        right.at[1] += 12.0;
        for mode in PuppetMode::ALL {
            let deformed = mesh.deform(&[left, right], mode);
            assert_eq!(
                deformed[left.vertex as usize],
                mesh.rest()[left.vertex as usize]
            );
            assert_eq!(deformed[right.vertex as usize], right.at);
            // Vertex moves: near the moved pin versus near the held one.
            let moved = |p: [f32; 2]| {
                let (v, _) = PuppetMesh::nearest_vertex(mesh.rest(), p).unwrap();
                dist(deformed[v as usize], mesh.rest()[v as usize])
            };
            let near = moved([44.0, 32.0]);
            let far = moved([20.0, 32.0]);
            assert!(near > far + 2.0, "{mode:?}: near {near}, far {far}");
            // And in pixels: the column under the moved pin went down, the
            // column under the held pin stayed.
            let out = mesh.warp(&src, &deformed);
            let alpha_centroid = |buf: &FilterBuffer, x: u32| {
                let (mut s, mut t) = (0.0, 0.0);
                for y in 0..64 {
                    let a = buf.get(x, y)[3];
                    s += a * y as f32;
                    t += a;
                }
                s / t
            };
            let rest_near = alpha_centroid(&src, 46);
            let rest_far = alpha_centroid(&src, 16);
            let near_shift = alpha_centroid(&out, 46) - rest_near;
            let far_shift = alpha_centroid(&out, 16) - rest_far;
            assert!(
                near_shift > 4.0 && near_shift > far_shift.abs() * 3.0,
                "{mode:?}: near shift {near_shift}, far shift {far_shift}"
            );
        }
    }

    #[test]
    fn rigid_keeps_the_bar_s_length_better_than_the_blend() {
        // Pull the two ends apart vertically; ARAP bends, the rest length of
        // the middle edges is kept closer than the linear blend keeps it.
        let (_, alpha) = bar();
        let mesh = PuppetMesh::from_alpha(&alpha, 64, 64, MeshDensity::Normal).unwrap();
        let a = pin_at(&mesh, [14.0, 32.0]);
        let mut b = pin_at(&mesh, [50.0, 32.0]);
        let mut c = pin_at(&mesh, [32.0, 32.0]);
        b.at[1] -= 10.0;
        c.at[1] += 10.0;
        let strain = |d: &[[f32; 2]]| {
            let mut total = 0.0;
            for (i, ring) in mesh.neighbours.iter().enumerate() {
                for &j in ring {
                    let rest = dist(mesh.rest[i], mesh.rest[j as usize]);
                    total += (dist(d[i], d[j as usize]) - rest).abs();
                }
            }
            total
        };
        let rigid = strain(&mesh.deform(&[a, b, c], PuppetMode::Rigid));
        let linear = strain(&mesh.deform(&[a, b, c], PuppetMode::Linear));
        assert!(rigid < linear, "rigid strain {rigid} vs linear {linear}");
    }

    #[test]
    fn one_pin_translates_everything() {
        let (src, alpha) = bar();
        let mesh = PuppetMesh::from_alpha(&alpha, 64, 64, MeshDensity::Normal).unwrap();
        let mut p = pin_at(&mesh, [32.0, 32.0]);
        p.at[0] += 5.0;
        let d = mesh.deform(&[p], PuppetMode::Rigid);
        for (r, v) in mesh.rest().iter().zip(&d) {
            assert!((v[0] - r[0] - 5.0).abs() < 1e-3 && (v[1] - r[1]).abs() < 1e-3);
        }
        let out = mesh.warp(&src, &d);
        assert_eq!(out.get(12, 30)[3], 0.0, "the left edge moved away");
        assert!(out.get(55, 30)[3] > 0.9, "the right edge moved in");
    }
}
