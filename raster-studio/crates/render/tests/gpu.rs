//! Real GPU tests driven through the offscreen readback path.
//!
//! Every test acquires a device via [`render::GpuContext::headless`] and SKIPS
//! (prints and returns) when no adapter can be created, so the suite still runs
//! on a machine with no GPU and no software fallback. On Windows the WARP
//! adapter means these normally do run.

use glam::Vec2;
use render::{
    Camera, Canvas, CompositeParams, CompositePass, GpuContext, GpuTexture, OffscreenTarget,
    Readback,
};

/// Format used for every canvas test: the canvas shader emits linear values and
/// relies on the hardware sRGB encode, so readback bytes are sRGB-encoded.
const SRGB: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Acquire a device, or `None` when this machine has no usable adapter.
fn gpu() -> Option<GpuContext> {
    match pollster::block_on(GpuContext::headless()) {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("SKIP: no GPU adapter available ({e:#})");
            None
        }
    }
}

macro_rules! gpu_or_skip {
    () => {
        match gpu() {
            Some(g) => g,
            None => return,
        }
    };
}

/// Render one camera view of `source` into a `size`x`size` offscreen target.
fn render_canvas(
    gpu: &GpuContext,
    source: &GpuTexture,
    camera: &Camera,
    size: u32,
) -> anyhow::Result<Readback> {
    render_canvas_to(gpu, Some(source), camera, size, SRGB)
}

/// As [`render_canvas`], but into an explicit target format and with an
/// optional source (`None` exercises the clear-only path).
fn render_canvas_to(
    gpu: &GpuContext,
    source: Option<&GpuTexture>,
    camera: &Camera,
    size: u32,
    format: wgpu::TextureFormat,
) -> anyhow::Result<Readback> {
    let target = OffscreenTarget::new(gpu, size, size, format)?;
    let mut canvas = Canvas::new(gpu, format);
    if let Some(source) = source {
        canvas.set_source(gpu, source);
    }
    canvas.update_camera(gpu, camera);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.render(&mut encoder, target.view());
    gpu.queue.submit(Some(encoder.finish()));

    target.read_rgba8(gpu)
}

fn fitted_camera(image: u32, viewport: u32) -> Camera {
    let mut camera = Camera::new(Vec2::splat(image as f32), Vec2::splat(viewport as f32));
    camera.fit();
    camera
}

#[track_caller]
fn assert_near(label: &str, actual: [u8; 4], expected: [u8; 3], tol: i32) {
    let ok = (0..3).all(|i| (i32::from(actual[i]) - i32::from(expected[i])).abs() <= tol);
    assert!(
        ok,
        "{label}: got {actual:?}, expected ~{expected:?} (tolerance {tol})"
    );
}

// ---------------------------------------------------------------------------
// Shader compilation
// ---------------------------------------------------------------------------

/// Both WGSL modules must compile and validate against their real pipeline
/// layouts. A naga error would otherwise surface only as an uncaptured device
/// error at the first frame.
#[test]
fn pipelines_compile_without_validation_errors() {
    let gpu = gpu_or_skip!();
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);

    let _canvas = Canvas::new(&gpu, SRGB);
    let _composite = CompositePass::new(&gpu, wgpu::TextureFormat::Rgba8Unorm);
    // Exercised indirectly by every mipmapped upload, but compile it explicitly
    // so a broken mipmap.wgsl fails here with a readable message.
    let _mips = render::MipGenerator::new(&gpu, SRGB);

    let err = pollster::block_on(gpu.device.pop_error_scope());
    assert!(err.is_none(), "shader/pipeline validation failed: {err:?}");
}

/// `Canvas` hard-codes 8-bit display encoding, so a float target must be
/// refused at construction instead of silently receiving values in the wrong
/// color space. `try_new` reports that as a normal error, which is the path a
/// caller taking its format from adapter capabilities should use.
#[test]
fn canvas_try_new_rejects_a_non_display_target_format() {
    let gpu = gpu_or_skip!();
    let err = Canvas::try_new(&gpu, wgpu::TextureFormat::Rgba16Float)
        .err()
        .expect("Canvas::try_new accepted a float target format");
    assert!(
        err.to_string().contains("Rgba16Float"),
        "error must name the offending format, got: {err}"
    );
    Canvas::try_new(&gpu, SRGB).expect("an 8-bit sRGB target must be accepted");
}

// ---------------------------------------------------------------------------
// Orientation
// ---------------------------------------------------------------------------

/// A 2x2 source with four distinct corners must come back in the same
/// orientation: not mirrored vertically, not smeared across rows.
///
/// This is the regression test for the degenerate `clip_to_uv` affine (which
/// made every screen row sample one texture row) and for the missing V flip
/// (which drew the image upside down).
#[test]
fn source_renders_with_correct_orientation() {
    let gpu = gpu_or_skip!();

    // Row-major from the TOP-LEFT: red, green / blue, white.
    #[rustfmt::skip]
    let pixels: [u8; 16] = [
        255, 0,   0,   255,   0,   255, 0,   255,
        0,   0,   255, 255,   255, 255, 255, 255,
    ];
    let source = GpuTexture::from_rgba8(&gpu, 2, 2, &pixels, "orientation-src");
    let camera = fitted_camera(2, 64);
    let img = render_canvas(&gpu, &source, &camera, 64).expect("render");

    let tl = img.pixel(16, 16);
    let tr = img.pixel(48, 16);
    let bl = img.pixel(16, 48);
    let br = img.pixel(48, 48);

    // Tolerance covers the ~1.5% bilinear bleed from neighbouring quadrants at
    // these probe points; it is far below the distance between the four colors.
    assert_near("top-left", tl, [255, 0, 0], 45);
    assert_near("top-right", tr, [0, 255, 0], 45);
    assert_near("bottom-left", bl, [0, 0, 255], 45);
    assert_near("bottom-right", br, [255, 255, 255], 45);

    // Explicitly: not smeared horizontally, not mirrored vertically.
    assert_ne!(tl, tr, "every column identical: uv.x is not varying");
    assert_ne!(tl, bl, "every row identical: uv.y is not varying");
}

/// The image must fill the viewport it was fitted to, edge to edge, with no
/// checkerboard leaking in and no wrap-around at the borders.
#[test]
fn fitted_image_covers_the_whole_viewport() {
    let gpu = gpu_or_skip!();
    let pixels: Vec<u8> = std::iter::repeat_n([0u8, 128, 255, 255], 64 * 64)
        .flatten()
        .collect();
    let source = GpuTexture::from_rgba8(&gpu, 64, 64, &pixels, "fill-src");
    let camera = fitted_camera(64, 64);
    let img = render_canvas(&gpu, &source, &camera, 64).expect("render");

    for &(x, y) in &[(0, 0), (63, 0), (0, 63), (63, 63), (32, 32)] {
        assert_near(&format!("({x},{y})"), img.pixel(x, y), [0, 128, 255], 6);
    }
}

// ---------------------------------------------------------------------------
// Transparency checkerboard
// ---------------------------------------------------------------------------

/// Transparent pixels INSIDE the image must show the checkerboard, not the
/// clear color. Before the fix the shader only drew the checker outside the
/// image bounds, so a transparent image rendered as a near-black rectangle.
#[test]
fn transparent_pixels_inside_the_image_show_the_checkerboard() {
    let gpu = gpu_or_skip!();
    let pixels = vec![0u8; 8 * 8 * 4]; // fully transparent
    let source = GpuTexture::from_rgba8(&gpu, 8, 8, &pixels, "transparent-src");
    let camera = fitted_camera(8, 64);
    let img = render_canvas(&gpu, &source, &camera, 64).expect("render");

    let light = render_shaders::CHECKER_LIGHT_SRGB_U8;
    let dark = render_shaders::CHECKER_DARK_SRGB_U8;
    let cell = render_shaders::CHECKER_CELL_PX;

    // Every probe is well inside the image, which covers the whole viewport.
    assert_near("cell (0,0)", img.pixel(2, 2), [light; 3], 2);
    assert_near("cell (1,0)", img.pixel(cell + 2, 2), [dark; 3], 2);
    assert_near("cell (0,1)", img.pixel(2, cell + 2), [dark; 3], 2);
    assert_near("cell (1,1)", img.pixel(cell + 2, cell + 2), [light; 3], 2);
    assert_eq!(img.pixel(2, 2)[3], 255, "canvas output must be opaque");
}

/// The checker is measured in framebuffer pixels, so its cell size must not
/// change with the viewport size or the camera zoom.
#[test]
fn checkerboard_cell_size_is_fixed_in_pixels() {
    let gpu = gpu_or_skip!();
    let pixels = vec![0u8; 4 * 4 * 4];
    let source = GpuTexture::from_rgba8(&gpu, 4, 4, &pixels, "checker-src");
    let cell = render_shaders::CHECKER_CELL_PX;
    let light = render_shaders::CHECKER_LIGHT_SRGB_U8;
    let dark = render_shaders::CHECKER_DARK_SRGB_U8;

    for size in [64u32, 128] {
        let camera = fitted_camera(4, size);
        let img = render_canvas(&gpu, &source, &camera, size).expect("render");
        assert_near(
            &format!("{size}px target, last light pixel"),
            img.pixel(cell - 1, 0),
            [light; 3],
            2,
        );
        assert_near(
            &format!("{size}px target, first dark pixel"),
            img.pixel(cell, 0),
            [dark; 3],
            2,
        );
    }
}

/// The checker constants are linear values written to an sRGB target, so they
/// must read back as the sRGB levels they were designed as (0.60 / 0.75) — not
/// as the much brighter sRGB encode of the raw 0.60 / 0.75 literals.
#[test]
fn checkerboard_is_srgb_corrected() {
    let gpu = gpu_or_skip!();
    let pixels = vec![0u8; 4 * 4 * 4];
    let source = GpuTexture::from_rgba8(&gpu, 4, 4, &pixels, "srgb-src");
    let camera = fitted_camera(4, 32);
    let img = render_canvas(&gpu, &source, &camera, 32).expect("render");

    // srgb_encode(0.60) would be 0.795 -> 203; srgb_encode(0.75) -> 0.881 -> 225.
    assert_near("light cell", img.pixel(0, 0), [191; 3], 2);
    assert_near(
        "dark cell",
        img.pixel(render_shaders::CHECKER_CELL_PX, 0),
        [153; 3],
        2,
    );
}

/// The clear color must land on sRGB 0.1 (byte 26) whichever encoding the
/// target has: pre-linearized for an `*-Srgb` format, raw for a plain unorm one.
/// Handing the linear 0.0100 to a non-encoding target renders a near-black
/// backdrop (byte 3).
#[test]
fn clear_color_is_srgb_on_both_target_encodings() {
    let gpu = gpu_or_skip!();
    let camera = fitted_camera(16, 16);
    for format in [SRGB, wgpu::TextureFormat::Rgba8Unorm] {
        let img = render_canvas_to(&gpu, None, &camera, 8, format).expect("render");
        assert_near(&format!("{format:?} clear"), img.pixel(4, 4), [26; 3], 2);
    }
}

/// Clear a canvas whose backdrop has been set, with no source texture.
fn render_backdrop(
    gpu: &GpuContext,
    backdrop: [u8; 3],
    size: u32,
    format: wgpu::TextureFormat,
) -> anyhow::Result<Readback> {
    let target = OffscreenTarget::new(gpu, size, size, format)?;
    let mut canvas = Canvas::new(gpu, format);
    canvas.set_backdrop(backdrop);
    assert_eq!(canvas.backdrop(), backdrop, "the setter did not take");

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.render(&mut encoder, target.view());
    gpu.queue.submit(Some(encoder.finish()));
    target.read_rgba8(gpu)
}

/// The backdrop is a *parameter*, and a parameter nobody has shown reaching the
/// framebuffer is only a field.
///
/// This is the pixel proof behind [`Canvas::set_backdrop`], the API this crate
/// grew so the area around an image could stop being a hardcoded grey:
/// `app-shell` hands it `design::ColorRole::BackgroundCanvas` at start-up and on
/// every theme change, and in Light mode that is #E9E9EE. Both target encodings
/// are checked, because the value has to be linearized for one and left alone
/// for the other — getting that backwards is what renders a light theme's
/// surround near-black.
#[test]
fn a_backdrop_the_host_sets_is_the_colour_the_canvas_clears_to() {
    let gpu = gpu_or_skip!();
    let light = [0xE9, 0xE9, 0xEE];
    for format in [SRGB, wgpu::TextureFormat::Rgba8Unorm] {
        let img = render_backdrop(&gpu, light, 8, format).expect("render");
        assert_near(
            &format!("{format:?} light backdrop"),
            img.pixel(4, 4),
            light,
            2,
        );
        // ...and it really is the value that was set, not the default the
        // canvas starts with.
        assert_ne!(light, render::DEFAULT_BACKDROP_SRGB);
        let dark = [0x1A, 0x1A, 0x1D];
        let img = render_backdrop(&gpu, dark, 8, format).expect("render");
        assert_near(
            &format!("{format:?} dark backdrop"),
            img.pixel(4, 4),
            dark,
            2,
        );
    }
}

/// A plain unorm target performs no sRGB encode, so `quad.wgsl` has to. The
/// bytes it produces must match what the hardware writes for an `*-Srgb`
/// target — otherwise an adapter that exposes no sRGB surface format (the
/// `unwrap_or(caps.formats[0])` path in `app-shell`) renders far too dark.
#[test]
fn linear_and_srgb_targets_produce_the_same_bytes() {
    let gpu = gpu_or_skip!();
    // Half transparent (checker shows through), half opaque mid-grey: exercises
    // the checker constants, the source sample and the blend between them.
    let mut pixels = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for _ in 0..16 {
            if y < 8 {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                pixels.extend_from_slice(&[128, 64, 192, 255]);
            }
        }
    }
    let source = GpuTexture::from_rgba8(&gpu, 16, 16, &pixels, "encode-src");
    let camera = fitted_camera(16, 32);

    let srgb_target = render_canvas_to(&gpu, Some(&source), &camera, 32, SRGB).expect("render");
    let unorm_target = render_canvas_to(
        &gpu,
        Some(&source),
        &camera,
        32,
        wgpu::TextureFormat::Rgba8Unorm,
    )
    .expect("render");

    for y in 0..32 {
        for x in 0..32 {
            let a = srgb_target.pixel(x, y);
            let b = unorm_target.pixel(x, y);
            let close = (0..4).all(|i| (i32::from(a[i]) - i32::from(b[i])).abs() <= 2);
            assert!(
                close,
                "({x},{y}): sRGB target gave {a:?}, unorm target gave {b:?}"
            );
        }
    }
    // Guard against both paths being uniformly wrong: the top half is the
    // transparent checkerboard, so it must show the documented cell values.
    assert_near(
        "unorm target checker",
        unorm_target.pixel(0, 0),
        [render_shaders::CHECKER_LIGHT_SRGB_U8; 3],
        2,
    );
}

// ---------------------------------------------------------------------------
// Mip chain
// ---------------------------------------------------------------------------

/// The downsample must average PREMULTIPLIED taps.
///
/// Source is 2x2: one opaque white texel and three transparent-black ones.
/// Straight-alpha averaging (a single filtered `textureSample`, which is what
/// this shader used to do) blends RGB and A independently and yields
/// `rgb = 0.25` linear (byte 137) — the transparent texels' black leaking into
/// the white one. Premultiplied averaging yields `a = 0.25` and
/// `rgb = (1.0 * 1.0 + 0 + 0 + 0) / 4 / 0.25 = 1.0`, i.e. still pure white.
///
/// This is what makes an alpha image grow a dark fringe as it minifies, so the
/// gap (255 vs 137) is the whole point of the mip chain.
#[test]
fn mip_downsample_is_alpha_weighted() {
    let gpu = gpu_or_skip!();
    #[rustfmt::skip]
    let pixels: [u8; 16] = [
        255, 255, 255, 255,   0, 0, 0, 0,
        0,   0,   0,   0,     0, 0, 0, 0,
    ];
    let tex = GpuTexture::from_rgba8(&gpu, 2, 2, &pixels, "alpha-mip-src");
    assert_eq!(tex.mip_level_count, 2);

    let level1 = tex.read_level(&gpu, 1).expect("read level 1");
    assert_eq!((level1.width(), level1.height()), (1, 1));
    let px = level1.pixel(0, 0);
    assert_near("level 1 color", px, [255, 255, 255], 2);
    assert!(
        (i32::from(px[3]) - 64).abs() <= 2,
        "level 1 alpha is {}, expected ~64 (mean of 1.0, 0, 0, 0)",
        px[3]
    );
}

/// The visible consequence of the test above, through the real canvas: at one
/// mip level of minification, an opaque white texel surrounded by transparent
/// ones must stay bright over the checkerboard.
///
/// The source is 16x16 with one opaque white texel per 2x2 block, so every mip
/// level >= 1 is uniformly white at alpha 0.25. Fitted into an 8x8 viewport that
/// is two source texels per screen pixel, and 8 px is one checker cell
/// ([`render_shaders::CHECKER_CELL_PX`]), so the whole viewport sits on the
/// light cell:
/// * premultiplied mips -> `mix(0.522527, 1.0, 0.25) = 0.6419` -> sRGB 210;
/// * straight-alpha mips -> `mix(0.522527, 0.25, 0.25) = 0.4543` -> sRGB 180.
///
/// The exact byte depends on which LOD the implementation picks, and `quad.wgsl`
/// clamps its uv before sampling, so at the target's outer edge the quad's
/// helper invocations can halve the derivative and pull the sample back toward
/// level 0. Two things follow, and both are deliberate:
/// * the driver-independent claim — that level 1 itself is not darkened — is
///   asserted directly through `read_level`, not inferred from the render;
/// * the render assertion only pins the DIRECTION (closer to 210 than to 180)
///   and only on interior pixels, so it holds on any conformant implementation
///   rather than on one that happens to land on LOD 1.0 exactly.
#[test]
fn minified_alpha_edges_do_not_darken() {
    let gpu = gpu_or_skip!();
    let mut pixels = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16u32 {
        for x in 0..16u32 {
            if x % 2 == 0 && y % 2 == 0 {
                pixels.extend_from_slice(&[255, 255, 255, 255]);
            } else {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    let tex = GpuTexture::from_rgba8(&gpu, 16, 16, &pixels, "fringe-src");
    // The claim proper: the generated level is white, not a darkened grey.
    let level1 = tex.read_level(&gpu, 1).expect("read level 1");
    assert_eq!((level1.width(), level1.height()), (8, 8));
    for y in 0..8 {
        for x in 0..8 {
            assert_near(
                &format!("level 1 must be un-darkened white at ({x},{y})"),
                level1.pixel(x, y),
                [255, 255, 255],
                2,
            );
            assert!(
                (i32::from(level1.pixel(x, y)[3]) - 64).abs() <= 2,
                "level 1 alpha at ({x},{y}) is {}, expected ~64",
                level1.pixel(x, y)[3]
            );
        }
    }

    // ...and the canvas shows it. `PREMULTIPLIED`/`STRAIGHT` are the two values
    // the render can land on; assert which one it is closer to, not the byte.
    const PREMULTIPLIED: i32 = 210;
    const STRAIGHT: i32 = 180;
    assert_eq!(
        render_shaders::CHECKER_CELL_PX,
        8,
        "the 8x8 viewport is no longer exactly one checker cell"
    );
    let camera = fitted_camera(16, 8);
    let img = render_canvas(&gpu, &tex, &camera, 8).expect("render");
    for y in 2..6 {
        for x in 2..6 {
            let px = img.pixel(x, y);
            let v = i32::from(px[0]);
            assert!(
                (v - PREMULTIPLIED).abs() < (v - STRAIGHT).abs(),
                "minified white over checker at ({x},{y}) is {px:?}: \
                 red {v} is closer to the straight-alpha value {STRAIGHT} \
                 than to the premultiplied one {PREMULTIPLIED}"
            );
            // ...and it did not simply come out white, which would satisfy the
            // comparison above without proving the checkerboard blend at all.
            assert!(
                v <= 232,
                "minified white over checker at ({x},{y}) is {px:?}: \
                 the checkerboard is not showing through alpha 0.25"
            );
            assert_eq!(px[1], px[0], "white must stay neutral, got {px:?}");
            assert_eq!(px[2], px[0], "white must stay neutral, got {px:?}");
        }
    }
}

/// Mip levels must keep the image's top-to-bottom order. Rows 0-1 are red and
/// rows 2-3 blue, so level 1's row 0 is red and its row 1 is blue; a flipped
/// downsample swaps them.
#[test]
fn mip_levels_keep_top_to_bottom_order() {
    let gpu = gpu_or_skip!();
    let mut pixels = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4 {
        let color = if y < 2 {
            [255u8, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        };
        for _ in 0..4 {
            pixels.extend_from_slice(&color);
        }
    }
    let tex = GpuTexture::from_rgba8(&gpu, 4, 4, &pixels, "mip-orientation-src");
    assert_eq!(tex.mip_level_count, 3);

    let level1 = tex.read_level(&gpu, 1).expect("read level 1");
    assert_eq!((level1.width(), level1.height()), (2, 2));
    for x in 0..2 {
        assert_near("level 1 row 0", level1.pixel(x, 0), [255, 0, 0], 2);
        assert_near("level 1 row 1", level1.pixel(x, 1), [0, 0, 255], 2);
    }
}

/// Building a `MipGenerator` compiles WGSL and creates a pipeline, so the
/// context hands out one per format rather than one per request.
#[test]
fn mip_generators_are_cached_per_format() {
    let gpu = gpu_or_skip!();
    let a = gpu.mip_generator(SRGB);
    let b = gpu.mip_generator(SRGB);
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "the second request for one format rebuilt the mip pipeline"
    );
    let other = gpu.mip_generator(wgpu::TextureFormat::Rgba8Unorm);
    assert!(
        !std::sync::Arc::ptr_eq(&a, &other),
        "one pipeline cannot serve two target formats"
    );
    assert_eq!(gpu.mip_pipelines_built(), 2);
}

/// ...and image upload must actually GO through that cache. Opening N images
/// may compile `mipmap.wgsl` once, not N times.
#[test]
fn image_upload_compiles_the_mip_pipeline_once() {
    let gpu = gpu_or_skip!();
    assert_eq!(
        gpu.mip_pipelines_built(),
        0,
        "fresh context is not warmed up"
    );

    let pixels = vec![200u8; 8 * 8 * 4];
    for i in 0..4 {
        let tex = GpuTexture::from_rgba8(&gpu, 8, 8, &pixels, &format!("upload-{i}"));
        assert!(tex.mip_level_count > 1);
    }

    assert_eq!(
        gpu.mip_pipelines_built(),
        1,
        "image upload builds its own MipGenerator instead of reusing the cached one"
    );
}

#[test]
fn uploaded_textures_get_a_full_mip_chain() {
    let gpu = gpu_or_skip!();
    let pixels = vec![255u8; 64 * 64 * 4];
    let tex = GpuTexture::from_rgba8(&gpu, 64, 64, &pixels, "mip-src");
    assert_eq!(tex.mip_level_count, 7);
    assert_eq!(tex.texture.mip_level_count(), 7);
}

/// A 1-texel red/black checkerboard minified to ~12.8 source texels per screen
/// pixel must resolve to a uniform half-red: every mip level >= 1 averages the
/// pattern to linear 0.5 red, which an sRGB target stores as ~(188, 0, 0).
///
/// Three failure modes are ruled out at once:
/// * mip levels never allocated -> level 0 aliases, red channel swings to ~234;
/// * mip levels allocated but never written -> they read back as transparent
///   black and the canvas shows the grey checkerboard through them, which fails
///   on the green/blue channels (this is why the pattern is red, not white);
/// * mip levels correct but never sampled -> same aliasing as the first case,
///   which the un-mipped control render pins down explicitly.
///
/// The control is compared by VARIANCE rather than by "the control must alias
/// into a specific band": how far a given driver's level-0 bilinear tap strays
/// from 0.5 is its own business, but that a mip chain reduces the spread of a
/// heavily minified high-frequency pattern holds on any conformant one.
///
/// The 5-pixel viewport is deliberate — it puts the sample points at fractional
/// texel coordinates. A power-of-two viewport lands them exactly on texel
/// boundaries, where a level-0 bilinear tap averages to 0.5 by luck and hides
/// the aliasing entirely.
#[test]
fn minified_detail_resolves_through_the_mip_chain() {
    let gpu = gpu_or_skip!();
    let mut pixels = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64u32 {
        for x in 0..64u32 {
            let v = if (x + y) % 2 == 0 { 255u8 } else { 0 };
            pixels.extend_from_slice(&[v, 0, 0, 255]);
        }
    }

    let camera = fitted_camera(64, 5);
    let mipped = GpuTexture::from_rgba8(&gpu, 64, 64, &pixels, "mipped-src");
    assert!(mipped.mip_level_count > 1, "source has no mip chain");
    let with_mips = render_canvas(&gpu, &mipped, &camera, 5).expect("render");

    let flat = GpuTexture::from_rgba8_with(
        &gpu,
        64,
        64,
        &pixels,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        false,
        "flat-src",
    );
    assert_eq!(flat.mip_level_count, 1);
    let without_mips = render_canvas(&gpu, &flat, &camera, 5).expect("render");

    let is_half_red =
        |px: [u8; 4]| (178..=198).contains(&px[0]) && px[1] <= 12 && px[2] <= 12 && px[3] == 255;
    for y in 0..5 {
        for x in 0..5 {
            let px = with_mips.pixel(x, y);
            assert!(
                is_half_red(px),
                "mipped pixel at ({x},{y}) is {px:?} — expected ~[188, 0, 0, 255]"
            );
        }
    }

    let mipped_variance = red_variance(&with_mips);
    let flat_variance = red_variance(&without_mips);
    assert!(
        mipped_variance < flat_variance,
        "the mip chain did not reduce aliasing: mipped red variance {mipped_variance:.3} \
         is not below the un-mipped render's {flat_variance:.3} \
         (mipped {:?}, un-mipped {:?})",
        with_mips.as_rgba8(),
        without_mips.as_rgba8()
    );
}

/// Variance of the red channel over every pixel of a readback — the aliasing
/// measure for [`minified_detail_resolves_through_the_mip_chain`]. A correctly
/// minified render of a uniform-average pattern is flat (variance ~0); an
/// un-minified one samples the pattern at whatever phase each pixel lands on and
/// scatters.
fn red_variance(img: &Readback) -> f64 {
    let reds: Vec<f64> = (0..img.height())
        .flat_map(|y| (0..img.width()).map(move |x| (x, y)))
        .map(|(x, y)| f64::from(img.pixel(x, y)[0]))
        .collect();
    let mean = reds.iter().sum::<f64>() / reds.len() as f64;
    reds.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / reds.len() as f64
}

// ---------------------------------------------------------------------------
// Composite
// ---------------------------------------------------------------------------

/// Non-`Rgba8Unorm` would drag the sRGB transfer function into the assertions;
/// the composite math is defined on linear values, so test it on a linear
/// format where the expected bytes are exact.
const LINEAR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn solid(gpu: &GpuContext, rgba: [u8; 4], label: &str) -> GpuTexture {
    let pixels: Vec<u8> = std::iter::repeat_n(rgba, 4 * 4).flatten().collect();
    GpuTexture::from_rgba8_with(gpu, 4, 4, &pixels, LINEAR, false, label)
}

/// A 4x4 texture whose four ROWS are `rows[0..4]`, top to bottom. Vertically
/// asymmetric on purpose: a mirrored pass shows up as a row-order swap.
fn row_striped(gpu: &GpuContext, rows: [[u8; 4]; 4], label: &str) -> GpuTexture {
    let mut pixels = Vec::with_capacity(4 * 4 * 4);
    for row in rows {
        for _ in 0..4 {
            pixels.extend_from_slice(&row);
        }
    }
    GpuTexture::from_rgba8_with(gpu, 4, 4, &pixels, LINEAR, false, label)
}

fn composite(
    gpu: &GpuContext,
    dst: &GpuTexture,
    src: &GpuTexture,
    params: CompositeParams,
) -> anyhow::Result<Readback> {
    let target = OffscreenTarget::new(gpu, 4, 4, LINEAR)?;
    let pass = CompositePass::new(gpu, LINEAR);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    pass.render(
        gpu,
        &mut encoder,
        &dst.view,
        &src.view,
        target.view(),
        params,
    );
    gpu.queue.submit(Some(encoder.finish()));
    target.read_rgba8(gpu)
}

/// Regression: `Multiply` over a fully transparent backdrop used to evaluate
/// `B(0, Cs) = 0` and make the source vanish. Per W3C Compositing 1 §9.2 the
/// blend function is weighted by the backdrop alpha, so with an empty backdrop
/// the source must pass through untouched.
#[test]
fn multiply_over_transparent_backdrop_keeps_the_source() {
    let gpu = gpu_or_skip!();
    let dst = solid(&gpu, [0, 0, 0, 0], "empty-dst");
    let src = solid(&gpu, [255, 64, 0, 255], "opaque-src");
    let out = composite(
        &gpu,
        &dst,
        &src,
        CompositeParams {
            blend_index: 1, // Multiply
            opacity: 1.0,
        },
    )
    .expect("composite");

    assert_near("multiply over empty", out.pixel(2, 2), [255, 64, 0], 2);
    assert_eq!(out.pixel(2, 2)[3], 255, "alpha must be fully opaque");
}

/// Darken has the same failure mode as Multiply (B(0, Cs) = 0).
#[test]
fn darken_over_transparent_backdrop_keeps_the_source() {
    let gpu = gpu_or_skip!();
    let dst = solid(&gpu, [0, 0, 0, 0], "empty-dst");
    let src = solid(&gpu, [200, 200, 200, 255], "grey-src");
    let out = composite(
        &gpu,
        &dst,
        &src,
        CompositeParams {
            blend_index: 4, // Darken
            opacity: 1.0,
        },
    )
    .expect("composite");
    assert_near("darken over empty", out.pixel(1, 1), [200, 200, 200], 2);
}

/// With an OPAQUE backdrop the blend function must apply in full, so the fix
/// above cannot have been "always ignore the backdrop".
#[test]
fn multiply_over_opaque_backdrop_still_multiplies() {
    let gpu = gpu_or_skip!();
    let dst = solid(&gpu, [128, 255, 255, 255], "opaque-dst");
    let src = solid(&gpu, [255, 128, 0, 255], "opaque-src");
    let out = composite(
        &gpu,
        &dst,
        &src,
        CompositeParams {
            blend_index: 1,
            opacity: 1.0,
        },
    )
    .expect("composite");

    // (128/255 * 1.0, 1.0 * 128/255, 1.0 * 0.0) -> (128, 128, 0)
    assert_near("multiply over opaque", out.pixel(2, 2), [128, 128, 0], 3);
}

/// A half-opaque backdrop must interpolate between the two behaviours above,
/// which pins the `(1 - alpha_b) * Cs + alpha_b * B(Cb, Cs)` weighting itself.
#[test]
fn multiply_over_half_opaque_backdrop_interpolates() {
    let gpu = gpu_or_skip!();
    // Premultiplied: red at alpha 0.5 -> rgb (128, 0, 0), a 128.
    let dst = solid(&gpu, [128, 0, 0, 128], "half-dst");
    let src = solid(&gpu, [0, 0, 255, 255], "blue-src");
    let out = composite(
        &gpu,
        &dst,
        &src,
        CompositeParams {
            blend_index: 1,
            opacity: 1.0,
        },
    )
    .expect("composite");

    // Cb = (1, 0, 0), Cs = (0, 0, 1), alpha_b = 0.502.
    // B = Cb*Cs = (0,0,0); cs = 0.498*(0,0,1) + 0.502*(0,0,0) = (0, 0, 0.498).
    // src.a = 1, so out_rgb = cs and out_a = 1.
    assert_near("multiply over half-opaque", out.pixel(2, 2), [0, 0, 127], 3);
    assert_eq!(out.pixel(2, 2)[3], 255);
}

/// `composite.wgsl` must agree with the crate's orientation convention: clip
/// `y = +1` is the TOP of the target and reads row 0 of the bound textures.
///
/// Every other composite fixture here is a uniform color, under which a
/// vertical mirror is invisible — this one is deliberately asymmetric, so
/// deleting the `1.0 -` from `composite.wgsl`'s `vs_main` reverses the rows and
/// fails on the very first assertion.
///
/// Normal blend over a fully transparent, fully opaque source is an exact
/// identity pass on a linear target, so the expected bytes are the input bytes.
#[test]
fn composite_preserves_vertical_orientation() {
    let gpu = gpu_or_skip!();
    let rows = [
        [255u8, 0, 0, 255],   // row 0: red
        [0, 255, 0, 255],     // row 1: green
        [0, 0, 255, 255],     // row 2: blue
        [255, 255, 255, 255], // row 3: white
    ];
    let src = row_striped(&gpu, rows, "striped-src");
    let dst = solid(&gpu, [0, 0, 0, 0], "empty-dst");
    let out = composite(
        &gpu,
        &dst,
        &src,
        CompositeParams {
            blend_index: 0, // Normal
            opacity: 1.0,
        },
    )
    .expect("composite");

    for (y, expected) in rows.iter().enumerate() {
        for x in 0..4 {
            assert_eq!(
                out.pixel(x, y as u32),
                *expected,
                "row {y} column {x} is not the source's row {y} — composite mirrored or smeared"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Readback harness
// ---------------------------------------------------------------------------

/// Readback must un-pad rows correctly. A width whose stride (4 * 17 = 68 B) is
/// not a multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` exercises that path.
#[test]
fn readback_unpads_rows_for_non_aligned_widths() {
    let gpu = gpu_or_skip!();
    let target = OffscreenTarget::new(&gpu, 17, 5, LINEAR).expect("target");
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear-only"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view(),
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    gpu.queue.submit(Some(encoder.finish()));

    let img = target.read_rgba8(&gpu).expect("readback");
    assert_eq!(img.width(), 17);
    assert_eq!(img.height(), 5);
    assert_eq!(img.as_rgba8().len(), 17 * 5 * 4);
    for y in 0..5 {
        for x in 0..17 {
            assert_eq!(
                img.pixel(x, y),
                [255, 0, 0, 255],
                "row padding leaked at ({x},{y})"
            );
        }
    }
}

/// Unsupported readback formats must be reported, not silently misread.
#[test]
fn readback_rejects_non_rgba8_formats() {
    let gpu = gpu_or_skip!();
    let target =
        OffscreenTarget::new(&gpu, 4, 4, wgpu::TextureFormat::Rgba16Float).expect("target");
    let err = target
        .read_rgba8(&gpu)
        .expect_err("must reject Rgba16Float");
    assert!(
        err.to_string().contains("Rgba16Float"),
        "unhelpful error: {err}"
    );
}

#[test]
fn offscreen_target_rejects_empty_size() {
    let gpu = gpu_or_skip!();
    assert!(OffscreenTarget::new(&gpu, 0, 16, LINEAR).is_err());
    assert!(OffscreenTarget::new(&gpu, 16, 0, LINEAR).is_err());
}
