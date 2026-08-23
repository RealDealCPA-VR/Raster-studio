//! Layout: wrapping, alignment, leading, indents, spacing, styled ranges.

use text_engine::{
    shape, Alignment, DecorationKind, FontLibrary, FontSlant, FontWeight, KernAdjustment,
    LineHeight, ScriptPosition, ShapedText, StyleOverride, StyleRun, TextFrame, TextRun,
};

fn library() -> FontLibrary {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library
}

fn line_texts(shaped: &ShapedText) -> Vec<&str> {
    shaped
        .lines
        .iter()
        .map(|line| &shaped.text[line.byte_start..line.byte_end])
        .collect()
}

fn line_width(shaped: &ShapedText, line: usize) -> f32 {
    shaped.lines[line].x_max - shaped.lines[line].x_min
}

const SENTENCE: &str = "the quick brown fox jumps over the lazy dog";

#[test]
fn point_text_never_wraps() {
    let mut library = library();
    let shaped = shape(&mut library, &TextRun::point(SENTENCE, "DejaVu Sans", 20.0));
    assert_eq!(shaped.lines.len(), 1);
    assert_eq!(line_texts(&shaped), vec![SENTENCE]);
    assert!(
        shaped.bounds.width > 300.0,
        "point text is as wide as it is"
    );
    assert!(!shaped.overflows(), "point text has no box to overflow");
}

#[test]
fn paragraph_text_wraps_at_the_expected_word() {
    let mut library = library();
    let shaped = shape(
        &mut library,
        &TextRun::paragraph(SENTENCE, "DejaVu Sans", 20.0, 200.0),
    );
    assert_eq!(
        line_texts(&shaped),
        vec!["the quick brown fox", "jumps over the lazy", "dog"],
        "the break must fall between whole words"
    );
    for line in &shaped.lines {
        assert!(
            line.x_max - line.x_min <= 200.0 + 1e-3,
            "no line may exceed the box width"
        );
    }
    // Global byte indices, not per-line ones.
    assert_eq!(shaped.lines[1].byte_start, "the quick brown fox ".len());
}

#[test]
fn a_wider_box_needs_fewer_lines() {
    let mut library = library();
    let narrow = shape(
        &mut library,
        &TextRun::paragraph(SENTENCE, "DejaVu Sans", 20.0, 200.0),
    );
    let wide = shape(
        &mut library,
        &TextRun::paragraph(SENTENCE, "DejaVu Sans", 20.0, 400.0),
    );
    assert!(wide.lines.len() < narrow.lines.len());
}

#[test]
fn alignment_positions_lines_in_the_box() {
    let mut library = library();
    let mut run = TextRun::paragraph("hi", "DejaVu Sans", 20.0, 300.0);

    run.paragraph.alignment = Alignment::Left;
    let left = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Center;
    let center = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Right;
    let right = shape(&mut library, &run);

    let width = line_width(&left, 0);
    assert!((left.lines[0].x_min - 0.0).abs() < 1e-3);
    assert!((right.lines[0].x_max - 300.0).abs() < 1e-3);
    assert!(
        (center.lines[0].x_min - (300.0 - width) / 2.0).abs() < 1e-3,
        "centred text must be inset by half the slack"
    );
    // Same glyphs, same widths, only the offset moves.
    for shaped in [&center, &right] {
        assert!((line_width(shaped, 0) - width).abs() < 1e-3);
    }
}

#[test]
fn point_text_aligns_its_lines_about_the_block() {
    let mut library = library();
    let mut run = TextRun::point("a\nlonger line\nmm", "DejaVu Sans", 20.0);

    run.paragraph.alignment = Alignment::Left;
    let left = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Center;
    let center = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Right;
    let right = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Justify;
    let justified = shape(&mut library, &run);

    assert_eq!(left.lines.len(), 3, "three paragraphs, three lines");
    let block = left
        .lines
        .iter()
        .fold(0.0_f32, |a, l| a.max(l.x_max - l.x_min));
    assert!(block > 0.0);
    assert!(
        left.lines.iter().any(|l| l.x_max - l.x_min < block - 1.0),
        "the lines must be of different widths for this test to mean anything"
    );

    for index in 0..left.lines.len() {
        let width = line_width(&left, index);
        assert!(
            (line_width(&center, index) - width).abs() < 1e-3
                && (line_width(&right, index) - width).abs() < 1e-3,
            "alignment moves lines, it does not resize them"
        );
        assert!(
            left.lines[index].x_min.abs() < 1e-3,
            "left alignment leaves every line flush at the origin"
        );
        let middle = (center.lines[index].x_min + center.lines[index].x_max) / 2.0;
        assert!(
            (middle - block / 2.0).abs() < 1e-3,
            "centred line {index} sits at {middle}, not the block centre {}",
            block / 2.0
        );
        assert!(
            (right.lines[index].x_max - block).abs() < 1e-3,
            "right-aligned line {index} ends at {}, not the block edge {block}",
            right.lines[index].x_max
        );
        assert!(
            (justified.lines[index].x_min - left.lines[index].x_min).abs() < 1e-3,
            "justification has nothing to stretch against without a box, so it \
             degrades to the start edge"
        );
    }

    // The glyphs move with the line box, not just the reported extents.
    for index in 0..left.glyphs.len() {
        let line = left.glyphs[index].line;
        let delta = center.lines[line].x_min - left.lines[line].x_min;
        assert!((center.glyphs[index].x - left.glyphs[index].x - delta).abs() < 1e-3);
        assert!((center.glyphs[index].draw_x - left.glyphs[index].draw_x - delta).abs() < 1e-3);
    }

    // The block itself keeps its size and its anchor: only the short lines move.
    for shaped in [&center, &right] {
        assert!((shaped.bounds.x - left.bounds.x).abs() < 1e-3);
        assert!((shaped.bounds.width - left.bounds.width).abs() < 1e-3);
    }

    // A caret placed on the short first line follows the alignment.
    let caret = center.caret_rect(0);
    assert!(
        (caret.x - center.lines[0].x_min).abs() < 1e-3,
        "the caret at index 0 sits at the centred line's own left edge"
    );
    assert_eq!(center.hit_test(caret.x, caret.y + caret.height / 2.0), 0);
}

#[test]
fn a_blank_paragraph_in_point_text_takes_the_alignment_too() {
    let mut library = library();
    let mut run = TextRun::point("a wide line\n\nx", "DejaVu Sans", 20.0);
    run.paragraph.alignment = Alignment::Center;
    let centered = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Right;
    let right = shape(&mut library, &run);

    assert_eq!(centered.lines.len(), 3);
    let block = line_width(&centered, 0);
    let blank = &centered.lines[1];
    assert_eq!(blank.glyph_count, 0, "the middle paragraph is empty");
    assert!(
        (blank.x_min - block / 2.0).abs() < 1e-3,
        "the caret on an empty centred line sits at the block's centre, at {}",
        blank.x_min
    );
    assert_eq!(blank.byte_start, blank.byte_end);
    assert!((right.lines[1].x_min - block).abs() < 1e-3);

    // And the caret geometry agrees with the line box.
    let caret = centered.caret_rect(blank.byte_start);
    assert!((caret.x - block / 2.0).abs() < 1e-3);
    assert_eq!(
        centered.hit_test(caret.x, caret.y + caret.height / 2.0),
        blank.byte_start
    );
}

#[test]
fn point_text_alignment_is_mirrored_for_a_right_to_left_block() {
    let mut library = library();
    // Two Hebrew paragraphs of different lengths: RTL, so the start edge is on
    // the right and justification must fall back to *that* edge.
    let text = "\u{05D0}\u{05D1}\u{05D2}\u{05D3}\n\u{05D0}";
    let mut run = TextRun::point(text, "DejaVu Sans", 24.0);

    run.paragraph.alignment = Alignment::Left;
    let left = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Right;
    let right = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Justify;
    let justified = shape(&mut library, &run);

    assert_eq!(left.lines.len(), 2);
    assert!(left.lines.iter().all(|l| l.rtl), "both lines read RTL");
    let block = left
        .lines
        .iter()
        .fold(0.0_f32, |a, l| a.max(l.x_max - l.x_min));

    for index in 0..2 {
        assert!(
            left.lines[index].x_min.abs() < 1e-3,
            "physical left alignment still means flush left for RTL text"
        );
        assert!(
            (right.lines[index].x_max - block).abs() < 1e-3,
            "right alignment puts both RTL lines against the block's right edge"
        );
        assert!(
            (justified.lines[index].x_max - block).abs() < 1e-3,
            "an unjustifiable RTL line falls back to its own start edge, the right"
        );
    }
}

#[test]
fn justify_stretches_every_line_but_the_last() {
    let mut library = library();
    let mut run = TextRun::paragraph("aa bb cc dd ee ff gg hh", "DejaVu Sans", 20.0, 150.0);
    run.paragraph.alignment = Alignment::Left;
    let ragged = shape(&mut library, &run);
    run.paragraph.alignment = Alignment::Justify;
    let justified = shape(&mut library, &run);

    assert!(justified.lines.len() >= 2);
    assert_eq!(line_texts(&ragged), line_texts(&justified));
    assert!(
        (justified.lines[0].x_max - 150.0).abs() < 1e-3,
        "the first line must reach the right edge"
    );
    assert!(
        line_width(&ragged, 0) < 150.0 - 1.0,
        "which it did not do before justification"
    );
    let last = justified.lines.len() - 1;
    assert!(
        (line_width(&justified, last) - line_width(&ragged, last)).abs() < 1e-3,
        "the last line of a paragraph is never justified"
    );
}

#[test]
fn leading_controls_the_distance_between_baselines() {
    let mut library = library();
    let mut run = TextRun::point("one\ntwo", "DejaVu Sans", 20.0);

    run.paragraph.line_height = LineHeight::Multiple(1.0);
    let tight = shape(&mut library, &run);
    run.paragraph.line_height = LineHeight::Multiple(2.0);
    let loose = shape(&mut library, &run);
    run.paragraph.line_height = LineHeight::Absolute(45.0);
    let absolute = shape(&mut library, &run);

    let gap = |s: &ShapedText| s.lines[1].baseline_y - s.lines[0].baseline_y;
    assert!((gap(&tight) - 20.0).abs() < 1e-3);
    assert!((gap(&loose) - 40.0).abs() < 1e-3);
    assert!((gap(&absolute) - 45.0).abs() < 1e-3);
}

#[test]
fn tracking_widens_the_line_and_changes_where_it_wraps() {
    let mut library = library();
    let mut run = TextRun::point("iiii", "DejaVu Sans", 40.0);
    let plain = shape(&mut library, &run);
    run.style.tracking = 200.0; // 0.2 em per glyph
    let tracked = shape(&mut library, &run);

    let delta = line_width(&tracked, 0) - line_width(&plain, 0);
    assert!(
        (delta - 4.0 * 0.2 * 40.0).abs() < 1e-2,
        "four glyphs each gain 0.2 em: {delta}"
    );

    // Tracking is fed to the shaper, so it also moves the line breaks.
    let mut boxed = TextRun::paragraph(SENTENCE, "DejaVu Sans", 20.0, 200.0);
    let before = shape(&mut library, &boxed).lines.len();
    boxed.style.tracking = 150.0;
    let after = shape(&mut library, &boxed).lines.len();
    assert!(
        after > before,
        "wider tracking must force more lines ({before} -> {after})"
    );
}

#[test]
fn first_line_indent_shifts_only_the_first_line_of_each_paragraph() {
    let mut library = library();
    let mut run = TextRun::paragraph(
        "the quick brown fox jumps over\nsecond paragraph here",
        "DejaVu Sans",
        20.0,
        200.0,
    );
    let plain = shape(&mut library, &run);
    run.paragraph.first_line_indent = 25.0;
    let indented = shape(&mut library, &run);

    assert_eq!(plain.lines.len(), indented.lines.len());
    let mut previous_paragraph = usize::MAX;
    for (before, after) in plain.lines.iter().zip(&indented.lines) {
        let first_of_paragraph = before.paragraph != previous_paragraph;
        previous_paragraph = before.paragraph;
        let shift = after.x_min - before.x_min;
        if first_of_paragraph {
            assert!(
                (shift - 25.0).abs() < 1e-3,
                "the first line of paragraph {} must move by the indent, moved {shift}",
                before.paragraph
            );
        } else {
            assert!(
                shift.abs() < 1e-3,
                "continuation lines must not move, moved {shift}"
            );
        }
    }
    assert!(
        indented.lines.iter().any(|l| l.paragraph == 1),
        "the test needs two paragraphs to be meaningful"
    );
}

#[test]
fn paragraph_spacing_adds_a_gap_between_paragraphs_only() {
    let mut library = library();
    let mut run = TextRun::paragraph(
        "the quick brown fox jumps over\nsecond",
        "DejaVu Sans",
        20.0,
        200.0,
    );
    let plain = shape(&mut library, &run);
    run.paragraph.space_before = 7.0;
    run.paragraph.space_after = 3.0;
    let spaced = shape(&mut library, &run);

    assert_eq!(plain.lines.len(), spaced.lines.len());
    for (before, after) in plain.lines.iter().zip(&spaced.lines) {
        let expected = after.paragraph as f32 * 10.0;
        assert!(
            (after.top - before.top - expected).abs() < 1e-3,
            "paragraph {} should drop by {expected}",
            after.paragraph
        );
    }
    assert!(spaced.bounds.height > plain.bounds.height);
}

#[test]
fn manual_kerning_shifts_the_glyphs_after_the_adjustment() {
    let mut library = library();
    let plain = shape(&mut library, &TextRun::point("ABC", "DejaVu Sans", 100.0));
    let kerned = shape(
        &mut library,
        &TextRun::point("ABC", "DejaVu Sans", 100.0)
            .with_kerning(vec![KernAdjustment::new(1, 500.0)]),
    );

    assert_eq!(plain.glyphs.len(), kerned.glyphs.len());
    assert!(
        (kerned.glyphs[0].x - plain.glyphs[0].x).abs() < 1e-3,
        "the glyph before the adjustment must not move"
    );
    for index in 1..3 {
        let shift = kerned.glyphs[index].x - plain.glyphs[index].x;
        assert!(
            (shift - 50.0).abs() < 1e-3,
            "glyph {index} should move by 500/1000 em of 100 px, moved {shift}"
        );
    }
    let tight = shape(
        &mut library,
        &TextRun::point("ABC", "DejaVu Sans", 100.0)
            .with_kerning(vec![KernAdjustment::new(1, -100.0)]),
    );
    assert!(
        tight.glyphs[1].x < plain.glyphs[1].x,
        "a negative adjustment must tighten"
    );
}

#[test]
fn style_runs_apply_to_character_ranges_not_the_whole_layer() {
    let mut library = library();
    let run = TextRun::point("abcd", "DejaVu Sans", 40.0).with_runs(vec![
        StyleRun::new(
            1,
            3,
            StyleOverride::default()
                .with_color([1.0, 0.0, 0.0, 1.0])
                .with_size_px(20.0),
        ),
        StyleRun::new(3, 4, StyleOverride::default().with_underline(true)),
    ]);
    let shaped = shape(&mut library, &run);
    assert_eq!(shaped.glyphs.len(), 4);

    let colors: Vec<[f32; 4]> = shaped
        .glyphs
        .iter()
        .map(|g| shaped.style_of(g).color)
        .collect();
    assert_eq!(colors[0], [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(colors[1], [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(colors[2], [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(colors[3], [0.0, 0.0, 0.0, 1.0]);

    assert!((shaped.glyphs[0].size_px - 40.0).abs() < 1e-3);
    assert!((shaped.glyphs[1].size_px - 20.0).abs() < 1e-3);
    assert!((shaped.glyphs[3].size_px - 40.0).abs() < 1e-3);
    assert!(
        shaped.glyphs[1].advance < shaped.glyphs[0].advance,
        "a smaller run must advance less"
    );

    // Exactly one underline, spanning only the last character.
    let underlines: Vec<_> = shaped
        .decorations
        .iter()
        .filter(|d| d.kind == DecorationKind::Underline)
        .collect();
    assert_eq!(underlines.len(), 1);
    assert!((underlines[0].rect.x - shaped.glyphs[3].x).abs() < 1e-3);
    assert!((underlines[0].rect.width - shaped.glyphs[3].advance).abs() < 1e-3);
}

#[test]
fn a_style_run_can_switch_family_and_weight() {
    let mut library = library();
    library.load_bytes(dejavu::sans::bold().to_vec());
    library.load_bytes(dejavu::serif::regular().to_vec());

    let run = TextRun::point("ab", "DejaVu Sans", 40.0).with_runs(vec![
        StyleRun::new(0, 1, StyleOverride::default().with_weight(FontWeight::BOLD)),
        StyleRun::new(1, 2, StyleOverride::default().with_family("DejaVu Serif")),
    ]);
    let shaped = shape(&mut library, &run);

    let first = library.face(shaped.glyphs[0].font).expect("face");
    let second = library.face(shaped.glyphs[1].font).expect("face");
    assert_eq!(first.family, "DejaVu Sans");
    assert_eq!(first.weight, FontWeight::BOLD);
    assert_eq!(second.family, "DejaVu Serif");
    assert!(!shaped.glyphs[0].synthetic_bold, "a real bold face exists");
}

#[test]
fn a_style_run_that_starts_mid_character_still_styles_its_range() {
    let mut library = library();
    library.load_bytes(dejavu::sans::bold().to_vec());

    // "é" occupies bytes 0..2 and 'b' byte 2..3, so a run starting at byte 1
    // starts inside a character. 'b' is squarely inside 1..3 either way and
    // must come out bold.
    let run = TextRun::point("\u{00e9}b", "DejaVu Sans", 40.0).with_runs(vec![StyleRun::new(
        1,
        3,
        StyleOverride::default().with_weight(FontWeight::BOLD),
    )]);
    let shaped = shape(&mut library, &run);

    let b = shaped
        .glyphs
        .iter()
        .find(|g| g.cluster_start == 2)
        .expect("a glyph for 'b'");
    assert_eq!(b.weight, FontWeight::BOLD);
    let face = library.face(b.font).expect("face");
    assert_eq!(
        face.weight,
        FontWeight::BOLD,
        "'b' must be shaped on the real bold face, not left at weight 400"
    );
    assert!(!b.synthetic_bold, "a real bold face was loaded");

    // The partly covered character joins the run rather than being dropped.
    let e = shaped
        .glyphs
        .iter()
        .find(|g| g.cluster_start == 0)
        .expect("a glyph for 'é'");
    assert_eq!(e.weight, FontWeight::BOLD);
}

#[test]
fn synthesis_flags_follow_what_the_family_actually_has() {
    let mut library = library();
    let bold_request = TextRun::point("H", "DejaVu Sans", 48.0).with_runs(vec![StyleRun::new(
        0,
        1,
        StyleOverride::default().with_weight(FontWeight::BOLD),
    )]);
    let italic_request = TextRun::point("H", "DejaVu Sans", 48.0).with_runs(vec![StyleRun::new(
        0,
        1,
        StyleOverride::default().with_slant(FontSlant::Italic),
    )]);

    let faux_bold = shape(&mut library, &bold_request);
    assert!(faux_bold.glyphs[0].synthetic_bold);
    assert!(!faux_bold.glyphs[0].synthetic_italic);

    let faux_italic = shape(&mut library, &italic_request);
    assert!(faux_italic.glyphs[0].synthetic_italic);
    assert!(!faux_italic.glyphs[0].synthetic_bold);

    let mut opted_out = bold_request.clone();
    opted_out.style.allow_synthetic_bold = false;
    assert!(
        !shape(&mut library, &opted_out).glyphs[0].synthetic_bold,
        "the style can refuse faux bold"
    );

    library.load_bytes(dejavu::sans::bold().to_vec());
    assert!(
        !shape(&mut library, &bold_request).glyphs[0].synthetic_bold,
        "installing the real bold face must stop the synthesis"
    );
}

#[test]
fn superscript_shrinks_and_raises_only_its_own_range() {
    let mut library = library();
    let run = TextRun::point("x2", "DejaVu Sans", 40.0).with_runs(vec![StyleRun::new(
        1,
        2,
        StyleOverride::default().with_script(ScriptPosition::Superscript),
    )]);
    let shaped = shape(&mut library, &run);
    let baseline = shaped.lines[0].baseline_y;

    assert!((shaped.glyphs[0].size_px - 40.0).abs() < 1e-3);
    assert!((shaped.glyphs[1].size_px - 40.0 * 0.583).abs() < 1e-2);
    assert!((shaped.glyphs[0].draw_y - baseline).abs() < 1e-3);
    assert!(
        (baseline - shaped.glyphs[1].draw_y - 0.333 * 40.0).abs() < 1e-2,
        "the superscript must sit a third of an em above the baseline"
    );

    let sub = TextRun::point("x2", "DejaVu Sans", 40.0).with_runs(vec![StyleRun::new(
        1,
        2,
        StyleOverride::default().with_script(ScriptPosition::Subscript),
    )]);
    let shaped = shape(&mut library, &sub);
    assert!(
        shaped.glyphs[1].draw_y > shaped.lines[0].baseline_y,
        "a subscript drops below the baseline"
    );
    assert_eq!(shaped.lines.len(), 1, "sub/superscript must not add a line");
}

#[test]
fn decorations_straddle_the_baseline_using_the_faces_own_metrics() {
    let mut library = library();
    let run = TextRun::point("under", "DejaVu Sans", 40.0).with_runs(vec![StyleRun::new(
        0,
        5,
        StyleOverride::default()
            .with_underline(true)
            .with_strikethrough(true),
    )]);
    let shaped = shape(&mut library, &run);
    let baseline = shaped.lines[0].baseline_y;

    assert_eq!(shaped.decorations.len(), 2);
    let underline = shaped
        .decorations
        .iter()
        .find(|d| d.kind == DecorationKind::Underline)
        .expect("underline");
    let strike = shaped
        .decorations
        .iter()
        .find(|d| d.kind == DecorationKind::Strikethrough)
        .expect("strikethrough");

    assert!(
        underline.rect.y > baseline,
        "underlines sit below the baseline"
    );
    assert!(strike.rect.bottom() < baseline, "strikeouts sit above it");
    assert!(underline.rect.height >= 1.0);
    for decoration in [underline, strike] {
        assert!((decoration.rect.x - shaped.lines[0].x_min).abs() < 1e-3);
        assert!(
            (decoration.rect.right() - shaped.lines[0].x_max).abs() < 1e-3,
            "a rule spans the styled run"
        );
    }

    let plain = shape(&mut library, &TextRun::point("under", "DejaVu Sans", 40.0));
    assert!(plain.decorations.is_empty());
}

#[test]
fn every_line_ending_starts_a_new_paragraph_with_global_indices() {
    let mut library = library();
    for (text, first, second) in [
        ("ab\ncd", "ab", "cd"),
        ("ab\r\ncd", "ab", "cd"),
        ("ab\rcd", "ab", "cd"),
    ] {
        let shaped = shape(&mut library, &TextRun::point(text, "DejaVu Sans", 20.0));
        assert_eq!(shaped.lines.len(), 2, "{text:?} is two paragraphs");
        assert_eq!(line_texts(&shaped), vec![first, second]);
        assert_eq!(shaped.lines[0].paragraph, 0);
        assert_eq!(shaped.lines[1].paragraph, 1);
        assert_eq!(
            shaped.lines[1].byte_end,
            text.len(),
            "indices address the layer's own string"
        );
    }
}

#[test]
fn the_origin_translates_the_whole_block() {
    let mut library = library();
    let at_zero = shape(&mut library, &TextRun::point("Hello", "DejaVu Sans", 32.0));
    let moved = shape(
        &mut library,
        &TextRun::point("Hello", "DejaVu Sans", 32.0).with_origin([100.0, 50.0]),
    );
    assert!((moved.bounds.x - at_zero.bounds.x - 100.0).abs() < 1e-3);
    assert!((moved.bounds.y - at_zero.bounds.y - 50.0).abs() < 1e-3);
    for (a, b) in at_zero.glyphs.iter().zip(&moved.glyphs) {
        assert!((b.x - a.x - 100.0).abs() < 1e-3);
        assert!((b.draw_y - a.draw_y - 50.0).abs() < 1e-3);
    }
}

#[test]
fn overset_is_reported_for_a_box_that_is_too_short() {
    let mut library = library();
    let mut run = TextRun::paragraph(SENTENCE, "DejaVu Sans", 20.0, 200.0);
    run.frame = TextFrame::Box {
        width: 200.0,
        height: Some(500.0),
    };
    let roomy = shape(&mut library, &run);
    assert!(!roomy.overflows());

    run.frame = TextFrame::Box {
        width: 200.0,
        height: Some(30.0),
    };
    let cramped = shape(&mut library, &run);
    assert!(cramped.overflows());
    assert_eq!(
        cramped.lines.len(),
        roomy.lines.len(),
        "overset lines are reported, not dropped"
    );
}

#[test]
fn zero_and_negative_sizes_do_not_panic() {
    let mut library = library();
    for size in [0.0_f32, -5.0, f32::MIN_POSITIVE] {
        let shaped = shape(&mut library, &TextRun::point("Hello", "DejaVu Sans", size));
        assert!(!shaped.lines.is_empty());
        assert!(shaped.bounds.height.is_finite());
    }
}
