//! W18-C: per-channel editing, driven through the real routes: the Channels
//! panel's own selection logic (`ChannelsState::click`, what a row click
//! runs), the per-frame menu context that mirrors it into the editor, then
//! the pointer route's stroke commit and the Filter menu's pixel pipeline.

use glam::Vec2;
use tools::ToolId;
use ui::canvas::{PointerInput, PointerPhase};
use ui::panels::channels::{component_kind, ChannelKind, ChannelModel};

use super::*;
use crate::dialogs::ScriptedDialogs;
use crate::menu_bridge::pixels;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::tool_input::ToolPointer;

const W: u32 = 64;
const H: u32 = 64;
const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);
/// The fixture's opaque base colour: every component is mid-range, so a
/// white stroke moves each of them.
const BASE: [u8; 4] = [40, 90, 150, 255];

fn editor(dir: &std::path::Path) -> Editor {
    let png = dir.join("canvas.png");
    let pixels: Vec<u8> = BASE
        .iter()
        .copied()
        .cycle()
        .take((W * H * 4) as usize)
        .collect();
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, W, H, &pixels).unwrap(),
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
    doc.camera.center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
    editor
}

fn screen(x: f32, y: f32) -> Vec2 {
    VIEWPORT * 0.5 + Vec2::new(x - W as f32 / 2.0, y - H as f32 / 2.0)
}

/// Press, drag through the document points, release: one stroke.
fn stroke(editor: &mut Editor, points: &[(f32, f32)]) {
    let mut pointer = ToolPointer::new();
    for (i, (x, y)) in points.iter().enumerate() {
        let phase = if i == 0 {
            PointerPhase::Down
        } else {
            PointerPhase::Move
        };
        pointer.handle(editor, PointerInput::at(phase, screen(*x, *y)), false, &[]);
    }
    let (x, y) = *points.last().unwrap();
    pointer.handle(
        editor,
        PointerInput::at(PointerPhase::Up, screen(x, y)),
        false,
        &[],
    );
}

/// A row click in the Channels panel (`ChannelsState::click`, the code the
/// panel's row runs), then the frame's menu context, which mirrors the
/// selection into the editor.
fn click_channel(editor: &mut Editor, ws: &mut ui::Workspace, row: ChannelKind, shift: bool) {
    let doc = editor.active().unwrap().document.clone();
    ws.channels.click(&doc, row, shift);
    let _ = crate::menu_bridge::context(editor, ws);
}

fn layer_pixels(editor: &Editor) -> Vec<u8> {
    let doc = editor.active().unwrap();
    pixels::read_layer(doc, doc.document.active_layer().unwrap())
}

fn layer_pixels16(editor: &Editor) -> Vec<u16> {
    let doc = editor.active().unwrap();
    doc.layer_rgba16(doc.document.active_layer().unwrap())
}

fn white_pencil(editor: &mut Editor) {
    editor.set_tool(ToolId::Pencil);
    editor.set_foreground([1.0, 1.0, 1.0, 1.0]);
}

/// Which components moved anywhere between two RGBA buffers.
fn moved<T: PartialEq + Copy>(before: &[T], after: &[T]) -> [bool; 4] {
    let mut out = [false; 4];
    for (b, a) in before.chunks(4).zip(after.chunks(4)) {
        for c in 0..4 {
            out[c] |= b[c] != a[c];
        }
    }
    out
}

fn blur(editor: &mut Editor) {
    let spec = ui::dialogs::filter_by_id(ui::menu::FilterId::GaussianBlur).unwrap();
    let invocation = ui::dialogs::FilterInvocation {
        filter: spec,
        params: ui::dialogs::FilterParams::defaults(spec.params),
    };
    crate::menu_bridge::run_filter_invocation(editor, &invocation).unwrap();
}

/// A diagonal of colour to give the blur edges to move.
fn paint_detail(editor: &mut Editor) {
    editor.set_tool(ToolId::Pencil);
    editor.set_foreground([0.9, 0.2, 0.7, 1.0]);
    stroke(editor, &[(8.0, 8.0), (56.0, 56.0)]);
    editor.set_foreground([0.1, 0.8, 0.3, 1.0]);
    stroke(editor, &[(8.0, 56.0), (56.0, 8.0)]);
}

#[test]
fn a_white_stroke_with_red_selected_changes_only_red_and_undo_restores() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let mut ws = ui::Workspace::new();
    click_channel(&mut ed, &mut ws, ChannelKind::Component(0), false);
    assert_eq!(
        ed.edit_channels(),
        Some(ChannelWrite {
            mask: 0b001,
            model: ChannelModel::Rgb
        })
    );
    white_pencil(&mut ed);
    let before = layer_pixels(&ed);
    let depth = ed.active().unwrap().history_depth();
    stroke(&mut ed, &[(10.0, 10.0), (50.0, 40.0)]);
    let after = layer_pixels(&ed);
    assert_eq!(
        moved(&before, &after),
        [true, false, false, false],
        "only red moved"
    );
    assert_eq!(ed.active().unwrap().history_depth(), depth + 1, "one step");
    assert!(ed.active_mut().unwrap().undo().unwrap());
    assert_eq!(layer_pixels(&ed), before, "undo restores every channel");
}

#[test]
fn shift_click_adds_blue_and_the_stroke_writes_red_and_blue_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let mut ws = ui::Workspace::new();
    click_channel(&mut ed, &mut ws, ChannelKind::Component(0), false);
    click_channel(&mut ed, &mut ws, ChannelKind::Component(2), true);
    assert_eq!(
        ws.channels.selected,
        ChannelKind::Components {
            mask: 0b101,
            model: ChannelModel::Rgb
        }
    );
    // The canvas shows the two selected components, as Photopea does.
    assert!(ws.channels.component_visible(0));
    assert!(!ws.channels.component_visible(1));
    assert!(ws.channels.component_visible(2));
    white_pencil(&mut ed);
    let before = layer_pixels(&ed);
    stroke(&mut ed, &[(10.0, 10.0), (50.0, 40.0)]);
    assert_eq!(
        moved(&before, &layer_pixels(&ed)),
        [true, false, true, false]
    );
}

#[test]
fn selecting_the_composite_paints_every_channel_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let mut ws = ui::Workspace::new();
    click_channel(&mut ed, &mut ws, ChannelKind::Component(0), false);
    click_channel(&mut ed, &mut ws, ChannelKind::Composite, false);
    assert_eq!(ed.edit_channels(), None);
    assert!(ws.channels.composite_visible(&color::ColorSpace::Srgb));
    white_pencil(&mut ed);
    let before = layer_pixels(&ed);
    stroke(&mut ed, &[(10.0, 10.0), (50.0, 40.0)]);
    assert_eq!(
        moved(&before, &layer_pixels(&ed)),
        [true, true, true, false]
    );
}

#[test]
fn gaussian_blur_with_green_selected_leaves_red_and_blue_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    paint_detail(&mut ed);
    let mut ws = ui::Workspace::new();
    click_channel(&mut ed, &mut ws, ChannelKind::Component(1), false);
    let before = layer_pixels(&ed);
    blur(&mut ed);
    let after = layer_pixels(&ed);
    assert_eq!(
        moved(&before, &after),
        [false, true, false, false],
        "the blur moved green and nothing else"
    );
    assert!(ed.active_mut().unwrap().undo().unwrap());
    assert_eq!(layer_pixels(&ed), before, "undo restores the blurred green");
}

#[test]
fn a_sixteen_bit_blur_with_green_selected_keeps_red_and_blue_at_sixteen_bits() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.active_mut().unwrap().convert_depth(16, false).unwrap();
    paint_detail(&mut ed);
    // A 16-bit value no 8-bit code widens to, so a trip through 8 bits
    // would show.
    {
        let doc = ed.active_mut().unwrap();
        let layer = doc.document.active_layer().unwrap();
        let mut samples = doc.layer_rgba16(layer);
        for px in samples.chunks_mut(4) {
            px[0] = px[0].saturating_add(77);
            px[2] = px[2].saturating_sub(77);
        }
        let seed = doc.layer_rgba16_command(layer, &samples, "Seed").unwrap();
        ed.apply_command(seed);
    }
    let mut ws = ui::Workspace::new();
    click_channel(&mut ed, &mut ws, ChannelKind::Component(1), false);
    let before = layer_pixels16(&ed);
    blur(&mut ed);
    let after = layer_pixels16(&ed);
    assert_eq!(moved(&before, &after), [false, true, false, false]);

    // And a brush stroke (RGBA8 tool output) keeps red and blue's exact
    // 16-bit samples too.
    white_pencil(&mut ed);
    let before = layer_pixels16(&ed);
    stroke(&mut ed, &[(10.0, 10.0), (50.0, 40.0)]);
    let after = layer_pixels16(&ed);
    assert_eq!(moved(&before, &after), [false, true, false, false]);
}

#[test]
fn a_cmyk_document_lists_its_inks_and_a_stroke_on_cyan_changes_only_cyan() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.apply_command(Command::SetMetaColorMode {
        from: editor_core::color_mode::mode::RGB,
        to: editor_core::color_mode::mode::CMYK,
    });
    let doc = ed.active().unwrap().document.clone();
    let ws_rows = ui::Workspace::new().channels.rows(&doc);
    let names: Vec<&str> = ws_rows[1..5].iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Cyan", "Magenta", "Yellow", "Black"]);
    assert_eq!(ws_rows[0].name, "CMYK");

    let mut ws = ui::Workspace::new();
    click_channel(
        &mut ed,
        &mut ws,
        component_kind(ChannelModel::Cmyk, 0),
        false,
    );
    assert_eq!(
        ed.edit_channels(),
        Some(ChannelWrite {
            mask: 0b0001,
            model: ChannelModel::Cmyk
        })
    );
    white_pencil(&mut ed);
    let before = layer_pixels(&ed);
    stroke(&mut ed, &[(10.0, 10.0), (50.0, 40.0)]);
    let after = layer_pixels(&ed);

    // Every changed pixel is the base colour with its cyan replaced by the
    // white stroke's (none) and its magenta, yellow and black kept.
    let base = color::cmyk::rgb8_to_cmyk([BASE[0], BASE[1], BASE[2]]);
    let white = color::cmyk::rgb8_to_cmyk([255, 255, 255]);
    let want = color::cmyk::cmyk_to_rgb8(color::cmyk::Cmyk { c: white.c, ..base });
    let mut changed = 0;
    for (b, a) in before.chunks(4).zip(after.chunks(4)) {
        if b != a {
            changed += 1;
            assert_eq!(&a[..3], &want[..], "{b:?} -> {a:?}");
            assert_eq!(a[3], b[3], "alpha kept");
        }
    }
    assert!(changed > 0, "the stroke changed pixels");
    assert!(ed.active_mut().unwrap().undo().unwrap());
    assert_eq!(layer_pixels(&ed), before);
}

#[test]
fn a_selection_for_another_model_masks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    // A CMYK selection left over from another tab, over an RGB document.
    let mut ws = ui::Workspace::new();
    ws.channels.selected = component_kind(ChannelModel::Cmyk, 0);
    let _ = crate::menu_bridge::context(&mut ed, &ws);
    assert_eq!(ed.edit_channels(), None);
}

#[test]
fn a_layer_mask_edit_is_never_channel_masked() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let layer = ed.active().unwrap().document.active_layer().unwrap();
    ed.apply_command(Command::SetLayerProperties {
        layer_id: layer,
        patch: editor_core::LayerPatch {
            mask: editor_core::Patch::Set(layer_model::LayerMask::new(layer_model::MaskId::new())),
            ..Default::default()
        },
    });
    let coverage = vec![200u8; (W * H) as usize];
    let doc = ed.active_mut().unwrap();
    let command = pixels::write_mask_coverage(doc, layer, &coverage, "Mask").unwrap();
    let write = ChannelWrite {
        mask: 0b001,
        model: ChannelModel::Rgb,
    };
    let masked = mask_command(doc, command.clone(), write);
    assert_eq!(masked, command, "a mask target passes through untouched");
}

#[test]
fn an_eraser_stroke_with_red_selected_changes_red_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let mut ws = ui::Workspace::new();
    click_channel(&mut ed, &mut ws, ChannelKind::Component(0), false);
    ed.set_tool(ToolId::Eraser);
    let before = layer_pixels(&ed);
    let depth = ed.active().unwrap().history_depth();
    stroke(&mut ed, &[(10.0, 10.0), (50.0, 40.0)]);
    let after = layer_pixels(&ed);
    let moved = moved(&before, &after);
    assert!(!moved[1] && !moved[2] && !moved[3], "{moved:?}");
    assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
    assert!(ed.active_mut().unwrap().undo().unwrap());
    assert_eq!(layer_pixels(&ed), before);
}

#[test]
fn a_lab_document_writes_only_lightness_when_lightness_is_selected() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.apply_command(Command::SetMetaColorMode {
        from: editor_core::color_mode::mode::RGB,
        to: editor_core::color_mode::mode::LAB,
    });
    let mut ws = ui::Workspace::new();
    click_channel(
        &mut ed,
        &mut ws,
        component_kind(ChannelModel::Lab, 0),
        false,
    );
    assert_eq!(
        ed.edit_channels(),
        Some(ChannelWrite {
            mask: 0b001,
            model: ChannelModel::Lab
        })
    );
    // A mid grey: lightness rises from the base's ~38 to ~53, where the
    // base's a and b are still inside sRGB (the tiles are RGB, so a Lab
    // result outside it is clamped).
    ed.set_tool(ToolId::Pencil);
    ed.set_foreground([0.5, 0.5, 0.5, 1.0]);
    let before = layer_pixels(&ed);
    stroke(&mut ed, &[(10.0, 10.0), (50.0, 40.0)]);
    let after = layer_pixels(&ed);
    let unit = |v: u8| f32::from(v) / 255.0;
    let lab_of = |p: &[u8]| color::model::rgb_to_lab([unit(p[0]), unit(p[1]), unit(p[2])]);
    let mut changed = 0;
    for (b, a) in before.chunks(4).zip(after.chunks(4)) {
        if b != a {
            changed += 1;
            let (lb, la) = (lab_of(b), lab_of(a));
            assert!(la[0] > lb[0] + 10.0, "lightness rose: {lb:?} -> {la:?}");
            // a and b are kept up to 8-bit rounding of the RGB tile.
            assert!((la[1] - lb[1]).abs() < 3.0, "a kept: {lb:?} -> {la:?}");
            assert!((la[2] - lb[2]).abs() < 3.0, "b kept: {lb:?} -> {la:?}");
            assert_eq!(a[3], b[3]);
        }
    }
    assert!(changed > 0);
}

#[test]
fn edit_fade_after_a_green_only_blur_keeps_red_and_blue() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    paint_detail(&mut ed);
    let mut ws = ui::Workspace::new();
    click_channel(&mut ed, &mut ws, ChannelKind::Component(1), false);
    let before = layer_pixels(&ed);
    blur(&mut ed);
    let spec = ui::dialogs::FadeSpec {
        opacity: 0.5,
        mode: layer_model::BlendMode::Normal,
    };
    crate::fade::fade_with(&mut ed, &spec).unwrap();
    let faded = layer_pixels(&ed);
    assert_eq!(
        moved(&before, &faded),
        [false, true, false, false],
        "the fade blended green alone"
    );
}

#[test]
fn a_paint_bucket_and_a_gradient_with_blue_selected_write_blue_alone() {
    for tool in [ToolId::PaintBucket, ToolId::Gradient] {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let mut ws = ui::Workspace::new();
        click_channel(&mut ed, &mut ws, ChannelKind::Component(2), false);
        ed.set_tool(tool);
        ed.set_foreground([1.0, 1.0, 1.0, 1.0]);
        let before = layer_pixels(&ed);
        stroke(&mut ed, &[(4.0, 30.0), (30.0, 30.0), (60.0, 30.0)]);
        assert_eq!(
            moved(&before, &layer_pixels(&ed)),
            [false, false, true, false],
            "{tool:?} moved blue alone"
        );
    }
}
