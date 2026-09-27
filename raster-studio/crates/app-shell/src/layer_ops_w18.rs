//! W18-G: Photopea's Layer ▸ Smart Object ▸ Turn into JPG and File ▸ Save
//! PSD/PSB….
//!
//! * **Turn into JPG** — the active smart object's embedded source is
//!   decoded, flattened onto white (a JPEG has no alpha) and re-encoded as a
//!   JPEG; the asset row keeps the JPEG's bytes under the source's name with
//!   a `.jpg` extension, and every smart object sharing the source shows the
//!   decoded JPEG. One undo step reverts the pixels and the source together.
//!   A linked object is refused: its source is its file.
//! * **Save PSD/PSB…** — the layered save (`import::psd_from_document`, the
//!   one every `.psd` export takes) written again with the Save PSD/PSB
//!   options its dialog confirmed (`ui::dialogs::export_as::psd_options`):
//!   PSD or PSB, a blank preview image, ZIP for pixel data, the file put
//!   into a ZIP archive.
//! * **Export As ▸ Artboards / Slices** — Photopea's options: a confirmed
//!   Export As job with either is parked by the dialog and asks for File ▸
//!   Export ▸ Artboards to Files (or Slices), whose route here writes every
//!   enabled row once per artboard, or per slice ("User Slices": the
//!   committed ones; "All Slices": those and the automatic ones covering the
//!   rest of the canvas, [`auto_slices`]). A job with a PDF row and its
//!   "reverse pages" is parked likewise and written whole, the PDF's pages
//!   (one per artboard) last first.

use std::path::{Path, PathBuf};

use editor_core::Command;
use layer_model::{AssetOrigin, LayerKind};
use raster::PixelRect;
use ui::dialogs::export_as::psd_options::{self, remembered_options, PsdSaveOptions};
use ui::dialogs::export_as::{take_parked_export, ExportExtras, SliceExport};

use crate::editor::Editor;

/// The JPEG quality Turn into JPG writes at.
pub const TURN_INTO_JPG_QUALITY: u8 = 90;

/// Straight-alpha RGBA8 flattened onto white, alpha set opaque.
fn onto_white(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for px in out.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        for c in &mut px[..3] {
            *c = ((u32::from(*c) * a + 255 * (255 - a) + 127) / 255) as u8;
        }
        px[3] = 255;
    }
    out
}

/// Layer ▸ Smart Object ▸ Turn into JPG.
pub(crate) fn turn_into_jpg(editor: &mut Editor) -> Result<String, String> {
    let (layers, tiles, asset, jpg_name, jpeg, size) = {
        let doc = editor.active().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        let asset = match doc.document.layers.get(id).map(|l| &l.kind) {
            Some(LayerKind::SmartObject(so)) => so.asset,
            _ => return Err("The active layer is not a smart object".to_string()),
        };
        let (name, bytes) = match doc.document.asset_origin(asset) {
            Some(AssetOrigin::Embedded { name, bytes }) => (name.clone(), bytes.clone()),
            Some(AssetOrigin::Linked { .. }) => {
                return Err(ui::menu::TURN_INTO_JPG_LINKED.to_string());
            }
            None => return Err("The smart object's asset is missing from the document".into()),
        };
        let source = raster::decode_surface_bytes(&bytes, raster::ImportLimits::default())
            .map_err(|e| format!("Turn into JPG: the source cannot be read: {e}"))?
            .into_decoded_image();
        let jpeg = raster::encode_with(
            raster::ExportFormat::Jpeg(TURN_INTO_JPG_QUALITY),
            source.width,
            source.height,
            raster::EncodedPixels::Rgba8(&onto_white(&source.rgba8)),
            &raster::EncodeOptions {
                icc_profile: source.icc_profile.clone(),
            },
        )
        .map_err(|e| format!("Turn into JPG: {e}"))?;
        let decoded = raster::decode_surface_bytes(&jpeg, raster::ImportLimits::default())
            .map_err(|e| format!("Turn into JPG: the JPEG does not read back: {e}"))?
            .into_decoded_image();
        let image = crate::import::DecodedImage {
            width: decoded.width,
            height: decoded.height,
            rgba8: decoded.rgba8,
            color_space: decoded.color_space,
            icc_profile: decoded.icc_profile,
        };
        let converted =
            crate::placement::working_space_pixels(&image, &doc.document.meta.color_space)
                .map_err(|e| e.to_string())?;
        let tiles = crate::placement::slice_source_tiles(&converted, glam::IVec2::ZERO);
        let layers: Vec<layer_model::LayerId> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|l| {
                matches!(
                    doc.document.layers.get(*l).map(|x| &x.kind),
                    Some(LayerKind::SmartObject(so)) if so.asset == asset
                )
            })
            .collect();
        let jpg_name = format!(
            "{}.jpg",
            Path::new(&name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Smart Object".to_string())
        );
        (
            layers,
            tiles,
            asset,
            jpg_name,
            jpeg,
            (image.width, image.height),
        )
    };
    let (before, after) = (
        editor
            .active()
            .and_then(|d| match d.document.asset_origin(asset) {
                Some(AssetOrigin::Embedded { bytes, .. }) => Some(bytes.len()),
                _ => None,
            })
            .unwrap_or(0),
        jpeg.len(),
    );
    let open = editor.active_mut().ok_or("No document is open")?;
    let mut commands: Vec<Command> = Vec::new();
    for layer_id in &layers {
        let mut edits = Vec::new();
        for (coord, bytes) in &tiles {
            let hash = open.tiles.insert_bytes(bytes.clone());
            edits.push(editor_core::pixels::TileEdit::set(*coord, hash));
        }
        if let Some(old) = open.document.layer_tiles(*layer_id) {
            for (c, _) in old.iter() {
                if !tiles.iter().any(|(coord, _)| *coord == c) {
                    edits.push(editor_core::pixels::TileEdit::clear(c));
                }
            }
        }
        commands.push(
            Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(*layer_id), edits)
                .map_err(|e| e.to_string())?,
        );
    }
    commands.push(Command::ReplaceAssetSource {
        asset,
        origin: AssetOrigin::Embedded {
            name: jpg_name.clone(),
            bytes: jpeg,
        },
        source_size: Some(size),
    });
    let depth = open.history_depth();
    editor.apply_command(Command::Transaction {
        label: "Turn into JPG".to_string(),
        commands,
    });
    if editor.active().map(|d| d.history_depth()) == Some(depth) {
        return Err("Turn into JPG was refused".to_string());
    }
    Ok(format!(
        "Turned the smart object's source into {jpg_name} ({after} bytes, was {before})"
    ))
}

/// `bytes` (a layered file this build wrote) written again with the Save
/// PSD/PSB `options`: a `.psb` when asked, the merged composite blank paper
/// when asked, every channel ZIP-compressed when asked.
pub(crate) fn psd_bytes_with(bytes: &[u8], options: PsdSaveOptions) -> Result<Vec<u8>, String> {
    // This build's own output: bounded by what it just wrote, not by the
    // untrusted-file defaults.
    let mut read = psd::ReadOptions::default();
    read.max_decoded_bytes = u64::MAX;
    read.max_resource_bytes = usize::MAX;
    read.max_tagged_block_bytes = usize::MAX;
    let file = psd::read_with(bytes, &read).map_err(|e| e.to_string())?;
    let mut write = psd::WriteOptions::default();
    if options.zip_pixel_data {
        write.layer_compression = psd::Compression::Zip;
        write.merged_compression = psd::Compression::Zip;
    }
    write.blank_preview = options.blank_preview;
    let out = if options.psb {
        psd::write_psb_with(&file, &write)
    } else {
        psd::write_with(&file, &write)
    }
    .map_err(|e| e.to_string())?;
    if !options.psb && psd::is_psb(&out) {
        return Err(format!(
            "this document is wider or taller than {} px, which a .psd cannot describe: \
             choose PSB",
            psd::write::MAX_DIMENSION
        ));
    }
    Ok(out)
}

/// File ▸ Save PSD/PSB…. The row's first run asks for its options dialog
/// (`psd_options::request_open`); the dialog's Save marks them confirmed
/// and asks for the row again, and that run asks where and writes the
/// layered file with the confirmed options — inside a `.zip` when "Put the
/// file into ZIP" is on.
pub(crate) fn save_psd_psb(editor: &mut Editor) -> Result<String, String> {
    editor.active().ok_or("No document is open")?;
    if !psd_options::take_confirmed() {
        psd_options::request_open();
        return Ok("Save PSD/PSB: choose the options".to_string());
    }
    let options = remembered_options();
    let suggested = editor
        .active()
        .map(|d| {
            d.suggested_export_path()
                .with_extension(options.extension())
        })
        .ok_or("No document is open")?;
    let Some(target) = editor.pick_save_path(&suggested) else {
        return Err("Save PSD/PSB: no destination chosen".to_string());
    };
    let target: PathBuf = target.with_extension(options.extension());
    let doc = editor.active_mut().ok_or("No document is open")?;
    // Card 077: the file this document was opened from is never silently
    // replaced by this build's export of it.
    if let Some(source) = doc.source_path() {
        if crate::doc::same_path(source, &target) {
            return Err(format!(
                "Save PSD/PSB: {} is the file this document was opened from",
                target.display()
            ));
        }
    }
    let rgba8 = doc
        .composite(doc.canvas_rect())
        .map_err(|e| e.to_string())?;
    let (bytes, notes) = crate::import::psd_from_document(&doc.document, &doc.tiles, &rgba8)
        .map_err(|e| e.to_string())?;
    let mut bytes = psd_bytes_with(&bytes, options)?;
    if options.put_into_zip {
        let stem = target
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string());
        let inner = format!("{stem}.{}", options.document_extension());
        bytes = psd::write::zip_container::wrap(&inner, &bytes)
            .map_err(|e| format!("Save PSD/PSB: {e}"))?;
    }
    crate::doc::write_atomically(&target, &bytes).map_err(|e| format!("Save PSD/PSB: {e}"))?;
    doc.set_psd_notes(notes);
    if !options.put_into_zip {
        doc.adopt_title_from(&target);
    }
    Ok(format!(
        "Saved {} ({} bytes)",
        target.display(),
        bytes.len()
    ))
}

// ------------------------------------------------ Export As, per region

/// File ▸ Export ▸ Artboards to Files…: an Export As job parked with
/// Photopea's "Artboards" (or "Slices") option is written here with every
/// row of the job; with none parked the row is W8-C's own export.
pub(crate) fn export_artboards(editor: &mut Editor) -> Result<String, String> {
    match take_parked_export() {
        Some((job, extras)) => export_parked(editor, &job, extras),
        None => crate::artboard_export::export_artboards(editor),
    }
}

/// File ▸ Export ▸ Slices…: likewise for a job parked with "Slices".
pub(crate) fn export_slices(editor: &mut Editor) -> Result<String, String> {
    match take_parked_export() {
        Some((job, extras)) => export_parked(editor, &job, extras),
        None => crate::slices_export::export_slices(editor),
    }
}

/// The document-space rects the user's slices do not cover, as Photopea's
/// "All Slices" adds them: the canvas cut into the grid every slice edge
/// draws, each row band's uncovered cells merged left to right while they
/// touch. With no user slice it is the whole canvas.
pub(crate) fn auto_slices(width: u32, height: u32, user: &[PixelRect]) -> Vec<PixelRect> {
    let (w, h) = (i64::from(width), i64::from(height));
    let clip = |r: &PixelRect| {
        let x0 = r.x.clamp(0, w);
        let y0 = r.y.clamp(0, h);
        let x1 = (r.x + i64::from(r.width)).clamp(0, w);
        let y1 = (r.y + i64::from(r.height)).clamp(0, h);
        (x0, y0, x1, y1)
    };
    let boxes: Vec<(i64, i64, i64, i64)> = user
        .iter()
        .map(clip)
        .filter(|(x0, y0, x1, y1)| x1 > x0 && y1 > y0)
        .collect();
    let mut xs = vec![0, w];
    let mut ys = vec![0, h];
    for (x0, y0, x1, y1) in &boxes {
        xs.extend([*x0, *x1]);
        ys.extend([*y0, *y1]);
    }
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    let covered = |x: i64, y: i64| {
        boxes
            .iter()
            .any(|(x0, y0, x1, y1)| x >= *x0 && x < *x1 && y >= *y0 && y < *y1)
    };
    let mut out = Vec::new();
    for band in ys.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        let mut run: Option<(i64, i64)> = None;
        for cell in xs.windows(2) {
            let (x0, x1) = (cell[0], cell[1]);
            if covered(x0, y0) {
                if let Some((a, b)) = run.take() {
                    out.push(PixelRect::new(a, y0, (b - a) as u32, (y1 - y0) as u32));
                }
            } else {
                run = Some(run.map_or((x0, x1), |(a, _)| (a, x1)));
            }
        }
        if let Some((a, b)) = run {
            out.push(PixelRect::new(a, y0, (b - a) as u32, (y1 - y0) as u32));
        }
    }
    out
}

/// One region an Export As job is cut into: the words its files are named
/// by (after the job's name) and the straight-alpha RGBA8 it shows.
struct Region {
    name: String,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// The rect `r` of the `w`-wide straight RGBA8 `canvas`, clipped to it.
fn crop(canvas: &[u8], w: u32, h: u32, r: PixelRect) -> Option<(u32, u32, Vec<u8>)> {
    let x0 = r.x.clamp(0, i64::from(w)) as usize;
    let y0 = r.y.clamp(0, i64::from(h)) as usize;
    let x1 = (r.x + i64::from(r.width)).clamp(0, i64::from(w)) as usize;
    let y1 = (r.y + i64::from(r.height)).clamp(0, i64::from(h)) as usize;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity(cw * ch * 4);
    for row in y0..y1 {
        let s = (row * w as usize + x0) * 4;
        out.extend_from_slice(&canvas[s..s + cw * 4]);
    }
    Some((cw as u32, ch as u32, out))
}

/// The regions `extras` cuts the active document into.
fn regions(editor: &mut Editor, extras: ExportExtras, base: &str) -> Result<Vec<Region>, String> {
    if extras.slices != SliceExport::No {
        crate::slices_export::restore_saved_slices(editor);
        let id = editor.active().ok_or("No document is open")?.id();
        let user = editor.slices.get(id).to_vec();
        let names: Vec<String> = editor
            .slices
            .options(id)
            .iter()
            .map(|o| o.name.clone())
            .collect();
        if extras.slices == SliceExport::User && user.is_empty() {
            return Err(
                "Export As: this document has no slices - draw them with the Slice tool and \
                 press Enter, or choose All Slices"
                    .to_string(),
            );
        }
        let doc = editor.active_mut().ok_or("No document is open")?;
        let (w, h) = (doc.document.width(), doc.document.height());
        let canvas = doc
            .composite(doc.canvas_rect())
            .map_err(|e| e.to_string())?;
        let mut taken = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (index, rect) in user.iter().enumerate() {
            let number = index + 1;
            let name = ui::dialogs::slice_options::slice_file_stem(
                base,
                names.get(index).map_or("", String::as_str),
                number,
            );
            let (cw, ch, rgba) = crop(&canvas, w, h, *rect)
                .ok_or_else(|| format!("Export As: slice {number} lies outside the canvas"))?;
            taken.insert(name.to_lowercase());
            out.push(Region {
                name,
                width: cw,
                height: ch,
                rgba,
            });
        }
        if extras.slices == SliceExport::All {
            let mut number = user.len();
            for rect in auto_slices(w, h, &user) {
                let name = loop {
                    number += 1;
                    let name = raster::export::sanitize_file_stem(&format!("{base}_{number:02}"));
                    if taken.insert(name.to_lowercase()) {
                        break name;
                    }
                };
                if let Some((cw, ch, rgba)) = crop(&canvas, w, h, rect) {
                    out.push(Region {
                        name,
                        width: cw,
                        height: ch,
                        rgba,
                    });
                }
            }
        }
        return Ok(out);
    }
    let doc = editor.active().ok_or("No document is open")?;
    let boards = layer_model::artboard::artboards(&doc.document.layers);
    if boards.is_empty() {
        return Err(
            "Export As: this document has no artboards - draw one with the Artboard tool, or \
             untick Artboards"
                .to_string(),
        );
    }
    let mut used: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(boards.len());
    for (group, board) in boards {
        let label = doc
            .document
            .layers
            .get(group)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        let rect = crate::artboard_export::artboard_document_rect(&doc.document, group, &board)
            .ok_or_else(|| format!("Export As: artboard '{label}' has no area"))?;
        let rgba = crate::artboard_export::artboard_pixels(doc, group, rect)?;
        let stem = raster::export::sanitize_file_stem(&format!("{base}_{label}"));
        let count = used.entry(stem.to_lowercase()).or_insert(0);
        *count += 1;
        let name = if *count == 1 {
            stem
        } else {
            format!("{stem}_{count}")
        };
        out.push(Region {
            name,
            width: rect.width,
            height: rect.height,
            rgba,
        });
    }
    Ok(out)
}

/// Write `job` cut into the regions `extras` asks for — one file per
/// artboard or per slice for every enabled row (`<region><suffix>.<ext>`),
/// through the exporter Export As uses — into the folder the picker gives.
pub(crate) fn export_parked(
    editor: &mut Editor,
    job: &ui::dialogs::ExportJob,
    extras: ExportExtras,
) -> Result<String, String> {
    // Neither per-artboard nor per-slice: the whole canvas, with the PDF's
    // "reverse pages".
    if !extras.artboards && extras.slices == SliceExport::No {
        return export_whole(editor, job, extras);
    }
    let base = if job.base_name.trim().is_empty() {
        "export".to_string()
    } else {
        raster::export::sanitize_file_stem(&job.base_name)
    };
    // Measure first: a document with nothing to cut says so before asking
    // for a folder.
    let cut = regions(editor, extras, &base)?;
    let Some(dir) = editor.pick_export_folder() else {
        return Err("Export As: no destination chosen".to_string());
    };
    ui::dialogs::export_as::remember_exported_job(job);
    let doc = editor.active().ok_or("No document is open")?;
    let (space, mode) = (
        doc.document.meta.color_space.clone(),
        doc.document.meta.color_mode,
    );
    let metadata = raster::export::ExportMetadata {
        icc_profile: None,
        icc_profile_space: None,
    };
    let mut written = 0usize;
    for region in &cut {
        let image =
            raster::export::linear_from_rgba8(region.width, region.height, &region.rgba, &space)
                .map_err(|e| format!("Export As: {}: {e}", region.name))?;
        for entry in job.entries.iter().filter(|e| e.enabled) {
            let mut preset = entry.preset.clone().for_color_mode(mode);
            preset.name = format!("{}{}", region.name, entry.suffix);
            let paths = raster::export::export_batch_to_dir(&dir, &image, &[preset], &metadata)
                .map_err(|e| format!("Export As: {}: {e}", region.name))?;
            written += paths.len();
        }
    }
    Ok(format!(
        "Exported {written} file(s) ({} {}) to {}",
        cut.len(),
        if extras.slices == SliceExport::No {
            "artboard(s)"
        } else {
            "slice(s)"
        },
        dir.display()
    ))
}

/// An Export As job written whole, as the batch writer writes it (every
/// enabled row, `<name><suffix>.<ext>`; an SVG, EXR, PDF, EMF or DXF row at
/// 100% rewritten from the layers as `jobs::run_export` does), except that
/// with Photopea's PDF option "reverse pages" a PDF row's pages (one per
/// artboard, in reading order) are written last first.
fn export_whole(
    editor: &mut Editor,
    job: &ui::dialogs::ExportJob,
    extras: ExportExtras,
) -> Result<String, String> {
    let Some(dir) = editor.pick_export_folder() else {
        return Err("Export As: no destination chosen".to_string());
    };
    ui::dialogs::export_as::remember_exported_job(job);
    let doc = editor.active_mut().ok_or("No document is open")?;
    let written = doc
        .export_job(job, &dir)
        .map_err(|e| format!("Export As: {e}"))?;
    let rows = job.entries.iter().filter(|e| e.enabled);
    let mut reversed = 0usize;
    for (entry, path) in rows.zip(&written) {
        let (format, whole) = (entry.preset.format, entry.preset.scale == 1.0);
        let err = |e: crate::doc::DocumentError| format!("Export As: {}: {e}", path.display());
        if !whole {
            continue;
        }
        if format == raster::ExportFormat::Svg {
            crate::doc::write_vector_svg(&doc.document, &doc.tiles, path).map_err(err)?;
        }
        if format == raster::ExportFormat::Exr {
            let rect = doc.canvas_rect();
            crate::depth32::write_float_tiff(path, format, &doc.document, || {
                Ok(compositor::composite_region(
                    &doc.document,
                    &doc.tiles,
                    rect,
                    0,
                    compositor::CompositeOptions::default(),
                )?)
            })
            .map_err(err)?;
        }
        if format == raster::ExportFormat::Pdf && extras.reverse_pages {
            let mut scene =
                crate::menu_bridge::w16k::vector_doc(&doc.document, &doc.tiles).map_err(err)?;
            scene.pages.reverse();
            let bytes = raster::codec::export_vector::encode_pdf(&scene)
                .map_err(|e| format!("Export As: {}: {e}", path.display()))?;
            crate::doc::write_atomically(path, &bytes)
                .map_err(|e| format!("Export As: {}: {e}", path.display()))?;
            reversed += 1;
        } else if raster::ExportFormat::VECTOR.contains(&format) {
            crate::menu_bridge::w16k::write_vector_export(&doc.document, &doc.tiles, format, path)
                .map_err(err)?;
        }
    }
    Ok(format!(
        "Exported {} file(s) to {}{}",
        written.len(),
        dir.display(),
        if reversed > 0 {
            " (PDF pages reversed)"
        } else {
            ""
        }
    ))
}

#[cfg(test)]
#[path = "layer_ops_w18_tests.rs"]
mod tests;
