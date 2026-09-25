//! W13-I: the Crop tool's Content-Aware fill and the Eyedropper's Sample
//! choice, driven through the real routes — the options bar's held values
//! through `Chrome::tool_options` and the shell's boundary conversion, a
//! pointer drag through `ToolPointer::handle`, Enter through
//! `ToolPointer::commit`, and the document's own history.

use glam::Vec2;

use raster::PixelRect;
use tools::{ToolId, ToolSetting};
use ui::canvas::{PointerInput, PointerPhase};

use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::tool_input::ToolPointer;

const W: u32 = 64;
const H: u32 = 64;
const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);

/// An editor holding one 64x64 document opened from a PNG whose pixels
/// `paint` decides, camera at 100% with the image centred.
fn editor_with(dir: &std::path::Path, paint: impl Fn(u32, u32) -> [u8; 4]) -> Editor {
    editor_with_dialogs(dir, paint, ScriptedDialogs::new())
}

/// [`editor_with`] over the given scripted file dialogs.
fn editor_with_dialogs(
    dir: &std::path::Path,
    paint: impl Fn(u32, u32) -> [u8; 4],
    dialogs: ScriptedDialogs,
) -> Editor {
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            rgba.extend_from_slice(&paint(x, y));
        }
    }
    let png = dir.join("w13i.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap(),
    )
    .unwrap();
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
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

/// What the options bar holds for `tool`, through the shell's own boundary
/// conversion — the settings a real press is seeded with.
fn bar(tool: ToolId, options: &[(&str, ui::OptionValue)]) -> Vec<(String, ToolSetting)> {
    let mut chrome = crate::chrome::Chrome::new();
    for (key, value) in options {
        chrome.set_tool_option(tool, key, *value);
    }
    chrome
        .tool_options(tool)
        .into_iter()
        .map(|(key, value)| {
            let setting = match value {
                ui::OptionValue::Float(v) => ToolSetting::Float(v),
                ui::OptionValue::Int(v) => ToolSetting::Int(v),
                ui::OptionValue::Bool(v) => ToolSetting::Bool(v),
                ui::OptionValue::Choice(v) => ToolSetting::Choice(v),
                ui::OptionValue::Color(v) => ToolSetting::Color(v),
            };
            (key, setting)
        })
        .collect()
}

/// Press at `from`, drag, release at `to`, with `tool` and the settings.
fn drag(
    pointer: &mut ToolPointer,
    editor: &mut Editor,
    tool: ToolId,
    settings: &[(String, ToolSetting)],
    from: (f32, f32),
    to: (f32, f32),
) {
    editor.set_tool(tool);
    let mid = ((from.0 + to.0) * 0.5, (from.1 + to.1) * 0.5);
    for (phase, (x, y)) in [
        (PointerPhase::Down, from),
        (PointerPhase::Move, mid),
        (PointerPhase::Move, to),
        (PointerPhase::Up, to),
    ] {
        let out = pointer.handle(
            editor,
            PointerInput::at(phase, screen(x, y)),
            false,
            settings,
        );
        assert!(
            out.failed.is_none(),
            "the gesture refused: {:?}",
            out.failed
        );
    }
}

fn composite(editor: &mut Editor) -> (u32, u32, Vec<u8>) {
    let doc = editor.active_mut().unwrap();
    let (w, h) = (doc.document.width(), doc.document.height());
    (w, h, doc.composite(PixelRect::new(0, 0, w, h)).unwrap())
}

fn px(buf: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// Red and blue rows, four pixels each: the fill has to invent more rows.
fn stripes(_x: u32, y: u32) -> [u8; 4] {
    if (y / 4).is_multiple_of(2) {
        [220, 30, 30, 255]
    } else {
        [30, 30, 220, 255]
    }
}

/// A crop box dragged 16 px past the right edge with Content-Aware ticked
/// grows the canvas, and the new columns are synthesised from the stripes
/// (opaque, red or blue, not transparent) — one Ctrl+Z takes the crop and
/// the fill back together. Unticked, the same drag is clipped to the canvas.
#[test]
fn a_content_aware_crop_past_the_canvas_fills_the_new_columns_in_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor_with(dir.path(), stripes);
    let mut pointer = ToolPointer::new();
    let on = bar(
        ToolId::Crop,
        &[(
            tools::edit::CROP_CONTENT_AWARE_KEY,
            ui::OptionValue::Bool(true),
        )],
    );
    drag(
        &mut pointer,
        &mut editor,
        ToolId::Crop,
        &on,
        (4.0, 4.0),
        (80.0, 60.0),
    );
    let before = editor.active().unwrap().history_depth();
    let outcome = pointer.commit(&mut editor);
    assert!(outcome.failed.is_none(), "{outcome:?}");
    let rect = outcome.cropped_to.expect("the crop was not performed");
    assert_eq!((rect.x, rect.width, rect.height), (4, 76, 56));
    assert_eq!(editor.active().unwrap().history_depth(), before + 1);
    let status = editor.status().unwrap_or_default().to_string();
    assert!(status.contains("filled"), "status: {status}");

    let (w, h, buf) = composite(&mut editor);
    assert_eq!((w, h), (76, 56));
    // New canvas column 70 is old column 74, past the old right edge.
    for y in 0..h {
        let p = px(&buf, w, 70, y);
        assert_eq!(p[3], 255, "row {y} of the new area is transparent: {p:?}");
        assert!(
            p[0] > 150 || p[2] > 150,
            "row {y} is not synthesised from the stripes: {p:?}"
        );
    }
    // The old pixels are where the crop put them.
    assert_eq!(px(&buf, w, 10, 0), stripes(14, 4), "old content moved");

    // One undo: the canvas and the pixels both come back.
    assert!(editor.active_mut().unwrap().undo().unwrap());
    let (w, h, buf) = composite(&mut editor);
    assert_eq!((w, h), (W, H));
    assert_eq!(px(&buf, w, 63, 0), stripes(63, 0));

    // Control: Content-Aware off clips the same drag to the canvas.
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor_with(dir.path(), stripes);
    let mut pointer = ToolPointer::new();
    let off = bar(ToolId::Crop, &[]);
    drag(
        &mut pointer,
        &mut editor,
        ToolId::Crop,
        &off,
        (4.0, 4.0),
        (80.0, 60.0),
    );
    let rect = pointer.commit(&mut editor).cropped_to.unwrap();
    assert_eq!((rect.width, rect.height), (60, 56));
}

/// Three layers: the opened red image at the bottom, an empty active layer
/// in the middle, a blue layer on top. The Eyedropper's Sample choice, set
/// in the options bar, decides which of them a click reads.
#[test]
fn the_eyedropper_sample_choice_reads_current_and_below_or_all_layers() {
    let pick = |sample: usize| -> [f32; 4] {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(dir.path(), |_, _| [255, 0, 0, 255]);
        let middle = {
            let layer = layer_model::Layer::raster("Middle");
            let id = layer.id;
            editor.apply_command(editor_core::Command::create_layer(layer));
            id
        };
        let top = layer_model::Layer::raster("Top");
        let top_id = top.id;
        editor.apply_command(editor_core::Command::create_layer(top));
        let blue: Vec<u8> = [0u8, 0, 255, 255].repeat((W * H) as usize);
        let paint = {
            let doc = editor.active_mut().unwrap();
            crate::menu_bridge::pixels::write_layer(doc, top_id, &blue, "Fixture").unwrap()
        };
        editor.apply_command(paint);
        editor.set_layer_selection(vec![middle], Some(middle));
        assert_eq!(
            editor.active().unwrap().document.active_layer(),
            Some(middle)
        );
        editor.set_foreground([0.0, 1.0, 0.0, 1.0]);
        let settings = bar(
            ToolId::Eyedropper,
            &[(
                tools::tool::SAMPLE_LAYERS_KEY,
                ui::OptionValue::Choice(sample),
            )],
        );
        let mut pointer = ToolPointer::new();
        drag(
            &mut pointer,
            &mut editor,
            ToolId::Eyedropper,
            &settings,
            (32.0, 32.0),
            (33.0, 32.0),
        );
        editor.foreground()
    };
    let below = pick(1);
    assert!(
        below[0] > 0.9 && below[2] < 0.1,
        "Current & Below reads the red under the empty layer: {below:?}"
    );
    // All Layers is the registry default: the bar holds no value for it, so
    // this is also the untouched Eyedropper.
    let all = pick(2);
    assert!(
        all[2] > 0.9 && all[0] < 0.1,
        "All Layers reads the blue on top: {all:?}"
    );
    let current = pick(0);
    assert!(
        current[3] < 0.1,
        "Current Layer reads the empty active layer: {current:?}"
    );
}

/// The active layer's own pixel at layer-space `(x, y)` — transparent when
/// its tile is absent.
fn layer_px(editor: &Editor, x: u32, y: u32) -> [u8; 4] {
    let doc = editor.active().unwrap();
    let layer = doc.document.active_layer().unwrap();
    let ts = raster::TILE_SIZE;
    let coord = raster::TileCoord::new((x / ts) as i32, (y / ts) as i32, 0);
    let Some(hash) = doc
        .document
        .layer_tiles(layer)
        .and_then(|tiles| tiles.get(coord))
    else {
        return [0; 4];
    };
    let stored = compositor::TileSource::tile(&doc.tiles, hash).unwrap();
    let bytes = raster::rgba8_view(stored);
    let i = (((y % ts) * ts + (x % ts)) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

/// Content-Aware together with Delete Cropped Pixels: the fill shares a
/// tile with pixels the crop deletes, and it must not bring them back.
/// Layer pixel (10, 0) lies above the new canvas (the crop starts at row
/// 4), so it is gone with or without the fill; the new columns are still
/// filled.
#[test]
fn a_content_aware_crop_with_delete_cropped_pixels_keeps_the_cropped_pixels_deleted() {
    let crop = |content_aware: bool| {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(dir.path(), stripes);
        assert_eq!(layer_px(&editor, 10, 0)[3], 255, "fixture");
        let mut pointer = ToolPointer::new();
        let settings = bar(
            ToolId::Crop,
            &[
                (
                    tools::edit::CROP_CONTENT_AWARE_KEY,
                    ui::OptionValue::Bool(content_aware),
                ),
                ("delete_cropped", ui::OptionValue::Bool(true)),
            ],
        );
        drag(
            &mut pointer,
            &mut editor,
            ToolId::Crop,
            &settings,
            (4.0, 4.0),
            (80.0, 60.0),
        );
        let outcome = pointer.commit(&mut editor);
        assert!(outcome.failed.is_none(), "{outcome:?}");
        editor
    };
    for content_aware in [false, true] {
        let editor = crop(content_aware);
        assert_eq!(
            layer_px(&editor, 10, 0)[3],
            0,
            "content_aware={content_aware}: a cropped-away pixel came back"
        );
        // A pixel inside the new canvas survives.
        assert_eq!(layer_px(&editor, 10, 8), stripes(10, 8));
    }
    // And the fill still ran: the new area (layer column 74) is opaque.
    let mut editor = crop(true);
    let (w, h, buf) = composite(&mut editor);
    assert_eq!((w, h), (76, 56));
    for y in 0..h {
        assert_eq!(px(&buf, w, 70, y)[3], 255, "row {y} of the new area");
    }
}

/// The Move options bar's Quick Export: its intent (the one the button
/// emits, pinned in `ui::view::toolbar`) routed through the chrome's own
/// road into the bridge, performed, and the PNG read back: the active layer
/// alone — the blue left half of the top layer, transparent elsewhere, the
/// red image under it hidden — at canvas size, where the dialog pointed.
#[test]
fn quick_export_writes_the_active_layer_alone_as_a_png() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("picked.png");
    let mut editor = editor_with_dialogs(
        dir.path(),
        |_, _| [255, 0, 0, 255],
        ScriptedDialogs::new().saving_to(&target),
    );
    let top = layer_model::Layer::raster("Top");
    let top_id = top.id;
    editor.apply_command(editor_core::Command::create_layer(top));
    let mut half = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W / 2 {
            let i = ((y * W + x) * 4) as usize;
            half[i..i + 4].copy_from_slice(&[0, 0, 255, 255]);
        }
    }
    let paint = {
        let doc = editor.active_mut().unwrap();
        crate::menu_bridge::pixels::write_layer(doc, top_id, &half, "Fixture").unwrap()
    };
    editor.apply_command(paint);
    editor.set_layer_selection(vec![top_id], Some(top_id));
    editor.set_tool(ToolId::Move);

    let mut chrome = crate::chrome::Chrome::new();
    let mut out = crate::chrome::ChromeOutput::default();
    chrome.menu_click(
        ui::Intent::Action(ui::menu::MenuAction::QuickExportLayer),
        &editor,
        &mut out,
    );
    assert_eq!(
        out.menu,
        vec![ui::menu::MenuAction::QuickExportLayer],
        "unrouted: {:?}",
        out.unrouted
    );
    let depth = editor.active().unwrap().history_depth();
    for action in &out.menu {
        crate::menu_bridge::perform(*action, &mut editor).unwrap();
    }
    assert_eq!(
        editor.active().unwrap().history_depth(),
        depth,
        "an export is not an edit"
    );
    let status = editor.status().unwrap_or_default().to_string();
    assert!(status.contains("Top"), "status: {status}");

    let png = raster::decode_path(&target).unwrap();
    assert_eq!((png.width, png.height), (W, H));
    assert_eq!(
        px(&png.rgba8, W, 5, 5),
        [0, 0, 255, 255],
        "the layer's blue"
    );
    assert_eq!(
        px(&png.rgba8, W, 50, 5)[3],
        0,
        "the red image under it is hidden: {:?}",
        px(&png.rgba8, W, 50, 5)
    );

    // Declined: no file, a loud refusal.
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor_with(dir.path(), |_, _| [255, 0, 0, 255]);
    let refused = crate::menu_bridge::perform(ui::menu::MenuAction::QuickExportLayer, &mut editor);
    assert!(refused.is_err(), "{refused:?}");
}

/// Content-Aware crop fills only an 8-bit document. In a 32-bit document
/// (Image ▸ Mode ▸ 32 Bits/Channel, through the menu) an HDR sample (red
/// 3.0) sits in the tile the fill would write; the crop still lands as one
/// step, the status line names the limit, the new columns stay transparent,
/// and the HDR sample is still 3.0 — a fill would have landed its tile
/// through the 8-bit view and clipped it to 1.0. A 16-bit document is
/// refused the same way.
#[test]
fn a_content_aware_crop_refuses_to_fill_a_16_or_32_bit_document() {
    use ui::menu::{ChannelDepth, MenuAction};
    for depth in [ChannelDepth::Sixteen, ChannelDepth::ThirtyTwo] {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(dir.path(), stripes);
        crate::menu_bridge::perform(MenuAction::SetBitDepth(depth), &mut editor).unwrap();
        let hdr = depth == ChannelDepth::ThirtyTwo;
        let ts = raster::TILE_SIZE as usize;
        let (hx, hy) = (40usize, 10usize);
        if hdr {
            assert_eq!(editor.active().unwrap().document.meta.bit_depth, 32);
            let doc = editor.active_mut().unwrap();
            let layer = doc.document.active_layer().unwrap();
            let coord = raster::TileCoord::new(0, 0, 0);
            let hash = doc.document.layer_tiles(layer).unwrap().get(coord).unwrap();
            let stored = compositor::TileSource::tile(&doc.tiles, hash).unwrap();
            let mut s = raster::depth32::tile_bytes_to_rgbaf32(stored);
            s[(hy * ts + hx) * 4] = 3.0;
            let h = doc
                .tiles
                .insert_bytes(raster::depth32::rgbaf32_to_tile_bytes(&s));
            doc.apply(
                editor_core::Command::paint_tiles(
                    editor_core::PixelTarget::Layer(layer),
                    [editor_core::TileEdit::set(coord, h)],
                )
                .unwrap(),
            )
            .unwrap();
        } else {
            assert_eq!(editor.active().unwrap().document.meta.bit_depth, 16);
        }
        let mut pointer = ToolPointer::new();
        let on = bar(
            ToolId::Crop,
            &[(
                tools::edit::CROP_CONTENT_AWARE_KEY,
                ui::OptionValue::Bool(true),
            )],
        );
        drag(
            &mut pointer,
            &mut editor,
            ToolId::Crop,
            &on,
            (4.0, 4.0),
            (80.0, 60.0),
        );
        let before = editor.active().unwrap().history_depth();
        let outcome = pointer.commit(&mut editor);
        assert!(outcome.failed.is_none(), "{depth:?}: {outcome:?}");
        let rect = outcome.cropped_to.expect("the crop was not performed");
        assert_eq!((rect.width, rect.height), (76, 56), "{depth:?}");
        assert_eq!(editor.active().unwrap().history_depth(), before + 1);
        let status = editor.status().unwrap_or_default().to_string();
        assert!(
            status.contains("8-bit documents only"),
            "{depth:?} status: {status}"
        );
        let (w, h, buf) = composite(&mut editor);
        for y in 0..h {
            assert_eq!(px(&buf, w, 70, y)[3], 0, "{depth:?}: row {y} was filled");
        }
        if hdr {
            let doc = editor.active().unwrap();
            let layer = doc.document.active_layer().unwrap();
            let hash = doc
                .document
                .layer_tiles(layer)
                .unwrap()
                .get(raster::TileCoord::new(0, 0, 0))
                .unwrap();
            let stored = compositor::TileSource::tile(&doc.tiles, hash).unwrap();
            let s = raster::depth32::tile_bytes_to_rgbaf32(stored);
            assert_eq!(s[(hy * ts + hx) * 4], 3.0, "the HDR sample was clipped");
        }
    }
}
