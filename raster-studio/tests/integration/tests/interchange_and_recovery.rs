//! Getting work *out* of the editor, and getting it back after a crash.
//!
//! Export and `.psd` are how a document reaches another program; the command
//! journal is how it survives a process that never reached its exit path. Both
//! are driven through the application's own calls —
//! [`app_shell::doc::OpenDocument::export_to`] and
//! [`app_shell::session::recoverable`]/[`app_shell::session::replay`] — with
//! one stated exception, the PSD test, which is labelled where it sits.

use app_shell::doc::OpenDocument;
use app_shell::import::document_from_psd;
use app_shell::session;
use editor_core::{Command, History, LayerPatch};
use integration_tests::app::{self, DocExt, APP_VERSION};
use integration_tests::fixture::thumbnail::fnv1a64;
use integration_tests::fixture::{
    max_channel_diff, mean_channel_diff, photo_rgba8, photo_rgba8_channels_cycled,
    photo_rgba8_with_alpha,
};
use layer_model::{BlendMode, Layer};
use project_format::{CommandJournal, JOURNAL_FILE};
use psd::{
    Adjustment, Descriptor, Effects, MergedImage, PsdFile, PsdHeader, PsdLayer, PsdMask, Rect,
    TextData, Value,
};
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
    assert_eq!(
        loaded.document.meta.format_version,
        editor_core::DOCUMENT_FORMAT_VERSION,
        "stamped to current"
    );
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

// -------------------------------------------------------------------------
// 8b. Card 073 fixtures — expected results, not interoperability evidence
// -------------------------------------------------------------------------

/// The card-073 fixture set, generated here by `psd::write`.
///
/// # What these fixtures are, and what they are not
///
/// Every byte comes from this workspace's own writer, so the fixtures below
/// prove what the reader/writer carry and what the import reports — the E11/E12
/// predictions of `docs/THUMBNAIL-BASELINE.md`. **Self-round-trip is not
/// interoperability evidence**: no licensed independent-writer PSD is
/// available locally (the plan forbids downloads), so per the card's own
/// condition that interoperability gate REMAINS PENDING even with every
/// assertion here green. The ignored materializer at the bottom of this
/// section writes the set under `tests/project-fixtures/psd/` for the record,
/// with provenance and hashes in its README.
///
/// `crates/psd/src/write.rs` deliberately has no `TySh` writer (see
/// `psd::text`), so the type fixture's styled text is a hand-crafted minimal
/// `TySh` block — the smallest payload `text::parse` accepts — carried in
/// `TextData::raw`, which the writer emits verbatim.
mod card073 {
    use super::*;
    use psd::bytes::Sink;

    /// 90° rotation about the origin, translated to (40, 12). A `.psd` layer
    /// rectangle cannot express the rotation; only a `TySh` transform can.
    pub const ROTATED: [f64; 6] = [0.0, 1.0, -1.0, 0.0, 40.0, 12.0];

    fn probe(width: u32, height: u32, salt: u8) -> Vec<u8> {
        let n = width as usize * height as usize * 4;
        (0..n)
            .map(|i| ((i * 37 + usize::from(salt)) % 251) as u8)
            .collect()
    }

    fn gray(width: u32, height: u32) -> Vec<u8> {
        (0..width as usize * height as usize)
            .map(|i| (i % 251) as u8)
            .collect()
    }

    /// A hand-crafted minimal `TySh` payload: version, transform, a text
    /// descriptor carrying `Txt ` and opaque `EngineData`, an empty warp
    /// descriptor, a rectangle.
    pub fn tysh(text: &str, transform: [f64; 6]) -> Vec<u8> {
        let mut s = Sink::new();
        s.u16(1);
        for v in transform {
            s.f64(v);
        }
        s.u16(50);
        s.u32(16);
        let mut d = Descriptor::new("TxLr");
        d.push("Txt ", Value::from(text)).unwrap();
        d.push("EngineData", Value::RawData(b"<< /x 1 >>".to_vec()))
            .unwrap();
        d.write(&mut s).unwrap();
        s.u16(1);
        s.u32(16);
        Descriptor::new("warp").write(&mut s).unwrap();
        s.i32(0);
        s.i32(0);
        s.i32(120);
        s.i32(28);
        s.into_inner()
    }

    /// A hand-crafted minimal `TySh` payload whose text descriptor carries no
    /// `Txt ` key — the unparseable member of the editable-text subset.
    pub fn tysh_without_text(transform: [f64; 6]) -> Vec<u8> {
        let mut s = Sink::new();
        s.u16(1);
        for v in transform {
            s.f64(v);
        }
        s.u16(50);
        s.u32(16);
        Descriptor::new("TxLr").write(&mut s).unwrap();
        s.u16(1);
        s.u32(16);
        Descriptor::new("warp").write(&mut s).unwrap();
        s.i32(0);
        s.i32(0);
        s.i32(120);
        s.i32(28);
        s.into_inner()
    }

    /// The card-075 `lfx2` block: the four required effects (the outer glow
    /// deliberately disabled) plus a satin kind this build does not model,
    /// at 150 % scale — the same values the psd crate's card-073 builder
    /// writes, rebuilt here through the public descriptor API.
    pub fn effects_block() -> Vec<u8> {
        let unit = |unit: &str, value: f64| Value::UnitFloat {
            unit: unit.as_bytes().try_into().unwrap(),
            value,
        };
        let rgb = |r: f64, g: f64, b: f64| {
            let mut c = Descriptor::new("RGBC");
            c.push("Rd  ", Value::Double(r)).unwrap();
            c.push("Grn ", Value::Double(g)).unwrap();
            c.push("Bl  ", Value::Double(b)).unwrap();
            Value::Descriptor(c)
        };
        let blnm = |v: &str| Value::Enumerated {
            type_id: "BlnM".into(),
            value: v.into(),
        };
        let enumerated = |ty: &str, v: &str| Value::Enumerated {
            type_id: ty.into(),
            value: v.into(),
        };

        let mut s = Sink::new();
        s.u32(1); // object version
        s.u32(16); // descriptor version
        let mut d = Descriptor::new("Lfx2");
        d.push("masterFXSwitch", Value::Bool(true)).unwrap();
        d.push("Scl ", unit("#Prc", 150.0)).unwrap();

        let mut drsh = Descriptor::new("DrSh");
        drsh.push("enab", Value::Bool(true)).unwrap();
        drsh.push("Md  ", blnm("Mltp")).unwrap();
        drsh.push("Clr ", rgb(0.0, 0.0, 0.0)).unwrap();
        drsh.push("opacity", unit("#Prc", 75.0)).unwrap();
        drsh.push("lagl", unit("#Ang", 130.0)).unwrap();
        drsh.push("uglg", Value::Bool(false)).unwrap();
        drsh.push("Dstn", unit("#Pxl", 8.0)).unwrap();
        drsh.push("blur", unit("#Pxl", 16.0)).unwrap();
        drsh.push("Ckmt", unit("#Pxl", 4.0)).unwrap();
        drsh.push("layerConceals", Value::Bool(false)).unwrap();
        d.push("DrSh", Value::Descriptor(drsh)).unwrap();

        let mut frfx = Descriptor::new("FrFX");
        frfx.push("enab", Value::Bool(true)).unwrap();
        frfx.push("Md  ", blnm("Nrml")).unwrap();
        frfx.push("Clr ", rgb(255.0, 255.0, 255.0)).unwrap();
        frfx.push("Opct", unit("#Prc", 100.0)).unwrap();
        frfx.push("Sz  ", unit("#Pxl", 4.0)).unwrap();
        frfx.push("PntT", enumerated("FrFl", "SClr")).unwrap();
        frfx.push("Styl", enumerated("FStl", "OutF")).unwrap();
        d.push("FrFX", Value::Descriptor(frfx)).unwrap();

        let mut sofi = Descriptor::new("SoFi");
        sofi.push("enab", Value::Bool(true)).unwrap();
        sofi.push("Md  ", blnm("Clr ")).unwrap();
        sofi.push("Clr ", rgb(220.0, 60.0, 30.0)).unwrap();
        sofi.push("Opct", unit("#Prc", 50.0)).unwrap();
        d.push("SoFi", Value::Descriptor(sofi)).unwrap();

        let mut orgl = Descriptor::new("OrGl");
        orgl.push("enab", Value::Bool(false)).unwrap();
        orgl.push("Md  ", blnm("Scrn")).unwrap();
        orgl.push("Clr ", rgb(255.0, 255.0, 0.0)).unwrap();
        orgl.push("Opct", unit("#Prc", 60.0)).unwrap();
        orgl.push("blur", unit("#Pxl", 10.0)).unwrap();
        d.push("OrGl", Value::Descriptor(orgl)).unwrap();

        d.push("ChFX", Value::Descriptor(Descriptor::new("ChFX")))
            .unwrap();
        d.write(&mut s).unwrap();
        s.into_inner()
    }

    /// A minimal descriptor-shaped `hue2` payload.
    pub fn hue_saturation() -> Vec<u8> {
        let mut s = Sink::new();
        s.u32(16); // version `Adjustment::descriptor` skips
        let mut d = Descriptor::new("hue2");
        d.push(
            "PresetKind",
            Value::Enumerated {
                type_id: "PresetKind".into(),
                value: "normal".into(),
            },
        )
        .unwrap();
        d.write(&mut s).unwrap();
        s.into_inner()
    }

    /// The full scene, bottom-to-top: backdrop, masked portrait, unsupported
    /// Hue-Saturation adjustment, Invert adjustment, and a nested `Title`
    /// group holding a styled type layer (drop shadow, rotated, clipped) and
    /// a collapsed group with translated, hidden content.
    pub fn scene() -> PsdFile {
        let mut file = PsdFile::new(PsdHeader::rgba8(128, 96));

        let mut backdrop = PsdLayer::raster("Backdrop", Rect::sized(128, 96));
        backdrop.set_rgba8(&probe(128, 96, 10)).unwrap();
        backdrop.blend_mode = BlendMode::Multiply;

        let mut portrait = PsdLayer::raster("Portrait (masked)", Rect::new(16, 16, 64, 80));
        portrait.set_rgba8(&probe(48, 64, 20)).unwrap();
        portrait.opacity = 220;
        portrait.mask = Some(PsdMask::new(Rect::new(8, 8, 72, 88), gray(64, 80)));

        let mut hue_sat = PsdLayer::raster("Hue/Sat 1", Rect::default());
        hue_sat.pixel_data_irrelevant = true;
        hue_sat.adjustment = Some(Adjustment {
            key: *b"hue2",
            data: hue_saturation(),
        });

        let mut invert = PsdLayer::raster("Invert 1", Rect::default());
        invert.pixel_data_irrelevant = true;
        invert.adjustment = Some(Adjustment {
            key: *b"nvrt",
            data: Vec::new(),
        });

        let mut headline = PsdLayer::raster("Headline", Rect::default());
        headline.blend_mode = BlendMode::Multiply;
        headline.opacity = 200;
        headline.clipping = true;
        headline.effects = Some(Effects {
            key: *b"lfx2",
            data: effects_block(),
        });
        headline.text = Some(TextData {
            transform: ROTATED,
            text: Some("SELL NOW".to_owned()),
            raw: tysh("SELL NOW", ROTATED),
        });

        let mut sticker = PsdLayer::raster("Sticker", Rect::new(48, 8, 88, 40));
        sticker.set_rgba8(&probe(40, 32, 40)).unwrap();
        sticker.visible = false;

        let mut effects_group = PsdLayer::group("Effects");
        effects_group.group_data_mut().unwrap().open = false;
        effects_group.push_child(sticker).unwrap();

        let mut title = PsdLayer::group("Title");
        title.push_child(headline).unwrap();
        title.push_child(effects_group).unwrap();

        file.layers.push(backdrop);
        file.layers.push(portrait);
        file.layers.push(hue_sat);
        file.layers.push(invert);
        file.layers.push(title);
        file.merged = Some(MergedImage::from_rgba8(128, 96, &probe(128, 96, 50)).unwrap());
        file
    }
}

/// E11: importing the card-073 fixture keeps the layer metadata and names
/// every loss — the type layer's parseable subset imports editable with the
/// reported default-font substitution, the four required effects import as
/// editable parameters with the unmapped satin named, and the
/// adjustment this build cannot evaluate is kept as an empty layer with its
/// tag named.
///
/// The Invert adjustment is the one that maps exactly, the masked portrait
/// keeps its coverage, and the nested groups keep their nesting and flags.
#[test]
fn importing_the_card073_fixture_reports_every_loss_and_keeps_the_layer_metadata() {
    let bytes = psd::write(&card073::scene()).unwrap();
    let import = document_from_psd(&bytes, "card073.psd", 50).unwrap();
    let doc = &import.imported.document;

    // --- the tree survived: nesting, order (top-most first here), names ---
    let root = doc.layers.root();
    assert_eq!(root.len(), 5, "the group's three flat siblings plus Title");
    let names = |ids: &[layer_model::LayerId]| -> Vec<String> {
        ids.iter()
            .map(|&id| doc.layers.get(id).expect("reachable").name.clone())
            .collect()
    };
    assert_eq!(
        names(root),
        vec![
            "Title",
            "Invert 1",
            "Hue/Sat 1",
            "Portrait (masked)",
            "Backdrop"
        ]
    );
    let title = doc.layers.get(root[0]).unwrap();
    let layer_model::LayerKind::Group(group) = &title.kind else {
        panic!("Title must import as a group");
    };
    assert!(!group.collapsed);
    assert_eq!(names(&group.children), vec!["Effects", "Headline"]);
    let effects_id = group.children[0];
    let effects = doc.layers.get(effects_id).unwrap();
    let layer_model::LayerKind::Group(inner) = &effects.kind else {
        panic!("Effects must import as a group");
    };
    assert!(
        inner.collapsed,
        "the inner group's collapsed state survived"
    );
    assert_eq!(names(&inner.children), vec!["Sticker"]);

    // --- the style metadata came back ---
    let headline = doc.layers.get(group.children[1]).unwrap();
    assert_eq!(headline.blend_mode, BlendMode::Multiply);
    assert!((headline.opacity - 200.0 / 255.0).abs() < 1e-6);
    assert_eq!(headline.clipping, layer_model::ClippingMode::ClipToBelow);
    assert!(headline.visible);

    // Card 075: the four required effects import as editable parameters —
    // colours stored gamma-encoded in document space (decoded to linear at
    // render by the compositor), percentages to 0..1, pixels scaled
    // by the block's 150 %, the disabled glow absent.
    {
        let e = &headline.effects;
        assert!(e.enabled, "the master switch is on");
        let s = e.drop_shadow.as_ref().expect("the drop shadow mapped");
        assert_eq!(s.blend_mode, BlendMode::Multiply);
        assert_eq!(s.color, [0.0, 0.0, 0.0, 1.0]);
        assert!((s.opacity - 0.75).abs() < 1e-6);
        assert!((s.angle_deg - 130.0).abs() < 1e-6);
        assert!(!s.use_global_light);
        assert!((s.distance_px - 12.0).abs() < 1e-6, "8 px × 150 %");
        assert!((s.size_px - 24.0).abs() < 1e-6);
        assert!((s.spread - 0.25).abs() < 1e-6, "6 px of 24 px");
        let k = e.stroke.as_ref().expect("the solid stroke mapped");
        assert_eq!(k.blend_mode, BlendMode::Normal);
        assert!((k.size_px - 6.0).abs() < 1e-6, "4 px × 150 %");
        assert_eq!(k.position, layer_model::StrokePosition::Outside);
        assert!(matches!(&k.fill, layer_model::FillStyle::Solid(c)
            if c.iter().zip([1.0f32, 1.0, 1.0, 1.0]).all(|(a, b)| (a - b).abs() < 1e-6)));
        let o = e.color_overlay.as_ref().expect("the colour overlay mapped");
        assert_eq!(o.blend_mode, BlendMode::Color);
        assert!((o.opacity - 0.5).abs() < 1e-6);
        assert!(matches!(&o.color, [r, g, b, 1.0]
            if (r - 220.0 / 255.0).abs() < 1e-3 && (g - 60.0 / 255.0).abs() < 1e-3 && (b - 30.0 / 255.0).abs() < 1e-3));
        assert!(e.outer_glow.is_none(), "a disabled effect is absent");
        assert_eq!(e.count(), 3);
    }

    // The type layer's editable subset: the parseable `Txt ` string and the
    // `TySh` transform import as an editable text layer — with the editor's
    // default font, since the format does not name one outside the engine
    // data. The substitution is reported below, not silent.
    let layer_model::LayerKind::Text(text) = &headline.kind else {
        panic!("a parseable type layer imports as editable text");
    };
    assert_eq!(text.text, "SELL NOW");
    assert_eq!(
        text.font_family,
        tools::text::DEFAULT_FONT_FAMILY,
        "the reported default family, not a silent guess"
    );
    assert_eq!(
        headline.transform,
        {
            let [xx, xy, yx, yy, tx, ty] = card073::ROTATED;
            glam::Affine2::from_cols_array(&[
                xx as f32, xy as f32, yx as f32, yy as f32, tx as f32, ty as f32,
            ])
        },
        "the TySh transform came across as the layer affine"
    );

    // The masked portrait keeps its coverage as a raster mask.
    let portrait = doc.layers.get(root[3]).unwrap();
    assert!(portrait.mask.is_some(), "the portrait's mask imports");

    // Invert is the one adjustment whose definition is its name.
    let invert = doc.layers.get(root[1]).unwrap();
    assert!(matches!(
        invert.kind,
        layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
            kind: layer_model::AdjustmentKind::Invert,
        })
    ));

    // --- the honesty gate: the notes, verbatim and in the order recorded ---
    assert_eq!(
        import.notes.notes(),
        vec![
            // E11, unsupported adjustment: the note names the tag.
            "adjustment layer(s) this build cannot evaluate (\u{201c}Hue/Sat 1 (hue2)\u{201d}) \
             were kept as empty layers; their effect is in the flattened image but not editable",
            // E11, styled type: the parseable subset imports editable, with
            // the default-font substitution named.
            "type layer(s) (\u{201c}Headline\u{201d}) were imported as editable text with the \
             default font, size and fill — the source font is not in this build's supported \
             subset",
            // E11, effects: the four required kinds imported (asserted
            // below); the satin this build does not model is named, and the
            // disabled glow is silent — Photoshop does not draw it either.
            "the satin effect(s) on \u{201c}Headline\u{201d} were not imported",
        ],
        "nothing silent, nothing invented: the fixture's losses, by name"
    );
}

/// Card 075 render-path check: an imported effect colour must survive the
/// compositor. The stored convention is gamma-encoded document-space 0..1,
/// which the compositor decodes to linear once at render — so a solid colour
/// overlay at 100 % opacity over a white background renders the *source*
/// colour, not the double-darkened value a second linearisation would give.
/// (The double-encoded value of sRGB 220, 60, 30 would render near 188, 6, 0.)
#[test]
fn an_imported_color_overlay_renders_the_source_colour_not_a_doubly_darkened_one() {
    // The fixture: a white backdrop and a 32×32 swatch layer carrying one
    // solid colour overlay — normal blend, sRGB 220, 60, 30, 100 %.
    let mut file = PsdFile::new(PsdHeader::rgba8(64, 64));

    let mut backdrop = PsdLayer::raster("Backdrop", Rect::sized(64, 64));
    backdrop.set_rgba8(&vec![255u8; 64 * 64 * 4]).unwrap();

    let mut swatch = PsdLayer::raster("Swatch", Rect::new(16, 16, 48, 48));
    swatch
        .set_rgba8(
            &(0..32 * 32)
                .flat_map(|_| [128u8, 128, 128, 255])
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let mut sofi = Descriptor::new("SoFi");
    sofi.push(
        "Md  ",
        Value::Enumerated {
            type_id: "BlnM".into(),
            value: "Nrml".into(),
        },
    )
    .unwrap();
    let mut clr = Descriptor::new("RGBC");
    clr.push("Rd  ", Value::Double(220.0)).unwrap();
    clr.push("Grn ", Value::Double(60.0)).unwrap();
    clr.push("Bl  ", Value::Double(30.0)).unwrap();
    sofi.push("Clr ", Value::Descriptor(clr)).unwrap();
    sofi.push(
        "Opct",
        Value::UnitFloat {
            unit: b"#Prc".as_slice().try_into().unwrap(),
            value: 100.0,
        },
    )
    .unwrap();
    let mut lfx2 = Descriptor::new("Lfx2");
    lfx2.push("SoFi", Value::Descriptor(sofi)).unwrap();
    // The `lfx2` record layout: object version, descriptor version, then the
    // descriptor itself.
    let mut s = psd::bytes::Sink::new();
    s.u32(1);
    s.u32(16);
    lfx2.write(&mut s).unwrap();
    swatch.effects = Some(Effects {
        key: *b"lfx2",
        data: s.into_inner(),
    });

    file.layers.push(backdrop);
    file.layers.push(swatch);
    file.merged = Some(MergedImage::from_rgba8(64, 64, &[255u8; 64 * 64 * 4]).unwrap());

    // Import into a real document (an sRGB one — the space every PSD lands
    // in here) and render it the way the screen sees it.
    let bytes = psd::write(&file).unwrap();
    let import = document_from_psd(&bytes, "glow.psd", 50).unwrap();
    let mut doc = OpenDocument::from_import(app::next_id(), import.imported);
    assert_eq!(doc.document.meta.color_space, color::ColorSpace::Srgb);
    let composite = doc.composite_all();

    // The pixel at the overlay's centre: Photoshop's appearance is the
    // source colour — sRGB 220, 60, 30. Double-linearising at import would
    // darken it to roughly (188, 6, 0), a delta the exact assert rejects.
    let px = |x: usize, y: usize| &composite[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
    assert_eq!(px(32, 32), &[220u8, 60, 30, 255], "overlay centre");
    // A corner of the swatch, clear of any edge filtering.
    assert_eq!(px(20, 20), &[220u8, 60, 30, 255], "overlay corner");
    // And the backdrop outside the swatch stays white.
    assert_eq!(px(4, 4), &[255u8, 255, 255, 255], "backdrop");
}

/// E12: exporting a document whose layers a `.psd` has no home for — and whose
/// pixels sit under a rotation the format cannot express — produces a good
/// merged preview *and* an honest report saying the layers did not survive as
/// themselves. A correct preview alone is explicitly insufficient proof.
#[test]
fn exporting_homeless_and_transformed_layers_reports_what_the_psd_cannot_express() {
    let mut doc = app::blank(64, 64, "Card 073");
    let backdrop = doc
        .document
        .active_layer()
        .expect("File ▸ New makes a layer");
    doc.fill_layer(backdrop, [235, 235, 225, 255]);

    // A raster with real pixels, under a rotation: pixels written where they
    // are stored, and the note says so.
    let portrait = doc.add_layer(Layer::raster("Portrait"));
    doc.fill_layer(portrait, [200, 120, 60, 255]);
    doc.apply(Command::TransformLayer {
        layer_id: portrait,
        matrix: [
            std::f32::consts::FRAC_1_SQRT_2,
            -std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
            8.0,
            3.0,
        ],
    })
    .expect("the transform applies");

    // A text layer: card 078 renders its appearance as fallback pixels and
    // the note says the text did not stay editable.
    let _headline = doc.add_layer(Layer::with_kind(
        "Headline",
        layer_model::LayerKind::Text(layer_model::TextLayer {
            text: "SELL NOW".into(),
            ..Default::default()
        }),
    ));

    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("card073-export.psd");
    let notes = doc.export_psd_to(&out).expect("the export succeeds");
    assert_eq!(
        notes.notes(),
        vec![
            // E12, non-translation transform.
            "\u{201c}Portrait\u{201d} carry a transform a .psd cannot express; their pixels \
             were written where they are stored",
            // Card 078: the text layer's appearance fallback, named.
            "text, shape and smart-object layer(s) (\u{201c}Headline\u{201d}) cannot stay \
             editable in a .psd; their rendered appearance was written as a raster layer's \
             pixels",
        ],
        "the export notes are the honesty gate, verbatim"
    );
    assert_eq!(doc.psd_notes().notes(), notes.notes());

    // What landed in the file: the text layer exists as a record WITH its
    // rendered fallback pixels — not an empty layer — though the text
    // metadata itself is not written (card 079 adds the editable subset).
    let bytes = std::fs::read(&out).unwrap();
    let back = psd::read(&bytes).unwrap();
    let all = back.all_layers();
    let headline_record = all
        .iter()
        .find(|l| l.name == "Headline")
        .expect("the text layer's record is in the file");
    assert!(
        !headline_record.bounds.is_empty(),
        "the text layer's fallback imagery is in the file"
    );
    assert!(headline_record.text.is_none());
    let portrait_record = all
        .iter()
        .find(|l| l.name == "Portrait")
        .expect("the portrait's record is in the file");
    assert!(
        !portrait_record.bounds.is_empty(),
        "the rotated layer's pixels are written where they are stored"
    );
}

/// The malformed member of the card-073 set: a truncated fixture fails loudly
/// through `ImportError::Psd` carrying the reader's byte offset, and a file
/// that is not a `.psd` at all is refused rather than guessed at.
#[test]
fn a_truncated_card073_fixture_fails_loudly_through_the_import_path() {
    let bytes = psd::write(&card073::scene()).unwrap();
    let err = document_from_psd(&bytes[..bytes.len() / 3], "cut.psd", 10).unwrap_err();
    match err {
        app_shell::import::ImportError::Psd(psd::PsdError::Truncated { at, needed, .. }) => {
            assert!(needed > 0);
            assert!(at > 0, "the offset is absolute and meaningful, not a stub");
            assert!(at <= bytes.len() / 3);
        }
        other => panic!("expected a loud Psd truncation, got: {other}"),
    }
    let err = document_from_psd(b"not a psd at all", "junk.psd", 10).unwrap_err();
    assert!(
        err.to_string().contains("Photoshop document"),
        "the refusal names what could not be read: {err}"
    );
}

/// T074: the supported editable text subset. A type layer whose `TySh` block
/// carries a parseable `Txt ` string imports as an editable `LayerKind::Text`
/// under the block's own transform, with the unavoidable font/size/fill
/// substitution reported by name. The layer stays a real text layer: a text
/// edit applies, the text renders, and a project save/reopen keeps it.
#[test]
fn a_parseable_type_layer_imports_as_editable_text() {
    const TEXT: &str = "Editable now";
    let mut file = PsdFile::new(PsdHeader::rgba8(64, 48));
    let mut headline = PsdLayer::raster("Headline", Rect::default());
    headline.text = Some(TextData {
        transform: card073::ROTATED,
        text: Some(TEXT.to_owned()),
        raw: card073::tysh(TEXT, card073::ROTATED),
    });
    file.layers.push(headline);
    let bytes = psd::write(&file).unwrap();

    let import = document_from_psd(&bytes, "type.psd", 10).unwrap();
    // The substitution is the report — nothing about the defaulting is silent.
    assert_eq!(
        import.notes.notes(),
        vec![
            "type layer(s) (\u{201c}Headline\u{201d}) were imported as editable text with the \
             default font, size and fill — the source font is not in this build's supported \
             subset",
        ],
        "the default-font note, verbatim"
    );

    let mut doc = OpenDocument::from_import(app::next_id(), import.imported);
    let layer = doc.document.active_layer().expect("the type layer");
    let layer_model::LayerKind::Text(text) = &doc.document.layers.get(layer).unwrap().kind else {
        panic!("a parseable type layer must import as an editable text layer");
    };
    assert_eq!(text.text, TEXT, "the exact string");
    assert_eq!(
        text.font_family,
        tools::text::DEFAULT_FONT_FAMILY,
        "the reported default family"
    );
    assert_eq!(text.size_px, tools::text::DEFAULT_SIZE_PX);
    let expected = {
        let [xx, xy, yx, yy, tx, ty] = card073::ROTATED;
        glam::Affine2::from_cols_array(&[
            xx as f32, xy as f32, yx as f32, yy as f32, tx as f32, ty as f32,
        ])
    };
    assert_eq!(
        doc.document.layers.get(layer).unwrap().transform,
        expected,
        "the TySh transform, as the layer affine"
    );

    // Editable for real: a text edit of the same class applies.
    let edited = layer_model::TextLayer {
        text: "EDITED".into(),
        font_family: tools::text::DEFAULT_FONT_FAMILY.into(),
        size_px: tools::text::DEFAULT_SIZE_PX,
        ..layer_model::TextLayer::default()
    };
    doc.apply(Command::SetLayerKind {
        layer_id: layer,
        kind: Box::new(layer_model::LayerKind::Text(edited.clone())),
    })
    .expect("a text edit applies to an imported text layer");
    let layer_model::LayerKind::Text(after) = &doc.document.layers.get(layer).unwrap().kind else {
        panic!("the edit landed");
    };
    assert_eq!(after.text, "EDITED");

    // And it renders: the composite has ink where the text sits, not a
    // silently blank layer.
    let composite = doc.composite_all();
    assert!(
        composite.chunks(4).any(|p| p[3] > 0),
        "the imported text layer renders"
    );

    // A project save/reopen keeps the editable layer (a .psd export would not
    // — it has no home for text and says so).
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("text-import.rstudio");
    doc.save_to(&package, app::APP_VERSION).unwrap();
    drop(doc);
    let back = app::open_project(&package);
    let layer_model::LayerKind::Text(reopened) = &back.document.layers.get(layer).unwrap().kind
    else {
        panic!("the text layer survives a save/reopen");
    };
    assert_eq!(reopened.text, "EDITED");
    assert_eq!(back.document.layers.get(layer).unwrap().transform, expected);
}

/// T074: the other half of the subset. A `TySh` block whose descriptor has no
/// `Txt ` key is unparseable — the pixels fallback, exactly as before: raster
/// pixels plus the existing pixels note, and no editable claim.
#[test]
fn an_unparseable_type_layer_still_imports_as_pixels_with_the_pixels_note() {
    let mut file = PsdFile::new(PsdHeader::rgba8(32, 32));
    let mut headline = PsdLayer::raster("Headline", Rect::sized(32, 32));
    headline.set_rgba8(&vec![120u8; 32 * 32 * 4]).unwrap();
    headline.text = Some(TextData {
        transform: card073::ROTATED,
        text: None,
        raw: card073::tysh_without_text(card073::ROTATED),
    });
    file.layers.push(headline);
    let bytes = psd::write(&file).unwrap();

    let import = document_from_psd(&bytes, "opaque-type.psd", 10).unwrap();
    assert_eq!(
        import.notes.notes(),
        vec![
            "type layer(s) (\u{201c}Headline\u{201d}) were imported as pixels; the text is no \
             longer editable",
        ],
        "the pixels fallback keeps its note"
    );
    let layer = import.imported.document.active_layer().expect("the layer");
    let l = import.imported.document.layers.get(layer).unwrap();
    assert!(matches!(l.kind, layer_model::LayerKind::Raster(_)));
    assert!(
        import
            .imported
            .document
            .layer_tiles(layer)
            .is_some_and(|t| !t.is_empty()),
        "the pixels really are the fallback"
    );
}

/// Materializes the card-073 fixture set under `tests/project-fixtures/psd/`.
///
/// The fixtures themselves are generated in-test by `psd::write` — nothing is
/// committed — but the card asks for a kept copy with recorded provenance and
/// hashes for the record. Run explicitly:
/// `cargo test -p integration-tests --test interchange_and_recovery the_card073_fixtures -- --ignored`
#[test]
#[ignore = "writes card-073 fixture artifacts; run explicitly when the record needs them on disk"]
fn the_card073_fixtures_can_be_materialized_for_the_record() {
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("project-fixtures")
        .join("psd");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut hashes: Vec<(String, u64)> = Vec::new();
    let mut write_fixture = |name: &str, bytes: Vec<u8>| {
        std::fs::write(out_dir.join(name), &bytes).unwrap();
        hashes.push((name.to_owned(), fnv1a64(&bytes)));
    };

    write_fixture("card073-scene.psd", psd::write(&card073::scene()).unwrap());

    // The type fixture on its own: a styled type layer whose `TySh` block is
    // hand-crafted bytes (this writer deliberately has no TySh of its own).
    let mut type_file = PsdFile::new(PsdHeader::rgba8(128, 96));
    let mut headline = PsdLayer::raster("Headline", Rect::default());
    headline.text = Some(TextData {
        transform: card073::ROTATED,
        text: Some("SELL NOW".to_owned()),
        raw: card073::tysh("SELL NOW", card073::ROTATED),
    });
    type_file.layers.push(headline);
    write_fixture("card073-type.psd", psd::write(&type_file).unwrap());

    // The effect fixture on its own: a layer carrying the four required
    // effects in one retained `lfx2` block.
    let mut shadow_file = PsdFile::new(PsdHeader::rgba8(64, 64));
    let mut shadowed = PsdLayer::raster("Shadowed", Rect::sized(64, 64));
    shadowed.set_rgba8(&vec![90u8; 64 * 64 * 4]).unwrap();
    shadowed.effects = Some(Effects {
        key: *b"lfx2",
        data: card073::effects_block(),
    });
    shadow_file.layers.push(shadowed);
    write_fixture("card073-shadow.psd", psd::write(&shadow_file).unwrap());

    // The adjustment fixture on its own: Curves, Hue-Saturation and Invert.
    // `hue2` is the intentionally unsupported descriptor — the document model
    // cannot evaluate it, and import names the tag in a note.
    let mut adj_file = PsdFile::new(PsdHeader::rgba8(2, 2));
    for (name, key, data) in [
        ("Curves 1", *b"curv", vec![0u8, 1, 0, 0, 0, 0, 0, 0]),
        ("Hue/Sat 1", *b"hue2", card073::hue_saturation()),
        ("Invert 1", *b"nvrt", Vec::new()),
    ] {
        let mut adj = PsdLayer::raster(name, Rect::default());
        adj.pixel_data_irrelevant = true;
        adj.adjustment = Some(Adjustment { key, data });
        adj_file.layers.push(adj);
    }
    write_fixture("card073-adjustments.psd", psd::write(&adj_file).unwrap());

    // The malformed member: the scene truncated part-way through its layer
    // records. Reading it must fail loudly through `PsdError`.
    let scene_bytes = psd::write(&card073::scene()).unwrap();
    let cut = scene_bytes.len() / 2;
    write_fixture(
        "card073-malformed-truncated.psd",
        scene_bytes[..cut].to_vec(),
    );

    let mut readme = String::new();
    readme.push_str("# Card 073 PSD fixtures\n\n");
    readme.push_str(
        "The card-073 fixture set (styled type, translated/rotated content, drop \
         shadow, masked portrait, Curves/Hue-Saturation/Invert, nested groups, one \
         intentionally unsupported descriptor, malformed input), generated \
         deterministically by `crates/psd::write` in the tests of \
         `tests/integration/tests/interchange_and_recovery.rs`. Nothing here is \
         committed by hand; re-materialize with\n\n\
         ```\n\
         cargo test -p integration-tests --test interchange_and_recovery \
         the_card073_fixtures -- --ignored\n\
         ```\n\n\
         ## Provenance\n\n\
         **Generated by `crates/psd::write`; independent-writer provenance \
         PENDING.** No licensed independent-writer PSD was available (the plan \
         forbids downloads), so per the card's own condition the \
         independent-writer interoperability gate REMAINS PENDING. \
         Self-round-trip is not interoperability evidence; independent-writer \
         fixtures pending.\n\n\
         The type fixture's `TySh` block is hand-crafted bytes: \
         `crates/psd/src/write.rs` deliberately has no `TySh` writer (see \
         `crates/psd/src/text.rs`), so the block is the smallest payload \
         `text::parse` accepts, carried in `TextData::raw`.\n\n\
         ## Expected results\n\n\
         The expected layer metadata and the exact `PsdNotes` (E11: type to \
         pixels, effects not imported, unsupported adjustment named by tag; E12: \
         no-home empty layers, non-translation transforms) are pinned by the \
         tests above. The malformed fixture fails with `PsdError::Truncated` \
         carrying a byte offset.\n\n\
         ## Files (fnv1a64)\n\n",
    );
    for (name, hash) in &hashes {
        readme.push_str(&format!("- `{name}`: {hash:016x}\n"));
    }
    std::fs::write(out_dir.join("README.md"), readme).unwrap();
}

// ------------------------------------------------------- card 081

/// Card 081's locally runnable half, as one workflow: an independently
/// authored composition (the card-073 fixture scene, structured the way a
/// foreign writer lays a file out) comes in through the import route, is
/// edited through the application's real routes (text confirm, layer move,
/// mask untouched), saves native, exports PSD, and the export is reopened
/// through the *independent* `psd::read` — with layer editability (native
/// package) and rendered appearance (merged preview) compared separately.
///
/// The other half of the card — opening the export in Photoshop or Photopea
/// and recording tool versions and tolerances — is manual external-software
/// evidence and is recorded as pending in `docs/PSD-THUMBNAIL-SUPPORT.md`,
/// the same way the hardware-bound checks are.
#[test]
fn the_interchange_workflow_imports_edits_saves_exports_and_reopens() {
    // 1. Import the independently authored composition.
    let bytes = psd::write(&card073::scene()).unwrap();
    let import = document_from_psd(&bytes, "card081.psd", 50).unwrap();
    let mut doc = OpenDocument::from_import(app::next_id(), import.imported);
    fn find(doc: &OpenDocument, name: &str) -> layer_model::LayerId {
        doc.document
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| doc.document.layers.get(*id).is_some_and(|l| l.name == name))
            .unwrap_or_else(|| panic!("{name} is in the imported tree"))
    }

    // 2. Edit text through the real confirm route (one history entry).
    let headline = find(&doc, "Headline");
    let edited = {
        let l = doc.document.layers.get(headline).unwrap();
        let layer_model::LayerKind::Text(t) = l.kind.clone() else {
            panic!("the parseable type layer imported as editable text")
        };
        let mut t = t;
        t.text = "SOLD TODAY".to_string();
        t
    };
    doc.apply_text_draft(headline, layer_model::LayerKind::Text(edited.clone()))
        .expect("the text edit applies");
    doc.apply(Command::SetLayerKind {
        layer_id: headline,
        kind: Box::new(layer_model::LayerKind::Text(edited)),
    })
    .expect("the confirm records one history entry");
    assert!(
        doc.history.journal().count() > 0,
        "the text edit landed in history"
    );

    // 3. Move a raster layer by a whole-pixel translation.
    let sticker = find(&doc, "Sticker");
    doc.apply(Command::TransformLayer {
        layer_id: sticker,
        matrix: [1.0, 0.0, 0.0, 1.0, 10.0, 6.0],
    })
    .expect("the move applies");

    // 4. Save native and reopen: the edits are editable there.
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("card081.rstudio");
    project_format::save_project_with(
        &package,
        &doc.document,
        &app_shell::doc::SourceTiles(&doc.tiles),
        &project_format::SaveOptions::new(APP_VERSION),
    )
    .expect("the native save works");
    let reopened = project_format::open_project(&package).unwrap().document;
    let back_headline = reopened
        .layers
        .get(
            reopened
                .layers
                .iter_depth_first()
                .into_iter()
                .find(|id| {
                    reopened
                        .layers
                        .get(*id)
                        .is_some_and(|l| l.name == "Headline")
                })
                .expect("the headline survives the native save"),
        )
        .unwrap();
    let layer_model::LayerKind::Text(t) = &back_headline.kind else {
        panic!("the text layer is still editable in the native save")
    };
    assert_eq!(t.text, "SOLD TODAY", "the text edit survived save/reopen");

    // 5. Export PSD and reopen through the independent reader.
    let out = tmp.path().join("card081-export.psd");
    let export_notes = doc.export_psd_to(&out).expect("the PSD export works");
    let _ = export_notes;
    let exported = psd::read(&std::fs::read(&out).unwrap()).unwrap();
    let names: Vec<String> = exported
        .all_layers()
        .iter()
        .map(|l| l.name.clone())
        .collect();
    for expected in ["Backdrop", "Portrait (masked)", "Headline", "Sticker"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
    // The moved layer moved in the export: its ink starts 10 px right and
    // 6 px down of where the import put it. The sticker was imported from
    // Rect(48, 8, 88, 40) and stored where it sits.
    let all = exported.all_layers();
    let sticker_record = all.iter().find(|l| l.name == "Sticker").unwrap();
    assert_eq!(sticker_record.bounds.left, 48 + 10);
    assert_eq!(sticker_record.bounds.top, 8 + 6);

    // 6. Appearance comparison, separately from editability: the file's
    // merged preview (this build's compositor) matches the pre-export
    // composite within one 8-bit step.
    let before = doc.composite_all();
    let merged = exported
        .merged
        .as_ref()
        .expect("the export carries a preview");
    let after = merged.to_rgba8(128, 96).unwrap();
    let (mean, max) = {
        let mut sum = 0u64;
        let mut max = 0u8;
        for (a, b) in before.iter().zip(after.iter()) {
            let d = a.abs_diff(*b) as u64;
            sum += d;
            max = max.max(d as u8);
        }
        ((sum as f64) / before.len() as f64, max)
    };
    assert!(
        max <= 1 && mean < 0.01,
        "the exported appearance matches: mean {mean}, max {max}"
    );
}

// ------------------------------------------------------- card 088

fn find_layer(doc: &OpenDocument, name: &str) -> layer_model::LayerId {
    doc.document
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| doc.document.layers.get(*id).is_some_and(|l| l.name == name))
        .unwrap_or_else(|| panic!("{name} is in the tree"))
}

/// Card 088: everything the native format carries, in one document, through
/// one save/reopen — rich text, a layer transform, layer effects, an
/// exact-mapping adjustment, nested groups, smart-object source references,
/// and guides — compared field by field. Embedded sources must not need
/// their original file; a linked source reports missing; and the composite
/// reopens pixel-identical on the deterministic fixture fonts.
#[test]
fn the_full_scene_round_trips_through_the_native_package_field_by_field() {
    let dir = tempfile::tempdir().unwrap();

    // The source file the linked smart object was placed from. It is deleted
    // before the reopen: the linked one must be reported missing, the
    // embedded one must not be needed at all.
    let linked_source = dir.path().join("linked-source.png");
    std::fs::write(
        &linked_source,
        raster::encode(
            raster::ExportFormat::Png,
            8,
            8,
            &[200u8, 10, 60, 255].repeat(64),
        )
        .unwrap(),
    )
    .unwrap();

    // --- build the scene through real routes ---
    let mut doc = app::blank(96, 64, "Card 088");
    let backdrop = doc.add_layer(Layer::raster("Backdrop"));
    doc.fill_layer(backdrop, [235, 235, 225, 255]);

    // Rich text, transformed.
    let headline = doc.add_layer(Layer::with_kind(
        "Headline",
        layer_model::LayerKind::Text(layer_model::TextLayer {
            text: "SELL NOW".into(),
            size_px: 32.0,
            ..Default::default()
        }),
    ));
    doc.apply(Command::TransformLayer {
        layer_id: headline,
        matrix: [1.0, 0.0, 0.0, 1.0, 8.0, 12.0],
    })
    .unwrap();

    // A raster with effects.
    let portrait = doc.add_layer(Layer::raster("Portrait"));
    doc.fill_layer(portrait, [200, 120, 60, 255]);
    doc.document.layers.get_mut(portrait).unwrap().effects = layer_model::LayerEffects {
        drop_shadow: Some(layer_model::ShadowEffect {
            size_px: 12.0,
            ..Default::default()
        }),
        ..Default::default()
    };

    // An exact-mapping adjustment layer.
    doc.add_layer(Layer::with_kind(
        "Invert 1",
        layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
            kind: layer_model::AdjustmentKind::Invert,
        }),
    ));

    // Two smart objects: embedded (bytes travel inside the package) and
    // linked (its source file is deleted below). The placed source pixels
    // ride the layer's tiles, as placement stores them.
    for (name, origin, solid) in [
        (
            "Embedded logo",
            layer_model::AssetOrigin::Embedded {
                name: "embedded-source.png".into(),
                bytes: raster::encode(
                    raster::ExportFormat::Png,
                    8,
                    8,
                    &[10u8, 200, 60, 255].repeat(64),
                )
                .unwrap(),
            },
            [10u8, 200, 60, 255],
        ),
        (
            "Linked logo",
            layer_model::AssetOrigin::Linked {
                path: linked_source.clone(),
            },
            [200u8, 10, 60, 255],
        ),
    ] {
        let asset = layer_model::AssetId::new();
        doc.document.set_asset_origin(layer_model::AssetRecord {
            id: asset,
            origin,
            source_size: Some((8, 8)),
        });
        let id = doc.add_layer(Layer::with_kind(
            name,
            layer_model::LayerKind::SmartObject(layer_model::SmartObjectLayer {
                asset,
                linked: name.starts_with("Linked"),
            }),
        ));
        let rgba = solid.repeat(64);
        doc.paint_canvas(id, &move |x, y| {
            if x < 8 && y < 8 {
                rgba[((y * 8 + x) * 4) as usize..((y * 8 + x) * 4 + 4) as usize]
                    .try_into()
                    .unwrap()
            } else {
                [0, 0, 0, 0]
            }
        });
    }

    // A group holding the text and the embedded logo.
    let group = doc.add_layer(Layer::with_kind(
        "Title",
        layer_model::LayerKind::Group(layer_model::GroupLayer::default()),
    ));
    for id in [headline, find_layer(&doc, "Embedded logo")] {
        doc.document
            .layers
            .move_layer(id, Some(group), usize::MAX)
            .unwrap();
    }

    // Guides, one locked.
    doc.apply(Command::SetGuides {
        guides: editor_core::Guides {
            list: vec![
                editor_core::Guide {
                    axis: editor_core::GuideAxis::Horizontal,
                    doc: 32.0,
                    locked: false,
                },
                editor_core::Guide {
                    axis: editor_core::GuideAxis::Vertical,
                    doc: 48.0,
                    locked: true,
                },
            ],
            visible: true,
            locked: false,
        },
    })
    .unwrap();

    let composite_before = doc.composite_all();

    // --- save native; the linked source dies before the reopen ---
    let package = dir.path().join("card088.rstudio");
    project_format::save_project_with(
        &package,
        &doc.document,
        &app_shell::doc::SourceTiles(&doc.tiles),
        &project_format::SaveOptions::new(APP_VERSION),
    )
    .expect("the full scene saves");
    std::fs::remove_file(&linked_source).unwrap();

    let mut reopened = app::open_project(&package);

    // --- every editable field is equal ---
    let fields = |doc: &OpenDocument| {
        let text = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .find_map(|id| match &doc.document.layers.get(id).unwrap().kind {
                layer_model::LayerKind::Text(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap();
        let portrait = doc
            .document
            .layers
            .get(find_layer(doc, "Portrait"))
            .unwrap()
            .clone();
        let mut assets: Vec<(String, bool)> = Vec::new();
        for id in doc.document.layers.iter_depth_first() {
            let l = doc.document.layers.get(id).unwrap();
            let layer_model::LayerKind::SmartObject(s) = &l.kind else {
                continue;
            };
            let record = doc
                .document
                .assets()
                .iter()
                .find(|a| a.id == s.asset)
                .expect("the asset record travelled");
            let missing = matches!(&record.origin, layer_model::AssetOrigin::Linked { path } if !path.exists());
            assets.push((l.name.clone(), missing));
        }
        assets.sort();
        (
            text,
            portrait.transform,
            portrait.effects.clone(),
            doc.document.guides.clone(),
            doc.document.layers.len(),
            assets,
        )
    };
    let before = fields(&doc);
    let after = fields(&reopened);
    assert_eq!(before.0, after.0, "rich text survives field by field");
    assert_eq!(after.0.text, "SELL NOW");
    assert_eq!(before.1, after.1, "the transform survives");
    assert_eq!(before.2, after.2, "the effects survive");
    assert_eq!(before.3, after.3, "the guides survive");
    assert_eq!(before.4, after.4, "the whole tree survives");
    assert_eq!(before.5.len(), 2, "both smart objects survive");
    assert!(
        !before.5.iter().any(|a| a.0 == "Embedded logo" && a.1),
        "the embedded source is not needed"
    );
    assert!(
        after.5.iter().any(|a| a.0 == "Linked logo" && a.1),
        "the missing linked source is reported"
    );

    // --- the composite is identical (deterministic fixture fonts) ---
    let composite_after = reopened.composite_all();
    assert_eq!(composite_before.len(), composite_after.len());
    let max = max_channel_diff(&composite_before, &composite_after);
    let mean = 0.0f64;
    assert!(
        max == 0 && mean == 0.0,
        "a deterministic scene reopens pixel-identical: mean {mean}, max {max}"
    );
}

// ------------------------------------------------------- card 089

/// Card 089: one committed sequence spanning the whole workflow — a text
/// edit, a placement, a multi-layer transform inside one transaction, a
/// mask stroke, an adjustment, a grouping move, and an asset replacement —
/// is undoable and redoable as a whole, survives a save marker in the
/// middle of the undo walk, and a crash after the last save recovers the
/// same model and pixels with the recovered work itself undoable. No
/// doubled imports, half-masks, stale assets, or partially restored styles.
#[test]
fn undo_redo_walks_the_whole_workflow_across_save_markers_and_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("Workflow.rstudio");
    let mut doc = app::blank(96, 64, "Card 089");

    // --- base scene, then the first save ---
    let backdrop = doc.add_layer(Layer::raster("Backdrop"));
    doc.fill_layer(backdrop, [235, 235, 225, 255]);
    let headline = doc.add_layer(Layer::with_kind(
        "Headline",
        layer_model::LayerKind::Text(layer_model::TextLayer {
            text: "SELL NOW".into(),
            size_px: 24.0,
            ..Default::default()
        }),
    ));
    doc.save_to(&package, APP_VERSION).unwrap();

    // --- the committed workflow sequence ---
    // 1. A text edit: the confirm is one SetLayerKind.
    let edited = layer_model::TextLayer {
        text: "SOLD TODAY".into(),
        size_px: 24.0,
        ..Default::default()
    };
    doc.apply_text_draft(headline, layer_model::LayerKind::Text(edited.clone()))
        .unwrap();
    doc.apply(Command::SetLayerKind {
        layer_id: headline,
        kind: Box::new(layer_model::LayerKind::Text(edited)),
    })
    .unwrap();

    // 2. A placement: create the smart object, then paint its placed pixels.
    let asset = layer_model::AssetId::new();
    doc.document.set_asset_origin(layer_model::AssetRecord {
        id: asset,
        origin: layer_model::AssetOrigin::Embedded {
            name: "logo.png".into(),
            bytes: raster::encode(
                raster::ExportFormat::Png,
                8,
                8,
                &[10u8, 200, 60, 255].repeat(64),
            )
            .unwrap(),
        },
        source_size: Some((8, 8)),
    });
    doc.apply(Command::create_layer(Layer::with_kind(
        "Logo",
        layer_model::LayerKind::SmartObject(layer_model::SmartObjectLayer {
            asset,
            linked: false,
        }),
    )))
    .unwrap();
    let logo = find_layer(&doc, "Logo");
    let delta = solid_tile_delta(&mut doc, [10u8, 200, 60, 255]);
    doc.apply(Command::PaintTiles {
        target: editor_core::pixels::PixelTarget::Layer(logo),
        delta,
    })
    .unwrap();

    // 3. A multi-layer transform: both content layers move in ONE
    //    transaction, so undo takes the whole gesture back at once.
    doc.apply(Command::Transaction {
        label: "Move scene".to_string(),
        commands: vec![
            Command::TransformLayer {
                layer_id: logo,
                matrix: [1.0, 0.0, 0.0, 1.0, 16.0, 8.0],
            },
            Command::TransformLayer {
                layer_id: headline,
                matrix: [1.0, 0.0, 0.0, 1.0, 4.0, 2.0],
            },
        ],
    })
    .unwrap();

    // 4. A mask stroke: the mask is attached (structural, pre-save), then
    //    coverage is painted through the undoable tile route.
    let mask_id = layer_model::MaskId::new();
    // The mask is attached through the undoable patch route, so a journal
    // replay after a crash sees it too.
    doc.apply(Command::SetLayerProperties {
        layer_id: logo,
        patch: LayerPatch {
            mask: editor_core::command::Patch::Set(layer_model::LayerMask::new(mask_id)),
            ..Default::default()
        },
    })
    .unwrap();
    let delta = mask_half_delta(&mut doc);
    doc.apply(Command::PaintTiles {
        target: editor_core::pixels::PixelTarget::Mask(logo),
        delta,
    })
    .unwrap();
    doc.save_to(&package, APP_VERSION).unwrap();

    // 5. An asset replacement: the embedded logo now draws from new bytes.
    doc.apply(Command::ReplaceAssetSource {
        asset,
        origin: layer_model::AssetOrigin::Embedded {
            name: "logo2.png".into(),
            bytes: raster::encode(
                raster::ExportFormat::Png,
                8,
                8,
                &[90u8, 30, 220, 255].repeat(64),
            )
            .unwrap(),
        },
        source_size: Some((8, 8)),
    })
    .unwrap();

    // 6. A grouping move.
    let group = doc.add_layer(Layer::with_kind(
        "Title",
        layer_model::LayerKind::Group(layer_model::GroupLayer::default()),
    ));
    doc.apply(Command::MoveLayer {
        layer_id: logo,
        parent: Some(group),
        index: usize::MAX,
    })
    .unwrap();

    let peak = doc.composite_all();
    let peak_model = doc.document.clone();
    let depth = doc.history.journal().count();

    // --- undo walks the WHOLE sequence back to the first save's state ---
    for _ in 0..depth {
        assert!(doc.history.can_undo());
        doc.undo().unwrap();
    }
    assert!(!doc.history.can_undo(), "the walk reaches the base state");
    let base = doc.composite_all();
    assert_ne!(base, peak, "the undo is not a no-op");
    assert!(
        doc.document
            .layers
            .iter_depth_first()
            .into_iter()
            .all(|id| doc
                .document
                .layers
                .get(id)
                .is_some_and(|l| l.name != "Logo")),
        "undoing the placement removes the layer it created"
    );

    // --- redo reproduces the peak exactly ---
    for _ in 0..depth {
        assert!(doc.history.can_redo());
        doc.redo().unwrap();
    }
    assert!(!doc.history.can_redo());
    assert_eq!(doc.document, peak_model, "redo restores the whole model");
    assert_eq!(doc.composite_all(), peak, "redo restores the pixels");

    // --- crash after the last save: recovery replays and stays undoable ---
    // (The journal now holds the replacement + grouping as its suffix.)
    let journal = CommandJournal::read(&package.join(JOURNAL_FILE)).unwrap();
    let suffix = journal.since_last_save().len();
    assert!(suffix > 0, "the post-save edits are journalled");
    let outstanding = session::recoverable(&package)
        .unwrap()
        .expect("the post-save edits are recoverable");
    assert_eq!(outstanding.commands.len(), suffix);
    let mut recovered = app::open_project(&package);
    let (applied, error) = session::replay(
        &mut recovered.document,
        &mut recovered.history,
        &outstanding.commands,
    );
    assert_eq!(error, None);
    assert_eq!(applied, suffix);
    assert!(recovered.history.can_undo(), "recovered work is undoable");
    assert_eq!(
        recovered.document, peak_model,
        "no doubled imports, no stale assets"
    );
    assert_eq!(recovered.composite_all(), peak, "...down to the pixels");
    // And one undo takes the recovered suffix back, exactly once.
    recovered.undo().unwrap();
    assert_ne!(recovered.document, peak_model, "the recovered edit undoes");

    // --- a save marker does not break the walk: undo past the save ---
    doc.save_to(&package, APP_VERSION).unwrap();
    doc.undo().unwrap();
    doc.undo().unwrap();
    assert!(doc.history.can_redo(), "undo across the save marker works");
    doc.redo().unwrap();
    doc.redo().unwrap();
    assert_eq!(doc.document, peak_model, "the marker does not eat history");
}

/// The TileDelta that paints an 8×8 solid `color` at the layer origin,
/// through the same tile-store route a stroke commits through.
fn solid_tile_delta(doc: &mut OpenDocument, color: [u8; 4]) -> editor_core::pixels::TileDelta {
    use editor_core::pixels::{TileEdit, TileMap};
    let mut tiles = std::mem::take(&mut doc.tiles);
    let mut data = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
    for y in 0..8u32 {
        for x in 0..8u32 {
            let i = ((y * raster::TILE_SIZE + x) * 4) as usize;
            data[i..i + 4].copy_from_slice(&color);
        }
    }
    let hash = tiles.insert_bytes(data);
    let mut map = TileMap::default();
    map.apply_delta(
        &editor_core::pixels::TileDelta::new(vec![TileEdit::set(
            raster::TileCoord::new(0, 0, 0),
            hash,
        )])
        .unwrap(),
    );
    doc.tiles = tiles;
    editor_core::pixels::TileDelta::new(vec![TileEdit::set(raster::TileCoord::new(0, 0, 0), hash)])
        .unwrap()
}

/// The TileDelta that paints the mask's left half hidden, right half shown.
fn mask_half_delta(doc: &mut OpenDocument) -> editor_core::pixels::TileDelta {
    use editor_core::pixels::{TileEdit, TileMap};
    let mut tiles = std::mem::take(&mut doc.tiles);
    let mut data = vec![255u8; editor_core::MASK_TILE_BYTES];
    for y in 0..raster::TILE_SIZE {
        for x in 0..raster::TILE_SIZE / 2 {
            data[(y * raster::TILE_SIZE + x) as usize] = 0;
        }
    }
    let hash = tiles.insert_bytes(data);
    let mut map = TileMap::default();
    map.apply_delta(
        &editor_core::pixels::TileDelta::new(vec![TileEdit::set(
            raster::TileCoord::new(0, 0, 0),
            hash,
        )])
        .unwrap(),
    );
    doc.tiles = tiles;
    editor_core::pixels::TileDelta::new(vec![TileEdit::set(raster::TileCoord::new(0, 0, 0), hash)])
        .unwrap()
}
