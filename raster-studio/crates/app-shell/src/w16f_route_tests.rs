//! W16-F: Free Transform's Ctrl gestures, Show Transform Controls' live
//! handles, and the arrow keys on an open transform, Path Select, Direct
//! Selection and Slice Select — each driven through the shell's own pointer
//! (`ToolPointer::handle`, the route every canvas sample takes) and its
//! arrow-key entry (`ToolPointer::nudge`, what the shell's `on_key` calls).

use glam::Vec2;
use tools::{Modifiers, ToolId, ToolSetting};
use ui::canvas::{PointerInput, PointerPhase};

use crate::action::NudgeDirection;
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::tool_input::ToolPointer;

const SIDE: u32 = 64;
const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);
const RED: [u8; 4] = [230, 20, 20, 255];

/// One opaque red 64x64 image at 100%, centred in the viewport.
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
    editor.open_path(&png).unwrap();
    let doc = editor.active_mut().unwrap();
    doc.set_viewport(VIEWPORT);
    doc.camera.zoom = 1.0;
    doc.camera.center = Vec2::splat(SIDE as f32 / 2.0);
    editor
}

fn screen(x: f32, y: f32) -> Vec2 {
    VIEWPORT * 0.5 + Vec2::new(x, y) - Vec2::splat(SIDE as f32 / 2.0)
}

/// Press at the first document point, move through the rest, release at the
/// last, with `m` held throughout. Answers the history steps it landed.
fn drag(
    pointer: &mut ToolPointer,
    editor: &mut Editor,
    pts: &[(f32, f32)],
    m: Modifiers,
    settings: &[(String, ToolSetting)],
) -> usize {
    let mut steps = 0;
    let mut send = |pointer: &mut ToolPointer, editor: &mut Editor, phase, (x, y): (f32, f32)| {
        let mut input = PointerInput::at(phase, screen(x, y));
        input.modifiers = m;
        steps += pointer.handle(editor, input, false, settings).steps;
    };
    send(pointer, editor, PointerPhase::Down, pts[0]);
    for p in &pts[1..] {
        send(pointer, editor, PointerPhase::Move, *p);
    }
    send(pointer, editor, PointerPhase::Up, *pts.last().unwrap());
    steps
}

fn composite(editor: &mut Editor) -> Vec<u8> {
    editor
        .active_mut()
        .unwrap()
        .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
        .unwrap()
}

fn alpha_at(rgba: &[u8], x: u32, y: u32) -> u8 {
    rgba[((y * SIDE + x) * 4 + 3) as usize]
}

fn live_corners(pointer: &mut ToolPointer) -> [Vec2; 4] {
    match pointer.live_geometry() {
        Some((_, tools::SessionGeometry::Transform { state, .. })) => state.corners,
        other => panic!("no live transform: {other:?}"),
    }
}

fn active_layer_transform(editor: &Editor) -> glam::Affine2 {
    let doc = &editor.active().unwrap().document;
    doc.layers
        .get(doc.active_layer().unwrap())
        .unwrap()
        .transform
}

const CTRL: Modifiers = Modifiers {
    shift: false,
    alt: false,
    ctrl: true,
};

#[test]
fn ctrl_dragging_a_corner_distorts_and_enter_lands_it_as_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::FreeTransform);
    let mut pointer = ToolPointer::new();
    // The top-left corner handle, Ctrl-dragged in to (16, 16).
    drag(
        &mut pointer,
        &mut editor,
        &[(0.0, 0.0), (16.0, 16.0)],
        CTRL,
        &[],
    );
    let corners = live_corners(&mut pointer);
    assert_eq!(corners[0], Vec2::new(16.0, 16.0), "the corner moved alone");
    assert_eq!(corners[1], Vec2::new(64.0, 0.0));
    assert_eq!(corners[3], Vec2::new(0.0, 64.0));
    let before = editor.active().unwrap().history_depth();
    let outcome = pointer.commit(&mut editor);
    assert_eq!(outcome.failed, None);
    assert_eq!(
        editor.active().unwrap().history_depth(),
        before + 1,
        "one step"
    );
    let rgba = composite(&mut editor);
    // Outside the distorted quad, near the old corner: cleared.
    assert_eq!(alpha_at(&rgba, 4, 4), 0);
    // Still inside it along the top edge (a uniform scale would have
    // emptied this pixel too).
    assert_eq!(alpha_at(&rgba, 60, 6), 255);
    assert_eq!(alpha_at(&rgba, 6, 60), 255);
    // The untouched bottom-right corner stays put: a free quad, which an
    // affine (Scale's parallelogram) commit would have pulled in to (48, 48).
    assert_eq!(alpha_at(&rgba, 60, 60), 255);
    assert_eq!(
        active_layer_transform(&editor),
        glam::Affine2::IDENTITY,
        "resampled into the pixels, not an affine layer transform"
    );
}

#[test]
fn ctrl_dragging_a_side_skews_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::FreeTransform);
    let mut pointer = ToolPointer::new();
    // The right side's handle, Ctrl-dragged 16 px down.
    drag(
        &mut pointer,
        &mut editor,
        &[(64.0, 32.0), (64.0, 48.0)],
        CTRL,
        &[],
    );
    let corners = live_corners(&mut pointer);
    assert_eq!(corners[1], Vec2::new(64.0, 16.0));
    assert_eq!(corners[2], Vec2::new(64.0, 80.0));
    assert_eq!(corners[0], Vec2::ZERO, "the left side stays");
    assert_eq!(corners[3], Vec2::new(0.0, 64.0), "the left side stays");
}

#[test]
fn a_show_transform_controls_corner_drag_scales_the_layer_in_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::Move);
    let settings = vec![("show_transform".to_string(), ToolSetting::Bool(true))];
    let mut pointer = ToolPointer::new();
    // The shell seeds the box between presses (every frame).
    pointer.begin_pending_session(&mut editor, &settings);
    let before = editor.active().unwrap().history_depth();
    let steps = drag(
        &mut pointer,
        &mut editor,
        &[(64.0, 64.0), (48.0, 48.0), (32.0, 32.0)],
        Modifiers::NONE,
        &settings,
    );
    assert_eq!(steps, 1, "the handle drag is one step");
    assert_eq!(editor.active().unwrap().history_depth(), before + 1);
    let m = active_layer_transform(&editor);
    assert!((m.matrix2.x_axis.x - 0.5).abs() < 1e-4, "{m:?}");
    assert!((m.matrix2.y_axis.y - 0.5).abs() < 1e-4, "{m:?}");
    assert!(
        m.translation.length() < 1e-3,
        "anchored at the top-left: {m:?}"
    );
}

#[test]
fn the_arrows_move_an_open_transform_box_and_enter_lands_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::FreeTransform);
    let mut pointer = ToolPointer::new();
    // A click inside the box opens the session and moves nothing.
    drag(
        &mut pointer,
        &mut editor,
        &[(20.0, 20.0)],
        Modifiers::NONE,
        &[],
    );
    assert_eq!(live_corners(&mut pointer)[0], Vec2::ZERO);
    assert_eq!(
        pointer.nudge(&mut editor, NudgeDirection::Right, false, false),
        Ok(1)
    );
    assert_eq!(
        pointer.nudge(&mut editor, NudgeDirection::Down, true, false),
        Ok(1)
    );
    assert_eq!(live_corners(&mut pointer)[0], Vec2::new(1.0, 10.0));
    let outcome = pointer.commit(&mut editor);
    assert_eq!(outcome.failed, None);
    let m = active_layer_transform(&editor);
    assert!(
        (m.translation - Vec2::new(1.0, 10.0)).length() < 1e-3,
        "{m:?}"
    );
}

/// Two square components: 10..20 and 40..50.
const TWO: &str = "M10 10 L20 10 L20 20 L10 20 Z M40 40 L50 40 L50 50 L40 50 Z";

fn add_shape(editor: &mut Editor, path: &str) -> layer_model::LayerId {
    let shape = layer_model::Layer::with_kind(
        "Shape",
        layer_model::LayerKind::Shape(layer_model::ShapeLayer::from_svg(path)),
    );
    let id = shape.id;
    editor.apply_command(editor_core::Command::create_layer(shape));
    id
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

#[test]
fn path_select_moves_nudges_reorders_and_deletes_one_component() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    let id = add_shape(&mut editor, TWO);
    editor.set_tool(ToolId::PathSelect);
    let mut pointer = ToolPointer::new();
    // Drag the second component 5 px right: only it moves.
    let steps = drag(
        &mut pointer,
        &mut editor,
        &[(45.0, 45.0), (50.0, 45.0)],
        Modifiers::NONE,
        &[],
    );
    assert_eq!(steps, 1);
    assert_eq!(
        mins(&path_of(&editor, id)),
        vec![(10.0, 10.0), (45.0, 40.0)]
    );
    // Shift+Right: ten more, one step.
    assert_eq!(
        pointer.nudge(&mut editor, NudgeDirection::Right, true, false),
        Ok(1)
    );
    assert_eq!(
        mins(&path_of(&editor, id)),
        vec![(10.0, 10.0), (55.0, 40.0)]
    );
    // The options bar's Send to Back: parked, then the confirm it raises.
    tools::path_select::request_component_op(tools::path_select::ComponentOp::SendToBack);
    let outcome = pointer.commit(&mut editor);
    assert_eq!(outcome.steps, 1);
    assert_eq!(
        mins(&path_of(&editor, id)),
        vec![(55.0, 40.0), (10.0, 10.0)]
    );
    // Delete removes the selected component only.
    tools::path_select::request_component_op(tools::path_select::ComponentOp::Delete);
    assert_eq!(pointer.commit(&mut editor).steps, 1);
    assert_eq!(mins(&path_of(&editor, id)), vec![(10.0, 10.0)]);
}

#[test]
fn direct_selection_arrows_nudge_the_shift_selected_knots() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    let id = add_shape(&mut editor, "M10 10 L50 10 L50 50 L10 50 Z");
    // Photopea: the shape is selected in the Layers panel first.
    editor.set_layer_selection(vec![id], Some(id));
    editor.set_tool(ToolId::DirectSelection);
    let mut pointer = ToolPointer::new();
    drag(
        &mut pointer,
        &mut editor,
        &[(10.0, 10.0)],
        Modifiers::NONE,
        &[],
    );
    drag(
        &mut pointer,
        &mut editor,
        &[(50.0, 10.0)],
        Modifiers::shift(),
        &[],
    );
    assert_eq!(
        pointer.nudge(&mut editor, NudgeDirection::Down, false, false),
        Ok(1)
    );
    let pts = vector::anchors::anchor_points(&path_of(&editor, id));
    assert_eq!(
        pts,
        vec![
            vector::Point::new(10.0, 11.0),
            vector::Point::new(50.0, 11.0),
            vector::Point::new(50.0, 50.0),
            vector::Point::new(10.0, 50.0),
        ]
    );
}

#[test]
fn slice_select_arrows_nudge_the_picked_slice_one_step_each() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    let doc = editor.active().unwrap().id();
    editor
        .slices
        .remember(doc, vec![raster::PixelRect::new(8, 8, 16, 16)]);
    editor.slices.set_picked(doc, Some(0));
    editor.set_tool(ToolId::SliceSelect);
    let mut pointer = ToolPointer::new();
    let before = editor.active().unwrap().history_depth();
    assert_eq!(
        pointer.nudge(&mut editor, NudgeDirection::Right, false, false),
        Ok(1)
    );
    assert_eq!(
        pointer.nudge(&mut editor, NudgeDirection::Down, true, false),
        Ok(1)
    );
    assert_eq!(editor.active().unwrap().history_depth(), before + 2);
    assert_eq!(
        editor.slices.get(doc),
        &[raster::PixelRect::new(9, 18, 16, 16)]
    );
}
