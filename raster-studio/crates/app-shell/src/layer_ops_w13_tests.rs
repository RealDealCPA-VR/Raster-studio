//! W13-G, driven the way the application drives it: the row is on the menu
//! bar, resolves against the shell's own menu context, `Chrome::menu_click`
//! routes the click, and the pick goes through `menu_bridge::perform`.

use std::path::{Path, PathBuf};

use editor_core::Command;
use layer_model::{
    effects::{
        BevelEffect, ColorOverlayEffect, GlowEffect, SatinEffect, ShadowEffect, StrokeEffect,
    },
    AssetOrigin, BlendMode, ClippingMode, LayerEffects, LayerId, LayerKind,
};
use ui::menu::{LayerExtraOp as Op, MenuAction, StackMode};

use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::menu_bridge::{context, menus, perform, resolve_intent};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const W: u32 = 64;
const H: u32 = 64;

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

fn editor_with(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    );
    ed.open_path(&png(dir, "base.png", W, H, 90)).unwrap();
    ed
}

/// A root raster layer named `name` holding `rgba` over the canvas, made
/// active.
fn painted(ed: &mut Editor, name: &str, rgba: &[u8]) -> LayerId {
    let layer = layer_model::Layer::raster(name);
    let id = layer.id;
    ed.apply_command(Command::create_layer(layer));
    let paint = {
        let doc = ed.active_mut().unwrap();
        crate::menu_bridge::pixels::write_layer(doc, id, rgba, name).unwrap()
    };
    ed.apply_command(paint);
    ed.set_active_layer(id);
    id
}

/// An opaque `w`x`h` block at (x, y) in `rgb`.
fn block(ed: &mut Editor, name: &str, (x, y, w, h): (u32, u32, u32, u32), rgb: [u8; 3]) -> LayerId {
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for yy in y..y + h {
        for xx in x..x + w {
            let i = ((yy * W + xx) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    painted(ed, name, &rgba)
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

fn row(op: Op) -> MenuAction {
    MenuAction::LayerExtra(op)
}

fn composite(ed: &mut Editor) -> Vec<u8> {
    let doc = ed.active_mut().unwrap();
    let rect = doc.canvas_rect();
    doc.composite(rect).unwrap()
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

fn depth(ed: &Editor) -> usize {
    ed.active().unwrap().history.undo_depth()
}

fn undo(ed: &mut Editor) {
    assert!(ed.active_mut().unwrap().undo().unwrap());
}

fn layer(ed: &Editor, id: LayerId) -> &layer_model::Layer {
    ed.active().unwrap().document.layers.get(id).unwrap()
}

fn root(ed: &Editor) -> Vec<LayerId> {
    ed.active().unwrap().document.layers.root().to_vec()
}

fn set_effects(ed: &mut Editor, id: LayerId, fx: LayerEffects) {
    ed.apply_command(Command::SetLayerProperties {
        layer_id: id,
        patch: editor_core::LayerPatch {
            effects: Some(Box::new(fx)),
            ..Default::default()
        },
    });
}

// ---------------------------------------------------------------------------
// Layer Style ▸ Scale Effects / Create Layers
// ---------------------------------------------------------------------------

#[test]
fn scale_effects_scales_every_size_as_one_step_and_is_greyed_without_a_style() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let id = block(&mut ed, "Box", (20, 20, 20, 16), [40, 90, 200]);
    assert_eq!(
        greyed(&mut ed, row(Op::ScaleEffects(200))),
        ui::menu::LAYER_EXTRA_NO_STYLE
    );
    let fx = LayerEffects {
        drop_shadow: Some(ShadowEffect {
            distance_px: 4.0,
            size_px: 6.0,
            ..Default::default()
        }),
        outer_glow: Some(GlowEffect {
            size_px: 5.0,
            ..Default::default()
        }),
        stroke: Some(StrokeEffect {
            size_px: 2.0,
            ..Default::default()
        }),
        ..Default::default()
    };
    set_effects(&mut ed, id, fx.clone());
    let before = depth(&ed);
    click(&mut ed, row(Op::ScaleEffects(200))).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    let got = &layer(&ed, id).effects;
    let shadow = got.drop_shadow.as_ref().unwrap();
    assert_eq!((shadow.distance_px, shadow.size_px), (8.0, 12.0));
    assert_eq!(shadow.opacity, 0.75, "an opacity is not a size");
    assert_eq!(got.outer_glow.as_ref().unwrap().size_px, 10.0);
    assert_eq!(got.stroke.as_ref().unwrap().size_px, 4.0);
    click(&mut ed, row(Op::ScaleEffects(25))).unwrap();
    assert_eq!(layer(&ed, id).effects.stroke.as_ref().unwrap().size_px, 1.0);
    undo(&mut ed);
    undo(&mut ed);
    assert_eq!(layer(&ed, id).effects, fx);
}

/// Every effect the compositor draws, on one layer.
fn full_style() -> LayerEffects {
    LayerEffects {
        drop_shadow: Some(ShadowEffect {
            distance_px: 4.0,
            size_px: 3.0,
            ..Default::default()
        }),
        outer_glow: Some(GlowEffect {
            size_px: 4.0,
            ..Default::default()
        }),
        color_overlay: Some(ColorOverlayEffect {
            color: [0.9, 0.2, 0.1, 1.0],
            opacity: 0.5,
            blend_mode: BlendMode::Normal,
        }),
        satin: Some(SatinEffect::default()),
        inner_glow: Some(GlowEffect {
            size_px: 3.0,
            ..Default::default()
        }),
        inner_shadow: Some(ShadowEffect {
            distance_px: 3.0,
            size_px: 2.0,
            ..Default::default()
        }),
        bevel_emboss: Some(BevelEffect::default()),
        stroke: Some(StrokeEffect {
            size_px: 2.0,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Create Layers turns a style of every kind into separate layers — the
/// exterior passes under the layer, the interior effects clipped above it,
/// the stroke above the clipping group — and the picture they composite is
/// the styled original's to within 2/255. One undo step puts the style back.
#[test]
fn create_layers_splits_the_style_and_recomposites_the_styled_original() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let id = block(&mut ed, "Box", (18, 20, 26, 20), [40, 150, 220]);
    assert_eq!(
        greyed(&mut ed, row(Op::CreateLayers)),
        ui::menu::LAYER_EXTRA_NO_STYLE
    );
    set_effects(&mut ed, id, full_style());
    let styled = composite(&mut ed);
    let layers_before = root(&ed).len();
    let before = depth(&ed);

    click(&mut ed, row(Op::CreateLayers)).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    assert!(
        layer(&ed, id).effects.is_empty(),
        "the style left the layer"
    );
    let order = root(&ed);
    // Drop shadow + outer glow, colour, satin, inner glow, inner shadow,
    // bevel highlights + shadows, stroke.
    assert_eq!(order.len(), layers_before + 9, "one layer per effect ink");
    let at = order.iter().position(|x| *x == id).unwrap();
    let name = |i: usize| layer(&ed, order[i]).name.clone();
    assert_eq!(name(at + 1), "Box's Outer Glow");
    assert_eq!(name(at + 2), "Box's Drop Shadow");
    assert_eq!(layer(&ed, order[at + 2]).blend_mode, BlendMode::Multiply);
    for i in 1..=6 {
        let clipped = layer(&ed, order[at - i]);
        assert_eq!(
            clipped.clipping,
            ClippingMode::ClipToBelow,
            "{} is clipped",
            clipped.name
        );
    }
    assert_eq!(name(at - 1), "Box's Color Fill");
    assert_eq!(name(at - 6), "Box's Bevel Shadows");
    assert_eq!(name(at - 7), "Box's Stroke");
    assert_eq!(layer(&ed, order[at - 7]).clipping, ClippingMode::None);

    let split = composite(&mut ed);
    let diff = max_diff(&styled, &split);
    assert!(
        diff <= 2,
        "the layers composite {diff}/255 away from the style"
    );

    undo(&mut ed);
    assert_eq!(root(&ed).len(), layers_before);
    assert_eq!(layer(&ed, id).effects, full_style());
    assert_eq!(composite(&mut ed), styled);
}

/// The split holds under a layer opacity and fill below 100% too, for
/// interior effects in Normal (the documented gap: in another mode an
/// interior effect mixes against the layer's faded coverage, which a clipped
/// layer does not see).
#[test]
fn create_layers_holds_under_partial_opacity_and_fill() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let id = block(&mut ed, "Box", (18, 20, 26, 20), [220, 200, 30]);
    let fx = LayerEffects {
        drop_shadow: Some(ShadowEffect::default()),
        color_overlay: Some(ColorOverlayEffect {
            color: [0.1, 0.3, 0.9, 1.0],
            opacity: 0.6,
            blend_mode: BlendMode::Normal,
        }),
        inner_shadow: Some(ShadowEffect {
            blend_mode: BlendMode::Normal,
            ..Default::default()
        }),
        stroke: Some(StrokeEffect::default()),
        ..Default::default()
    };
    set_effects(&mut ed, id, fx);
    ed.apply_command(Command::SetLayerProperties {
        layer_id: id,
        patch: editor_core::LayerPatch {
            opacity: Some(0.7),
            fill_opacity: Some(0.5),
            ..Default::default()
        },
    });
    let styled = composite(&mut ed);
    click(&mut ed, row(Op::CreateLayers)).unwrap();
    let diff = max_diff(&styled, &composite(&mut ed));
    assert!(diff <= 2, "{diff}/255 away from the style");
}

// ---------------------------------------------------------------------------
// New ▸ Artboard from Layers
// ---------------------------------------------------------------------------

#[test]
fn artboard_from_layers_wraps_the_selection_in_an_artboard_at_their_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let a = block(&mut ed, "A", (4, 6, 10, 8), [200, 20, 20]);
    let b = block(&mut ed, "B", (30, 20, 12, 30), [20, 200, 20]);
    ed.set_layer_selection(vec![a, b], Some(b));
    let picture = composite(&mut ed);
    let before = depth(&ed);
    click(&mut ed, row(Op::ArtboardFromLayers)).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    let doc = &ed.active().unwrap().document;
    let group = doc.active_layer().unwrap();
    let (_, board) = layer_model::artboard::artboard_of(&doc.layers, group).expect("an artboard");
    assert_eq!(
        (board.x, board.y, board.width, board.height),
        (4, 6, 38, 44),
        "the union of the two layers' ink"
    );
    let children = doc.layers.get(group).unwrap().children().to_vec();
    assert_eq!(children.len(), 3, "both layers and the background plate");
    assert_eq!(&children[..2], &[b, a], "the stack order is kept");
    assert_eq!(composite(&mut ed), picture, "a transparent artboard");
    // Nested now, so the row is greyed with the reason.
    ed.set_layer_selection(vec![a], Some(a));
    assert_eq!(
        greyed(&mut ed, row(Op::ArtboardFromLayers)),
        "Artboards sit at the top of the stack: select top-level layers"
    );
    undo(&mut ed);
    assert!(root(&ed).contains(&a) && root(&ed).contains(&b));
}

// ---------------------------------------------------------------------------
// Layer Mask ▸ From Transparency
// ---------------------------------------------------------------------------

#[test]
fn from_transparency_moves_the_alpha_into_a_mask_and_keeps_the_picture() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    // A soft ramp of coverage.
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for y in 10..50u32 {
        for x in 10..50u32 {
            let i = ((y * W + x) * 4) as usize;
            let a = ((x - 10) * 6 + 10) as u8;
            rgba[i..i + 4].copy_from_slice(&[250, 120, 10, a]);
        }
    }
    let id = painted(&mut ed, "Soft", &rgba);
    let picture = composite(&mut ed);
    let before = depth(&ed);
    click(&mut ed, row(Op::MaskFromTransparency)).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    assert!(layer(&ed, id).mask.is_some(), "a mask was added");
    // The layer's own pixels are opaque wherever they had coverage.
    let pixels = crate::menu_bridge::pixels::read_layer(ed.active().unwrap(), id);
    let alpha_at = |x: u32, y: u32| pixels[((y * W + x) * 4 + 3) as usize];
    assert_eq!(alpha_at(12, 20), 255);
    assert_eq!(alpha_at(5, 5), 0);
    let diff = max_diff(&picture, &composite(&mut ed));
    assert!(diff <= 1, "{diff}/255 away");
    assert_eq!(
        greyed(&mut ed, row(Op::MaskFromTransparency)),
        "The layer already has a mask - delete it first"
    );
    undo(&mut ed);
    assert!(layer(&ed, id).mask.is_none());
    assert_eq!(composite(&mut ed), picture);
}

// ---------------------------------------------------------------------------
// Smart Object ▸ Reset Transform / Stack Mode
// ---------------------------------------------------------------------------

fn placed(dir: &Path, w: u32, h: u32) -> (Editor, LayerId) {
    let src = png(dir, "placed.png", w, h, 30);
    let mut ed = editor_with(dir, ScriptedDialogs::new().placing(&src));
    perform(MenuAction::PlaceEmbedded, &mut ed).unwrap();
    let id = ed.active().unwrap().document.active_layer().unwrap();
    assert!(matches!(layer(&ed, id).kind, LayerKind::SmartObject(_)));
    (ed, id)
}

#[test]
fn reset_transform_puts_the_object_back_at_its_source_size_about_its_centre() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, id) = placed(dir.path(), 16, 12);
    ed.apply_command(Command::SetLayerProperties {
        layer_id: id,
        patch: editor_core::LayerPatch {
            transform: Some(glam::Affine2::IDENTITY.to_cols_array()),
            ..Default::default()
        },
    });
    assert_eq!(
        greyed(&mut ed, row(Op::ResetTransform)),
        "The smart object is already at its source's size and angle"
    );
    let posed = glam::Affine2::from_translation(glam::Vec2::new(30.0, 20.0))
        * glam::Affine2::from_angle(0.5)
        * glam::Affine2::from_scale(glam::Vec2::new(2.0, 1.5));
    ed.apply_command(Command::SetLayerProperties {
        layer_id: id,
        patch: editor_core::LayerPatch {
            transform: Some(posed.to_cols_array()),
            ..Default::default()
        },
    });
    let centre = posed.transform_point2(glam::Vec2::new(8.0, 6.0));
    let before = depth(&ed);
    click(&mut ed, row(Op::ResetTransform)).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    let t = layer(&ed, id).transform;
    assert_eq!(t.matrix2, glam::Mat2::IDENTITY, "unscaled and upright");
    let now = t.transform_point2(glam::Vec2::new(8.0, 6.0));
    assert!((now - centre).length() <= 0.75, "{now} vs {centre}");
    undo(&mut ed);
    assert!(layer(&ed, id).transform.abs_diff_eq(posed, 1e-5));
}

#[test]
fn stack_mode_bakes_the_statistic_of_the_objects_layers_above_it() {
    let dir = tempfile::tempdir().unwrap();
    // A three-layer document written as a PSD: 20, 100 and 240 in red.
    let (bytes, sw, sh) = {
        let mut src = editor_with(dir.path(), ScriptedDialogs::new());
        for (i, v) in [20u8, 100, 240].into_iter().enumerate() {
            let rgba = [v, 10, 10, 255].repeat((W * H) as usize);
            painted(&mut src, &format!("L{i}"), &rgba);
        }
        // The opened image beneath stays out of the stack.
        let base = *root(&src).last().unwrap();
        src.apply_command(Command::DeleteLayer { layer_id: base });
        let flat = composite(&mut src);
        let doc = src.active().unwrap();
        let (bytes, _) =
            crate::import::psd_from_document(&doc.document, &doc.tiles, &flat).unwrap();
        (bytes, W, H)
    };
    let (mut ed, id) = placed(dir.path(), sw, sh);
    assert_eq!(
        greyed(&mut ed, row(Op::StackMode(StackMode::Median))),
        "The smart object's source is a single image, not a stack of layers"
    );
    let LayerKind::SmartObject(so) = &layer(&ed, id).kind else {
        unreachable!()
    };
    let asset = so.asset;
    ed.active_mut()
        .unwrap()
        .document
        .set_asset_origin(layer_model::AssetRecord {
            id: asset,
            origin: AssetOrigin::Embedded {
                name: "stack.psd".to_string(),
                bytes,
            },
            source_size: Some((sw, sh)),
        });
    let before = depth(&ed);
    click(&mut ed, row(Op::StackMode(StackMode::Median))).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    let result = ed.active().unwrap().document.active_layer().unwrap();
    assert_ne!(result, id);
    assert!(!layer(&ed, id).visible, "the object is kept, hidden");
    assert_eq!(
        layer(&ed, result).name,
        format!("{} (Median)", layer(&ed, id).name)
    );
    let px = crate::menu_bridge::pixels::read_layer(ed.active().unwrap(), result);
    assert_eq!(px[0], 100, "the median of 20, 100 and 240");
    undo(&mut ed);
    assert!(layer(&ed, id).visible);
    ed.set_active_layer(id);
    // Maximum over the same stack.
    click(&mut ed, row(Op::StackMode(StackMode::Maximum))).unwrap();
    let result = ed.active().unwrap().document.active_layer().unwrap();
    let px = crate::menu_bridge::pixels::read_layer(ed.active().unwrap(), result);
    assert_eq!(px[0], 240);
}

#[test]
fn the_stack_statistics_are_the_textbook_ones() {
    use crate::menu_bridge::layer_ops_w13::stack_statistic;
    let v = || vec![0.2f32, 0.4, 0.4, 1.0];
    let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
    assert!(close(stack_statistic(StackMode::Mean, &mut v()), 0.5));
    assert!(close(stack_statistic(StackMode::Median, &mut v()), 0.4));
    assert!(close(stack_statistic(StackMode::Minimum, &mut v()), 0.2));
    assert!(close(stack_statistic(StackMode::Maximum, &mut v()), 1.0));
    assert!(close(stack_statistic(StackMode::Range, &mut v()), 0.8));
    assert!(close(stack_statistic(StackMode::Summation, &mut v()), 1.0));
    assert!(close(stack_statistic(StackMode::Variance, &mut v()), 0.09));
    assert!(close(
        stack_statistic(StackMode::StandardDeviation, &mut v()),
        0.3
    ));
    // Three distinct values in four samples: 1.5 bits of 2.
    assert!(close(stack_statistic(StackMode::Entropy, &mut v()), 0.75));
}

// ---------------------------------------------------------------------------
// Animation ▸ Make Frames / Unmake Frames / Merge
// ---------------------------------------------------------------------------

#[test]
fn make_unmake_and_merge_frames_through_the_menu() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let base = root(&ed)[0];
    let a = block(&mut ed, "Walk", (4, 4, 10, 10), [200, 20, 20]);
    let b = block(&mut ed, "Run", (30, 30, 10, 10), [20, 200, 20]);
    assert_eq!(
        greyed(&mut ed, row(Op::MergeFrames)),
        "The document has no frames: use Make Frames first"
    );
    assert_eq!(
        greyed(&mut ed, row(Op::UnmakeFrames)),
        "No selected layer is a frame"
    );
    ed.set_layer_selection(vec![a, b], Some(b));
    let before = depth(&ed);
    click(&mut ed, row(Op::MakeFrames)).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    assert_eq!(layer(&ed, a).name, "_a_Walk,100");
    assert_eq!(layer(&ed, b).name, "_a_Run,100");
    assert_eq!(
        greyed(&mut ed, row(Op::MakeFrames)),
        "Select top-level layers that are not frames yet"
    );
    // What an animation export would write, before the merge.
    let frames_before = {
        let doc = ed.active().unwrap();
        crate::import::composite_animation_frames(&doc.document, &doc.tiles).unwrap()
    };
    let before = depth(&ed);
    click(&mut ed, row(Op::MergeFrames)).unwrap();
    assert_eq!(depth(&ed), before + 1, "one undo step");
    let now = root(&ed);
    assert_eq!(now.len(), 2, "only the two frames remain");
    assert!(!now.contains(&base), "the background was merged into them");
    let names: Vec<String> = now.iter().map(|id| layer(&ed, *id).name.clone()).collect();
    assert_eq!(
        names,
        ["_a_Run,100", "_a_Walk,100"],
        "the play order is kept"
    );
    let frames_after = {
        let doc = ed.active().unwrap();
        crate::import::composite_animation_frames(&doc.document, &doc.tiles).unwrap()
    };
    assert_eq!(frames_after, frames_before, "each frame looks as it did");
    undo(&mut ed);
    assert!(root(&ed).contains(&base));
    ed.set_layer_selection(vec![a], Some(a));
    click(&mut ed, row(Op::UnmakeFrames)).unwrap();
    assert_eq!(layer(&ed, a).name, "Walk");
    assert_eq!(layer(&ed, b).name, "_a_Run,100", "only the selected frame");
}

// ---------------------------------------------------------------------------
// Where the rows sit
// ---------------------------------------------------------------------------

/// The actions in the Layer menu's submenu at `path` (labels, outermost
/// first), as the shell's own menu bar draws it.
fn layer_submenu(ed: &mut Editor, path: &[&str]) -> Vec<MenuAction> {
    let bar = menus(ed);
    let layer_menu = bar
        .iter()
        .find(|m| m.title == "Layer")
        .expect("a Layer menu");
    let mut entries = &layer_menu.entries;
    for label in path {
        entries = entries
            .iter()
            .find_map(|e| match e {
                ui::menu::Entry::Submenu { label: l, entries } if l == label => Some(entries),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {label} submenu"));
    }
    entries.iter().flat_map(ui::menu::Entry::actions).collect()
}

#[test]
fn each_row_sits_in_photopeas_submenu() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let cases: Vec<(&[&str], Vec<Op>)> = vec![
        (&["New"], vec![Op::ArtboardFromLayers]),
        (&["Layer Mask"], vec![Op::MaskFromTransparency]),
        (&["Layer Style"], vec![Op::CreateLayers]),
        (
            &["Layer Style", "Scale Effects"],
            ui::menu::SCALE_EFFECTS_PERCENTS
                .iter()
                .map(|p| Op::ScaleEffects(*p))
                .collect(),
        ),
        (&["Smart Object"], vec![Op::ResetTransform]),
        (
            &["Smart Object", "Stack Mode"],
            StackMode::ALL.iter().map(|m| Op::StackMode(*m)).collect(),
        ),
        (
            &["Animation"],
            vec![Op::MakeFrames, Op::UnmakeFrames, Op::MergeFrames],
        ),
    ];
    for (path, ops) in cases {
        let rows = layer_submenu(&mut ed, path);
        for op in ops {
            assert!(rows.contains(&row(op)), "{op:?} is not under {path:?}");
        }
    }
    // Every row greys with a reason on a plain unstyled pixel layer where it
    // cannot apply, rather than performing nothing.
    for op in [
        Op::CreateLayers,
        Op::ResetTransform,
        Op::StackMode(StackMode::Mean),
        Op::UnmakeFrames,
        Op::MergeFrames,
    ] {
        assert!(!greyed(&mut ed, row(op)).is_empty(), "{op:?}");
    }
}

// ---------------------------------------------------------------------------
// Greyed before the click, never refused after it
// ---------------------------------------------------------------------------

/// Whether the row resolves to a click (not greyed) on the shell's context.
fn enabled(ed: &mut Editor, action: MenuAction) -> bool {
    let chrome = Chrome::new();
    let ctx = context(ed, chrome.workspace());
    resolve_intent(action, &ctx, ed).is_ok()
}

fn lock_all(ed: &mut Editor, id: LayerId, all: bool) {
    ed.apply_command(Command::SetLayerProperties {
        layer_id: id,
        patch: editor_core::LayerPatch {
            locked: Some(layer_model::LockState {
                all,
                ..Default::default()
            }),
            ..Default::default()
        },
    });
}

#[test]
fn merge_is_greyed_while_any_layer_is_locked() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let base = root(&ed)[0];
    let a = block(&mut ed, "Walk", (4, 4, 10, 10), [200, 20, 20]);
    let b = block(&mut ed, "Run", (30, 30, 10, 10), [20, 200, 20]);
    ed.set_layer_selection(vec![a, b], Some(b));
    click(&mut ed, row(Op::MakeFrames)).unwrap();
    assert!(enabled(&mut ed, row(Op::MergeFrames)), "unlocked: enabled");
    lock_all(&mut ed, base, true);
    assert!(layer(&ed, base).locked.all);
    assert_eq!(
        greyed(&mut ed, row(Op::MergeFrames)),
        "A locked layer cannot be merged away: unlock it first"
    );
    lock_all(&mut ed, base, false);
    assert!(enabled(&mut ed, row(Op::MergeFrames)));
    click(&mut ed, row(Op::MergeFrames)).unwrap();
}

#[test]
fn create_layers_is_greyed_while_the_style_is_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let id = block(&mut ed, "Box", (20, 20, 20, 16), [40, 90, 200]);
    let on = LayerEffects {
        drop_shadow: Some(ShadowEffect {
            distance_px: 4.0,
            size_px: 6.0,
            ..Default::default()
        }),
        ..Default::default()
    };
    set_effects(
        &mut ed,
        id,
        LayerEffects {
            enabled: false,
            ..on.clone()
        },
    );
    assert_eq!(
        greyed(&mut ed, row(Op::CreateLayers)),
        "The layer's style is switched off"
    );
    set_effects(&mut ed, id, on);
    assert!(enabled(&mut ed, row(Op::CreateLayers)));
    click(&mut ed, row(Op::CreateLayers)).unwrap();
}

#[test]
fn from_transparency_is_greyed_on_an_empty_layer() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let empty = layer_model::Layer::raster("Empty");
    let id = empty.id;
    ed.apply_command(Command::create_layer(empty));
    ed.set_active_layer(id);
    assert_eq!(
        greyed(&mut ed, row(Op::MaskFromTransparency)),
        "The layer is empty: it has no transparency to read"
    );
    block(&mut ed, "Box", (2, 2, 4, 4), [1, 2, 3]);
    assert!(enabled(&mut ed, row(Op::MaskFromTransparency)));
}

#[test]
fn smart_object_rows_are_greyed_when_the_source_cannot_serve_them() {
    let dir = tempfile::tempdir().unwrap();
    // A one-layer PSD: nothing to stack.
    let bytes = {
        let mut src = editor_with(dir.path(), ScriptedDialogs::new());
        painted(
            &mut src,
            "Only",
            &[50u8, 10, 10, 255].repeat((W * H) as usize),
        );
        let base = *root(&src).last().unwrap();
        src.apply_command(Command::DeleteLayer { layer_id: base });
        let flat = composite(&mut src);
        let doc = src.active().unwrap();
        crate::import::psd_from_document(&doc.document, &doc.tiles, &flat)
            .unwrap()
            .0
    };
    assert_eq!(ui::menu::psd_header_layer_count(&bytes), Some(1));
    let (mut ed, id) = placed(dir.path(), W, H);
    let LayerKind::SmartObject(so) = &layer(&ed, id).kind else {
        unreachable!()
    };
    let asset = so.asset;
    ed.active_mut()
        .unwrap()
        .document
        .set_asset_origin(layer_model::AssetRecord {
            id: asset,
            origin: AssetOrigin::Embedded {
                name: "one.psd".to_string(),
                bytes,
            },
            source_size: None,
        });
    assert_eq!(
        greyed(&mut ed, row(Op::StackMode(StackMode::Median))),
        "Stack Mode needs two or more visible layers in the smart object"
    );
    ed.apply_command(Command::SetLayerProperties {
        layer_id: id,
        patch: editor_core::LayerPatch {
            transform: Some(glam::Affine2::from_scale(glam::Vec2::splat(2.0)).to_cols_array()),
            ..Default::default()
        },
    });
    assert_eq!(
        greyed(&mut ed, row(Op::ResetTransform)),
        "The smart object does not record its source's size"
    );
}

/// A `depth` PSD of `reds.len()` opaque full-canvas layers, red `reds[i]`,
/// written by this repo's own PSD writer: at 16 and 32 bits the layers go in
/// an `Lr16` / `Lr32` tagged block and the classic layer-info length is 0.
fn deep_psd(depth: psd::Depth, reds: &[u8]) -> Vec<u8> {
    let header = psd::PsdHeader {
        channels: 4,
        width: W,
        height: H,
        depth,
        color_mode: psd::ColorMode::Rgb,
    };
    let sample = |v: u8| -> Vec<u8> {
        match depth {
            psd::Depth::Eight => vec![v],
            psd::Depth::Sixteen => (u16::from(v) * 257).to_be_bytes().to_vec(),
            psd::Depth::ThirtyTwo => (f32::from(v) / 255.0).to_be_bytes().to_vec(),
        }
    };
    let plane = |v: u8| sample(v).repeat((W * H) as usize);
    let mut file = psd::model::PsdFile::new(header);
    for (i, v) in reds.iter().enumerate() {
        let mut layer =
            psd::model::PsdLayer::raster(format!("L{i}"), psd::model::Rect::sized(W, H));
        layer.channels = vec![
            psd::model::Channel::new(psd::model::CHANNEL_ALPHA, plane(255)),
            psd::model::Channel::new(0, plane(*v)),
            psd::model::Channel::new(1, plane(10)),
            psd::model::Channel::new(2, plane(10)),
        ];
        file.layers.push(layer);
    }
    psd::write::write(&file).unwrap()
}

#[test]
fn stack_mode_counts_the_layers_of_a_deep_bit_depth_source_too() {
    let dir = tempfile::tempdir().unwrap();
    for bits in [psd::Depth::Sixteen, psd::Depth::ThirtyTwo] {
        let stack = deep_psd(bits, &[20, 100, 240]);
        let single = deep_psd(bits, &[50]);
        let key: &[u8] = if bits == psd::Depth::Sixteen {
            b"Lr16"
        } else {
            b"Lr32"
        };
        assert!(
            stack.windows(4).any(|w| w == key),
            "{bits:?}: the layers are not in the nested block"
        );
        let counts = (
            ui::menu::psd_header_layer_count(&stack),
            ui::menu::psd_header_layer_count(&single),
        );
        let (mut ed, id) = placed(dir.path(), W, H);
        let LayerKind::SmartObject(so) = &layer(&ed, id).kind else {
            unreachable!()
        };
        let asset = so.asset;
        let embed = |ed: &mut Editor, bytes: Vec<u8>| {
            ed.active_mut()
                .unwrap()
                .document
                .set_asset_origin(layer_model::AssetRecord {
                    id: asset,
                    origin: AssetOrigin::Embedded {
                        name: "deep.psd".to_string(),
                        bytes,
                    },
                    source_size: Some((W, H)),
                });
        };
        // One layer in the nested block: greyed, for the true reason.
        embed(&mut ed, single);
        assert_eq!(
            greyed(&mut ed, row(Op::StackMode(StackMode::Median))),
            "Stack Mode needs two or more visible layers in the smart object",
            "{bits:?}"
        );
        // Three: the row is live and the click performs.
        embed(&mut ed, stack);
        let before = depth(&ed);
        click(&mut ed, row(Op::StackMode(StackMode::Median)))
            .unwrap_or_else(|e| panic!("{bits:?}: {e}"));
        assert_eq!(depth(&ed), before + 1, "one undo step");
        let result = ed.active().unwrap().document.active_layer().unwrap();
        assert_ne!(result, id);
        let px = crate::menu_bridge::pixels::read_layer(ed.active().unwrap(), result);
        assert!(px[0].abs_diff(100) <= 1, "{bits:?}: median {}", px[0]);
        // The helper the gate reads says so directly as well.
        assert_eq!(counts, (Some(3), Some(1)), "{bits:?}");
    }
}

#[test]
fn the_scale_effects_rows_read_as_bare_percentages() {
    assert_eq!(Op::ScaleEffects(25).label(), "25%");
    assert_eq!(Op::ScaleEffects(200).label(), "200%");
}
