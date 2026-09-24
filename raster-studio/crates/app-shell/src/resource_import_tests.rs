//! W9-N: File > Open of resource files, SVGs and Export to TGA, driven the
//! way the user drives them — `Action::Open` with the picker answered, the
//! real chrome drawing frames — and read back where the user would see the
//! result (the Swatches panel, the options bar's ramp, the Layer Style
//! pattern list, the document's profile in an exported file).

use std::path::{Path, PathBuf};

use psd::bytes::Sink;
use psd::{Descriptor, Value};

use crate::action::Action;
use crate::chrome::{install_theme, Chrome};
use crate::dialogs::ScriptedDialogs;
use crate::editor::{Editor, Effect};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    );
    ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
    ed
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn png(dir: &Path) -> PathBuf {
    let rgba = vec![200u8; 8 * 8 * 4];
    write(
        dir,
        "doc.png",
        &raster::encode(raster::ExportFormat::Png, 8, 8, &rgba).unwrap(),
    )
}

/// Draw `n` frames of the real chrome.
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

fn aco(colors: &[(&str, [u16; 3])]) -> Vec<u8> {
    let mut s = Sink::new();
    s.u16(1);
    s.u16(colors.len() as u16);
    for (_, c) in colors {
        s.u16(0);
        for v in c {
            s.u16(*v);
        }
        s.u16(0);
    }
    s.u16(2);
    s.u16(colors.len() as u16);
    for (name, c) in colors {
        s.u16(0);
        for v in c {
            s.u16(*v);
        }
        s.u16(0);
        s.unicode_string(name);
    }
    s.into_inner()
}

fn grd_two_stop() -> Vec<u8> {
    grd(&[("Teal to orange", [0.0, 128.0, 128.0], [255.0, 128.0, 0.0])])
}

/// A version-5 `.grd` of two-stop gradients: `(name, from rgb, to rgb)`.
fn grd(gradients: &[(&str, [f64; 3], [f64; 3])]) -> Vec<u8> {
    let obj = |class: &str, items: Vec<(&str, Value)>| {
        let mut d = Descriptor::new(class);
        for (k, v) in items {
            d.push(k, v).unwrap();
        }
        Value::Descriptor(d)
    };
    let stop = |r: f64, g: f64, b: f64, at: i32| {
        obj(
            "Clrt",
            vec![
                (
                    "Clr ",
                    obj(
                        "RGBC",
                        vec![
                            ("Rd  ", Value::Double(r)),
                            ("Grn ", Value::Double(g)),
                            ("Bl  ", Value::Double(b)),
                        ],
                    ),
                ),
                ("Lctn", Value::Integer(at)),
                ("Mdpn", Value::Integer(50)),
            ],
        )
    };
    let list = gradients
        .iter()
        .map(|(name, a, b)| {
            obj(
                "Grdn",
                vec![
                    ("Nm  ", Value::Text((*name).into())),
                    ("Intr", Value::Double(4096.0)),
                    (
                        "Clrs",
                        Value::List(vec![
                            stop(a[0], a[1], a[2], 0),
                            stop(b[0], b[1], b[2], 4096),
                        ]),
                    ),
                    ("Trns", Value::List(vec![])),
                ],
            )
        })
        .collect();
    let mut root = Descriptor::new("null");
    root.push("GrdL", Value::List(list)).unwrap();
    let mut s = Sink::new();
    s.tag(b"8BGR");
    s.u16(5);
    s.u32(16);
    root.write(&mut s).unwrap();
    s.into_inner()
}

fn pat(name: &str) -> Vec<u8> {
    let pattern = psd::pattern::PsdPattern {
        name: name.to_string(),
        id: "id".into(),
        width: 2,
        height: 1,
        rgba8: vec![255, 0, 0, 255, 0, 0, 255, 255],
    };
    let mut out = b"8BPT".to_vec();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&psd::pattern::encode_block(&[pattern]));
    out
}

fn csh_triangle(name: &str) -> Vec<u8> {
    let mut s = Sink::new();
    s.tag(b"cush");
    s.u32(2);
    s.u32(1);
    s.unicode_string(name);
    s.align_to(4);
    s.u32(1);
    let body = s.begin_len();
    s.pascal_string("tri", 1);
    for v in [0u32, 0, 1, 1] {
        s.u32(v);
    }
    let fx = |v: f64| (v * f64::from(1u32 << 24)) as i32;
    s.u16(0);
    s.u16(3);
    s.zeros(22);
    for [x, y] in [[0.5, 0.0], [1.0, 1.0], [0.0, 1.0]] {
        s.u16(2);
        for _ in 0..3 {
            s.i32(fx(y));
            s.i32(fx(x));
        }
    }
    s.end_len(body);
    s.into_inner()
}

fn icc_rgb(description: &str) -> Vec<u8> {
    let mut desc = b"desc\0\0\0\0".to_vec();
    desc.extend_from_slice(&(description.len() as u32 + 1).to_be_bytes());
    desc.extend_from_slice(description.as_bytes());
    desc.push(0);
    let offset = 128 + 4 + 12;
    let mut p = vec![0u8; 128];
    p[0..4].copy_from_slice(&((offset + desc.len()) as u32).to_be_bytes());
    p[12..16].copy_from_slice(b"mntr");
    p[16..20].copy_from_slice(b"RGB ");
    p[36..40].copy_from_slice(b"acsp");
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(b"desc");
    p.extend_from_slice(&(offset as u32).to_be_bytes());
    p.extend_from_slice(&(desc.len() as u32).to_be_bytes());
    p.extend_from_slice(&desc);
    p
}

/// File > Open of an `.aco` puts its colours in the Swatches panel the
/// chrome draws, and the next frame writes them to the preferences file.
#[test]
fn file_open_of_swatches_lands_in_the_swatches_panel_and_the_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let teal = [0.0, 0.5, 0.5, 1.0];
    let file = write(
        dir.path(),
        "brand.aco",
        &aco(&[("Brand teal", [0, 32768, 32768])]),
    );
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    frames(&mut chrome, &ctx, &mut ed, 2);
    assert!(chrome.workspace().swatches.index_of(teal).is_none());

    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Panels));
    assert!(
        ed.status().unwrap_or_default().contains("1 swatch"),
        "{:?}",
        ed.status()
    );
    frames(&mut chrome, &ctx, &mut ed, 2);
    let swatches = &chrome.workspace().swatches;
    let at = swatches
        .index_of(teal)
        .expect("the swatch reached the panel");
    assert_eq!(swatches.swatches()[at].name, "Brand teal");
    let saved = Preferences::load(&AppPaths::rooted(dir.path().join("config")).preferences_file());
    assert!(
        saved
            .swatches
            .as_ref()
            .is_some_and(|s| s.iter().any(|w| w.name == "Brand teal")),
        "the imported swatch was not written to the preferences file"
    );
}

/// File > Open of a `.grd`: the gradient is kept in the presets and becomes
/// the ramp the gradient tool paints (the editor's copy) and shows (the
/// options bar's copy, handed over on the next frame).
#[test]
fn file_open_of_gradients_sets_the_gradient_tools_ramp() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "ramps.grd", &grd_two_stop());
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    frames(&mut chrome, &ctx, &mut ed, 1);

    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Tool));
    assert_eq!(ed.presets().gradients().len(), 1);
    assert_eq!(ed.presets().gradients()[0].name, "Teal to orange");
    let ramp = ed.gradient_ramp().clone();
    assert_eq!(ramp.stops.len(), 2);
    assert_eq!(ramp.stops[1].color, [1.0, 128.0 / 255.0, 0.0, 1.0]);
    frames(&mut chrome, &ctx, &mut ed, 1);
    let shown = chrome.workspace().options.gradient(tools::ToolId::Gradient);
    assert_eq!(shown.stops.len(), 2, "{shown:?}");
    assert!(
        (shown.stops[1].color[1] - 128.0 / 255.0).abs() < 1e-6,
        "{shown:?}"
    );
    // Persisted with the presets.
    let reloaded = asset_store::presets::PresetStore::load(
        &AppPaths::rooted(dir.path().join("config")).presets_file(),
    );
    assert_eq!(reloaded.gradients().len(), 1);
}

/// File > Open of a `.pat`: the pattern joins the presets, becomes the
/// active pattern, and is offered by the Layer Style dialog's pattern list.
#[test]
fn file_open_of_patterns_reaches_the_pattern_presets() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "tiles.pat", &pat("Red blue"));
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Tool));
    assert_eq!(
        ed.active_pattern().map(|p| p.name.as_str()),
        Some("Red blue")
    );
    let tiles = crate::doc::pattern_tiles(ed.presets());
    assert!(
        tiles.iter().any(|t| t.name() == "Red blue"),
        "not offered to Layer Style"
    );
    assert_eq!(
        ed.active_pattern().unwrap().rgba8,
        [255, 0, 0, 255, 0, 0, 255, 255]
    );
}

/// File > Open of a `.csh` keeps the shapes, and says where they went.
#[test]
fn file_open_of_custom_shapes_keeps_them_in_the_presets() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "shapes.csh", &csh_triangle("Triangle"));
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Tool));
    let shapes = ed.presets().shapes();
    assert_eq!(shapes.len(), 1);
    assert_eq!(shapes[0].name, "Triangle");
    assert_eq!(shapes[0].unit_svg_path().matches('C').count(), 3);
    assert!(ed.status().unwrap_or_default().contains(super::SHAPES_NOTE));
}

/// File > Open of an `.icc` assigns it to the active document, and the
/// exported PNG carries that profile.
#[test]
fn file_open_of_a_profile_assigns_it_and_the_export_carries_it() {
    let dir = tempfile::tempdir().unwrap();
    let profile = icc_rgb("Test RGB");
    let file = write(dir.path(), "test.icc", &profile);
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file.clone()));
    // No document: refused, and nothing is consumed silently.
    assert!(ed.dispatch(Action::Open).is_err());

    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    ed.open_path(&png(dir.path())).unwrap();
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentEdited));
    let doc = ed.active_mut().unwrap();
    assert!(doc.is_dirty());
    match &doc.document.meta.color_space {
        color::ColorSpace::IccProfile { profile: p, .. } => assert_eq!(p, &profile),
        other => panic!("not assigned: {other:?}"),
    }
    let out = dir.path().join("out.png");
    doc.export_to(&out).unwrap();
    let back = raster::decode_surface_path(&out, raster::ImportLimits::default()).unwrap();
    assert_eq!(back.icc_profile.as_deref(), Some(profile.as_slice()));
}

/// Damaged or unsupported resource files are errors — no panic, nothing
/// added — and `.atn` says why it is not imported.
#[test]
fn a_damaged_resource_or_an_action_file_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let broken = write(dir.path(), "broken.aco", &[0, 1, 0, 200, 0, 0]);
    let atn = write(dir.path(), "steps.atn", b"\0\0\0\x10");
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new().opening(broken).opening(atn),
    );
    assert!(ed.dispatch(Action::Open).is_err());
    assert!(ed.panel_imports.swatches.is_empty());
    let err = ed.dispatch(Action::Open).unwrap_err().to_string();
    assert!(err.contains(".atn"), "{err}");
}

/// File > Open of an `.svg` goes through the import worker and opens as a
/// document at the SVG's own size, its shapes rasterised.
#[test]
fn file_open_of_an_svg_opens_a_rasterised_document() {
    let dir = tempfile::tempdir().unwrap();
    let svg = write(
        dir.path(),
        "logo.svg",
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12"><rect width="12" height="12" fill="#00ff00"/></svg>"##,
    );
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(svg));
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::DocumentSet));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while ed.imports_pending() && std::time::Instant::now() < deadline {
        ed.poll_imports();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let doc = ed.active_mut().expect("the SVG opened");
    assert_eq!((doc.document.width(), doc.document.height()), (24, 12));
    let rect = doc.canvas_rect();
    let rgba = doc.composite(rect).unwrap();
    assert_eq!(&rgba[0..4], &[0, 255, 0, 255], "the rect is drawn");
    let right = ((6 * 24 + 20) * 4) as usize;
    assert_eq!(rgba[right + 3], 0, "outside the rect is transparent");
}

/// Export to a `.tga` path writes a TGA the decoder reads back.
#[test]
fn export_to_a_tga_path_writes_a_tga() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&png(dir.path())).unwrap();
    let out = dir.path().join("out.tga");
    ed.active_mut().unwrap().export_to(&out).unwrap();
    let back = raster::decode_surface_path(&out, raster::ImportLimits::default()).unwrap();
    assert_eq!(back.source_format, raster::ImportFormat::Tga);
    assert_eq!((back.width, back.height), (8, 8));
    let px = back.pixels.into_rgba8();
    for (got, want) in px[0..4].iter().zip([200u8; 4]) {
        assert!(got.abs_diff(want) <= 1, "{:?}", &px[0..4]);
    }
}

/// One frame of the real chrome with `events`.
fn frame_with(
    chrome: &mut Chrome,
    ctx: &egui::Context,
    ed: &mut Editor,
    events: Vec<egui::Event>,
) -> crate::chrome::ChromeOutput {
    let mut out = crate::chrome::ChromeOutput::default();
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            out = chrome.ui(ctx, ed);
        },
    );
    out
}

/// W9-N round 3: every gradient of a `.grd`, not just the first, is
/// reachable - File > Open, then Gradient Editor from the options bar, the
/// imported chips are drawn in the preset strip, and clicking the THIRD one
/// and confirming makes it the gradient tools' ramp (options bar and editor).
#[test]
fn every_imported_gradient_is_a_chip_in_the_gradient_editor_and_a_click_uses_it() {
    let dir = tempfile::tempdir().unwrap();
    let names = ["W9N ramp one", "W9N ramp two", "W9N ramp three"];
    let file = write(
        dir.path(),
        "three.grd",
        &grd(&[
            (names[0], [255.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            (names[1], [0.0, 255.0, 0.0], [0.0, 0.0, 0.0]),
            (names[2], [0.0, 0.0, 255.0], [255.0, 255.0, 0.0]),
        ]),
    );
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    ed.open_path(&png(dir.path())).unwrap();
    ed.set_tool(tools::ToolId::Gradient);
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    frames(&mut chrome, &ctx, &mut ed, 1);
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Tool));
    assert!(
        ed.status()
            .unwrap_or_default()
            .contains(super::GRADIENTS_NOTE),
        "{:?}",
        ed.status()
    );
    frames(&mut chrome, &ctx, &mut ed, 1);
    // The first became the ramp on import; the third is what we pick.
    let before = chrome.workspace().options.gradient(tools::ToolId::Gradient);
    assert_eq!(before.stops[0].color, [1.0, 0.0, 0.0, 1.0]);

    chrome
        .workspace_for_test()
        .emit(ui::Intent::OpenGradientEditor);
    // Until the modal stops moving (it sizes itself over the first frames),
    // so the rectangle read back is where the next frame's pointer lands.
    frames(&mut chrome, &ctx, &mut ed, 8);
    let listed = ui::dialogs::gradient_editor::imported_gradients();
    for name in names {
        assert!(listed.iter().any(|(n, _)| n == name), "{name} not listed");
    }
    let third = listed
        .iter()
        .position(|(n, _)| n == names[2])
        .expect("the third gradient is in the preset strip's list");
    let chip = ctx
        .read_response(ui::dialogs::gradient_editor::imported_chip_id(third))
        .expect("the third imported gradient's chip was not drawn")
        .rect;
    let at = chip.center();
    let click = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    frame_with(
        &mut chrome,
        &ctx,
        &mut ed,
        vec![egui::Event::PointerMoved(at), click(true), click(false)],
    );
    let picked = chrome
        .dialogs_for_test()
        .active_gradient_editor_for_test()
        .gradient()
        .clone();
    assert_eq!(picked.stops[0].color, [0.0, 0.0, 1.0, 1.0], "{picked:?}");
    assert_eq!(picked.stops[1].color, [1.0, 1.0, 0.0, 1.0], "{picked:?}");

    let out = frame_with(
        &mut chrome,
        &ctx,
        &mut ed,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
            physical_key: None,
        }],
    );
    assert!(!out.dialog_open, "Enter did not close the editor");
    let shown = chrome.workspace().options.gradient(tools::ToolId::Gradient);
    assert_eq!(shown.stops[0].color, [0.0, 0.0, 1.0, 1.0], "{shown:?}");
    // What the shell hands the editor's stroke ramp (`set_gradient_ramp`).
    let ramp = out
        .set_gradient_ramp
        .expect("the stroke ramp was not read back");
    assert_eq!(ramp.stops[0].color, [0.0, 0.0, 1.0, 1.0]);
}

/// W9-N round 3: an imported `.csh` shape can be drawn - File > Open, the
/// Custom Shape tool's Shape list (the schema the options bar draws) names
/// it past the built-in library, picking it holds that index, and a canvas
/// drag with the options bar's settings makes a shape layer of ITS outline.
#[test]
fn an_imported_custom_shape_is_in_the_shape_list_and_a_drag_draws_it() {
    use ui::canvas::{PointerInput, PointerPhase};

    let dir = tempfile::tempdir().unwrap();
    let name = "W9N drawable triangle";
    let file = write(dir.path(), "tri.csh", &csh_triangle(name));
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(file));
    ed.open_path(&png(dir.path())).unwrap();
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    frames(&mut chrome, &ctx, &mut ed, 1);
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Tool));
    frames(&mut chrome, &ctx, &mut ed, 1);

    let tool = tools::ToolId::CustomShape;
    let info = tools::registry::info(tool).unwrap();
    let schema = ui::tool_options::shown_schema(&chrome.workspace().options, info);
    let spec = schema
        .iter()
        .find(|s| s.key == "preset")
        .expect("the Shape option");
    let tools::OptionKind::Choice { choices, .. } = spec.kind else {
        panic!("{spec:?}")
    };
    let index = choices
        .iter()
        .position(|c| *c == name)
        .unwrap_or_else(|| panic!("{name} is not in the Shape list: {choices:?}"));
    assert!(index >= vector::CUSTOM_SHAPE_NAMES.len());
    chrome
        .workspace_for_test()
        .options
        .set(tool, "preset", ui::OptionValue::Choice(index));
    assert_eq!(
        chrome.workspace().options.get(tool, "preset"),
        Some(ui::OptionValue::Choice(index)),
        "the pick was clamped away"
    );

    // The shell's route: the options bar's held values, as tool settings.
    let settings: Vec<(String, tools::ToolSetting)> = chrome
        .tool_options(tool)
        .into_iter()
        .map(|(k, v)| {
            let s = match v {
                ui::OptionValue::Float(v) => tools::ToolSetting::Float(v),
                ui::OptionValue::Int(v) => tools::ToolSetting::Int(v),
                ui::OptionValue::Bool(v) => tools::ToolSetting::Bool(v),
                ui::OptionValue::Choice(v) => tools::ToolSetting::Choice(v),
                ui::OptionValue::Color(v) => tools::ToolSetting::Color(v),
            };
            (k, s)
        })
        .collect();
    ed.set_tool(tool);
    let viewport = glam::Vec2::new(400.0, 300.0);
    {
        let doc = ed.active_mut().unwrap();
        doc.set_viewport(viewport);
        doc.camera.zoom = 1.0;
        doc.camera.center = glam::Vec2::new(4.0, 4.0);
    }
    let screen = |x: f32, y: f32| viewport * 0.5 + glam::Vec2::new(x - 4.0, y - 4.0);
    let mut pointer = crate::tool_input::ToolPointer::new();
    let layers_before = ed.active().unwrap().document.layers.len();
    for (phase, (x, y)) in [
        (PointerPhase::Down, (1.0, 1.0)),
        (PointerPhase::Move, (4.0, 4.0)),
        (PointerPhase::Move, (7.0, 7.0)),
        (PointerPhase::Up, (7.0, 7.0)),
    ] {
        let _ = pointer.handle(
            &mut ed,
            PointerInput::at(phase, screen(x, y)),
            false,
            &settings,
        );
    }
    let doc = ed.active().unwrap();
    assert_eq!(doc.document.layers.len(), layers_before + 1, "no layer");
    let shape = doc
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| doc.document.layers.get(id))
        .find_map(|layer| match &layer.kind {
            layer_model::LayerKind::Shape(s) => Some(s.clone()),
            _ => None,
        })
        .expect("a shape layer");
    // The imported triangle: three cubic segments.
    let drawn = vector::parse_svg(&shape.path_svg).unwrap();
    let (_, want) = tools::registry::custom_shape_at(index);
    assert_eq!(
        drawn.segments().len(),
        want.segments().len(),
        "{}",
        shape.path_svg
    );
    assert_eq!(shape.path_svg.matches('C').count(), 3, "{}", shape.path_svg);
    let last_builtin = vector::CUSTOM_SHAPE_NAMES.len() - 1;
    let (_, fallback) = tools::registry::custom_shape_at(last_builtin);
    assert_ne!(
        fallback.segments().len(),
        drawn.segments().len(),
        "the drawn outline is the built-in fallback, not the import"
    );
}

/// W9-N round 3: shapes and gradients imported in an earlier session (kept
/// in the presets file) are in the pickers again after a restart, from the
/// first chrome frame.
#[test]
fn presets_kept_from_an_earlier_session_are_in_the_pickers_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::rooted(dir.path().join("config"));
    let mut store = asset_store::presets::PresetStore::default();
    let shape = match asset_store::resources::parse(
        asset_store::resources::ResourceKind::Shapes,
        &csh_triangle("W9N kept triangle"),
    )
    .unwrap()
    {
        asset_store::resources::Resource::Shapes(loaded) => loaded.items[0].clone(),
        other => panic!("{other:?}"),
    };
    store.define_shape(shape);
    let gradient = match asset_store::resources::parse(
        asset_store::resources::ResourceKind::Gradients,
        &grd(&[("W9N kept ramp", [9.0, 9.0, 9.0], [99.0, 99.0, 99.0])]),
    )
    .unwrap()
    {
        asset_store::resources::Resource::Gradients(loaded) => loaded.items[0].clone(),
        other => panic!("{other:?}"),
    };
    store.define_gradient(gradient);
    store.save(&paths.presets_file()).unwrap();

    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    assert!(!tools::registry::custom_shape_choices().contains(&"W9N kept triangle"));
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    frames(&mut chrome, &ctx, &mut ed, 1);
    assert!(tools::registry::custom_shape_choices().contains(&"W9N kept triangle"));
    assert!(ui::dialogs::gradient_editor::imported_gradients()
        .iter()
        .any(|(n, _)| n == "W9N kept ramp"));
}
