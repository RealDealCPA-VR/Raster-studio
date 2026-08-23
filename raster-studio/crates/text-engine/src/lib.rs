//! Text engine: font handling, shaping, layout, rasterisation and the editing
//! geometry an editable text layer needs.
//!
//! # The shape of the thing
//!
//! * [`TextRun`] is the **model**: a serialisable description of a text layer —
//!   the string, a base [`CharStyle`], per-range [`StyleRun`]s, a
//!   [`ParagraphStyle`], a [`TextFrame`] (point text or a wrapping box), manual
//!   [`KernAdjustment`]s, and the layer-space origin. It converts to and from
//!   [`layer_model::TextLayer`], which stays the minimal on-disk shape.
//! * [`FontLibrary`] is the **font side**: enumerate what the machine has, load
//!   a family from bytes, and match a family/weight/slant request to a face —
//!   reporting when the match is too light or too upright and must be faked.
//! * [`shape`] is the **layout**: it produces a [`ShapedText`] of positioned
//!   glyphs and visual lines.
//! * [`rasterize`] and [`render_linear`] are the **pixels**: an 8-bit coverage
//!   mask, or that mask filled with each run's colour in linear premultiplied
//!   space.
//! * [`ShapedText::hit_test`], [`ShapedText::caret_rect`] and
//!   [`ShapedText::selection_rects`] are the **editing geometry**.
//!
//! # What this crate does not do itself
//!
//! Shaping is not hand-rolled. Cluster formation, ligature substitution,
//! kerning, mark attachment, script itemisation, bidi reordering and font
//! fallback all come from `cosmic-text` (harfrust + swash + fontdb). A
//! per-glyph advance loop is exactly what makes text look amateur, so there
//! isn't one anywhere in here.
//!
//! # Known limitations
//!
//! * [`ParagraphStyle::first_line_indent`] shifts the first visual line of a
//!   paragraph after wrapping; the wrap width itself is not reduced by the
//!   indent, so a long first line can overhang by up to the indent.
//! * Manual [`KernAdjustment`]s are applied after line breaking, so they move
//!   glyphs but do not change where a line wraps. Tracking, which is fed to the
//!   shaper as letter spacing, *does* affect wrapping.
//! * One [`ParagraphStyle`] applies to the whole layer; per-paragraph settings
//!   would need a second range list.
//! * The output is a *coverage* mask, so a colour glyph (`COLR`/`CBDT` emoji)
//!   is reduced to its alpha and then filled with the run's colour — it comes
//!   out as a silhouette. Carrying colour glyphs through would need a second,
//!   RGBA glyph path.
//! * A boxed frame's `height` is advisory: layout reports every line and sets
//!   [`ShapedText::overflows`], leaving the clipping decision to the caller.
//! * `OpenType` feature control is limited to ligatures and kerning, and the
//!   only variation axis driven is `wght`.
//! * Subpixel glyph positioning is **horizontal only**: baselines snap to whole
//!   pixels, the usual vertical hinting, so a sub-pixel vertical move of the
//!   layer does not move the text by that fraction. See the `raster` module.
//! * [`ParagraphStyle::alignment`] on point text aligns the lines about the
//!   block's own widest line, because there is no box to align against, and
//!   [`Alignment::Justify`] there has nothing to stretch to and degrades to the
//!   paragraph's start edge.
//! * Rasterisation clamps pen positions and decoration rules to ±10^7 layer
//!   pixels. Layout still reports the true positions; it is only the glyph
//!   scaler, whose pixel component is an `i32`, that is protected. Text that
//!   far off-canvas rasterises at the clamp instead of overflowing.
//! * A [`FontLibrary`] with no faces at all cannot shape anything, so [`shape`]
//!   reports no glyphs and one zero-width line per paragraph, each owning that
//!   paragraph's whole byte range. The caret, hit test, selection and rasteriser
//!   all still answer on that result, and [`ShapedText::caret_rect`] still puts
//!   an index on the line its paragraph is on — but the lines have no width, so
//!   every caret on a line shares one x and [`ShapedText::hit_test`] can only
//!   answer with the paragraph's start. The text is invisible either way;
//!   callers that care should check [`FontLibrary::is_empty`] and say so in the
//!   UI.
//! * Vertical writing modes are not implemented.
//!
//! ```
//! use text_engine::{shape, rasterize, FontLibrary, GlyphRasterCache, TextRun};
//!
//! let mut library = FontLibrary::empty();
//! library.load_bytes(dejavu::sans::regular().to_vec());
//!
//! let run = TextRun::point("Hello", "DejaVu Sans", 32.0);
//! let shaped = shape(&mut library, &run);
//! assert_eq!(shaped.lines.len(), 1);
//!
//! let mut cache = GlyphRasterCache::new();
//! let mask = rasterize(&mut library, &mut cache, &shaped);
//! assert!(mask.total_coverage() > 0);
//! ```

mod edit;
mod font;
mod layout;
mod model;
mod raster;
mod style;

pub use edit::CaretStop;
pub use font::{FaceMatch, FaceMetrics, FaceRecord, FamilyRecord, FontId, FontLibrary};
pub use layout::{
    shape, Decoration, DecorationKind, Rect, ShapedGlyph, ShapedLine, ShapedText, MIN_FONT_SIZE_PX,
};
pub use model::{Alignment, KernAdjustment, LineHeight, ParagraphStyle, TextFrame, TextRun};
pub use raster::{
    fill_linear, rasterize, render_linear, synthetic_bold_radius, CoverageMask, GlyphImage,
    GlyphKey, GlyphRasterCache, LinearImage,
};
pub use style::{
    resolve_style, CharStyle, FontSlant, FontWeight, ScriptPosition, StyleOverride, StyleRun,
    SCRIPT_SIZE_FACTOR, SUBSCRIPT_DROP, SUPERSCRIPT_RISE,
};

/// Whether the text engine is implemented.
///
/// This returned `false` for the whole of phases 1 and 2, while the crate was
/// a placeholder and the UI greyed the text tools out. It is now `true`, and
/// the crate's tests are what justify the claim: they shape a known string
/// with a known font, form a ligature, wrap at the expected word, align runs,
/// rasterise a non-empty in-bounds mask, and round-trip caret and hit-test.
#[must_use]
pub const fn is_available() -> bool {
    true
}
