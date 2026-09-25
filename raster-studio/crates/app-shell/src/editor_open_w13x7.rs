//! W13X-7: File > Open of a multi-page PDF asks first, and a Paint.NET file
//! opens its layers.
//!
//! A child module of `editor_open_any` (declared there with `#[path]`), so
//! every open route — File > Open (the picker), a drag-and-drop, File > Open
//! Recent and the command line — reaches it through
//! [`Editor::open_resource_file`], ahead of the W13-D route
//! (`editor_open_pages`).
//!
//! # PDF / AI: the import dialog
//!
//! A PDF (or a PDF-compatible `.ai`) of two or more pages opens nothing
//! straight away: its pages are listed with a thumbnail each and the request
//! is parked for the chrome, whose dialog host takes it on its next frame
//! (`DialogHost::refresh_preview`) and shows [`ui::dialogs::pdf_import`]:
//! which pages, what resolution, artboards in one document or separate
//! documents, as Photopea asks. Confirming parks the answer here with the
//! file's path and asks the shell to open that path again (the
//! `ChromeOutput::open_recent` road, which the shell applies through
//! [`Editor::open_paths`]); that open finds the answer and renders exactly
//! the chosen pages at the chosen resolution. Cancel opens nothing. W16-K:
//! a one-page `.pdf` asks too (its resolution and how it opens), as
//! Photopea's does; a one-page Illustrator `.ai` still opens as a picture
//! straight away (Photopea reads `.ai` with its own reader, not this
//! dialog).
//!
//! Several multi-page files opened at once (a drop of two PDFs, two on the
//! command line) queue: each asks in turn, in the order they were opened,
//! the next as soon as the one before it is answered. The same file asked
//! for twice before it is answered asks once.
//!
//! What was chosen is remembered per document, so File > Revert reads the
//! same pages at the same resolution back ([`reopen_for_revert`]).
//!
//! # Paint.NET: layers
//!
//! A `.pdn` whose object graph [`pdn::read_layers`] can follow opens as one
//! raster layer per Paint.NET layer — name, opacity, visibility, blend
//! mode — bottom to top as Paint.NET stacks them. A blend mode with no
//! equivalent here (Reflect, Glow, Negation, Xor) opens as Normal and the
//! status line says so. When the layers cannot be read, the W13-D route
//! opens the file's flattened thumbnail, and the status line says why the
//! layers were not read.
//!
//! File > Revert of a `.pdn` reads its layers back the same way
//! ([`reopen_for_revert`]). A document that opened as layers and whose file
//! no longer yields them is not reverted to the thumbnail: Revert refuses
//! and says why, and the layers stay as they are.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};

use compositor::MemoryTileSource;
use editor_core::pixels::{PixelKey, TileDelta, TileEdit};
use editor_core::{Document, History};
use layer_model::{BlendMode, Layer, LayerId};
use raster::codec::formats::pdf;
use raster::codec::formats::vector_docs::pdn::{self, PdnBlend, PdnDocument};
use raster::{DecodedSurface, ImportFormat, ImportLimits, TileGrid};
use ui::dialogs::pdf_import::{PdfImportDialog, PdfImportSpec, PdfOpenMode, PdfPageThumb};

use super::super::{Action, ActionError, DocumentId, Editor, Effect, OpenDocument};
use super::open_pages::{document_from_pages, w13d_format};
use crate::import::{DecodedImage, ImportedDocument, PsdImport, PsdNotes};

/// The longest side of a page thumbnail in the import dialog, in pixels.
pub const THUMB_PX: u32 = 96;

/// A PDF waiting for its import dialog: the file and the dialog's state.
#[derive(Debug)]
pub struct PdfImportRequest {
    pub path: PathBuf,
    pub dialog: PdfImportDialog,
}

impl PdfImportRequest {
    /// Draw one frame. `true` when the dialog is done (confirmed or
    /// cancelled); a confirmation is parked for the open route and the path
    /// rides `out.open_recent` to the shell, which opens it again.
    pub fn drive(&mut self, ctx: &egui::Context, out: &mut crate::chrome::ChromeOutput) -> bool {
        match self.dialog.show(ctx) {
            ui::dialogs::DialogOutcome::Open => false,
            ui::dialogs::DialogOutcome::Cancelled => true,
            ui::dialogs::DialogOutcome::Confirmed(spec) => {
                park_confirmed(self.path.clone(), spec);
                out.open_recent = Some(self.path.clone());
                true
            }
        }
    }
}

thread_local! {
    /// The import dialogs multi-page opens asked for, oldest first, waiting
    /// for the chrome's dialog host (which shows one at a time).
    static PENDING: RefCell<VecDeque<PdfImportRequest>> = const { RefCell::new(VecDeque::new()) };
    /// The answers the dialogs gave, each waiting for the open of its path.
    static CONFIRMED: RefCell<Vec<(PathBuf, PdfImportSpec)>> = const { RefCell::new(Vec::new()) };
    /// The pages, resolution and mode each document opened from a PDF
    /// with, for File > Revert.
    static OPENED_WITH: RefCell<HashMap<DocumentId, PdfImportSpec>> =
        RefCell::new(HashMap::new());
    /// The documents that opened as a `.pdn`'s layers, for File > Revert.
    static PDN_LAYERED: RefCell<HashSet<DocumentId>> = RefCell::new(HashSet::new());
}

/// The oldest import dialog waiting to be shown, taken (each at most once).
pub(crate) fn take_pending() -> Option<PdfImportRequest> {
    PENDING.with(|p| p.borrow_mut().pop_front())
}

/// Queue `request`; one already waiting for the same file is replaced in
/// its place, so a file asks once.
fn queue_pending(request: PdfImportRequest) {
    PENDING.with(|p| {
        let mut queue = p.borrow_mut();
        match queue.iter_mut().find(|r| r.path == request.path) {
            Some(slot) => *slot = request,
            None => queue.push_back(request),
        }
    });
}

/// Tests of the older open routes: answer the parked dialog with its
/// opening state (every page, 72 dpi, artboards) and open the file, as a
/// press of Enter and the shell's `open_recent` apply would.
#[cfg(test)]
pub(crate) fn accept_import_defaults_for_test(editor: &mut Editor) {
    let request = take_pending().expect("the PDF import dialog was asked for");
    let spec = request
        .dialog
        .confirm()
        .expect("the opening state confirms");
    park_confirmed(request.path.clone(), spec);
    editor.open_paths(&[request.path]);
}

fn park_confirmed(path: PathBuf, spec: PdfImportSpec) {
    CONFIRMED.with(|c| {
        let mut parked = c.borrow_mut();
        parked.retain(|(p, _)| *p != path);
        parked.push((path, spec));
    });
}

fn take_confirmed(path: &Path) -> Option<PdfImportSpec> {
    CONFIRMED.with(|c| {
        let mut parked = c.borrow_mut();
        let at = parked.iter().position(|(p, _)| p == path)?;
        Some(parked.remove(at).1)
    })
}

fn read_limited(path: &Path, limits: ImportLimits) -> Result<Vec<u8>, String> {
    let cap = limits.max_alloc_bytes.saturating_mul(4);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > cap {
        return Err(format!("the file is larger than {cap} bytes"));
    }
    Ok(bytes)
}

fn image_of(surface: DecodedSurface) -> DecodedImage {
    DecodedImage {
        width: surface.width,
        height: surface.height,
        color_space: surface.color_space,
        icc_profile: surface.icc_profile,
        rgba8: surface.pixels.into_rgba8(),
    }
}

/// "1, 3 and 4".
fn page_list(pages: &[usize]) -> String {
    let names: Vec<String> = pages.iter().map(|p| (p + 1).to_string()).collect();
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        Some((only, _)) => only.clone(),
        None => String::new(),
    }
}

/// The artboard document of `pages` (rendered, in `indices` order), its
/// "Page <n>" layers named by their page numbers in the file.
fn artboards_of(
    pages: &[DecodedSurface],
    indices: &[usize],
    title: &str,
    depth: usize,
) -> Result<ImportedDocument, String> {
    let mut imported = document_from_pages(pages, title, depth)?;
    let tree = &mut imported.document.layers;
    for id in tree.iter_depth_first() {
        if let Some(layer) = tree.get_mut(id) {
            let number = layer
                .name
                .strip_prefix("Page ")
                .and_then(|n| n.parse::<usize>().ok());
            if let Some(page) = number.and_then(|n| indices.get(n.wrapping_sub(1))) {
                layer.name = format!("Page {}", page + 1);
            }
        }
    }
    imported.document.mark_saved();
    Ok(imported)
}

/// The document(s) a confirmed import opens, each with the pages it holds.
fn documents_for(
    editor: &mut Editor,
    path: &Path,
    spec: &PdfImportSpec,
) -> Result<Vec<(OpenDocument, Vec<usize>)>, String> {
    let limits = ImportLimits::default();
    let bytes = read_limited(path, limits)?;
    let rendered =
        pdf::render_selected(&bytes, &spec.pages, spec.dpi, limits).map_err(|e| e.to_string())?;
    let depth = editor.prefs.history_depth;
    let title = DecodedImage::title_for(path);
    match spec.mode {
        PdfOpenMode::Artboards => {
            let imported = artboards_of(&rendered, &spec.pages, &title, depth)?;
            let doc = OpenDocument::open_psd_import(
                editor.mint_id(),
                path,
                PsdImport {
                    imported,
                    notes: PsdNotes::default(),
                    merged_preview: None,
                },
            );
            Ok(vec![(doc, spec.pages.clone())])
        }
        PdfOpenMode::SeparateDocuments => {
            let mut out = Vec::with_capacity(rendered.len());
            for (surface, &page) in rendered.into_iter().zip(&spec.pages) {
                let mut doc = OpenDocument::open_image_decoded(
                    editor.mint_id(),
                    path,
                    image_of(surface),
                    depth,
                )
                .map_err(|e| e.to_string())?;
                doc.document.meta.title = format!("{title} - Page {}", page + 1);
                doc.document.mark_saved();
                out.push((doc, vec![page]));
            }
            Ok(out)
        }
    }
}

/// W13X-7: File > Revert of a document opened through the import dialog
/// (the same pages at the same resolution), or of a Paint.NET file (its
/// layers; see [`reopen_pdn_for_revert`]). `None` for any other document.
pub(crate) fn reopen_for_revert(
    id: DocumentId,
    path: &Path,
    depth: usize,
) -> Option<Result<OpenDocument, String>> {
    if w13d_format(path) == Some(ImportFormat::Pdn) {
        return reopen_pdn_for_revert(id, path, depth);
    }
    let spec = OPENED_WITH.with(|m| m.borrow().get(&id).cloned())?;
    let limits = ImportLimits::default();
    let result = (|| {
        let bytes = read_limited(path, limits)?;
        let rendered = pdf::render_selected(&bytes, &spec.pages, spec.dpi, limits)
            .map_err(|e| e.to_string())?;
        let title = DecodedImage::title_for(path);
        if spec.mode == PdfOpenMode::SeparateDocuments {
            let surface = rendered.into_iter().next().ok_or("no page was rendered")?;
            let mut doc = OpenDocument::open_image_decoded(id, path, image_of(surface), depth)
                .map_err(|e| e.to_string())?;
            doc.document.meta.title = format!("{title} - Page {}", spec.pages[0] + 1);
            return Ok(doc);
        }
        let imported = artboards_of(&rendered, &spec.pages, &title, depth)?;
        Ok(OpenDocument::open_psd_import(
            id,
            path,
            PsdImport {
                imported,
                notes: PsdNotes::default(),
                merged_preview: None,
            },
        ))
    })();
    Some(result)
}

// -------------------------------------------------------------------- PDN

/// A `.pdn`'s layers as the document `id`, or why they cannot be read.
fn pdn_document(
    id: DocumentId,
    path: &Path,
    depth: usize,
) -> Result<(OpenDocument, usize, Vec<String>), String> {
    let limits = ImportLimits::default();
    let bytes = read_limited(path, limits)?;
    let parsed = pdn::read_layers(&bytes, limits).map_err(|e| e.to_string())?;
    let title = DecodedImage::title_for(path);
    let (imported, notes) = document_from_pdn(&parsed, &title, depth)?;
    let doc = OpenDocument::open_psd_import(
        id,
        path,
        PsdImport {
            imported,
            notes: PsdNotes::default(),
            merged_preview: None,
        },
    );
    Ok((doc, parsed.layers.len(), notes))
}

/// File > Revert of a `.pdn`: its layers when they can be read. When they
/// cannot, a document that opened as layers is an error (Revert refuses
/// rather than flatten it to the thumbnail); one that opened as its
/// thumbnail is `None`, and reverts to the thumbnail as before.
fn reopen_pdn_for_revert(
    id: DocumentId,
    path: &Path,
    depth: usize,
) -> Option<Result<OpenDocument, String>> {
    match pdn_document(id, path, depth) {
        Ok((doc, _, _)) => Some(Ok(doc)),
        Err(why) if PDN_LAYERED.with(|s| s.borrow().contains(&id)) => Some(Err(format!(
            "it opened as layers and its layers can no longer be read ({why}); nothing was reverted"
        ))),
        Err(_) => None,
    }
}

/// The document model's blend mode for a Paint.NET one, or `None` when
/// there is no equivalent.
fn blend_mode(blend: &PdnBlend) -> Option<BlendMode> {
    Some(match blend {
        PdnBlend::Normal => BlendMode::Normal,
        PdnBlend::Multiply => BlendMode::Multiply,
        PdnBlend::Additive => BlendMode::LinearDodge,
        PdnBlend::ColorBurn => BlendMode::ColorBurn,
        PdnBlend::ColorDodge => BlendMode::ColorDodge,
        PdnBlend::Overlay => BlendMode::Overlay,
        PdnBlend::Difference => BlendMode::Difference,
        PdnBlend::Lighten => BlendMode::Lighten,
        PdnBlend::Darken => BlendMode::Darken,
        PdnBlend::Screen => BlendMode::Screen,
        PdnBlend::Reflect
        | PdnBlend::Glow
        | PdnBlend::Negation
        | PdnBlend::Xor
        | PdnBlend::Other(_) => return None,
    })
}

/// A `.pdn`'s layers as a document: one raster layer per Paint.NET layer,
/// the last one on top and active; and a note per blend mode that opened as
/// Normal.
pub fn document_from_pdn(
    pdn: &PdnDocument,
    title: &str,
    history_depth: usize,
) -> Result<(ImportedDocument, Vec<String>), String> {
    let (width, height) = (pdn.width, pdn.height);
    if !editor_core::canvas_size_is_supported(width, height) {
        return Err(format!(
            "a {width}x{height} canvas is not one this build can open"
        ));
    }
    let mut document = Document::new(width, height, title);
    let mut tiles = MemoryTileSource::new();
    let mut notes = Vec::new();
    let mut top: Option<LayerId> = None;
    for source in &pdn.layers {
        let mut layer = Layer::raster(source.name.clone());
        layer.visible = source.visible;
        layer.opacity = f32::from(source.opacity) / 255.0;
        layer.blend_mode = blend_mode(&source.blend).unwrap_or_else(|| {
            notes.push(format!(
                "layer {:?} uses Paint.NET's {} blend mode, which has no equivalent here; it \
                 opened as Normal",
                source.name,
                source.blend.stem()
            ));
            BlendMode::Normal
        });
        // Paint.NET lists its layers bottom first: each goes on top.
        let id = document
            .layers
            .insert_at(layer, None, 0)
            .map_err(|e| e.to_string())?;
        let grid = TileGrid::from_rgba8(width, height, &source.rgba).map_err(|e| e.to_string())?;
        let edits: Vec<TileEdit> = grid
            .iter()
            .filter(|(_, tile)| tile.data().iter().any(|b| *b != 0))
            .map(|(coord, tile)| TileEdit::set(coord, tiles.insert_bytes(tile.data().to_vec())))
            .collect();
        if !edits.is_empty() {
            let delta = TileDelta::new(edits).map_err(|e| e.to_string())?;
            document.pixels.apply(PixelKey::Layer(id), &delta);
        }
        top = Some(id);
    }
    let layer = top.ok_or("the file has no layers")?;
    document
        .set_active_layer(Some(layer))
        .map_err(|e| e.to_string())?;
    document.mark_saved();
    Ok((
        ImportedDocument {
            document,
            history: History::with_limit(history_depth),
            tiles,
            layer,
        },
        notes,
    ))
}

impl Editor {
    /// W13X-7: the PDF import dialog and the Paint.NET layers (see the
    /// module docs); `None` for anything else, which the W13-D route and
    /// the ordinary open take.
    pub(crate) fn open_w13x7_document(
        &mut self,
        path: &Path,
    ) -> Option<Result<Effect, ActionError>> {
        match w13d_format(path)? {
            ImportFormat::Pdf => self.open_pdf_with_dialog(path),
            ImportFormat::Pdn => self.open_pdn_layers(path),
            _ => None,
        }
    }

    fn open_pdf_with_dialog(&mut self, path: &Path) -> Option<Result<Effect, ActionError>> {
        let failed =
            |e: String| ActionError::failed(Action::Open, format!("{}: {e}", path.display()));
        if let Some(spec) = take_confirmed(path) {
            return Some(self.open_pdf_pages(path, &spec).map_err(failed));
        }
        let limits = ImportLimits::default();
        // A file that cannot be read or counted is left to the W13-D route,
        // which reports why.
        let bytes = read_limited(path, limits).ok()?;
        // W16-K: every page count asks but none (left to the W13-D route,
        // which says why), and a one-page `.ai` (see the module docs).
        let pages = pdf::page_count(&bytes).ok()?;
        let ai = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ai"));
        if pages == 0 || (pages < 2 && ai) {
            return None;
        }
        let previews = match pdf::page_previews(&bytes, THUMB_PX, limits) {
            Ok(p) => p,
            Err(e) => return Some(Err(failed(e.to_string()))),
        };
        let thumbs = previews
            .pages
            .into_iter()
            .map(|p| PdfPageThumb {
                width_pt: p.width_pt,
                height_pt: p.height_pt,
                width: p.thumbnail.width,
                height: p.thumbnail.height,
                rgba: p.thumbnail.pixels.into_rgba8(),
            })
            .collect();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let request = PdfImportRequest {
            path: path.to_path_buf(),
            dialog: PdfImportDialog::new(name, previews.total, thumbs),
        };
        queue_pending(request);
        self.status = Some(format!(
            "{}: choose the pages to open, their resolution and how they open",
            path.display()
        ));
        self.touch();
        Some(Ok(Effect::Tool))
    }

    /// Open the pages a confirmed import dialog chose.
    fn open_pdf_pages(&mut self, path: &Path, spec: &PdfImportSpec) -> Result<Effect, String> {
        let docs = documents_for(self, path, spec)?;
        let count = docs.len();
        for (doc, pages) in docs {
            let id = doc.id();
            let opened = PdfImportSpec {
                pages,
                ..spec.clone()
            };
            OPENED_WITH.with(|m| m.borrow_mut().insert(id, opened));
            self.install_opened(doc, path);
        }
        let n = spec.pages.len();
        let what = if n == 1 { "page" } else { "pages" };
        let how = match spec.mode {
            PdfOpenMode::Artboards => format!("{n} {what}, one artboard each"),
            PdfOpenMode::SeparateDocuments => format!("{count} documents, one per page"),
        };
        self.status = Some(format!(
            "Opened {}: {how} ({what} {}) at {} dpi",
            path.display(),
            page_list(&spec.pages),
            spec.dpi
        ));
        self.touch();
        Ok(Effect::DocumentSet)
    }

    fn open_pdn_layers(&mut self, path: &Path) -> Option<Result<Effect, ActionError>> {
        // A file that cannot be read at all is left to the W13-D route,
        // which reports why.
        read_limited(path, ImportLimits::default()).ok()?;
        let id = self.mint_id();
        let why = match pdn_document(id, path, self.prefs.history_depth) {
            Ok((doc, count, notes)) => {
                PDN_LAYERED.with(|s| s.borrow_mut().insert(id));
                self.install_opened(doc, path);
                let mut status = format!("Opened {}: its {count} Paint.NET layers", path.display());
                for note in notes {
                    status.push_str("; ");
                    status.push_str(&note);
                }
                self.status = Some(status);
                self.touch();
                return Some(Ok(Effect::DocumentSet));
            }
            Err(why) => why,
        };
        // The thumbnail, through the W13-D route, saying why.
        let result = self.open_w13d_document(path)?;
        Some(match result {
            Ok(effect) => {
                let status = self.status.take().unwrap_or_default();
                self.status = Some(format!("{status} (its layers could not be read: {why})"));
                Ok(effect)
            }
            Err(e) => Err(ActionError::failed(
                Action::Open,
                format!("{e} (its layers could not be read either: {why})"),
            )),
        })
    }
}

#[cfg(test)]
#[path = "editor_open_w13x7_tests.rs"]
mod tests;
