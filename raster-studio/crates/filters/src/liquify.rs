//! Liquify: brush-driven warping through a displacement field.
//!
//! The warp is held as a **backward displacement field**: for every point `p`
//! of the result, the field stores the offset `d(p)` such that the result at
//! `p` is the source sampled at `p + d(p)`. A field of zeros is the identity,
//! and [`LiquifyField::apply`] copies the source through untouched in that
//! case, bit for bit.
//!
//! The field lives at a **bounded resolution**: its long side is at most
//! [`MAX_FIELD_SIDE`] cells, whatever the image size, so a stroke on an 8K
//! canvas touches at most a quarter of a million cells. [`LiquifyField::apply`]
//! upsamples it bilinearly and resamples the source bilinearly, so the same
//! field drives a small preview and the full-resolution layer and the two
//! cannot disagree about where anything went.
//!
//! Brush strokes *compose*: a forward-warp dab of `delta` makes the new result
//! at `p` equal to the old result at `p - w * delta`, so the new field is
//! `d'(p) = -w * delta + d(p - w * delta)`. Dragging a stroke across content
//! that an earlier stroke already moved drags the moved content, as Photopea's
//! does. Twirl, Pucker, Bloat and Push Left compose the same way; Reconstruct
//! scales the field toward zero; the Freeze mask scales every other tool's
//! effect down to nothing where it is painted.
//!
//! Coordinates are **image pixels**, continuous, with `(0.5, 0.5)` the centre
//! of the top-left pixel — the same convention as
//! [`FilterBuffer::sample_bilinear`].

use crate::buffer::FilterBuffer;
use crate::support::{fill_tiles, EdgeMode};

/// The long side of the displacement field, in cells.
pub const MAX_FIELD_SIDE: u32 = 512;
/// The smallest brush diameter, in image pixels.
pub const MIN_BRUSH_SIZE: f32 = 1.0;
/// The largest brush diameter, in image pixels.
pub const MAX_BRUSH_SIZE: f32 = 1500.0;

/// How far one Twirl dab at full weight turns the content, in radians.
const TWIRL_RATE: f32 = 0.12;
/// How far one Pucker or Bloat dab at full weight scales the content.
const SCALE_RATE: f32 = 0.08;
/// How much of the displacement one Reconstruct dab at full weight removes.
const RECONSTRUCT_RATE: f32 = 0.25;
/// How much Freeze or Thaw one dab at full weight paints.
const MASK_RATE: f32 = 0.5;

/// One Liquify tool.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum LiquifyTool {
    /// Pushes content along the stroke.
    #[default]
    ForwardWarp,
    /// Takes the displacement back toward the original.
    Reconstruct,
    /// Rotates the content under the brush clockwise while it is held.
    TwirlClockwise,
    /// Draws the content under the brush toward its centre.
    Pucker,
    /// Pushes the content under the brush away from its centre.
    Bloat,
    /// Moves content perpendicular to the stroke: left of the direction of
    /// travel (dragging up pushes left).
    PushLeft,
    /// Paints the freeze mask: frozen content is not moved by any tool.
    FreezeMask,
    /// Erases the freeze mask.
    ThawMask,
}

impl LiquifyTool {
    /// Every tool, in Photopea's toolbar order.
    pub const ALL: [LiquifyTool; 8] = [
        LiquifyTool::ForwardWarp,
        LiquifyTool::Reconstruct,
        LiquifyTool::TwirlClockwise,
        LiquifyTool::Pucker,
        LiquifyTool::Bloat,
        LiquifyTool::PushLeft,
        LiquifyTool::FreezeMask,
        LiquifyTool::ThawMask,
    ];

    /// The tool's name.
    pub fn label(self) -> &'static str {
        match self {
            LiquifyTool::ForwardWarp => "Forward Warp",
            LiquifyTool::Reconstruct => "Reconstruct",
            LiquifyTool::TwirlClockwise => "Twirl Clockwise",
            LiquifyTool::Pucker => "Pucker",
            LiquifyTool::Bloat => "Bloat",
            LiquifyTool::PushLeft => "Push Left",
            LiquifyTool::FreezeMask => "Freeze Mask",
            LiquifyTool::ThawMask => "Thaw Mask",
        }
    }

    /// Whether the tool acts where the brush rests (a held press keeps
    /// acting), rather than only along the brush's motion.
    pub fn acts_in_place(self) -> bool {
        !matches!(self, LiquifyTool::ForwardWarp | LiquifyTool::PushLeft)
    }
}

/// The brush every tool paints with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LiquifyBrush {
    /// Diameter, in image pixels.
    pub size: f32,
    /// 0..=1: how strongly one dab acts.
    pub pressure: f32,
    /// 0..=1: how much of the effect reaches the brush's rim. Low density is
    /// a soft brush that acts mostly at its centre; high density acts almost
    /// evenly across it.
    pub density: f32,
}

impl Default for LiquifyBrush {
    fn default() -> Self {
        Self {
            size: 100.0,
            pressure: 0.5,
            density: 0.5,
        }
    }
}

impl LiquifyBrush {
    /// The brush with every value finite and in range.
    pub fn sanitized(self) -> Self {
        let clamp = |v: f32, lo: f32, hi: f32, fallback: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                fallback
            }
        };
        Self {
            size: clamp(self.size, MIN_BRUSH_SIZE, MAX_BRUSH_SIZE, 100.0),
            pressure: clamp(self.pressure, 0.0, 1.0, 0.0),
            density: clamp(self.density, 0.0, 1.0, 0.5),
        }
    }

    /// Radius, in image pixels.
    pub fn radius(self) -> f32 {
        self.sanitized().size * 0.5
    }

    /// The brush weight at `t` = distance / radius (0 at and past the rim).
    pub fn falloff(self, t: f32) -> f32 {
        if !(0.0..1.0).contains(&t) {
            return 0.0;
        }
        let density = self.sanitized().density;
        let soft = (1.0 - t * t) * (1.0 - t * t);
        let hard = 1.0 - t * t * t * t;
        soft + (hard - soft) * density
    }
}

/// A warp: the displacement field and the freeze mask, at bounded resolution.
#[derive(Clone, PartialEq, Debug)]
pub struct LiquifyField {
    width: u32,
    height: u32,
    fw: u32,
    fh: u32,
    dx: Vec<f32>,
    dy: Vec<f32>,
    freeze: Vec<f32>,
}

impl LiquifyField {
    /// The identity warp over a `width` x `height` image.
    pub fn new(width: u32, height: u32) -> Self {
        let long = width.max(height);
        let (fw, fh) = if long == 0 {
            (0, 0)
        } else if long <= MAX_FIELD_SIDE {
            (width, height)
        } else {
            let cell = long as f64 / MAX_FIELD_SIDE as f64;
            (
                ((width as f64 / cell).ceil() as u32).clamp(1, MAX_FIELD_SIDE),
                ((height as f64 / cell).ceil() as u32).clamp(1, MAX_FIELD_SIDE),
            )
        };
        let n = fw as usize * fh as usize;
        Self {
            width,
            height,
            fw,
            fh,
            dx: vec![0.0; n],
            dy: vec![0.0; n],
            freeze: vec![0.0; n],
        }
    }

    /// The image size the field was made for.
    pub fn image_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The field's own resolution, in cells.
    pub fn field_size(&self) -> (u32, u32) {
        (self.fw, self.fh)
    }

    /// Whether the field moves nothing.
    pub fn is_identity(&self) -> bool {
        self.dx.iter().chain(self.dy.iter()).all(|v| *v == 0.0)
    }

    /// Image pixels per field cell, per axis.
    fn cell(&self) -> (f32, f32) {
        if self.fw == 0 || self.fh == 0 {
            return (1.0, 1.0);
        }
        (
            self.width as f32 / self.fw as f32,
            self.height as f32 / self.fh as f32,
        )
    }

    /// The largest displacement anywhere, in image pixels.
    pub fn max_displacement(&self) -> f32 {
        self.dx
            .iter()
            .zip(&self.dy)
            .map(|(x, y)| (x * x + y * y).sqrt())
            .fold(0.0, f32::max)
    }

    /// Bilinear sample of a per-cell plane at image coordinates, clamped at
    /// the field's edges.
    fn sample_plane(&self, plane: &[f32], x: f32, y: f32) -> f32 {
        if plane.is_empty() {
            return 0.0;
        }
        let (cx, cy) = self.cell();
        let fx = if x.is_finite() { x / cx - 0.5 } else { 0.0 };
        let fy = if y.is_finite() { y / cy - 0.5 } else { 0.0 };
        let max_x = (self.fw - 1) as f32;
        let max_y = (self.fh - 1) as f32;
        let fx = fx.clamp(0.0, max_x);
        let fy = fy.clamp(0.0, max_y);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(self.fw as usize - 1);
        let y1 = (y0 + 1).min(self.fh as usize - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let w = self.fw as usize;
        let at = |xx: usize, yy: usize| plane[yy * w + xx];
        let a = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
        let b = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
        a + (b - a) * ty
    }

    /// The displacement at image coordinates `(x, y)`.
    pub fn displacement_at(&self, x: f32, y: f32) -> [f32; 2] {
        [
            self.sample_plane(&self.dx, x, y),
            self.sample_plane(&self.dy, x, y),
        ]
    }

    /// The freeze mask at image coordinates `(x, y)`, 0..=1.
    pub fn freeze_at(&self, x: f32, y: f32) -> f32 {
        self.sample_plane(&self.freeze, x, y)
    }

    /// Put every displacement back to zero (Restore All). The freeze mask is
    /// kept, as Photopea keeps it.
    pub fn restore_all(&mut self) {
        self.dx.iter_mut().for_each(|v| *v = 0.0);
        self.dy.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Clear the freeze mask.
    pub fn thaw_all(&mut self) {
        self.freeze.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Paint one stroke segment from `from` to `to` (image coordinates).
    ///
    /// Motion tools ([`LiquifyTool::ForwardWarp`], [`LiquifyTool::PushLeft`])
    /// act along the segment and do nothing for a zero-length one. The
    /// in-place tools act at every step along the segment, including a
    /// zero-length one — a held press — so holding Twirl keeps turning. Long
    /// segments are split at a quarter of the radius so a fast drag leaves no
    /// gaps. Non-finite points are ignored.
    pub fn stroke(&mut self, tool: LiquifyTool, brush: LiquifyBrush, from: [f32; 2], to: [f32; 2]) {
        if self.dx.is_empty() || !from.iter().chain(to.iter()).all(|v| v.is_finite()) {
            return;
        }
        let brush = brush.sanitized();
        let radius = brush.radius();
        let delta = [to[0] - from[0], to[1] - from[1]];
        let len = (delta[0] * delta[0] + delta[1] * delta[1]).sqrt();
        if !tool.acts_in_place() && len == 0.0 {
            return;
        }
        let spacing = (radius * 0.25).max(0.5);
        let steps = ((len / spacing).ceil() as usize).clamp(1, 4096);
        let step = [delta[0] / steps as f32, delta[1] / steps as f32];
        for k in 0..steps {
            let at = [
                from[0] + step[0] * (k + 1) as f32,
                from[1] + step[1] * (k + 1) as f32,
            ];
            self.dab(tool, brush, at, step);
        }
    }

    /// One dab at `center` moving by `delta` (motion tools).
    fn dab(&mut self, tool: LiquifyTool, brush: LiquifyBrush, center: [f32; 2], delta: [f32; 2]) {
        let radius = brush.radius();
        let (cx, cy) = self.cell();
        let w = self.fw as usize;
        let i0 = (((center[0] - radius) / cx).floor().max(0.0)) as usize;
        let j0 = (((center[1] - radius) / cy).floor().max(0.0)) as usize;
        let i1 = ((((center[0] + radius) / cx).ceil()).max(0.0) as usize).min(self.fw as usize);
        let j1 = ((((center[1] + radius) / cy).ceil()).max(0.0) as usize).min(self.fh as usize);
        if i0 >= i1 || j0 >= j1 {
            return;
        }
        // Composition samples the field as it was before this dab.
        let composes = !matches!(
            tool,
            LiquifyTool::Reconstruct | LiquifyTool::FreezeMask | LiquifyTool::ThawMask
        );
        let old = composes.then(|| (self.dx.clone(), self.dy.clone()));
        for j in j0..j1 {
            for i in i0..i1 {
                let p = [(i as f32 + 0.5) * cx, (j as f32 + 0.5) * cy];
                let rel = [p[0] - center[0], p[1] - center[1]];
                let dist = (rel[0] * rel[0] + rel[1] * rel[1]).sqrt();
                let f = brush.falloff(dist / radius);
                if f <= 0.0 {
                    continue;
                }
                let idx = j * w + i;
                match tool {
                    LiquifyTool::FreezeMask => {
                        self.freeze[idx] =
                            (self.freeze[idx] + f * brush.pressure * MASK_RATE * 2.0).min(1.0);
                        continue;
                    }
                    LiquifyTool::ThawMask => {
                        self.freeze[idx] =
                            (self.freeze[idx] - f * brush.pressure * MASK_RATE * 2.0).max(0.0);
                        continue;
                    }
                    _ => {}
                }
                let weight = f * brush.pressure * (1.0 - self.freeze[idx]);
                if weight <= 0.0 {
                    continue;
                }
                if tool == LiquifyTool::Reconstruct {
                    let keep = 1.0 - (weight * RECONSTRUCT_RATE).min(1.0);
                    self.dx[idx] *= keep;
                    self.dy[idx] *= keep;
                    continue;
                }
                // Where the new result at `p` reads the old result from.
                let q = match tool {
                    LiquifyTool::ForwardWarp => {
                        [p[0] - delta[0] * weight, p[1] - delta[1] * weight]
                    }
                    LiquifyTool::PushLeft => {
                        // Left of the direction of travel, in y-down space.
                        let left = [delta[1], -delta[0]];
                        [p[0] - left[0] * weight, p[1] - left[1] * weight]
                    }
                    LiquifyTool::TwirlClockwise => {
                        // The content turns by +theta (clockwise on a y-down
                        // screen), so the result reads from -theta.
                        let theta = -TWIRL_RATE * weight;
                        let (s, c) = theta.sin_cos();
                        [
                            center[0] + rel[0] * c - rel[1] * s,
                            center[1] + rel[0] * s + rel[1] * c,
                        ]
                    }
                    LiquifyTool::Pucker => {
                        let k = 1.0 + SCALE_RATE * weight;
                        [center[0] + rel[0] * k, center[1] + rel[1] * k]
                    }
                    LiquifyTool::Bloat => {
                        let k = 1.0 - SCALE_RATE * weight;
                        [center[0] + rel[0] * k, center[1] + rel[1] * k]
                    }
                    LiquifyTool::Reconstruct | LiquifyTool::FreezeMask | LiquifyTool::ThawMask => p,
                };
                if let Some((old_dx, old_dy)) = &old {
                    let d = [
                        self.sample_plane(old_dx, q[0], q[1]),
                        self.sample_plane(old_dy, q[0], q[1]),
                    ];
                    self.dx[idx] = q[0] - p[0] + d[0];
                    self.dy[idx] = q[1] - p[1] + d[1];
                }
            }
        }
    }

    /// Warp `src` by this field.
    ///
    /// `src` may be any size: the field is laid over it by the ratio of the
    /// two, so a downscaled preview and the full-resolution layer warp the
    /// same way. A zero-displacement pixel is copied through untouched, so the
    /// identity field returns `src` exactly. Outside the image the source's
    /// edge pixels repeat ([`EdgeMode::Clamp`]).
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        if src.is_empty() || self.width == 0 || self.height == 0 || self.is_identity() {
            return src.clone();
        }
        let (sw, sh) = src.dimensions();
        let kx = sw as f32 / self.width as f32;
        let ky = sh as f32 / self.height as f32;
        let mut out = src.clone();
        fill_tiles(sw, sh, out.pixels_mut(), |x, y| {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = self.displacement_at(px / kx, py / ky);
            if d[0] == 0.0 && d[1] == 0.0 {
                return src.get(x, y);
            }
            src.sample_bilinear(px + d[0] * kx, py + d[1] * ky, EdgeMode::Clamp)
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white 64x64 image with a black vertical bar two pixels wide at
    /// x = 20..22.
    fn bar() -> FilterBuffer {
        let mut buf = FilterBuffer::filled(64, 64, [1.0, 1.0, 1.0, 1.0]).unwrap();
        for y in 0..64 {
            for x in 20..22 {
                buf.set(x, y, [0.0, 0.0, 0.0, 1.0]);
            }
        }
        buf
    }

    /// The darkness-weighted mean x of row `y`.
    fn dark_centroid(buf: &FilterBuffer, y: u32) -> f32 {
        let (w, _) = buf.dimensions();
        let (mut sum, mut total) = (0.0, 0.0);
        for x in 0..w {
            let dark = 1.0 - buf.get(x, y)[0];
            sum += dark * x as f32;
            total += dark;
        }
        sum / total
    }

    fn brush() -> LiquifyBrush {
        LiquifyBrush {
            size: 40.0,
            pressure: 1.0,
            density: 0.5,
        }
    }

    #[test]
    fn forward_warp_with_zero_strokes_is_identity() {
        let src = bar();
        let field = LiquifyField::new(64, 64);
        assert!(field.is_identity());
        assert_eq!(field.apply(&src), src);
        // A zero-length Forward Warp stroke moves nothing either.
        let mut field = LiquifyField::new(64, 64);
        field.stroke(
            LiquifyTool::ForwardWarp,
            brush(),
            [20.0, 32.0],
            [20.0, 32.0],
        );
        assert!(field.is_identity());
        assert_eq!(field.apply(&src), src);
    }

    #[test]
    fn one_forward_warp_stroke_moves_pixels_in_the_stroke_direction() {
        let src = bar();
        let before = dark_centroid(&src, 32);
        let mut field = LiquifyField::new(64, 64);
        field.stroke(
            LiquifyTool::ForwardWarp,
            brush(),
            [21.0, 32.0],
            [31.0, 32.0],
        );
        assert!(!field.is_identity());
        let out = field.apply(&src);
        let after = dark_centroid(&out, 32);
        assert!(
            after > before + 3.0,
            "the bar moved right with the stroke: {before} -> {after}"
        );
        // A row far outside the brush is untouched.
        for x in 0..64 {
            assert_eq!(out.get(x, 2), src.get(x, 2));
        }
        // Stroking left moves it left.
        let mut field = LiquifyField::new(64, 64);
        field.stroke(
            LiquifyTool::ForwardWarp,
            brush(),
            [21.0, 32.0],
            [11.0, 32.0],
        );
        assert!(dark_centroid(&field.apply(&src), 32) < before - 3.0);
    }

    #[test]
    fn reconstruct_after_a_warp_returns_toward_identity() {
        let src = bar();
        let mut field = LiquifyField::new(64, 64);
        field.stroke(
            LiquifyTool::ForwardWarp,
            brush(),
            [21.0, 32.0],
            [31.0, 32.0],
        );
        let warped = field.max_displacement();
        let warped_centroid = dark_centroid(&field.apply(&src), 32);
        // A brush wide enough to cover the whole warp.
        let wide = LiquifyBrush {
            size: 120.0,
            ..brush()
        };
        for _ in 0..40 {
            field.stroke(LiquifyTool::Reconstruct, wide, [26.0, 32.0], [26.0, 32.0]);
        }
        let rebuilt = field.max_displacement();
        assert!(
            rebuilt < warped * 0.1,
            "reconstruct shrank the field: {warped} -> {rebuilt}"
        );
        let original = dark_centroid(&src, 32);
        let now = dark_centroid(&field.apply(&src), 32);
        assert!(
            (now - original).abs() < (warped_centroid - original).abs() * 0.2,
            "the bar went back: original {original}, warped {warped_centroid}, now {now}"
        );
    }

    #[test]
    fn frozen_content_does_not_move() {
        let src = bar();
        let mut field = LiquifyField::new(64, 64);
        for _ in 0..4 {
            field.stroke(LiquifyTool::FreezeMask, brush(), [21.0, 32.0], [21.0, 32.0]);
        }
        assert!(field.freeze_at(21.0, 32.0) >= 0.999);
        field.stroke(
            LiquifyTool::ForwardWarp,
            brush(),
            [21.0, 32.0],
            [31.0, 32.0],
        );
        assert_eq!(field.displacement_at(21.5, 32.5), [0.0, 0.0]);
        assert_eq!(field.apply(&src).get(21, 32), src.get(21, 32));
        field.thaw_all();
        assert_eq!(field.freeze_at(21.0, 32.0), 0.0);
    }

    #[test]
    fn twirl_pucker_bloat_and_push_left_each_move_content() {
        let src = bar();
        for tool in [
            LiquifyTool::TwirlClockwise,
            LiquifyTool::Pucker,
            LiquifyTool::Bloat,
        ] {
            let mut field = LiquifyField::new(64, 64);
            field.stroke(tool, brush(), [28.0, 32.0], [28.0, 32.0]);
            assert!(!field.is_identity(), "{tool:?} did nothing held in place");
            assert_ne!(field.apply(&src), src, "{tool:?}");
        }
        // Dragging straight up pushes left.
        let mut field = LiquifyField::new(64, 64);
        field.stroke(LiquifyTool::PushLeft, brush(), [21.0, 40.0], [21.0, 30.0]);
        let before = dark_centroid(&src, 35);
        let after = dark_centroid(&field.apply(&src), 35);
        assert!(after < before - 1.0, "push left: {before} -> {after}");
        // Bloat pushes the bar (left of centre) further left.
        let mut field = LiquifyField::new(64, 64);
        for _ in 0..5 {
            field.stroke(LiquifyTool::Bloat, brush(), [28.0, 32.0], [28.0, 32.0]);
        }
        assert!(dark_centroid(&field.apply(&src), 32) < before - 1.0);
    }

    #[test]
    fn the_field_is_bounded_and_drives_any_size_the_same_way() {
        let field = LiquifyField::new(4000, 1000);
        let (fw, fh) = field.field_size();
        assert!(fw <= MAX_FIELD_SIDE && fh <= MAX_FIELD_SIDE);
        assert_eq!(fw, MAX_FIELD_SIDE);
        // A half-size preview warps the bar to the same place, scaled.
        let src = bar();
        let mut field = LiquifyField::new(64, 64);
        field.stroke(
            LiquifyTool::ForwardWarp,
            brush(),
            [21.0, 32.0],
            [31.0, 32.0],
        );
        let mut half = FilterBuffer::filled(32, 32, [1.0; 4]).unwrap();
        for y in 0..32 {
            half.set(10, y, [0.0, 0.0, 0.0, 1.0]);
        }
        let full = dark_centroid(&field.apply(&src), 32);
        let small = dark_centroid(&field.apply(&half), 16);
        assert!(
            (small * 2.0 - full).abs() < 2.5,
            "full {full} vs half {small}"
        );
    }

    #[test]
    fn degenerate_inputs_never_panic() {
        let mut empty = LiquifyField::new(0, 0);
        empty.stroke(LiquifyTool::ForwardWarp, brush(), [0.0, 0.0], [5.0, 5.0]);
        assert!(empty.is_identity());
        let one = FilterBuffer::filled(1, 1, [0.5; 4]).unwrap();
        let mut field = LiquifyField::new(1, 1);
        field.stroke(
            LiquifyTool::ForwardWarp,
            LiquifyBrush {
                size: f32::NAN,
                pressure: f32::INFINITY,
                density: -3.0,
            },
            [0.0, 0.0],
            [f32::NAN, 1.0],
        );
        assert_eq!(field.apply(&one), one);
    }
}
