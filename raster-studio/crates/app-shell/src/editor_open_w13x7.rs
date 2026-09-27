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
//! a one-page `.pdf` whose page the layer reader cannot keep live (an
//! image, a clip, a shading on it) asks too (its resolution and how it
//! opens); a one-page `.pdf` of paths and text, and a one-page `.ai`, do
//! not: W16-I's route (`import_vector_w16`) opens their layers.
//!
//! Several multi-page files opened at once (a drop of two PDFs, two on the
//! command line) queue: each asks in turn, in the order they were opened,
//! the next as soon as the one before it is answered. The same file asked
//! for twice before it is answered asks once.
//!
//! What was chosen is remembered per document, so File > Revert reads the
//! same pages at the same resolution back ([`reopen_for_revert`]).
//!
//! W18-H: each chosen page opens as **layers**, as Photopea opens a PDF:
//! its paths as shape layers and its text as text layers (the W16-I page
//! reader, `pdf::layers`), inside the page's artboard, scaled to the chosen
//! resolution (a shape or text layer carries the scale in its transform).
//! A page the reader cannot keep live (an image, a clip, a shading on it),
//! or one whose flattened elements would need resampling at a resolution
//! other than 72 dpi, opens as one picture inside its artboard, and the
//! import report says which page and why. "Separate documents" opens each
//! page the same way as a document of its own. File > Revert rebuilds the
//! same layers. W18-H also routes File > Revert of a `.kra` / `.dxf` to its
//! layered reader (`import_layered_w16::reopen_layered_for_revert`).
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
use raster::codec::formats::vector_docs::design_files::{
    Affine, DesignDocument, DesignKind, DesignNode,
};
use raster::codec::formats::vector_docs::pdn::{self, PdnBlend, PdnDocument};
use raster::codec::svg_import::layers::VectorLayers;
use raster::{DecodedSurface, ImportFormat, ImportLimits, TileGrid};
use ui::dialogs::pdf_import::{PdfImportDialog, PdfImportSpec, PdfOpenMode, PdfPageThumb};

use super::super::{Action, ActionError, DocumentId, Editor, Effect, OpenDocument};
use super::open_pages::w13d_format;
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

/// W18-H: `node` and its subtree placed by `outer` (a translation after a
/// uniform `scale`, applied after each node's own transform). A shape or
/// text node whose own transform is a translation takes the scale into its
/// geometry (the path's points, the stroke width, the text size), so the
/// layer draws its edges at the chosen resolution rather than resampling a
/// 72 dpi rendering; a shape with a live gradient (`keep`, by walk index,
/// counted in `index`) keeps the scale in its transform, where the gradient
/// geometry expects it.
fn place(
    node: &mut DesignNode,
    outer: Affine,
    scale: f64,
    index: &mut usize,
    keep: &HashSet<usize>,
) {
    let here = *index;
    *index += 1;
    let bake =
        (scale - 1.0).abs() > 1e-9 && node.transform.is_translation() && !keep.contains(&here);
    let full = outer.then(node.transform);
    match &mut node.kind {
        DesignKind::Shape {
            path_svg, stroke, ..
        } if bake => {
            if let Ok(path) = vector::parse_svg(path_svg) {
                *path_svg = vector::to_svg(&path.transform(&vector::Affine::new(full.0)));
                if let Some(s) = stroke {
                    s.width *= scale as f32;
                }
                node.transform = Affine::IDENTITY;
                node.width *= scale;
                node.height *= scale;
                return;
            }
        }
        DesignKind::Text {
            size, box_width, ..
        } if bake => {
            *size *= scale as f32;
            if let Some(w) = box_width {
                *w *= scale as f32;
            }
            node.transform = Affine::translate(full.0[4], full.0[5]);
            node.width *= scale;
            node.height *= scale;
            return;
        }
        _ => {}
    }
    node.transform = full;
    if let DesignKind::Artboard { children, .. } | DesignKind::Group { children } = &mut node.kind {
        for child in children {
            place(child, outer, scale, index, keep);
        }
    }
}

/// W18-H: whether `nodes` hold pixels (which the layer mapping places but
/// does not resample).
fn holds_pixels(nodes: &[DesignNode]) -> bool {
    nodes
        .iter()
        .any(|n| matches!(n.kind, DesignKind::Bitmap { .. }) || holds_pixels(n.children()))
}

/// W18-H: page `page` of `bytes` as layer nodes placed at `origin` and
/// scaled from points to `dpi`, with their live gradients (keyed in the
/// page's own walk order) and the reader's notes; or why the page does not
/// open as layers.
fn page_as_layers(
    bytes: &[u8],
    page: usize,
    dpi: u32,
    origin: (u32, u32),
    limits: ImportLimits,
) -> Result<VectorLayers, String> {
    let mut layers = pdf::layers::page_layers(bytes, page, limits).map_err(|e| e.to_string())?;
    let scale = f64::from(dpi) / 72.0;
    if (scale - 1.0).abs() > 1e-9 && holds_pixels(&layers.design.nodes) {
        return Err(format!(
            "{} of its elements open as pixels, which are not resampled to {dpi} dpi",
            layers.flattened.max(1)
        ));
    }
    let outer = Affine::translate(f64::from(origin.0), f64::from(origin.1))
        .then(Affine([scale, 0.0, 0.0, scale, 0.0, 0.0]));
    let keep: HashSet<usize> = layers.gradients.iter().map(|(i, _)| *i).collect();
    let mut index = 0;
    for node in &mut layers.design.nodes {
        place(node, outer, scale, &mut index, &keep);
    }
    Ok(layers)
}

/// W18-H: the chosen pages (rendered at the chosen resolution, in `pages`
/// order) as one layer tree: an artboard per page, page 1 on top, holding
/// the page's layers, or its picture when it does not open as layers.
fn layered_artboards(
    bytes: &[u8],
    rendered: &[DecodedSurface],
    pages: &[usize],
    dpi: u32,
    limits: ImportLimits,
) -> Result<VectorLayers, String> {
    if rendered.is_empty() || rendered.len() != pages.len() {
        return Err("the PDF has no pages".into());
    }
    let sizes: Vec<(u32, u32)> = rendered.iter().map(|p| (p.width, p.height)).collect();
    let (origins, (canvas_w, canvas_h)) = super::open_pages::page_layout(&sizes);
    let mut nodes = Vec::with_capacity(pages.len());
    let mut notes = Vec::new();
    let mut gradients = Vec::new();
    let mut flattened = 0usize;
    // The combined walk index of the next artboard.
    let mut at = 0usize;
    // Last page first: the first node is the bottom-most, so page 1 ends on
    // top of the Layers panel.
    for k in (0..pages.len()).rev() {
        let (page, surface, origin) = (pages[k], &rendered[k], origins[k]);
        let name = format!("Page {}", page + 1);
        let children = match page_as_layers(bytes, page, dpi, origin, limits) {
            Ok(layers) => {
                let walked = layers.design.walk().len();
                gradients.extend(
                    layers
                        .gradients
                        .iter()
                        .map(|(i, g)| (at + 1 + i, g.clone())),
                );
                flattened += layers.flattened;
                notes.extend(
                    layers
                        .design
                        .notes
                        .iter()
                        .map(|n| format!("page {}: {n}", page + 1)),
                );
                at += 1 + walked;
                layers.design.nodes
            }
            Err(why) => {
                notes.push(format!("page {} opened as one picture: {why}", page + 1));
                at += 2;
                vec![DesignNode {
                    name: name.clone(),
                    visible: true,
                    opacity: 1.0,
                    transform: Affine::translate(f64::from(origin.0), f64::from(origin.1)),
                    width: f64::from(surface.width),
                    height: f64::from(surface.height),
                    kind: DesignKind::Bitmap {
                        width: surface.width,
                        height: surface.height,
                        rgba: surface.pixels.clone().into_rgba8(),
                    },
                }]
            }
        };
        nodes.push(DesignNode {
            name,
            visible: true,
            opacity: 1.0,
            transform: Affine::translate(f64::from(origin.0), f64::from(origin.1)),
            width: f64::from(surface.width),
            height: f64::from(surface.height),
            kind: DesignKind::Artboard {
                background: Some([1.0, 1.0, 1.0, 1.0]),
                children,
            },
        });
    }
    let what = if pages.len() == 1 { "page" } else { "pages" };
    Ok(VectorLayers {
        design: DesignDocument {
            format: ImportFormat::Pdf,
            nodes,
            notes,
            opened: format!("{what} {}", page_list(pages)),
        },
        width: canvas_w,
        height: canvas_h,
        flattened,
        gradients,
    })
}

/// W18-H: one chosen page as a document of its own, as layers (see the
/// module docs); `Err` with why when it does not open as layers.
fn separate_layers(
    bytes: &[u8],
    surface: &DecodedSurface,
    page: usize,
    dpi: u32,
    limits: ImportLimits,
) -> Result<VectorLayers, String> {
    let mut layers = page_as_layers(bytes, page, dpi, (0, 0), limits)?;
    layers.width = surface.width;
    layers.height = surface.height;
    Ok(layers)
}

/// W18-H: the document(s) an import opens, each with the pages it holds,
/// and the import report's notes.
type BuiltDocuments = (Vec<(OpenDocument, Vec<usize>)>, Vec<String>);

/// A confirmed import's document(s) as `id_for` names them, each with the
/// pages it holds, and the import report's notes (W18-H: see the module
/// docs).
fn build_documents(
    path: &Path,
    spec: &PdfImportSpec,
    depth: usize,
    mut id_for: impl FnMut() -> DocumentId,
) -> Result<BuiltDocuments, String> {
    let limits = ImportLimits::default();
    let bytes = read_limited(path, limits)?;
    let rendered =
        pdf::render_selected(&bytes, &spec.pages, spec.dpi, limits).map_err(|e| e.to_string())?;
    let title = DecodedImage::title_for(path);
    let as_doc = |id: DocumentId, imported: ImportedDocument| {
        OpenDocument::open_psd_import(
            id,
            path,
            PsdImport {
                imported,
                notes: PsdNotes::default(),
                merged_preview: None,
            },
        )
    };
    match spec.mode {
        PdfOpenMode::Artboards => {
            let layers = layered_artboards(&bytes, &rendered, &spec.pages, spec.dpi, limits)?;
            let mut import =
                super::open_pages::vector_w16::document_from_vector(&layers, &title, depth)?;
            // The layer mapping makes the top-most leaf active; the first
            // page's artboard is the one a page import selects.
            if let Some(&board) = import.imported.document.layers.root().first() {
                let _ = import.imported.document.set_active_layer(Some(board));
                import.imported.layer = board;
            }
            import.imported.document.mark_saved();
            let doc = as_doc(id_for(), import.imported);
            Ok((vec![(doc, spec.pages.clone())], import.notes))
        }
        PdfOpenMode::SeparateDocuments => {
            let mut out = Vec::with_capacity(rendered.len());
            let mut notes = Vec::new();
            for (surface, &page) in rendered.iter().zip(&spec.pages) {
                let name = format!("{title} - Page {}", page + 1);
                let layered =
                    separate_layers(&bytes, surface, page, spec.dpi, limits).and_then(|l| {
                        super::open_pages::vector_w16::document_from_vector(&l, &name, depth)
                    });
                let mut doc = match layered {
                    Ok(import) => {
                        notes.extend(
                            import
                                .notes
                                .iter()
                                .map(|n| format!("page {}: {n}", page + 1)),
                        );
                        as_doc(id_for(), import.imported)
                    }
                    Err(why) => {
                        notes.push(format!("page {} opened as one picture: {why}", page + 1));
                        OpenDocument::open_image_decoded(
                            id_for(),
                            path,
                            image_of(surface.clone()),
                            depth,
                        )
                        .map_err(|e| e.to_string())?
                    }
                };
                doc.document.meta.title = name;
                doc.document.mark_saved();
                out.push((doc, vec![page]));
            }
            Ok((out, notes))
        }
    }
}

/// The document(s) a confirmed import opens, each with the pages it holds,
/// and the import report's notes.
fn documents_for(
    editor: &mut Editor,
    path: &Path,
    spec: &PdfImportSpec,
) -> Result<BuiltDocuments, String> {
    let depth = editor.prefs.history_depth;
    build_documents(path, spec, depth, || editor.mint_id())
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
    // W18-H: a `.kra` / `.dxf` comes back as its layers.
    if let Some(result) =
        crate::editor::resource_import::w16::layered::reopen_layered_for_revert(id, path, depth)
    {
        return Some(result);
    }
    let spec = OPENED_WITH.with(|m| m.borrow().get(&id).cloned())?;
    // W18-H: the same layers the open built (see the module docs).
    let result = build_documents(path, &spec, depth, || id).and_then(|(docs, _)| {
        docs.into_iter()
            .next()
            .map(|(doc, _)| doc)
            .ok_or_else(|| "no page was rendered".to_string())
    });
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
        // which says why), a one-page `.ai`, and a one-page `.pdf` whose
        // page reads as live layers (W16-I's route opens those; see the
        // module docs).
        let pages = pdf::page_count(&bytes).ok()?;
        let ai = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ai"));
        if pages == 0 || (pages < 2 && (ai || pdf::layers::page_layers(&bytes, 0, limits).is_ok()))
        {
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
        let (docs, notes) = documents_for(self, path, spec)?;
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
        let mut status = format!(
            "Opened {}: {how} ({what} {}) at {} dpi",
            path.display(),
            page_list(&spec.pages),
            spec.dpi
        );
        // W18-H: what did not open as layers, in the import report.
        if let Some(text) = super::import_design::report(ImportFormat::Pdf, &notes, path) {
            status.push_str(&format!(
                ", as layers ({} not mapped exactly; see the import report)",
                if notes.len() == 1 {
                    "1 thing".to_string()
                } else {
                    format!("{} things", notes.len())
                }
            ));
            self.dialogs.report_notice(
                &format!("{} import report", ImportFormat::Pdf.name()),
                &text,
            );
        } else {
            status.push_str(", as layers");
        }
        self.status = Some(status);
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
