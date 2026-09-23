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
use editor_core::command::DirtyReach;
use editor_core::{Command, CommandError, Document, History};
use layer_model::{LayerId, LayerKind};
use project_format::{
    CommandJournal, DocumentDigest, ProjectError, SaveOptions, TileBytes, JOURNAL_FILE,
};
use raster::{PixelRect, TileHash, TILE_SIZE};
use render::Camera;

use crate::dirty::DirtyTiles;
use crate::import::{DecodedImage, ImportError, ImportedDocument};

/// Image ▸ Mode ▸ 8/16 Bits/Channel and the 16-bit apply boundary.
#[path = "doc_depth.rs"]
mod depth;
pub use depth::NarrowedReads;

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
    #[error(transparent)]
    Export(#[from] raster::ExportError),
    #[error(transparent)]
    Grid(#[from] raster::GridError),
    #[error(transparent)]
    Pixel(#[from] editor_core::PixelError),
    #[error("writing print output failed: {0}")]
    Io(String),
    /// Card 077: exporting over the imported original would replace a file
    /// from another application with this build's reduced representation of
    /// it. Refused loudly; the user picks another name (or saves native
    /// first).
    #[error(
        "refusing to overwrite the imported original `{0}` — exporting here \
         would replace it with a reduced representation; choose a different \
         file name, or use File > Save As with the native .rstudio format first"
    )]
    OriginalOverwrite(PathBuf),
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

/// `true` when two paths name the same file — canonically when both resolve,
/// case-insensitively otherwise (this build's primary host is Windows).
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}

/// Write `bytes` to `path` without destroying what is already there if the
/// write fails.
///
/// The same rule [`raster::encode_to_path`] follows: a save that dies half way
/// through must not have eaten the previous version. The temporary file is a
/// sibling so the rename stays on one filesystem, and it is removed when the
/// rename does not happen.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
    /// W4-G: the document as opened — the History Brush's default source.
    /// Hashes only: the bytes stay in `tiles`, as undo needs them.
    opened: std::sync::Arc<tools::history_brush::HistorySource>,
    /// W4-G: the Colour Sampler's points, document pixels, placement order.
    /// The document's, as in Photoshop, so they outlive a tool switch; lent to
    /// the tool as `ToolContext::samplers` and read by the Info panel.
    pub(crate) samplers: Vec<glam::Vec2>,
    pub document: Document,
    pub history: History,
    pub tiles: MemoryTileSource,
    pub camera: Camera,
    /// The live preview's override, if a gesture is showing one (card 013).
    /// Never journaled, never saved, cleared when the gesture ends.
    pub(crate) preview: Option<PreviewOverride>,
    /// Bumped by every `set_preview`, so each preview frame's tiles are
    /// cached under keys no earlier frame can serve.
    pub(crate) preview_generation: u64,
    /// W4-B: the live stroke preview — the in-flight stroke's tiles, laid
    /// over their target for display only. Never journaled, never saved,
    /// never in history; cleared when the stroke is released or abandoned.
    pub(crate) paint_preview: Option<PaintPreview>,
    /// Where the `.rstudio` package lives, once it has one.
    project_path: Option<PathBuf>,
    /// The modification times linked asset sources were read at, by asset id:
    /// what "the linked source changed" is measured against.
    pub asset_stamps: std::collections::HashMap<layer_model::AssetId, std::time::SystemTime>,
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
    /// W2-G: while a save of this document's *snapshot* runs on a worker,
    /// commands accepted here are journaled to this side file instead of the
    /// package journal — see [`OpenDocument::begin_journal_hold`].
    journal_hold: Option<PathBuf>,
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

/// Bilinear resample of one 8-bit channel — mask coverage.
///
/// Coverage is not colour: putting it through the linear-light pipeline would
/// gamma-correct a value that means "how much of this pixel is selected", so
/// it gets its own plain-space resampler. Edges clamp, so the outermost
/// half-pixel is a smooth ramp rather than a fade to nothing.
fn resample_coverage(src: &[u8], w: u32, h: u32, dw: u32, dh: u32) -> Vec<u8> {
    if (w, h) == (dw, dh) {
        return src.to_vec();
    }
    let (w, h, dw, dh) = (w as usize, h as usize, dw as usize, dh as usize);
    let at = |x: i64, y: i64| -> f32 {
        let x = x.clamp(0, w as i64 - 1) as usize;
        let y = y.clamp(0, h as i64 - 1) as usize;
        f32::from(src[y * w + x])
    };
    let mut out = vec![0u8; dw * dh];
    let (fx, fy) = (w as f64 / dw as f64, h as f64 / dh as f64);
    for dy in 0..dh {
        let sy = (dy as f64 + 0.5) * fy - 0.5;
        let y0 = sy.floor() as i64;
        let ty = (sy - y0 as f64) as f32;
        for dx in 0..dw {
            let sx = (dx as f64 + 0.5) * fx - 0.5;
            let x0 = sx.floor() as i64;
            let tx = (sx - x0 as f64) as f32;
            let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
            let bottom = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
            let v = top * (1.0 - ty) + bottom * ty;
            out[dy * dw + dx] = (v + 0.5) as u8;
        }
    }
    out
}

/// Bilinear sample of a straight-RGBA8 buffer, premultiplied on the way in and
/// un-premultiplied on the way out.
///
/// Interpolating straight alpha averages colours without weighing them by
/// coverage, which is how a rotated photograph grows dark halos. Outside the
/// buffer reads as transparent, so the rotated corners stay empty.
fn sample_bilinear_premultiplied(rgba: &[u8], w: usize, h: usize, x: f64, y: f64) -> [u8; 4] {
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;
    let at = |xx: i64, yy: i64| -> [f32; 4] {
        if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
            return [0.0; 4];
        }
        let i = (yy as usize * w + xx as usize) * 4;
        let a = rgba[i + 3] as f32 / 255.0;
        [
            rgba[i] as f32 / 255.0 * a,
            rgba[i + 1] as f32 / 255.0 * a,
            rgba[i + 2] as f32 / 255.0 * a,
            a,
        ]
    };
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let mut top = [0.0f32; 4];
    let mut bottom = [0.0f32; 4];
    for c in 0..4 {
        top[c] = lerp(at(x0, y0)[c], at(x0 + 1, y0)[c], fx);
        bottom[c] = lerp(at(x0, y0 + 1)[c], at(x0 + 1, y0 + 1)[c], fx);
    }
    let mut px = [0.0f32; 4];
    for c in 0..4 {
        px[c] = lerp(top[c], bottom[c], fy);
    }
    let a = px[3];
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    if a <= 0.0 {
        return [0, 0, 0, 0];
    }
    [to8(px[0] / a), to8(px[1] / a), to8(px[2] / a), to8(a)]
}

/// One layer's temporary stand-in transform for the live preview (card 013).
/// Held outside the `Document` on purpose: a preview edits nothing, writes no
/// history, and survives no save.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewOverride {
    pub layer: LayerId,
    pub transform: glam::Affine2,
}

/// W4-B: a running stroke's tiles, held beside the document the way
/// [`PreviewOverride`] is: a lens, not an edit. `tiles` maps each previewed
/// coordinate of `key` to the hash the stroke leaves there (`None` = cleared);
/// `bytes` holds exactly the blobs those hashes name, so a preview never adds
/// a blob to the document's own store and drops its bytes with itself.
#[derive(Debug, Clone, Default)]
pub(crate) struct PaintPreview {
    target: Option<editor_core::PixelTarget>,
    key: Option<editor_core::PixelKey>,
    tiles: std::collections::BTreeMap<raster::TileCoord, Option<TileHash>>,
    bytes: std::collections::HashMap<TileHash, Vec<u8>>,
}

impl PaintPreview {
    /// The previewed tiles as a delta over the committed target.
    fn delta(&self) -> Option<editor_core::TileDelta> {
        editor_core::TileDelta::new(self.tiles.iter().map(|(coord, hash)| match hash {
            Some(h) => editor_core::pixels::TileEdit::set(*coord, *h),
            None => editor_core::pixels::TileEdit::clear(*coord),
        }))
        .ok()
    }
}

/// W4-B: the coordinates a stroke preview shows and the hash at each.
#[cfg(test)]
pub(crate) type PreviewedTiles = Vec<(raster::TileCoord, Option<TileHash>)>;

/// The document's tile store with a live stroke's own blobs in front of it
/// (W4-B). Content-addressed on both sides, so a hash names the same bytes
/// wherever it is found and the compositor's cache keys stay honest.
struct PreviewTileSource<'a> {
    base: &'a MemoryTileSource,
    extra: Option<&'a std::collections::HashMap<TileHash, Vec<u8>>>,
}

impl TileSource for PreviewTileSource<'_> {
    fn tile(&self, hash: TileHash) -> Option<&[u8]> {
        self.extra
            .and_then(|extra| extra.get(&hash))
            .map(Vec::as_slice)
            .or_else(|| self.base.tile(hash))
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
            opened: std::sync::Arc::new(tools::history_brush::HistorySource::of(
                &imported.document,
            )),
            samplers: Vec::new(),
            document: imported.document,
            history: imported.history,
            tiles: imported.tiles,
            camera: Camera::new(size, size),
            preview: None,
            preview_generation: 0,
            paint_preview: None,
            project_path: None,
            asset_stamps: std::collections::HashMap::new(),
            source_path: None,
            compositor: TileCompositor::new(),
            // Nothing has been presented yet, so everything is outstanding.
            dirty: DirtyTiles::all(),
            source_sixteen_bit: false,
            psd_notes: crate::import::PsdNotes::default(),
            undone_labels: Vec::new(),
            fit_pending: true,
            journal_hold: None,
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
        let sixteen_bit = {
            // A 16-bit-capable export destination can carry more than eight
            // bits; record the depth the file actually arrived in so the
            // export route below can choose to honor it.
            let surface = raster::decode_surface_path(path, raster::ImportLimits::default())?;
            surface.format() == raster::PixelFormat::Rgba16
        };
        Self::open_image_decoded(id, path, image, history_depth).map(|mut open| {
            open.source_sixteen_bit = sixteen_bit;
            open.document.meta.bit_depth = if sixteen_bit { 16 } else { 8 };
            open
        })
    }

    /// Card 087: build the document an off-thread import job decoded. The
    /// pixels and the title arrive as pure data; the tile construction (the
    /// part that touches the editor's caches) stays on the interaction
    /// thread.
    pub fn open_image_decoded(
        id: DocumentId,
        path: &Path,
        image: DecodedImage,
        history_depth: usize,
    ) -> Result<Self, DocumentError> {
        let title = DecodedImage::title_for(path);
        let imported = crate::import::document_from_image(&image, &title, history_depth)?;
        let mut open = OpenDocument::from_import(id, imported);
        open.source_path = Some(path.to_path_buf());
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
        Self::open_psd_bytes(id, path, &bytes, history_depth)
    }

    /// Card 087: build the layered document from bytes an off-thread job
    /// already read (bounded by the same ceiling the synchronous read
    /// applies); the layered parse stays on the interaction thread.
    pub fn open_psd_bytes(
        id: DocumentId,
        path: &Path,
        bytes: &[u8],
        history_depth: usize,
    ) -> Result<Self, DocumentError> {
        let title = DecodedImage::title_for(path);
        let import = crate::import::document_from_psd(bytes, &title, history_depth)?;
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
            opened: std::sync::Arc::new(tools::history_brush::HistorySource::of(&document)),
            samplers: Vec::new(),
            document,
            history: History::with_limit(history_depth),
            tiles,
            camera: Camera::new(size, size),
            preview: None,
            preview_generation: 0,
            paint_preview: None,
            project_path: Some(path.to_path_buf()),
            asset_stamps: std::collections::HashMap::new(),
            source_path: None,
            compositor: TileCompositor::new(),
            dirty: DirtyTiles::all(),
            source_sixteen_bit: false,
            psd_notes: crate::import::PsdNotes::default(),
            undone_labels: Vec::new(),
            fit_pending: true,
            journal_hold: None,
        })
    }

    /// A blank document — File ▸ New, with the transparency checkerboard.
    pub fn blank(
        id: DocumentId,
        width: u32,
        height: u32,
        title: &str,
        history_depth: usize,
    ) -> Result<Self, DocumentError> {
        OpenDocument::blank_with_background(
            id,
            width,
            height,
            title,
            history_depth,
            crate::import::BlankBackground::Transparent,
        )
    }

    /// A blank document whose base layer starts filled — the New Document
    /// dialog's background choice.
    pub fn blank_with_background(
        id: DocumentId,
        width: u32,
        height: u32,
        title: &str,
        history_depth: usize,
        background: crate::import::BlankBackground,
    ) -> Result<Self, DocumentError> {
        let mut doc = OpenDocument::from_import(
            id,
            crate::import::blank_document(width, height, title, history_depth, background)?,
        );
        // A 16-bit solid background arrives as RGBA16 tiles; the document's
        // depth says so, so edits and exports stay at 16 bits.
        if let crate::import::BlankBackground::Solid { depth, .. } = background {
            doc.set_initial_bit_depth(depth);
        }
        Ok(doc)
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
            opened: std::sync::Arc::new(tools::history_brush::HistorySource::of(&doc)),
            samplers: Vec::new(),
            document: doc,
            history: History::default(),
            tiles: self.tiles.clone(),
            camera: Camera::new(size, size),
            preview: None,
            preview_generation: 0,
            paint_preview: None,
            project_path: None,
            asset_stamps: std::collections::HashMap::new(),
            source_path: None,
            compositor: TileCompositor::new(),
            dirty: DirtyTiles::all(),
            source_sixteen_bit: self.source_sixteen_bit,
            psd_notes: crate::import::PsdNotes::default(),
            undone_labels: Vec::new(),
            fit_pending: true,
            journal_hold: None,
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

    /// Whether the document works at 16 bits per channel — a 16-bit source,
    /// a 16-bit New Document, or Image ▸ Mode ▸ 16 Bits/Channel. Drives the
    /// File Info window and the 16-bit export route; Image ▸ Mode ▸ 8
    /// Bits/Channel turns it off again.
    pub fn is_sixteen_bit(&self) -> bool {
        self.document.meta.bit_depth == 16
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
        // A 16-bit document keeps its layer tiles 16-bit when a tool hands it
        // RGBA8 output (see `doc_depth.rs`); an 8-bit document is untouched.
        let command = depth::fit_to_document_depth(&self.document, &mut self.tiles, command);
        self.history.apply(&mut self.document, command.clone())?;
        // A new command drops the redo stack (standard linear history), so the
        // labels mirroring it go too.
        self.undone_labels.clear();
        self.dirty.record(&command);
        // Only after the command was accepted: `project-format` requires the
        // journal to record commands that are already in the document, or a
        // recovery would replay one the snapshot never had.
        //
        // W2-G: while a save of this document's snapshot is in flight, the
        // record goes to the hold's side file, never to the package journal:
        // the worker copies that journal's prefix at a moment this thread
        // cannot see and the swap then deletes the old one, so a record put
        // there now would either be filed ahead of the new save marker (and
        // never replayed) or deleted with the old package. The side file is
        // absorbed after the marker once the save has landed — see
        // [`OpenDocument::begin_journal_hold`].
        let journal = match (&self.journal_hold, &self.project_path) {
            (Some(side), _) => Some(side.clone()),
            (None, Some(project)) => Some(project.join(JOURNAL_FILE)),
            (None, None) => None,
        };
        if let Some(journal) = journal {
            if let Err(e) = CommandJournal::append(&journal, &command) {
                // A journal that cannot be written costs crash recovery, not
                // the edit — the edit is already in the document.
                tracing::warn!("cannot journal the command: {e}");
            }
        }
        Ok(())
    }

    /// Undo one step. Reports whether anything was undone.
    ///
    /// Invalidates only what the step reaches: the inverse `History` is about
    /// to apply names its layers and tiles ([`Command::dirty_reach`]), and
    /// their on-canvas extent is read **before and after** the inverse runs,
    /// so a moved layer's old spot is redrawn as well as its new one. A step
    /// whose reach is not a rectangle (a re-order, a canvas resize) still
    /// marks the whole canvas — see `crate::dirty`.
    pub fn undo(&mut self) -> Result<bool, CommandError> {
        // Read the label *before* the step moves off the done stack.
        let label = self.history.undo_label().map(str::to_string);
        let reach = self.history.peek_undo().map(Command::dirty_reach);
        let before = reach.as_ref().map(|r| self.reach_tiles(r));
        let undone = self.history.undo(&mut self.document)?;
        if undone {
            self.undone_labels
                .push(label.unwrap_or_else(|| "Step".to_string()));
            self.mark_step(reach, before);
        }
        Ok(undone)
    }

    /// Redo one step. Reports whether anything was redone. Invalidates on the
    /// same terms as [`OpenDocument::undo`].
    pub fn redo(&mut self) -> Result<bool, CommandError> {
        let reach = self.history.peek_redo().map(Command::dirty_reach);
        let before = reach.as_ref().map(|r| self.reach_tiles(r));
        let redone = self.history.redo(&mut self.document)?;
        if redone {
            self.undone_labels.pop();
            self.mark_step(reach, before);
        }
        Ok(redone)
    }

    /// Record an undo/redo step's invalidation: the tiles its reach covered
    /// before the step (`before`) plus the ones it covers now.
    fn mark_step(&mut self, reach: Option<DirtyReach>, before: Option<DirtyTiles>) {
        match (reach, before) {
            (Some(reach), Some(before)) => {
                self.dirty.merge(&before);
                if !self.dirty.is_all() {
                    let after = self.reach_tiles(&reach);
                    self.dirty.merge(&after);
                }
            }
            // The step applied but nothing was peeked first — not reachable
            // through `History`'s API, but the whole canvas is the honest
            // answer if it ever is.
            _ => self.dirty.mark_all(),
        }
    }

    /// The presenter tiles `reach` invalidates, read against the document
    /// **as it is now**. Called once before a step and once after, because a
    /// layer's extent is different on the two sides of a move, a delete or a
    /// text edit.
    ///
    /// Every answer here is a superset of the pixels that change, never a
    /// guess: the moment a layer's extent cannot be stated as rectangles in
    /// document space — an adjustment (it reaches everything beneath it), a
    /// group (its children's own effects are not in its bounds), a layer under
    /// a transformed or styled ancestor (its bounds are in the parent's
    /// space), or a compositor query that fails — the answer is the whole
    /// canvas.
    fn reach_tiles(&self, reach: &DirtyReach) -> DirtyTiles {
        if reach.is_everything() {
            return DirtyTiles::all();
        }
        let mut out = DirtyTiles::none();
        for layer in reach.layers() {
            if !self.mark_layer_extent(&mut out, layer) {
                return DirtyTiles::all();
            }
        }
        for (layer, mask, coords) in reach.tile_groups() {
            if !self.extent_is_rectangular(layer) {
                return DirtyTiles::all();
            }
            let coords: Vec<raster::TileCoord> = coords.iter().copied().collect();
            match compositor::bounds::edited_tiles_on_document(
                &self.document,
                &self.tiles,
                layer,
                mask,
                &coords,
                CompositeOptions::default(),
            ) {
                Ok(Some(rects)) => {
                    for r in rects {
                        self.mark_rect_tiles(&mut out, r);
                    }
                }
                Ok(None) | Err(_) => return DirtyTiles::all(),
            }
        }
        out
    }

    /// Mark the styled extent of `layer` — and of every member of its
    /// clipping group, since a clipped layer shows through its base's shape
    /// and the base's shape decides what the clipped layers show. `false`
    /// when the extent is not rectangular and the caller must mark all.
    fn mark_layer_extent(&self, out: &mut DirtyTiles, layer: LayerId) -> bool {
        if !self.document.layers.contains(layer) {
            // Not there on this side of the step (created by it, or deleted
            // by it): nothing to mark on this side.
            return true;
        }
        let members: Vec<LayerId> = match self.document.layers.clipping_group(layer) {
            Some(group) => std::iter::once(group.base)
                .chain(group.clipped.iter().copied())
                .collect(),
            None => vec![layer],
        };
        for id in members {
            if !self.extent_is_rectangular(id) {
                return false;
            }
            match compositor::bounds::styled_bounds(
                &self.document,
                &self.tiles,
                id,
                0,
                CompositeOptions::default(),
            ) {
                Ok(Some(rect)) => self.mark_rect_tiles(out, rect),
                Ok(None) => {}
                Err(_) => return false,
            }
        }
        true
    }

    /// Whether `layer`'s pixels on the canvas are confined to rectangles
    /// `compositor::bounds` can state in document space.
    fn extent_is_rectangular(&self, layer: LayerId) -> bool {
        let Some(l) = self.document.layers.get(layer) else {
            return true;
        };
        match l.kind {
            // An adjustment rewrites every pixel beneath it; a group's bounds
            // do not include its children's own effects.
            LayerKind::Adjustment(_) | LayerKind::Group(_) => return false,
            LayerKind::Raster(_)
            | LayerKind::Generator(_)
            | LayerKind::SmartObject(_)
            | LayerKind::Text(_)
            | LayerKind::Shape(_) => {}
        }
        // `styled_bounds` maps through the layer's own transform only, so the
        // answer is in the parent's space. It is document space exactly when
        // no ancestor moves or styles its children.
        let mut cursor = self.document.layers.parent_of(layer);
        while let Some(pid) = cursor {
            let Some(parent) = self.document.layers.get(pid) else {
                return false;
            };
            let moved = !parent.transform.abs_diff_eq(glam::Affine2::IDENTITY, 1e-6);
            if moved || parent.effects.affects_composite() {
                return false;
            }
            cursor = self.document.layers.parent_of(pid);
        }
        true
    }

    /// Mark every level-0 tile `rect` touches, clamped to the canvas.
    fn mark_rect_tiles(&self, out: &mut DirtyTiles, rect: PixelRect) {
        let x0 = rect.x.max(0);
        let y0 = rect.y.max(0);
        let x1 = rect.right().min(i64::from(self.document.width()));
        let y1 = rect.bottom().min(i64::from(self.document.height()));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let t = i64::from(TILE_SIZE);
        for ty in y0.div_euclid(t)..=(y1 - 1).div_euclid(t) {
            for tx in x0.div_euclid(t)..=(x1 - 1).div_euclid(t) {
                out.insert(raster::TileCoord::new(tx as i32, ty as i32, 0));
            }
        }
    }

    /// How many commands are applied right now — the point on the timeline the
    /// document stands at.
    pub fn history_depth(&self) -> usize {
        self.history.undo_depth()
    }

    /// W4-G: the document as it was opened (or created, or duplicated) —
    /// what the History Brush paints back by default.
    pub fn opened_state(&self) -> std::sync::Arc<tools::history_brush::HistorySource> {
        std::sync::Arc::clone(&self.opened)
    }

    /// W4-G: the History Brush source the user picked: History panel row
    /// `row` (`0` is the document as opened, always available even once the
    /// oldest steps have been compacted away; `n` is the state after the
    /// `n`th step of [`OpenDocument::history_timeline`], undone steps
    /// included). The state is rebuilt on a copy — the document and its
    /// history are not touched — by undoing or redoing from where the
    /// document stands. `None` for a row past the end of the timeline.
    pub fn history_state(
        &self,
        row: usize,
    ) -> Option<std::sync::Arc<tools::history_brush::HistorySource>> {
        if row == 0 {
            return Some(self.opened_state());
        }
        let here = self.history.undo_depth();
        if row > here + self.history.redo_depth() {
            return None;
        }
        if row == here {
            return Some(std::sync::Arc::new(
                tools::history_brush::HistorySource::of(&self.document),
            ));
        }
        let mut document = self.document.clone();
        let mut history = self.history.clone();
        for _ in row..here {
            if !history.undo(&mut document).ok()? {
                return None;
            }
        }
        for _ in here..row {
            if !history.redo(&mut document).ok()? {
                return None;
            }
        }
        Some(std::sync::Arc::new(
            tools::history_brush::HistorySource::of(&document),
        ))
    }

    /// W4-G: the Colour Sampler's points on this document.
    pub fn samplers(&self) -> &[glam::Vec2] {
        &self.samplers
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
    /// Composite `region` as the document stands, plus any live preview
    /// override.
    ///
    /// The preview generation rides [`CompositeOptions`], so preview tiles are
    /// cached under keys no committed frame can hit and no earlier preview can
    /// serve; the override reaches the compositor through
    /// `composite_region_with`, which reads the committed document and writes
    /// nothing.
    ///
    /// W4-B: a live stroke preview is laid over its target for the length of
    /// this call — its tiles swapped into the target's map, composited, and
    /// swapped straight back — so the frame shows the stroke while the
    /// committed document is, on return, exactly what it was. The tile cache
    /// is keyed by content hashes, so only the tiles the stroke reaches are
    /// recomposited.
    pub fn composite(&mut self, region: PixelRect) -> Result<Vec<u8>, DocumentError> {
        let lens = self
            .paint_preview
            .as_ref()
            .and_then(|p| Some((p.key?, p.delta()?)));
        let restore = lens.map(|(key, delta)| (key, self.document.pixels.apply(key, &delta)));
        let result = self.composite_lensed(region);
        if let Some((key, previous)) = restore {
            self.document.pixels.apply(key, &previous);
        }
        result
    }

    /// [`Self::composite`] against the document as it stands right now, the
    /// transform preview's override and the stroke preview's blobs included.
    fn composite_lensed(&mut self, region: PixelRect) -> Result<Vec<u8>, DocumentError> {
        let source = PreviewTileSource {
            base: &self.tiles,
            extra: self.paint_preview.as_ref().map(|p| &p.bytes),
        };
        let opts = CompositeOptions {
            preview_generation: self.preview_generation,
            ..Default::default()
        };
        let over = self.preview.map(|p| compositor::LayerOverride {
            layer: p.layer,
            transform: p.transform,
            generation: self.preview_generation,
        });
        let canvas = match over {
            Some(over) => compositor::composite_region_with(
                &self.document,
                &source,
                region,
                0,
                opts,
                Some(over),
            )?,
            None => self
                .compositor
                .composite_region(&self.document, &source, region, 0, opts)?,
        };
        Ok(canvas.to_rgba8(&self.document.meta.color_space))
    }

    /// Show `layer` transformed by `transform` for this document's frames
    /// until [`Self::clear_preview`] (card 013). No history entry, no dirty
    /// flag, nothing journaled: a preview is a lens, not an edit.
    pub fn set_preview(&mut self, layer: LayerId, transform: glam::Affine2) {
        self.preview = Some(PreviewOverride { layer, transform });
        self.preview_generation += 1;
    }

    /// End the live preview. The committed document is exactly what it was
    /// before it started.
    pub fn clear_preview(&mut self) {
        self.preview = None;
    }

    /// Card 039: whether a preview lens is live on this document.
    pub fn has_preview(&self) -> bool {
        self.preview.is_some()
    }

    /// W4-B: fold one live stroke answer into this document's stroke
    /// preview, and invalidate the tiles whose previewed hash changed —
    /// through the same reach machinery an undo uses, so the presenter
    /// recomposites and re-uploads only what the stroke reached. An answer
    /// that replaces the preview on the same target is diffed against the
    /// preview it replaces, hash by hash, so a tile that comes back
    /// unchanged is not invalidated; only a preview of another target is
    /// dropped wholesale. No history entry, no dirty flag, nothing journaled.
    ///
    /// Reports how many tile coordinates of the target changed.
    pub fn update_paint_preview(&mut self, live: tools::stroke::LivePaint) -> usize {
        use tools::stroke::LiveTile;
        let mut touched = std::collections::BTreeSet::new();
        let same_target = self
            .paint_preview
            .as_ref()
            .is_some_and(|p| p.key == Some(live.key) && p.target == Some(live.target));
        // What a replacing answer is diffed against: the tiles it replaces.
        let mut replaced = None;
        if live.replace || !same_target {
            if let Some(old) = self.paint_preview.take() {
                if same_target {
                    replaced = Some(old.tiles);
                } else if let Some(target) = old.target {
                    self.mark_preview_tiles(target, old.tiles.keys().copied());
                }
            }
            self.paint_preview = Some(PaintPreview {
                target: Some(live.target),
                key: Some(live.key),
                ..PaintPreview::default()
            });
        }
        let preview = self.paint_preview.as_mut().expect("installed above");
        for (coord, tile) in live.tiles {
            let now = match tile {
                LiveTile::Committed => None,
                LiveTile::Cleared => Some(None),
                LiveTile::Bytes(bytes) => {
                    // In a 16-bit document the release's RGBA8 tiles are
                    // widened at the apply boundary (`depth::fit_to_document_depth`,
                    // via `raster::widen_rgba8_over`); the preview widens its
                    // tiles the same way, so what it shows is what commits.
                    let bytes = match live.target {
                        editor_core::PixelTarget::Layer(layer)
                            if self.document.meta.bit_depth == 16 =>
                        {
                            let old = self
                                .document
                                .layer_tiles(layer)
                                .and_then(|m| m.get(coord))
                                .and_then(|h| self.tiles.tile(h));
                            raster::widen_rgba8_over(&bytes, old).unwrap_or(bytes)
                        }
                        _ => bytes,
                    };
                    let hash = TileHash::of(&bytes);
                    preview.bytes.entry(hash).or_insert(bytes);
                    Some(Some(hash))
                }
            };
            let before = match now {
                Some(state) => preview.tiles.insert(coord, state),
                None => preview.tiles.remove(&coord),
            };
            if before != now {
                touched.insert(coord);
            }
        }
        if let Some(replaced) = replaced {
            touched = replaced
                .keys()
                .chain(preview.tiles.keys())
                .copied()
                .filter(|coord| replaced.get(coord) != preview.tiles.get(coord))
                .collect();
        }
        let live_hashes: std::collections::HashSet<TileHash> =
            preview.tiles.values().flatten().copied().collect();
        preview.bytes.retain(|hash, _| live_hashes.contains(hash));
        let count = touched.len();
        self.mark_preview_tiles(live.target, touched);
        count
    }

    /// W4-B: end the stroke preview, invalidating the tiles it covered. The
    /// committed document is exactly what it was before the stroke started
    /// (or what the release committed).
    pub fn clear_paint_preview(&mut self) {
        if let Some(old) = self.paint_preview.take() {
            if let Some(target) = old.target {
                self.mark_preview_tiles(target, old.tiles.keys().copied());
            }
        }
    }

    /// W4-B: whether a stroke preview is live on this document.
    pub fn has_paint_preview(&self) -> bool {
        self.paint_preview.is_some()
    }

    /// W4-B: the live stroke preview's tiles — its pixel key and, per
    /// coordinate, the hash it shows (`None` = cleared). For tests that pin
    /// the preview against what the release commits.
    #[cfg(test)]
    pub(crate) fn paint_preview_tiles(&self) -> Option<(editor_core::PixelKey, PreviewedTiles)> {
        let p = self.paint_preview.as_ref()?;
        Some((p.key?, p.tiles.iter().map(|(c, h)| (*c, *h)).collect()))
    }

    /// Invalidate the canvas tiles `coords` of `target` reach, with the reach
    /// machinery undo and redo use (`reach_tiles`).
    fn mark_preview_tiles(
        &mut self,
        target: editor_core::PixelTarget,
        coords: impl IntoIterator<Item = raster::TileCoord>,
    ) {
        let edits: Vec<editor_core::pixels::TileEdit> = coords
            .into_iter()
            .map(editor_core::pixels::TileEdit::clear)
            .collect();
        if edits.is_empty() {
            return;
        }
        let Ok(delta) = editor_core::TileDelta::new(edits) else {
            self.dirty.mark_all();
            return;
        };
        let reach = Command::PaintTiles { target, delta }.dirty_reach();
        let tiles = self.reach_tiles(&reach);
        self.dirty.merge(&tiles);
    }

    /// Apply a live text draft (card 025): the layer's kind becomes `kind`
    /// **outside history** — no entry, nothing journaled — so the canvas
    /// renders every keystroke while undo stays clean. The session's confirm
    /// lands the one `SetLayerKind` history entry (the document already shows
    /// the draft, so the command is the reconciliation), and its cancel
    /// applies the original payload through this same route. Refused when the
    /// layer is gone or the payload fails its own validation.
    pub fn apply_text_draft(
        &mut self,
        layer: LayerId,
        kind: LayerKind,
    ) -> Result<(), DocumentError> {
        let Some(l) = self.document.layers.get(layer) else {
            return Err(DocumentError::Command(CommandError::InvalidPayload {
                reason: "the layer is gone".to_string(),
            }));
        };
        // The same blanket lock `SetLayerKind` enforces: a locked layer must
        // not be mutated even by a live draft, or confirm would fail later
        // and strand the draft (card 025).
        if l.locked.all {
            return Err(DocumentError::Command(CommandError::LayerLocked(layer)));
        }
        // The same-class guard `SetLayerKind` enforces: a draft may retype a
        // text layer, never convert a raster/group/adjustment one in place.
        if std::mem::discriminant(&l.kind) != std::mem::discriminant(&kind) {
            return Err(DocumentError::Command(CommandError::InvalidPayload {
                reason: "a text draft cannot change the layer's class".to_string(),
            }));
        }
        let LayerKind::Text(text) = &kind else {
            return Err(DocumentError::Command(CommandError::InvalidPayload {
                reason: "a text draft must be a text layer".to_string(),
            }));
        };
        text.validate().map_err(|e| {
            DocumentError::Command(CommandError::InvalidPayload {
                reason: e.to_string(),
            })
        })?;
        // A keystroke redraws the text's own extent, not the canvas: the
        // styled ink box before the edit (the glyphs being replaced) and
        // after it (the glyphs now shown), on the same terms as an undo.
        let reach = DirtyReach::layer(layer);
        let before = self.reach_tiles(&reach);
        let Some(l) = self.document.layers.get_mut(layer) else {
            return Err(DocumentError::Command(CommandError::InvalidPayload {
                reason: "the layer is gone".to_string(),
            }));
        };
        l.kind = kind;
        self.mark_step(Some(reach), Some(before));
        Ok(())
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
    /// layer draws on its own — every layer that is not this one or one of
    /// its ancestors hidden, this one forced visible — with its effects and
    /// blending honoured by the real compositor. Returns `(width, height,
    /// rgba8)`, both reduced to keep `width,height <= max_edge` (box-sampled,
    /// never upscaled). `&self` (free compositor, not the cache) so a frame
    /// with only an immutable borrow can upload one per layer.
    ///
    /// **This call is not cached.** It composites, and the Layers panel must
    /// not call it every frame: the panel goes through
    /// [`LayerThumbCache::layer_thumbnail`], which serves a stored thumbnail
    /// until the layer's [`LayerThumbCache::layer_fingerprint`] changes and
    /// calls this only then.
    ///
    /// What is composited is the layer's *styled bounds* clipped to the
    /// canvas, in tile-row bands, each band folded into the thumbnail as it
    /// comes — never the whole canvas as one level-0 buffer (a 4000x3000
    /// document is 192 MB of f32 canvas that way). Pixels outside the bounds
    /// are transparent by construction, so the result is what a whole-canvas
    /// composite followed by [`OpenDocument::box_downscale`] gives (pinned by
    /// `a_content_bounded_thumbnail_matches_the_whole_canvas_downscale`). The
    /// tile store holds level-0 tiles only (`import` emits level 0, the
    /// presenter minifies on the CPU), so a composite at a mip level would
    /// draw every raster layer empty; bands are how the cost is bounded.
    pub fn layer_thumbnail(
        &self,
        layer_id: layer_model::LayerId,
        max_edge: u32,
    ) -> Result<(u32, u32, Vec<u8>), DocumentError> {
        let staged = self.staged_for_thumbnail(layer_id);
        let (w, h) = (self.document.width(), self.document.height());
        let (tw, th) = Self::thumb_extent(w, h, max_edge);
        let mut acc = ThumbAccumulator::new(w, h, tw, th);
        let canvas = self.canvas_rect();
        let bounds = compositor::bounds::styled_bounds(
            &staged,
            &self.tiles,
            layer_id,
            0,
            CompositeOptions::default(),
        )?
        .and_then(|b| intersect_rects(b, canvas));
        if let Some(bounds) = bounds {
            let band = i64::from(TILE_SIZE);
            let mut y = bounds.y;
            let y_end = y + i64::from(bounds.height);
            while y < y_end {
                let rows = band.min(y_end - y);
                let region = PixelRect::new(bounds.x, y, bounds.width, rows as u32);
                THUMB_COMPOSITES.with(|c| c.set(c.get() + 1));
                let canvas = compositor::composite_region(
                    &staged,
                    &self.tiles,
                    region,
                    0,
                    CompositeOptions::default(),
                )?;
                let rgba8 = canvas.to_rgba8(&self.document.meta.color_space);
                acc.add_region(&rgba8, region);
                y += rows;
            }
        }
        Ok((tw, th, acc.finish()))
    }

    /// The document with `layer_id` and its ancestors visible and every other
    /// layer hidden: what a thumbnail of that layer alone composites. The
    /// ancestors are shown because a hidden group hides its children; the
    /// layer itself because the panel shows what a layer *holds*, not whether
    /// it is currently shown. Descendants keep their own visibility.
    fn staged_for_thumbnail(&self, layer_id: layer_model::LayerId) -> Document {
        let mut staged = self.document.clone();
        let mut keep = std::collections::HashSet::new();
        let mut cursor = Some(layer_id);
        while let Some(id) = cursor {
            keep.insert(id);
            cursor = staged.layers.parent_of(id);
        }
        // The subtree under the layer is left alone: a group's thumbnail is
        // its children as the user has them.
        let mut subtree = std::collections::HashSet::new();
        let mut stack = vec![layer_id];
        while let Some(id) = stack.pop() {
            if let Some(l) = staged.layers.get(id) {
                stack.extend(l.children().iter().copied());
            }
            subtree.insert(id);
        }
        for other in staged.layers.iter_depth_first() {
            if other != layer_id && subtree.contains(&other) {
                continue;
            }
            if let Some(l) = staged.layers.get_mut(other) {
                l.visible = keep.contains(&other);
            }
        }
        staged
    }

    /// The thumbnail extent [`OpenDocument::box_downscale`] would settle on
    /// for a `w`x`h` source under `max_edge`: same rounding, so the two paths
    /// agree pixel for pixel.
    fn thumb_extent(w: u32, h: u32, max_edge: u32) -> (u32, u32) {
        let m = w.max(h);
        if max_edge == 0 || m <= max_edge || w == 0 || h == 0 {
            return (w, h);
        }
        let scale = max_edge as f64 / m as f64;
        (
            ((w as f64 * scale).round() as u32).max(1),
            ((h as f64 * scale).round() as u32).max(1),
        )
    }

    /// Card 059: the layer's MASK coverage as a grayscale thumbnail — the
    /// pose-sampled store read (card 058's read_mask_coverage), black hidden
    /// to white revealed, opaque, downscaled like
    /// [`OpenDocument::layer_thumbnail`] takes. Not cached here either; the
    /// panel goes through [`LayerThumbCache::mask_thumbnail`].
    pub fn mask_thumbnail(
        &self,
        layer_id: layer_model::LayerId,
        max_edge: u32,
    ) -> Result<(u32, u32, Vec<u8>), DocumentError> {
        let (w, h) = (self.document.width(), self.document.height());
        let mut coverage = crate::menu_bridge::read_mask_coverage(self, layer_id, w, h);
        // Card 059 (review round 1): the compositor applies the mask's
        // INVERTED flag on top of the store — the thumbnail must present the
        // same field the composite is masked with, not the raw store.
        if self
            .document
            .layers
            .get(layer_id)
            .and_then(|l| l.mask.as_ref())
            .is_some_and(|m| m.inverted)
        {
            coverage.iter_mut().for_each(|v| *v = 255 - *v);
        }
        // Grayscale, fully opaque: the mask plane as an image.
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for &v in &coverage {
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
        Ok(Self::box_downscale(&rgba, w, h, max_edge))
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

    /// Rotate the whole canvas 90° clockwise (or anticlockwise), swapping the
    /// dimensions and rotating every layer's pixels within one undoable
    /// transaction — same shape as [`Self::resize_canvas`].
    pub fn rotate_canvas_90(&mut self, clockwise: bool) -> Result<Command, DocumentError> {
        let old_w = self.document.width();
        let old_h = self.document.height();
        let (new_w, new_h) = (old_h, old_w);
        let ids: Vec<layer_model::LayerId> = self.document.layers.iter_depth_first();
        let mut commands = Vec::new();
        commands.push(Command::SetCanvasSize {
            size: glam::UVec2::new(new_w, new_h),
        });
        for id in ids {
            let rgba = self.layer_pixels(id)?;
            let mut out = vec![0u8; new_w as usize * new_h as usize * 4];
            for y in 0..old_h {
                for x in 0..old_w {
                    let (nx, ny) = if clockwise {
                        (y, old_w - 1 - x)
                    } else {
                        (old_h - 1 - y, x)
                    };
                    let si = (y as usize * old_w as usize + x as usize) * 4;
                    let di = (ny as usize * new_w as usize + nx as usize) * 4;
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
            label: "Rotate 90°".to_string(),
            commands,
        })
    }

    /// Image ▸ Rotation ▸ Arbitrary: rotate the canvas and every layer by
    /// `degrees` (positive clockwise) as one undoable step.
    ///
    /// Only the **non-orthogonal** angles come through here: the shell routes
    /// exact right angles to the same fixed commands the menu's 90/180 items
    /// use, so a 90° asked for through this dialog produces byte-identical
    /// pixels to the fixed one. Everything else resamples: the canvas grows to
    /// the rotated bounding box, each layer's pixels are inverse-mapped with a
    /// premultiplied-alpha bilinear sampler, and empty corners stay
    /// transparent.
    pub fn rotate_canvas_arbitrary(&mut self, degrees: f64) -> Result<Command, DocumentError> {
        let rad = degrees.to_radians();
        let (sin, cos) = rad.sin_cos();
        let (w, h) = (self.document.width(), self.document.height());
        let new_w = ((w as f64 * cos.abs() + h as f64 * sin.abs()).ceil() as u32).max(1);
        let new_h = ((w as f64 * sin.abs() + h as f64 * cos.abs()).ceil() as u32).max(1);
        let mut commands = vec![Command::SetCanvasSize {
            size: glam::UVec2::new(new_w, new_h),
        }];
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let (ncx, ncy) = (new_w as f64 / 2.0, new_h as f64 / 2.0);
        for id in self.document.layers.iter_depth_first() {
            let rgba = self.layer_pixels(id)?;
            let mut out = vec![0u8; new_w as usize * new_h as usize * 4];
            for y in 0..new_h {
                for x in 0..new_w {
                    // Inverse map: where in the old image did this new pixel
                    // come from? Rotating the offset back by −θ about the
                    // centres. Half-pixel centres keep the rotation symmetric.
                    let dx = x as f64 + 0.5 - ncx;
                    let dy = y as f64 + 0.5 - ncy;
                    let sx = cos * dx + sin * dy + cx;
                    let sy = -sin * dx + cos * dy + cy;
                    let px = sample_bilinear_premultiplied(
                        &rgba,
                        w as usize,
                        h as usize,
                        sx - 0.5,
                        sy - 0.5,
                    );
                    let di = (y as usize * new_w as usize + x as usize) * 4;
                    out[di..di + 4].copy_from_slice(&px);
                }
            }
            let grid = raster::TileGrid::from_rgba8(new_w, new_h, &out)?;
            let mut edits = Vec::new();
            for (coord, tile) in grid.iter() {
                let hash = self.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::TileEdit::set(coord, hash));
            }
            commands.push(Command::paint_tiles(
                editor_core::PixelTarget::Layer(id),
                edits,
            )?);
        }
        Ok(Command::Transaction {
            label: "Rotate Canvas".to_string(),
            commands,
        })
    }

    /// Build the Image ▸ Reveal All command: grow the canvas so every layer's
    /// content fits, as one undoable step.
    ///
    /// The union of the pixel layers' content bounds — each tile map's extent
    /// pushed through the layer's own transform — is compared against the
    /// canvas. Growth never moves the origin: content at negative coordinates
    /// brings a translation of every root layer along, the same way a crop's
    /// inverse would. Nothing is filled; the exposed area stays transparent —
    /// Reveal All changes the frame, not the picture.
    ///
    /// Measured with each layer's own transform only: a layer inside a
    /// *transformed group* is measured in the group's frame, which can
    /// under-measure it. That is a named limitation, not a silent one — the
    /// common cases (a layer dragged partly off the canvas, an oversized
    /// paste) are measured exactly.
    pub fn reveal_all_command(&mut self) -> Result<Command, DocumentError> {
        let (w, h) = (self.document.width(), self.document.height());
        let (mut min_x, mut min_y) = (i64::MAX, i64::MAX);
        let (mut max_x, mut max_y) = (i64::MIN, i64::MIN);
        for id in self.document.layers.iter_depth_first() {
            let Some(layer) = self.document.layers.get(id) else {
                continue;
            };
            // Only kinds that own a tile map have measurable pixel bounds;
            // text and shape bounds live in their own parametric geometry.
            if !matches!(
                layer.kind,
                layer_model::LayerKind::Raster(_) | layer_model::LayerKind::Generator(_)
            ) {
                continue;
            }
            let Some(map) = self.document.pixels.tiles(editor_core::PixelKey::Layer(id)) else {
                continue;
            };
            // Content bounds, not tile bounds: a tile's extent is tile-
            // aligned, so an 8×8 image on tile (0,0) would measure as 256×256
            // and Reveal All would quadruple the canvas. Scanning each tile's
            // alpha for the non-transparent rect is exact and bounded by the
            // tile size.
            let mut x0 = i64::MAX;
            let mut y0 = i64::MAX;
            let mut x1 = i64::MIN;
            let mut y1 = i64::MIN;
            let ts = TILE_SIZE as usize;
            for (coord, hash) in map.iter() {
                let Some(stored) = self.tiles.tile(hash) else {
                    continue;
                };
                // W4-F: a 16-bit tile's alpha is read at 8 bits.
                let bytes = raster::rgba8_view(stored);
                if bytes.len() < ts * ts * 4 {
                    continue;
                }
                let (ox, oy) = coord.pixel_origin();
                let (mut lx0, mut ly0) = (ts, ts);
                let (mut lx1, mut ly1) = (0usize, 0usize);
                let mut any = false;
                for ty in 0..ts {
                    let row = ty * ts;
                    for tx in 0..ts {
                        if bytes[(row + tx) * 4 + 3] != 0 {
                            any = true;
                            lx0 = lx0.min(tx);
                            ly0 = ly0.min(ty);
                            lx1 = lx1.max(tx + 1);
                            ly1 = ly1.max(ty + 1);
                        }
                    }
                }
                if !any {
                    continue;
                }
                x0 = x0.min(ox + lx0 as i64);
                y0 = y0.min(oy + ly0 as i64);
                x1 = x1.max(ox + lx1 as i64);
                y1 = y1.max(oy + ly1 as i64);
            }
            if x0 > x1 {
                continue;
            }
            // Push the pixel-space rect through the layer's transform: the
            // four corners mapped, then the bounding box of those.
            let t = layer.transform;
            let corners = [
                glam::Vec2::new(x0 as f32, y0 as f32),
                glam::Vec2::new(x1 as f32, y0 as f32),
                glam::Vec2::new(x0 as f32, y1 as f32),
                glam::Vec2::new(x1 as f32, y1 as f32),
            ];
            for c in corners {
                let d = t.transform_point2(c);
                min_x = min_x.min(d.x.floor() as i64);
                min_y = min_y.min(d.y.floor() as i64);
                max_x = max_x.max(d.x.ceil() as i64);
                max_y = max_y.max(d.y.ceil() as i64);
            }
        }
        // Nothing measured (or nothing outside): Reveal All is a no-op —
        // an empty transaction, which history records as nothing at all.
        if min_x > max_x
            || (min_x >= 0 && max_x <= i64::from(w) && min_y >= 0 && max_y <= i64::from(h))
        {
            return Ok(Command::Transaction {
                label: "Reveal All".to_string(),
                commands: Vec::new(),
            });
        }
        // Content at negative coordinates moves the origin: translate every
        // root layer so the union starts at (0, 0), then grow to fit.
        let shift_x = (-min_x).max(0);
        let shift_y = (-min_y).max(0);
        let new_w = (max_x + shift_x).max(i64::from(w)) as u32;
        let new_h = (max_y + shift_y).max(i64::from(h)) as u32;
        let mut commands = vec![Command::SetCanvasSize {
            size: glam::UVec2::new(new_w, new_h),
        }];
        if shift_x > 0 || shift_y > 0 {
            let delta = glam::Vec2::new(shift_x as f32, shift_y as f32);
            for id in self.document.layers.root() {
                commands.push(Command::TransformLayer {
                    layer_id: *id,
                    matrix: tools::edit::translation_matrix(delta),
                });
            }
        }
        Ok(Command::Transaction {
            label: "Reveal All".to_string(),
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
        // Card 077: the original `.psd` this document was opened from is never
        // silently overwritten with this build's (necessarily reduced) export
        // of it. Path equality is checked canonically when the files exist,
        // case-insensitively otherwise (Windows).
        if let Some(source) = self.source_path.as_deref() {
            if exports_as_psd(source) && same_path(source, path) {
                return Err(DocumentError::OriginalOverwrite(source.to_path_buf()));
            }
        }
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
        // A tagged document re-tags: the profile it opened with rides back
        // into the file (the codec writes the iCCP chunk for the formats
        // that carry one).
        let encode_options = match &self.document.meta.color_space {
            color::ColorSpace::IccProfile { profile, .. } if !profile.is_empty() => {
                raster::EncodeOptions::with_icc(profile.clone())
            }
            _ => raster::EncodeOptions::default(),
        };
        if self.is_sixteen_bit() && format.supports_16_bit() {
            let rgba16 = self.composite_rgba16(rect)?;
            raster::encode_to_path(
                path,
                format,
                self.document.width(),
                self.document.height(),
                raster::EncodedPixels::Rgba16(&rgba16),
                &encode_options,
            )?
        } else {
            let rgba8 = self.composite(rect)?;
            raster::encode_to_path(
                path,
                format,
                self.document.width(),
                self.document.height(),
                raster::EncodedPixels::Rgba8(&rgba8),
                &encode_options,
            )?
        };
        Ok(())
    }

    /// Build the Image ▸ Image Size command for this document: one
    /// [`Command::ResampleImage`] carrying the complete new tile map of every
    /// pixel target — layers through the colour-managed resampler, raster
    /// masks through a bilinear coverage pass.
    ///
    /// `None` resample in the spec means the user unlocked the pixel
    /// dimensions to change print metadata only; nothing here stores a ppi,
    /// so the shell treats that as a no-op rather than a silent rewrite.
    pub fn resample_command(
        &mut self,
        spec: &ui::dialogs::ImageSizeSpec,
    ) -> Result<Command, DocumentError> {
        let Some(filter) = spec.resample else {
            return Ok(Command::Transaction {
                label: "Image Size".to_string(),
                commands: Vec::new(),
            });
        };
        let (w, h) = (self.document.width(), self.document.height());
        let (dw, dh) = (spec.width, spec.height);
        let rect = PixelRect::new(0, 0, w, h);
        let new_rect = PixelRect::new(0, 0, dw, dh);
        let mut changes = Vec::new();
        // Walk the layer tree, not the pixel store: the store's mask keys name
        // the mask, while `PixelTarget::Mask` names the layer that owns it.
        for id in self.document.layers.iter_depth_first() {
            let Some(layer) = self.document.layers.get(id) else {
                continue;
            };
            let mut targets = vec![editor_core::PixelTarget::Layer(id)];
            if layer.mask_id().is_some() {
                targets.push(editor_core::PixelTarget::Mask(id));
            }
            for target in targets {
                let key = match target {
                    editor_core::PixelTarget::Layer(id) => editor_core::PixelKey::Layer(id),
                    editor_core::PixelTarget::Mask(id) => {
                        match self.document.layers.get(id).and_then(|l| l.mask_id()) {
                            Some(mask) => editor_core::PixelKey::Mask(mask),
                            None => continue,
                        }
                    }
                };
                let Some(map) = self.document.pixels.tiles(key) else {
                    continue;
                };
                let mut edits: Vec<editor_core::TileEdit> = Vec::new();
                match key {
                    editor_core::PixelKey::Layer(_) => {
                        let rgba = self.materialize_rgba(map, rect)?;
                        let image = raster::export::linear_from_rgba8(
                            w,
                            h,
                            &rgba,
                            &self.document.meta.color_space,
                        )?;
                        let scaled = raster::export::resample(&image, dw, dh, filter)?;
                        let out = raster::export::rgba8_from_linear(
                            &scaled,
                            &self.document.meta.color_space,
                        )?;
                        let grid = raster::TileGrid::from_rgba8(dw, dh, &out)
                            .map_err(DocumentError::Grid)?;
                        let deep = self.document.meta.bit_depth == 16;
                        for (coord, tile) in grid.iter() {
                            // W4-F: a 16-bit document keeps 16-bit tiles (the
                            // resample itself runs at 8 bits).
                            let hash = match deep
                                .then(|| raster::widen_rgba8_tile(tile.data()))
                                .flatten()
                            {
                                Some(wide) => self.tiles.insert_bytes(wide),
                                None => self.tiles.insert_tile(tile),
                            };
                            edits.push(editor_core::TileEdit::set(coord, hash));
                        }
                    }
                    editor_core::PixelKey::Mask(_) => {
                        let coverage = self.materialize_coverage(map, rect)?;
                        let out = resample_coverage(&coverage, w, h, dw, dh);
                        // Coverage tiles: a tile that is all zero is exactly what
                        // an absent tile means, so it is removed rather than
                        // stored (see `import.rs`'s mask writer).
                        let ts = TILE_SIZE as i64;
                        let cx = (i64::from(new_rect.width) + ts - 1) / ts;
                        let cy = (i64::from(new_rect.height) + ts - 1) / ts;
                        for ty in 0..cy {
                            for tx in 0..cx {
                                let coord = raster::TileCoord::new(tx as i32, ty as i32, 0);
                                let (ox, oy) = coord.pixel_origin();
                                let mut data = vec![0u8; editor_core::MASK_TILE_BYTES];
                                let stride = TILE_SIZE as usize;
                                let vw = (i64::from(new_rect.width) - ox).clamp(0, ts) as usize;
                                let vh = (i64::from(new_rect.height) - oy).clamp(0, ts) as usize;
                                for y in 0..vh {
                                    let src = ((oy as usize + y) * dw as usize) + ox as usize;
                                    let dst = y * stride;
                                    data[dst..dst + vw].copy_from_slice(&out[src..src + vw]);
                                }
                                if data.iter().all(|&b| b == 0) {
                                    edits.push(editor_core::TileEdit { coord, hash: None });
                                } else {
                                    let hash = self.tiles.insert_bytes(data);
                                    edits.push(editor_core::TileEdit::set(coord, hash));
                                }
                            }
                        }
                    }
                }
                // Tiles the new canvas no longer covers are removed, not left
                // dangling past the document edge.
                let ts = TILE_SIZE as i64;
                let nx = (i64::from(new_rect.width) + ts - 1) / ts;
                let ny = (i64::from(new_rect.height) + ts - 1) / ts;
                for (coord, _) in map.iter() {
                    let beyond = i64::from(coord.x) >= nx || i64::from(coord.y) >= ny;
                    if beyond {
                        edits.push(editor_core::TileEdit { coord, hash: None });
                    }
                }
                changes.push((target, editor_core::TileDelta::new(edits)?));
            }
        }
        Ok(Command::ResampleImage {
            size: glam::UVec2::new(dw, dh),
            changes,
        })
    }

    /// One target's pixels as a straight-RGBA8 buffer the size of the canvas.
    fn materialize_rgba(
        &self,
        map: &editor_core::TileMap,
        rect: PixelRect,
    ) -> Result<Vec<u8>, DocumentError> {
        let mut out = vec![0u8; (rect.width as usize) * (rect.height as usize) * 4];
        let ts = TILE_SIZE as i64;
        for (coord, hash) in map.iter() {
            let Some(stored) = self.tiles.tile(hash) else {
                continue;
            };
            // W4-F: a 16-bit tile is materialised rounded to 8 bits.
            let bytes = raster::rgba8_view(stored);
            if bytes.len() != raster::depth::RGBA8_TILE_BYTES {
                continue;
            }
            let (ox, oy) = coord.pixel_origin();
            let (x0, y0) = (rect.x.max(ox), rect.y.max(oy));
            let (x1, y1) = (rect.right().min(ox + ts), rect.bottom().min(oy + ts));
            for y in y0..y1 {
                let src = (((y - oy) as usize) * TILE_SIZE as usize + ((x0 - ox) as usize)) * 4;
                let dst =
                    ((y - rect.y) as usize * rect.width as usize + (x0 - rect.x) as usize) * 4;
                let n = ((x1 - x0) as usize) * 4;
                out[dst..dst + n].copy_from_slice(&bytes[src..src + n]);
            }
        }
        Ok(out)
    }

    /// One target's mask coverage as a one-byte-per-pixel canvas buffer.
    fn materialize_coverage(
        &self,
        map: &editor_core::TileMap,
        rect: PixelRect,
    ) -> Result<Vec<u8>, DocumentError> {
        let mut out = vec![0u8; (rect.width as usize) * (rect.height as usize)];
        let ts = TILE_SIZE as i64;
        for (coord, hash) in map.iter() {
            let Some(bytes) = self.tiles.tile(hash) else {
                continue;
            };
            let (ox, oy) = coord.pixel_origin();
            let (x0, y0) = (rect.x.max(ox), rect.y.max(oy));
            let (x1, y1) = (rect.right().min(ox + ts), rect.bottom().min(oy + ts));
            for y in y0..y1 {
                let src = ((y - oy) as usize) * TILE_SIZE as usize + (x0 - ox) as usize;
                let dst = (y - rect.y) as usize * rect.width as usize + (x0 - rect.x) as usize;
                let n = (x1 - x0) as usize;
                out[dst..dst + n].copy_from_slice(&bytes[src..src + n]);
            }
        }
        Ok(out)
    }

    /// Build the Image ▸ Canvas Size command: re-frame the document without
    /// resampling — [`Command::SetCanvasSize`] plus one translation per root
    /// layer so the existing content lands where the dialog's anchor put it,
    /// and, when the dialog asked for a fill colour, [`Command::fill_region`]
    /// on the bottom-most raster layer over the area the old canvas did not
    /// cover. The whole thing is one undoable step, the same shape a crop is.
    ///
    /// The fill targets the bottom raster layer because that is what the
    /// canvas-extension colour means in a layered document: the backdrop
    /// behind every layer. With no raster layer at the bottom (groups and
    /// adjustments only) there is nothing to fill and the exposed area stays
    /// transparent, whatever the dialog said — a colour needs a layer to land
    /// on, and inventing one would be a bigger decision than this dialog
    /// makes.
    pub fn canvas_size_command(
        &mut self,
        spec: &ui::dialogs::CanvasSizeSpec,
    ) -> Result<Command, DocumentError> {
        let (w, h) = (self.document.width(), self.document.height());
        let (new_w, new_h) = (spec.width, spec.height);
        let (ox, oy) = spec.offset;
        let mut commands = vec![Command::SetCanvasSize {
            size: glam::UVec2::new(new_w, new_h),
        }];
        if ox != 0 || oy != 0 {
            // Content lands *at* the offset: every root layer moves by it as
            // one unit (children ride with their parents).
            let delta = glam::Vec2::new(ox as f32, oy as f32);
            for id in self.document.layers.root() {
                commands.push(Command::TransformLayer {
                    layer_id: *id,
                    matrix: tools::edit::translation_matrix(delta),
                });
            }
        }
        let fill = match spec.background {
            ui::dialogs::BackgroundContents::Transparent => None,
            ui::dialogs::BackgroundContents::White => Some([255u8, 255, 255, 255]),
            ui::dialogs::BackgroundContents::Black => Some([0u8, 0, 0, 255]),
            ui::dialogs::BackgroundContents::Custom(rgba) => {
                Some(crate::menu_bridge::rgba8_of(rgba))
            }
        };
        if let Some(color) = fill {
            // The bottom-most layer that owns pixels is the backdrop.
            let bottom = self
                .document
                .layers
                .root()
                .iter()
                .rev()
                .copied()
                .find(|id| {
                    self.document
                        .layers
                        .get(*id)
                        .is_some_and(|l| matches!(l.kind, layer_model::LayerKind::Raster(_)))
                });
            if let Some(bottom) = bottom {
                let key = editor_core::PixelKey::Layer(bottom);
                // `FillRegion`'s rect is in the *layer's* pixel space, and this
                // transaction also translates the layer by the anchor offset —
                // so a strip at document position E is layer pixels at E -
                // offset.
                // Strips share tiles (a 100→200 enlarge writes tile (0,0)
                // twice), and each fill's edge tile must preserve what the
                // tile holds *at the moment that fill applies* — so the edge
                // bytes are accumulated here, in lockstep with the commands,
                // instead of every fill reading the pre-transaction store and
                // clobbering the one before it.
                let mut working: std::collections::BTreeMap<raster::TileCoord, Vec<u8>> =
                    std::collections::BTreeMap::new();
                for rect in Self::exposed_rects((w, h), (new_w, new_h), (ox, oy)) {
                    let rect = PixelRect::new(rect.x - ox, rect.y - oy, rect.width, rect.height);
                    let mut edges = Vec::new();
                    let ts = TILE_SIZE as i64;
                    let x0 = rect.x.div_euclid(ts);
                    let x1 = (rect.right() - 1).div_euclid(ts);
                    let y0 = rect.y.div_euclid(ts);
                    let y1 = (rect.bottom() - 1).div_euclid(ts);
                    for ty in y0..=y1 {
                        for tx in x0..=x1 {
                            let coord = raster::TileCoord::new(tx as i32, ty as i32, 0);
                            // Keep whatever the tile already holds outside the
                            // rect — the store's bytes, or what an earlier
                            // strip's fill already wrote to this tile.
                            let mut data = match working.get(&coord) {
                                Some(bytes) => bytes.clone(),
                                None => match self
                                    .document
                                    .pixels
                                    .tiles(key)
                                    .and_then(|map| map.get(coord))
                                    .and_then(|hash| self.tiles.tile(hash))
                                {
                                    Some(bytes) => bytes.to_vec(),
                                    None => vec![
                                        0u8;
                                        raster::Tile::byte_len(raster::PixelFormat::Rgba8)
                                    ],
                                },
                            };
                            let (tox, toy) = coord.pixel_origin();
                            let cx0 = (rect.x.max(tox) - tox) as usize;
                            let cy0 = (rect.y.max(toy) - toy) as usize;
                            let cx1 = (rect.right().min(tox + ts) - tox) as usize;
                            let cy1 = (rect.bottom().min(toy + ts) - toy) as usize;
                            // W4-F: a 16-bit tile (a 16-bit document) takes
                            // the colour widened, at its own 8-byte stride.
                            let wide: Vec<u8> = color
                                .iter()
                                .flat_map(|c| raster::depth::widen_sample(*c).to_ne_bytes())
                                .collect();
                            let px: &[u8] = if data.len() == raster::depth::RGBA16_TILE_BYTES {
                                &wide
                            } else {
                                &color
                            };
                            let bpp = px.len();
                            for y in cy0..cy1 {
                                let row = y * TILE_SIZE as usize;
                                for x in cx0..cx1 {
                                    data[(row + x) * bpp..(row + x) * bpp + bpp]
                                        .copy_from_slice(px);
                                }
                            }
                            let hash = self.tiles.insert_bytes(data.clone());
                            working.insert(coord, data);
                            edges.push(editor_core::TileEdit::set(coord, hash));
                        }
                    }
                    commands.push(Command::fill_region(
                        editor_core::PixelTarget::Layer(bottom),
                        rect,
                        editor_core::FillColor(color),
                        edges,
                    )?);
                }
            }
        }
        Ok(Command::Transaction {
            label: "Canvas Size".to_string(),
            commands,
        })
    }

    /// The parts of the new canvas the old canvas did not cover, as up to
    /// four rectangles, clamped to the new canvas and never empty.
    fn exposed_rects(old: (u32, u32), new: (u32, u32), offset: (i64, i64)) -> Vec<PixelRect> {
        let (ox, oy) = offset;
        let canvas = PixelRect::new(0, 0, new.0, new.1);
        // The old content's rectangle in new-canvas coordinates.
        let content = PixelRect::new(ox, oy, old.0, old.1);
        let mut out = Vec::new();
        let strip = |y: i64, x: i64, height: u32, width: u32| -> Option<PixelRect> {
            if width == 0 || height == 0 {
                return None;
            }
            let r = PixelRect::new(x, y, width, height);
            // Clamp to the canvas: a crop can push a strip partly or wholly
            // outside it.
            let x0 = r.x.max(canvas.x);
            let y0 = r.y.max(canvas.y);
            let x1 = r.right().min(canvas.right());
            let y1 = r.bottom().min(canvas.bottom());
            if x1 > x0 && y1 > y0 {
                Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
            } else {
                None
            }
        };
        // Above the content, below it, and the two sides between them.
        if let Some(r) = strip(0, 0, oy.max(0) as u32, new.0) {
            out.push(r);
        }
        if let Some(r) = strip(
            content.bottom().max(0),
            0,
            new.1.saturating_sub(content.bottom().max(0) as u32),
            new.0,
        ) {
            out.push(r);
        }
        if let Some(r) = strip(
            content.y.max(0),
            0,
            content.height.min(new.1),
            ox.max(0) as u32,
        ) {
            out.push(r);
        }
        if let Some(r) = strip(
            content.y.max(0),
            content.right(),
            content.height.min(new.1),
            new.0.saturating_sub(content.right().max(0) as u32),
        ) {
            out.push(r);
        }
        out
    }

    /// Run an Export As job: every enabled entry, written to `dir`.
    ///
    /// One composite feeds the whole batch; `raster`'s batch exporter groups
    /// presets by target size so exactly one scaled image is alive at a time,
    /// and every file is written atomically. Each file is named
    /// `{base_name}{suffix}.{ext}` — the preset's own name is replaced,
    /// because the dialog's rows all start life named "export".
    pub fn export_job(
        &mut self,
        job: &ui::dialogs::ExportJob,
        dir: &std::path::Path,
    ) -> Result<Vec<PathBuf>, DocumentError> {
        let rect = self.canvas_rect();
        let rgba = self.composite(rect)?;
        let (w, h) = (self.document.width(), self.document.height());
        // The composite is straight RGBA8 in the document's own space; the
        // exporter's linear working buffer is what every preset resamples and
        // converts from.
        let image =
            raster::export::linear_from_rgba8(w, h, &rgba, &self.document.meta.color_space)?;
        let presets: Vec<raster::ExportPreset> = job
            .entries
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| {
                let mut preset = entry.preset.clone();
                preset.name = format!("{}{}", job.base_name, entry.suffix);
                preset
            })
            .collect();
        let metadata = raster::export::ExportMetadata {
            icc_profile: None,
            icc_profile_space: None,
        };
        let paths = raster::export::export_batch_to_dir(dir, &image, &presets, &metadata)?;
        Ok(paths)
    }

    /// A downscaled straight-RGBA8 composite of the whole document, for the
    /// Export As dialog's live preview — at most `max_edge` on a side.
    ///
    /// `&self` via the free compositor (the same road
    /// [`OpenDocument::layer_thumbnail`] takes), so the chrome can refresh the
    /// dialog's proxy from a frame that only borrows the editor.
    pub fn export_preview(
        &self,
        max_edge: u32,
    ) -> Result<ui::dialogs::PreviewSource, DocumentError> {
        let rect = self.canvas_rect();
        let canvas = compositor::composite_region(
            &self.document,
            &self.tiles,
            rect,
            0,
            CompositeOptions::default(),
        )?;
        let rgba = canvas.to_rgba8(&self.document.meta.color_space);
        let (w, h) = (self.document.width(), self.document.height());
        let (dw, dh, down) = Self::box_downscale(&rgba, w, h, max_edge);
        ui::dialogs::PreviewSource::new(dw, dh, down)
            .ok_or_else(|| DocumentError::Io("preview buffer did not match its size".into()))
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

    /// W2-G: a save of this document's *snapshot* has landed at `path` on the
    /// job thread; make `path` this document's location.
    ///
    /// `unchanged` says the live document still matches the snapshot that was
    /// written, so it is now clean. When it was edited while the save ran it
    /// stays dirty: the package holds the snapshot, not those edits, and a
    /// clean flag would tell the user work is on disk that is not.
    pub fn adopt_saved(&mut self, path: &Path, unchanged: bool) {
        self.project_path = Some(path.to_path_buf());
        self.document.set_path(Some(path.to_path_buf()));
        if unchanged {
            self.document.mark_saved();
        }
    }

    /// W2-G: wrap a `.psd` the job thread has already parsed
    /// ([`crate::import::document_from_psd`]) — the same document
    /// [`OpenDocument::open_psd_bytes`] builds, without the parse on this
    /// thread.
    pub fn open_psd_import(id: DocumentId, path: &Path, import: crate::import::PsdImport) -> Self {
        let mut open = OpenDocument::from_import(id, import.imported);
        open.source_path = Some(path.to_path_buf());
        open.psd_notes = import.notes;
        open
    }

    /// W2-G: a save of this document's snapshot has just been handed to a
    /// worker; until [`OpenDocument::end_journal_hold`], journal every
    /// accepted command to `side` rather than to the package journal.
    ///
    /// Why a side file and not the journal: the worker copies the package
    /// journal's valid prefix at a moment this thread cannot see, then swaps
    /// the new package in and deletes the old one. A record appended before
    /// that read lands ahead of the new save marker and is treated as work the
    /// snapshot already holds; one appended after it is deleted with the old
    /// package. Recovery would replay neither, and the first command that no
    /// longer applied would stop the replay for good. In the side file the
    /// records survive the swap, and a crash while the hold is open leaves
    /// the file next to the package for the next open to absorb.
    ///
    /// A stale side file at `side` (an earlier hold this process never
    /// finished — the caller absorbs those at open) is replaced, not appended
    /// to. One hold at a time: a second call while one is open is a no-op.
    pub fn begin_journal_hold(&mut self, side: PathBuf) {
        if self.journal_hold.is_some() {
            return;
        }
        if let Err(e) = std::fs::remove_file(&side) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("cannot clear the journal hold {}: {e}", side.display());
            }
        }
        self.journal_hold = Some(side);
    }

    /// W2-G: the save has landed (or failed); commands go back to the package
    /// journal. Returns the side file the hold wrote to — the caller absorbs
    /// it into the journal the document now has
    /// ([`project_format::CommandJournal::absorb`]).
    pub fn end_journal_hold(&mut self) -> Option<PathBuf> {
        self.journal_hold.take()
    }

    /// W2-G: the side file the open hold journals to, if a hold is open.
    pub fn journal_hold(&self) -> Option<&Path> {
        self.journal_hold.as_deref()
    }

    /// W2-G: what an export to `.psd` that ran on a worker could not carry;
    /// replaces the report left by the last exchange.
    pub fn set_psd_notes(&mut self, notes: crate::import::PsdNotes) {
        self.psd_notes = notes;
    }
}

// ---------------------------------------------------------------------------
// Layer thumbnails: the cache the Layers panel reads through
// ---------------------------------------------------------------------------

thread_local! {
    /// How many times [`OpenDocument::layer_thumbnail`] has asked the
    /// compositor for a band, on this thread. The measure the throttling
    /// tests count with (compositor calls, never wall clock): a cached frame
    /// adds nothing here. Per thread so parallel tests do not count each
    /// other's work.
    static THUMB_COMPOSITES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The calling thread's thumbnail compositor-call count so far.
pub fn thumbnail_composites() -> u64 {
    THUMB_COMPOSITES.with(|c| c.get())
}

/// `a ∩ b`, or `None` when they do not overlap.
fn intersect_rects(a: PixelRect, b: PixelRect) -> Option<PixelRect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// A `tw`x`th` thumbnail of a `w`x`h` canvas being filled one region at a
/// time. Every thumbnail pixel is the mean of the canvas pixels it covers —
/// the same footprint rule as [`OpenDocument::box_downscale`] (the integer
/// source pixels in `[ox*w/tw, (ox+1)*w/tw)`) — and canvas pixels no region
/// ever supplied count as transparent, which is exactly what they are
/// outside a layer's styled bounds.
struct ThumbAccumulator {
    w: u32,
    h: u32,
    tw: u32,
    th: u32,
    acc: Vec<[f64; 4]>,
}

impl ThumbAccumulator {
    fn new(w: u32, h: u32, tw: u32, th: u32) -> Self {
        Self {
            w,
            h,
            tw,
            th,
            acc: vec![[0.0; 4]; (tw as usize) * (th as usize)],
        }
    }

    /// The canvas footprint `[lo, hi)` of thumbnail row/column `o` along an
    /// axis of `n` thumbnail pixels over `len` canvas pixels.
    fn footprint(o: u32, n: u32, len: u32) -> (f64, f64) {
        let lo = (o as f64 / n as f64) * len as f64;
        let hi = ((o + 1) as f64 / n as f64) * len as f64;
        (lo, hi.max(lo + 1.0))
    }

    /// How many integer positions lie in `[lo, hi)`.
    fn count(lo: f64, hi: f64) -> usize {
        (hi.ceil() - lo.ceil()).max(1.0) as usize
    }

    /// Which thumbnail index along an axis covers canvas position `s`.
    fn index_of(s: i64, n: u32, len: u32) -> u32 {
        // The footprint of index `o` is `[o*len/n, (o+1)*len/n)`, so the
        // index for `s` is `floor((s+1)*n/len)` corrected for rounding.
        let mut o = (((s as f64 + 1.0) * n as f64) / len as f64).floor() as i64;
        o = o.clamp(0, n as i64 - 1);
        // Correct off-by-one at footprint edges either way.
        while o > 0 && (s as f64) < Self::footprint(o as u32, n, len).0 {
            o -= 1;
        }
        while o + 1 < n as i64 && (s as f64) >= Self::footprint(o as u32, n, len).1 {
            o += 1;
        }
        o as u32
    }

    /// Which thumbnail index covers canvas position `s`, or `None` when `s`
    /// is off the canvas.
    fn slot(s: i64, n: u32, len: u32) -> Option<u32> {
        if s < 0 || s >= i64::from(len) {
            return None;
        }
        Some(Self::index_of(s, n, len))
    }

    /// Fold a straight-alpha RGBA8 `region` of the canvas into the sums.
    fn add_region(&mut self, rgba8: &[u8], region: PixelRect) {
        if self.w == 0 || self.h == 0 || self.tw == 0 || self.th == 0 {
            return;
        }
        // One lookup per column, not one per pixel.
        let cols: Vec<Option<u32>> = (0..region.width)
            .map(|rx| Self::slot(region.x + i64::from(rx), self.tw, self.w))
            .collect();
        for ry in 0..region.height {
            let Some(oy) = Self::slot(region.y + i64::from(ry), self.th, self.h) else {
                continue;
            };
            let row = (ry * region.width * 4) as usize;
            for (rx, ox) in cols.iter().enumerate() {
                let Some(ox) = ox else {
                    continue;
                };
                let i = row + rx * 4;
                let oi = (oy * self.tw + *ox) as usize;
                for c in 0..4 {
                    self.acc[oi][c] += rgba8[i + c] as f64;
                }
            }
        }
    }

    /// The thumbnail: every sum divided by its footprint's pixel count.
    fn finish(self) -> Vec<u8> {
        let mut out = vec![0u8; (self.tw as usize) * (self.th as usize) * 4];
        for oy in 0..self.th {
            let (ylo, yhi) = Self::footprint(oy, self.th, self.h);
            let ny = Self::count(ylo, yhi);
            for ox in 0..self.tw {
                let (xlo, xhi) = Self::footprint(ox, self.tw, self.w);
                let n = (ny * Self::count(xlo, xhi)) as f64;
                let oi = (oy * self.tw + ox) as usize;
                for c in 0..4 {
                    out[oi * 4 + c] = (self.acc[oi][c] / n).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }
}

/// One stored thumbnail and the fingerprint of what it was made from.
#[derive(Debug, Clone)]
pub struct ThumbEntry {
    fingerprint: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What a cache lookup handed back.
#[derive(Debug)]
pub struct Thumb<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    /// `true` when this call composited; `false` when the stored thumbnail
    /// was still current and is what you got.
    pub fresh: bool,
}

/// The Layers panel's thumbnail cache: one entry per layer (and one per
/// mask), keyed by a fingerprint of everything the thumbnail depends on.
///
/// The fingerprint is cheap to take every frame — it hashes the layer's
/// serialized parameters (visibility normalised, since the thumbnail shows
/// the layer's content whether or not it is shown), its descendants', every
/// tile hash under their pixel and mask keys, the canvas size, the colour
/// space and the requested edge — and the compositor runs only when it
/// changes. Painting one layer therefore recomposites that layer's
/// thumbnail and no other's. Owned by the chrome, not the document: it is
/// view state, like the textures it feeds.
#[derive(Debug, Default)]
pub struct LayerThumbCache {
    layers: std::collections::HashMap<LayerId, ThumbEntry>,
    masks: std::collections::HashMap<LayerId, ThumbEntry>,
    recomposites: u64,
    hits: u64,
}

impl LayerThumbCache {
    /// Everything the thumbnail of `layer_id` at `max_edge` depends on, as
    /// one hash. Two documents whose fingerprints agree draw the same
    /// thumbnail; a changed pixel, parameter, effect, mask or canvas size
    /// changes it.
    pub fn layer_fingerprint(open: &OpenDocument, layer_id: LayerId, max_edge: u32) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let doc = &open.document;
        doc.layers.get(layer_id)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        0x54_48_55_4d_u32.hash(&mut h); // "THUM": the layer flavour
        max_edge.hash(&mut h);
        doc.width().hash(&mut h);
        doc.height().hash(&mut h);
        format!("{:?}", doc.meta.color_space).hash(&mut h);
        // The layer and its whole subtree (a group's thumbnail is its
        // children's), in tree order. The layer's own visibility is
        // normalised: the thumbnail shows it whether or not it is shown.
        let mut stack = vec![layer_id];
        while let Some(id) = stack.pop() {
            let Some(layer) = doc.layers.get(id) else {
                continue;
            };
            id.hash(&mut h);
            if id == layer_id {
                let mut shown = layer.clone();
                shown.visible = true;
                Self::hash_layer(&shown, &mut h);
            } else {
                Self::hash_layer(layer, &mut h);
            }
            Self::hash_tiles(doc.layer_tiles(id), &mut h);
            Self::hash_tiles(doc.mask_tiles(id), &mut h);
            stack.extend(layer.children().iter().rev().copied());
        }
        // The ancestors: a group's opacity, blend, mask and transform reach
        // its children's composite. Visibility normalised, as the composite
        // forces them shown.
        let mut cursor = doc.layers.parent_of(layer_id);
        while let Some(id) = cursor {
            let Some(group) = doc.layers.get(id) else {
                break;
            };
            id.hash(&mut h);
            let mut shown = group.clone();
            shown.visible = true;
            // The children list is the subtree's business, hashed above.
            if let LayerKind::Group(g) = &mut shown.kind {
                g.children.clear();
            }
            Self::hash_layer(&shown, &mut h);
            Self::hash_tiles(doc.mask_tiles(id), &mut h);
            cursor = doc.layers.parent_of(id);
        }
        Some(h.finish())
    }

    /// Everything the mask thumbnail of `layer_id` depends on: the mask's
    /// parameters (pose, inversion), its tiles, the canvas and the edge.
    /// `None` when the layer has no mask.
    pub fn mask_fingerprint(open: &OpenDocument, layer_id: LayerId, max_edge: u32) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let doc = &open.document;
        let layer = doc.layers.get(layer_id)?;
        let mask = layer.mask.as_ref()?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        0x4d_41_53_4b_u32.hash(&mut h); // "MASK": the mask flavour
        max_edge.hash(&mut h);
        doc.width().hash(&mut h);
        doc.height().hash(&mut h);
        // The mask is read through the layer's document pose (card 058), so
        // the layer's transform and its ancestors' are part of the key.
        let mut cursor = Some(layer_id);
        while let Some(id) = cursor {
            if let Some(l) = doc.layers.get(id) {
                l.transform
                    .to_cols_array()
                    .iter()
                    .for_each(|v| v.to_bits().hash(&mut h));
            }
            cursor = doc.layers.parent_of(id);
        }
        Self::hash_bytes(&rmp_serde::to_vec(mask).unwrap_or_default(), &mut h);
        Self::hash_tiles(doc.mask_tiles(layer_id), &mut h);
        Some(h.finish())
    }

    fn hash_layer(layer: &layer_model::Layer, h: &mut impl std::hash::Hasher) {
        Self::hash_bytes(&rmp_serde::to_vec(layer).unwrap_or_default(), h);
    }

    fn hash_bytes(bytes: &[u8], h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        bytes.hash(h);
    }

    fn hash_tiles(map: Option<&editor_core::TileMap>, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        match map {
            None => 0u8.hash(h),
            Some(map) => {
                1u8.hash(h);
                map.len().hash(h);
                for (coord, hash) in map.iter() {
                    coord.x.hash(h);
                    coord.y.hash(h);
                    coord.level.hash(h);
                    hash.0.hash(h);
                }
            }
        }
    }

    /// Whether the stored layer thumbnail (if any) still matches the
    /// document. Cheap — a fingerprint, no compositing — so the panel can
    /// decide what to spend its per-frame budget on.
    pub fn layer_is_current(&self, open: &OpenDocument, layer_id: LayerId, max_edge: u32) -> bool {
        match (
            self.layers.get(&layer_id),
            Self::layer_fingerprint(open, layer_id, max_edge),
        ) {
            (Some(e), Some(fp)) => e.fingerprint == fp,
            _ => false,
        }
    }

    /// Whether the stored mask thumbnail (if any) still matches the document.
    pub fn mask_is_current(&self, open: &OpenDocument, layer_id: LayerId, max_edge: u32) -> bool {
        match (
            self.masks.get(&layer_id),
            Self::mask_fingerprint(open, layer_id, max_edge),
        ) {
            (Some(e), Some(fp)) => e.fingerprint == fp,
            _ => false,
        }
    }

    /// The layer's thumbnail: stored when its fingerprint is unchanged,
    /// recomposited (through [`OpenDocument::layer_thumbnail`]) otherwise.
    pub fn layer_thumbnail(
        &mut self,
        open: &OpenDocument,
        layer_id: LayerId,
        max_edge: u32,
    ) -> Result<Thumb<'_>, DocumentError> {
        let fp = Self::layer_fingerprint(open, layer_id, max_edge).ok_or(
            DocumentError::Command(CommandError::LayerNotFound(layer_id)),
        )?;
        let fresh = match self.layers.get(&layer_id) {
            Some(e) if e.fingerprint == fp => false,
            _ => {
                let (width, height, rgba) = open.layer_thumbnail(layer_id, max_edge)?;
                self.layers.insert(
                    layer_id,
                    ThumbEntry {
                        fingerprint: fp,
                        width,
                        height,
                        rgba,
                    },
                );
                true
            }
        };
        if fresh {
            self.recomposites += 1;
        } else {
            self.hits += 1;
        }
        let e = &self.layers[&layer_id];
        Ok(Thumb {
            width: e.width,
            height: e.height,
            rgba: &e.rgba,
            fresh,
        })
    }

    /// The mask's thumbnail, cached the same way. `Err` for a layer without
    /// a mask.
    pub fn mask_thumbnail(
        &mut self,
        open: &OpenDocument,
        layer_id: LayerId,
        max_edge: u32,
    ) -> Result<Thumb<'_>, DocumentError> {
        let fp = Self::mask_fingerprint(open, layer_id, max_edge).ok_or(DocumentError::Command(
            CommandError::LayerNotFound(layer_id),
        ))?;
        let fresh = match self.masks.get(&layer_id) {
            Some(e) if e.fingerprint == fp => false,
            _ => {
                let (width, height, rgba) = open.mask_thumbnail(layer_id, max_edge)?;
                self.masks.insert(
                    layer_id,
                    ThumbEntry {
                        fingerprint: fp,
                        width,
                        height,
                        rgba,
                    },
                );
                true
            }
        };
        if fresh {
            self.recomposites += 1;
        } else {
            self.hits += 1;
        }
        let e = &self.masks[&layer_id];
        Ok(Thumb {
            width: e.width,
            height: e.height,
            rgba: &e.rgba,
            fresh,
        })
    }

    /// Drop the entries of layers that are no longer in the stack.
    pub fn retain(&mut self, live: &std::collections::HashSet<LayerId>) {
        self.layers.retain(|id, _| live.contains(id));
        self.masks.retain(|id, _| live.contains(id));
    }

    /// Drop the mask entry of one layer (its mask was removed).
    pub fn forget_mask(&mut self, layer_id: LayerId) {
        self.masks.remove(&layer_id);
    }

    /// Forget everything (the active document changed).
    pub fn clear(&mut self) {
        self.layers.clear();
        self.masks.clear();
    }

    /// How many thumbnails this cache has composited since it was created.
    pub fn recomposites(&self) -> u64 {
        self.recomposites
    }

    /// How many lookups were served from a stored thumbnail.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// How many layer entries are stored.
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Whether no layer entry is stored.
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::pixels::{PixelTarget, TileDelta, TileEdit};

    /// Card 025: the live text draft lands on the document outside history,
    /// and restoring the original through the same route returns the layer
    /// byte-for-byte — no entry, nothing journaled, locks honoured.
    #[test]
    fn a_text_draft_reconciles_outside_history() {
        let mut doc = OpenDocument::blank(DocumentId(9101), 64, 64, "draft", 32).unwrap();
        let original = layer_model::TextLayer {
            text: "Headline".to_string(),
            font_family: "DejaVu Sans".to_string(),
            size_px: 32.0,
            style: layer_model::text::BaseStyle {
                weight: layer_model::text::Weight::BOLD,
                ..layer_model::text::BaseStyle::default()
            },
            ..layer_model::TextLayer::default()
        };
        let id = {
            let layer = Layer::with_kind("Type", layer_model::LayerKind::Text(original.clone()));
            let id = layer.id;
            doc.apply(Command::create_layer(layer)).unwrap();
            id
        };
        let depth = doc.history_depth();

        // The draft: applied directly, no history entry.
        let draft = layer_model::TextLayer {
            text: "Edited".to_string(),
            ..original.clone()
        };
        doc.apply_text_draft(id, layer_model::LayerKind::Text(draft.clone()))
            .unwrap();
        assert_eq!(doc.history_depth(), depth, "the draft is not history");
        let layer_model::LayerKind::Text(seen) = &doc.document.layers.get(id).unwrap().kind else {
            panic!("the layer is text");
        };
        assert_eq!(seen.text, "Edited", "the canvas shows the draft");

        // The restore: byte-for-byte back, still no history entry.
        doc.apply_text_draft(id, layer_model::LayerKind::Text(original.clone()))
            .unwrap();
        let layer_model::LayerKind::Text(restored) = &doc.document.layers.get(id).unwrap().kind
        else {
            panic!("the layer is text");
        };
        assert_eq!(restored, &original, "styles included");
        assert_eq!(doc.history_depth(), depth);

        // A payload that fails its own validation is refused before it can
        // touch the layer.
        let broken = layer_model::TextLayer {
            text: "x".to_string(),
            size_px: -4.0,
            ..layer_model::TextLayer::default()
        };
        assert!(doc
            .apply_text_draft(id, layer_model::LayerKind::Text(broken))
            .is_err());
        let layer_model::LayerKind::Text(after) = &doc.document.layers.get(id).unwrap().kind else {
            panic!("the layer is text");
        };
        assert_eq!(after, &original, "the refused draft changed nothing");

        // A blanket-locked layer refuses the draft, like SetLayerKind does.
        doc.apply(Command::SetLayerProperties {
            layer_id: id,
            patch: editor_core::LayerPatch {
                locked: Some(layer_model::LockState {
                    all: true,
                    ..layer_model::LockState::default()
                }),
                ..editor_core::LayerPatch::default()
            },
        })
        .unwrap();
        let depth = doc.history_depth();
        assert!(doc
            .apply_text_draft(id, LayerKind::Text(draft.clone()))
            .is_err());
        assert_eq!(doc.history_depth(), depth);
        let layer_model::LayerKind::Text(locked) = &doc.document.layers.get(id).unwrap().kind
        else {
            panic!("the layer is text");
        };
        assert_eq!(locked, &original, "the locked layer kept its payload");

        // Round-2 N4: a draft can retype a text layer, never convert a
        // non-text one in place (the same-class guard).
        let raster = {
            let layer = Layer::raster("Pixels");
            let id = layer.id;
            doc.apply(Command::create_layer(layer)).unwrap();
            id
        };
        assert!(doc
            .apply_text_draft(raster, layer_model::LayerKind::Text(original.clone()))
            .is_err());
        assert!(matches!(
            doc.document.layers.get(raster).unwrap().kind,
            layer_model::LayerKind::Raster(_)
        ));
    }

    /// Card 013: a preview moves the layer's pixels on screen while the
    /// committed document, its history and its dirty flag stay exactly as
    /// they were — and clearing the preview restores the exact baseline.
    #[test]
    fn a_preview_moves_pixels_on_screen_and_touches_nothing() {
        let mut doc = OpenDocument::blank(DocumentId(9001), 64, 64, "preview", 32).unwrap();
        // A second layer with ink in one corner, through the real command route.
        let overlay = {
            let layer = Layer::raster("Overlay");
            let id = layer.id;
            doc.apply(Command::create_layer(layer)).unwrap();
            let mut bytes = vec![0u8; (TILE_SIZE * TILE_SIZE * 4) as usize];
            for y in 8..16u32 {
                for x in 8..16u32 {
                    let i = ((y * TILE_SIZE + x) * 4) as usize;
                    bytes[i..i + 4].copy_from_slice(&[10, 10, 10, 255]);
                }
            }
            let hash = doc.tiles.insert_bytes(bytes);
            doc.apply(
                Command::paint_tiles(
                    PixelTarget::Layer(id),
                    vec![TileEdit::set(TileCoord::new(0, 0, 0), hash)],
                )
                .unwrap(),
            )
            .unwrap();
            id
        };
        let region = PixelRect::new(0, 0, 64, 64);
        let baseline = doc.composite(region).unwrap();
        let depth_before = doc.history_depth();
        // The setup above is real work; the preview must add no dirt of its own.
        let dirty_before = doc.is_dirty();

        // The lens: the layer renders 20 px to the right.
        doc.set_preview(
            overlay,
            glam::Affine2::from_translation(glam::vec2(20.0, 0.0)),
        );
        let previewed = doc.composite(region).unwrap();
        let at = |buf: &[u8], x: u32, y: u32| {
            let i = ((y * 64 + x) * 4) as usize;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        assert_ne!(at(&previewed, 8, 12), at(&baseline, 8, 12), "the ink moved");
        assert_eq!(
            at(&previewed, 28, 12),
            [10, 10, 10, 255],
            "the ink shows at the previewed position"
        );
        assert_eq!(
            doc.history_depth(),
            depth_before,
            "a preview writes no history entry"
        );
        assert_eq!(doc.is_dirty(), dirty_before, "and adds no dirty flag");

        // Clearing the lens restores the committed pixels exactly — the
        // preview generation kept them out of every cache key.
        doc.clear_preview();
        let restored = doc.composite(region).unwrap();
        assert_eq!(restored, baseline, "cancel restores the exact baseline");
        assert_eq!(doc.preview, None);
    }

    /// Card 013: another open document's preview is its own — a preview held
    /// by one document cannot reach another's composite or cache.
    #[test]
    fn another_document_cannot_reuse_a_preview() {
        let mut a = OpenDocument::blank(DocumentId(9002), 64, 64, "a", 32).unwrap();
        let mut b = OpenDocument::blank(DocumentId(9003), 64, 64, "b", 32).unwrap();
        let layer = a.document.active_layer().unwrap();
        let region = PixelRect::new(0, 0, 64, 64);
        let b_before = b.composite(region).unwrap();

        a.set_preview(
            layer,
            glam::Affine2::from_translation(glam::vec2(10.0, 0.0)),
        );
        assert!(a.preview.is_some());
        assert!(b.preview.is_none(), "each document holds its own lens");
        assert_eq!(b.composite(region).unwrap(), b_before);
    }

    use layer_model::Layer;
    use raster::{Tile, TileCoord, TILE_SIZE};

    fn image(width: u32, height: u32, value: u8) -> DecodedImage {
        DecodedImage {
            width,
            height,
            rgba8: vec![value; (width as usize) * (height as usize) * 4],
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        }
    }

    /// A deterministic pseudo-random RGBA8 buffer — flat pixels compress
    /// identically at every JPEG quality, so the quality test needs noise.
    fn noise_image(width: u32, height: u32) -> DecodedImage {
        let mut state = 0x1234_5678u32;
        let mut rgba8 = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let (r, g) = ((state >> 16) as u8, (state >> 8) as u8);
            let b = state as u8;
            rgba8.extend_from_slice(&[r, g, b, 255]);
        }
        DecodedImage {
            width,
            height,
            rgba8,
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
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
    fn export_job_writes_every_entry_and_honours_quality_and_scale() {
        let imported =
            crate::import::document_from_image(&noise_image(64, 64), "noise.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        let dir = tempfile::tempdir().unwrap();
        let job = ui::dialogs::ExportJob {
            base_name: "noise".to_string(),
            entries: vec![
                ui::dialogs::ExportEntry::new("-q30", raster::ExportFormat::Jpeg(30), 1.0),
                ui::dialogs::ExportEntry::new("-q95", raster::ExportFormat::Jpeg(95), 1.0),
                ui::dialogs::ExportEntry::new("-half", raster::ExportFormat::Png, 0.5),
            ],
        };
        let paths = d.export_job(&job, dir.path()).unwrap();
        assert_eq!(paths.len(), 3, "one file per entry: {paths:?}");
        let q30 = std::fs::read(dir.path().join("noise-q30.jpg")).unwrap();
        let q95 = std::fs::read(dir.path().join("noise-q95.jpg")).unwrap();
        assert!(
            q30.len() < q95.len(),
            "JPEG quality 30 ({} bytes) must be smaller than quality 95 ({} bytes)",
            q30.len(),
            q95.len()
        );
        let half = raster::decode_path(&dir.path().join("noise-half.png")).unwrap();
        assert_eq!(
            (half.width, half.height),
            (32, 32),
            "scale 0.5 halves both axes"
        );
    }

    #[test]
    fn export_preview_is_the_whole_composite_at_most_the_proxy_cap() {
        let d = doc_of(600, 400);
        let proxy = d
            .export_preview(ui::dialogs::export_as::MAX_PROXY_SIDE)
            .unwrap();
        assert_eq!(proxy.width(), 512);
        assert_eq!(proxy.height(), 341, "aspect preserved: 600x400 at cap 512");
        // A small document is never upscaled.
        let small = doc_of(8, 8);
        let proxy = small.export_preview(64).unwrap();
        assert_eq!((proxy.width(), proxy.height()), (8, 8));
    }

    #[test]
    fn resample_image_size_resizes_and_undoes_byte_for_byte() {
        let imported =
            crate::import::document_from_image(&noise_image(800, 600), "big.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        let before_map = d
            .document
            .layer_tiles(d.document.active_layer().unwrap())
            .unwrap()
            .iter()
            .collect::<Vec<_>>();
        let before_composite = d.composite(d.canvas_rect()).unwrap();

        let spec = ui::dialogs::ImageSizeSpec {
            width: 400,
            height: 300,
            resolution_ppi: 72.0,
            resample: Some(raster::ResampleFilter::Lanczos3),
        };
        let command = d.resample_command(&spec).unwrap();
        d.history
            .apply(&mut d.document, command)
            .expect("the resample applies");
        assert_eq!((d.document.width(), d.document.height()), (400, 300));
        // The layer's tiles now fit the new canvas exactly: nothing dangles
        // past the edge.
        let map = d
            .document
            .layer_tiles(d.document.active_layer().unwrap())
            .unwrap();
        let ts = TILE_SIZE as i64;
        for (coord, _) in map.iter() {
            assert!(i64::from(coord.x) * ts < 400 && i64::from(coord.y) * ts < 300);
        }

        d.history.undo(&mut d.document).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (800, 600));
        let after_map = d
            .document
            .layer_tiles(d.document.active_layer().unwrap())
            .unwrap()
            .iter()
            .collect::<Vec<_>>();
        assert_eq!(before_map, after_map, "undo restored the exact tile map");
        let after_composite = d.composite(d.canvas_rect()).unwrap();
        assert_eq!(
            before_composite, after_composite,
            "undo restored the original pixels byte-for-byte"
        );

        // Redo resamples again to the same result.
        d.history.redo(&mut d.document).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (400, 300));
    }

    #[test]
    fn resample_image_size_scales_a_raster_mask_with_the_layer() {
        let imported =
            crate::import::document_from_image(&noise_image(64, 64), "m.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        // Add a mask, then reveal only the left half of it.
        let layer = d.document.active_layer().unwrap();
        let attach = Command::SetLayerProperties {
            layer_id: layer,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(layer_model::LayerMask::new(
                    layer_model::MaskId::new(),
                )),
                ..Default::default()
            },
        };
        d.history.apply(&mut d.document, attach).unwrap();
        let mask_id = d
            .document
            .layers
            .get(layer)
            .unwrap()
            .mask_id()
            .expect("the layer has a mask now");
        // Reveal the whole mask. The canvas is 64×64, inside one 256×256
        // coverage tile, so the fill's rect is a *partial* tile: its interior
        // derivation is empty by design, and the caller — which owns the bytes
        // — rasterizes the edge tile itself.
        let rect = raster::PixelRect::new(0, 0, 64, 64);
        let ts = TILE_SIZE as usize;
        let mut data = vec![0u8; editor_core::MASK_TILE_BYTES];
        for y in 0..64 {
            data[y * ts..y * ts + 64].fill(editor_core::MaskCoverage::REVEALED.0);
        }
        let revealed = d.tiles.insert_bytes(data);
        let fill = Command::fill_region(
            editor_core::PixelTarget::Mask(layer),
            rect,
            editor_core::MaskCoverage::REVEALED,
            [editor_core::TileEdit::set(
                raster::TileCoord::new(0, 0, 0),
                revealed,
            )],
        )
        .unwrap();
        d.history.apply(&mut d.document, fill).unwrap();
        assert!(
            d.document
                .pixels
                .tiles(editor_core::PixelKey::Mask(mask_id))
                .is_some(),
            "the mask has coverage tiles"
        );

        let spec = ui::dialogs::ImageSizeSpec {
            width: 32,
            height: 32,
            resolution_ppi: 72.0,
            resample: Some(raster::ResampleFilter::Triangle),
        };
        let command = d.resample_command(&spec).unwrap();
        d.history.apply(&mut d.document, command).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (32, 32));
        let map = d
            .document
            .pixels
            .tiles(editor_core::PixelKey::Mask(mask_id))
            .unwrap();
        let ts = TILE_SIZE as i64;
        for (coord, _) in map.iter() {
            assert!(
                i64::from(coord.x) * ts < 32 && i64::from(coord.y) * ts < 32,
                "mask tiles fit the new canvas"
            );
        }
        d.history.undo(&mut d.document).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (64, 64));
    }

    #[test]
    fn canvas_size_anchored_top_left_keeps_the_pixels_and_undoes() {
        let imported =
            crate::import::document_from_image(&noise_image(100, 100), "sq.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        let before = d.composite(d.canvas_rect()).unwrap();

        let spec = ui::dialogs::CanvasSizeSpec {
            width: 200,
            height: 200,
            offset: ui::dialogs::Anchor::TopLeft.offset((100, 100), (200, 200)),
            anchor: ui::dialogs::Anchor::TopLeft,
            background: ui::dialogs::BackgroundContents::Transparent,
        };
        let command = d.canvas_size_command(&spec).unwrap();
        d.history.apply(&mut d.document, command).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (200, 200));
        // The original pixels did not move: the top-left 100×100 of the new
        // composite is exactly the old one.
        assert_eq!(d.composite(PixelRect::new(0, 0, 100, 100)).unwrap(), before);
        // The exposed area composites fully transparent.
        assert!(
            d.composite(PixelRect::new(150, 150, 50, 50))
                .unwrap()
                .iter()
                .all(|&b| b == 0),
            "the exposed area was not transparent"
        );
        d.history.undo(&mut d.document).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (100, 100));
        assert_eq!(d.composite(d.canvas_rect()).unwrap(), before);
    }

    #[test]
    fn canvas_size_anchored_centre_offsets_the_content() {
        let imported =
            crate::import::document_from_image(&noise_image(100, 100), "sq.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        let before = d.composite(d.canvas_rect()).unwrap();

        let spec = ui::dialogs::CanvasSizeSpec {
            width: 200,
            height: 200,
            offset: ui::dialogs::Anchor::Center.offset((100, 100), (200, 200)),
            anchor: ui::dialogs::Anchor::Center,
            background: ui::dialogs::BackgroundContents::Transparent,
        };
        let command = d.canvas_size_command(&spec).unwrap();
        d.history.apply(&mut d.document, command).unwrap();
        // The content landed centred: (50, 50).
        assert_eq!(
            d.composite(PixelRect::new(50, 50, 100, 100)).unwrap(),
            before
        );
        assert!(
            d.composite(PixelRect::new(0, 0, 50, 50))
                .unwrap()
                .iter()
                .all(|&b| b == 0),
            "the corner the content moved away from was not empty"
        );
        d.history.undo(&mut d.document).unwrap();
        assert_eq!(d.composite(d.canvas_rect()).unwrap(), before);
    }

    #[test]
    fn canvas_size_fill_colours_the_exposed_area_on_the_backdrop() {
        let imported =
            crate::import::document_from_image(&noise_image(100, 100), "sq.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);

        let spec = ui::dialogs::CanvasSizeSpec {
            width: 200,
            height: 200,
            offset: ui::dialogs::Anchor::TopLeft.offset((100, 100), (200, 200)),
            anchor: ui::dialogs::Anchor::TopLeft,
            background: ui::dialogs::BackgroundContents::White,
        };
        let command = d.canvas_size_command(&spec).unwrap();
        d.history.apply(&mut d.document, command).unwrap();
        let exposed = d.composite(PixelRect::new(150, 150, 50, 50)).unwrap();
        assert!(
            exposed
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 255, 255, 255]),
            "the exposed area was not filled white"
        );
        // The original pixels survived beside the fill.
        let kept = d.composite(PixelRect::new(0, 0, 100, 100)).unwrap();
        assert!(!kept.iter().all(|&b| b == 255), "the fill ate the content");
        d.history.undo(&mut d.document).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (100, 100));
        assert_eq!(
            d.composite(d.canvas_rect()).unwrap(),
            d.composite(d.canvas_rect()).unwrap(),
        );
        // The fill was one undoable step together with the resize: the undo
        // above removed the whole transaction, so redo brings both back.
        d.history.redo(&mut d.document).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (200, 200));
    }

    #[test]
    fn an_arbitrary_rotation_grows_resamples_and_undoes_exactly() {
        let imported =
            crate::import::document_from_image(&noise_image(64, 64), "r.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        let before_composite = d.composite(d.canvas_rect()).unwrap();

        let command = d.rotate_canvas_arbitrary(37.0).unwrap();
        d.history.apply(&mut d.document, command).unwrap();
        let (w, h) = (d.document.width(), d.document.height());
        assert!(w > 64 && h > 64, "the canvas grew: {w}x{h}");
        assert_ne!(d.composite(d.canvas_rect()).unwrap(), before_composite);

        d.history.undo(&mut d.document).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (64, 64));
        assert_eq!(
            d.composite(d.canvas_rect()).unwrap(),
            before_composite,
            "undo did not restore the pixels byte-for-byte"
        );
    }

    #[test]
    fn reveal_all_grows_the_canvas_to_the_layer_content_and_undoes() {
        let imported =
            crate::import::document_from_image(&noise_image(32, 32), "bit.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        let layer = d.document.active_layer().unwrap();
        let content = d.composite(PixelRect::new(0, 0, 32, 32)).unwrap();

        // Drag the layer 48 pixels past the right edge of the 64-wide canvas.
        let nudge = Command::TransformLayer {
            layer_id: layer,
            matrix: tools::edit::translation_matrix(glam::Vec2::new(48.0, 0.0)),
        };
        d.history.apply(&mut d.document, nudge).unwrap();

        let command = d.reveal_all_command().unwrap();
        d.history.apply(&mut d.document, command).unwrap();
        // The content's right edge is 48 + 32 = 80: the canvas grew to 80×32
        // and the content is exactly where the user dragged it.
        assert_eq!((d.document.width(), d.document.height()), (80, 32));
        assert_eq!(
            d.composite(PixelRect::new(48, 0, 32, 32)).unwrap(),
            content,
            "the dragged content moved when the canvas revealed it"
        );
        d.history.undo(&mut d.document).unwrap();
        assert_eq!((d.document.width(), d.document.height()), (32, 32));
    }

    #[test]
    fn reveal_all_pulls_negative_content_back_inside_and_undoes() {
        let imported =
            crate::import::document_from_image(&noise_image(32, 32), "bit.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        let layer = d.document.active_layer().unwrap();
        let content = d.composite(PixelRect::new(0, 0, 32, 32)).unwrap();

        // Drag the layer 20 pixels off the left edge: its pixels sit at
        // negative document coordinates, which a grow alone cannot contain.
        let nudge = Command::TransformLayer {
            layer_id: layer,
            matrix: tools::edit::translation_matrix(glam::Vec2::new(-20.0, 0.0)),
        };
        d.history.apply(&mut d.document, nudge).unwrap();

        let command = d.reveal_all_command().unwrap();
        d.history.apply(&mut d.document, command).unwrap();
        // The union (−20..12) shifts to (0..32): the canvas need not grow,
        // but every root layer moves +20 so nothing sits outside the origin.
        assert_eq!((d.document.width(), d.document.height()), (32, 32));
        assert_eq!(
            d.composite(PixelRect::new(0, 0, 32, 32)).unwrap(),
            content,
            "the off-canvas content did not come back inside"
        );
        d.history.undo(&mut d.document).unwrap();
        // Undo put the layer back at −20 and the canvas back at 32×32.
        assert_eq!((d.document.width(), d.document.height()), (32, 32));
    }

    #[test]
    fn reveal_all_on_a_contained_canvas_reveals_nothing() {
        let imported =
            crate::import::document_from_image(&noise_image(8, 8), "bit.png", 100).unwrap();
        let mut d = OpenDocument::from_import(DocumentId(1), imported);
        let before = d.composite(d.canvas_rect()).unwrap();
        // The image fills its own canvas exactly: nothing to reveal, so the
        // command is the empty transaction the performer skips recording.
        let command = d.reveal_all_command().unwrap();
        assert!(
            matches!(&command, Command::Transaction { commands, .. } if commands.is_empty()),
            "a contained canvas grew a reveal command"
        );
        assert_eq!(d.composite(d.canvas_rect()).unwrap(), before);
        assert_eq!(d.history.undo_depth(), 0);
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
            rgba.as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [128, 128, 128, 128]),
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
    fn exporting_over_the_imported_original_is_refused_loudly() {
        // Card 077: the original .psd this document was opened from is never
        // silently replaced by this build's reduced export of it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artwork.psd");
        std::fs::write(&path, layered_psd_bytes()).unwrap();
        let mut d = OpenDocument::open_image(DocumentId(11), &path, 100).unwrap();

        let err = d.export_psd_to(&path).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("refusing to overwrite"),
            "the refusal must say why: {message}"
        );
        assert!(
            message.contains("Save As"),
            "the refusal says what to do instead: {message}"
        );
        // The original is intact after the refusal.
        assert_eq!(std::fs::read(&path).unwrap(), layered_psd_bytes());

        // A different name is not the original: that export is allowed.
        let other = dir.path().join("export.psd");
        assert!(d.export_psd_to(&other).is_ok());
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

    // -----------------------------------------------------------------------
    // W1-A: the Layers panel's thumbnail cache
    // -----------------------------------------------------------------------

    /// One opaque `[v, v, v, 255]` tile at `coord` of `id`, through the real
    /// command route.
    fn paint_tile(d: &mut OpenDocument, id: LayerId, coord: raster::TileCoord, v: u8) {
        let mut bytes = Vec::with_capacity((TILE_SIZE * TILE_SIZE * 4) as usize);
        for _ in 0..TILE_SIZE * TILE_SIZE {
            bytes.extend_from_slice(&[v, v, v, 255]);
        }
        let hash = d.tiles.insert_bytes(bytes);
        d.apply(
            Command::paint_tiles(PixelTarget::Layer(id), vec![TileEdit::set(coord, hash)]).unwrap(),
        )
        .unwrap();
    }

    /// A raster layer with one opaque grey tile at the canvas origin.
    fn inked_layer(d: &mut OpenDocument, name: &str, v: u8) -> LayerId {
        let layer = layer_model::Layer::raster(name);
        let id = layer.id;
        d.apply(Command::create_layer(layer)).unwrap();
        paint_tile(d, id, raster::TileCoord::new(0, 0, 0), v);
        id
    }

    #[test]
    fn a_layer_thumbnail_is_served_from_the_cache_until_that_layer_changes() {
        let mut d = OpenDocument::blank(DocumentId(7001), 600, 400, "stack", 32).unwrap();
        let a = inked_layer(&mut d, "a", 40);
        let b = inked_layer(&mut d, "b", 90);
        let mut cache = LayerThumbCache::default();

        // First ask: composited.
        let before = thumbnail_composites();
        let first = cache.layer_thumbnail(&d, a, 64).unwrap();
        assert!(first.fresh, "the first lookup composites");
        let (fw, fh, first_px) = (first.width, first.height, first.rgba.to_vec());
        assert!(thumbnail_composites() > before, "the compositor ran");
        assert_eq!((cache.recomposites(), cache.hits()), (1, 0));

        // Second ask, nothing changed: the stored pixels, no compositor call.
        let mid = thumbnail_composites();
        let again = cache.layer_thumbnail(&d, a, 64).unwrap();
        assert!(!again.fresh, "the second lookup is a cache hit");
        assert_eq!(
            (again.width, again.height, again.rgba),
            (fw, fh, &first_px[..])
        );
        assert_eq!(thumbnail_composites(), mid, "a hit never composites");
        assert_eq!((cache.recomposites(), cache.hits()), (1, 1));

        // The other layer has its own entry.
        assert!(cache.layer_thumbnail(&d, b, 64).unwrap().fresh);
        assert_eq!(cache.recomposites(), 2);
        assert!(cache.layer_is_current(&d, a, 64) && cache.layer_is_current(&d, b, 64));

        // Painting `b` invalidates exactly `b`.
        paint_tile(&mut d, b, raster::TileCoord::new(1, 0, 0), 200);
        assert!(
            cache.layer_is_current(&d, a, 64),
            "a's pixels did not change"
        );
        assert!(!cache.layer_is_current(&d, b, 64), "b's pixels did");
        assert!(!cache.layer_thumbnail(&d, a, 64).unwrap().fresh);
        assert!(cache.layer_thumbnail(&d, b, 64).unwrap().fresh);
        assert_eq!(cache.recomposites(), 3);

        // The thumbnail shows the layer's content whether or not the layer is
        // shown, so toggling visibility is not a change to it...
        d.document.layers.get_mut(a).unwrap().visible = false;
        assert!(
            cache.layer_is_current(&d, a, 64),
            "visibility is not content"
        );
        // ...while a parameter the composite honours is.
        d.document.layers.get_mut(a).unwrap().opacity = 0.5;
        assert!(
            !cache.layer_is_current(&d, a, 64),
            "opacity changes the thumbnail"
        );
        // A different edge is a different thumbnail.
        assert!(!cache.layer_is_current(&d, b, 32));
    }

    #[test]
    fn a_content_bounded_thumbnail_matches_the_whole_canvas_downscale() {
        // Ink in one tile away from the origin; the rest of the canvas is
        // transparent. The thumbnail composites the layer's bounds only and
        // must still equal "composite the whole canvas, then box-downscale".
        let mut d = OpenDocument::blank(DocumentId(7002), 700, 500, "corner", 32).unwrap();
        let layer = d.document.active_layer().unwrap();
        let mut bytes = Vec::with_capacity((TILE_SIZE * TILE_SIZE * 4) as usize);
        let mut state = 0x9e37_79b9u32;
        for _ in 0..TILE_SIZE * TILE_SIZE {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            bytes.extend_from_slice(&[
                (state >> 24) as u8,
                (state >> 16) as u8,
                (state >> 8) as u8,
                255,
            ]);
        }
        let hash = d.tiles.insert_bytes(bytes);
        d.apply(
            Command::paint_tiles(
                PixelTarget::Layer(layer),
                vec![TileEdit::set(raster::TileCoord::new(1, 1, 0), hash)],
            )
            .unwrap(),
        )
        .unwrap();

        let whole = d.composite(d.canvas_rect()).unwrap();
        let reference = OpenDocument::box_downscale(&whole, 700, 500, 64);
        let thumb = d.layer_thumbnail(layer, 64).unwrap();
        assert_eq!(
            (thumb.0, thumb.1),
            (reference.0, reference.1),
            "same extent"
        );
        assert_eq!(
            thumb.2, reference.2,
            "same pixels as the whole-canvas downscale"
        );
        // And it is a real picture: ink where the tile is, nothing elsewhere.
        let (tw, th) = (thumb.0 as usize, thumb.1 as usize);
        let px = |x: usize, y: usize| &thumb.2[(y * tw + x) * 4..(y * tw + x) * 4 + 4];
        assert_eq!(px(0, 0)[3], 0, "the empty corner is transparent");
        assert_eq!(px(tw - 1, th - 1)[3], 0, "the far corner is transparent");
        // Tile (1,1) covers canvas 256..512 on both axes: about 0.37..0.73 of the way.
        let (cx, cy) = ((tw as f32 * 0.55) as usize, (th as f32 * 0.55) as usize);
        assert_eq!(
            px(cx, cy)[3],
            255,
            "the inked tile is opaque in the thumbnail"
        );
    }

    #[test]
    fn a_hidden_layer_and_a_layer_in_a_hidden_group_still_thumbnail_their_content() {
        let mut d = OpenDocument::blank(DocumentId(7003), 300, 300, "hidden", 32).unwrap();
        let a = inked_layer(&mut d, "a", 77);
        d.document.layers.get_mut(a).unwrap().visible = false;
        let (_, _, px) = d.layer_thumbnail(a, 64).unwrap();
        assert_eq!(
            &px[0..4],
            &[77, 77, 77, 255],
            "a hidden layer shows its pixels"
        );

        // A child of a hidden group: the group is an ancestor, so it stays
        // visible for the child's thumbnail.
        let b = inked_layer(&mut d, "b", 33);
        let group = layer_model::Layer::group("g");
        let gid = group.id;
        d.apply(Command::create_layer(group)).unwrap();
        d.document.layers.move_layer(b, Some(gid), 0).unwrap();
        d.document.layers.get_mut(gid).unwrap().visible = false;
        let (_, _, px) = d.layer_thumbnail(b, 64).unwrap();
        assert_eq!(
            &px[0..4],
            &[33, 33, 33, 255],
            "a child of a hidden group shows its pixels"
        );
        // And the group's own thumbnail is its children.
        let (_, _, px) = d.layer_thumbnail(gid, 64).unwrap();
        assert_eq!(&px[0..4], &[33, 33, 33, 255], "a group shows its children");
    }

    #[test]
    fn the_thumbnail_accumulator_agrees_with_box_downscale_on_split_regions() {
        // Two regions that together cover the canvas, fed in bands, equal the
        // one-shot downscale of the whole buffer.
        let (w, h) = (37u32, 23u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for i in 0..w * h {
            let v = (i * 37 % 251) as u8;
            rgba.extend_from_slice(&[v, v.wrapping_mul(3), v.wrapping_add(9), 255 - v / 2]);
        }
        let (tw, th, reference) = OpenDocument::box_downscale(&rgba, w, h, 10);
        assert_eq!((tw, th), OpenDocument::thumb_extent(w, h, 10));
        let mut acc = ThumbAccumulator::new(w, h, tw, th);
        // Top band rows 0..9, bottom band rows 9..23, each a sub-buffer.
        for (y0, rows) in [(0u32, 9u32), (9, 14)] {
            let band: Vec<u8> =
                rgba[(y0 * w * 4) as usize..((y0 + rows) * w * 4) as usize].to_vec();
            acc.add_region(&band, PixelRect::new(0, i64::from(y0), w, rows));
        }
        assert_eq!(acc.finish(), reference);
    }

    // ------------------------------------------------- partial invalidation ---

    /// The level-0 tiles `rect` touches on a `w` x `h` canvas — the test's own
    /// arithmetic, so it can say what the document *should* have marked.
    fn tiles_under(rect: PixelRect, w: u32, h: u32) -> std::collections::BTreeSet<TileCoord> {
        let mut out = std::collections::BTreeSet::new();
        let t = i64::from(TILE_SIZE);
        let x0 = rect.x.max(0);
        let y0 = rect.y.max(0);
        let x1 = rect.right().min(i64::from(w));
        let y1 = rect.bottom().min(i64::from(h));
        if x1 <= x0 || y1 <= y0 {
            return out;
        }
        for ty in y0.div_euclid(t)..=(y1 - 1).div_euclid(t) {
            for tx in x0.div_euclid(t)..=(x1 - 1).div_euclid(t) {
                out.insert(TileCoord::new(tx as i32, ty as i32, 0));
            }
        }
        out
    }

    fn styled(d: &OpenDocument, id: LayerId) -> Option<PixelRect> {
        compositor::bounds::styled_bounds(&d.document, &d.tiles, id, 0, CompositeOptions::default())
            .unwrap()
    }

    /// The finding's second half: every keystroke of a live text draft marked
    /// the whole canvas. A keystroke reaches the text's styled ink box before
    /// and after the edit, and nothing else — on a 4K canvas that is a
    /// handful of tiles out of 256.
    #[test]
    fn a_text_keystroke_invalidates_only_the_tiles_under_the_text() {
        let mut d = OpenDocument::blank(DocumentId(4), 4096, 4096, "type", 32).unwrap();
        let original = layer_model::TextLayer {
            text: "Headline".to_string(),
            font_family: "DejaVu Sans".to_string(),
            size_px: 48.0,
            ..layer_model::TextLayer::default()
        };
        let layer = Layer::with_kind("Type", LayerKind::Text(original.clone()));
        let id = layer.id;
        d.apply(Command::create_layer(layer)).unwrap();
        let _ = d.take_dirty();

        let before = styled(&d, id).expect("the text has an ink box");
        d.apply_text_draft(
            id,
            LayerKind::Text(layer_model::TextLayer {
                text: "Headline, and then a good deal more of it".to_string(),
                ..original.clone()
            }),
        )
        .unwrap();
        let after = styled(&d, id).expect("the longer text has an ink box");
        assert!(after.right() > before.right(), "the draft grew the ink box");

        let dirty = d.take_dirty();
        assert!(
            !dirty.is_all(),
            "a keystroke re-uploaded the whole 4096x4096 canvas"
        );
        let marked: std::collections::BTreeSet<TileCoord> = dirty.tiles().collect();
        let mut expected = tiles_under(before, 4096, 4096);
        expected.extend(tiles_under(after, 4096, 4096));
        assert!(!expected.is_empty());
        assert_eq!(
            marked, expected,
            "exactly the tiles under the old and the new ink box"
        );
        assert!(marked.len() < 16 * 16);

        // And a keystroke that *shrinks* the text still clears the glyphs it
        // removed: the old box is in the set as well as the new one.
        let wide = after;
        d.apply_text_draft(id, LayerKind::Text(original)).unwrap();
        let narrow = styled(&d, id).unwrap();
        let marked: std::collections::BTreeSet<TileCoord> = d.take_dirty().tiles().collect();
        let mut expected = tiles_under(wide, 4096, 4096);
        expected.extend(tiles_under(narrow, 4096, 4096));
        assert_eq!(marked, expected);
    }

    /// Undoing a move has two spots to redraw: where the layer is now (it
    /// leaves) and where it was (it returns). Marking only one of them is a
    /// ghost on the canvas.
    #[test]
    fn undoing_a_moved_layer_redraws_where_it_left_and_where_it_returns() {
        let mut d = doc_of(TILE_SIZE * 4, TILE_SIZE * 3); // 12 tiles
        let ink = Layer::raster("Ink");
        let id = ink.id;
        d.apply(Command::create_layer(ink)).unwrap();
        let mut tile = Tile::transparent(raster::PixelFormat::Rgba8);
        tile.data_mut().fill(250);
        let hash = d.tiles.insert_tile(&tile);
        d.apply(Command::PaintTiles {
            target: PixelTarget::Layer(id),
            delta: TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash)),
        })
        .unwrap();
        d.apply(Command::TransformLayer {
            layer_id: id,
            matrix: [1.0, 0.0, 0.0, 1.0, 300.0, 0.0],
        })
        .unwrap();
        let _ = d.take_dirty();

        assert!(d.undo().unwrap());
        let dirty = d.take_dirty();
        assert!(
            !dirty.is_all(),
            "an undo of a move re-uploaded the whole canvas"
        );
        // The spot it returns to, and the tiles it left: the moved tile is
        // x 300..556, grown by the compositor's two-pixel bilinear margin to
        // 298..558 x -2..258, which reaches into row 1. Rows 2 and column 3
        // are untouched: 5 of 12 tiles.
        assert_eq!(
            dirty.tiles().collect::<Vec<_>>(),
            vec![
                TileCoord::new(0, 0, 0),
                TileCoord::new(1, 0, 0),
                TileCoord::new(1, 1, 0),
                TileCoord::new(2, 0, 0),
                TileCoord::new(2, 1, 0),
            ],
        );

        assert!(d.redo().unwrap());
        let dirty = d.take_dirty();
        assert!(!dirty.is_all());
        assert_eq!(dirty.tiles().count(), 5, "and the same five on redo");
    }

    /// The fallbacks that keep partial invalidation honest: an adjustment
    /// reaches everything beneath it, and a layer under a transformed group
    /// has bounds in the group's space, not the document's.
    #[test]
    fn an_adjustment_or_a_transformed_ancestor_still_invalidates_everything() {
        let mut d = doc_of(TILE_SIZE * 2, TILE_SIZE * 2);
        let adj = Layer::with_kind(
            "Invert",
            LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: layer_model::AdjustmentKind::Invert,
            }),
        );
        let adj_id = adj.id;
        d.apply(Command::create_layer(adj)).unwrap();
        let _ = d.take_dirty();
        assert!(d.undo().unwrap());
        assert!(d.take_dirty().is_all(), "undoing an adjustment's creation");
        assert!(d.redo().unwrap());
        assert!(d.take_dirty().is_all());
        d.apply(Command::SetLayerProperties {
            layer_id: adj_id,
            patch: editor_core::LayerPatch {
                opacity: Some(0.5),
                ..Default::default()
            },
        })
        .unwrap();
        let _ = d.take_dirty();
        assert!(d.undo().unwrap());
        assert!(d.take_dirty().is_all(), "undoing an adjustment's opacity");

        // A raster child of a moved group.
        let group = Layer::with_kind("G", LayerKind::Group(layer_model::GroupLayer::default()));
        let group_id = group.id;
        d.apply(Command::create_layer(group)).unwrap();
        d.apply(Command::TransformLayer {
            layer_id: group_id,
            matrix: [1.0, 0.0, 0.0, 1.0, 100.0, 0.0],
        })
        .unwrap();
        let child = Layer::raster("Child");
        let child_id = child.id;
        d.apply(Command::create_layer(child)).unwrap();
        d.apply(Command::MoveLayer {
            layer_id: child_id,
            parent: Some(group_id),
            index: 0,
        })
        .unwrap();
        let mut tile = Tile::transparent(raster::PixelFormat::Rgba8);
        tile.data_mut().fill(9);
        let hash = d.tiles.insert_tile(&tile);
        d.apply(Command::PaintTiles {
            target: PixelTarget::Layer(child_id),
            delta: TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash)),
        })
        .unwrap();
        let _ = d.take_dirty();
        assert!(d.undo().unwrap());
        assert!(
            d.take_dirty().is_all(),
            "a stroke under a moved group has no document-space rectangle here"
        );
    }
}
