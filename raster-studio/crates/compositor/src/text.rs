//! Text layers: from a [`layer_model::TextLayer`] to premultiplied linear
//! pixels.
//!
//! Nothing here shapes or rasterises anything itself. `text-engine` already
//! owns cluster formation, ligatures, kerning, bidi, fallback and the glyph
//! scaler; this module's whole job is to make that reachable from a composite —
//! to answer "what pixels does this text layer have, in its own space, at this
//! mip level?" so that [`crate::composite`] can mask, transform, blend and
//! style it like any other layer.
//!
//! # Why there is a process-wide font library
//!
//! Shaping needs `&mut FontLibrary` (fontdb memory-maps faces lazily and the
//! glyph scaler caches per face), and a composite runs on a rayon pool from a
//! `&Ctx`. Threading a library through [`crate::composite_region`] would change
//! the signature every caller in the workspace uses, so the library lives here
//! behind a [`Mutex`], created once. That is also what makes the rendered-text
//! cache shared rather than per-frame: a text layer is re-shaped when its
//! string, family or size changes, not once per tile per frame.
//!
//! The library is seeded from the machine's installed fonts. [`load_font`] adds
//! a face the machine does not have — an application's bundled UI font, or a
//! test's fixture — and invalidates what was cached against the old set.
//!
//! # Known limits
//!
//! * [`layer_model::TextLayer`] carries a string, a family and a size and
//!   nothing else, so a text layer composites in `text_engine::CharStyle`'s
//!   default colour (opaque black) with default tracking, ligatures and
//!   kerning. Per-run colour, weight and slant exist in `text_engine::TextRun`
//!   but have nowhere to be *stored*; recolouring a text layer is done with a
//!   colour-overlay layer effect, which does persist.
//! * The layer's origin is the run's origin, which is `(0, 0)`: text is
//!   positioned by the layer's transform, not by a field of its own.
//! * Below one hundredth of a pixel of em size — reachable only at an extreme
//!   mip level — the run is dropped rather than shaped, because the shaper
//!   clamps at `MIN_FONT_SIZE_PX` and shaping *up* to the clamp would draw the
//!   text larger the further out the user zoomed.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use layer_model::TextLayer;
use raster::PixelRect;
use text_engine::{
    render_linear, shape, FontLibrary, GlyphRasterCache, LinearImage, TextRun, MIN_FONT_SIZE_PX,
};

/// How many rendered runs are kept before the cache is dropped wholesale.
///
/// A document has tens of text layers, not thousands, and each entry is one
/// run's ink rather than a canvas, so a flat cap with a clear-on-overflow
/// policy costs nothing to reason about and cannot leak.
const MAX_CACHED_RUNS: usize = 64;

/// Everything about a run that decides its pixels, and nothing else.
/// Everything about a run that decides its pixels, and nothing else (card
/// 020). The whole persisted layer hashes into `content` - base style, spans,
/// paragraph, frame, kerning - because a weight-only or colour-only edit must
/// shape differently even though text, family and size all match. That was the
/// warm-cache defect: the old key named three fields, so a restyled layer was
/// served its old pixels.
#[derive(Clone, PartialEq, Eq, Hash)]
struct RunKey {
    text: String,
    family: String,
    /// Em size **at the mip level being composited**, by bit pattern so the key
    /// is `Eq + Hash`.
    size_bits: u32,
    /// Bumped whenever a face is added, so entries shaped against the old font
    /// set can never be handed out.
    generation: u64,
    /// The full layer's content hash (card 019): style, spans, paragraph,
    /// frame, kerning.
    content: u64,
}

struct Engine {
    library: FontLibrary,
    glyphs: GlyphRasterCache,
    runs: HashMap<RunKey, Arc<LinearImage>>,
    generation: u64,
}

/// One hash over every layer field that decides rendered pixels (card 020).
/// Floats hash by bit pattern so -0.0 and 0.0 stay distinct the way the shaper
/// would treat them; enums hash by discriminant.
/// The canonical text fingerprint: every pixel-affecting field of the layer,
/// in one place. The run cache keys on it (`RunKey.content`) and the tile
/// cache hashes it through `hash_layer_props` — card 020's "one fingerprint,
/// not two incomplete field lists", which card 022's stretch field finally
/// made testable end to end (a width-only edit must move pixels).
pub(crate) fn hash_layer(layer: &TextLayer) -> u64 {
    let mut h = DefaultHasher::new();
    layer.text.hash(&mut h);
    layer.font_family.hash(&mut h);
    layer.size_px.to_bits().hash(&mut h);
    layer.style.weight.0.hash(&mut h);
    match layer.style.slant {
        layer_model::text::Slant::Normal => 0u8.hash(&mut h),
        layer_model::text::Slant::Italic => 1u8.hash(&mut h),
    }
    match layer.style.stretch {
        layer_model::text::Stretch::UltraCondensed => 0u8.hash(&mut h),
        layer_model::text::Stretch::ExtraCondensed => 1u8.hash(&mut h),
        layer_model::text::Stretch::Condensed => 2u8.hash(&mut h),
        layer_model::text::Stretch::SemiCondensed => 3u8.hash(&mut h),
        layer_model::text::Stretch::Normal => 4u8.hash(&mut h),
        layer_model::text::Stretch::SemiExpanded => 5u8.hash(&mut h),
        layer_model::text::Stretch::Expanded => 6u8.hash(&mut h),
        layer_model::text::Stretch::ExtraExpanded => 7u8.hash(&mut h),
        layer_model::text::Stretch::UltraExpanded => 8u8.hash(&mut h),
    }
    for c in layer.style.fill {
        c.to_bits().hash(&mut h);
    }
    layer.style.underline.hash(&mut h);
    layer.style.strikethrough.hash(&mut h);
    match layer.style.script {
        layer_model::text::Script::Normal => 0u8.hash(&mut h),
        layer_model::text::Script::Superscript => 1u8.hash(&mut h),
        layer_model::text::Script::Subscript => 2u8.hash(&mut h),
    }
    layer.style.tracking.to_bits().hash(&mut h);
    layer.style.ligatures.hash(&mut h);
    layer.style.kerning.hash(&mut h);
    layer.style.synthetic_bold.hash(&mut h);
    layer.style.synthetic_italic.hash(&mut h);
    // W3-J: scale, baseline shift, caps and anti-alias change the pixels.
    layer.style.horizontal_scale.to_bits().hash(&mut h);
    layer.style.vertical_scale.to_bits().hash(&mut h);
    layer.style.baseline_shift.to_bits().hash(&mut h);
    layer.style.caps.hash(&mut h);
    layer.style.anti_alias.hash(&mut h);
    for span in &layer.spans {
        span.start.hash(&mut h);
        span.end.hash(&mut h);
        if let Some(f) = &span.style.family {
            f.hash(&mut h);
        }
        if let Some(v) = span.style.size_px {
            v.to_bits().hash(&mut h);
        }
        if let Some(w) = span.style.weight {
            w.0.hash(&mut h);
        }
        if let Some(s) = span.style.slant {
            match s {
                layer_model::text::Slant::Normal => 0u8.hash(&mut h),
                layer_model::text::Slant::Italic => 1u8.hash(&mut h),
            }
        }
        if let Some(s) = span.style.stretch {
            match s {
                layer_model::text::Stretch::UltraCondensed => 0u8.hash(&mut h),
                layer_model::text::Stretch::ExtraCondensed => 1u8.hash(&mut h),
                layer_model::text::Stretch::Condensed => 2u8.hash(&mut h),
                layer_model::text::Stretch::SemiCondensed => 3u8.hash(&mut h),
                layer_model::text::Stretch::Normal => 4u8.hash(&mut h),
                layer_model::text::Stretch::SemiExpanded => 5u8.hash(&mut h),
                layer_model::text::Stretch::Expanded => 6u8.hash(&mut h),
                layer_model::text::Stretch::ExtraExpanded => 7u8.hash(&mut h),
                layer_model::text::Stretch::UltraExpanded => 8u8.hash(&mut h),
            }
        }
        if let Some(fill) = span.style.fill {
            for c in fill {
                c.to_bits().hash(&mut h);
            }
        }
        span.style.underline.hash(&mut h);
        span.style.strikethrough.hash(&mut h);
        if let Some(s) = span.style.script {
            match s {
                layer_model::text::Script::Normal => 0u8.hash(&mut h),
                layer_model::text::Script::Superscript => 1u8.hash(&mut h),
                layer_model::text::Script::Subscript => 2u8.hash(&mut h),
            }
        }
        if let Some(t) = span.style.tracking {
            t.to_bits().hash(&mut h);
        }
    }
    match layer.paragraph.alignment {
        layer_model::text::Alignment::Left => 0u8.hash(&mut h),
        layer_model::text::Alignment::Center => 1u8.hash(&mut h),
        layer_model::text::Alignment::Right => 2u8.hash(&mut h),
        layer_model::text::Alignment::Justified => 3u8.hash(&mut h),
        layer_model::text::Alignment::JustifyLastCenter => 4u8.hash(&mut h),
        layer_model::text::Alignment::JustifyLastRight => 5u8.hash(&mut h),
        layer_model::text::Alignment::JustifyAll => 6u8.hash(&mut h),
    }
    match layer.paragraph.leading {
        layer_model::text::Leading::Multiple(v) => {
            0u8.hash(&mut h);
            v.to_bits().hash(&mut h);
        }
        layer_model::text::Leading::Absolute(v) => {
            1u8.hash(&mut h);
            v.to_bits().hash(&mut h);
        }
    }
    layer.paragraph.first_line_indent.to_bits().hash(&mut h);
    layer.paragraph.space_before.to_bits().hash(&mut h);
    layer.paragraph.space_after.to_bits().hash(&mut h);
    layer.paragraph.left_indent.to_bits().hash(&mut h);
    layer.paragraph.right_indent.to_bits().hash(&mut h);
    match layer.frame {
        layer_model::text::Frame::Point => 0u8.hash(&mut h),
        layer_model::text::Frame::Box { width, height } => {
            1u8.hash(&mut h);
            width.to_bits().hash(&mut h);
            height.map(|v| v.to_bits()).hash(&mut h);
        }
    }
    for k in &layer.kerning {
        k.index.hash(&mut h);
        k.amount.to_bits().hash(&mut h);
    }
    h.finish()
}

fn engine() -> MutexGuard<'static, Engine> {
    static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();
    let lock = ENGINE.get_or_init(|| {
        Mutex::new(Engine {
            library: FontLibrary::with_system_fonts(),
            glyphs: GlyphRasterCache::new(),
            runs: HashMap::new(),
            generation: 0,
        })
    });
    // A poisoned lock means some other thread panicked mid-render. The state
    // behind it is a font list and two caches — recoverable data, not an
    // invariant — so taking it back is better than failing every later frame.
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Add a font family to the compositor's library from an in-memory font file.
///
/// Returns the number of faces the file contributed. Every cached run is
/// dropped, because a newly available face can change which face a family
/// resolves to and therefore what the same string looks like.
///
/// This is how an application ships a font the machine does not have, and how
/// a test pins what it is drawing with.
pub fn load_font(data: Vec<u8>) -> usize {
    let mut e = engine();
    let added = e.library.load_bytes(data).len();
    if added > 0 {
        e.generation = e.generation.wrapping_add(1);
        e.runs.clear();
        e.glyphs.clear();
    }
    added
}

/// The family names the compositor can shape text with, sorted.
pub fn font_families() -> Vec<String> {
    engine().library.family_names()
}

/// Every installed face of one family — the Character panel's face rows.
///
/// Weight, slant and stretch all come from the faces themselves, so a family's
/// condensed faces appear here exactly when they are installed (card 022).
/// Empty when the family is not installed; pair with [`font_substitute_for`]
/// to explain what shaping would use instead.
pub fn font_family_faces(family: &str) -> Vec<text_engine::FaceRecord> {
    engine()
        .library
        .families()
        .into_iter()
        .find(|f| f.name == family)
        .map(|f| f.faces)
        .unwrap_or_default()
}

/// The substitute shaping will use for a family that is not installed, or
/// `None` when the request needs no substitution (installed, generic sans, or
/// a fontless library). The same rule `attrs_for` applies before shaping, so
/// a report shown here is the render the user gets.
pub fn font_substitute_for(family: &str) -> Option<String> {
    engine().library.substitute_for(family)
}

/// How many of the run's laid-out lines fall past a wrapping box's own height
/// — the overset status card 023 shows in the Paragraph panel.
///
/// `None` when the run is point text or the box has no height of its own (an
/// auto-height box cannot overflow) — both answered without shaping. The
/// engine never clips — it reports every line — so this is the count the UI
/// can surface instead of silently losing text. Shaping happens with the same
/// library the canvas renders with and is uncached, so callers ask only while
/// their panel is open and only for fixed-height boxes.
pub fn text_overset_lines(run: &text_engine::TextRun) -> Option<usize> {
    // No fixed height, nothing to measure — answer before paying for a shape.
    let height = match run.frame {
        text_engine::TextFrame::Box {
            height: Some(h), ..
        } => h,
        _ => return None,
    };
    let mut e = engine();
    let shaped = text_engine::shape(&mut e.library, run);
    Some(
        shaped
            .lines
            .iter()
            .filter(|line| line.bottom > height + 1e-4)
            .count(),
    )
}

/// The run's laid-out content height, in layer pixels — what an auto-height
/// box is currently growing to. The Paragraph panel seeds a newly fixed box
/// height with it, so fixing the height never silently oversets the text.
pub fn text_content_height(run: &text_engine::TextRun) -> f32 {
    let mut e = engine();
    text_engine::shape(&mut e.library, run).bounds.height
}

/// The byte caret index for a click at layer-local `(x, y)` — card 026's
/// enter-an-existing-layer hit test.
///
/// `None` when the click falls outside the laid-out block, padded by half a
/// line height so leading, descenders and the block's edges stay grabbable.
/// Inside, the answer is the engine's own [`text_engine::ShapedText::hit_test`]
/// — the same caret geometry the overlays use — so a click between two glyphs
/// lands on the caret stop between them, ligatures and bidi included.
///
/// # Deadlock
/// This takes the engine lock. A caller holding [`engine()`](crate::text) must
/// drop it before calling — see the test that pins the pitfall.
pub fn text_hit_index(run: &text_engine::TextRun, x: f32, y: f32) -> Option<usize> {
    let mut e = engine();
    let shaped = text_engine::shape(&mut e.library, run);
    let pad = shaped.line_height * 0.5;
    let b = shaped.bounds;
    let inside =
        x >= b.x - pad && x <= b.x + b.width + pad && y >= b.y - pad && y <= b.y + b.height + pad;
    if !inside {
        return None;
    }
    Some(shaped.hit_test(x, y))
}

/// Card 030: the shaped caret rectangle for byte `index` — x at the bidi-
/// resolved stop, y spanning the line. Zero-width; the overlay draws the bar.
pub fn text_caret_rect(run: &text_engine::TextRun, index: usize) -> text_engine::Rect {
    let mut e = engine();
    let shaped = text_engine::shape(&mut e.library, run);
    shaped.caret_rect(index)
}

/// Card 030: shaped selection rectangles for the byte range — one or more per
/// line for bidirectional text; empty or inverted ranges produce none.
pub fn text_selection_rects(
    run: &text_engine::TextRun,
    start: usize,
    end: usize,
) -> Vec<text_engine::Rect> {
    let mut e = engine();
    let shaped = text_engine::shape(&mut e.library, run);
    shaped.selection_rects(start, end)
}

/// `true` when the library holds no faces at all, so every text layer will
/// composite to nothing however it is styled.
pub fn no_fonts() -> bool {
    engine().library.is_empty()
}

/// The em size a layer is shaped at when composited at `level`, or `None` when
/// it is too small to be worth shaping.
fn level_size(layer: &TextLayer, level: u8) -> Option<f32> {
    let size = layer.size_px * 2.0f32.powi(-(level as i32));
    (size.is_finite() && size >= MIN_FONT_SIZE_PX).then_some(size)
}

/// The layer's ink, in its own pixel space at `level`, premultiplied and
/// linear.
///
/// `None` when there is nothing to draw: an empty string, an em size below the
/// shaper's floor, or a library with no face that can carry the text.
pub(crate) fn run_image(layer: &TextLayer, level: u8) -> Option<Arc<LinearImage>> {
    if layer.text.is_empty() {
        return None;
    }
    let size = level_size(layer, level)?;
    let mut e = engine();
    let key = RunKey {
        text: layer.text.clone(),
        family: layer.font_family.clone(),
        size_bits: size.to_bits(),
        generation: e.generation,
        content: hash_layer(layer),
    };
    if let Some(hit) = e.runs.get(&key) {
        return (!hit.is_empty()).then(|| Arc::clone(hit));
    }

    let mut run = TextRun::from(layer);
    run.style.size_px = size;
    // Borrow the three fields apart so the shaper and the rasteriser can hold
    // `&mut library` and `&mut glyphs` at once.
    let Engine {
        library,
        glyphs,
        runs,
        ..
    } = &mut *e;
    let shaped = shape(library, &run);
    let image = Arc::new(render_linear(library, glyphs, &shaped));
    if runs.len() >= MAX_CACHED_RUNS {
        runs.clear();
    }
    runs.insert(key, Arc::clone(&image));
    (!image.is_empty()).then_some(image)
}

/// The rect the layer's ink occupies in its own space at `level`, empty when
/// there is none.
pub(crate) fn ink_bounds(layer: &TextLayer, level: u8) -> PixelRect {
    match run_image(layer, level) {
        Some(img) => PixelRect::new(
            i64::from(img.origin_x),
            i64::from(img.origin_y),
            img.width,
            img.height,
        ),
        None => PixelRect::new(0, 0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loading the fixture font is what every text test depends on, so it is
    /// asserted rather than assumed.
    #[test]
    fn the_fixture_font_loads_and_shapes() {
        assert!(crate::testkit::text_fixture_family().len() > 1);
        assert!(font_families().iter().any(|f| f == "DejaVu Sans"));
        assert!(!no_fonts());

        let layer = TextLayer {
            text: "Hg".into(),
            font_family: "DejaVu Sans".into(),
            size_px: 64.0,
            ..Default::default()
        };
        let img = run_image(&layer, 0).expect("ink");
        assert!(img.width > 0 && img.height > 0);
        assert!(
            img.data.as_chunks::<4>().0.iter().any(|p| p[3] > 0.5),
            "real ink"
        );
    }

    #[test]
    fn an_empty_or_unrenderably_small_run_has_no_ink() {
        crate::testkit::text_fixture_family();
        let mut layer = TextLayer {
            text: String::new(),
            font_family: "DejaVu Sans".into(),
            size_px: 32.0,
            ..Default::default()
        };
        assert!(run_image(&layer, 0).is_none());
        assert!(ink_bounds(&layer, 0).is_empty());

        layer.text = "x".into();
        // Level 20 divides the em size by a million: below the shaper's floor.
        assert!(level_size(&layer, 20).is_none());
        assert!(run_image(&layer, 20).is_none());
        // And a level that only halves it still has ink.
        assert!(run_image(&layer, 1).is_some());
    }

    #[test]
    fn a_run_is_rendered_once_and_then_served_from_the_cache() {
        crate::testkit::text_fixture_family();
        let layer = TextLayer {
            text: "cache me".into(),
            font_family: "DejaVu Sans".into(),
            size_px: 24.0,

            ..Default::default()
        };
        let first = run_image(&layer, 0).expect("ink");
        let second = run_image(&layer, 0).expect("ink");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the second ask must not re-shape"
        );
        // A different size is a different entry, not the same one resized.
        let mut bigger = layer.clone();
        bigger.size_px = 48.0;
        let other = run_image(&bigger, 0).expect("ink");
        assert!(!Arc::ptr_eq(&first, &other));
        assert!(other.width > first.width);
    }

    #[test]
    fn ink_bounds_match_the_image_and_scale_with_the_level() {
        crate::testkit::text_fixture_family();
        let layer = TextLayer {
            text: "Wide enough to measure".into(),
            font_family: "DejaVu Sans".into(),
            size_px: 40.0,

            ..Default::default()
        };
        let full = ink_bounds(&layer, 0);
        let half = ink_bounds(&layer, 1);
        assert!(!full.is_empty() && !half.is_empty());
        // Half the em size is about half the ink, give or take hinting.
        let ratio = f64::from(half.width) / f64::from(full.width);
        assert!((0.4..0.6).contains(&ratio), "{ratio}");
        let img = run_image(&layer, 0).unwrap();
        assert_eq!(full.width, img.width);
        assert_eq!(full.x, i64::from(img.origin_x));
    }
}

// -- cards 019 + 020: complete styled rendering and cache identity ----------

#[cfg(test)]
// -- cards 019 + 020: complete styled rendering and cache identity ----------
#[cfg(test)]
mod styled_tests {
    use super::*;
    use layer_model::text::{
        BaseStyle, Frame, Kern, Leading, Paragraph, Stretch, StyleOverride, StyleSpan, Weight,
    };
    use text_engine::TextFrame;

    fn styled(fill: [f32; 4], weight: u16, tracking: f32) -> TextLayer {
        TextLayer {
            text: "Run".into(),
            font_family: "DejaVu Sans".into(),
            size_px: 64.0,
            style: BaseStyle {
                fill,
                weight: Weight(weight),
                tracking,
                ..BaseStyle::default()
            },
            ..TextLayer::default()
        }
    }

    /// Whole-image identity: dimensions plus every pixel position. A pure
    /// horizontal shift must register as a difference.
    fn differs(a: &LinearImage, b: &LinearImage) -> bool {
        a.width != b.width
            || a.height != b.height
            || a.data.iter().zip(b.data.iter()).any(|(p, q)| p != q)
    }

    fn ink(img: &LinearImage) -> Vec<[f32; 4]> {
        img.data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0.5)
            .map(|p| [p[0], p[1], p[2], p[3]])
            .collect()
    }

    /// Card 019: the same string renders differently when any styled field
    /// changes - weight, fill colour, tracking. Card 020: a warm cache must
    /// never serve one run's pixels for another's.
    #[test]
    fn style_changes_shape_different_runs_through_the_cache() {
        crate::testkit::text_fixture_family();
        assert!(!no_fonts());
        let regular = styled([0.0, 0.0, 0.0, 1.0], 400, 0.0);
        let bold = styled([0.0, 0.0, 0.0, 1.0], 700, 0.0);
        let green = styled([0.1, 0.8, 0.3, 1.0], 400, 0.0);
        let tracked = styled([0.0, 0.0, 0.0, 1.0], 400, 200.0);

        let a = run_image(&regular, 0).expect("regular ink");
        let b = run_image(&bold, 0).expect("bold ink");
        let c = run_image(&green, 0).expect("green ink");
        let d = run_image(&tracked, 0).expect("tracked ink");

        // Weight and tracking change the geometry.
        assert!(differs(&a, &b), "regular and bold differ");
        assert!(differs(&a, &d), "tracking changes the run");

        // Colour changes the pixels, not the geometry: black ink carries no
        // chroma; green's green channel dominates its red.
        assert!(
            ink(&a)
                .iter()
                .all(|p| p[0] <= f32::EPSILON && p[1] <= f32::EPSILON),
            "black ink carries no colour"
        );
        let c_ink = ink(&c);
        assert!(!c_ink.is_empty());
        assert!(c_ink.iter().all(|p| p[1] >= p[0]), "green dominates red");

        // Determinism: the same content answers with the same pixels - a warm
        // hit serves the cached Arc, and after an eviction a re-render must be
        // pixel-identical, never another run's pixels.
        let again = run_image(&regular, 0).expect("cached");
        assert!(!differs(&a, &again), "the warm cache serves the same run");
    }

    /// Paragraph settings and the frame reach the layout: centred multiline
    /// text is not left-aligned text, absolute leading spreads the lines,
    /// kerning widens a line, and a wrapping box reflows.
    #[test]
    fn paragraph_alignment_leading_and_kerning_reach_the_layout() {
        crate::testkit::text_fixture_family();
        assert!(!no_fonts());
        let base = TextLayer {
            text: "ab\ncd".into(),
            font_family: "DejaVu Sans".into(),
            size_px: 48.0,
            paragraph: Paragraph {
                alignment: layer_model::text::Alignment::Left,
                ..Paragraph::default()
            },
            ..TextLayer::default()
        };
        let centered = TextLayer {
            paragraph: Paragraph {
                alignment: layer_model::text::Alignment::Center,
                ..Paragraph::default()
            },
            ..base.clone()
        };
        let leading = TextLayer {
            paragraph: Paragraph {
                leading: Leading::Absolute(96.0),
                ..Paragraph::default()
            },
            ..base.clone()
        };
        let kerned = TextLayer {
            kerning: vec![Kern {
                index: 1,
                amount: 200.0,
            }],
            ..base.clone()
        };
        let boxed = TextLayer {
            frame: Frame::Box {
                width: 40.0,
                height: Some(200.0),
            },
            ..base.clone()
        };

        let a = run_image(&base, 0).expect("ink");
        let b = run_image(&centered, 0).expect("ink");
        let c = run_image(&leading, 0).expect("ink");
        let d = run_image(&kerned, 0).expect("ink");
        let e = run_image(&boxed, 0).expect("ink");
        assert!(differs(&a, &b), "alignment moves the lines");
        assert!(differs(&a, &c), "absolute leading spreads the lines");
        assert!(differs(&a, &d), "kerning widens the line");
        assert!(differs(&a, &e), "the box reflows the text");
    }

    /// A styled span overrides the base for its own bytes: two colour groups
    /// in one run.
    #[test]
    fn a_span_colours_its_own_bytes() {
        crate::testkit::text_fixture_family();
        assert!(!no_fonts());
        let mut layer = styled([0.0, 0.0, 0.0, 1.0], 400, 0.0);
        layer.text = "abcd".into();
        layer.spans = vec![StyleSpan {
            start: 2,
            end: 4,
            style: StyleOverride {
                fill: Some([0.9, 0.1, 0.1, 1.0]),
                ..StyleOverride::default()
            },
        }];
        let img = run_image(&layer, 0).expect("ink");
        let reds = img
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0.5 && p[0] > 0.5)
            .count();
        let blacks = img
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0.5 && p[0] <= f32::EPSILON)
            .count();
        assert!(reds > 0, "the red span painted its bytes");
        assert!(blacks > 0, "the unspanned bytes stayed black");
    }

    /// Semitransparent edges composite: coverage produces partial-alpha
    /// pixels at glyph borders.
    #[test]
    fn glyph_edges_carry_partial_alpha() {
        crate::testkit::text_fixture_family();
        assert!(!no_fonts());
        let layer = styled([0.0, 0.0, 0.0, 1.0], 400, 0.0);
        let img = run_image(&layer, 0).expect("ink");
        let partial = img
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0.0 && p[3] < 1.0)
            .count();
        assert!(partial > 0, "antialiased edges exist in the ink");
    }

    /// Card 020's headline: the layer's whole content hashes into the key, so
    /// editing any styled field invalidates the warm cache - and undo (the
    /// layer back to its old content) restores the first render exactly.
    #[test]
    fn editing_any_styled_field_invalidates_the_cache_and_undo_restores() {
        crate::testkit::text_fixture_family();
        assert!(!no_fonts());
        let base = styled([0.0, 0.0, 0.0, 1.0], 400, 0.0);
        let first = run_image(&base, 0).expect("ink");

        let mut white = base.clone();
        white.style.fill = [1.0, 1.0, 1.0, 1.0];
        let second = run_image(&white, 0).expect("ink");
        assert!(differs(&first, &second), "a fill-only change reshapes");

        let mut spanned = base.clone();
        spanned.text = "abc".into();
        spanned.spans = vec![StyleSpan {
            start: 0,
            end: 3,
            style: StyleOverride {
                weight: Some(Weight(700)),
                ..StyleOverride::default()
            },
        }];
        let third = run_image(&spanned, 0).expect("ink");
        assert!(differs(&first, &third), "a span-only change reshapes");

        let boxed = TextLayer {
            frame: Frame::Box {
                width: 50.0,
                height: None,
            },
            ..base.clone()
        };
        let fourth = run_image(&boxed, 0).expect("ink");
        assert!(differs(&first, &fourth), "a frame change reshapes");

        // Manual kerning inserts space *before* the character at `index`;
        // index 0 sits at the line start and is a documented no-op, so kern
        // before the second character.
        let kerned = TextLayer {
            kerning: vec![Kern {
                index: 1,
                amount: 300.0,
            }],
            ..base.clone()
        };
        let fifth = run_image(&kerned, 0).expect("ink");
        assert!(differs(&first, &fifth), "a kerning change reshapes");

        let restored = run_image(&base, 0).expect("ink");
        assert!(
            !differs(&first, &restored),
            "undo restores the first render"
        );
        let _ = second;
    }

    /// Card 022: the condensed fixture face joins "DejaVu Sans" as a second
    /// width; selecting it through the model's stretch field must change the
    /// pixels, and the narrower design must measure narrower.
    #[test]
    fn the_condensed_face_renders_narrower_ink() {
        crate::testkit::text_fixture_family();
        crate::testkit::text_condensed_face();
        assert!(!no_fonts());
        let normal = TextLayer {
            text: "Headline".into(),
            font_family: "DejaVu Sans".into(),
            size_px: 64.0,
            ..TextLayer::default()
        };
        let condensed = TextLayer {
            style: BaseStyle {
                stretch: Stretch::SemiCondensed,
                ..BaseStyle::default()
            },
            ..normal.clone()
        };
        let a = run_image(&normal, 0).expect("regular ink");
        let b = run_image(&condensed, 0).expect("condensed ink");
        assert!(differs(&a, &b), "the condensed face changes the run");
        assert!(
            b.width < a.width,
            "condensed ink is narrower: {} vs {}",
            b.width,
            a.width
        );
        // The face rows come from the library itself, so the picker sees both
        // widths of the one family.
        let faces = crate::font_family_faces("DejaVu Sans");
        let widths: std::collections::BTreeSet<_> = faces.iter().map(|f| f.stretch).collect();
        assert!(
            widths.contains(&text_engine::FontStretch::Normal)
                && widths.contains(&text_engine::FontStretch::SemiCondensed),
            "both widths listed: {widths:?}"
        );
    }

    /// Card 022: a stretch-only edit is a pixel-affecting edit, so it must
    /// re-key the run cache (hash identity), and neighbours must not corrupt
    /// each other's warm entries.
    #[test]
    fn a_stretch_only_edit_rekeys_the_run_cache() {
        crate::testkit::text_fixture_family();
        crate::testkit::text_condensed_face();
        let normal = TextLayer {
            text: "Headline".into(),
            font_family: "DejaVu Sans".into(),
            size_px: 64.0,
            ..TextLayer::default()
        };
        let condensed = TextLayer {
            style: BaseStyle {
                stretch: Stretch::SemiCondensed,
                ..BaseStyle::default()
            },
            ..normal.clone()
        };
        assert_ne!(
            super::hash_layer(&normal),
            super::hash_layer(&condensed),
            "stretch is part of the cache identity"
        );
        let first = run_image(&normal, 0).expect("ink");
        let _ = run_image(&condensed, 0).expect("ink");
        let again = run_image(&normal, 0).expect("cached");
        assert!(
            !differs(&first, &again),
            "the warm cache serves the same run after a stretch-only neighbour"
        );
        // A span-level stretch override reaches the hash too.
        let spanned = TextLayer {
            spans: vec![StyleSpan {
                start: 0,
                end: 4,
                style: StyleOverride {
                    stretch: Some(Stretch::Condensed),
                    ..StyleOverride::default()
                },
            }],
            ..normal.clone()
        };
        assert_ne!(
            super::hash_layer(&normal),
            super::hash_layer(&spanned),
            "a span stretch override is part of the cache identity"
        );
    }

    /// Card 023: the overset status the Paragraph panel shows — counted for a
    /// fixed-height box, absent whenever there is no fixed height, and the
    /// content-height seed keeps a newly fixed box from instantly oversetting.
    #[test]
    fn overset_is_counted_for_a_fixed_box_and_absent_without_one() {
        crate::testkit::text_fixture_family();
        let text = "Wrap wrap wrap wrap wrap wrap wrap";
        let auto = TextRun::paragraph(text, "DejaVu Sans", 24.0, 120.0);
        assert_eq!(
            text_overset_lines(&auto),
            None,
            "an auto-height box cannot overflow"
        );
        let point = TextRun::point(text, "DejaVu Sans", 24.0);
        assert_eq!(
            text_overset_lines(&point),
            None,
            "point text cannot overflow"
        );

        // One 24px line is about 28.8px tall (auto leading 1.2), so a 30px
        // box holds the first line and the rest report.
        let mut fixed = auto.clone();
        fixed.frame = TextFrame::Box {
            width: 120.0,
            height: Some(30.0),
        };
        let overset = text_overset_lines(&fixed).expect("a fixed height reports a count");
        assert!(overset > 0, "the narrow fixed box overflows: {overset}");

        let mut roomy = fixed.clone();
        roomy.frame = TextFrame::Box {
            width: 120.0,
            height: Some(1000.0),
        };
        assert_eq!(
            text_overset_lines(&roomy),
            Some(0),
            "a tall box holds every line"
        );

        // The seed the panel uses: fixing the height at the current content
        // height keeps every line visible.
        let content = text_content_height(&auto);
        assert!(content > 0.0);
        let mut seeded = auto.clone();
        seeded.frame = TextFrame::Box {
            width: 120.0,
            height: Some(content),
        };
        assert_eq!(
            text_overset_lines(&seeded),
            Some(0),
            "fixing at the laid-out height oversets nothing"
        );
    }

    /// Card 026: the click→caret hit test — inside the laid-out block the
    /// answer is the engine's own caret geometry (here: the caret stop at the
    /// click's x, verified against hit_test itself); outside it is `None`, so
    /// a Type click past the text falls through to layer creation.
    #[test]
    fn a_click_places_the_caret_inside_the_block_and_misses_outside_it() {
        crate::testkit::text_fixture_family();
        let run = TextRun::point("Hello World", "DejaVu Sans", 24.0);
        // The engine lock is scoped: the facade takes it again internally.
        let (bounds, line_height) = {
            let mut e = engine();
            let shaped = text_engine::shape(&mut e.library, &run);
            (shaped.bounds, shaped.line_height)
        };
        let mid_line_y = bounds.y + line_height * 0.5;
        let mid_gap_x = bounds.x + bounds.width * 0.5;

        let inside =
            text_hit_index(&run, mid_gap_x, mid_line_y).expect("a click inside the block hits");
        assert!(inside <= run.text.len());
        // The facade agrees with the engine's own geometry for the same point.
        let engine_answer = {
            let mut e = engine();
            text_engine::shape(&mut e.library, &run).hit_test(mid_gap_x, mid_line_y)
        };
        assert_eq!(inside, engine_answer);

        // The caret stop at the click's x is deterministic: the click at the
        // block's left edge names the first caret stop (byte 0).
        let left = text_hit_index(&run, bounds.x, mid_line_y).expect("left edge hits");
        assert_eq!(left, 0, "the block's left edge is the first caret stop");

        // Far outside — no hit, no caret.
        assert_eq!(
            text_hit_index(&run, bounds.x + bounds.width + 500.0, mid_line_y),
            None,
            "a click well past the ink is not a text hit"
        );
        assert_eq!(
            text_hit_index(&run, mid_gap_x, bounds.y + bounds.height + 500.0),
            None,
            "a click well below the ink is not a text hit"
        );
    }
}
