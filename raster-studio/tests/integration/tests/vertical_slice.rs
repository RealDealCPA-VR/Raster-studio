//! Phase-0 vertical-slice integration test.
//!
//! Proves the non-negotiable "save, close, reopen, and continue editing without
//! corruption" property across real crate boundaries:
//!   1. Build a layered document via commands + history.
//!   2. Journal the accepted commands.
//!   3. Save the `.rstudio` package and load it back.
//!   4. Recover unsaved work by replaying the journal onto a fresh document.
//!   5. Undo/redo round-trips leave state consistent.

use editor_core::{Command, Document, History, LayerPatch};
use layer_model::Layer;
use project_format::{load_project, save_project, CommandJournal};

#[test]
fn build_save_load_replay_undo() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("Slice.rstudio");
    let journal = tmp.path().join("session.journal");

    // 1. Build a document through history (as the app would).
    let mut doc = Document::new(2048, 1536, "Slice");
    let mut hist = History::with_limit(0);

    let bg = Layer::raster("Background");
    let bg_id = bg.id;
    let create_bg = Command::CreateLayer { layer: bg };
    hist.apply(&mut doc, create_bg.clone()).unwrap();
    CommandJournal::append(&journal, &create_bg).unwrap();

    let set_props = Command::SetLayerProperties {
        layer_id: bg_id,
        patch: LayerPatch {
            opacity: Some(0.5),
            ..Default::default()
        },
    };
    hist.apply(&mut doc, set_props.clone()).unwrap();
    CommandJournal::append(&journal, &set_props).unwrap();

    assert_eq!(doc.layers.len(), 1);
    assert_eq!(doc.layers.get(bg_id).unwrap().opacity, 0.5);

    // 2. Save + load the package; state must survive a round-trip.
    save_project(&pkg, &doc).unwrap();
    let loaded = load_project(&pkg).unwrap();
    assert_eq!(loaded.meta.size, doc.meta.size);
    assert_eq!(loaded.layers.len(), 1);
    assert_eq!(loaded.layers.get(bg_id).unwrap().opacity, 0.5);

    // 3. Crash-recovery: replay the journal onto a fresh doc == same state.
    let mut recovered = Document::new(2048, 1536, "Slice");
    for cmd in CommandJournal::read_all(&journal).unwrap() {
        cmd.apply(&mut recovered).unwrap();
    }
    assert_eq!(recovered.layers.get(bg_id).unwrap().opacity, 0.5);

    // 4. Undo/redo consistency on the live history.
    assert!(hist.undo(&mut doc).unwrap()); // undo opacity change
    assert_eq!(doc.layers.get(bg_id).unwrap().opacity, 1.0);
    assert!(hist.redo(&mut doc).unwrap()); // redo it
    assert_eq!(doc.layers.get(bg_id).unwrap().opacity, 0.5);
}

#[test]
fn transaction_import_is_atomic_and_undoable() {
    let mut doc = Document::new(512, 512, "Import");
    let mut hist = History::with_limit(0);

    // Simulate an "import" that adds several layers atomically.
    let l1 = Layer::raster("Imported A");
    let l2 = Layer::raster("Imported B");
    let g = Layer::group("Imported Group");
    let tx = Command::Transaction {
        label: "Import Assets".into(),
        commands: vec![
            Command::CreateLayer { layer: g },
            Command::CreateLayer { layer: l1 },
            Command::CreateLayer { layer: l2 },
        ],
    };
    hist.apply(&mut doc, tx).unwrap();
    assert_eq!(doc.layers.len(), 3);
    assert_eq!(hist.undo_label(), Some("Import Assets"));

    // A single undo reverses the whole transaction.
    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.layers.len(), 0);
}
