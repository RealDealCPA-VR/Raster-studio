//! W8-C: File ▸ Export ▸ Artboards to Files…: one image per artboard.
//!
//! Every artboard of the active document ([`layer_model::artboard::artboards`],
//! the groups whose bottom child is a background plate) is composited **on
//! its own** — every layer that is neither the artboard, inside it, nor one of
//! its ancestors is hidden for the render — over the artboard's rect mapped
//! into the document through the artboard group's transform. The artboard's
//! plate supplies its background and the compositor clips its children to
//! the rect, so each file is exactly what the artboard shows, including any
//! part that runs off the canvas.
//!
//! Files are named `<document>_<artboard>.<ext>` in panel order (a name used
//! twice gets `_2`, `_3`, …) and written with the first row of the last
//! confirmed Export As job, exactly as File ▸ Export ▸ Slices… does
//! ([`crate::slices_export`]); a plain PNG at 100% before any Export As.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use raster::PixelRect;

use crate::doc::OpenDocument;
use crate::editor::Editor;

/// File ▸ Export ▸ Artboards to Files…: ask for a folder and write every
/// artboard of the active document into it.
pub fn export_artboards(editor: &mut Editor) -> Result<String, String> {
    let doc = editor.active().ok_or("No document is open")?;
    if layer_model::artboard::artboards(&doc.document.layers).is_empty() {
        return Err(
            "Export Artboards: this document has no artboards - draw one with the Artboard tool"
                .to_string(),
        );
    }
    let Some(dir) = editor.pick_export_folder() else {
        return Err("Export Artboards: no destination chosen".to_string());
    };
    let entry = ui::dialogs::export_as::last_confirmed_entry();
    let doc = editor.active_mut().ok_or("No document is open")?;
    let written = write_artboards(doc, &entry.preset, &dir)?;
    Ok(format!(
        "Exported {} artboard(s) as {} to {}",
        written.len(),
        entry.preset.format.extension().to_uppercase(),
        dir.display()
    ))
}

/// The document-space rect `group`'s artboard covers: its rect (in the
/// group's own space) through the group's document transform, rounded out.
pub fn artboard_document_rect(
    doc: &editor_core::Document,
    group: layer_model::LayerId,
    board: &layer_model::Artboard,
) -> Option<PixelRect> {
    let t = crate::interaction_geometry::document_transform_of(doc, group, 0).ok()?;
    let (x0, y0) = (board.x as f32, board.y as f32);
    let (x1, y1) = (x0 + board.width as f32, y0 + board.height as f32);
    let corners = [
        glam::Vec2::new(x0, y0),
        glam::Vec2::new(x1, y0),
        glam::Vec2::new(x1, y1),
        glam::Vec2::new(x0, y1),
    ]
    .map(|p| t.transform_point2(p));
    let lo = corners
        .iter()
        .fold(glam::Vec2::splat(f32::INFINITY), |a, p| a.min(*p));
    let hi = corners
        .iter()
        .fold(glam::Vec2::splat(f32::NEG_INFINITY), |a, p| a.max(*p));
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let (lx, ly) = (lo.x.floor() as i64, lo.y.floor() as i64);
    let (hx, hy) = (hi.x.ceil() as i64, hi.y.ceil() as i64);
    (hx > lx && hy > ly).then(|| PixelRect::new(lx, ly, (hx - lx) as u32, (hy - ly) as u32))
}

/// A file-name-safe version of an artboard's name.
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Artboard".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Write every artboard of `doc` into `dir` with `preset`'s format and
/// settings. Returns the paths written, in panel order.
pub fn write_artboards(
    doc: &mut OpenDocument,
    preset: &raster::ExportPreset,
    dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    let boards = layer_model::artboard::artboards(&doc.document.layers);
    let stem = Path::new(doc.title())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| doc.title().to_string());
    let space = doc.document.meta.color_space.clone();
    let metadata = raster::export::ExportMetadata {
        icc_profile: None,
        icc_profile_space: None,
    };
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut written = Vec::with_capacity(boards.len());
    for (group, board) in boards {
        let name = doc
            .document
            .layers
            .get(group)
            .map(|l| safe_name(&l.name))
            .unwrap_or_else(|| "Artboard".to_string());
        let rect = artboard_document_rect(&doc.document, group, &board)
            .ok_or_else(|| format!("Export Artboards: '{name}' has no area"))?;
        let rgba = artboard_pixels(doc, group, rect)?;
        let image = raster::export::linear_from_rgba8(rect.width, rect.height, &rgba, &space)
            .map_err(|e| format!("Export Artboards: '{name}': {e}"))?;
        let count = used.entry(name.clone()).or_insert(0);
        *count += 1;
        let mut named = preset.clone();
        named.name = if *count == 1 {
            format!("{stem}_{name}")
        } else {
            format!("{stem}_{name}_{count}")
        };
        let paths = raster::export::export_batch_to_dir(dir, &image, &[named], &metadata)
            .map_err(|e| format!("Export Artboards: '{name}': {e}"))?;
        written.extend(paths);
    }
    Ok(written)
}

/// `group` composited alone over `rect` (document pixels), as straight sRGB
/// bytes: every layer outside the artboard's own branch is hidden.
pub fn artboard_pixels(
    doc: &OpenDocument,
    group: layer_model::LayerId,
    rect: PixelRect,
) -> Result<Vec<u8>, String> {
    let mut staged = doc.document.clone();
    let tree = &doc.document.layers;
    let mut ancestors = Vec::new();
    let mut up = tree.parent_of(group);
    while let Some(p) = up {
        ancestors.push(p);
        up = tree.parent_of(p);
    }
    let inside = |id: layer_model::LayerId| {
        let mut at = Some(id);
        while let Some(a) = at {
            if a == group {
                return true;
            }
            at = tree.parent_of(a);
        }
        false
    };
    for id in tree.iter_depth_first() {
        if inside(id) || ancestors.contains(&id) {
            continue;
        }
        if let Some(l) = staged.layers.get_mut(id) {
            l.visible = false;
        }
    }
    let canvas = compositor::composite_region(
        &staged,
        &doc.tiles,
        rect,
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| format!("Export Artboards: {e}"))?;
    Ok(canvas.to_rgba8(&doc.document.meta.color_space))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    #[test]
    fn with_no_artboards_the_export_refuses_before_asking_for_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new().exporting_folder(&out)),
        );
        let path = dir.path().join("plain.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 4, 4, &[255u8; 64]).unwrap(),
        )
        .unwrap();
        ed.open_path(&path).unwrap();
        let reason = export_artboards(&mut ed).unwrap_err();
        assert!(reason.contains("no artboards"), "{reason}");
        assert!(!out.exists());
    }

    #[test]
    fn names_are_made_file_safe() {
        assert_eq!(safe_name("Hero / Mobile"), "Hero _ Mobile");
        assert_eq!(safe_name("  "), "Artboard");
    }
}
