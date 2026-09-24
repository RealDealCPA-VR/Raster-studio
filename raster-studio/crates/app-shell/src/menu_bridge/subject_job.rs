//! W10-K: Select ▸ Subject runs on a worker.
//!
//! The pipeline ([`selection::select_subject`]: saliency seeds, then GrabCut's
//! colour mixtures and max-flow cuts) takes from tens of milliseconds to a
//! few seconds, so the interaction thread only snapshots the active pixel
//! layer, hands the work to the editor's [`crate::jobs::Spawner`] (worker
//! threads in the desktop binary, inline in the deterministic unit tests),
//! and [`poll`] lands the result as ONE undoable `SetSelection` step. While it
//! runs, the status line reports its progress.
//!
//! # A stale result never lands
//!
//! The job remembers the document, the layer, the selection and the pixels
//! it was computed from; when it finishes, it applies only if all four are
//! still what the user is looking at. Otherwise it is dropped and the status
//! line says why. One Select Subject runs at a time.
//!
//! # Where the in-flight job lives
//!
//! In a thread-local queue owned by this module, drained by [`poll`], which
//! `Editor::poll_jobs` calls (the shell's `pump_jobs`, run from
//! `about_to_wait` even on an idle window) and [`super::context`] calls on
//! every frame the chrome draws. [`pending`] reports whether one is in
//! flight, and `Editor::jobs_pending` includes it, so the event loop keeps
//! waking at the job poll rate while Select Subject runs.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use layer_model::LayerId;

use crate::doc::DocumentId;
use crate::editor::Editor;

const NAME: &str = "Select Subject";

type Outcome = Result<selection::SubjectOutcome, String>;

/// A Select Subject in flight: its receiver and the snapshot it must still
/// match to apply.
struct Pending {
    rx: Receiver<Outcome>,
    /// The worker's progress in thousandths.
    progress: Arc<AtomicU32>,
    doc: DocumentId,
    layer: LayerId,
    selection: editor_core::Selection,
    pixels: Vec<u8>,
    started: Instant,
}

thread_local! {
    static JOBS: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
    /// The worker thread of the last job this thread started (tests read it).
    static LAST_WORKER: RefCell<Option<Arc<Mutex<Option<std::thread::ThreadId>>>>> =
        const { RefCell::new(None) };
}

/// Whether a Select Subject is running.
pub(crate) fn pending() -> bool {
    JOBS.with(|jobs| !jobs.borrow().is_empty())
}

/// The thread the most recent job started from this thread ran on, once it
/// has run.
#[cfg(test)]
pub(crate) fn last_worker_thread() -> Option<std::thread::ThreadId> {
    LAST_WORKER.with(|w| w.borrow().as_ref().and_then(|m| *m.lock().ok()?))
}

/// Select ▸ Subject: start the pipeline over the active pixel layer.
///
/// Refused here, with no job, when there is no document or pixel layer, the
/// canvas is empty, or a Select Subject is already running. With the inline
/// spawner the selection has landed by the time this returns; with worker
/// threads the answer says it is running and [`poll`] lands it.
pub(crate) fn start(editor: &mut Editor) -> Result<String, String> {
    if pending() {
        return Err(format!("{NAME} is already running"));
    }
    let layer = super::pixel_layer(editor)?;
    let (w, h) = super::canvas_of(editor)?;
    if w == 0 || h == 0 {
        return Err("The canvas has no pixels".to_string());
    }
    let (doc, selection, pixels) = {
        let open = editor.active().ok_or("No document is open")?;
        (
            open.id(),
            open.document.selection.clone(),
            super::pixels::read_layer(open, layer),
        )
    };
    let progress = Arc::new(AtomicU32::new(0));
    let worker = Arc::new(Mutex::new(None));
    LAST_WORKER.with(|w| *w.borrow_mut() = Some(worker.clone()));
    let (tx, rx) = channel();
    let input = pixels.clone();
    let (progress_w, worker_w) = (progress.clone(), worker);
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        if let Ok(mut slot) = worker_w.lock() {
            *slot = Some(std::thread::current().id());
        }
        let outcome = selection::ImageBuffer::from_rgba8(glam::IVec2::ZERO, w, h, input)
            .and_then(|image| {
                selection::select_subject(
                    &image.view(),
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
        started: Instant::now(),
    };
    match job.rx.try_recv() {
        // Inline spawner: already done — apply it now, as one step.
        Ok(outcome) => finish(editor, job, outcome),
        Err(TryRecvError::Empty) => {
            let status = running_status(&job);
            JOBS.with(|jobs| jobs.borrow_mut().push(job));
            Ok(status)
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

/// Land every finished Select Subject; say how far a running one has got.
/// Called from `Editor::poll_jobs` and once a frame from [`super::context`].
pub(crate) fn poll(editor: &mut Editor) {
    if !pending() {
        return;
    }
    let jobs = JOBS.with(|jobs| std::mem::take(&mut *jobs.borrow_mut()));
    for job in jobs {
        match job.rx.try_recv() {
            Ok(outcome) => {
                let message = match finish(editor, job, outcome) {
                    Ok(m) | Err(m) => m,
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
}

/// Apply a finished job as one `SetSelection` step, if what it was computed
/// from is still what the user is looking at.
fn finish(editor: &mut Editor, job: Pending, outcome: Outcome) -> Result<String, String> {
    let found = outcome?;
    let still = match editor.active() {
        Some(open) if open.id() == job.doc => {
            open.document.selection == job.selection
                && super::pixel_layer(editor).ok() == Some(job.layer)
                && super::pixels::read_layer(open, job.layer) == job.pixels
        }
        _ => false,
    };
    if !still {
        return Err(format!(
            "{NAME} was discarded: the document, layer, pixels or selection changed while it ran"
        ));
    }
    match found {
        selection::SubjectOutcome::NothingFound(why) => Err(why.to_string()),
        selection::SubjectOutcome::Selected(mask) => {
            super::set_selection(editor, |_, _, _| Ok(editor_core::Selection::Mask(mask)))?;
            Ok("Selected the subject".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use ui::menu::MenuAction;

    const W: u32 = 96;
    const H: u32 = 80;

    fn inside_disc(x: u32, y: u32) -> bool {
        (x as f32 + 0.5 - 50.0).powi(2) + (y as f32 + 0.5 - 38.0).powi(2) < 22.0 * 22.0
    }

    /// A textured orange disc on a textured teal background, or a flat fill.
    fn editor_with_image(dir: &std::path::Path, flat: bool) -> Editor {
        let mut rgba = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                let t = ((x * 7 + y * 13) % 11) as i32 * 4 - 20;
                let c: [i32; 3] = if flat {
                    [120, 120, 120]
                } else if inside_disc(x, y) {
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
        let png = dir.join("subject.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        editor
    }

    fn selection(editor: &Editor) -> editor_core::Selection {
        editor.active().unwrap().document.selection.clone()
    }

    fn iou(sel: &editor_core::Selection) -> f64 {
        let (mut inter, mut uni) = (0u32, 0u32);
        for y in 0..H {
            for x in 0..W {
                let a = sel.coverage_at(glam::IVec2::new(x as i32, y as i32)) >= 0.5;
                let b = inside_disc(x, y);
                inter += u32::from(a && b);
                uni += u32::from(a || b);
            }
        }
        f64::from(inter) / f64::from(uni.max(1))
    }

    /// The Select menu lists Subject, the bridge resolves it enabled over a
    /// pixel layer, and performing the resolved action selects the disc as
    /// one undoable step.
    #[test]
    fn select_subject_is_a_live_select_menu_row_that_selects_the_disc() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with_image(dir.path(), false);
        let select = super::super::menus(&editor)
            .into_iter()
            .find(|m| m.actions().contains(&MenuAction::ColorRange))
            .expect("the Select menu");
        assert!(select.actions().contains(&MenuAction::SelectSubject));
        let context = super::super::context(&mut editor, &ui::Workspace::new());
        let intent =
            super::super::resolve_intent(MenuAction::SelectSubject, &context, &editor).unwrap();
        let ui::Intent::Action(action) = intent else {
            panic!("Select Subject resolved to {intent:?}");
        };
        let message = super::super::perform(action, &mut editor).unwrap();
        assert_eq!(message, "Selected the subject");
        let score = iou(&selection(&editor));
        assert!(score >= 0.9, "IoU {score}");
        editor.dispatch(crate::action::Action::Undo).unwrap();
        assert_eq!(selection(&editor), editor_core::Selection::None);
    }

    #[test]
    fn a_flat_image_selects_nothing_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with_image(dir.path(), true);
        let refused = super::super::perform(MenuAction::SelectSubject, &mut editor).unwrap_err();
        assert_eq!(refused, selection::subject::NOTHING_STANDS_OUT);
        assert_eq!(selection(&editor), editor_core::Selection::None);
    }

    /// Wait (bounded) for the worker, landing it through the per-frame
    /// `context` build exactly as the chrome does.
    fn pump_until_landed(editor: &mut Editor) {
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while pending() {
            assert!(Instant::now() < deadline, "the worker never finished");
            std::thread::yield_now();
            let _ = super::super::context(editor, &ui::Workspace::new());
        }
    }

    /// With worker threads the menu action returns at once, the selection is
    /// untouched until a frame polls, and the work ran on another thread.
    #[test]
    fn the_job_runs_off_the_ui_thread_and_lands_on_a_later_frame() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with_image(dir.path(), false);
        editor.set_spawner(crate::jobs::spawn_thread);
        let message = super::super::perform(MenuAction::SelectSubject, &mut editor).unwrap();
        assert!(
            message.starts_with("Select Subject is running"),
            "{message}"
        );
        assert_eq!(selection(&editor), editor_core::Selection::None);
        assert!(pending());
        let refused = super::super::perform(MenuAction::SelectSubject, &mut editor).unwrap_err();
        assert_eq!(refused, "Select Subject is already running");
        pump_until_landed(&mut editor);
        let worker = last_worker_thread().expect("the worker recorded its thread");
        assert_ne!(worker, std::thread::current().id());
        assert_eq!(editor.status(), Some("Selected the subject"));
        assert!(iou(&selection(&editor)) >= 0.9);
    }

    /// A selection changed while the worker ran means the result is stale:
    /// it is dropped and the status line says so.
    #[test]
    fn a_result_computed_from_a_stale_selection_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with_image(dir.path(), false);
        editor.set_spawner(crate::jobs::spawn_thread);
        super::super::perform(MenuAction::SelectSubject, &mut editor).unwrap();
        let changed = editor_core::Selection::Rect {
            min: glam::IVec2::ZERO,
            max: glam::IVec2::new(4, 4),
        };
        editor.active_mut().unwrap().document.selection = changed.clone();
        pump_until_landed(&mut editor);
        assert_eq!(selection(&editor), changed);
        assert!(editor.status().unwrap().contains("was discarded"));
    }
}
