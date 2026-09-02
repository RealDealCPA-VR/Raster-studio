//! Getting work *out* of the editor, and getting it back after a crash.
//!
//! Export and `.psd` are how a document reaches another program; the command
//! journal is how it survives a process that never reached its exit path. Both
//! are driven through the application's own calls —
//! [`app_shell::doc::OpenDocument::export_to`] and
//! [`app_shell::session::recoverable`]/[`app_shell::session::replay`] — with
//! one stated exception, the PSD test, which is labelled where it sits.

use app_shell::doc::OpenDocument;
use app_shell::session;
use editor_core::{Command, History, LayerPatch};
use integration_tests::app::{self, DocExt, APP_VERSION};
use integration_tests::fixture::{
    max_channel_diff, mean_channel_diff, photo_rgba8, photo_rgba8_channels_cycled,
    photo_rgba8_with_alpha,
};
use layer_model::{BlendMode, Layer};
use project_format::{CommandJournal, JOURNAL_FILE};
use psd::{MergedImage, PsdFile, PsdHeader, PsdLayer, PsdMask, Rect};
use raster::{TileCoord, TILE_SIZE};

// ---------------------------------------------------------------------------
// 7. Export
// ---------------------------------------------------------------------------

/// A document whose composite is a real picture and is fully opaque, so a
/// container with no alpha channel is comparable without a flattening step
/// changing the answer.
fn photo_document(width: u32, height: u32) -> OpenDocument {
    let mut doc = app::blank(width, height, "Export");
    let layer = doc
        .document
        .active_layer()
        .expect("File ▸ New makes a layer");
    let source = photo_rgba8(width, height);
    doc.paint_canvas(layer, &move |x, y| {
        let i = (y as usize * width as usize + x as usize) * 4;
        [source[i], source[i + 1], source[i + 2], source[i + 3]]
    });
    doc
}

#[test]
fn a_png_export_decodes_back_to_the_composite_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("flat.png");
    let (w, h) = (300u32, 200u32);
    let mut doc = photo_document(w, h);
    let composite = doc.composite_all();

    // What File ▸ Export runs: the extension picks the format, the canvas is
    // composited, the file is written.
    doc.export_to(&out).unwrap();

    let decoded = raster::decode_path(&out).unwrap();
    assert_eq!((decoded.width, decoded.height), (w, h));
    assert_eq!(
        decoded.rgba8, composite,
        "PNG is lossless: the file must decode to the composite, exactly"
    );
}

#[test]
fn a_jpeg_export_decodes_back_to_the_composite_within_the_formats_tolerance() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("flat.jpg");
    let (w, h) = (256u32, 192u32);
    let mut doc = photo_document(w, h);
    let composite = doc.composite_all();

    doc.export_to(&out).unwrap();

    let decoded = raster::decode_path(&out).unwrap();
    assert_eq!((decoded.width, decoded.height), (w, h));
    assert!(
        decoded.rgba8.iter().skip(3).step_by(4).all(|a| *a == 255),
        "JPEG has no alpha channel; every pixel must come back opaque"
    );

    // Both sides of the comparison below are opaque: the decode because JPEG
    // carries no alpha (asserted above), the composite by construction. That is
    // the premise that lets a whole-pixel bound speak for the planes JPEG
    // actually stores — with the alpha term identically zero, the four-channel
    // mean is exactly three quarters of the colour-plane mean. Assert the
    // premise rather than trusting the fixture's doc comment for it.
    assert!(
        composite.iter().skip(3).step_by(4).all(|a| *a == 255),
        "the fixture's composite must be opaque, or `mean_channel_diff` below \
         would be measuring a channel this format does not carry"
    );

    // JPEG is lossy and chroma-subsampled, so the bound is on how far it may
    // stray, not on equality. The fixture is a hard case for it — a per-pixel
    // checkerboard on top of two ramps — which is why the ceiling sits well
    // above the mean.
    //
    // The two numbers are the measured error plus a small margin for encoder
    // version drift, not round numbers picked to be safe: this encoder produces
    // worst = 37 and mean = 1.149 today (equivalently, a colour-plane mean of
    // 1.532 — the same measurement scaled by 4/3, which is why there is no
    // third bound here). Slack is not free — every code between the measurement
    // and the bound is a regression the test would accept.
    let worst = max_channel_diff(&decoded.rgba8, &composite);
    let mean = mean_channel_diff(&decoded.rgba8, &composite);
    assert!(worst <= 45, "worst channel error was {worst}");
    assert!(mean <= 1.6, "mean channel error was {mean:.3}");

    // ...and it is genuinely the same picture, not a coincidence of tolerances.
    //
    // The control is the same generator with its colour planes cycled: same
    // size, same histogram, same full opacity, different picture. That last
    // part is the whole point — the composite here is opaque and JPEG forces
    // every decoded alpha to 255, so a control that differed only in *alpha*
    // would have bit-identical RGB and would clear the bound on the strength of
    // a channel this format does not even carry.
    let other = photo_rgba8_channels_cycled(w, h);
    let control = mean_channel_diff(&other, &composite);
    assert!(
        control > 1.6,
        "the tolerance is loose enough to accept a different picture \
         (control mean was {control:.3})"
    );
    // The control is the one place a colour-plane-only comparison still has to
    // be made by hand. The decoded file's opacity is asserted above; the
    // control's is not, so its whole-pixel mean could in principle be carried
    // by alpha alone — precisely the mistake this control replaced. Measure the
    // planes JPEG actually stores and require the control to clear the same
    // bound there. Today this reads 79.4, against 1.532 for the decoded file.
    let rgb_only = |a: &[u8], b: &[u8]| -> f64 {
        let (mut total, mut n) = (0u64, 0u64);
        for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            for c in 0..3 {
                total += u64::from(pa[c].abs_diff(pb[c]));
                n += 1;
            }
        }
        total as f64 / n as f64
    };
    let control_rgb = rgb_only(&other, &composite);
    assert!(
        control_rgb > 1.6,
        "the control differs from the composite only in alpha, which JPEG \
         discards — it is not a control at all (colour-plane mean was \
         {control_rgb:.3})"
    );
}

#[test]
fn an_export_to_a_format_the_product_cannot_write_is_refused_rather_than_guessed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut doc = photo_document(32, 32);
    let out = tmp.path().join("flat.exr");
    let err = doc.export_to(&out).unwrap_err();
    assert!(err.to_string().contains("exr"), "{err}");
    assert!(!out.exists(), "a refusal must not leave a file behind");
}

// ---------------------------------------------------------------------------
// 8. PSD interchange — at the file level only
// ---------------------------------------------------------------------------

/// A layered `.psd` written by this workspace is read back whole.
///
/// # What this does *not* cover, stated rather than implied
///
/// There is no bridge between an [`editor_core::Document`] and a
/// [`psd::PsdFile`] *reader* anywhere in the workspace. `app-shell` imports
/// images only through `raster::decode_path`, which has no PSD decoder behind
/// it, so the application cannot open a `.psd`. It *can* write one:
/// `OpenDocument::export_psd_to` lowers a document to a layered PSD through
/// `crate::import::psd_from_document`, and `export_to` routes `.psd`
/// destinations there rather than flattening — so "save as PSD" keeps its
/// layers. The refusal test above now pins a format the product genuinely
/// cannot write (`.exr`) instead of the one this wave added.
///
/// What it does prove is that the `psd` crate's writer and reader agree about
/// structure, blend modes, clipping, visibility, masks, per-layer pixels and
/// the merged composite, and that a read-then-write round trip is byte stable.
/// That is the half of the interchange that exists; the Document↔PsdFile
/// converter is the half that does not, and it belongs on the product backlog
/// rather than in a comment here pretending otherwise.
#[test]
fn a_layered_psd_file_round_trips_through_the_psd_crate_with_structure_and_pixels() {
    const W: u32 = 64;
    const H: u32 = 48;

    let bottom_pixels = photo_rgba8(W, H);
    let top_pixels = photo_rgba8_with_alpha(W, H);
    let merged_pixels = photo_rgba8(W, H);
    let mask_data: Vec<u8> = (0..(W * H)).map(|i| (i % 256) as u8).collect();

    let mut file = PsdFile::new(PsdHeader::rgba8(W, H));

    let mut bottom = PsdLayer::raster("Bottom", Rect::sized(W, H));
    bottom.set_rgba8(&bottom_pixels).unwrap();
    bottom.blend_mode = BlendMode::Multiply;
    bottom.opacity = 200;
    bottom.mask = Some(PsdMask::new(Rect::sized(W, H), mask_data.clone()));

    let mut top = PsdLayer::raster("Top", Rect::sized(W, H));
    top.set_rgba8(&top_pixels).unwrap();
    top.blend_mode = BlendMode::Screen;
    top.clipping = true;
    top.visible = false;

    let mut group = PsdLayer::group("Group");
    // Bottom-to-top, the order the format itself uses.
    group.push_child(bottom).unwrap();
    group.push_child(top).unwrap();
    file.layers.push(group);
    file.merged = Some(MergedImage::from_rgba8(W, H, &merged_pixels).unwrap());

    assert_eq!(
        file.record_count(),
        4,
        "a group is its own record plus a closing divider, so 2 + 2 rasters"
    );

    // --- write, then read it as another program would ---
    let bytes = psd::write(&file).unwrap();
    let back = psd::read(&bytes).unwrap();

    assert_eq!(back.header.width, W);
    assert_eq!(back.header.height, H);
    assert_eq!(back.header.channels, 4);
    assert_eq!(back.header.depth, psd::Depth::Eight);
    assert_eq!(back.header.color_mode, psd::ColorMode::Rgb);
    assert!(back.warnings.is_empty(), "warnings: {:?}", back.warnings);

    // --- the structure came back as a tree, not a flat list ---
    assert_eq!(back.layers.len(), 1, "one root layer: the group");
    let g = &back.layers[0];
    assert!(g.is_group());
    assert_eq!(g.name, "Group");
    assert_eq!(g.children().len(), 2);
    assert_eq!(back.all_layers().len(), 3);

    let read_bottom = &g.children()[0];
    let read_top = &g.children()[1];
    assert_eq!(read_bottom.name, "Bottom");
    assert_eq!(read_top.name, "Top", "and in the same bottom-to-top order");

    // --- and every property that describes how it looks ---
    assert_eq!(read_bottom.blend_mode, BlendMode::Multiply);
    assert_eq!(read_bottom.opacity, 200);
    assert!(!read_bottom.clipping);
    assert!(read_bottom.visible);
    assert_eq!(read_top.blend_mode, BlendMode::Screen);
    assert!(read_top.clipping, "the clipping flag survived");
    assert!(!read_top.visible, "and so did the hidden flag");

    // --- and the pixels ---
    assert_eq!(read_bottom.bounds, Rect::sized(W, H));
    assert_eq!(
        read_bottom.rgba8().expect("bottom has channels"),
        bottom_pixels,
        "an opaque layer's pixels survived"
    );
    assert_eq!(
        read_top.rgba8().expect("top has channels"),
        top_pixels,
        "and so did a layer with partial alpha"
    );

    // --- the mask ---
    let mask = read_bottom.mask.as_ref().expect("the mask survived");
    assert_eq!(mask.bounds, Rect::sized(W, H));
    assert_eq!(mask.data, mask_data);

    // --- and the composite every other reader shows first ---
    assert_eq!(
        back.merged
            .as_ref()
            .expect("a merged composite")
            .to_rgba8(W, H)
            .expect("interleaves"),
        merged_pixels
    );

    // Writing what was read produces the same file: a round trip is stable, so
    // opening and saving does not slowly rewrite someone's document.
    assert_eq!(psd::write(&back).unwrap(), bytes);
}

// ---------------------------------------------------------------------------
// 9. Crash recovery
// ---------------------------------------------------------------------------

/// A package saved twice with edits on both sides of the second save, plus the
/// edits made after it, as an unclean shutdown leaves them.
struct Crashed {
    package: std::path::PathBuf,
    in_memory: editor_core::Document,
    in_memory_pixels: Vec<u8>,
    /// Commands journalled between the first save and the second — the ones the
    /// snapshot on disk already contains.
    before_last_save: usize,
    /// Commands journalled after the second save — the ones a recovery owes.
    after_last_save: usize,
    _tmp: tempfile::TempDir,
}

/// Save, edit, save again, edit again, then stop.
///
/// The shape matters and it is the *normal* one: `save_project_with` copies the
/// valid prefix of the existing journal into every package it writes
/// (`package.rs::carry_journal_forward`), so from the user's second save onward
/// the journal always holds records on **both** sides of a save marker. A
/// fixture that saves once into a fresh package leaves `first_unsaved` at zero,
/// and every assertion about "the suffix after the last save" then passes over
/// an empty prefix — which is to say it proves nothing.
///
/// Every command here goes through [`OpenDocument::apply`], which is what
/// journals it: the record is appended *after* the command is accepted, never
/// before, or a recovery would replay one the snapshot never had.
fn crash_after_editing() -> Crashed {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("Session.rstudio");

    let mut doc = app::blank(TILE_SIZE, TILE_SIZE, "Session");
    let lower = doc
        .document
        .active_layer()
        .expect("File ▸ New makes a layer");
    doc.fill_layer(lower, [40, 90, 160, 255]);

    // --- the first save. Nothing is journalled before it: the document has no
    //     package yet, so `apply` has nowhere to write. ---
    doc.save_to(&package, APP_VERSION).unwrap();
    let journal = package.join(JOURNAL_FILE);
    assert!(
        CommandJournal::read(&journal)
            .unwrap()
            .since_last_save()
            .is_empty(),
        "nothing is outstanding immediately after a save"
    );

    // --- work between the two saves. These *are* journalled, and the second
    //     save carries them forward in front of its own marker. ---
    let upper = doc.add_layer(Layer::raster("Upper"));
    doc.fill_layer(upper, [220, 90, 40, 190]);
    let before_last_save = 2; // create + paint

    doc.save_to(&package, APP_VERSION).unwrap();
    let carried = CommandJournal::read(&journal).unwrap();
    assert_eq!(
        carried.commands().len(),
        before_last_save,
        "the second save must carry the first save's journal forward"
    );
    assert!(
        carried.since_last_save().is_empty(),
        "...and put its own marker after all of it"
    );

    // --- work the user did after the last save. Every one of these re-uses
    //     tiles the package already holds, which is what lets the recovered
    //     document be compared pixel for pixel — see the test below for the
    //     case that does not. ---
    let edits = vec![
        Command::SetLayerProperties {
            layer_id: upper,
            patch: LayerPatch {
                opacity: Some(0.35),
                ..Default::default()
            },
        },
        Command::SetLayerProperties {
            layer_id: lower,
            patch: LayerPatch {
                blend_mode: Some(BlendMode::Screen),
                ..Default::default()
            },
        },
        Command::TransformLayer {
            layer_id: upper,
            matrix: [1.0, 0.0, 0.0, 1.0, 9.0, -4.0],
        },
        Command::create_layer(Layer::raster("Added after the save")),
    ];
    let after_last_save = edits.len();
    for cmd in edits {
        doc.apply(cmd).unwrap();
    }

    let crashed = Crashed {
        package,
        in_memory: doc.document.clone(),
        in_memory_pixels: doc.composite_all(),
        before_last_save,
        after_last_save,
        _tmp: tmp,
    };
    // The process dies here. Nothing is flushed, nothing is saved, no clean
    // exit marker is written.
    drop(doc);
    crashed
}

#[test]
fn work_done_after_the_last_save_is_recovered_and_work_before_it_is_not_replayed_twice() {
    let crashed = crash_after_editing();
    let journal = crashed.package.join(JOURNAL_FILE);

    // --- what the journal holds: records on both sides of the marker ---
    let recovery = CommandJournal::read(&journal).unwrap();
    assert!(!recovery.truncated());
    assert!(
        recovery.last_save().is_some(),
        "the save marker is what makes the suffix a suffix"
    );
    assert_eq!(
        recovery.commands().len(),
        crashed.before_last_save + crashed.after_last_save,
        "the journal must carry the pre-save records too, or the suffix logic \
         is never exercised"
    );
    assert_eq!(
        recovery.since_last_save().len(),
        crashed.after_last_save,
        "only the commands recorded after the last save may be replayed"
    );
    assert!(
        recovery.records_read() > recovery.since_last_save().len() as u64,
        "records read ({}) must exceed the replayable suffix ({}), or this \
         fixture is not testing the marker at all",
        recovery.records_read(),
        recovery.since_last_save().len()
    );

    // --- the next run opens the package it finds ---
    let mut doc = app::open_project(&crashed.package);
    assert_ne!(
        doc.document, crashed.in_memory,
        "the snapshot on disk is older than what was in memory — \
         otherwise this test proves nothing"
    );

    // --- and asks the session layer what is outstanding ---
    let outstanding = session::recoverable(&crashed.package)
        .unwrap()
        .expect("the post-save edits are recoverable");
    assert!(!outstanding.truncated);
    assert_eq!(
        outstanding.commands.len(),
        crashed.after_last_save,
        "recovery offered {} commands but only {} were made after the save; \
         replaying the pre-save records would duplicate the snapshot's own work",
        outstanding.commands.len(),
        crashed.after_last_save
    );

    // --- and replays exactly those, through History, so the restore is
    //     itself undoable ---
    let (applied, error) =
        session::replay(&mut doc.document, &mut doc.history, &outstanding.commands);
    assert_eq!(error, None, "the replay stopped early");
    assert_eq!(applied, crashed.after_last_save);
    assert!(doc.history.can_undo(), "recovered work must be undoable");

    // --- and gets back exactly what was in memory ---
    assert_eq!(
        doc.document, crashed.in_memory,
        "the recovered document must equal the one that was lost"
    );
    assert_eq!(
        doc.composite_all(),
        crashed.in_memory_pixels,
        "...down to the pixels on screen"
    );
}

#[test]
fn the_save_marker_is_what_pairs_a_journal_with_its_snapshot() {
    // The digest half of the same mechanism: `replay_onto` refuses a journal
    // whose marker names a different document, and applies only the suffix.
    let crashed = crash_after_editing();
    let journal = crashed.package.join(JOURNAL_FILE);
    let loaded = project_format::open_project(&crashed.package).unwrap();
    assert!(
        !loaded.recovered_from_interrupted_save,
        "the save itself completed; it is the session that did not"
    );

    let recovery = CommandJournal::read(&journal).unwrap();
    let mut recovered = loaded.document.clone();
    let applied = recovery
        .replay_onto(&mut recovered, loaded.document_digest)
        .unwrap();
    assert_eq!(applied, crashed.after_last_save);
    assert_eq!(recovered, crashed.in_memory);

    // A journal whose marker describes some other snapshot is refused outright
    // rather than applied to a document it was never recorded against.
    let mut other = loaded.document;
    let err = recovery
        .replay_onto(
            &mut other,
            project_format::DocumentDigest::of(b"not this document"),
        )
        .unwrap_err();
    assert!(
        matches!(err, project_format::ProjectError::SnapshotMismatch { .. }),
        "{err}"
    );
}

#[test]
fn a_journal_torn_by_the_crash_recovers_its_intact_prefix_and_says_so() {
    let crashed = crash_after_editing();
    let journal = crashed.package.join(JOURNAL_FILE);

    // What a process killed part way through an append leaves behind.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap();
        f.write_all(b"{\"Transaction\":{\"label\":\"half a rec")
            .unwrap();
    }

    let mut doc = app::open_project(&crashed.package);
    let outstanding = session::recoverable(&crashed.package)
        .unwrap()
        .expect("the intact records still recover");
    assert!(
        outstanding.truncated,
        "the tear must be reported, not hidden"
    );
    assert_eq!(
        outstanding.commands.len(),
        crashed.after_last_save,
        "every intact record after the marker still replays, and none before it"
    );

    let (applied, error) =
        session::replay(&mut doc.document, &mut doc.history, &outstanding.commands);
    assert_eq!((applied, error), (crashed.after_last_save, None));
    assert_eq!(doc.document, crashed.in_memory);
}

#[test]
fn a_replay_that_cannot_finish_keeps_what_it_managed() {
    // A journal from a crashed process can end in a record that no longer
    // applies. The earlier commands are real work and must survive.
    let mut doc = app::blank(64, 64, "Partial");
    let good = Command::create_layer(Layer::raster("kept"));
    let bad = Command::DeleteLayer {
        layer_id: layer_model::LayerId::new(),
    };
    let mut history = History::new();
    let before = doc.document.layers.len();
    let (applied, error) = session::replay(&mut doc.document, &mut history, &[good, bad]);
    assert_eq!(applied, 1);
    assert!(error.is_some(), "the failure must be reported");
    assert_eq!(
        doc.document.layers.len(),
        before + 1,
        "the good command survived"
    );
}

/// A known gap, pinned rather than described.
///
/// The journal records a paint as the tile *hashes* the layer carries
/// afterwards — that is what makes a hundred-tile stroke one small, invertible
/// record. The bytes behind those hashes live in the tile store, and the tile
/// store is only written to a package by a *save*. So a stroke made after the
/// last save is recovered as a correct document that references pixels no
/// package holds, and the compositor reads an unresolvable hash as transparent.
///
/// Recovering those bytes needs a scratch tile store that is written as tiles
/// are produced rather than at save time. Nothing in the workspace has one yet.
#[test]
fn a_paint_made_after_the_last_save_recovers_its_reference_but_not_its_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("Painted.rstudio");

    let mut doc = app::blank(TILE_SIZE, TILE_SIZE, "Painted");
    let layer = doc.document.active_layer().unwrap();
    doc.fill_layer(layer, [10, 20, 30, 255]);
    doc.save_to(&package, APP_VERSION).unwrap();

    // A stroke after the save, journalled by `apply` like every other command.
    let before_paint = doc.document.clone();
    doc.paint_layer(layer, &[TileCoord::new(0, 0, 0)], &|_, x, y| {
        [(x % 256) as u8, (y % 256) as u8, 200, 255]
    });
    let painted_hash = doc
        .document
        .layer_tiles(layer)
        .unwrap()
        .get(TileCoord::new(0, 0, 0))
        .unwrap();
    assert_ne!(
        doc.document, before_paint,
        "the paint really did change the document"
    );
    let in_memory = doc.document.clone();
    drop(doc);

    // --- recovery ---
    let mut back = app::open_project(&package);
    let outstanding = session::recoverable(&package)
        .unwrap()
        .expect("the paint is recoverable as a command");
    assert_eq!(outstanding.commands.len(), 1);
    let (applied, error) =
        session::replay(&mut back.document, &mut back.history, &outstanding.commands);
    assert_eq!((applied, error), (1, None));

    // The document is right...
    assert_eq!(back.document, in_memory);
    assert_eq!(
        back.document
            .layer_tiles(layer)
            .unwrap()
            .get(TileCoord::new(0, 0, 0)),
        Some(painted_hash)
    );

    // ...and the bytes it names are not in the package, because no save ever
    // wrote them. This is the gap; when a scratch tile store lands, this
    // assertion is what has to be inverted.
    assert!(
        back.tile_bytes(painted_hash).is_none(),
        "the package unexpectedly holds the stroke's bytes — if a scratch \
         tile store now persists them, invert this assertion"
    );
}

/// P3.10: the checked-in v1 fixture opens through the real migration path.
///
/// The fixture was written by a build stamped `format_version = 1` (see the
/// generator's history in git); loading it runs the gate, the 1→2 no-op step,
/// and the 2→3 repair — which strips the pixel store and selection no real v1
/// build could have written — and stamps the result as the current format.
#[test]
fn a_v1_fixture_opens_through_the_migration_path() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/old-format-v1.rstudio");
    let loaded = project_format::open_project(&fixture).unwrap();
    assert_eq!(loaded.document.meta.format_version, 3, "stamped to current");
    assert_eq!(
        loaded.document.pixels.tile_count(),
        0,
        "the 2->3 repair stripped the pixel store a v1 file cannot justify"
    );
    assert_eq!(
        loaded.document.selection,
        editor_core::Selection::None,
        "the repair cleared the selection too"
    );
    assert!(
        loaded.document.width() == 64 && loaded.document.height() == 64,
        "the geometry survived the migration"
    );
}
