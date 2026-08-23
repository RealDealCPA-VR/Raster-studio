//! Shaping: advances, ligatures, kerning, complex scripts, bidi.
//!
//! Every assertion here runs against one embedded font so the numbers are the
//! same on every machine.

use text_engine::{shape, FontLibrary, ShapedText, TextRun};

fn library() -> FontLibrary {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library
}

fn shaped(text: &str, size_px: f32) -> (FontLibrary, ShapedText) {
    let mut library = library();
    let run = TextRun::point(text, "DejaVu Sans", size_px);
    let shaped = shape(&mut library, &run);
    (library, shaped)
}

fn line_width(shaped: &ShapedText) -> f32 {
    shaped.lines[0].x_max - shaped.lines[0].x_min
}

#[test]
fn advances_are_stable_and_non_degenerate() {
    let (_library, shaped) = shaped("Hello", 32.0);
    assert_eq!(shaped.glyphs.len(), 5);

    // Golden advances for DejaVu Sans at 32 px. These come from the font's own
    // hmtx table, so they are exact binary fractions and reproducible.
    let expected = [24.0625_f32, 19.6875, 8.890625, 8.890625, 19.578125];
    for (glyph, want) in shaped.glyphs.iter().zip(expected) {
        assert!(
            (glyph.advance - want).abs() < 1e-3,
            "advance {} should be {want}",
            glyph.advance
        );
        assert!(glyph.advance > 0.0, "no glyph may have a zero advance here");
    }

    // The pen walks forward by exactly the advances.
    let mut pen = shaped.glyphs[0].x;
    for glyph in &shaped.glyphs {
        assert!(
            (glyph.x - pen).abs() < 1e-3,
            "glyph x should follow the pen"
        );
        pen += glyph.advance;
    }
    assert!((line_width(&shaped) - 81.109_375).abs() < 1e-3);

    // Non-degenerate: this is a proportional font, not a fixed advance loop.
    assert_eq!(
        shaped.glyphs[2].advance, shaped.glyphs[3].advance,
        "the two l's must match"
    );
    assert_ne!(
        shaped.glyphs[0].advance, shaped.glyphs[1].advance,
        "H and e must not share an advance"
    );

    // Clusters partition the string.
    let mut cursor = 0;
    for glyph in &shaped.glyphs {
        assert_eq!(glyph.cluster_start, cursor);
        cursor = glyph.cluster_end;
    }
    assert_eq!(cursor, "Hello".len());
}

#[test]
fn a_ligature_shapes_to_fewer_glyphs_than_characters() {
    let mut library = library();
    let mut run = TextRun::point("fifl", "DejaVu Sans", 64.0);

    let with_ligatures = shape(&mut library, &run);
    assert_eq!(
        with_ligatures.glyphs.len(),
        2,
        "fi and fl each collapse to one glyph"
    );
    assert_eq!("fifl".chars().count(), 4);

    // The ligature glyph still reports the whole two-character cluster, which
    // is what lets the caret sit between the f and the i.
    assert_eq!(
        (
            with_ligatures.glyphs[0].cluster_start,
            with_ligatures.glyphs[0].cluster_end
        ),
        (0, 2)
    );

    run.style.ligatures = false;
    let without = shape(&mut library, &run);
    assert_eq!(
        without.glyphs.len(),
        4,
        "turning `liga` off must restore one glyph per character"
    );
}

#[test]
fn kerning_tightens_a_kerned_pair() {
    let mut library = library();
    let mut run = TextRun::point("AVAVAV", "DejaVu Sans", 64.0);

    let kerned = shape(&mut library, &run);
    run.style.kerning = false;
    let unkerned = shape(&mut library, &run);

    assert_eq!(kerned.glyphs.len(), unkerned.glyphs.len());
    assert!(
        line_width(&kerned) < line_width(&unkerned) - 1.0,
        "AV pairs must kern: {} should be well under {}",
        line_width(&kerned),
        line_width(&unkerned)
    );
}

#[test]
fn arabic_uses_contextual_forms() {
    let mut library = library();
    // U+0628 ARABIC LETTER BEH: isolated on its own, initial/medial/final in
    // a run of three. A naive per-character mapping would give the same glyph
    // three times.
    let isolated = shape(
        &mut library,
        &TextRun::point("\u{0628}", "DejaVu Sans", 32.0),
    );
    let joined = shape(
        &mut library,
        &TextRun::point("\u{0628}\u{0628}\u{0628}", "DejaVu Sans", 32.0),
    );

    assert_eq!(isolated.glyphs.len(), 1);
    assert_eq!(joined.glyphs.len(), 3);

    let ids: Vec<u16> = joined.glyphs.iter().map(|g| g.glyph_id).collect();
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "initial, medial and final forms must all differ: {ids:?}"
    );
    assert!(
        !ids.contains(&isolated.glyphs[0].glyph_id),
        "none of the joined forms may be the isolated form"
    );
    assert!(
        joined.lines[0].rtl,
        "an Arabic paragraph reads right to left"
    );
}

#[test]
fn bidi_reorders_a_mixed_paragraph() {
    let mut library = library();
    // Hebrew (RTL) followed by Latin (LTR) inside an RTL paragraph.
    let text = "\u{05D0}\u{05D1}\u{05D2} abc";
    let shaped = shape(&mut library, &TextRun::point(text, "DejaVu Sans", 32.0));

    assert!(shaped.lines[0].rtl);
    assert!(
        shaped.glyphs.iter().any(|g| g.rtl),
        "the Hebrew must be marked right-to-left"
    );
    assert!(
        shaped.glyphs.iter().any(|g| !g.rtl),
        "the Latin must be marked left-to-right"
    );

    let hebrew: Vec<&_> = shaped.glyphs.iter().filter(|g| g.rtl).collect();
    let latin: Vec<&_> = shaped.glyphs.iter().filter(|g| !g.rtl).collect();

    // Within the Hebrew stretch, later characters sit further left.
    for pair in hebrew.windows(2) {
        if pair[0].cluster_start < pair[1].cluster_start {
            assert!(
                pair[0].x > pair[1].x,
                "RTL text must run leftwards as the string advances"
            );
        }
    }
    // Within the Latin stretch, later characters sit further right.
    let mut sorted = latin.clone();
    sorted.sort_by_key(|g| g.cluster_start);
    for pair in sorted.windows(2) {
        assert!(
            pair[0].x < pair[1].x,
            "the embedded LTR run must still read left to right"
        );
    }
    // The whole Latin run sits to the left of the whole Hebrew run.
    let latin_right = latin
        .iter()
        .fold(f32::NEG_INFINITY, |a, g| a.max(g.x + g.advance));
    let hebrew_left = hebrew.iter().fold(f32::INFINITY, |a, g| a.min(g.x));
    assert!(
        latin_right <= hebrew_left + 1e-3,
        "in an RTL paragraph the trailing Latin run is placed first (leftmost)"
    );
}

#[test]
fn a_combining_sequence_shapes_as_one_cluster() {
    // "a" + COMBINING ACUTE ACCENT is three bytes and two scalar values, but
    // it is a single grapheme and the shaper reports it as one cluster.
    let (_library, shaped) = shaped("a\u{0301}", 32.0);
    assert!(!shaped.glyphs.is_empty());
    assert_eq!(shaped.glyphs[0].cluster_start, 0);
    assert_eq!(
        shaped
            .glyphs
            .last()
            .expect("at least one glyph")
            .cluster_end,
        "a\u{0301}".len()
    );
    assert_eq!(
        shaped.glyphs.len(),
        1,
        "DejaVu Sans composes a + acute into a single glyph"
    );
}

#[test]
fn every_glyph_names_the_face_it_came_from() {
    let mut library = library();
    let expected = library
        .resolve(
            "DejaVu Sans",
            text_engine::FontWeight::NORMAL,
            text_engine::FontSlant::Normal,
        )
        .expect("resolves")
        .id;
    let shaped = shape(&mut library, &TextRun::point("Hello", "DejaVu Sans", 20.0));
    assert!(shaped.glyphs.iter().all(|g| g.font == expected));
}
