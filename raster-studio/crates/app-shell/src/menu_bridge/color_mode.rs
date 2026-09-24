//! W7-D: Image ▸ Mode ▸ RGB / Grayscale / Lab / CMYK / Indexed.
//!
//! Every conversion is ONE undo step:
//! [`editor_core::color_mode::convert_color_mode`] assembles the mode flag and
//! one `PaintTiles` per layer whose pixels the target mode constrains, and the
//! pixel mapping is here, where the tile store is:
//!
//! * **RGB / Lab** — the flag alone: 8-bit sRGB round-trips through Lab, so
//!   the pixels are already what the mode means (Photopea's approach: one RGB
//!   working buffer).
//! * **Grayscale** — each visible pixel collapses to its Rec.601 luma.
//! * **CMYK** — each visible pixel the documented ink model
//!   (`color::cmyk`, no ICC press profile) cannot print is replaced by its
//!   printable round trip; a printable one is left exactly as it is.
//! * **Indexed** — one palette over every layer's visible pixels
//!   (`color::quantize`, the dialog's source/count/dither), then every tile
//!   is mapped onto it. Diffusion runs per 256-px tile.
//!
//! * **Bitmap** (W10-H, from Grayscale only) — the visible composite over
//!   white becomes ONE opaque layer of pure black and white
//!   (`color::bitmap`: 50% threshold, pattern / diffusion dither or a
//!   halftone screen); every old layer goes, one undo step.
//! * **Duotone** (W10-H, from Grayscale only) — every visible grey is
//!   printed through one to four inks, each through its own curve
//!   (`color::duotone`), baked into the tiles; the layers stay.
//!
//! A 16-bit document converts at 8 bits and its apply boundary widens the
//! result back, the same road the Grayscale conversion took before W7-D.

use color::cmyk::{is_out_of_gamut, ProofLut};
use color::quantize::{self, Histogram};
use compositor::TileSource;
use editor_core::color_mode::{convert_color_mode, mode};
use raster::TILE_SIZE;

use crate::editor::Editor;

/// W10-H: the Bitmap and Duotone dialogs' confirmed answers (their defaults
/// when the pick came from anywhere else).
#[derive(Debug, Clone, Default)]
pub(crate) struct ModeOptions {
    pub bitmap: Option<color::bitmap::BitmapMethod>,
    pub duotone: Option<color::duotone::DuotoneSpec>,
}

/// Convert the active document into `target`. `indexed` is the Indexed Color
/// dialog's confirmed spec and `options` the Bitmap / Duotone dialogs' (the
/// defaults when the pick came from anywhere else).
pub(crate) fn set_color_mode_with(
    editor: &mut Editor,
    target: ui::menu::ColorMode,
    indexed: Option<ui::dialogs::IndexedSpec>,
    options: ModeOptions,
) -> Result<String, String> {
    let to = target as u8;
    let label = format!("Change Colour Mode to {}", target.label());
    let Some(doc) = editor.active_mut() else {
        return Err("No document is open".into());
    };
    if doc.document.meta.color_mode == to {
        return Err("The document is already in that colour mode".into());
    }
    let tile_bytes = (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4;

    // W10-H: Bitmap and Duotone are reached from Grayscale only.
    if !editor_core::color_mode::conversion_allowed(doc.document.meta.color_mode, to) {
        return Err(format!(
            "{} is reached from Grayscale: convert to Grayscale first",
            target.label().trim_end_matches('…')
        ));
    }
    if to == mode::BITMAP {
        return bitmap_flattened(editor, options.bitmap.unwrap_or_default(), label);
    }
    let duotone = (to == mode::DUOTONE).then(|| options.duotone.unwrap_or_default().lut());

    // W10-H: Indexed Color flattens a document of more than one layer, as
    // Photoshop does, so no blending between layers can composite a colour
    // outside the palette; the status line says so.
    if to == mode::INDEXED && doc.document.layers.iter_depth_first().len() > 1 {
        return indexed_flattened(editor, indexed.unwrap_or_default(), label);
    }

    // Indexed: the palette is the whole document's, built before any tile is
    // mapped so every layer shares it.
    let palette = if to == mode::INDEXED {
        let spec = indexed.unwrap_or_default();
        let mut histogram = Histogram::new();
        for layer_id in doc.document.layers.iter_depth_first() {
            let Some(map) = doc.document.layer_tiles(layer_id) else {
                continue;
            };
            for (coord, hash) in map.iter() {
                if coord.level != 0 {
                    continue;
                }
                if let Some(stored) = doc.tiles.tile(hash) {
                    histogram.add_rgba8(&raster::rgba8_view(stored));
                }
            }
        }
        let palette = quantize::build_palette(&histogram, spec.palette, spec.colors)
            .map_err(|e| format!("Cannot convert to Indexed Color: {e}"))?;
        Some((palette, spec.dither))
    } else {
        None
    };

    let proof = ProofLut::shared();
    let tiles = &mut doc.tiles;
    let command = convert_color_mode(&doc.document, to, label, |_, _, hash| {
        let stored = tiles.tile(hash)?;
        let mut bytes = raster::rgba8_view(stored).into_owned();
        if bytes.len() != tile_bytes {
            return None;
        }
        match to {
            mode::GRAYSCALE => {
                for px in bytes.as_chunks_mut::<4>().0 {
                    let luma = (0.299 * f32::from(px[0])
                        + 0.587 * f32::from(px[1])
                        + 0.114 * f32::from(px[2]))
                    .round()
                    .clamp(0.0, 255.0) as u8;
                    px[..3].fill(luma);
                }
            }
            mode::CMYK => {
                for px in bytes.as_chunks_mut::<4>().0 {
                    let rgb = [px[0], px[1], px[2]];
                    if px[3] > 0 && is_out_of_gamut(rgb) {
                        px[..3].copy_from_slice(&proof.proof(rgb));
                    }
                }
            }
            mode::INDEXED => {
                let (palette, dither) = palette.as_ref()?;
                quantize::remap_rgba8(&mut bytes, TILE_SIZE as usize, palette, *dither);
            }
            // W10-H: each grey printed through the inks.
            mode::DUOTONE => {
                let lut = duotone.as_ref()?;
                for px in bytes.as_chunks_mut::<4>().0 {
                    let ink = lut[usize::from(luma8([px[0], px[1], px[2]]))];
                    px[..3].copy_from_slice(&ink);
                }
            }
            _ => return None,
        }
        Some(tiles.insert_bytes(bytes))
    })
    .map_err(|e| e.to_string())?;

    let revision = editor.revision();
    editor.apply_command(command);
    let applied = editor
        .active()
        .is_some_and(|d| d.document.meta.color_mode == to);
    if !applied || editor.revision() == revision {
        return Err(format!("Could not convert to {}", target.label()));
    }
    Ok(format!("Changed colour mode to {}", target.label()))
}

/// Rec.601 luma of an 8-bit colour, as the Grayscale conversion takes it.
fn luma8(rgb: [u8; 3]) -> u8 {
    (0.299 * f32::from(rgb[0]) + 0.587 * f32::from(rgb[1]) + 0.114 * f32::from(rgb[2]))
        .round()
        .clamp(0.0, 255.0) as u8
}

/// W10-H: Image > Mode > Bitmap. The visible composite, over white (a
/// bitmap has no transparency), becomes one grey plane; `method` reduces it
/// to pure black and white across the whole canvas (so a diffusion's error
/// and a screen's cells cross tile edges without a seam); the result is ONE
/// opaque "Background" layer, every old layer is deleted and the mode flag
/// set — one Transaction, so one undo gives the layers and the mode back.
fn bitmap_flattened(
    editor: &mut Editor,
    method: color::bitmap::BitmapMethod,
    label: String,
) -> Result<String, String> {
    let doc = editor.active_mut().ok_or("No document is open")?;
    let (w, h) = (doc.document.width(), doc.document.height());
    let canvas = compositor::composite_region(
        &doc.document,
        &doc.tiles,
        doc.canvas_rect(),
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())?
    .to_rgba8(&doc.document.meta.color_space);
    let gray: Vec<u8> = canvas
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = f32::from(p[3]) / 255.0;
            let g = f32::from(luma8([p[0], p[1], p[2]]));
            (g * a + 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8
        })
        .collect();
    let bw = color::bitmap::to_bitmap(&gray, w as usize, h as usize, method);
    let rgba: Vec<u8> = bw.iter().flat_map(|&v| [v, v, v, 255]).collect();
    let grid = raster::TileGrid::from_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
    let layer = layer_model::Layer::raster("Background");
    let new_id = layer.id;
    let old: Vec<layer_model::LayerId> = doc.document.layers.iter_depth_first();
    let mut commands = vec![
        editor_core::Command::SetMetaColorMode {
            from: doc.document.meta.color_mode,
            to: mode::BITMAP,
        },
        editor_core::Command::create_layer(layer),
    ];
    let edits: Vec<_> = grid
        .iter()
        .map(|(coord, tile)| {
            editor_core::pixels::TileEdit::set(coord, doc.tiles.insert_bytes(tile.data().to_vec()))
        })
        .collect();
    commands.push(
        editor_core::Command::paint_tiles(editor_core::PixelTarget::Layer(new_id), edits)
            .map_err(|e| e.to_string())?,
    );
    for id in old.iter().rev() {
        commands.push(editor_core::Command::DeleteLayer { layer_id: *id });
    }
    let layers = old.len();
    let revision = editor.revision();
    editor.apply_command(editor_core::Command::Transaction { label, commands });
    let applied = editor
        .active()
        .is_some_and(|d| d.document.meta.color_mode == mode::BITMAP);
    if !applied || editor.revision() == revision {
        return Err("Could not convert to Bitmap".into());
    }
    Ok(if layers > 1 {
        format!("Changed colour mode to Bitmap; its {layers} layers were flattened into one")
    } else {
        "Changed colour mode to Bitmap".to_string()
    })
}

/// W10-H: Image > Mode > Indexed Color on a document of several layers: the
/// visible composite becomes one "Background" layer quantised onto a palette
/// built from that composite, every old layer is deleted and the mode flag
/// set — one Transaction, so one undo restores the layers and the mode.
fn indexed_flattened(
    editor: &mut Editor,
    spec: ui::dialogs::IndexedSpec,
    label: String,
) -> Result<String, String> {
    let doc = editor.active_mut().ok_or("No document is open")?;
    let (w, h) = (doc.document.width(), doc.document.height());
    let canvas = compositor::composite_region(
        &doc.document,
        &doc.tiles,
        doc.canvas_rect(),
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())?
    .to_rgba8(&doc.document.meta.color_space);
    let mut histogram = Histogram::new();
    histogram.add_rgba8(&canvas);
    let palette = quantize::build_palette(&histogram, spec.palette, spec.colors)
        .map_err(|e| format!("Cannot convert to Indexed Color: {e}"))?;
    let grid = raster::TileGrid::from_rgba8(w, h, &canvas).map_err(|e| e.to_string())?;
    let layer = layer_model::Layer::raster("Background");
    let new_id = layer.id;
    let old: Vec<layer_model::LayerId> = doc.document.layers.iter_depth_first();
    let mut commands = vec![
        editor_core::Command::SetMetaColorMode {
            from: doc.document.meta.color_mode,
            to: mode::INDEXED,
        },
        editor_core::Command::create_layer(layer),
    ];
    let mut edits = Vec::new();
    for (coord, tile) in grid.iter() {
        let mut bytes = tile.data().to_vec();
        quantize::remap_rgba8(&mut bytes, TILE_SIZE as usize, &palette, spec.dither);
        if bytes.iter().all(|&b| b == 0) {
            continue;
        }
        edits.push(editor_core::pixels::TileEdit::set(
            coord,
            doc.tiles.insert_bytes(bytes),
        ));
    }
    if !edits.is_empty() {
        commands.push(
            editor_core::Command::paint_tiles(editor_core::PixelTarget::Layer(new_id), edits)
                .map_err(|e| e.to_string())?,
        );
    }
    // Deepest first, so deleting a group never strands a child on the list.
    for id in old.iter().rev() {
        commands.push(editor_core::Command::DeleteLayer { layer_id: *id });
    }
    let layers = old.len();
    let revision = editor.revision();
    editor.apply_command(editor_core::Command::Transaction { label, commands });
    let applied = editor
        .active()
        .is_some_and(|d| d.document.meta.color_mode == mode::INDEXED);
    if !applied || editor.revision() == revision {
        return Err("Could not convert to Indexed Color".into());
    }
    Ok(format!(
        "Changed colour mode to Indexed Color; its {layers} layers were flattened into one"
    ))
}

/// W8-B: Image > Adjustments > Levels / Curves on a Lab document run on its
/// L, a and b channels ([`adjustments::LabTone`]): the dialog lists
/// Lightness, a and b there, opening on Lightness
/// (`AdjustmentDialog::set_lab_channels`), so the stored red/green/blue
/// fields it confirms mean L/a/b, and a stored composite acts on L only. `None`
/// when the document is not in Lab mode or `kind` is neither Levels nor
/// Curves: the caller then applies it in RGB as before.
pub(crate) fn run_lab_tone(
    editor: &mut Editor,
    kind: &layer_model::AdjustmentKind,
    label: &str,
) -> Option<Result<String, String>> {
    if editor.active()?.document.meta.color_mode != mode::LAB {
        return None;
    }
    let tone = adjustments::LabTone::from_kind(kind)?;
    if tone.is_identity() {
        return Some(Err(format!(
            "{label} is at its identity setting, so applying it would change \
             nothing; move a control in its dialog first"
        )));
    }
    Some(
        super::edit_active_pixels(editor, label, |buffer, _| {
            tone.apply_premultiplied_rgba(buffer.pixels_mut());
            Ok(())
        })
        .map(|()| format!("{label} applied on the Lab channels")),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{context, perform, resolve_intent};
    use crate::chrome::ChromeOutput;
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use raster::PixelRect;
    use std::collections::HashSet;
    use ui::menu::{ColorMode, MenuAction};

    /// A 64x32 document: a saturated green / blue-red gradient on the left,
    /// mid grey on the right — many colours, some unprintable.
    fn fixture(dir: &std::path::Path) -> Editor {
        fixture_with(dir, ScriptedDialogs::new())
    }

    /// [`fixture`] answering the host's file pickers with `dialogs`.
    fn fixture_with(dir: &std::path::Path, dialogs: ScriptedDialogs) -> Editor {
        let (w, h) = (64u32, 32u32);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if x < 32 {
                    rgba.extend_from_slice(&[
                        (x * 8) as u8,
                        255 - (y * 4) as u8,
                        (y * 8) as u8,
                        255,
                    ]);
                } else {
                    rgba.extend_from_slice(&[128, 128, 128, 255]);
                }
            }
        }
        let path = dir.join("modes.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        );
        ed.open_path(&path).unwrap();
        ed
    }

    fn composite(ed: &mut Editor) -> Vec<u8> {
        let doc = ed.active_mut().unwrap();
        let rect = PixelRect::new(0, 0, doc.document.width(), doc.document.height());
        doc.composite(rect).unwrap()
    }

    fn distinct(rgba: &[u8]) -> usize {
        rgba.as_chunks::<4>()
            .0
            .iter()
            .map(|p| [p[0], p[1], p[2]])
            .collect::<HashSet<_>>()
            .len()
    }

    fn history_len(ed: &Editor) -> usize {
        ed.active().unwrap().history.journal().count()
    }

    /// W8-B: open an Image > Adjustments dialog through the real host,
    /// let `edit` set its parameters, press Enter, and perform the pick the
    /// confirmation put in the frame's output — the road a user's Ctrl+L /
    /// Ctrl+M takes.
    fn confirm_adjustment_through_the_host(
        ed: &mut Editor,
        id: ui::menu::AdjustmentId,
        edit: impl FnOnce(&mut ui::dialogs::AdjustmentDialog),
    ) -> Result<String, String> {
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::ApplyAdjustment(id), ed));
        edit(host.active_adjustment_dialog_for_test());
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = ChromeOutput::default();
        let _ = ctx.run(input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let _ = ctx.run(input(vec![enter]), |ctx| host.ui(ctx, None, &mut out));
        assert!(!host.is_open(), "Enter did not confirm the dialog");
        assert_eq!(out.menu, vec![MenuAction::ApplyAdjustment(id)]);
        perform(out.menu[0], ed)
    }

    fn lab_of(px: &[u8]) -> [f32; 3] {
        color::model::rgb_to_lab([px[0], px[1], px[2]].map(|c| f32::from(c) / 255.0))
    }

    /// W8-B: on a Lab document, Image > Adjustments > Levels opens listing the
    /// Lab channels, and a Lightness mapping confirmed in it darkens L and
    /// keeps every colour's a and b — the mid grey stays neutral, which the
    /// same numbers run on RGB's red channel would not do.
    #[test]
    fn levels_on_a_lab_document_runs_on_lightness_through_the_dialog_and_menu() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        perform(MenuAction::SetColorMode(ColorMode::Lab), &mut ed).unwrap();
        let before = composite(&mut ed);
        let steps = history_len(&ed);
        const ID: [f32; 5] = [0.0, 1.0, 1.0, 0.0, 1.0];
        let status =
            confirm_adjustment_through_the_host(&mut ed, ui::menu::AdjustmentId::Levels, |d| {
                assert!(
                    d.lab_channels(),
                    "the Levels dialog does not know it is Lab"
                );
                assert_eq!(d.levels_channel(), 1, "Levels opens off Lightness");
                assert!(d.set_kind(layer_model::AdjustmentKind::LevelsFull {
                    composite: ID,
                    red: [0.0, 1.0, 1.0, 0.0, 0.5],
                    green: ID,
                    blue: ID,
                }));
            })
            .unwrap();
        assert!(status.contains("Lab"), "{status}");
        assert_eq!(history_len(&ed), steps + 1, "one undo step");
        let after = composite(&mut ed);
        // (48, 0): the mid grey. Neutral before and after, and darker.
        let at = 48 * 4;
        let (g0, g1) = (&before[at..at + 4], &after[at..at + 4]);
        assert!(
            g1[0] == g1[1] && g1[1] == g1[2],
            "the grey took a cast: {g1:?}"
        );
        assert!(
            g1[0] < g0[0] - 30,
            "the grey did not darken: {g0:?} -> {g1:?}"
        );
        let (l0, l1) = (lab_of(g0), lab_of(g1));
        assert!((l1[0] - l0[0] * 0.5).abs() < 1.5, "L {l0:?} -> {l1:?}");
        // (8, 4): a saturated green darkens and keeps its hue. (Its chroma
        // is at the sRGB gamut edge, so a darker L clips some of it on the way
        // back to RGB; the a/b direction is what Lightness must not turn.)
        let at = (8 + 4 * 64) * 4;
        let (c0, c1) = (lab_of(&before[at..at + 4]), lab_of(&after[at..at + 4]));
        assert!(c1[0] < c0[0] - 10.0, "L {c0:?} -> {c1:?}");
        let hue = |c: [f32; 3]| c[2].atan2(c[1]).to_degrees();
        assert!((hue(c1) - hue(c0)).abs() < 5.0, "hue {c0:?} -> {c1:?}");
    }

    /// W8-B: a composite Levels confirmed on a Lab document through the menu
    /// (the shape a plain Levels, a preset or an older file stores) acts on
    /// Lightness only: the mid grey darkens and stays neutral. Run on a and
    /// b too, a black point of 0.2 casts it blue-cyan.
    #[test]
    fn a_composite_levels_on_a_lab_document_keeps_the_grey_neutral() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        perform(MenuAction::SetColorMode(ColorMode::Lab), &mut ed).unwrap();
        let before = composite(&mut ed);
        const ID: [f32; 5] = [0.0, 1.0, 1.0, 0.0, 1.0];
        confirm_adjustment_through_the_host(&mut ed, ui::menu::AdjustmentId::Levels, |d| {
            assert!(d.set_kind(layer_model::AdjustmentKind::LevelsFull {
                composite: [0.2, 1.0, 1.0, 0.0, 1.0],
                red: ID,
                green: ID,
                blue: ID,
            }));
        })
        .unwrap();
        let after = composite(&mut ed);
        let at = 48 * 4;
        let (g0, g1) = (&before[at..at + 4], &after[at..at + 4]);
        let spread = g1[..3].iter().max().unwrap() - g1[..3].iter().min().unwrap();
        assert!(spread <= 1, "the grey took a cast: {g0:?} -> {g1:?}");
        assert!(
            g1[0] < g0[0] - 10,
            "the grey did not darken: {g0:?} -> {g1:?}"
        );
    }

    /// W8-B: Curves on a Lab document: raising the a curve through the dialog
    /// pushes the grey towards magenta (a > 0) at the same lightness. On an
    /// RGB document the same stored curve is green's and turns it green.
    #[test]
    fn curves_on_a_lab_document_moves_the_a_channel_and_rgb_documents_keep_green() {
        let curves = layer_model::AdjustmentKind::CurvesFull {
            composite: vec![[0.0, 0.0], [1.0, 1.0]],
            red: vec![[0.0, 0.0], [1.0, 1.0]],
            green: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
            blue: vec![[0.0, 0.0], [1.0, 1.0]],
        };
        let at = 48 * 4;
        let run = |lab: bool| {
            let dir = tempfile::tempdir().unwrap();
            let mut ed = fixture(dir.path());
            if lab {
                perform(MenuAction::SetColorMode(ColorMode::Lab), &mut ed).unwrap();
            }
            let before = composite(&mut ed);
            let kind = curves.clone();
            confirm_adjustment_through_the_host(&mut ed, ui::menu::AdjustmentId::Curves, |d| {
                assert_eq!(d.lab_channels(), lab);
                assert_eq!(d.curve_editor().channel(), usize::from(lab));
                assert!(d.set_kind(kind));
            })
            .unwrap();
            let after = composite(&mut ed);
            (lab_of(&before[at..at + 4]), after[at..at + 4].to_vec())
        };
        let (grey, lab_after) = run(true);
        let moved = lab_of(&lab_after);
        assert!(moved[1] > 10.0, "a did not move: {grey:?} -> {moved:?}");
        assert!(
            (moved[0] - grey[0]).abs() < 3.0,
            "L moved: {grey:?} -> {moved:?}"
        );
        let (_, rgb_after) = run(false);
        assert!(
            rgb_after[1] > rgb_after[0] && rgb_after[1] > rgb_after[2],
            "on RGB the curve is green's: {rgb_after:?}"
        );
    }

    #[test]
    fn the_image_mode_rows_are_enabled_and_cmyk_clamps_green_but_not_grey_in_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        let chrome = crate::chrome::Chrome::new();
        let menu = context(&mut ed, chrome.workspace());
        for &mode in &[ColorMode::Lab, ColorMode::Cmyk, ColorMode::Indexed] {
            assert!(
                resolve_intent(MenuAction::SetColorMode(mode), &menu, &ed).is_ok(),
                "{mode:?} is greyed"
            );
        }
        let before = composite(&mut ed);
        let steps = history_len(&ed);
        perform(MenuAction::SetColorMode(ColorMode::Cmyk), &mut ed).unwrap();
        assert_eq!(history_len(&ed), steps + 1, "one undo step");
        let after = composite(&mut ed);
        assert_eq!(ed.active().unwrap().document.meta.color_mode, 3);
        // Top-left pixel is (0, 255, 0): unprintable, so it moved; the grey
        // half printed as it was.
        assert_ne!(&after[0..3], &before[0..3], "saturated green was clamped");
        assert!(
            color::cmyk::max_channel_delta(
                [after[0], after[1], after[2]],
                color::cmyk::ProofLut::shared().proof([after[0], after[1], after[2]])
            ) <= color::cmyk::GAMUT_THRESHOLD + 2
        );
        let grey_at = (40 * 4) as usize;
        assert_eq!(&after[grey_at..grey_at + 4], &[128, 128, 128, 255]);
        // The menu now ticks CMYK.
        let chrome = crate::chrome::Chrome::new();
        assert_eq!(
            context(&mut ed, chrome.workspace()).color_mode,
            ColorMode::Cmyk
        );
        // One undo restores both pixels and mode.
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(ed.active().unwrap().document.meta.color_mode, 0);
        assert_eq!(composite(&mut ed), before);
    }

    #[test]
    fn lab_changes_the_mode_and_no_pixel() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        let before = composite(&mut ed);
        perform(MenuAction::SetColorMode(ColorMode::Lab), &mut ed).unwrap();
        assert_eq!(ed.active().unwrap().document.meta.color_mode, 2);
        assert_eq!(composite(&mut ed), before);
        assert!(perform(MenuAction::SetColorMode(ColorMode::Lab), &mut ed).is_err());
        // Lab -> RGB is one step too.
        perform(MenuAction::SetColorMode(ColorMode::Rgb), &mut ed).unwrap();
        assert_eq!(ed.active().unwrap().document.meta.color_mode, 0);
    }

    #[test]
    fn indexed_asks_in_its_dialog_and_sixteen_colours_leave_at_most_sixteen() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        let before = composite(&mut ed);
        assert!(distinct(&before) > 16);
        let mut chrome = crate::chrome::Chrome::new();
        let menu = context(&mut ed, chrome.workspace());
        let intent =
            resolve_intent(MenuAction::SetColorMode(ColorMode::Indexed), &menu, &ed).unwrap();
        let mut out = ChromeOutput::default();
        chrome.menu_click(intent, &ed, &mut out);
        assert!(chrome.dialog_open(), "Indexed Color… opened its dialog");
        assert!(out.menu.is_empty(), "nothing converted before the answer");
        chrome
            .dialogs_for_test()
            .active_indexed_dialog_for_test()
            .set_spec(ui::dialogs::IndexedSpec {
                palette: color::quantize::PaletteKind::Adaptive,
                colors: 16,
                dither: color::quantize::Dither::Diffusion,
            });
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let input = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input(Vec::new()), |ctx| {
            chrome.dialogs_for_test().ui(ctx, None, &mut out)
        });
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let _ = ctx.run(input(vec![enter]), |ctx| {
            chrome.dialogs_for_test().ui(ctx, None, &mut out)
        });
        assert!(!chrome.dialog_open());
        assert_eq!(out.menu, vec![MenuAction::SetColorMode(ColorMode::Indexed)]);
        let steps = history_len(&ed);
        for pick in std::mem::take(&mut out.menu) {
            perform(pick, &mut ed).unwrap();
        }
        assert_eq!(history_len(&ed), steps + 1, "one undo step");
        assert_eq!(ed.active().unwrap().document.meta.color_mode, 4);
        let after = composite(&mut ed);
        let n = distinct(&after);
        assert!(n <= 16, "{n} colours after a 16-colour conversion");
        // GIF export writes that palette.
        let gif = dir.path().join("out.gif");
        ed.active_mut().unwrap().export_to(&gif).unwrap();
        // One undo is the original.
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(composite(&mut ed), before);
        // The GIF, opened again, holds exactly the indexed colours.
        ed.open_path(&gif).unwrap();
        let reopened = composite(&mut ed);
        assert_eq!(reopened, after, "the palette came through exactly");
    }

    /// W10-H: add a raster layer over the fixture holding a half-transparent
    /// wash of `rgba` over the whole 64x32 canvas.
    fn add_wash_layer(ed: &mut Editor, rgba: [u8; 4]) {
        let doc = ed.active_mut().unwrap();
        let pixels: Vec<u8> = std::iter::repeat_n(rgba, 64 * 32).flatten().collect();
        let grid = raster::TileGrid::from_rgba8(64, 32, &pixels).unwrap();
        let layer = layer_model::Layer::raster("wash");
        let id = layer.id;
        let edits: Vec<_> = grid
            .iter()
            .map(|(coord, tile)| {
                editor_core::pixels::TileEdit::set(
                    coord,
                    doc.tiles.insert_bytes(tile.data().to_vec()),
                )
            })
            .collect();
        let command = editor_core::Command::Transaction {
            label: "wash".into(),
            commands: vec![
                editor_core::Command::create_layer(layer),
                editor_core::Command::paint_tiles(editor_core::PixelTarget::Layer(id), edits)
                    .unwrap(),
            ],
        };
        ed.apply_command(command);
    }

    /// W10-H: Indexed Color flattens a layered document like Photoshop: one
    /// layer remains, the composite holds only palette colours (a half-
    /// transparent layer over a quantised one would blend new ones), the
    /// status line warns that the layers were flattened, and one undo gives
    /// the layers and the mode back.
    #[test]
    fn indexed_color_flattens_a_layered_document_in_one_step_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        add_wash_layer(&mut ed, [200, 30, 90, 128]);
        assert_eq!(
            ed.active()
                .unwrap()
                .document
                .layers
                .iter_depth_first()
                .len(),
            2
        );
        let before = composite(&mut ed);
        let steps = history_len(&ed);
        let status = set_color_mode_for_test(
            &mut ed,
            ui::dialogs::IndexedSpec {
                palette: color::quantize::PaletteKind::Adaptive,
                colors: 8,
                dither: color::quantize::Dither::None,
            },
        );
        assert!(status.contains("flattened"), "no warning: {status}");
        assert_eq!(history_len(&ed), steps + 1, "one undo step");
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.meta.color_mode, 4);
        assert_eq!(doc.document.layers.iter_depth_first().len(), 1);
        let after = composite(&mut ed);
        let n = distinct(&after);
        assert!(n <= 8, "{n} colours in an 8-colour indexed composite");
        ed.active_mut().unwrap().undo().unwrap();
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.meta.color_mode, 0);
        assert_eq!(doc.document.layers.iter_depth_first().len(), 2);
        assert_eq!(composite(&mut ed), before);
    }

    fn set_color_mode_for_test(ed: &mut Editor, spec: ui::dialogs::IndexedSpec) -> String {
        super::set_color_mode_with(ed, ColorMode::Indexed, Some(spec), Default::default()).unwrap()
    }

    /// The SOF0 component count of a baseline JPEG and whether it carries
    /// the Adobe APP14 marker a CMYK JPEG is read by.
    fn jpeg_components(bytes: &[u8]) -> (u8, bool) {
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "not a JPEG");
        let mut i = 2;
        let mut adobe = false;
        while i + 9 < bytes.len() {
            assert_eq!(bytes[i], 0xFF, "marker expected at {i}");
            let marker = bytes[i + 1];
            let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
            if marker == 0xEE && &bytes[i + 4..i + 9] == b"Adobe" {
                adobe = true;
            }
            if marker == 0xC0 {
                return (bytes[i + 9], adobe);
            }
            i += 2 + len;
        }
        panic!("no SOF0");
    }

    /// The PNG IHDR (bit depth, colour type).
    fn png_layout(bytes: &[u8]) -> (u8, u8) {
        assert_eq!(&bytes[12..16], b"IHDR");
        (bytes[24], bytes[25])
    }

    #[test]
    fn file_export_of_a_cmyk_document_writes_a_cmyk_jpeg_and_an_rgb_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let rgb_out = dir.path().join("rgb.jpg");
        let mut ed = fixture_with(
            dir.path(),
            ScriptedDialogs::new().exporting_to(rgb_out.clone()),
        );
        ed.dispatch(crate::action::Action::Export).unwrap();
        let rgb = std::fs::read(&rgb_out).unwrap();
        assert_eq!(jpeg_components(&rgb).0, 3, "an RGB document writes RGB");

        let cmyk_out = dir.path().join("cmyk.jpg");
        let mut ed = fixture_with(
            dir.path(),
            ScriptedDialogs::new().exporting_to(cmyk_out.clone()),
        );
        perform(MenuAction::SetColorMode(ColorMode::Cmyk), &mut ed).unwrap();
        ed.dispatch(crate::action::Action::Export).unwrap();
        let cmyk = std::fs::read(&cmyk_out).unwrap();
        assert_eq!(
            jpeg_components(&cmyk),
            (4, true),
            "File > Export of a CMYK document writes a four-component Adobe JPEG"
        );
        // And this application reads its own file back as colour: the grey
        // half prints as the grey it was.
        let decoded = raster::decode_path(&cmyk_out).unwrap();
        let grey = ((10 * 64 + 48) * 4) as usize;
        for c in 0..3 {
            assert!(
                decoded.rgba8[grey + c].abs_diff(128) <= 10,
                "grey came back as {:?}",
                &decoded.rgba8[grey..grey + 4]
            );
        }
    }

    #[test]
    fn export_as_opened_on_a_lab_document_says_lab_goes_out_as_rgb() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        perform(MenuAction::SetColorMode(ColorMode::Lab), &mut ed).unwrap();
        let mut chrome = crate::chrome::Chrome::new();
        let menu = context(&mut ed, chrome.workspace());
        let intent =
            resolve_intent(MenuAction::Export(raster::ExportFormat::Tiff), &menu, &ed).unwrap();
        let mut out = ChromeOutput::default();
        chrome.menu_click(intent, &ed, &mut out);
        assert!(chrome.dialog_open(), "Export As opened");
        let note = chrome
            .dialogs_for_test()
            .active_export_dialog_for_test()
            .ink_note();
        assert_eq!(note, Some(ui::strings::tr("ui.export_as.lab.as.rgb")));
    }

    #[test]
    fn export_as_writes_cmyk_jpeg_and_tiff_for_cmyk_and_a_palette_png_for_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        let job = |base: &str| ui::dialogs::ExportJob {
            base_name: base.to_string(),
            entries: vec![
                ui::dialogs::ExportEntry::new("", raster::ExportFormat::Jpeg(90), 1.0),
                ui::dialogs::ExportEntry::new("", raster::ExportFormat::Tiff, 1.0),
                ui::dialogs::ExportEntry::new("", raster::ExportFormat::Png, 1.0),
            ],
        };
        perform(MenuAction::SetColorMode(ColorMode::Cmyk), &mut ed).unwrap();
        let cmyk_dir = dir.path().join("cmyk");
        ed.request_export(job("c"), cmyk_dir.clone());
        let jpeg = std::fs::read(cmyk_dir.join("c.jpg")).unwrap();
        assert_eq!(jpeg_components(&jpeg), (4, true), "CMYK JPEG");
        let tiff = std::fs::read(cmyk_dir.join("c.tif")).unwrap();
        // PhotometricInterpretation (tag 262) = 5, separated (CMYK).
        let le = &tiff[..2] == b"II";
        let tag = |t: u16| {
            if le {
                t.to_le_bytes()
            } else {
                t.to_be_bytes()
            }
        };
        let photometric = tiff
            .windows(10)
            .find(|w| w[..2] == tag(262) && w[2..4] == tag(3))
            .map(|w| if le { w[8] } else { w[9] })
            .expect("a PhotometricInterpretation entry");
        assert_eq!(photometric, 5, "CMYK TIFF");
        let png = std::fs::read(cmyk_dir.join("c.png")).unwrap();
        assert_ne!(
            png_layout(&png).1,
            3,
            "a PNG cannot carry CMYK: it stays RGB"
        );

        perform(MenuAction::SetColorMode(ColorMode::Indexed), &mut ed).unwrap();
        let indexed_dir = dir.path().join("indexed");
        ed.request_export(job("i"), indexed_dir.clone());
        let png = std::fs::read(indexed_dir.join("i.png")).unwrap();
        assert_eq!(png_layout(&png), (8, 3), "Indexed writes a palette PNG-8");
        let jpeg = std::fs::read(indexed_dir.join("i.jpg")).unwrap();
        assert_eq!(jpeg_components(&jpeg).0, 3, "an Indexed JPEG is RGB");
    }

    /// W8-B: a 64x64 document, an opaque gradient on the left half and fully
    /// transparent on the right, its camera at 100% centred (screen
    /// `(200, 150)` is document `(32, 32)`), answering File > Export with
    /// `export_to`.
    fn half_transparent(dir: &std::path::Path, export_to: std::path::PathBuf) -> Editor {
        let (w, h) = (64u32, 64u32);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if x < 32 {
                    rgba.extend_from_slice(&[(x * 8) as u8, (y * 4) as u8, 90, 255]);
                } else {
                    rgba.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let path = dir.join("half.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new().exporting_to(export_to)),
        );
        ed.open_path(&path).unwrap();
        let doc = ed.active_mut().unwrap();
        doc.set_viewport(glam::Vec2::new(400.0, 300.0));
        doc.camera.zoom = 1.0;
        doc.camera.center = glam::Vec2::new(32.0, 32.0);
        ed
    }

    /// A real Brush drag through the pointer route, in document points.
    fn brush_stroke(ed: &mut Editor, points: &[(f32, f32)]) {
        use ui::canvas::{PointerInput, PointerPhase};
        ed.set_tool(tools::ToolId::Brush);
        ed.set_foreground([0.9, 0.1, 0.1, 1.0]);
        let screen = |x: f32, y: f32| glam::Vec2::new(200.0 + x - 32.0, 150.0 + y - 32.0);
        let mut pointer = crate::tool_input::ToolPointer::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            let phase = if i == 0 {
                PointerPhase::Down
            } else {
                PointerPhase::Move
            };
            pointer.handle(ed, PointerInput::at(phase, screen(x, y)), false, &[]);
        }
        let (x, y) = *points.last().unwrap();
        pointer.handle(
            ed,
            PointerInput::at(PointerPhase::Up, screen(x, y)),
            false,
            &[],
        );
    }

    /// A real drag of whatever tool is active, through the pointer route,
    /// in document points (the camera of [`half_transparent`]).
    fn drag(ed: &mut Editor, points: &[(f32, f32)]) {
        use ui::canvas::{PointerInput, PointerPhase};
        let screen = |x: f32, y: f32| glam::Vec2::new(200.0 + x - 32.0, 150.0 + y - 32.0);
        let mut pointer = crate::tool_input::ToolPointer::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            let phase = if i == 0 {
                PointerPhase::Down
            } else {
                PointerPhase::Move
            };
            pointer.handle(ed, PointerInput::at(phase, screen(x, y)), false, &[]);
        }
        let (x, y) = *points.last().unwrap();
        pointer.handle(
            ed,
            PointerInput::at(PointerPhase::Up, screen(x, y)),
            false,
            &[],
        );
    }

    /// W10-H: a Brush, Pencil or Eraser stroke at 1% flow on a 16-bit layer
    /// is blended and written at 16 bits through the real pointer route.
    /// The layer was widened from 8 bits, so every stored code is a multiple
    /// of 257; a dab rounded through 8 bits would leave only such codes (and
    /// at 1% flow would mostly not move a pixel at all), while a 16-bit dab
    /// lands between them.
    #[test]
    fn one_percent_flow_strokes_on_a_sixteen_bit_layer_write_sixteen_bit_codes() {
        for tool in [
            tools::ToolId::Brush,
            tools::ToolId::Pencil,
            tools::ToolId::Eraser,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut ed = half_transparent(dir.path(), dir.path().join("unused.png"));
            perform(
                MenuAction::SetBitDepth(ui::menu::ChannelDepth::Sixteen),
                &mut ed,
            )
            .unwrap();
            let layer = ed.active().unwrap().document.active_layer().unwrap();
            let before = ed.active().unwrap().layer_rgba16(layer);
            assert!(
                before.iter().all(|c| c % 257 == 0),
                "precondition: a widened 8-bit layer"
            );
            ed.set_tool(tool);
            ed.set_foreground([0.9, 0.1, 0.1, 1.0]);
            let mut brush = *ed.brush();
            brush.flow = 0.01;
            brush.opacity = 1.0;
            brush.size = 12.0;
            brush.hardness = 1.0;
            ed.set_brush(brush);
            drag(&mut ed, &[(6.0, 10.0), (14.0, 10.0), (22.0, 10.0)]);
            let doc = ed.active().unwrap();
            assert_eq!(doc.document.meta.bit_depth, 16);
            let after = doc.layer_rgba16(layer);
            let changed: Vec<usize> = (0..before.len() / 4)
                .filter(|&i| before[i * 4..i * 4 + 4] != after[i * 4..i * 4 + 4])
                .collect();
            assert!(
                changed.len() > 50,
                "{tool:?}: the stroke moved {} pixels",
                changed.len()
            );
            let between = changed
                .iter()
                .filter(|&&i| after[i * 4..i * 4 + 4].iter().any(|c| c % 257 != 0))
                .count();
            assert!(
                between * 10 >= changed.len() * 9,
                "{tool:?}: only {between} of {} painted pixels hold a code between two 8-bit ones",
                changed.len()
            );
        }
    }

    /// Open `action`'s dialog through the chrome's real menu click, let
    /// `edit` set it, press Enter, and perform the pick the confirmation put
    /// in the frame's output.
    fn confirm_image_gap(
        ed: &mut Editor,
        action: MenuAction,
        edit: impl FnOnce(&mut super::super::image_dialogs::ImageDialog),
    ) -> Result<String, String> {
        let mut chrome = crate::chrome::Chrome::new();
        let menu = context(ed, chrome.workspace());
        let intent = resolve_intent(action, &menu, ed)?;
        let mut out = ChromeOutput::default();
        chrome.menu_click(intent, ed, &mut out);
        assert!(chrome.dialog_open(), "{action:?} opened its dialog");
        assert!(out.menu.is_empty(), "nothing ran before the answer");
        edit(chrome.dialogs_for_test().active_image_gap_for_test());
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let input = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input(Vec::new()), |ctx| {
            chrome.dialogs_for_test().ui(ctx, None, &mut out)
        });
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let _ = ctx.run(input(vec![enter]), |ctx| {
            chrome.dialogs_for_test().ui(ctx, None, &mut out)
        });
        assert!(!chrome.dialog_open(), "Enter did not confirm the dialog");
        assert_eq!(out.menu, vec![action]);
        perform(action, ed)
    }

    /// W10-H: Bitmap is greyed (with a reason) on an RGB document and
    /// refused by the arm; from Grayscale its dialog's method flattens the
    /// two layers into ONE opaque layer of pure black and white, one undo
    /// step, and undo gives the layers and the mode back.
    #[test]
    fn bitmap_is_reached_from_grayscale_and_leaves_one_black_and_white_layer() {
        use super::super::image_dialogs::ImageDialog;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        let chrome = crate::chrome::Chrome::new();
        let menu = context(&mut ed, chrome.workspace());
        let bitmap = MenuAction::SetColorMode(ColorMode::Bitmap);
        let why = resolve_intent(bitmap, &menu, &ed).unwrap_err();
        assert!(why.contains("Grayscale"), "{why}");
        assert!(perform(bitmap, &mut ed).is_err());
        add_wash_layer(&mut ed, [200, 30, 90, 128]);
        perform(MenuAction::SetColorMode(ColorMode::Grayscale), &mut ed).unwrap();
        let steps = history_len(&ed);
        let status = confirm_image_gap(&mut ed, bitmap, |d| match d {
            ImageDialog::Bitmap(d) => d.set_method(color::bitmap::BitmapMethod::Halftone(
                color::bitmap::Halftone::default(),
            )),
            other => panic!("{other:?}"),
        })
        .unwrap();
        assert!(status.contains("flattened"), "{status}");
        assert_eq!(history_len(&ed), steps + 1, "one undo step");
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.meta.color_mode, 5);
        assert_eq!(doc.document.layers.iter_depth_first().len(), 1);
        let after = composite(&mut ed);
        for px in after.as_chunks::<4>().0 {
            assert!(
                matches!(px, [0, 0, 0, 255] | [255, 255, 255, 255]),
                "{px:?} is not pure black or white"
            );
        }
        let whites = after
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] == 255)
            .count();
        assert!(
            whites > 0 && whites < after.len() / 4,
            "a screen, not a fill"
        );
        ed.active_mut().unwrap().undo().unwrap();
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.meta.color_mode, 1);
        assert_eq!(doc.document.layers.iter_depth_first().len(), 2);
    }

    /// W10-H: Duotone from Grayscale prints every grey through the dialog's
    /// inks (here one red ink): the layers stay, every pixel is the ink
    /// model's colour for its old grey, one undo step.
    #[test]
    fn duotone_prints_every_grey_through_the_dialogs_inks() {
        use super::super::image_dialogs::ImageDialog;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        perform(MenuAction::SetColorMode(ColorMode::Grayscale), &mut ed).unwrap();
        let grey = composite(&mut ed);
        let mut spec = color::duotone::DuotoneSpec::of_type(color::duotone::DuotoneType::Monotone);
        spec.inks[0].color = [200, 20, 20];
        let chosen = spec.clone();
        let steps = history_len(&ed);
        confirm_image_gap(
            &mut ed,
            MenuAction::SetColorMode(ColorMode::Duotone),
            |d| match d {
                ImageDialog::Duotone(d) => d.set_spec(chosen),
                other => panic!("{other:?}"),
            },
        )
        .unwrap();
        assert_eq!(history_len(&ed), steps + 1);
        assert_eq!(ed.active().unwrap().document.meta.color_mode, 6);
        let after = composite(&mut ed);
        for (g, p) in grey.as_chunks::<4>().0.iter().zip(after.as_chunks::<4>().0) {
            assert_eq!([p[0], p[1], p[2]], spec.render(g[0]), "grey {}", g[0]);
        }
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(composite(&mut ed), grey);
    }

    /// W10-H: Image > Apply Image and Calculations open their dialogs from
    /// the menu and the confirmed spec runs: Apply Image of the document's
    /// own inverted grey in Normal changes the layer as one undo step, and
    /// Calculations into a new channel adds a saved "Alpha" selection.
    #[test]
    fn apply_image_and_calculations_run_from_their_dialogs() {
        use super::super::image_dialogs::ImageDialog;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = fixture(dir.path());
        let before = composite(&mut ed);
        let steps = history_len(&ed);
        confirm_image_gap(&mut ed, MenuAction::ApplyImage, |d| match d {
            ImageDialog::ApplyImage(d) => {
                let mut spec = d.spec();
                spec.source.channel = ui::dialogs::SourceChannel::Gray;
                spec.source.invert = true;
                spec.blend = layer_model::BlendMode::Normal;
                d.set_spec(spec);
            }
            other => panic!("{other:?}"),
        })
        .unwrap();
        assert_eq!(history_len(&ed), steps + 1);
        let after = composite(&mut ed);
        let grey_at = 40 * 4;
        assert_eq!(&before[grey_at..grey_at + 3], &[128, 128, 128]);
        assert_eq!(
            &after[grey_at..grey_at + 3],
            &[127, 127, 127],
            "inverted grey"
        );
        confirm_image_gap(&mut ed, MenuAction::Calculations, |d| match d {
            ImageDialog::Calculations(_) => {}
            other => panic!("{other:?}"),
        })
        .unwrap();
        let saved = &ed.active().unwrap().document.saved_selections;
        assert_eq!(saved.len(), 1);
        assert!(saved[0].0.starts_with("Alpha"), "{}", saved[0].0);
    }

    fn distinct_rgba(rgba: &[u8]) -> usize {
        rgba.as_chunks::<4>().0.iter().collect::<HashSet<_>>().len()
    }

    #[test]
    fn file_export_of_an_indexed_document_after_a_soft_stroke_writes_a_palette_png() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("indexed.png");
        let mut ed = half_transparent(dir.path(), out.clone());
        perform(MenuAction::SetColorMode(ColorMode::Indexed), &mut ed).unwrap();
        assert_eq!(ed.active().unwrap().document.meta.color_mode, 4);
        let converted = composite(&mut ed);
        // The palette: at most 256 visible colours (plus transparency, which
        // alone takes a 256-colour palette to 257 RGBA values).
        let visible: Vec<u8> = converted
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0)
            .flatten()
            .copied()
            .collect();
        assert!(distinct(&visible) <= 256);
        // A soft brush across both halves: antialiased alpha over the
        // transparent half, blended colours over the palette half.
        brush_stroke(
            &mut ed,
            &[
                (4.0, 20.0),
                (20.0, 26.0),
                (36.0, 32.0),
                (52.0, 38.0),
                (60.0, 44.0),
            ],
        );
        let painted = composite(&mut ed);
        let soft = painted
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0 && p[3] < 255)
            .count();
        assert!(soft > 0, "the stroke left soft alpha");
        assert!(
            distinct_rgba(&painted) > 256,
            "precondition: the stroke took the image past 256 RGBA colours ({})",
            distinct_rgba(&painted)
        );
        ed.dispatch(crate::action::Action::Export).unwrap();
        let bytes = std::fs::read(&out).unwrap_or_else(|e| {
            panic!(
                "File > Export wrote nothing ({e}); status {:?}",
                ed.status()
            )
        });
        assert_eq!(png_layout(&bytes), (8, 3), "a palette PNG-8");
        let decoded = raster::decode_path(&out).unwrap();
        for (want, got) in painted
            .as_chunks::<4>()
            .0
            .iter()
            .zip(decoded.rgba8.as_chunks::<4>().0)
        {
            // 1-bit transparency, Photoshop's Indexed.
            let opaque = want[3] >= raster::export::ink::INDEXED_ALPHA_THRESHOLD;
            assert_eq!(got[3], if opaque { 255 } else { 0 }, "{want:?} -> {got:?}");
        }
    }

    #[test]
    fn file_export_of_a_lab_document_says_in_the_status_that_it_writes_rgb() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("lab.tif");
        let mut ed = fixture_with(dir.path(), ScriptedDialogs::new().exporting_to(out.clone()));
        ed.dispatch(crate::action::Action::Export).unwrap();
        let rgb_status = ed.status().unwrap_or_default().to_string();
        assert!(rgb_status.starts_with("Exported"), "{rgb_status}");
        assert!(!rgb_status.contains("Lab"), "an RGB document: {rgb_status}");

        let mut ed = fixture_with(dir.path(), ScriptedDialogs::new().exporting_to(out.clone()));
        perform(MenuAction::SetColorMode(ColorMode::Lab), &mut ed).unwrap();
        ed.dispatch(crate::action::Action::Export).unwrap();
        let status = ed.status().unwrap_or_default().to_string();
        assert!(status.starts_with("Exported"), "{status}");
        assert!(
            status.contains("a Lab document is written as RGB"),
            "File > Export of a Lab document names the RGB it wrote: {status}"
        );
        let decoded = raster::decode_path(&out).unwrap();
        assert_eq!(decoded.rgba8.len(), 64 * 32 * 4, "an RGB TIFF was written");
    }

    /// W8-B: `OpenDocument::export_to` and File > Export's worker share one
    /// colour-mode branch: its CMYK and Indexed arms, through both routes,
    /// write the same bytes.
    #[test]
    fn the_shared_ink_branch_writes_cmyk_and_indexed_for_both_export_routes() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (ColorMode::Cmyk, "c.jpg"),
            (ColorMode::Cmyk, "c.tif"),
            (ColorMode::Indexed, "i.png"),
        ];
        for (mode, name) in cases {
            let worker_out = dir.path().join(format!("worker-{name}"));
            let mut ed = fixture_with(
                dir.path(),
                ScriptedDialogs::new().exporting_to(worker_out.clone()),
            );
            perform(MenuAction::SetColorMode(mode), &mut ed).unwrap();
            ed.dispatch(crate::action::Action::Export).unwrap();
            let doc_out = dir.path().join(format!("doc-{name}"));
            ed.active_mut().unwrap().export_to(&doc_out).unwrap();
            let worker = std::fs::read(&worker_out).unwrap();
            let direct = std::fs::read(&doc_out).unwrap();
            assert_eq!(worker, direct, "{mode:?} {name}: the two routes differ");
            match name {
                "c.jpg" => assert_eq!(jpeg_components(&direct), (4, true)),
                "c.tif" => assert_eq!(&direct[..4], b"II*\0"),
                _ => assert_eq!(png_layout(&direct), (8, 3)),
            }
            // And the shared function itself: written, or passed back for
            // an RGB file.
            let doc = ed.active_mut().unwrap();
            let size = (doc.document.width(), doc.document.height());
            let rect = PixelRect::new(0, 0, size.0, size.1);
            let color_mode = doc.document.meta.color_mode;
            let shared = dir.path().join(format!("shared-{name}"));
            let format = crate::doc::export_format_for(&shared).unwrap();
            assert!(
                crate::doc::write_in_document_ink(&shared, format, color_mode, size, || doc
                    .composite(rect))
                .unwrap()
            );
            assert_eq!(std::fs::read(&shared).unwrap(), direct);
            let webp = dir.path().join("rgb.webp");
            assert!(!crate::doc::write_in_document_ink(
                &webp,
                raster::ExportFormat::WebP,
                color_mode,
                size,
                || panic!("no composite for a container that cannot carry the ink")
            )
            .unwrap());
            assert!(!webp.exists());
            // An Indexed GIF is carried by GIF's own palette encoder, so the
            // shared branch does not build a composite it would hand back.
            let gif = dir.path().join("indexed.gif");
            assert!(!crate::doc::write_in_document_ink(
                &gif,
                raster::ExportFormat::Gif,
                color_mode,
                size,
                || panic!("no composite for a GIF, which the RGB road palettises")
            )
            .unwrap());
            assert!(!gif.exists());
        }
    }
}
