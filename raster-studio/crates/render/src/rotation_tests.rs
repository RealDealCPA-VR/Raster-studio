//! Rotated-view rendering, proven on a real (headless) device.
//!
//! [`crate::Camera::rotation`] is a claim about pixels: a view turned a
//! quarter turn clockwise must come out of the canvas pass as the upright
//! frame turned a quarter turn clockwise — image *and* transparency
//! checkerboard together, since the checker belongs to the document and not
//! to the window. Every test here SKIPS (prints and returns) when no adapter
//! can be created, the same policy as the crate's `tests/gpu.rs`.

use glam::Vec2;

use crate::{Camera, Canvas, GpuContext, GpuTexture, OffscreenTarget, Readback};

const SRGB: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("SKIP: no GPU adapter available ({e:#})");
            None
        }
    }
}

/// A 32x32 source that is different in every quadrant, with a transparent
/// hole in the top-left one so the checkerboard shows through it: red,
/// green, blue and white — nothing about it survives a turn unchanged.
fn quadrant_source(gpu: &GpuContext) -> GpuTexture {
    let n = 32u32;
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let i = ((y * n + x) * 4) as usize;
            let px: [u8; 4] = match (x < n / 2, y < n / 2) {
                // The hole: the checker shows through here.
                (true, true) if x < 8 && y < 8 => [0, 0, 0, 0],
                (true, true) => [255, 0, 0, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [0, 0, 255, 255],
                (false, false) => [255, 255, 255, 255],
            };
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    GpuTexture::from_rgba8(gpu, n, n, &rgba, "quadrants").expect("upload")
}

fn render(gpu: &GpuContext, source: &GpuTexture, camera: &Camera, size: u32) -> Readback {
    let target = OffscreenTarget::new(gpu, size, size, SRGB).expect("target");
    let mut canvas = Canvas::new(gpu, SRGB);
    canvas.set_source(gpu, source);
    canvas.update_camera(gpu, camera);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.render(&mut encoder, target.view());
    gpu.queue.submit(Some(encoder.finish()));
    target.read_rgba8(gpu).expect("readback")
}

fn close(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    (0..4).all(|i| (i32::from(a[i]) - i32::from(b[i])).abs() <= tol)
}

/// A quarter turn clockwise of the view is a quarter turn clockwise of the
/// frame: pixel `(x, y)` of the turned frame is pixel `(y, S-1-x)` of the
/// upright one (the transpose, then a vertical flip — a pure transpose would
/// be a reflection, and Rotate View is not a mirror). Compared over every
/// pixel, which covers the image, the pasteboard beyond it and the
/// checkerboard through the hole.
#[test]
fn a_quarter_turn_renders_the_upright_frame_turned_a_quarter_turn() {
    let Some(gpu) = gpu() else { return };
    let source = quadrant_source(&gpu);
    // 64 px: eight 8-px checker cells a side, so the window-anchored checker
    // has the OPPOSITE parity after a quarter turn about the centre — a
    // checker that stayed nailed to the window fails this comparison.
    let size = 64u32;
    let mut upright = Camera::new(Vec2::splat(32.0), Vec2::splat(size as f32));
    // Half the viewport, so a pasteboard border surrounds the image.
    upright.zoom = 1.0;
    let mut turned = upright.clone();
    turned.set_rotation(std::f32::consts::FRAC_PI_2);

    let a = render(&gpu, &source, &upright, size);
    let b = render(&gpu, &source, &turned, size);

    // Anti-vacuity: the two frames differ, so an ignored rotation cannot pass.
    assert_ne!(a, b, "the turned frame is identical to the upright one");

    let mut mismatches = Vec::new();
    for y in 0..size {
        for x in 0..size {
            let want = a.pixel(y, size - 1 - x);
            let got = b.pixel(x, y);
            if !close(got, want, 3) {
                mismatches.push(((x, y), got, want));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} pixels differ from the turned upright frame; first: {:?}",
        mismatches.len(),
        size * size,
        &mismatches[..mismatches.len().min(6)]
    );

    // The named landmarks, so a failure reads as geometry rather than as a
    // pixel count. The image spans 16..48 on screen. Clockwise, its red
    // top-left quadrant lands top-RIGHT (and the checker hole, doc [0,8)², at
    // the very corner: screen [40,48)x[16,24)); its green top-right quadrant
    // lands bottom-right.
    let red = b.pixel(36, 28);
    assert!(
        close(red, [255, 0, 0, 255], 3),
        "top-right of the image is {red:?}, not red"
    );
    let light = render_shaders::CHECKER_LIGHT_SRGB_U8;
    let dark = render_shaders::CHECKER_DARK_SRGB_U8;
    let hole = b.pixel(44, 20);
    assert!(
        close(hole, [light, light, light, 255], 3) || close(hole, [dark, dark, dark, 255], 3),
        "the hole did not turn with the picture: {hole:?}"
    );
    let green = b.pixel(44, 44);
    assert!(
        close(green, [0, 255, 0, 255], 3),
        "bottom-right of the image is {green:?}, not green"
    );
}

/// Zero rotation is exactly the frame the crate always drew: the field's
/// default changes nothing.
#[test]
fn zero_rotation_is_the_classic_frame() {
    let Some(gpu) = gpu() else { return };
    let source = quadrant_source(&gpu);
    let camera = Camera::new(Vec2::splat(32.0), Vec2::splat(64.0));
    assert_eq!(camera.rotation, 0.0);
    let img = render(&gpu, &source, &camera, 64);
    // Image spans 16..48; red top-left quadrant, checker cell (2,2) light.
    assert!(
        close(img.pixel(28, 28), [255, 0, 0, 255], 3),
        "{:?}",
        img.pixel(28, 28)
    );
    let light = render_shaders::CHECKER_LIGHT_SRGB_U8;
    assert!(
        close(img.pixel(18, 18), [light, light, light, 255], 3),
        "{:?}",
        img.pixel(18, 18)
    );
}

/// Every pixel of `got` against `want(x, y)`, which names the pixel of the
/// reference frame that must appear at `(x, y)`.
fn assert_frame_is(
    got: &Readback,
    reference: &Readback,
    size: u32,
    want: impl Fn(u32, u32) -> (u32, u32),
    what: &str,
) {
    let mut mismatches = Vec::new();
    for y in 0..size {
        for x in 0..size {
            let (rx, ry) = want(x, y);
            let expected = reference.pixel(rx, ry);
            let actual = got.pixel(x, y);
            if !close(actual, expected, 3) {
                mismatches.push(((x, y), actual, expected));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "{what}: {} of {} pixels differ; first: {:?}",
        mismatches.len(),
        size * size,
        &mismatches[..mismatches.len().min(6)]
    );
}

/// View > Flip Horizontal renders the upright frame column-reversed: pixel
/// `(x, y)` of the flipped frame is pixel `(S-1-x, y)` of the upright one —
/// image, pasteboard and the checkerboard through the hole alike. With eight
/// 8-px cells a side, a checker that stayed nailed to the window has the
/// opposite parity under the reversal and fails this comparison.
#[test]
fn flip_horizontal_renders_the_column_reversed_frame() {
    let Some(gpu) = gpu() else { return };
    let source = quadrant_source(&gpu);
    let size = 64u32;
    let upright = Camera::new(Vec2::splat(32.0), Vec2::splat(size as f32));
    let mut flipped = upright.clone();
    flipped.flip_horizontal();

    let a = render(&gpu, &source, &upright, size);
    let b = render(&gpu, &source, &flipped, size);
    assert_ne!(a, b, "the flipped frame is identical to the upright one");
    assert_frame_is(&b, &a, size, |x, y| (size - 1 - x, y), "flip H");

    // Landmarks: the red top-left quadrant is now top-RIGHT, and the hole
    // (doc [0,8)^2) sits at screen [40,48)x[16,24) showing the checker.
    assert!(
        close(b.pixel(36, 28), [255, 0, 0, 255], 3),
        "{:?}",
        b.pixel(36, 28)
    );
    let light = render_shaders::CHECKER_LIGHT_SRGB_U8;
    let dark = render_shaders::CHECKER_DARK_SRGB_U8;
    let hole = b.pixel(44, 20);
    assert!(
        close(hole, [light, light, light, 255], 3) || close(hole, [dark, dark, dark, 255], 3),
        "the hole did not mirror with the picture: {hole:?}"
    );

    // And Flip Vertical is the row-reversed frame.
    let mut v = upright.clone();
    v.flip_vertical();
    let c = render(&gpu, &source, &v, size);
    assert_frame_is(&c, &a, size, |x, y| (x, size - 1 - y), "flip V");
}

/// Flip then unflip is the identity: the frame is exactly the upright one.
#[test]
fn flip_then_unflip_renders_the_upright_frame() {
    let Some(gpu) = gpu() else { return };
    let source = quadrant_source(&gpu);
    let size = 64u32;
    let upright = Camera::new(Vec2::splat(32.0), Vec2::splat(size as f32));
    let mut cam = upright.clone();
    cam.flip_horizontal();
    cam.flip_vertical();
    cam.flip_horizontal();
    cam.flip_vertical();
    assert_eq!(
        render(&gpu, &source, &cam, size),
        render(&gpu, &source, &upright, size)
    );
}

/// A mirror composes with a quarter turn: Flip Horizontal plus a quarter
/// turn clockwise renders the upright frame transposed (screen = F·R·doc, so
/// pixel `(x, y)` shows upright pixel `(y, x)`), and undoing both returns
/// the upright frame.
#[test]
fn flip_plus_a_quarter_turn_round_trips() {
    let Some(gpu) = gpu() else { return };
    let source = quadrant_source(&gpu);
    let size = 64u32;
    let upright = Camera::new(Vec2::splat(32.0), Vec2::splat(size as f32));
    let mut cam = upright.clone();
    cam.flip_horizontal();
    cam.rotate_by(std::f32::consts::FRAC_PI_2);

    let a = render(&gpu, &source, &upright, size);
    let b = render(&gpu, &source, &cam, size);
    assert_frame_is(&b, &a, size, |x, y| (y, x), "flip H + 90");

    cam.rotate_by(-std::f32::consts::FRAC_PI_2);
    cam.flip_horizontal();
    assert_eq!(render(&gpu, &source, &cam, size), a);
}
