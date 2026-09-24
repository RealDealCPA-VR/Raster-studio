//! W10-H: Image ▸ Mode ▸ 32 Bits/Channel through the real routes — the
//! menu bridge, the canvas pointer route a brush takes, and File ▸ Export.

use glam::Vec2;
use raster::{TileCoord, TILE_SIZE};
use ui::canvas::{PointerInput, PointerPhase};
use ui::menu::{ChannelDepth, FilterId, MenuAction};

use crate::action::Action;
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::import::BlankBackground;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::tool_input::ToolPointer;

use super::*;

const RGBA8: usize = raster::depth::RGBA8_TILE_BYTES;

fn editor(dir: &std::path::Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

fn tile_bytes(ed: &Editor) -> Vec<(TileCoord, Vec<u8>)> {
    let doc = ed.active().unwrap();
    let mut out = Vec::new();
    for id in doc.document.layers.iter_depth_first() {
        if let Some(map) = doc.document.layer_tiles(id) {
            for (c, h) in map.iter() {
                out.push((c, doc.tiles.tile(h).unwrap().to_vec()));
            }
        }
    }
    out
}

fn to_32(ed: &mut Editor) {
    crate::menu_bridge::perform(MenuAction::SetBitDepth(ChannelDepth::ThirtyTwo), ed).unwrap();
    assert_eq!(ed.active().unwrap().document.meta.bit_depth, 32);
}

/// Give the active layer's tile (0, 0) `f32` content: red is an HDR ramp
/// (`0.5 + x / 16`, above 1.0 from x = 8), green sits between 16-bit codes.
fn hdr_ramp(ed: &mut Editor) -> Vec<f32> {
    let doc = ed.active_mut().unwrap();
    let layer = doc.document.active_layer().unwrap();
    let ts = TILE_SIZE as usize;
    let mut s = vec![0f32; ts * ts * 4];
    for y in 0..ts {
        for x in 0..ts {
            let i = (y * ts + x) * 4;
            s[i..i + 4].copy_from_slice(&[0.5 + x as f32 / 16.0, 0.123_456_7, 0.25, 1.0]);
        }
    }
    let h = doc
        .tiles
        .insert_bytes(raster::depth32::rgbaf32_to_tile_bytes(&s));
    doc.apply(
        Command::paint_tiles(
            PixelTarget::Layer(layer),
            [TileEdit::set(TileCoord::new(0, 0, 0), h)],
        )
        .unwrap(),
    )
    .unwrap();
    s
}

#[test]
fn mode_32_bits_converts_every_tile_losslessly_in_one_undoable_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(
        300,
        64,
        "flat",
        BlankBackground::Solid {
            rgba8: [12, 34, 56, 200],
            depth: raster::BitDepth::Eight,
        },
    )
    .unwrap();
    let before = tile_bytes(&ed);
    assert!(before.iter().all(|(_, b)| b.len() == RGBA8));
    let steps = ed.active().unwrap().history_depth();
    to_32(&mut ed);
    assert_eq!(ed.active().unwrap().history_depth(), steps + 1, "one step");
    let float = tile_bytes(&ed);
    assert!(float.iter().all(|(_, b)| b.len() == F32_TILE_BYTES));
    let px = raster::depth32::tile_bytes_to_rgbaf32(&float[0].1);
    assert_eq!(
        px[..4],
        [12.0 / 255.0, 34.0 / 255.0, 56.0 / 255.0, 200.0 / 255.0]
    );
    // The same row is now the greyed, checked one.
    assert_eq!(
        crate::menu_bridge::perform(MenuAction::SetBitDepth(ChannelDepth::ThirtyTwo), &mut ed)
            .unwrap_err(),
        "The document is already at that depth"
    );
    // Undo returns the depth and the exact 8-bit tiles; redo the f32 ones.
    let doc = ed.active_mut().unwrap();
    assert!(doc.undo().unwrap());
    assert_eq!(doc.document.meta.bit_depth, 8);
    assert_eq!(tile_bytes(&ed), before, "undo is byte-exact");
    assert!(ed.active_mut().unwrap().redo().unwrap());
    assert_eq!(tile_bytes(&ed), float);
    // 32 -> 8 is the identity on content that came from 8 bits.
    crate::menu_bridge::perform(MenuAction::SetBitDepth(ChannelDepth::Eight), &mut ed).unwrap();
    assert_eq!(ed.active().unwrap().document.meta.bit_depth, 8);
    assert_eq!(tile_bytes(&ed), before, "8 -> 32 -> 8 is lossless");
}

#[test]
fn thirty_two_to_sixteen_clips_hdr_and_keeps_sixteen_bit_precision() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(64, 64, "hdr", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    hdr_ramp(&mut ed);
    crate::menu_bridge::perform(MenuAction::SetBitDepth(ChannelDepth::Sixteen), &mut ed).unwrap();
    assert_eq!(ed.active().unwrap().document.meta.bit_depth, 16);
    let t = raster::tile_bytes_to_rgba16(&tile_bytes(&ed)[0].1);
    // x = 0: red 0.5, green between codes; x = 20: red 1.75 clips to white.
    assert_eq!(t[..4], [32_768, 8_091, 16_384, 65_535]);
    assert_eq!(t[20 * 4], 65_535);
}

/// Filter > Blur on a 32-bit document runs in f32: the HDR red of the ramp
/// (up to 4.4) is still far above 1.0 after the blur, the tiles stay f32,
/// and the uniform green (0.1234567, between two 16-bit codes) is not
/// rounded to a 16-bit code.
#[test]
fn a_menu_filter_on_a_32_bit_document_runs_in_f32_and_keeps_hdr() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(64, 64, "hdr", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    let src = hdr_ramp(&mut ed);
    let steps = ed.active().unwrap().history_depth();
    crate::menu_bridge::perform(MenuAction::Filter(FilterId::Blur), &mut ed).unwrap();
    assert_eq!(ed.active().unwrap().history_depth(), steps + 1);
    let tiles = tile_bytes(&ed);
    assert!(tiles.iter().all(|(_, b)| b.len() == F32_TILE_BYTES));
    let out = raster::depth32::tile_bytes_to_rgbaf32(&tiles[0].1);
    let ts = TILE_SIZE as usize;
    let at = |x: usize, y: usize| &out[(y * ts + x) * 4..(y * ts + x) * 4 + 4];
    // Interior of the ramp, rows away from the canvas edge.
    let red = at(40, 30)[0];
    assert!(red > 2.5, "the blur clipped HDR: red {red}");
    assert_ne!(red, src[(30 * ts + 40) * 4], "the blur changed the pixel");
    // Below the HDR range the blurred ramp holds values between 16-bit codes.
    let green_off_grid = (0..ts)
        .filter(|x| {
            let v = at(*x, 30)[1] * 65_535.0;
            (v - v.round()).abs() > 0.01
        })
        .count();
    assert!(green_off_grid > 0, "the green plane went through 16 bits");
}

/// A brush drag through the pointer route on a 32-bit document: the tile it
/// crosses stays f32, every pixel the brush did not touch keeps its exact
/// f32 value (HDR included), and the painted ones are the foreground.
#[test]
fn a_brush_stroke_on_a_32_bit_document_keeps_the_untouched_f32_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(64, 64, "paint", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    let src = hdr_ramp(&mut ed);
    let viewport = Vec2::new(200.0, 200.0);
    {
        let doc = ed.active_mut().unwrap();
        doc.set_viewport(viewport);
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(32.0, 32.0);
    }
    let screen = |x: f32, y: f32| viewport * 0.5 + Vec2::new(x - 32.0, y - 32.0);
    ed.set_tool(tools::ToolId::Brush);
    ed.set_foreground([0.0, 0.0, 1.0, 1.0]);
    let mut pointer = ToolPointer::new();
    let steps = ed.active().unwrap().history_depth();
    for (i, x) in [10.0f32, 30.0, 50.0].iter().enumerate() {
        let phase = if i == 0 {
            PointerPhase::Down
        } else {
            PointerPhase::Move
        };
        pointer.handle(
            &mut ed,
            PointerInput::at(phase, screen(*x, 32.0)),
            false,
            &[],
        );
    }
    pointer.handle(
        &mut ed,
        PointerInput::at(PointerPhase::Up, screen(50.0, 32.0)),
        false,
        &[],
    );
    assert_eq!(
        ed.active().unwrap().history_depth(),
        steps + 1,
        "one stroke"
    );
    let tiles = tile_bytes(&ed);
    assert!(
        tiles.iter().all(|(_, b)| b.len() == F32_TILE_BYTES),
        "the stroke landed as f32 tiles: {:?}",
        tiles.iter().map(|(_, b)| b.len()).collect::<Vec<_>>()
    );
    let out = raster::depth32::tile_bytes_to_rgbaf32(&tiles[0].1);
    let ts = TILE_SIZE as usize;
    let px = |v: &[f32], x: usize, y: usize| v[(y * ts + x) * 4..(y * ts + x) * 4 + 4].to_vec();
    for (x, y) in [(40, 5), (60, 60), (20, 0)] {
        assert_eq!(px(&out, x, y), px(&src, x, y), "untouched ({x}, {y})");
    }
    assert!(px(&src, 40, 5)[0] > 1.0, "the untouched pixel is HDR");
    let painted = px(&out, 30, 32);
    assert!(
        painted[2] > 0.99 && painted[0] < 0.01,
        "the stroke painted: {painted:?}"
    );
}

/// The documented limit of the apply boundary (README, parity matrix): a
/// Brush stroke that paints the clipped colour over HDR — white at 100% over
/// a uniform (3.0, 3.0, 3.0, 1.0) layer — matches the old pixel clipped, so
/// the boundary cannot tell it from an untouched pixel and keeps the 3.0.
#[test]
fn painting_the_clipped_colour_over_hdr_leaves_the_hdr_value() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(64, 64, "paint", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    let ts = TILE_SIZE as usize;
    {
        let doc = ed.active_mut().unwrap();
        let layer = doc.document.active_layer().unwrap();
        let s = [3.0f32, 3.0, 3.0, 1.0].repeat(ts * ts);
        let h = doc
            .tiles
            .insert_bytes(raster::depth32::rgbaf32_to_tile_bytes(&s));
        doc.apply(
            Command::paint_tiles(
                PixelTarget::Layer(layer),
                [TileEdit::set(TileCoord::new(0, 0, 0), h)],
            )
            .unwrap(),
        )
        .unwrap();
    }
    let viewport = Vec2::new(200.0, 200.0);
    {
        let doc = ed.active_mut().unwrap();
        doc.set_viewport(viewport);
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(32.0, 32.0);
    }
    let screen = |x: f32, y: f32| viewport * 0.5 + Vec2::new(x - 32.0, y - 32.0);
    ed.set_tool(tools::ToolId::Brush);
    ed.set_foreground([1.0, 1.0, 1.0, 1.0]);
    let mut pointer = ToolPointer::new();
    let steps = ed.active().unwrap().history_depth();
    for (phase, x) in [
        (PointerPhase::Down, 10.0f32),
        (PointerPhase::Move, 30.0),
        (PointerPhase::Move, 50.0),
        (PointerPhase::Up, 50.0),
    ] {
        pointer.handle(
            &mut ed,
            PointerInput::at(phase, screen(x, 32.0)),
            false,
            &[],
        );
    }
    assert_eq!(
        ed.active().unwrap().history_depth(),
        steps + 1,
        "one stroke"
    );
    let out = tile0(&ed);
    let i = (32 * ts + 30) * 4;
    assert_eq!(
        out[i..i + 4],
        [3.0, 3.0, 3.0, 1.0],
        "painted centre (30, 32)"
    );
}

/// File > Export of a 32-bit document to `.tif`: a 32-bit float TIFF of the
/// linear composite with the HDR values intact.
#[test]
fn file_export_of_a_32_bit_document_writes_a_float_tiff_with_hdr() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("hdr.tif");
    let mut ed = editor(dir.path(), ScriptedDialogs::new().exporting_to(out.clone()));
    ed.new_document_with(64, 8, "hdr", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    hdr_ramp(&mut ed);
    ed.dispatch(Action::Export).unwrap();
    let (w, h, px) = raster::depth32::decode_tiff_rgbaf32(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!((w, h), (64, 8));
    let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
    // x = 56: encoded red 4.0 -> linear sRGB far above 1.
    let red = at(56, 3)[0];
    let want = color::srgb_to_linear(4.0);
    assert!(
        red > 10.0 && (red - want).abs() < 1e-3 * want,
        "{red} vs {want}"
    );
    // x = 0: encoded green 0.1234567, linear, at f32 precision.
    let green = at(0, 3)[1];
    assert!((green - color::srgb_to_linear(0.123_456_7)).abs() < 1e-6);
    // The same through OpenDocument::export_to.
    let again = dir.path().join("again.tiff");
    ed.active_mut().unwrap().export_to(&again).unwrap();
    let (_, _, px2) =
        raster::depth32::decode_tiff_rgbaf32(&std::fs::read(&again).unwrap()).unwrap();
    assert_eq!(px2, px);
    // An 8-bit export of the same document is still an 8-bit PNG, clipped.
    let png = dir.path().join("flat.png");
    ed.active_mut().unwrap().export_to(&png).unwrap();
    let flat = raster::decode_path(&png).unwrap();
    assert_eq!(flat.rgba8[56 * 4], 255);
}

/// File > Save of a 32-bit document to `.rstudio` and File > Open of it: the
/// depth and the exact f32 tiles (HDR included) come back.
#[test]
fn a_32_bit_document_saves_and_reopens_with_the_same_f32_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("hdr.rstudio");
    let mut ed = editor(dir.path(), ScriptedDialogs::new().saving_to(target.clone()));
    ed.new_document_with(64, 64, "hdr", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    hdr_ramp(&mut ed);
    let before = tile_bytes(&ed);
    assert!(before.iter().all(|(_, b)| b.len() == F32_TILE_BYTES));
    ed.dispatch(Action::Save).expect("a 32-bit document saves");
    assert!(!ed.active().unwrap().is_dirty(), "{:?}", ed.status());
    let mut again = editor(dir.path(), ScriptedDialogs::new());
    again.open_path(&target).unwrap();
    assert_eq!(again.active().unwrap().document.meta.bit_depth, 32);
    assert_eq!(tile_bytes(&again), before, "the same f32 tiles came back");
}

/// The layer's tile (0, 0) as `f32` samples.
fn tile0(ed: &Editor) -> Vec<f32> {
    let tiles = tile_bytes(ed);
    assert!(
        tiles.iter().all(|(_, b)| b.len() == F32_TILE_BYTES),
        "every tile is f32: {:?}",
        tiles.iter().map(|(_, b)| b.len()).collect::<Vec<_>>()
    );
    raster::depth32::tile_bytes_to_rgbaf32(&tiles[0].1)
}

/// Like [`hdr_ramp`], with blue an HDR ramp down the rows (`1.5 + y / 32`)
/// so a vertical flip moves values too.
fn hdr_grid(ed: &mut Editor) -> Vec<f32> {
    let doc = ed.active_mut().unwrap();
    let layer = doc.document.active_layer().unwrap();
    let ts = TILE_SIZE as usize;
    let mut s = vec![0f32; ts * ts * 4];
    for y in 0..ts {
        for x in 0..ts {
            let i = (y * ts + x) * 4;
            let blue = 1.5 + y as f32 / 32.0;
            s[i..i + 4].copy_from_slice(&[0.5 + x as f32 / 16.0, 0.123_456_7, blue, 1.0]);
        }
    }
    let h = doc
        .tiles
        .insert_bytes(raster::depth32::rgbaf32_to_tile_bytes(&s));
    doc.apply(
        Command::paint_tiles(
            PixelTarget::Layer(layer),
            [TileEdit::set(TileCoord::new(0, 0, 0), h)],
        )
        .unwrap(),
    )
    .unwrap();
    s
}

/// Image Rotation 180 and Flip Canvas, and Flip Layer, on a 32-bit document
/// move the f32 samples themselves: every canvas pixel is exactly the source
/// pixel the map sends there, HDR values included (the round-2 defect left
/// columns 8..55 unflipped after a Flip Canvas Horizontal).
#[test]
fn flips_and_rotations_on_a_32_bit_document_move_every_f32_value() {
    use ui::menu::{CanvasRotation as CR, TransformOp as T};
    type Map = fn(usize, usize) -> (usize, usize);
    let cases: [(MenuAction, Map); 4] = [
        (MenuAction::RotateCanvas(CR::FlipHorizontal), |x, y| {
            (63 - x, y)
        }),
        (MenuAction::RotateCanvas(CR::Deg180), |x, y| {
            (63 - x, 63 - y)
        }),
        (MenuAction::RotateCanvas(CR::FlipVertical), |x, y| {
            (x, 63 - y)
        }),
        (MenuAction::Transform(T::FlipHorizontal), |x, y| (63 - x, y)),
    ];
    for (action, map) in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path(), ScriptedDialogs::new());
        ed.new_document_with(64, 64, "hdr", BlankBackground::Transparent)
            .unwrap();
        to_32(&mut ed);
        let src = hdr_grid(&mut ed);
        crate::menu_bridge::perform(action, &mut ed).unwrap();
        let out = tile0(&ed);
        let ts = TILE_SIZE as usize;
        for y in 0..64 {
            for x in 0..64 {
                let (dx, dy) = map(x, y);
                let s = (y * ts + x) * 4;
                let d = (dy * ts + dx) * 4;
                assert_eq!(
                    out[d..d + 4],
                    src[s..s + 4],
                    "{action:?}: ({x}, {y}) -> ({dx}, {dy})"
                );
            }
        }
    }
}

/// The apply boundary itself, for an 8/16-bit edit that moved clipped
/// content (the Move tool, a crop, any menu edit still on that road): an
/// old HDR pixel whose clipped value matches the output is NOT kept outside
/// a paint stroke (it lands as the clipped output), while an in-range one
/// is; inside a paint stroke of an in-place tool the HDR value is kept.
#[test]
fn the_apply_boundary_keeps_clipped_hdr_only_under_an_in_place_stroke() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(64, 64, "hdr", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    let src = hdr_ramp(&mut ed);
    let ts = TILE_SIZE as usize;
    // The clipped 16-bit view of the layer, flipped: what an 8/16-bit route
    // produces for Flip Canvas Horizontal.
    let clipped =
        raster::depth32::rgbaf32_tile_to_rgba16(&raster::depth32::rgbaf32_to_tile_bytes(&src))
            .unwrap();
    let mut flipped = clipped.clone();
    for y in 0..64 {
        for x in 0..64 {
            let (s, d) = ((y * ts + x) * 4, (y * ts + 63 - x) * 4);
            flipped[d..d + 4].copy_from_slice(&clipped[s..s + 4]);
        }
    }
    let command = |ed: &mut Editor| {
        let doc = ed.active_mut().unwrap();
        let layer = doc.document.active_layer().unwrap();
        let h = doc
            .tiles
            .insert_bytes(raster::rgba16_to_tile_bytes(&flipped));
        Command::paint_tiles(
            PixelTarget::Layer(layer),
            [TileEdit::set(TileCoord::new(0, 0, 0), h)],
        )
        .unwrap()
    };
    let at = |v: &[f32], x: usize| v[(10 * ts + x) * 4];
    // Outside a stroke: x = 10 had 1.125 and receives x = 53's 3.8125, which
    // clipped to 1.0 on the 16-bit road. It is 1.0, not the stale 1.125.
    let c = command(&mut ed);
    ed.apply_command(c);
    let out = tile0(&ed);
    assert_eq!(at(&src, 10), 1.125);
    assert_eq!(at(&out, 10), 1.0, "a moved clipped pixel is not left stale");
    assert_eq!(at(&out, 53), 1.0);
    // x = 63 receives x = 0's in-range 0.5 (the code 32768 widened).
    assert!((at(&out, 63) - 0.5).abs() < 1.0 / 131_070.0);
    // Green is uniform 0.1234567 (between codes, in range): kept exactly.
    assert_eq!(out[(10 * ts + 20) * 4 + 1], 0.123_456_7);
    // Undo, and the same tile under a paint stroke of the Brush: the
    // matching HDR pixel is one the stroke left alone, so it is kept.
    assert!(ed.active_mut().unwrap().undo().unwrap());
    assert_eq!(tile0(&ed), src);
    let c = command(&mut ed);
    with_in_place_stroke(in_place_tool(tools::ToolId::Brush), || ed.apply_command(c));
    assert_eq!(at(&tile0(&ed), 10), 1.125);
    // The marker is restored afterwards, and a mover never sets it.
    assert!(!in_place_tool(tools::ToolId::Move));
    assert!(!in_place_tool(tools::ToolId::Patch));
    assert!(!in_place_tool(tools::ToolId::CloneStamp));
    assert!(!IN_PLACE_STROKE.with(std::cell::Cell::get));
}

/// Edit > Clear inside a selection on a 32-bit document: the cleared pixels
/// are transparent and every pixel outside keeps its exact f32 value.
/// Image > Image Size resamples in f32: the tiles stay f32 and the HDR red
/// survives the resample.
#[test]
fn clear_and_image_size_on_a_32_bit_document_stay_in_f32() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(64, 64, "hdr", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    let src = hdr_ramp(&mut ed);
    ed.apply_command(Command::SetSelection {
        selection: editor_core::Selection::Rect {
            min: glam::IVec2::ZERO,
            max: glam::IVec2::new(32, 64),
        },
    });
    crate::menu_bridge::perform(MenuAction::ClearPixels, &mut ed).unwrap();
    let out = tile0(&ed);
    let ts = TILE_SIZE as usize;
    for x in [0usize, 20, 31] {
        assert_eq!(out[(5 * ts + x) * 4 + 3], 0.0, "cleared ({x}, 5)");
    }
    for x in [32usize, 45, 63] {
        let i = (5 * ts + x) * 4;
        assert_eq!(out[i..i + 4], src[i..i + 4], "kept ({x}, 5)");
    }
    assert!(src[(5 * ts + 45) * 4] > 3.0);
    ed.apply_command(Command::SetSelection {
        selection: editor_core::Selection::None,
    });
    let doc = ed.active_mut().unwrap();
    let command = doc
        .resample_command(&ui::dialogs::ImageSizeSpec {
            width: 32,
            height: 32,
            resolution_ppi: 72.0,
            resample: Some(raster::ResampleFilter::Triangle),
        })
        .unwrap();
    ed.apply_command(command);
    assert_eq!(ed.active().unwrap().document.width(), 32);
    let small = tile0(&ed);
    let red = small[(5 * ts + 28) * 4];
    assert!(red > 3.0, "Image Size clipped the HDR red: {red}");
}

/// Edit > Fill (the menu route `MenuAction::FillDialog` takes) on a 32-bit
/// document fills at f32: the selected pixels take the foreground exactly
/// and every pixel outside the selection keeps its f32 value, HDR included.
#[test]
fn edit_fill_on_a_32_bit_document_keeps_the_unfilled_hdr_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(64, 64, "hdr", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    let src = hdr_ramp(&mut ed);
    ed.apply_command(Command::SetSelection {
        selection: editor_core::Selection::Rect {
            min: glam::IVec2::ZERO,
            max: glam::IVec2::new(32, 64),
        },
    });
    ed.set_foreground([0.0, 0.0, 1.0, 1.0]);
    crate::menu_bridge::perform(MenuAction::FillDialog, &mut ed).unwrap();
    let out = tile0(&ed);
    let ts = TILE_SIZE as usize;
    for x in [0usize, 20, 31] {
        let i = (5 * ts + x) * 4;
        assert_eq!(out[i..i + 4], [0.0, 0.0, 1.0, 1.0], "filled ({x}, 5)");
    }
    for x in [32usize, 45, 63] {
        let i = (5 * ts + x) * 4;
        assert_eq!(out[i..i + 4], src[i..i + 4], "kept ({x}, 5)");
    }
    assert_eq!(src[(5 * ts + 32) * 4], 2.5);
    assert!(ed.active_mut().unwrap().undo().unwrap());
    assert_eq!(tile0(&ed), src);
}

/// Fill the active layer's tile (0, 0) with one 16-bit colour.
fn fill_16(ed: &mut Editor) {
    let doc = ed.active_mut().unwrap();
    let layer = doc.document.active_layer().unwrap();
    let ts = TILE_SIZE as usize;
    let s = [12_345u16, 0, 0, 65_535].repeat(ts * ts);
    let h = doc.tiles.insert_bytes(raster::rgba16_to_tile_bytes(&s));
    doc.apply(
        Command::paint_tiles(
            PixelTarget::Layer(layer),
            [TileEdit::set(TileCoord::new(0, 0, 0), h)],
        )
        .unwrap(),
    )
    .unwrap();
}

/// Dragging a layer into another document respects both depths: a 32-bit
/// layer is refused by an 8-bit or a 16-bit document with the real depths
/// in the reason, and a 16-bit layer copied into a 32-bit document lands as
/// f32 tiles.
#[test]
fn copying_a_layer_across_documents_respects_the_32_bit_depth() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.new_document_with(64, 64, "eight", BlankBackground::Transparent)
        .unwrap();
    let eight = ed.active().unwrap().id();
    ed.new_document_with(64, 64, "sixteen", BlankBackground::Transparent)
        .unwrap();
    crate::menu_bridge::perform(MenuAction::SetBitDepth(ChannelDepth::Sixteen), &mut ed).unwrap();
    fill_16(&mut ed);
    let sixteen = ed.active().unwrap().id();
    let layer16 = ed.active().unwrap().document.active_layer().unwrap();
    ed.new_document_with(64, 64, "float", BlankBackground::Transparent)
        .unwrap();
    to_32(&mut ed);
    hdr_ramp(&mut ed);
    let float = ed.active().unwrap().id();
    let layer = ed.active().unwrap().document.active_layer().unwrap();
    assert_eq!(
        ed.duplicate_layer_into_document(layer, eight, None)
            .unwrap_err(),
        "A 32-bit layer cannot be copied into an 8-bit document"
    );
    assert_eq!(
        ed.duplicate_layer_into_document(layer, sixteen, None)
            .unwrap_err(),
        "A 32-bit layer cannot be copied into a 16-bit document"
    );
    // The 16-bit layer (with pixels) into the 32-bit document.
    let index = ed
        .documents()
        .iter()
        .position(|d| d.id() == sixteen)
        .unwrap();
    ed.activate(index).unwrap();
    ed.duplicate_layer_into_document(layer16, float, None)
        .unwrap();
    let doc = ed.active().unwrap();
    assert_eq!(doc.id(), float);
    let copy = doc.document.active_layer().unwrap();
    let map = doc.document.layer_tiles(copy).unwrap();
    let (_, hash) = map.iter().next().unwrap();
    let bytes = doc.tiles.tile(hash).unwrap();
    assert_eq!(bytes.len(), F32_TILE_BYTES, "the copy landed as f32");
    assert_eq!(
        raster::depth32::tile_bytes_to_rgbaf32(bytes)[..4],
        [12_345.0 / 65_535.0, 0.0, 0.0, 1.0]
    );
}
