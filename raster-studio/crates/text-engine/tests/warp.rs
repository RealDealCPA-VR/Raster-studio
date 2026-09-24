//! W9-K: Warp Text, Type on a Path, Convert to Shape outlines and loading a
//! font file - asserted on the rasterised pixels, not on the maps alone.

use layer_model::text::{TextPath, TextWarp, WarpStyle};
use text_engine::{
    outline_svg, rasterize, register_session_font, shape, with_shared_library, CoverageMask,
    FontLibrary, GlyphRasterCache, TextRun,
};

fn library() -> FontLibrary {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library
}

fn mask_of(run: &TextRun) -> CoverageMask {
    let mut library = library();
    let mut cache = GlyphRasterCache::new();
    let shaped = shape(&mut library, run);
    rasterize(&mut library, &mut cache, &shaped)
}

/// Ink blobs separated by empty columns, left to right, each as its
/// coverage-weighted centroid `(x, y)` in layer space.
fn column_blobs(mask: &CoverageMask) -> Vec<(f32, f32)> {
    let mut blobs = Vec::new();
    let mut acc: Option<(f64, f64, f64)> = None;
    for col in 0..mask.width as i32 {
        let x = mask.origin_x + col;
        let mut column = (0.0f64, 0.0f64, 0.0f64);
        for row in 0..mask.height as i32 {
            let y = mask.origin_y + row;
            let c = f64::from(mask.coverage(x, y));
            column.0 += c * (f64::from(x) + 0.5);
            column.1 += c * (f64::from(y) + 0.5);
            column.2 += c;
        }
        if column.2 > 0.0 {
            let a = acc.get_or_insert((0.0, 0.0, 0.0));
            a.0 += column.0;
            a.1 += column.1;
            a.2 += column.2;
        } else if let Some(a) = acc.take() {
            blobs.push(((a.0 / a.2) as f32, (a.1 / a.2) as f32));
        }
    }
    if let Some(a) = acc {
        blobs.push(((a.0 / a.2) as f32, (a.1 / a.2) as f32));
    }
    blobs
}

/// The circle through three points: `(cx, cy, r)`.
fn circle(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> (f32, f32, f32) {
    let d = 2.0 * (a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1));
    let sq = |p: (f32, f32)| p.0 * p.0 + p.1 * p.1;
    let cx = (sq(a) * (b.1 - c.1) + sq(b) * (c.1 - a.1) + sq(c) * (a.1 - b.1)) / d;
    let cy = (sq(a) * (c.0 - b.0) + sq(b) * (a.0 - c.0) + sq(c) * (b.0 - a.0)) / d;
    (cx, cy, ((a.0 - cx).powi(2) + (a.1 - cy).powi(2)).sqrt())
}

const BARS: &str = "I   I   I   I   I   I   I";

/// Arc bends the baseline: flat, the seven bars' ink centres share one y;
/// under Arc at +50 % the middle bar rides well above the end bars and every
/// centre sits on one circle.
#[test]
fn arc_warp_bends_the_baseline_onto_a_circle() {
    let flat = TextRun::point(BARS, "DejaVu Sans", 48.0);
    let blobs = column_blobs(&mask_of(&flat));
    assert_eq!(blobs.len(), 7, "seven separate bars: {blobs:?}");
    let y0 = blobs[0].1;
    assert!(
        blobs.iter().all(|b| (b.1 - y0).abs() < 0.5),
        "flat text has a straight baseline: {blobs:?}"
    );

    let mut arced = flat.clone();
    arced.warp = TextWarp::new(WarpStyle::Arc);
    let blobs = column_blobs(&mask_of(&arced));
    assert_eq!(blobs.len(), 7, "the bars stay separate under the arc");
    let (first, mid, last) = (blobs[0], blobs[3], blobs[6]);
    assert!(
        first.1 - mid.1 > 20.0 && last.1 - mid.1 > 20.0,
        "the middle rides above the ends: {blobs:?}"
    );
    assert!((first.1 - last.1).abs() < 1.5, "the arc is symmetric");
    let (cx, cy, r) = circle(first, mid, last);
    for b in &blobs {
        let d = ((b.0 - cx).powi(2) + (b.1 - cy).powi(2)).sqrt();
        assert!(
            (d - r).abs() < 2.0,
            "every glyph centre follows the arc: {b:?} is {d} from ({cx}, {cy}), r = {r}"
        );
    }

    // A negative bend bulges the other way.
    arced.warp.bend = -0.5;
    let blobs = column_blobs(&mask_of(&arced));
    assert!(blobs[0].1 - blobs[3].1 < -20.0, "a negative bend sags");
}

/// Every style changes the pixels, and None is byte-identical to no warp.
#[test]
fn every_warp_style_moves_ink_and_none_changes_nothing() {
    let flat = TextRun::point("Warp me", "DejaVu Sans", 40.0);
    let base = mask_of(&flat);
    let mut none = flat.clone();
    none.warp = TextWarp::default();
    assert_eq!(mask_of(&none), base, "no warp renders the unwarped bitmap");
    for style in WarpStyle::ALL {
        let mut run = flat.clone();
        run.warp = TextWarp::new(style);
        let warped = mask_of(&run);
        assert!(warped.total_coverage() > 0, "{style:?} still draws");
        assert_ne!(warped, base, "{style:?} changes the ink");
    }
}

/// Type on a circle: every inked pixel sits in the ring just outside the
/// circle (glyphs stand on it), and the text wraps round a real arc of it.
#[test]
fn type_on_a_circle_path_places_the_glyphs_on_the_circle() {
    let (cx, cy, radius) = (300.0f32, 300.0f32, 120.0f32);
    let points: Vec<[f32; 2]> = (0..180)
        .map(|i| {
            // Starting at the top and running clockwise on screen.
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 / 180.0 * std::f32::consts::TAU;
            [cx + radius * a.cos(), cy + radius * a.sin()]
        })
        .collect();
    let size = 32.0;
    let mut run = TextRun::point("ROUND AND ROUND", "DejaVu Sans", size);
    run.path = Some(TextPath {
        points,
        closed: true,
        start: 0.0,
    });
    let mask = mask_of(&run);
    assert!(mask.total_coverage() > 0);
    let mut angles = Vec::new();
    for row in 0..mask.height as i32 {
        for col in 0..mask.width as i32 {
            let (x, y) = (mask.origin_x + col, mask.origin_y + row);
            if mask.coverage(x, y) < 128 {
                continue;
            }
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = (dx * dx + dy * dy).sqrt();
            assert!(
                d > radius - 3.0 && d < radius + size,
                "ink at ({x}, {y}) is {d} from the centre, off the circle's ring"
            );
            angles.push(dy.atan2(dx));
        }
    }
    let lo = angles.iter().copied().fold(f32::MAX, f32::min);
    let hi = angles.iter().copied().fold(f32::MIN, f32::max);
    assert!(
        (hi - lo).to_degrees() > 90.0,
        "the text runs round the circle, not in a clump: {}..{}",
        lo.to_degrees(),
        hi.to_degrees()
    );
    // And the glyphs are no longer where the flat layout put them.
    let flat = mask_of(&TextRun::point("ROUND AND ROUND", "DejaVu Sans", size));
    assert_ne!(flat, mask, "the path moved the glyphs");
}

/// Convert to Shape's outline is the drawn text: the SVG's coordinate
/// extent matches the mask's ink bounds, warped or not.
#[test]
fn the_outline_svg_covers_the_rendered_ink() {
    for warp in [TextWarp::default(), TextWarp::new(WarpStyle::Bulge)] {
        let mut run = TextRun::point("Shape", "DejaVu Sans", 64.0);
        run.warp = warp;
        let mut library = library();
        let shaped = shape(&mut library, &run);
        let svg = outline_svg(&mut library, &shaped);
        assert!(svg.starts_with('M'), "path data: {svg:.40}");
        let nums: Vec<f32> = svg
            .split(|c: char| c.is_ascii_alphabetic() || c.is_whitespace())
            .filter_map(|t| t.parse().ok())
            .collect();
        let xs: Vec<f32> = nums.iter().step_by(2).copied().collect();
        let ys: Vec<f32> = nums.iter().skip(1).step_by(2).copied().collect();
        let min = |v: &[f32]| v.iter().copied().fold(f32::MAX, f32::min);
        let max = |v: &[f32]| v.iter().copied().fold(f32::MIN, f32::max);
        let ink = mask_of(&run).ink_bounds().expect("ink");
        assert!((min(&xs) - ink.x).abs() < 2.0, "{warp:?} left edge");
        assert!((max(&xs) - ink.right()).abs() < 2.0, "{warp:?} right edge");
        assert!((min(&ys) - ink.y).abs() < 2.0, "{warp:?} top edge");
        assert!(
            (max(&ys) - ink.bottom()).abs() < 2.0,
            "{warp:?} bottom edge"
        );
    }
}

/// Loading a font file makes its family available: from a file on disk into
/// a library, and as a session font into the shared library.
#[test]
fn loading_a_font_file_makes_its_family_available() {
    let dir = std::env::temp_dir().join(format!("raster-w9k-font-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("DejaVuSerifCondensed.ttf");
    std::fs::write(&file, dejavu::serif_condensed::regular()).unwrap();

    let mut probe = FontLibrary::empty();
    let ids = probe.load_file(&file).expect("readable");
    assert_eq!(ids.len(), 1, "one face in the file");
    let family = probe.families()[0].name.clone();
    assert!(probe.has_family(&family));

    let mut not_a_font = FontLibrary::empty();
    let junk = dir.join("junk.ttf");
    std::fs::write(&junk, b"not a font").unwrap();
    assert!(not_a_font.load_file(&junk).unwrap().is_empty());
    assert_eq!(register_session_font(b"not a font".to_vec()), 0);

    assert_eq!(
        register_session_font(std::fs::read(&file).unwrap()),
        1,
        "a session font"
    );
    assert!(
        with_shared_library(|library| library.has_family(&family)),
        "the shared library picks the session font up"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
