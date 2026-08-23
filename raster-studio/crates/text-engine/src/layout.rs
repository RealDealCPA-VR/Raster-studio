//! Shaping and layout.
//!
//! [`shape`] turns a [`TextRun`] into a [`ShapedText`]: positioned glyphs,
//! visual lines, and decoration rectangles, all in layer space with y growing
//! downwards and the layer origin at [`TextRun::origin`].
//!
//! Shaping itself — cluster formation, ligatures, kerning, mark attachment,
//! bidi reordering, script itemisation and font fallback — is delegated to
//! `cosmic-text`/`harfrust`. This module owns everything the shaper does not
//! model: paragraph spacing, first-line indent, manual kerning, sub/superscript
//! baseline shifts, decoration geometry and the mapping from paragraph-local
//! byte offsets back to offsets in the layer's own string.

use std::collections::BTreeSet;

use cosmic_text::{
    Align, Attrs, AttrsList, Buffer, BufferLine, CacheKeyFlags, Family, FeatureTag, FontFeatures,
    LineEnding, LineIter, Metrics, Shaping, Weight, Wrap,
};

use crate::font::{db_style, FontId, FontLibrary};
use crate::model::{Alignment, TextFrame, TextRun};
use crate::style::{resolve_style, CharStyle, FontWeight, StyleRun};

/// Smallest font size the engine will shape at. Zero-size text would divide by
/// zero inside the shaper's em-relative maths.
pub const MIN_FONT_SIZE_PX: f32 = 0.01;

/// An axis-aligned rectangle in layer space.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width; never negative.
    pub width: f32,
    /// Height; never negative.
    pub height: f32,
}

impl Rect {
    /// Build a rectangle from two corners in any order.
    #[must_use]
    pub fn from_corners(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            x: x0.min(x1),
            y: y0.min(y1),
            width: (x1 - x0).abs(),
            height: (y1 - y0).abs(),
        }
    }

    /// Right edge.
    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    /// Bottom edge.
    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// The smallest rectangle containing both.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self::from_corners(
            self.x.min(other.x),
            self.y.min(other.y),
            self.right().max(other.right()),
            self.bottom().max(other.bottom()),
        )
    }

    /// Whether `other` lies entirely inside `self` (edges may touch).
    #[must_use]
    pub fn contains_rect(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
}

/// One positioned glyph.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedGlyph {
    /// Face the glyph came from — may differ from the requested family when
    /// the shaper fell back for a missing codepoint.
    pub font: FontId,
    /// Glyph index inside that face.
    pub glyph_id: u16,
    /// Byte range of the cluster this glyph represents, in the layer's string.
    /// A ligature spans several characters; a mark shares its base's range.
    pub cluster_start: usize,
    /// End of the cluster range, exclusive.
    pub cluster_end: usize,
    /// Left edge of the glyph's hit box in layer space.
    pub x: f32,
    /// Width of the hit box: the glyph's advance including tracking.
    pub advance: f32,
    /// Pen origin x used for rasterisation (includes the shaper's x offset).
    pub draw_x: f32,
    /// Pen origin y used for rasterisation: the baseline, plus any
    /// sub/superscript shift and the shaper's y offset.
    pub draw_y: f32,
    /// Size this glyph was shaped at, after the sub/superscript factor.
    pub size_px: f32,
    /// Weight requested for this glyph.
    pub weight: FontWeight,
    /// Index into [`ShapedText::lines`].
    pub line: usize,
    /// Whether the glyph belongs to a right-to-left run.
    pub rtl: bool,
    /// The chosen face is too light and must be emboldened when rasterised.
    pub synthetic_bold: bool,
    /// The chosen face is upright and must be skewed when rasterised.
    pub synthetic_italic: bool,
    /// Index into [`ShapedText::styles`].
    pub style_index: usize,
}

/// One visual line — one row of glyphs after wrapping.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedLine {
    /// Index of the paragraph this line belongs to.
    pub paragraph: usize,
    /// First index into [`ShapedText::glyphs`].
    pub first_glyph: usize,
    /// Number of glyphs on this line.
    pub glyph_count: usize,
    /// First byte of the layer's string represented on this line.
    pub byte_start: usize,
    /// One past the last byte represented on this line.
    pub byte_end: usize,
    /// Baseline in layer space.
    pub baseline_y: f32,
    /// Top of the line box.
    pub top: f32,
    /// Bottom of the line box.
    pub bottom: f32,
    /// Left edge of the line's glyphs.
    pub x_min: f32,
    /// Right edge of the line's glyphs.
    pub x_max: f32,
    /// Whether the paragraph reads right to left.
    pub rtl: bool,
}

impl ShapedLine {
    /// Range of glyph indices on this line.
    #[must_use]
    pub const fn glyph_range(&self) -> std::ops::Range<usize> {
        self.first_glyph..self.first_glyph + self.glyph_count
    }
}

/// Which kind of rule a [`Decoration`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecorationKind {
    /// Underline.
    Underline,
    /// Strikethrough.
    Strikethrough,
}

/// A rule to be filled with the run's colour, positioned from the face's own
/// underline/strikeout metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoration {
    /// Underline or strikethrough.
    pub kind: DecorationKind,
    /// Geometry in layer space.
    pub rect: Rect,
    /// Linear straight RGBA of the run.
    pub color: [f32; 4],
    /// Index into [`ShapedText::lines`].
    pub line: usize,
}

/// The laid-out result.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedText {
    /// The string that was laid out; hit-testing and caret maths need it.
    pub text: String,
    /// Resolved styles, one per style segment; glyphs index into this.
    pub styles: Vec<CharStyle>,
    /// Positioned glyphs, in visual order within each line.
    pub glyphs: Vec<ShapedGlyph>,
    /// Visual lines, top to bottom.
    pub lines: Vec<ShapedLine>,
    /// Underline and strikethrough rules.
    pub decorations: Vec<Decoration>,
    /// Union of all line boxes.
    pub bounds: Rect,
    /// Base em size used for layout.
    pub base_size_px: f32,
    /// Resolved leading.
    pub line_height: f32,
    /// Height of the frame, if the run was boxed.
    pub frame_height: Option<f32>,
}

impl ShapedText {
    /// Whether boxed text is taller than its box (overset).
    #[must_use]
    pub fn overflows(&self) -> bool {
        self.frame_height
            .is_some_and(|h| self.bounds.height > h + 1e-4)
    }

    /// Style of a glyph.
    #[must_use]
    pub fn style_of(&self, glyph: &ShapedGlyph) -> &CharStyle {
        self.styles
            .get(glyph.style_index)
            .unwrap_or_else(|| self.styles.last().expect("styles is never empty"))
    }
}

struct Segment {
    start: usize,
    end: usize,
    style: CharStyle,
}

/// Shape and lay out a text run.
///
/// The returned [`ShapedText`] is self-contained: it owns the string and every
/// position it reports, so editing helpers and the rasteriser need nothing
/// else from the shaper.
pub fn shape(library: &mut FontLibrary, run: &TextRun) -> ShapedText {
    let base_size = run.style.size_px.max(MIN_FONT_SIZE_PX);
    let line_height = run
        .paragraph
        .line_height
        .resolve(base_size)
        .max(MIN_FONT_SIZE_PX);
    let segments = build_segments(run);
    let styles: Vec<CharStyle> = segments.iter().map(|s| s.style.clone()).collect();

    let align = match run.paragraph.alignment {
        Alignment::Left => Align::Left,
        Alignment::Center => Align::Center,
        Alignment::Right => Align::Right,
        Alignment::Justify => Align::Justified,
    };

    let (wrap, width_opt) = match run.frame {
        TextFrame::Point => (Wrap::None, None),
        TextFrame::Box { width, .. } => (Wrap::WordOrGlyph, Some(width.max(0.0))),
    };
    let frame_height = match run.frame {
        TextFrame::Point => None,
        TextFrame::Box { height, .. } => height,
    };

    // The base style also drives lines that contain no styled segment at all
    // (an empty paragraph), so the caret there has the right height.
    let default_attrs = attrs_for(&run.style, usize::MAX, line_height);

    let mut buffer = Buffer::new_empty(Metrics::new(base_size, line_height));
    let font_system = library.system_mut();
    buffer.set_wrap(font_system, wrap);
    // Height is deliberately not handed to the shaper: it clips runs, and we
    // would rather report every line and let the caller decide about overset.
    buffer.set_size(font_system, width_opt, None);

    let paragraph_offsets = paragraph_offsets(&run.text);
    buffer.lines.clear();
    for (index, &offset) in paragraph_offsets.iter().enumerate() {
        let (text, ending) = paragraph_slice(&run.text, &paragraph_offsets, index);
        let mut attrs_list = AttrsList::new(&default_attrs);
        for (seg_index, seg) in segments.iter().enumerate() {
            let start = seg.start.max(offset);
            let end = seg.end.min(offset + text.len());
            if start < end {
                attrs_list.add_span(
                    start - offset..end - offset,
                    &attrs_for(&seg.style, seg_index, line_height),
                );
            }
        }
        let mut line = BufferLine::new(text, ending, attrs_list, Shaping::Advanced);
        line.set_align(Some(align));
        buffer.lines.push(line);
    }
    buffer.shape_until_scroll(font_system, false);

    let mut out = ShapedText {
        text: run.text.clone(),
        styles,
        glyphs: Vec::new(),
        lines: Vec::new(),
        decorations: Vec::new(),
        bounds: Rect::default(),
        base_size_px: base_size,
        line_height,
        frame_height,
    };

    let paragraph_step = run.paragraph.space_before + run.paragraph.space_after;
    let empty_line_x = empty_line_x(run);
    let mut previous_paragraph: Option<usize> = None;

    for layout_run in buffer.layout_runs() {
        let paragraph = layout_run.line_i;
        let offset = paragraph_offsets.get(paragraph).copied().unwrap_or(0);
        let extra_y = paragraph as f32 * paragraph_step;
        let first_of_paragraph = previous_paragraph != Some(paragraph);
        previous_paragraph = Some(paragraph);
        let indent = if first_of_paragraph {
            run.paragraph.first_line_indent
        } else {
            0.0
        };
        let indent = if layout_run.rtl { -indent } else { indent };

        let line_index = out.lines.len();
        let first_glyph = out.glyphs.len();
        let baseline_y = layout_run.line_y + extra_y + run.origin[1];
        let top = layout_run.line_top + extra_y + run.origin[1];
        let bottom = top + layout_run.line_height;

        let line_byte_start = layout_run
            .glyphs
            .iter()
            .map(|g| g.start)
            .min()
            .map_or(offset, |local| offset + local);

        for glyph in layout_run.glyphs {
            let style_index = glyph.metadata;
            let style = out.styles.get(style_index).unwrap_or(&run.style);
            let cluster_start = offset + glyph.start;
            let cluster_end = offset + glyph.end;
            let shift = kern_shift(
                run,
                base_size,
                line_byte_start,
                cluster_start,
                layout_run.rtl,
            );
            let x = glyph.x + indent + shift + run.origin[0];
            let baseline = baseline_y + glyph.y + style.script.baseline_shift(base_size);
            let declared = library
                .declared_weight(FontId(glyph.font_id))
                .unwrap_or(FontWeight(glyph.font_weight.0));
            let synthetic_bold = style.allow_synthetic_bold
                && FontWeight(glyph.font_weight.0).needs_synthesis(declared);
            let synthetic_italic = style.allow_synthetic_italic
                && glyph.cache_key_flags.contains(CacheKeyFlags::FAKE_ITALIC);
            out.glyphs.push(ShapedGlyph {
                font: FontId(glyph.font_id),
                glyph_id: glyph.glyph_id,
                cluster_start,
                cluster_end,
                x,
                advance: glyph.w,
                draw_x: x + glyph.font_size * glyph.x_offset,
                draw_y: baseline - glyph.font_size * glyph.y_offset,
                size_px: glyph.font_size,
                weight: FontWeight(glyph.font_weight.0),
                line: line_index,
                rtl: glyph.level.is_rtl(),
                synthetic_bold,
                synthetic_italic,
                style_index,
            });
        }

        let glyph_count = out.glyphs.len() - first_glyph;
        let line_glyphs = &out.glyphs[first_glyph..];
        let (x_min, x_max) = if line_glyphs.is_empty() {
            let x = empty_line_x + indent + run.origin[0];
            (x, x)
        } else {
            line_glyphs
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), g| {
                    (lo.min(g.x), hi.max(g.x + g.advance))
                })
        };
        let byte_start = line_glyphs
            .iter()
            .map(|g| g.cluster_start)
            .min()
            .unwrap_or(offset);
        let byte_end = line_glyphs
            .iter()
            .map(|g| g.cluster_end)
            .max()
            .unwrap_or(offset);

        out.lines.push(ShapedLine {
            paragraph,
            first_glyph,
            glyph_count,
            byte_start,
            byte_end,
            baseline_y,
            top,
            bottom,
            x_min,
            x_max,
            rtl: layout_run.rtl,
        });
    }

    extend_line_ends(&mut out, &paragraph_offsets, &run.text);
    align_point_text(&mut out, run);
    out.bounds = compute_bounds(&out.lines);
    let rules = decorations(library, &out);
    out.decorations = rules;
    out
}

/// Byte offset of each paragraph, mirroring how the shaper splits lines.
fn paragraph_offsets(text: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut last_ending = LineEnding::default();
    for (range, ending) in LineIter::new(text) {
        offsets.push(range.start);
        last_ending = ending;
    }
    if last_ending != LineEnding::None {
        // A trailing line break (or an entirely empty string) leaves one more
        // empty paragraph for the caret to live on.
        offsets.push(text.len());
    }
    offsets
}

fn paragraph_slice<'a>(text: &'a str, offsets: &[usize], index: usize) -> (&'a str, LineEnding) {
    let start = offsets.get(index).copied().unwrap_or(text.len());
    let next = offsets.get(index + 1).copied().unwrap_or(text.len());
    let tail = &text[start..next];
    let ending = if tail.ends_with("\r\n") {
        LineEnding::CrLf
    } else if tail.ends_with("\n\r") {
        LineEnding::LfCr
    } else if tail.ends_with('\n') {
        LineEnding::Lf
    } else if tail.ends_with('\r') {
        LineEnding::Cr
    } else {
        LineEnding::None
    };
    (&tail[..tail.len() - ending.as_str().len()], ending)
}

/// The last visual line of a paragraph owns everything up to the paragraph's
/// end, so a caret placed after the final character is still reachable.
fn extend_line_ends(out: &mut ShapedText, offsets: &[usize], text: &str) {
    for index in 0..out.lines.len() {
        let paragraph = out.lines[index].paragraph;
        let is_last_of_paragraph = out
            .lines
            .get(index + 1)
            .is_none_or(|next| next.paragraph != paragraph);
        if !is_last_of_paragraph {
            continue;
        }
        let (slice, _) = paragraph_slice(text, offsets, paragraph);
        let end = offsets.get(paragraph).copied().unwrap_or(0) + slice.len();
        let line = &mut out.lines[index];
        line.byte_end = line.byte_end.max(end);
        if line.glyph_count == 0 {
            line.byte_start = line.byte_end;
        }
    }
}

/// Align the lines of *point* text about the block they form.
///
/// The shaper aligns a line inside a line box, and point text has none: with
/// no wrap width every paragraph is measured against itself, so a single-line
/// paragraph is always exactly as wide as its own line and the alignment
/// correction is unavoidably zero. Every line of a multi-paragraph point-text
/// layer therefore comes back flush left whatever the setting says.
///
/// Point text is instead aligned about its own block — the widest line — which
/// is what centring a multi-line point-text layer means. Justification has
/// nothing to stretch to without a box, so it degrades to the paragraph's
/// start edge: left for a left-to-right line, right for a right-to-left one.
fn align_point_text(out: &mut ShapedText, run: &TextRun) {
    if !matches!(run.frame, TextFrame::Point) || out.lines.is_empty() {
        return;
    }
    let block_width = out
        .lines
        .iter()
        .fold(0.0_f32, |widest, line| widest.max(line.x_max - line.x_min));
    if !block_width.is_finite() {
        return;
    }
    for index in 0..out.lines.len() {
        let line = &out.lines[index];
        let slack = block_width - (line.x_max - line.x_min);
        let delta = match run.paragraph.alignment {
            Alignment::Left => 0.0,
            Alignment::Center => slack / 2.0,
            Alignment::Right => slack,
            // Mirrored: an RTL paragraph's start edge is on the right.
            Alignment::Justify if line.rtl => slack,
            Alignment::Justify => 0.0,
        };
        if delta == 0.0 || !delta.is_finite() {
            continue;
        }
        let range = line.glyph_range();
        let line = &mut out.lines[index];
        line.x_min += delta;
        line.x_max += delta;
        for glyph in &mut out.glyphs[range] {
            glyph.x += delta;
            glyph.draw_x += delta;
        }
    }
}

fn compute_bounds(lines: &[ShapedLine]) -> Rect {
    let mut bounds: Option<Rect> = None;
    for line in lines {
        let rect = Rect::from_corners(line.x_min, line.top, line.x_max, line.bottom);
        bounds = Some(bounds.map_or(rect, |b| b.union(&rect)));
    }
    bounds.unwrap_or_default()
}

/// Where the caret sits on a line with no glyphs at all.
///
/// A boxed frame has a line box to align against, so the empty line goes to
/// the box edge the alignment names. Point text has no box; its empty lines
/// start at zero here and are moved into place afterwards by
/// [`align_point_text`], along with every other line.
fn empty_line_x(run: &TextRun) -> f32 {
    match (run.paragraph.alignment, run.frame) {
        (Alignment::Center, TextFrame::Box { width, .. }) => width / 2.0,
        (Alignment::Right, TextFrame::Box { width, .. }) => width,
        _ => 0.0,
    }
}

fn kern_shift(
    run: &TextRun,
    base_size: f32,
    line_byte_start: usize,
    index: usize,
    rtl: bool,
) -> f32 {
    if run.kerning.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    for adjustment in &run.kerning {
        if adjustment.index > line_byte_start && adjustment.index <= index {
            total += adjustment.amount;
        }
    }
    let px = total / 1000.0 * base_size;
    if rtl {
        -px
    } else {
        px
    }
}

fn build_segments(run: &TextRun) -> Vec<Segment> {
    let text = &run.text;
    if text.is_empty() {
        return vec![Segment {
            start: 0,
            end: 0,
            style: run.style.clone(),
        }];
    }
    // Clamp once, up front, and use the clamped ranges for *both* the segment
    // boundaries and the style resolution. Resolving against the raw ranges
    // while splitting on clamped ones silently drops any run that does not
    // start on a character boundary: the clamped boundary is below the run's
    // own `start`, so `StyleRun::contains` never matches it.
    let runs = clamped_runs(text, &run.runs);
    let mut bounds: BTreeSet<usize> = BTreeSet::new();
    bounds.insert(0);
    bounds.insert(text.len());
    for style_run in &runs {
        if style_run.start < style_run.end {
            bounds.insert(style_run.start);
            bounds.insert(style_run.end);
        }
    }
    let ordered: Vec<usize> = bounds.into_iter().collect();
    ordered
        .windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| Segment {
            start: w[0],
            end: w[1],
            style: resolve_style(&run.style, &runs, w[0]),
        })
        .collect()
}

/// Widen every run's byte range outwards to the nearest character boundaries.
///
/// A range that begins or ends part-way through a multi-byte character still
/// means that character to the user, so the range grows to include it rather
/// than losing it — and, more importantly, the range the boundaries are built
/// from is then the same range the style resolution tests against.
fn clamped_runs(text: &str, runs: &[StyleRun]) -> Vec<StyleRun> {
    runs.iter()
        .map(|run| StyleRun {
            start: floor_char_boundary(text, run.start),
            end: ceil_char_boundary(text, run.end),
            style: run.style.clone(),
        })
        .collect()
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut index = index;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut index = index;
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn attrs_for(style: &CharStyle, metadata: usize, line_height: f32) -> Attrs<'_> {
    let mut features = FontFeatures::new();
    if !style.ligatures {
        features.disable(FeatureTag::STANDARD_LIGATURES);
        features.disable(FeatureTag::CONTEXTUAL_LIGATURES);
    }
    if !style.kerning {
        features.disable(FeatureTag::KERNING);
    }
    let family = if style.family.is_empty() {
        Family::SansSerif
    } else {
        Family::Name(style.family.as_str())
    };
    let mut attrs = Attrs::new()
        .family(family)
        .weight(Weight(style.weight.0))
        .style(db_style(style.slant))
        .metadata(metadata)
        .metrics(Metrics::new(style.effective_size_px(), line_height))
        .font_features(features);
    if style.tracking != 0.0 {
        attrs = attrs.letter_spacing(style.tracking / 1000.0);
    }
    attrs
}

fn decorations(library: &mut FontLibrary, text: &ShapedText) -> Vec<Decoration> {
    let mut out = Vec::new();
    for (line_index, line) in text.lines.iter().enumerate() {
        let glyphs = &text.glyphs[line.glyph_range()];
        let mut start = 0usize;
        while start < glyphs.len() {
            let head = &glyphs[start];
            let mut end = start + 1;
            while end < glyphs.len()
                && glyphs[end].style_index == head.style_index
                && glyphs[end].font == head.font
                && (glyphs[end].size_px - head.size_px).abs() < 1e-4
            {
                end += 1;
            }
            let style = text.style_of(head);
            if style.underline || style.strikethrough {
                let group = &glyphs[start..end];
                let x0 = group.iter().fold(f32::INFINITY, |a, g| a.min(g.x));
                let x1 = group
                    .iter()
                    .fold(f32::NEG_INFINITY, |a, g| a.max(g.x + g.advance));
                if let Some(metrics) = library.face_metrics(head.font, head.weight) {
                    let scale = metrics.scale(head.size_px);
                    let baseline = head.draw_y;
                    if style.underline {
                        out.push(rule(
                            DecorationKind::Underline,
                            x0,
                            x1,
                            baseline - metrics.underline_offset * scale,
                            metrics.underline_thickness * scale,
                            style.color,
                            line_index,
                        ));
                    }
                    if style.strikethrough {
                        out.push(rule(
                            DecorationKind::Strikethrough,
                            x0,
                            x1,
                            baseline - metrics.strikeout_offset * scale,
                            metrics.strikeout_thickness * scale,
                            style.color,
                            line_index,
                        ));
                    }
                }
            }
            start = end;
        }
    }
    out
}

fn rule(
    kind: DecorationKind,
    x0: f32,
    x1: f32,
    center_y: f32,
    thickness: f32,
    color: [f32; 4],
    line: usize,
) -> Decoration {
    let thickness = thickness.max(1.0);
    Decoration {
        kind,
        rect: Rect {
            x: x0,
            y: center_y - thickness / 2.0,
            width: (x1 - x0).max(0.0),
            height: thickness,
        },
        color,
        line,
    }
}
