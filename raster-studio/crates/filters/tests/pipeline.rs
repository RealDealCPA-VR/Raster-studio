//! End-to-end checks that the crate is usable from outside: a real tile grid
//! goes in, a filter chain runs, and a real tile grid comes out.
//!
//! These use images larger than one `raster::TILE_SIZE` in both axes and not a
//! multiple of it, so the parallel band decomposition and the partial edge
//! tiles are both exercised.

use filters::blur::MAX_BLUR_RADIUS;
use filters::{
    add_noise, box_blur, clouds, convolve, despeckle, emboss, find_edges, gaussian_blur,
    gradient_fill, high_pass, lens_flare, maximum, median, minimum, mosaic, motion_blur, offset,
    pinch, polar_coordinates, radial_blur, ripple, shear, solarize, spherize, twirl, unsharp_mask,
    wave, zigzag, CloudParams, EdgeMode, FilterBuffer, Gradient, GradientKind, Interpolation,
    Kernel, LensFlare, NoiseDistribution, PolarMode, RadialBlur, Sampling, Wave, ZigZag,
    ZigZagKind,
};
use raster::{PixelFormat, TileGrid};

/// 300x260: wider and taller than one 256-pixel tile, and a multiple of
/// neither, so every edge tile is partial.
const W: u32 = 300;
const H: u32 = 260;

fn source_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            bytes.push((x % 256) as u8);
            bytes.push((y % 256) as u8);
            bytes.push(((x + y) % 256) as u8);
            bytes.push(255);
        }
    }
    bytes
}

fn source_grid() -> TileGrid {
    TileGrid::from_rgba8(W, H, &source_bytes()).expect("well-formed source")
}

#[test]
fn a_filter_chain_round_trips_through_the_tile_grid() {
    let grid = source_grid();
    let buf = FilterBuffer::from_tile_grid(&grid).expect("8-bit grid decodes");
    assert_eq!(buf.dimensions(), (W, H));

    let stage1 = gaussian_blur(&buf, 2.5, EdgeMode::Clamp);
    let stage2 = unsharp_mask(&stage1, 0.8, 1.5, 0.0, EdgeMode::Clamp);
    let stage3 = add_noise(&stage2, 0.02, NoiseDistribution::Gaussian, true, 1234);

    let out = stage3.to_tile_grid().expect("re-encodes");
    assert_eq!(out.dimensions(), (W, H));
    let bytes = out.to_rgba8().expect("8-bit grid");
    assert_eq!(bytes.len(), (W * H * 4) as usize);
    // The chain must have done something, and must have kept the image opaque.
    assert_ne!(bytes, source_bytes());
    assert!(bytes.chunks_exact(4).all(|p| p[3] == 255));
}

#[test]
fn decoding_and_re_encoding_without_a_filter_is_lossless() {
    let grid = source_grid();
    let buf = FilterBuffer::from_tile_grid(&grid).unwrap();
    assert_eq!(
        buf.to_tile_grid().unwrap().to_rgba8().unwrap(),
        source_bytes()
    );
}

#[test]
fn a_non_rgba8_grid_is_refused_rather_than_guessed_at() {
    let grid = TileGrid::new(8, 8, PixelFormat::Rgba16);
    assert!(FilterBuffer::from_tile_grid(&grid).is_err());
}

/// The headline invariant, run across a tile boundary at full crate scope:
/// every blur of a constant image is that constant.
#[test]
fn blurs_of_a_flat_field_are_flat_across_tile_boundaries() {
    let flat = FilterBuffer::filled(W, H, [0.31, 0.42, 0.53, 0.75]).unwrap();
    let sampling = Sampling::new(EdgeMode::Clamp, Interpolation::Bilinear);
    let candidates = [
        ("gaussian", gaussian_blur(&flat, 6.0, EdgeMode::Clamp)),
        ("box", box_blur(&flat, 7, EdgeMode::Mirror)),
        ("motion", motion_blur(&flat, 33.0, 12.0, sampling)),
        ("radial", radial_blur(&flat, &RadialBlur::spin(W, H, 25.0))),
        ("median", median(&flat, 5, EdgeMode::Wrap)),
        ("mosaic", mosaic(&flat, 16)),
    ];
    for (name, out) in candidates {
        for (i, px) in out.pixels().iter().enumerate() {
            for (c, expected) in [0.31f32, 0.42, 0.53, 0.75].iter().enumerate() {
                assert!(
                    (px[c] - expected).abs() < 1e-5,
                    "{name}: pixel {i} channel {c} drifted to {px:?}"
                );
            }
        }
    }
}

/// Nothing in the crate may panic on a degenerate image or a runaway
/// parameter. This is one sweep over every public filter.
#[test]
fn no_filter_panics_on_degenerate_input() {
    let sampling = Sampling::new(EdgeMode::Mirror, Interpolation::Bicubic);
    let kernel = Kernel::new(3, vec![1.0, 2.0, 1.0, 2.0, 4.0, 2.0, 1.0, 2.0, 1.0]).unwrap();
    let cloud = CloudParams::default();

    for (w, h) in [(1u32, 1u32), (1, 9), (9, 1), (0, 5), (5, 0), (0, 0)] {
        let src = FilterBuffer::filled(w, h, [0.4, 0.5, 0.6, 0.9]).unwrap();
        let c = (w as f32 * 0.5, h as f32 * 0.5);

        // Blur.
        gaussian_blur(&src, 1e9, EdgeMode::Clamp);
        box_blur(&src, u32::MAX, EdgeMode::Wrap);
        motion_blur(&src, 1e9, 1e9, sampling);
        radial_blur(&src, &RadialBlur::zoom(w, h, 5.0));
        filters::lens_blur(&src, 1e9, 7, 12.0, EdgeMode::Mirror);
        filters::surface_blur(&src, u32::MAX, 1e9, EdgeMode::Clamp);

        // Sharpen.
        unsharp_mask(&src, 1e9, 1e9, -1.0, EdgeMode::Wrap);
        filters::smart_sharpen(&src, 5.0, 3.0, 1e-9, EdgeMode::Mirror);

        // Noise.
        add_noise(&src, 1e9, NoiseDistribution::Uniform, false, 0);
        despeckle(&src, EdgeMode::Wrap);
        median(&src, u32::MAX, EdgeMode::Mirror);
        filters::dust_and_scratches(&src, u32::MAX, 1e-9, EdgeMode::Clamp);
        filters::reduce_noise(&src, 1e9, 2.0, EdgeMode::Wrap);

        // Distort.
        pinch(&src, c, 1e9, 5.0, sampling);
        spherize(&src, c, 1e-9, -5.0, sampling);
        twirl(&src, c, 1e9, 1e4, sampling);
        ripple(&src, 1e9, 1e-9, sampling);
        shear(&src, 1e9, -1e9, sampling);
        wave(&src, &Wave::default(), sampling);
        zigzag(
            &src,
            &ZigZag {
                kind: ZigZagKind::AroundCenter,
                center: c,
                radius: 1e9,
                amount: 1e9,
                ridges: 1e9,
            },
            sampling,
        );
        polar_coordinates(&src, PolarMode::RectangularToPolar, sampling);
        polar_coordinates(&src, PolarMode::PolarToRectangular, sampling);

        // Stylize.
        emboss(&src, 1e9, 1e9, 1e9, sampling);
        find_edges(&src, EdgeMode::Wrap);
        filters::oil_paint(&src, u32::MAX, u32::MAX, EdgeMode::Mirror);
        solarize(&src);
        filters::wind(
            &src,
            filters::WindDirection::FromRight,
            1e9,
            7,
            EdgeMode::Wrap,
        );
        filters::diffuse(
            &src,
            u32::MAX,
            filters::DiffuseMode::LightenOnly,
            7,
            EdgeMode::Mirror,
        );

        // Pixelate.
        mosaic(&src, u32::MAX);
        filters::crystallize(&src, u32::MAX, 3);
        filters::pointillize(&src, u32::MAX, 3, [0.0; 4]);
        filters::color_halftone(&src, 1e9, [15.0, 75.0, 0.0]);

        // Render.
        clouds(w, h, &cloud).unwrap();
        filters::difference_clouds(&src, &cloud);
        filters::fibers(w, h, &filters::FiberParams::default()).unwrap();
        lens_flare(&src, &LensFlare::default());
        gradient_fill(
            w,
            h,
            &Gradient::two_stop(
                GradientKind::Diamond,
                (0.0, 0.0),
                (0.0, 0.0),
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 0.0],
            ),
        )
        .unwrap();

        // Other.
        high_pass(&src, 1e9, EdgeMode::Wrap);
        offset(&src, i64::MIN, i64::MAX, EdgeMode::Mirror);
        minimum(&src, u32::MAX, EdgeMode::Wrap);
        maximum(&src, u32::MAX, EdgeMode::Mirror);
        convolve(&src, &kernel, EdgeMode::Clamp);
    }
}

/// A blur kernel is bounded, so an absurd sigma cannot allocate without limit.
#[test]
fn kernel_size_is_bounded() {
    let k = filters::blur::gaussian_kernel(f32::MAX);
    assert!(k.len() <= 2 * MAX_BLUR_RADIUS as usize + 1);
    let sum: f64 = k.iter().map(|v| *v as f64).sum();
    assert!((sum - 1.0).abs() < 1e-6, "sum {sum}");
}

/// Two seeds, two images; one seed, one image — checked through the public
/// API on a buffer large enough to be split across parallel bands.
#[test]
fn seeded_filters_are_reproducible_at_scale() {
    let src = FilterBuffer::from_tile_grid(&source_grid()).unwrap();
    assert_eq!(
        add_noise(&src, 0.1, NoiseDistribution::Gaussian, false, 42),
        add_noise(&src, 0.1, NoiseDistribution::Gaussian, false, 42)
    );
    assert_ne!(
        add_noise(&src, 0.1, NoiseDistribution::Gaussian, false, 42),
        add_noise(&src, 0.1, NoiseDistribution::Gaussian, false, 43)
    );
    let p = CloudParams {
        seed: 9,
        scale: 40.0,
        ..CloudParams::default()
    };
    assert_eq!(clouds(W, H, &p).unwrap(), clouds(W, H, &p).unwrap());
}
