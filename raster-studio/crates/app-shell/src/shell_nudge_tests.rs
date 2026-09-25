//! W13X-1: the arrow keys nudge, driven through the shell's own key route —
//! `on_modifiers` + `on_key`, what winit's `ModifiersChanged` and
//! `KeyboardInput` call — never through a helper.
//!
//! * Move tool: an arrow moves the active layer 1 px, Shift+arrow 10 px,
//!   each press one history step;
//! * Move tool, Alt+arrow: the layer is duplicated and the COPY moves (N+1
//!   layers, the original stays), one step, one Undo takes it all back;
//! * Move tool with a pixel selection: the selected pixels move (the layer
//!   itself does not), the outline travels with them;
//! * a selection tool: the arrows move the outline only — no pixel, no layer;
//! * a text field holding the keyboard, or a Type-tool run's caret: no nudge;
//! * a held arrow's repeats keep nudging; Alt copies on the press only.

use super::*;
use winit::keyboard::Key as WKey;

use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use editor_core::Selection;
use glam::IVec2;
use tools::ToolId;

const SIDE: u32 = 64;

fn shell_with_image(dir: &std::path::Path) -> Shell {
    // A left half red, a right half blue: a moved pixel is visible.
    let mut rgba = vec![0u8; (SIDE * SIDE * 4) as usize];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let i = ((y * SIDE + x) * 4) as usize;
            let px = if x < SIDE / 2 {
                [220, 20, 20, 255]
            } else {
                [20, 20, 220, 255]
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
    editor.set_tool(ToolId::Move);
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(400.0, 300.0));
    shell
}

/// One arrow press and release through the shell, with `mods` held.
fn arrow(shell: &mut Shell, owner: KeyboardOwner, mods: ModifiersState, key: NamedKey) {
    shell.on_modifiers(mods);
    shell.on_key(owner, &WKey::Named(key), ElementState::Pressed, false);
    shell.on_key(owner, &WKey::Named(key), ElementState::Released, false);
    shell.on_modifiers(ModifiersState::empty());
}

fn press(shell: &mut Shell, mods: ModifiersState, key: NamedKey) {
    arrow(shell, KeyboardOwner::default(), mods, key);
}

fn active_layer(shell: &Shell) -> layer_model::LayerId {
    shell
        .editor
        .active()
        .unwrap()
        .document
        .active_layer()
        .expect("an active layer")
}

fn offset_of(shell: &Shell, layer: layer_model::LayerId) -> Vec2 {
    shell
        .editor
        .active()
        .unwrap()
        .document
        .layers
        .get(layer)
        .expect("the layer")
        .transform
        .translation
}

fn layer_count(shell: &Shell) -> usize {
    shell
        .editor
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .len()
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

fn pixel(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * SIDE + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

#[test]
fn an_arrow_moves_the_active_layer_one_pixel_each_press_a_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let layer = active_layer(&shell);
    let before = depth(&shell);
    press(&mut shell, ModifiersState::empty(), NamedKey::ArrowRight);
    assert_eq!(offset_of(&shell, layer), Vec2::new(1.0, 0.0));
    assert_eq!(depth(&shell), before + 1, "one nudge is one history step");
    press(&mut shell, ModifiersState::empty(), NamedKey::ArrowDown);
    press(&mut shell, ModifiersState::empty(), NamedKey::ArrowLeft);
    press(&mut shell, ModifiersState::empty(), NamedKey::ArrowDown);
    assert_eq!(offset_of(&shell, layer), Vec2::new(0.0, 2.0));
    assert_eq!(
        depth(&shell),
        before + 4,
        "each nudge is its own step (Photopea)"
    );
    // The first undo takes back the last nudge only.
    shell.editor.dispatch(Action::Undo).unwrap();
    assert_eq!(offset_of(&shell, layer), Vec2::new(0.0, 1.0));
}

#[test]
fn shift_and_an_arrow_moves_the_layer_ten_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let layer = active_layer(&shell);
    press(&mut shell, ModifiersState::SHIFT, NamedKey::ArrowDown);
    assert_eq!(offset_of(&shell, layer), Vec2::new(0.0, 10.0));
    press(&mut shell, ModifiersState::SHIFT, NamedKey::ArrowLeft);
    assert_eq!(offset_of(&shell, layer), Vec2::new(-10.0, 10.0));
}

#[test]
fn alt_and_an_arrow_nudges_a_copy_and_leaves_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let original = active_layer(&shell);
    let layers = layer_count(&shell);
    let before = depth(&shell);
    press(&mut shell, ModifiersState::ALT, NamedKey::ArrowLeft);
    assert_eq!(layer_count(&shell), layers + 1, "Alt+arrow made a copy");
    assert_eq!(
        offset_of(&shell, original),
        Vec2::ZERO,
        "the original stays where it was"
    );
    let copy = active_layer(&shell);
    assert_ne!(copy, original, "the copy is the active layer now");
    assert_eq!(offset_of(&shell, copy), Vec2::new(-1.0, 0.0));
    assert_eq!(depth(&shell), before + 1, "copy and move are ONE step");
    // Alt+Shift: a copy of the copy, ten pixels on.
    press(
        &mut shell,
        ModifiersState::ALT | ModifiersState::SHIFT,
        NamedKey::ArrowUp,
    );
    assert_eq!(layer_count(&shell), layers + 2);
    let second = active_layer(&shell);
    assert_eq!(offset_of(&shell, second), Vec2::new(-1.0, -10.0));
    assert_eq!(offset_of(&shell, copy), Vec2::new(-1.0, 0.0));
    shell.editor.dispatch(Action::Undo).unwrap();
    shell.editor.dispatch(Action::Undo).unwrap();
    assert_eq!(layer_count(&shell), layers, "two undos give the stack back");
}

#[test]
fn a_held_arrow_keeps_nudging_and_alt_copies_on_the_press_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let layers = layer_count(&shell);
    let key = WKey::Named(NamedKey::ArrowRight);
    let owner = KeyboardOwner::default();
    shell.on_modifiers(ModifiersState::ALT);
    shell.on_key(owner, &key, ElementState::Pressed, false);
    shell.on_key(owner, &key, ElementState::Pressed, true);
    shell.on_key(owner, &key, ElementState::Pressed, true);
    shell.on_key(owner, &key, ElementState::Released, false);
    shell.on_modifiers(ModifiersState::empty());
    assert_eq!(layer_count(&shell), layers + 1, "one copy, at the press");
    let copy = active_layer(&shell);
    assert_eq!(
        offset_of(&shell, copy),
        Vec2::new(3.0, 0.0),
        "the press and both repeats moved the copy"
    );
}

#[test]
fn the_move_tool_with_a_pixel_selection_moves_the_selected_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let layer = active_layer(&shell);
    // A 4x4 box straddling the red/blue border (x 30..34).
    set_selection(&mut shell, IVec2::new(30, 10), IVec2::new(34, 14));
    let before = composite(&mut shell);
    assert_eq!(pixel(&before, 34, 12), [20, 20, 220, 255]);
    let steps = depth(&shell);
    press(&mut shell, ModifiersState::SHIFT, NamedKey::ArrowRight);
    assert_eq!(depth(&shell), steps + 1, "one step");
    assert_eq!(
        offset_of(&shell, layer),
        Vec2::ZERO,
        "the layer itself does not move"
    );
    assert_eq!(
        selection(&shell).bounds(),
        Some((IVec2::new(40, 10), IVec2::new(44, 14))),
        "the marching ants travel with the pixels"
    );
    let after = composite(&mut shell);
    // The red column at x 30..32 landed at x 40..42.
    assert_eq!(pixel(&after, 40, 12), [220, 20, 20, 255]);
    assert_eq!(pixel(&after, 41, 12), [220, 20, 20, 255]);
    // Outside the box nothing changed.
    assert_eq!(pixel(&after, 10, 40), pixel(&before, 10, 40));
}

#[test]
fn with_a_selection_tool_the_arrows_move_the_outline_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::RectMarquee);
    let layer = active_layer(&shell);
    set_selection(&mut shell, IVec2::new(2, 2), IVec2::new(8, 8));
    let pixels = composite(&mut shell);
    let steps = depth(&shell);
    press(&mut shell, ModifiersState::empty(), NamedKey::ArrowRight);
    assert_eq!(
        selection(&shell),
        Selection::Rect {
            min: IVec2::new(3, 2),
            max: IVec2::new(9, 8)
        }
    );
    press(&mut shell, ModifiersState::SHIFT, NamedKey::ArrowDown);
    assert_eq!(
        selection(&shell),
        Selection::Rect {
            min: IVec2::new(3, 12),
            max: IVec2::new(9, 18)
        }
    );
    assert_eq!(depth(&shell), steps + 2, "each outline nudge is a step");
    assert_eq!(offset_of(&shell, layer), Vec2::ZERO, "no layer moved");
    assert_eq!(composite(&mut shell), pixels, "no pixel changed");
    // Alt with a selection tool copies nothing.
    let layers = layer_count(&shell);
    press(&mut shell, ModifiersState::ALT, NamedKey::ArrowLeft);
    assert_eq!(layer_count(&shell), layers);
    assert_eq!(
        selection(&shell),
        Selection::Rect {
            min: IVec2::new(2, 12),
            max: IVec2::new(8, 18)
        }
    );
    // Undo puts the outline back where the last nudge found it.
    shell.editor.dispatch(Action::Undo).unwrap();
    assert_eq!(
        selection(&shell),
        Selection::Rect {
            min: IVec2::new(3, 12),
            max: IVec2::new(9, 18)
        }
    );
}

#[test]
fn a_lasso_mask_outline_moves_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    shell.editor.set_tool(ToolId::Lasso);
    let mask =
        selection::marquee::ellipse(selection::rect::Rect::from_xywh(10, 10, 12, 8)).unwrap();
    shell.editor.active_mut().unwrap().document.selection = Selection::Mask(mask.clone());
    let layer = active_layer(&shell);
    press(&mut shell, ModifiersState::empty(), NamedKey::ArrowUp);
    let moved = selection(&shell);
    let (min, max) = moved.bounds().expect("still a selection");
    let mask = Selection::Mask(mask);
    let (omin, omax) = mask.bounds().unwrap();
    assert_eq!((min, max), (omin - IVec2::Y, omax - IVec2::Y));
    for y in 8..20 {
        for x in 8..24 {
            let p = IVec2::new(x, y);
            assert_eq!(
                moved.coverage_at(p - IVec2::Y),
                mask.coverage_at(p),
                "the outline moved whole pixels exactly, at {p:?}"
            );
        }
    }
    assert_eq!(offset_of(&shell, layer), Vec2::ZERO, "no layer moved");
}

#[test]
fn typing_in_a_panel_field_does_not_nudge() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let layer = active_layer(&shell);
    let steps = depth(&shell);
    let typing = KeyboardOwner {
        egui_text_focus: true,
        recording_shortcut: false,
    };
    for key in [
        NamedKey::ArrowLeft,
        NamedKey::ArrowRight,
        NamedKey::ArrowUp,
        NamedKey::ArrowDown,
    ] {
        arrow(&mut shell, typing, ModifiersState::empty(), key);
        arrow(&mut shell, typing, ModifiersState::ALT, key);
    }
    assert_eq!(offset_of(&shell, layer), Vec2::ZERO);
    assert_eq!(depth(&shell), steps);
    // The same keys with the canvas holding the keyboard do nudge.
    press(&mut shell, ModifiersState::empty(), NamedKey::ArrowUp);
    assert_eq!(offset_of(&shell, layer), Vec2::new(0.0, -1.0));
}

#[test]
fn the_arrows_do_not_nudge_while_a_type_run_holds_the_caret() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let layer = active_layer(&shell);
    // Open a Type-tool run with a click on the canvas, the user's way.
    shell.editor.set_tool(ToolId::Type);
    {
        let doc = shell.editor.active_mut().unwrap();
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(SIDE as f32 / 2.0, SIDE as f32 / 2.0);
    }
    shell.cursor = Vec2::new(200.0, 150.0);
    shell.on_pointer(PointerPhase::Down, PointerButton::Primary, false);
    shell.on_pointer(PointerPhase::Up, PointerButton::Primary, false);
    assert!(shell.pointer.is_text_editing(), "the click opened a run");
    let layers = layer_count(&shell);
    for key in [NamedKey::ArrowUp, NamedKey::ArrowDown, NamedKey::ArrowLeft] {
        press(&mut shell, ModifiersState::empty(), key);
        press(&mut shell, ModifiersState::SHIFT, key);
    }
    // Even with the Move tool lent by the palette mid-run, the caret keeps
    // the arrows.
    shell.editor.set_tool(ToolId::Move);
    press(&mut shell, ModifiersState::empty(), NamedKey::ArrowDown);
    assert_eq!(offset_of(&shell, layer), Vec2::ZERO, "no layer moved");
    assert_eq!(layer_count(&shell), layers, "no copy was made");
}
