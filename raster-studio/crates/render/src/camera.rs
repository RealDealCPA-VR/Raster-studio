//! 2D pan/zoom/rotate camera. Produces the affine that the quad shader uses
//! to map clip-space (-1..1) to source-image UV (0..1).
//!
//! # Rotation convention
//!
//! [`Camera::rotation`] is the view rotation in radians, **positive clockwise
//! on screen** — the same convention as `ui::CanvasCamera` (whose document
//! y axis also points down), so a rotation read off the one can be written
//! onto the other without a sign flip. The view rotates about the centre of
//! the viewport, which is where `center` sits, so rotating never moves the
//! point under the middle of the window:
//!
//! ```text
//!   image = center + R(-rotation) · F · (screen - viewport_centre) / zoom
//! ```
//!
//! where `viewport_centre = viewport_origin + viewport_size / 2`: the middle of
//! the canvas area the host gave the camera, in surface pixels.
//!
//! # Mirror convention
//!
//! [`Camera::flip_x`] / [`Camera::flip_y`] mirror the view left-to-right /
//! top-to-bottom about the viewport centre, *on screen*: `F` above is
//! `diag(±1, ±1)`, applied after the rotation on the way to the screen
//! (`screen = viewport/2 + F · zoom · R(rotation) · (image - center)`). That is
//! `ui::CanvasCamera`'s `F · S · R` basis, so the two cameras agree about
//! every pixel with both a turn and a mirror in force. Like the rotation, a
//! mirror edits no pixel: it is a view setting.

use glam::Vec2;

/// Smallest zoom a camera may be driven to: one image pixel per hundred screen
/// pixels of the whole picture. Below it a document is a dot.
///
/// Published rather than kept as a literal inside [`Camera::zoom_at`] because a
/// host that sets [`Camera::zoom`] directly — a typed zoom level, a Navigator
/// slider — must clamp to the same range a wheel gesture does, or the two
/// routes to the same number disagree.
pub const MIN_ZOOM: f32 = 0.01;
/// Largest zoom a camera may be driven to: sixty-four screen pixels per image
/// pixel, which is where a single pixel fills a small window.
pub const MAX_ZOOM: f32 = 64.0;

/// A pannable, zoomable 2D view of an image of known pixel size.
#[derive(Debug, Clone)]
pub struct Camera {
    /// Center of the view, in image pixel coordinates.
    pub center: Vec2,
    /// Zoom factor: screen pixels per image pixel (1.0 = 100%).
    pub zoom: f32,
    /// View rotation in radians, positive clockwise on screen, about the
    /// viewport centre. Write it through [`Camera::set_rotation`], which wraps
    /// it into `(-π, π]` and refuses a non-finite angle.
    pub rotation: f32,
    /// Mirror the view left-to-right about the viewport centre. The app's
    /// View > Flip Horizontal item is meant to set it; in this build the app
    /// shell still greys that item and never sets the flag.
    pub flip_x: bool,
    /// Mirror the view top-to-bottom about the viewport centre. The app's
    /// View > Flip Vertical item is meant to set it; in this build the app
    /// shell still greys that item and never sets the flag.
    pub flip_y: bool,
    /// Size of the image being viewed, in pixels.
    pub image_size: Vec2,
    /// Size of the viewport, in pixels: the canvas area the image is drawn
    /// into, which is not the whole surface once a host paints panels around
    /// it. Fit, fill and centring all divide by this.
    pub viewport_size: Vec2,
    /// Top-left of the viewport within the surface, in pixels. Screen
    /// coordinates ([`Camera::screen_to_image`], [`Camera::zoom_at`]) are
    /// measured from the surface's corner, so the image centres on
    /// `viewport_origin + viewport_size / 2` — the middle of the canvas area,
    /// not of the window. Zero (the default) is a viewport that starts at the
    /// surface's corner; [`crate::Canvas::render_in`] draws into exactly this
    /// rectangle.
    pub viewport_origin: Vec2,
}

impl Camera {
    pub fn new(image_size: Vec2, viewport_size: Vec2) -> Self {
        Self {
            center: image_size * 0.5,
            zoom: 1.0,
            rotation: 0.0,
            flip_x: false,
            flip_y: false,
            image_size,
            viewport_size,
            viewport_origin: Vec2::ZERO,
        }
    }

    /// The middle of the viewport, in surface pixels: where `center` is drawn.
    pub fn viewport_center(&self) -> Vec2 {
        self.viewport_origin + self.viewport_size * 0.5
    }

    /// Pan by a delta given in *screen* pixels.
    ///
    /// Honours the rotation: dragging right on a view turned a quarter turn
    /// moves the camera along the image's y axis, so the picture follows the
    /// pointer.
    pub fn pan_screen(&mut self, delta_px: Vec2) {
        self.center -= self.screen_vector_to_image(delta_px);
    }

    /// Set the view rotation, wrapped into `(-π, π]`. A non-finite angle is
    /// ignored rather than written, so the affine can never go non-finite
    /// through this route.
    pub fn set_rotation(&mut self, radians: f32) {
        if !radians.is_finite() {
            return;
        }
        self.rotation = wrap_angle(radians);
    }

    /// Turn the view by `radians` about the viewport centre.
    pub fn rotate_by(&mut self, radians: f32) {
        self.set_rotation(self.rotation + radians);
    }

    /// Put the view back upright (View ▸ Reset View Rotation).
    pub fn reset_rotation(&mut self) {
        self.rotation = 0.0;
    }

    /// Whether the view is turned at all.
    pub fn is_rotated(&self) -> bool {
        self.rotation != 0.0
    }

    /// Mirror the view left-to-right (a toggle).
    pub fn flip_horizontal(&mut self) {
        self.flip_x = !self.flip_x;
    }

    /// Mirror the view top-to-bottom (a toggle).
    pub fn flip_vertical(&mut self) {
        self.flip_y = !self.flip_y;
    }

    /// Whether the view is mirrored on either axis.
    pub fn is_flipped(&self) -> bool {
        self.flip_x || self.flip_y
    }

    /// The on-screen mirror as a per-axis sign: `F = diag(x, y)`.
    fn flip_signs(&self) -> Vec2 {
        Vec2::new(
            if self.flip_x { -1.0 } else { 1.0 },
            if self.flip_y { -1.0 } else { 1.0 },
        )
    }

    /// A screen-space *direction* in image space: un-mirrored, rotated back by
    /// the view rotation and divided by the zoom, with no translation.
    fn screen_vector_to_image(&self, delta_px: Vec2) -> Vec2 {
        let (s, c) = self.rotation.sin_cos();
        // F is its own inverse and the last thing applied on the way to the
        // screen, so it is the first thing undone.
        let d = delta_px * self.flip_signs();
        // R(-rotation): the inverse of the clockwise-on-screen turn.
        let unturned = Vec2::new(c * d.x + s * d.y, -s * d.x + c * d.y);
        unturned / self.zoom.max(1e-6)
    }

    /// Zoom toward a screen-space anchor (e.g. the cursor) by a multiplier.
    pub fn zoom_at(&mut self, anchor_screen: Vec2, factor: f32) {
        let before = self.screen_to_image(anchor_screen);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.screen_to_image(anchor_screen);
        // Keep the anchor point stationary in image space.
        self.center += before - after;
    }

    /// Scale the view so the whole image fits the viewport.
    pub fn fit(&mut self) {
        let sx = self.viewport_size.x / self.image_size.x.max(1.0);
        let sy = self.viewport_size.y / self.image_size.y.max(1.0);
        self.zoom = sx.min(sy);
        self.center = self.image_size * 0.5;
    }

    /// Convert a screen-space point to image-space pixels, through the pan,
    /// the zoom, the view rotation and the view mirror.
    pub fn screen_to_image(&self, screen: Vec2) -> Vec2 {
        let from_center = screen - self.viewport_center();
        self.center + self.screen_vector_to_image(from_center)
    }

    /// The frame the transparency checkerboard is drawn in, for `quad.wgsl`'s
    /// fourth uniform row: `[cos, sin, viewport_centre.x, viewport_centre.y]`.
    ///
    /// The checker keeps its on-screen cell size under every zoom, so it is
    /// computed in framebuffer pixels — but it belongs to the *document*, so
    /// it must turn with the picture when the view is rotated rather than stay
    /// nailed to the window. The shader turns each fragment back by the view
    /// rotation about the viewport centre before looking up the cell; at zero
    /// rotation that is the identity and the pattern is exactly what it always
    /// was.
    ///
    /// # Under a mirror
    ///
    /// The checker mirrors with the document too, yet the shader's frame is a
    /// *rotation* about a point, `q = k + [[C, S], [-S, C]] · (p - k)`, which
    /// no `(C, S)` makes a reflection. It does not have to be one, because the
    /// pattern is symmetric: the cell parity `floor(x/8) + floor(y/8)` is
    /// unchanged by swapping x and y (`W = [[0, 1], [1, 0]]`), exactly, at
    /// every real point. So the wanted lookup `P(c0 + A·(p - c0))`, with the
    /// orientation-reversing `A = R(-rotation)·F`, equals
    /// `P(W·(c0 + A·(p - c0)))`, and `M = W·A` *is* a rotation. Matching
    /// `k + M·(p - k)` to `W·c0 + M·(p - c0)` gives `(I - M)·k = W·c0 - M·c0`.
    /// `I - M` is singular only where `M = I`, so the negated swap `-W` (under
    /// which the pattern is also symmetric off exact cell boundaries) is used
    /// whenever it keeps `C <= 0`: then `det(I - M) = 2 - 2C >= 2` and `k` is
    /// well-conditioned. A double mirror is `F = -I`, a half turn, which needs
    /// no trick at all.
    pub fn checker_frame(&self) -> [f32; 4] {
        let (s, c) = self.rotation.sin_cos();
        // In framebuffer pixels, like the fragment position the shader turns:
        // the canvas pass draws into the viewport rectangle, but
        // `@builtin(position)` is still measured from the surface's corner.
        let centre = self.viewport_center();
        match (self.flip_x, self.flip_y) {
            (false, false) => [c, s, centre.x, centre.y],
            // F = -I: the frame is turned half a turn further.
            (true, true) => [-c, -s, centre.x, centre.y],
            (flip_x, _) => {
                // W·A, written as the shader's [[C, S], [-S, C]]:
                //   flip_x: A = [[-c, s], [s, c]]   -> W·A = [[s, c], [-c, s]]
                //   flip_y: A = [[c, -s], [-s, -c]] -> W·A = [[-s, -c], [c, -s]]
                let (big_c, big_s) = if flip_x { (s, c) } else { (-s, -c) };
                // sigma picks W or -W; keep C <= 0 so I - M is invertible.
                let sigma = if big_c > 0.0 { -1.0 } else { 1.0 };
                let (big_c, big_s) = (big_c * sigma, big_s * sigma);
                // rhs = sigma·W·c0 - M·c0, with M = [[C, S], [-S, C]].
                let swapped = Vec2::new(centre.y, centre.x) * sigma;
                let m_c0 = Vec2::new(
                    big_c * centre.x + big_s * centre.y,
                    -big_s * centre.x + big_c * centre.y,
                );
                let rhs = swapped - m_c0;
                // (I - M)^-1 = [[1 - C, S], [-S, 1 - C]] / ((1 - C)^2 + S^2).
                let one_c = 1.0 - big_c;
                let det = one_c * one_c + big_s * big_s;
                let k = Vec2::new(
                    one_c * rhs.x + big_s * rhs.y,
                    -big_s * rhs.x + one_c * rhs.y,
                ) / det;
                [big_c, big_s, k.x, k.y]
            }
        }
    }

    /// The affine used by `quad.wgsl`: maps clip-space (-1..1) to UV (0..1).
    ///
    /// Returned as two rows `[ax, bx, cx, ay]` and `[by, cy, 0, 0]` matching
    /// the shader's `Camera` uniform layout, which evaluates
    ///
    /// ```text
    /// u = ax*clip.x + bx*clip.y + cx
    /// v = ay*clip.x + by*clip.y + cy
    /// ```
    ///
    /// Constraints this must satisfy:
    ///
    /// * At zero rotation `u` depends on `clip.x` only and `v` on `clip.y`
    ///   only — both cross terms (`bx`, `ay`) are exactly zero. A stray
    ///   non-zero `ay` smears one texture row across every screen row. Under a
    ///   rotation the cross terms carry `sin(rotation)` and the diagonal
    ///   `cos(rotation)`: the view turns about the viewport centre, so
    ///   `(cx, cy)` is the camera centre regardless of the angle.
    /// * `by` is NEGATIVE at zero rotation: clip `y = +1` is the top of the
    ///   screen and must map to `v = 0`, the first (top) row of the source
    ///   texture. See the orientation convention documented in
    ///   `render_shaders`.
    /// * `image_size` components are treated as at least 1 px, so a degenerate
    ///   image cannot produce a non-finite affine.
    ///
    /// `m1[2..]` are the unused slots of the 2x3 affine. `Canvas::update_camera`
    /// overwrites `m1[2]` with the target's sRGB-encode flag before upload; the
    /// camera itself has no opinion on color spaces.
    pub fn clip_to_uv(&self) -> ([f32; 4], [f32; 4]) {
        let image = Vec2::new(self.image_size.x.max(1.0), self.image_size.y.max(1.0));
        // Visible image-space extent (half width/height) at current zoom.
        let half = self.viewport_size / (2.0 * self.zoom.max(1e-6));
        let (s, c) = self.rotation.sin_cos();
        let f = self.flip_signs();

        // The screen offset of a clip point, in image pixels before the turn,
        // is (half.x * clip.x, -half.y * clip.y): clip.y is flipped because +1
        // is the screen top, which is the image top (smaller y). The view
        // mirror F = diag(f.x, f.y) is undone first (it is applied last on the
        // way to the screen), then the view rotation with R(-rotation) =
        // [[c, s], [-s, c]], then the result is divided by the image size to
        // land in UV.
        let ax = c * f.x * half.x / image.x;
        let bx = -s * f.y * half.y / image.x;
        let cx = self.center.x / image.x;
        let ay = -s * f.x * half.x / image.y;
        let by = -c * f.y * half.y / image.y;
        let cy = self.center.y / image.y;

        ([ax, bx, cx, ay], [by, cy, 0.0, 0.0])
    }
}

/// Wrap an angle into `(-π, π]`.
fn wrap_angle(radians: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let wrapped = radians.rem_euclid(TAU);
    if wrapped > PI {
        wrapped - TAU
    } else {
        wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera {
        Camera::new(Vec2::new(4096.0, 2160.0), Vec2::new(1280.0, 720.0))
    }

    /// Evaluate the affine exactly as `quad.wgsl`'s fragment stage does.
    fn shader_uv(c: &Camera, clip: Vec2) -> Vec2 {
        let (m0, m1) = c.clip_to_uv();
        let (ax, bx, cx, ay) = (m0[0], m0[1], m0[2], m0[3]);
        let (by, cy) = (m1[0], m1[1]);
        Vec2::new(
            ax * clip.x + bx * clip.y + cx,
            ay * clip.x + by * clip.y + cy,
        )
    }

    #[test]
    fn clip_center_maps_to_camera_center_in_uv() {
        let mut c = cam();
        c.zoom = 0.5;
        c.center = Vec2::new(1000.0, 800.0);
        let uv = shader_uv(&c, Vec2::ZERO);
        assert!((uv.x - 1000.0 / 4096.0).abs() < 1e-6, "u = {}", uv.x);
        assert!((uv.y - 800.0 / 2160.0).abs() < 1e-6, "v = {}", uv.y);
    }

    #[test]
    fn u_ignores_clip_y_and_v_ignores_clip_x() {
        let mut c = cam();
        c.zoom = 0.37;
        c.center = Vec2::new(1234.0, 567.0);
        let a = shader_uv(&c, Vec2::new(0.5, -1.0));
        let b = shader_uv(&c, Vec2::new(0.5, 1.0));
        assert!(
            (a.x - b.x).abs() < 1e-6,
            "u drifted with clip.y: {a:?} {b:?}"
        );

        let l = shader_uv(&c, Vec2::new(-1.0, 0.25));
        let r = shader_uv(&c, Vec2::new(1.0, 0.25));
        assert!(
            (l.y - r.y).abs() < 1e-6,
            "v drifted with clip.x: {l:?} {r:?}"
        );
    }

    /// Regression: the old affine put the v scale on the clip.x coefficient, so
    /// v was constant down every column and every screen row sampled the same
    /// texture row.
    #[test]
    fn v_actually_varies_down_the_screen() {
        let mut c = cam();
        c.fit();
        let top = shader_uv(&c, Vec2::new(0.0, 1.0));
        let bottom = shader_uv(&c, Vec2::new(0.0, -1.0));
        assert!(
            (top.y - bottom.y).abs() > 0.1,
            "v is constant down the screen: top={} bottom={}",
            top.y,
            bottom.y
        );
    }

    /// clip y = +1 is the top of the screen and must show the TOP of the image.
    #[test]
    fn v_is_flipped_so_image_is_not_upside_down() {
        let mut c = cam();
        c.fit();
        let top = shader_uv(&c, Vec2::new(0.0, 1.0));
        let bottom = shader_uv(&c, Vec2::new(0.0, -1.0));
        assert!(
            top.y < bottom.y,
            "image renders upside down: top v={} bottom v={}",
            top.y,
            bottom.y
        );
    }

    /// A square image fitted into a square viewport must map the clip corners
    /// exactly onto the UV corners, with (−1,+1) at (0,0).
    #[test]
    fn fitted_square_maps_clip_corners_to_uv_corners() {
        let mut c = Camera::new(Vec2::splat(256.0), Vec2::splat(64.0));
        c.fit();
        let tl = shader_uv(&c, Vec2::new(-1.0, 1.0));
        let br = shader_uv(&c, Vec2::new(1.0, -1.0));
        assert!(tl.abs_diff_eq(Vec2::ZERO, 1e-6), "top-left uv = {tl:?}");
        assert!(br.abs_diff_eq(Vec2::ONE, 1e-6), "bottom-right uv = {br:?}");
    }

    #[test]
    fn zero_sized_image_yields_finite_affine() {
        let c = Camera::new(Vec2::ZERO, Vec2::new(800.0, 600.0));
        let (m0, m1) = c.clip_to_uv();
        for v in m0.iter().chain(m1.iter()) {
            assert!(v.is_finite(), "non-finite affine term {v}");
        }
    }

    #[test]
    fn screen_center_maps_to_camera_center() {
        let c = cam();
        let img = c.screen_to_image(c.viewport_size * 0.5);
        assert!((img - c.center).length() < 1e-3);
    }

    #[test]
    fn zoom_at_keeps_anchor_stationary() {
        let mut c = cam();
        let anchor = Vec2::new(300.0, 200.0);
        let before = c.screen_to_image(anchor);
        c.zoom_at(anchor, 2.0);
        let after = c.screen_to_image(anchor);
        assert!((before - after).length() < 1e-2, "anchor drifted");
    }

    #[test]
    fn fit_sets_zoom_to_min_ratio() {
        let mut c = cam();
        c.fit();
        assert!((c.zoom - (1280.0 / 4096.0)).abs() < 1e-4);
    }

    #[test]
    fn pan_moves_center_inversely_to_zoom() {
        let mut c = cam();
        c.zoom = 2.0;
        let start = c.center;
        c.pan_screen(Vec2::new(100.0, 0.0));
        assert!((c.center.x - (start.x - 50.0)).abs() < 1e-3);
    }

    /// The rotation is a real turn of the view, not a reflection: at a quarter
    /// turn clockwise the image's top edge is on the screen's right, and the
    /// affine at 90° is the affine at 0° evaluated at the turned clip point.
    #[test]
    fn a_quarter_turn_puts_the_image_top_on_the_screen_right() {
        let mut upright = Camera::new(Vec2::splat(256.0), Vec2::splat(64.0));
        upright.fit();
        let mut turned = upright.clone();
        turned.set_rotation(std::f32::consts::FRAC_PI_2);

        // Clockwise: the image's top edge swings to the screen's right and its
        // left edge to the screen's top. So screen right of centre (clip
        // (1, 0)) shows the image TOP (v below the centre's), and screen top
        // (clip (0, 1)) shows the image LEFT (u below the centre's).
        let right = shader_uv(&turned, Vec2::new(1.0, 0.0));
        assert!(
            right.y < 0.5 - 0.4,
            "screen right is not the image top: {right:?}"
        );
        assert!((right.x - 0.5).abs() < 1e-5, "{right:?}");
        let top = shader_uv(&turned, Vec2::new(0.0, 1.0));
        assert!(
            top.x < 0.5 - 0.4,
            "screen top is not the image left: {top:?}"
        );
        assert!((top.y - 0.5).abs() < 1e-5, "{top:?}");

        // Exactly: uv_90(x, y) == uv_0(-y, x) — a clockwise turn of the clip
        // point.
        for clip in [
            Vec2::new(0.3, -0.7),
            Vec2::new(-1.0, 1.0),
            Vec2::new(0.9, 0.2),
        ] {
            let a = shader_uv(&turned, clip);
            let b = shader_uv(&upright, Vec2::new(-clip.y, clip.x));
            assert!(
                a.abs_diff_eq(b, 1e-5),
                "clip {clip:?}: turned {a:?} vs upright {b:?}"
            );
        }
    }

    /// The point under the middle of the window does not move when the view
    /// turns: rotation is about the viewport centre.
    #[test]
    fn rotating_keeps_the_viewport_centre_fixed() {
        let mut c = cam();
        c.zoom = 0.5;
        c.center = Vec2::new(1000.0, 800.0);
        for angle in [0.3, -1.2, std::f32::consts::PI, 2.9] {
            c.set_rotation(angle);
            let uv = shader_uv(&c, Vec2::ZERO);
            assert!(
                (uv.x - 1000.0 / 4096.0).abs() < 1e-6,
                "u = {} at {angle}",
                uv.x
            );
            assert!(
                (uv.y - 800.0 / 2160.0).abs() < 1e-6,
                "v = {} at {angle}",
                uv.y
            );
            let mid = c.screen_to_image(c.viewport_size * 0.5);
            assert!((mid - c.center).length() < 1e-3, "{mid:?} at {angle}");
        }
    }

    /// `screen_to_image` (what a click goes through) and `clip_to_uv` (what
    /// the shader draws) are the same mapping under rotation, so the pixel the
    /// user clicks on is the pixel they are looking at.
    #[test]
    fn screen_to_image_agrees_with_the_shader_affine_when_rotated() {
        let mut c = cam();
        c.zoom = 0.37;
        c.center = Vec2::new(1234.0, 567.0);
        c.set_rotation(std::f32::consts::FRAC_PI_4);
        for screen in [
            Vec2::new(0.0, 0.0),
            Vec2::new(1280.0, 720.0),
            Vec2::new(300.0, 650.0),
            Vec2::new(900.0, 100.0),
        ] {
            let clip = Vec2::new(screen.x / 1280.0 * 2.0 - 1.0, 1.0 - screen.y / 720.0 * 2.0);
            let via_shader = shader_uv(&c, clip) * c.image_size;
            let via_click = c.screen_to_image(screen);
            assert!(
                (via_shader - via_click).length() < 1e-2,
                "screen {screen:?}: shader {via_shader:?} vs click {via_click:?}"
            );
        }
    }

    #[test]
    fn set_rotation_wraps_and_refuses_nonsense_and_reset_uprights() {
        use std::f32::consts::PI;
        let mut c = cam();
        assert!(!c.is_rotated());
        c.set_rotation(3.0 * PI);
        assert!((c.rotation - PI).abs() < 1e-4, "{}", c.rotation);
        c.set_rotation(-0.25);
        assert!((c.rotation + 0.25).abs() < 1e-6);
        c.set_rotation(f32::NAN);
        assert!((c.rotation + 0.25).abs() < 1e-6, "NaN was written");
        c.set_rotation(f32::INFINITY);
        assert!((c.rotation + 0.25).abs() < 1e-6, "inf was written");
        c.rotate_by(0.25);
        assert_eq!(c.rotation, 0.0);
        c.set_rotation(1.0);
        assert!(c.is_rotated());
        c.reset_rotation();
        assert_eq!(c.rotation, 0.0);
        let (m0, m1) = c.clip_to_uv();
        for v in m0.iter().chain(m1.iter()) {
            assert!(v.is_finite());
        }
    }

    /// Dragging the picture follows the pointer on a turned view: a rightward
    /// drag on a quarter-turned view pans along the image's y axis.
    #[test]
    fn pan_follows_the_pointer_under_rotation() {
        let mut c = cam();
        c.zoom = 2.0;
        c.set_rotation(std::f32::consts::FRAC_PI_2);
        let start = c.center;
        c.pan_screen(Vec2::new(100.0, 0.0));
        let moved = c.center - start;
        assert!(moved.x.abs() < 1e-3, "{moved:?}");
        assert!((moved.y.abs() - 50.0).abs() < 1e-3, "{moved:?}");
        // The zoom anchor still holds under rotation.
        let anchor = Vec2::new(300.0, 200.0);
        let before = c.screen_to_image(anchor);
        c.zoom_at(anchor, 2.0);
        let after = c.screen_to_image(anchor);
        assert!((before - after).length() < 1e-2, "anchor drifted");
    }

    #[test]
    fn checker_frame_carries_the_turn_and_the_viewport_centre() {
        let mut c = cam();
        assert_eq!(c.checker_frame(), [1.0, 0.0, 640.0, 360.0]);
        c.set_rotation(std::f32::consts::FRAC_PI_2);
        let [cos, sin, x, y] = c.checker_frame();
        assert!(cos.abs() < 1e-6 && (sin - 1.0).abs() < 1e-6, "{cos} {sin}");
        assert_eq!((x, y), (640.0, 360.0));
    }

    /// Every (flip_x, flip_y) pair.
    const FLIPS: [(bool, bool); 4] = [(false, false), (true, false), (false, true), (true, true)];

    /// Flip Horizontal is a mirror of the screen about the viewport centre:
    /// the affine of the flipped camera at clip (x, y) is the upright affine
    /// at (-x, y); Flip Vertical at (x, -y).
    #[test]
    fn a_flip_mirrors_the_affine_about_the_viewport_centre() {
        let mut upright = cam();
        upright.zoom = 0.37;
        upright.center = Vec2::new(1234.0, 567.0);
        let mut h = upright.clone();
        h.flip_horizontal();
        let mut v = upright.clone();
        v.flip_vertical();
        for clip in [
            Vec2::new(0.3, -0.7),
            Vec2::new(-1.0, 1.0),
            Vec2::new(0.9, 0.2),
        ] {
            let want_h = shader_uv(&upright, Vec2::new(-clip.x, clip.y));
            let want_v = shader_uv(&upright, Vec2::new(clip.x, -clip.y));
            assert!(shader_uv(&h, clip).abs_diff_eq(want_h, 1e-6), "{clip:?}");
            assert!(shader_uv(&v, clip).abs_diff_eq(want_v, 1e-6), "{clip:?}");
        }
        // Anti-vacuity: a flip is not a no-op.
        assert!(!shader_uv(&h, Vec2::new(0.5, 0.0))
            .abs_diff_eq(shader_uv(&upright, Vec2::new(0.5, 0.0)), 1e-3));
    }

    /// Flipping twice is the identity, on the affine, the click mapping and
    /// the checker frame alike.
    #[test]
    fn flip_then_unflip_is_the_identity() {
        let mut c = cam();
        c.zoom = 0.8;
        c.set_rotation(0.6);
        let before = (c.clip_to_uv(), c.checker_frame());
        let at = Vec2::new(300.0, 650.0);
        let click = c.screen_to_image(at);
        c.flip_horizontal();
        c.flip_vertical();
        assert!(c.is_flipped());
        assert_ne!(c.clip_to_uv(), before.0);
        c.flip_horizontal();
        c.flip_vertical();
        assert!(!c.is_flipped());
        assert_eq!((c.clip_to_uv(), c.checker_frame()), before);
        assert_eq!(c.screen_to_image(at), click);
    }

    /// The click mapping and the drawn affine stay one mapping with a mirror
    /// and a turn in force together, at every flip pair and several angles.
    #[test]
    fn screen_to_image_agrees_with_the_shader_affine_when_flipped_and_turned() {
        for (fx, fy) in FLIPS {
            for angle in [0.0, std::f32::consts::FRAC_PI_2, 0.7, -2.4] {
                let mut c = cam();
                c.zoom = 0.37;
                c.center = Vec2::new(1234.0, 567.0);
                c.set_rotation(angle);
                c.flip_x = fx;
                c.flip_y = fy;
                for screen in [
                    Vec2::new(0.0, 0.0),
                    Vec2::new(1280.0, 720.0),
                    Vec2::new(300.0, 650.0),
                    Vec2::new(900.0, 100.0),
                ] {
                    let clip =
                        Vec2::new(screen.x / 1280.0 * 2.0 - 1.0, 1.0 - screen.y / 720.0 * 2.0);
                    let via_shader = shader_uv(&c, clip) * c.image_size;
                    let via_click = c.screen_to_image(screen);
                    assert!(
                        (via_shader - via_click).length() < 1e-2,
                        "flip ({fx},{fy}) angle {angle} screen {screen:?}: \
                         shader {via_shader:?} vs click {via_click:?}"
                    );
                }
            }
        }
    }

    /// A flipped quarter turn round-trips: flipping horizontally then turning
    /// a quarter turn, then undoing both in the opposite order, is the
    /// upright camera again; and on the way, flip H + 90 degrees is the
    /// same view as 270 degrees + flip V (a mirror identity), so the
    /// composition order is the documented one.
    #[test]
    fn flip_and_a_quarter_turn_round_trip() {
        use std::f32::consts::FRAC_PI_2;
        let mut c = cam();
        c.zoom = 0.5;
        let upright = c.clone();
        c.flip_horizontal();
        c.rotate_by(FRAC_PI_2);
        let mut other = upright.clone();
        other.flip_vertical();
        other.set_rotation(-FRAC_PI_2);
        for clip in [Vec2::new(0.3, -0.7), Vec2::new(-1.0, 1.0)] {
            assert!(
                shader_uv(&c, clip).abs_diff_eq(shader_uv(&other, clip), 1e-5),
                "{clip:?}"
            );
        }
        c.rotate_by(-FRAC_PI_2);
        c.flip_horizontal();
        for clip in [Vec2::new(0.3, -0.7), Vec2::new(-1.0, 1.0)] {
            assert!(shader_uv(&c, clip).abs_diff_eq(shader_uv(&upright, clip), 1e-6));
        }
    }

    /// The checkerboard parity `quad.wgsl` computes, emulated on the CPU from
    /// the frame this camera uploads.
    fn shader_checker_parity(frame: [f32; 4], p: Vec2) -> i64 {
        let [c, s, kx, ky] = frame;
        let d = p - Vec2::new(kx, ky);
        let q = Vec2::new(kx, ky) + Vec2::new(c * d.x + s * d.y, -s * d.x + c * d.y);
        ((q.x / 8.0).floor() as i64 + (q.y / 8.0).floor() as i64).rem_euclid(2)
    }

    /// The checker mirrors with the document: at every pixel centre, the cell
    /// the shader looks up is the cell of the point the document mapping
    /// puts there (`c0 + R(-rotation)·F·(p - c0)`), for every flip pair and
    /// at angles that exercise both branches of the swap trick.
    #[test]
    fn the_checker_frame_mirrors_with_the_document() {
        let mut c = Camera::new(Vec2::splat(32.0), Vec2::new(64.0, 48.0));
        for (fx, fy) in FLIPS {
            for angle in [
                0.0,
                std::f32::consts::FRAC_PI_2,
                -std::f32::consts::FRAC_PI_2,
                0.3,
                2.0,
            ] {
                c.set_rotation(angle);
                c.flip_x = fx;
                c.flip_y = fy;
                let frame = c.checker_frame();
                assert!(frame.iter().all(|v| v.is_finite()), "{frame:?}");
                let centre = c.viewport_size * 0.5;
                let (s, co) = c.rotation.sin_cos();
                let f = c.flip_signs();
                let mut mismatches = 0;
                for y in 0..48 {
                    for x in 0..64 {
                        let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                        let d = (p - centre) * f;
                        let t = centre + Vec2::new(co * d.x + s * d.y, -s * d.x + co * d.y);
                        let want =
                            ((t.x / 8.0).floor() as i64 + (t.y / 8.0).floor() as i64).rem_euclid(2);
                        if shader_checker_parity(frame, p) != want {
                            mismatches += 1;
                        }
                    }
                }
                // A turned lookup can land a hair either side of a cell edge
                // through f32 rounding; a mirror that was ignored misses half
                // the frame.
                assert!(
                    mismatches <= 8,
                    "flip ({fx},{fy}) angle {angle}: {mismatches} checker pixels wrong"
                );
            }
        }
    }

    /// Dragging the picture follows the pointer on a mirrored view: a
    /// rightward drag on a horizontally flipped view moves the camera centre
    /// the other way from the upright view.
    #[test]
    fn pan_follows_the_pointer_under_a_flip() {
        let mut c = cam();
        c.zoom = 2.0;
        c.flip_horizontal();
        let start = c.center;
        let grabbed = c.screen_to_image(Vec2::new(300.0, 200.0));
        c.pan_screen(Vec2::new(100.0, 0.0));
        assert!(
            (c.center.x - (start.x + 50.0)).abs() < 1e-3,
            "{:?}",
            c.center
        );
        // The grabbed image point is under the moved pointer.
        let now = c.screen_to_image(Vec2::new(400.0, 200.0));
        assert!((now - grabbed).length() < 1e-3, "{now:?} vs {grabbed:?}");
    }

    /// W5-A: a viewport that starts inside the surface (the canvas area
    /// between a host's panels) centres the image on the area's centre, and
    /// fits to the area's size, not the surface's.
    #[test]
    fn an_offset_viewport_centres_and_fits_in_its_own_rectangle() {
        let mut c = Camera::new(Vec2::new(320.0, 180.0), Vec2::new(760.0, 780.0));
        c.viewport_origin = Vec2::new(44.0, 70.0);
        c.fit();
        assert!((c.zoom - 760.0 / 320.0).abs() < 1e-6);
        let mid = c.screen_to_image(Vec2::new(44.0 + 380.0, 70.0 + 390.0));
        assert!((mid - Vec2::new(160.0, 90.0)).length() < 1e-3, "{mid:?}");
        // The area's top-left corner is the image point half an area left and
        // up of the centre.
        let corner = c.screen_to_image(c.viewport_origin);
        let want = Vec2::new(160.0, 90.0) - Vec2::new(380.0, 390.0) / c.zoom;
        assert!((corner - want).length() < 1e-3, "{corner:?} vs {want:?}");
        // The checker turns about the same centre the image does.
        let [_, _, kx, ky] = c.checker_frame();
        assert_eq!((kx, ky), (424.0, 460.0));
        // Zooming about a pointer keeps the pointer's image point under it.
        let at = Vec2::new(600.0, 300.0);
        let before = c.screen_to_image(at);
        c.zoom_at(at, 1.7);
        assert!((c.screen_to_image(at) - before).length() < 1e-3);
    }
}
