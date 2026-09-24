//! W10-E: File ▸ Automate ▸ Batch… and File ▸ Automate ▸ Convert Formats…,
//! run on the job worker.
//!
//! # What a batch does
//!
//! Every image file directly inside the source folder (the extensions
//! File ▸ Open reads, plus `.psd`; sub-folders are not entered), in name
//! order, is opened as a document of its own — the same
//! [`OpenDocument::open_image`] File ▸ Open uses —, for **Batch** has the
//! chosen Action's recorded steps replayed on it (the Actions panel's replay:
//! each step retargeted to the layer at the same stack position, its tile
//! bytes re-inserted into this document's store), is flattened through the
//! compositor and is written into the destination folder by
//! `raster::export` in the chosen format, JPEG quality and scale. A file that
//! cannot be opened, replayed or written is skipped and listed, with its
//! reason, in `batch-errors.txt` in the destination — Photoshop's "Log
//! Errors to File".
//!
//! An Action replays what it recorded: parametric steps (a new adjustment or
//! fill layer, a layer property, a new layer) apply to each file as
//! themselves, while a pixel step replays the *pixels* it recorded — the
//! Actions panel records tile deltas, not filter parameters. So a batch of
//! "New Adjustment Layer ▸ Invert" inverts every file, while a recorded
//! brush stroke lands the same stroke on every file.
//!
//! # The worker
//!
//! The file list, the Action's steps and the settings are handed to the
//! editor's [`crate::jobs::Spawner`] (a worker thread in the desktop build,
//! inline in the unit tests). The worker opens, replays, composites and
//! encodes; it reports each finished file on a channel, and [`poll`] —
//! called from `menu_bridge::context` once a frame — puts "Batch: 2 of 5 …"
//! on the status line and the summary when it ends. Nothing the worker does
//! touches an open document.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use editor_core::Command;
use ui::dialogs::{BatchMode, BatchSpec};

use crate::doc::{DocumentId, OpenDocument};
use crate::editor::{Editor, RecordedEdit};

/// The error log a batch leaves in its destination when a file failed.
pub const ERROR_LOG: &str = "batch-errors.txt";

/// What a finished batch did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatchReport {
    pub mode_label: String,
    /// Every file written, in input order.
    pub written: Vec<PathBuf>,
    /// Every input that failed, with why.
    pub failed: Vec<(PathBuf, String)>,
    /// The error log, when one was written.
    pub log: Option<PathBuf>,
}

impl BatchReport {
    /// The status-line sentence.
    pub fn summary(&self) -> String {
        let mut s = format!("{}: wrote {} file(s)", self.mode_label, self.written.len());
        if !self.failed.is_empty() {
            s.push_str(&format!(", {} failed", self.failed.len()));
            if let Some(log) = &self.log {
                s.push_str(&format!(" (see {})", log.display()));
            }
        }
        s
    }
}

/// One message from the worker.
enum Progress {
    File {
        done: usize,
        total: usize,
        name: String,
    },
    Finished(BatchReport),
}

struct Pending {
    rx: Receiver<Progress>,
    label: &'static str,
}

thread_local! {
    static JOBS: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
}

/// Whether a batch is still running on this thread's editor.
pub fn pending() -> bool {
    JOBS.with(|jobs| !jobs.borrow().is_empty())
}

/// The image files a batch reads from `dir`, in name order.
pub fn batch_inputs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .is_some_and(|e| {
                    crate::dialogs::IMAGE_EXTENSIONS.contains(&e.as_str()) || e == "psd"
                })
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Start the batch `spec` asks for. Refused here, before any worker, when
/// the source has no images, the destination cannot be made, the Action is
/// gone or a batch is already running. With the inline spawner the batch has
/// finished by the time this returns and the answer is its summary.
pub fn start(editor: &mut Editor, spec: BatchSpec) -> Result<String, String> {
    let label = match spec.mode {
        BatchMode::Batch => "Batch",
        BatchMode::ConvertFormats => "Convert Formats",
    };
    if pending() {
        return Err(format!("{label}: a batch is still running"));
    }
    let inputs = batch_inputs(&spec.source).map_err(|e| format!("{label}: {e}"))?;
    if inputs.is_empty() {
        return Err(format!(
            "{label}: {} holds no image this editor opens",
            spec.source.display()
        ));
    }
    std::fs::create_dir_all(&spec.destination)
        .map_err(|e| format!("{label}: cannot create {}: {e}", spec.destination.display()))?;
    let edits: Option<Vec<RecordedEdit>> = match spec.mode {
        BatchMode::Batch => {
            let index = spec.action.ok_or(format!("{label}: choose an Action"))?;
            let action = editor
                .actions()
                .get(index)
                .ok_or(format!("{label}: that Action is no longer in the library"))?;
            Some(action.edits.clone())
        }
        BatchMode::ConvertFormats => None,
    };
    let (tx, rx) = channel();
    let total = inputs.len();
    let worker_tx = tx.clone();
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        let report = run(&inputs, &spec, edits.as_deref(), label, |done, name| {
            let _ = worker_tx.send(Progress::File {
                done,
                total,
                name: name.to_string(),
            });
        });
        let _ = worker_tx.send(Progress::Finished(report));
    });
    if let Err(e) = (editor.spawner())(format!("batch:{label}"), body) {
        return Err(format!("{label}: could not start the worker: {e}"));
    }
    drop(tx);
    JOBS.with(|jobs| jobs.borrow_mut().push(Pending { rx, label }));
    editor.set_status(format!("{label}: 0 of {total} files"));
    // Inline spawner: already finished — land it now.
    Ok(poll(editor).unwrap_or_else(|| format!("{label}: 0 of {total} files")))
}

/// Apply every message the running batches sent. Once a frame. Returns the
/// summary of a batch that finished during this call.
pub fn poll(editor: &mut Editor) -> Option<String> {
    let mut finished = None;
    let mut status = None;
    JOBS.with(|jobs| {
        let mut jobs = jobs.borrow_mut();
        jobs.retain(|job| loop {
            match job.rx.try_recv() {
                Ok(Progress::File { done, total, name }) => {
                    status = Some(format!("{}: {done} of {total} files ({name})", job.label));
                }
                Ok(Progress::Finished(report)) => {
                    finished = Some(report.summary());
                    return false;
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => {
                    finished = Some(format!(
                        "{}: the worker stopped without a report",
                        job.label
                    ));
                    return false;
                }
            }
        });
    });
    if let Some(message) = finished.clone().or(status) {
        editor.set_status(message);
    }
    finished
}

/// The worker's body: every input in order, then the error log.
fn run(
    inputs: &[PathBuf],
    spec: &BatchSpec,
    edits: Option<&[RecordedEdit]>,
    label: &str,
    mut progress: impl FnMut(usize, &str),
) -> BatchReport {
    let mut report = BatchReport {
        mode_label: label.to_string(),
        ..BatchReport::default()
    };
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    for (i, input) in inputs.iter().enumerate() {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".to_string());
        let stem = raster::sanitize_file_stem(&stem);
        let count = used.entry(stem.to_ascii_lowercase()).or_insert(0);
        *count += 1;
        let name = if *count == 1 {
            stem
        } else {
            format!("{stem}_{count}")
        };
        match process(input, spec, edits, &name) {
            Ok(path) => report.written.push(path),
            Err(e) => report.failed.push((input.clone(), e)),
        }
        progress(
            i + 1,
            &input
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }
    if !report.failed.is_empty() {
        let mut log = format!("{label} errors\n");
        for (path, reason) in &report.failed {
            log.push_str(&format!("{}: {reason}\n", path.display()));
        }
        let path = spec.destination.join(ERROR_LOG);
        if crate::doc::write_atomically(&path, log.as_bytes()).is_ok() {
            report.log = Some(path);
        }
    }
    report
}

/// Open, replay, flatten and write one file.
fn process(
    input: &Path,
    spec: &BatchSpec,
    edits: Option<&[RecordedEdit]>,
    name: &str,
) -> Result<PathBuf, String> {
    let mut doc = OpenDocument::open_image(
        DocumentId(u64::MAX),
        input,
        editor_core::DEFAULT_HISTORY_LIMIT,
    )
    .map_err(|e| format!("cannot open: {e}"))?;
    if let Some(edits) = edits {
        let applied = replay(&mut doc, edits);
        if applied == 0 && !edits.is_empty() {
            return Err("none of the Action's steps applied to this file".to_string());
        }
    }
    let (w, h) = (doc.document.width(), doc.document.height());
    let rgba = doc
        .composite(doc.canvas_rect())
        .map_err(|e| format!("cannot flatten: {e}"))?;
    let space = doc.document.meta.color_space.clone();
    let image = raster::export::linear_from_rgba8(w, h, &rgba, &space)
        .map_err(|e| format!("cannot convert: {e}"))?;
    let mut preset = raster::ExportPreset::new(name, spec.export_format())
        .with_scale(spec.scale())
        .for_color_mode(doc.document.meta.color_mode);
    preset.name = name.to_string();
    let metadata = raster::export::ExportMetadata {
        icc_profile: None,
        icc_profile_space: None,
    };
    raster::export::export_batch_to_dir(&spec.destination, &image, &[preset], &metadata)
        .map_err(|e| format!("cannot write: {e}"))?
        .into_iter()
        .next()
        .ok_or_else(|| "nothing was written".to_string())
}

/// Replay recorded steps on `doc` exactly as the Actions panel's Play does
/// (`Editor::replay`): each step's tile bytes re-inserted into this
/// document's store under fresh hashes, a step that painted a layer
/// retargeted to the layer at the same stack position (skipped when this
/// document's stack is shallower). Answers how many steps applied.
pub(crate) fn replay(doc: &mut OpenDocument, edits: &[RecordedEdit]) -> usize {
    let mut applied = 0;
    for edit in edits {
        let stack = doc.document.layers.iter_depth_first();
        let mut map = BTreeMap::new();
        for (hash, bytes) in &edit.tiles {
            let fresh = doc.tiles.insert_bytes(bytes.clone());
            map.insert(format!("{hash:?}"), fresh);
        }
        let command = match edit.layer {
            Some(index) => match stack.get(index) {
                Some(&target) => remap(edit.command.clone(), target, &map),
                None => continue,
            },
            None => edit.command.clone(),
        };
        let before = doc.history_depth();
        let _ = doc.apply(command);
        if doc.history_depth() != before {
            applied += 1;
        }
    }
    applied
}

fn rekey(
    delta: &editor_core::TileDelta,
    map: &BTreeMap<String, raster::TileHash>,
) -> Option<editor_core::TileDelta> {
    let edits: Vec<editor_core::pixels::TileEdit> = delta
        .edits()
        .iter()
        .map(|edit| editor_core::pixels::TileEdit {
            coord: edit.coord,
            hash: edit.hash.map(|h| *map.get(&format!("{h:?}")).unwrap_or(&h)),
        })
        .collect();
    editor_core::pixels::TileDelta::new(edits).ok()
}

fn retarget(old: editor_core::PixelTarget, to: layer_model::LayerId) -> editor_core::PixelTarget {
    match old {
        editor_core::PixelTarget::Layer(_) => editor_core::PixelTarget::Layer(to),
        // As `Editor::replay`: a mask target keeps the layer it names.
        other => other,
    }
}

fn remap(
    command: Command,
    target: layer_model::LayerId,
    map: &BTreeMap<String, raster::TileHash>,
) -> Command {
    match command {
        Command::PaintTiles { target: old, delta } => Command::PaintTiles {
            target: retarget(old, target),
            delta: rekey(&delta, map).unwrap_or(delta),
        },
        Command::FillRegion {
            target: old,
            rect,
            value,
            delta,
        } => Command::FillRegion {
            target: retarget(old, target),
            rect,
            value,
            delta: rekey(&delta, map).unwrap_or(delta),
        },
        Command::Transaction { label, commands } => Command::Transaction {
            label,
            commands: commands
                .into_iter()
                .map(|c| remap(c, target, map))
                .collect(),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worker's per-file messages become "n of N files (name)" on the
    /// status line, and its report the summary, removing the job.
    #[test]
    fn progress_reaches_the_status_line_and_the_report_ends_the_job() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = Editor::with_state(
            crate::prefs::AppPaths::rooted(dir.path()),
            crate::prefs::Preferences::default(),
            crate::recent::RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new()),
        );
        let (tx, rx) = channel();
        JOBS.with(|jobs| jobs.borrow_mut().push(Pending { rx, label: "Batch" }));
        tx.send(Progress::File {
            done: 2,
            total: 5,
            name: "b.png".into(),
        })
        .unwrap();
        assert_eq!(poll(&mut ed), None);
        assert_eq!(ed.status(), Some("Batch: 2 of 5 files (b.png)"));
        assert!(pending());
        let report = BatchReport {
            mode_label: "Batch".into(),
            written: vec![PathBuf::from("a.png")],
            failed: vec![(PathBuf::from("c.png"), "cannot open".into())],
            log: Some(PathBuf::from(ERROR_LOG)),
        };
        tx.send(Progress::Finished(report.clone())).unwrap();
        assert_eq!(poll(&mut ed), Some(report.summary()));
        assert!(report.summary().contains("1 failed (see batch-errors.txt)"));
        assert!(!pending());
    }

    /// The worker body reports every input, in order, as it goes.
    #[test]
    fn the_worker_reports_each_file_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("in"), dir.path().join("out"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        for name in ["b.png", "a.png"] {
            std::fs::write(
                src.join(name),
                raster::encode(raster::ExportFormat::Png, 2, 2, &[7u8; 16]).unwrap(),
            )
            .unwrap();
        }
        let inputs = batch_inputs(&src).unwrap();
        assert_eq!(
            inputs,
            vec![src.join("a.png"), src.join("b.png")],
            "name order"
        );
        let mut spec = BatchSpec::new(BatchMode::ConvertFormats);
        spec.source = src;
        spec.destination = dst.clone();
        let mut seen = Vec::new();
        let report = run(&inputs, &spec, None, "Convert Formats", |done, name| {
            seen.push((done, name.to_string()))
        });
        assert_eq!(seen, vec![(1, "a.png".into()), (2, "b.png".into())]);
        assert_eq!(report.written, vec![dst.join("a.png"), dst.join("b.png")]);
        assert!(report.failed.is_empty() && report.log.is_none());
    }
}
