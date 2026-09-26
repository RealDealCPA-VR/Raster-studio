//! W16-L: File > Open of a Krita `.kra` and an AutoCAD `.dxf` opens their
//! **layers**, as Photopea does.
//!
//! A child of `resource_import_w16` (declared there with `#[path]`), so the
//! routing table every open route shares (`Editor::open_resource_file`:
//! the File > Open picker, a drop, File > Open Recent, the command line)
//! reaches it through `Editor::open_resource` before the flat import job.
//!
//! | File | Opens as |
//! |---|---|
//! | `.kra` (by content: a ZIP whose `mimetype` names Krita) | one raster layer per paint layer and one group per group layer, from `maindoc.xml` and the layers' tiled LZF pixels (`raster::codec::formats::kra::layers`), with each layer's name, opacity, visibility and blend mode. A layer kind the reader leaves out (filter, fill, vector, clone, file layers; masks; CMYK or Lab paint layers) is listed in the "Krita import report". When the layers cannot be read at all, the merged image opens as one picture and the status line says why |
//! | `.dxf` (by content) | one group per DXF layer holding a shape (or text) layer per entity, on the drawing's fitted canvas, over a white `Background` shape (`formats::more_formats_w16::dxf::layers`, through the SVG layer reader and the W16-I vector mapping). When the layers cannot be read, the drawing opens as one picture and the status line says why |
//!
//! File > Revert of either file reopens it as the flat image the import job
//! decodes: the Revert route (`Editor::open_pages_document`) is outside this
//! module.

use std::io::Read as _;
use std::path::Path;

use compositor::MemoryTileSource;
use editor_core::pixels::{PixelKey, TileDelta, TileEdit};
use editor_core::{Document, History};
use layer_model::{BlendMode, GroupBlending, GroupLayer, Layer, LayerId, LayerKind};
use raster::codec::formats::kra::{self, layers::KraDocument, layers::KraLayer};
use raster::codec::formats::more_formats_w16::dxf;
use raster::{ImportFormat, ImportLimits, TileGrid};

use super::super::super::{Action, ActionError, DocumentId, Editor, Effect, OpenDocument};
use crate::import::{DecodedImage, ImportedDocument, PsdImport, PsdNotes};

/// Which of this route's formats `path` holds, by content (and extension:
/// a DXF is plain text, so a `.dxf` name is asked for too).
pub fn layered_format(path: &Path) -> Option<ImportFormat> {
    let mut head = Vec::with_capacity(256);
    std::fs::File::open(path)
        .ok()?
        .take(256)
        .read_to_end(&mut head)
        .ok()?;
    if kra::looks_like_kra(&head) {
        return Some(ImportFormat::Kra);
    }
    let named_dxf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dxf"));
    (named_dxf && dxf::looks_like_dxf(&head)).then_some(ImportFormat::Dxf)
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

/// The document model's blend mode for a Krita composite-op id, or `None`
/// when there is no equivalent.
pub fn krita_blend(id: &str) -> Option<BlendMode> {
    Some(match id {
        "normal" => BlendMode::Normal,
        "dissolve" => BlendMode::Dissolve,
        "darken" => BlendMode::Darken,
        "multiply" => BlendMode::Multiply,
        "burn" => BlendMode::ColorBurn,
        "linear_burn" => BlendMode::LinearBurn,
        "darker color" => BlendMode::DarkerColor,
        "lighten" => BlendMode::Lighten,
        "screen" => BlendMode::Screen,
        "dodge" => BlendMode::ColorDodge,
        "linear_dodge" | "add" => BlendMode::LinearDodge,
        "lighter color" => BlendMode::LighterColor,
        "overlay" => BlendMode::Overlay,
        "soft_light" => BlendMode::SoftLight,
        "hard_light" => BlendMode::HardLight,
        "vivid_light" => BlendMode::VividLight,
        "linear_light" => BlendMode::LinearLight,
        "pin_light" => BlendMode::PinLight,
        "hard_mix" | "hard_mix_photoshop" => BlendMode::HardMix,
        "diff" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "subtract" => BlendMode::Subtract,
        "divide" => BlendMode::Divide,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminize" => BlendMode::Luminosity,
        _ => return None,
    })
}

struct Build {
    width: u32,
    height: u32,
    document: Document,
    tiles: MemoryTileSource,
    notes: Vec<String>,
    /// The first paint layer met (the topmost), made active.
    first_paint: Option<LayerId>,
}

impl Build {
    /// Insert `items` (topmost first, as Krita lists them) under `parent`.
    fn items(&mut self, items: &[KraLayer], parent: Option<LayerId>) -> Result<(), String> {
        for (index, item) in items.iter().enumerate() {
            let mut layer = if item.is_group {
                Layer::with_kind(
                    item.name.clone(),
                    LayerKind::Group(GroupLayer {
                        children: Vec::new(),
                        collapsed: false,
                        blending: GroupBlending::Isolated,
                    }),
                )
            } else {
                Layer::raster(item.name.clone())
            };
            layer.visible = item.visible;
            layer.opacity = item.opacity.clamp(0.0, 1.0);
            layer.blend_mode = krita_blend(&item.blend).unwrap_or_else(|| {
                self.notes.push(format!(
                    "layer {:?} uses Krita's \"{}\" blend mode, which has no equivalent here; \
                     it opened as Normal",
                    item.name, item.blend
                ));
                BlendMode::Normal
            });
            let id = self
                .document
                .layers
                .insert_at(layer, parent, index)
                .map_err(|e| e.to_string())?;
            if item.is_group {
                self.items(&item.children, Some(id))?;
                continue;
            }
            self.first_paint.get_or_insert(id);
            self.paint(id, item)?;
        }
        Ok(())
    }

    /// `item`'s pixels onto layer `id`, clipped to the canvas.
    fn paint(&mut self, id: LayerId, item: &KraLayer) -> Result<(), String> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (lw, lh) = (item.width as usize, item.height as usize);
        if lw == 0 || lh == 0 || item.rgba.len() < lw * lh * 4 {
            return Ok(());
        }
        let mut canvas = vec![0u8; w * h * 4];
        let mut any = false;
        for y in 0..lh {
            // Saturating: the reader bounds offsets, but this must not
            // overflow on any `KraLayer` it is handed.
            let cy = item.y.saturating_add(y as i64);
            if cy < 0 || cy >= h as i64 {
                continue;
            }
            // The run of this row that lands on the canvas.
            let x0 = item.x.saturating_neg().clamp(0, lw as i64) as usize;
            let x1 = (w as i64).saturating_sub(item.x).clamp(0, lw as i64) as usize;
            if x0 >= x1 {
                continue;
            }
            let src = (y * lw + x0) * 4..(y * lw + x1) * 4;
            let dst_x = item.x.saturating_add(x0 as i64) as usize;
            let dst = (cy as usize * w + dst_x) * 4;
            canvas[dst..dst + src.len()].copy_from_slice(&item.rgba[src]);
            any = true;
        }
        if !any {
            return Ok(());
        }
        let grid =
            TileGrid::from_rgba8(self.width, self.height, &canvas).map_err(|e| e.to_string())?;
        let edits: Vec<TileEdit> = grid
            .iter()
            .filter(|(_, tile)| tile.data().iter().any(|b| *b != 0))
            .map(|(coord, tile)| {
                TileEdit::set(coord, self.tiles.insert_bytes(tile.data().to_vec()))
            })
            .collect();
        if !edits.is_empty() {
            let delta = TileDelta::new(edits).map_err(|e| e.to_string())?;
            self.document.pixels.apply(PixelKey::Layer(id), &delta);
        }
        Ok(())
    }
}

/// A `.kra`'s layers as a document, and what did not map (the reader's
/// notes, then the blend modes that opened as Normal).
pub fn document_from_kra(
    kra: &KraDocument,
    title: &str,
    history_depth: usize,
) -> Result<(ImportedDocument, Vec<String>), String> {
    let (width, height) = (kra.width, kra.height);
    if !editor_core::canvas_size_is_supported(width, height) {
        return Err(format!(
            "a {width}x{height} canvas is not one this build can open"
        ));
    }
    let mut build = Build {
        width,
        height,
        document: Document::new(width, height, title),
        tiles: MemoryTileSource::new(),
        notes: kra.notes.clone(),
        first_paint: None,
    };
    build.items(&kra.layers, None)?;
    let Build {
        mut document,
        tiles,
        notes,
        first_paint,
        ..
    } = build;
    let active = match first_paint.or_else(|| document.layers.iter_depth_first().first().copied()) {
        Some(id) => id,
        None => document
            .layers
            .insert_at(Layer::raster("Background"), None, 0)
            .map_err(|e| e.to_string())?,
    };
    document
        .set_active_layer(Some(active))
        .map_err(|e| e.to_string())?;
    // Opening a file is not an edit.
    document.mark_saved();
    Ok((
        ImportedDocument {
            document,
            history: History::with_limit(history_depth),
            tiles,
            layer: active,
        },
        notes,
    ))
}

/// How many layers and groups a tree holds.
fn count(layers: &[KraLayer]) -> (usize, usize) {
    layers.iter().fold((0, 0), |(l, g), item| {
        if item.is_group {
            let (cl, cg) = count(&item.children);
            (l + cl, g + 1 + cg)
        } else {
            (l + 1, g)
        }
    })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `path`'s layers as the document `id`, the status summary and the notes.
fn layered(
    format: ImportFormat,
    id: DocumentId,
    path: &Path,
    depth: usize,
) -> Result<(OpenDocument, String, Vec<String>), String> {
    let limits = ImportLimits::default();
    let bytes = read_limited(path, limits)?;
    let title = DecodedImage::title_for(path);
    let (imported, summary, notes) = if format == ImportFormat::Kra {
        let kra = kra::layers::read(&bytes, limits).map_err(|e| e.to_string())?;
        let (layers, groups) = count(&kra.layers);
        let (imported, notes) = document_from_kra(&kra, &title, depth)?;
        let summary = if groups == 0 {
            plural(layers, "layer", "layers")
        } else {
            format!(
                "{} in {}",
                plural(layers, "layer", "layers"),
                plural(groups, "group", "groups")
            )
        };
        (imported, summary, notes)
    } else {
        let vector = dxf::layers(&bytes, limits).map_err(|e| e.to_string())?;
        let import = crate::editor::open_any::open_pages::vector_w16::document_from_vector(
            &vector, &title, depth,
        )?;
        (import.imported, import.summary, import.notes)
    };
    let doc = OpenDocument::open_psd_import(
        id,
        path,
        PsdImport {
            imported,
            notes: PsdNotes::default(),
            merged_preview: None,
        },
    );
    Ok((doc, summary, notes))
}

/// The import report's text, or `None` when everything mapped.
fn report(format: ImportFormat, notes: &[String], path: &Path) -> Option<String> {
    if notes.is_empty() {
        return None;
    }
    let mut out = format!(
        "Some parts of this {} file did not map exactly:\n",
        format.name()
    );
    for note in notes {
        out.push_str("\n- ");
        out.push_str(note);
    }
    out.push_str(&format!("\n\n{}", path.display()));
    Some(out)
}

impl Editor {
    /// Open `path` as its layers when it is a `.kra` or a `.dxf`; `None`
    /// for any other file.
    pub(crate) fn open_layered_w16(&mut self, path: &Path) -> Option<Result<Effect, ActionError>> {
        let format = layered_format(path)?;
        let depth = self.prefs.history_depth;
        let id = self.mint_id();
        let why = match layered(format, id, path, depth) {
            Ok((doc, summary, notes)) => {
                self.install_opened(doc, path);
                let mut status = format!("Opened {}: {summary}", path.display());
                if let Some(text) = report(format, &notes, path) {
                    status.push_str(&format!(
                        " ({} not mapped exactly; see the import report)",
                        plural(notes.len(), "thing", "things")
                    ));
                    self.dialogs
                        .report_notice(&format!("{} import report", format.name()), &text);
                }
                self.status = Some(status);
                self.touch();
                return Some(Ok(Effect::DocumentSet));
            }
            Err(why) => why,
        };
        // The layers could not be read: the flat image (a `.kra`'s merged
        // image, a DXF drawn as one picture), saying why.
        let failed =
            |e: String| ActionError::failed(Action::Open, format!("{}: {e}", path.display()));
        let limits = ImportLimits::default();
        let opened = read_limited(path, limits).and_then(|bytes| {
            let surface = raster::decode_surface_bytes_as(&bytes, limits, format)
                .map_err(|e| e.to_string())?;
            let image = DecodedImage {
                width: surface.width,
                height: surface.height,
                color_space: surface.color_space,
                icc_profile: surface.icc_profile,
                rgba8: surface.pixels.into_rgba8(),
            };
            let id = self.mint_id();
            OpenDocument::open_image_decoded(id, path, image, depth).map_err(|e| e.to_string())
        });
        Some(match opened {
            Ok(doc) => {
                self.install_opened(doc, path);
                self.status = Some(format!(
                    "Opened {}: the image as one picture; its layers could not be read ({why})",
                    path.display()
                ));
                self.touch();
                Ok(Effect::DocumentSet)
            }
            Err(e) => Err(failed(format!(
                "its layers could not be read ({why}), and {e}"
            ))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint_layer(name: &str, x: i64, y: i64) -> KraLayer {
        KraLayer {
            name: name.into(),
            x,
            y,
            width: 4,
            height: 4,
            opacity: 1.0,
            visible: true,
            blend: "normal".into(),
            is_group: false,
            children: Vec::new(),
            rgba: vec![255; 4 * 4 * 4],
        }
    }

    /// `Build::paint` is handed whatever `KraLayer` it gets: offsets at the
    /// i64 edges clip away instead of overflowing, and an in-range layer
    /// still paints.
    #[test]
    fn a_kra_layer_at_the_i64_edges_clips_away_without_overflow() {
        let kra = KraDocument {
            width: 8,
            height: 8,
            layers: vec![
                paint_layer("min x", i64::MIN, 0),
                paint_layer("max x", i64::MAX, 0),
                paint_layer("min y", 0, i64::MIN),
                paint_layer("max y", 0, i64::MAX),
                paint_layer("in range", -2, 3),
            ],
            notes: Vec::new(),
        };
        let (imported, _) = document_from_kra(&kra, "edges", 10).unwrap();
        let painted: Vec<String> = imported
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| {
                imported
                    .document
                    .pixels
                    .tiles(PixelKey::Layer(*id))
                    .is_some_and(|t| !t.is_empty())
            })
            .filter_map(|id| imported.document.layers.get(id).map(|l| l.name.clone()))
            .collect();
        assert_eq!(painted, ["in range"]);
    }
}
