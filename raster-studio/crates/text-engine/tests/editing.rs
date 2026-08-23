//! Editing geometry: hit-testing, carets and selection rectangles.

use text_engine::{shape, FontLibrary, ShapedText, TextRun};

fn library() -> FontLibrary {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library
}

/// index -> caret rect -> hit-test -> the same index, for every character
/// boundary in the string.
fn assert_caret_round_trip(shaped: &ShapedText) {
    for index in 0..=shaped.text.len() {
        if !shaped.text.is_char_boundary(index) {
            continue;
        }
        let caret = shaped.caret_rect(index);
        assert!(caret.height > 0.0, "the caret must have the line's height");
        let hit = shaped.hit_test(caret.x, caret.y + caret.height / 2.0);
        assert_eq!(
            hit, index,
            "caret {caret:?} for byte {index} of {:?} hit-tested back to {hit}",
            shaped.text
        );
    }
}

#[test]
fn caret_and_hit_test_round_trip_on_point_text() {
    let mut library = library();
    // "fi" is a ligature, so this also covers a caret *inside* a single glyph.
    let shaped = shape(&mut library, &TextRun::point("fix me", "DejaVu Sans", 32.0));
    assert_caret_round_trip(&shaped);
}

#[test]
fn caret_and_hit_test_round_trip_on_wrapped_text() {
    let mut library = library();
    let shaped = shape(
        &mut library,
        &TextRun::paragraph(
            "the quick brown fox jumps over the lazy dog",
            "DejaVu Sans",
            20.0,
            200.0,
        ),
    );
    assert!(shaped.lines.len() > 2, "the text must actually wrap");
    assert_caret_round_trip(&shaped);
}

#[test]
fn caret_and_hit_test_round_trip_across_paragraphs() {
    let mut library = library();
    let shaped = shape(
        &mut library,
        &TextRun::point("one\ntwo\n\nfour", "DejaVu Sans", 24.0),
    );
    assert_eq!(shaped.lines.len(), 4);
    assert_caret_round_trip(&shaped);
}

/// Hebrew, a space, then Latin: one direction change in the middle.
const BIDI: &str = "\u{05D0}\u{05D1}\u{05D2} abc";

#[test]
fn caret_and_hit_test_round_trip_on_bidi_text() {
    let mut library = library();
    let shaped = shape(&mut library, &TextRun::point(BIDI, "DejaVu Sans", 32.0));
    assert!(
        shaped.glyphs.iter().any(|g| g.rtl) && shaped.glyphs.iter().any(|g| !g.rtl),
        "the string must really shape into both directions"
    );
    assert_caret_round_trip(&shaped);
}

#[test]
fn the_caret_at_a_direction_boundary_belongs_to_the_run_that_owns_it() {
    let mut library = library();
    let shaped = shape(&mut library, &TextRun::point(BIDI, "DejaVu Sans", 32.0));

    // Byte 7 is the first byte of "abc". The caret there is the caret *before*
    // the 'a', so it sits at the 'a' glyph's leading edge — the left-hand end
    // of the Latin run — and not at the far end of it where the trailing edge
    // of the preceding right-to-left space glyph lies.
    let a = shaped
        .glyphs
        .iter()
        .find(|g| g.cluster_start == 7)
        .expect("a glyph for 'a'");
    assert!(!a.rtl, "'a' shapes into a left-to-right run");
    let caret = shaped.caret_rect(7);
    assert!(
        (caret.x - a.x).abs() < 1e-3,
        "caret_rect(7).x = {} but the 'a' glyph starts at {}",
        caret.x,
        a.x
    );
    assert!(
        (caret.x - shaped.lines[0].x_min).abs() < 1e-3,
        "the Latin run starts at the left edge of the line: {caret:?}"
    );

    // And the end of the string keeps its own, different, position.
    let end = shaped.caret_rect(shaped.text.len());
    assert!(
        (end.x - caret.x).abs() > 1.0,
        "the caret after 'c' must not share the caret before 'a'"
    );
    assert_eq!(
        shaped.hit_test(end.x, end.y + end.height / 2.0),
        shaped.text.len()
    );
}

#[test]
fn caret_and_hit_test_round_trip_on_right_to_left_text() {
    let mut library = library();
    let shaped = shape(
        &mut library,
        &TextRun::point("\u{05D0}\u{05D1}\u{05D2}", "DejaVu Sans", 32.0),
    );
    assert!(
        shaped.glyphs.iter().all(|g| g.rtl),
        "pure Hebrew shapes entirely right to left"
    );
    assert_caret_round_trip(&shaped);
    assert!(
        shaped.caret_rect(0).x > shaped.caret_rect(6).x,
        "the caret before the first character is at the right-hand end"
    );
}

#[test]
fn a_caret_inside_a_ligature_is_subdivided() {
    let mut library = library();
    let shaped = shape(&mut library, &TextRun::point("fi", "DejaVu Sans", 64.0));
    assert_eq!(shaped.glyphs.len(), 1, "one glyph covers both characters");

    let start = shaped.caret_rect(0).x;
    let middle = shaped.caret_rect(1).x;
    let end = shaped.caret_rect(2).x;
    assert!(
        start < middle && middle < end,
        "the caret must have somewhere to stand between f and i: {start} {middle} {end}"
    );
    assert!(
        (middle - (start + end) / 2.0).abs() < 1e-3,
        "the split is proportional across the ligature's advance"
    );
}

#[test]
fn carets_sit_on_the_line_they_belong_to() {
    let mut library = library();
    let shaped = shape(&mut library, &TextRun::point("ab\ncd", "DejaVu Sans", 24.0));
    let first = shaped.caret_rect(0);
    let second = shaped.caret_rect(3);
    assert!(
        second.y > first.y,
        "the second paragraph's caret must be lower"
    );
    assert!((first.height - shaped.line_height).abs() < 1e-3);
    assert_eq!(shaped.line_of_index(0), 0);
    assert_eq!(shaped.line_of_index(4), 1);
}

#[test]
fn hit_testing_clamps_outside_the_block() {
    let mut library = library();
    let shaped = shape(
        &mut library,
        &TextRun::point("abc\ndef", "DejaVu Sans", 24.0),
    );

    assert_eq!(
        shaped.hit_test(-1000.0, -1000.0),
        0,
        "above and left is the start"
    );
    assert_eq!(
        shaped.hit_test(1000.0, 1000.0),
        shaped.text.len(),
        "below and right is the end"
    );
    assert_eq!(shaped.line_at_y(-50.0), 0);
    assert_eq!(shaped.line_at_y(10_000.0), shaped.lines.len() - 1);
}

#[test]
fn selection_rectangles_cover_the_selected_glyphs() {
    let mut library = library();
    let shaped = shape(&mut library, &TextRun::point("abcdef", "DejaVu Sans", 32.0));

    let rects = shaped.selection_rects(1, 4);
    assert_eq!(
        rects.len(),
        1,
        "a contiguous LTR selection is one rectangle"
    );
    let rect = rects[0];
    assert!((rect.x - shaped.glyphs[1].x).abs() < 1e-3);
    assert!((rect.right() - (shaped.glyphs[3].x + shaped.glyphs[3].advance)).abs() < 1e-3);
    assert!((rect.y - shaped.lines[0].top).abs() < 1e-3);
    assert!((rect.height - shaped.line_height).abs() < 1e-3);

    // A selection of everything spans the whole line.
    let all = shaped.selection_rects(0, shaped.text.len());
    assert_eq!(all.len(), 1);
    assert!((all[0].x - shaped.lines[0].x_min).abs() < 1e-3);
    assert!((all[0].right() - shaped.lines[0].x_max).abs() < 1e-3);
}

#[test]
fn a_selection_spanning_lines_produces_one_rectangle_per_line() {
    let mut library = library();
    let shaped = shape(
        &mut library,
        &TextRun::point("ab\ncd\nef", "DejaVu Sans", 24.0),
    );
    let rects = shaped.selection_rects(1, 7);
    assert_eq!(rects.len(), 3, "one rectangle per touched line");
    for pair in rects.windows(2) {
        assert!(pair[1].y > pair[0].y);
    }
}

#[test]
fn an_empty_or_inverted_selection_has_no_rectangles() {
    let mut library = library();
    let shaped = shape(&mut library, &TextRun::point("abc", "DejaVu Sans", 24.0));
    assert!(shaped.selection_rects(2, 2).is_empty());
    assert!(shaped.selection_rects(3, 1).is_empty());
}

#[test]
fn a_bidi_selection_can_split_into_several_rectangles() {
    let mut library = library();
    let shaped = shape(
        &mut library,
        &TextRun::point("\u{05D0}\u{05D1}\u{05D2} abc", "DejaVu Sans", 32.0),
    );
    // Select from the middle of the Hebrew across the space into the Latin:
    // the two directional stretches are not adjacent on screen.
    let rects = shaped.selection_rects(2, 9);
    assert!(
        rects.len() >= 2,
        "a selection straddling a direction change needs more than one rectangle, got {rects:?}"
    );
}

#[test]
fn empty_and_whitespace_only_text_does_not_panic() {
    let mut library = library();
    for text in ["", " ", "   ", "\t", "\n", "\n\n", " \n \n "] {
        let shaped = shape(&mut library, &TextRun::point(text, "DejaVu Sans", 32.0));
        assert!(!shaped.lines.is_empty(), "{text:?} still has a caret line");

        assert_caret_round_trip(&shaped);
        let caret = shaped.caret_rect(0);
        assert!(caret.height > 0.0);
        assert_eq!(shaped.hit_test(0.0, 0.0), 0);
        // Wildly out-of-range indices must be answered, not panic.
        let _ = shaped.caret_rect(usize::MAX);
        let _ = shaped.selection_rects(0, usize::MAX);
        let _ = shaped.caret_stops(999);
    }
}

#[test]
fn caret_stops_are_unique_and_ordered_along_the_line() {
    let mut library = library();
    let shaped = shape(&mut library, &TextRun::point("hello", "DejaVu Sans", 32.0));
    let stops = shaped.caret_stops(0);
    assert_eq!(stops.len(), 6, "five characters give six caret positions");

    let mut indices: Vec<usize> = stops.iter().map(|s| s.index).collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(indices.len(), stops.len(), "no index appears twice");
    assert_eq!(indices, vec![0, 1, 2, 3, 4, 5]);

    let mut by_index = stops.clone();
    by_index.sort_by_key(|s| s.index);
    for pair in by_index.windows(2) {
        assert!(pair[0].x < pair[1].x, "LTR caret stops advance rightwards");
    }
}
