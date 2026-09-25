//! W13X-8: a Sketch, Adobe XD or Figma file opened as **layers**, the way
//! Photopea opens them: artboards as artboards, groups as groups, vector
//! shapes as shape layers (path, fill, stroke), text as text layers (string,
//! font, size, colour) and bitmaps as raster layers.
//!
//! `raster::codec::formats::vector_docs::design_files` reads the file into a
//! format-neutral tree; this module maps that tree onto the document model
//! ([`document_from_design`]) and is the open route
//! ([`Editor::open_design_document`]), which [`Editor::open_resource_file`]
//! asks before the preview route, so File > Open (the picker), a drop,
//! File > Open Recent and the command line all reach it. File > Revert
//! rebuilds the layers through [`Editor::open_design_layered`].
//!
//! # Placement
//!
//! The canvas is the bounding box of everything on the opened page (for a
//! file of artboards: the artboards), moved so its top-left is `(0, 0)`.
//! Shapes and text keep their whole placement (translation, rotation, scale)
//! as the layer's transform, so they stay editable; artboards and bitmaps are
//! pixels and sit at their rounded top-left.
//!
//! # What does not map
//!
//! Everything the reader could not carry over (gradients, effects, symbol
//! instances, extra pages, …) is in [`DesignImport::notes`] and shown as the
//! "<format> import report" when the file opens: nothing is dropped
//! silently. When the layers cannot be read at all (a damaged document, or a
//! file with no pages), the file's embedded preview opens instead and the
//! status line says why.

use std::io::Read;
use std::path::Path;

use compositor::MemoryTileSource;
use editor_core::pixels::{PixelKey, TileDelta, TileEdit};
use editor_core::{Document, History};
use layer_model::text::{BaseStyle, Frame, Slant, Weight};
use layer_model::{
    Artboard, GroupBlending, GroupLayer, Layer, LayerId, LayerKind, RasterLayer, ShapeFillRule,
    ShapeLayer, ShapeStroke, ShapeStrokeAlign, TextLayer,
};
use raster::codec::formats::vector_docs::{
    self,
    design_files::{self, Affine, DesignDocument, DesignKind, DesignNode, StrokeAlign},
};
use raster::{ImportFormat, ImportLimits, TileCoord, TILE_SIZE};

use super::super::{Action, ActionError, DocumentId, Editor, Effect, OpenDocument};
use crate::import::{DecodedImage, ImportedDocument, PsdImport, PsdNotes};

/// A design file mapped onto the document model.
pub struct DesignImport {
    pub imported: ImportedDocument,
    /// What did not map, one sentence each (empty when everything did).
    pub notes: Vec<String>,
    /// What opened, for the status line (for example
    /// `1 artboard and 3 layers from page "Page 1"`).
    pub summary: String,
}

/// `true` for the formats this route reads as layers.
pub fn is_design_format(format: ImportFormat) -> bool {
    matches!(
        format,
        ImportFormat::Sketch | ImportFormat::Xd | ImportFormat::Fig
    )
}

fn to_linear(c: [f32; 4]) -> [f32; 4] {
    [
        color::srgb_to_linear(c[0]),
        color::srgb_to_linear(c[1]),
        color::srgb_to_linear(c[2]),
        c[3],
    ]
}

fn to_bytes(c: [f32; 4]) -> [u8; 4] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn affine2(t: Affine) -> glam::Affine2 {
    glam::Affine2::from_cols_array(&t.0.map(|v| v as f32))
}

/// `rgba` (`w` x `h`) as tiles with its top-left at document pixel `(x, y)`.
fn tiles_at(
    rgba: &[u8],
    (w, h): (u32, u32),
    (x, y): (i64, i64),
    tiles: &mut MemoryTileSource,
) -> Vec<TileEdit> {
    if w == 0 || h == 0 || rgba.len() as u64 != u64::from(w) * u64::from(h) * 4 {
        return Vec::new();
    }
    let ts = i64::from(TILE_SIZE);
    let stride = TILE_SIZE as usize * 4;
    let (x1, y1) = (x + i64::from(w), y + i64::from(h));
    let mut out = Vec::new();
    for ty in y.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
        for tx in x.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
            let (ox, oy) = (tx * ts, ty * ts);
            let (cx0, cx1) = (x.max(ox), x1.min(ox + ts));
            let (cy0, cy1) = (y.max(oy), y1.min(oy + ts));
            let mut data = vec![0u8; stride * TILE_SIZE as usize];
            for py in cy0..cy1 {
                let src = (((py - y) * i64::from(w) + (cx0 - x)) as usize) * 4;
                let dst = ((py - oy) as usize) * stride + ((cx0 - ox) as usize) * 4;
                let n = ((cx1 - cx0) as usize) * 4;
                data[dst..dst + n].copy_from_slice(&rgba[src..src + n]);
            }
            if data.iter().all(|b| *b == 0) {
                continue;
            }
            let (Ok(tx), Ok(ty)) = (i32::try_from(tx), i32::try_from(ty)) else {
                continue;
            };
            let hash = tiles.insert_bytes(data);
            out.push(TileEdit::set(TileCoord::new(tx, ty, 0), hash));
        }
    }
    out
}

struct Build {
    document: Document,
    tiles: MemoryTileSource,
    /// Document space of the design to canvas space.
    shift: Affine,
    boards: usize,
    layers: usize,
    /// The first leaf layer created (the top-most, as layers are made
    /// bottom first and each new one goes on top).
    top_leaf: Option<LayerId>,
}

impl Build {
    fn paint(
        &mut self,
        id: LayerId,
        rgba: &[u8],
        size: (u32, u32),
        at: (i64, i64),
    ) -> Result<(), String> {
        let edits = tiles_at(rgba, size, at, &mut self.tiles);
        if !edits.is_empty() {
            let delta = TileDelta::new(edits).map_err(|e| e.to_string())?;
            self.document.pixels.apply(PixelKey::Layer(id), &delta);
        }
        Ok(())
    }

    /// `node` (and its subtree) as the top-most child of `parent`.
    fn node(&mut self, node: &DesignNode, parent: Option<LayerId>) -> Result<(), String> {
        let placed = self.shift.then(node.transform);
        let origin = (
            placed.0[4].round().clamp(-1e9, 1e9) as i64,
            placed.0[5].round().clamp(-1e9, 1e9) as i64,
        );
        let mut layer = match &node.kind {
            DesignKind::Artboard { .. } => Layer::group(node.name.clone()),
            DesignKind::Group { .. } => Layer::with_kind(
                node.name.clone(),
                LayerKind::Group(GroupLayer {
                    children: Vec::new(),
                    collapsed: false,
                    blending: GroupBlending::PassThrough,
                }),
            ),
            DesignKind::Shape {
                path_svg,
                fill,
                stroke,
                even_odd,
            } => {
                let mut l = Layer::with_kind(
                    node.name.clone(),
                    LayerKind::Shape(ShapeLayer {
                        path_svg: path_svg.clone(),
                        fill: *fill,
                        fill_rule: if *even_odd {
                            ShapeFillRule::EvenOdd
                        } else {
                            ShapeFillRule::NonZero
                        },
                        stroke: stroke.as_ref().map(|s| ShapeStroke {
                            color: s.color,
                            width_px: s.width,
                            align: match s.align {
                                StrokeAlign::Center => ShapeStrokeAlign::Center,
                                StrokeAlign::Inside => ShapeStrokeAlign::Inside,
                                StrokeAlign::Outside => ShapeStrokeAlign::Outside,
                            },
                            ..ShapeStroke::default()
                        }),
                        ..ShapeLayer::default()
                    }),
                );
                l.transform = affine2(placed);
                l
            }
            DesignKind::Text {
                text,
                font_family,
                bold,
                italic,
                size,
                color,
                box_width,
            } => {
                let mut l = Layer::with_kind(
                    node.name.clone(),
                    LayerKind::Text(TextLayer {
                        text: text.clone(),
                        font_family: font_family.clone(),
                        size_px: *size,
                        style: BaseStyle {
                            weight: if *bold { Weight::BOLD } else { Weight::NORMAL },
                            slant: if *italic {
                                Slant::Italic
                            } else {
                                Slant::Normal
                            },
                            fill: to_linear(*color),
                            ..BaseStyle::default()
                        },
                        frame: match box_width {
                            Some(w) if *w > 0.0 => Frame::Box {
                                width: *w,
                                height: None,
                            },
                            _ => Frame::Point,
                        },
                        ..TextLayer::default()
                    }),
                );
                l.transform = affine2(placed);
                l
            }
            DesignKind::Bitmap { .. } => Layer::raster(node.name.clone()),
        };
        layer.visible = node.visible;
        layer.opacity = node.opacity.clamp(0.0, 1.0);
        let id = self
            .document
            .layers
            .insert_at(layer, parent, 0)
            .map_err(|e| e.to_string())?;
        match &node.kind {
            DesignKind::Artboard {
                background,
                children,
            } => {
                self.boards += 1;
                let (w, h) = (
                    (node.width.round() as u32).max(1),
                    (node.height.round() as u32).max(1),
                );
                let bg = background.unwrap_or([0.0; 4]);
                let plate = Layer::with_kind(
                    "Artboard Background",
                    LayerKind::Raster(RasterLayer {
                        artboard: Some(Artboard {
                            x: origin.0,
                            y: origin.1,
                            width: w,
                            height: h,
                            background: to_linear(bg),
                        }),
                        ..RasterLayer::default()
                    }),
                );
                let plate = self
                    .document
                    .layers
                    .insert_at(plate, Some(id), 0)
                    .map_err(|e| e.to_string())?;
                let px = to_bytes(bg);
                let rgba: Vec<u8> = (0..u64::from(w) * u64::from(h)).flat_map(|_| px).collect();
                self.paint(plate, &rgba, (w, h), origin)?;
                for child in children {
                    self.node(child, Some(id))?;
                }
            }
            DesignKind::Group { children } => {
                for child in children {
                    self.node(child, Some(id))?;
                }
            }
            DesignKind::Bitmap {
                width,
                height,
                rgba,
            } => {
                self.layers += 1;
                self.paint(id, rgba, (*width, *height), origin)?;
                self.top_leaf = Some(id);
            }
            DesignKind::Shape { .. } | DesignKind::Text { .. } => {
                self.layers += 1;
                self.top_leaf = Some(id);
            }
        }
        Ok(())
    }
}

/// `design` as a layered document titled `title` (see the module docs).
/// Opening is not an edit: the history is empty and the document clean.
pub fn document_from_design(
    design: &DesignDocument,
    title: &str,
    history_depth: usize,
) -> Result<DesignImport, String> {
    // The canvas: everything drawn on the page (groups have no box of their
    // own; their contents count).
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for node in design.walk() {
        if matches!(node.kind, DesignKind::Group { .. }) {
            continue;
        }
        for (x, y) in node.corners() {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 > x1 || y0 > y1 {
        return Err(format!(
            "the {} file's {} holds no layers",
            design.format.name(),
            design.opened
        ));
    }
    let (ox, oy) = (x0.floor(), y0.floor());
    let (w, h) = (
        ((x1.ceil() - ox).max(1.0)).min(f64::from(u32::MAX)) as u32,
        ((y1.ceil() - oy).max(1.0)).min(f64::from(u32::MAX)) as u32,
    );
    let limits = ImportLimits::default();
    if !editor_core::canvas_size_is_supported(w, h)
        || w > limits.max_width
        || h > limits.max_height
        || u64::from(w) * u64::from(h) > limits.max_pixels
    {
        return Err(format!(
            "the {} file's layers need a {w}x{h} canvas, past the import limit",
            design.format.name()
        ));
    }
    let mut build = Build {
        document: Document::new(w, h, title),
        tiles: MemoryTileSource::new(),
        shift: Affine::translate(-ox, -oy),
        boards: 0,
        layers: 0,
        top_leaf: None,
    };
    for node in &design.nodes {
        build.node(node, None)?;
    }
    let Build {
        mut document,
        tiles,
        boards,
        layers,
        top_leaf,
        ..
    } = build;
    let active = match top_leaf.or_else(|| document.layers.iter_depth_first().first().copied()) {
        Some(id) => id,
        None => return Err("the file holds no layers".into()),
    };
    document
        .set_active_layer(Some(active))
        .map_err(|e| e.to_string())?;
    document.mark_saved();
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let summary = if boards > 0 {
        format!(
            "its layers: {} and {} from {}",
            plural(boards, "artboard", "artboards"),
            plural(layers, "layer", "layers"),
            design.opened
        )
    } else {
        format!(
            "its layers: {} from {}",
            plural(layers, "layer", "layers"),
            design.opened
        )
    };
    Ok(DesignImport {
        imported: ImportedDocument {
            document,
            history: History::with_limit(history_depth),
            tiles,
            layer: active,
        },
        notes: design.notes.clone(),
        summary,
    })
}

/// The design import report: what did not map.
pub fn report(format: ImportFormat, notes: &[String], source: &Path) -> Option<String> {
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
    out.push_str(&format!(
        "\n\nThe original file {} was not modified.",
        source.display()
    ));
    Some(out)
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

/// Read `path` (a Sketch / XD / Figma file) as layers.
fn read_layered(
    format: ImportFormat,
    bytes: &[u8],
    path: &Path,
    history_depth: usize,
) -> Result<DesignImport, String> {
    let design = design_files::read_design(format, bytes, ImportLimits::default())
        .map_err(|e| e.to_string())?;
    document_from_design(&design, &DecodedImage::title_for(path), history_depth)
}

impl Editor {
    /// W13X-8: File > Revert of a design file rebuilds its layers; `None`
    /// for any other file (or one whose layers cannot be read: it reverts to
    /// its preview, as it opened).
    pub(crate) fn open_design_layered(
        id: DocumentId,
        path: &Path,
        history_depth: usize,
    ) -> Result<Option<OpenDocument>, String> {
        let Some(format) = super::open_pages::w13d_format(path).filter(|f| is_design_format(*f))
        else {
            return Ok(None);
        };
        let bytes = read_limited(path, ImportLimits::default())?;
        Ok(read_layered(format, &bytes, path, history_depth)
            .ok()
            .map(|import| {
                OpenDocument::open_psd_import(
                    id,
                    path,
                    PsdImport {
                        imported: import.imported,
                        notes: PsdNotes::default(),
                        merged_preview: None,
                    },
                )
            }))
    }

    /// W13X-8: open `path` as layers when it is a Sketch, XD or Figma file;
    /// `None` for anything else. When the layers cannot be read the
    /// embedded preview opens and the status line says why; when there is
    /// no preview either, the open fails with both reasons.
    pub(crate) fn open_design_document(
        &mut self,
        path: &Path,
    ) -> Option<Result<Effect, ActionError>> {
        let format = super::open_pages::w13d_format(path).filter(|f| is_design_format(*f))?;
        let failed =
            |e: String| ActionError::failed(Action::Open, format!("{}: {e}", path.display()));
        let limits = ImportLimits::default();
        let depth = self.prefs.history_depth;
        let bytes = match read_limited(path, limits) {
            Ok(bytes) => bytes,
            Err(e) => return Some(Err(failed(e))),
        };
        let id = self.mint_id();
        match read_layered(format, &bytes, path, depth) {
            Ok(import) => {
                let doc = OpenDocument::open_psd_import(
                    id,
                    path,
                    PsdImport {
                        imported: import.imported,
                        notes: PsdNotes::default(),
                        merged_preview: None,
                    },
                );
                self.install_opened(doc, path);
                let mut status = format!("Opened {}: {}", path.display(), import.summary);
                if let Some(text) = report(format, &import.notes, path) {
                    status.push_str(&format!(
                        " ({} not mapped exactly; see the import report)",
                        if import.notes.len() == 1 {
                            "1 thing".to_string()
                        } else {
                            format!("{} things", import.notes.len())
                        }
                    ));
                    self.dialogs
                        .report_notice(&format!("{} import report", format.name()), &text);
                }
                self.status = Some(status);
                self.touch();
                Some(Ok(Effect::DocumentSet))
            }
            Err(why) => {
                let opened = vector_docs::decode_described(format, &bytes, limits)
                    .map_err(|e| e.to_string())
                    .and_then(|(surface, note)| {
                        let image = DecodedImage {
                            width: surface.width,
                            height: surface.height,
                            color_space: surface.color_space,
                            icc_profile: surface.icc_profile,
                            rgba8: surface.pixels.into_rgba8(),
                        };
                        OpenDocument::open_image_decoded(id, path, image, depth)
                            .map(|doc| (doc, note))
                            .map_err(|e| e.to_string())
                    });
                Some(match opened {
                    Ok((doc, note)) => {
                        self.install_opened(doc, path);
                        self.status = Some(format!(
                            "Opened {}: {note}; its layers could not be read ({why})",
                            path.display()
                        ));
                        self.touch();
                        Ok(Effect::DocumentSet)
                    }
                    Err(preview) => Err(failed(format!(
                        "its layers could not be read ({why}), and {preview}"
                    ))),
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "import_design_tests.rs"]
mod tests;
