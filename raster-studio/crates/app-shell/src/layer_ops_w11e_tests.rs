//! W11-E, driven the way the application drives it: the menu row (or chord,
//! or Layers-panel row menu) resolves against the shell's own menu context,
//! `Chrome::menu_click` routes the click, and the pick goes through
//! `menu_bridge::perform`. The colour label and the row menu are also driven
//! through a real `Chrome` frame: a right-click on the layer row, a click on
//! the menu row, and the chip the next frame paints.

use std::path::{Path, PathBuf};

use editor_core::Command;
use glam::Vec2;
use layer_model::{AssetOrigin, ColorLabel, LayerId, LayerKind};
use ui::menu::MenuAction;

use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::menu_bridge::{context, menus, perform, resolve_intent};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const W: u32 = 64;
const H: u32 = 64;
const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);

fn png(dir: &Path, name: &str, w: u32, h: u32, seed: u8) -> PathBuf {
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        px.copy_from_slice(&[seed, (i % 251) as u8, seed.wrapping_mul(3), 255]);
    }
    let bytes = raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A 64x64 opened image, camera at 100% centred (screen (200, 150) is
/// document (32, 32)), as the tool-input tests set it up.
fn editor_with(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    );
    ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
    ed.open_path(&png(dir, "base.png", W, H, 90)).unwrap();
    let doc = ed.active_mut().unwrap();
    doc.set_viewport(VIEWPORT);
    doc.camera.zoom = 1.0;
    doc.camera.center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
    ed
}

/// A raster layer named `name` holding an opaque `w`x`h` block at (x, y)
/// in `rgb`, created on top and made active.
fn block(ed: &mut Editor, name: &str, (x, y, w, h): (u32, u32, u32, u32), rgb: [u8; 3]) -> LayerId {
    let layer = layer_model::Layer::raster(name);
    let id = layer.id;
    ed.apply_command(Command::create_layer(layer));
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for yy in y..y + h {
        for xx in x..x + w {
            let i = ((yy * W + xx) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let paint = {
        let doc = ed.active_mut().unwrap();
        crate::menu_bridge::pixels::write_layer(doc, id, &rgba, name).unwrap()
    };
    ed.apply_command(paint);
    ed.set_active_layer(id);
    id
}

/// The row is on the menu bar, enabled, and a click on it performs.
fn click(ed: &mut Editor, action: MenuAction) -> Result<String, String> {
    assert!(
        menus(ed)
            .iter()
            .flat_map(|m| m.actions())
            .any(|a| a == action),
        "{action:?} is not on the menu bar"
    );
    let mut chrome = Chrome::new();
    let ctx = context(ed, chrome.workspace());
    let intent = resolve_intent(action, &ctx, ed)
        .unwrap_or_else(|reason| panic!("{action:?} is greyed: {reason}"));
    let mut out = ChromeOutput::default();
    chrome.menu_click(intent, ed, &mut out);
    let picks = std::mem::take(&mut out.menu);
    assert_eq!(picks, vec![action], "the click routes to perform");
    perform(action, ed)
}

fn greyed(ed: &mut Editor, action: MenuAction) -> String {
    let chrome = Chrome::new();
    let ctx = context(ed, chrome.workspace());
    resolve_intent(action, &ctx, ed).expect_err("greyed")
}

fn composite(ed: &mut Editor) -> Vec<u8> {
    let doc = ed.active_mut().unwrap();
    let rect = doc.canvas_rect();
    doc.composite(rect).unwrap()
}

fn depth(ed: &Editor) -> usize {
    ed.active().unwrap().history.undo_depth()
}

fn undo(ed: &mut Editor) {
    assert!(ed.active_mut().unwrap().undo().unwrap());
}

fn root(ed: &Editor) -> Vec<LayerId> {
    ed.active().unwrap().document.layers.root().to_vec()
}

fn select(ed: &mut Editor, layers: Vec<LayerId>, active: LayerId) {
    ed.set_layer_selection(layers, Some(active));
}

// ---------------------------------------------------------------------------
// Merge Layers
// ---------------------------------------------------------------------------

/// Ctrl+E over two NON-adjacent selected layers merges exactly those two
/// into one layer in the topmost one's slot — the layer between them stays
/// — with the picture unchanged, as one undo step. The row reads Merge
/// Layers on the bar and in the Layers panel's row menu.
#[test]
fn ctrl_e_over_a_multi_selection_merges_the_selected_layers() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    // Apart, so restacking the bottom one above the middle one shows no
    // pixel and the merged picture can be compared whole.
    let low = block(&mut ed, "Low", (2, 2, 10, 10), [200, 20, 20]);
    let mid = block(&mut ed, "Mid", (22, 22, 10, 10), [20, 200, 20]);
    let top = block(&mut ed, "Top", (42, 42, 10, 10), [20, 20, 200]);
    let background = *root(&ed).last().unwrap();
    select(&mut ed, vec![top, low], top);

    // The chord is Merge Down's, and over two layers it reads Merge Layers.
    assert_eq!(
        ui::menu::action_for_shortcut(ui::Shortcut::ctrl('e'), 0),
        Some(MenuAction::MergeDown)
    );
    let chrome = Chrome::new();
    let ctx = context(&mut ed, chrome.workspace());
    assert_eq!(MenuAction::MergeDown.label_in(&ctx), ui::menu::MERGE_LAYERS);
    let row = ui::context_menu::layer_items(&ctx)
        .into_iter()
        .find(|i| i.action == MenuAction::MergeDown)
        .expect("the row menu has the merge row");
    assert_eq!(row.label, ui::menu::MERGE_LAYERS);
    assert!(row.resolution.is_enabled());

    let picture = composite(&mut ed);
    let before = depth(&ed);
    let said = click(&mut ed, MenuAction::MergeDown).unwrap();
    assert!(said.contains("Merge Layers"), "{said}");
    assert_eq!(depth(&ed), before + 1, "one undo step");
    let order = root(&ed);
    assert_eq!(order.len(), 3, "two layers became one: {order:?}");
    assert_eq!(order[1], mid, "the unselected middle layer stays put");
    assert_eq!(order[2], background);
    let merged = order[0];
    assert!(merged != top && merged != low);
    assert_eq!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(merged)
            .unwrap()
            .name,
        "Top",
        "named after the topmost merged layer"
    );
    assert_eq!(ed.active().unwrap().document.active_layer(), Some(merged));
    assert_eq!(composite(&mut ed), picture, "merging changes no pixel");

    undo(&mut ed);
    assert_eq!(root(&ed), vec![top, mid, low, background]);

    // One layer selected: Ctrl+E is Merge Down again.
    select(&mut ed, vec![top], top);
    let ctx = context(&mut ed, chrome.workspace());
    assert_eq!(MenuAction::MergeDown.label_in(&ctx), "Merge Down");
    click(&mut ed, MenuAction::MergeDown).unwrap();
    assert_eq!(root(&ed).len(), 3);
    assert!(root(&ed).contains(&low), "Merge Down left the bottom layer");
}

/// A selected group merges with what it draws: Ctrl+E over a layer and a
/// group composites the group's children too (the picture is unchanged) and
/// removes the group with them, as one undo step.
#[test]
fn merge_layers_over_a_selected_group_keeps_the_groups_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let low = block(&mut ed, "Low", (2, 2, 10, 10), [200, 20, 20]);
    let child = block(&mut ed, "Child", (30, 30, 10, 10), [20, 20, 200]);
    perform(MenuAction::GroupLayers, &mut ed).unwrap();
    let group = ed
        .active()
        .unwrap()
        .document
        .layers
        .parent_of(child)
        .unwrap();
    let background = *root(&ed).last().unwrap();
    assert_eq!(root(&ed), vec![group, low, background]);
    select(&mut ed, vec![group, low], group);

    let picture = composite(&mut ed);
    let before = depth(&ed);
    let said = click(&mut ed, MenuAction::MergeDown).unwrap();
    assert!(said.contains("Merge Layers"), "{said}");
    assert_eq!(depth(&ed), before + 1, "one undo step");
    let order = root(&ed);
    assert_eq!(
        order.len(),
        2,
        "the group and the layer became one: {order:?}"
    );
    let doc = &ed.active().unwrap().document;
    assert!(!doc.layers.contains(child), "the child went with its group");
    assert!(matches!(
        doc.layers.get(order[0]).unwrap().kind,
        LayerKind::Raster(_)
    ));
    assert_eq!(composite(&mut ed), picture, "the group's block survives");

    undo(&mut ed);
    assert_eq!(root(&ed), vec![group, low, background]);
    assert_eq!(
        ed.active().unwrap().document.layers.parent_of(child),
        Some(group)
    );
}

/// Two layers selected inside an unselected, half-opaque group merge in
/// place: the result stays in the group and the picture is unchanged (the
/// group's opacity is neither lost nor applied twice).
#[test]
fn merge_layers_inside_a_group_stays_in_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let a = block(&mut ed, "A", (2, 2, 10, 10), [200, 20, 20]);
    perform(MenuAction::GroupLayers, &mut ed).unwrap();
    let group = ed.active().unwrap().document.layers.parent_of(a).unwrap();
    let b = block(&mut ed, "B", (30, 30, 10, 10), [20, 20, 200]);
    ed.apply_command(Command::MoveLayer {
        layer_id: b,
        parent: Some(group),
        index: 0,
    });
    ed.active_mut()
        .unwrap()
        .document
        .layers
        .get_mut(group)
        .unwrap()
        .opacity = 0.5;
    select(&mut ed, vec![b, a], b);

    let picture = composite(&mut ed);
    click(&mut ed, MenuAction::MergeDown).unwrap();
    let doc = &ed.active().unwrap().document;
    let LayerKind::Group(g) = &doc.layers.get(group).unwrap().kind else {
        panic!("the group stays a group");
    };
    assert_eq!(g.children.len(), 1, "two children became one");
    assert_eq!(composite(&mut ed), picture, "merging changes no pixel");
}

// ---------------------------------------------------------------------------
// Transform Again / Again with Copy
// ---------------------------------------------------------------------------

fn screen(doc_x: f32, doc_y: f32) -> Vec2 {
    VIEWPORT * 0.5 + Vec2::new(doc_x - W as f32 / 2.0, doc_y - H as f32 / 2.0)
}

/// Commit a real Free Transform: press on a handle of the session's box,
/// drag it, release, Enter — the pointer route.
fn free_transform(ed: &mut Editor, from: (f32, f32), to: (f32, f32)) {
    use ui::canvas::{PointerInput, PointerPhase};
    let mut pointer = crate::tool_input::ToolPointer::new();
    ed.set_tool(tools::ToolId::FreeTransform);
    for (phase, (x, y)) in [
        (PointerPhase::Down, from),
        (PointerPhase::Move, to),
        (PointerPhase::Up, to),
    ] {
        pointer.handle(ed, PointerInput::at(phase, screen(x, y)), false, &[]);
    }
    let outcome = pointer.commit(ed);
    assert_eq!(outcome.failed, None, "{outcome:?}");
    assert_eq!(outcome.steps, 1, "{outcome:?}");
}

fn transform_of(ed: &Editor, id: LayerId) -> glam::Affine2 {
    ed.active()
        .unwrap()
        .document
        .layers
        .get(id)
        .unwrap()
        .transform
}

fn close(a: glam::Affine2, b: glam::Affine2) -> bool {
    a.to_cols_array()
        .iter()
        .zip(b.to_cols_array())
        .all(|(x, y)| (x - y).abs() < 1e-3)
}

/// Shift+Ctrl+T repeats the committed free transform on the layer (greyed,
/// with the reason, until one has been committed); Shift+Alt+Ctrl+T puts it
/// on a duplicate and leaves the original where it was. One undo step each.
#[test]
fn transform_again_repeats_the_last_free_transform_and_again_with_copy_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let id = block(&mut ed, "Mark", (16, 16, 32, 32), [10, 10, 10]);
    assert_eq!(
        greyed(&mut ed, MenuAction::TransformAgain),
        ui::menu::NO_TRANSFORM_TO_REPEAT
    );
    assert_eq!(
        ui::menu::action_for_shortcut(ui::Shortcut::ctrl_shift('t'), 0),
        Some(MenuAction::TransformAgain)
    );
    assert_eq!(
        ui::menu::action_for_shortcut(ui::Shortcut::ctrl_alt_shift('t'), 0),
        Some(MenuAction::TransformAgainCopy)
    );

    let t0 = transform_of(&ed, id);
    // The session's box is the canvas: drag its top-left corner inwards
    // (a corner drag keeps the aspect: a scale about the far corner).
    free_transform(&mut ed, (0.0, 0.0), (8.0, 8.0));
    let t1 = transform_of(&ed, id);
    assert!(!close(t1, t0), "the free transform moved the layer");
    let delta = t1 * t0.inverse();
    assert!(
        close(ed.last_transform().expect("recorded"), delta),
        "the record is the committed document-space affine"
    );

    let before = depth(&ed);
    click(&mut ed, MenuAction::TransformAgain).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    let t2 = transform_of(&ed, id);
    assert!(close(t2, delta * t1), "the same affine again: {t2:?}");

    let count = ed.active().unwrap().document.layers.len();
    let before = depth(&ed);
    click(&mut ed, MenuAction::TransformAgainCopy).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step for copy + transform");
    let doc = &ed.active().unwrap().document;
    assert_eq!(doc.layers.len(), count + 1);
    let copy = doc.active_layer().unwrap();
    assert_ne!(copy, id);
    assert!(close(transform_of(&ed, id), t2), "the original stays");
    assert!(close(transform_of(&ed, copy), delta * t2), "the copy moved");
    let root = root(&ed);
    assert_eq!(
        root.iter().position(|l| *l == copy).unwrap() + 1,
        root.iter().position(|l| *l == id).unwrap(),
        "the copy sits directly above its source"
    );

    undo(&mut ed);
    assert_eq!(ed.active().unwrap().document.layers.len(), count);
    undo(&mut ed);
    assert!(close(transform_of(&ed, id), t1));
}

// ---------------------------------------------------------------------------
// Arrange > Reverse, Select Linked Layers
// ---------------------------------------------------------------------------

/// Reverse flips the selected layers end for end while an unselected layer
/// between them keeps its slot; greyed with one layer; one undo step.
#[test]
fn arrange_reverse_flips_the_selected_layers_and_leaves_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let a = block(&mut ed, "A", (0, 0, 4, 4), [1, 2, 3]);
    let b = block(&mut ed, "B", (4, 4, 4, 4), [4, 5, 6]);
    let c = block(&mut ed, "C", (8, 8, 4, 4), [7, 8, 9]);
    let d = block(&mut ed, "D", (12, 12, 4, 4), [10, 11, 12]);
    let background = *root(&ed).last().unwrap();
    assert_eq!(root(&ed), vec![d, c, b, a, background]);
    assert_eq!(
        greyed(&mut ed, MenuAction::ReverseLayers),
        "Select two or more layers to reverse"
    );
    select(&mut ed, vec![d, b, a], d);
    let before = depth(&ed);
    click(&mut ed, MenuAction::ReverseLayers).unwrap();
    assert_eq!(depth(&ed), before + 1);
    assert_eq!(root(&ed), vec![a, c, b, d, background], "C keeps its slot");
    undo(&mut ed);
    assert_eq!(root(&ed), vec![d, c, b, a, background]);
}

/// Select Linked Layers adds the selection's link partners (its own group
/// only) and is greyed when there are none to add.
#[test]
fn select_linked_layers_selects_the_link_group() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let a = block(&mut ed, "A", (0, 0, 4, 4), [1, 2, 3]);
    let b = block(&mut ed, "B", (4, 4, 4, 4), [4, 5, 6]);
    let c = block(&mut ed, "C", (8, 8, 4, 4), [7, 8, 9]);
    let d = block(&mut ed, "D", (12, 12, 4, 4), [10, 11, 12]);
    assert_eq!(
        greyed(&mut ed, MenuAction::SelectLinkedLayers),
        "No other layer is linked to the selection"
    );
    // Link A+C into one group, B+D into another (two Link Layers clicks).
    select(&mut ed, vec![a, c], a);
    click(&mut ed, MenuAction::LinkLayers).unwrap();
    select(&mut ed, vec![b, d], b);
    click(&mut ed, MenuAction::LinkLayers).unwrap();

    select(&mut ed, vec![c], c);
    click(&mut ed, MenuAction::SelectLinkedLayers).unwrap();
    let doc = &ed.active().unwrap().document;
    let got: std::collections::HashSet<LayerId> = doc.layer_selection().into_iter().collect();
    let want: std::collections::HashSet<LayerId> = [a, c].into_iter().collect();
    assert_eq!(got, want, "only C's own group");
    assert_eq!(doc.active_layer(), Some(c), "the active layer stays");
    assert_eq!(
        greyed(&mut ed, MenuAction::SelectLinkedLayers),
        "No other layer is linked to the selection"
    );
}

// ---------------------------------------------------------------------------
// Smart Object > Convert to Linked / Embed Linked
// ---------------------------------------------------------------------------

fn asset_of(ed: &Editor, id: LayerId) -> (layer_model::AssetId, AssetOrigin, bool) {
    let LayerKind::SmartObject(so) = &ed.active().unwrap().document.layers.get(id).unwrap().kind
    else {
        panic!("not a smart object");
    };
    let origin = ed
        .active()
        .unwrap()
        .document
        .asset_origin(so.asset)
        .cloned()
        .unwrap();
    (so.asset, origin, so.linked)
}

/// Convert to Linked writes the embedded bytes to the picked file and links
/// the object to it (pixels unchanged, one undo step); Embed Linked reads
/// the file back in and embeds it (one undo step). Each is greyed over the
/// other kind, with the reason.
#[test]
fn convert_to_linked_writes_the_source_and_embed_linked_reads_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let source = png(dir.path(), "source.png", 16, 12, 30);
    let target = dir.path().join("linked.png");
    let mut ed = editor_with(
        dir.path(),
        ScriptedDialogs::new().placing(&source).saving_to(&target),
    );
    perform(MenuAction::PlaceEmbedded, &mut ed).unwrap();
    let id = ed.active().unwrap().document.active_layer().unwrap();
    let (_, origin, linked) = asset_of(&ed, id);
    let AssetOrigin::Embedded { bytes, .. } = origin else {
        panic!("placed embedded");
    };
    assert!(!linked);
    assert_eq!(
        greyed(&mut ed, MenuAction::EmbedLinked),
        "The smart object is already embedded"
    );
    let tiles = ed.active().unwrap().document.layer_tiles(id).cloned();

    let before = depth(&ed);
    click(&mut ed, MenuAction::ConvertToLinked).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    assert_eq!(std::fs::read(&target).unwrap(), bytes, "byte for byte");
    let (_, origin, linked) = asset_of(&ed, id);
    assert_eq!(
        origin,
        AssetOrigin::Linked {
            path: target.clone()
        }
    );
    assert!(linked);
    assert_eq!(
        ed.active().unwrap().document.layer_tiles(id).cloned(),
        tiles,
        "the pixels do not change"
    );
    assert_eq!(
        greyed(&mut ed, MenuAction::ConvertToLinked),
        "The smart object is already linked"
    );

    let before = depth(&ed);
    click(&mut ed, MenuAction::EmbedLinked).unwrap();
    assert_eq!(depth(&ed), before + 1);
    let (_, origin, linked) = asset_of(&ed, id);
    assert!(!linked);
    let AssetOrigin::Embedded {
        name,
        bytes: embedded,
    } = origin
    else {
        panic!("embedded again");
    };
    assert_eq!(embedded, bytes);
    assert_eq!(name, "linked.png");

    undo(&mut ed);
    assert!(asset_of(&ed, id).2, "undo: linked again");
    undo(&mut ed);
    assert!(
        matches!(asset_of(&ed, id).1, AssetOrigin::Embedded { .. }) && !asset_of(&ed, id).2,
        "undo: embedded as placed"
    );
}

// ---------------------------------------------------------------------------
// Colour labels
// ---------------------------------------------------------------------------

/// The layer-row colour label through the menu bar: every selected layer
/// takes it in one undo step, it is saved with the document, and No Color
/// clears it.
#[test]
fn a_color_label_is_one_undo_step_for_the_selection_and_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let a = block(&mut ed, "A", (0, 0, 4, 4), [1, 2, 3]);
    let b = block(&mut ed, "B", (4, 4, 4, 4), [4, 5, 6]);
    assert_eq!(
        greyed(&mut ed, MenuAction::SetLayerColor(ColorLabel::NoColor)),
        "The layer has no color label"
    );
    select(&mut ed, vec![a, b], b);
    let before = depth(&ed);
    click(&mut ed, MenuAction::SetLayerColor(ColorLabel::Violet)).unwrap();
    assert_eq!(depth(&ed), before + 1);
    let extras = &ed.active().unwrap().document.extras;
    assert_eq!(extras.color_label(a), ColorLabel::Violet);
    assert_eq!(extras.color_label(b), ColorLabel::Violet);

    // Saved: the whole document's serialization (what a project writes)
    // carries it, and reading it back restores it.
    let json = serde_json::to_string(&ed.active().unwrap().document).unwrap();
    let back: editor_core::Document = serde_json::from_str(&json).unwrap();
    assert_eq!(back.extras.color_label(a), ColorLabel::Violet);
    assert_eq!(back.extras.color_label(b), ColorLabel::Violet);

    // A duplicate wears its source's label.
    select(&mut ed, vec![b], b);
    crate::layer_ops::duplicate_layer(&mut ed, None).unwrap();
    let copy = ed.active().unwrap().document.active_layer().unwrap();
    assert_eq!(
        ed.active().unwrap().document.extras.color_label(copy),
        ColorLabel::Violet
    );

    undo(&mut ed);
    undo(&mut ed);
    assert_eq!(
        ed.active().unwrap().document.extras.color_label(a),
        ColorLabel::NoColor
    );
}

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    }
}

fn frame(
    ctx: &egui::Context,
    chrome: &mut Chrome,
    ed: &mut Editor,
    events: Vec<egui::Event>,
) -> (ChromeOutput, egui::FullOutput) {
    let mut out = ChromeOutput::default();
    let full = ctx.run(raw_input(events), |ctx| {
        out = chrome.ui(ctx, ed);
    });
    (out, full)
}

fn press(at: egui::Pos2, button: egui::PointerButton) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton {
            pos: at,
            button,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        },
        egui::Event::PointerButton {
            pos: at,
            button,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        },
    ]
}

/// Every filled rectangle a frame painted, with its fill.
fn filled_rects(full: &egui::FullOutput) -> Vec<(egui::Rect, egui::Color32)> {
    fn walk(shape: &egui::Shape, out: &mut Vec<(egui::Rect, egui::Color32)>) {
        match shape {
            egui::Shape::Rect(r) => out.push((r.rect, r.fill)),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &full.shapes {
        walk(&clipped.shape, &mut out);
    }
    out
}

/// The real Layers panel: a right-click on the layer row opens the row menu,
/// whose colour rows are there (after Merge / Flatten); a click on Red
/// routes to the application and labels the layer as one undo step, and the
/// next frame paints a red chip in that row's left margin. No chip before.
#[test]
fn the_layer_row_menu_labels_the_layer_and_the_row_paints_its_chip() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let id = block(&mut ed, "Tagged", (0, 0, 8, 8), [1, 2, 3]);
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    chrome.emit(ui::Intent::ApplyLayout(ui::LayoutId::Minimal));
    chrome.emit(ui::Intent::SetPanelOpen {
        panel: ui::PanelId::Layers,
        open: true,
    });
    for _ in 0..4 {
        frame(&ctx, &mut chrome, &mut ed, Vec::new());
    }
    let red = {
        let [r, g, b] = ColorLabel::Red.rgb().unwrap();
        egui::Color32::from_rgba_unmultiplied(r, g, b, 255)
    };
    let row = ctx
        .read_response(ui::view::ids::layer_row(id))
        .expect("the layer row is drawn")
        .rect;
    let (_, full) = frame(&ctx, &mut chrome, &mut ed, Vec::new());
    assert!(
        !filled_rects(&full).iter().any(|(_, c)| *c == red),
        "no chip before a label"
    );

    // Right-click the row: the row menu arms, carrying the colour rows.
    frame(
        &ctx,
        &mut chrome,
        &mut ed,
        press(row.center(), egui::PointerButton::Secondary),
    );
    frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let menu_ctx = context(&mut ed, chrome.workspace());
    let items = ui::context_menu::layer_items(&menu_ctx);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    let red_at = items
        .iter()
        .position(|i| i.action == MenuAction::SetLayerColor(ColorLabel::Red))
        .expect("the row menu offers Red");
    assert_eq!(
        &labels[red_at - 1..red_at + 7],
        &["No Color", "Red", "Orange", "Yellow", "Green", "Blue", "Violet", "Gray"]
    );
    let at = ctx
        .read_response(ui::context_menu::ids::context_item(red_at))
        .expect("the Red row is drawn")
        .rect
        .center();
    let before = depth(&ed);
    let (out, _) = frame(
        &ctx,
        &mut chrome,
        &mut ed,
        press(at, egui::PointerButton::Primary),
    );
    assert_eq!(
        out.menu,
        vec![MenuAction::SetLayerColor(ColorLabel::Red)],
        "the click routes to the application"
    );
    for action in out.menu {
        perform(action, &mut ed).unwrap();
    }
    assert_eq!(depth(&ed), before + 1);
    assert_eq!(
        ed.active().unwrap().document.extras.color_label(id),
        ColorLabel::Red
    );

    frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let row = ctx
        .read_response(ui::view::ids::layer_row(id))
        .unwrap()
        .rect;
    let (_, full) = frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let chip = filled_rects(&full)
        .into_iter()
        .find(|(_, c)| *c == red)
        .expect("the row paints a red chip")
        .0;
    assert_eq!(chip.left(), row.left(), "in the row's left margin");
    assert_eq!(chip.top(), row.top());
    assert_eq!(chip.height(), row.height());
    assert!(chip.width() > 0.0 && chip.width() < row.width() / 4.0);
}

// ---------------------------------------------------------------------------
// New Layer Based Slice
// ---------------------------------------------------------------------------

/// New Layer Based Slice adds a slice over the active layer's ink, named
/// after the layer, picked, with the Slice Select tool raised; File >
/// Export > Slices then has it.
#[test]
fn new_layer_based_slice_adds_a_slice_over_the_layers_ink() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let id = block(&mut ed, "Hero", (10, 6, 20, 12), [1, 2, 3]);
    let said = click(&mut ed, MenuAction::NewLayerBasedSlice).unwrap();
    assert!(said.contains("Hero"), "{said}");
    let doc_id = ed.active().unwrap().id();
    assert_eq!(
        ed.slices.get(doc_id),
        &[raster::PixelRect::new(10, 6, 20, 12)]
    );
    assert_eq!(ed.slices.options(doc_id)[0].name, "Hero");
    assert_eq!(ed.slices.picked(doc_id), Some(0));
    assert_eq!(ed.tool(), tools::ToolId::SliceSelect);
    // A second one over the same layer is refused; another layer adds.
    ed.set_active_layer(id);
    let refused = click(&mut ed, MenuAction::NewLayerBasedSlice).unwrap_err();
    assert!(refused.contains("already"), "{refused}");
    block(&mut ed, "Hero", (40, 40, 8, 8), [4, 5, 6]);
    click(&mut ed, MenuAction::NewLayerBasedSlice).unwrap();
    assert_eq!(ed.slices.get(doc_id).len(), 2);
    assert_eq!(
        ed.slices.options(doc_id)[1].name,
        "Hero 2",
        "a second slice may not export over the first's file"
    );
}

/// New Layer Based Slice is one undo step (Photoshop): Undo removes the slice
/// from the document AND the editor's slice store, Redo brings it back.
#[test]
fn new_layer_based_slice_is_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    block(&mut ed, "Hero", (10, 6, 20, 12), [1, 2, 3]);
    let depth = ed.active().unwrap().history_depth();
    click(&mut ed, MenuAction::NewLayerBasedSlice).unwrap();
    let doc_id = ed.active().unwrap().id();
    assert_eq!(ed.active().unwrap().history_depth(), depth + 1, "one step");
    assert_eq!(ed.active().unwrap().document.slices.len(), 1);

    ed.dispatch(crate::Action::Undo).unwrap();
    assert!(ed.active().unwrap().document.slices.is_empty());
    assert!(
        ed.slices.get(doc_id).is_empty(),
        "the store follows the undo"
    );

    ed.dispatch(crate::Action::Redo).unwrap();
    assert_eq!(ed.active().unwrap().document.slices.len(), 1);
    assert_eq!(
        ed.slices.get(doc_id),
        &[raster::PixelRect::new(10, 6, 20, 12)],
        "the store follows the redo"
    );
}
