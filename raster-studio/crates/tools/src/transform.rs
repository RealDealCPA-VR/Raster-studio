//! Free transform: scale, rotate, skew, distort, perspective and warp.
//!
//! Free transform is modal, and that shapes the whole design. A drag does not
//! commit anything — it edits a *state* (a destination quad, plus a control
//! mesh for warp) that the UI keeps drawing until the user presses Enter. Only
//! [`TransformTool::commit`] touches pixels, and it emits exactly one command
//! for the whole session however many handles were dragged.
//!
//! # Mapping and resampling
//! Every mode reduces to one of two maps.
//!
//! * Scale, rotate, skew, distort and perspective are all **projective**: four
//!   source corners to four destination corners. The map is a 3×3 homography
//!   solved from those eight correspondences; affine modes are simply the
//!   subset whose bottom row stays `[0, 0, 1]`.
//! * Warp is a **bicubic Bézier patch** over a 4×4 control mesh.
//!
//! Both are resampled by *inverse* mapping — for each destination pixel, find
//! where it came from — because forward scatter leaves holes. Sampling is
//! bicubic in linear premultiplied light, which is the only place it is correct
//! to interpolate: bicubic on gamma-encoded values darkens every edge it
//! crosses, and on straight alpha it drags the colour of transparent pixels
//! into the fringe.
//!
//! # The singular case
//! Dragging a corner onto its opposite collapses the quad. The homography then
//! has no inverse, and inverse mapping through it produces NaN for every pixel
//! — which would be written straight into the layer. That case is detected
//! before anything is read or written and refused with
//! [`editor_core::CommandError::NotInvertible`].

use editor_core::Command;
use filters::{EdgeMode, FilterBuffer};
use glam::{IVec2, Vec2};
use layer_model::LayerId;
use raster::PixelRect;
use selection::transform_selection;

use crate::error::ToolError;
use crate::patch::{ColorPatch, CoveragePatch};
use crate::tool::{PaintTarget, PointerEvent, SessionGeometry, Tool, ToolContext, ToolId};

/// Which kind of edit a handle drag performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransformMode {
    /// Corners and edges resize the box; the quad stays a parallelogram.
    #[default]
    Scale,
    /// Handles spin the quad about the pivot.
    Rotate,
    /// Edges slide parallel to themselves.
    Skew,
    /// Every corner moves independently.
    Distort,
    /// A corner and its edge-mate move in opposite directions, which is what
    /// makes a rectangle read as a receding plane.
    Perspective,
    /// The control mesh bends the interior, not just the outline.
    Warp,
}

impl TransformMode {
    /// Every mode, in the registry's choice-option order — the same order the
    /// options bar's segmented control and [`Tool::set_choice`] speak.
    pub const ALL: [TransformMode; 6] = [
        TransformMode::Scale,
        TransformMode::Rotate,
        TransformMode::Skew,
        TransformMode::Distort,
        TransformMode::Perspective,
        TransformMode::Warp,
    ];

    /// The mode a choice index names, clamped into range.
    pub fn from_index(index: usize) -> Self {
        Self::ALL
            .get(index)
            .copied()
            .unwrap_or(TransformMode::Scale)
    }
}

/// What the pointer grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// `0`=top-left, `1`=top-right, `2`=bottom-right, `3`=bottom-left.
    Corner(usize),
    /// `0`=top, `1`=right, `2`=bottom, `3`=left.
    Edge(usize),
    /// The ring just outside corner `i`, where the cursor becomes a rotate arrow.
    Rotate(usize),
    /// The rotation centre, itself draggable.
    Pivot,
    /// A warp control point, `(row, column)` in `0..4`.
    Mesh(usize, usize),
    /// Anywhere inside the quad: move the whole thing.
    Inside,
}

/// How close the pointer has to be to grab a handle, in document pixels.
pub const HANDLE_RADIUS: f32 = 6.0;

/// The band outside a corner that rotates instead of scaling.
pub const ROTATE_BAND: f32 = 18.0;

/// A 4×4 Bézier control mesh over the source box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarpMesh {
    /// Control points, `[row][column]`, row 0 at the top.
    pub points: [[Vec2; 4]; 4],
}

impl WarpMesh {
    /// The mesh that changes nothing.
    ///
    /// Evenly spaced control points make a cubic Bézier the identity map on its
    /// parameter, so this patch maps the unit square onto `rect` exactly — the
    /// property [`WarpMesh::eval`]'s test pins.
    pub fn identity(rect: PixelRect) -> Self {
        let mut points = [[Vec2::ZERO; 4]; 4];
        for (r, row) in points.iter_mut().enumerate() {
            for (c, p) in row.iter_mut().enumerate() {
                *p = Vec2::new(
                    rect.x as f32 + rect.width as f32 * (c as f32 / 3.0),
                    rect.y as f32 + rect.height as f32 * (r as f32 / 3.0),
                );
            }
        }
        Self { points }
    }

    fn basis(t: f32) -> [f32; 4] {
        let u = 1.0 - t;
        [u * u * u, 3.0 * t * u * u, 3.0 * t * t * u, t * t * t]
    }

    /// Where parameter `(u, v)` in the unit square lands.
    pub fn eval(&self, u: f32, v: f32) -> Vec2 {
        let bu = Self::basis(u.clamp(0.0, 1.0));
        let bv = Self::basis(v.clamp(0.0, 1.0));
        let mut out = Vec2::ZERO;
        for (row, wv) in self.points.iter().zip(bv) {
            for (p, wu) in row.iter().zip(bu) {
                out += *p * (wv * wu);
            }
        }
        out
    }

    fn is_finite(&self) -> bool {
        self.points
            .iter()
            .all(|r| r.iter().all(|p| p.x.is_finite() && p.y.is_finite()))
    }
}

/// A 3×3 projective map, row-major.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography(pub [f64; 9]);

impl Homography {
    /// Solve the map taking `src` to `dst`, corner by corner.
    ///
    /// `None` when the correspondences are degenerate — three collinear
    /// corners, a collapsed quad, a non-finite point. That `None` is what
    /// becomes [`editor_core::CommandError::NotInvertible`].
    pub fn from_quads(src: [Vec2; 4], dst: [Vec2; 4]) -> Option<Self> {
        for p in src.iter().chain(dst.iter()) {
            if !p.x.is_finite() || !p.y.is_finite() {
                return None;
            }
        }
        // Eight unknowns (h8 fixed at 1), two equations per corner.
        let mut a = [[0.0f64; 9]; 8];
        for i in 0..4 {
            let (x, y) = (src[i].x as f64, src[i].y as f64);
            let (u, v) = (dst[i].x as f64, dst[i].y as f64);
            a[i * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            a[i * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        let h = solve8(&mut a)?;
        let m = [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0];
        if !m.iter().all(|v| v.is_finite()) {
            return None;
        }
        // The eight-equation system can be solvable while the map it describes
        // is not: three collinear destination corners give a perfectly
        // well-conditioned solve whose 3×3 matrix has rank 2 and squashes the
        // plane onto a line. That map has no inverse either, so it is refused
        // here rather than one layer up, where it would already have produced
        // NaN. Compared against the matrix's own magnitude, since a
        // determinant's scale is cubic in the coefficients.
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        let scale = m.iter().fold(0.0f64, |a, v| a.max(v.abs())).max(1e-12);
        if !det.is_finite() || det.abs() < 1e-9 * scale * scale * scale {
            return None;
        }
        Some(Homography(m))
    }

    /// Map a point; `None` when it lands on the horizon (`w == 0`).
    pub fn apply(&self, p: Vec2) -> Option<Vec2> {
        let m = &self.0;
        let (x, y) = (p.x as f64, p.y as f64);
        let w = m[6] * x + m[7] * y + m[8];
        if w.abs() < 1e-12 || !w.is_finite() {
            return None;
        }
        let u = (m[0] * x + m[1] * y + m[2]) / w;
        let v = (m[3] * x + m[4] * y + m[5]) / w;
        if !u.is_finite() || !v.is_finite() {
            return None;
        }
        Some(Vec2::new(u as f32, v as f32))
    }
}

/// Gaussian elimination with partial pivoting on an 8×9 augmented system.
fn solve8(a: &mut [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let mut pivot = col;
        for r in col + 1..8 {
            if a[r][col].abs() > a[pivot][col].abs() {
                pivot = r;
            }
        }
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let d = a[col][col];
        for v in a[col].iter_mut() {
            *v /= d;
        }
        for r in 0..8 {
            if r == col {
                continue;
            }
            let f = a[r][col];
            if f == 0.0 {
                continue;
            }
            let pivot_row = a[col];
            for (v, p) in a[r].iter_mut().zip(pivot_row.iter()).skip(col) {
                *v -= f * *p;
            }
        }
    }
    let mut out = [0.0f64; 8];
    for (i, o) in out.iter_mut().enumerate() {
        *o = a[i][8];
        if !o.is_finite() {
            return None;
        }
    }
    Some(out)
}

/// The live state of a transform session.
#[derive(Debug, Clone, PartialEq)]
pub struct TransformState {
    /// The pixels being transformed.
    pub source: PixelRect,
    /// Where the source's four corners currently sit.
    pub corners: [Vec2; 4],
    /// Rotation centre.
    pub pivot: Vec2,
    /// The warp control mesh, once warp mode has been entered.
    pub mesh: Option<WarpMesh>,
}

impl TransformState {
    /// A session that has not moved anything yet.
    pub fn new(source: PixelRect) -> Self {
        let (x0, y0) = (source.x as f32, source.y as f32);
        let (x1, y1) = (source.right() as f32, source.bottom() as f32);
        Self {
            source,
            corners: [
                Vec2::new(x0, y0),
                Vec2::new(x1, y0),
                Vec2::new(x1, y1),
                Vec2::new(x0, y1),
            ],
            pivot: Vec2::new((x0 + x1) * 0.5, (y0 + y1) * 0.5),
            mesh: None,
        }
    }

    /// The source rect's own corners, in the same order as `corners`.
    pub fn source_corners(&self) -> [Vec2; 4] {
        let (x0, y0) = (self.source.x as f32, self.source.y as f32);
        let (x1, y1) = (self.source.right() as f32, self.source.bottom() as f32);
        [
            Vec2::new(x0, y0),
            Vec2::new(x1, y0),
            Vec2::new(x1, y1),
            Vec2::new(x0, y1),
        ]
    }

    /// Every grab point the UI should draw, with what grabbing it does.
    pub fn handles(&self, mode: TransformMode) -> Vec<(Handle, Vec2)> {
        let mut out = Vec::with_capacity(25);
        if mode == TransformMode::Warp {
            let mesh = self.mesh.unwrap_or_else(|| WarpMesh::identity(self.source));
            for r in 0..4 {
                for c in 0..4 {
                    out.push((Handle::Mesh(r, c), mesh.points[r][c]));
                }
            }
            return out;
        }
        for (i, c) in self.corners.iter().enumerate() {
            out.push((Handle::Corner(i), *c));
        }
        for i in 0..4 {
            let a = self.corners[i];
            let b = self.corners[(i + 1) % 4];
            out.push((Handle::Edge(i), (a + b) * 0.5));
        }
        out.push((Handle::Pivot, self.pivot));
        out
    }

    /// What is under the pointer.
    ///
    /// Order matters and is the order the user expects: the explicit handles
    /// first, then the rotate band that surrounds each corner, then the
    /// interior. Without the band, rotating would require a modifier; with it
    /// placed before the corners, scaling would be impossible.
    /// Handle hit regions in **screen** pixels (card 039): the radii are
    /// constant on screen, so `zoom` divides them into document space — at
    /// low zoom the handles stay grabbable, at high zoom they don't sprawl.
    pub fn hit_test(&self, p: Vec2, mode: TransformMode) -> Option<Handle> {
        self.hit_test_zoomed(p, mode, 1.0)
    }

    /// [`Self::hit_test`] at an explicit view zoom.
    pub fn hit_test_zoomed(&self, p: Vec2, mode: TransformMode, zoom: f32) -> Option<Handle> {
        let zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        let handle_radius = HANDLE_RADIUS / zoom;
        let rotate_band = ROTATE_BAND / zoom;
        let mut best: Option<(f32, Handle)> = None;
        for (h, pos) in self.handles(mode) {
            let d = (p - pos).length();
            if d <= handle_radius && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, h));
            }
        }
        if let Some((_, h)) = best {
            return Some(h);
        }
        if mode != TransformMode::Warp {
            for (i, c) in self.corners.iter().enumerate() {
                let d = (p - *c).length();
                if d > handle_radius && d <= handle_radius + rotate_band {
                    return Some(Handle::Rotate(i));
                }
            }
        }
        if point_in_quad(p, &self.corners) {
            return Some(Handle::Inside);
        }
        None
    }

    /// Apply one drag step.
    ///
    /// `from` and `to` are the previous and current pointer positions, so a
    /// drag is expressed as a delta and repeated calls compose.
    pub fn drag(&mut self, mode: TransformMode, handle: Handle, from: Vec2, to: Vec2) {
        self.drag_with(mode, handle, from, to, false, false)
    }

    /// Card 041: the same gesture with the keyboard modifiers applied —
    /// `preserve_aspect` (default corner scaling) and `around_center`
    /// (Alt). Shift flips the aspect constraint; Alt re-pivots the scale.
    pub fn drag_with(
        &mut self,
        mode: TransformMode,
        handle: Handle,
        from: Vec2,
        to: Vec2,
        preserve_aspect: bool,
        around_center: bool,
    ) {
        if !to.x.is_finite() || !to.y.is_finite() || !from.x.is_finite() || !from.y.is_finite() {
            return;
        }
        let delta = to - from;
        match handle {
            Handle::Pivot => self.pivot += delta,
            Handle::Inside => {
                for c in self.corners.iter_mut() {
                    *c += delta;
                }
                self.pivot += delta;
                if let Some(m) = &mut self.mesh {
                    for row in m.points.iter_mut() {
                        for p in row.iter_mut() {
                            *p += delta;
                        }
                    }
                }
            }
            Handle::Rotate(_) => {
                let a0 = (from - self.pivot).y.atan2((from - self.pivot).x);
                let a1 = (to - self.pivot).y.atan2((to - self.pivot).x);
                self.rotate_by(a1 - a0);
            }
            Handle::Mesh(r, c) => {
                let mesh = self
                    .mesh
                    .get_or_insert_with(|| WarpMesh::identity(self.source));
                mesh.points[r][c] += delta;
            }
            Handle::Corner(i) => match mode {
                TransformMode::Rotate => {
                    let a0 = (from - self.pivot).y.atan2((from - self.pivot).x);
                    let a1 = (to - self.pivot).y.atan2((to - self.pivot).x);
                    self.rotate_by(a1 - a0);
                }
                TransformMode::Distort | TransformMode::Warp => self.corners[i] = to,
                TransformMode::Perspective => {
                    // The corner and its neighbour along the *nearest* edge
                    // move apart, which is the gesture that turns a rectangle
                    // into a receding plane.
                    let mate = (i + 1) % 4;
                    let prev = (i + 3) % 4;
                    let e_next = (self.corners[mate] - self.corners[i]).length();
                    let e_prev = (self.corners[prev] - self.corners[i]).length();
                    let other = if e_next <= e_prev { mate } else { prev };
                    self.corners[i] += delta;
                    self.corners[other] -= delta;
                }
                _ => self.scale_corner(i, to, preserve_aspect, around_center),
            },
            Handle::Edge(i) => match mode {
                TransformMode::Skew => {
                    // Slide the edge along its own direction.
                    let a = self.corners[i];
                    let b = self.corners[(i + 1) % 4];
                    let dir = (b - a).normalize_or_zero();
                    let slide = dir * delta.dot(dir);
                    self.corners[i] += slide;
                    self.corners[(i + 1) % 4] += slide;
                }
                TransformMode::Distort | TransformMode::Warp => {
                    self.corners[i] += delta;
                    self.corners[(i + 1) % 4] += delta;
                }
                _ => {
                    // Move the edge outward, keeping the opposite one put.
                    let a = self.corners[i];
                    let b = self.corners[(i + 1) % 4];
                    let edge = (b - a).normalize_or_zero();
                    let normal = Vec2::new(-edge.y, edge.x);
                    let push = normal * delta.dot(normal);
                    self.corners[i] += push;
                    self.corners[(i + 1) % 4] += push;
                }
            },
        }
    }

    fn rotate_by(&mut self, angle: f32) {
        if !angle.is_finite() {
            return;
        }
        let (s, c) = angle.sin_cos();
        let rot = |p: Vec2, pivot: Vec2| {
            let d = p - pivot;
            pivot + Vec2::new(d.x * c - d.y * s, d.x * s + d.y * c)
        };
        let pivot = self.pivot;
        for p in self.corners.iter_mut() {
            *p = rot(*p, pivot);
        }
        if let Some(m) = &mut self.mesh {
            for row in m.points.iter_mut() {
                for p in row.iter_mut() {
                    *p = rot(*p, pivot);
                }
            }
        }
    }

    /// Move corner `i` while keeping the quad a parallelogram anchored at the
    /// opposite corner — what "scale" means once the box has been rotated.
    fn scale_corner(&mut self, i: usize, to: Vec2, preserve_aspect: bool, around_center: bool) {
        // Card 041: Alt scales around the quad's center — every corner moves
        // symmetrically, the center stays put.
        if around_center {
            let center =
                (self.corners[0] + self.corners[1] + self.corners[2] + self.corners[3]) * 0.25;
            let rel_i = self.corners[i] - center;
            let rel_to = to - center;
            let (fx, fy) = if preserve_aspect {
                let fx = if rel_i.x.abs() > 1e-6 {
                    rel_to.x / rel_i.x
                } else {
                    1.0
                };
                let fy = if rel_i.y.abs() > 1e-6 {
                    rel_to.y / rel_i.y
                } else {
                    1.0
                };
                let uniform = if fx.abs() > fy.abs() { fx } else { fy };
                (uniform, uniform)
            } else {
                (
                    if rel_i.x.abs() > 1e-6 {
                        rel_to.x / rel_i.x
                    } else {
                        1.0
                    },
                    if rel_i.y.abs() > 1e-6 {
                        rel_to.y / rel_i.y
                    } else {
                        1.0
                    },
                )
            };
            for corner in self.corners.iter_mut() {
                let rel = *corner - center;
                *corner = center + Vec2::new(rel.x * fx, rel.y * fy);
            }
            return;
        }
        let opp = (i + 2) % 4;
        let o = self.corners[opp];
        let a = self.corners[(i + 1) % 4];
        let b = self.corners[(i + 3) % 4];
        let ea = a - o;
        let eb = b - o;
        let det = ea.x * eb.y - ea.y * eb.x;
        if det.abs() < 1e-6 {
            self.corners[i] = to;
            return;
        }
        let v = to - o;
        let sa = (v.x * eb.y - v.y * eb.x) / det;
        let sb = (ea.x * v.y - ea.y * v.x) / det;
        // Card 041: default corner scaling preserves aspect — the two axis
        // factors collapse to one (the larger magnitude), so the quad scales
        // uniformly. Shift (preserve_aspect = false) keeps the free skew.
        let (sa, sb) = if preserve_aspect {
            let uniform = if sa.abs() > sb.abs() { sa } else { sb };
            (uniform, uniform)
        } else {
            (sa, sb)
        };
        self.corners[i] = o + ea * sa + eb * sb;
        self.corners[(i + 1) % 4] = o + ea * sa;
        self.corners[(i + 3) % 4] = o + eb * sb;
    }

    /// Card 041: the numeric-fields draft model — X/Y/W/H set the quad
    /// absolutely, the same draft dragging mutates. Non-finite and
    /// non-positive sizes are REFUSED (the caller keeps its previous
    /// values); no invalid matrix is ever committed from numbers.
    pub fn set_rect(&mut self, x: f32, y: f32, w: f32, h: f32) -> Result<(), ToolError> {
        if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
            return Err(ToolError::NotFinite {
                what: "rect",
                value: if x.is_finite() { w } else { x },
            });
        }
        if w <= 0.0 || h <= 0.0 {
            return Err(ToolError::Degenerate);
        }
        self.corners = [
            Vec2::new(x, y),
            Vec2::new(x + w, y),
            Vec2::new(x + w, y + h),
            Vec2::new(x, y + h),
        ];
        self.pivot = Vec2::new(x + w * 0.5, y + h * 0.5);
        Ok(())
    }

    /// Bounding rect of the destination, clipped to `canvas`.
    ///
    /// `mode` is not decoration: the mesh only describes the destination in
    /// [`TransformMode::Warp`], and it is exactly the gate
    /// [`TransformTool::commit`] and [`resample`] use. A session that visited
    /// warp mode and then switched to scale still *carries* its mesh — nothing
    /// throws it away, because switching back has to restore it — so consulting
    /// the mesh unconditionally would bound the destination by a stale box and
    /// the commit would silently clip the user's scale to it. The mesh's
    /// control points bound the Bézier patch (a Bézier surface stays inside its
    /// control hull), so in warp mode they are the right point set.
    /// The destination bounds WITHOUT the canvas clamp (card 044). The
    /// clamped variant is the canvas-facing answer used for overlays.
    pub fn dest_bounds_unclipped(&self, mode: TransformMode) -> Option<PixelRect> {
        self.dest_bounds_raw(mode)
    }

    /// The destination bounds clamped to `canvas` — the historical answer,
    /// kept for the pinned tests and future canvas-facing callers.
    pub fn dest_bounds(&self, canvas: PixelRect, mode: TransformMode) -> Option<PixelRect> {
        let raw = self.dest_bounds_raw(mode)?;
        let x0 = raw.x.max(canvas.x);
        let y0 = raw.y.max(canvas.y);
        let x1 = raw.right().min(canvas.right());
        let y1 = raw.bottom().min(canvas.bottom());
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// The true bounds of the transformed quad, wherever they land.
    fn dest_bounds_raw(&self, mode: TransformMode) -> Option<PixelRect> {
        let mut lo = Vec2::splat(f32::INFINITY);
        let mut hi = Vec2::splat(f32::NEG_INFINITY);
        let pts: Vec<Vec2> = match self.mesh.filter(|_| mode == TransformMode::Warp) {
            Some(m) => {
                let mut v = Vec::new();
                for r in 0..4 {
                    for c in 0..4 {
                        v.push(m.points[r][c]);
                    }
                }
                v
            }
            None => self.corners.to_vec(),
        };
        for p in pts {
            if !p.x.is_finite() || !p.y.is_finite() {
                return None;
            }
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let x0 = lo.x.floor() as i64;
        let y0 = lo.y.floor() as i64;
        let x1 = hi.x.ceil() as i64 + 1;
        let y1 = hi.y.ceil() as i64 + 1;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }
}

fn point_in_quad(p: Vec2, q: &[Vec2; 4]) -> bool {
    let mut sign = 0i32;
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) % 4];
        let cross = (b - a).perp_dot(p - a);
        let s = if cross > 0.0 {
            1
        } else if cross < 0.0 {
            -1
        } else {
            0
        };
        if s != 0 {
            if sign == 0 {
                sign = s;
            } else if sign != s {
                return false;
            }
        }
    }
    true
}

/// Invert the bilinear map of a quad at `p`, returning `(s, t)` in the unit
/// square, or `None` when `p` is outside it.
fn inverse_bilinear(p: Vec2, q: [Vec2; 4]) -> Option<(f32, f32)> {
    // Newton on f(s,t) = P(s,t) - p. Eight iterations: the round-trip test
    // pins the residual, and eight is what it is pinned at.
    let (mut s, mut t) = (0.5f32, 0.5f32);
    for _ in 0..8 {
        let a = q[0] + (q[1] - q[0]) * s;
        let b = q[3] + (q[2] - q[3]) * s;
        let f = a + (b - a) * t - p;
        let dfds = (q[1] - q[0]) * (1.0 - t) + (q[2] - q[3]) * t;
        let dfdt = b - a;
        let det = dfds.x * dfdt.y - dfds.y * dfdt.x;
        if det.abs() < 1e-9 {
            return None;
        }
        let ds = (f.x * dfdt.y - f.y * dfdt.x) / det;
        let dt = (dfds.x * f.y - dfds.y * f.x) / det;
        s -= ds;
        t -= dt;
        if !s.is_finite() || !t.is_finite() {
            return None;
        }
    }
    if (-0.001..=1.001).contains(&s) && (-0.001..=1.001).contains(&t) {
        Some((s.clamp(0.0, 1.0), t.clamp(0.0, 1.0)))
    } else {
        None
    }
}

/// Resample a transformed copy of `src` into a fresh plane.
///
/// `patch_rect` is the plane both buffers live on; `state` says where the
/// source box goes. Returns the new plane, with everything outside the source
/// box left exactly as it was and the source box itself emptied before the
/// transformed pixels are laid down.
pub fn resample(
    src: &FilterBuffer,
    patch_rect: PixelRect,
    state: &TransformState,
    mode: TransformMode,
) -> Result<FilterBuffer, ToolError> {
    let (w, h) = (src.width(), src.height());
    let origin = IVec2::new(patch_rect.x as i32, patch_rect.y as i32);
    let mut out = src.clone();

    // Clear the source box: its content is being moved, not copied.
    for y in state.source.y..state.source.bottom() {
        for x in state.source.x..state.source.right() {
            let lx = x - patch_rect.x;
            let ly = y - patch_rect.y;
            if lx < 0 || ly < 0 || lx >= w as i64 || ly >= h as i64 {
                continue;
            }
            out.set(lx as u32, ly as u32, [0.0; 4]);
        }
    }

    let sample = |u: f32, v: f32| -> [f32; 4] {
        // `u`, `v` are document coordinates; the buffer is patch-local.
        src.sample_bicubic(u - origin.x as f32, v - origin.y as f32, EdgeMode::Clamp)
    };

    match (mode, state.mesh) {
        (TransformMode::Warp, Some(mesh)) => {
            if !mesh.is_finite() {
                return Err(ToolError::not_invertible());
            }
            const N: usize = 16;
            let sx = state.source.x as f32;
            let sy = state.source.y as f32;
            let sw = state.source.width as f32;
            let sh = state.source.height as f32;
            for gy in 0..N {
                for gx in 0..N {
                    let u0 = gx as f32 / N as f32;
                    let u1 = (gx + 1) as f32 / N as f32;
                    let v0 = gy as f32 / N as f32;
                    let v1 = (gy + 1) as f32 / N as f32;
                    let quad = [
                        mesh.eval(u0, v0),
                        mesh.eval(u1, v0),
                        mesh.eval(u1, v1),
                        mesh.eval(u0, v1),
                    ];
                    let lo = quad
                        .iter()
                        .fold(Vec2::splat(f32::INFINITY), |a, b| a.min(*b));
                    let hi = quad
                        .iter()
                        .fold(Vec2::splat(f32::NEG_INFINITY), |a, b| a.max(*b));
                    if !lo.x.is_finite() || !hi.x.is_finite() {
                        return Err(ToolError::not_invertible());
                    }
                    // Clip the cell's document-space box to the patch *before*
                    // scanning it. Without this the inner loop walks the raw
                    // bounding box, so the work done per cell scales with how
                    // far the user dragged a handle rather than with the region
                    // being written — a 400,000 px handle scans 400,000 rows to
                    // discard all but a few hundred. Clipping first makes the
                    // warp branch cost O(patch), the same as the homography
                    // branch below.
                    let px0 = (lo.x.floor() as i64).max(patch_rect.x);
                    let px1 = (hi.x.ceil() as i64).min(patch_rect.right() - 1);
                    let py0 = (lo.y.floor() as i64).max(patch_rect.y);
                    let py1 = (hi.y.ceil() as i64).min(patch_rect.bottom() - 1);
                    for py in py0..=py1 {
                        for px in px0..=px1 {
                            let lx = px - patch_rect.x;
                            let ly = py - patch_rect.y;
                            if lx < 0 || ly < 0 || lx >= w as i64 || ly >= h as i64 {
                                continue;
                            }
                            let p = Vec2::new(px as f32 + 0.5, py as f32 + 0.5);
                            let Some((s, t)) = inverse_bilinear(p, quad) else {
                                continue;
                            };
                            let u = u0 + (u1 - u0) * s;
                            let v = v0 + (v1 - v0) * t;
                            let c = sample(sx + u * sw, sy + v * sh);
                            if c[3] > 0.0 || c[0] > 0.0 || c[1] > 0.0 || c[2] > 0.0 {
                                out.set(lx as u32, ly as u32, c);
                            }
                        }
                    }
                }
            }
        }
        _ => {
            // Invert by solving destination -> source directly, so the refusal
            // happens before a single pixel is touched.
            let inv = Homography::from_quads(state.corners, state.source_corners())
                .ok_or_else(ToolError::not_invertible)?;
            let dest = state.corners;
            for y in 0..h {
                for x in 0..w {
                    let p = Vec2::new(
                        origin.x as f32 + x as f32 + 0.5,
                        origin.y as f32 + y as f32 + 0.5,
                    );
                    if !point_in_quad(p, &dest) {
                        continue;
                    }
                    let Some(s) = inv.apply(p) else {
                        continue;
                    };
                    if s.x < state.source.x as f32
                        || s.y < state.source.y as f32
                        || s.x >= state.source.right() as f32
                        || s.y >= state.source.bottom() as f32
                    {
                        continue;
                    }
                    out.set(x, y, sample(s.x, s.y));
                }
            }
        }
    }
    Ok(out)
}

/// The affine that maps three source points onto three destination points —
/// the parallelogram a Scale-mode gizmo produces. `None` when either triangle
/// is degenerate, which a collapsed destination always is.
/// The affine that carries `src`'s corners onto `dst`'s (plan card 013): the
/// transform a live preview renders with, straight from the handle state.
/// `None` for a degenerate quad.
pub fn quad_affine(src: [Vec2; 4], dst: [Vec2; 4]) -> Option<glam::Affine2> {
    let (s0, s1, s3) = (src[0], src[1], src[3]);
    let (c0, c1, c3) = (dst[0], dst[1], dst[3]);
    let a = s1 - s0;
    let b = s3 - s0;
    let det = a.x * b.y - a.y * b.x;
    if !det.is_finite() || det.abs() < 1e-9 {
        return None;
    }
    // Solve the 2x2 that maps the source basis onto the destination basis:
    // columns [ma | mb] = M [a | b], so M = [ma | mb] [a | b]⁻¹, and the
    // affine carries the translation so s0 lands on c0.
    let ma = c1 - c0;
    let mb = c3 - c0;
    // [a|b]⁻¹ = 1/det · [ b.y −b.x ; −a.y a.x ]
    let inv = glam::Mat2::from_cols_array(&[b.y / det, -a.y / det, -b.x / det, a.x / det]);
    let m = glam::Mat2::from_cols_array(&[ma.x, ma.y, mb.x, mb.y]) * inv;
    Some(glam::Affine2 {
        matrix2: m,
        translation: c0 - m * s0,
    })
}

/// The free transform tool.
pub struct TransformTool {
    pub mode: TransformMode,
    pub state: Option<TransformState>,
    /// Transform the active SELECTION's mask instead of the layer's pixels —
    /// the `target` option's second choice.
    pub selection_only: bool,
    /// The layer the session is transforming, captured at pointer-down so the
    /// preview (card 013) and the commit aim at the same layer even if the
    /// selection changes mid-gesture.
    pub layer: Option<LayerId>,
    /// Card 036: every selected participant (ancestor-normalized), recorded
    /// once at session start. One element = the plain single-layer path.
    targets: Vec<LayerId>,
    /// Card 035: the layer's parent chain, recorded once at session start —
    /// the baseline record the commit conjugates through, immune to an
    /// active-layer switch mid-session.
    parent: Option<glam::Affine2>,
    grabbed: Option<Handle>,
    last: Vec2,
}

impl Default for TransformTool {
    fn default() -> Self {
        Self {
            mode: TransformMode::Scale,
            state: None,
            selection_only: false,
            parent: None,
            targets: Vec::new(),
            layer: None,
            grabbed: None,
            last: Vec2::ZERO,
        }
    }
}

impl TransformTool {
    /// A tool set to one mode, with no session running.
    pub fn with_mode(mode: TransformMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Start a session over `source`.
    pub fn begin(&mut self, source: PixelRect) -> Result<(), ToolError> {
        if source.is_empty() {
            return Err(ToolError::Degenerate);
        }
        self.state = Some(TransformState::new(source));
        Ok(())
    }

    /// Handle positions for the UI, empty when no session is running.
    pub fn handles(&self) -> Vec<(Handle, Vec2)> {
        self.state
            .as_ref()
            .map(|s| s.handles(self.mode))
            .unwrap_or_default()
    }

    /// Commit the session: resample once, emit one command, end the session.
    pub fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let Some(state) = self.state.clone() else {
            return Ok(());
        };
        // The selection target resamples the selection MASK, not pixels: an
        // affine (Scale mode's parallelogram) maps source corners to current
        // ones. The projective modes are refused with a sentence rather than
        // silently downgraded.
        if self.selection_only {
            if self.mode != TransformMode::Scale {
                self.state = None;
                self.grabbed = None;
                return Err(ToolError::Degenerate);
            }
            let xf = quad_affine(state.source_corners(), state.corners)
                .ok_or_else(ToolError::not_invertible)?;
            let canvas = selection::rect::Rect::from_xywh(
                ctx.canvas.x as i32,
                ctx.canvas.y as i32,
                ctx.canvas.width,
                ctx.canvas.height,
            );
            let next = transform_selection(
                &ctx.selection,
                canvas,
                xf,
                selection::transform::ResampleFilter::Bilinear,
            )?;
            self.state = None;
            self.grabbed = None;
            ctx.emit(Command::SetSelection { selection: next });
            return Ok(());
        }
        // Refuse before reading anything: a collapsed quad has no inverse, and
        // mapping through it would write NaN into every pixel it touched.
        match state.mesh.filter(|_| self.mode == TransformMode::Warp) {
            Some(mesh) if !mesh.is_finite() => return Err(ToolError::not_invertible()),
            Some(_) => {}
            None => {
                if Homography::from_quads(state.corners, state.source_corners()).is_none() {
                    return Err(ToolError::not_invertible());
                }
            }
        }
        // Card 044: a quad collapsed to a line or a point has no inverse.
        // The canvas-clamped destination bounds used to catch this
        // incidentally; the unclamped bounds no longer do, so the guard is
        // explicit — the quad's own signed area, independent of the canvas.
        let area = quad_signed_area(state.corners);
        if !area.is_finite() || area.abs() < 1e-4 {
            self.state = None;
            self.grabbed = None;
            return Err(ToolError::not_invertible());
        }

        // Card 035: whole-layer move/scale/rotate/flip/skew commits as a
        // LAYER TRANSFORM, never a resample — the raster tiles keep their
        // hashes and Text/Shape/SmartObject kinds stay exactly what they
        // are. The gizmo's corner delta is a document-space affine; it
        // conjugates through the recorded parent chain onto the layer's own
        // transform (pre-multiplied, TransformLayer's contract). Warp and
        // the projective modes are not affine — they keep the resample path.
        if !self.selection_only
            && ctx.paint_target == PaintTarget::Layer
            && matches!(
                self.mode,
                TransformMode::Scale | TransformMode::Rotate | TransformMode::Skew
            )
        {
            // The session's captured layer, falling back to the context's
            // active layer for sessions begun directly (tests, future menu
            // bootstrap) — identity comes from wherever the session started.
            let layer = self
                .layer
                .or(ctx.active_layer)
                .ok_or(ToolError::NoActiveLayer)?;
            let delta_doc = quad_affine(state.source_corners(), state.corners)
                .ok_or_else(ToolError::not_invertible)?;
            // The baseline record from session start — not the context's
            // live value: an active-layer switch mid-session must not
            // re-aim the conjugation at another layer's chain.
            let parent = self.parent.unwrap_or(glam::Affine2::IDENTITY);
            let delta_parent = parent.inverse() * delta_doc * parent;
            // Card 036: more than one selected participant moves together —
            // one document-space delta, the shell conjugates per participant
            // and wraps them in ONE transaction. A participant locked after
            // session start still refuses the whole commit (all-or-nothing).
            // Card 043: a linked participant pulls the whole link chain in
            // BEFORE the lock check — the transform tool's policy refuses
            // the entire commit when any chain member is locked.
            let mut participants: Vec<LayerId> = self.targets.clone();
            // The active-layer fallback exists for sessions begun without a
            // pointer-down (tests, menu bootstrap) — it must NOT re-insert a
            // participant the ancestor normalization already dropped, or a
            // group+child selection double-moves the child.
            if participants.is_empty() {
                participants.push(layer);
            }
            // Card 043: the link chain joins before the lock check — minus
            // ids an already-selected ancestor shadows (a linked child of a
            // linked selected group rides the group, once).
            let targets = &participants;
            let participants: Vec<LayerId> =
                crate::with_link_chain(&participants, &ctx.linked_layers)
                    .into_iter()
                    .filter(|id| {
                        let shadowed = targets.iter().any(|top| {
                            if *top == *id {
                                return false;
                            }
                            let mut parent = ctx.parent_of(*id);
                            while let Some(p) = parent {
                                if p == *top {
                                    break;
                                }
                                parent = ctx.parent_of(p);
                            }
                            parent.is_some()
                        });
                        !shadowed
                    })
                    .collect();
            for locked in participants.iter().map(|t| ctx.layer_lock(*t)) {
                if locked == Some(true) {
                    return Err(ToolError::LayerLocked);
                }
            }
            if participants.len() > 1 {
                ctx.emit_request(crate::tool::ToolRequest::TransformLayers {
                    layers: participants,
                    delta: delta_doc.to_cols_array(),
                });
            } else {
                ctx.emit(Command::TransformLayer {
                    layer_id: layer,
                    matrix: delta_parent.to_cols_array(),
                });
            }
            self.state = None;
            self.grabbed = None;
            return Ok(());
        }
        // Card 044: the non-affine modes only exist as a pixel-patch
        // resample — on a parametric layer that would silently rasterize
        // the editable geometry. Refused with a sentence instead; the
        // affine modes stay editable for every kind.
        if ctx.active_layer_parametric
            && ctx.paint_target == PaintTarget::Layer
            && matches!(
                self.mode,
                TransformMode::Distort | TransformMode::Perspective | TransformMode::Warp
            )
        {
            self.state = None;
            self.grabbed = None;
            return Err(ToolError::NonAffineParametric);
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        // Card 044: neither the destination nor the union is canvas-clamped
        // — the tile store holds off-canvas content, and clipping here would
        // drop stored ink merely because it lies outside the picture. The
        // patch-size cap still bounds the allocation.
        let dest = state
            .dest_bounds_unclipped(self.mode)
            .ok_or(ToolError::Degenerate)?;
        let rect = union_unclipped(state.source, dest).ok_or(ToolError::Degenerate)?;
        let delta = match ctx.paint_target {
            PaintTarget::Layer => {
                let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
                let src = patch.buffer().clone();
                let out = resample(&src, patch.rect(), &state, self.mode)?;
                patch.replace(out)?;
                patch.commit(ctx.tiles, key)?
            }
            PaintTarget::Mask => {
                // Moving, scaling or warping a layer mask is the same geometry
                // problem, so it runs the identical resampler over the coverage
                // plane lifted into premultiplied grey — not a ColorPatch,
                // which would store a four-byte-per-pixel tile in the mask's
                // one-byte-per-pixel slot.
                let mut patch = CoveragePatch::load(ctx.tiles, key, rect)?;
                let src = patch.to_buffer()?;
                let out = resample(&src, patch.rect(), &state, self.mode)?;
                patch.replace_from_buffer(&out)?;
                patch.commit(ctx.tiles, key)?
            }
        };
        self.state = None;
        self.grabbed = None;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }
}

/// Card 044: the quad's signed area (shoelace) — the collapse detector.
fn quad_signed_area(corners: [Vec2; 4]) -> f32 {
    let mut area = 0.0f32;
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        area += a.x * b.y - b.x * a.y;
    }
    area * 0.5
}

fn union_unclipped(a: PixelRect, b: PixelRect) -> Option<PixelRect> {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = a.right().max(b.right());
    let y1 = a.bottom().max(b.bottom());
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}
impl Tool for TransformTool {
    fn id(&self) -> ToolId {
        ToolId::FreeTransform
    }

    fn set_choice(&mut self, key: &str, index: usize) {
        match key {
            "mode" => {
                // A live session keeps the mode it started with: switching to
                // Warp half-way through a drag would re-shape the quad under
                // the pointer.
                if self.state.is_none() {
                    self.mode = TransformMode::from_index(index);
                }
            }
            "target" => self.selection_only = index == 1,
            _ => {}
        }
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.state.is_none() {
            // No session yet: start one over the explicit pixel selection,
            // else the ACTIVE LAYER'S CONTENT (card 034: a small logo's box
            // surrounds the logo; a text layer's box surrounds the text) —
            // the whole canvas only when the layer has no ink to surround.
            self.layer = ctx.active_layer;
            self.parent = ctx.active_layer_parent_transform;
            // Card 036: the selected set, normalized — a participant whose
            // ancestor is also selected is dropped (its transform moves with
            // the ancestor; moving both would double it). Locked participants
            // refuse the whole session up front: all-or-nothing, documented —
            // a partial commit of a mixed selection is exactly the surprise
            // this refusal exists to prevent.
            let mut targets: Vec<LayerId> = Vec::new();
            for candidate in ctx.selected_layers.clone() {
                let mut ancestor = ctx.parent_of(candidate);
                let mut shadowed = false;
                while let Some(a) = ancestor {
                    if ctx.selected_layers.contains(&a) {
                        shadowed = true;
                        break;
                    }
                    ancestor = ctx.parent_of(a);
                }
                if shadowed || targets.contains(&candidate) {
                    continue;
                }
                targets.push(candidate);
            }
            for locked in targets.iter().map(|t| ctx.layer_lock(*t)) {
                if locked == Some(true) {
                    return Err(ToolError::LayerLocked);
                }
            }
            // The session's representative: the active layer when it
            // survived normalization, else the set's first participant. The
            // gizmo (and the single-layer commit path) aims here, so the
            // recorded parent chain must be THIS layer's — not the context's
            // active layer's.
            let representative = if targets.contains(&ctx.active_layer.unwrap_or_default()) {
                ctx.active_layer
            } else {
                targets.first().copied()
            };
            self.layer = representative.or(self.layer);
            // Empty selection (a restored project, say): keep card 035's
            // context fallback instead of clobbering it with None.
            self.parent = representative
                .and_then(|l| ctx.parent_transform_of(l))
                .or(self.parent);
            self.targets = targets.clone();
            let src = match ctx.selection.bounds() {
                Some((min, max)) => PixelRect::new(
                    min.x as i64,
                    min.y as i64,
                    (max.x - min.x).max(0) as u32,
                    (max.y - min.y).max(0) as u32,
                ),
                // The stored extent is tile-aligned (a 64px image in one
                // 256px tile reads as 256px) — clipping to the canvas keeps
                // the handles on the picture.
                None => {
                    let inked = ctx.active_layer_content_bounds.unwrap_or(ctx.canvas);
                    let x0 = inked.x.max(ctx.canvas.x);
                    let y0 = inked.y.max(ctx.canvas.y);
                    let x1 =
                        (inked.x + inked.width as i64).min(ctx.canvas.x + ctx.canvas.width as i64);
                    let y1 = (inked.y + inked.height as i64)
                        .min(ctx.canvas.y + ctx.canvas.height as i64);
                    PixelRect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
                }
            };
            self.begin(src)?;
        }
        let mode = self.mode;
        self.grabbed = self
            .state
            .as_ref()
            .and_then(|s| s.hit_test_zoomed(event.pos, mode, ctx.view.zoom));
        self.last = event.pos;
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let (Some(state), Some(handle)) = (self.state.as_mut(), self.grabbed) {
            // Card 041: Shift flips the default aspect preservation; Alt
            // scales around the center.
            let shift = event.modifiers.shift;
            let alt = event.modifiers.alt;
            let preserve = matches!(handle, Handle::Corner(_)) && !shift;
            state.drag_with(self.mode, handle, self.last, event.pos, preserve, alt);
            self.last = event.pos;
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        // Releasing a handle ends the *drag*, not the transform: free transform
        // stays live until it is committed or cancelled.
        self.grabbed = None;
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.state = None;
        self.grabbed = None;
        self.layer = None;
        self.parent = None;
        self.targets = Vec::new();
    }

    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        TransformTool::commit(self, ctx)
    }

    fn has_pending_commit(&self) -> bool {
        self.state.is_some()
    }

    fn is_active(&self) -> bool {
        self.state.is_some()
    }

    /// The live session's overlay geometry (card 012): the state exactly as
    /// the tool holds it, the mode, and the handle being dragged.
    fn live_geometry(&self) -> Option<SessionGeometry> {
        let state = self.state.as_ref()?;
        Some(SessionGeometry::Transform {
            state: state.clone(),
            mode: self.mode,
            active: self.grabbed,
            layer: self.layer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> PixelRect {
        PixelRect::new(0, 0, 100, 100)
    }

    #[test]
    fn an_identity_quad_gives_the_identity_homography() {
        let s = TransformState::new(rect());
        let h = Homography::from_quads(s.source_corners(), s.corners).unwrap();
        for p in [
            Vec2::new(0.0, 0.0),
            Vec2::new(50.0, 25.0),
            Vec2::new(100.0, 100.0),
        ] {
            let q = h.apply(p).unwrap();
            assert!((q - p).length() < 1e-3, "{p:?} -> {q:?}");
        }
    }

    #[test]
    fn a_collapsed_quad_has_no_homography() {
        let s = TransformState::new(rect());
        // Every corner on one point.
        let collapsed = [Vec2::ZERO; 4];
        assert!(Homography::from_quads(s.source_corners(), collapsed).is_none());
        assert!(Homography::from_quads(collapsed, s.corners).is_none());
        // Three collinear corners.
        let line = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(20.0, 0.0),
            Vec2::new(0.0, 10.0),
        ];
        assert!(Homography::from_quads(s.source_corners(), line).is_none());
        // A non-finite corner.
        let nan = [
            Vec2::new(f32::NAN, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        assert!(Homography::from_quads(s.source_corners(), nan).is_none());
    }

    #[test]
    fn a_perspective_quad_maps_its_own_corners_exactly() {
        let s = TransformState::new(rect());
        let dst = [
            Vec2::new(20.0, 0.0),
            Vec2::new(80.0, 0.0),
            Vec2::new(120.0, 100.0),
            Vec2::new(-20.0, 100.0),
        ];
        let h = Homography::from_quads(s.source_corners(), dst).unwrap();
        for (a, b) in s.source_corners().iter().zip(dst.iter()) {
            let q = h.apply(*a).unwrap();
            assert!((q - *b).length() < 1e-2, "{a:?} -> {q:?}, wanted {b:?}");
        }
        // It is genuinely projective: the bottom row is not [0,0,1].
        assert!(h.0[6].abs() + h.0[7].abs() > 1e-6);
    }

    #[test]
    fn an_identity_warp_mesh_is_the_identity_map() {
        let m = WarpMesh::identity(PixelRect::new(10, 20, 100, 60));
        for (u, v, x, y) in [
            (0.0, 0.0, 10.0, 20.0),
            (1.0, 0.0, 110.0, 20.0),
            (0.5, 0.5, 60.0, 50.0),
            (0.25, 0.75, 35.0, 65.0),
            (1.0, 1.0, 110.0, 80.0),
        ] {
            let p = m.eval(u, v);
            assert!(
                (p - Vec2::new(x, y)).length() < 1e-3,
                "({u},{v}) -> {p:?}, wanted ({x},{y})"
            );
        }
    }

    #[test]
    fn scaling_a_corner_keeps_the_quad_a_parallelogram() {
        let mut s = TransformState::new(rect());
        s.drag(
            TransformMode::Scale,
            Handle::Corner(2),
            Vec2::new(100.0, 100.0),
            Vec2::new(150.0, 200.0),
        );
        let [a, b, c, d] = s.corners;
        // Opposite edges stay parallel and equal.
        assert!(((b - a) - (c - d)).length() < 1e-3);
        assert!(((d - a) - (c - b)).length() < 1e-3);
        assert!((c - Vec2::new(150.0, 200.0)).length() < 1e-3);
        // The anchored corner did not move.
        assert!((a - Vec2::new(0.0, 0.0)).length() < 1e-6);
    }

    #[test]
    fn distort_moves_one_corner_and_leaves_the_others() {
        let mut s = TransformState::new(rect());
        let before = s.corners;
        s.drag(
            TransformMode::Distort,
            Handle::Corner(1),
            Vec2::new(100.0, 0.0),
            Vec2::new(130.0, -20.0),
        );
        assert_eq!(s.corners[0], before[0]);
        assert_eq!(s.corners[2], before[2]);
        assert_eq!(s.corners[3], before[3]);
        assert!((s.corners[1] - Vec2::new(130.0, -20.0)).length() < 1e-6);
    }

    #[test]
    fn rotating_preserves_edge_lengths() {
        let mut s = TransformState::new(rect());
        let before: Vec<f32> = (0..4)
            .map(|i| (s.corners[(i + 1) % 4] - s.corners[i]).length())
            .collect();
        s.drag(
            TransformMode::Rotate,
            Handle::Rotate(0),
            Vec2::new(100.0, 50.0),
            Vec2::new(50.0, 100.0),
        );
        for i in 0..4 {
            let now = (s.corners[(i + 1) % 4] - s.corners[i]).length();
            assert!(
                (now - before[i]).abs() < 1e-3,
                "edge {i}: {before:?} -> {now}"
            );
        }
    }

    /// Skew slides an edge *along itself*. The component of the drag across the
    /// edge is discarded — that is what distinguishes skew from distort, which
    /// would follow the pointer in both axes — and the opposite edge stays put,
    /// so the quad remains a parallelogram.
    #[test]
    fn skew_slides_an_edge_along_itself_and_ignores_the_across_component() {
        let mut s = TransformState::new(rect());
        let before = s.corners;
        // Edge 0 is the top edge, corners 0 -> 1, midpoint (50, 0). Drag it
        // right *and* down; only the rightward part may take effect.
        s.drag(
            TransformMode::Skew,
            Handle::Edge(0),
            Vec2::new(50.0, 0.0),
            Vec2::new(70.0, 10.0),
        );
        assert!((s.corners[0] - Vec2::new(20.0, 0.0)).length() < 1e-4);
        assert!((s.corners[1] - Vec2::new(120.0, 0.0)).length() < 1e-4);
        // The opposite edge did not move at all.
        assert_eq!(s.corners[2], before[2]);
        assert_eq!(s.corners[3], before[3]);
        // Still a parallelogram, and the slid edge kept its length.
        let [a, b, c, d] = s.corners;
        assert!(((b - a) - (c - d)).length() < 1e-3);
        assert!(((b - a).length() - 100.0).abs() < 1e-3);
        // A drag purely across the edge is a no-op.
        let mut s2 = TransformState::new(rect());
        s2.drag(
            TransformMode::Skew,
            Handle::Edge(0),
            Vec2::new(50.0, 0.0),
            Vec2::new(50.0, 40.0),
        );
        assert_eq!(s2.corners, before);
    }

    /// Perspective splays one edge: the dragged corner and its neighbour move
    /// by opposite deltas, so opposite edges stop being parallel — which is
    /// exactly what scale and distort must never do, and what makes the
    /// resulting map a genuine (still invertible) homography rather than an
    /// affine one.
    #[test]
    fn perspective_splays_an_edge_and_stays_invertible() {
        let mut s = TransformState::new(rect());
        let before = s.corners;
        // Corner 1 is (100, 0); its two edges are equal length, so the tie
        // breaks toward corner 2.
        s.drag(
            TransformMode::Perspective,
            Handle::Corner(1),
            Vec2::new(100.0, 0.0),
            Vec2::new(110.0, -5.0),
        );
        let delta = Vec2::new(10.0, -5.0);
        assert!((s.corners[1] - (before[1] + delta)).length() < 1e-4);
        assert!((s.corners[2] - (before[2] - delta)).length() < 1e-4);
        // The far edge is untouched.
        assert_eq!(s.corners[0], before[0]);
        assert_eq!(s.corners[3], before[3]);

        // The dragged edge is now longer than the one opposite it, and the two
        // are no longer parallel: this is not an affine map.
        let [a, b, c, d] = s.corners;
        let right = (c - b).length();
        let left = (a - d).length();
        assert!(right > left + 5.0, "edge did not splay: {right} vs {left}");
        assert!(
            ((b - a) - (c - d)).length() > 1.0,
            "perspective produced a parallelogram"
        );

        // And it still inverts, so a commit resamples rather than refusing.
        let h = Homography::from_quads(s.corners, s.source_corners())
            .expect("a splayed quad must still be invertible");
        for (i, corner) in s.corners.iter().enumerate() {
            let back = h.apply(*corner).unwrap();
            assert!(
                (back - before[i]).length() < 1e-2,
                "corner {i} did not map back: {back:?} vs {:?}",
                before[i]
            );
        }
    }

    /// The warp branch scans each Bézier cell's document-space box, and that box
    /// is clipped to the patch so the per-cell cost tracks the region being
    /// written rather than how far a handle was dragged. The clip has to be
    /// *exact*: one column too tight and the last pixel of the patch is cleared
    /// by the source-box wipe and never rewritten, leaving a transparent seam
    /// down the right and bottom edges of every warp.
    #[test]
    fn an_identity_warp_rewrites_every_pixel_of_the_patch_including_its_edges() {
        let rect = PixelRect::new(0, 0, 48, 48);
        let mut src = FilterBuffer::filled(48, 48, [0.0; 4]).unwrap();
        for y in 0..48 {
            for x in 0..48 {
                src.set(x, y, [0.1, 0.5, 0.9, 1.0]);
            }
        }
        let mut s = TransformState::new(rect);
        s.mesh = Some(WarpMesh::identity(rect));
        let out = resample(&src, rect, &s, TransformMode::Warp).unwrap();

        // Every pixel, edges included. The source box covers the whole patch,
        // so `resample` wipes all of it before the warp writes it back: any
        // pixel the scan misses shows up as a hole.
        for y in 0..48 {
            for x in 0..48 {
                let p = out.get(x, y);
                assert!(p[3] > 0.5, "identity warp left a hole at ({x}, {y}): {p:?}");
                assert!(
                    (p[2] - 0.9).abs() < 0.05,
                    "identity warp moved the colour at ({x}, {y}): {p:?}"
                );
            }
        }
    }

    /// Clipping is not allowed to change the picture, only the work. A mesh
    /// dragged far outside the patch must produce the same pixels as the same
    /// mesh does when everything stays in view.
    #[test]
    fn a_wildly_dragged_warp_handle_still_finishes_and_writes_only_the_patch() {
        let rect = PixelRect::new(0, 0, 32, 32);
        let mut src = FilterBuffer::filled(32, 32, [0.0; 4]).unwrap();
        for y in 0..32 {
            for x in 0..32 {
                src.set(x, y, [0.3, 0.3, 0.3, 1.0]);
            }
        }
        let mut s = TransformState::new(rect);
        let mut mesh = WarpMesh::identity(rect);
        // A handle 400,000 px away: the raw cell bounding boxes are enormous,
        // but the scan may only ever touch the 32x32 patch.
        mesh.points[0][0] += Vec2::new(400_000.0, 400_000.0);
        s.mesh = Some(mesh);
        let out = resample(&src, rect, &s, TransformMode::Warp).unwrap();
        for y in 0..32 {
            for x in 0..32 {
                assert!(
                    out.get(x, y).iter().all(|c| c.is_finite()),
                    "warp wrote a non-finite pixel at ({x}, {y})"
                );
            }
        }
    }

    /// Both modes have to survive the full commit path, not just the geometry:
    /// a resample that produced NaN would still pass the corner assertions
    /// above.
    #[test]
    fn skew_and_perspective_resample_to_finite_pixels() {
        for mode in [TransformMode::Skew, TransformMode::Perspective] {
            let source = PixelRect::new(0, 0, 32, 32);
            let mut src = FilterBuffer::filled(64, 64, [0.0; 4]).unwrap();
            for y in 0..32 {
                for x in 0..32 {
                    src.set(x, y, [0.2, 0.4, 0.6, 1.0]);
                }
            }
            let mut s = TransformState::new(source);
            match mode {
                TransformMode::Skew => s.drag(
                    mode,
                    Handle::Edge(0),
                    Vec2::new(16.0, 0.0),
                    Vec2::new(28.0, 0.0),
                ),
                _ => s.drag(
                    mode,
                    Handle::Corner(1),
                    Vec2::new(32.0, 0.0),
                    Vec2::new(38.0, -4.0),
                ),
            }
            let out = resample(&src, PixelRect::new(0, 0, 64, 64), &s, mode).unwrap();
            let mut painted = 0;
            for y in 0..64 {
                for x in 0..64 {
                    let p = out.get(x, y);
                    assert!(
                        p.iter().all(|c| c.is_finite()),
                        "{mode:?} wrote {p:?} at ({x},{y})"
                    );
                    if p[3] > 0.0 {
                        painted += 1;
                    }
                }
            }
            assert!(painted > 500, "{mode:?} moved almost nothing: {painted}");
        }
    }

    #[test]
    fn hit_testing_finds_corners_then_the_rotate_band_then_the_interior() {
        let s = TransformState::new(rect());
        assert_eq!(
            s.hit_test(Vec2::new(0.0, 0.0), TransformMode::Scale),
            Some(Handle::Corner(0))
        );
        assert_eq!(
            s.hit_test(Vec2::new(50.0, 0.0), TransformMode::Scale),
            Some(Handle::Edge(0))
        );
        assert!(matches!(
            s.hit_test(Vec2::new(-12.0, -12.0), TransformMode::Scale),
            Some(Handle::Rotate(_))
        ));
        assert_eq!(
            s.hit_test(Vec2::new(50.0, 50.0), TransformMode::Scale),
            Some(Handle::Pivot)
        );
        assert_eq!(
            s.hit_test(Vec2::new(30.0, 70.0), TransformMode::Scale),
            Some(Handle::Inside)
        );
        assert_eq!(
            s.hit_test(Vec2::new(-500.0, -500.0), TransformMode::Scale),
            None
        );
        // Warp mode exposes the 16 mesh points instead.
        assert_eq!(s.handles(TransformMode::Warp).len(), 16);
        assert!(matches!(
            s.hit_test(Vec2::new(0.0, 0.0), TransformMode::Warp),
            Some(Handle::Mesh(0, 0))
        ));
    }

    #[test]
    fn inverse_bilinear_round_trips_a_skewed_quad() {
        let q = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 2.0),
            Vec2::new(12.0, 9.0),
            Vec2::new(1.0, 8.0),
        ];
        for (s, t) in [(0.0, 0.0), (1.0, 0.0), (0.5, 0.5), (0.25, 0.9)] {
            let a = q[0] + (q[1] - q[0]) * s;
            let b = q[3] + (q[2] - q[3]) * s;
            let p = a + (b - a) * t;
            let (s2, t2) = inverse_bilinear(p, q).unwrap();
            assert!((s - s2).abs() < 1e-3 && (t - t2).abs() < 1e-3);
        }
        assert!(inverse_bilinear(Vec2::new(-50.0, -50.0), q).is_none());
    }

    #[test]
    fn a_transform_without_a_selection_surrounds_the_layers_ink_not_the_canvas() {
        use crate::tiles::MemoryTiles;
        use editor_core::Selection;
        // Card 034: a small logo's initial box surrounds the logo. The shell
        // publishes the active layer's content bounds; the pointer-down
        // session uses them instead of the canvas.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
        ctx.active_layer = Some(layer_model::LayerId::new());
        ctx.active_layer_content_bounds = Some(PixelRect::new(40, 44, 24, 18));
        let mut tool = TransformTool::default();
        tool.on_pointer_down(
            &mut ctx,
            PointerEvent {
                pos: Vec2::new(128.0, 128.0),
                pressure: 1.0,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        let handles = tool.handles();
        assert!(!handles.is_empty(), "the session started on the press");
        // Every handle sits inside the logo's rect (padded a little for the
        // corner extension), nowhere near the canvas edges.
        for (_, p) in handles {
            assert!(
                p.x >= 40.0 - 1.0 && p.x <= 64.0 + 1.0 && p.y >= 44.0 - 1.0 && p.y <= 62.0 + 1.0,
                "handle {p:?} left the logo's bounds"
            );
        }

        // An explicit pixel selection still wins over the layer bounds.
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
        ctx.active_layer = Some(layer_model::LayerId::new());
        ctx.active_layer_content_bounds = Some(PixelRect::new(40, 44, 24, 18));
        ctx.selection = Selection::Rect {
            min: glam::IVec2::new(100, 100),
            max: glam::IVec2::new(130, 130),
        };
        let mut tool = TransformTool::default();
        tool.on_pointer_down(
            &mut ctx,
            PointerEvent {
                pos: Vec2::new(110.0, 110.0),
                pressure: 1.0,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        for (_, p) in tool.handles() {
            assert!(
                p.x >= 99.0 && p.x <= 131.0 && p.y >= 99.0 && p.y <= 131.0,
                "the selection's geometry is the transform's source: {p:?}"
            );
        }

        // No ink and no selection: the canvas fallback keeps its meaning.
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
        let mut tool = TransformTool::default();
        tool.on_pointer_down(
            &mut ctx,
            PointerEvent {
                pos: Vec2::new(32.0, 32.0),
                pressure: 1.0,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        assert!(
            matches!(tool.state.as_ref().map(|s| s.source), Some(src) if src == PixelRect::new(0, 0, 64, 64))
        );
    }

    #[test]
    fn handle_hit_regions_are_constant_in_screen_pixels() {
        // Card 039: at 4× zoom a 10-document-px offset from a corner is
        // 40 screen px — far outside the 6px handle — and outside the
        // 24px/4 = 6-document-px rotate band, so it is an Inside grab. At
        // zoom 1 the same 10px offset IS inside the rotate band.
        let s = TransformState::new(PixelRect::new(0, 0, 96, 96));
        let near_corner = Vec2::new(10.0, 10.0);
        assert!(
            matches!(
                s.hit_test_zoomed(near_corner, TransformMode::Scale, 4.0),
                Some(Handle::Inside)
            ),
            "the bands shrink with zoom: a 40-screen-px offset is inside the quad"
        );
        assert!(
            matches!(
                s.hit_test_zoomed(near_corner, TransformMode::Scale, 1.0),
                Some(Handle::Rotate(0))
            ),
            "at zoom 1 the 10px offset sits in corner 0's rotate band"
        );
    }

    #[test]
    fn corner_scaling_preserves_aspect_by_default_and_shift_frees_it() {
        // Card 041: default corner scaling is uniform; Shift allows skew.
        let mut s = TransformState::new(PixelRect::new(0, 0, 100, 100));
        s.drag_with(
            TransformMode::Scale,
            Handle::Corner(2),
            Vec2::new(100.0, 100.0),
            Vec2::new(150.0, 190.0),
            true,
            false,
        );
        let w = s.corners[1].x - s.corners[0].x;
        let h = s.corners[3].y - s.corners[0].y;
        assert!((w - h).abs() < 1e-3, "the aspect is preserved: {w}x{h}");

        let mut s = TransformState::new(PixelRect::new(0, 0, 100, 100));
        s.drag_with(
            TransformMode::Scale,
            Handle::Corner(2),
            Vec2::new(100.0, 100.0),
            Vec2::new(150.0, 190.0),
            false,
            false,
        );
        let w = s.corners[1].x - s.corners[0].x;
        let h = s.corners[3].y - s.corners[0].y;
        assert!(
            (w - 150.0).abs() < 1e-3 && (h - 190.0).abs() < 1e-3,
            "free with Shift"
        );
    }

    #[test]
    fn alt_scales_around_the_center_instead_of_the_opposite_corner() {
        let mut s = TransformState::new(PixelRect::new(0, 0, 100, 100));
        let pivot_before = s.pivot;
        s.drag_with(
            TransformMode::Scale,
            Handle::Corner(2),
            Vec2::new(100.0, 100.0),
            Vec2::new(60.0, 60.0),
            true,
            true,
        );
        // Around-center scaling keeps the CENTER fixed (within the drag's
        // uniform factor) — the opposite corner moved symmetrically.
        let center = (s.corners[0] + s.corners[2]) * 0.5;
        assert!(
            (center - pivot_before).length() < 12.0,
            "the center barely moves under around-center scaling: {center:?}"
        );
    }

    #[test]
    fn numeric_edits_share_the_drag_draft_and_refuse_invalid_sizes() {
        let mut s = TransformState::new(PixelRect::new(10, 20, 100, 100));
        // The numeric X/Y/W/H draft: the same corners dragging mutates.
        s.set_rect(30.0, 40.0, 80.0, 60.0).unwrap();
        assert_eq!(s.corners[0], Vec2::new(30.0, 40.0));
        assert_eq!(s.corners[2], Vec2::new(110.0, 100.0));
        assert_eq!(s.pivot, Vec2::new(70.0, 70.0), "the pivot re-centers");
        // Non-finite and non-positive sizes are refused without mutation.
        assert!(s.set_rect(f32::NAN, 0.0, 10.0, 10.0).is_err());
        assert!(s.set_rect(0.0, 0.0, 0.0, 10.0).is_err());
        assert!(s.set_rect(0.0, 0.0, -5.0, 10.0).is_err());
        assert_eq!(
            s.corners[0],
            Vec2::new(30.0, 40.0),
            "refusals keep the draft"
        );
    }
}
