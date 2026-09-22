//! W1-B2: the three CSS generic family names the Type tool's Font choice
//! offers (`sans-serif`, `serif`, `monospace`) shape with the library's
//! pinned generic families, not with the missing-family substitute.
//!
//! Before this, `attrs_for` asked `has_family("serif")`, which no
//! installation answers (nothing ships a family literally called "serif"),
//! and fell through to `Family::SansSerif` — so two of the Font combo's three
//! choices drew exactly the same glyphs as the first. These tests load one
//! face of each DejaVu generic and assert on the face every glyph actually
//! came from.

use std::collections::BTreeSet;

use text_engine::{shape, FontLibrary, TextRun};

fn three_generics() -> FontLibrary {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library.load_bytes(dejavu::serif::regular().to_vec());
    library.load_bytes(dejavu::sans_mono::regular().to_vec());
    assert_eq!(
        library.family_names(),
        vec![
            "DejaVu Sans".to_string(),
            "DejaVu Sans Mono".to_string(),
            "DejaVu Serif".to_string(),
        ]
    );
    library
}

/// The family of every glyph a run shaped with, as a set — one entry when
/// the whole run came from one face.
fn families_used(library: &mut FontLibrary, family: &str) -> BTreeSet<String> {
    let shaped = shape(library, &TextRun::point("Mixed 1lI", family, 24.0));
    assert!(!shaped.glyphs.is_empty(), "{family:?} shaped nothing");
    shaped
        .glyphs
        .iter()
        .filter_map(|g| library.face(g.font).map(|f| f.family))
        .collect()
}

#[test]
fn the_three_generic_names_shape_with_three_different_pinned_families() {
    let mut library = three_generics();
    let sans = families_used(&mut library, "sans-serif");
    let serif = families_used(&mut library, "serif");
    let mono = families_used(&mut library, "monospace");
    assert_eq!(sans, BTreeSet::from(["DejaVu Sans".to_string()]));
    assert_eq!(serif, BTreeSet::from(["DejaVu Serif".to_string()]));
    assert_eq!(mono, BTreeSet::from(["DejaVu Sans Mono".to_string()]));
    // The empty request is the generic sans, as it always was.
    assert_eq!(families_used(&mut library, ""), sans);
}

/// A monospace run is the one property a user can see without reading the
/// face name: every glyph advances the same distance. Sans does not.
#[test]
fn a_monospace_request_advances_every_glyph_equally_where_sans_does_not() {
    let mut library = three_generics();
    let mut advances = |family: &str| -> Vec<i32> {
        let shaped = shape(&mut library, &TextRun::point("iiWW", family, 32.0));
        shaped
            .glyphs
            .iter()
            .map(|g| (g.advance * 100.0).round() as i32)
            .collect()
    };
    let mono = advances("monospace");
    assert_eq!(mono.len(), 4);
    assert!(
        mono.iter().all(|a| *a == mono[0]),
        "monospace: equal advances, got {mono:?}"
    );
    let sans = advances("sans-serif");
    assert!(
        sans[0] < sans[2],
        "sans: an i is narrower than a W, got {sans:?}"
    );
}

/// Card 022's reporting contract holds for the generic names: they are not
/// "missing families" and no substitute is reported for them, because
/// shaping does not substitute — it resolves the pinned generic.
#[test]
fn a_generic_name_is_not_reported_as_a_missing_family() {
    let library = three_generics();
    assert_eq!(library.substitute_for("serif"), None);
    assert_eq!(library.substitute_for("monospace"), None);
    assert_eq!(library.substitute_for("sans-serif"), None);
    assert_eq!(library.substitute_for(""), None);
    assert_eq!(
        library.substitute_for("No Such Family"),
        Some("DejaVu Sans".to_string()),
        "a concrete missing family is still reported"
    );
}
