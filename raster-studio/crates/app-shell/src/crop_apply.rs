//! W4-D: performing a crop — the Crop tool's [`CropRequest`] made into the
//! one undoable command that does all of it.
//!
//! A crop is a new canvas plus a map from it back to the old one. The map is
//! the kept rectangle's own offset for a plain crop; with a straighten angle
//! it also rotates about the rectangle's centre (the quad
//! [`CropRequest::straightened_corners`] names), and with the W x H x
//! Resolution preset it scales the rectangle onto exactly that many pixels —
//! uniformly, from the largest W:H region centred in the rectangle, so the
//! picture is never stretched.
//! Every root layer is pre-multiplied by the inverse of that map
//! ([`Command::TransformLayer`] acts in document space, and a group carries
//! its subtree), and the canvas becomes the new size
//! ([`Command::SetCanvasSize`]). Nothing is resampled, so nothing is lost:
//! the pixels outside the new canvas stay in their layers, and Image ▸ Reveal
//! All brings them back.
//!
//! **Delete Cropped Pixels** is the one destructive half: every raster
//! layer's pixels that land outside the new canvas are cleared with a
//! [`Command::PaintTiles`] per layer, tested pixel by pixel through the
//! layer's full transform chain, so a rotated or scaled crop clears exactly
//! what falls outside.
//!
//! All of it is one [`Command::Transaction`], so a crop is one Ctrl+Z.
//!
//! Limits, named rather than hidden: the straighten and the scale are carried
//! by the layer transforms, not baked into the pixels; Delete Cropped Pixels
//! clears raster layers' own pixels only (layer masks, text and shape layers
//! keep their content); and the Resolution field converts inches to pixels
//! but is not stored on the document, which has no resolution field.

use glam::{Affine2, UVec2, Vec2};

use editor_core::{Command, Document, PixelKey, PixelTarget, TileEdit};
use raster::TILE_SIZE;
use tools::CropRequest;

use crate::doc::OpenDocument;

/// The geometry of one crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CropPlan {
    /// The new canvas, in pixels.
    pub size: UVec2,
    /// Old-canvas point to new-canvas point: what every root layer is
    /// pre-multiplied by.
    pub to_new: Affine2,
}

/// The plan for `req`, or `None` when it describes no canvas at all.
pub(crate) fn plan(req: &CropRequest) -> Option<CropPlan> {
    let rect = req.rect;
    if rect.width == 0 || rect.height == 0 {
        return None;
    }
    let (w, h) = req
        .output_size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((rect.width, rect.height));
    let angle = if req.straighten.is_finite() {
        req.straighten
    } else {
        0.0
    };
    let origin = Vec2::new(rect.x as f32, rect.y as f32);
    let to_new = if angle == 0.0 && (w, h) == (rect.width, rect.height) {
        // A plain crop is a pure translation, built exactly rather than
        // through an inverse, so the undo it records is exact.
        Affine2::from_translation(-origin)
    } else {
        let centre = origin + Vec2::new(rect.width as f32, rect.height as f32) * 0.5;
        // Round 2: one scale for both axes, so the W x H x Resolution preset
        // never stretches the picture. The source is the largest W:H region
        // centred in the kept box (the whole box when its ratio already is
        // W:H, which the Crop tool's ratio lock makes it to within a pixel).
        let (rw, rh) = (rect.width as f32, rect.height as f32);
        let s = (rw / w as f32).min(rh / h as f32);
        let scale = Vec2::splat(s);
        let to_old = Affine2::from_translation(centre)
            * Affine2::from_angle(angle)
            * Affine2::from_scale(scale)
            * Affine2::from_translation(-Vec2::new(w as f32, h as f32) * 0.5);
        let to_new = to_old.inverse();
        if !to_new.is_finite() {
            return None;
        }
        to_new
    };
    Some(CropPlan {
        size: UVec2::new(w, h),
        to_new,
    })
}

/// The non-destructive half: the canvas resize and one transform per root
/// layer (a group's transform already carries its subtree, so moving the
/// children too would move them twice).
pub(crate) fn geometry_commands(document: &Document, plan: &CropPlan) -> Vec<Command> {
    let mut commands = vec![Command::SetCanvasSize { size: plan.size }];
    if plan.to_new != Affine2::IDENTITY {
        let matrix = plan.to_new.to_cols_array();
        for id in document.layers.root() {
            commands.push(Command::TransformLayer {
                layer_id: *id,
                matrix,
            });
        }
    }
    commands
}

/// A layer's full transform: layer space to (old) document space, through
/// every ancestor group.
fn world_transform(document: &Document, id: layer_model::LayerId) -> Affine2 {
    let mut t = document
        .layers
        .get(id)
        .map(|l| l.transform)
        .unwrap_or(Affine2::IDENTITY);
    let mut parent = document.layers.parent_of(id);
    while let Some(p) = parent {
        if let Some(group) = document.layers.get(p) {
            t = group.transform * t;
        }
        parent = document.layers.parent_of(p);
    }
    t
}

/// W5-F: where one stored tile's pixel centres land against the new canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileFate {
    /// Every centre is inside: the tile is kept untouched.
    Kept,
    /// Every centre is outside: the tile is cleared whole.
    Cleared,
    /// The canvas edge crosses the tile: only this case tests each pixel.
    Crossing,
}

/// How far (in new-canvas pixels) a tile's corner centres must clear the
/// canvas edge before the whole tile is classified without per-pixel tests.
/// An affine map sends the tile's grid of centres into the parallelogram its
/// four corner centres span, so a margin this size absorbs the rounding of
/// `transform_point2` and keeps the classification identical to testing
/// every pixel.
const FATE_MARGIN: f32 = 1.0 / 64.0;

/// Classify the tile whose top-left pixel is `(ox, oy)` by its four corner
/// pixel centres under `to_new`. The canvas `[0, w) x [0, h)` is convex, so
/// four corners inside mean every centre is; four corners past the same edge
/// mean none is. Anything else — including a corner within [`FATE_MARGIN`]
/// of an edge — is [`TileFate::Crossing`].
fn tile_fate(to_new: Affine2, ox: i64, oy: i64, ts: usize, w: f32, h: f32) -> TileFate {
    let last = (ts - 1) as f32 + 0.5;
    let corners = [(0.5, 0.5), (last, 0.5), (0.5, last), (last, last)]
        .map(|(dx, dy)| to_new.transform_point2(Vec2::new(ox as f32 + dx, oy as f32 + dy)));
    if !corners.iter().all(|p| p.is_finite()) {
        return TileFate::Crossing;
    }
    let m = FATE_MARGIN;
    if corners
        .iter()
        .all(|p| p.x >= m && p.y >= m && p.x < w - m && p.y < h - m)
    {
        return TileFate::Kept;
    }
    let outside = [
        corners.iter().all(|p| p.x < -m),
        corners.iter().all(|p| p.y < -m),
        corners.iter().all(|p| p.x >= w + m),
        corners.iter().all(|p| p.y >= h + m),
    ];
    if outside.iter().any(|b| *b) {
        TileFate::Cleared
    } else {
        TileFate::Crossing
    }
}

thread_local! {
    /// W5-F: tiles [`delete_outside`] tested pixel by pixel on this thread —
    /// the counter its classification gate is tested by.
    static PIXEL_TESTED_TILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The destructive half (Delete Cropped Pixels): clear every raster layer's
/// pixels whose centre lands outside the new canvas.
///
/// Tile maps are in layer space, so the edits are independent of the
/// transforms [`geometry_commands`] adds in the same transaction.
///
/// W5-F: each tile is first classified by its corners ([`tile_fate`]): a
/// tile wholly inside is left alone and one wholly outside is cleared
/// without reading its bytes, so only the tiles the canvas edge crosses are
/// tested pixel by pixel.
pub(crate) fn delete_outside(
    doc: &mut OpenDocument,
    plan: &CropPlan,
) -> Result<Vec<Command>, String> {
    delete_outside_with(doc, plan, true)
}

fn delete_outside_with(
    doc: &mut OpenDocument,
    plan: &CropPlan,
    classify: bool,
) -> Result<Vec<Command>, String> {
    let ts = TILE_SIZE as usize;
    let (w, h) = (plan.size.x as f32, plan.size.y as f32);
    let mut commands = Vec::new();
    for id in doc.document.layers.iter_depth_first() {
        let Some(layer) = doc.document.layers.get(id) else {
            continue;
        };
        if !matches!(layer.kind, layer_model::LayerKind::Raster(_)) {
            continue;
        }
        let Some(map) = doc.document.pixels.tiles(PixelKey::Layer(id)) else {
            continue;
        };
        let entries: Vec<_> = map.iter().collect();
        let to_new = plan.to_new * world_transform(&doc.document, id);
        let mut edits = Vec::new();
        for (coord, hash) in entries {
            if coord.level != 0 {
                continue;
            }
            let (ox, oy) = coord.pixel_origin();
            let fate = if classify {
                tile_fate(to_new, ox, oy, ts, w, h)
            } else {
                TileFate::Crossing
            };
            let rewritten = match fate {
                TileFate::Kept => None,
                TileFate::Cleared => Some(None),
                TileFate::Crossing => {
                    PIXEL_TESTED_TILES.with(|c| c.set(c.get() + 1));
                    let Some(bytes) = compositor::TileSource::tile(&doc.tiles, hash) else {
                        continue;
                    };
                    if bytes.is_empty() || bytes.len() % (ts * ts) != 0 {
                        continue;
                    }
                    let bpp = bytes.len() / (ts * ts);
                    let mut out: Option<Vec<u8>> = None;
                    let mut kept = 0usize;
                    for ty in 0..ts {
                        for tx in 0..ts {
                            let p = to_new.transform_point2(Vec2::new(
                                ox as f32 + tx as f32 + 0.5,
                                oy as f32 + ty as f32 + 0.5,
                            ));
                            if p.x >= 0.0 && p.y >= 0.0 && p.x < w && p.y < h {
                                kept += 1;
                                continue;
                            }
                            let i = (ty * ts + tx) * bpp;
                            if bytes[i..i + bpp].iter().any(|b| *b != 0) {
                                out.get_or_insert_with(|| bytes.to_vec())[i..i + bpp].fill(0);
                            }
                        }
                    }
                    if kept == 0 {
                        Some(None)
                    } else {
                        out.map(Some)
                    }
                }
            };
            match rewritten {
                Some(None) => edits.push(TileEdit::clear(coord)),
                Some(Some(bytes)) => {
                    let hash = doc.tiles.insert_bytes(bytes);
                    edits.push(TileEdit::set(coord, hash));
                }
                None => {}
            }
        }
        if !edits.is_empty() {
            commands.push(
                Command::paint_tiles(PixelTarget::Layer(id), edits).map_err(|e| e.to_string())?,
            );
        }
    }
    Ok(commands)
}

/// The whole crop `req` asks for, as one undoable transaction — or `None`
/// when it describes no canvas at all.
pub(crate) fn crop(doc: &mut OpenDocument, req: &CropRequest) -> Option<Result<Command, String>> {
    let plan = plan(req)?;
    let mut commands = geometry_commands(&doc.document, &plan);
    if req.delete_cropped {
        match delete_outside(doc, &plan) {
            Ok(more) => commands.extend(more),
            Err(e) => return Some(Err(e)),
        }
    }
    Some(Ok(Command::Transaction {
        label: "Crop".into(),
        commands,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut rgba = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                rgba.extend_from_slice(&paint(x, y));
            }
        }
        let png = dir.join("canvas.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap(),
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

    /// What the options bar holds for the Crop tool, through the shell's own
    /// boundary conversion — the settings a real press is seeded with.
    fn bar(options: &[(&str, ui::OptionValue)]) -> Vec<(String, ToolSetting)> {
        let mut chrome = crate::chrome::Chrome::new();
        for (key, value) in options {
            chrome.set_tool_option(ToolId::Crop, key, *value);
        }
        chrome
            .tool_options(ToolId::Crop)
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

    /// Press at `from`, drag, release at `to`, with the Crop tool and the
    /// given options-bar settings.
    fn drag(
        pointer: &mut ToolPointer,
        editor: &mut Editor,
        settings: &[(String, ToolSetting)],
        from: (f32, f32),
        to: (f32, f32),
    ) {
        editor.set_tool(ToolId::Crop);
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
            assert!(out.failed.is_none(), "the press refused: {:?}", out.failed);
        }
    }

    fn size(editor: &Editor) -> (u32, u32) {
        let doc = editor.active().unwrap();
        (doc.document.width(), doc.document.height())
    }

    fn composite(editor: &mut Editor) -> Vec<u8> {
        let (w, h) = size(editor);
        editor
            .active_mut()
            .unwrap()
            .composite(PixelRect::new(0, 0, w, h))
            .unwrap()
    }

    fn red_at(buf: &[u8], width: u32, x: u32, y: u32) -> u8 {
        buf[((y * width + x) * 4) as usize]
    }

    /// The 16:9 preset, chosen in the options bar, locks the dragged box to
    /// 16:9 — and the Free control on the same drag does not.
    #[test]
    fn a_16_9_preset_locks_the_dragged_box_to_16_9() {
        let sixteen_nine = tools::edit::CROP_RATIO_LABELS
            .iter()
            .position(|l| *l == "16:9")
            .unwrap();
        for (settings, want_locked) in [
            (
                bar(&[("ratio", ui::OptionValue::Choice(sixteen_nine))]),
                true,
            ),
            (bar(&[]), false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut editor = editor_with(dir.path(), |_, _| [255; 4]);
            let mut pointer = ToolPointer::new();
            drag(
                &mut pointer,
                &mut editor,
                &settings,
                (4.0, 4.0),
                (60.0, 20.0),
            );
            let outcome = pointer.commit(&mut editor);
            let rect = outcome.cropped_to.expect("the crop was not performed");
            let ratio = rect.width as f32 / rect.height as f32;
            if want_locked {
                assert!(
                    (ratio - 16.0 / 9.0).abs() < 0.06,
                    "the 16:9 preset let the box be {}x{}",
                    rect.width,
                    rect.height
                );
            } else {
                assert_eq!((rect.width, rect.height), (56, 16), "control: Free");
            }
            assert_eq!(size(&editor), (rect.width, rect.height));
        }
    }

    /// Round 2: the 16:9 lock holds when the drag runs past the canvas — to
    /// the far corner, past the bottom, past the right edge, and backwards
    /// past the top left. The box shrinks to fit, it does not lose its
    /// ratio (these drags came out 60x56 and 60x36 when the lock was applied
    /// before a per-axis clip).
    #[test]
    fn a_16_9_box_dragged_past_the_canvas_keeps_its_ratio() {
        let sixteen_nine = tools::edit::CROP_RATIO_LABELS
            .iter()
            .position(|l| *l == "16:9")
            .unwrap();
        let settings = bar(&[("ratio", ui::OptionValue::Choice(sixteen_nine))]);
        for (from, to) in [
            ((4.0, 4.0), (60.0, 60.0)),
            ((4.0, 4.0), (40.0, 40.0)),
            ((4.0, 4.0), (90.0, 30.0)),
            ((4.0, 40.0), (30.0, 90.0)),
            ((60.0, 60.0), (-20.0, 0.0)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut editor = editor_with(dir.path(), |_, _| [255; 4]);
            let mut pointer = ToolPointer::new();
            drag(&mut pointer, &mut editor, &settings, from, to);
            let outcome = pointer.commit(&mut editor);
            let rect = outcome.cropped_to.expect("the crop was not performed");
            let (w, h) = (rect.width as f32, rect.height as f32);
            assert!(
                (w - h * 16.0 / 9.0).abs() <= 1.0,
                "{from:?}->{to:?} gave a {}x{} box (ratio {})",
                rect.width,
                rect.height,
                w / h
            );
            assert!(
                rect.x >= 0 && rect.y >= 0 && rect.right() <= W as i64 && rect.bottom() <= H as i64,
                "{from:?}->{to:?}: {rect:?} is off the canvas"
            );
            assert!(h >= 8.0, "{from:?}->{to:?}: the box collapsed to {rect:?}");
            assert_eq!(size(&editor), (rect.width, rect.height));
        }
    }

    /// W x H x Resolution: 2 x 1 inches at 20 px/in is a 40x20 canvas,
    /// exactly, whatever box was dragged; the same preset in pixels is the
    /// pixel size typed.
    #[test]
    fn the_w_x_h_x_resolution_preset_produces_exactly_that_pixel_size() {
        let size_preset = tools::edit::CROP_RATIO_SIZE;
        for (units, width, height, res, want) in [
            (1usize, 2.0f32, 1.0f32, 20.0f32, (40u32, 20u32)),
            (0, 30.0, 10.0, 72.0, (30, 10)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut editor =
                editor_with(
                    dir.path(),
                    |x, _| if x < 32 { [0, 0, 0, 255] } else { [255; 4] },
                );
            let mut pointer = ToolPointer::new();
            let settings = bar(&[
                ("ratio", ui::OptionValue::Choice(size_preset)),
                ("units", ui::OptionValue::Choice(units)),
                ("width", ui::OptionValue::Float(width)),
                ("height", ui::OptionValue::Float(height)),
                ("resolution", ui::OptionValue::Float(res)),
            ]);
            drag(
                &mut pointer,
                &mut editor,
                &settings,
                (2.0, 2.0),
                (62.0, 50.0),
            );
            let steps = editor.active().unwrap().history_depth();
            let outcome = pointer.commit(&mut editor);
            assert!(outcome.failed.is_none(), "{outcome:?}");
            assert_eq!(size(&editor), want, "units {units}");
            assert_eq!(editor.active().unwrap().history_depth(), steps + 1);
            // Round 2: one scale for both axes — the picture is resized, not
            // stretched. Every root layer's transform has equal-length axes.
            let doc = &editor.active().unwrap().document;
            for id in doc.layers.root() {
                let t = doc.layers.get(*id).unwrap().transform;
                let (sx, sy) = (t.matrix2.x_axis.length(), t.matrix2.y_axis.length());
                assert!(
                    (sx - sy).abs() <= 1e-4 * sx.max(sy),
                    "units {units}: the crop stretched the picture ({sx} x {sy})"
                );
            }
            // The kept region was scaled onto the canvas, not cut to its top
            // left: the black left half and white right half both survive.
            let buf = composite(&mut editor);
            assert!(red_at(&buf, want.0, 1, want.1 / 2) < 60);
            assert!(red_at(&buf, want.0, want.0 - 2, want.1 / 2) > 200);
        }
    }

    /// The Overlay the options bar holds reaches the live crop box the shell
    /// paints: the tool publishes it with its geometry, and the painter's
    /// overlay (built exactly as `Chrome` builds it) carries that guide's
    /// lines — eight-by-eight Grid lines, not the default thirds.
    #[test]
    fn the_overlay_choice_reaches_the_painted_crop_box() {
        let grid = tools::edit::CROP_OVERLAY_LABELS
            .iter()
            .position(|l| *l == "Grid")
            .unwrap();
        for (settings, want_lines) in [
            (bar(&[("overlay", ui::OptionValue::Choice(grid))]), 14usize),
            (bar(&[]), 4),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut editor = editor_with(dir.path(), |_, _| [255; 4]);
            let mut pointer = ToolPointer::new();
            drag(
                &mut pointer,
                &mut editor,
                &settings,
                (8.0, 8.0),
                (40.0, 40.0),
            );
            let Some((_, tools::SessionGeometry::Crop { rect, guide, .. })) =
                pointer.live_geometry()
            else {
                panic!("the released crop box published no geometry");
            };
            let camera = ui::canvas::CanvasCamera {
                center: Vec2::new(32.0, 32.0),
                zoom: 1.0,
                ..ui::canvas::CanvasCamera::default()
            };
            let viewport =
                ui::canvas::Viewport::new(VIEWPORT, ui::canvas::PanelInsets::uniform(0.0), 1.0);
            let overlay = ui::canvas::crop::build(
                ui::canvas::DocRect::from_corners(rect[0], rect[1]),
                &camera,
                &viewport,
                ui::canvas::CropGuide::from(guide),
                8.0,
            );
            assert_eq!(overlay.guides.len(), want_lines, "{guide:?}");
        }
    }

    /// A 64x64 white canvas with a black line three pixels thick at +10
    /// degrees (clockwise, y down) through (8, 20).
    fn tilted_line(x: u32, y: u32) -> [u8; 4] {
        let slope = 10f32.to_radians().tan();
        let on =
            (8..=56).contains(&x) && ((y as f32) - (20.0 + slope * (x as f32 - 8.0))).abs() <= 1.2;
        if on {
            [0, 0, 0, 255]
        } else {
            [255; 4]
        }
    }

    /// The row the black line crosses column `x` at, or `None`.
    fn dark_row(buf: &[u8], width: u32, height: u32, x: u32) -> Option<f32> {
        let (mut sum, mut n) = (0.0f32, 0.0f32);
        for y in 0..height {
            // Opaque and dark: the corners a rotation uncovers are
            // transparent, not part of the mark.
            let alpha = buf[((y * width + x) * 4 + 3) as usize];
            if alpha > 200 && red_at(buf, width, x, y) < 128 {
                sum += y as f32;
                n += 1.0;
            }
        }
        (n > 0.0).then(|| sum / n)
    }

    /// How far the line's row wanders across the middle columns.
    fn spread(buf: &[u8], width: u32, height: u32) -> f32 {
        let rows: Vec<f32> = (12..52)
            .filter_map(|x| dark_row(buf, width, height, x))
            .collect();
        assert!(rows.len() > 36, "the line is not on the canvas: {rows:?}");
        let lo = rows.iter().cloned().fold(f32::MAX, f32::min);
        let hi = rows.iter().cloned().fold(f32::MIN, f32::max);
        hi - lo
    }

    /// Straighten: a line drawn along the tilted mark in Straighten mode
    /// rotates the content so the mark comes out level, in one undo step.
    #[test]
    fn a_straighten_line_along_a_10_degree_mark_levels_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with(dir.path(), tilted_line);
        let before = composite(&mut editor);
        let tilt = spread(&before, W, H);
        assert!(tilt > 5.0, "fixture: the mark is not tilted ({tilt})");

        let mut pointer = ToolPointer::new();
        let settings = bar(&[("straighten_line", ui::OptionValue::Bool(true))]);
        let slope = 10f32.to_radians().tan();
        drag(
            &mut pointer,
            &mut editor,
            &settings,
            (8.0, 20.0),
            (56.0, 20.0 + slope * 48.0),
        );
        assert!(
            pointer.has_pending_commit(),
            "the line set nothing to confirm"
        );
        let steps = editor.active().unwrap().history_depth();
        let outcome = pointer.commit(&mut editor);
        assert!(outcome.failed.is_none(), "{outcome:?}");
        assert_eq!(editor.active().unwrap().history_depth(), steps + 1);
        assert_eq!(size(&editor), (W, H), "no box: the whole canvas is kept");

        let after = composite(&mut editor);
        let level = spread(&after, W, H);
        assert!(
            level <= 1.5,
            "the mark still wanders {level} rows after straightening (was {tilt})"
        );

        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(composite(&mut editor), before, "one undo restores it all");
    }

    /// Delete Cropped Pixels off: what the crop cut away is still in the
    /// layer, and Image ▸ Reveal All brings it back. On: it is gone.
    #[test]
    fn delete_off_keeps_the_cut_pixels_for_reveal_all_and_delete_on_does_not() {
        let mark = |x: u32, y: u32| {
            if (x, y) == (5, 5) {
                [0, 0, 0, 255]
            } else {
                [255; 4]
            }
        };
        for delete in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut editor = editor_with(dir.path(), mark);
            let before = composite(&mut editor);
            let mut pointer = ToolPointer::new();
            let settings = bar(&[("delete_cropped", ui::OptionValue::Bool(delete))]);
            drag(
                &mut pointer,
                &mut editor,
                &settings,
                (20.0, 20.0),
                (50.0, 50.0),
            );
            let steps = editor.active().unwrap().history_depth();
            let outcome = pointer.commit(&mut editor);
            assert!(outcome.failed.is_none(), "{outcome:?}");
            assert_eq!(size(&editor), (30, 30));
            assert_eq!(
                editor.active().unwrap().history_depth(),
                steps + 1,
                "a crop is one undo entry (delete {delete})"
            );

            let revealed =
                crate::menu_bridge::perform(ui::menu::MenuAction::RevealAll, &mut editor);
            if delete {
                assert_eq!(
                    size(&editor),
                    (30, 30),
                    "deleted pixels were revealed: {revealed:?}"
                );
                // Undo the crop: the pixels come back with it.
                assert!(editor.active_mut().unwrap().undo().unwrap());
                assert_eq!(
                    composite(&mut editor),
                    before,
                    "undo restores the deleted pixels"
                );
            } else {
                assert_eq!(size(&editor), (W, H), "Reveal All: {revealed:?}");
                let buf = composite(&mut editor);
                assert!(
                    red_at(&buf, W, 5, 5) < 40,
                    "the cut-away mark did not come back"
                );
                assert_eq!(buf, before, "Reveal All restores the whole image");
            }
        }
    }

    /// An editor holding one `side` x `side` opaque document with a
    /// diagonal pattern, so every tile holds different, non-zero bytes.
    fn big_editor(dir: &std::path::Path, side: u32) -> Editor {
        let mut rgba = Vec::with_capacity((side * side * 4) as usize);
        for y in 0..side {
            for x in 0..side {
                rgba.extend_from_slice(&[(x % 251) as u8, (y % 241) as u8, 90, 255]);
            }
        }
        let png = dir.join("big.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, side, side, &rgba).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        editor
    }

    /// W5-F: Delete Cropped Pixels tests pixel by pixel only the tiles the
    /// new canvas edge crosses. A 1280x1280 canvas is 5x5 tiles; keeping
    /// 300..1000 on both axes leaves one tile wholly inside, 16 wholly
    /// outside, and 8 crossed by the edge — and the classified result is
    /// the same set of edits the all-pixel path makes, straight or rotated.
    #[test]
    fn delete_cropped_pixels_tests_only_the_tiles_the_edge_crosses() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = big_editor(dir.path(), 1280);
        let doc = editor.active_mut().unwrap();
        let tiles_of = |doc: &OpenDocument| {
            doc.document
                .layers
                .iter_depth_first()
                .into_iter()
                .filter_map(|id| doc.document.pixels.tiles(PixelKey::Layer(id)))
                .map(|m| m.len())
                .sum::<usize>()
        };
        assert_eq!(tiles_of(doc), 25, "fixture: one layer of 5x5 tiles");
        let straight = CropPlan {
            size: UVec2::new(700, 700),
            to_new: Affine2::from_translation(-Vec2::new(300.0, 300.0)),
        };
        let before = PIXEL_TESTED_TILES.with(|c| c.get());
        let fast = delete_outside(doc, &straight).unwrap();
        let tested = PIXEL_TESTED_TILES.with(|c| c.get()) - before;
        assert_eq!(tested, 8, "tiles tested pixel by pixel");
        let edits = |commands: &[Command]| -> Vec<String> {
            commands.iter().map(|c| format!("{c:?}")).collect()
        };
        let slow = delete_outside_with(doc, &straight, false).unwrap();
        assert_eq!(
            edits(&fast),
            edits(&slow),
            "classification changed the result"
        );
        // 16 cleared + 8 rewritten; the inside tile is untouched.
        let Command::PaintTiles { delta, .. } = &fast[0] else {
            panic!("not a paint: {fast:?}");
        };
        assert_eq!(delta.edits().len(), 24, "{delta:?}");

        // A straightened crop: the classification still agrees pixel for
        // pixel with testing everything.
        let rotated = plan(&CropRequest {
            rect: PixelRect::new(200, 180, 800, 760),
            straighten: 0.3,
            delete_cropped: true,
            output_size: None,
        })
        .unwrap();
        let before = PIXEL_TESTED_TILES.with(|c| c.get());
        let fast = delete_outside(doc, &rotated).unwrap();
        let tested = PIXEL_TESTED_TILES.with(|c| c.get()) - before;
        let slow = delete_outside_with(doc, &rotated, false).unwrap();
        assert_eq!(
            edits(&fast),
            edits(&slow),
            "rotated: classification changed the result"
        );
        assert!(tested < 25, "rotated: every tile was tested ({tested})");
    }
}
