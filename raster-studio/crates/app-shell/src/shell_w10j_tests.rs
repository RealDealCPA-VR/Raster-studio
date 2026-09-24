//! W10-J: File > New's Artboard box and Edit > Content-Aware Scale > With
//! Handles, driven through the shell's own routes only: `apply_chrome` for
//! what the New Document dialog confirmed, real chrome frames (`Chrome::ui`)
//! for the menu row and the options bar (its W field dragged with real egui
//! pointer events), and the key path `window_event` feeds (`on_key`: the
//! Alt+Shift+Ctrl+C chord, Enter).

use super::*;
use winit::keyboard::Key as WKey;

use crate::chrome::ChromeOutput;
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use tools::transform::{keys, TransformMode};
use tools::ToolId;

const SIDE: u32 = 100;
/// The two black bars of the test image: 4 px wide each, in the busy right
/// part of an otherwise flat grey picture.
const BARS: [std::ops::Range<u32>; 2] = [60..64, 80..84];

fn editor(dir: &std::path::Path) -> crate::editor::Editor {
    crate::editor::Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

/// A flat grey 100x100 image with two 4 px black bars, no selection, the
/// Brush in hand. Seam carving takes its seams out of the flat grey and
/// keeps the bars whole; a plain scale thins them.
fn shell_with_bars(dir: &std::path::Path) -> Shell {
    let mut rgba = vec![128u8; (SIDE * SIDE * 4) as usize];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let i = ((y * SIDE + x) * 4) as usize;
            rgba[i + 3] = 255;
            if BARS.iter().any(|b| b.contains(&x)) {
                rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
    }
    let png = dir.join("bars.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, SIDE, SIDE, &rgba).unwrap(),
    )
    .unwrap();
    let mut editor = editor(dir);
    editor.open_path(&png).unwrap();
    editor.set_tool(ToolId::Brush);
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(400.0, 300.0));
    shell
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    ctx
}

/// One chrome frame carrying `events`, and the shell performing what it
/// meant — the two steps the render loop takes.
fn frame(ctx: &egui::Context, shell: &mut Shell, events: Vec<egui::Event>) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(4000.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = ChromeOutput::default();
    let _ = ctx.run(input, |ctx| {
        out = shell.chrome.ui(ctx, &mut shell.editor);
    });
    shell.apply_chrome(out);
}

fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

/// Whether the options bar drew Free Transform's field `key` last frame.
fn field_drawn(ctx: &egui::Context, shell: &mut Shell, key: &'static str) -> bool {
    frame(ctx, shell, Vec::new());
    ctx.read_response(ui::view::ids::tool_option(ToolId::FreeTransform, key))
        .is_some()
}

/// Drag the options bar's field `key` by `by` points, a frame per step.
fn drag_field(ctx: &egui::Context, shell: &mut Shell, key: &'static str, by: egui::Vec2) {
    frame(ctx, shell, Vec::new());
    let id = ui::view::ids::tool_option(ToolId::FreeTransform, key);
    let from = ctx
        .read_response(id)
        .unwrap_or_else(|| panic!("{key} was not drawn"))
        .rect
        .center();
    frame(
        ctx,
        shell,
        vec![egui::Event::PointerMoved(from), button(from, true)],
    );
    for step in 1..=4 {
        let at = from + by * (step as f32 / 4.0);
        frame(ctx, shell, vec![egui::Event::PointerMoved(at)]);
    }
    frame(
        ctx,
        shell,
        vec![
            egui::Event::PointerMoved(from + by),
            button(from + by, false),
        ],
    );
    frame(ctx, shell, vec![egui::Event::PointerGone]);
}

fn enter(shell: &mut Shell) {
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Named(NamedKey::Enter),
        ElementState::Pressed,
        false,
    );
}

fn published_mode(shell: &Shell) -> Option<TransformMode> {
    shell
        .chrome
        .workspace()
        .canvas
        .sessions
        .transform
        .as_ref()
        .map(|(_, mode)| *mode)
}

fn held_float(shell: &Shell, key: &str) -> f32 {
    shell
        .chrome
        .tool_options(ToolId::FreeTransform)
        .into_iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_float())
        .unwrap_or_else(|| panic!("the bar holds no {key}"))
}

/// What the options bar shows for `key`, touched or at its default.
fn bar_float(shell: &Shell, key: &str) -> f32 {
    shell
        .chrome
        .workspace()
        .options
        .get(ToolId::FreeTransform, key)
        .and_then(|v| v.as_float())
        .unwrap_or_else(|| panic!("Free Transform has no {key}"))
}

fn composite(shell: &mut Shell) -> Vec<u8> {
    shell
        .editor
        .active_mut()
        .unwrap()
        .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
        .unwrap()
}

/// Opaque black pixels along row `y`.
fn black_in_row(rgba: &[u8], y: u32) -> usize {
    (0..SIDE)
        .filter(|x| {
            let i = ((y * SIDE + x) * 4) as usize;
            rgba[i] < 64 && rgba[i + 3] > 192
        })
        .count()
}

/// Opaque pixels along row `y`.
fn opaque_in_row(rgba: &[u8], y: u32) -> usize {
    (0..SIDE)
        .filter(|x| rgba[((y * SIDE + x) * 4 + 3) as usize] > 192)
        .count()
}

/// The menu row (or the chord) that starts the session, then W dragged
/// narrower in the options bar and Enter: the committed composite, the typed
/// W, and how many steps the commit added.
fn narrow_through(
    start: impl FnOnce(&egui::Context, &mut Shell),
    mode: TransformMode,
) -> (Vec<u8>, f32, usize) {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_bars(dir.path());
    let ctx = ctx();
    frame(&ctx, &mut shell, Vec::new());
    let original = composite(&mut shell);
    assert_eq!(
        black_in_row(&original, 50),
        8,
        "precondition: two 4 px bars"
    );
    start(&ctx, &mut shell);
    frame(&ctx, &mut shell, Vec::new());
    assert_eq!(shell.editor.tool(), ToolId::FreeTransform);
    assert_eq!(
        published_mode(&shell),
        Some(mode),
        "the gizmo is up in its mode, with no canvas press"
    );
    let before = shell.editor.active().unwrap().history_depth();
    drag_field(&ctx, &mut shell, keys::W, egui::vec2(-60.0, 0.0));
    let typed_w = held_float(&shell, keys::W);
    assert!(
        (40.0..95.0).contains(&typed_w),
        "the drag narrowed W: {typed_w}"
    );
    enter(&mut shell);
    assert!(
        !shell.pointer.has_pending_commit(),
        "Enter ended the session"
    );
    let steps = shell.editor.active().unwrap().history_depth() - before;
    (composite(&mut shell), typed_w, steps)
}

/// W10-J round 2: Edit > Content-Aware Scale > With Handles, from the menu
/// row, is Free Transform in its Content-Aware mode: the options bar shows
/// the Amount field (and Scale mode does not), the handles resize the box
/// (here the bar's W, a real drag), and Enter commits ONE undoable step in
/// which the bars keep their width — seam carving, not the plain scale the
/// same drag makes through Edit > Transform > Scale.
#[test]
fn content_aware_scale_with_handles_from_the_menu_commits_a_seam_carved_step() {
    let (carved, typed_w, steps) = narrow_through(
        |ctx, shell| {
            shell
                .chrome
                .emit(ui::Intent::Action(ui::MenuAction::ContentAwareScaleFree));
            frame(ctx, shell, Vec::new());
            assert!(
                field_drawn(ctx, shell, keys::CA_AMOUNT),
                "the options bar shows Amount in the Content-Aware mode"
            );
            assert_eq!(
                bar_float(shell, keys::CA_AMOUNT),
                100.0,
                "Amount starts at 100%"
            );
        },
        TransformMode::ContentAware,
    );
    assert_eq!(steps, 1, "one undo step");
    let width = (SIDE as f32 * typed_w / 100.0).round() as i64;
    let opaque = opaque_in_row(&carved, 50) as i64;
    assert!(
        (opaque - width).abs() <= 1,
        "row 50 is {opaque} px wide; W {typed_w}% of 100 is {width}"
    );
    assert_eq!(
        black_in_row(&carved, 50),
        8,
        "seam carving kept both bars whole"
    );

    // The Amount field reaches the commit: dragged down to 0% in the bar,
    // the same session is a plain scale and the bars thin.
    let (blended, _, steps) = narrow_through(
        |ctx, shell| {
            shell
                .chrome
                .emit(ui::Intent::Action(ui::MenuAction::ContentAwareScaleFree));
            frame(ctx, shell, Vec::new());
            drag_field(ctx, shell, keys::CA_AMOUNT, egui::vec2(-2000.0, 0.0));
            assert_eq!(
                bar_float(shell, keys::CA_AMOUNT),
                0.0,
                "Amount dragged to 0%"
            );
        },
        TransformMode::ContentAware,
    );
    assert_eq!(steps, 1);
    assert!(
        black_in_row(&blended, 50) < 8,
        "Amount 0% is a plain scale: {}",
        black_in_row(&blended, 50)
    );

    // The control: the same drag through Edit > Transform > Scale.
    let (scaled, typed_w, steps) = narrow_through(
        |ctx, shell| {
            shell
                .chrome
                .emit(ui::Intent::Action(ui::MenuAction::Transform(
                    ui::menu::TransformOp::Scale,
                )));
            frame(ctx, shell, Vec::new());
            assert!(
                !field_drawn(ctx, shell, keys::CA_AMOUNT),
                "Scale mode shows no Amount field"
            );
        },
        TransformMode::Scale,
    );
    assert_eq!(steps, 1);
    assert!(typed_w < 95.0);
    assert!(
        black_in_row(&scaled, 50) < 8,
        "a plain scale thins the bars: {}",
        black_in_row(&scaled, 50)
    );
}

/// W10-J round 2: the menu row's chord, Alt+Shift+Ctrl+C, through the key
/// path, starts the same Content-Aware session.
#[test]
fn alt_shift_ctrl_c_starts_content_aware_scale_with_handles() {
    let (carved, _, steps) = narrow_through(
        |ctx, shell| {
            shell.modifiers = ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SHIFT;
            shell.on_key(
                KeyboardOwner::default(),
                &WKey::Character("C".into()),
                ElementState::Pressed,
                false,
            );
            shell.modifiers = ModifiersState::empty();
            frame(ctx, shell, Vec::new());
        },
        TransformMode::ContentAware,
    );
    assert_eq!(steps, 1);
    assert_eq!(black_in_row(&carved, 50), 8, "seam carved");
}

/// W10-J round 2: File > New with the Artboard box ticked, as the confirmed
/// dialog hands it to the shell: the canvas is one artboard, "Artboard 1",
/// over the whole canvas in the chosen background, with an empty "Layer 1"
/// inside it to draw on, active; the document starts clean with nothing to
/// undo. The box unticked makes no artboard.
#[test]
fn a_new_document_with_the_artboard_box_is_one_canvas_sized_artboard() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_bars(dir.path());
    for (background, fill) in [
        (ui::dialogs::BackgroundContents::White, [1.0, 1.0, 1.0, 1.0]),
        (ui::dialogs::BackgroundContents::Transparent, [0.0; 4]),
    ] {
        for artboard in [true, false] {
            let spec = ui::dialogs::NewDocumentSpec {
                title: "Boards".to_string(),
                width: 120,
                height: 80,
                resolution_ppi: 72.0,
                color_mode: ui::dialogs::ColorMode::Rgb,
                color_space: color::ColorSpace::Srgb,
                bit_depth: raster::BitDepth::Eight,
                background,
                artboard,
            };
            shell.apply_chrome(ChromeOutput {
                dialog: Some(ui::dialogs::DialogAction::NewDocument(Box::new(spec))),
                ..Default::default()
            });
            let doc = shell.editor.active_mut().unwrap();
            assert_eq!(doc.title(), "Boards");
            let boards = layer_model::artboard::artboards(&doc.document.layers);
            if !artboard {
                assert!(boards.is_empty(), "unticked, no artboard: {boards:?}");
                continue;
            }
            assert_eq!(boards.len(), 1, "{background:?}: {boards:?}");
            let (group, board) = boards[0];
            assert_eq!(
                board,
                layer_model::Artboard {
                    x: 0,
                    y: 0,
                    width: 120,
                    height: 80,
                    background: fill,
                },
                "{background:?}"
            );
            let tree = &doc.document.layers;
            assert_eq!(tree.get(group).unwrap().name, "Artboard 1");
            let active = doc.document.active_layer().expect("an active layer");
            assert_eq!(tree.get(active).unwrap().name, "Layer 1");
            assert_eq!(tree.parent_of(active), Some(group), "inside the artboard");
            assert_eq!(doc.history_depth(), 0, "nothing to undo");
            assert!(!doc.is_dirty(), "a new document starts clean");
            let px = doc
                .composite(raster::PixelRect::new(0, 0, 120, 80))
                .unwrap();
            let expected: [u8; 4] = if fill[3] > 0.0 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            assert_eq!(&px[..4], &expected, "{background:?}");
        }
    }
}
