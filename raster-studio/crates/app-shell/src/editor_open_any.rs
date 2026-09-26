//! W11-D: one routing table for every way a file reaches the editor, Paste
//! with no document open, and File > Revert.
//!
//! A child module of [`crate::editor`] (declared there with `#[path]`, like
//! `resource_import`), so it extends [`Editor`] without widening its surface
//! elsewhere.
//!
//! # One route
//!
//! File > Open (the picker), a drag-and-drop, File > Open Recent and the
//! files named on the command line all ask [`Editor::open_resource_file`]
//! first. A file that feeds a library rather than opening as a picture —
//! brushes (`.abr`), styles (`.asl`), patterns / gradients / custom shapes /
//! swatches / a profile (`.pat` `.grd` `.csh` `.aco` `.ase` `.icc` `.icm`),
//! a font (`.ttf` `.otf` `.ttc` `.otc`) or a 3D LUT (`.cube`) — lands in its
//! importer there, with a status line saying what landed; anything else
//! opens as a document. [`Editor::open_any`] is that decision for the
//! synchronous routes; the picker keeps its off-thread image import for the
//! document half.
//!
//! # Revert
//!
//! File > Revert reads the document's file again and puts what it holds in
//! place of the current layers, canvas size, selection, guides and document
//! records as **one history step** labelled "Revert", as Photoshop's Revert
//! is: Undo takes it back. Because it can be undone it asks nothing first.

use std::path::{Path, PathBuf};

use editor_core::pixels::{PixelKey, PixelTarget, TileEdit};
use editor_core::{Command, LayerPatch};
use layer_model::{AdjustmentKind, AdjustmentLayer, Layer, LayerKind, LockState};

use super::{Action, ActionError, DocumentId, Editor, Effect, OpenDocument};
use compositor::TileSource;

/// The history label File > Revert's step carries.
pub const REVERT_LABEL: &str = "Revert";

/// W11-D: whether `path` names a 3D LUT that dropping or opening turns into
/// a Color Lookup adjustment layer.
pub fn is_cube_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cube"))
}

impl Editor {
    /// W11-D: whether `path` is a file that feeds a library (or, for a
    /// `.cube`, becomes an adjustment layer) instead of opening as a
    /// document — the files [`Self::open_resource_file`] routes.
    pub fn is_library_file(path: &Path) -> bool {
        crate::dialogs::is_font_path(path)
            || Self::is_abr_path(path)
            || crate::dialogs::is_style_library_path(path)
            || is_cube_path(path)
            || Self::is_resource_path(path)
            // W13-K: a script opens in the File > Script window.
            || crate::script::is_script_path(path)
            // W16-M: a video opens as a document holding a video layer.
            || crate::timeline::video_layers::is_video_path(path)
    }

    /// W11-D: route a library file to its importer. `None` when `path` is
    /// not one (it opens as a document); otherwise the importer's outcome,
    /// with the status line already set by the importer.
    pub fn open_resource_file(&mut self, path: &Path) -> Option<Result<Effect, ActionError>> {
        let failed = |e: String| ActionError::failed(Action::Open, e);
        // W16-M: a video file opens as a video layer (Photopea: File >
        // Open of an MP4), ahead of the image decode that refuses it.
        if let Some(result) = self.open_video_file(path) {
            return Some(result);
        }
        // W9-K: a font file's faces load for the session and the Type
        // tool's Font list offers its family.
        if crate::dialogs::is_font_path(path) {
            return Some(
                self.load_font_file(path)
                    .map(|_| Effect::Tool)
                    .map_err(failed),
            );
        }
        // W9-E: a brush file adds brushes to the Brushes panel.
        if Self::is_abr_path(path) {
            return Some(self.import_abr(path).map(|_| Effect::Tool).map_err(failed));
        }
        // W9-H: a style library adds its styles to the style presets.
        if crate::dialogs::is_style_library_path(path) {
            return Some(
                self.import_style_library(path)
                    .map(|_| Effect::Tool)
                    .map_err(failed),
            );
        }
        // W11-D: a `.cube` becomes a Color Lookup adjustment layer.
        if is_cube_path(path) {
            return Some(self.import_cube_lut(path));
        }
        // W9-N: patterns, gradients, shapes, swatches, a profile.
        if Self::is_resource_path(path) {
            return Some(self.open_resource(path));
        }
        // W13X-8: Sketch / XD / Figma open as their layers (artboards,
        // groups, shapes, text, bitmaps); the embedded preview only when the
        // layers cannot be read, and the status line says why.
        if let Some(result) = self.open_design_document(path) {
            return Some(result);
        }
        // W13X-7: a multi-page PDF / AI asks which pages, at what resolution
        // and how first (the import dialog); a Paint.NET file opens its layers.
        if let Some(result) = self.open_w13x7_document(path) {
            return Some(result);
        }
        // W13-D: PDF / AI (a multi-page PDF opens one artboard per page),
        // WMF / EMF, and the preview formats (EPS, PDN, Sketch, XD, FIG),
        // which open with a status line saying what was read.
        if let Some(result) = self.open_w13d_document(path) {
            return Some(result);
        }
        // W13-K: a `.jsx` / `.js` opens in the File > Script window with its
        // source shown; it runs only when the user presses Run there.
        if crate::script::is_script_path(path) {
            let opened = crate::script::open_file(path);
            if opened.is_ok() {
                self.set_status(format!(
                    "Opened {} in File > Script; press Run to run it",
                    path.display()
                ));
            }
            return Some(opened);
        }
        None
    }

    /// W11-D: open `path` whatever it is — the route drag-and-drop, File >
    /// Open Recent and the command line share. A library file goes to its
    /// importer and answers `Ok(None)`; anything else opens as a document
    /// (image, animation, `.psd`, `.rstudio` package or a file inside one)
    /// and answers its id.
    pub fn open_any(&mut self, path: &Path) -> Result<Option<DocumentId>, String> {
        if let Some(result) = self.open_resource_file(path) {
            return result.map(|_| None).map_err(|e| e.to_string());
        }
        self.open_path(path).map(Some).map_err(|e| e.to_string())
    }

    /// W11-D: a `.cube` file as a Color Lookup adjustment layer on the
    /// active document, one undo step, and the new layer made active. The
    /// file is size-checked before it is read and parsed with the same
    /// bounds the Color Lookup dialog's "Load" applies.
    pub fn import_cube_lut(&mut self, path: &Path) -> Result<Effect, ActionError> {
        let fail = |e: &dyn std::fmt::Display| {
            ActionError::failed(Action::Open, format!("{}: {e}", path.display()))
        };
        if self.active().is_none() {
            return Err(ActionError::unavailable(
                Action::Open,
                "open a document to apply this colour lookup table to",
            ));
        }
        let text = adjustments::extended::read_cube_file(path).map_err(|e| fail(&e))?;
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let lut = adjustments::Lut3d::parse_cube(&stem, &text).map_err(|e| fail(&e))?;
        let layer = Layer::with_kind(
            "Color Lookup",
            LayerKind::Adjustment(AdjustmentLayer {
                kind: AdjustmentKind::ColorLookup {
                    name: lut.name().to_string(),
                    size: lut.size() as u32,
                    table: lut.table().to_vec(),
                },
            }),
        );
        let id = layer.id;
        let doc = self
            .active_mut()
            .ok_or_else(|| fail(&"no document is open"))?;
        doc.apply(Command::create_layer(layer))
            .map_err(|e| fail(&e))?;
        let _ = doc.document.set_active_layer(Some(id));
        self.status = Some(format!(
            "Added a Color Lookup layer from {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        self.touch();
        Ok(Effect::DocumentEdited)
    }

    // ------------------------------------------------------------------ paste

    /// W11-D: Paste with no document open (Photopea): a new document the
    /// size of the clipboard's image, ready for the paste that follows.
    /// `os_image` is the OS clipboard image the caller already read for the
    /// paste itself, so the document is sized from the same read that is
    /// pasted (the OS clipboard is not read a second time here); without one
    /// the editor's own copy sizes it. Refused, and nothing created, when
    /// neither holds an image.
    pub fn new_document_for_paste(
        &mut self,
        os_image: Option<&crate::clipboard::ClipboardImage>,
    ) -> Result<(u32, u32), String> {
        let (w, h) = os_image
            .map(|image| (image.width, image.height))
            .or_else(|| self.clipboard().map(|c| (c.width, c.height)))
            .ok_or("The clipboard is empty")?;
        self.untitled_count += 1;
        let title = if self.untitled_count == 1 {
            "Untitled".to_string()
        } else {
            format!("Untitled {}", self.untitled_count)
        };
        self.new_document_with(w, h, &title, crate::import::BlankBackground::Transparent)
            .map_err(|e| e.to_string())?;
        Ok((w, h))
    }

    // ----------------------------------------------------------------- revert

    /// W11-D: the file File > Revert reads for the active document — its
    /// `.rstudio` package when it has one, else the image it was opened
    /// from. `None` for a document that has never been on disk.
    pub fn revert_source(&self) -> Option<PathBuf> {
        let doc = self.active()?;
        doc.project_path()
            .or_else(|| doc.source_path())
            .map(Path::to_path_buf)
    }

    /// W11-D: File > Revert. Reads the active document's file again and
    /// replaces the layers, their pixels, the canvas size, the selection,
    /// the guides and the document records with what it holds, as one
    /// history step ("Revert") that Undo takes back. The document is clean
    /// afterwards: it matches its file.
    pub fn revert_active(&mut self) -> Result<String, String> {
        let path = self
            .revert_source()
            .ok_or("The document has never been saved")?;
        let (id, depth) = {
            let doc = self.active().ok_or("No document is open")?;
            (doc.id(), self.prefs.history_depth)
        };
        // W13-D: a multi-page PDF comes back as its page artboards.
        // W13X-7: a PDF opened through the import dialog comes back as the
        // pages, resolution and mode it was opened with, and a Paint.NET
        // file as its layers.
        let chosen = w13x7::reopen_for_revert(id, &path, depth)
            .transpose()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let pages = match chosen {
            Some(doc) => Some(doc),
            None => Self::open_pages_document(id, &path, depth)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .map(|(doc, _, _)| doc),
        };
        // W13X-8: a Sketch / XD / Figma file comes back as its layers.
        let design = Self::open_design_layered(id, &path, depth)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let saved = if let Some(doc) = pages {
            Ok(doc)
        } else if let Some(doc) = design {
            Ok(doc)
        } else if Self::is_project_path(&path) {
            OpenDocument::open_project(id, &path, depth)
        } else {
            match Self::open_animated(id, &path, depth) {
                Ok(Some(doc)) => Ok(doc),
                Ok(None) => OpenDocument::open_image(id, &path, depth),
                Err(e) => Err(e),
            }
        }
        .map_err(|e| format!("{}: {e}", path.display()))?;
        let doc = self.active_mut().ok_or("No document is open")?;
        let current_depth = doc.document.meta.bit_depth;
        if current_depth != 8 && current_depth != saved.document.meta.bit_depth {
            return Err(format!(
                "Revert: the document is {current_depth}-bit and its file is {}-bit; \
                 convert it back (Image > Mode) before reverting",
                saved.document.meta.bit_depth
            ));
        }
        let command = revert_command(doc, &saved)?;
        doc.apply(command).map_err(|e| format!("Revert: {e}"))?;
        let _ = doc.document.set_active_layer(saved.document.active_layer());
        doc.document.mark_saved();
        let message = format!("Reverted to {}", path.display());
        self.status = Some(message.clone());
        self.touch();
        Ok(message)
    }
}

/// The one transaction that turns `doc` into `saved`: every current layer
/// out, the saved layers (and their pixels) in, the canvas, selection,
/// guides and records set. Tile bytes the saved file holds are filed in
/// `doc`'s store first, so the commands only carry hashes, as every edit
/// does. Locks are lifted for the swap and put back as they were saved, so
/// a locked layer neither blocks the revert nor comes back unlocked.
fn revert_command(doc: &mut OpenDocument, saved: &OpenDocument) -> Result<Command, String> {
    let mut commands = Vec::new();
    let unlocked = LockState::default();
    // Out: unlock, then delete every top-level layer (its subtree goes with
    // it; the inverse puts each back where it was).
    for id in doc.document.layers.iter_depth_first() {
        if doc.document.layers.get(id).is_some_and(|l| l.locked.all) {
            commands.push(Command::SetLayerProperties {
                layer_id: id,
                patch: LayerPatch {
                    locked: Some(unlocked),
                    ..Default::default()
                },
            });
        }
    }
    for id in doc.document.layers.root().to_vec() {
        commands.push(Command::DeleteLayer { layer_id: id });
    }
    let size = saved.document.meta.size;
    if size != doc.document.meta.size {
        commands.push(Command::SetCanvasSize { size });
    }
    if saved.document.meta.color_mode != doc.document.meta.color_mode {
        commands.push(Command::SetMetaColorMode {
            from: doc.document.meta.color_mode,
            to: saved.document.meta.color_mode,
        });
    }
    if saved.document.meta.bit_depth != doc.document.meta.bit_depth {
        commands.push(Command::SetMetaBitDepth {
            from: doc.document.meta.bit_depth,
            to: saved.document.meta.bit_depth,
        });
    }
    // In: the saved tree, unlocked, one detached subtree per top-level
    // layer, each restored at its own index.
    let mut tree = saved.document.layers.clone();
    let mut relock = Vec::new();
    for id in tree.iter_depth_first() {
        if let Some(layer) = tree.get_mut(id) {
            if layer.locked != unlocked {
                relock.push((id, layer.locked));
                layer.locked = unlocked;
            }
        }
    }
    let roots = tree.root().to_vec();
    let mut subtrees = Vec::with_capacity(roots.len());
    for id in roots.iter().rev() {
        subtrees.push(tree.remove(*id).map_err(|e| e.to_string())?);
    }
    for subtree in subtrees.into_iter().rev() {
        commands.push(Command::RestoreLayers { subtree });
    }
    // Their pixels: every target the saved layers own, set tile for tile;
    // a tile the current store still holds under that key and the file does
    // not is cleared.
    for id in saved.document.layers.iter_depth_first() {
        let Some(layer) = saved.document.layers.get(id) else {
            continue;
        };
        let mut targets = Vec::new();
        if matches!(
            layer.kind,
            LayerKind::Raster(_) | LayerKind::Generator(_) | LayerKind::SmartObject(_)
        ) {
            targets.push((PixelTarget::Layer(id), PixelKey::Layer(id)));
        }
        if let Some(mask) = layer.mask_id() {
            targets.push((PixelTarget::Mask(id), PixelKey::Mask(mask)));
        }
        if let LayerKind::SmartObject(so) = &layer.kind {
            if let Some(m) = &so.filter_mask {
                targets.push((PixelTarget::FilterMask(id), PixelKey::Mask(m.id)));
            }
        }
        for (target, key) in targets {
            let mut edits = Vec::new();
            let wanted = saved.document.pixels.tiles(key);
            if let Some(map) = wanted {
                for (coord, hash) in map.iter() {
                    let bytes = saved
                        .tiles
                        .tile(hash)
                        .ok_or_else(|| format!("Revert: the file is missing tile {coord:?}"))?;
                    let filed = doc.tiles.insert_bytes(bytes.to_vec());
                    edits.push(TileEdit::set(coord, filed));
                }
            }
            if let Some(current) = doc.document.pixels.tiles(key) {
                for (coord, _) in current.iter() {
                    if !wanted.is_some_and(|m| m.contains(coord)) {
                        edits.push(TileEdit::clear(coord));
                    }
                }
            }
            if !edits.is_empty() {
                commands.push(Command::paint_tiles(target, edits).map_err(|e| e.to_string())?);
            }
        }
    }
    for (id, locked) in relock {
        commands.push(Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                locked: Some(locked),
                ..Default::default()
            },
        });
    }
    commands.push(Command::SetSelection {
        selection: saved.document.selection.clone(),
    });
    commands.push(Command::SetGuides {
        guides: saved.document.guides.clone(),
    });
    commands.push(Command::SetDocumentExtras {
        extras: Box::new(saved.document.extras.clone()),
    });
    // Smart objects name their sources in the asset table: register the ones
    // the file names and the document lacks (append-only, like placement).
    for record in saved.document.assets() {
        if !doc.document.assets().iter().any(|a| a.id == record.id) {
            doc.document.set_asset_origin(record.clone());
        }
    }
    Ok(Command::Transaction {
        label: REVERT_LABEL.to_string(),
        commands,
    })
}

#[cfg(test)]
#[path = "editor_open_any_tests.rs"]
mod tests;

/// W13-D: PDF pages as artboards, and the preview formats' open route.
#[path = "editor_open_pages.rs"]
pub(crate) mod open_pages;

/// W13X-7: the PDF import dialog's open route and Paint.NET layers.
#[path = "editor_open_w13x7.rs"]
pub(crate) mod w13x7;

/// W13X-8: Sketch / XD / Figma opened as layers.
#[path = "import_design.rs"]
pub(crate) mod import_design;
