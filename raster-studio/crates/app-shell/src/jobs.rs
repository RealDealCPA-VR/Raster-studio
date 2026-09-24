//! Off-thread long operations (card 087, extended by W2-G).
//!
//! Five things used to run on the interaction thread that have no business
//! there: reading and decoding a file on open, parsing a layered `.psd`,
//! saving a package (Ctrl+S, Save As, and the autosave that fires every five
//! minutes on every dirty document), running an Export As batch, and
//! compositing and encoding a File ▸ Export… file or an Export Layers…
//! folder. Each is pure data work — bytes, a `Document`, a tile map — with no
//! UI type in reach, so each runs on a worker; the interaction thread polls
//! the outcome once per frame and applies it there.
//!
//! # A save works on a snapshot
//!
//! A [`SaveJob`] carries its *own* `Document` and `MemoryTileSource`, cloned
//! at spawn time. The document is content-addressed — it holds tile hashes,
//! the tile map holds the bytes — so the clone is the identity of the pixels,
//! and the user can keep painting while the worker writes: nothing the worker
//! reads can change under it, and nothing it writes can reach the live
//! document. What it hands back is a [`SaveOutcome`], applied by
//! [`crate::Editor::poll_saves`], which decides whether the live document is
//! now clean (its digest still matches the snapshot's) or was edited while the
//! save ran.
//!
//! # Stale completions cannot apply
//!
//! Every import job carries the [`crate::Editor`]'s import generation at spawn
//! time. The generation is bumped whenever pending imports are cancelled, and
//! a completion whose generation no longer matches is dropped unread at poll
//! time — an out-of-order or cancelled completion can never mutate the
//! document set. The document a completed job becomes is minted fresh at
//! apply time, so a completed import can never land inside the wrong document
//! either. A save or export completion names the document by id; one whose
//! document has since been closed updates nothing but the status line.
//!
//! # The spawner is a seam
//!
//! Every job goes through a [`Spawner`]: a plain function that either runs a
//! body on another thread ([`spawn_thread`], what the desktop binary uses) or
//! says why not. [`run_inline`] runs the body before returning — the
//! deterministic mode the unit tests run in — and a test can supply its own
//! that *queues* bodies, to hold a job in flight for exactly as many frames
//! as it wants to observe.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use compositor::MemoryTileSource;
use editor_core::Document;
use project_format::{SaveOptions, SaveProgress, SaveReport};

use crate::doc::DocumentId;

/// A way to start a worker: given a thread name and its body, either run the
/// body on another thread or say why not. Injected so the refusal route can
/// be exercised without exhausting the machine's threads, and so a test can
/// hold a job in flight deterministically.
pub type Spawner = fn(String, Box<dyn FnOnce() + Send>) -> std::io::Result<()>;

/// The production spawner: one named OS thread per job.
pub fn spawn_thread(name: String, body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name)
        .spawn(body)
        .map(|_handle| ())
}

/// The inline spawner: the body runs to completion before this returns, on
/// the calling thread. Every job then completes before the call that started
/// it returns, which is what the synchronous unit tests rely on.
pub fn run_inline(_name: String, body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    body();
    Ok(())
}

// --------------------------------------------------------------- imports

/// What a finished import job hands back. Pure data: a worker thread must
/// never touch UI types, so everything here is `Send` by construction.
pub enum ImportOutcome {
    /// A flat image, decoded off-thread.
    Image {
        path: PathBuf,
        generation: u64,
        decoded: Result<crate::import::DecodedImage, String>,
        /// The source carries 16 bits per channel. Recorded off-thread the
        /// same way the synchronous `OpenDocument::open_image` does, so File >
        /// Open and drag-and-drop agree on a 16-bit PNG/TIFF's depth.
        sixteen_bit: bool,
    },
    /// A `.psd`, read *and* parsed off-thread: the layered document, its
    /// tiles and the fidelity notes arrive ready to wrap. (Before W2-G only
    /// the bytes travelled and the parse — the expensive half — ran on the
    /// interaction thread.)
    Psd {
        path: PathBuf,
        generation: u64,
        /// Boxed: a parsed document is a few hundred bytes of headers next
        /// to the flat variant's, and the outcome travels by value.
        parsed: Result<Box<crate::import::PsdImport>, String>,
    },
    /// W9-J: an animated GIF / APNG / WebP, decoded off-thread into one
    /// `_a_<name>,<delay ms>` frame layer per frame (Photopea's convention).
    Animated {
        path: PathBuf,
        generation: u64,
        /// Boxed for the same reason as [`Self::Psd`]'s document.
        parsed: Result<Box<crate::import::ImportedDocument>, String>,
    },
}

impl ImportOutcome {
    /// The job's spawn-time generation.
    pub fn generation(&self) -> u64 {
        match self {
            Self::Image { generation, .. }
            | Self::Psd { generation, .. }
            | Self::Animated { generation, .. } => *generation,
        }
    }

    /// The file the job came from.
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::Image { path, .. } | Self::Psd { path, .. } | Self::Animated { path, .. } => path,
        }
    }

    /// `true` when the outcome belongs to a cancelled generation and must be
    /// dropped unread.
    pub fn is_stale(&self, current: u64) -> bool {
        self.generation() != current
    }
}

/// Largest flat image the worker will decode, matching the synchronous
/// route's own ceiling philosophy: finite, far past any real document.
const MAX_IMPORT_BYTES: u64 = 2 << 30;

/// Spawn a worker that reads and decodes `path`, reporting one
/// [`ImportOutcome`] tagged with `generation`. The receiver is the only
/// channel back; dropping it simply discards the result.
///
/// When the OS refuses the thread (out of handles, out of address space —
/// rare, but a live process can hit it), no worker runs and the *failure*
/// arrives on the receiver instead, shaped like the import it would have
/// been: the caller's poll reports it through the same route as a decode
/// error, and the interaction thread never panics over it.
pub fn spawn_import(path: PathBuf, generation: u64) -> Receiver<ImportOutcome> {
    spawn_import_with(
        path,
        generation,
        editor_core::DEFAULT_HISTORY_LIMIT,
        spawn_thread,
    )
}

/// [`spawn_import`] with the history depth a parsed `.psd`'s document starts
/// with and the thread spawner injected.
pub fn spawn_import_with(
    path: PathBuf,
    generation: u64,
    history_depth: usize,
    spawn: Spawner,
) -> Receiver<ImportOutcome> {
    let (tx, rx) = channel();
    let name = format!("import:{}", path.display());
    let worker_path = path.clone();
    let worker_tx = tx.clone();
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        let outcome = run(worker_path, generation, history_depth);
        // A send fails only when the receiver is gone — the user quit or
        // the job was superseded. That is not an error; the result is
        // simply not needed any more.
        let _ = worker_tx.send(outcome);
    });
    if let Err(e) = spawn(name, body) {
        let reason = format!("could not start the import worker: {e}");
        let _ = tx.send(failed_outcome(path, generation, reason));
    }
    rx
}

/// The outcome an import that never ran reports: the same variant a real
/// worker would have produced for `path`, carrying `reason` as its error.
fn failed_outcome(path: PathBuf, generation: u64, reason: String) -> ImportOutcome {
    if crate::import::looks_like_psd(&path) {
        ImportOutcome::Psd {
            path,
            generation,
            parsed: Err(reason),
        }
    } else {
        ImportOutcome::Image {
            path,
            generation,
            decoded: Err(reason),
            sixteen_bit: false,
        }
    }
}

/// The worker's body: read, and decode what decodes — a flat image's pixels,
/// a `.psd`'s whole layer tree.
fn run(path: PathBuf, generation: u64, history_depth: usize) -> ImportOutcome {
    // W10-F: a GIMP `.xcf` opens layered, its report on the PSD road's notes.
    if crate::import::looks_like_xcf(&path) {
        let parsed = read_bounded(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                let title = crate::import::DecodedImage::title_for(&path);
                crate::import::document_from_xcf(&bytes, &title, history_depth)
                    .map(Box::new)
                    .map_err(|e| e.to_string())
            });
        return ImportOutcome::Psd {
            path,
            generation,
            parsed,
        };
    }
    if crate::import::looks_like_psd(&path) {
        let parsed = read_bounded(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                let title = crate::import::DecodedImage::title_for(&path);
                crate::import::document_from_psd(&bytes, &title, history_depth)
                    .map(Box::new)
                    .map_err(|e| e.to_string())
            });
        ImportOutcome::Psd {
            path,
            generation,
            parsed,
        }
    } else {
        let bytes = read_bounded(&path);
        // W9-J: an animation of two or more frames opens as frame layers. A
        // file whose animation cannot be read (damaged later frames, or past
        // `raster::animation`'s frame bounds) still opens the way it always
        // did — its first frame, through the flat decode below.
        if let Ok(bytes) = &bytes {
            match raster::animation::decode_animation_bytes(bytes, raster::ImportLimits::default())
            {
                Ok(Some(animation)) => {
                    let title = crate::import::DecodedImage::title_for(&path);
                    let parsed =
                        crate::import::document_from_animation(&animation, &title, history_depth)
                            .map(Box::new)
                            .map_err(|e| e.to_string());
                    return ImportOutcome::Animated {
                        path,
                        generation,
                        parsed,
                    };
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    "{}: animation unreadable, opening the first frame: {e}",
                    path.display()
                ),
            }
        }
        let (decoded, sixteen_bit) = match bytes {
            Ok(bytes) => {
                let decoded =
                    crate::import::DecodedImage::decode_bytes(&bytes).map_err(|e| e.to_string());
                let sixteen_bit = decoded.is_ok()
                    && raster::decode_surface_bytes(&bytes, raster::ImportLimits::default())
                        .is_ok_and(|s| s.format() == raster::PixelFormat::Rgba16);
                (decoded, sixteen_bit)
            }
            Err(e) => (Err(e.to_string()), false),
        };
        ImportOutcome::Image {
            path,
            generation,
            decoded,
            sixteen_bit,
        }
    }
}

/// Read a whole file, refusing anything past [`MAX_IMPORT_BYTES`] before it
/// is read — the same rule the synchronous PSD path applies.
fn read_bounded(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let declared = file.metadata()?.len();
    if declared > MAX_IMPORT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "this file is {declared} bytes, more than the {MAX_IMPORT_BYTES} this build will read"
            ),
        ));
    }
    let mut bytes = Vec::new();
    // Not `with_capacity(declared)`: that reserves whatever the file claims
    // (the metadata can lie). `take` bounds what is actually read.
    file.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "this file is {} bytes, more than the {MAX_IMPORT_BYTES} this build will read",
                bytes.len()
            ),
        ));
    }
    Ok(bytes)
}

// ----------------------------------------------------------------- saves

/// Which kind of write a [`SaveJob`] is — it decides what the completion does
/// to the live document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    /// Ctrl+S / Save As: on success the document adopts the path and, if it
    /// was not edited meanwhile, becomes clean.
    Save,
    /// The timer's safety net: the document stays dirty and adopts nothing.
    /// `scratch` says the target is in the scratch directory (a document with
    /// no package of its own) rather than the document's own package.
    Autosave { scratch: bool },
}

/// A save, ready to run anywhere: an immutable snapshot of the document and
/// its pixels, and where to put them.
pub struct SaveJob {
    pub id: DocumentId,
    pub kind: SaveKind,
    pub target: PathBuf,
    /// The document as it was at spawn time. Content-addressed, so this clone
    /// *is* the pixels' identity; the bytes are in `tiles`.
    pub document: Document,
    pub tiles: MemoryTileSource,
    pub app_version: String,
    /// Shared with the interaction thread, which reads it for the status bar.
    pub progress: Arc<SaveProgress>,
}

/// What a finished save hands back.
pub struct SaveOutcome {
    pub id: DocumentId,
    pub kind: SaveKind,
    pub target: PathBuf,
    /// The package's report, or why it was not written. A failed save has
    /// left the previous package exactly as it was (`project_format`'s swap
    /// guarantees it), so the only thing to do with the error is say it.
    pub result: Result<SaveReport, String>,
}

/// Spawn a worker that writes `job`'s snapshot to its target.
pub fn spawn_save_with(job: SaveJob, spawn: Spawner) -> Receiver<SaveOutcome> {
    let (tx, rx) = channel();
    let name = format!("save:{}", job.target.display());
    let (id, kind, target) = (job.id, job.kind, job.target.clone());
    let worker_tx = tx.clone();
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        let result = run_save(&job).map_err(|e| e.to_string());
        let _ = worker_tx.send(SaveOutcome {
            id: job.id,
            kind: job.kind,
            target: job.target,
            result,
        });
    });
    if let Err(e) = spawn(name, body) {
        let _ = tx.send(SaveOutcome {
            id,
            kind,
            target,
            result: Err(format!("could not start the save worker: {e}")),
        });
    }
    rx
}

/// The worker's body: the same package write the synchronous route did.
fn run_save(job: &SaveJob) -> Result<SaveReport, crate::doc::DocumentError> {
    if let Some(parent) = job.target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| crate::doc::DocumentError::Io(e.to_string()))?;
        }
    }
    let report = project_format::save_project_with(
        &job.target,
        &job.document,
        &crate::doc::SourceTiles(&job.tiles),
        &SaveOptions::new(&job.app_version).reporting_to(job.progress.clone()),
    )?;
    Ok(report)
}

// --------------------------------------------------------------- exports

/// An Export As batch, ready to run anywhere: a snapshot of the document and
/// its pixels, the dialog's job, and the folder the picker chose.
pub struct ExportJob {
    pub id: DocumentId,
    pub document: Document,
    pub tiles: MemoryTileSource,
    pub job: ui::dialogs::ExportJob,
    pub dir: PathBuf,
}

/// What a finished export hands back: the files written, or why not.
pub struct ExportOutcome {
    pub id: DocumentId,
    /// The folder an Export As batch or an Export Layers run wrote into, or
    /// the one file a File ▸ Export… wrote (`single`).
    pub dir: PathBuf,
    /// Which route this was, for the wording of the completion.
    pub route: ExportRoute,
    pub result: Result<Vec<PathBuf>, String>,
    /// What a `.psd` export could not carry, when the file was a `.psd`.
    pub psd_notes: Option<crate::import::PsdNotes>,
}

/// The three export routes that share [`ExportOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportRoute {
    /// Export As: every enabled preset into a folder.
    Batch,
    /// File ▸ Export…: one flattened file (or a layered `.psd`).
    File,
    /// File ▸ Export Layers…: one PNG per layer into a folder.
    Layers,
}

/// A File ▸ Export…, ready to run anywhere: a snapshot of the document and
/// its pixels, the file to write, and whether the source was deep enough to
/// go out at 16 bits.
pub struct FileExportJob {
    pub id: DocumentId,
    pub target: PathBuf,
    pub document: Document,
    pub tiles: MemoryTileSource,
    /// [`crate::doc::OpenDocument::is_sixteen_bit`] at spawn time.
    pub sixteen_bit: bool,
}

/// An Export Layers…, ready to run anywhere: a snapshot and the folder.
pub struct LayerExportJob {
    pub id: DocumentId,
    pub document: Document,
    pub tiles: MemoryTileSource,
    pub dir: PathBuf,
}

/// Spawn a worker that composites `job`'s snapshot and writes it as one
/// file — the same file [`crate::doc::OpenDocument::export_to`] writes, off
/// the interaction thread. A `.psd` destination keeps its layers and hands
/// its fidelity notes back on the outcome.
pub fn spawn_file_export_with(job: FileExportJob, spawn: Spawner) -> Receiver<ExportOutcome> {
    let (tx, rx) = channel();
    let name = format!("export:{}", job.target.display());
    let (id, target) = (job.id, job.target.clone());
    let worker_tx = tx.clone();
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        let (result, psd_notes) = match run_file_export(&job) {
            Ok(notes) => (Ok(vec![job.target.clone()]), notes),
            Err(e) => (Err(e.to_string()), None),
        };
        let _ = worker_tx.send(ExportOutcome {
            id: job.id,
            dir: job.target,
            route: ExportRoute::File,
            result,
            psd_notes,
        });
    });
    if let Err(e) = spawn(name, body) {
        let _ = tx.send(ExportOutcome {
            id,
            dir: target,
            route: ExportRoute::File,
            result: Err(format!("could not start the export worker: {e}")),
            psd_notes: None,
        });
    }
    rx
}

/// The worker's body for one file: what `OpenDocument::export_to` did on the
/// interaction thread, on a snapshot. The checks that need the *live*
/// document — the destination's format by name, card 077's refusal to
/// overwrite the `.psd` the document came from — ran before the job was
/// spawned, so a failure here is one of encoding or the disk.
fn run_file_export(
    job: &FileExportJob,
) -> Result<Option<crate::import::PsdNotes>, crate::doc::DocumentError> {
    let doc = &job.document;
    // W10-F: an SVG keeps shape layers as `<path>` and text as `<text>`,
    // through the same writer `OpenDocument::export_to` calls.
    if crate::doc::exports_as_svg(&job.target) {
        crate::doc::write_vector_svg(doc, &job.tiles, &job.target)?;
        return Ok(None);
    }
    let (w, h) = (doc.width(), doc.height());
    let rect = raster::PixelRect::new(0, 0, w, h);
    let canvas = compositor::composite_region(
        doc,
        &job.tiles,
        rect,
        0,
        compositor::CompositeOptions::default(),
    )?;
    if crate::doc::exports_as_psd(&job.target) {
        // The flattened image every previewer shows comes from this
        // application's compositor, not the `psd` crate's flattener.
        let rgba8 = canvas.to_rgba8(&doc.meta.color_space);
        let (bytes, notes) = crate::import::psd_from_document(doc, &job.tiles, &rgba8)?;
        crate::doc::write_atomically(&job.target, &bytes)
            .map_err(crate::import::ImportError::from)?;
        return Ok(Some(notes));
    }
    let format = crate::doc::export_format_for(&job.target).ok_or_else(|| {
        crate::doc::DocumentError::UnknownExportFormat(
            job.target
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| job.target.display().to_string()),
        )
    })?;
    // W7-D: a CMYK document writes a CMYK JPEG/TIFF and an Indexed one a
    // palette PNG (8 bits, `color::cmyk`'s documented ink model, no ICC
    // press profile); every other pairing falls through to the RGB file.
    // W8-B: the one colour-mode branch `OpenDocument::export_to` calls too.
    if crate::doc::write_in_document_ink(&job.target, format, doc.meta.color_mode, (w, h), || {
        Ok(canvas.to_rgba8(&doc.meta.color_space))
    })? {
        return Ok(None);
    }
    // W10-H: a 32 Bits/Channel document writes a 32-bit float TIFF.
    if crate::depth32::write_float_tiff(&job.target, format, doc, || Ok(canvas.clone()))? {
        return Ok(None);
    }
    // A tagged document re-tags: the profile it opened with rides back into
    // the file (the codec writes the iCCP chunk for the formats that carry
    // one).
    let encode_options = match &doc.meta.color_space {
        color::ColorSpace::IccProfile { profile, .. } if !profile.is_empty() => {
            raster::EncodeOptions::with_icc(profile.clone())
        }
        _ => raster::EncodeOptions::default(),
    };
    // A deep source may go back out at 16 bits to the formats that carry
    // them; the composite is `f32` either way.
    if job.sixteen_bit && format.supports_16_bit() {
        let rgba16 = canvas.to_rgba16(&doc.meta.color_space);
        raster::encode_to_path(
            &job.target,
            format,
            w,
            h,
            raster::EncodedPixels::Rgba16(&rgba16),
            &encode_options,
        )?;
    } else {
        let rgba8 = canvas.to_rgba8(&doc.meta.color_space);
        raster::encode_to_path(
            &job.target,
            format,
            w,
            h,
            raster::EncodedPixels::Rgba8(&rgba8),
            &encode_options,
        )?;
    }
    Ok(None)
}

/// Spawn a worker that writes each layer of `job`'s snapshot as its own PNG
/// into its folder — File ▸ Export Layers…, off the interaction thread.
pub fn spawn_layer_export_with(job: LayerExportJob, spawn: Spawner) -> Receiver<ExportOutcome> {
    let (tx, rx) = channel();
    let name = format!("export-layers:{}", job.dir.display());
    let (id, dir) = (job.id, job.dir.clone());
    let worker_tx = tx.clone();
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        let result = run_layer_export(&job).map_err(|e| e.to_string());
        let _ = worker_tx.send(ExportOutcome {
            id: job.id,
            dir: job.dir,
            route: ExportRoute::Layers,
            result,
            psd_notes: None,
        });
    });
    if let Err(e) = spawn(name, body) {
        let _ = tx.send(ExportOutcome {
            id,
            dir,
            route: ExportRoute::Layers,
            result: Err(format!("could not start the export worker: {e}")),
            psd_notes: None,
        });
    }
    rx
}

/// The worker's body for Export Layers: each layer composited *alone*
/// (every other layer hidden) over transparent, through the real compositor,
/// so effects and blends are honoured per layer.
fn run_layer_export(job: &LayerExportJob) -> Result<Vec<PathBuf>, crate::doc::DocumentError> {
    let doc = &job.document;
    let (w, h) = (doc.width(), doc.height());
    let rect = raster::PixelRect::new(0, 0, w, h);
    let mut written = Vec::new();
    for id in doc.layers.iter_depth_first() {
        let mut staged = doc.clone();
        for other in staged.layers.iter_depth_first() {
            if other != id {
                if let Some(l) = staged.layers.get_mut(other) {
                    l.visible = false;
                }
            }
        }
        let canvas = compositor::composite_region(
            &staged,
            &job.tiles,
            rect,
            0,
            compositor::CompositeOptions::default(),
        )?;
        let rgba8 = canvas.to_rgba8(&doc.meta.color_space);
        let name = doc
            .layers
            .get(id)
            .map(|l| crate::editor::safe_file_name(&l.name))
            .unwrap_or_else(|| "layer".to_string());
        let path = job.dir.join(format!("{name}.png"));
        raster::encode_to_path(
            &path,
            raster::ExportFormat::Png,
            w,
            h,
            raster::EncodedPixels::Rgba8(&rgba8),
            &raster::EncodeOptions::default(),
        )?;
        written.push(path);
    }
    Ok(written)
}

/// Spawn a worker that composites `job`'s snapshot and writes every enabled
/// preset into its folder.
pub fn spawn_export_with(job: ExportJob, spawn: Spawner) -> Receiver<ExportOutcome> {
    let (tx, rx) = channel();
    let name = format!("export:{}", job.dir.display());
    let (id, dir) = (job.id, job.dir.clone());
    let worker_tx = tx.clone();
    let body: Box<dyn FnOnce() + Send> = Box::new(move || {
        let result = run_export(&job).map_err(|e| e.to_string());
        let _ = worker_tx.send(ExportOutcome {
            id: job.id,
            dir: job.dir,
            route: ExportRoute::Batch,
            result,
            psd_notes: None,
        });
    });
    if let Err(e) = spawn(name, body) {
        let _ = tx.send(ExportOutcome {
            id,
            dir,
            route: ExportRoute::Batch,
            result: Err(format!("could not start the export worker: {e}")),
            psd_notes: None,
        });
    }
    rx
}

/// The worker's body: one composite through the free compositor (the road
/// the dialog's own preview takes), then `raster`'s batch exporter.
fn run_export(job: &ExportJob) -> Result<Vec<PathBuf>, crate::doc::DocumentError> {
    let doc = &job.document;
    let (w, h) = (doc.width(), doc.height());
    let canvas = compositor::composite_region(
        doc,
        &job.tiles,
        raster::PixelRect::new(0, 0, w, h),
        0,
        compositor::CompositeOptions::default(),
    )?;
    let rgba = canvas.to_rgba8(&doc.meta.color_space);
    let image = raster::export::linear_from_rgba8(w, h, &rgba, &doc.meta.color_space)?;
    let presets: Vec<raster::ExportPreset> = job
        .job
        .entries
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| {
            // W7-D: a CMYK document goes out as CMYK JPEG/TIFF, an
            // Indexed one as a palette PNG.
            let mut preset = entry.preset.clone().for_color_mode(doc.meta.color_mode);
            preset.name = format!("{}{}", job.job.base_name, entry.suffix);
            preset
        })
        .collect();
    let metadata = raster::export::ExportMetadata {
        icc_profile: None,
        icc_profile_space: None,
    };
    let written = raster::export::export_batch_to_dir(&job.dir, &image, &presets, &metadata)?;
    // W10-F: an SVG row at 100% is rewritten with shape layers as `<path>`
    // and text as `<text>` (`doc::write_vector_svg`); a scaled SVG row keeps
    // the codec's embedded-image SVG at its scaled size.
    for (preset, path) in presets.iter().zip(&written) {
        if preset.format == raster::ExportFormat::Svg && preset.scale == 1.0 {
            crate::doc::write_vector_svg(doc, &job.tiles, path)?;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(dir: &Path, w: u32, h: u32, v: u8) -> PathBuf {
        let path = dir.join(format!("img-{w}x{h}-{v}.png"));
        std::fs::write(
            &path,
            raster::encode(
                raster::ExportFormat::Png,
                w,
                h,
                &vec![v; (w * h * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn an_import_job_decodes_off_thread_and_reports_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), 8, 6, 77);
        let rx = spawn_import(path.clone(), 3);
        match rx.recv().expect("the job completes") {
            ImportOutcome::Image {
                path: p,
                generation,
                decoded,
                ..
            } => {
                assert_eq!(p, path);
                assert_eq!(generation, 3);
                let img = decoded.expect("the png decodes");
                assert_eq!((img.width, img.height), (8, 6));
                assert!(img.rgba8.iter().all(|&b| b == 77));
            }
            _ => panic!("expected an image outcome"),
        }
    }

    #[test]
    fn a_psd_job_parses_the_layer_tree_off_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(8, 8));
        let canvas = psd::Rect::sized(8, 8);
        let mut layer = psd::PsdLayer::raster("L", canvas);
        layer.set_rgba8(&vec![9u8; 8 * 8 * 4]).unwrap();
        file.layers = vec![layer];
        let path = dir.path().join("job.psd");
        std::fs::write(&path, psd::write(&file).unwrap()).unwrap();

        let rx = spawn_import(path.clone(), 1);
        match rx.recv().unwrap() {
            ImportOutcome::Psd {
                path: p, parsed, ..
            } => {
                assert_eq!(p, path);
                let import = parsed.expect("the psd parses on the worker");
                assert_eq!(import.imported.document.layers.len(), 1);
                assert!(
                    !import.imported.tiles.is_empty(),
                    "the pixels arrived with the tree"
                );
            }
            _ => panic!("expected a psd outcome"),
        }
    }

    #[test]
    fn a_missing_file_is_a_reported_failure_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let rx = spawn_import(dir.path().join("gone.png"), 0);
        match rx.recv().unwrap() {
            ImportOutcome::Image { decoded, .. } => assert!(decoded.is_err()),
            _ => panic!("expected an image outcome"),
        }
    }

    #[test]
    fn a_oversized_file_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.png");
        // A sparse-looking lie: the header says PNG, the metadata says huge.
        // The worker must refuse on the metadata before reserving anything.
        let mut f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_IMPORT_BYTES + 2).unwrap();
        std::io::Write::write_all(&mut f, b"RIFF-not-a-real-image").unwrap();
        drop(f);
        let rx = spawn_import(path, 0);
        match rx.recv().unwrap() {
            ImportOutcome::Image { decoded, .. } => {
                let e = match decoded {
                    Err(e) => e,
                    Ok(_) => panic!("expected a refusal"),
                };
                assert!(e.contains("more than the"), "{e}");
            }
            _ => panic!("expected an image outcome"),
        }
    }

    fn refuse(_name: String, _body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "no more threads",
        ))
    }

    #[test]
    fn a_refused_thread_is_reported_on_the_receiver_not_panicked() {
        let rx = spawn_import_with(PathBuf::from("photo.png"), 4, 8, refuse);
        match rx.recv().expect("the refusal arrives on the receiver") {
            ImportOutcome::Image {
                path,
                generation,
                decoded,
                ..
            } => {
                assert_eq!(path, PathBuf::from("photo.png"));
                assert_eq!(generation, 4);
                let e = decoded.expect_err("an import that never ran is a failure");
                assert!(e.contains("import worker"), "{e}");
                assert!(e.contains("no more threads"), "{e}");
            }
            _ => panic!("expected an image outcome"),
        }
        // A .psd that never ran fails in the .psd shape, so the poller's
        // existing failure route applies unchanged. (`looks_like_psd` goes by
        // content, so the file has to exist and open with the signature.)
        let dir = tempfile::tempdir().unwrap();
        let psd = dir.path().join("layers.psd");
        std::fs::write(&psd, b"8BPS\0\x01 not a whole document").unwrap();
        let rx = spawn_import_with(psd, 4, 8, refuse);
        match rx.recv().unwrap() {
            ImportOutcome::Psd { parsed, .. } => assert!(parsed.is_err()),
            _ => panic!("expected a psd outcome"),
        }
    }

    #[test]
    fn staleness_is_a_generation_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), 4, 4, 5);
        let rx = spawn_import(path, 7);
        let outcome = rx.recv().unwrap();
        assert!(!outcome.is_stale(7));
        assert!(outcome.is_stale(8), "a cancelled generation is stale");
    }

    /// A 2x2-tile document with real pixels, and its tiles.
    fn painted() -> (Document, MemoryTileSource) {
        let mut doc = Document::new(512, 512, "painted");
        let layer = layer_model::Layer::raster("L");
        let id = layer.id;
        doc.layers.push_root(layer).unwrap();
        let mut tiles = MemoryTileSource::new();
        for (x, y, v) in [(0, 0, 10u8), (1, 0, 20), (0, 1, 30), (1, 1, 40)] {
            let px = raster::TILE_SIZE as usize * raster::TILE_SIZE as usize;
            let hash = tiles.insert_bytes([v, v, v, 255].repeat(px));
            doc.pixels.apply(
                editor_core::PixelKey::Layer(id),
                &editor_core::TileDelta::single(editor_core::TileEdit::set(
                    raster::TileCoord::new(x, y, 0),
                    hash,
                )),
            );
        }
        (doc, tiles)
    }

    #[test]
    fn a_save_job_writes_the_snapshot_and_reports_progress() {
        let dir = tempfile::tempdir().unwrap();
        let (doc, tiles) = painted();
        let progress = SaveProgress::new();
        let target = dir.path().join("deep").join("P.rstudio");
        let rx = spawn_save_with(
            SaveJob {
                id: DocumentId(1),
                kind: SaveKind::Save,
                target: target.clone(),
                document: doc.clone(),
                tiles,
                app_version: "test".into(),
                progress: progress.clone(),
            },
            spawn_thread,
        );
        let outcome = rx.recv().unwrap();
        assert_eq!(outcome.id, DocumentId(1));
        assert_eq!(outcome.target, target);
        let report = outcome.result.expect("the save succeeds");
        assert_eq!(report.tiles.blobs_written, 4);
        assert_eq!((progress.tiles_total(), progress.tiles_done()), (4, 4));
        assert_eq!(
            project_format::load_project(&target).unwrap().layers.len(),
            1
        );
    }

    #[test]
    fn a_save_that_cannot_start_reports_the_refusal() {
        let (doc, tiles) = painted();
        let rx = spawn_save_with(
            SaveJob {
                id: DocumentId(2),
                kind: SaveKind::Autosave { scratch: true },
                target: PathBuf::from("nowhere.rstudio"),
                document: doc,
                tiles,
                app_version: "test".into(),
                progress: SaveProgress::new(),
            },
            refuse,
        );
        let outcome = rx.recv().unwrap();
        assert_eq!(outcome.kind, SaveKind::Autosave { scratch: true });
        let e = outcome
            .result
            .expect_err("a save that never ran is a failure");
        assert!(e.contains("save worker"), "{e}");
    }

    /// W2-G: File ▸ Export… is a job too: the snapshot is composited and
    /// encoded on the worker, and the file carries the pixels.
    #[test]
    fn a_file_export_job_writes_the_composite_off_thread() {
        let dir = tempfile::tempdir().unwrap();
        let (doc, tiles) = painted();
        let target = dir.path().join("flat.png");
        let rx = spawn_file_export_with(
            FileExportJob {
                id: DocumentId(5),
                target: target.clone(),
                document: doc,
                tiles,
                sixteen_bit: false,
            },
            spawn_thread,
        );
        let outcome = rx.recv().unwrap();
        assert_eq!(outcome.route, ExportRoute::File);
        assert_eq!(outcome.dir, target);
        assert_eq!(
            outcome.result.expect("the export succeeds"),
            vec![target.clone()]
        );
        assert!(outcome.psd_notes.is_none(), "not a .psd");
        let img = raster::codec::decode_bytes(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!((img.width, img.height), (512, 512));
        assert_eq!(&img.rgba8[..4], &[10, 10, 10, 255]);
        // The bottom-right tile was painted 40.
        let last = img.rgba8.len() - 4;
        assert_eq!(&img.rgba8[last..], &[40, 40, 40, 255]);
    }

    /// W2-G: a destination whose format nothing can write is a reported
    /// failure on the receiver, not a panic on the worker.
    #[test]
    fn a_file_export_to_an_unknown_format_reports_the_failure() {
        let dir = tempfile::tempdir().unwrap();
        let (doc, tiles) = painted();
        let rx = spawn_file_export_with(
            FileExportJob {
                id: DocumentId(6),
                target: dir.path().join("flat.xyz"),
                document: doc,
                tiles,
                sixteen_bit: false,
            },
            spawn_thread,
        );
        let e = rx.recv().unwrap().result.expect_err("no codec for .xyz");
        assert!(e.contains("xyz"), "{e}");
    }

    /// W2-G: Export Layers… writes one PNG per layer on the worker.
    #[test]
    fn a_layer_export_job_writes_one_png_per_layer() {
        let dir = tempfile::tempdir().unwrap();
        let (mut doc, tiles) = painted();
        doc.layers
            .push_root(layer_model::Layer::raster("Second"))
            .unwrap();
        let rx = spawn_layer_export_with(
            LayerExportJob {
                id: DocumentId(7),
                document: doc,
                tiles,
                dir: dir.path().to_path_buf(),
            },
            spawn_thread,
        );
        let outcome = rx.recv().unwrap();
        assert_eq!(outcome.route, ExportRoute::Layers);
        let paths = outcome.result.expect("the export succeeds");
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(dir.path().join("L.png").is_file());
        assert!(dir.path().join("Second.png").is_file());
    }

    #[test]
    fn an_export_job_composites_the_snapshot_and_writes_every_preset() {
        let dir = tempfile::tempdir().unwrap();
        let (doc, tiles) = painted();
        let rx = spawn_export_with(
            ExportJob {
                id: DocumentId(3),
                document: doc,
                tiles,
                job: ui::dialogs::ExportJob {
                    base_name: "out".into(),
                    entries: vec![
                        ui::dialogs::ExportEntry::new("", raster::ExportFormat::Png, 1.0),
                        ui::dialogs::ExportEntry::new("@half", raster::ExportFormat::Png, 0.5),
                    ],
                },
                dir: dir.path().to_path_buf(),
            },
            spawn_thread,
        );
        let outcome = rx.recv().unwrap();
        let paths = outcome.result.expect("the export succeeds");
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(dir.path().join("out.png").is_file());
        let mut sizes: Vec<(u32, u32)> = paths
            .iter()
            .map(|p| {
                assert!(p.is_file(), "{p:?}");
                let img = raster::codec::decode_bytes(&std::fs::read(p).unwrap()).unwrap();
                (img.width, img.height)
            })
            .collect();
        sizes.sort();
        assert_eq!(sizes, vec![(256, 256), (512, 512)], "{paths:?}");
        let full = raster::codec::decode_bytes(&std::fs::read(dir.path().join("out.png")).unwrap())
            .unwrap();
        // The top-left tile was painted 10/10/10: the export carries the
        // pixels, not a blank canvas.
        assert_eq!(&full.rgba8[..4], &[10, 10, 10, 255]);
    }
}
