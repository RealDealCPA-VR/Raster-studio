//! W11-I: Layer ▸ Align with two or more layers selected and no pixel
//! selection aligns the layers to *each other* (their combined bounds), as
//! Photopea's learn/layer-manipulation page describes ("so they all have
//! centers at the same point, or to get their upper edge to the same
//! height"); one layer alone still aligns to the canvas. Driven through the
//! real menu route: the row resolved against the live editor, clicked
//! through the chrome, the pick performed.

use editor_core::Command;
use layer_model::LayerId;
use ui::menu::{AlignEdge, MenuAction};

use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::menu_bridge::pixels;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const W: u32 = 64;
const H: u32 = 48;

fn opened(dir: &std::path::Path) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::default()),
    );
    let rgba: Vec<u8> = (0..W * H).flat_map(|_| [0u8, 0, 0, 0]).collect();
    let path = dir.join("blank.png");
    std::fs::write(
        &path,
        raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap(),
    )
    .unwrap();
    ed.open_path(&path).expect("the probe opens");
    ed
}

fn block_layer(ed: &mut Editor, name: &str, x: u32, y: u32, size: u32) -> LayerId {
    let layer = layer_model::Layer::raster(name);
    let id = layer.id;
    ed.apply_command(Command::create_layer(layer));
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for yy in y..(y + size).min(H) {
        for xx in x..(x + size).min(W) {
            let i = ((yy * W + xx) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let paint = {
        let doc = ed.active_mut().unwrap();
        pixels::write_layer(doc, id, &rgba, name).unwrap()
    };
    ed.apply_command(paint);
    ed.set_active_layer(id);
    id
}

fn ink(ed: &Editor, id: LayerId) -> raster::PixelRect {
    let doc = ed.active().unwrap();
    crate::tool_input::tight_document_bounds(&doc.document, &doc.tiles, id)
        .expect("the block layer has ink")
}

fn align_through_menu(ed: &mut Editor, edge: AlignEdge) {
    let action = MenuAction::AlignLayers(edge);
    let mut chrome = Chrome::new();
    let ctx = crate::menu_bridge::context(ed, chrome.workspace());
    let intent = crate::menu_bridge::resolve_intent(action, &ctx, ed)
        .unwrap_or_else(|reason| panic!("{action:?} is disabled: {reason}"));
    let mut out = ChromeOutput::default();
    chrome.menu_click(intent, ed, &mut out);
    assert_eq!(out.menu, vec![action]);
    for pick in out.menu {
        crate::menu_bridge::perform(pick, ed).expect("the align applied");
    }
}

#[test]
fn align_with_several_layers_aligns_them_to_each_other_not_the_canvas() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = opened(dir.path());
    let a = block_layer(&mut ed, "A", 10, 20, 4);
    let b = block_layer(&mut ed, "B", 30, 6, 8);
    ed.set_layer_selection(vec![a, b], Some(b));

    // Left: both left edges meet the leftmost (10), not the canvas's 0.
    align_through_menu(&mut ed, AlignEdge::Left);
    assert_eq!(ink(&ed, a).x, 10, "the leftmost layer stays put");
    assert_eq!(ink(&ed, b).x, 10, "the other joins it, not the canvas edge");

    // Top: both top edges meet the topmost (6).
    align_through_menu(&mut ed, AlignEdge::Top);
    assert_eq!(ink(&ed, a).y, 6);
    assert_eq!(ink(&ed, b).y, 6);

    // Bottom: both bottom edges meet the lowest bottom (6 + 8 = 14), not 48.
    align_through_menu(&mut ed, AlignEdge::Bottom);
    let (ba, bb) = (ink(&ed, a), ink(&ed, b));
    assert_eq!(ba.y + ba.height as i64, 14);
    assert_eq!(bb.y + bb.height as i64, 14);

    // A pixel selection still wins over the layers' bounds.
    ed.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
        min: glam::IVec2::new(40, 0),
        max: glam::IVec2::new(60, 40),
    };
    align_through_menu(&mut ed, AlignEdge::Right);
    let (ba, bb) = (ink(&ed, a), ink(&ed, b));
    assert_eq!(ba.x + ba.width as i64, 60);
    assert_eq!(bb.x + bb.width as i64, 60);
}

#[test]
fn align_with_one_layer_still_aligns_to_the_canvas() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = opened(dir.path());
    let a = block_layer(&mut ed, "A", 10, 20, 4);
    ed.set_layer_selection(vec![a], Some(a));
    align_through_menu(&mut ed, AlignEdge::Left);
    assert_eq!(ink(&ed, a).x, 0, "one layer aligns to the canvas edge");
}
