//! Rasterisation: coverage masks, the glyph cache, synthesis, linear fills.

use text_engine::{
    fill_linear, rasterize, render_linear, shape, synthetic_bold_radius, CoverageMask, FontLibrary,
    FontSlant, FontWeight, GlyphRasterCache, StyleOverride, StyleRun, TextRun,
};

fn library() -> FontLibrary {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library
}

#[test]
fn a_rendered_mask_is_non_empty_and_tight_within_its_bounds() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let shaped = shape(&mut library, &TextRun::point("Hello", "DejaVu Sans", 32.0));
    let mask = rasterize(&mut library, &mut cache, &shaped);

    assert!(!mask.is_empty());
    assert!(mask.total_coverage() > 0, "the mask must carry ink");

    let ink = mask.ink_bounds().expect("there is ink");
    assert!(
        mask.rect().contains_rect(&ink),
        "ink {ink:?} must lie inside the mask {:?}",
        mask.rect()
    );
    assert_eq!(
        ink,
        mask.rect(),
        "the mask is sized to its ink, so the two must coincide"
    );

    // Every non-zero sample is addressable through the layer-space accessor,
    // and nothing outside the mask reports coverage.
    let mut counted = 0u64;
    for row in 0..mask.height as i32 {
        for col in 0..mask.width as i32 {
            let value = mask.coverage(mask.origin_x + col, mask.origin_y + row);
            counted += u64::from(value);
        }
    }
    assert_eq!(counted, mask.total_coverage());
    assert_eq!(mask.coverage(mask.origin_x - 1, mask.origin_y), 0);
    assert_eq!(
        mask.coverage(mask.origin_x, mask.origin_y + mask.height as i32),
        0
    );

    // The ink sits where the layout said it would: inside the line box, and
    // within the line's horizontal extent.
    let line = &shaped.lines[0];
    assert!(ink.y >= line.top - 1.0 && ink.bottom() <= line.bottom + 1.0);
    assert!(ink.x >= line.x_min - 2.0 && ink.right() <= line.x_max + 2.0);
}

/// Sum of the coverage in one row of a mask.
fn row_sum(mask: &CoverageMask, row: u32) -> u64 {
    (0..mask.width)
        .map(|col| u64::from(mask.coverage(mask.origin_x + col as i32, mask.origin_y + row as i32)))
        .sum()
}

/// Sum of the coverage in one column of a mask.
fn column_sum(mask: &CoverageMask, col: u32) -> u64 {
    (0..mask.height)
        .map(|row| u64::from(mask.coverage(mask.origin_x + col as i32, mask.origin_y + row as i32)))
        .sum()
}

#[test]
fn the_mask_box_is_pinned_to_golden_geometry() {
    // Golden values for the embedded DejaVu Sans face at 32 px, in the same
    // style as the golden advances in tests/shaping.rs. `ink_bounds() ==
    // rect()` is a tautology — the mask is *sized* from the glyph rects the ink
    // lives in — so the box itself has to be pinned by number, or an off-by-one
    // in the bounds computation would silently clip the bottom row and right
    // column of every glyph with the suite still green.
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let shaped = shape(&mut library, &TextRun::point("Hello", "DejaVu Sans", 32.0));
    let mask = rasterize(&mut library, &mut cache, &shaped);

    assert_eq!(
        (mask.origin_x, mask.origin_y, mask.width, mask.height),
        (3, 6, 77, 24),
        "the mask box for \"Hello\" at 32 px is fixed by the face's outlines"
    );
    assert_eq!(mask.total_coverage(), 155_717);

    // Every edge band of the box carries ink, and carries a known amount of it.
    // Shrinking or growing the box by one pixel on any side shifts a different
    // row or column into these positions and changes all four numbers.
    assert_eq!(row_sum(&mask, 0), 1_468, "top row");
    assert_eq!(row_sum(&mask, mask.height - 1), 6_065, "bottom row");
    assert_eq!(column_sum(&mask, 0), 5_060, "left column");
    assert_eq!(column_sum(&mask, mask.width - 1), 303, "right column");
    for edge in [
        row_sum(&mask, 0),
        row_sum(&mask, mask.height - 1),
        column_sum(&mask, 0),
        column_sum(&mask, mask.width - 1),
    ] {
        assert!(edge > 0, "the box is tight: no blank edge band");
    }

    // Nothing spills over the edges of the box.
    for step in 1..=2 {
        assert_eq!(row_sum_outside(&mask, -step), 0);
        assert_eq!(row_sum_outside(&mask, mask.height as i32 - 1 + step), 0);
    }
}

/// Coverage reported for a row that lies outside the mask, plus one column of
/// slack on each side.
fn row_sum_outside(mask: &CoverageMask, row: i32) -> u64 {
    (-1..=mask.width as i32)
        .map(|col| u64::from(mask.coverage(mask.origin_x + col, mask.origin_y + row)))
        .sum()
}

#[test]
fn a_decoration_pads_the_box_and_leaves_the_glyph_ink_strictly_interior() {
    // An underline starts at the line's x_min — the pen, not the first glyph's
    // ink — so the rule pushes the mask's left edge out past where any glyph
    // draws. That gives a box with a known blank margin above the rule, which
    // is the one case where the ink is *strictly* inside the box it is
    // reported in, and it pins the left edge independently of the goldens
    // above.
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let run = TextRun::point("under", "DejaVu Sans", 40.0).with_runs(vec![StyleRun::new(
        0,
        5,
        StyleOverride::default().with_underline(true),
    )]);
    let shaped = shape(&mut library, &run);
    let mask = rasterize(&mut library, &mut cache, &shaped);
    let rule = shaped.decorations[0].rect;

    assert_eq!(mask.origin_x, 0, "the rule starts at the pen, x = 0");
    assert!(
        mask.coverage(mask.origin_x, (rule.y + rule.height / 2.0) as i32) > 200,
        "the rule reaches the very first column of the box"
    );

    // Ink above the rule, in mask-relative columns.
    let last_glyph_row = rule.y.floor() as i32 - mask.origin_y;
    assert!(last_glyph_row > 0);
    let mut first_inked = None;
    for col in 0..mask.width as i32 {
        let inked = (0..last_glyph_row)
            .any(|row| mask.coverage(mask.origin_x + col, mask.origin_y + row) != 0);
        if inked {
            first_inked = Some(col);
            break;
        }
    }
    assert_eq!(
        first_inked,
        Some(3),
        "the 'u' begins exactly three columns inside the box the rule opened; \
         a one-pixel shift of the box's left edge changes this"
    );
}

#[test]
fn baselines_snap_to_whole_pixels_so_glyphs_share_one_cached_image() {
    // Subpixel positioning is horizontal only, and this is the test that says
    // so: four distinct sub-pixel *vertical* offsets rasterise to one cached
    // image and byte-identical coverage, while four horizontal ones do not.
    let mut library = library();
    let mut cache = GlyphRasterCache::new();

    let mut vertical = Vec::new();
    for step in [0.0_f32, 0.25, 0.5, 0.75] {
        let shaped = shape(
            &mut library,
            &TextRun::point("o", "DejaVu Sans", 24.0).with_origin([0.0, step]),
        );
        vertical.push(rasterize(&mut library, &mut cache, &shaped));
    }
    assert_eq!(cache.len(), 1, "one glyph image serves every baseline");
    assert_eq!((cache.misses(), cache.hits()), (1, 3));
    for mask in &vertical[1..] {
        assert_eq!(
            mask.data, vertical[0].data,
            "a vertical subpixel move must not change the coverage"
        );
        assert_eq!(mask.origin_x, vertical[0].origin_x);
    }
    // The baseline is *floored*, so the mask steps by whole pixels and it
    // steps when the baseline crosses a pixel boundary, not when it passes the
    // half-way mark. For this string at this size the baseline sits about
    // 0.6 px into a pixel, so the crossing falls between the second and third
    // quarter-pixel offsets.
    let steps: Vec<i32> = vertical.iter().map(|m| m.origin_y).collect();
    assert_eq!(steps[1], steps[0], "0.25 px does not reach the next pixel");
    assert_eq!(steps[2], steps[0] + 1, "0.5 px crosses it");
    assert_eq!(steps[3], steps[2], "0.75 px is still that same pixel");

    // The horizontal axis is the one that really is subpixel-positioned.
    cache.clear();
    for step in [0.0_f32, 0.25, 0.5, 0.75] {
        let shaped = shape(
            &mut library,
            &TextRun::point("o", "DejaVu Sans", 24.0).with_origin([step, 0.0]),
        );
        rasterize(&mut library, &mut cache, &shaped);
    }
    assert_eq!(cache.len(), 4, "four horizontal bins, four images");
}

#[test]
fn the_mask_follows_the_layer_origin() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let base = shape(&mut library, &TextRun::point("H", "DejaVu Sans", 32.0));
    let moved = shape(
        &mut library,
        &TextRun::point("H", "DejaVu Sans", 32.0).with_origin([40.0, 25.0]),
    );
    let a = rasterize(&mut library, &mut cache, &base);
    let b = rasterize(&mut library, &mut cache, &moved);

    assert_eq!(b.origin_x - a.origin_x, 40);
    assert_eq!(b.origin_y - a.origin_y, 25);
    assert_eq!(a.width, b.width);
    assert_eq!(a.height, b.height);
    assert_eq!(a.data, b.data, "a whole-pixel move must not re-rasterise");

    // The same must hold when the move puts the baseline *above* layer-space
    // y = 0: a whole-pixel translation is a whole-pixel translation, with no
    // rounding-towards-zero seam at the origin.
    let up = shape(
        &mut library,
        &TextRun::point("H", "DejaVu Sans", 32.0).with_origin([-40.0, -100.0]),
    );
    let c = rasterize(&mut library, &mut cache, &up);
    assert!(c.origin_y < 0, "the test needs a negative baseline");
    assert_eq!(c.origin_x - a.origin_x, -40);
    assert_eq!(c.origin_y - a.origin_y, -100);
    assert_eq!(a.width, c.width);
    assert_eq!(a.height, c.height);
    assert_eq!(
        a.data, c.data,
        "a negative whole-pixel move must not re-rasterise either"
    );
}

#[test]
fn subpixel_positions_are_cached_separately_and_differ() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let on_grid = shape(&mut library, &TextRun::point("o", "DejaVu Sans", 24.0));
    let half_pixel = shape(
        &mut library,
        &TextRun::point("o", "DejaVu Sans", 24.0).with_origin([0.5, 0.0]),
    );

    let a = rasterize(&mut library, &mut cache, &on_grid);
    assert_eq!((cache.misses(), cache.hits()), (1, 0));
    let b = rasterize(&mut library, &mut cache, &half_pixel);
    assert_eq!(
        (cache.misses(), cache.hits()),
        (2, 0),
        "half a pixel to the right is a different rasterisation"
    );
    assert_eq!(cache.len(), 2);
    assert_ne!(
        a.data, b.data,
        "subpixel positioning must actually change the coverage"
    );

    let again = rasterize(&mut library, &mut cache, &on_grid);
    assert_eq!(
        (cache.misses(), cache.hits()),
        (2, 1),
        "the second pass must be served from the cache"
    );
    assert_eq!(again.data, a.data);

    cache.clear();
    assert!(cache.is_empty());
    assert_eq!((cache.misses(), cache.hits()), (0, 0));
}

#[test]
fn faux_bold_thickens_the_glyph_and_can_be_refused() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let plain = shape(&mut library, &TextRun::point("H", "DejaVu Sans", 64.0));
    let bold_request = TextRun::point("H", "DejaVu Sans", 64.0).with_runs(vec![StyleRun::new(
        0,
        1,
        StyleOverride::default().with_weight(FontWeight::BOLD),
    )]);
    let bold = shape(&mut library, &bold_request);

    let thin = rasterize(&mut library, &mut cache, &plain);
    let thick = rasterize(&mut library, &mut cache, &bold);

    assert!(
        thick.total_coverage() > thin.total_coverage(),
        "emboldening must add ink: {} vs {}",
        thick.total_coverage(),
        thin.total_coverage()
    );
    assert!(
        thick.width > thin.width,
        "the smear widens the glyph: {} vs {}",
        thick.width,
        thin.width
    );
    assert_eq!(thick.height, thin.height, "faux bold is horizontal only");

    // Where the extra ink lands, not just how much of it there is. The smear
    // is symmetric about the original stem: the emboldened glyph must grow by
    // exactly the radius on each side and stay centred, or every faux-bold
    // glyph drifts out of alignment with the advance the shaper gave it.
    let radius = synthetic_bold_radius(64.0) as f32;
    assert!(
        (radius - 2.0).abs() < f32::EPSILON,
        "3% of 64 px rounds to 2"
    );
    let plain_ink = thin.ink_bounds().expect("ink");
    let bold_ink = thick.ink_bounds().expect("ink");
    assert!(
        (bold_ink.x - (plain_ink.x - radius)).abs() < f32::EPSILON,
        "the left edge must move by exactly -{radius}: {} -> {}",
        plain_ink.x,
        bold_ink.x
    );
    assert!(
        (bold_ink.right() - (plain_ink.right() + radius)).abs() < f32::EPSILON,
        "the right edge must move by exactly +{radius}: {} -> {}",
        plain_ink.right(),
        bold_ink.right()
    );
    let plain_centre = plain_ink.x + plain_ink.width / 2.0;
    let bold_centre = bold_ink.x + bold_ink.width / 2.0;
    assert!(
        (bold_centre - plain_centre).abs() < 1e-3,
        "the two ink centres must coincide: {plain_centre} vs {bold_centre}"
    );
    assert!((plain_ink.y - bold_ink.y).abs() < f32::EPSILON);

    let mut opted_out = bold_request;
    opted_out.style.allow_synthetic_bold = false;
    let untouched = shape(&mut library, &opted_out);
    let untouched = rasterize(&mut library, &mut cache, &untouched);
    assert_eq!(
        untouched.data, thin.data,
        "refusing faux bold must give the regular face back exactly"
    );
}

#[test]
fn faux_italic_skews_the_glyph() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let plain = shape(&mut library, &TextRun::point("H", "DejaVu Sans", 64.0));
    let italic = shape(
        &mut library,
        &TextRun::point("H", "DejaVu Sans", 64.0).with_runs(vec![StyleRun::new(
            0,
            1,
            StyleOverride::default().with_slant(FontSlant::Italic),
        )]),
    );

    let upright = rasterize(&mut library, &mut cache, &plain);
    let slanted = rasterize(&mut library, &mut cache, &italic);
    assert!(
        slanted.width > upright.width,
        "a 14 degree skew widens the bitmap: {} vs {}",
        slanted.width,
        upright.width
    );
    assert_eq!(slanted.height, upright.height);
    assert_ne!(slanted.data, upright.data);
}

#[test]
fn whitespace_and_empty_strings_rasterise_to_nothing_without_panicking() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    for text in ["", "   ", "\t", "\n", "\n\n", " \t \n "] {
        let shaped = shape(&mut library, &TextRun::point(text, "DejaVu Sans", 32.0));
        let mask = rasterize(&mut library, &mut cache, &shaped);
        assert!(mask.is_empty(), "{text:?} has no ink");
        assert_eq!(mask.total_coverage(), 0);
        assert!(mask.ink_bounds().is_none());
        assert_eq!(mask.coverage(0, 0), 0);

        let image = render_linear(&mut library, &mut cache, &shaped);
        assert!(image.is_empty());
        assert_eq!(image.pixel(0, 0), [0.0; 4]);
    }
}

#[test]
fn absurd_and_non_finite_origins_do_not_panic() {
    // The scaler splits a pen position into an i32 pixel and a subpixel bin,
    // and its rounding adds one to that pixel — so an unclamped position near
    // i32::MAX overflows. A NaN origin makes every rule NaN, which must not be
    // allowed to stretch the mask from zero out to the glyphs either.
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let underlined = |origin: [f32; 2]| {
        TextRun::point("Hi", "DejaVu Sans", 24.0)
            .with_runs(vec![StyleRun::new(
                0,
                2,
                StyleOverride::default().with_underline(true),
            )])
            .with_origin(origin)
    };

    for origin in [
        [1.0e12_f32, 1.0e12],
        [-1.0e12, -1.0e12],
        // Only one axis is absurd, and only just: the glyph pens clamp while
        // the rule's rectangle is still finite, still has a measurable height
        // at this magnitude, and sits millions of pixels away from them. The
        // two have to be kept in the same range or the mask is asked to span
        // the whole gap between them.
        [0.0, 1.2e7],
        [1.2e7, 0.0],
        [0.0, -1.2e7],
        [0.0, 1.0e12],
        [1.0e12, 0.0],
        [f32::MAX, f32::MAX],
        [f32::INFINITY, f32::NEG_INFINITY],
        [f32::NAN, f32::NAN],
    ] {
        let shaped = shape(&mut library, &underlined(origin));
        let mask = rasterize(&mut library, &mut cache, &shaped);
        assert!(
            mask.width < 100_000 && mask.height < 100_000,
            "origin {origin:?} produced a {}x{} mask",
            mask.width,
            mask.height
        );
        let image = render_linear(&mut library, &mut cache, &shaped);
        assert_eq!((image.width, image.height), (mask.width, mask.height));
    }

    // A NaN origin still draws the text; it just draws it at zero.
    let shaped = shape(&mut library, &underlined([f32::NAN, f32::NAN]));
    let mask = rasterize(&mut library, &mut cache, &shaped);
    assert!(mask.total_coverage() > 0);
}

#[test]
fn decorations_are_rasterised_into_the_mask() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let plain = shape(&mut library, &TextRun::point("under", "DejaVu Sans", 40.0));
    let underlined = shape(
        &mut library,
        &TextRun::point("under", "DejaVu Sans", 40.0).with_runs(vec![StyleRun::new(
            0,
            5,
            StyleOverride::default().with_underline(true),
        )]),
    );

    let without = rasterize(&mut library, &mut cache, &plain);
    let with = rasterize(&mut library, &mut cache, &underlined);
    assert!(with.total_coverage() > without.total_coverage());
    assert!(
        with.rect().bottom() > without.rect().bottom(),
        "the rule extends the mask below the glyphs"
    );

    // The rule itself is opaque along its whole length.
    let rule = underlined.decorations[0].rect;
    let row = (rule.y + rule.height / 2.0).floor() as i32;
    let samples = (rule.x.ceil() as i32..rule.right().floor() as i32)
        .map(|x| with.coverage(x, row))
        .collect::<Vec<_>>();
    assert!(!samples.is_empty());
    assert!(
        samples.iter().all(|&v| v > 200),
        "the underline must be solid across the run"
    );
}

#[test]
fn coverage_is_used_as_linear_alpha() {
    let mut half = CoverageMask::new(0, 0, 1, 1);
    half.data[0] = 128;

    let white = fill_linear(&half, [1.0, 1.0, 1.0, 1.0]);
    let pixel = white.pixel(0, 0);
    let expected = 128.0 / 255.0;
    for channel in pixel {
        assert!(
            (channel - expected).abs() < 1e-5,
            "half coverage of white is {expected} in linear light, got {channel}"
        );
    }
    // A gamma-space blend would have produced sRGB 0.502, i.e. linear 0.216.
    assert!(
        pixel[0] > 0.4,
        "the fill must not be doing an sRGB-space blend"
    );

    // Premultiplication: a half-transparent red at full coverage.
    let mut full = CoverageMask::new(0, 0, 1, 1);
    full.data[0] = 255;
    let red = fill_linear(&full, [1.0, 0.0, 0.0, 0.5]);
    assert_eq!(red.pixel(0, 0), [0.5, 0.0, 0.0, 0.5]);
}

#[test]
fn each_style_run_is_filled_with_its_own_colour() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let run = TextRun::point("AB", "DejaVu Sans", 40.0).with_runs(vec![StyleRun::new(
        1,
        2,
        StyleOverride::default().with_color([1.0, 0.0, 0.0, 1.0]),
    )]);
    let shaped = shape(&mut library, &run);
    let image = render_linear(&mut library, &mut cache, &shaped);

    let mut black = 0;
    let mut red = 0;
    for pixel in image.data.chunks_exact(4) {
        if pixel[3] < 0.9 {
            continue;
        }
        if pixel[0] > 0.9 && pixel[1] < 0.1 {
            red += 1;
        } else if pixel[0] < 0.1 {
            black += 1;
        }
    }
    assert!(black > 0, "the A must be filled black");
    assert!(red > 0, "the B must be filled red");

    // Alpha never exceeds one, and premultiplied channels never exceed alpha.
    for pixel in image.data.chunks_exact(4) {
        assert!(pixel[3] <= 1.0 + 1e-6);
        for channel in &pixel[..3] {
            assert!(*channel <= pixel[3] + 1e-6);
        }
    }
}

#[test]
fn overlapping_glyphs_of_one_colour_do_not_double_darken() {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    // Heavy negative tracking forces the glyphs to overlap.
    let mut run = TextRun::point("WWW", "DejaVu Sans", 48.0);
    run.style.tracking = -600.0;
    let shaped = shape(&mut library, &run);
    let image = render_linear(&mut library, &mut cache, &shaped);

    assert!(!image.is_empty());
    for pixel in image.data.chunks_exact(4) {
        assert!(
            pixel[3] <= 1.0 + 1e-6,
            "coverage must saturate at one, got {}",
            pixel[3]
        );
    }
}
