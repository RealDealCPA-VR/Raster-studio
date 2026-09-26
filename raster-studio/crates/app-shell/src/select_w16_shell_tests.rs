//! W16-A: the selection tools through the shell's own routes — `on_pointer`
//! (what winit's `MouseInput` / `CursorMoved` call), `on_modifiers` and
//! `on_key` — never through a helper:
//!
//! * every selection tool: a drag inside the selection (New mode, no
//!   modifier) moves the OUTLINE, live, pixels untouched, ONE `SetSelection`
//!   step; Shift constrains it to 45 degrees; Escape puts it back; a press
//!   outside still starts a new selection, and a Shift press inside still adds;
//! * Polygonal Lasso: Enter closes, a double-click closes, Backspace/Delete
//!   remove the last point (Delete does not clear pixels), one step each;
//! * Magnetic Lasso: click, then MOVE (no button) lays edge-snapped anchors,
//!   a click adds one, Backspace removes the last, Enter / double-click /
//!   a click on the start closes;
//! * Lasso: Alt during the drag draws straight segments, released with Alt
//!   the outline stays open for clicks, letting Alt go closes it.

use super::*;
use winit::keyboard::Key as WKey;

use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use editor_core::Selection;
use glam::IVec2;
use tools::ToolId;

const SIDE: u32 = 64;
/// The dark square the magnetic lasso snaps to: `[SQ0, SQ1)` on both axes.
const SQ0: u32 = 20;
const SQ1: u32 = 44;

fn shell_with_image(dir: &std::path::Path) -> Shell {
    // A dark square on a light field: an edge for the magnetic lasso, and
    // pixels a wrongly moved selection would show.
    let mut rgba = vec![0u8; (SIDE * SIDE * 4) as usize];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let i = ((y * SIDE + x) * 4) as usize;
            let dark = (SQ0..SQ1).contains(&x) && (SQ0..SQ1).contains(&y);
            let px = if dark {
                [20, 20, 20, 255]
            } else {
                [230, 230, 230, 255]
            };
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    let png = dir.join("a.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, SIDE, SIDE, &rgba).unwrap(),
    )
    .unwrap();
    let mut editor = crate::editor::Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    editor.open_path(&png).unwrap();
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(400.0, 300.0));
    {
        let doc = shell.editor.active_mut().unwrap();
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(SIDE as f32 / 2.0, SIDE as f32 / 2.0);
    }
    shell
}

/// Put the cursor over document point `doc`, through the camera the pointer
/// route itself maps with.
fn aim(shell: &mut Shell, doc: Vec2) {
    let open = shell.editor.active().unwrap();
    let viewport = crate::tool_input::canvas_viewport(&open.camera);
    let camera = crate::interaction_geometry::canvas_camera_of(&open.camera);
    shell.cursor = camera.screen_pt_of(&viewport, doc);
}

fn sample(shell: &mut Shell, phase: PointerPhase, doc: Vec2, mods: ModifiersState) {
    shell.on_modifiers(mods);
    aim(shell, doc);
    shell.on_pointer(phase, PointerButton::Primary, false);
}

fn down(shell: &mut Shell, x: f32, y: f32) {
    sample(
        shell,
        PointerPhase::Down,
        Vec2::new(x, y),
        ModifiersState::empty(),
    );
}

fn to(shell: &mut Shell, x: f32, y: f32) {
    sample(
        shell,
        PointerPhase::Move,
        Vec2::new(x, y),
        ModifiersState::empty(),
    );
}

fn up(shell: &mut Shell, x: f32, y: f32) {
    sample(
        shell,
        PointerPhase::Up,
        Vec2::new(x, y),
        ModifiersState::empty(),
    );
}

fn click(shell: &mut Shell, x: f32, y: f32) {
    down(shell, x, y);
    up(shell, x, y);
}

fn key(shell: &mut Shell, named: NamedKey) {
    shell.on_modifiers(ModifiersState::empty());
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Named(named),
        ElementState::Pressed,
        false,
    );
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Named(named),
        ElementState::Released,
        false,
    );
}

fn depth(shell: &Shell) -> usize {
    shell.editor.active().unwrap().history_depth()
}

fn selection(shell: &Shell) -> Selection {
    shell.editor.active().unwrap().document.selection.clone()
}

fn set_selection(shell: &mut Shell, min: IVec2, max: IVec2) {
    shell.editor.active_mut().unwrap().document.selection = Selection::Rect { min, max };
}

fn composite(shell: &mut Shell) -> Vec<u8> {
    shell
        .editor
        .active_mut()
        .unwrap()
        .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
        .unwrap()
}

fn covered(shell: &Shell, x: i32, y: i32) -> bool {
    selection(shell).coverage_at(IVec2::new(x, y)) > 0.5
}

/// The live lasso outline the shell publishes, if any.
fn lasso_points(shell: &mut Shell) -> Option<Vec<Vec2>> {
    match shell.pointer.live_geometry() {
        Some((_, tools::SessionGeometry::Lasso { points, .. })) => Some(points),
        _ => None,
    }
}

const SELECTION_TOOLS: [ToolId; 8] = [
    ToolId::RectMarquee,
    ToolId::EllipseMarquee,
    ToolId::Lasso,
    ToolId::PolygonalLasso,
    ToolId::MagneticLasso,
    ToolId::MagicWand,
    ToolId::QuickSelect,
    ToolId::ObjectSelection,
];

#[test]
fn a_drag_inside_the_selection_moves_the_outline_with_every_selection_tool() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let pixels = composite(&mut shell);
    for tool in SELECTION_TOOLS {
        shell.editor.set_tool(tool);
        let (min, max) = (IVec2::new(10, 10), IVec2::new(30, 30));
        set_selection(&mut shell, min, max);
        let before = depth(&shell);

        down(&mut shell, 20.0, 20.0);
        to(&mut shell, 22.0, 21.0);
        to(&mut shell, 25.0, 23.0);
        // Live: the ants are traced from the document's selection field.
        assert!(
            covered(&shell, 33, 31) && !covered(&shell, 11, 11),
            "{tool:?}: the outline did not follow the pointer mid-drag: {:?}",
            selection(&shell)
        );
        assert_eq!(depth(&shell), before, "{tool:?}: a step before the release");
        to(&mut shell, 27.0, 25.0);
        up(&mut shell, 27.0, 25.0);

        let offset = IVec2::new(7, 5);
        let moved = selection(&shell);
        assert!(
            covered(&shell, 17 + 1, 15 + 1)
                && covered(&shell, 36, 34)
                && !covered(&shell, 12, 12)
                && !covered(&shell, 38, 36),
            "{tool:?}: the outline did not land {offset:?} away: {moved:?}"
        );
        if let Selection::Rect { min: m, max: n } = moved {
            assert_eq!((m, n), (min + offset, max + offset), "{tool:?}");
        }
        assert_eq!(depth(&shell), before + 1, "{tool:?}: not ONE history step");
        assert_eq!(
            composite(&mut shell),
            pixels,
            "{tool:?}: moving the outline moved pixels"
        );
        assert!(
            shell.pointer.live_geometry().is_none(),
            "{tool:?}: the tool's own gesture was left running"
        );
        shell.editor.active_mut().unwrap().undo().unwrap();
        assert_eq!(
            selection(&shell),
            Selection::Rect { min, max },
            "{tool:?}: one undo did not put the outline back"
        );
    }
}

#[test]
fn shift_during_an_outline_drag_constrains_it_to_45_degrees() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::RectMarquee);
    let (min, max) = (IVec2::new(10, 10), IVec2::new(30, 30));
    for (end, expect) in [
        (Vec2::new(32.0, 23.0), IVec2::new(12, 0)),
        (Vec2::new(28.0, 30.0), IVec2::new(9, 9)),
        (Vec2::new(21.0, 34.0), IVec2::new(0, 14)),
    ] {
        set_selection(&mut shell, min, max);
        down(&mut shell, 20.0, 20.0);
        to(&mut shell, 24.0, 22.0);
        sample(&mut shell, PointerPhase::Move, end, ModifiersState::SHIFT);
        sample(&mut shell, PointerPhase::Up, end, ModifiersState::SHIFT);
        assert_eq!(
            selection(&shell),
            Selection::Rect {
                min: min + expect,
                max: max + expect
            },
            "a Shift drag to {end:?} is not constrained to 45 degrees"
        );
    }
}

#[test]
fn escape_mid_drag_puts_the_outline_back_without_a_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::Lasso);
    let (min, max) = (IVec2::new(10, 10), IVec2::new(30, 30));
    set_selection(&mut shell, min, max);
    let before = depth(&shell);
    down(&mut shell, 20.0, 20.0);
    to(&mut shell, 30.0, 30.0);
    assert_ne!(selection(&shell), Selection::Rect { min, max });
    key(&mut shell, NamedKey::Escape);
    assert_eq!(selection(&shell), Selection::Rect { min, max });
    up(&mut shell, 30.0, 30.0);
    assert_eq!(selection(&shell), Selection::Rect { min, max });
    assert_eq!(depth(&shell), before);
}

#[test]
fn a_press_outside_or_with_shift_still_selects_and_does_not_move_the_outline() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::RectMarquee);
    let (min, max) = (IVec2::new(10, 10), IVec2::new(30, 30));
    set_selection(&mut shell, min, max);
    // Outside: a new rectangle replaces the selection.
    down(&mut shell, 40.0, 40.0);
    to(&mut shell, 45.0, 45.0);
    up(&mut shell, 50.0, 50.0);
    assert!(
        covered(&shell, 45, 45) && !covered(&shell, 20, 20),
        "a press outside did not start a new selection: {:?}",
        selection(&shell).bounds()
    );
    // Shift inside: Add — the old outline stays where it was.
    set_selection(&mut shell, min, max);
    sample(
        &mut shell,
        PointerPhase::Down,
        Vec2::new(20.0, 20.0),
        ModifiersState::SHIFT,
    );
    sample(
        &mut shell,
        PointerPhase::Move,
        Vec2::new(40.0, 40.0),
        ModifiersState::SHIFT,
    );
    sample(
        &mut shell,
        PointerPhase::Up,
        Vec2::new(40.0, 40.0),
        ModifiersState::SHIFT,
    );
    assert!(covered(&shell, 11, 11), "Shift inside moved the outline");
    assert!(covered(&shell, 38, 38), "Shift inside did not add");
}

#[test]
fn enter_closes_a_polygonal_lasso_as_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::PolygonalLasso);
    let before = depth(&shell);
    click(&mut shell, 10.0, 10.0);
    click(&mut shell, 50.0, 10.0);
    click(&mut shell, 50.0, 50.0);
    click(&mut shell, 10.0, 50.0);
    assert_eq!(selection(&shell), Selection::None, "closed before Enter");
    key(&mut shell, NamedKey::Enter);
    assert!(covered(&shell, 30, 30) && !covered(&shell, 5, 5));
    assert_eq!(depth(&shell), before + 1);
    assert!(lasso_points(&mut shell).is_none(), "the outline stayed up");
}

#[test]
fn a_double_click_closes_a_polygonal_lasso() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::PolygonalLasso);
    let before = depth(&shell);
    click(&mut shell, 10.0, 10.0);
    click(&mut shell, 50.0, 10.0);
    click(&mut shell, 50.0, 50.0);
    click(&mut shell, 10.0, 50.0);
    click(&mut shell, 10.0, 50.0);
    assert!(
        covered(&shell, 30, 30) && !covered(&shell, 5, 5),
        "a double-click did not close the outline: {:?}",
        selection(&shell)
    );
    assert_eq!(depth(&shell), before + 1);
    assert!(lasso_points(&mut shell).is_none());
}

#[test]
fn backspace_and_delete_remove_the_polygonal_lassos_last_point() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::PolygonalLasso);
    let pixels = composite(&mut shell);
    let before = depth(&shell);
    click(&mut shell, 10.0, 10.0);
    click(&mut shell, 50.0, 10.0);
    click(&mut shell, 50.0, 50.0);
    click(&mut shell, 60.0, 60.0);
    click(&mut shell, 62.0, 30.0);
    assert_eq!(lasso_points(&mut shell).map(|p| p.len()), Some(5));
    key(&mut shell, NamedKey::Backspace);
    assert_eq!(lasso_points(&mut shell).map(|p| p.len()), Some(4));
    shell.chrome.workspace_for_test().drain_intents();
    key(&mut shell, NamedKey::Delete);
    assert_eq!(lasso_points(&mut shell).map(|p| p.len()), Some(3));
    assert_eq!(
        shell.chrome.workspace_for_test().drain_intents(),
        Vec::new(),
        "Delete also went to the keymap's Clear"
    );
    assert_eq!(composite(&mut shell), pixels, "Delete cleared pixels");
    assert_eq!(depth(&shell), before, "a removed point is not a step");
    click(&mut shell, 10.0, 50.0);
    key(&mut shell, NamedKey::Enter);
    // The square (10,10)-(50,50): the removed (60,60) and (62,30) are gone.
    assert!(covered(&shell, 30, 30));
    assert!(!covered(&shell, 55, 40), "a removed point still shaped it");
    assert_eq!(depth(&shell), before + 1);
    // Backspace past the first point ends the outline.
    click(&mut shell, 5.0, 5.0);
    key(&mut shell, NamedKey::Backspace);
    assert!(lasso_points(&mut shell).is_none());
}

#[test]
fn the_magnetic_lasso_is_click_then_move_with_snapped_anchors() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::MagneticLasso);
    let before = depth(&shell);
    // Click just outside the square's top-left corner, release, then MOVE
    // (no button) along its edges, two pixels off them.
    click(&mut shell, 18.0, 18.0);
    let start = lasso_points(&mut shell).expect("the click started an outline");
    assert_eq!(start.len(), 1);
    for x in [22.0, 26.0, 30.0, 34.0, 38.0, 42.0, 46.0] {
        to(&mut shell, x, 18.0);
    }
    let laid = lasso_points(&mut shell).unwrap();
    let on_top: Vec<&Vec2> = laid[1..]
        .iter()
        .filter(|p| p.x > 24.0 && p.x < 40.0)
        .collect();
    assert!(
        !on_top.is_empty(),
        "moving the pointer laid no anchor along the top edge: {laid:?}"
    );
    // Edge-snapped: the anchors sit on the square's top edge rows (19/20),
    // not on the pointer's row 18.
    for p in on_top {
        assert!(
            (p.y - 20.0).abs() <= 1.0,
            "anchor {p:?} was not pulled onto the edge"
        );
    }
    for y in [22.0, 26.0, 30.0, 34.0, 38.0, 42.0, 46.0] {
        to(&mut shell, 46.0, y);
    }
    // A click adds an anchor; Backspace removes it again.
    let n = lasso_points(&mut shell).unwrap().len();
    click(&mut shell, 30.0, 46.0);
    assert_eq!(lasso_points(&mut shell).unwrap().len(), n + 1);
    key(&mut shell, NamedKey::Backspace);
    assert_eq!(lasso_points(&mut shell).unwrap().len(), n);
    for x in [42.0, 38.0, 34.0, 30.0, 26.0, 22.0, 18.0] {
        to(&mut shell, x, 46.0);
    }
    for y in [42.0, 38.0, 34.0, 30.0, 26.0, 22.0] {
        to(&mut shell, 18.0, y);
    }
    assert_eq!(selection(&shell), Selection::None, "closed before Enter");
    key(&mut shell, NamedKey::Enter);
    assert!(
        covered(&shell, 32, 32) && covered(&shell, 22, 22) && !covered(&shell, 8, 8),
        "the magnetic outline lost the square: {:?}",
        selection(&shell).bounds()
    );
    assert_eq!(depth(&shell), before + 1);
    assert!(lasso_points(&mut shell).is_none());
}

#[test]
fn the_magnetic_lasso_closes_on_a_double_click_or_a_click_on_its_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::MagneticLasso);
    let trace = |shell: &mut Shell| {
        click(shell, 18.0, 18.0);
        for x in [26.0, 34.0, 42.0, 46.0] {
            to(shell, x, 18.0);
        }
        for y in [26.0, 34.0, 42.0, 46.0] {
            to(shell, 46.0, y);
        }
        for x in [38.0, 30.0, 22.0, 18.0] {
            to(shell, x, 46.0);
        }
    };
    let before = depth(&shell);
    trace(&mut shell);
    click(&mut shell, 18.0, 40.0);
    click(&mut shell, 18.0, 40.0);
    assert!(covered(&shell, 32, 32), "a double-click did not close");
    assert_eq!(depth(&shell), before + 1);
    assert!(lasso_points(&mut shell).is_none());

    shell.editor.active_mut().unwrap().undo().unwrap();
    trace(&mut shell);
    to(&mut shell, 18.0, 30.0);
    click(&mut shell, 22.5, 17.0);
    assert!(
        covered(&shell, 32, 32),
        "a click on the start did not close"
    );
    assert_eq!(depth(&shell), before + 1);
}

#[test]
fn alt_during_a_lasso_drag_draws_straight_segments_until_alt_is_let_go() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::Lasso);
    let before = depth(&shell);
    let alt = ModifiersState::ALT;
    down(&mut shell, 10.0, 10.0);
    to(&mut shell, 20.0, 10.0);
    // Alt pressed mid-drag: the pointer wobbles, the segment stays straight.
    sample(&mut shell, PointerPhase::Move, Vec2::new(30.0, 2.0), alt);
    sample(&mut shell, PointerPhase::Move, Vec2::new(50.0, 10.0), alt);
    // Released with Alt held: the outline stays open.
    sample(&mut shell, PointerPhase::Up, Vec2::new(50.0, 10.0), alt);
    assert_eq!(
        selection(&shell),
        Selection::None,
        "released with Alt, closed"
    );
    assert!(lasso_points(&mut shell).is_some());
    // Clicks with Alt held place straight corners.
    sample(&mut shell, PointerPhase::Down, Vec2::new(50.0, 50.0), alt);
    sample(&mut shell, PointerPhase::Up, Vec2::new(50.0, 50.0), alt);
    sample(&mut shell, PointerPhase::Down, Vec2::new(10.0, 50.0), alt);
    sample(&mut shell, PointerPhase::Up, Vec2::new(10.0, 50.0), alt);
    let points = lasso_points(&mut shell).unwrap();
    assert!(
        !points.iter().any(|p| p.y < 9.0),
        "the Alt segment followed the wobble: {points:?}"
    );
    // Letting Alt go closes it.
    to(&mut shell, 10.0, 40.0);
    assert!(
        covered(&shell, 30, 30) && !covered(&shell, 30, 5),
        "letting Alt go did not close the outline: {:?}",
        selection(&shell).bounds()
    );
    assert_eq!(depth(&shell), before + 1);
    assert!(lasso_points(&mut shell).is_none());
}

#[test]
fn escape_drops_an_open_polygonal_or_magnetic_outline_and_keeps_the_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let (min, max) = (IVec2::new(2, 2), IVec2::new(8, 8));
    for tool in [ToolId::PolygonalLasso, ToolId::MagneticLasso] {
        shell.editor.set_tool(tool);
        set_selection(&mut shell, min, max);
        let before = depth(&shell);
        // Presses outside the selection: each adds a point, none is a drag.
        click(&mut shell, 12.0, 12.0);
        click(&mut shell, 50.0, 12.0);
        to(&mut shell, 50.0, 30.0);
        click(&mut shell, 50.0, 50.0);
        assert!(
            lasso_points(&mut shell).is_some_and(|p| p.len() >= 3),
            "{tool:?}: no open outline to cancel"
        );
        key(&mut shell, NamedKey::Escape);
        assert!(
            lasso_points(&mut shell).is_none(),
            "{tool:?}: Escape left the outline up"
        );
        assert_eq!(
            selection(&shell),
            Selection::Rect { min, max },
            "{tool:?}: Escape changed the selection"
        );
        assert_eq!(depth(&shell), before, "{tool:?}: Escape took a step");
        // Nothing left for Enter to close.
        key(&mut shell, NamedKey::Enter);
        assert_eq!(selection(&shell), Selection::Rect { min, max });
        assert_eq!(
            depth(&shell),
            before,
            "{tool:?}: Enter closed a dropped outline"
        );
    }
}

#[test]
fn delete_with_no_open_lasso_outline_still_reaches_the_keymaps_clear() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::PolygonalLasso);
    set_selection(&mut shell, IVec2::new(20, 20), IVec2::new(30, 30));
    shell.chrome.workspace_for_test().drain_intents();
    key(&mut shell, NamedKey::Delete);
    assert_eq!(
        shell.chrome.workspace_for_test().drain_intents(),
        vec![ui::Intent::Action(ui::menu::MenuAction::ClearPixels)],
        "Delete was swallowed by the lasso route with no outline open"
    );
}
