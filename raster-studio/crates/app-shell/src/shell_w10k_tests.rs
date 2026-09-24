//! W10-K: Select ▸ Subject driven through the shell's own routes — the menu
//! intent emitted into the real chrome, performed by a real chrome frame and
//! `apply_chrome`, the pipeline run by the editor's job spawner, and the
//! result landed by `pump_jobs` (what `about_to_wait` calls on an idle
//! window), with no further chrome frame.

use super::*;
use crate::chrome::ChromeOutput;
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const W: u32 = 96;
const H: u32 = 80;

fn inside_disc(x: u32, y: u32) -> bool {
    (x as f32 + 0.5 - 50.0).powi(2) + (y as f32 + 0.5 - 38.0).powi(2) < 22.0 * 22.0
}

/// A textured orange disc on a textured teal background, opened in a shell
/// whose editor runs jobs through `spawner`.
fn shell_with_disc(dir: &std::path::Path, spawner: crate::jobs::Spawner) -> Shell {
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
    let mut editor = crate::editor::Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    editor.open_path(&png).unwrap();
    editor.set_spawner(spawner);
    Shell::new(editor, Vec::new())
}

thread_local! {
    static QUEUED: std::cell::RefCell<Vec<Box<dyn FnOnce() + Send>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A spawner that holds the job body until the test runs it.
fn queue_job(_name: String, body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    QUEUED.with(|q| q.borrow_mut().push(body));
    Ok(())
}

/// Run every held job body on a real worker thread, joined.
fn run_queued_on_a_worker() -> usize {
    let bodies: Vec<_> = QUEUED.with(|q| q.borrow_mut().drain(..).collect());
    let n = bodies.len();
    for body in bodies {
        std::thread::spawn(body)
            .join()
            .expect("the job body completes");
    }
    n
}

/// One frame of the real chrome, applied by the shell.
fn frame(shell: &mut Shell, ctx: &egui::Context) {
    let mut out = ChromeOutput::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| {
        out = shell.chrome.ui(ctx, &mut shell.editor);
    });
    shell.apply_chrome(out);
}

fn selection(shell: &Shell) -> editor_core::Selection {
    shell.editor.active().unwrap().document.selection.clone()
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

/// While Select Subject runs, `pump_jobs` reports work in flight (so
/// `about_to_wait` wakes at the job poll rate instead of sleeping), and once
/// the worker has finished, `pump_jobs` alone — no chrome frame, no pointer
/// move — lands the selection as one undo step.
#[test]
fn select_subject_is_a_job_pump_jobs_waits_on_and_lands() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_disc(dir.path(), queue_job);
    let depth = shell.editor.active().unwrap().history_depth();
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);

    shell
        .chrome
        .emit(ui::Intent::Action(ui::menu::MenuAction::SelectSubject));
    frame(&mut shell, &ctx);

    assert_eq!(selection(&shell), editor_core::Selection::None);
    assert!(
        shell.editor.jobs_pending(),
        "Select Subject is a job in flight"
    );
    for _ in 0..3 {
        assert!(
            shell.pump_jobs(),
            "still in flight: the loop must keep waking"
        );
    }
    assert!(
        shell
            .editor
            .status()
            .is_some_and(|s| s.starts_with("Select Subject is running")),
        "{:?}",
        shell.editor.status()
    );
    assert_eq!(selection(&shell), editor_core::Selection::None);

    assert_eq!(run_queued_on_a_worker(), 1);
    assert!(!shell.pump_jobs(), "nothing left in flight");
    assert_eq!(shell.editor.status(), Some("Selected the subject"));
    let score = iou(&selection(&shell));
    assert!(score >= 0.9, "IoU {score}");
    assert_eq!(shell.editor.active().unwrap().history_depth(), depth + 1);
}
