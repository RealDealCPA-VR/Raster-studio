//! End-to-end tests for the package format.
//!
//! Every test here is anchored to a specific defect: a hostile package that
//! used to be read, a save that used to lose the pixels, a crash that used to
//! lose the project. Where the fix is a refusal, the test also builds the
//! artefact that would have been accepted.

use std::path::{Path, PathBuf};

use editor_core::{Document, PixelKey, TileDelta, TileEdit};
use layer_model::{Layer, LayerMask, MaskId};
use raster::{TileCoord, TileHash};

use crate::atomic;
use crate::manifest::{FileDigest, Manifest};
use crate::package::{
    load_project, open_project, save_project, save_project_with, SaveOptions, DOCUMENT_FILE,
    JOURNAL_FILE, MANIFEST_FILE,
};
use crate::preview::PREVIEW_FILE;
use crate::tiles::{solid_tile, NoTiles};
use crate::{AssetInput, CommandJournal, ProjectError};

const APP: &str = "Raster Studio 3.1.4";

fn opts() -> SaveOptions {
    SaveOptions::new(APP)
}

fn read_manifest(pkg: &Path) -> Manifest {
    serde_json::from_slice(&std::fs::read(pkg.join(MANIFEST_FILE)).unwrap()).unwrap()
}

fn write_manifest(pkg: &Path, m: &Manifest) {
    std::fs::write(
        pkg.join(MANIFEST_FILE),
        serde_json::to_vec_pretty(m).unwrap(),
    )
    .unwrap();
}

/// A document with real pixels: two painted layer tiles and a mask tile.
fn painted() -> (
    Document,
    compositor::MemoryTileSource,
    Vec<(TileHash, Vec<u8>)>,
) {
    let mut doc = Document::new(512, 512, "Painted");
    let mut layer = Layer::raster("Paint");
    let mask_id = MaskId::new();
    layer.mask = Some(LayerMask::new(mask_id));
    let layer_id = layer.id;
    doc.layers.push_root(layer).unwrap();

    let red = solid_tile([220, 30, 40, 255]);
    let blue = solid_tile([10, 60, 200, 255]);
    let coverage = vec![137u8; editor_core::MASK_TILE_BYTES];
    let (rh, bh, ch) = (
        TileHash::of(&red),
        TileHash::of(&blue),
        TileHash::of(&coverage),
    );

    doc.pixels.apply(
        PixelKey::Layer(layer_id),
        &TileDelta::new([
            TileEdit::set(TileCoord::new(0, 0, 0), rh),
            TileEdit::set(TileCoord::new(1, 1, 0), bh),
        ])
        .unwrap(),
    );
    doc.pixels.apply(
        PixelKey::Mask(mask_id),
        &TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), ch)),
    );

    let mut source = compositor::MemoryTileSource::new();
    for bytes in [&red, &blue, &coverage] {
        source.insert_bytes(bytes.clone());
    }
    (doc, source, vec![(rh, red), (bh, blue), (ch, coverage)])
}

// ---------------------------------------------------------------- round trip

#[test]
fn save_then_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("MyProject.rstudio");

    let mut doc = Document::new(320, 240, "MyProject");
    doc.layers.push_root(Layer::raster("Background")).unwrap();
    doc.layers.push_root(Layer::group("Group")).unwrap();

    save_project(&pkg, &doc).unwrap();
    assert!(pkg.join(MANIFEST_FILE).exists());
    assert!(pkg.join(DOCUMENT_FILE).exists());

    let loaded = load_project(&pkg).unwrap();
    assert_eq!(loaded.meta.size, doc.meta.size);
    assert_eq!(loaded.layers.len(), 2);
    assert_eq!(loaded, doc);
    assert_eq!(loaded.path(), Some(pkg.as_path()));
}

#[test]
fn atomic_overwrite_preserves_on_second_save() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    let doc = Document::new(100, 100, "P");
    save_project(&pkg, &doc).unwrap();
    save_project(&pkg, &doc).unwrap();
    assert!(load_project(&pkg).is_ok());
}

#[test]
fn rejects_non_package() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_project(dir.path());
    assert!(matches!(err, Err(ProjectError::NotAPackage(_))));
}

// -------------------------------------------------------------------- pixels

#[test]
fn a_painted_document_round_trips_its_pixels_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("Painted.rstudio");
    let (doc, source, blobs) = painted();

    let report = save_project_with(&pkg, &doc, &source, &opts()).unwrap();
    assert_eq!(report.tiles.blobs_written, 3, "two layer tiles and a mask");

    let loaded = open_project(&pkg).unwrap();
    assert_eq!(loaded.document, doc, "the document itself must survive");

    // ...and so must every byte behind every hash it names.
    for (hash, bytes) in &blobs {
        let stored = loaded
            .tiles
            .get(asset_store::BlobHash(hash.0))
            .unwrap_or_else(|e| panic!("tile {} came back missing: {e}", hash.to_hex()));
        assert_eq!(&*stored, bytes.as_slice(), "tile {} changed", hash.to_hex());
    }
    assert_eq!(loaded.tiles.len(), 3);

    // ...and the compositor can be handed them straight away.
    let source = loaded.tile_source().unwrap();
    for (hash, bytes) in &blobs {
        assert_eq!(
            compositor::TileSource::tile(&source, *hash),
            Some(bytes.as_slice())
        );
    }
}

#[test]
fn the_empty_tiles_directory_bug_is_a_refusal_now() {
    // The original save created `tiles/` and left it empty: paint, save,
    // reopen, blank canvas. Saving pixels through a source that does not hold
    // them is now an error rather than a silently blank package.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    let (doc, _, _) = painted();
    let err = save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap_err();
    assert!(matches!(err, ProjectError::MissingTile { .. }), "{err}");
    assert!(!pkg.exists(), "a failed save leaves nothing behind");
}

#[test]
fn identical_tiles_across_layers_are_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    let bytes = solid_tile([1, 2, 3, 255]);
    let hash = TileHash::of(&bytes);
    let mut doc = Document::new(256, 256, "Dedup");
    for name in ["A", "B", "C"] {
        let l = Layer::raster(name);
        let id = l.id;
        doc.layers.push_root(l).unwrap();
        doc.pixels.apply(
            PixelKey::Layer(id),
            &TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash)),
        );
    }
    let mut source = compositor::MemoryTileSource::new();
    source.insert_bytes(bytes.clone());

    let report = save_project_with(&pkg, &doc, &source, &opts()).unwrap();
    assert_eq!(report.tiles.blobs_written, 1);
    assert_eq!(report.tiles.references_deduplicated, 2);
    assert_eq!(report.tiles.bytes_written, bytes.len() as u64);
}

// ------------------------------------------------------------ path traversal

/// Rewrite a good package's manifest to point the document somewhere else, and
/// re-seal it so the integrity check would pass. This is the hostile artefact:
/// a package that is well-formed in every way except that it names a file
/// outside itself.
fn repoint_document(pkg: &Path, to: &str) {
    let mut m = read_manifest(pkg);
    m.document_path = to.to_string();
    m.seal();
    write_manifest(pkg, &m);
}

#[test]
fn an_absolute_document_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("Hostile.rstudio");
    save_project(&pkg, &Document::new(8, 8, "H")).unwrap();

    // Plant a file at the target so a successful traversal would be visible
    // rather than merely erroring for some other reason.
    let secret = dir.path().join("secret");
    std::fs::write(&secret, b"not the document").unwrap();

    for target in [
        secret.display().to_string(),
        "/etc/shadow".to_string(),
        "/etc/passwd".to_string(),
        "C:/Windows/win.ini".to_string(),
    ] {
        repoint_document(&pkg, &target);
        let err = open_project(&pkg).unwrap_err();
        assert!(
            matches!(
                err,
                ProjectError::UnsafePath {
                    field: "document_path",
                    ..
                }
            ),
            "accepted {target:?}: {err:?}"
        );
    }
}

#[test]
fn a_parent_traversal_document_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("Hostile.rstudio");
    save_project(&pkg, &Document::new(8, 8, "H")).unwrap();
    std::fs::write(dir.path().join("outside.msgpack"), b"anything").unwrap();

    for target in [
        "../outside.msgpack",
        "../../outside.msgpack",
        "sub/../../outside.msgpack",
        "..",
    ] {
        repoint_document(&pkg, target);
        let err = open_project(&pkg).unwrap_err();
        assert!(
            matches!(
                err,
                ProjectError::UnsafePath {
                    field: "document_path",
                    ..
                }
            ),
            "accepted {target:?}: {err:?}"
        );
    }
}

#[test]
fn a_document_path_naming_another_file_inside_the_package_is_still_refused() {
    // Safe as a path, but the loader does not take instructions about where the
    // document lives at all.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project(&pkg, &Document::new(8, 8, "P")).unwrap();
    repoint_document(&pkg, "previews/preview.png");
    let err = open_project(&pkg).unwrap_err();
    assert!(
        matches!(
            err,
            ProjectError::UnexpectedPath {
                field: "document_path",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn the_path_check_runs_before_the_integrity_check() {
    // Order matters: integrity detects damage, not malice. A hostile package
    // can seal itself perfectly, so the path refusal cannot be allowed to
    // depend on the seal failing.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project(&pkg, &Document::new(8, 8, "P")).unwrap();

    let mut m = read_manifest(&pkg);
    m.document_path = "/etc/passwd".into();
    m.seal();
    assert!(
        m.verify_seal(),
        "the hostile manifest verifies, as expected"
    );
    write_manifest(&pkg, &m);

    assert!(matches!(
        open_project(&pkg).unwrap_err(),
        ProjectError::UnsafePath { .. }
    ));
}

#[test]
fn a_contents_key_that_escapes_the_package_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project(&pkg, &Document::new(8, 8, "P")).unwrap();
    let mut m = read_manifest(&pkg);
    m.contents
        .insert("../../etc/passwd".into(), FileDigest::of(b""));
    m.seal();
    write_manifest(&pkg, &m);
    let err = open_project(&pkg).unwrap_err();
    assert!(
        matches!(
            err,
            ProjectError::UnsafePath {
                field: "contents",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn a_symlinked_tile_shard_is_refused_by_the_loader() {
    // Every name in this package is plain, the manifest seals, and each blob
    // hashes to the name it is filed under — and the loader is still reading
    // files that are not in the package, because a *directory* on the way down
    // is a link. Only a per-component check catches it.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    let (doc, source, blobs) = painted();
    save_project_with(&pkg, &doc, &source, &opts()).unwrap();
    assert!(open_project(&pkg).is_ok(), "the honest package loads");

    let shard = blobs[0].0.to_hex()[..2].to_string();
    let inside = pkg.join("tiles").join(&shard);
    let outside = dir.path().join("elsewhere");
    std::fs::rename(&inside, &outside).unwrap();
    if let Err(e) = crate::safepath::try_symlink_dir(&outside, &inside) {
        if cfg!(unix) {
            panic!("could not stage the symlink: {e}");
        }
        eprintln!("skipped: this machine cannot create a directory symlink ({e})");
        return;
    }

    // The blobs are still readable through the link...
    assert!(inside
        .join(format!("{}.tile", blobs[0].0.to_hex()))
        .is_file());
    // ...and the package is refused anyway.
    let err = open_project(&pkg).unwrap_err();
    assert!(
        matches!(err, ProjectError::Symlink { ref path } if path == &format!("tiles/{shard}")),
        "{err}"
    );
}

// ---------------------------------------------------------------- integrity

#[test]
fn a_tampered_document_fails_the_integrity_check() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    let mut doc = Document::new(64, 64, "P");
    doc.layers.push_root(Layer::raster("L")).unwrap();
    save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();
    assert!(open_project(&pkg).is_ok());

    // Flip one byte, keeping the length identical so it is the digest and not
    // the size that catches it.
    let path = pkg.join(DOCUMENT_FILE);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    let err = open_project(&pkg).unwrap_err();
    assert!(
        matches!(err, ProjectError::IntegrityMismatch { ref path } if path == DOCUMENT_FILE),
        "{err}"
    );
}

#[test]
fn a_tampered_preview_fails_the_integrity_check() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project_with(&pkg, &Document::new(64, 64, "P"), &NoTiles, &opts()).unwrap();
    std::fs::write(pkg.join(PREVIEW_FILE), b"not a png").unwrap();
    let err = open_project(&pkg).unwrap_err();
    assert!(
        matches!(err, ProjectError::IntegrityMismatch { ref path } if path == PREVIEW_FILE),
        "{err}"
    );
}

#[test]
fn a_tampered_manifest_fails_its_own_seal() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project_with(&pkg, &Document::new(64, 64, "P"), &NoTiles, &opts()).unwrap();
    let mut m = read_manifest(&pkg);
    m.app_version = "1.0.0-evil".into(); // no re-seal
    write_manifest(&pkg, &m);
    assert!(matches!(
        open_project(&pkg).unwrap_err(),
        ProjectError::ManifestIntegrityMismatch
    ));
}

#[test]
fn a_document_that_is_present_but_unlisted_is_refused() {
    // The manifest is the inventory. A payload nothing vouches for is what a
    // substitution looks like.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project_with(&pkg, &Document::new(64, 64, "P"), &NoTiles, &opts()).unwrap();
    let mut m = read_manifest(&pkg);
    m.contents.remove(DOCUMENT_FILE);
    m.seal();
    write_manifest(&pkg, &m);
    assert!(matches!(
        open_project(&pkg).unwrap_err(),
        ProjectError::IntegrityMismatch { .. }
    ));
}

#[test]
fn a_contents_entry_naming_anything_else_is_sealed_but_never_checked() {
    // The scope of the digest check, pinned so the crate docs stay true. Only
    // `document.msgpack`, `assets/index.json` and `previews/preview.png` are
    // verified against `contents`; the loader opens a fixed set of files and
    // takes no instruction from the inventory about what else to look at, so an
    // entry naming some other (path-safe) file is carried through the seal and
    // then ignored. If a later change starts verifying every entry, this test
    // fails and the "Untrusted input" section of the crate docs is what needs
    // rewriting with it.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project_with(&pkg, &Document::new(64, 64, "P"), &NoTiles, &opts()).unwrap();

    std::fs::write(pkg.join("ai").join("notes.json"), b"actual bytes").unwrap();
    let mut m = read_manifest(&pkg);
    m.contents.insert(
        "ai/notes.json".into(),
        FileDigest::of(b"quite different bytes"),
    );
    m.seal();
    write_manifest(&pkg, &m);

    let loaded = open_project(&pkg).expect("an unverified entry is not a refusal today");
    assert!(loaded.manifest.contents.contains_key("ai/notes.json"));
}

// ------------------------------------------------------------------ versions

#[test]
fn a_version_999_document_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("Future.rstudio");
    save_project_with(&pkg, &Document::new(64, 64, "F"), &NoTiles, &opts()).unwrap();

    let mut doc = Document::new(64, 64, "F");
    doc.meta.format_version = 999;
    let bytes = rmp_serde::to_vec_named(&doc).unwrap();
    std::fs::write(pkg.join(DOCUMENT_FILE), &bytes).unwrap();

    // Re-seal so the version gate is what refuses it, not the digest.
    let mut m = read_manifest(&pkg);
    m.contents
        .insert(DOCUMENT_FILE.into(), FileDigest::of(&bytes));
    m.seal();
    write_manifest(&pkg, &m);

    let err = open_project(&pkg).unwrap_err();
    assert!(
        matches!(
            err,
            ProjectError::UnsupportedDocumentVersion { found: 999, .. }
        ),
        "{err}"
    );
    assert!(
        err.to_string().contains("newer Raster Studio"),
        "the message has to be readable by the person holding the file: {err}"
    );
}

#[test]
fn an_unsupported_package_layout_version_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project_with(&pkg, &Document::new(8, 8, "P"), &NoTiles, &opts()).unwrap();

    for v in [1u32, crate::MANIFEST_VERSION + 1, 999] {
        let mut m = read_manifest(&pkg);
        m.manifest_version = v;
        m.seal();
        write_manifest(&pkg, &m);
        let err = open_project(&pkg).unwrap_err();
        assert!(
            matches!(err, ProjectError::UnsupportedVersion { found, .. } if found == v),
            "layout version {v} was accepted: {err}"
        );
    }
}

#[test]
fn an_older_document_loads_and_comes_back_stamped_current() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("Old.rstudio");
    save_project_with(&pkg, &Document::new(8, 8, "Old"), &NoTiles, &opts()).unwrap();

    let mut doc = Document::new(8, 8, "Old");
    doc.layers.push_root(Layer::raster("L")).unwrap();
    doc.meta.format_version = crate::MIN_DOCUMENT_VERSION;
    let bytes = rmp_serde::to_vec_named(&doc).unwrap();
    std::fs::write(pkg.join(DOCUMENT_FILE), &bytes).unwrap();
    let mut m = read_manifest(&pkg);
    m.contents
        .insert(DOCUMENT_FILE.into(), FileDigest::of(&bytes));
    m.seal();
    write_manifest(&pkg, &m);

    let loaded = open_project(&pkg).unwrap();
    assert_eq!(loaded.migrated_from, Some(crate::MIN_DOCUMENT_VERSION));
    assert_eq!(
        loaded.document.meta.format_version,
        crate::MAX_DOCUMENT_VERSION,
        "a migrated document must report the format it is now in"
    );
    assert_eq!(loaded.document.layers.len(), 1);
}

// ------------------------------------------------------------- crash safety

#[test]
fn a_crash_between_the_renames_still_leaves_a_loadable_project() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("Work.rstudio");

    let mut first = Document::new(64, 64, "Work");
    first.layers.push_root(Layer::raster("First")).unwrap();
    save_project_with(&pkg, &first, &NoTiles, &opts()).unwrap();

    let mut second = first.clone();
    second.layers.push_root(Layer::raster("Second")).unwrap();

    atomic::CRASH_BETWEEN_RENAMES.with(|c| c.set(true));
    let err = save_project_with(&pkg, &second, &NoTiles, &opts()).unwrap_err();
    assert!(err.to_string().contains("simulated crash"), "{err}");

    // This is the state the old code left behind for good: nothing at the path
    // the user saved to, and nothing that looked for the sibling.
    assert!(!pkg.exists(), "the crash window is real");

    let loaded = open_project(&pkg).unwrap();
    assert!(loaded.recovered_from_interrupted_save);
    assert_eq!(loaded.document, first, "the previous save is intact");
    assert!(pkg.exists(), "the package is back where the user left it");

    // And the recovery is durable: reopening finds a normal package.
    let again = open_project(&pkg).unwrap();
    assert!(!again.recovered_from_interrupted_save);

    // What is left over, named rather than accidental. A crash returns nothing
    // and so cleans nothing up: the interrupted save's `.new-` directory is
    // still on disk, and `recover` will never adopt or delete it (it cannot
    // tell a dead save's temp from a live one's). That leak is documented under
    // "Known limits" in the crate docs; this pins it so it stays a decision.
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "Work.rstudio")
        .collect();
    assert_eq!(
        leftovers.len(),
        1,
        "expected exactly one stray: {leftovers:?}"
    );
    assert!(
        leftovers[0].starts_with("Work.rstudio.new-"),
        "the stray must be the interrupted save's temp, not a lost backup: {leftovers:?}"
    );
    assert!(
        !leftovers[0].contains(".bak-"),
        "a surviving backup would mean recovery did not run: {leftovers:?}"
    );
}

#[test]
fn a_failed_save_leaves_the_previous_package_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    let mut good = Document::new(64, 64, "P");
    good.layers.push_root(Layer::raster("Keep")).unwrap();
    save_project_with(&pkg, &good, &NoTiles, &opts()).unwrap();

    // A save that fails while building the package: it references a tile no
    // source holds.
    let (painted_doc, _, _) = painted();
    assert!(save_project_with(&pkg, &painted_doc, &NoTiles, &opts()).is_err());

    assert_eq!(open_project(&pkg).unwrap().document, good);
}

#[test]
fn a_save_never_deletes_another_saves_in_flight_temp() {
    // The old code used one fixed name, `<pkg>.tmp`, and began every save with
    // `remove_dir_all` on it — so two saves running at once destroyed each
    // other's work in progress.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");

    let foreign: Vec<PathBuf> = vec![
        dir.path().join("P.rstudio.tmp"),
        dir.path().join("P.rstudio.new-cafe-1-0"),
        dir.path().join("P.rstudio.old"),
    ];
    for f in &foreign {
        std::fs::create_dir(f).unwrap();
        std::fs::write(f.join("in-flight"), b"someone else's save").unwrap();
    }

    save_project_with(&pkg, &Document::new(8, 8, "P"), &NoTiles, &opts()).unwrap();
    save_project_with(&pkg, &Document::new(8, 8, "P"), &NoTiles, &opts()).unwrap();

    for f in &foreign {
        assert!(
            f.join("in-flight").is_file(),
            "{} was clobbered by an unrelated save",
            f.display()
        );
    }
}

// ------------------------------------------------------------------- journal

#[test]
fn the_package_journal_anchors_recovery_to_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("Session.rstudio");

    // The session as the application runs it: an initial save creates the
    // package (and with it the journal), then every accepted command is
    // journalled inside it.
    let mut doc = Document::new(64, 64, "Session");
    save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();
    let journal = pkg.join(JOURNAL_FILE);

    let a_cmd = editor_core::Command::create_layer(Layer::raster("A"));
    a_cmd.apply(&mut doc).unwrap();
    CommandJournal::append(&journal, &a_cmd).unwrap();
    let report = save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();

    // Unsaved work after the snapshot.
    let b = Layer::raster("B");
    let b_id = b.id;
    let b_cmd = editor_core::Command::create_layer(b);
    b_cmd.apply(&mut doc).unwrap();
    CommandJournal::append(&journal, &b_cmd).unwrap();

    // Crash. Reopen: snapshot has one layer, journal has two records.
    let loaded = open_project(&pkg).unwrap();
    assert_eq!(loaded.document.layers.len(), 1);
    assert_eq!(loaded.document_digest, report.document);

    let rec = CommandJournal::read(&journal).unwrap();
    assert_eq!(rec.commands().len(), 2, "A was journalled before the save");
    assert_eq!(rec.since_last_save().len(), 1, "only B is unsaved");

    let mut recovered = loaded.document;
    assert_eq!(
        rec.replay_onto(&mut recovered, loaded.document_digest)
            .unwrap(),
        1
    );
    assert_eq!(recovered.layers.len(), 2, "not three: A is not duplicated");
    assert!(recovered.layers.get(b_id).is_some());
}

#[test]
fn saving_again_carries_the_journal_forward_and_re_anchors_it() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("S.rstudio");
    let journal = pkg.join(JOURNAL_FILE);

    let mut doc = Document::new(64, 64, "S");
    save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();

    let cmd = editor_core::Command::create_layer(Layer::raster("A"));
    cmd.apply(&mut doc).unwrap();
    CommandJournal::append(&journal, &cmd).unwrap();

    let report = save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();
    let rec = CommandJournal::read(&journal).unwrap();
    assert_eq!(rec.commands().len(), 1, "the record survived the save");
    assert!(
        rec.since_last_save().is_empty(),
        "and is now inside the snapshot"
    );
    assert_eq!(rec.last_save().unwrap().document, report.document);

    let loaded = open_project(&pkg).unwrap();
    let mut d = loaded.document;
    assert_eq!(
        rec.replay_onto(&mut d, loaded.document_digest).unwrap(),
        0,
        "nothing left to replay"
    );
    assert_eq!(d.layers.len(), 1);
}

#[test]
fn a_torn_journal_in_the_package_does_not_cost_the_save_marker() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("S.rstudio");
    let journal = pkg.join(JOURNAL_FILE);

    let mut doc = Document::new(64, 64, "S");
    save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();
    let cmd = editor_core::Command::create_layer(Layer::raster("A"));
    cmd.apply(&mut doc).unwrap();
    CommandJournal::append(&journal, &cmd).unwrap();

    // Simulate a crash mid-append: a record with no terminator.
    let mut bytes = std::fs::read(&journal).unwrap();
    bytes.extend_from_slice(br#"{"Command":{"CreateLay"#);
    std::fs::write(&journal, &bytes).unwrap();
    assert!(CommandJournal::read(&journal).unwrap().truncated());

    // Saving copies the valid prefix only, so the marker appended after it is
    // reachable.
    let report = save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();
    let rec = CommandJournal::read(&journal).unwrap();
    assert!(!rec.truncated(), "the torn tail was dropped, not carried");
    assert_eq!(rec.last_save().unwrap().document, report.document);
    assert_eq!(rec.commands().len(), 1);
}

#[test]
fn a_symlinked_journal_is_refused_by_the_loader_and_by_every_writer() {
    // The write primitive this test exists to keep closed: `commands.journal`
    // is neither content-addressed nor digest-verified *and* it is the one file
    // the application writes back into a package it did not build. Point it at
    // a file outside the package and, before the fix, `open_project` succeeded,
    // `append` wrote attacker-chosen JSON onto the end of that file, and
    // `clear` truncated it to zero bytes.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("Hostile.rstudio");
    save_project_with(&pkg, &Document::new(8, 8, "H"), &NoTiles, &opts()).unwrap();
    assert!(open_project(&pkg).is_ok(), "the honest package loads");

    let victim = dir.path().join("victim.txt");
    let original = b"# the user's shell rc, or another project's document\n";
    std::fs::write(&victim, original).unwrap();

    let journal = pkg.join(JOURNAL_FILE);
    std::fs::remove_file(&journal).unwrap();
    if let Err(e) = crate::safepath::try_symlink_file(&victim, &journal) {
        if cfg!(unix) {
            panic!("could not stage the symlink: {e}");
        }
        eprintln!("skipped: this machine cannot create a file symlink ({e})");
        return;
    }
    // The link really does resolve to the victim, so this is the live primitive
    // and not merely a broken path that would error for some other reason.
    assert_eq!(std::fs::read(&journal).unwrap(), original);

    // 1. The package does not open at all.
    let err = open_project(&pkg).unwrap_err();
    assert!(
        matches!(err, ProjectError::Symlink { ref path } if path == JOURNAL_FILE),
        "{err}"
    );

    // 2. And the writers refuse independently, because the link can be planted
    //    after a successful open — the journal is written for the whole session.
    let cmd = editor_core::Command::create_layer(Layer::raster("payload"));
    let err = CommandJournal::append(&journal, &cmd).unwrap_err();
    assert!(matches!(err, ProjectError::Symlink { .. }), "{err}");
    let err = CommandJournal::mark_saved(&journal, crate::DocumentDigest::of(b"x")).unwrap_err();
    assert!(matches!(err, ProjectError::Symlink { .. }), "{err}");
    let err = CommandJournal::clear(&journal).unwrap_err();
    assert!(matches!(err, ProjectError::Symlink { .. }), "{err}");
    // Reading it is refused too, rather than reading a file outside the package.
    let err = CommandJournal::read(&journal).unwrap_err();
    assert!(matches!(err, ProjectError::Symlink { .. }), "{err}");

    // 3. Byte for byte, the file outside the package is untouched.
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        original,
        "the file outside the package was written to or truncated"
    );

    // 4. Saving over the hostile package refuses by name rather than opaquely.
    let err = save_project_with(&pkg, &Document::new(8, 8, "H"), &NoTiles, &opts()).unwrap_err();
    assert!(
        matches!(err, ProjectError::Symlink { ref path } if path == JOURNAL_FILE),
        "{err}"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), original);
}

#[test]
fn a_journal_that_is_a_directory_is_refused_rather_than_opened() {
    // No symlink privilege needed for this one, so it runs everywhere: the
    // writers must refuse any journal path that is not a regular file.
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join(JOURNAL_FILE);
    std::fs::create_dir(&journal).unwrap();

    let cmd = editor_core::Command::create_layer(Layer::raster("payload"));
    assert!(matches!(
        CommandJournal::append(&journal, &cmd).unwrap_err(),
        ProjectError::NotAFile { .. }
    ));
    assert!(matches!(
        CommandJournal::clear(&journal).unwrap_err(),
        ProjectError::NotAFile { .. }
    ));
    assert!(journal.is_dir(), "and it is still a directory");
}

#[test]
fn the_journal_prefix_a_save_copies_comes_from_the_bytes_it_read() {
    // `carry_journal_forward` used to read the source journal twice: once for
    // the bytes it copies and once, separately, for the prefix length. The
    // application appends to an open package's journal, so the second read can
    // see a longer file — and a length measured over the longer buffer cuts the
    // shorter copy mid-record. That torn record then sits in front of the save
    // marker appended next, the reader stops at it, `last_save()` is `None`,
    // and recovery replays the whole journal onto a snapshot that already
    // contains it: the duplicate-every-command bug the marker exists to stop.
    //
    // The invariant that makes the two reads impossible to disagree about:
    // for *any* buffer, the prefix `parse` reports is inside that buffer and
    // parses cleanly with nothing torn.
    let cmd = editor_core::Command::create_layer(Layer::raster("A"));
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("j");
    CommandJournal::append(&staging, &cmd).unwrap();
    CommandJournal::append(&staging, &cmd).unwrap();
    let whole = std::fs::read(&staging).unwrap();

    // The disagreement itself, made concrete. `short` is the buffer a save has
    // in hand — it ends mid-record because an append was in flight when it read
    // — and `whole` is what a second, independent read a moment later returns.
    // The old code measured `whole` and sliced `short`, and the `.min()` clamp
    // that made that not panic is what silently copied the torn record.
    let mid = whole.iter().position(|b| *b == b'\n').unwrap() + 1 + 6;
    let short = &whole[..mid];
    let stale_len = CommandJournal::parse(&whole).valid_bytes() as usize;
    assert!(
        stale_len > short.len(),
        "the two reads must disagree or this test proves nothing"
    );
    assert!(
        CommandJournal::parse(&short[..stale_len.min(short.len())]).truncated(),
        "the old two-read clamp copies a torn record"
    );
    let honest = CommandJournal::parse(short).valid_bytes() as usize;
    assert!(
        !CommandJournal::parse(&short[..honest]).truncated(),
        "measuring the buffer in hand does not"
    );

    for cut in 0..=whole.len() {
        let torn = &whole[..cut];
        let rec = CommandJournal::parse(torn);
        let valid = rec.valid_bytes() as usize;
        assert!(
            valid <= torn.len(),
            "the prefix escaped the buffer it was measured on ({valid} > {})",
            torn.len()
        );
        let copied = CommandJournal::parse(&torn[..valid]);
        assert!(
            !copied.truncated(),
            "the copy ends inside a record at cut {cut}"
        );
        assert_eq!(
            copied.records_read(),
            rec.records_read(),
            "the copy lost an intact record at cut {cut}"
        );
        assert!(
            valid == 0 || torn[valid - 1] == b'\n',
            "a prefix must end on a record terminator (cut {cut})"
        );
    }

    // And end to end: a save over a journal whose tail is torn leaves a package
    // whose journal ends on a record boundary, so the marker written after it
    // is reachable.
    let pkg = dir.path().join("S.rstudio");
    let mut doc = Document::new(64, 64, "S");
    save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();
    let journal = pkg.join(JOURNAL_FILE);
    cmd.apply(&mut doc).unwrap();
    CommandJournal::append(&journal, &cmd).unwrap();
    let mut bytes = std::fs::read(&journal).unwrap();
    bytes.extend_from_slice(br#"{"Command":{"CreateLay"#);
    std::fs::write(&journal, &bytes).unwrap();

    let report = save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();
    let carried = std::fs::read(&journal).unwrap();
    assert_eq!(*carried.last().unwrap(), b'\n');
    let rec = CommandJournal::read(&journal).unwrap();
    assert!(!rec.truncated());
    assert_eq!(
        rec.last_save().map(|m| m.document),
        Some(report.document),
        "the marker has to be reachable, or recovery replays everything"
    );
}

#[test]
fn a_journal_that_grows_while_the_save_reads_it_still_carries_a_whole_prefix() {
    // The two-read bug, driven end to end. `carry_journal_forward` used to read
    // the source journal twice — once for the bytes it copies, once for the
    // prefix length — and the application appends to an open package's journal
    // for the whole session, so the second read can see a longer file. The
    // length then belongs to a buffer that is not the one being sliced, the
    // `.min()` clamp turns that into a copy that ends mid-record, and the save
    // marker appended immediately afterwards becomes unreachable: `last_save()`
    // is `None`, `since_last_save()` is the entire journal, and recovery
    // reapplies every command the snapshot already contains.
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("S.rstudio");
    let mut doc = Document::new(64, 64, "S");
    save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();
    let journal = pkg.join(JOURNAL_FILE);

    // The journal now holds the first save's marker; add one command after it.
    let cmd = editor_core::Command::create_layer(Layer::raster("A"));
    cmd.apply(&mut doc).unwrap();
    CommandJournal::append(&journal, &cmd).unwrap();
    let whole = std::fs::read(&journal).unwrap();

    // Rewind the file so it ends halfway through that last record — what a
    // reader arriving mid-append sees — and hand the rest to the seam, which
    // completes it *after* the save has read the file. That is the exact state
    // in which the two reads disagree.
    let last_nl = whole[..whole.len() - 1]
        .iter()
        .rposition(|b| *b == b'\n')
        .unwrap();
    let cut = last_nl + 1 + (whole.len() - last_nl - 1) / 2;
    std::fs::write(&journal, &whole[..cut]).unwrap();
    crate::package::GROW_JOURNAL_AFTER_READ.with(|c| {
        *c.borrow_mut() = Some(whole[cut..].to_vec());
    });

    let report = save_project_with(&pkg, &doc, &NoTiles, &opts()).unwrap();

    let rec = CommandJournal::read(&journal).unwrap();
    assert!(
        !rec.truncated(),
        "a torn record was carried into the new package, in front of the marker"
    );
    assert_eq!(
        rec.last_save().map(|m| m.document),
        Some(report.document),
        "the save marker is unreachable, so recovery would replay everything"
    );
    assert!(
        rec.since_last_save().is_empty(),
        "nothing is unsaved: the snapshot was just written"
    );
}

// ------------------------------------------------------------------ manifest

#[test]
fn the_manifest_records_the_application_version_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project_with(&pkg, &Document::new(8, 8, "P"), &NoTiles, &opts()).unwrap();
    let m = open_project(&pkg).unwrap().manifest;
    assert_eq!(m.app_version, APP);
    assert_ne!(
        m.app_version,
        env!("CARGO_PKG_VERSION"),
        "this used to record project-format's own version for every build"
    );
}

// -------------------------------------------------------------------- assets

#[test]
fn collecting_assets_makes_a_portable_package() {
    let dir = tempfile::tempdir().unwrap();
    let external = dir.path().join("texture.bin");
    std::fs::write(&external, b"texture bytes").unwrap();
    let doc = Document::new(8, 8, "P");

    // Linked, not collected.
    let linked = dir.path().join("Linked.rstudio");
    let o = opts().with_assets(vec![AssetInput::linked("image/png", &external)]);
    save_project_with(&linked, &doc, &NoTiles, &o).unwrap();
    let loaded = open_project(&linked).unwrap();
    assert!(!loaded.manifest.assets_collected);
    assert!(matches!(
        loaded.assets[0].source,
        asset_store::AssetSource::Linked { .. }
    ));

    // Collected.
    let portable = dir.path().join("Portable.rstudio");
    let o = opts().collecting_assets(vec![AssetInput::linked("image/png", &external)]);
    let report = save_project_with(&portable, &doc, &NoTiles, &o).unwrap();
    assert!(report.assets.collected);
    std::fs::remove_file(&external).unwrap();

    let loaded = open_project(&portable).unwrap();
    assert!(loaded.manifest.assets_collected);
    assert!(matches!(
        loaded.assets[0].source,
        asset_store::AssetSource::Embedded
    ));
    assert_eq!(
        &*loaded.tiles.get(loaded.assets[0].hash).unwrap(),
        &b"texture bytes"[..],
        "the bytes are in the package, not on the machine that saved it"
    );
}

#[test]
fn a_package_with_no_assets_is_portable() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    save_project_with(&pkg, &Document::new(8, 8, "P"), &NoTiles, &opts()).unwrap();
    assert!(open_project(&pkg).unwrap().manifest.assets_collected);
}

// ------------------------------------------------------------------- preview

#[test]
fn the_package_carries_a_composite_preview_of_the_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    let (doc, source, _) = painted();

    let report = save_project_with(&pkg, &doc, &source, &opts()).unwrap();
    assert_eq!(report.preview, Some((512, 512)), "512 is the long edge cap");

    let loaded = open_project(&pkg).unwrap();
    let png = loaded.preview.expect("previews/ used to be created empty");
    let decoded = raster::codec::decode_bytes(&png).unwrap();
    assert_eq!((decoded.width, decoded.height), (512, 512));
    // The mask is 137/255 coverage over an opaque tile, so the top-left corner
    // is partly transparent red rather than nothing at all.
    let top_left: [u8; 4] = decoded.rgba8[..4].try_into().unwrap();
    assert!(top_left[3] > 0 && top_left[3] < 255, "got {top_left:?}");
    assert!(
        top_left[0] > top_left[2],
        "should read as red: {top_left:?}"
    );
}

#[test]
fn a_preview_can_be_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("P.rstudio");
    let mut o = opts();
    o.write_preview = false;
    let report = save_project_with(&pkg, &Document::new(64, 64, "P"), &NoTiles, &o).unwrap();
    assert_eq!(report.preview, None);
    assert!(!pkg.join(PREVIEW_FILE).exists());
    assert!(open_project(&pkg).unwrap().preview.is_none());
}
