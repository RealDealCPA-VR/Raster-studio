//! A [`FontLibrary`] with no faces at all must still lay out.
//!
//! `FontLibrary::empty()` is public, is what `Default` returns, and is what a
//! machine with no installed fonts gives back from `with_system_fonts()`. The
//! shaper underneath cannot cope with that — it takes the first font its query
//! yields and `expect`s one to exist — and the workspace builds releases with
//! `panic = "abort"`, so reaching it would take the whole process down with no
//! way for a caller to recover. Everything here pins the guard that stops it.

use text_engine::{
    rasterize, shape, Alignment, FontLibrary, GlyphRasterCache, ShapedText, TextRun,
};

fn shaped(text: &str) -> ShapedText {
    let mut library = FontLibrary::empty();
    assert!(library.is_empty(), "the fixture must have no faces");
    shape(&mut library, &TextRun::point(text, "Nope", 24.0))
}

/// The reported case: any non-empty string aborted the process.
#[test]
fn shaping_words_without_any_font_does_not_panic() {
    let shaped = shaped("Hello");
    assert!(shaped.glyphs.is_empty(), "there is no face to shape with");
    assert_eq!(shaped.text, "Hello", "the string is still carried");
    assert_eq!(shaped.lines.len(), 1, "one paragraph, one line");
}

/// Whitespace-only is one of the strings the crate promises never to panic on,
/// and it aborted too — a space is a real cluster and reaches the shaper.
#[test]
fn shaping_whitespace_without_any_font_does_not_panic() {
    let shaped = shaped("   ");
    assert!(shaped.glyphs.is_empty());
    assert_eq!(shaped.lines.len(), 1);
}

#[test]
fn shaping_the_empty_string_without_any_font_does_not_panic() {
    let shaped = shaped("");
    assert!(shaped.glyphs.is_empty());
    assert_eq!(shaped.lines.len(), 1);
}

/// The caret still has somewhere to be, and it is a real, finite line box —
/// not the zero-height `Rect::default()` that an empty `lines` would produce.
#[test]
fn the_caret_is_finite_and_line_tall_without_any_font() {
    for text in ["Hello", "   ", ""] {
        let shaped = shaped(text);
        assert!(!shaped.lines.is_empty(), "{text:?} must produce a line");
        let caret = shaped.caret_rect(0);
        assert!(
            caret.x.is_finite() && caret.y.is_finite(),
            "{text:?} caret must be finite"
        );
        assert_eq!(caret.width, 0.0, "the caret is a zero-width leading edge");
        assert!(
            (caret.height - shaped.line_height).abs() < 1e-4,
            "{text:?} caret must span the line box: {} vs {}",
            caret.height,
            shaped.line_height
        );
        // The whole block is exactly one line box tall and has no width.
        assert!((shaped.bounds.height - shaped.line_height).abs() < 1e-4);
        assert_eq!(shaped.bounds.width, 0.0);
    }
}

/// Hit-testing and selection stay total: they answer, rather than indexing off
/// the end of an empty `lines`.
///
/// The line owns the paragraph's whole byte range, so both of its endpoints
/// offer a caret and — the line having no width — they offer it at the same x.
/// `hit_test` breaks that tie towards the lower index, so every point in the
/// plane maps to the paragraph's start. That is the deliberate half of the
/// trade: owning the range is what lets `caret_rect` find the right *line*, and
/// on a zero-width line the two endpoints are equally defensible answers.
#[test]
fn hit_testing_and_selection_stay_total_without_any_font() {
    let shaped = shaped("Hello");
    assert_eq!(
        shaped.lines[0].byte_start, 0,
        "the line owns the whole paragraph, from its start"
    );
    assert_eq!(shaped.lines[0].byte_end, 5, "...to its end");
    for x in [-1000.0, 0.0, 1000.0] {
        for y in [-1000.0, 0.0, 1000.0] {
            assert_eq!(shaped.hit_test(x, y), 0);
        }
    }
    assert!(
        shaped.selection_rects(0, 5).is_empty(),
        "nothing was drawn, so nothing is selected"
    );
}

/// Rasterising a fontless layout produces an empty mask rather than ink.
#[test]
fn rasterising_without_any_font_gives_an_empty_mask() {
    let mut library = FontLibrary::empty();
    let mut cache = GlyphRasterCache::new();
    let shaped = shape(&mut library, &TextRun::point("Hello", "Nope", 24.0));
    let mask = rasterize(&mut library, &mut cache, &shaped);
    assert!(mask.is_empty(), "no faces, no ink");
    assert_eq!(mask.total_coverage(), 0);
    assert!(cache.is_empty(), "nothing was ever rasterised");
}

/// Paragraphs still stack, with the paragraph spacing applied, so a fontless
/// layout is a degenerate version of the real one rather than a different one.
#[test]
fn paragraphs_still_stack_without_any_font() {
    let mut library = FontLibrary::empty();
    let mut run = TextRun::point("one\ntwo\nthree", "Nope", 20.0);
    run.paragraph.space_before = 3.0;
    run.paragraph.space_after = 2.0;
    run.origin = [7.0, 11.0];
    let shaped = shape(&mut library, &run);

    assert_eq!(shaped.lines.len(), 3);
    let step = shaped.line_height + 5.0;
    for (index, line) in shaped.lines.iter().enumerate() {
        assert_eq!(line.paragraph, index);
        assert!(
            (line.top - (index as f32 * step + 11.0)).abs() < 1e-4,
            "line {index} top {} should be {}",
            line.top,
            index as f32 * step + 11.0
        );
        assert!((line.bottom - line.top - shaped.line_height).abs() < 1e-4);
        // cosmic-text centres a glyphless line's baseline in its line box.
        assert!((line.baseline_y - (line.top + shaped.line_height / 2.0)).abs() < 1e-4);
        assert_eq!(line.x_min, 7.0, "the layer origin still applies");
        assert_eq!(line.x_max, line.x_min);
    }
    // Each line owns its own paragraph's whole byte range — start and end —
    // so every index in the string is on the line its paragraph is on.
    assert_eq!(
        (shaped.lines[0].byte_start, shaped.lines[0].byte_end),
        (0, 3)
    );
    assert_eq!(
        (shaped.lines[1].byte_start, shaped.lines[1].byte_end),
        (4, 7)
    );
    assert_eq!(
        (shaped.lines[2].byte_start, shaped.lines[2].byte_end),
        (8, 13)
    );
}

/// The synthesised geometry is not invented: for a paragraph with no glyphs on
/// it, a library that *has* fonts produces exactly the same line, so the
/// fontless path is the real layout degenerating rather than a second one.
#[test]
fn the_fontless_line_matches_the_shaped_one_for_a_glyphless_paragraph() {
    let mut with_font = FontLibrary::empty();
    with_font.load_bytes(dejavu::sans::regular().to_vec());
    let mut run = TextRun::point("\n\n", "DejaVu Sans", 24.0);
    run.paragraph.space_before = 4.0;
    run.paragraph.space_after = 1.0;
    run.origin = [5.0, 9.0];
    let shaped = shape(&mut with_font, &run);

    let mut without_font = FontLibrary::empty();
    let fontless = shape(&mut without_font, &run);

    assert_eq!(shaped.lines.len(), 3, "three glyphless paragraphs");
    assert_eq!(
        shaped.lines, fontless.lines,
        "a glyphless paragraph lays out the same with or without faces"
    );
    assert_eq!(shaped.bounds, fontless.bounds);
}

/// The caret lands on the line its index is really on.
///
/// Every synthesised line owns its paragraph's whole byte range, so
/// `line_of_index` finds it for the paragraph's interior indices too — not just
/// for the single point at its end. Index 0 is the most common caret index there
/// is (a fresh document, `Ctrl+Home`, a restored selection); before the range was
/// owned it reported the *last* line of the block, because `line_of_index` fell
/// through its loop and returned `lines.len() - 1`.
///
/// The answer is pinned against the same run shaped with a real font rather than
/// against hand-written numbers, so the fontless path cannot drift away from the
/// one it degenerates from.
#[test]
fn every_index_is_on_its_own_paragraphs_line_without_any_font() {
    let text = "one\ntwo\nthree";
    let run = TextRun::point(text, "DejaVu Sans", 20.0);

    let mut without_font = FontLibrary::empty();
    let fontless = shape(&mut without_font, &run);

    let mut with_font = FontLibrary::empty();
    with_font.load_bytes(dejavu::sans::regular().to_vec());
    let shaped = shape(&mut with_font, &run);

    assert_eq!(fontless.lines.len(), 3);
    assert_eq!(
        shaped.lines.len(),
        3,
        "no wrapping, so one line per paragraph"
    );

    assert_eq!(fontless.line_of_index(0), 0, "the caret starts on line 0");
    assert!(
        (fontless.caret_rect(0).y - fontless.lines[0].top).abs() < 1e-4,
        "the caret for index 0 is drawn on the first line, not the last: {} vs {}",
        fontless.caret_rect(0).y,
        fontless.lines[0].top
    );

    // Every index in the string, its end included, maps to the same line it
    // maps to when a font is present, and that caret is drawn at that line's top.
    for index in 0..=text.len() {
        let line = fontless.line_of_index(index);
        assert_eq!(
            line,
            shaped.line_of_index(index),
            "index {index} should be on the same line with and without a font"
        );
        let want = match index {
            0..=3 => 0,
            4..=7 => 1,
            _ => 2,
        };
        assert_eq!(line, want, "index {index} belongs to paragraph {want}");
        assert!(
            (fontless.caret_rect(index).y - fontless.lines[line].top).abs() < 1e-4,
            "the caret for index {index} should be drawn on line {line}"
        );
    }
}

/// The same agreement, over the strings that make paragraph splitting awkward:
/// mixed CR/LF/CRLF terminators, leading and trailing breaks, multi-byte
/// characters, and a right-to-left script.
///
/// Pinned as *equivalence* rather than as absolute ownership on purpose. A byte
/// in the middle of a `\r\n` terminator belongs to no line in either layout —
/// it is not a caret position — and the guarantee worth having is that the
/// fontless answer is the answer a font would have given, not that it is
/// prettier than one.
#[test]
fn line_ownership_without_a_font_matches_the_fonted_layout() {
    for text in [
        "",
        "a",
        "\n\n",
        "x\n",
        "\nx",
        "héllo\nwörld",
        "a\r\nb\rc\nd",
        "مرحبا\nbye",
        "😀\n😀😀",
    ] {
        let mut with_font = FontLibrary::empty();
        with_font.load_bytes(dejavu::sans::regular().to_vec());
        let shaped = shape(&mut with_font, &TextRun::point(text, "DejaVu Sans", 20.0));

        let mut without_font = FontLibrary::empty();
        let fontless = shape(
            &mut without_font,
            &TextRun::point(text, "DejaVu Sans", 20.0),
        );

        assert_eq!(
            fontless.lines.len(),
            shaped.lines.len(),
            "{text:?}: same number of lines"
        );
        for index in 0..=text.len() {
            if !text.is_char_boundary(index) {
                continue;
            }
            let line = fontless.line_of_index(index);
            assert_eq!(
                line,
                shaped.line_of_index(index),
                "{text:?}: index {index} is on a different line without a font"
            );
            assert!(
                (fontless.caret_rect(index).y - fontless.lines[line].top).abs() < 1e-4,
                "{text:?}: the caret for index {index} is off line {line}"
            );
        }
    }
}

/// The documented limit of the agreement above: a **wrapping box** collapses.
///
/// One line per paragraph is the line a font would have produced only for a run
/// that does not wrap. Glyphless text has no advances to break on, so a boxed
/// paragraph that a font wraps over many visual lines is a single line here, and
/// `line_of_index` answers with the paragraph's line rather than the wrapped
/// one. That is what the rustdoc on `shape` and on `push_fontless_lines` now
/// says, and this pins it so the qualification is backed rather than asserted.
#[test]
fn a_wrapping_box_collapses_to_one_line_per_paragraph_without_any_font() {
    let text = "hello world this is a much longer sentence that wraps";
    let run = TextRun::paragraph(text, "DejaVu Sans", 20.0, 90.0);

    let mut with_font = FontLibrary::empty();
    with_font.load_bytes(dejavu::sans::regular().to_vec());
    let shaped = shape(&mut with_font, &run);

    let mut without_font = FontLibrary::empty();
    let fontless = shape(&mut without_font, &run);

    assert!(
        shaped.lines.len() > 1,
        "the fonted box must really wrap for this to say anything, got {} line(s)",
        shaped.lines.len()
    );
    assert_eq!(
        fontless.lines.len(),
        1,
        "one paragraph, one fontless line, however narrow the box"
    );
    assert_eq!(fontless.lines[0].byte_start, 0);
    assert_eq!(
        fontless.lines[0].byte_end,
        text.len(),
        "the single line owns the paragraph's whole range"
    );

    // Every index answers with that one line — including the ones the fonted
    // layout puts on a later line, which is the divergence being documented.
    let mut disagreements = 0;
    for index in 0..=text.len() {
        assert_eq!(
            fontless.line_of_index(index),
            0,
            "index {index} is on the paragraph's only fontless line"
        );
        if shaped.line_of_index(index) != 0 {
            disagreements += 1;
        }
    }
    assert!(
        disagreements > text.len() / 2,
        "most indices should differ from the fonted layout, only {disagreements} did"
    );
}

/// The collapse is per paragraph, not per layer: two wrapping paragraphs still
/// give two lines, each owning its own paragraph.
#[test]
fn a_wrapping_box_keeps_one_line_for_each_paragraph_without_any_font() {
    let text =
        "hello world this is a much longer sentence that wraps\nand a second one that also wraps";
    let run = TextRun::paragraph(text, "DejaVu Sans", 20.0, 90.0);

    let mut with_font = FontLibrary::empty();
    with_font.load_bytes(dejavu::sans::regular().to_vec());
    let shaped = shape(&mut with_font, &run);

    let mut without_font = FontLibrary::empty();
    let fontless = shape(&mut without_font, &run);

    assert!(shaped.lines.len() > 2, "both paragraphs wrap with a font");
    assert_eq!(
        fontless.lines.len(),
        2,
        "two paragraphs, two fontless lines"
    );
    let split = text.find('\n').expect("the fixture has two paragraphs");
    assert_eq!(
        (fontless.lines[0].byte_start, fontless.lines[0].byte_end),
        (0, split)
    );
    assert_eq!(
        (fontless.lines[1].byte_start, fontless.lines[1].byte_end),
        (split + 1, text.len())
    );
    for index in 0..=text.len() {
        let want = usize::from(index > split);
        assert_eq!(
            fontless.line_of_index(index),
            want,
            "index {index} belongs to paragraph {want}"
        );
    }
}

/// The other documented difference: the synthesised line is left to right, so
/// the first-line indent is **not** mirrored for a right-to-left paragraph the
/// way the shaped path mirrors it.
///
/// Both halves are observable through public fields — `ShapedLine::rtl` and the
/// sign of the indent in `x_min`/`x_max` — so the doc says so plainly instead of
/// claiming the direction cannot be seen.
#[test]
fn the_fontless_line_is_left_to_right_and_does_not_mirror_the_indent() {
    let mut run = TextRun::point("مرحبا", "DejaVu Sans", 20.0);
    run.paragraph.first_line_indent = 10.0;

    let mut with_font = FontLibrary::empty();
    with_font.load_bytes(dejavu::sans::regular().to_vec());
    let shaped = shape(&mut with_font, &run);
    assert!(
        shaped.lines[0].rtl,
        "the fixture must really be a right-to-left paragraph"
    );
    assert!(
        shaped.lines[0].x_min < 0.0,
        "the shaped path mirrors the indent leftwards, got x_min {}",
        shaped.lines[0].x_min
    );

    let mut without_font = FontLibrary::empty();
    let fontless = shape(&mut without_font, &run);
    assert_eq!(fontless.lines.len(), 1);
    assert!(
        !fontless.lines[0].rtl,
        "the synthesised line is reported left to right"
    );
    // x = empty_line_x (0 for point text) + first_line_indent + origin[0].
    assert!(
        (fontless.lines[0].x_min - 10.0).abs() < 1e-4,
        "the indent is applied unmirrored, got x_min {}",
        fontless.lines[0].x_min
    );
    assert_eq!(
        fontless.lines[0].x_max, fontless.lines[0].x_min,
        "the line still has no width"
    );
    assert!(
        (fontless.bounds.x - 10.0).abs() < 1e-4,
        "and the unmirrored indent is visible in the bounds too, got {}",
        fontless.bounds.x
    );
}

/// A boxed frame still parks its empty lines against the edge the alignment
/// names, exactly as it does for an empty paragraph in a library with fonts.
#[test]
fn boxed_alignment_still_places_the_empty_line_without_any_font() {
    for (alignment, want) in [
        (Alignment::Left, 0.0),
        (Alignment::Center, 60.0),
        (Alignment::Right, 120.0),
    ] {
        let mut library = FontLibrary::empty();
        let mut run = TextRun::paragraph("Hello", "Nope", 20.0, 120.0);
        run.paragraph.alignment = alignment;
        let shaped = shape(&mut library, &run);
        assert_eq!(shaped.lines.len(), 1);
        assert!(
            (shaped.lines[0].x_min - want).abs() < 1e-4,
            "{alignment:?} should sit at {want}, got {}",
            shaped.lines[0].x_min
        );
    }
}
