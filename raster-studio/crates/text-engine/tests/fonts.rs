//! Font enumeration, loading and matching.

use text_engine::{shape, FontLibrary, FontSlant, FontStretch, FontWeight, TextRun};

fn regular_only() -> FontLibrary {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library
}

#[test]
fn an_empty_library_has_no_fonts() {
    let library = FontLibrary::empty();
    assert!(library.is_empty());
    assert_eq!(library.face_count(), 0);
    assert!(library.family_names().is_empty());
    assert!(library.families().is_empty());
    assert!(!library.has_family("DejaVu Sans"));
    assert!(library
        .resolve(
            "DejaVu Sans",
            FontWeight::NORMAL,
            FontStretch::Normal,
            FontSlant::Normal
        )
        .is_none());
}

#[test]
fn loading_bytes_registers_the_family() {
    let mut library = FontLibrary::empty();
    let ids = library.load_bytes(dejavu::sans::regular().to_vec());

    assert_eq!(ids.len(), 1, "the blob holds exactly one face");
    assert_eq!(library.face_count(), 1);
    assert_eq!(library.family_names(), vec!["DejaVu Sans".to_string()]);
    assert!(library.has_family("DejaVu Sans"));

    let face = library
        .face(ids[0])
        .expect("the loaded face is retrievable");
    assert_eq!(face.family, "DejaVu Sans");
    assert_eq!(face.post_script_name, "DejaVuSans");
    assert_eq!(face.weight, FontWeight::NORMAL);
    assert_eq!(face.slant, FontSlant::Normal);
    assert!(!face.monospaced);
}

#[test]
fn faces_of_one_family_are_grouped_and_ordered() {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library.load_bytes(dejavu::sans::bold().to_vec());
    library.load_bytes(dejavu::sans::oblique().to_vec());

    let families = library.families();
    assert_eq!(families.len(), 1, "all three are one family");
    let faces = &families[0].faces;
    assert_eq!(faces.len(), 3);
    // Sorted by weight, then upright before slanted. DejaVu's oblique face
    // declares itself italic in its OS/2 table, and the database reports what
    // the font says rather than what the filename says.
    assert_eq!(
        faces
            .iter()
            .map(|f| (f.weight.0, f.slant))
            .collect::<Vec<_>>(),
        vec![
            (400, FontSlant::Normal),
            (400, FontSlant::Italic),
            (700, FontSlant::Normal),
        ]
    );
}

#[test]
fn resolve_flags_synthetic_bold_only_when_no_bold_face_exists() {
    let light_only = regular_only();
    let faked = light_only
        .resolve(
            "DejaVu Sans",
            FontWeight::BOLD,
            FontStretch::Normal,
            FontSlant::Normal,
        )
        .expect("the regular face still matches");
    assert!(faked.synthetic_bold, "400 cannot stand in for 700");
    assert!(!faked.synthetic_italic);

    let mut with_bold = regular_only();
    with_bold.load_bytes(dejavu::sans::bold().to_vec());
    let real = with_bold
        .resolve(
            "DejaVu Sans",
            FontWeight::BOLD,
            FontStretch::Normal,
            FontSlant::Normal,
        )
        .expect("bold matches");
    assert!(!real.synthetic_bold, "a real bold face needs no synthesis");
    assert_ne!(
        real.id, faked.id,
        "the bold request must pick a different face once bold is installed"
    );
}

#[test]
fn resolve_flags_synthetic_italic_only_when_no_slanted_face_exists() {
    let upright_only = regular_only();
    let faked = upright_only
        .resolve(
            "DejaVu Sans",
            FontWeight::NORMAL,
            FontStretch::Normal,
            FontSlant::Italic,
        )
        .expect("the upright face still matches");
    assert!(faked.synthetic_italic);

    let mut with_oblique = regular_only();
    with_oblique.load_bytes(dejavu::sans::oblique().to_vec());
    let real = with_oblique
        .resolve(
            "DejaVu Sans",
            FontWeight::NORMAL,
            FontStretch::Normal,
            FontSlant::Italic,
        )
        .expect("oblique matches an italic request");
    assert!(!real.synthetic_italic);
}

#[test]
fn an_unknown_family_does_not_resolve() {
    let library = regular_only();
    assert!(library
        .resolve(
            "No Such Family",
            FontWeight::NORMAL,
            FontStretch::Normal,
            FontSlant::Normal
        )
        .is_none());
}

#[test]
fn the_generic_sans_serif_family_is_repointed_at_what_is_installed() {
    // The shaping stack's built-in generic defaults name families that are
    // usually absent; without repair an empty family string resolves to
    // nothing at all.
    let library = regular_only();
    let matched = library
        .resolve(
            "",
            FontWeight::NORMAL,
            FontStretch::Normal,
            FontSlant::Normal,
        )
        .expect("the generic sans-serif family must resolve to the one loaded family");
    assert_eq!(
        library.face(matched.id).expect("face exists").family,
        "DejaVu Sans"
    );
}

#[test]
fn face_metrics_come_from_the_font() {
    let mut library = regular_only();
    let id = library
        .resolve(
            "DejaVu Sans",
            FontWeight::NORMAL,
            FontStretch::Normal,
            FontSlant::Normal,
        )
        .expect("resolves")
        .id;
    let metrics = library
        .face_metrics(id, FontWeight::NORMAL)
        .expect("metrics available");

    assert_eq!(metrics.units_per_em, 2048.0, "DejaVu is a 2048-upem font");
    assert!(metrics.ascent > 0.0);
    assert!(metrics.descent < 0.0);
    assert!(
        metrics.underline_offset < 0.0,
        "underlines sit below the baseline in font units"
    );
    assert!(metrics.underline_thickness > 0.0);
    assert!(
        metrics.strikeout_offset > 0.0,
        "strikeouts sit above the baseline"
    );
    // A 2048-upem face at 32px scales by 1/64.
    assert!((metrics.scale(32.0) - 32.0 / 2048.0).abs() < 1e-9);
}

#[test]
fn the_system_library_is_usable_when_the_machine_has_fonts() {
    let mut library = FontLibrary::with_system_fonts();
    if library.face_count() == 0 {
        // A machine with no fonts installed at all is a legitimate state, and
        // the claim is not merely that construction survived it: laying text
        // out against that library must be safe too, because the shaper
        // underneath aborts on an empty database. `tests/no_fonts.rs` pins the
        // behaviour in full; here we only prove the *system* library reaches
        // the same guard.
        assert!(library.family_names().is_empty());
        let shaped = shape(&mut library, &TextRun::point("Hello", "", 24.0));
        assert!(shaped.glyphs.is_empty());
        assert!(!shaped.lines.is_empty(), "the caret still has a line");
        return;
    }
    assert!(!library.family_names().is_empty());
    assert!(
        library
            .resolve(
                "",
                FontWeight::NORMAL,
                FontStretch::Normal,
                FontSlant::Normal
            )
            .is_some(),
        "the generic sans-serif family must resolve on a machine that has fonts"
    );
    let first = library.families().remove(0);
    assert!(!first.faces.is_empty());
    assert!(library.has_family(&first.name));
}

// -- card 022: face selection reaches condensed faces; substitution is
//    reported and applied by one shared rule -------------------------------

fn condensed_and_regular() -> FontLibrary {
    let mut library = FontLibrary::empty();
    library.load_bytes(dejavu::sans::regular().to_vec());
    library.load_bytes(dejavu::sans_condensed::regular().to_vec());
    library
}

/// The condensed fixture face joins the **same family** as the regular one:
/// DejaVu's name tables both declare "DejaVu Sans", so the family carries
/// several widths - exactly the face-selection scenario card 022 requires.
#[test]
fn a_condensed_face_is_listed_and_resolvable_within_its_family() {
    let library = condensed_and_regular();
    let family = library
        .families()
        .into_iter()
        .find(|f| f.name == "DejaVu Sans")
        .expect("both faces group under the one family");
    assert_eq!(family.faces.len(), 2, "regular + condensed faces listed");
    let widths: std::collections::BTreeSet<_> = family.faces.iter().map(|f| f.stretch).collect();
    assert!(
        widths.contains(&FontStretch::Normal) && widths.contains(&FontStretch::SemiCondensed),
        "the condensed face's width class reaches the picker"
    );
    let normal = library
        .resolve(
            "DejaVu Sans",
            FontWeight::NORMAL,
            FontStretch::Normal,
            FontSlant::Normal,
        )
        .expect("regular resolves");
    let condensed = library
        .resolve(
            "DejaVu Sans",
            FontWeight::NORMAL,
            FontStretch::SemiCondensed,
            FontSlant::Normal,
        )
        .expect("condensed resolves");
    assert_ne!(
        normal.id, condensed.id,
        "the width request lands on the condensed face, not the regular one"
    );
    assert_eq!(
        library.face(condensed.id).unwrap().post_script_name,
        "DejaVuSansCondensed"
    );
    // An over-narrow request degrades to the family's closest face instead of
    // dropping text.
    let degraded = library.resolve(
        "DejaVu Sans",
        FontWeight::NORMAL,
        FontStretch::UltraCondensed,
        FontSlant::Normal,
    );
    assert!(
        degraded.is_some(),
        "the family still answers a width request"
    );
}

#[test]
fn a_missing_family_gets_the_documented_substitute() {
    let library = condensed_and_regular();
    assert_eq!(
        library.substitute_for("DejaVu Sans"),
        None,
        "an installed family needs no substitution"
    );
    assert_eq!(
        library.substitute_for(""),
        None,
        "the empty family is the generic sans, not a missing font"
    );
    let substitute = library
        .substitute_for("No Such Family")
        .expect("a missing family is reported");
    assert_eq!(
        substitute, "DejaVu Sans",
        "the substitute is the library's pinned default sans (SANS_PREFERENCES first)"
    );
    assert_eq!(
        FontLibrary::empty().substitute_for("No Such Family"),
        None,
        "a fontless library cannot name a substitute"
    );
}

#[test]
fn shaping_uses_exactly_the_reported_substitute() {
    let mut library = condensed_and_regular();
    let substitute = library.substitute_for("No Such Family").expect("reported");
    let shaped = shape(
        &mut library,
        &TextRun::point("Headline", "No Such Family", 20.0),
    );
    assert!(!shaped.glyphs.is_empty(), "the text still shapes");
    let families: std::collections::BTreeSet<String> = shaped
        .glyphs
        .iter()
        .filter_map(|g| library.face(g.font).map(|f| f.family))
        .collect();
    assert_eq!(
        families,
        std::collections::BTreeSet::from([substitute]),
        "every glyph comes from the family the substitution report named"
    );
}

#[test]
fn family_lookup_matches_the_shaper_exactly_case_sensitively() {
    // The substitution report and the shaper must agree on what "installed"
    // means, so `has_family` pins fontdb's own matching contract for
    // `Family::Name`: exact string equality (fontdb 0.23 `query` filters
    // `face.families.iter().any(|family| family.0 == name)` — no case
    // folding). If a fontdb upgrade ever folds case, this test fails and
    // `has_family`/`substitute_for` must fold with it.
    let library = condensed_and_regular();
    assert!(library.has_family("DejaVu Sans"));
    assert!(
        !library.has_family("dejavu sans"),
        "a case variant is not an installed family under fontdb's exact match"
    );
}
