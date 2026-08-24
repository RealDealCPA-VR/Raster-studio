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
}

struct Engine {
    library: FontLibrary,
    glyphs: GlyphRasterCache,
    runs: HashMap<RunKey, Arc<LinearImage>>,
    generation: u64,
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
        };
        let img = run_image(&layer, 0).expect("ink");
        assert!(img.width > 0 && img.height > 0);
        assert!(img.data.chunks_exact(4).any(|p| p[3] > 0.5), "real ink");
    }

    #[test]
    fn an_empty_or_unrenderably_small_run_has_no_ink() {
        crate::testkit::text_fixture_family();
        let mut layer = TextLayer {
            text: String::new(),
            font_family: "DejaVu Sans".into(),
            size_px: 32.0,
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
