//! One open document: its content, its history, its pixels, and its view.
//!
//! These four travel together and are useless apart. The [`Document`] holds
//! *content hashes*, so it means nothing without the tile source that resolves
//! them; the [`History`] is what makes every change reversible; the camera is
//! per-document because switching tabs must not throw away where the user was
//! looking.
//!
//! # The document is what is on screen
//!
//! [`OpenDocument::composite`] is the only way the canvas gets pixels. There is
//! no second path in which an image is drawn without being in the document —
//! that path is exactly what made the layers panel say "No layers yet" under a
//! visible photograph.

use std::path::{Path, PathBuf};

use compositor::{CompositeOptions, MemoryTileSource, TileCompositor, TileSource};
use editor_core::{Command, CommandError, Document, History};
use project_format::{
    CommandJournal, DocumentDigest, ProjectError, SaveOptions, TileBytes, JOURNAL_FILE,
};
use raster::{PixelRect, TileHash};
use render::Camera;

use crate::dirty::DirtyTiles;
use crate::import::{DecodedImage, ImportError, ImportedDocument};

/// Identity of an open document, stable while it is open. Tabs are addressed by
/// this rather than by index, so closing one does not silently re-target
/// another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(pub u64);

/// Adapter letting `project-format` read the tiles the compositor holds.
///
/// The two crates define the same lookup under different trait names —
/// `compositor::TileSource` for reading, `project_format::TileBytes` for
/// writing — and neither depends on the other, so the application supplies the
/// bridge.
pub struct SourceTiles<'a>(pub &'a MemoryTileSource);

impl TileBytes for SourceTiles<'_> {
    fn tile_bytes(&self, hash: TileHash) -> Option<&[u8]> {
        self.0.tile(hash)
    }
}

/// Why a document could not be opened, saved or exported.
#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Composite(#[from] compositor::CompositeError),
    #[error(transparent)]
    Encode(#[from] raster::CodecError),
    #[error(transparent)]
    Command(#[from] CommandError),
    /// The composite could not be put on the GPU. Reported rather than fatal:
    /// wgpu's default answer to an out-of-range texture is a panic, and this
    /// build aborts on panic, which would take every other open document's
    /// unsaved work with it.
    #[error(transparent)]
    Texture(#[from] render::TextureError),
    /// A composite could not be downscaled to the size the GPU will hold. Only
    /// reachable for a document too big to present at its own resolution — see
    /// [`crate::presenter::downscale_levels`] — and, like the texture error, it
    /// is reported rather than fatal.
    #[error(transparent)]
    Mip(#[from] raster::mipmap::MipError),
    #[error("`{0}` is not a file format Raster Studio can export to")]
    UnknownExportFormat(String),
    #[error("this document has never been saved, so it has no location to save to")]
    NoPath,
}

/// The name the next new layer gets.
///
/// It increments rather than always saying "New Layer", and it skips names that
/// are taken: `Layer {len + 1}` alone repeats itself as soon as a layer has
/// been deleted. One function because both routes to "add a layer" — the menu
/// (through [`crate::Action::NewLayer`]) and the layers dock's `+` — must give
/// the same answer.
pub fn next_layer_name(doc: &Document) -> String {
    let taken: Vec<&str> = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| doc.layers.get(id).map(|l| l.name.as_str()))
        .collect();
    let mut n = doc.layers.len() + 1;
    loop {
        let candidate = format!("Layer {n}");
        if !taken.iter().any(|t| *t == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// The export format implied by a file name.
pub fn export_format_for(path: &Path) -> Option<raster::ExportFormat> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => raster::ExportFormat::Png,
        "jpg" | "jpeg" | "jpe" | "jfif" => raster::ExportFormat::Jpeg(90),
        "webp" => raster::ExportFormat::WebP,
        "tif" | "tiff" => raster::ExportFormat::Tiff,
        "gif" => raster::ExportFormat::Gif,
        "bmp" | "dib" => raster::ExportFormat::Bmp,
        _ => return None,
    })
}

/// A document the user has open.
pub struct OpenDocument {
    id: DocumentId,
    pub document: Document,
    pub history: History,
    pub tiles: MemoryTileSource,
    pub camera: Camera,
    /// Where the `.rstudio` package lives, once it has one.
    project_path: Option<PathBuf>,
    /// The image this document was imported from, if any. Only used to suggest
    /// an export name.
    source_path: Option<PathBuf>,
    compositor: TileCompositor,
    dirty: DirtyTiles,
    /// The camera still owes the user a fit — see [`OpenDocument::set_viewport`].
    ///
    /// Set at construction and cleared by the first real viewport, because at
    /// construction the only size known is the document's own and fitting an
    /// image to itself is exactly `zoom = 1.0`.
    fit_pending: bool,
    /// Labels of the steps that have been undone, mirroring `History`'s redo
    /// stack: last is the one a redo would re-apply.
    ///
    /// `History` hands out labels for the *done* stack (through its journal)
    /// and for the single next redo, but not for the redo stack as a whole —
    /// and the history dock has to draw the whole timeline, or a step vanishes
    /// from the panel the moment it is undone and cannot be clicked back.
    undone_labels: Vec<String>,
}

impl std::fmt::Debug for OpenDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenDocument")
            .field("id", &self.id)
            .field("title", &self.title())
            .field("dirty", &self.is_dirty())
            .field("project_path", &self.project_path)
            .finish()
    }
}

impl OpenDocument {
    /// Wrap an imported document, with a camera waiting to be fitted.
    ///
    /// The camera is *not* fitted here. See [`OpenDocument::set_viewport`]: the
    /// only size known at this point is the document's own, and fitting an
    /// image to a viewport the same size as itself is `zoom = 1.0` — which is
    /// how a 6000×4000 photograph used to open as a 100% centre crop.
    pub fn from_import(id: DocumentId, imported: ImportedDocument) -> Self {
        let size = glam::Vec2::new(
            imported.document.width() as f32,
            imported.document.height() as f32,
        );
        OpenDocument {
            id,
            document: imported.document,
            history: imported.history,
            tiles: imported.tiles,
            camera: Camera::new(size, size),
            project_path: None,
            source_path: None,
            compositor: TileCompositor::new(),
            // Nothing has been presented yet, so everything is outstanding.
            dirty: DirtyTiles::all(),
            undone_labels: Vec::new(),
            fit_pending: true,
        }
    }

    /// Open an image file as a new document with one raster layer.
    pub fn open_image(
        id: DocumentId,
        path: &Path,
        history_depth: usize,
    ) -> Result<Self, DocumentError> {
        let image = DecodedImage::decode_path(path)?;
        let title = DecodedImage::title_for(path);
        let imported = crate::import::document_from_image(&image, &title, history_depth)?;
        let mut open = OpenDocument::from_import(id, imported);
        open.source_path = Some(path.to_path_buf());
        Ok(open)
    }

    /// Open a `.rstudio` package.
    pub fn open_project(
        id: DocumentId,
        path: &Path,
        history_depth: usize,
    ) -> Result<Self, DocumentError> {
        let loaded = project_format::open_project(path)?;
        let tiles = loaded.tile_source()?;
        let mut document = loaded.document;
        document.set_path(Some(path.to_path_buf()));
        let size = glam::Vec2::new(document.width() as f32, document.height() as f32);
        Ok(OpenDocument {
            id,
            document,
            history: History::with_limit(history_depth),
            tiles,
            camera: Camera::new(size, size),
            project_path: Some(path.to_path_buf()),
            source_path: None,
            compositor: TileCompositor::new(),
            dirty: DirtyTiles::all(),
            undone_labels: Vec::new(),
            fit_pending: true,
        })
    }

    /// A blank document — File ▸ New.
    pub fn blank(
        id: DocumentId,
        width: u32,
        height: u32,
        title: &str,
        history_depth: usize,
    ) -> Result<Self, DocumentError> {
        Ok(OpenDocument::from_import(
            id,
            crate::import::blank_document(width, height, title, history_depth)?,
        ))
    }

    /// Tell the document how big the area it is drawn in actually is.
    ///
    /// # Why this is not just `camera.viewport_size = …`
    ///
    /// Opening a file has to *fit* it to the window, and the constructor
    /// cannot: the only size it knows is the document's own, so the camera it
    /// builds has `viewport_size == image_size` and [`Camera::fit`] there is
    /// arithmetically `zoom = min(w/w, h/h) = 1.0` — a call that looks like a
    /// fit and is exactly a no-op. That is how a 6000×4000 photograph opened at
    /// 100% showing the middle 1280×720 of it.
    ///
    /// So the fit is deferred to the first frame that knows the window's size,
    /// and happens **once**: every later frame and every resize only moves
    /// `viewport_size`, because re-fitting would throw away the zoom and the
    /// pan the user chose every time the window changed shape.
    ///
    /// A degenerate viewport (a minimised window reports 0×0) is not a real
    /// size: it moves nothing and does not consume the pending fit, or the
    /// document would come back from the taskbar at `zoom = 0`.
    ///
    /// # Opening never enlarges
    ///
    /// The fit is clamped to 100%. Fitting *on open* means "show all of it",
    /// and an image already smaller than the window is already all there —
    /// blowing a 32×32 icon up to 3000% because the window is big is not what
    /// any editor does. View ▸ Fit on Screen ([`crate::Action::ZoomFit`]) is
    /// the explicit request, and that one does scale up.
    pub fn set_viewport(&mut self, viewport: glam::Vec2) {
        if !(viewport.x >= 1.0 && viewport.y >= 1.0) {
            return;
        }
        self.camera.viewport_size = viewport;
        if self.fit_pending {
            self.camera.fit();
            self.camera.zoom = self.camera.zoom.min(1.0);
            self.fit_pending = false;
        }
    }

    /// `true` until the camera has been fitted to a real viewport.
    ///
    /// Exposed so a caller that draws before it knows its own size can tell
    /// "the user chose 100%" from "nobody has fitted this yet".
    pub fn awaiting_fit(&self) -> bool {
        self.fit_pending
    }

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn title(&self) -> &str {
        &self.document.meta.title
    }

    pub fn is_dirty(&self) -> bool {
        self.document.is_dirty()
    }

    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// The label on this document's tab: a bullet marks unsaved changes.
    pub fn tab_label(&self) -> String {
        if self.is_dirty() {
            format!("• {}", self.title())
        } else {
            self.title().to_string()
        }
    }

    /// Where a "Save As" dialog should start.
    pub fn suggested_save_path(&self) -> PathBuf {
        if let Some(p) = &self.project_path {
            return p.clone();
        }
        let stem = self
            .source_path
            .as_deref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.title().to_string());
        let dir = self
            .source_path
            .as_deref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_default();
        dir.join(format!("{stem}.{}", crate::dialogs::PROJECT_EXTENSION))
    }

    /// Where an "Export" dialog should start.
    pub fn suggested_export_path(&self) -> PathBuf {
        let base = self
            .source_path
            .clone()
            .or_else(|| self.project_path.clone())
            .unwrap_or_else(|| PathBuf::from(self.title()));
        let stem = base
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.title().to_string());
        let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
        dir.join(format!("{stem}.png"))
    }

    /// Run a command through history. The **only** way this type's document is
    /// mutated, which is what keeps undo/redo uniform.
    pub fn apply(&mut self, command: Command) -> Result<(), CommandError> {
        self.history.apply(&mut self.document, command.clone())?;
        // A new command drops the redo stack (standard linear history), so the
        // labels mirroring it go too.
        self.undone_labels.clear();
        self.dirty.record(&command);
        // Only after the command was accepted: `project-format` requires the
        // journal to record commands that are already in the document, or a
        // recovery would replay one the snapshot never had.
        if let Some(project) = &self.project_path {
            if let Err(e) = CommandJournal::append(&project.join(JOURNAL_FILE), &command) {
                // A journal that cannot be written costs crash recovery, not
                // the edit — the edit is already in the document.
                tracing::warn!("cannot journal the command: {e}");
            }
        }
        Ok(())
    }

    /// Undo one step. Reports whether anything was undone.
    pub fn undo(&mut self) -> Result<bool, CommandError> {
        // Read the label *before* the step moves off the done stack.
        let label = self.history.undo_label().map(str::to_string);
        let undone = self.history.undo(&mut self.document)?;
        if undone {
            self.undone_labels
                .push(label.unwrap_or_else(|| "Step".to_string()));
            // `History` applies the inverse internally and does not hand it
            // back, so the reach of the change is not knowable here. Marking
            // the whole canvas is the honest answer; see `crate::dirty`.
            self.dirty.mark_all();
        }
        Ok(undone)
    }

    /// Redo one step. Reports whether anything was redone.
    pub fn redo(&mut self) -> Result<bool, CommandError> {
        let redone = self.history.redo(&mut self.document)?;
        if redone {
            self.undone_labels.pop();
            self.dirty.mark_all();
        }
        Ok(redone)
    }

    /// How many commands are applied right now — the point on the timeline the
    /// document stands at.
    pub fn history_depth(&self) -> usize {
        self.history.undo_depth()
    }

    /// Every step of this document's timeline, oldest first, undone steps
    /// included.
    ///
    /// The first `history_depth()` entries have been applied; the rest are
    /// undone and would come back with a redo.
    ///
    /// The redo half is clamped to `History::redo_depth()` rather than trusted:
    /// lowering the history limit (a preferences change) drops the *newest*
    /// entries of the redo stack, and a panel drawn from a stale mirror would
    /// offer a step no redo could reach.
    pub fn history_timeline(&self) -> Vec<String> {
        let mut out: Vec<String> = self.history.journal().map(|c| c.label()).collect();
        let live = self.history.redo_depth().min(self.undone_labels.len());
        // The stack is newest-undone first, so chronological order is its
        // reverse; compaction takes from the front, so the live tail is what
        // survives.
        out.extend(
            self.undone_labels[self.undone_labels.len() - live..]
                .iter()
                .rev()
                .cloned(),
        );
        out
    }

    /// The whole canvas as a rectangle.
    pub fn canvas_rect(&self) -> PixelRect {
        PixelRect::new(0, 0, self.document.width(), self.document.height())
    }

    /// Composite `region`, reusing every cached tile whose inputs are unchanged.
    pub fn composite(&mut self, region: PixelRect) -> Result<Vec<u8>, DocumentError> {
        let canvas = self.compositor.composite_region(
            &self.document,
            &self.tiles,
            region,
            0,
            CompositeOptions::default(),
        )?;
        Ok(canvas.to_rgba8(&self.document.meta.color_space))
    }

    /// Hit/miss counters of this document's tile cache — how the "an edit
    /// recomposites only what changed" claim is checked.
    pub fn cache_stats(&self) -> compositor::CacheStats {
        self.compositor.stats()
    }

    /// Tiles invalidated since the presenter last looked.
    pub fn dirty(&self) -> &DirtyTiles {
        &self.dirty
    }

    /// Hand the presenter the outstanding invalidation and clear it.
    pub fn take_dirty(&mut self) -> DirtyTiles {
        self.dirty.take()
    }

    /// Force a full redraw — used when the presenter's texture is (re)created.
    pub fn invalidate_all(&mut self) {
        self.dirty.mark_all();
    }

    /// Write a package at `path` without adopting it.
    ///
    /// This is what autosave uses. It deliberately does **not** clear the dirty
    /// flag or move the document's location: an autosave into the scratch
    /// directory is a safety net, not the save the user asked for, and treating
    /// it as one would tell the user their work is safe in a file they have
    /// never heard of.
    pub fn write_snapshot(&self, path: &Path, app_version: &str) -> Result<(), DocumentError> {
        project_format::save_project_with(
            path,
            &self.document,
            &SourceTiles(&self.tiles),
            &SaveOptions::new(app_version),
        )?;
        Ok(())
    }

    /// Save to `path`, which becomes this document's location.
    pub fn save_to(&mut self, path: &Path, app_version: &str) -> Result<(), DocumentError> {
        let report = project_format::save_project_with(
            path,
            &self.document,
            &SourceTiles(&self.tiles),
            &SaveOptions::new(app_version),
        )?;
        self.project_path = Some(path.to_path_buf());
        self.document.set_path(Some(path.to_path_buf()));
        self.document.mark_saved();
        // The save marker is what makes crash recovery replay only the
        // commands accepted *after* this point.
        if let Err(e) = CommandJournal::mark_saved(&path.join(JOURNAL_FILE), report.document) {
            tracing::warn!("cannot write the save marker: {e}");
        }
        Ok(())
    }

    /// Forget where this document was loaded from, and call it unsaved work.
    ///
    /// What restoring a *scratch autosave* needs. The content is real and the
    /// user wants it back, but the package it came out of lives in the
    /// application's scratch directory — somewhere the user never chose and
    /// cannot be expected to find again. Adopting that path would let the tab
    /// show a clean document whose only copy is a file the next crash recovery
    /// would overwrite, so the restored document is deliberately left with no
    /// location and a dirty flag: Ctrl+S asks where to put it.
    pub fn detach_from_disk(&mut self) {
        self.project_path = None;
        self.document.set_path(None);
        self.document.mark_dirty();
    }

    /// Save to wherever this document already lives.
    pub fn save(&mut self, app_version: &str) -> Result<(), DocumentError> {
        let path = self.project_path.clone().ok_or(DocumentError::NoPath)?;
        self.save_to(&path, app_version)
    }

    /// Flatten the document and write it as an image file.
    pub fn export_to(&mut self, path: &Path) -> Result<(), DocumentError> {
        let format = export_format_for(path).ok_or_else(|| {
            DocumentError::UnknownExportFormat(
                path.extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            )
        })?;
        let rgba8 = self.composite(self.canvas_rect())?;
        raster::encode_to_path(
            path,
            format,
            self.document.width(),
            self.document.height(),
            raster::EncodedPixels::Rgba8(&rgba8),
            &raster::EncodeOptions::default(),
        )?;
        Ok(())
    }

    /// The digest of this document as it would be written — for pairing a save
    /// marker with a snapshot.
    pub fn document_digest(&self) -> Option<DocumentDigest> {
        rmp_serde::to_vec_named(&self.document)
            .ok()
            .map(|b| DocumentDigest::of(&b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::pixels::{PixelTarget, TileDelta, TileEdit};
    use layer_model::Layer;
    use raster::{Tile, TileCoord, TILE_SIZE};

    fn image(width: u32, height: u32, value: u8) -> DecodedImage {
        DecodedImage {
            width,
            height,
            rgba8: vec![value; (width as usize) * (height as usize) * 4],
        }
    }

    fn doc_of(width: u32, height: u32) -> OpenDocument {
        let imported =
            crate::import::document_from_image(&image(width, height, 128), "test.png", 100)
                .unwrap();
        OpenDocument::from_import(DocumentId(1), imported)
    }

    #[test]
    fn a_fresh_document_is_clean_and_fits_the_canvas() {
        let d = doc_of(600, 400);
        assert!(!d.is_dirty());
        assert_eq!(d.tab_label(), "test.png");
        assert_eq!(d.canvas_rect(), PixelRect::new(0, 0, 600, 400));
        assert!(d.project_path().is_none());
    }

    #[test]
    fn an_image_larger_than_the_window_is_fitted_once_the_window_size_is_known() {
        // The defect: `from_import` called `camera.fit()` while the camera's
        // viewport was still the image's *own* size, and
        // `min(w/w, h/h) == 1.0` — a call that reads like a fit and is
        // arithmetically a no-op. So a photograph opened at 100%, showing the
        // middle of it, under a constructor whose doc said "sizing the camera
        // to the canvas". 1200x800 into 256x144 has the same shape as
        // 6000x4000 into a 1280x720 window and costs a thousandth of the RAM.
        let mut d = doc_of(1200, 800);
        assert_eq!(d.camera.zoom, 1.0, "nothing can be fitted yet");
        assert!(d.awaiting_fit());

        // A minimised window reports 0x0. That is not a size, and it must not
        // consume the pending fit or the document comes back at zoom 0.
        d.set_viewport(glam::Vec2::ZERO);
        assert!(d.awaiting_fit(), "0x0 counted as a viewport");
        assert_eq!(d.camera.zoom, 1.0);

        d.set_viewport(glam::Vec2::new(256.0, 144.0));
        assert!(!d.awaiting_fit(), "the fit is owed only once");
        assert!(
            (d.camera.zoom - 144.0 / 800.0).abs() < 1e-6,
            "the whole image does not fit: zoom {}",
            d.camera.zoom
        );
        // Which is to say: every corner of the image is inside the viewport.
        let half = glam::Vec2::new(1200.0, 800.0) * 0.5 * d.camera.zoom;
        assert!(half.x <= 128.0 + 1e-3 && half.y <= 72.0 + 1e-3, "{half:?}");
        assert_eq!(d.camera.center, glam::Vec2::new(600.0, 400.0));
    }

    #[test]
    fn an_image_smaller_than_the_window_opens_at_its_own_size() {
        // The other direction of the same fix, and the reason the open-time fit
        // is clamped: "fit" here means "all of it is on screen", which a small
        // image already is. Scaling a thumbnail up to fill a 4K window because
        // it happens to be small is not fitting, it is magnifying.
        let mut d = doc_of(64, 48);
        d.set_viewport(glam::Vec2::new(1600.0, 900.0));
        assert!(!d.awaiting_fit());
        assert_eq!(d.camera.zoom, 1.0, "opening enlarged the image");

        // View ▸ Fit on Screen is the explicit ask, and it still scales up.
        d.camera.fit();
        assert!(d.camera.zoom > 1.0, "Fit on Screen must still fill");
    }

    #[test]
    fn a_resize_moves_the_viewport_without_undoing_the_users_zoom() {
        // The other half of the same fix: the fit is deferred, not repeated.
        // Re-fitting on every viewport change would snap the view back every
        // time the window was resized or a panel opened.
        let mut d = doc_of(1200, 800);
        d.set_viewport(glam::Vec2::new(256.0, 144.0));

        d.camera.zoom = 4.0;
        d.camera.center = glam::Vec2::new(100.0, 200.0);
        d.set_viewport(glam::Vec2::new(320.0, 200.0));

        assert_eq!(d.camera.viewport_size, glam::Vec2::new(320.0, 200.0));
        assert_eq!(d.camera.zoom, 4.0, "the resize re-fitted");
        assert_eq!(d.camera.center, glam::Vec2::new(100.0, 200.0));
    }

    #[test]
    fn dirty_state_tracks_edits_and_clears_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = doc_of(300, 200);
        assert!(!d.is_dirty(), "just opened");
        assert_eq!(d.tab_label(), "test.png");

        d.apply(Command::create_layer(Layer::raster("second")))
            .unwrap();
        assert!(d.is_dirty(), "an edit makes the document unsaved work");
        assert_eq!(d.tab_label(), "• test.png", "the tab says so");

        let project = dir.path().join("p.rstudio");
        d.save_to(&project, "test").unwrap();
        assert!(!d.is_dirty(), "a save clears it");
        assert_eq!(d.tab_label(), "test.png");
        assert_eq!(d.project_path(), Some(project.as_path()));

        // ...and the next edit dirties it again.
        d.apply(Command::create_layer(Layer::raster("third")))
            .unwrap();
        assert!(d.is_dirty());

        // Undo back to the saved content still counts as unsaved: the flag
        // records "changed since the save", which an undo is.
        d.undo().unwrap();
        assert!(d.is_dirty());
    }

    #[test]
    fn a_saved_document_reopens_with_its_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.rstudio");
        let mut d = doc_of(300, 200);
        let before = d.composite(d.canvas_rect()).unwrap();
        d.save_to(&project, "test").unwrap();

        let mut back = OpenDocument::open_project(DocumentId(2), &project, 100).unwrap();
        assert!(
            back.awaiting_fit(),
            "a reopened package is fitted when the window's size is known, not \
             against its own canvas size, where a fit is a no-op"
        );
        assert_eq!(back.document.layers.len(), 1);
        assert_eq!(back.project_path(), Some(project.as_path()));
        assert!(!back.is_dirty());
        let after = back.composite(back.canvas_rect()).unwrap();
        assert_eq!(after, before, "the pixels came back");
    }

    #[test]
    fn an_edit_recomposites_only_the_tiles_it_touched() {
        // 3x2 tiles, so there is plenty that must *not* be recomputed.
        let mut d = doc_of(TILE_SIZE * 3, TILE_SIZE * 2);
        let layer = d.document.active_layer().unwrap();

        d.composite(d.canvas_rect()).unwrap();
        let warm = d.cache_stats();
        assert_eq!(warm.misses, 6, "six tiles composited cold");
        // What the presenter does after it has uploaded a frame.
        assert!(
            d.take_dirty().is_all(),
            "a never-presented document owes a full upload"
        );

        // A second identical frame recomputes nothing at all.
        d.composite(d.canvas_rect()).unwrap();
        assert_eq!(
            d.cache_stats().misses,
            warm.misses,
            "a static frame is free"
        );
        assert_eq!(d.cache_stats().hits, 6);

        // Repaint exactly one tile.
        let mut tile = Tile::transparent(raster::PixelFormat::Rgba8);
        tile.data_mut().fill(77);
        let hash = d.tiles.insert_tile(&tile);
        let coord = TileCoord::new(1, 0, 0);
        d.apply(Command::PaintTiles {
            target: PixelTarget::Layer(layer),
            delta: TileDelta::single(TileEdit::set(coord, hash)),
        })
        .unwrap();

        let before = d.cache_stats();
        d.composite(d.canvas_rect()).unwrap();
        let after = d.cache_stats();
        assert_eq!(
            after.misses - before.misses,
            1,
            "only the painted tile may be recomposited"
        );
        assert_eq!(after.hits - before.hits, 5);

        // And the dirty set the presenter is handed names that one tile.
        let dirty = d.take_dirty();
        assert!(!dirty.is_all());
        assert_eq!(dirty.tiles().collect::<Vec<_>>(), vec![coord]);
        assert!(d.dirty().is_empty(), "taking it clears it");
    }

    #[test]
    fn undo_and_redo_report_whether_they_did_anything() {
        let mut d = doc_of(64, 64);
        assert!(!d.undo().unwrap(), "nothing to undo yet");
        assert!(!d.redo().unwrap());

        d.apply(Command::create_layer(Layer::raster("x"))).unwrap();
        assert_eq!(d.document.layers.len(), 2);
        assert!(d.undo().unwrap());
        assert_eq!(d.document.layers.len(), 1);
        assert!(d.redo().unwrap());
        assert_eq!(d.document.layers.len(), 2);
        assert!(d.dirty().is_all(), "an undo redraws the canvas");
    }

    #[test]
    fn export_writes_a_real_image_of_the_composite() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = doc_of(40, 30);
        let out = dir.path().join("flat.png");
        d.export_to(&out).unwrap();

        let decoded = raster::decode_path(&out).unwrap();
        assert_eq!((decoded.width, decoded.height), (40, 30));
        let expected = d.composite(d.canvas_rect()).unwrap();
        assert_eq!(decoded.rgba8, expected);
    }

    #[test]
    fn export_refuses_a_format_it_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = doc_of(8, 8);
        let err = d.export_to(&dir.path().join("out.psd")).unwrap_err();
        assert!(err.to_string().contains("psd"), "{err}");
        assert!(!dir.path().join("out.psd").exists(), "nothing was written");
    }

    #[test]
    fn every_offered_export_extension_maps_to_a_format() {
        for ext in ["png", "jpg", "jpeg", "webp", "tif", "tiff", "gif", "bmp"] {
            assert!(
                export_format_for(Path::new(&format!("x.{ext}"))).is_some(),
                ".{ext} is offered by the export dialog but has no format"
            );
        }
        assert_eq!(export_format_for(Path::new("x")), None);
        assert_eq!(export_format_for(Path::new("x.exr")), None);
        // Case does not matter — Windows hands back whatever the user typed.
        assert_eq!(
            export_format_for(Path::new("x.PNG")),
            Some(raster::ExportFormat::Png)
        );
    }

    #[test]
    fn suggested_paths_follow_where_the_document_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("holiday.png");
        let img = image(16, 16, 200);
        std::fs::write(
            &src,
            raster::encode(raster::ExportFormat::Png, 16, 16, &img.rgba8).unwrap(),
        )
        .unwrap();

        let d = OpenDocument::open_image(DocumentId(3), &src, 10).unwrap();
        assert_eq!(
            d.suggested_save_path(),
            dir.path().join("holiday.rstudio"),
            "save suggests a project next to the image"
        );
        assert_eq!(
            d.suggested_export_path(),
            dir.path().join("holiday.png"),
            "export suggests the image itself"
        );
    }

    #[test]
    fn an_edit_after_a_save_lands_in_the_journal_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.rstudio");
        let mut d = doc_of(64, 64);
        d.save_to(&project, "test").unwrap();
        assert!(
            crate::session::recoverable(&project).unwrap().is_none(),
            "nothing outstanding right after a save"
        );

        d.apply(Command::create_layer(Layer::raster("unsaved")))
            .unwrap();
        let rec = crate::session::recoverable(&project)
            .unwrap()
            .expect("the edit is recoverable");
        assert_eq!(rec.commands.len(), 1);

        // Saving again moves the marker past it.
        d.save_to(&project, "test").unwrap();
        assert!(crate::session::recoverable(&project).unwrap().is_none());
    }

    #[test]
    fn a_restored_autosave_keeps_its_pixels_but_loses_its_location() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch").join("autosave-1.rstudio");
        let mut d = doc_of(64, 48);
        d.apply(Command::create_layer(Layer::raster("unsaved work")))
            .unwrap();
        let before = d.composite(d.canvas_rect()).unwrap();
        d.write_snapshot(&scratch, "test").unwrap();

        let mut back = OpenDocument::open_project(DocumentId(9), &scratch, 100).unwrap();
        assert_eq!(back.project_path(), Some(scratch.as_path()));
        assert!(!back.is_dirty());

        back.detach_from_disk();
        assert_eq!(back.project_path(), None, "the scratch path is not a home");
        assert!(
            back.is_dirty(),
            "the user still has to choose where it goes"
        );
        assert_eq!(back.document.path(), None);
        assert_eq!(
            back.composite(back.canvas_rect()).unwrap(),
            before,
            "the pixels survived the round trip"
        );
        assert_eq!(back.document.layers.len(), 2);
    }

    #[test]
    fn saving_a_document_that_has_no_path_is_refused_rather_than_guessed() {
        let mut d = doc_of(8, 8);
        let err = d.save("test").unwrap_err();
        assert!(matches!(err, DocumentError::NoPath), "{err}");
    }
}
