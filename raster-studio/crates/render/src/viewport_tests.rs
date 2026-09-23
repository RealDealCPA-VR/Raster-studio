//! The canvas area, proven on a real (headless) device.
//!
//! A host that paints panels around the canvas gives the camera the rectangle
//! between them ([`crate::Camera::viewport_origin`] /
//! [`crate::Camera::viewport_size`]). [`crate::Canvas::render_in`] must then
//! fit and centre the document in *that* rectangle and write no document pixel
//! outside it: the part of the surface under a dock stays the flat backdrop.
//! SKIPS (prints and returns) when no adapter can be created, the same policy
//! as `rotation_tests` and the crate's `tests/gpu.rs`.

use glam::Vec2;

use crate::{Camera, Canvas, GpuContext, GpuTexture, OffscreenTarget, Readback};

const SRGB: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const RED: [u8; 4] = [255, 0, 0, 255];

fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("SKIP: no GPU adapter available ({e:#})");
            None
        }
    }
}

fn red_source(gpu: &GpuContext, w: u32, h: u32) -> GpuTexture {
    let rgba: Vec<u8> = RED
        .iter()
        .copied()
        .cycle()
        .take((w * h * 4) as usize)
        .collect();
    GpuTexture::from_rgba8(gpu, w, h, &rgba, "red").expect("upload")
}

fn render_in(gpu: &GpuContext, source: &GpuTexture, camera: &Camera, w: u32, h: u32) -> Readback {
    let target = OffscreenTarget::new(gpu, w, h, SRGB).expect("target");
    let mut canvas = Canvas::new(gpu, SRGB);
    canvas.set_source(gpu, source);
    canvas.update_camera(gpu, camera);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.render_in(&mut encoder, target.view(), (w, h));
    gpu.queue.submit(Some(encoder.finish()));
    target.read_rgba8(gpu).expect("readback")
}

fn close(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    (0..4).all(|i| (i32::from(a[i]) - i32::from(b[i])).abs() <= tol)
}

/// A 160x90 surface whose right 60 px and top 10 px are "panels": the canvas
/// area is 100x80 at (0, 10). A 32x16 red document fitted there fills its
/// width (zoom 100/32) and is centred on the area's centre (50, 50) — not the
/// window's (80, 45) — and every pixel outside the area is the backdrop.
#[test]
fn the_document_is_fitted_centred_and_confined_to_the_canvas_area() {
    let Some(gpu) = gpu() else { return };
    let (sw, sh) = (160u32, 90u32);
    let source = red_source(&gpu, 32, 16);
    let mut camera = Camera::new(Vec2::new(32.0, 16.0), Vec2::new(100.0, 80.0));
    camera.viewport_origin = Vec2::new(0.0, 10.0);
    camera.fit();
    let frame = render_in(&gpu, &source, &camera, sw, sh);

    let backdrop = crate::DEFAULT_BACKDROP_SRGB;
    let backdrop = [backdrop[0], backdrop[1], backdrop[2], 255];
    let (mut min, mut max) = ((u32::MAX, u32::MAX), (0u32, 0u32));
    let mut outside = Vec::new();
    for y in 0..sh {
        for x in 0..sw {
            let p = frame.pixel(x, y);
            let in_area = x < 100 && (10..90).contains(&y);
            if !in_area && !close(p, backdrop, 2) {
                outside.push(((x, y), p));
            }
            if close(p, RED, 3) {
                min = (min.0.min(x), min.1.min(y));
                max = (max.0.max(x), max.1.max(y));
            }
        }
    }
    assert!(
        outside.is_empty(),
        "{} pixels outside the canvas area are not the backdrop; first: {:?}",
        outside.len(),
        &outside[..outside.len().min(6)]
    );
    // The fitted document: 100 px wide, 50 px tall, centred on (50, 50).
    assert!(
        min.0 <= 1 && max.0 >= 98,
        "red spans x {}..{}",
        min.0,
        max.0
    );
    let centre = Vec2::new((min.0 + max.0 + 1) as f32, (min.1 + max.1 + 1) as f32) * 0.5;
    assert!(
        (centre - Vec2::new(50.0, 50.0)).length() <= 1.0,
        "the document is centred on {centre:?}, not the canvas area's (50, 50)"
    );
    assert!(
        (max.1 + 1 - min.1).abs_diff(50) <= 1,
        "red spans y {}..{}",
        min.1,
        max.1
    );
}

/// A viewport left over from a bigger window overhangs the target: the pass
/// still draws (cut back to the target) instead of handing wgpu an
/// out-of-bounds rectangle, and a viewport wholly outside draws only the clear.
#[test]
fn an_overhanging_or_outside_viewport_is_clamped_not_a_validation_error() {
    let Some(gpu) = gpu() else { return };
    let source = red_source(&gpu, 8, 8);
    let mut camera = Camera::new(Vec2::splat(8.0), Vec2::new(200.0, 200.0));
    camera.viewport_origin = Vec2::new(10.0, 10.0);
    camera.zoom = 1.0;
    let frame = render_in(&gpu, &source, &camera, 64, 64);
    assert!(close(
        frame.pixel(0, 0),
        {
            let b = crate::DEFAULT_BACKDROP_SRGB;
            [b[0], b[1], b[2], 255]
        },
        2
    ));
    camera.viewport_origin = Vec2::new(500.0, 500.0);
    let frame = render_in(&gpu, &source, &camera, 64, 64);
    assert!((0..64).all(|x| !close(frame.pixel(x, 32), RED, 3)));
}
