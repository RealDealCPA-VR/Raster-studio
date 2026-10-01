//! W13-I: Quick Export — the Move tool options bar's button and File ▸
//! Export ▸ Quick Export Selected Layers… (one [`ui::menu::MenuAction`], so the
//! bar and the menu cannot drift).
//!
//! The active layer is composited alone over transparent at canvas size,
//! through the real compositor, so its blend, opacity, mask and effects are
//! honoured the way Export Layers honours them. "Alone" hides every layer
//! that is neither the active layer, one of its enclosing groups (which must
//! stay visible for it to draw) nor, for a group, one of its children. The
//! PNG is written where the user picks, the suggested name being
//! `<document>-<layer>.png` beside the document. It runs on the interaction
//! thread: one layer, one encode.
//!
//! W18-I: Photopea's quick export also takes the **selected layers**, a
//! **scale** and **PNG or SVG**, chosen in the Quick Export window
//! (`ui::panels::quick_export_w18`) that the row and the button open:
//!
//! * with several layers selected, each one is written alone to its own file
//!   beside the one picked, named `<picked name>-<layer>` (one pick, one file
//!   per layer, in stack order, top first);
//! * **PNG** or **SVG**: SVG writes the layer alone through the vector SVG
//!   writer (shapes as paths, text as text, the rest embedded;
//!   `crate::doc::write_vector_svg`) at canvas size, being resolution-free;
//! * the scale, 1x, 2x, 3x or 4x: a PNG is resampled (Lanczos-3) to
//!   that size and its name carries Photoshop's `@2x` suffix.
//!
//! The destination is asked with the Export picker (PNG and SVG among its
//! filters); the window's format sets the written file's extension.

use std::cell::{Cell, RefCell};

use layer_model::LayerId;
use ui::panels::quick_export_w18::{QuickExportChoice, QuickExportDialog, QuickExportFormat};

use crate::editor::Editor;

/// Every enclosing group of `id`, innermost first.
fn ancestors(layers: &layer_model::LayerTree, id: LayerId) -> Vec<LayerId> {
    let mut out = Vec::new();
    let mut parent = layers.parent_of(id);
    while let Some(p) = parent {
        out.push(p);
        parent = layers.parent_of(p);
    }
    out
}

/// A copy of `document` in which only `ids` draw: every other layer is
/// hidden except their enclosing groups and, for a group, its children.
fn staged_alone(document: &editor_core::Document, ids: &[LayerId]) -> editor_core::Document {
    let mut staged = document.clone();
    let keep_above: Vec<LayerId> = ids
        .iter()
        .flat_map(|id| ancestors(&staged.layers, *id))
        .collect();
    for other in staged.layers.iter_depth_first() {
        let related = ids.contains(&other)
            || keep_above.contains(&other)
            || ancestors(&staged.layers, other)
                .iter()
                .any(|a| ids.contains(a));
        if !related {
            if let Some(layer) = staged.layers.get_mut(other) {
                layer.visible = false;
            }
        }
    }
    staged
}

/// The canvas-sized straight-alpha RGBA8 of layer `id` composited alone.
pub(crate) fn layer_alone_rgba8(
    doc: &crate::doc::OpenDocument,
    id: LayerId,
) -> Result<(u32, u32, Vec<u8>), String> {
    let staged = staged_alone(&doc.document, &[id]);
    let (w, h) = (staged.width(), staged.height());
    let canvas = compositor::composite_region(
        &staged,
        &doc.tiles,
        raster::PixelRect::new(0, 0, w, h),
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    Ok((w, h, canvas.to_rgba8(&staged.meta.color_space)))
}

/// W18-I: the scale Photoshop's export suffix on `stem` asks for (`name@2x`
/// is 2, `name@0.5x` is a half), with the stem it leaves; `(stem, None)`
/// when there is no suffix or it is outside 0.01x..=10x.
pub(crate) fn scale_suffix(stem: &str) -> (&str, Option<f32>) {
    let Some((base, tail)) = stem.rsplit_once('@') else {
        return (stem, None);
    };
    let Some(number) = tail.strip_suffix(['x', 'X']) else {
        return (stem, None);
    };
    match number.parse::<f32>() {
        Ok(s) if s.is_finite() && (0.01..=10.0).contains(&s) => (base, Some(s)),
        _ => (stem, None),
    }
}

/// W18-I: the layers Quick Export writes: the selected layers in stack
/// order (top first), or the active layer when the selection does not hold
/// it.
fn export_layers(document: &editor_core::Document) -> Option<Vec<LayerId>> {
    let active = document.active_layer()?;
    let selection = document.layer_selection();
    if selection.len() < 2 || !selection.contains(&active) {
        return Some(vec![active]);
    }
    Some(
        document
            .layers
            .iter_depth_first()
            .into_iter()
            .rev()
            .filter(|id| selection.contains(id))
            .collect(),
    )
}

thread_local! {
    /// W18-I: the open Quick Export window, if any.
    static WINDOW: RefCell<Option<QuickExportDialog>> = const { RefCell::new(None) };
    /// W18-I: what the window's Export… confirmed, for the next pick.
    static CONFIRMED: Cell<Option<QuickExportChoice>> = const { Cell::new(None) };
    /// W18-I: the last confirmed choice, which the window opens on.
    static LAST: Cell<QuickExportChoice> = const {
        Cell::new(QuickExportChoice {
            format: QuickExportFormat::Png,
            scale: 1.0,
        })
    };
}

/// W18-I: Quick Export, the menu row and the Move bar's button. With no
/// confirmed choice it opens the Quick Export window (format and scale, on
/// the last choice); the window's Export… parks its choice and clicks the
/// row again, and that pick asks where and writes the files.
pub(crate) fn quick_export_layer(editor: &mut Editor) -> Result<String, String> {
    let doc = editor
        .active()
        .ok_or_else(|| "No document is open".to_string())?;
    let layers = export_layers(&doc.document)
        .ok_or_else(|| "Quick Export needs an active layer".to_string())?
        .len();
    match CONFIRMED.with(Cell::take) {
        Some(choice) => export_with(editor, choice),
        None => {
            let dialog = QuickExportDialog::new(layers, LAST.with(Cell::get));
            WINDOW.with(|w| *w.borrow_mut() = Some(dialog));
            Ok("Quick Export: choose the format and the scale".to_string())
        }
    }
}

/// W18-I: draw the Quick Export window while it is open. Export… parks the
/// choice and clicks the Quick Export row through `on_click`, which the
/// chrome routes back to [`quick_export_layer`].
pub(crate) fn draw_window(ctx: &egui::Context, on_click: &mut dyn FnMut(ui::Intent)) {
    use ui::dialogs::chrome::DialogOutcome;
    let outcome = WINDOW.with(|w| w.borrow_mut().as_mut().map(|d| d.show(ctx)));
    match outcome {
        Some(DialogOutcome::Confirmed(choice)) => {
            WINDOW.with(|w| *w.borrow_mut() = None);
            CONFIRMED.with(|c| c.set(Some(choice)));
            LAST.with(|l| l.set(choice));
            on_click(ui::Intent::Action(ui::menu::MenuAction::QuickExportLayer));
        }
        Some(DialogOutcome::Cancelled) => WINDOW.with(|w| *w.borrow_mut() = None),
        _ => {}
    }
}

/// W18-I: whether the Quick Export window is open.
#[cfg(test)]
pub(crate) fn window_is_open() -> bool {
    WINDOW.with(|w| w.borrow().is_some())
}

/// W18-I: a test's stand-in for the window's Export… press: `choice` (or
/// the open window's current one) parked for the next pick.
#[cfg(test)]
pub(crate) fn confirm_for_test(choice: Option<QuickExportChoice>) {
    let open = WINDOW.with(|w| w.borrow_mut().take()).map(|d| d.choice());
    CONFIRMED.with(|c| c.set(choice.or(open)));
}

/// Quick Export with `choice`: ask where (the Export picker, suggesting the
/// format's extension and the scale's `@Nx` suffix), write the selected
/// layers (or the active one) alone, say so.
fn export_with(editor: &mut Editor, choice: QuickExportChoice) -> Result<String, String> {
    let svg = choice.format == QuickExportFormat::Svg;
    let extension = choice.format.extension();
    // An SVG is resolution-free and written at canvas size.
    let scale = if svg { 1.0 } else { choice.scale };
    let scaled = (scale - 1.0).abs() > f32::EPSILON;
    let suffix = if scaled {
        format!("@{}", ui::panels::quick_export_w18::scale_label(scale))
    } else {
        String::new()
    };
    let (ids, names, suggested) = {
        let doc = editor
            .active()
            .ok_or_else(|| "No document is open".to_string())?;
        let ids = export_layers(&doc.document)
            .ok_or_else(|| "Quick Export needs an active layer".to_string())?;
        let names: Vec<String> = ids
            .iter()
            .map(|id| {
                doc.document
                    .layers
                    .get(*id)
                    .map(|l| l.name.clone())
                    .unwrap_or_default()
            })
            .collect();
        let beside = doc.suggested_export_path();
        let stem = beside
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = if ids.len() == 1 {
            format!(
                "{stem}-{}{suffix}.{extension}",
                crate::editor::safe_file_name(&names[0])
            )
        } else {
            format!("{stem}{suffix}.{extension}")
        };
        (ids, names, beside.with_file_name(file))
    };
    let Some(picked) = editor.pick_export_path(&suggested) else {
        return Err("Quick Export: no destination chosen".to_string());
    };
    // The window's format decides the file, whatever the name was typed as.
    let picked = picked.with_extension(extension);
    let picked_stem = picked
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (base, _) = scale_suffix(&picked_stem);
    let targets: Vec<std::path::PathBuf> = if ids.len() == 1 {
        vec![picked.clone()]
    } else {
        names
            .iter()
            .map(|name| {
                picked.with_file_name(format!(
                    "{base}-{}{suffix}.{extension}",
                    crate::editor::safe_file_name(name)
                ))
            })
            .collect()
    };
    for (id, target) in ids.iter().zip(&targets) {
        let doc = editor
            .active()
            .ok_or_else(|| "No document is open".to_string())?;
        if svg {
            let staged = staged_alone(&doc.document, &[*id]);
            crate::doc::write_vector_svg(&staged, &doc.tiles, target)
                .map_err(|e| format!("Quick Export: {e}"))?;
            continue;
        }
        let (w, h, rgba) = layer_alone_rgba8(doc, *id)?;
        let (w, h, rgba) = if scaled {
            let tw = ((w as f32 * scale).round() as u32).max(1);
            let th = ((h as f32 * scale).round() as u32).max(1);
            let space = &doc.document.meta.color_space;
            let linear = raster::linear_from_rgba8(w, h, &rgba, space)
                .map_err(|e| format!("Quick Export: {e}"))?;
            let resampled = raster::resample(&linear, tw, th, raster::ResampleFilter::Lanczos3)
                .map_err(|e| format!("Quick Export: {e}"))?;
            let out = raster::rgba8_from_linear(&resampled, space)
                .map_err(|e| format!("Quick Export: {e}"))?;
            (tw, th, out)
        } else {
            (w, h, rgba)
        };
        raster::encode_to_path(
            target,
            raster::ExportFormat::Png,
            w,
            h,
            raster::EncodedPixels::Rgba8(&rgba),
            &raster::EncodeOptions::default(),
        )
        .map_err(|e| format!("Quick Export: {e}"))?;
    }
    // The bridge puts the returned line on the status bar.
    Ok(if ids.len() == 1 {
        format!(
            "Quick Export: layer \"{}\" written to {}",
            names[0],
            targets[0].display()
        )
    } else {
        format!(
            "Quick Export: {} layers written beside {}",
            ids.len(),
            targets[0].display()
        )
    })
}
