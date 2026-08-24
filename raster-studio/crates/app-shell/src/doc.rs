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
    #[error("writing print output failed: {0}")]
    Io(String),
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
///
/// `.psd` is deliberately absent: [`raster::ExportFormat`] describes flat
/// images, and writing a layered document as one flattened frame is the exact
/// thing that made "save a PSD" worthless. [`exports_as_psd`] answers for that
/// destination, and [`OpenDocument::export_to`] asks it first.
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

/// `true` when this destination asks for a layered Photoshop document.
///
/// By name, unlike [`crate::import::looks_like_psd`], which asks by content:
/// nothing exists at an export destination yet, so the name is all there is.
pub fn exports_as_psd(path: &Path) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("psd"))
        .unwrap_or(false)
}

/// Write `bytes` to `path` without destroying what is already there if the
/// write fails.
///
/// The same rule [`raster::encode_to_path`] follows: a save that dies half way
/// through must not have eaten the previous version. The temporary file is a
/// sibling so the rename stays on one filesystem, and it is removed when the
/// rename does not happen.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".to_string());
    let temp_name = format!(".{name}.{}.part", std::process::id());
    let temp = match dir {
        Some(dir) => dir.join(temp_name),
        None => PathBuf::from(temp_name),
    };
    if let Err(e) = std::fs::write(&temp, bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
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
    /// Whether the source this document was opened from carried 16 bits per
    /// channel. When true and the export format supports it,
    /// [`OpenDocument::export_to`] writes 16 bits per channel so a deep source
    /// is not crushed to eight on the way out. A `.rstudio` package does not
    /// record this, so it stays false for a project re-opened from disk.
    source_sixteen_bit: bool,
    /// The camera still owes the user a fit — see [`OpenDocument::set_viewport`].
    ///
    /// Set at construction and cleared by the first real viewport, because at
    /// construction the only size known is the document's own and fitting an
    /// image to itself is exactly `zoom = 1.0`.
    fit_pending: bool,
    /// What the last PSD exchange — the open that created this document, or the
    /// last export to a `.psd` — could not carry across.
    ///
    /// Empty is the good case. It is kept on the document rather than reported
    /// once and forgotten because "which parts of my file did not survive?" is
    /// a question the user asks *later*, once they notice something missing.
    psd_notes: crate::import::PsdNotes,
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
            source_sixteen_bit: false,
            psd_notes: crate::import::PsdNotes::default(),
            undone_labels: Vec::new(),
            fit_pending: true,
        }
    }

    /// Open an image file as a new document.
    ///
    /// A Photoshop document takes the other road: it is a *document*, not a
    /// picture, and [`OpenDocument::open_psd`] builds its real layer tree
    /// rather than flattening it. The choice is made on the file's leading
    /// bytes, not its extension — see [`crate::import::looks_like_psd`].
    pub fn open_image(
        id: DocumentId,
        path: &Path,
        history_depth: usize,
    ) -> Result<Self, DocumentError> {
        if crate::import::looks_like_psd(path) {
            return OpenDocument::open_psd(id, path, history_depth);
        }
        let image = DecodedImage::decode_path(path)?;
        let title = DecodedImage::title_for(path);
        let imported = crate::import::document_from_image(&image, &title, history_depth)?;
        let mut open = OpenDocument::from_import(id, imported);
        open.source_path = Some(path.to_path_buf());
        // A 16-bit-capable export destination can carry more than eight bits;
        // record the depth the file actually arrived in so the export route
        // below can choose to honor it.
        let surface = raster::decode_surface_path(path, raster::ImportLimits::default())?;
        open.source_sixteen_bit = surface.format() == raster::PixelFormat::Rgba16;
        Ok(open)
    }

    /// Open a `.psd` as a layered document: groups, masks, blend modes,
    /// opacity and pixels, not a flattened frame.
    ///
    /// Anything the file carried that this document model has no home for is
    /// left in [`OpenDocument::psd_notes`] rather than dropped in silence.
    pub fn open_psd(
        id: DocumentId,
        path: &Path,
        history_depth: usize,
    ) -> Result<Self, DocumentError> {
        let bytes = crate::import::read_psd_bytes(path)?;
        let title = DecodedImage::title_for(path);
        let import = crate::import::document_from_psd(&bytes, &title, history_depth)?;
        let mut open = OpenDocument::from_import(id, import.imported);
        open.source_path = Some(path.to_path_buf());
        open.psd_notes = import.notes;
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
            source_sixteen_bit: false,
            psd_notes: crate::import::PsdNotes::default(),
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

    /// A fresh copy of the document's current state as a new document: same
    /// pixels, same layer tree, same tiles — a new id, an empty undo stack
    /// (the copy *starts* at the current state) and a `copy` title. The engine
    /// half of File ▸ Duplicate Document.
    pub fn duplicate(&self, new_id: DocumentId) -> Self {
        let mut doc = self.document.clone();
        doc.meta.title = format!("{} copy", self.title());
        let size = glam::Vec2::new(doc.width() as f32, doc.height() as f32);
        OpenDocument {
            id: new_id,
            document: doc,
            history: History::default(),
            tiles: self.tiles.clone(),
            camera: Camera::new(size, size),
            project_path: None,
            source_path: None,
            compositor: TileCompositor::new(),
            dirty: DirtyTiles::all(),
            source_sixteen_bit: self.source_sixteen_bit,
            psd_notes: crate::import::PsdNotes::default(),
            undone_labels: Vec::new(),
            fit_pending: true,
        }
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

    /// Whether this document was opened from a 16-bit source — drives the
    /// File Info window and the 16-bit export route.
    pub fn is_sixteen_bit(&self) -> bool {
        self.source_sixteen_bit
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

    /// Box-sample `rgba8` (`w`x`h`, straight alpha) down to a thumbnail keeping
    /// aspect ratio under `max_edge` on both axes. Every output pixel is the mean
    /// of the source pixels it covers, so a downscale never deletes a channel's
    /// contribution.
    fn box_downscale(rgba8: &[u8], w: u32, h: u32, max_edge: u32) -> (u32, u32, Vec<u8>) {
        let scale = if max_edge == 0 {
            1.0
        } else {
            let m = w.max(h);
            if m <= max_edge {
                1.0
            } else {
                max_edge as f64 / m as f64
            }
        };
        if scale >= 1.0 || w == 0 || h == 0 {
            return (w, h, rgba8.to_vec());
        }
        let nw = ((w as f64 * scale).round() as u32).max(1);
        let nh = ((h as f64 * scale).round() as u32).max(1);
        let mut out = vec![0u8; (nw as usize) * (nh as usize) * 4];

        for oy in 0..nh {
            for ox in 0..nw {
                let x0 = (ox as f64 / nw as f64) * w as f64;
                let x1 = ((ox + 1) as f64 / nw as f64) * w as f64;
                let y0 = (oy as f64 / nh as f64) * h as f64;
                let y1 = ((oy + 1) as f64 / nh as f64) * h as f64;
                let sx0 = x0.floor() as usize;
                let sx1 = x1.ceil().max(x0 + 1.0) as usize;
                let sy0 = y0.floor() as usize;
                let sy1 = y1.ceil().max(y0 + 1.0) as usize;
                let mut acc = [0f64; 4];
                let mut n = 0usize;
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        if (sx as f64) < x1
                            && (sy as f64) < y1
                            && (sx as f64) >= x0
                            && (sy as f64) >= y0
                        {
                            let i = ((sy as u32 * w) as usize + sx) * 4;
                            acc[0] += rgba8[i] as f64;
                            acc[1] += rgba8[i + 1] as f64;
                            acc[2] += rgba8[i + 2] as f64;
                            acc[3] += rgba8[i + 3] as f64;
                            n += 1;
                        }
                    }
                }
                let oi = ((oy * nw) as usize + ox as usize) * 4;
                for c in 0..4 {
                    out[oi + c] = if n == 0 {
                        0
                    } else {
                        (acc[c] / n as f64).round().clamp(0.0, 255.0) as u8
                    };
                }
            }
        }
        (nw, nh, out)
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

    /// Composite `region` as straight-alpha 16-bit RGBA in the document's
    /// colour space, for exports that can carry a third sample of precision.
    pub fn composite_rgba16(&mut self, region: PixelRect) -> Result<Vec<u16>, DocumentError> {
        let canvas = self.compositor.composite_region(
            &self.document,
            &self.tiles,
            region,
            0,
            CompositeOptions::default(),
        )?;
        Ok(canvas.to_rgba16(&self.document.meta.color_space))
    }

    /// A small (fitted to `max_edge`) RGBA8 preview of the canvas a *single*
    /// layer draws on its own — every other layer hidden — with its effects
    /// and blending honoured by the real compositor. This is the engine half
    /// of the Layers/History pixel thumbnails: the panel asks for a cached
    /// thumbnail per layer rather than re-tracing a glyph. Returns
    /// `(width, height, rgba8)`, both reduced to keep `width,height <= max_edge`
    /// (box-sampled, never upscaled). `&self` (free compositor, not the cache)
    /// so a frame with only an immutable borrow can upload one per layer.
    pub fn layer_thumbnail(
        &self,
        layer_id: layer_model::LayerId,
        max_edge: u32,
    ) -> Result<(u32, u32, Vec<u8>), DocumentError> {
        let mut staged = self.document.clone();
        for other in staged.layers.iter_depth_first() {
            if other != layer_id {
                if let Some(l) = staged.layers.get_mut(other) {
                    l.visible = false;
                }
            }
        }
        let rect = self.canvas_rect();
        let canvas = compositor::composite_region(
            &staged,
            &self.tiles,
            rect,
            0,
            CompositeOptions::default(),
        )?;
        let rgba8 = canvas.to_rgba8(&self.document.meta.color_space);
        let (w, h) = (self.document.width(), self.document.height());
        Ok(Self::box_downscale(&rgba8, w, h, max_edge))
    }

    /// The pixels of one layer composited *alone* (every other layer hidden)
    /// over transparent at **full resolution** — no downscale — for the
    /// embedded-document editor: the bytes an `Edit Contents` tab should show
    /// and that a `Commit` writes back. `&self`, so a caller holding only an
    /// immutable borrow can seed a thumbnail or a contents document.
    pub fn layer_pixels(&self, layer_id: layer_model::LayerId) -> Result<Vec<u8>, DocumentError> {
        let mut staged = self.document.clone();
        for other in staged.layers.iter_depth_first() {
            if other != layer_id {
                if let Some(l) = staged.layers.get_mut(other) {
                    l.visible = false;
                }
            }
        }
        let rect = self.canvas_rect();
        let canvas = compositor::composite_region(
            &staged,
            &self.tiles,
            rect,
            0,
            CompositeOptions::default(),
        )?;
        Ok(canvas.to_rgba8(&self.document.meta.color_space))
    }

    /// Resample the whole canvas to `new_w × new_h`, moving every pixel-bearing
    /// layer's content by `src_min` and cropping or padding with transparency
    /// at the edges. Returns an undoable [`Command::Transaction`]: a
    /// [`Command::SetCanvasSize`] (which editor_core already validates and
    /// inverts) plus one [`Command::PaintTiles`] per layer carrying the
    /// resampled tile hashes. This is the engine half of Image Size / Canvas
    /// Size / Crop to Selection / Trim / Reveal All.
    pub fn resize_canvas(
        &mut self,
        new_w: u32,
        new_h: u32,
        src_min: glam::IVec2,
    ) -> Result<Command, DocumentError> {
        let old_w = self.document.width();
        let old_h = self.document.height();
        let ids: Vec<layer_model::LayerId> = self.document.layers.iter_depth_first();
        let mut commands = Vec::new();
        commands.push(Command::SetCanvasSize {
            size: glam::UVec2::new(new_w, new_h),
        });
        for id in ids {
            let rgba = self.layer_pixels(id)?;
            let mut out = vec![0u8; new_w as usize * new_h as usize * 4];
            for dy in 0..new_h {
                let sy = dy as i64 + src_min.y as i64;
                if sy < 0 || sy >= old_h as i64 {
                    continue;
                }
                for dx in 0..new_w {
                    let sx = dx as i64 + src_min.x as i64;
                    if sx < 0 || sx >= old_w as i64 {
                        continue;
                    }
                    let si = (sy as usize * old_w as usize + sx as usize) * 4;
                    let di = (dy as usize * new_w as usize + dx as usize) * 4;
                    out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                }
            }
            let grid = raster::TileGrid::from_rgba8(new_w, new_h, &out)
                .map_err(|e| DocumentError::Io(e.to_string()))?;
            let mut edits = Vec::new();
            for (coord, tile) in grid.iter() {
                let hash = self.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            commands.push(
                Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(id), edits)
                    .map_err(|e| DocumentError::Io(e.to_string()))?,
            );
        }
        Ok(Command::Transaction {
            label: "Resize Canvas".to_string(),
            commands,
        })
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

    /// What the last PSD exchange could not carry — see the field's note.
    pub fn psd_notes(&self) -> &crate::import::PsdNotes {
        &self.psd_notes
    }

    /// Write the document as a layered `.psd`.
    ///
    /// The flattened image every previewer and thumbnailer shows is taken from
    /// *this* application's compositor, not from the `psd` crate's fallback
    /// flattener — that one ignores clipping groups, layer effects and
    /// adjustment layers, and a file whose thumbnail disagrees with its layers
    /// is worse than one with no thumbnail at all.
    ///
    /// Returns what the document could not express in the format; the same
    /// report is left on [`OpenDocument::psd_notes`].
    pub fn export_psd_to(&mut self, path: &Path) -> Result<crate::import::PsdNotes, DocumentError> {
        let rgba8 = self.composite(self.canvas_rect())?;
        let (bytes, notes) = crate::import::psd_from_document(&self.document, &self.tiles, &rgba8)?;
        write_atomically(path, &bytes).map_err(crate::import::ImportError::from)?;
        self.psd_notes = notes.clone();
        Ok(notes)
    }

    /// Write the document as an image file.
    ///
    /// Flattened for every format but `.psd`, which keeps its layers.
    pub fn export_to(&mut self, path: &Path) -> Result<(), DocumentError> {
        if exports_as_psd(path) {
            self.export_psd_to(path)?;
            return Ok(());
        }
        let format = export_format_for(path).ok_or_else(|| {
            DocumentError::UnknownExportFormat(
                path.extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            )
        })?;
        // A deep source (PNG/TIFF at 16 bits) may go back out at 16 bits to the
        // formats that carry them, instead of being crushed to eight on the way
        // out. The composite is `f32` either way; the depth only changes how it
        // is quantized into the file.
        let rect = self.canvas_rect();
        if self.source_sixteen_bit && format.supports_16_bit() {
            let rgba16 = self.composite_rgba16(rect)?;
            raster::encode_to_path(
                path,
                format,
                self.document.width(),
                self.document.height(),
                raster::EncodedPixels::Rgba16(&rgba16),
                &raster::EncodeOptions::default(),
            )?
        } else {
            let rgba8 = self.composite(rect)?;
            raster::encode_to_path(
                path,
                format,
                self.document.width(),
                self.document.height(),
                raster::EncodedPixels::Rgba8(&rgba8),
                &raster::EncodeOptions::default(),
            )?
        };
        Ok(())
    }

    /// Write the whole composite as a print-ready single-page PDF (the S1.8
    /// Print path). Alpha is composited onto white paper; the file carries no
    /// document state, only the rendered pixels at the document's own size.
    pub fn print_to(&mut self, path: &std::path::Path) -> Result<(), DocumentError> {
        let rect = self.canvas_rect();
        let rgba8 = self.composite(rect)?;
        let (w, h) = (self.document.width(), self.document.height());
        let pdf = raster::pdf::encode_pdf(w, h, &rgba8);
        std::fs::write(path, pdf).map_err(|e| DocumentError::Io(e.to_string()))?;
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
    fn print_to_writes_a_well_formed_pdf_of_the_composite() {
        let mut d = doc_of(40, 30);
        let dir = std::env::temp_dir().join(format!("rs-print-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.pdf");
        d.print_to(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.4"), "PDF header");
        assert!(
            bytes.windows(9).any(|w| w == b"startxref"),
            "startxref present"
        );
        assert!(
            bytes.ends_with(
                b"%%EOF
"
            ),
            "trailer"
        );
        // The page media box is the document's own pixel size (this check is
        // done on the ASCII head of the file, before the compressed stream).
        assert!(
            String::from_utf8_lossy(&bytes).contains("/MediaBox [0 0 40 30]"),
            "media box matches the composite"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_layer_thumbnail_composites_that_layer_alone_and_fits() {
        let d = doc_of(600, 400);
        let layer = d.document.active_layer().unwrap();
        // A uniform 128 layer: a downscale is uniform, and its extent obeys the cap.
        let (tw, th, rgba) = d.layer_thumbnail(layer, 64).unwrap();
        assert!(tw <= 64 && th <= 64, "fits max_edge: {tw}x{th}");
        assert_eq!(tw as usize * th as usize * 4, rgba.len());
        assert!(
            rgba.chunks_exact(4).all(|p| *p == [128, 128, 128, 128]),
            "uniform source stays uniform (first px: {:?})",
            &rgba[0..4]
        );
        // Aspect ratio is preserved: 600x400 at edge 64 -> 64x~42.
        assert!((tw as f32 / th as f32 - 1.5).abs() < 0.05, "{tw}x{th}");
        // A document already under the cap is not upscaled.
        let small = doc_of(10, 8);
        let sl = small.document.active_layer().unwrap();
        let (sw, sh, _) = small.layer_thumbnail(sl, 64).unwrap();
        assert_eq!((sw, sh), (10, 8), "never upscales");
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
        let err = d.export_to(&dir.path().join("out.exr")).unwrap_err();
        assert!(err.to_string().contains("exr"), "{err}");
        assert!(!dir.path().join("out.exr").exists(), "nothing was written");
        // A destination with no extension at all names no format either.
        assert!(d.export_to(&dir.path().join("out")).is_err());
    }

    #[test]
    fn a_deep_source_export_stays_sixteen_bit_and_an_eight_bit_source_still_exports_at_eight() {
        // A 16-bit source must not be crushed to eight bits by an export to a
        // format that can carry sixteen (PNG, TIFF); an 8-bit source must keep
        // the long-standing 8-bit path so existing round-trips stay byte-exact.
        let dir = tempfile::tempdir().unwrap();

        let deep = dir.path().join("deep.png");
        let one = vec![
            0xABCDu16, 0x1020, 0x3040, 0xFFFF, // straight alpha, fully opaque
        ];
        raster::encode_to_path(
            &deep,
            raster::ExportFormat::Png,
            1,
            1,
            raster::EncodedPixels::Rgba16(&one),
            &raster::EncodeOptions::default(),
        )
        .unwrap();
        let d = OpenDocument::open_image(DocumentId(100), &deep, 10).unwrap();
        let out = dir.path().join("flattened.png");
        let mut d = d;
        d.export_to(&out).unwrap();
        let back = raster::decode_surface_path(&out, raster::ImportLimits::default()).unwrap();
        assert_eq!(
            back.format(),
            raster::PixelFormat::Rgba16,
            "a deep source must export at 16 bits, not 8"
        );

        let shallow = dir.path().join("shallow.png");
        let eight = vec![128u8, 0, 0, 255];
        raster::encode_to_path(
            &shallow,
            raster::ExportFormat::Png,
            1,
            1,
            raster::EncodedPixels::Rgba8(&eight),
            &raster::EncodeOptions::default(),
        )
        .unwrap();
        let mut s = OpenDocument::open_image(DocumentId(101), &shallow, 10).unwrap();
        assert!(
            !s.source_sixteen_bit,
            "an 8-bit source is remembered as 8-bit"
        );
        let out8 = dir.path().join("flattened8.png");
        s.export_to(&out8).unwrap();
        let back8 = raster::decode_surface_path(&out8, raster::ImportLimits::default()).unwrap();
        assert_eq!(
            back8.format(),
            raster::PixelFormat::Rgba8,
            "an 8-bit source keeps the 8-bit export path"
        );
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

    // ------------------------------------------------------------------ PSD

    /// A two-layer `.psd`: a red background under a half-opaque Multiply layer
    /// inside a group. Small, but it exercises every part of the path — a
    /// group, a blend mode, an opacity and pixels at an offset.
    fn layered_psd_bytes() -> Vec<u8> {
        let (w, h) = (40u32, 30u32);
        let canvas = psd::Rect::sized(w, h);
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(w, h));

        let mut background = psd::PsdLayer::raster("Background", canvas);
        background
            .set_rgba8(&[220u8, 40, 40, 255].repeat((w * h) as usize))
            .unwrap();

        let patch = psd::Rect::new(8, 6, 24, 20);
        let mut top = psd::PsdLayer::raster("Top", patch);
        top.set_rgba8(&[20u8, 220, 90, 255].repeat((patch.width() * patch.height()) as usize))
            .unwrap();
        top.blend_mode = layer_model::BlendMode::Multiply;
        top.opacity = 128;

        let mut group = psd::PsdLayer::group("Folder");
        group.push_child(top).unwrap();

        file.layers = vec![background, group];
        psd::write(&file).expect("the fixture is writable")
    }

    fn layer_names(doc: &Document, ids: &[layer_model::LayerId]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| doc.layers.get(*id).map(|l| l.name.clone()))
            .collect()
    }

    #[test]
    fn file_open_on_a_psd_builds_a_layered_document() {
        // `Editor::open_path` calls exactly this for every non-project path, so
        // this is File ▸ Open, drag-and-drop and the recent-files list at once.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artwork.psd");
        std::fs::write(&path, layered_psd_bytes()).unwrap();

        let d = OpenDocument::open_image(DocumentId(7), &path, 100).unwrap();
        assert_eq!(d.title(), "artwork.psd");
        assert_eq!(
            layer_names(&d.document, d.document.layers.root()),
            ["Folder", "Background"],
            "the layer tree is real, not one flattened frame"
        );
        let folder = d.document.layers.get(d.document.layers.root()[0]).unwrap();
        assert!(folder.is_group());
        let top = d.document.layers.get(folder.children()[0]).unwrap();
        assert_eq!(top.name, "Top");
        assert_eq!(top.blend_mode, layer_model::BlendMode::Multiply);
        assert!((top.opacity - 128.0 / 255.0).abs() < 1e-6);

        assert!(!d.is_dirty(), "opening a file is not unsaved work");
        assert_eq!(d.source_path(), Some(path.as_path()));
        assert!(
            d.psd_notes().is_empty(),
            "nothing was lost: {:?}",
            d.psd_notes()
        );
        // Save As next to it still suggests the project package.
        assert_eq!(d.suggested_save_path(), dir.path().join("artwork.rstudio"));
    }

    #[test]
    fn a_psd_is_recognised_by_its_contents_and_not_by_its_name() {
        let dir = tempfile::tempdir().unwrap();
        // A Photoshop document that somebody renamed.
        let mislabelled = dir.path().join("artwork.png");
        std::fs::write(&mislabelled, layered_psd_bytes()).unwrap();
        let d = OpenDocument::open_image(DocumentId(8), &mislabelled, 10).unwrap();
        assert_eq!(d.document.layers.len(), 3, "it is still a layered document");

        // ...and a PNG that somebody renamed the other way still opens as the
        // picture it is, rather than being sent to the PSD reader.
        let png = raster::encode(raster::ExportFormat::Png, 6, 4, &[90u8; 96]).unwrap();
        let lying = dir.path().join("photo.psd");
        std::fs::write(&lying, &png).unwrap();
        let d = OpenDocument::open_image(DocumentId(9), &lying, 10).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (6, 4));
        assert_eq!(d.document.layers.len(), 1);
    }

    #[test]
    fn a_corrupt_psd_reports_an_error_instead_of_opening_a_blank_document() {
        let dir = tempfile::tempdir().unwrap();
        let good = layered_psd_bytes();
        for (name, bytes) in [
            ("cut.psd", &good[..good.len() / 3]),
            ("head.psd", &good[..20]),
            ("stub.psd", &b"8BPS"[..]),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            let err = OpenDocument::open_image(DocumentId(10), &path, 10)
                .err()
                .unwrap_or_else(|| panic!("{name} must not open"));
            let told = err.to_string();
            assert!(
                told.contains("Photoshop document"),
                "{name} said {told:?} — the user has to learn it was the file"
            );
        }
    }

    #[test]
    fn exporting_to_a_psd_keeps_the_layers_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.psd");
        std::fs::write(&source, layered_psd_bytes()).unwrap();
        let mut d = OpenDocument::open_image(DocumentId(11), &source, 100).unwrap();
        let before = d.composite(d.canvas_rect()).unwrap();

        let out = dir.path().join("out.psd");
        let notes = d.export_psd_to(&out).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert!(out.is_file());
        assert_eq!(&std::fs::read(&out).unwrap()[..4], b"8BPS");

        let mut back = OpenDocument::open_image(DocumentId(12), &out, 100).unwrap();
        assert_eq!(
            layer_names(&back.document, back.document.layers.root()),
            ["Folder", "Background"],
            "a saved .psd reopens with its structure"
        );
        let folder = back
            .document
            .layers
            .get(back.document.layers.root()[0])
            .unwrap();
        assert!(folder.is_group());
        let top = back.document.layers.get(folder.children()[0]).unwrap();
        assert_eq!(top.name, "Top");
        assert_eq!(top.blend_mode, layer_model::BlendMode::Multiply);
        assert_eq!(back.composite(back.canvas_rect()).unwrap(), before);

        // The generic export route reaches the same code, which is what File ▸
        // Export with a `.psd` name does.
        let via_export = dir.path().join("again.psd");
        d.export_to(&via_export).unwrap();
        assert_eq!(
            std::fs::read(&via_export).unwrap(),
            std::fs::read(&out).unwrap()
        );
        assert!(exports_as_psd(&via_export));
        assert!(exports_as_psd(Path::new("X.PSD")));
        assert!(!exports_as_psd(Path::new("x.png")));
    }

    #[test]
    fn a_psd_export_replaces_the_previous_file_without_leaving_a_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.psd");
        std::fs::write(&out, b"the previous version").unwrap();

        let mut d = doc_of(24, 16);
        d.export_to(&out).unwrap();
        assert_eq!(&std::fs::read(&out).unwrap()[..4], b"8BPS");

        // Exporting again over a real file works too, and nothing is left
        // beside it: an export that scatters `.part` files is a bug report.
        d.export_to(&out).unwrap();
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n != "out.psd")
            .collect();
        assert!(strays.is_empty(), "left behind {strays:?}");
    }

    #[test]
    fn a_psd_export_reports_what_it_could_not_write() {
        // A vector-mask layer and a blanket lock: two things this document
        // model has and a `.psd` does not. The file is still written; the user
        // is told what did not go into it.
        let mut d = doc_of(32, 24);
        let id = d.document.active_layer().unwrap();
        {
            let layer = d.document.layers.get_mut(id).unwrap();
            layer.locked.all = true;
            layer.set_mask(layer_model::LayerMask::vector(layer_model::MaskId::new()));
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("noted.psd");
        let notes = d.export_psd_to(&out).unwrap();
        let told = notes.summary().expect("this document loses things");
        assert!(told.contains("blanket lock"), "{told}");
        assert!(out.is_file(), "the file is still written");
        assert_eq!(d.psd_notes().summary(), Some(told));
    }
}
