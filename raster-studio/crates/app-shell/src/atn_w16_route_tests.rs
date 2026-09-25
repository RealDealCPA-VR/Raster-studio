//! W16-H: a Photoshop-recorded action (Levels, Image Size, Gaussian Blur,
//! Duplicate) opened through File > Open and played from the Actions panel
//! through the real chrome frame; and a recording of the common edits
//! exported as `.atn`, parsed back with every step, and played on a fresh
//! document to the same result.

use std::path::{Path, PathBuf};

use asset_store::resources::atn::{self, AtnAction, AtnSet, AtnStep, StepOp};
use psd::{Descriptor, RefItem, Value};
use ui::panels::actions::{self as panel, ActionsRequest, ActionsView};

use crate::action::Action;
use crate::chrome::{install_theme, Chrome};
use crate::dialogs::ScriptedDialogs;
use crate::editor::{Editor, Effect};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

/// 64 x 64: the left half grey 40, the right half grey 220, opaque.
fn halves(dir: &Path) -> PathBuf {
    let (w, h) = (64usize, 64usize);
    let mut rgba = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let v = if x < w / 2 { 40 } else { 220 };
            let i = (y * w + x) * 4;
            rgba[i..i + 3].copy_from_slice(&[v, v, v]);
        }
    }
    let path = dir.join("halves.png");
    std::fs::write(
        &path,
        raster::encode(raster::ExportFormat::Png, w as u32, h as u32, &rgba).unwrap(),
    )
    .unwrap();
    path
}

fn d(class: &str, items: Vec<(&str, Value)>) -> Descriptor {
    let mut out = Descriptor::new(class);
    for (k, v) in items {
        out.push(k, v).unwrap();
    }
    out
}

fn uf(unit: &[u8; 4], value: f64) -> Value {
    Value::UnitFloat { unit: *unit, value }
}

/// The four steps as Photoshop records them — descriptors laid down key by
/// key here, not by this application's writer.
fn photoshop_set() -> AtnSet {
    let levels = AtnStep::new(
        "Lvls",
        "Levels",
        Some(d(
            "null",
            vec![
                (
                    "presetKind",
                    Value::Enumerated {
                        type_id: "presetKindType".into(),
                        value: "presetKindCustom".into(),
                    },
                ),
                (
                    "Adjs",
                    Value::List(vec![Value::Descriptor(d(
                        "LvlA",
                        vec![
                            (
                                "Chnl",
                                Value::Reference(vec![RefItem::Enumerated {
                                    name: String::new(),
                                    class_id: "Chnl".into(),
                                    type_id: "Chnl".into(),
                                    value: "Cmps".into(),
                                }]),
                            ),
                            (
                                "Inpt",
                                Value::List(vec![Value::Integer(20), Value::Integer(235)]),
                            ),
                        ],
                    ))]),
                ),
            ],
        )),
    );
    let image_size = AtnStep::new(
        "ImgS",
        "Image Size",
        Some(d(
            "null",
            vec![
                ("Wdth", uf(b"#Prc", 50.0)),
                ("CnsP", Value::Bool(true)),
                (
                    "Intr",
                    Value::Enumerated {
                        type_id: "Intp".into(),
                        value: "Bcbc".into(),
                    },
                ),
            ],
        )),
    );
    let mut blur = AtnStep::new(
        "gaussianBlur",
        "Gaussian Blur",
        Some(d("null", vec![("Rds ", uf(b"#Pxl", 2.0))])),
    );
    blur.char_id = false;
    let duplicate = AtnStep::new(
        "Dplc",
        "Duplicate",
        Some(d(
            "null",
            vec![
                (
                    "null",
                    Value::Reference(vec![RefItem::Enumerated {
                        name: String::new(),
                        class_id: "Lyr ".into(),
                        type_id: "Ordn".into(),
                        value: "Trgt".into(),
                    }]),
                ),
                ("Nm  ", Value::Text("Copy of it".into())),
                ("Vrsn", Value::Integer(5)),
            ],
        )),
    );
    AtnSet {
        name: "Recorded".into(),
        expanded: true,
        actions: vec![AtnAction {
            name: "Tone and size".into(),
            steps: vec![levels, image_size, blur, duplicate],
            ..AtnAction::default()
        }],
    }
}

fn frames(chrome: &mut Chrome, ctx: &egui::Context, ed: &mut Editor, n: usize) {
    for _ in 0..n {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                ..Default::default()
            },
            |ctx| {
                let _ = chrome.ui(ctx, ed);
            },
        );
    }
}

fn levels_of(v: f64) -> f64 {
    ((v - 20.0) / 215.0 * 255.0).clamp(0.0, 255.0)
}

#[test]
fn a_photoshop_levels_image_size_blur_duplicate_action_plays_and_changes_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let atn_path = dir.path().join("recorded.atn");
    std::fs::write(&atn_path, atn::write(&photoshop_set()).unwrap()).unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(atn_path));
    ed.open_path(&halves(dir.path())).unwrap();
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Panels));
    let status = ed.status().unwrap_or_default().to_string();
    assert!(
        status.contains("“Recorded”") && !status.contains("no equivalent"),
        "every step maps: {status}"
    );

    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    frames(&mut chrome, &ctx, &mut ed, 2);
    let view = ActionsView::published(&ctx);
    let index = view
        .actions
        .iter()
        .position(|a| a.name == "Recorded / Tone and size")
        .expect("the imported action is listed under its set");
    assert!(
        view.actions[index]
            .steps
            .iter()
            .all(|s| !s.contains("skipped")),
        "{:?}",
        view.actions[index].steps
    );

    let layers_before = ed.active().unwrap().document.layers.len();
    panel::request(&ctx, ActionsRequest::Play(index));
    frames(&mut chrome, &ctx, &mut ed, 1);
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.starts_with("Played 4 step(s)"), "{status}");

    let doc = ed.active_mut().unwrap();
    assert_eq!(
        (doc.document.width(), doc.document.height()),
        (32, 32),
        "Image Size 50% (height in proportion)"
    );
    assert_eq!(doc.document.layers.len(), layers_before + 1, "Duplicate");
    let top = doc.document.layers.root()[0];
    assert_eq!(doc.document.layers.get(top).unwrap().name, "Copy of it");
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    let grey = |x: usize, y: usize| f64::from(rgba[(y * 32 + x) * 4]);
    assert!(
        (grey(2, 16) - levels_of(40.0)).abs() <= 3.0,
        "Levels moved the dark half from 40 to {} (expected {:.1})",
        grey(2, 16),
        levels_of(40.0)
    );
    assert!(
        (grey(29, 16) - levels_of(220.0)).abs() <= 3.0,
        "Levels moved the light half from 220 to {} (expected {:.1})",
        grey(29, 16),
        levels_of(220.0)
    );
    let (lo, hi) = (levels_of(40.0) + 20.0, levels_of(220.0) - 20.0);
    assert!(
        grey(15, 16) > lo && grey(15, 16) < hi && grey(16, 16) > lo && grey(16, 16) < hi,
        "Gaussian Blur softened the edge: {} {}",
        grey(15, 16),
        grey(16, 16)
    );
    assert!(doc.history.undo_depth() >= 4, "one undo step per step");
}

/// The default set's index in the library.
fn default_set(ed: &Editor) -> usize {
    ed.action_sets()
        .iter()
        .position(|s| s == super::DEFAULT_SET)
        .expect("the default set is listed")
}

#[test]
fn a_recording_of_the_common_edits_exports_every_step_and_replays_to_the_same_document() {
    use ui::menu::{AdjustmentId, ColorMode, MenuAction};
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    let png = halves(dir.path());
    ed.open_path(&png).unwrap();
    ed.open_path(&png).unwrap();
    ed.activate(0).unwrap();

    ed.start_recording();
    // Invert and Grayscale on the background, through the menu.
    crate::menu_bridge::perform(MenuAction::ApplyAdjustment(AdjustmentId::Invert), &mut ed)
        .unwrap();
    crate::menu_bridge::perform(MenuAction::SetColorMode(ColorMode::Grayscale), &mut ed).unwrap();
    // Layer ▸ New ▸ Layer, then the Layers panel's rename, opacity, blend.
    // (The recorder captures the layer the command makes; the new layer
    // becomes the active one, as Layer ▸ New ▸ Layer leaves it.)
    let layer = layer_model::Layer::raster("Layer 1");
    let new_id = layer.id;
    ed.apply_command(editor_core::Command::create_layer(layer));
    ed.set_layer_selection(vec![new_id], Some(new_id));
    ed.apply_command(editor_core::Command::SetLayerProperties {
        layer_id: new_id,
        patch: editor_core::LayerPatch {
            name: Some("Glow".into()),
            opacity: Some(0.5),
            blend_mode: Some(layer_model::BlendMode::Screen),
            ..Default::default()
        },
    });
    ed.apply_command(editor_core::Command::SetLayerProperties {
        layer_id: new_id,
        patch: editor_core::LayerPatch {
            visible: Some(false),
            ..Default::default()
        },
    });
    // A Free Transform commit on it: move, scale, turn.
    let m = glam::Affine2::from_translation(glam::vec2(3.0, -2.0))
        * glam::Affine2::from_angle(0.25)
        * glam::Affine2::from_scale(glam::vec2(0.5, 1.5));
    ed.apply_command(editor_core::Command::TransformLayer {
        layer_id: new_id,
        matrix: m.to_cols_array(),
    });
    // Duplicate Layer, Select All, Deselect, Image Size, Canvas Size,
    // Flatten — each through its own route.
    crate::layer_ops::duplicate_layer(&mut ed, None).unwrap();
    crate::menu_bridge::perform(MenuAction::SelectAll, &mut ed).unwrap();
    crate::menu_bridge::perform(MenuAction::Deselect, &mut ed).unwrap();
    {
        let doc = ed.active_mut().unwrap();
        let spec = ui::dialogs::ImageSizeSpec {
            width: 48,
            height: 40,
            resolution_ppi: 72.0,
            resample: Some(raster::ResampleFilter::Lanczos3),
        };
        let command = doc.resample_command(&spec).unwrap();
        ed.apply_command(command);
    }
    {
        let doc = ed.active_mut().unwrap();
        let anchor = ui::dialogs::Anchor::at(2, 2).unwrap();
        let spec = ui::dialogs::CanvasSizeSpec {
            width: 60,
            height: 50,
            offset: anchor.offset((48, 40), (60, 50)),
            anchor,
            background: ui::dialogs::BackgroundContents::Transparent,
        };
        let command = doc.canvas_size_command(&spec).unwrap();
        ed.apply_command(command);
    }
    crate::menu_bridge::perform(MenuAction::FlattenImage, &mut ed).unwrap();
    let edits = ed.stop_recording().unwrap();
    let labels: Vec<String> = edits.iter().map(|e| e.command.label()).collect();
    assert_eq!(edits.len(), 12, "every edit was captured: {labels:?}");
    ed.keep_recording(&edits);

    let (set, left_out) = ed.action_set_as_atn(default_set(&ed)).unwrap();
    assert_eq!(left_out, 0, "every recorded edit has a Photoshop step");
    let steps = &set.actions[0].steps;
    let ops: Vec<StepOp> = steps
        .iter()
        .map(|s| atn::interpret(s).unwrap_or_else(|e| panic!("{}: {e}", s.name)))
        .collect();
    let names: Vec<&str> = steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Invert",
            "Convert Mode",
            "Make",
            "Set",
            "Hide",
            "Transform",
            "Duplicate",
            "Set Selection",
            "Set Selection",
            "Image Size",
            "Crop",
            "Flatten Image"
        ],
        "{ops:?}"
    );
    let back = atn::parse(&atn::write(&set).unwrap()).unwrap();
    assert_eq!(back, set, "export -> parse keeps every step");

    // Played on the untouched second document, the exported steps land on
    // the same document the recording made.
    let recorded = {
        let doc = ed.active_mut().unwrap();
        let rgba = doc.composite(doc.canvas_rect()).unwrap();
        (
            doc.document.width(),
            doc.document.height(),
            doc.document.layers.len(),
            doc.document.meta.color_mode,
            rgba,
        )
    };
    let mut exported = back;
    exported.name = "Exported".into();
    ed.import_action_set(exported, "exported.atn");
    let index = ed
        .actions()
        .iter()
        .position(|a| a.set == "Exported")
        .expect("imported");
    ed.activate(1).unwrap();
    let report = ed.play_action_from(index, 0).unwrap();
    assert!(
        report.skipped.is_empty() && report.failed.is_empty(),
        "{report:?}"
    );
    assert_eq!(report.applied, 12);
    let doc = ed.active_mut().unwrap();
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    assert_eq!(
        (
            doc.document.width(),
            doc.document.height(),
            doc.document.layers.len(),
            doc.document.meta.color_mode,
        ),
        (recorded.0, recorded.1, recorded.2, recorded.3)
    );
    assert_eq!(
        rgba, recorded.4,
        "the replayed pixels are the recorded ones"
    );
}

#[test]
fn a_free_transform_step_scales_the_layer_about_its_centre() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&halves(dir.path())).unwrap();
    let id = ed.active().unwrap().document.active_layer().unwrap();
    let step = StepOp::Transform {
        target: atn::TransformTarget::Layer,
        pivot: atn::Pivot::Bounds([0.5, 0.5]),
        offset: [0.0, 0.0],
        scale: [50.0, 50.0],
        angle: 0.0,
        skew: [0.0, 0.0],
    }
    .to_step();
    ed.import_action_set(
        AtnSet {
            name: "T".into(),
            expanded: true,
            actions: vec![AtnAction {
                name: "Half".into(),
                steps: vec![step],
                ..AtnAction::default()
            }],
        },
        "t.atn",
    );
    let index = ed.actions().iter().position(|a| a.set == "T").unwrap();
    let report = ed.play_action_from(index, 0).unwrap();
    assert_eq!(report.applied, 1, "{report:?}");
    let t = ed
        .active()
        .unwrap()
        .document
        .layers
        .get(id)
        .unwrap()
        .transform;
    let corner = t.transform_point2(glam::Vec2::ZERO);
    let far = t.transform_point2(glam::vec2(64.0, 64.0));
    assert!(
        (corner - glam::vec2(16.0, 16.0)).length() < 1e-3
            && (far - glam::vec2(48.0, 48.0)).length() < 1e-3,
        "half size about the centre: {corner} {far}"
    );
}

#[test]
fn a_recorded_transform_decomposes_into_the_same_matrix() {
    let m = glam::Affine2::from_translation(glam::vec2(3.0, -2.0))
        * glam::Affine2::from_angle(0.4)
        * glam::Affine2::from_mat2(glam::Mat2::from_cols(
            glam::vec2(1.0, 0.0),
            glam::vec2(0.3, 1.0),
        ))
        * glam::Affine2::from_scale(glam::vec2(0.5, -1.5));
    let Some(StepOp::Transform {
        offset,
        scale,
        angle,
        skew,
        ..
    }) = super::atn_record::transform_of(m.to_cols_array())
    else {
        panic!("a transform step")
    };
    let rebuilt = glam::Affine2::from_translation(glam::vec2(offset[0] as f32, offset[1] as f32))
        * glam::Affine2::from_mat2(super::atn_play::transform_linear(scale, angle, skew));
    for (a, b) in rebuilt.to_cols_array().iter().zip(m.to_cols_array()) {
        assert!((a - b).abs() < 1e-4, "{rebuilt:?} vs {m:?}");
    }
}
