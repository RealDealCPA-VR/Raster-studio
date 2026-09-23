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
//! A 16-bit document converts at 8 bits and its apply boundary widens the
//! result back, the same road the Grayscale conversion took before W7-D.

use color::cmyk::{is_out_of_gamut, ProofLut};
use color::quantize::{self, Histogram};
use compositor::TileSource;
use editor_core::color_mode::{convert_color_mode, mode};
use raster::TILE_SIZE;

use crate::editor::Editor;

/// Convert the active document into `target`. `indexed` is the Indexed Color
/// dialog's confirmed spec (the defaults when the pick came from anywhere
/// else).
pub(crate) fn set_color_mode(
    editor: &mut Editor,
    target: ui::menu::ColorMode,
    indexed: Option<ui::dialogs::IndexedSpec>,
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
}
