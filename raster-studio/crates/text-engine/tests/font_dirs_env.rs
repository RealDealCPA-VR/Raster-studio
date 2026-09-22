//! The `RASTER_STUDIO_FONT_DIRS` seam in [`FontLibrary::with_system_fonts`].
//!
//! `with_system_fonts` is the one constructor the application and the
//! integration tests use, and the environment variable is how a test process
//! stands in for a machine with a different — or no — font installation (the
//! bare `ubuntu-latest` runner that kept `font_selection_reports_substitution_
//! and_keeps_the_requested_family` red). If the routing from the variable to
//! [`FontLibrary::from_font_dirs`] were lost, every reproduction command that
//! sets the variable would silently degrade into an ordinary system-font scan
//! and report green; this binary pins the routing itself, not the helper.
//!
//! It is one test in its own binary on purpose: the variable is process-global,
//! and `tests/fonts.rs::the_system_library_is_usable_when_the_machine_has_fonts`
//! must keep seeing the real installation.

use text_engine::{FontLibrary, FONT_DIRS_ENV};

/// A fresh directory under the OS temp folder, unique to this call.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "raster-studio-font-dirs-env-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn the_env_var_replaces_the_system_scan_in_with_system_fonts() {
    // Set but empty: the bare-runner shape. No system fonts, so nothing is
    // present and there is nothing to substitute with.
    std::env::set_var(FONT_DIRS_ENV, "");
    let library = FontLibrary::with_system_fonts();
    assert!(
        library.is_empty(),
        "an empty {FONT_DIRS_ENV} means no system fonts at all, got {:?}",
        library.family_names()
    );
    assert_eq!(
        library.substitute_for("x"),
        None,
        "a fontless library names no substitute"
    );

    // A directory holding one file: that file is the whole library, whatever
    // the machine has installed.
    let dir = scratch_dir("dejavu");
    std::fs::write(dir.join("DejaVuSans.ttf"), dejavu::sans::regular()).unwrap();
    std::env::set_var(FONT_DIRS_ENV, &dir);
    let library = FontLibrary::with_system_fonts();
    assert_eq!(
        library.family_names(),
        vec!["DejaVu Sans".to_string()],
        "only the family in the listed directory is present"
    );
    assert_eq!(
        library
            .substitute_for("Raster Test Missing Family")
            .as_deref(),
        Some("DejaVu Sans"),
        "the generic sans is pinned to what the directory holds"
    );

    std::env::remove_var(FONT_DIRS_ENV);
    let _ = std::fs::remove_dir_all(&dir);
}
