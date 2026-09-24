//! W11-G: the Object Selection tool's rectangle, run on a job worker.
//!
//! [`tools::select::ObjectSelectionTool`] only records the dragged rectangle;
//! its release emits one `SelectionEdit` whose `incoming` is that rectangle.
//! [`super::ToolPointer::handle`] hands that edit here instead of folding it:
//! [`start`] snapshots the active pixel layer and hands
//! `selection::select_object` (GrabCut initialised from the rectangle) to the
//! editor's [`crate::jobs::Spawner`] (worker threads in the desktop binary,
//! inline in the tests), and the finish folds the object's mask into the
//! selection with the gesture's mode as ONE `SetSelection` step.
//!
//! As with Select > Subject (`menu_bridge::subject_job`), a result lands only
//! if the document, the layer, the selection and the pixels it was computed
//! from are all unchanged; otherwise it is dropped and the status line says
//! why. One Object Selection runs at a time. The in-flight job lives in a
//! thread-local queue that [`poll`] drains; `ToolPointer::settle_preview`
//! (once a frame) and `Editor::poll_jobs` both call it, and
//! `Editor::jobs_pending` counts it, so the desktop shell's idle loop keeps
//! waking at its job poll rate until the result lands, as it does for
//! Select > Subject.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

use editor_core::{Command, Selection};
use layer_model::LayerId;
use selection::{BooleanOp, Rect};

use crate::doc::DocumentId;
use crate::editor::Editor;

const NAME: &str = "Object Selection";

type Outcome = Result<selection::SubjectOutcome, String>;

struct Pending {
    rx: Receiver<Outcome>,
    progress: Arc<AtomicU32>,
    doc: DocumentId,
    layer: LayerId,
    selection: Selection,
    pixels: Vec<u8>,
    canvas: Rect,
    op: BooleanOp,
    started: Instant,
}

thread_local! {
    static JOBS: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
}

/// Whether an Object Selection is running.
pub(crate) fn pending() -> bool {
    JOBS.with(|jobs| !jobs.borrow().is_empty())
}

/// The active layer, when it owns pixels.
fn pixel_layer(editor: &Editor) -> Result<LayerId, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let id = doc.document.active_layer().ok_or("Select a layer first")?;
    let layer = doc
        .document
        .layers
        .get(id)
        .ok_or("The active layer is not in the document")?;
    match &layer.kind {
        layer_model::LayerKind::Raster(_) | layer_model::LayerKind::Generator(_) => Ok(id),
        other => Err(format!(
            "Object Selection works on a pixel layer; the active layer is a {}",
            editor_core::layer_class_name(other)
        )),
    }
}

/// Start `select_object` over `rect` (document pixels, `[min, max)`) on the
/// active pixel layer. With the inline spawner the selection has landed (or
/// been refused) by the time this returns, and `Ok(true)` means a
/// `SetSelection` step landed. With worker threads the job is queued and
/// `Ok(false)` comes back with the status line saying it is running.
pub(crate) fn start(
    editor: &mut Editor,
    rect: (glam::IVec2, glam::IVec2),
    op: BooleanOp,
    canvas: Rect,
) -> Result<bool, String> {
    if pending() {
        return Err(format!("{NAME} is already running"));
    }
    let layer = pixel_layer(editor)?;
    let (doc, selection, pixels, w, h) = {
        let open = editor.active().ok_or("No document is open")?;
        (
            open.id(),
            open.document.selection.clone(),
            crate::menu_bridge::pixels::read_layer(open, layer),
            open.document.width(),
            open.document.height(),
        )
    };
    if w == 0 || h == 0 {
        return Err("The canvas has no pixels".to_string());
    }
    let bounds = Rect::from_xywh(
        rect.0.x,
        rect.0.y,
        (rect.1.x - rect.0.x).max(0) as u32,
        (rect.1.y - rect.0.y).max(0) as u32,
    );
    let progress = Arc::new(AtomicU32::new(0));
    let (tx, rx) = channel();
    let input = pixels.clone();
    let progress_w = progress.clone();
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        let outcome = selection::ImageBuffer::from_rgba8(glam::IVec2::ZERO, w, h, input)
            .and_then(|image| {
                selection::select_object(
                    &image.view(),
                    bounds,
                    &selection::SubjectOptions::default(),
                    &mut |p| progress_w.store((p * 1000.0) as u32, Ordering::Relaxed),
                )
            })
            .map_err(|e| format!("{NAME} failed: {e}"));
        // The receiver may be gone (the editor closed): nothing to tell.
        let _ = tx.send(outcome);
    });
    let spawn = editor.spawner();
    spawn(format!("{NAME} job"), body)
        .map_err(|e| format!("{NAME} could not start a worker: {e}"))?;
    let job = Pending {
        rx,
        progress,
        doc,
        layer,
        selection,
        pixels,
        canvas,
        op,
        started: Instant::now(),
    };
    match job.rx.try_recv() {
        // Inline spawner: already done, applied now as one step.
        Ok(outcome) => {
            let message = finish(editor, job, outcome)?;
            editor.set_status(message);
            Ok(true)
        }
        Err(TryRecvError::Empty) => {
            editor.set_status(running_status(&job));
            JOBS.with(|jobs| jobs.borrow_mut().push(job));
            Ok(false)
        }
        Err(TryRecvError::Disconnected) => Err(format!(
            "{NAME} failed: the worker stopped without reporting"
        )),
    }
}

fn running_status(job: &Pending) -> String {
    format!(
        "{NAME} is running ({}%, {:.1} s)",
        job.progress.load(Ordering::Relaxed).min(1000) / 10,
        job.started.elapsed().as_secs_f32()
    )
}

/// Land every finished Object Selection and say how far a running one has
/// got. Reports whether a selection step landed.
pub(crate) fn poll(editor: &mut Editor) -> bool {
    if !pending() {
        return false;
    }
    let mut landed = false;
    let jobs = JOBS.with(|jobs| std::mem::take(&mut *jobs.borrow_mut()));
    for job in jobs {
        match job.rx.try_recv() {
            Ok(outcome) => {
                let message = match finish(editor, job, outcome) {
                    Ok(m) => {
                        landed = true;
                        m
                    }
                    Err(m) => m,
                };
                editor.set_status(message);
            }
            Err(TryRecvError::Empty) => {
                editor.set_status(running_status(&job));
                JOBS.with(|jobs| jobs.borrow_mut().push(job));
            }
            Err(TryRecvError::Disconnected) => editor.set_status(format!(
                "{NAME} failed: the worker stopped without reporting"
            )),
        }
    }
    landed
}

/// Fold a finished job's mask into the selection with the gesture's mode, as
/// one `SetSelection` step, if what it was computed from is still current.
fn finish(editor: &mut Editor, job: Pending, outcome: Outcome) -> Result<String, String> {
    let found = outcome?;
    let still = match editor.active() {
        Some(open) if open.id() == job.doc => {
            open.document.selection == job.selection
                && pixel_layer(editor).ok() == Some(job.layer)
                && crate::menu_bridge::pixels::read_layer(open, job.layer) == job.pixels
        }
        _ => false,
    };
    if !still {
        return Err(format!(
            "{NAME} was discarded: the document, layer, pixels or selection changed while it ran"
        ));
    }
    let mask = match found {
        selection::SubjectOutcome::NothingFound(why) => return Err(why.to_string()),
        selection::SubjectOutcome::Selected(mask) => mask,
    };
    let next =
        selection::combine_selection(job.canvas, &job.selection, &Selection::Mask(mask), job.op)
            .map_err(|e| format!("{NAME} failed: {e}"))?;
    if next == job.selection {
        return Err(format!("{NAME} left the selection as it was"));
    }
    editor.apply_command(Command::SetSelection { selection: next });
    Ok("Selected the object".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    const W: u32 = 96;
    const H: u32 = 80;

    fn inside_disc(x: u32, y: u32) -> bool {
        (x as f32 + 0.5 - 50.0).powi(2) + (y as f32 + 0.5 - 38.0).powi(2) < 22.0 * 22.0
    }

    /// A textured orange disc on a textured teal ground.
    fn editor(dir: &std::path::Path) -> Editor {
        let mut rgba = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                let t = ((x * 7 + y * 13) % 11) as i32 * 4 - 20;
                let c: [i32; 3] = if inside_disc(x, y) {
                    [220 + t / 2, 130 + t, 40]
                } else {
                    [40, 120 + t, 150 - t]
                };
                for v in c {
                    rgba.push(v.clamp(0, 255) as u8);
                }
                rgba.push(255);
            }
        }
        let png = dir.join("disc.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&png).unwrap();
        ed
    }

    fn selected(ed: &Editor, x: u32, y: u32) -> bool {
        let sel = &ed.active().unwrap().document.selection;
        sel.coverage_at(glam::IVec2::new(x as i32, y as i32)) >= 0.5
    }

    /// With worker threads the rectangle's GrabCut runs off the interaction
    /// thread: nothing is selected until a frame's `settle_preview` polls,
    /// a second drag while it runs is refused, and the result lands as one
    /// history step.
    #[test]
    fn the_job_runs_on_a_worker_and_lands_on_a_later_frame() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_spawner(crate::jobs::spawn_thread);
        let depth = ed.active().unwrap().history_depth();
        let canvas = Rect::from_xywh(0, 0, W, H);
        let rect = (glam::IVec2::new(20, 8), glam::IVec2::new(82, 70));
        let landed = start(&mut ed, rect, BooleanOp::Replace, canvas).unwrap();
        assert!(!landed, "a worker job cannot have landed yet");
        assert!(pending());
        assert!(ed
            .status()
            .unwrap()
            .starts_with("Object Selection is running"));
        assert_eq!(
            start(&mut ed, rect, BooleanOp::Replace, canvas).unwrap_err(),
            "Object Selection is already running"
        );
        let mut pointer = crate::tool_input::ToolPointer::new();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while pending() {
            assert!(Instant::now() < deadline, "the worker never finished");
            std::thread::yield_now();
            pointer.settle_preview(&mut ed);
        }
        assert_eq!(ed.status(), Some("Selected the object"));
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        assert!(selected(&ed, 50, 38), "the disc's centre");
        assert!(!selected(&ed, 22, 10), "the rectangle's corner");
    }

    /// The shell's idle loop (`Shell::about_to_wait` -> `pump_jobs`) only
    /// keeps waking while `Editor::jobs_pending` is true, and lands results
    /// through `Editor::poll_jobs`: a running Object Selection must count,
    /// and `poll_jobs` alone must land it, or an idle window would sleep on
    /// the finished job until the user moved the mouse.
    #[test]
    fn the_editor_job_pump_counts_and_lands_the_job() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_spawner(crate::jobs::spawn_thread);
        let depth = ed.active().unwrap().history_depth();
        let canvas = Rect::from_xywh(0, 0, W, H);
        let rect = (glam::IVec2::new(20, 8), glam::IVec2::new(82, 70));
        assert!(!ed.jobs_pending());
        assert!(!start(&mut ed, rect, BooleanOp::Replace, canvas).unwrap());
        assert!(ed.jobs_pending(), "the idle loop must see the running job");
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while ed.jobs_pending() {
            assert!(Instant::now() < deadline, "the worker never finished");
            std::thread::yield_now();
            ed.poll_jobs();
        }
        assert_eq!(ed.status(), Some("Selected the object"));
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        assert!(selected(&ed, 50, 38), "the disc's centre");
    }

    /// A selection changed while the worker ran makes the result stale: it
    /// is dropped and the status line says why.
    #[test]
    fn a_result_computed_from_a_stale_selection_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_spawner(crate::jobs::spawn_thread);
        let canvas = Rect::from_xywh(0, 0, W, H);
        let rect = (glam::IVec2::new(20, 8), glam::IVec2::new(82, 70));
        start(&mut ed, rect, BooleanOp::Replace, canvas).unwrap();
        let changed = Selection::Rect {
            min: glam::IVec2::ZERO,
            max: glam::IVec2::new(4, 4),
        };
        ed.active_mut().unwrap().document.selection = changed.clone();
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while pending() {
            assert!(Instant::now() < deadline, "the worker never finished");
            std::thread::yield_now();
            poll(&mut ed);
        }
        assert_eq!(ed.active().unwrap().document.selection, changed);
        assert!(ed.status().unwrap().contains("was discarded"));
    }
}
