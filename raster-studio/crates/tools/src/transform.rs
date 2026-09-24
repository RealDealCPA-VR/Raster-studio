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

use editor_core::{Command, Selection};
use filters::{EdgeMode, FilterBuffer, Interpolation, Sampling};
use glam::{IVec2, Vec2};
use layer_model::LayerId;
use raster::PixelRect;
use selection::transform_selection;

use crate::error::ToolError;
use crate::patch::{ColorPatch, CoveragePatch};
use crate::tool::{
    PaintTarget, PointerEvent, SessionGeometry, Tool, ToolContext, ToolId, ToolSetting,
};

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
    /// W10-J: Edit > Content-Aware Scale. The handles resize an upright box
    /// as in [`TransformMode::Scale`] (no rotation), and the commit retargets
    /// the layer into that box by seam carving
    /// ([`filters::content_aware_scale`]), blended with a plain scale by the
    /// options bar's Amount ([`keys::CA_AMOUNT`]): 100% is all seam carving,
    /// 0% a plain scale. Photoshop's Protect (an alpha channel) and Protect
    /// Skin Tones options do not exist here.
    ContentAware,
}

impl TransformMode {
    /// Every mode, in the registry's choice-option order — the same order the
    /// options bar's segmented control and [`Tool::set_choice`] speak.
    pub const ALL: [TransformMode; 7] = [
        TransformMode::Scale,
        TransformMode::Rotate,
        TransformMode::Skew,
        TransformMode::Distort,
        TransformMode::Perspective,
        TransformMode::Warp,
        TransformMode::ContentAware,
    ];

    /// W10-J: the options bar's labels for the Mode choice, in
    /// [`TransformMode::ALL`] order.
    pub const LABELS: &'static [&'static str] = &[
        "Scale",
        "Rotate",
        "Skew",
        "Distort",
        "Perspective",
        "Warp",
        "Content-Aware",
    ];

    /// W10-J: the Mode choice index of [`TransformMode::ContentAware`].
    pub const CONTENT_AWARE_INDEX: usize = 6;

    /// W10-J: whether the options bar shows the Free Transform option `key`
    /// while the Mode choice holds `mode_index`. Content-Aware Scale's Amount
    /// ([`keys::CA_AMOUNT`]) means something only in
    /// [`TransformMode::ContentAware`], so it is hidden in every other mode;
    /// every other option always shows.
    pub fn option_shown(key: &str, mode_index: usize) -> bool {
        key != keys::CA_AMOUNT || mode_index == Self::CONTENT_AWARE_INDEX
    }

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
    /// W9-L: the vertical skew (degrees) the options bar last typed. Any
    /// parallelogram splits into rotation, scale and skew in many ways;
    /// [`NumericTransform::read`] splits it with this vertical skew, so the
    /// bar reads back the V Skew the user typed. (Four bytes, deliberately:
    /// the state rides inside `SessionGeometry`, whose variants must stay
    /// close in size, and this fits in the struct's existing padding.)
    pub skew_v: f32,
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
            skew_v: 0.0,
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
            // W10-J: Content-Aware Scale keeps the box upright.
            Handle::Rotate(_) if mode == TransformMode::ContentAware => {}
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
    resample_with(src, patch_rect, state, mode, Interpolation::Bicubic)
}

/// W9-L: [`resample`] through a chosen filter — Free Transform's
/// Interpolation (Nearest / Bilinear / Bicubic).
pub fn resample_with(
    src: &FilterBuffer,
    patch_rect: PixelRect,
    state: &TransformState,
    mode: TransformMode,
    interpolation: Interpolation,
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

    // W9-L: the session's Interpolation (bicubic by default).
    let sampling = Sampling::new(EdgeMode::Clamp, interpolation);
    let sample = |u: f32, v: f32| -> [f32; 4] {
        // `u`, `v` are document coordinates; the buffer is patch-local.
        src.sample(u - origin.x as f32, v - origin.y as f32, sampling)
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

// ---------------------------------------------------------------------------
// W9-L: the numeric options bar and the warp presets
// ---------------------------------------------------------------------------

/// The option keys of Free Transform's numeric options bar. Declared once
/// here so the registry, the tool and the options bar cannot disagree.
pub mod keys {
    /// Reference point: a 3 x 3 grid index, row-major from the top left.
    pub const REFERENCE: &str = "reference";
    /// The reference point's document X / Y.
    pub const X: &str = "x";
    pub const Y: &str = "y";
    /// Width / height as a percentage of the source box.
    pub const W: &str = "w";
    pub const H: &str = "h";
    /// Keep W and H in proportion when one of them is edited.
    pub const LINK: &str = "link";
    /// Rotation in degrees, clockwise.
    pub const ANGLE: &str = "angle";
    /// Horizontal / vertical skew in degrees.
    pub const SKEW_H: &str = "skew_h";
    pub const SKEW_V: &str = "skew_v";
    /// Nearest / Bilinear / Bicubic.
    pub const INTERPOLATION: &str = "interpolation";
    /// The warp preset ([`super::WARP_PRESET_LABELS`]).
    pub const WARP: &str = "warp";
    /// The warp preset's Bend, `-100..=100`.
    pub const BEND: &str = "bend";
    /// W10-J: Content-Aware Scale's Amount, `0..=100` percent: how much of
    /// the result is seam carving rather than a plain scale.
    pub const CA_AMOUNT: &str = "ca_amount";
    /// The edit counter: the options bar bumps it with every numeric edit,
    /// and the tool applies the numeric fields only when it has moved on.
    pub const NUMERIC_SEQ: &str = "numeric_seq";
    /// Not an option: the application's "apply what the bar holds now"
    /// signal. After forwarding the bar's keys to a LIVE session between
    /// presses, the shell sends `Bool(true)` under this key and the tool
    /// runs [`super::TransformTool::apply_pending_numeric`] — so a typed
    /// field or a picked Warp preset reshapes the quad on screen at once.
    pub const APPLY_NUMERIC: &str = "apply_numeric";
    /// Every key whose value is geometry, applied together on a
    /// [`NUMERIC_SEQ`] change.
    pub const GEOMETRY: &[&str] = &[
        REFERENCE,
        X,
        Y,
        W,
        H,
        ANGLE,
        SKEW_H,
        SKEW_V,
        WARP,
        BEND,
        NUMERIC_SEQ,
    ];
}

/// The reference-point grid, row-major from the top left, as the registry's
/// choice labels.
pub const REFERENCE_LABELS: &[&str] = &[
    "Top Left",
    "Top",
    "Top Right",
    "Left",
    "Centre",
    "Right",
    "Bottom Left",
    "Bottom",
    "Bottom Right",
];

/// The centre of the reference grid — the default reference point.
pub const REFERENCE_CENTRE: usize = 4;

/// The Interpolation choice labels, index for index with
/// [`INTERPOLATIONS`].
pub const INTERPOLATION_LABELS: &[&str] = &["Nearest Neighbour", "Bilinear", "Bicubic"];

/// The filters an Interpolation choice index names.
pub const INTERPOLATIONS: [Interpolation; 3] = [
    Interpolation::Nearest,
    Interpolation::Bilinear,
    Interpolation::Bicubic,
];

/// Where reference point `index` sits in the unit square of the box.
pub fn reference_uv(index: usize) -> Vec2 {
    let index = index.min(8);
    Vec2::new((index % 3) as f32 * 0.5, (index / 3) as f32 * 0.5)
}

/// The point at unit-square `uv` of a quad (bilinear, so the edge midpoints
/// and the centre of any quad are where the eye expects them).
pub fn quad_point(corners: &[Vec2; 4], uv: Vec2) -> Vec2 {
    let top = corners[0] + (corners[1] - corners[0]) * uv.x;
    let bottom = corners[3] + (corners[2] - corners[3]) * uv.x;
    top + (bottom - top) * uv.y
}

/// W9-L: Free Transform's numeric fields — Photopea's options bar while a
/// transform is live. The quad they describe is the source box scaled by
/// `w` / `h` percent, skewed, rotated by `angle` degrees clockwise about its
/// reference point, with the reference point landing on (`x`, `y`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericTransform {
    /// The reference point, an index into [`REFERENCE_LABELS`].
    pub reference: usize,
    pub x: f32,
    pub y: f32,
    /// Width, percent of the source box.
    pub w: f32,
    /// Height, percent of the source box.
    pub h: f32,
    /// Degrees, clockwise.
    pub angle: f32,
    /// Degrees.
    pub skew_h: f32,
    /// Degrees.
    pub skew_v: f32,
}

impl Default for NumericTransform {
    fn default() -> Self {
        Self {
            reference: REFERENCE_CENTRE,
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
            angle: 0.0,
            skew_h: 0.0,
            skew_v: 0.0,
        }
    }
}

/// The largest skew the fields accept: 90 degrees is a collapsed box.
pub const MAX_SKEW_DEG: f32 = 89.0;

impl NumericTransform {
    /// The linear part: rotate . skew . scale.
    fn linear(&self) -> glam::Mat2 {
        let scale = glam::Mat2::from_diagonal(Vec2::new(self.w / 100.0, self.h / 100.0));
        let skew = glam::Mat2::from_cols(
            Vec2::new(1.0, self.skew_v.to_radians().tan()),
            Vec2::new(self.skew_h.to_radians().tan(), 1.0),
        );
        glam::Mat2::from_angle(self.angle.to_radians()) * skew * scale
    }

    /// The document-space affine these fields put `source` through.
    pub fn affine(&self, source: PixelRect) -> Option<glam::Affine2> {
        let values = [
            self.x,
            self.y,
            self.w,
            self.h,
            self.angle,
            self.skew_h,
            self.skew_v,
        ];
        if values.iter().any(|v| !v.is_finite())
            || self.skew_h.abs() > MAX_SKEW_DEG
            || self.skew_v.abs() > MAX_SKEW_DEG
        {
            return None;
        }
        let m = self.linear();
        let det = m.determinant();
        if !det.is_finite() || det.abs() < 1e-6 {
            return None;
        }
        let uv = reference_uv(self.reference);
        let anchor = Vec2::new(
            source.x as f32 + uv.x * source.width as f32,
            source.y as f32 + uv.y * source.height as f32,
        );
        let translation = Vec2::new(self.x, self.y) - m * anchor;
        Some(glam::Affine2::from_mat2_translation(m, translation))
    }

    /// The destination quad these fields describe for `source`, or `None`
    /// when they collapse it (a zero size, a 90 degree skew, a non-finite
    /// value) — the caller keeps the quad it has.
    pub fn corners(&self, source: PixelRect) -> Option<[Vec2; 4]> {
        let a = self.affine(source)?;
        let (x0, y0) = (source.x as f32, source.y as f32);
        let (x1, y1) = (source.right() as f32, source.bottom() as f32);
        Some(
            [
                Vec2::new(x0, y0),
                Vec2::new(x1, y0),
                Vec2::new(x1, y1),
                Vec2::new(x0, y1),
            ]
            .map(|p| a.transform_point2(p)),
        )
    }

    /// What the options bar shows for `state` at `reference`: the quad's
    /// parallelogram part read back as scale, rotation and horizontal skew,
    /// split with the vertical skew the bar last typed
    /// ([`TransformState::skew_v`]), so typed values read back as typed and
    /// the fields always rebuild the quad they describe.
    pub fn read(state: &TransformState, reference: usize) -> NumericTransform {
        let reference = reference.min(8);
        let mut out = Self::decompose(state);
        out.reference = reference;
        let p = quad_point(&state.corners, reference_uv(reference));
        out.x = p.x;
        out.y = p.y;
        out
    }

    /// Scale, rotation and horizontal skew of the quad's parallelogram part
    /// (corners 0, 1 and 3), given its vertical skew `state.skew_v`.
    fn decompose(state: &TransformState) -> NumericTransform {
        let sw = (state.source.width as f32).max(1e-6);
        let sh = (state.source.height as f32).max(1e-6);
        let skew_v = if state.skew_v.is_finite() {
            state.skew_v.clamp(-MAX_SKEW_DEG, MAX_SKEW_DEG)
        } else {
            0.0
        };
        let tv = skew_v.to_radians().tan();
        let c = state.corners;
        // col0 = R * (sx, sx * tv): the rotation is col0's angle less the
        // skew's own.
        let col0 = (c[1] - c[0]) / sw;
        let col1 = (c[3] - c[0]) / sh;
        let angle = col0.y.atan2(col0.x) - tv.atan();
        let sx = col0.length() / (1.0 + tv * tv).sqrt();
        let q = glam::Mat2::from_angle(-angle) * col1;
        let sy = q.y;
        let skew_h = if sy.abs() > 1e-6 {
            (q.x / sy).atan().to_degrees()
        } else {
            0.0
        };
        NumericTransform {
            w: sx * 100.0,
            h: sy * 100.0,
            angle: angle.to_degrees(),
            skew_h,
            skew_v,
            ..NumericTransform::default()
        }
    }
}

/// W9-L: Photopea's warp presets, in the registry's order after "None".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpPreset {
    Arc,
    Arch,
    Bulge,
    Flag,
    Wave,
    Fish,
    Rise,
    Fisheye,
    Inflate,
    Squeeze,
    Twist,
}

/// The Warp choice labels: "None" (the mesh the handles make), then every
/// [`WarpPreset`] in [`WarpPreset::ALL`] order.
pub const WARP_PRESET_LABELS: &[&str] = &[
    "None", "Arc", "Arch", "Bulge", "Flag", "Wave", "Fish", "Rise", "Fisheye", "Inflate",
    "Squeeze", "Twist",
];

impl WarpPreset {
    pub const ALL: [WarpPreset; 11] = [
        WarpPreset::Arc,
        WarpPreset::Arch,
        WarpPreset::Bulge,
        WarpPreset::Flag,
        WarpPreset::Wave,
        WarpPreset::Fish,
        WarpPreset::Rise,
        WarpPreset::Fisheye,
        WarpPreset::Inflate,
        WarpPreset::Squeeze,
        WarpPreset::Twist,
    ];

    /// The preset a Warp choice index names; `None` for index 0 ("None")
    /// and past the end.
    pub fn from_choice(index: usize) -> Option<WarpPreset> {
        index.checked_sub(1).and_then(|i| Self::ALL.get(i).copied())
    }

    /// Where the control point at unit-square `(u, v)` of a `size` box
    /// moves, in pixels, for a bend of `b` in `-1..=1`.
    fn offset(self, u: f32, v: f32, b: f32, size: Vec2) -> Vec2 {
        use std::f32::consts::{FRAC_PI_2, TAU};
        // 0 at the two edges, 1 across the middle.
        let hump = |t: f32| 4.0 * t * (1.0 - t);
        let d = match self {
            WarpPreset::Arc => Vec2::new(0.0, -b * hump(u) * (0.5 - 0.25 * v)),
            WarpPreset::Arch => Vec2::new(0.0, -b * 0.5 * hump(u)),
            WarpPreset::Bulge => Vec2::new(0.0, -b * 0.5 * hump(u) * (1.0 - 2.0 * v)),
            WarpPreset::Flag => Vec2::new(0.0, b * 0.25 * (TAU * u).sin()),
            WarpPreset::Wave => Vec2::new(0.0, b * 0.25 * (TAU * u).sin() * (1.0 - v)),
            WarpPreset::Fish => Vec2::new(0.0, -b * (1.0 - 2.0 * v) * hump(u) * (1.0 - u)),
            WarpPreset::Rise => {
                let smooth = u * u * (3.0 - 2.0 * u);
                Vec2::new(0.0, b * 0.5 * (1.0 - 2.0 * smooth))
            }
            WarpPreset::Fisheye => {
                let c = Vec2::new(u - 0.5, v - 0.5);
                let g = (1.0 - c.length_squared() / 0.5).max(0.0);
                c * (b * g)
            }
            WarpPreset::Inflate => Vec2::new(
                b * 0.25 * (2.0 * u - 1.0) * hump(v),
                b * 0.25 * (2.0 * v - 1.0) * hump(u),
            ),
            WarpPreset::Squeeze => Vec2::new(
                -b * 0.25 * (2.0 * u - 1.0) * hump(v),
                b * 0.25 * (2.0 * v - 1.0) * hump(u),
            ),
            WarpPreset::Twist => {
                // Rotate about the centre, most at the centre and not at all
                // at the corners, in pixel space so the box keeps its aspect.
                let c = Vec2::new((u - 0.5) * size.x, (v - 0.5) * size.y);
                let r_max = (size * 0.5).length().max(1e-6);
                let a = b * FRAC_PI_2 * (1.0 - c.length() / r_max).max(0.0);
                let turned = glam::Mat2::from_angle(a) * c;
                return turned - c;
            }
        };
        d * size
    }
}

/// W9-L: the control mesh a warp preset gives the box `rect` at `bend`
/// percent (`-100..=100`). A bend of zero is the identity mesh.
pub fn warp_preset_mesh(preset: WarpPreset, rect: PixelRect, bend: f32) -> WarpMesh {
    let b = if bend.is_finite() {
        bend.clamp(-100.0, 100.0) / 100.0
    } else {
        0.0
    };
    let size = Vec2::new(rect.width as f32, rect.height as f32);
    let mut mesh = WarpMesh::identity(rect);
    for (r, row) in mesh.points.iter_mut().enumerate() {
        for (c, p) in row.iter_mut().enumerate() {
            let (u, v) = (c as f32 / 3.0, r as f32 / 3.0);
            *p += preset.offset(u, v, b, size);
        }
    }
    mesh
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
    /// W5-C: the session began over a pixel selection, so the commit floats
    /// the selected pixels (Photopea) instead of moving the whole layer, and
    /// the whole-layer preview lens stays off.
    floating: bool,
    /// W9-L: the numeric fields as the options bar last sent them, applied
    /// together by [`TransformTool::apply_pending_numeric`].
    pub numeric: NumericTransform,
    /// W9-L: Link — W and H move together.
    pub link: bool,
    /// W9-L: the options bar's Interpolation, handed to every session.
    pub interpolation: Interpolation,
    /// W9-L: the options bar's Warp choice (0 = None) and its Bend.
    pub warp: usize,
    pub bend: f32,
    /// W10-J: Content-Aware Scale's Amount, `0..=100` ([`keys::CA_AMOUNT`]).
    pub ca_amount: f32,
    /// W9-L: the options bar's edit counter, and the value of it the live
    /// session has already applied (or began at).
    numeric_seq: i32,
    numeric_seen: i32,
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
            floating: false,
            numeric: NumericTransform::default(),
            link: false,
            interpolation: Interpolation::Bicubic,
            warp: 0,
            bend: 50.0,
            ca_amount: 100.0,
            numeric_seq: 0,
            numeric_seen: 0,
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
        // W9-L: a new session starts from its own box, never from the
        // numbers typed into the last one.
        self.numeric_seen = self.numeric_seq;
        Ok(())
    }

    /// W9-L: apply the options bar's numeric fields to the live session,
    /// once per edit: when the edit counter differs from the value this
    /// session last applied (or began at). Any change counts, not only an
    /// increase: the options-bar Reset puts the held counter back to 0
    /// while a live session still holds the last number. The fields are absolute, so
    /// applying them is idempotent; the counter is what stops a press on
    /// the canvas from re-applying numbers a handle drag has since moved
    /// away from. Returns `true` when the quad changed.
    ///
    /// Link: when exactly one of W and H differs from the quad's current
    /// read-back, the other follows in proportion.
    ///
    /// A Warp preset (any choice but None) replaces the mesh with the
    /// preset's, carried through the same affine, and switches the session
    /// to Warp mode so the commit resamples through it.
    pub fn apply_pending_numeric(&mut self) -> bool {
        let seq = self.numeric_seq;
        let fresh = seq != self.numeric_seen;
        self.numeric_seen = seq;
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        if !fresh {
            return false;
        }
        let mut n = self.numeric;
        n.reference = n.reference.min(8);
        if self.link {
            let now = NumericTransform::read(state, n.reference);
            let w_moved = (n.w - now.w).abs() > 1e-3;
            let h_moved = (n.h - now.h).abs() > 1e-3;
            if w_moved && !h_moved && now.w.abs() > 1e-6 {
                n.h = now.h * n.w / now.w;
            } else if h_moved && !w_moved && now.h.abs() > 1e-6 {
                n.w = now.w * n.h / now.h;
            }
        }
        let (Some(corners), Some(affine)) = (n.corners(state.source), n.affine(state.source))
        else {
            return false;
        };
        if quad_signed_area(corners).abs() < 1e-4 {
            return false;
        }
        state.corners = corners;
        state.pivot = Vec2::new(n.x, n.y);
        state.skew_v = n.skew_v;
        if let Some(preset) = WarpPreset::from_choice(self.warp) {
            let mut mesh = warp_preset_mesh(preset, state.source, self.bend);
            for row in mesh.points.iter_mut() {
                for p in row.iter_mut() {
                    *p = affine.transform_point2(*p);
                }
            }
            state.mesh = Some(mesh);
            self.mode = TransformMode::Warp;
        }
        true
    }

    /// W5-C: start a session from the context alone, before any pointer
    /// contact: over the explicit pixel selection, else the active layer's
    /// ink. What a canvas press does when no session is live, and what
    /// Edit > Free Transform (Ctrl+T) does straight away so the handles are
    /// on screen before the first click.
    pub fn begin_from_context(&mut self, ctx: &ToolContext<'_>) -> Result<(), ToolError> {
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
                let x1 = (inked.x + inked.width as i64).min(ctx.canvas.x + ctx.canvas.width as i64);
                let y1 =
                    (inked.y + inked.height as i64).min(ctx.canvas.y + ctx.canvas.height as i64);
                PixelRect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
            }
        };
        self.begin(src)?;
        // W5-C: a session over a selection floats its pixels at commit.
        self.floating = !self.selection_only && ctx.selection.bounds().is_some();
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

        // W10-J: Content-Aware Scale retargets the whole layer into the box.
        if self.mode == TransformMode::ContentAware {
            let result = self.commit_content_aware(ctx, &state);
            self.state = None;
            self.grabbed = None;
            self.floating = false;
            return result;
        }

        // W5-C: a pixel selection floats. The selected pixels are lifted,
        // carried through the session and laid back over the rest of the
        // layer, the selection travelling with them: ONE transaction. A
        // parametric layer keeps its whole-layer transform (floating part of
        // a text layer would rasterize it), and a mask target keeps the
        // coverage path below.
        if ctx.paint_target == PaintTarget::Layer
            && !ctx.active_layer_parametric
            && ctx.selection.bounds().is_some()
            && !selection_covers_all_ink(ctx)
        {
            let command =
                float_selection_with(ctx, &state, self.mode, "Free Transform", self.interpolation);
            self.state = None;
            self.grabbed = None;
            self.floating = false;
            ctx.emit(command?);
            return Ok(());
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
                // W7-C: a 16-bit layer is resampled at 16 bits.
                let mut patch = ColorPatch::load_native(ctx.tiles, key, rect)?;
                let src = patch.buffer().clone();
                let out = resample_with(&src, patch.rect(), &state, self.mode, self.interpolation)?;
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
                let out = resample_with(&src, patch.rect(), &state, self.mode, self.interpolation)?;
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

impl TransformTool {
    /// W10-J: the Content-Aware Scale commit: the layer's source box seam
    /// carved into the upright box the handles left, at the session's Amount,
    /// as one `PaintTiles` step. A mask target and a parametric layer (text,
    /// shape, smart object) are refused rather than rasterised.
    fn commit_content_aware(
        &self,
        ctx: &mut ToolContext<'_>,
        state: &TransformState,
    ) -> Result<(), ToolError> {
        if ctx.paint_target != PaintTarget::Layer {
            return Err(ToolError::UnsupportedOnMask);
        }
        if ctx.active_layer_parametric {
            return Err(ToolError::NonAffineParametric);
        }
        let dest = content_aware_dest(state).ok_or(ToolError::Degenerate)?;
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let rect = union_unclipped(state.source, dest).ok_or(ToolError::Degenerate)?;
        let mut patch = ColorPatch::load_native(ctx.tiles, key, rect)?;
        let src = patch.buffer().clone();
        let out = content_aware_resample(&src, rect, state.source, dest, self.ca_amount / 100.0)?;
        patch.replace(out)?;
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }
}

/// W10-J: the upright box a Content-Aware Scale session lands in: the
/// corners' bounding box, rounded to whole pixels. `None` when it has no area
/// or a corner is not finite.
pub fn content_aware_dest(state: &TransformState) -> Option<PixelRect> {
    if !state.corners.iter().all(|c| c.is_finite()) {
        return None;
    }
    let lo = state
        .corners
        .iter()
        .copied()
        .fold(Vec2::INFINITY, Vec2::min);
    let hi = state
        .corners
        .iter()
        .copied()
        .fold(Vec2::NEG_INFINITY, Vec2::max);
    let (x0, y0) = (lo.x.round() as i64, lo.y.round() as i64);
    let (x1, y1) = (hi.x.round() as i64, hi.y.round() as i64);
    (x1 > x0 && y1 > y0).then(|| PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// W10-J: Content-Aware Scale over one plane. `src` lives on `patch_rect`;
/// the pixels of `source` (document space) are cleared and laid back down
/// retargeted to `dest`: seam carved ([`filters::content_aware_scale`]),
/// blended with a plain bilinear scale of the same box by `1 - amount`
/// (`amount` in `0..=1`; 1 is all seam carving). Everything outside `source`
/// and `dest` is left exactly as it was.
pub fn content_aware_resample(
    src: &FilterBuffer,
    patch_rect: PixelRect,
    source: PixelRect,
    dest: PixelRect,
    amount: f32,
) -> Result<FilterBuffer, ToolError> {
    if source.is_empty() || dest.is_empty() || !amount.is_finite() {
        return Err(ToolError::Degenerate);
    }
    let amount = amount.clamp(0.0, 1.0);
    let (w, h) = (src.width(), src.height());
    let local = |x: i64, y: i64| -> Option<(u32, u32)> {
        let (lx, ly) = (x - patch_rect.x, y - patch_rect.y);
        (lx >= 0 && ly >= 0 && lx < i64::from(w) && ly < i64::from(h))
            .then_some((lx as u32, ly as u32))
    };
    let mut region = FilterBuffer::transparent(source.width, source.height)?;
    for y in 0..source.height {
        for x in 0..source.width {
            if let Some((lx, ly)) = local(source.x + i64::from(x), source.y + i64::from(y)) {
                region.set(x, y, src.get(lx, ly));
            }
        }
    }
    let carved = if amount > 0.0 {
        Some(
            filters::content_aware_scale(&region, dest.width, dest.height)
                .map_err(|_| ToolError::Degenerate)?,
        )
    } else {
        None
    };
    let sampling = Sampling::new(EdgeMode::Clamp, Interpolation::Bilinear);
    let (sx, sy) = (
        source.width as f32 / dest.width as f32,
        source.height as f32 / dest.height as f32,
    );
    let mut out = src.clone();
    for y in source.y..source.bottom() {
        for x in source.x..source.right() {
            if let Some((lx, ly)) = local(x, y) {
                out.set(lx, ly, [0.0; 4]);
            }
        }
    }
    for y in 0..dest.height {
        for x in 0..dest.width {
            let Some((lx, ly)) = local(dest.x + i64::from(x), dest.y + i64::from(y)) else {
                continue;
            };
            let carve = carved.as_ref().map(|c| c.get(x, y));
            let px = if amount >= 1.0 {
                carve.unwrap_or([0.0; 4])
            } else {
                let plain = region.sample(
                    (x as f32 + 0.5) * sx - 0.5,
                    (y as f32 + 0.5) * sy - 0.5,
                    sampling,
                );
                let carve = carve.unwrap_or(plain);
                std::array::from_fn(|i| carve[i] * amount + plain[i] * (1.0 - amount))
            };
            out.set(lx, ly, px);
        }
    }
    Ok(out)
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

/// W5-C: `true` when `m` is (numerically) the identity map.
fn is_identity_affine(m: &glam::Affine2) -> bool {
    m.abs_diff_eq(glam::Affine2::IDENTITY, 1e-5)
}

/// W5-C: the session state carried from document space into the layer's own
/// pixel space through `to_layer` (document -> layer pixels, the shell's
/// `sample_to_layer`). The document map `H` (source corners -> corners) is
/// conjugated to `M * H * M^-1` and re-anchored on the axis-aligned box that
/// covers the mapped source, so the resampler reads the layer's real tiles.
/// Warp is exact only while `M` keeps axes (translate/scale); a rotated layer
/// refuses the warp float rather than bending the wrong pixels.
fn state_in_layer_space(
    state: &TransformState,
    mode: TransformMode,
    to_layer: glam::Affine2,
) -> Result<(TransformState, TransformMode), ToolError> {
    if is_identity_affine(&to_layer) {
        return Ok((state.clone(), mode));
    }
    let map = |p: Vec2| to_layer.transform_point2(p);
    let src = state.source_corners().map(map);
    let lo = src
        .iter()
        .fold(Vec2::splat(f32::INFINITY), |a, b| a.min(*b));
    let hi = src
        .iter()
        .fold(Vec2::splat(f32::NEG_INFINITY), |a, b| a.max(*b));
    if !lo.is_finite() || !hi.is_finite() {
        return Err(ToolError::not_invertible());
    }
    let x0 = lo.x.floor() as i64;
    let y0 = lo.y.floor() as i64;
    let x1 = (hi.x.ceil() as i64).max(x0 + 1);
    let y1 = (hi.y.ceil() as i64).max(y0 + 1);
    let source = PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
    let mut out = TransformState::new(source);
    out.pivot = map(state.pivot);
    out.skew_v = state.skew_v;
    match state.mesh.filter(|_| mode == TransformMode::Warp) {
        Some(mesh) => {
            let m = to_layer.matrix2;
            if m.x_axis.y.abs() > 1e-5 || m.y_axis.x.abs() > 1e-5 {
                return Err(ToolError::not_invertible());
            }
            let mut points = mesh.points;
            for row in points.iter_mut() {
                for p in row.iter_mut() {
                    *p = map(*p);
                }
            }
            out.mesh = Some(WarpMesh { points });
            out.corners = [points[0][0], points[0][3], points[3][3], points[3][0]];
            Ok((out, TransformMode::Warp))
        }
        None => {
            let h = Homography::from_quads(state.source_corners(), state.corners)
                .ok_or_else(ToolError::not_invertible)?;
            let back = to_layer.inverse();
            let mut corners = out.corners;
            for c in corners.iter_mut() {
                let doc = back.transform_point2(*c);
                let moved = h.apply(doc).ok_or_else(ToolError::not_invertible)?;
                *c = map(moved);
            }
            out.corners = corners;
            // Every non-warp mode resamples through the same homography
            // branch; Distort names "four free corners", which the
            // conjugated quad is.
            Ok((out, TransformMode::Distort))
        }
    }
}

/// W5-C: the selection carried through the same session: an affine
/// resample of the mask for Scale/Rotate/Skew (the Transform Selection
/// path), and the coverage plane pushed through the pixel resampler for the
/// projective modes and Warp, so the marching ants land exactly where the
/// floated pixels did.
fn transformed_selection(
    selection: &Selection,
    canvas: PixelRect,
    state: &TransformState,
    mode: TransformMode,
) -> Result<Selection, ToolError> {
    let canvas_rect = selection::rect::Rect::from_xywh(
        canvas.x as i32,
        canvas.y as i32,
        canvas.width,
        canvas.height,
    );
    if matches!(
        mode,
        TransformMode::Scale | TransformMode::Rotate | TransformMode::Skew
    ) {
        let xf = quad_affine(state.source_corners(), state.corners)
            .ok_or_else(ToolError::not_invertible)?;
        return Ok(transform_selection(
            selection,
            canvas_rect,
            xf,
            selection::transform::ResampleFilter::Bilinear,
        )?);
    }
    let dest = state
        .dest_bounds_unclipped(mode)
        .ok_or(ToolError::Degenerate)?;
    let rect = union_unclipped(state.source, dest).ok_or(ToolError::Degenerate)?;
    // The same allocation cap every patch obeys.
    crate::patch::TileBox::covering(rect)?;
    let mut plane = FilterBuffer::transparent(rect.width, rect.height)?;
    for y in state.source.y..state.source.bottom() {
        for x in state.source.x..state.source.right() {
            let c = selection.coverage_at(IVec2::new(x as i32, y as i32));
            if c > 0.0 {
                plane.set((x - rect.x) as u32, (y - rect.y) as u32, [c; 4]);
            }
        }
    }
    let moved = resample(&plane, rect, state, mode)?;
    let coverage: Vec<u8> = moved
        .pixels()
        .iter()
        .map(|p| (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect();
    let mask = editor_core::SelectionMask::new(
        IVec2::new(rect.x as i32, rect.y as i32),
        rect.width,
        rect.height,
        coverage,
    )?;
    Ok(Selection::Mask(mask))
}

/// A hard-edged rectangular selection that contains every inked pixel of the
/// active layer selects the whole layer: floating it would resample pixels
/// for a result the lossless whole-layer transform gives exactly (card 035),
/// so the commit keeps the layer transform. A partial or soft selection floats.
fn selection_covers_all_ink(ctx: &ToolContext<'_>) -> bool {
    let Selection::Rect { min, max } = ctx.selection else {
        return false;
    };
    let Some(ink) = ctx.active_layer_ink_bounds else {
        return false;
    };
    let (ink_max_x, ink_max_y) = (ink.x + i64::from(ink.width), ink.y + i64::from(ink.height));
    i64::from(min.x) <= ink.x
        && i64::from(min.y) <= ink.y
        && i64::from(max.x) >= ink_max_x
        && i64::from(max.y) >= ink_max_y
}

/// W5-C: Photopea's floating selection. The selected pixels of the active
/// layer are LIFTED (weighted by the selection's coverage), carried through
/// `state` with the patch resampler, and laid back OVER what the lift left
/// behind: every pixel the selection did not cover stays byte-identical.
/// The selection travels with them. Both land as ONE undoable transaction
/// (`label` names it in History). Used by the free transform's commit and by
/// the Move tool's drag when a selection is active.
pub fn float_selection(
    ctx: &mut ToolContext<'_>,
    state: &TransformState,
    mode: TransformMode,
    label: &str,
) -> Result<Command, ToolError> {
    float_selection_with(ctx, state, mode, label, Interpolation::Bicubic)
}

/// W9-L: [`float_selection`] resampled through a chosen filter — Free
/// Transform's Interpolation.
pub fn float_selection_with(
    ctx: &mut ToolContext<'_>,
    state: &TransformState,
    mode: TransformMode,
    label: &str,
    interpolation: Interpolation,
) -> Result<Command, ToolError> {
    let selection = ctx.selection.clone();
    if selection.bounds().is_none() {
        return Err(ToolError::Degenerate);
    }
    ctx.require_layer_target()?;
    if let Some(layer) = ctx.active_layer {
        if ctx.layer_lock(layer) == Some(true) {
            return Err(ToolError::LayerLocked);
        }
    }
    let target = ctx.pixel_target()?;
    let key = ctx.pixel_key()?;
    let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
    let from_layer = to_layer.inverse();
    let (layer_state, layer_mode) = state_in_layer_space(state, mode, to_layer)?;
    let dest = layer_state
        .dest_bounds_unclipped(layer_mode)
        .ok_or(ToolError::Degenerate)?;
    let rect = union_unclipped(layer_state.source, dest).ok_or(ToolError::Degenerate)?;
    // W7-C: a 16-bit layer floats and lands at 16 bits.
    let mut patch = ColorPatch::load_native(ctx.tiles, key, rect)?;
    let prect = patch.rect();
    let src = patch.buffer().clone();
    let (w, h) = (src.width(), src.height());
    // Split the plane: `lifted` carries the selected share of every pixel,
    // `rest` what the lift leaves behind. Premultiplied, so one scale per
    // pixel splits colour and alpha together.
    let mut lifted = FilterBuffer::transparent(w, h)?;
    let mut rest = src.clone();
    let mut any = false;
    let src_box = layer_state.source;
    for y in src_box.y.max(prect.y)..src_box.bottom().min(prect.bottom()) {
        for x in src_box.x.max(prect.x)..src_box.right().min(prect.right()) {
            let doc = from_layer.transform_point2(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
            let c = selection.coverage_at(IVec2::new(doc.x.floor() as i32, doc.y.floor() as i32));
            if c <= 0.0 {
                continue;
            }
            let (lx, ly) = ((x - prect.x) as u32, (y - prect.y) as u32);
            let p = src.get(lx, ly);
            lifted.set(lx, ly, p.map(|v| v * c));
            rest.set(lx, ly, p.map(|v| v * (1.0 - c)));
            any = true;
        }
    }
    let mut commands = Vec::new();
    if any {
        let moved = resample_with(&lifted, prect, &layer_state, layer_mode, interpolation)?;
        let mut out = rest;
        for (o, m) in out.pixels_mut().iter_mut().zip(moved.pixels()) {
            let keep = 1.0 - m[3].clamp(0.0, 1.0);
            for c in 0..4 {
                o[c] = m[c] + o[c] * keep;
            }
        }
        patch.replace(out)?;
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            commands.push(Command::PaintTiles { target, delta });
        }
    }
    let next = transformed_selection(&selection, ctx.canvas, state, mode)?;
    commands.push(Command::SetSelection { selection: next });
    Ok(Command::Transaction {
        label: label.to_string(),
        commands,
    })
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

    /// `mode` (and the historical `target`) go through [`Tool::set_choice`];
    /// W9-L adds the numeric options bar. Interpolation and Link take effect
    /// at once; the geometry keys ([`keys::GEOMETRY`]) are held and applied
    /// together by [`TransformTool::apply_pending_numeric`] — at the next
    /// press, at the commit, or on [`keys::APPLY_NUMERIC`], which the shell
    /// sends every frame a session is live between presses.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        let mismatch = || {
            Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            })
        };
        let float = |v: f32| crate::error::finite("transform field", v);
        match (key, setting) {
            ("mode" | "target", ToolSetting::Choice(i)) => self.set_choice(key, i),
            (keys::REFERENCE, ToolSetting::Choice(i)) if i < REFERENCE_LABELS.len() => {
                self.numeric.reference = i
            }
            (keys::X, ToolSetting::Float(v)) => self.numeric.x = float(v)?,
            (keys::Y, ToolSetting::Float(v)) => self.numeric.y = float(v)?,
            (keys::W, ToolSetting::Float(v)) => self.numeric.w = float(v)?,
            (keys::H, ToolSetting::Float(v)) => self.numeric.h = float(v)?,
            (keys::ANGLE, ToolSetting::Float(v)) => self.numeric.angle = float(v)?,
            (keys::SKEW_H, ToolSetting::Float(v)) => {
                self.numeric.skew_h = float(v)?.clamp(-MAX_SKEW_DEG, MAX_SKEW_DEG)
            }
            (keys::SKEW_V, ToolSetting::Float(v)) => {
                self.numeric.skew_v = float(v)?.clamp(-MAX_SKEW_DEG, MAX_SKEW_DEG)
            }
            (keys::LINK, ToolSetting::Bool(v)) => self.link = v,
            (keys::INTERPOLATION, ToolSetting::Choice(i)) if i < INTERPOLATIONS.len() => {
                self.interpolation = INTERPOLATIONS[i];
            }
            (keys::WARP, ToolSetting::Choice(i)) if i < WARP_PRESET_LABELS.len() => self.warp = i,
            (keys::BEND, ToolSetting::Float(v)) => self.bend = float(v)?.clamp(-100.0, 100.0),
            (keys::CA_AMOUNT, ToolSetting::Float(v)) => {
                self.ca_amount = float(v)?.clamp(0.0, 100.0)
            }
            (keys::NUMERIC_SEQ, ToolSetting::Int(v)) => self.numeric_seq = v,
            (keys::APPLY_NUMERIC, ToolSetting::Bool(true)) => {
                self.apply_pending_numeric();
            }
            (keys::APPLY_NUMERIC, ToolSetting::Bool(false)) => {}
            (
                "mode"
                | "target"
                | keys::REFERENCE
                | keys::X
                | keys::Y
                | keys::W
                | keys::H
                | keys::ANGLE
                | keys::SKEW_H
                | keys::SKEW_V
                | keys::LINK
                | keys::INTERPOLATION
                | keys::WARP
                | keys::BEND
                | keys::CA_AMOUNT
                | keys::NUMERIC_SEQ
                | keys::APPLY_NUMERIC,
                _,
            ) => return mismatch(),
            _ => {
                return Err(ToolError::UnknownOption {
                    key: key.to_owned(),
                })
            }
        }
        Ok(())
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.state.is_none() {
            self.begin_from_context(ctx)?;
        }
        // W9-L: numbers typed since the last press land before the hit test,
        // so the press grabs the quad the user is looking at.
        self.apply_pending_numeric();
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
        self.floating = false;
    }

    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        // W9-L: Enter commits the numbers the options bar holds.
        self.apply_pending_numeric();
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
            // W5-C: a floating selection moves only its pixels; naming the
            // layer would lens the WHOLE layer as the preview.
            layer: if self.floating { None } else { self.layer },
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

/// W9-L: the numeric options bar, Link, Interpolation and the warp presets,
/// driven the way the shell drives Free Transform: the registry builds the
/// tool, the options bar's values arrive through `set_setting` under the
/// registry's keys, and the live session is read back through
/// `live_geometry` — what the shell publishes to the canvas and the bar.
#[cfg(test)]
mod w9l_tests {
    use super::*;
    use crate::registry;
    use crate::tiles::MemoryTiles;
    use editor_core::PixelKey;

    const SIDE: u32 = 200;

    fn live(tool: &dyn Tool) -> (TransformState, TransformMode) {
        match tool.live_geometry() {
            Some(SessionGeometry::Transform { state, mode, .. }) => (state, mode),
            other => panic!("no live transform: {other:?}"),
        }
    }

    fn near(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-2
    }

    /// The whole set the options bar writes for one edit.
    fn numeric_settings(n: NumericTransform, seq: i32) -> Vec<(&'static str, ToolSetting)> {
        vec![
            (keys::REFERENCE, ToolSetting::Choice(n.reference)),
            (keys::X, ToolSetting::Float(n.x)),
            (keys::Y, ToolSetting::Float(n.y)),
            (keys::W, ToolSetting::Float(n.w)),
            (keys::H, ToolSetting::Float(n.h)),
            (keys::ANGLE, ToolSetting::Float(n.angle)),
            (keys::SKEW_H, ToolSetting::Float(n.skew_h)),
            (keys::SKEW_V, ToolSetting::Float(n.skew_v)),
            (keys::NUMERIC_SEQ, ToolSetting::Int(seq)),
        ]
    }

    fn send(tool: &mut dyn Tool, settings: &[(&'static str, ToolSetting)]) {
        for (key, value) in settings {
            tool.set_setting(key, *value)
                .unwrap_or_else(|e| panic!("refused {key}: {e}"));
        }
    }

    /// A press (and release) well off the quad: it grabs nothing.
    fn press(tool: &mut dyn Tool, ctx: &mut ToolContext<'_>) {
        tool.on_pointer_down(ctx, PointerEvent::at(900.0, 900.0))
            .unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(900.0, 900.0))
            .unwrap();
    }

    #[test]
    fn the_registry_declares_every_numeric_key_the_tool_answers() {
        let info = registry::info(ToolId::FreeTransform).unwrap();
        for key in keys::GEOMETRY
            .iter()
            .chain([keys::LINK, keys::INTERPOLATION].iter())
        {
            assert!(
                info.options.iter().any(|o| o.key == *key),
                "Free Transform declares no {key}"
            );
        }
    }

    #[test]
    fn typed_fields_set_the_live_quad_once_per_edit_and_never_a_new_session() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, SIDE, SIDE));
        let mut tool = registry::make(ToolId::FreeTransform);
        press(&mut *tool, &mut ctx);
        let (state, _) = live(&*tool);
        assert_eq!(state.corners, state.source_corners(), "a fresh box");

        // Half size, a quarter turn clockwise, centred on (100, 100).
        let typed = NumericTransform {
            x: 100.0,
            y: 100.0,
            w: 50.0,
            h: 50.0,
            angle: 90.0,
            ..NumericTransform::default()
        };
        send(&mut *tool, &numeric_settings(typed, 1));
        press(&mut *tool, &mut ctx);
        let (state, _) = live(&*tool);
        // The source's top-left, (-50, -50) from the centre at half size,
        // turned a quarter clockwise in a y-down document: (+50, -50).
        assert!(near(state.corners[0], Vec2::new(150.0, 50.0)), "{state:?}");
        assert!(near(state.corners[2], Vec2::new(50.0, 150.0)), "{state:?}");
        // The bar reads back exactly what was typed.
        let back = NumericTransform::read(&state, REFERENCE_CENTRE);
        assert!((back.angle - 90.0).abs() < 1e-3 && (back.w - 50.0).abs() < 1e-3);

        // A value held with the SAME counter is not re-applied by a press:
        // the counter is what says "the user edited".
        tool.set_setting(keys::X, ToolSetting::Float(0.0)).unwrap();
        press(&mut *tool, &mut ctx);
        assert_eq!(live(&*tool).0.corners, state.corners);
        // The next edit moves the counter, and the whole set lands.
        send(
            &mut *tool,
            &numeric_settings(NumericTransform { x: 0.0, ..typed }, 2),
        );
        press(&mut *tool, &mut ctx);
        let moved = live(&*tool).0;
        assert!(near(
            quad_point(&moved.corners, reference_uv(4)),
            Vec2::new(0.0, 100.0)
        ));

        // Enter commits the numbers even with no press in between.
        send(
            &mut *tool,
            &numeric_settings(NumericTransform { x: 40.0, ..typed }, 3),
        );
        let pending = live(&*tool).0;
        assert!(
            near(pending.corners[0], moved.corners[0]),
            "held until used"
        );
        // The commit applies them first (here it then refuses: no layer).
        assert!(matches!(
            tool.commit(&mut ctx),
            Err(ToolError::NoActiveLayer)
        ));
        let committed = live(&*tool).0;
        assert!(near(
            quad_point(&committed.corners, reference_uv(4)),
            Vec2::new(40.0, 100.0)
        ));

        // A new session starts from its own box, not from the last numbers.
        tool.cancel(&mut ctx);
        press(&mut *tool, &mut ctx);
        let fresh = live(&*tool).0;
        assert_eq!(fresh.corners, fresh.source_corners());
    }

    #[test]
    fn link_keeps_w_and_h_in_proportion() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, SIDE, SIDE));
        let mut tool = registry::make(ToolId::FreeTransform);
        press(&mut *tool, &mut ctx);
        let now = NumericTransform::read(&live(&*tool).0, REFERENCE_CENTRE);
        tool.set_setting(keys::LINK, ToolSetting::Bool(true))
            .unwrap();
        // Only W was edited; H still reads what the quad shows (100).
        send(
            &mut *tool,
            &numeric_settings(NumericTransform { w: 40.0, ..now }, 1),
        );
        press(&mut *tool, &mut ctx);
        let back = NumericTransform::read(&live(&*tool).0, REFERENCE_CENTRE);
        assert!((back.w - 40.0).abs() < 1e-3, "{back:?}");
        assert!((back.h - 40.0).abs() < 1e-3, "H followed W: {back:?}");
        // Unlinked, H stays.
        tool.set_setting(keys::LINK, ToolSetting::Bool(false))
            .unwrap();
        send(
            &mut *tool,
            &numeric_settings(NumericTransform { w: 80.0, ..back }, 2),
        );
        press(&mut *tool, &mut ctx);
        let back = NumericTransform::read(&live(&*tool).0, REFERENCE_CENTRE);
        assert!((back.w - 80.0).abs() < 1e-3 && (back.h - 40.0).abs() < 1e-3);
    }

    #[test]
    fn an_edit_counter_that_went_back_down_still_applies() {
        // The options-bar Reset puts the held counter back to 0 while this
        // session still holds the last number: the next edit arrives with
        // a LOWER counter and must still land.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, SIDE, SIDE));
        let mut tool = registry::make(ToolId::FreeTransform);
        press(&mut *tool, &mut ctx);
        let now = NumericTransform::read(&live(&*tool).0, REFERENCE_CENTRE);
        send(
            &mut *tool,
            &numeric_settings(NumericTransform { w: 40.0, ..now }, 5),
        );
        tool.set_setting(keys::APPLY_NUMERIC, ToolSetting::Bool(true))
            .unwrap();
        let back = NumericTransform::read(&live(&*tool).0, REFERENCE_CENTRE);
        assert!((back.w - 40.0).abs() < 1e-3, "{back:?}");
        send(
            &mut *tool,
            &numeric_settings(NumericTransform { w: 70.0, ..back }, 1),
        );
        tool.set_setting(keys::APPLY_NUMERIC, ToolSetting::Bool(true))
            .unwrap();
        let back = NumericTransform::read(&live(&*tool).0, REFERENCE_CENTRE);
        assert!(
            (back.w - 70.0).abs() < 1e-3,
            "the lower counter applied: {back:?}"
        );
    }

    #[test]
    fn the_read_back_is_the_typed_skew_and_rebuilds_any_dragged_parallelogram() {
        let source = PixelRect::new(0, 0, 100, 50);
        let typed = NumericTransform {
            reference: 0,
            x: 10.0,
            y: 20.0,
            w: 150.0,
            h: 80.0,
            angle: 30.0,
            skew_h: 10.0,
            skew_v: 15.0,
        };
        let mut state = TransformState::new(source);
        state.corners = typed.corners(source).unwrap();
        state.skew_v = typed.skew_v;
        let back = NumericTransform::read(&state, 0);
        assert!((back.skew_v - 15.0).abs() < 1e-3, "{back:?}");
        assert!(near(Vec2::new(back.x, back.y), Vec2::new(10.0, 20.0)));
        // At the bottom-right reference, X / Y are that corner.
        let br = NumericTransform::read(&state, 8);
        assert!(near(Vec2::new(br.x, br.y), state.corners[2]));
        // Split with no vertical skew, the read-back still rebuilds the
        // same quad.
        state.skew_v = 0.0;
        let dec = NumericTransform::read(&state, 0);
        let rebuilt = dec.corners(source).unwrap();
        for (a, b) in rebuilt.iter().zip(state.corners.iter()) {
            assert!(near(*a, *b), "{rebuilt:?} vs {:?}", state.corners);
        }
        // A collapsed size or a 90 degree skew is refused, not applied.
        assert!(NumericTransform { w: 0.0, ..typed }
            .corners(source)
            .is_none());
        assert!(NumericTransform {
            skew_h: 90.0,
            ..typed
        }
        .corners(source)
        .is_none());
    }

    #[test]
    fn every_warp_preset_bends_the_mesh_its_own_way() {
        let rect = PixelRect::new(0, 0, 120, 60);
        let identity = WarpMesh::identity(rect);
        assert_eq!(WARP_PRESET_LABELS.len(), WarpPreset::ALL.len() + 1);
        let meshes: Vec<WarpMesh> = WarpPreset::ALL
            .iter()
            .map(|p| warp_preset_mesh(*p, rect, 50.0))
            .collect();
        for (i, (preset, mesh)) in WarpPreset::ALL.iter().zip(&meshes).enumerate() {
            assert_ne!(*mesh, identity, "{preset:?} does nothing");
            assert_eq!(
                warp_preset_mesh(*preset, rect, 0.0),
                identity,
                "{preset:?} at bend 0"
            );
            assert_ne!(
                warp_preset_mesh(*preset, rect, -50.0),
                *mesh,
                "{preset:?} ignores the bend's sign"
            );
            for (other, m) in WarpPreset::ALL.iter().zip(&meshes).skip(i + 1) {
                assert_ne!(mesh, m, "{preset:?} and {other:?} are the same warp");
            }
            assert_eq!(WarpPreset::from_choice(i + 1), Some(*preset));
            assert_eq!(
                WARP_PRESET_LABELS[i + 1],
                format!("{preset:?}"),
                "label order"
            );
        }
        assert_eq!(WarpPreset::from_choice(0), None);
    }

    #[test]
    fn picking_a_warp_preset_warps_the_live_session_and_the_commit_resamples_it() {
        let mut tiles = MemoryTiles::new();
        let layer = layer_model::LayerId::new();
        let key = PixelKey::Layer(layer);
        // A 64x64 layer, a white band across rows 24..40 on black.
        let ts = raster::TILE_SIZE as usize;
        let mut data = vec![0u8; ts * ts * 4];
        for y in 0..64usize {
            for x in 0..64usize {
                let v = if (24..40).contains(&y) { 255 } else { 0 };
                data[(y * ts + x) * 4..(y * ts + x) * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        tiles.put(key, raster::TileCoord::new(0, 0, 0), data);
        let before = tiles.pixel(key, 32, 12);
        let mut tool = registry::make(ToolId::FreeTransform);
        let commands = {
            let mut ctx =
                ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64)).with_layer(layer);
            press(&mut *tool, &mut ctx);
            let now = NumericTransform::read(&live(&*tool).0, REFERENCE_CENTRE);
            let arch = WARP_PRESET_LABELS
                .iter()
                .position(|l| *l == "Arch")
                .unwrap();
            tool.set_setting(keys::WARP, ToolSetting::Choice(arch))
                .unwrap();
            tool.set_setting(keys::BEND, ToolSetting::Float(60.0))
                .unwrap();
            send(&mut *tool, &numeric_settings(now, 1));
            press(&mut *tool, &mut ctx);
            let (state, mode) = live(&*tool);
            assert_eq!(
                mode,
                TransformMode::Warp,
                "a preset puts the session on Warp"
            );
            assert_eq!(
                state.mesh,
                Some(warp_preset_mesh(WarpPreset::Arch, state.source, 60.0))
            );
            tool.commit(&mut ctx).unwrap();
            ctx.drain()
        };
        let delta = commands
            .into_iter()
            .find_map(|c| match c {
                Command::PaintTiles { delta, .. } => Some(delta),
                _ => None,
            })
            .expect("a warp commit paints");
        tiles.apply_delta(key, &delta);
        // The arch lifts the middle of the band: row 12 at the centre was
        // black and is now covered by the lifted band.
        let after = tiles.pixel(key, 32, 12);
        assert_ne!(after, before, "the arch moved nothing at the centre");
    }

    /// A distorted commit of an 8 px checker, rotated 10 degrees by the
    /// numeric Angle, with the Interpolation at `choice`: the distinct grey
    /// levels among the opaque pixels it leaves.
    fn rotated_checker_levels(choice: usize) -> usize {
        let mut tiles = MemoryTiles::new();
        let layer = layer_model::LayerId::new();
        let key = PixelKey::Layer(layer);
        let ts = raster::TILE_SIZE as usize;
        let mut data = vec![0u8; ts * ts * 4];
        for y in 0..64usize {
            for x in 0..64usize {
                let v = if ((x / 8) + (y / 8)) % 2 == 0 { 255 } else { 0 };
                data[(y * ts + x) * 4..(y * ts + x) * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        tiles.put(key, raster::TileCoord::new(0, 0, 0), data);
        let mut tool = registry::make(ToolId::FreeTransform);
        let distort = TransformMode::ALL
            .iter()
            .position(|m| *m == TransformMode::Distort)
            .unwrap();
        let commands = {
            let mut ctx =
                ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64)).with_layer(layer);
            tool.set_setting("mode", ToolSetting::Choice(distort))
                .unwrap();
            tool.set_setting(keys::INTERPOLATION, ToolSetting::Choice(choice))
                .unwrap();
            press(&mut *tool, &mut ctx);
            let now = NumericTransform::read(&live(&*tool).0, REFERENCE_CENTRE);
            send(
                &mut *tool,
                &numeric_settings(NumericTransform { angle: 10.0, ..now }, 1),
            );
            tool.commit(&mut ctx).unwrap();
            ctx.drain()
        };
        for c in commands {
            if let Command::PaintTiles { delta, .. } = c {
                tiles.apply_delta(key, &delta);
            }
        }
        let mut levels: Vec<u8> = Vec::new();
        for y in 16..48 {
            for x in 16..48 {
                let p = tiles.pixel(key, x, y);
                if p[3] == 255 && !levels.contains(&p[0]) {
                    levels.push(p[0]);
                }
            }
        }
        levels.len()
    }

    #[test]
    fn the_interpolation_choice_reaches_a_resampled_commit() {
        let at = |label: &str| {
            INTERPOLATION_LABELS
                .iter()
                .position(|l| *l == label)
                .unwrap()
        };
        assert_eq!(rotated_checker_levels(at("Nearest Neighbour")), 2);
        assert!(rotated_checker_levels(at("Bilinear")) > 2);
        assert!(rotated_checker_levels(at("Bicubic")) > 2);
    }

    #[test]
    fn interpolation_reaches_the_session_and_picks_the_resampling_filter() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, SIDE, SIDE));
        let mut tool = registry::make(ToolId::FreeTransform);
        press(&mut *tool, &mut ctx);
        let mut concrete = TransformTool::default();
        assert_eq!(concrete.interpolation, Interpolation::Bicubic);
        let nearest = INTERPOLATION_LABELS
            .iter()
            .position(|l| *l == "Nearest Neighbour")
            .unwrap();
        tool.set_setting(keys::INTERPOLATION, ToolSetting::Choice(nearest))
            .unwrap();
        concrete
            .set_setting(keys::INTERPOLATION, ToolSetting::Choice(nearest))
            .unwrap();
        assert_eq!(concrete.interpolation, Interpolation::Nearest);

        // A 2x2 checker of 4 px cells, scaled 3x: nearest keeps only the two
        // source values; bicubic makes new ones along every cell edge.
        let rect = PixelRect::new(0, 0, 32, 32);
        let mut src = FilterBuffer::transparent(32, 32).unwrap();
        for y in 0..8u32 {
            for x in 0..8u32 {
                let v = if ((x / 4) + (y / 4)) % 2 == 0 {
                    1.0
                } else {
                    0.0
                };
                src.set(x, y, [v, v, v, 1.0]);
            }
        }
        let distinct = |interp: Interpolation| {
            let mut s = TransformState::new(PixelRect::new(0, 0, 8, 8));
            s.set_rect(0.0, 0.0, 24.0, 24.0).unwrap();
            let out = resample_with(&src, rect, &s, TransformMode::Scale, interp).unwrap();
            let mut seen: Vec<u32> = Vec::new();
            for y in 0..24u32 {
                for x in 0..24u32 {
                    let bits = out.get(x, y)[0].to_bits();
                    if !seen.contains(&bits) {
                        seen.push(bits);
                    }
                }
            }
            seen.len()
        };
        assert_eq!(distinct(Interpolation::Nearest), 2);
        assert!(distinct(Interpolation::Bilinear) > 2);
        assert!(distinct(Interpolation::Bicubic) > 2);
    }

    // ---- W10-J: Content-Aware Scale as a transform mode -------------------

    /// A 64x64 opaque layer: a light ground with a black 8 px square at
    /// x 4..12, y 28..36.
    fn square_layer() -> (MemoryTiles, layer_model::LayerId) {
        let mut tiles = MemoryTiles::new();
        let layer = layer_model::LayerId::new();
        let ts = raster::TILE_SIZE as usize;
        let mut data = vec![0u8; ts * ts * 4];
        for y in 0..64usize {
            for x in 0..64usize {
                let dark = (4..12).contains(&x) && (28..36).contains(&y);
                let v = if dark { 0 } else { 220 };
                data[(y * ts + x) * 4..(y * ts + x) * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        tiles.put(
            PixelKey::Layer(layer),
            raster::TileCoord::new(0, 0, 0),
            data,
        );
        (tiles, layer)
    }

    /// Drag the right edge's handle of the live box from x 64 to x 48, then
    /// commit; the dark pixels left on row 32, and the commands emitted.
    fn cas_commit(amount: f32) -> (usize, usize) {
        let (mut tiles, layer) = square_layer();
        let key = PixelKey::Layer(layer);
        let mut tool = registry::make(ToolId::FreeTransform);
        tool.set_setting(
            "mode",
            ToolSetting::Choice(TransformMode::CONTENT_AWARE_INDEX),
        )
        .unwrap();
        tool.set_setting(keys::CA_AMOUNT, ToolSetting::Float(amount))
            .unwrap();
        let commands = {
            let mut ctx =
                ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64)).with_layer(layer);
            press(&mut *tool, &mut ctx);
            let (state, mode) = live(&*tool);
            assert_eq!(mode, TransformMode::ContentAware);
            assert_eq!(state.source, PixelRect::new(0, 0, 64, 64));
            tool.on_pointer_down(&mut ctx, PointerEvent::at(64.0, 32.0))
                .unwrap();
            tool.on_pointer_move(&mut ctx, PointerEvent::at(56.0, 32.0))
                .unwrap();
            tool.on_pointer_move(&mut ctx, PointerEvent::at(48.0, 32.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(48.0, 32.0))
                .unwrap();
            let (state, _) = live(&*tool);
            assert_eq!(
                content_aware_dest(&state),
                Some(PixelRect::new(0, 0, 48, 64)),
                "the right edge handle narrowed the box: {state:?}"
            );
            tool.commit(&mut ctx).unwrap();
            ctx.drain()
        };
        let paints: Vec<_> = commands
            .into_iter()
            .filter_map(|c| match c {
                Command::PaintTiles { delta, .. } => Some(delta),
                _ => None,
            })
            .collect();
        let count = paints.len();
        for delta in &paints {
            tiles.apply_delta(key, delta);
        }
        let dark = (0..64)
            .filter(|&x| tiles.pixel(key, x, 32)[3] > 0 && tiles.pixel(key, x, 32)[0] < 110)
            .count();
        (dark, count)
    }

    #[test]
    fn content_aware_mode_carves_the_ground_and_keeps_the_square_as_one_step() {
        let (carved, steps) = cas_commit(100.0);
        assert_eq!(steps, 1, "one PaintTiles step");
        assert_eq!(carved, 8, "seam carving keeps the square 8 px wide");
        // Amount 0 is a plain scale: the square shrinks with the box.
        let (plain, _) = cas_commit(0.0);
        assert!(
            (5..=6).contains(&plain),
            "a plain 75% scale makes it about 6 px, not {plain}"
        );
    }

    #[test]
    fn content_aware_mode_keeps_the_box_upright_and_is_offered_by_the_options_bar() {
        let mut s = TransformState::new(PixelRect::new(0, 0, 40, 20));
        let before = s.corners;
        s.drag(
            TransformMode::ContentAware,
            Handle::Rotate(0),
            Vec2::new(-4.0, -4.0),
            Vec2::new(10.0, -12.0),
        );
        assert_eq!(s.corners, before, "no rotation in Content-Aware Scale");
        let info = registry::info(ToolId::FreeTransform).unwrap();
        let mode = info.options.iter().find(|o| o.key == "mode").unwrap();
        assert_eq!(
            format!("{:?}", mode.kind).matches("Content-Aware").count(),
            1,
            "the Mode choice offers Content-Aware: {:?}",
            mode.kind
        );
        assert!(info.options.iter().any(|o| o.key == keys::CA_AMOUNT));
        assert_eq!(
            TransformMode::from_index(TransformMode::CONTENT_AWARE_INDEX),
            TransformMode::ContentAware
        );
    }
}
