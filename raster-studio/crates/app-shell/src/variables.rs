//! W10-E: Image ▸ Variables — data-driven graphics.
//!
//! The definitions (a text-replacement or visibility variable bound to a
//! layer) and the data sets (one value per variable, imported from a CSV)
//! are `ui::dialogs::variables`'s types; this module keeps them with each
//! open document, previews a set on the document and exports one file per
//! set.
//!
//! * **Preview** applies one set to the live document as ONE undo step: a
//!   `SetLayerKind` per text variable (the layer's text replaced; its styled
//!   ranges and kerning, which index the old text's bytes, are dropped so the
//!   base style runs over the new text) and a `SetLayerProperties` per
//!   visibility variable.
//! * **Export** never touches the live document: for each set a copy of the
//!   document with that set applied ([`staged_document`]) is flattened
//!   through the compositor and written as `<document>_<set>.<ext>` into a
//!   folder the user picks.
//!
//! # Kept for the session
//!
//! As with File Info (`crate::file_extras`), `editor_core::Document` has no
//! field for variables and this wave does not own it, so definitions and data
//! sets are kept per open document, by [`DocumentId`], for as long as the
//! document is open; a `.rstudio` save does not carry them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use editor_core::{Command, Document, LayerPatch};
use layer_model::LayerKind;
use ui::dialogs::{
    DataSet, VariableDef, VariableKind, VariableLayer, VariablesDialog, VariablesPage,
    VariablesRequest, VariablesSpec,
};

use crate::doc::{DocumentId, OpenDocument};
use crate::editor::Editor;

/// One document's definitions and data sets.
pub type Stored = (Vec<VariableDef>, Vec<DataSet>);

thread_local! {
    static STORE: RefCell<HashMap<DocumentId, Stored>> = RefCell::new(HashMap::new());
}

/// The definitions and data sets kept for `id`.
pub fn stored(id: DocumentId) -> Stored {
    STORE.with(|s| s.borrow().get(&id).cloned().unwrap_or_default())
}

/// The Variables dialog over the active document, on `page`.
pub fn dialog(editor: &Editor, page: VariablesPage) -> Option<VariablesDialog> {
    let open = editor.active()?;
    let tree = &open.document.layers;
    // Top of the stack first, as the Layers panel lists them (the tree's
    // depth-first order is top-most first).
    let layers: Vec<VariableLayer> = tree
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| {
            let layer = tree.get(id)?;
            Some(VariableLayer {
                layer: id,
                name: layer.name.clone(),
                is_text: matches!(layer.kind, LayerKind::Text(_)),
            })
        })
        .collect();
    let (defs, sets) = stored(open.id());
    Some(VariablesDialog::new(page, layers, &defs, sets))
}

/// Keep a confirmed Variables dialog's definitions and sets with the active
/// document, then do what it asked.
pub fn perform(editor: &mut Editor, spec: VariablesSpec) -> Result<String, String> {
    let id = editor.active().ok_or("No document is open")?.id();
    STORE.with(|s| {
        s.borrow_mut()
            .insert(id, (spec.defs.clone(), spec.sets.clone()))
    });
    match spec.request {
        VariablesRequest::Save => {
            let message = format!(
                "Variables: {} defined, {} data set(s)",
                spec.defs.len(),
                spec.sets.len()
            );
            editor.set_status(message.clone());
            Ok(message)
        }
        VariablesRequest::Preview(index) => {
            let set = spec
                .sets
                .get(index)
                .ok_or("Variables: that data set no longer exists")?;
            let command = {
                let doc = &editor.active().ok_or("No document is open")?.document;
                preview_command(doc, &spec.defs, set)?
            };
            let before = editor.active().map_or(0, |d| d.history_depth());
            editor.apply_command(command);
            if editor.active().map_or(0, |d| d.history_depth()) == before {
                return Err(format!(
                    "Variables: {} changes nothing on this document",
                    set.name
                ));
            }
            let message = format!("Variables: previewing {}", set.name);
            editor.set_status(message.clone());
            Ok(message)
        }
        VariablesRequest::Export(format) => {
            if spec.sets.is_empty() {
                return Err("Variables: import a CSV of data sets first".to_string());
            }
            let Some(dir) = editor.pick_export_folder() else {
                return Err("Variables: no destination chosen".to_string());
            };
            let open = editor.active().ok_or("No document is open")?;
            let written =
                export_sets(open, &spec.defs, &spec.sets, format.export_format(90), &dir)?;
            let message = format!(
                "Variables: exported {} data set(s) to {}",
                written.len(),
                dir.display()
            );
            editor.set_status(message.clone());
            Ok(message)
        }
    }
}

/// `layer`'s text layer with its text replaced by `text` — the styled ranges
/// and kerning, which index the old text's bytes, dropped.
fn replaced_text(layer: &layer_model::Layer, text: &str) -> Option<LayerKind> {
    let LayerKind::Text(t) = &layer.kind else {
        return None;
    };
    let mut t = t.clone();
    t.text = text.to_string();
    t.spans.clear();
    t.kerning.clear();
    Some(LayerKind::Text(t))
}

/// A copy of `document` with `set` applied: every text variable's layer
/// holding the set's text, every visibility variable's layer shown or
/// hidden. Variables the set does not name are left as they are.
pub fn staged_document(document: &Document, defs: &[VariableDef], set: &DataSet) -> Document {
    let mut staged = document.clone();
    for def in defs {
        let Some(value) = set.value(&def.name) else {
            continue;
        };
        let Some(layer) = staged.layers.get_mut(def.layer) else {
            continue;
        };
        match def.kind {
            VariableKind::Text => {
                if let Some(kind) = replaced_text(layer, value) {
                    layer.kind = kind;
                }
            }
            VariableKind::Visibility => {
                layer.visible = ui::dialogs::variables::visibility_value(value)
            }
        }
    }
    staged
}

/// The one undoable step that applies `set` to `document`.
pub fn preview_command(
    document: &Document,
    defs: &[VariableDef],
    set: &DataSet,
) -> Result<Command, String> {
    let mut commands = Vec::new();
    for def in defs {
        let Some(value) = set.value(&def.name) else {
            continue;
        };
        let Some(layer) = document.layers.get(def.layer) else {
            return Err(format!(
                "Variables: the layer {} is bound to is gone",
                def.name
            ));
        };
        match def.kind {
            VariableKind::Text => {
                if let Some(kind) = replaced_text(layer, value) {
                    commands.push(Command::SetLayerKind {
                        layer_id: def.layer,
                        kind: Box::new(kind),
                    });
                }
            }
            VariableKind::Visibility => {
                let visible = ui::dialogs::variables::visibility_value(value);
                if layer.visible != visible {
                    commands.push(Command::SetLayerProperties {
                        layer_id: def.layer,
                        patch: LayerPatch {
                            visible: Some(visible),
                            ..LayerPatch::default()
                        },
                    });
                }
            }
        }
    }
    Ok(Command::Transaction {
        label: format!("Apply {}", set.name),
        commands,
    })
}

/// Write one flattened file per data set into `dir`, named
/// `<document>_<set>.<ext>`. Returns the paths, in set order.
pub fn export_sets(
    open: &OpenDocument,
    defs: &[VariableDef],
    sets: &[DataSet],
    format: raster::ExportFormat,
    dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    let document = &open.document;
    let (w, h) = (document.width(), document.height());
    let rect = raster::PixelRect::new(0, 0, w, h);
    let stem = Path::new(open.title())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| open.title().to_string());
    let metadata = raster::export::ExportMetadata {
        icc_profile: None,
        icc_profile_space: None,
    };
    let mut written = Vec::with_capacity(sets.len());
    for set in sets {
        let staged = staged_document(document, defs, set);
        let canvas = compositor::composite_region(
            &staged,
            &open.tiles,
            rect,
            0,
            compositor::CompositeOptions::default(),
        )
        .map_err(|e| format!("Variables: {}: {e}", set.name))?;
        let rgba = canvas.to_rgba8(&document.meta.color_space);
        let image = raster::export::linear_from_rgba8(w, h, &rgba, &document.meta.color_space)
            .map_err(|e| format!("Variables: {}: {e}", set.name))?;
        let mut preset =
            raster::ExportPreset::new("set", format).for_color_mode(document.meta.color_mode);
        preset.name = format!("{stem}_{}", set.name);
        let paths = raster::export::export_batch_to_dir(dir, &image, &[preset], &metadata)
            .map_err(|e| format!("Variables: {}: {e}", set.name))?;
        written.extend(paths);
    }
    Ok(written)
}
