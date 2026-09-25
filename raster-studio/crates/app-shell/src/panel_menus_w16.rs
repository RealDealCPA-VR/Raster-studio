//! W16-E: the application half of the panel menus
//! ([`ui::panels::panel_menus_w16`]).
//!
//! The panels post [`PanelRequest`]s for the work only the application can
//! do: a file picker, the style preset store, the history, the document
//! camera, a pass over the composite's pixels. [`poll`] takes the queue once
//! a frame (from [`crate::menu_bridge::context`], the per-frame hook the
//! job pollers already use) and says on the status line what happened;
//! [`publish`] hands the Navigator the document camera's angle.
//!
//! What each request writes, the existing importers read back: the swatches
//! as an `.aco` File > Open adds to the Swatches panel, the brushes as a
//! sampled `.abr` it adds to the Brushes panel, the styles as an `.asl` it
//! adds to the style presets.

use std::path::{Path, PathBuf};

use asset_store::abr::AbrBrush;
use editor_core::{Command, Selection, SelectionMask};
use glam::IVec2;
use layer_model::LayerEffects;
use ui::panels::channels::ChannelKind;
use ui::panels::panel_menus_w16::{self as menus, Library, PanelRequest};

use crate::action::Action;
use crate::editor::Editor;

/// Carry out every request the panels queued since the last frame. Each
/// one's outcome, success or refusal, is put on the status line.
pub(crate) fn poll(editor: &mut Editor) {
    for request in menus::take_requests() {
        match perform(editor, request) {
            Ok(Some(message)) | Err(message) => editor.set_status(message),
            Ok(None) => {}
        }
    }
}

/// Hand the Navigator's Angle field the active camera's rotation.
pub(crate) fn publish(ctx: &egui::Context, editor: &Editor) {
    let degrees = editor
        .active()
        .map(|d| d.camera.rotation.to_degrees())
        .filter(|d| d.is_finite())
        .unwrap_or(0.0);
    let wrapped = ui::panels::navigator::parse_angle(&degrees.to_string()).unwrap_or(0.0);
    menus::publish_view_angle(ctx, wrapped);
}

/// One request. `Ok(None)` leaves the status line to whatever route ran
/// (File > Open's own import message, say).
pub(crate) fn perform(
    editor: &mut Editor,
    request: PanelRequest,
) -> Result<Option<String>, String> {
    match request {
        PanelRequest::OpenLibrary(_) => match editor.dispatch(Action::Open) {
            Ok(_) => Ok(None),
            Err(crate::editor::ActionError::Cancelled(_)) => Ok(None),
            Err(e) => Err(e.to_string()),
        },
        PanelRequest::ExportSwatches(swatches) => export_swatches(editor, &swatches).map(Some),
        PanelRequest::ExportBrushes(brushes) => export_brushes(editor, &brushes).map(Some),
        PanelRequest::ExportStyles(index) => export_styles(editor, index).map(Some),
        PanelRequest::RenameStyle { index, name } => rename_style(editor, index, &name).map(Some),
        PanelRequest::DeleteStyle(index) => delete_style(editor, index).map(Some),
        PanelRequest::NewAlphaChannel => new_alpha_channel(editor).map(Some),
        PanelRequest::DeleteAlphaChannel(index) => delete_alpha_channel(editor, index).map(Some),
        PanelRequest::LoadChannelSelection(kind) => load_channel(editor, kind).map(Some),
        PanelRequest::ClearHistory => clear_history(editor).map(Some),
        PanelRequest::SetViewAngle(degrees) => {
            let doc = editor.active_mut().ok_or("No document is open")?;
            doc.camera.set_rotation(degrees.to_radians());
            Ok(None)
        }
    }
}

/// Ask where to write a `library` file, suggesting `stem.ext`; the picked
/// path gains the extension when it has none.
fn pick(editor: &mut Editor, library: Library, stem: &str) -> Result<PathBuf, String> {
    let ext = library.extension();
    let suggested = PathBuf::from(format!("{stem}.{ext}"));
    let Some(mut path) = editor.pick_save_path(&suggested) else {
        return Err("Export cancelled".to_string());
    };
    // The save picker is the application's generic one (its filter names
    // the project format), so the library's own extension is enforced here:
    // whatever was typed, the file is written as what it is.
    let typed = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if typed.as_deref() != Some(ext) {
        path.set_extension(ext);
    }
    Ok(path)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::doc::write_atomically(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Export as .ACO.
fn export_swatches(editor: &mut Editor, swatches: &[(String, [f32; 4])]) -> Result<String, String> {
    if swatches.is_empty() {
        return Err("There are no swatches to export".to_string());
    }
    let path = pick(editor, Library::Swatches, "Swatches")?;
    write(&path, &asset_store::resources::aco::write(swatches))?;
    Ok(format!(
        "Exported {} to {}",
        plural(swatches.len(), "swatch", "swatches"),
        path.display()
    ))
}

/// A brush's tip as an 8-bit coverage plane: a sampled tip's own pixels, or
/// the computed round tip drawn at its diameter with its hardness,
/// roundness and angle (an `.abr` stores sampled tips only).
pub(crate) fn tip_plane(settings: &tools::BrushSettings) -> Option<AbrBrush> {
    if let tools::brush::BrushTip::Sampled(id) = settings.tip {
        let tip = tools::brush::sampled_tip(id)?;
        return Some(AbrBrush {
            width: tip.width(),
            height: tip.height(),
            alpha8: tip.alpha().to_vec(),
        });
    }
    let side = settings
        .size
        .round()
        .clamp(1.0, asset_store::abr::MAX_SIDE as f32) as u32;
    let r = side as f32 / 2.0;
    let hardness = settings.hardness.clamp(0.0, 1.0);
    let minor = settings.roundness.clamp(0.05, 1.0);
    let (sin, cos) = settings.angle.sin_cos();
    let mut alpha8 = Vec::with_capacity((side * side) as usize);
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (x as f32 + 0.5 - r, y as f32 + 0.5 - r);
            // Into the tip's own frame: unrotate, then unsquash the minor axis.
            let (u, v) = (dx * cos + dy * sin, (-dx * sin + dy * cos) / minor);
            let d = (u * u + v * v).sqrt() / r.max(f32::EPSILON);
            let a = if settings.aliased {
                if d <= 1.0 {
                    1.0
                } else {
                    0.0
                }
            } else if d <= hardness {
                1.0
            } else if d >= 1.0 {
                0.0
            } else {
                1.0 - (d - hardness) / (1.0 - hardness).max(f32::EPSILON)
            };
            alpha8.push((a * 255.0).round() as u8);
        }
    }
    Some(AbrBrush {
        width: side,
        height: side,
        alpha8,
    })
}

/// Export as .ABR.
fn export_brushes(
    editor: &mut Editor,
    brushes: &[(String, tools::BrushSettings)],
) -> Result<String, String> {
    let tips: Vec<AbrBrush> = brushes.iter().filter_map(|(_, s)| tip_plane(s)).collect();
    if tips.is_empty() {
        return Err("There are no brushes to export".to_string());
    }
    let skipped = brushes.len() - tips.len();
    let bytes = asset_store::abr::write_abr(&tips).map_err(|e| e.to_string())?;
    let path = pick(editor, Library::Brushes, "Brushes")?;
    write(&path, &bytes)?;
    let mut message = format!(
        "Exported {} to {}",
        plural(tips.len(), "brush", "brushes"),
        path.display()
    );
    if skipped > 0 {
        message.push_str(&format!(
            " ({skipped} whose sampled tip is no longer loaded were left out)"
        ));
    }
    Ok(message)
}

/// The style presets as `(name, effects)`, in panel order (the ones whose
/// JSON parses, which is what the Styles panel lists).
fn styles(editor: &Editor) -> Vec<(String, LayerEffects)> {
    crate::menu_bridge::asl_import::style_presets(editor.presets())
}

/// One style's `Styl` descriptor bytes (from its version word), for
/// `asset_store::asl::write_asl`: the effects as `psd` writes a layer's
/// `lfx2` block, wrapped as the style's `Lefx` item. Answers the effect
/// kinds the writer could not express too.
fn style_descriptor(effects: &LayerEffects) -> Option<(Vec<u8>, Vec<String>)> {
    let (lfx2, unmapped) = psd::effects::export_effects(effects)?;
    // `lfx2`: a u32 object-effects version, a u32 descriptor version, then
    // the effects descriptor.
    let opts = psd::ReadOptions::default();
    let mut cur = psd::bytes::Cursor::new(lfx2.get(8..)?);
    let lefx = psd::Descriptor::read(&mut cur, &opts).ok()?;
    let mut styl = psd::Descriptor::new("Styl");
    styl.push("Lefx", psd::Value::Descriptor(lefx)).ok()?;
    let mut sink = psd::bytes::Sink::new();
    sink.u32(16);
    styl.write(&mut sink).ok()?;
    Some((sink.into_inner(), unmapped))
}

/// Export as .ASL: style `index`, or every style.
fn export_styles(editor: &mut Editor, index: Option<usize>) -> Result<String, String> {
    let all = styles(editor);
    let chosen: Vec<(String, LayerEffects)> = match index {
        Some(i) => vec![all
            .get(i)
            .cloned()
            .ok_or("That style is no longer in the list")?],
        None => all,
    };
    if chosen.is_empty() {
        return Err("There are no styles to export".to_string());
    }
    let mut encoded: Vec<(String, Vec<u8>)> = Vec::new();
    let mut left_out: Vec<String> = Vec::new();
    for (name, effects) in &chosen {
        match style_descriptor(effects) {
            Some((bytes, unmapped)) => {
                left_out.extend(unmapped.into_iter().map(|k| format!("{name}: {k}")));
                encoded.push((name.clone(), bytes));
            }
            None => left_out.push(format!("{name}: no effect this build writes")),
        }
    }
    if encoded.is_empty() {
        return Err(format!(
            "No style could be written ({})",
            left_out.join("; ")
        ));
    }
    let stem = match index {
        Some(_) => encoded[0].0.clone(),
        None => "Styles".to_string(),
    };
    let triples: Vec<(&str, &str, &[u8])> = encoded
        .iter()
        .map(|(n, b)| (n.as_str(), "", b.as_slice()))
        .collect();
    let bytes = asset_store::asl::write_asl(&triples);
    let path = pick(editor, Library::Styles, &stem)?;
    write(&path, &bytes)?;
    let mut message = format!(
        "Exported {} to {}",
        plural(encoded.len(), "style", "styles"),
        path.display()
    );
    if !left_out.is_empty() {
        message.push_str(&format!(" (not written: {})", left_out.join("; ")));
    }
    Ok(message)
}

/// The position in the stored list of the `index`th style the panel shows
/// (the panel leaves out a preset whose JSON does not parse).
fn stored_style_index(editor: &Editor, index: usize) -> Option<usize> {
    editor
        .presets()
        .styles()
        .iter()
        .enumerate()
        .filter(|(_, (_, json))| serde_json::from_str::<LayerEffects>(json).is_ok())
        .nth(index)
        .map(|(i, _)| i)
}

/// Rewrite the stored style list through the store's own serialized form
/// (the store has no remove or rename of its own), keeping every other
/// library it holds, and save it.
fn edit_styles(
    editor: &mut Editor,
    edit: impl FnOnce(&mut Vec<(String, String)>),
) -> Result<(), String> {
    let mut value = serde_json::to_value(editor.presets()).map_err(|e| e.to_string())?;
    let mut list: Vec<(String, String)> = editor.presets().styles().to_vec();
    edit(&mut list);
    value["styles"] = serde_json::to_value(&list).map_err(|e| e.to_string())?;
    let store: asset_store::presets::PresetStore =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    *editor.presets_mut() = store;
    let _ = editor.presets().save(&editor.paths().presets_file());
    Ok(())
}

/// The Styles panel's Name Change.
fn rename_style(editor: &mut Editor, index: usize, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A style needs a name".to_string());
    }
    let at = stored_style_index(editor, index).ok_or("That style is no longer in the list")?;
    let old = editor.presets().styles()[at].0.clone();
    if editor.presets().styles().iter().any(|(n, _)| n == name) && old != name {
        return Err(format!("A style is already called \u{201c}{name}\u{201d}"));
    }
    edit_styles(editor, |list| list[at].0 = name.to_string())?;
    Ok(format!(
        "Renamed the style \u{201c}{old}\u{201d} to \u{201c}{name}\u{201d}"
    ))
}

/// The Styles panel's Delete.
fn delete_style(editor: &mut Editor, index: usize) -> Result<String, String> {
    let at = stored_style_index(editor, index).ok_or("That style is no longer in the list")?;
    let old = editor.presets().styles()[at].0.clone();
    edit_styles(editor, |list| {
        list.remove(at);
    })?;
    Ok(format!("Deleted the style \u{201c}{old}\u{201d}"))
}

/// Channels > New: an empty (all black) alpha channel named like
/// Photopea's, "Alpha N".
fn new_alpha_channel(editor: &mut Editor) -> Result<String, String> {
    let doc = editor.active_mut().ok_or("No document is open")?;
    let names = crate::dialog_host::saved_selection_names(&doc.document);
    let name = ui::dialogs::selection_name::next_alpha_name(&names);
    // An empty rect, as an empty spot channel is stored: `Selection::None`
    // would mean "everything" to a reader that materialises it.
    let empty = Selection::Rect {
        min: IVec2::ZERO,
        max: IVec2::ZERO,
    };
    doc.document.saved_selections.push((name.clone(), empty));
    doc.document.mark_dirty();
    Ok(format!("Added the empty channel \u{201c}{name}\u{201d}"))
}

/// Channels > Delete on alpha channel `index`. An alpha channel open for
/// editing is closed first (its painting stored back), so no edit session
/// outlives its channel or points at the wrong one.
fn delete_alpha_channel(editor: &mut Editor, index: usize) -> Result<String, String> {
    let editing = editor
        .active()
        .ok_or("No document is open")?
        .document
        .extras
        .alpha_edit
        .is_some();
    if editing {
        crate::menu_bridge::perform(ui::menu::MenuAction::CloseAlphaChannel, editor)?;
    }
    let doc = editor.active_mut().ok_or("No document is open")?;
    if index >= doc.document.saved_selections.len() {
        return Err("That channel is no longer in the document".to_string());
    }
    let (name, _) = doc.document.saved_selections.remove(index);
    doc.document.mark_dirty();
    Ok(format!("Deleted the channel \u{201c}{name}\u{201d}"))
}

/// The coverage a colour channel loads as: the composite's luminosity
/// (Rec. 601 weights, as Photoshop's) for the composite row, one
/// component's values for a component row, each scaled by the pixel's
/// alpha so transparent pixels select nothing.
pub(crate) fn channel_coverage(rgba8: &[u8], kind: ChannelKind) -> Option<Vec<u8>> {
    // `None` weights: the composite's luminosity; `Some(i)`: component `i`.
    let component = match kind {
        ChannelKind::Composite => None,
        ChannelKind::Component(i) if i < 3 => Some(i),
        _ => return None,
    };
    Some(
        rgba8
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let value = match component {
                    None => {
                        0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])
                    }
                    Some(i) => f32::from(p[i]),
                };
                (value * f32::from(p[3]) / 255.0).round().clamp(0.0, 255.0) as u8
            })
            .collect(),
    )
}

/// Load a colour channel as the selection, one undoable `SetSelection`.
fn load_channel(editor: &mut Editor, kind: ChannelKind) -> Result<String, String> {
    let doc = editor.active_mut().ok_or("No document is open")?;
    let (w, h) = (doc.document.width(), doc.document.height());
    let rgba8 = doc
        .composite(raster::PixelRect::new(0, 0, w, h))
        .map_err(|e| e.to_string())?;
    let coverage =
        channel_coverage(&rgba8, kind).ok_or("That channel cannot be loaded as a selection")?;
    let mask = SelectionMask::new(IVec2::ZERO, w, h, coverage).map_err(|e| e.to_string())?;
    editor.apply_command(Command::SetSelection {
        selection: Selection::Mask(mask),
    });
    Ok(match kind {
        ChannelKind::Composite => "Loaded the composite's luminosity as the selection".to_string(),
        _ => "Loaded the channel as the selection".to_string(),
    })
}

/// History > Clear History: forget the active document's undo and redo
/// steps, keeping the document exactly as it is (Photopea's Clear History).
fn clear_history(editor: &mut Editor) -> Result<String, String> {
    let doc = editor.active_mut().ok_or("No document is open")?;
    let dropped = doc.history.purge();
    if dropped == 0 {
        return Err("There is no history to clear".to_string());
    }
    Ok(format!(
        "Cleared the history ({})",
        plural(dropped, "step", "steps")
    ))
}

#[cfg(test)]
#[path = "panel_menus_w16_tests.rs"]
mod tests;
