//! W7-I: Content-Aware Fill and Content-Aware Scale driven through the
//! shell's own routes — Edit ▸ Fill… opened by a menu intent, confirmed by a
//! real key press in a real chrome frame (`Chrome::ui`), applied by
//! `apply_chrome`, and the synthesis run by the editor's job spawner and
//! landed by `pump_jobs`. Every assertion is on the layer's pixels and the
//! undo timeline.

use super::*;
use crate::chrome::ChromeOutput;

use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const W: u32 = 64;
const H: u32 = 64;
/// The hole the tests fill: a 16x16 square in the middle.
const HOLE: (i32, i32, i32, i32) = (24, 24, 40, 40);

/// Vertical stripes, 8 px period, black and white.
fn stripe(x: u32) -> [u8; 4] {
    if (x / 4).is_multiple_of(2) {
        [0, 0, 0, 255]
    } else {
        [255, 255, 255, 255]
    }
}

fn in_hole(x: u32, y: u32) -> bool {
    let (x0, y0, x1, y1) = HOLE;
    (x0..x1).contains(&(x as i32)) && (y0..y1).contains(&(y as i32))
}

/// A stripes image whose hole is punched red (a colour the stripes never
/// use), opened in a shell with the hole selected.
fn shell_with_damaged_stripes(dir: &std::path::Path, spawner: crate::jobs::Spawner) -> Shell {
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            rgba.extend_from_slice(&if in_hole(x, y) {
                [255, 0, 0, 255]
            } else {
                stripe(x)
            });
        }
    }
    let png = dir.join("stripes.png");
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
    editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
        min: glam::IVec2::new(HOLE.0, HOLE.1),
        max: glam::IVec2::new(HOLE.2, HOLE.3),
    };
    editor.set_spawner(spawner);
    Shell::new(editor, Vec::new())
}

fn layer_pixels(shell: &Shell) -> Vec<u8> {
    let doc = shell.editor.active().unwrap();
    let layer = doc.document.active_layer().unwrap();
    crate::menu_bridge::pixels::read_layer(doc, layer)
}

/// How many hole pixels follow their column's stripe, and how many are
/// still the red the hole was punched with.
fn hole_score(pixels: &[u8]) -> (usize, usize, usize) {
    let (mut good, mut red, mut total) = (0, 0, 0);
    for y in 0..H {
        for x in 0..W {
            if !in_hole(x, y) {
                continue;
            }
            total += 1;
            let i = ((y * W + x) * 4) as usize;
            let px = &pixels[i..i + 4];
            if px == [255, 0, 0, 255] {
                red += 1;
            }
            let want = stripe(x);
            if (0..3).all(|c| px[c].abs_diff(want[c]) < 64) {
                good += 1;
            }
        }
    }
    (good, red, total)
}

/// One frame of the real chrome, applied by the shell.
fn frame(shell: &mut Shell, ctx: &egui::Context, events: Vec<egui::Event>) {
    let mut out = ChromeOutput::default();
    let input = egui::RawInput {
        events,
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

fn enter() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

/// Edit ▸ Fill… opened through the chrome, Contents set to Content-Aware,
/// confirmed with Enter in a real frame.
fn fill_content_aware_through_the_dialog(shell: &mut Shell) {
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    shell
        .chrome
        .emit(ui::Intent::Action(ui::menu::MenuAction::FillDialog));
    for _ in 0..3 {
        frame(shell, &ctx, Vec::new());
    }
    // The Contents combo's choice: the dialog the host opened, re-made with
    // Contents: Content-Aware (what picking that combo row sets).
    {
        let dialog = shell
            .chrome
            .dialogs_for_test()
            .active_fill_dialog_for_test();
        let mut spec = dialog.spec();
        spec.contents = ui::dialogs::FillContents::ContentAware;
        *dialog = ui::dialogs::FillDialog::new(spec, Vec::new());
    }
    frame(shell, &ctx, Vec::new());
    frame(shell, &ctx, vec![enter()]);
}

thread_local! {
    static QUEUED: std::cell::RefCell<Vec<Box<dyn FnOnce() + Send>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A spawner that holds every job until the test runs it.
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

/// The Fill dialog's Content-Aware choice reaches the pixels: the red hole
/// is rebuilt as stripes, nothing outside the selection moves, and it is ONE
/// undo step that one Undo takes back.
#[test]
fn edit_fill_content_aware_through_the_dialog_rebuilds_the_hole_in_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_damaged_stripes(dir.path(), crate::jobs::run_inline);
    let before = layer_pixels(&shell);
    let depth = shell.editor.active().unwrap().history_depth();

    fill_content_aware_through_the_dialog(&mut shell);

    let after = layer_pixels(&shell);
    assert_ne!(
        after, before,
        "the confirmed Content-Aware fill changed no pixel"
    );
    let (good, red, total) = hole_score(&after);
    assert_eq!(red, 0, "{red}/{total} hole pixels are still red");
    assert!(
        good * 100 >= total * 90,
        "only {good}/{total} filled pixels follow their column's stripe"
    );
    for y in 0..H {
        for x in 0..W {
            if !in_hole(x, y) {
                let i = ((y * W + x) * 4) as usize;
                assert_eq!(after[i..i + 4], before[i..i + 4], "({x},{y}) moved");
            }
        }
    }
    assert_eq!(
        shell.editor.active().unwrap().history_depth(),
        depth + 1,
        "the fill is exactly one undo step"
    );
    shell.perform(Action::Undo);
    assert_eq!(layer_pixels(&shell), before, "one Undo takes the fill back");
}

/// The synthesis runs on the job worker: while it is held, the shell keeps
/// processing frames and the pixels have not moved; once it runs (on a real
/// worker thread), the next `pump_jobs` lands it as one undo step.
#[test]
fn a_content_aware_fill_runs_on_the_worker_while_frames_keep_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_damaged_stripes(dir.path(), queue_job);
    let before = layer_pixels(&shell);
    let depth = shell.editor.active().unwrap().history_depth();

    fill_content_aware_through_the_dialog(&mut shell);

    assert!(shell.editor.jobs_pending(), "the fill is a job in flight");
    for _ in 0..3 {
        assert!(shell.pump_jobs(), "still in flight");
        shell.apply_chrome(ChromeOutput::default());
    }
    assert_eq!(
        layer_pixels(&shell),
        before,
        "nothing lands before the worker ran"
    );

    assert_eq!(run_queued_on_a_worker(), 1);
    assert!(!shell.pump_jobs(), "nothing left in flight");
    let after = layer_pixels(&shell);
    let (good, red, total) = hole_score(&after);
    assert_eq!(red, 0, "{red}/{total} hole pixels are still red");
    assert!(
        good * 100 >= total * 90,
        "{good}/{total} follow the stripes"
    );
    assert_eq!(shell.editor.active().unwrap().history_depth(), depth + 1);
}

/// A result computed for a selection the user has since changed is dropped,
/// not painted into the new one.
#[test]
fn a_content_aware_fill_whose_selection_changed_while_it_ran_is_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_damaged_stripes(dir.path(), queue_job);
    let before = layer_pixels(&shell);

    fill_content_aware_through_the_dialog(&mut shell);
    assert!(shell.editor.jobs_pending());
    shell.editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
        min: glam::IVec2::new(0, 0),
        max: glam::IVec2::new(8, 8),
    };
    assert_eq!(run_queued_on_a_worker(), 1);
    assert!(!shell.pump_jobs());
    assert_eq!(layer_pixels(&shell), before, "a stale result landed");
    assert!(
        shell
            .editor
            .status()
            .is_some_and(|s| s.contains("discarded")),
        "{:?}",
        shell.editor.status()
    );
}

/// Edit ▸ Content-Aware Scale also runs on the worker and lands as one step.
#[test]
fn content_aware_scale_runs_on_the_worker_and_lands_as_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_damaged_stripes(dir.path(), queue_job);
    shell.editor.active_mut().unwrap().document.selection = editor_core::Selection::None;
    let before = layer_pixels(&shell);
    let depth = shell.editor.active().unwrap().history_depth();

    shell
        .chrome
        .emit(ui::Intent::Action(ui::menu::MenuAction::ContentAwareScale(
            ui::menu::ContentAwareScaleStep::Width80,
        )));
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    frame(&mut shell, &ctx, Vec::new());
    assert!(shell.editor.jobs_pending(), "the scale is a job in flight");
    assert_eq!(layer_pixels(&shell), before);

    assert_eq!(run_queued_on_a_worker(), 1);
    assert!(!shell.pump_jobs());
    let after = layer_pixels(&shell);
    assert_ne!(after, before, "the scale changed nothing");
    // 80% of 64 is 51 columns, centred: the first 6 columns are transparent.
    for y in 0..H {
        let i = (y * W * 4) as usize;
        assert_eq!(
            after[i + 3],
            0,
            "column 0 of row {y} is not a transparent band"
        );
    }
    assert_eq!(shell.editor.active().unwrap().history_depth(), depth + 1);
}
