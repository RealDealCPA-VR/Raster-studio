//! W18-I: the application half of three panel gaps.
//!
//! * **Brushes ▸ Export as .ABR** writes each brush's **name and dynamics**
//!   (the `desc` section, [`asset_store::abr::write_abr_presets`]) after its
//!   sampled tip, where W16-E's export wrote the tips alone.
//! * **Styles ▸ Export as .ASL** writes each style's **Blending Options**
//!   object and the **patterns** its Pattern Overlay and pattern stroke name
//!   ([`asset_store::asl::write_asl_library`]), where W16-E's export wrote
//!   the effects alone and named a pattern overlay as not written.
//! * The Swatches list's **folders** (W16-E, egui memory only) are kept in
//!   `swatch-folders.json` beside the presets file: read on the first frame,
//!   written whenever they change.
//!
//! [`poll`] takes the two export requests off the panel-menu queue before
//! W16-E's poller sees it (the same per-frame hook,
//! [`crate::menu_bridge::context`]); every other request is handed to
//! `panel_menus_w16::perform` unchanged. [`sync_swatch_folders`] runs from
//! [`crate::menu_bridge::draw`], the per-frame hook that holds the context.
//!
//! A style preset here is an effect block only (its blending options are
//! not kept when it is defined or imported), so the object is written at
//! Photoshop's defaults: Normal, 100% opacity, 100% fill.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::panels::panel_menus_w16::{self as menus, Library, PanelRequest, SwatchFolder};

use crate::editor::Editor;

/// Carry out the queued panel requests: the two exports here, the rest by
/// W16-E's own handler. Each outcome goes on the status line.
pub(crate) fn poll(editor: &mut Editor) {
    for request in menus::take_requests() {
        let outcome = match request {
            PanelRequest::ExportBrushes(brushes) => export_brushes(editor, &brushes).map(Some),
            PanelRequest::ExportStyles(index) => export_styles(editor, index).map(Some),
            other => crate::menu_bridge::panel_menus_w16::perform(editor, other),
        };
        match outcome {
            Ok(Some(message)) | Err(message) => editor.set_status(message),
            Ok(None) => {}
        }
    }
}

/// Ask where to write a `library` file, suggesting `stem.ext`; the library's
/// own extension is enforced, as W16-E's export does.
fn pick(editor: &mut Editor, library: Library, stem: &str) -> Result<PathBuf, String> {
    let ext = library.extension();
    let suggested = PathBuf::from(format!("{stem}.{ext}"));
    let Some(mut path) = editor.pick_save_path(&suggested) else {
        return Err("Export cancelled".to_string());
    };
    let typed = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if typed.as_deref() != Some(ext) {
        path.set_extension(ext);
    }
    Ok(path)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// One brush as an `.abr` preset: its tip plane, its name and every setting
/// the format carries (diameter, angle, roundness, spacing, hardness, and
/// the Shape Dynamics / Scattering / Transfer jitters).
pub(crate) fn abr_preset(
    name: &str,
    settings: &tools::BrushSettings,
) -> Option<asset_store::abr::AbrPreset> {
    let tip = crate::menu_bridge::panel_menus_w16::tip_plane(settings)?;
    let d = &settings.dynamics;
    let mut preset = asset_store::abr::AbrPreset::new(name, tip);
    preset.diameter = settings.size;
    preset.angle_deg = settings.angle.to_degrees();
    preset.roundness = settings.roundness;
    preset.spacing = settings.spacing;
    preset.hardness = settings.hardness;
    preset.dynamics = asset_store::abr::AbrDynamics {
        size_jitter: d.size_jitter,
        min_diameter: d.min_diameter,
        angle_jitter: d.angle_jitter,
        roundness_jitter: d.roundness_jitter,
        min_roundness: d.min_roundness,
        scatter: d.scatter,
        scatter_both_axes: d.scatter_both_axes,
        count: d.count,
        count_jitter: d.count_jitter,
        opacity_jitter: d.opacity_jitter,
        flow_jitter: d.flow_jitter,
    };
    Some(preset)
}

/// Brushes ▸ Export as .ABR, with names and dynamics.
fn export_brushes(
    editor: &mut Editor,
    brushes: &[(String, tools::BrushSettings)],
) -> Result<String, String> {
    let presets: Vec<_> = brushes
        .iter()
        .filter_map(|(name, s)| abr_preset(name, s))
        .collect();
    if presets.is_empty() {
        return Err("There are no brushes to export".to_string());
    }
    let skipped = brushes.len() - presets.len();
    let bytes = asset_store::abr::write_abr_presets(&presets).map_err(|e| e.to_string())?;
    let path = pick(editor, Library::Brushes, "Brushes")?;
    crate::doc::write_atomically(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut message = format!(
        "Exported {} to {}",
        plural(presets.len(), "brush", "brushes"),
        path.display()
    );
    if skipped > 0 {
        message.push_str(&format!(
            " ({skipped} whose sampled tip is no longer loaded were left out)"
        ));
    }
    Ok(message)
}

/// One style's `Styl` descriptor (from its version word) with its Blending
/// Options, the patterns it names, and the effect kinds not written.
fn style_descriptor(
    effects: &layer_model::LayerEffects,
) -> Option<(Vec<u8>, Vec<psd::pattern::PsdPattern>, Vec<String>)> {
    let (lfx2, unmapped, patterns) = psd::effects::export_effects_with_patterns(effects)?;
    let opts = psd::ReadOptions::default();
    let mut cur = psd::bytes::Cursor::new(lfx2.get(8..)?);
    let lefx = psd::Descriptor::read(&mut cur, &opts).ok()?;
    let mut styl = psd::Descriptor::new("Styl");
    styl.push("Lefx", psd::Value::Descriptor(lefx)).ok()?;
    let mut sink = psd::bytes::Sink::new();
    sink.u32(16);
    styl.write(&mut sink).ok()?;
    let with_options = asset_store::asl::with_blend_options(
        &sink.into_inner(),
        &asset_store::asl::AslBlendOptions::default(),
    )
    .ok()?;
    Some((with_options, patterns, unmapped))
}

/// Styles ▸ Export as .ASL: style `index`, or every style, with Blending
/// Options and patterns.
fn export_styles(editor: &mut Editor, index: Option<usize>) -> Result<String, String> {
    let all = crate::menu_bridge::asl_import::style_presets(editor.presets());
    let chosen: Vec<(String, layer_model::LayerEffects)> = match index {
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
    let mut patterns: Vec<psd::pattern::PsdPattern> = Vec::new();
    let mut left_out: Vec<String> = Vec::new();
    for (name, effects) in &chosen {
        match style_descriptor(effects) {
            Some((bytes, used, unmapped)) => {
                left_out.extend(unmapped.into_iter().map(|k| format!("{name}: {k}")));
                for pattern in used {
                    if !patterns.iter().any(|p| p.id == pattern.id) {
                        patterns.push(pattern);
                    }
                }
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
    let bytes = asset_store::asl::write_asl_library(&triples, &patterns);
    let path = pick(editor, Library::Styles, &stem)?;
    crate::doc::write_atomically(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut message = format!(
        "Exported {} to {}",
        plural(encoded.len(), "style", "styles"),
        path.display()
    );
    if !patterns.is_empty() {
        message.push_str(&format!(
            " with {}",
            plural(patterns.len(), "pattern", "patterns")
        ));
    }
    if !left_out.is_empty() {
        message.push_str(&format!(" (not written: {})", left_out.join("; ")));
    }
    Ok(message)
}

// ---------------------------------------------------------------------------
// Swatch folders, kept across sessions
// ---------------------------------------------------------------------------

/// A swatch folder as the file keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct SavedFolder {
    name: String,
    open: bool,
    members: Vec<[u8; 4]>,
}

impl Default for SavedFolder {
    fn default() -> Self {
        Self {
            name: String::new(),
            open: true,
            members: Vec::new(),
        }
    }
}

/// Where the Swatches list's folders are kept: beside the presets file.
pub(crate) fn swatch_folders_file(editor: &Editor) -> PathBuf {
    editor
        .paths()
        .presets_file()
        .with_file_name("swatch-folders.json")
}

fn saved_key() -> egui::Id {
    egui::Id::new("raster-w18-swatch-folders-saved")
}

fn to_saved(folders: &[SwatchFolder]) -> Vec<SavedFolder> {
    folders
        .iter()
        .map(|f| SavedFolder {
            name: f.name.clone(),
            open: f.open,
            members: f.members.clone(),
        })
        .collect()
}

/// Once a frame: on the first, the folders the file keeps become the
/// Swatches list's (a file that does not parse is left alone and read as
/// none); on every later one, folders that changed are written back.
pub(crate) fn sync_swatch_folders(ctx: &egui::Context, editor: &Editor) {
    let file = swatch_folders_file(editor);
    let current = to_saved(&menus::swatch_folders(ctx));
    let saved: Option<Vec<SavedFolder>> = ctx.data(|d| d.get_temp(saved_key()));
    match saved {
        None => {
            let loaded: Vec<SavedFolder> = std::fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            if current.is_empty() && !loaded.is_empty() {
                menus::set_swatch_folders(
                    ctx,
                    loaded
                        .iter()
                        .map(|f| SwatchFolder {
                            name: f.name.clone(),
                            open: f.open,
                            members: f.members.clone(),
                        })
                        .collect(),
                );
                ctx.data_mut(|d| d.insert_temp(saved_key(), loaded));
            } else {
                ctx.data_mut(|d| d.insert_temp(saved_key(), current));
            }
        }
        Some(saved) if saved != current => {
            if let Ok(json) = serde_json::to_vec_pretty(&current) {
                if let Some(dir) = file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if crate::doc::write_atomically(&file, &json).is_ok() {
                    ctx.data_mut(|d| d.insert_temp(saved_key(), current));
                }
            }
        }
        Some(_) => {}
    }
}

#[cfg(test)]
#[path = "library_w18_tests.rs"]
mod tests;
