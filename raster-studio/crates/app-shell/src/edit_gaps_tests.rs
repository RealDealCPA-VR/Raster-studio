//! W10-G: the Edit gaps driven through the real routes — the menu row's
//! resolution, `DialogHost::open_for_menu_action`, the host's frame with
//! Enter, and `menu_bridge::perform` against a live editor.

use super::*;
use crate::dialog_host::DialogHost;
use crate::dialogs::ScriptedDialogs;
use crate::menu_bridge::{context, perform, resolve, Pick};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use filters::align::Similarity;
use filters::EdgeMode;
use ui::Workspace;

fn editor(dir: &std::path::Path) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
    ed
}

/// An editor with one `w x h` document whose only layer holds `rgba`.
fn with_document(dir: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> (Editor, LayerId) {
    let mut ed = editor(dir);
    let bytes = raster::encode(raster::ExportFormat::Png, w, h, rgba).unwrap();
    let path = dir.join("w10g.png");
    std::fs::write(&path, bytes).unwrap();
    ed.open_path(&path).expect("the fixture opens");
    let layer = ed.active().unwrap().document.active_layer().unwrap();
    crate::fade::forget();
    (ed, layer)
}

/// A new raster layer on top holding `rgba`.
fn add_layer(ed: &mut Editor, name: &str, rgba: &[u8]) -> LayerId {
    let layer = layer_model::Layer::raster(name);
    let id = layer.id;
    ed.apply_command(Command::create_layer(layer));
    let paint = {
        let doc = ed.active_mut().unwrap();
        pixels::write_layer(doc, id, rgba, "Fixture").unwrap()
    };
    ed.apply_command(paint);
    id
}

fn frame(host: &mut DialogHost, ctx: &egui::Context, keys: &[egui::Key]) -> ChromeOutput {
    let events = keys
        .iter()
        .map(|key| egui::Event::Key {
            key: *key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        })
        .collect();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = ChromeOutput::default();
    let _ = ctx.run(input, |ctx| host.ui(ctx, None, &mut out));
    out
}

fn egui_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    ctx
}

/// The row is live and the host opens its dialog; `edit` sets the dialog
/// up; Enter confirms it and the pick rides `out.menu` to `perform`.
fn confirm_through_host(
    ed: &mut Editor,
    action: MenuAction,
    edit: impl FnOnce(&mut GapDialog),
) -> Result<String, String> {
    let live = context(ed, &Workspace::new());
    match resolve(action, &live, ed) {
        Ok(Pick::Menu(a)) if a == action => {}
        other => panic!("{} is not a live row: {other:?}", action.label()),
    }
    let ctx = egui_ctx();
    let mut host = DialogHost::default();
    assert!(
        host.open_for_menu_action(&action, ed),
        "{} opened nothing",
        action.label()
    );
    edit(host.active_edit_gap_for_test());
    let _ = frame(&mut host, &ctx, &[]);
    let out = frame(&mut host, &ctx, &[egui::Key::Enter]);
    assert!(!host.is_open(), "Enter closed the dialog");
    assert_eq!(out.menu, vec![action]);
    perform(action, ed)
}

fn gradient(w: u32, h: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend([(x * 255 / w) as u8, (y * 255 / h) as u8, 90, 255]);
        }
    }
    rgba
}

#[test]
fn fade_at_fifty_percent_after_invert_gives_mid_values_as_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (24, 16);
    let original = gradient(w, h);
    let (mut ed, layer) = with_document(dir.path(), w, h, &original);

    // Nothing fadeable yet: the row is greyed and says why.
    let live = context(&mut ed, &Workspace::new());
    assert_eq!(live.fade_step, None);
    assert_eq!(
        MenuAction::Fade.resolve(&live),
        ui::Resolution::Disabled(ui::menu::FADE_NOTHING)
    );

    perform(
        MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Invert),
        &mut ed,
    )
    .unwrap();
    let inverted = pixels::read_layer(ed.active().unwrap(), layer);
    let depth = ed.active().unwrap().history.undo_depth();
    let live = context(&mut ed, &Workspace::new());
    assert_eq!(live.fade_step.as_deref(), Some("Apply Invert"));

    let spec = FadeSpec {
        opacity: 0.5,
        mode: layer_model::BlendMode::Normal,
    };
    let message = confirm_through_host(&mut ed, MenuAction::Fade, |d| match d {
        GapDialog::Fade(f) => f.set_spec(spec),
        other => panic!("{other:?}"),
    })
    .unwrap();
    assert!(message.starts_with("Fade Apply Invert"), "{message}");

    let faded = pixels::read_layer(ed.active().unwrap(), layer);
    for (i, v) in faded.iter().enumerate() {
        if i % 4 == 3 {
            assert_eq!(*v, 255);
            continue;
        }
        // Half the original and half its inverse is mid-grey.
        assert!(
            (f32::from(*v) - 127.5).abs() <= 1.5,
            "channel {i}: {} inverted to {}, faded to {v}",
            original[i],
            inverted[i]
        );
    }
    let open = ed.active().unwrap();
    assert_eq!(
        open.history.undo_depth(),
        depth,
        "the fade replaced the step"
    );
    assert_eq!(open.history.undo_label(), Some("Fade Apply Invert"));
    // A fade is not itself fadeable, and one undo is the original.
    assert_eq!(context(&mut ed, &Workspace::new()).fade_step, None);
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), original);
}

#[test]
fn fade_greys_out_once_the_step_is_undone_or_followed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, _) = with_document(dir.path(), 8, 8, &gradient(8, 8));
    perform(
        MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Invert),
        &mut ed,
    )
    .unwrap();
    assert!(crate::fade::fadeable(&ed).is_some());
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(crate::fade::fadeable(&ed), None, "undone");
    ed.active_mut().unwrap().redo().unwrap();
    assert!(
        crate::fade::fadeable(&ed).is_some(),
        "redone, it is the last step again"
    );
    ed.apply_command(Command::create_layer(layer_model::Layer::raster("Later")));
    assert_eq!(crate::fade::fadeable(&ed), None, "another step followed");
    let mut host = DialogHost::default();
    assert!(!host.open_for_menu_action(&MenuAction::Fade, &ed));
    assert_eq!(
        perform(MenuAction::Fade, &mut ed).unwrap_err(),
        ui::menu::FADE_NOTHING
    );
}

/// A scene with plenty of corners.
fn scene(w: u32, h: u32) -> FilterBuffer {
    let mut b = FilterBuffer::transparent(w, h).unwrap();
    let hash = |i: u32, k: u32| -> f32 {
        let mut v = i.wrapping_mul(0x9E37_79B9) ^ k.wrapping_mul(0x85EB_CA6B);
        v ^= v >> 15;
        v = v.wrapping_mul(0x2C1B_3C6D);
        v ^= v >> 12;
        (v % 10_000) as f32 / 10_000.0
    };
    for y in 0..h {
        for x in 0..w {
            let g = 0.1 + 0.3 * (x as f32 / w as f32) + 0.1 * (y as f32 / h as f32);
            b.set(x, y, [g, g, g, 1.0]);
        }
    }
    for i in 0..80 {
        let cx = hash(i, 1) * w as f32;
        let cy = hash(i, 2) * h as f32;
        let rw = 4.0 + hash(i, 3) * 20.0;
        let rh = 4.0 + hash(i, 4) * 20.0;
        let v = hash(i, 5);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                let inside = if i % 3 == 0 {
                    dx * dx + dy * dy < rw * rw * 0.5
                } else {
                    dx.abs() < rw * 0.5 && dy.abs() < rh * 0.5
                };
                if inside {
                    b.set(x, y, [v, v * 0.8, v * 0.6, 1.0]);
                }
            }
        }
    }
    b
}

/// `moving(p) = reference(s(p))`.
fn through(reference: &FilterBuffer, s: Similarity) -> FilterBuffer {
    let (w, h) = reference.dimensions();
    let mut out = FilterBuffer::transparent(w, h).unwrap();
    for y in 0..h {
        for x in 0..w {
            let p = s.apply([f64::from(x) + 0.5, f64::from(y) + 0.5]);
            out.set(
                x,
                y,
                reference.sample_bilinear(p[0] as f32, p[1] as f32, EdgeMode::Clamp),
            );
        }
    }
    out
}

#[test]
fn auto_align_recovers_a_known_shift_and_rotation_as_a_layer_transform() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (200, 200);
    let reference = scene(w, h);
    let angle = 3.0f64.to_radians();
    let (sn, cs) = angle.sin_cos();
    let (cx, cy) = (100.0, 100.0);
    let truth = Similarity {
        scale: 1.0,
        angle,
        tx: cx - (cs * cx - sn * cy) + 7.3,
        ty: cy - (sn * cx + cs * cy) - 5.6,
    };
    let moving = through(&reference, truth);
    let (mut ed, bottom) = with_document(dir.path(), w, h, &reference.to_rgba8());
    let top = add_layer(&mut ed, "Moved", &moving.to_rgba8());

    // One layer selected: greyed, with the reason.
    ed.set_layer_selection(vec![top], Some(top));
    let live = context(&mut ed, &Workspace::new());
    assert!(matches!(
        MenuAction::AutoAlignLayers.resolve(&live),
        ui::Resolution::Disabled(_)
    ));

    ed.set_layer_selection(vec![top, bottom], Some(top));
    let depth = ed.active().unwrap().history.undo_depth();
    confirm_through_host(&mut ed, MenuAction::AutoAlignLayers, |_| {}).unwrap();
    let open = ed.active().unwrap();
    assert_eq!(open.history.undo_depth(), depth + 1, "one undo step");
    assert_eq!(
        open.document.layers.get(bottom).unwrap().transform,
        glam::Affine2::IDENTITY,
        "the reference stays put"
    );
    let t = open.document.layers.get(top).unwrap().transform;
    let got_angle = f64::from(t.matrix2.x_axis.y.atan2(t.matrix2.x_axis.x));
    let off = (got_angle - angle).to_degrees();
    assert!(off.abs() <= 0.5, "rotation off by {off} degrees: {t:?}");
    for p in [[100.0, 100.0], [30.0, 30.0], [170.0, 40.0], [40.0, 170.0]] {
        let a = t.transform_point2(glam::Vec2::new(p[0] as f32, p[1] as f32));
        let b = truth.apply(p);
        let err = (f64::from(a.x) - b[0]).hypot(f64::from(a.y) - b[1]);
        assert!(err <= 1.0, "{p:?}: {a:?} vs {b:?} ({err} px)");
    }
}

fn detail(w: u32, h: u32) -> FilterBuffer {
    let mut b = FilterBuffer::transparent(w, h).unwrap();
    for y in 0..h {
        for x in 0..w {
            let k = (x / 2).wrapping_mul(73_856_093) ^ (y / 2).wrapping_mul(19_349_663);
            let v = (k % 997) as f32 / 996.0;
            b.set(x, y, [v, v, v, 1.0]);
        }
    }
    b
}

#[test]
fn auto_blend_stack_keeps_the_sharp_half_of_each_layer_in_a_new_layer() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (96, 64);
    let sharp = detail(w, h);
    let soft = filters::blur::gaussian_blur(&sharp, 3.0, EdgeMode::Clamp);
    let halves = |left: &FilterBuffer, right: &FilterBuffer| {
        let mut out = left.clone();
        for y in 0..h {
            for x in w / 2..w {
                out.set(x, y, right.get(x, y));
            }
        }
        out.to_rgba8()
    };
    let (mut ed, near) = with_document(dir.path(), w, h, &halves(&sharp, &soft));
    let far = add_layer(&mut ed, "Far", &halves(&soft, &sharp));
    ed.set_layer_selection(vec![near, far], Some(far));
    let before = ed.active().unwrap().document.layers.len();
    confirm_through_host(&mut ed, MenuAction::AutoBlendLayers, |d| match d {
        GapDialog::AutoBlend(b) => b.set_spec(AutoBlendSpec {
            method: filters::blend_layers::BlendMethod::StackImages,
        }),
        other => panic!("{other:?}"),
    })
    .unwrap();
    let open = ed.active().unwrap();
    assert_eq!(open.document.layers.len(), before + 1, "one new layer");
    assert_eq!(open.history.undo_label(), Some("Auto-Blend Layers"));
    let blended = open
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| {
            open.document
                .layers
                .get(*id)
                .unwrap()
                .name
                .starts_with("Auto-Blend")
        })
        .expect("the blended layer");
    let out = pixels::read_layer(open, blended);
    let want = sharp.to_rgba8();
    let err = |xs: std::ops::Range<u32>, img: &[u8]| {
        let (mut s, mut n) = (0.0f32, 0.0f32);
        for y in 0..h {
            for x in xs.clone() {
                let i = ((y * w + x) * 4) as usize;
                s += (f32::from(img[i]) - f32::from(want[i])).abs();
                n += 1.0;
            }
        }
        s / n
    };
    assert!(err(0..40, &out) < 2.0, "left: {}", err(0..40, &out));
    assert!(err(56..96, &out) < 2.0, "right: {}", err(56..96, &out));
    // Each input was clearly wrong on its soft half.
    let near_px = pixels::read_layer(open, near);
    let far_px = pixels::read_layer(open, far);
    assert!(err(56..96, &near_px) > 10.0);
    assert!(err(0..40, &far_px) > 10.0);
}

#[test]
fn perspective_warp_moves_the_plane_as_one_step_and_an_unmoved_quad_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (48, 32);
    let original = gradient(w, h);
    let (mut ed, layer) = with_document(dir.path(), w, h, &original);
    let depth = ed.active().unwrap().history.undo_depth();

    // An unmoved quad: Enter does not confirm (the identity is blocked).
    let ctx = egui_ctx();
    let mut host = DialogHost::default();
    assert!(host.open_for_menu_action(&MenuAction::PerspectiveWarp, &ed));
    match host.active_edit_gap_for_test() {
        GapDialog::PerspectiveWarp(d) => {
            d.add_quad([4.0, 4.0], [40.0, 28.0]).unwrap();
        }
        other => panic!("{other:?}"),
    }
    let _ = frame(&mut host, &ctx, &[]);
    let out = frame(&mut host, &ctx, &[egui::Key::Enter]);
    assert!(out.menu.is_empty() && host.is_open());

    let message = confirm_through_host(&mut ed, MenuAction::PerspectiveWarp, |d| match d {
        GapDialog::PerspectiveWarp(d) => {
            let q = d.add_quad([4.0, 4.0], [40.0, 28.0]).unwrap();
            d.set_mode(ui::dialogs::WarpMode::Warp);
            d.move_corner(q, 1, [46.0, 1.0]);
            d.move_corner(q, 2, [44.0, 31.0]);
        }
        other => panic!("{other:?}"),
    })
    .unwrap();
    assert_eq!(message, "Perspective Warp applied");
    let open = ed.active().unwrap();
    assert_eq!(open.history.undo_depth(), depth + 1);
    let after = pixels::read_layer(open, layer);
    assert_ne!(after, original);
    // Outside every quad the layer is untouched.
    assert_eq!(after[..4], original[..4]);
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), original);
}

fn pattern(name: &str, v: u8) -> PatternPreset {
    PatternPreset {
        name: name.into(),
        width: 2,
        height: 2,
        rgba8: vec![v; 16],
    }
}

#[test]
fn preset_manager_renames_reorders_deletes_and_round_trips_json() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    for (name, v) in [("Dots", 10), ("Grid", 20), ("Noise", 30)] {
        ed.presets_mut().define_pattern(pattern(name, v));
    }
    ed.presets_mut().define_style("Glow", "{}".into());
    let export = dir.path().join("exported.json");

    let message = confirm_through_host(&mut ed, MenuAction::PresetManager, |d| {
        let GapDialog::PresetManager(d) = d else {
            panic!("{d:?}")
        };
        // Export the untouched library through the file road.
        PICKED_PRESET_FILE_FOR_TEST.with(|p| *p.borrow_mut() = Some(export.clone()));
        serve_file_request(d, PresetFileRequest::Export);
        d.set_kind(ui::dialogs::PresetKind::Patterns);
        d.select(Some(2));
        assert!(d.rename_selected("Static"));
        assert!(d.move_selected(-1));
        d.select(Some(0));
        assert!(d.delete_selected());
    })
    .unwrap();
    assert!(message.contains("presets kept"), "{message}");
    let names = |store: &PresetStore| store.pattern_names();
    assert_eq!(names(ed.presets()), ["Static", "Grid"]);
    // The pixels travelled with the renamed entry.
    assert_eq!(
        ed.presets().pattern("Static").unwrap().rgba8,
        vec![30u8; 16]
    );
    assert_eq!(ed.presets().styles().len(), 1);
    // Saved: a fresh load of the store file has the same patterns.
    let saved = PresetStore::load(&ed.paths().presets_file());
    assert_eq!(names(&saved), ["Static", "Grid"]);

    // The exported file imports back, bringing "Dots" and "Noise" again.
    let imported = read_library(&export).unwrap();
    assert_eq!(imported.patterns.len(), 3);
    confirm_through_host(&mut ed, MenuAction::PresetManager, |d| {
        let GapDialog::PresetManager(d) = d else {
            panic!("{d:?}")
        };
        PICKED_PRESET_FILE_FOR_TEST.with(|p| *p.borrow_mut() = Some(export.clone()));
        serve_file_request(d, PresetFileRequest::Import);
    })
    .unwrap();
    assert_eq!(names(ed.presets()), ["Static", "Grid", "Dots", "Noise"]);

    // A file whose payload is not a preset is refused at import.
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, r#"{"patterns":[{"name":"X","data":"not json"}]}"#).unwrap();
    assert!(read_library(&bad).is_err());
}

/// Swatches and tool presets live on the chrome's workspace, not in the
/// preset store: the per-frame `Editor::sync_panel_presets` publishes them
/// for the dialog, and a confirmed edit lands back in the panels (and the
/// preferences file) on the next frame.
#[test]
fn preset_manager_edits_the_swatches_and_tool_presets_panels() {
    use ui::dialogs::PresetKind;
    use ui::panels::tool_presets::ToolPreset;
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let mut w = Workspace::new();
    let first = w.swatches.get(0).unwrap().clone();
    let second = w.swatches.get(1).unwrap().clone();
    let swatch_count = w.swatches.len();
    for name in ["Soft Brush", "Hard Brush"] {
        let preset = ToolPreset::capture(name, tools::ToolId::Brush, &w.options);
        w.tool_presets.add(preset);
    }
    ed.sync_panel_presets(&mut w);

    let export = dir.path().join("with-panels.json");
    let message = confirm_through_host(&mut ed, MenuAction::PresetManager, |d| {
        let GapDialog::PresetManager(d) = d else {
            panic!("{d:?}")
        };
        // The dialog opened over the panels' lists.
        assert_eq!(d.library().swatches.len(), swatch_count);
        assert_eq!(d.library().swatches[0].name, first.name);
        let tools: Vec<_> = d
            .library()
            .tool_presets
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(tools, ["Soft Brush", "Hard Brush"]);
        PICKED_PRESET_FILE_FOR_TEST.with(|p| *p.borrow_mut() = Some(export.clone()));
        serve_file_request(d, PresetFileRequest::Export);
        d.set_kind(PresetKind::Swatches);
        d.select(Some(1));
        assert!(d.rename_selected("Ink"));
        assert!(d.move_selected(-1));
        d.set_kind(PresetKind::ToolPresets);
        d.select(Some(0));
        assert!(d.delete_selected());
    })
    .unwrap();
    assert!(message.contains("presets kept"), "{message}");

    // The next frame puts the edit into the panels.
    ed.sync_panel_presets(&mut w);
    assert_eq!(w.swatches.len(), swatch_count);
    assert_eq!(w.swatches.get(0).unwrap().name, "Ink");
    assert_eq!(w.swatches.get(0).unwrap().rgba, second.rgba);
    assert_eq!(w.swatches.get(1).unwrap().name, first.name);
    let names: Vec<_> = w
        .tool_presets
        .presets()
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["Hard Brush"]);
    assert_eq!(w.tool_presets.presets()[0].tool, tools::ToolId::Brush);
    // ...and the frame after writes the palette to the preferences file.
    ed.sync_panel_presets(&mut w);
    let prefs = std::fs::read_to_string(ed.paths().preferences_file()).unwrap();
    assert!(prefs.contains("\"Ink\""), "{prefs}");

    // Nothing edited: refused, and the panels are left alone.
    let err = confirm_through_host(&mut ed, MenuAction::PresetManager, |_| {}).unwrap_err();
    assert!(err.contains("nothing changed"), "{err}");

    // The exported file carries both kinds and imports back.
    let imported = read_library(&export).unwrap();
    assert_eq!(imported.swatches.len(), swatch_count);
    assert_eq!(imported.tool_presets.len(), 2);
    confirm_through_host(&mut ed, MenuAction::PresetManager, |d| {
        let GapDialog::PresetManager(d) = d else {
            panic!("{d:?}")
        };
        PICKED_PRESET_FILE_FOR_TEST.with(|p| *p.borrow_mut() = Some(export.clone()));
        serve_file_request(d, PresetFileRequest::Import);
    })
    .unwrap();
    ed.sync_panel_presets(&mut w);
    let names: Vec<_> = w
        .tool_presets
        .presets()
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["Hard Brush", "Soft Brush"]);

    // A swatch or tool preset payload that is not one is refused at import.
    let bad = dir.path().join("bad-swatch.json");
    std::fs::write(&bad, r#"{"swatches":[{"name":"X","data":"[1,2]"}]}"#).unwrap();
    assert!(read_library(&bad).is_err());
    let bad = dir.path().join("bad-tool.json");
    std::fs::write(
        &bad,
        r#"{"tool_presets":[{"name":"X","data":"{\"name\":\"X\",\"tool\":\"No Such Tool\"}"}]}"#,
    )
    .unwrap();
    assert!(read_library(&bad).is_err());
}

/// Fade after a selection-limited filter, in a mode that changes a pixel
/// blended with itself: the pixels the filter did not touch stay exactly as
/// they were, the ones it did are laid over the original by the mode.
#[test]
fn fade_multiply_after_a_selection_limited_invert_keeps_the_unselected_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (16, 8);
    let original = gradient(w, h);
    let (mut ed, layer) = with_document(dir.path(), w, h, &original);
    ed.apply_command(Command::SetSelection {
        selection: editor_core::Selection::Rect {
            min: glam::IVec2::ZERO,
            max: glam::IVec2::new(8, 8),
        },
    });
    perform(
        MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Invert),
        &mut ed,
    )
    .unwrap();
    let inverted = pixels::read_layer(ed.active().unwrap(), layer);
    // The right half is outside the selection: the invert left it alone.
    for y in 0..h {
        for x in 8..w {
            let i = ((y * w + x) * 4) as usize;
            assert_eq!(inverted[i..i + 4], original[i..i + 4]);
        }
    }
    let spec = FadeSpec {
        opacity: 1.0,
        mode: layer_model::BlendMode::Multiply,
    };
    confirm_through_host(&mut ed, MenuAction::Fade, |d| match d {
        GapDialog::Fade(f) => f.set_spec(spec),
        other => panic!("{other:?}"),
    })
    .unwrap();
    let faded = pixels::read_layer(ed.active().unwrap(), layer);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if x >= 8 {
                assert_eq!(
                    faded[i..i + 4],
                    original[i..i + 4],
                    "({x}, {y}) is outside the selection and must not be re-blended"
                );
            } else {
                for c in 0..3 {
                    let want = f32::from(original[i + c]) * f32::from(inverted[i + c]) / 255.0;
                    assert!(
                        (f32::from(faded[i + c]) - want).abs() <= 1.5,
                        "({x}, {y}) channel {c}: {} x {} faded to {}",
                        original[i + c],
                        inverted[i + c],
                        faded[i + c]
                    );
                }
            }
        }
    }
}

/// The live menu row names the step it fades ("Fade Apply Invert…"), from
/// the same menu context the menu bar draws with.
#[test]
fn the_fade_row_names_the_step_it_fades() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, _) = with_document(dir.path(), 8, 8, &gradient(8, 8));
    let live = context(&mut ed, &Workspace::new());
    assert_eq!(MenuAction::Fade.label_in(&live), MenuAction::Fade.label());
    perform(
        MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Invert),
        &mut ed,
    )
    .unwrap();
    let live = context(&mut ed, &Workspace::new());
    assert_eq!(MenuAction::Fade.label_in(&live), "Fade Apply Invert…");
}
