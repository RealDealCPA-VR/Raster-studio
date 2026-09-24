//! W10-H: Image ▸ Mode ▸ 32 Bits/Channel.
//!
//! A 32-bit document (`DocumentMeta::bit_depth == 32`) keeps every raster
//! layer tile as `f32` ([`raster::depth32`]: straight alpha, colour in the
//! document's encoding, values above `1.0` kept as HDR headroom). What works
//! at `f32` here:
//!
//! * the conversion itself, 8/16 → 32 losslessly and 32 → 16/8 by clipping
//!   (and dithering on the way to 8), as one undoable step
//!   ([`depth_conversion`]);
//! * compositing (`compositor`'s `fill_layer` reads `f32` tiles);
//! * every filter and adjustment that runs through
//!   `menu_bridge::edit_active_pixels` ([`edit_active_pixels_f32`]): the
//!   layer is read as `f32`, filtered in the `f32` [`filters::FilterBuffer`]
//!   and written back as `f32` tiles, never clipped;
//! * File ▸ Export to `.tif`/`.tiff`, which writes a 32-bit float TIFF of the
//!   linear composite ([`write_float_tiff`]), and (W11-H) to `.exr` — File ▸
//!   Export and an Export As row at 100% alike — a 32-bit float OpenEXR of it;
//! * the whole-layer remaps (Image ▸ Image Rotation 180°/Flip Canvas, the
//!   layer flips), Edit ▸ Clear and Edit ▸ Fill, and Image ▸ Image Size,
//!   which read, move and write `f32` samples ([`layer_rgbaf32`],
//!   [`layer_rgbaf32_command`], [`resample_layer_f32_edits`]).
//!
//! Everything else a 32-bit document meets reads its tiles through the
//! shared 8/16-bit readers (`raster::rgba8_view`, `rgba16_samples`), which
//! clip an `f32` tile into range, and writes 8- or 16-bit output that
//! [`fit_to_f32_document`] lands back in `f32`. There a sample keeps its old
//! `f32` value when the output equals it rounded to the output's depth and
//! the old value is inside `0..=1` (so the kept value is within half an
//! output code of the output). An old value outside `0..=1` is kept on a
//! match only under a paint stroke of an [`in_place_tool`], and only when the
//! whole pixel matches; under any other edit that lands 8/16-bit tiles (the
//! Free Transform/Patch/Content-Aware Move/Clone tools, a menu command still
//! on the 8/16-bit road) it lands as the clipped output: the edit loses that
//! pixel's HDR headroom, it never leaves a stale value where moved content
//! should be.

use compositor::{MemoryTileSource, TileSource};
use editor_core::pixels::{PixelTarget, TileEdit};
use editor_core::{Command, Document};

use crate::doc::{DocumentError, OpenDocument};
use crate::editor::Editor;

/// Bytes in an `f32` layer tile.
pub const F32_TILE_BYTES: usize = raster::depth32::RGBAF32_TILE_BYTES;

/// The Image ▸ Mode ▸ 8/16/32 Bits/Channel conversion when either end is
/// 32 bits, built but not applied (the menu hands it to
/// [`Editor::apply_command`]). `None` when neither end is 32: that is
/// `OpenDocument::depth_conversion`'s 8 ↔ 16.
pub(crate) fn depth_conversion(
    doc: &mut OpenDocument,
    to_bits: u8,
    dither: bool,
) -> Option<Result<Command, String>> {
    let from = doc.document.meta.bit_depth;
    if from != 32 && to_bits != 32 {
        return None;
    }
    Some(build_conversion(doc, from, to_bits, dither))
}

fn build_conversion(
    doc: &mut OpenDocument,
    from: u8,
    to_bits: u8,
    dither: bool,
) -> Result<Command, String> {
    use editor_core::color_mode::mode;
    if !matches!(to_bits, 8 | 16 | 32) {
        return Err(format!("{to_bits} bits per channel is not available"));
    }
    if from == to_bits {
        return Err("The document is already at that depth".to_string());
    }
    if to_bits == 32 && !matches!(doc.document.meta.color_mode, mode::RGB | mode::GRAYSCALE) {
        return Err("32 Bits/Channel is for RGB and Grayscale documents".to_string());
    }
    let mut commands = vec![Command::SetMetaBitDepth { from, to: to_bits }];
    for layer_id in doc.document.layers.iter_depth_first() {
        let Some(map) = doc.document.layer_tiles(layer_id) else {
            continue;
        };
        let converted: Vec<_> = map
            .iter()
            .filter_map(|(coord, hash)| {
                let bytes = doc.tiles.tile(hash)?;
                let out = convert_tile(bytes, to_bits, dither)?;
                Some((coord, out))
            })
            .collect();
        if converted.is_empty() {
            continue;
        }
        let edits: Vec<TileEdit> = converted
            .into_iter()
            .map(|(coord, bytes)| TileEdit::set(coord, doc.tiles.insert_bytes(bytes)))
            .collect();
        commands.push(
            Command::paint_tiles(PixelTarget::Layer(layer_id), edits).map_err(|e| e.to_string())?,
        );
    }
    Ok(Command::Transaction {
        label: format!("Convert to {to_bits} Bits/Channel"),
        commands,
    })
}

/// One layer tile at `to_bits`, or `None` when it is already there (or is
/// not a colour tile).
fn convert_tile(bytes: &[u8], to_bits: u8, dither: bool) -> Option<Vec<u8>> {
    use raster::depth::{RGBA16_TILE_BYTES, RGBA8_TILE_BYTES};
    use raster::depth32::{narrow_rgbaf32_tile, widen_to_rgbaf32_tile};
    match (to_bits, bytes.len()) {
        (32, _) => widen_to_rgbaf32_tile(bytes),
        (16, F32_TILE_BYTES) => narrow_rgbaf32_tile(bytes, 16, false),
        (16, RGBA8_TILE_BYTES) => raster::widen_rgba8_tile(bytes),
        (8, F32_TILE_BYTES) => narrow_rgbaf32_tile(bytes, 8, dither),
        (8, RGBA16_TILE_BYTES) => raster::narrow_rgba16_tile(bytes, dither),
        _ => None,
    }
}

/// Image ▸ Mode ▸ <depth> from the menu when either end is 32 bits: the
/// conversion as one undoable step through [`Editor::apply_command`].
/// `None` hands the click back to the 8 ↔ 16 route.
pub(crate) fn perform_set_bit_depth(
    editor: &mut Editor,
    depth: ui::menu::ChannelDepth,
) -> Option<Result<String, String>> {
    let doc = editor.active_mut()?;
    let command = match depth_conversion(doc, depth.bits(), true)? {
        Ok(command) => command,
        Err(e) => return Some(Err(e)),
    };
    editor.apply_command(command);
    Some(match editor.active().map(|d| d.document.meta.bit_depth) {
        Some(bits) if bits == depth.bits() => Ok(format!("Converted to {}", depth.label())),
        _ => Err(format!("Could not convert to {}", depth.label())),
    })
}

/// A layer's pixels over the canvas rectangle as straight `f32` RGBA in the
/// document's encoding: the `f32` twin of `OpenDocument::layer_rgba16`
/// (tiles outside the canvas dropped, absent tiles transparent black).
pub(crate) fn layer_rgbaf32(doc: &OpenDocument, layer: layer_model::LayerId) -> Vec<f32> {
    layer_rgbaf32_in(&doc.document, &doc.tiles, layer)
}

/// [`layer_rgbaf32`] over a document and its tile store held apart.
fn layer_rgbaf32_in(
    document: &Document,
    tiles: &MemoryTileSource,
    layer: layer_model::LayerId,
) -> Vec<f32> {
    let (w, h) = (document.width() as usize, document.height() as usize);
    let mut out = vec![0f32; w * h * 4];
    let Some(map) = document.layer_tiles(layer) else {
        return out;
    };
    let ts = raster::TILE_SIZE as usize;
    for (coord, hash) in map.iter() {
        if coord.level != 0 {
            continue;
        }
        let Some(samples) = tiles.tile(hash).and_then(raster::depth32::rgbaf32_samples) else {
            continue;
        };
        let ox = coord.x as i64 * ts as i64;
        let oy = coord.y as i64 * ts as i64;
        let x0 = ox.max(0);
        let x1 = (ox + ts as i64).min(w as i64);
        if x1 <= x0 {
            continue;
        }
        let n = (x1 - x0) as usize * 4;
        for row in 0..ts {
            let y = oy + row as i64;
            if y < 0 || y >= h as i64 {
                continue;
            }
            let s = (row * ts + (x0 - ox) as usize) * 4;
            let d = (y as usize * w + x0 as usize) * 4;
            out[d..d + n].copy_from_slice(&samples[s..s + n]);
        }
    }
    out
}

/// The command that makes `rgba` (canvas-sized straight `f32` RGBA) the
/// layer's pixels as `f32` tiles, clearing tiles the new image no longer
/// covers: the `f32` twin of `OpenDocument::layer_rgba16_command`.
pub(crate) fn layer_rgbaf32_command(
    doc: &mut OpenDocument,
    layer: layer_model::LayerId,
    rgba: &[f32],
    label: &str,
) -> Result<Command, String> {
    let (w, h) = (
        doc.document.width() as usize,
        doc.document.height() as usize,
    );
    if rgba.len() != w * h * 4 {
        return Err("The pixel buffer does not match the canvas".to_string());
    }
    let previous: Vec<raster::TileCoord> = doc
        .document
        .layer_tiles(layer)
        .map(|m| m.iter().map(|(c, _)| c).collect())
        .unwrap_or_default();
    let mut edits = f32_tile_edits(&mut doc.tiles, (w, h), rgba);
    let covered: std::collections::HashSet<raster::TileCoord> =
        edits.iter().map(|e| e.coord).collect();
    for coord in previous {
        if !covered.contains(&coord) {
            edits.push(TileEdit::clear(coord));
        }
    }
    let paint =
        Command::paint_tiles(PixelTarget::Layer(layer), edits).map_err(|e| e.to_string())?;
    Ok(Command::Transaction {
        label: label.to_string(),
        commands: vec![paint],
    })
}

/// Store a `w x h` straight `f32` RGBA canvas buffer as `f32` tiles: one
/// edit per covering tile, padding zeroed, row-major.
fn f32_tile_edits(
    tiles: &mut MemoryTileSource,
    (w, h): (usize, usize),
    rgba: &[f32],
) -> Vec<TileEdit> {
    let ts = raster::TILE_SIZE as usize;
    let mut edits = Vec::new();
    for ty in 0..h.div_ceil(ts) {
        for tx in 0..w.div_ceil(ts) {
            let mut tile = vec![0f32; ts * ts * 4];
            let vw = (w - tx * ts).min(ts);
            let vh = (h - ty * ts).min(ts);
            for row in 0..vh {
                let s = ((ty * ts + row) * w + tx * ts) * 4;
                tile[row * ts * 4..row * ts * 4 + vw * 4].copy_from_slice(&rgba[s..s + vw * 4]);
            }
            let hash = tiles.insert_bytes(raster::depth32::rgbaf32_to_tile_bytes(&tile));
            edits.push(TileEdit::set(
                raster::TileCoord::new(tx as i32, ty as i32, 0),
                hash,
            ));
        }
    }
    edits
}

/// Image ▸ Image Size for one layer of a 32-bit document: its `f32` pixels
/// resampled in linear light (premultiplied, the same transfer the `f32`
/// filter route uses) and stored as `f32` tiles, nothing clipped but alpha —
/// the `f32` twin of `doc_depth::resample_layer16_edits`.
pub(crate) fn resample_layer_f32_edits(
    document: &Document,
    tiles: &mut MemoryTileSource,
    layer: layer_model::LayerId,
    (dw, dh): (u32, u32),
    filter: raster::ResampleFilter,
) -> Result<Vec<TileEdit>, DocumentError> {
    let (w, h) = (document.width(), document.height());
    let before = layer_rgbaf32_in(document, tiles, layer);
    let pixels: Vec<f32> = to_filter_pixels(&before).into_iter().flatten().collect();
    let image = raster::export::LinearImage::from_premultiplied(w, h, pixels)?;
    let scaled = raster::export::resample(&image, dw, dh, filter)?;
    let px: Vec<[f32; 4]> = scaled
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect();
    let after = from_filter_pixels(&px);
    Ok(f32_tile_edits(tiles, (dw as usize, dh as usize), &after))
}

/// Straight `f32` RGBA (document encoding) as the filter buffer's linear,
/// premultiplied pixels — the same transfer `FilterBuffer::from_rgba16`
/// applies, with nothing clipped but alpha.
fn to_filter_pixels(rgba: &[f32]) -> Vec<[f32; 4]> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            color::premultiply([
                color::srgb_to_linear(p[0]),
                color::srgb_to_linear(p[1]),
                color::srgb_to_linear(p[2]),
                p[3].clamp(0.0, 1.0),
            ])
        })
        .collect()
}

/// The inverse of [`to_filter_pixels`].
fn from_filter_pixels(px: &[[f32; 4]]) -> Vec<f32> {
    let mut out = Vec::with_capacity(px.len() * 4);
    for p in px {
        let s = color::unpremultiply(*p);
        out.extend_from_slice(&[
            color::linear_to_srgb(s[0]),
            color::linear_to_srgb(s[1]),
            color::linear_to_srgb(s[2]),
            s[3].clamp(0.0, 1.0),
        ]);
    }
    out
}

/// `menu_bridge::edit_active_pixels` in a 32-bit document: read the layer as
/// `f32`, run `op` on the `f32` filter buffer, fold the result by the
/// selection and write it back as `f32` tiles — one undoable step, never
/// rounded or clipped to a smaller depth.
pub(crate) fn edit_active_pixels_f32(
    editor: &mut Editor,
    layer: layer_model::LayerId,
    (w, h): (u32, u32),
    label: &str,
    op: impl FnOnce(&mut filters::FilterBuffer, &color::ColorSpace) -> Result<(), String>,
) -> Result<(), String> {
    let (selection, space, before) = {
        let doc = editor.active().ok_or("No document is open")?;
        (
            doc.document.selection.clone(),
            doc.document.meta.color_space.clone(),
            layer_rgbaf32(doc, layer),
        )
    };
    let mut buffer = filters::FilterBuffer::from_pixels(w, h, to_filter_pixels(&before))
        .map_err(|e| e.to_string())?;
    op(&mut buffer, &space)?;
    if buffer.dimensions() != (w, h) {
        return Err(format!(
            "{label} changed the image from {w}x{h} to {:?}, and this build \
             cannot resize a layer",
            buffer.dimensions()
        ));
    }
    let mut after = from_filter_pixels(buffer.pixels());
    crate::menu_bridge::pixels::mask_by_selection(&before, &mut after, &selection, w, h);
    if after == before {
        return Err(format!("{label} changed nothing"));
    }
    let doc = editor.active_mut().ok_or("No document is open")?;
    let command = layer_rgbaf32_command(doc, layer, &after, label)?;
    editor.apply_command(command);
    Ok(())
}

/// Whether `tool` writes every pixel of its output from that same pixel of
/// the layer (plus paint), so a pixel whose stroked result equals its old
/// value clipped is one the stroke left alone: the brushes, erasers, fills,
/// gradient and toning tools. Tools that carry pixels from elsewhere
/// (Clone/Healing/Patch, Smudge/Blur/Sharpen, Mixer, History Brush, Move,
/// Free Transform, Content-Aware Move, the crops) are not.
pub(crate) fn in_place_tool(tool: tools::ToolId) -> bool {
    use tools::ToolId as T;
    matches!(
        tool,
        T::Brush
            | T::Pencil
            | T::ColorReplacement
            | T::Eraser
            | T::BackgroundEraser
            | T::MagicEraser
            | T::Gradient
            | T::PaintBucket
            | T::PatternFill
            | T::Dodge
            | T::Burn
            | T::Sponge
            | T::RedEye
    )
}

thread_local! {
    /// Set while a paint stroke of an [`in_place_tool`] is being applied:
    /// the only time [`fit_to_f32_document`] keeps a clipped old value.
    static IN_PLACE_STROKE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` (the application of a tool's commands) with the in-place marker
/// set to `in_place`, restoring the previous marker afterwards.
pub(crate) fn with_in_place_stroke<R>(in_place: bool, f: impl FnOnce() -> R) -> R {
    let previous = IN_PLACE_STROKE.with(|c| c.replace(in_place));
    let out = f();
    IN_PLACE_STROKE.with(|c| c.set(previous));
    out
}

/// The apply boundary of a 32-bit document: an RGBA8 or RGBA16 tile a
/// layer `PaintTiles` names (a tool's output, an 8/16-bit whole-layer edit)
/// is landed as `f32` before the command reaches history
/// ([`raster::depth32::widen_over_rgbaf32`]; see the module notes for which
/// old `f32` values survive). A depth conversion passes through untouched.
pub(crate) fn fit_to_f32_document(
    document: &Document,
    tiles: &mut MemoryTileSource,
    command: Command,
) -> Command {
    match command {
        Command::PaintTiles {
            target: PixelTarget::Layer(layer),
            delta,
        } => {
            let mut changed = false;
            let keep_clipped = IN_PLACE_STROKE.with(std::cell::Cell::get);
            let edits: Vec<TileEdit> = delta
                .iter()
                .map(|edit| {
                    let Some(hash) = edit.hash else {
                        return *edit;
                    };
                    let old = document
                        .layer_tiles(layer)
                        .and_then(|m| m.get(edit.coord))
                        .and_then(|h| tiles.tile(h));
                    let Some(float) = tiles.tile(hash).and_then(|new| {
                        raster::depth32::widen_over_rgbaf32(new, old, keep_clipped)
                    }) else {
                        return *edit;
                    };
                    changed = true;
                    TileEdit::set(edit.coord, tiles.insert_bytes(float))
                })
                .collect();
            let target = PixelTarget::Layer(layer);
            if !changed {
                return Command::PaintTiles { target, delta };
            }
            Command::paint_tiles(target, edits).unwrap_or(Command::PaintTiles { target, delta })
        }
        Command::Transaction { label, commands }
            if !commands
                .iter()
                .any(|c| matches!(c, Command::SetMetaBitDepth { .. })) =>
        {
            Command::Transaction {
                label,
                commands: commands
                    .into_iter()
                    .map(|c| fit_to_f32_document(document, tiles, c))
                    .collect(),
            }
        }
        other => other,
    }
}

/// File ▸ Export of a 32-bit document to TIFF: a 32-bit floating-point TIFF
/// of the composite in linear light (the compositor's linear sRGB working
/// space), straight alpha, nothing clipped — HDR values above 1.0 reach the
/// file. `Ok(false)` for any other document or format, which then takes the
/// 8/16-bit route. Called by `OpenDocument::export_to` and the File ▸
/// Export worker (`jobs::run_file_export`).
pub(crate) fn write_float_tiff(
    path: &std::path::Path,
    format: raster::ExportFormat,
    document: &Document,
    canvas: impl FnOnce() -> Result<compositor::Canvas, DocumentError>,
) -> Result<bool, DocumentError> {
    // W11-H: and to `.exr`, a 32-bit float OpenEXR of the same linear
    // composite (premultiplied, as OpenEXR defines alpha).
    let exr = format == raster::ExportFormat::Exr;
    if document.meta.bit_depth != 32 || !(exr || format == raster::ExportFormat::Tiff) {
        return Ok(false);
    }
    let canvas = canvas()?;
    let samples: Vec<f32> = canvas.to_straight().into_iter().flatten().collect();
    let (w, h) = (document.width(), document.height());
    let bytes = if exr {
        raster::codec::formats::float::encode_exr_linear(w, h, &samples)?
    } else {
        raster::depth32::encode_tiff_rgbaf32(w, h, &samples)?
    };
    crate::doc::write_atomically(path, &bytes).map_err(crate::import::ImportError::from)?;
    Ok(true)
}

#[cfg(test)]
#[path = "depth32_tests.rs"]
mod tests;
