//! W13-D: File > Open of the document formats the flat import job cannot
//! say enough about: PDF / PDF-compatible AI (one artboard per page), and
//! EPS, Paint.NET PDN, Sketch, Adobe XD, Figma FIG and WMF / EMF, which open
//! as one picture with a status line that says what was read (for the
//! preview formats: that it is the file's embedded preview).
//!
//! A child module of `editor_open_any` (declared there with `#[path]`), so
//! every open route reaches it through [`Editor::open_resource_file`], which
//! File > Open (the picker), a drag-and-drop, File > Open Recent and the
//! command line all ask first. File > Revert of a multi-page PDF rebuilds
//! its artboards through [`Editor::open_pages_document`].
//!
//! # Pages as artboards
//!
//! Photopea opens each PDF page as an artboard. Here every page becomes an
//! artboard group ([`layer_model::artboard`]): a white background plate the
//! size of the page and, above it, a "Page <n>" raster layer holding the
//! page as `hayro` rendered it. The artboards are laid out in rows of
//! [`PAGES_PER_ROW`], each placed on a tile boundary with at least
//! [`PAGE_GAP`] pixels between neighbours. A single-page PDF opens as a
//! plain one-layer document, as any picture does.

use std::io::Read;
use std::path::Path;

use compositor::MemoryTileSource;
use editor_core::pixels::{PixelTarget, TileEdit};
use editor_core::{Command, Document, History};
use layer_model::{Artboard, Layer, LayerId, LayerKind, RasterLayer};
use raster::codec::formats::{pdf, vector_docs};
use raster::{DecodedSurface, ImportFormat, ImportLimits, TileCoord, TileGrid, TILE_SIZE};

use super::super::{Action, ActionError, DocumentId, Editor, Effect, OpenDocument};
use crate::import::{DecodedImage, ImportedDocument, PsdImport, PsdNotes};

/// Pixels (at least) between neighbouring page artboards.
pub const PAGE_GAP: u32 = 64;
/// Artboards per row before the layout wraps.
pub const PAGES_PER_ROW: usize = 10;

/// Which W13-D format `path` holds, by content first and, for the ZIP
/// containers that have no signature of their own, by extension. `None`
/// for anything else (it opens the ordinary way).
pub fn w13d_format(path: &Path) -> Option<ImportFormat> {
    let mut head = Vec::with_capacity(1024);
    std::fs::File::open(path)
        .ok()?
        .take(1024)
        .read_to_end(&mut head)
        .ok()?;
    let ours = |f: ImportFormat| {
        matches!(
            f,
            ImportFormat::Pdf
                | ImportFormat::Eps
                | ImportFormat::Pdn
                | ImportFormat::Sketch
                | ImportFormat::Xd
                | ImportFormat::Fig
                | ImportFormat::Wmf
                | ImportFormat::Emf
        )
    };
    let sniff = &head[..head.len().min(64)];
    if let Some(format) = raster::codec::formats::sniff(sniff) {
        return ours(format).then_some(format);
    }
    if pdf::looks_like_pdf(&head) {
        return Some(ImportFormat::Pdf);
    }
    let by_name = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(ImportFormat::from_extension)?;
    (matches!(
        by_name,
        ImportFormat::Sketch | ImportFormat::Xd | ImportFormat::Fig
    ) && head.starts_with(b"PK"))
    .then_some(by_name)
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

fn round_up(v: u32) -> u32 {
    v.div_ceil(TILE_SIZE).saturating_mul(TILE_SIZE)
}

/// Where each page's artboard goes, and the canvas that holds them all.
pub fn page_layout(sizes: &[(u32, u32)]) -> (Vec<(u32, u32)>, (u32, u32)) {
    let mut origins = Vec::with_capacity(sizes.len());
    let (mut canvas_w, mut canvas_h) = (0u32, 0u32);
    let mut y = 0u32;
    for row in sizes.chunks(PAGES_PER_ROW) {
        let mut x = 0u32;
        let mut row_h = 0u32;
        for &(w, h) in row {
            origins.push((x, y));
            canvas_w = canvas_w.max(x.saturating_add(w));
            row_h = row_h.max(h);
            x = round_up(x.saturating_add(w).saturating_add(PAGE_GAP));
        }
        canvas_h = y.saturating_add(row_h);
        y = round_up(canvas_h.saturating_add(PAGE_GAP));
    }
    (origins, (canvas_w, canvas_h))
}

/// `rgba` (`w` x `h`) painted into `layer` with its top-left at the tile
/// `(dx, dy)`.
fn paint_at(
    layer: LayerId,
    (w, h): (u32, u32),
    rgba: &[u8],
    (dx, dy): (i32, i32),
    tiles: &mut MemoryTileSource,
) -> Result<Command, String> {
    let grid = TileGrid::from_rgba8(w, h, rgba).map_err(|e| e.to_string())?;
    let edits: Vec<TileEdit> = grid
        .iter()
        .map(|(c, tile)| {
            let hash = tiles.insert_bytes(tile.data().to_vec());
            TileEdit::set(TileCoord::new(c.x + dx, c.y + dy, c.level), hash)
        })
        .collect();
    Command::paint_tiles(PixelTarget::Layer(layer), edits).map_err(|e| e.to_string())
}

/// A document with one artboard per page (see the module docs). The first
/// page's layer is active; opening is not an edit, so the history is empty
/// and the document is clean.
pub fn document_from_pages(
    pages: &[DecodedSurface],
    title: &str,
    history_depth: usize,
) -> Result<ImportedDocument, String> {
    if pages.is_empty() {
        return Err("the PDF has no pages".into());
    }
    let sizes: Vec<(u32, u32)> = pages.iter().map(|p| (p.width, p.height)).collect();
    let (origins, (canvas_w, canvas_h)) = page_layout(&sizes);
    let limits = ImportLimits::default();
    if canvas_w > limits.max_width
        || canvas_h > limits.max_height
        || u64::from(canvas_w) * u64::from(canvas_h) > limits.max_pixels
    {
        return Err(format!(
            "the pages laid out as artboards need a {canvas_w}x{canvas_h} canvas, past the import limit"
        ));
    }
    let mut document = Document::new(canvas_w, canvas_h, title);
    let mut history = History::with_limit(history_depth);
    let mut tiles = MemoryTileSource::new();
    let mut first = None;
    // Created last page first, so page 1 is the top of the Layers panel.
    for (index, (page, &(x, y))) in pages.iter().zip(&origins).enumerate().rev() {
        let name = format!("Page {}", index + 1);
        let tile = ((x / TILE_SIZE) as i32, (y / TILE_SIZE) as i32);
        let board = Artboard {
            x: i64::from(x),
            y: i64::from(y),
            width: page.width,
            height: page.height,
            background: [1.0, 1.0, 1.0, 1.0],
        };
        let group = Layer::group(&name);
        let plate = Layer::with_kind(
            "Artboard Background",
            LayerKind::Raster(RasterLayer {
                artboard: Some(board),
                ..RasterLayer::default()
            }),
        );
        let content = Layer::raster(&name);
        let (group_id, plate_id, content_id) = (group.id, plate.id, content.id);
        let white = vec![255u8; page.width as usize * page.height as usize * 4];
        let rgba = page.pixels.clone().into_rgba8();
        let commands = vec![
            Command::create_layer(group),
            Command::create_layer(plate),
            paint_at(
                plate_id,
                (page.width, page.height),
                &white,
                tile,
                &mut tiles,
            )?,
            Command::MoveLayer {
                layer_id: plate_id,
                parent: Some(group_id),
                index: 0,
            },
            Command::create_layer(content),
            paint_at(
                content_id,
                (page.width, page.height),
                &rgba,
                tile,
                &mut tiles,
            )?,
            Command::MoveLayer {
                layer_id: content_id,
                parent: Some(group_id),
                index: 0,
            },
        ];
        history
            .apply(
                &mut document,
                Command::Transaction {
                    label: format!("Open {name}"),
                    commands,
                },
            )
            .map_err(|e| e.to_string())?;
        first = Some(content_id);
    }
    let layer = first.ok_or("the PDF has no pages")?;
    document
        .set_active_layer(Some(layer))
        .map_err(|e| e.to_string())?;
    document.mark_saved();
    history.clear();
    Ok(ImportedDocument {
        document,
        history,
        tiles,
        layer,
    })
}

impl Editor {
    /// W13-D: a PDF of two or more pages as its artboard document; `None`
    /// for anything else (a one-page PDF included: it opens as a picture).
    pub(crate) fn open_pages_document(
        id: DocumentId,
        path: &Path,
        history_depth: usize,
    ) -> Result<Option<(OpenDocument, usize, usize)>, String> {
        // W16-I: an SVG, EPS or one-page PDF / AI that reads as layers comes
        // back (File > Revert) as its layers.
        if let Some(doc) = Self::open_vector_layered(id, path, history_depth) {
            return Ok(Some((doc, 1, 1)));
        }
        if w13d_format(path) != Some(ImportFormat::Pdf) {
            return Ok(None);
        }
        let limits = ImportLimits::default();
        let bytes = read_limited(path, limits)?;
        if pdf::page_count(&bytes).map_err(|e| e.to_string())? < 2 {
            return Ok(None);
        }
        let rendered = pdf::render_pages(&bytes, limits).map_err(|e| e.to_string())?;
        let title = DecodedImage::title_for(path);
        let imported = document_from_pages(&rendered.pages, &title, history_depth)?;
        let doc = OpenDocument::open_psd_import(
            id,
            path,
            PsdImport {
                imported,
                notes: PsdNotes::default(),
                merged_preview: None,
            },
        );
        Ok(Some((doc, rendered.pages.len(), rendered.total)))
    }

    /// W13-D: open `path` when it is one of this wave's document formats
    /// (see the module docs); `None` otherwise.
    pub(crate) fn open_w13d_document(
        &mut self,
        path: &Path,
    ) -> Option<Result<Effect, ActionError>> {
        // W16-I: an SVG, EPS or one-page PDF / AI opens as its layers; when
        // they cannot be read, an EPS / PDF opens below as one picture and
        // the status line says why.
        let no_layers = match self.open_vector_document(path) {
            vector_w16::VectorOpen::Done(result) => return Some(result),
            vector_w16::VectorOpen::NoLayers(why) => Some(why),
            vector_w16::VectorOpen::NotOurs => None,
        };
        let format = w13d_format(path)?;
        let failed =
            |e: String| ActionError::failed(Action::Open, format!("{}: {e}", path.display()));
        let depth = self.prefs.history_depth;
        let id = self.mint_id();
        let limits = ImportLimits::default();
        let outcome = (|| -> Result<(OpenDocument, String), String> {
            if format == ImportFormat::Pdf {
                if let Some((doc, shown, total)) = Self::open_pages_document(id, path, depth)? {
                    let note = if shown < total {
                        format!(
                            "the first {shown} of its {total} pages, one artboard each ({} pages is the most one open renders)",
                            pdf::MAX_PAGES
                        )
                    } else {
                        format!("{shown} pages, one artboard each")
                    };
                    return Ok((doc, note));
                }
            }
            let bytes = read_limited(path, limits)?;
            let (surface, note) = match format {
                ImportFormat::Pdf => (
                    pdf::render_page(&bytes, 0, limits).map_err(|e| e.to_string())?,
                    "its page, rendered at one pixel per point".to_string(),
                ),
                ImportFormat::Wmf | ImportFormat::Emf => (
                    raster::decode_surface_bytes_as(&bytes, limits, format)
                        .map_err(|e| e.to_string())?,
                    format!(
                        "its common {} drawing records (arcs, clipping, dash styles and EMF+ are not drawn)",
                        format.name()
                    ),
                ),
                _ => vector_docs::decode_described(format, &bytes, limits)
                    .map_err(|e| e.to_string())?,
            };
            let doc = OpenDocument::open_image_decoded(id, path, image_of(surface), depth)
                .map_err(|e| e.to_string())?;
            Ok((doc, note))
        })();
        Some(match outcome {
            Ok((doc, note)) => {
                self.install_opened(doc, path);
                let note = match no_layers {
                    Some(why) => format!("{note}; its layers could not be read ({why})"),
                    None => note,
                };
                self.status = Some(format!("Opened {}: {note}", path.display()));
                self.touch();
                Ok(Effect::DocumentSet)
            }
            Err(e) => Err(failed(e)),
        })
    }
}

#[cfg(test)]
#[path = "editor_open_pages_tests.rs"]
mod tests;

/// W16-I: SVG, EPS and one-page PDF / AI opened as layers.
#[path = "import_vector_w16.rs"]
pub(crate) mod vector_w16;
