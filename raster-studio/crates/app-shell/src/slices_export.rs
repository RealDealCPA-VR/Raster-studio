//! File ▸ Export ▸ Slices…: one file per Slice-tool region.
//!
//! The Slice tool publishes its regions when the gesture is committed (Enter);
//! [`crate::tool_input`] hands them to [`remember_committed`], which keeps them
//! per document in the editor's [`SliceStore`] — the latest committed set
//! replaces the previous one, as a new slice set does in Photopea. File ▸
//! Export ▸ Slices… ([`export_slices`]) then asks for a folder once and writes
//! every region of the active document's set, cut from the full composite, as
//! `<document>_01.<ext>`, `<document>_02.<ext>`, … in slice order.
//!
//! The format and its settings (quality, scale, resampling, depth) are the
//! first row of the last Export As job the shell handed to the writer (the
//! folder picker answered) —
//! [`ui::dialogs::export_as::last_confirmed_entry`] — and a plain PNG at 100%
//! before any Export As has been written. Each file goes through the same
//! colour-managed exporter as Export As ([`raster::export::export_batch_to_dir`]),
//! so a slice is byte-for-byte what exporting that region alone would write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use raster::PixelRect;

use crate::doc::{DocumentId, OpenDocument};
use crate::editor::Editor;

/// The committed slice sets, per open document.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SliceStore {
    by_document: HashMap<DocumentId, Vec<PixelRect>>,
}

impl SliceStore {
    /// Replace `document`'s slice set. An empty set forgets it.
    pub fn remember(&mut self, document: DocumentId, rects: Vec<PixelRect>) {
        if rects.is_empty() {
            self.by_document.remove(&document);
        } else {
            self.by_document.insert(document, rects);
        }
    }

    /// `document`'s committed slice set, in slice order (empty when none).
    pub fn get(&self, document: DocumentId) -> &[PixelRect] {
        self.by_document
            .get(&document)
            .map_or(&[][..], Vec::as_slice)
    }
}

/// Keep a slice set the Slice tool just committed on the active document, and
/// return the sentence the status bar shows for it.
pub fn remember_committed(editor: &mut Editor, slices: &[tools::Slice]) -> String {
    let Some(id) = editor.active().map(OpenDocument::id) else {
        return format!("{} slice(s) defined, but no document is open", slices.len());
    };
    editor
        .slices
        .remember(id, slices.iter().map(|s| s.rect).collect());
    format!(
        "{} slice(s) defined; File > Export > Slices writes one file each",
        slices.len()
    )
}

/// File ▸ Export ▸ Slices…: ask for a folder and write every committed slice
/// of the active document into it.
pub fn export_slices(editor: &mut Editor) -> Result<String, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let rects = editor.slices.get(doc.id()).to_vec();
    if rects.is_empty() {
        return Err(
            "Export Slices: this document has no slices - draw them with the Slice tool \
             and press Enter"
                .to_string(),
        );
    }
    let Some(dir) = editor.pick_export_folder() else {
        return Err("Export Slices: no destination chosen".to_string());
    };
    let entry = ui::dialogs::export_as::last_confirmed_entry();
    let doc = editor.active_mut().ok_or("No document is open")?;
    let written = write_slices(doc, &rects, &entry.preset, &dir)?;
    Ok(format!(
        "Exported {} slice(s) as {} to {}",
        written.len(),
        entry.preset.format.extension().to_uppercase(),
        dir.display()
    ))
}

/// Write `rects` of `doc`'s composite into `dir`, one file each, with
/// `preset`'s format and settings. Returns the paths written, in slice order.
///
/// A region is clipped to the canvas; one that misses the canvas entirely is
/// an error naming it rather than a file with no pixels.
pub fn write_slices(
    doc: &mut OpenDocument,
    rects: &[PixelRect],
    preset: &raster::ExportPreset,
    dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    let (w, h) = (doc.document.width(), doc.document.height());
    let composite = doc
        .composite(doc.canvas_rect())
        .map_err(|e| e.to_string())?;
    let stem = Path::new(doc.title())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| doc.title().to_string());
    let space = doc.document.meta.color_space.clone();
    let metadata = raster::export::ExportMetadata {
        icc_profile: None,
        icc_profile_space: None,
    };
    let mut written = Vec::with_capacity(rects.len());
    for (index, rect) in rects.iter().enumerate() {
        let number = index + 1;
        let x0 = rect.x.clamp(0, i64::from(w));
        let y0 = rect.y.clamp(0, i64::from(h));
        let x1 = (rect.x + i64::from(rect.width)).clamp(0, i64::from(w));
        let y1 = (rect.y + i64::from(rect.height)).clamp(0, i64::from(h));
        if x1 <= x0 || y1 <= y0 {
            return Err(format!(
                "Export Slices: slice {number} lies outside the canvas"
            ));
        }
        let (cw, ch) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut crop = vec![0u8; cw * ch * 4];
        for row in 0..ch {
            let s = ((y0 as usize + row) * w as usize + x0 as usize) * 4;
            crop[row * cw * 4..(row + 1) * cw * 4].copy_from_slice(&composite[s..s + cw * 4]);
        }
        let image = raster::export::linear_from_rgba8(cw as u32, ch as u32, &crop, &space)
            .map_err(|e| format!("Export Slices: slice {number}: {e}"))?;
        let mut named = preset.clone();
        named.name = format!("{stem}_{number:02}");
        let paths = raster::export::export_batch_to_dir(dir, &image, &[named], &metadata)
            .map_err(|e| format!("Export Slices: slice {number}: {e}"))?;
        written.extend(paths);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    /// A 48x32 document whose left half is red and right half blue, opened in
    /// an editor that answers the folder picker with `out` (when given).
    fn editor_with(dir: &Path, out: Option<&Path>) -> Editor {
        let mut dialogs = ScriptedDialogs::new();
        if let Some(out) = out {
            dialogs = dialogs.exporting_folder(out);
        }
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        );
        let (w, h) = (48u32, 32u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(if x < w / 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let path = dir.join("poster.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
        )
        .unwrap();
        ed.open_path(&path).expect("the probe opens");
        ed
    }

    fn slice(x: i64, y: i64, width: u32, height: u32) -> tools::Slice {
        tools::Slice {
            rect: PixelRect::new(x, y, width, height),
            name: String::new(),
        }
    }

    #[test]
    fn with_no_slices_the_export_refuses_before_asking_for_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let mut ed = editor_with(dir.path(), Some(&out));
        let reason = export_slices(&mut ed).unwrap_err();
        assert!(reason.contains("no slices"), "{reason}");
        assert!(!out.exists(), "a folder was written with nothing to export");
    }

    #[test]
    fn a_later_slice_set_replaces_the_earlier_one() {
        let mut store = SliceStore::default();
        let id = DocumentId(7);
        store.remember(id, vec![PixelRect::new(0, 0, 1, 1)]);
        store.remember(
            id,
            vec![PixelRect::new(2, 2, 3, 3), PixelRect::new(0, 0, 1, 1)],
        );
        assert_eq!(store.get(id).len(), 2);
        assert!(store.get(DocumentId(8)).is_empty());
        store.remember(id, Vec::new());
        assert!(store.get(id).is_empty());
    }

    #[test]
    fn a_slice_outside_the_canvas_is_named_not_written_empty() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let mut ed = editor_with(dir.path(), Some(&out));
        remember_committed(&mut ed, &[slice(100, 100, 8, 8)]);
        let reason = export_slices(&mut ed).unwrap_err();
        assert!(reason.contains("slice 1 lies outside"), "{reason}");
    }
}
