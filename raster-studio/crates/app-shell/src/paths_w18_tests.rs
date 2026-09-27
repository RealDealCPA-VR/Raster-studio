//! W18-E: the path tools' gaps, each driven through the shell's own routes:
//! the canvas pointer (`ToolPointer::handle`), the keymap's Delete chord
//! raised into the chrome (`Chrome::emit`, what `Shell::perform_menu_chord`
//! does) and the frame's menu picks performed (`menu_bridge::perform`, what
//! `Shell::apply_chrome` does), and headless chrome frames whose painted
//! shapes and clicked options-bar buttons are read back.

use glam::Vec2;
use tools::{Modifiers, ToolId};
use ui::canvas::{PointerInput, PointerPhase};

use crate::chrome::{install_theme, Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::tool_input::ToolPointer;

const SIDE: u32 = 64;
const RED: [u8; 4] = [230, 20, 20, 255];
/// Two square components: 10..20 and 40..50.
const TWO: &str = "M10 10 L20 10 L20 20 L10 20 Z M40 40 L50 40 L50 50 L40 50 Z";
const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);

/// An opaque red 64x64 image, the camera at 4x so a knot is 4 screen points
/// from the next pixel.
fn editor(dir: &std::path::Path) -> Editor {
    let png = dir.join("red.png");
    let rgba: Vec<u8> = RED.repeat((SIDE * SIDE) as usize);
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, SIDE, SIDE, &rgba).unwrap(),
    )
    .unwrap();
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    editor.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
    editor.open_path(&png).unwrap();
    let doc = editor.active_mut().unwrap();
    doc.set_viewport(Vec2::new(SCREEN.x, SCREEN.y));
    doc.camera.zoom = 4.0;
    doc.camera.center = Vec2::splat(SIDE as f32 / 2.0);
    editor
}

fn add_shape(editor: &mut Editor, path: &str) -> layer_model::LayerId {
    let shape = layer_model::Layer::with_kind(
        "Shape",
        layer_model::LayerKind::Shape(layer_model::ShapeLayer::from_svg(path)),
    );
    let id = shape.id;
    editor.apply_command(editor_core::Command::create_layer(shape));
    editor.set_layer_selection(vec![id], Some(id));
    id
}

/// Where document point `(x, y)` is on screen, through the camera the pointer
/// route and the overlay both map with.
fn screen(editor: &Editor, x: f32, y: f32) -> Vec2 {
    let open = editor.active().unwrap();
    let viewport = crate::tool_input::canvas_viewport(&open.camera);
    let camera = crate::tool_input::canvas_camera_of(&open.camera);
    camera.screen_pt_of(&viewport, Vec2::new(x, y))
}

/// A click at document `(x, y)` through the shell's pointer route.
fn click(pointer: &mut ToolPointer, editor: &mut Editor, x: f32, y: f32, m: Modifiers) {
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let mut input = PointerInput::at(phase, screen(editor, x, y));
        input.modifiers = m;
        pointer.handle(editor, input, false, &[]);
    }
}

fn path_of(editor: &Editor, id: layer_model::LayerId) -> vector::Path {
    let doc = &editor.active().unwrap().document;
    let layer_model::LayerKind::Shape(shape) = &doc.layers.get(id).unwrap().kind else {
        panic!("not a shape");
    };
    vector::svg::parse(&shape.path_svg).unwrap()
}

fn mins(path: &vector::Path) -> Vec<(f64, f64)> {
    tools::path_select::components(path)
        .iter()
        .map(|p| (p.bounds().min.x, p.bounds().min.y))
        .collect()
}

fn depth(editor: &Editor) -> usize {
    editor.active().unwrap().history_depth()
}

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
        events,
        ..Default::default()
    }
}

/// One chrome frame (two passes: egui learns the sizes on the first) with
/// `events` on the second; the second pass's painted shapes (flattened) and
/// both passes' commands and menu picks.
fn frame(
    ctx: &egui::Context,
    chrome: &mut Chrome,
    editor: &mut Editor,
    events: Vec<egui::Event>,
) -> (Vec<egui::Shape>, ChromeOutput) {
    let mut shapes = Vec::new();
    let mut out = ChromeOutput::default();
    for pass in 0..2 {
        let input = raw_input(if pass == 1 {
            events.clone()
        } else {
            Vec::new()
        });
        let mut this = ChromeOutput::default();
        let full = ctx.run(input, |ctx| {
            this = chrome.ui(ctx, editor);
        });
        out.commands.extend(this.commands);
        out.menu.extend(this.menu);
        shapes = flat(full.shapes.iter().map(|c| c.shape.clone()).collect());
    }
    (shapes, out)
}

fn flat(shapes: Vec<egui::Shape>) -> Vec<egui::Shape> {
    let mut out = Vec::new();
    for s in shapes {
        match s {
            egui::Shape::Vec(inner) => out.extend(flat(inner)),
            other => out.push(other),
        }
    }
    out
}

fn themed() -> egui::Context {
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    ctx
}

/// Perform what a frame's output asks of the editor, as `Shell::apply_chrome`
/// does: its commands, then its menu picks.
fn apply(editor: &mut Editor, out: ChromeOutput) -> Vec<Result<String, String>> {
    for command in out.commands {
        editor.apply_command(command);
    }
    out.menu
        .into_iter()
        .map(|action| crate::menu_bridge::perform(action, editor))
        .collect()
}

/// The Delete key: the keymap's chord, raised into the chrome as the shell
/// does, the frame's picks performed.
fn press_delete(chrome: &mut Chrome, editor: &mut Editor) -> Vec<Result<String, String>> {
    let keymap = crate::keymap::Keymap::default();
    let action = keymap
        .menu_action_for(&crate::keymap::Chord::plain(crate::keymap::Key::Delete))
        .expect("Delete is bound");
    assert_eq!(action, ui::menu::MenuAction::ClearPixels);
    chrome.emit(ui::Intent::Action(action));
    let ctx = themed();
    let (_, out) = frame(&ctx, chrome, editor, Vec::new());
    assert_eq!(out.menu, vec![action], "Delete reached the frame's picks");
    apply(editor, out)
}

fn composite_alpha(editor: &mut Editor, x: u32, y: u32) -> [u8; 4] {
    let rgba = editor
        .active_mut()
        .unwrap()
        .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
        .unwrap();
    let i = ((y * SIDE + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

#[test]
fn delete_with_path_select_removes_the_selected_component_in_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let id = add_shape(&mut ed, TWO);
    ed.set_tool(ToolId::PathSelect);
    let mut pointer = ToolPointer::new();
    let mut chrome = Chrome::new();
    click(&mut pointer, &mut ed, 45.0, 45.0, Modifiers::NONE);
    let before = depth(&ed);
    let outcome = press_delete(&mut chrome, &mut ed);
    assert_eq!(outcome, vec![Ok("Deleted 1 path component(s)".to_string())]);
    assert_eq!(depth(&ed), before + 1, "one step");
    assert_eq!(
        mins(&path_of(&ed, id)),
        vec![(10.0, 10.0)],
        "the other stays"
    );
    // The pixels were not cleared: the red image shows everywhere the
    // remaining square does not cover.
    assert_eq!(composite_alpha(&mut ed, 60, 60), RED);
    assert_eq!(composite_alpha(&mut ed, 45, 45), RED);
    // One undo brings the component back.
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(mins(&path_of(&ed, id)), vec![(10.0, 10.0), (40.0, 40.0)]);
    // Nothing selected any more: the key goes back to Edit > Clear.
    assert_eq!(tools::path_select::selected_path_parts().components, None);
}

#[test]
fn delete_with_direct_selection_removes_the_selected_knot() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let id = add_shape(&mut ed, TWO);
    ed.set_tool(ToolId::DirectSelection);
    let mut pointer = ToolPointer::new();
    let mut chrome = Chrome::new();
    click(&mut pointer, &mut ed, 20.0, 10.0, Modifiers::NONE);
    let before = depth(&ed);
    let outcome = press_delete(&mut chrome, &mut ed);
    assert_eq!(outcome, vec![Ok("Deleted 1 knot(s)".to_string())]);
    assert_eq!(depth(&ed), before + 1, "one step");
    let knots = vector::anchors::from_path(&path_of(&ed, id));
    assert_eq!(knots[0].anchors.len(), 3, "the first square lost a knot");
    assert!(knots[0]
        .anchors
        .iter()
        .all(|a| a.pos != vector::Point::new(20.0, 10.0)));
    assert_eq!(knots[1].anchors.len(), 4);
    assert_eq!(composite_alpha(&mut ed, 60, 60), RED);
}

/// Every filled square of the anchor size centred near `at`, with its fill.
fn knot_fills(shapes: &[egui::Shape], at: Vec2) -> Vec<egui::Color32> {
    let side = design::Space::XSmall.pt() * 1.5;
    shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r)
                if (r.rect.center().x - at.x).abs() < 1.0
                    && (r.rect.center().y - at.y).abs() < 1.0
                    && (r.rect.width() - side).abs() < 0.5
                    && r.fill != egui::Color32::TRANSPARENT =>
            {
                Some(r.fill)
            }
            _ => None,
        })
        .collect()
}

/// Whether a stroked polyline of `color` passes through screen point `at`.
fn outline_through(shapes: &[egui::Shape], color: egui::Color32, at: Vec2) -> bool {
    shapes.iter().any(|s| match s {
        egui::Shape::Path(p) => {
            p.stroke.color == egui::epaint::ColorMode::Solid(color)
                && p.points
                    .iter()
                    .any(|q| (q.x - at.x).abs() < 1.0 && (q.y - at.y).abs() < 1.0)
        }
        egui::Shape::LineSegment { points, stroke } => {
            stroke.color == egui::epaint::ColorMode::Solid(color)
                && points
                    .iter()
                    .any(|q| (q.x - at.x).abs() < 1.0 && (q.y - at.y).abs() < 1.0)
        }
        _ => false,
    })
}

#[test]
fn the_selected_knot_is_drawn_filled_and_show_paths_hides_the_outline() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    add_shape(&mut ed, TWO);
    ed.set_tool(ToolId::DirectSelection);
    let mut pointer = ToolPointer::new();
    let mut chrome = Chrome::new();
    click(&mut pointer, &mut ed, 20.0, 10.0, Modifiers::NONE);
    let ctx = themed();
    let (shapes, _) = frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let style = ui::canvas::CanvasStyle::from_context(&ctx);
    let selected = screen(&ed, 20.0, 10.0);
    let other = screen(&ed, 50.0, 50.0);
    assert_eq!(
        knot_fills(&shapes, selected),
        vec![style.path_anchor_selected],
        "the selected knot is filled"
    );
    assert_eq!(
        knot_fills(&shapes, other),
        vec![style.path_anchor],
        "an unselected knot is hollow"
    );
    assert!(
        outline_through(&shapes, style.path_stroke, screen(&ed, 40.0, 40.0)),
        "the path's outline is drawn"
    );

    // Path Select: the selected component's knots are all filled, the other
    // component's are not drawn.
    ed.set_tool(ToolId::PathSelect);
    let mut pointer = ToolPointer::new();
    click(&mut pointer, &mut ed, 45.0, 45.0, Modifiers::NONE);
    let (shapes, _) = frame(&ctx, &mut chrome, &mut ed, Vec::new());
    assert_eq!(knot_fills(&shapes, other), vec![style.path_anchor_selected]);
    assert!(knot_fills(&shapes, selected).is_empty());

    // View > Show > Paths off: no outline, no knots.
    chrome.emit(ui::Intent::SetViewFlag {
        flag: ui::ViewFlag::Paths,
        on: false,
    });
    let (hidden, _) = frame(&ctx, &mut chrome, &mut ed, Vec::new());
    assert!(!chrome.workspace().view_flags.get(ui::ViewFlag::Paths));
    assert!(!outline_through(
        &hidden,
        style.path_stroke,
        screen(&ed, 40.0, 40.0)
    ));
    assert!(knot_fills(&hidden, other).is_empty());
}

/// Press and release the primary button on the widget `id` drew last.
fn click_widget(
    ctx: &egui::Context,
    chrome: &mut Chrome,
    editor: &mut Editor,
    id: egui::Id,
) -> ChromeOutput {
    let (_, _) = frame(ctx, chrome, editor, Vec::new());
    let rect = ctx
        .read_response(id)
        .unwrap_or_else(|| panic!("{id:?} was not drawn"))
        .rect;
    assert!(
        egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN).contains_rect(rect),
        "{id:?} is drawn off a {SCREEN:?} window at {rect:?}"
    );
    let button = |pressed: bool| egui::Event::PointerButton {
        pos: rect.center(),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let full = ctx.run(
        raw_input(vec![egui::Event::PointerMoved(rect.center()), button(true)]),
        |ctx| {
            let _ = chrome.ui(ctx, editor);
        },
    );
    drop(full);
    let mut out = ChromeOutput::default();
    let _ = ctx.run(raw_input(vec![button(false)]), |ctx| {
        out = chrome.ui(ctx, editor);
    });
    // The request parked by the click is answered on the next frame at the
    // latest; fold that frame's output in.
    let mut next = ChromeOutput::default();
    let _ = ctx.run(raw_input(Vec::new()), |ctx| {
        next = chrome.ui(ctx, editor);
    });
    out.commands.extend(next.commands);
    out.menu.extend(next.menu);
    out
}

#[test]
fn the_pen_bar_makes_a_selection_a_vector_mask_and_a_shape() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let background = ed.active().unwrap().document.active_layer().unwrap();
    let path_layer = add_shape(&mut ed, "M8 8 L24 8 L24 24 L8 24 Z");
    ed.set_tool(ToolId::Pen);
    let mut chrome = Chrome::new();
    let ctx = themed();
    let id = |key| ui::view::ids::tool_option(ToolId::Pen, key);

    // No current path: the buttons are greyed and a click does nothing.
    let out = click_widget(&ctx, &mut chrome, &mut ed, id("make_selection"));
    assert!(out.commands.is_empty(), "{:?}", out.commands);
    assert_eq!(tools::path_select::take_pen_make(), None);

    // The path selected in the Paths panel is the current path.
    chrome.workspace_for_test().paths.selected = Some(path_layer);
    let out = click_widget(&ctx, &mut chrome, &mut ed, id("make_selection"));
    let results = apply(&mut ed, out);
    assert!(results.is_empty());
    assert_eq!(
        ed.active().unwrap().document.selection.bounds(),
        Some((glam::IVec2::new(8, 8), glam::IVec2::new(24, 24))),
        "Make Selection loaded the path"
    );

    let layers_before = ed
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .len();
    let out = click_widget(&ctx, &mut chrome, &mut ed, id("make_shape"));
    apply(&mut ed, out);
    let doc = &ed.active().unwrap().document;
    assert_eq!(doc.layers.iter_depth_first().len(), layers_before + 1);
    let made = doc.active_layer().unwrap();
    let layer_model::LayerKind::Shape(shape) = &doc.layers.get(made).unwrap().kind else {
        panic!("Make Shape made no shape layer");
    };
    assert!(shape.fill.is_some(), "the shape is filled");
    assert_eq!(
        mins(&vector::svg::parse(&shape.path_svg).unwrap()),
        vec![(8.0, 8.0)]
    );

    // Mask: Layer > Vector Mask > Current Path on the active layer.
    ed.set_layer_selection(vec![background], Some(background));
    chrome.workspace_for_test().paths.selected = Some(path_layer);
    let out = click_widget(&ctx, &mut chrome, &mut ed, id("make_mask"));
    assert_eq!(
        out.menu,
        vec![ui::menu::MenuAction::VectorMask(
            ui::menu::VectorMaskOp::CurrentPath
        )]
    );
    let results = apply(&mut ed, out);
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
    let doc = &ed.active().unwrap().document;
    let mask = doc.layers.get(background).unwrap().mask.clone();
    assert!(
        mask.is_some_and(|m| m.vector.is_some()),
        "the background carries a vector mask"
    );
}
