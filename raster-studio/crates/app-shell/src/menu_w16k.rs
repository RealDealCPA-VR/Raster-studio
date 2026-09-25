//! W16-K: the menu rows, chords and options-bar buttons the final parity
//! audit found missing, performed here for `menu_bridge::perform`, and the
//! vector File > Export As writers' scene.
//!
//! * View > Mode > Fullscreen / Standard / Menu Bar and Canvas
//!   ([`set_screen_mode`]): the screen mode `F` steps through, set directly.
//! * Layer > New > Artboard and the Artboard bar's + buttons
//!   ([`add_artboard`]): an empty artboard - the canvas when there is none
//!   yet, else one the size of the active (else the top-most) artboard, set
//!   [`ARTBOARD_GAP`] pixels to its side, with its background.
//! * The Crop bar's Crop by > Current Layer ([`crop_to_layer`]).
//! * File > Export As > PDF / EMF / DXF ([`vector_doc`],
//!   [`write_vector_export`]): the layer tree as vector paths where a layer
//!   is a shape or text layer SVG-like stacking can say, and as that one
//!   layer rendered alone otherwise; one page per artboard (in reading
//!   order), else the canvas. A page whose stack uses a blend mode, a mask,
//!   a clipping run or an adjustment layer is one image of its composite.

use std::path::Path;

use compositor::MemoryTileSource;
use editor_core::{Command, Document, PixelKey, PixelTarget};
use layer_model::{BlendMode, ClippingMode, Layer, LayerId, LayerKind};
use raster::codec::export_vector::{self as ev, Item, Page, Paint, Seg, VectorDoc, VectorPath};
use raster::PixelRect;
use ui::menu::{ArtboardSide, ScreenModeItem};

use crate::action::Action;
use crate::doc::DocumentError;
use crate::editor::Editor;

/// The space a new neighbouring artboard keeps from the one it is added
/// beside, in document pixels.
pub const ARTBOARD_GAP: i64 = 100;

// ---------------------------------------------------------------------------
// View > Mode
// ---------------------------------------------------------------------------

/// View > Mode > `item`: step the screen mode (the one `F` cycles) until the
/// window is in the row's mode.
pub(crate) fn set_screen_mode(editor: &mut Editor, item: ScreenModeItem) -> Result<String, String> {
    let want = item.mode();
    for _ in 0..ui::palette::ScreenMode::ALL.len() {
        if editor.screen_mode() == want {
            break;
        }
        editor
            .dispatch(Action::CycleScreenMode)
            .map_err(|e| e.to_string())?;
    }
    if editor.screen_mode() != want {
        return Err(format!("Screen mode: could not reach {}", item.label()));
    }
    Ok(format!("Screen mode: {}", item.label()))
}

// ---------------------------------------------------------------------------
// Artboards
// ---------------------------------------------------------------------------

/// The artboard `id` is, or lies inside.
fn artboard_around(doc: &Document, id: LayerId) -> Option<(LayerId, layer_model::Artboard)> {
    let mut cursor = Some(id);
    while let Some(at) = cursor {
        if let Some((_, board)) = layer_model::artboard::artboard_of(&doc.layers, at) {
            return Some((at, board));
        }
        cursor = doc.layers.parent_of(at);
    }
    None
}

/// A [`tools::tiles::TileAccess`] over a document's references and its byte
/// store, for painting a new artboard's plate.
struct PlateTiles<'a> {
    doc: &'a Document,
    tiles: &'a mut MemoryTileSource,
}

impl tools::tiles::TileAccess for PlateTiles<'_> {
    fn tile_hash(&self, key: PixelKey, coord: raster::TileCoord) -> Option<raster::TileHash> {
        self.doc.pixels.tiles(key).and_then(|m| m.get(coord))
    }

    fn bytes(&self, hash: raster::TileHash) -> Option<&[u8]> {
        compositor::TileSource::tile(&*self.tiles, hash)
    }

    fn store(&mut self, data: Vec<u8>) -> raster::TileHash {
        self.tiles.insert_bytes(data)
    }
}

/// Layer > New > Artboard (`side` `None`) or an Artboard bar + button: add
/// an empty artboard as one undo step and make it the active layer.
pub(crate) fn add_artboard(
    editor: &mut Editor,
    side: Option<ArtboardSide>,
) -> Result<String, String> {
    let (command, group_id, name) = {
        let open = editor.active_mut().ok_or("No document is open")?;
        let doc = &open.document;
        let boards = layer_model::artboard::artboards(&doc.layers);
        let reference = doc
            .active_layer()
            .and_then(|id| artboard_around(doc, id))
            .or_else(|| boards.first().copied());
        let (rect, background) = match (reference, side) {
            (None, Some(_)) => return Err("There is no artboard to add one beside".to_string()),
            // The first artboard is the canvas, white, as Photopea's.
            (None, None) => (
                PixelRect::new(0, 0, doc.width(), doc.height()),
                [1.0, 1.0, 1.0, 1.0],
            ),
            (Some((_, b)), side) => {
                let (w, h) = (i64::from(b.width), i64::from(b.height));
                let (x, y) = match side.unwrap_or(ArtboardSide::Right) {
                    ArtboardSide::Right => (b.x + w + ARTBOARD_GAP, b.y),
                    ArtboardSide::Left => (b.x - w - ARTBOARD_GAP, b.y),
                    ArtboardSide::Below => (b.x, b.y + h + ARTBOARD_GAP),
                    ArtboardSide::Above => (b.x, b.y - h - ARTBOARD_GAP),
                };
                (PixelRect::new(x, y, b.width, b.height), b.background)
            }
        };
        let name = format!("Artboard {}", boards.len() + 1);
        let group = Layer::group(name.clone());
        let plate = Layer::with_kind(
            "Artboard Background",
            LayerKind::Raster(layer_model::RasterLayer {
                artboard: Some(layer_model::Artboard {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                    background,
                }),
                ..layer_model::RasterLayer::default()
            }),
        );
        let (group_id, plate_id) = (group.id, plate.id);
        let mut commands = vec![
            Command::create_layer(group),
            Command::MoveLayer {
                layer_id: group_id,
                parent: None,
                index: 0,
            },
            Command::create_layer(plate),
            Command::MoveLayer {
                layer_id: plate_id,
                parent: Some(group_id),
                index: 0,
            },
        ];
        // The plate's pixels are its colour over its rect (the Artboard
        // tool's rule), so the artboard shows and exports through the
        // ordinary raster path.
        if background[3] > 0.0 {
            let key = PixelKey::Layer(plate_id);
            let mut access = PlateTiles {
                doc: &open.document,
                tiles: &mut open.tiles,
            };
            let mut patch =
                tools::ColorPatch::load(&access, key, rect).map_err(|e| e.to_string())?;
            let a = background[3].clamp(0.0, 1.0);
            let px = [background[0] * a, background[1] * a, background[2] * a, a];
            for y in rect.y..rect.bottom() {
                for x in rect.x..rect.right() {
                    patch.set(glam::IVec2::new(x as i32, y as i32), px);
                }
            }
            let delta = patch.commit(&mut access, key).map_err(|e| e.to_string())?;
            if !delta.is_empty() {
                commands.push(Command::PaintTiles {
                    target: PixelTarget::Layer(plate_id),
                    delta,
                });
            }
        }
        (
            Command::Transaction {
                label: "Artboard".to_string(),
                commands,
            },
            group_id,
            name,
        )
    };
    let before = editor.active().map(|d| d.history_depth());
    editor.apply_command(command);
    if editor.active().map(|d| d.history_depth()) == before {
        return Err("The new artboard was refused".to_string());
    }
    editor.set_layer_selection(vec![group_id], Some(group_id));
    Ok(format!("Added {name}"))
}

/// The Crop bar's Crop by > Current Layer: the canvas cropped to the active
/// layer's ink bounds.
pub(crate) fn crop_to_layer(editor: &mut Editor) -> Result<String, String> {
    let bounds = {
        let open = editor.active().ok_or("No document is open")?;
        let id = open
            .document
            .active_layer()
            .ok_or("There is no active layer")?;
        crate::tool_input::tight_document_bounds(&open.document, &open.tiles, id)
            .filter(|r| r.width > 0 && r.height > 0)
            .ok_or("The active layer has no pixels to crop to")?
    };
    editor.resize_canvas(
        bounds.width,
        bounds.height,
        glam::IVec2::new(bounds.x as i32, bounds.y as i32),
    )?;
    Ok(format!(
        "Cropped to the current layer ({} x {})",
        bounds.width, bounds.height
    ))
}

// ---------------------------------------------------------------------------
// File > Export As > PDF / EMF / DXF
// ---------------------------------------------------------------------------

/// Whether the visible stack under `ids` can be said with source-over
/// paths and images (see the module docs).
fn stackable(doc: &Document, ids: &[LayerId]) -> bool {
    ids.iter().all(|id| {
        let Some(l) = doc.layers.get(*id) else {
            return true;
        };
        if !l.visible {
            return true;
        }
        let plain = l.mask.is_none() && l.clipping == ClippingMode::None;
        match &l.kind {
            LayerKind::Group(g) => {
                plain
                    && l.effects.is_empty()
                    && (l.blend_mode == BlendMode::Normal
                        || g.blending == layer_model::GroupBlending::PassThrough)
                    && stackable(doc, &g.children)
            }
            LayerKind::Adjustment(_) => false,
            _ => plain && l.blend_mode == BlendMode::Normal,
        }
    })
}

fn to_pt(t: glam::Affine2, p: vector::Point, origin: (i64, i64)) -> [f64; 2] {
    let q = t.transform_point2(glam::Vec2::new(p.x as f32, p.y as f32));
    [
        f64::from(q.x) - origin.0 as f64,
        f64::from(q.y) - origin.1 as f64,
    ]
}

/// SVG path data in a layer's space as page segments.
fn segments(d: &str, t: glam::Affine2, origin: (i64, i64)) -> Option<Vec<Seg>> {
    let path = vector::svg::parse(d).ok()?;
    let mut out = Vec::new();
    let mut last = vector::point(0.0, 0.0);
    for el in path.elements() {
        match *el {
            vector::PathEl::MoveTo(p) => {
                out.push(Seg::Move(to_pt(t, p, origin)));
                last = p;
            }
            vector::PathEl::LineTo(p) => {
                out.push(Seg::Line(to_pt(t, p, origin)));
                last = p;
            }
            vector::PathEl::QuadTo(c, p) => {
                // A quadratic is the cubic with its controls two thirds of
                // the way to the quadratic's.
                let c1 = vector::point(
                    last.x + (c.x - last.x) * 2.0 / 3.0,
                    last.y + (c.y - last.y) * 2.0 / 3.0,
                );
                let c2 =
                    vector::point(p.x + (c.x - p.x) * 2.0 / 3.0, p.y + (c.y - p.y) * 2.0 / 3.0);
                out.push(Seg::Cubic(
                    to_pt(t, c1, origin),
                    to_pt(t, c2, origin),
                    to_pt(t, p, origin),
                ));
                last = p;
            }
            vector::PathEl::CurveTo(a, b, p) => {
                out.push(Seg::Cubic(
                    to_pt(t, a, origin),
                    to_pt(t, b, origin),
                    to_pt(t, p, origin),
                ));
                last = p;
            }
            vector::PathEl::ClosePath => out.push(Seg::Close),
        }
    }
    (!out.is_empty()).then_some(out)
}

fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Encoded straight RGBA (a shape layer's paint) as a PDF paint.
fn encoded_paint(c: [f32; 4], alpha: f32) -> Paint {
    Paint {
        rgb: [byte(c[0]), byte(c[1]), byte(c[2])],
        alpha: (c[3] * alpha).clamp(0.0, 1.0),
    }
}

/// Linear straight RGBA (a text fill, an artboard background) as a paint.
fn linear_paint(space: &color::ColorSpace, c: [f32; 4], alpha: f32) -> Paint {
    let e = color::from_linear(space, [c[0], c[1], c[2]].map(|v| v.clamp(0.0, 1.0)));
    encoded_paint([e[0], e[1], e[2], c[3]], alpha)
}

/// The vector path a layer is, or `None` when it must go out as an image.
fn vector_item(doc: &Document, id: LayerId, alpha: f32, origin: (i64, i64)) -> Option<VectorPath> {
    let layer = doc.layers.get(id)?;
    if !layer.effects.is_empty() {
        return None;
    }
    let t = crate::interaction_geometry::document_transform_of(doc, id, 0).ok()?;
    let alpha = alpha * layer.opacity * layer.effective_fill_opacity();
    match &layer.kind {
        LayerKind::Shape(shape) => {
            if !shape.fill_paint.is_solid() {
                return None;
            }
            let stroke = match &shape.stroke {
                Some(s) if s.align != layer_model::ShapeStrokeAlign::Center => return None,
                Some(s) if !s.dash.is_empty() => return None,
                Some(s) => Some(ev::Stroke {
                    paint: encoded_paint(s.color, alpha),
                    width: f64::from(s.width_px.max(0.0)),
                }),
                None => None,
            };
            Some(VectorPath {
                segs: segments(&shape.path_svg, t, origin)?,
                fill: shape.fill.map(|f| encoded_paint(f, alpha)),
                even_odd: shape.fill_rule == layer_model::ShapeFillRule::EvenOdd,
                stroke,
            })
        }
        LayerKind::Text(text) => {
            // One fill for every glyph: style runs go out as an image.
            if !text.spans.is_empty() || text.text.trim().is_empty() {
                return None;
            }
            let run = text_engine::TextRun::from(text);
            let d = text_engine::with_shared_library(|library| {
                let shaped = text_engine::shape(library, &run);
                text_engine::outline_svg(library, &shaped)
            });
            Some(VectorPath {
                segs: segments(&d, t, origin)?,
                fill: Some(linear_paint(&doc.meta.color_space, text.style.fill, alpha)),
                even_odd: false,
                stroke: None,
            })
        }
        _ => None,
    }
}

/// `unit` rendered alone over `page` (its ancestors kept, every other layer
/// hidden), cropped to its ink.
fn layer_image(
    doc: &Document,
    tiles: &MemoryTileSource,
    unit: LayerId,
    page: PixelRect,
) -> Result<Option<ev::PlacedImage>, DocumentError> {
    let mut staged = doc.clone();
    let mut keep = std::collections::HashSet::new();
    let mut cursor = Some(unit);
    while let Some(id) = cursor {
        keep.insert(id);
        cursor = staged.layers.parent_of(id);
    }
    let mut stack = vec![unit];
    while let Some(id) = stack.pop() {
        keep.insert(id);
        if let Some(l) = staged.layers.get(id) {
            stack.extend(l.children().iter().copied());
        }
    }
    for id in staged.layers.iter_depth_first() {
        if !keep.contains(&id) {
            if let Some(l) = staged.layers.get_mut(id) {
                l.visible = false;
            }
        }
    }
    image_of(&staged, tiles, page)
}

fn image_of(
    doc: &Document,
    tiles: &MemoryTileSource,
    page: PixelRect,
) -> Result<Option<ev::PlacedImage>, DocumentError> {
    let canvas =
        compositor::composite_region(doc, tiles, page, 0, compositor::CompositeOptions::default())?;
    let rgba = canvas.to_rgba8(&doc.meta.color_space);
    let (w, h) = (page.width as usize, page.height as usize);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if rgba[(y * w + x) * 4 + 3] > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 >= x1 {
        return Ok(None);
    }
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        out.extend_from_slice(&rgba[(y * w + x0) * 4..(y * w + x1) * 4]);
    }
    Ok(Some(ev::PlacedImage {
        x: x0 as i64,
        y: y0 as i64,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
        rgba: out,
    }))
}

/// Bottom to top over `ids` (stored top-most first), each visible layer as
/// a path or an image, `alpha` the enclosing groups' opacity.
fn walk(
    doc: &Document,
    tiles: &MemoryTileSource,
    ids: &[LayerId],
    alpha: f32,
    page: PixelRect,
    items: &mut Vec<Item>,
) -> Result<(), DocumentError> {
    let origin = (page.x, page.y);
    for id in ids.iter().rev() {
        let Some(layer) = doc.layers.get(*id) else {
            continue;
        };
        if !layer.visible {
            continue;
        }
        match &layer.kind {
            LayerKind::Group(g) => {
                let inner = alpha * layer.opacity * layer.effective_fill_opacity();
                walk(doc, tiles, &g.children, inner, page, items)?;
            }
            // An artboard's plate is its background rectangle.
            LayerKind::Raster(r) if r.artboard.is_some() => {
                let board = r.artboard.expect("checked");
                if board.background[3] > 0.0 {
                    let (x, y) = ((board.x - page.x) as f64, (board.y - page.y) as f64);
                    let (w, h) = (f64::from(board.width), f64::from(board.height));
                    items.push(Item::Path(VectorPath {
                        segs: vec![
                            Seg::Move([x, y]),
                            Seg::Line([x + w, y]),
                            Seg::Line([x + w, y + h]),
                            Seg::Line([x, y + h]),
                            Seg::Close,
                        ],
                        fill: Some(linear_paint(
                            &doc.meta.color_space,
                            board.background,
                            alpha * layer.opacity,
                        )),
                        even_odd: false,
                        stroke: None,
                    }));
                }
            }
            _ => match vector_item(doc, *id, alpha, origin) {
                Some(path) => items.push(Item::Path(path)),
                None => {
                    if let Some(img) = layer_image(doc, tiles, *id, page)? {
                        items.push(Item::Image(img));
                    }
                }
            },
        }
    }
    Ok(())
}

/// One page per artboard (in reading order: top to bottom, then left to
/// right), else the canvas.
pub fn vector_doc(doc: &Document, tiles: &MemoryTileSource) -> Result<VectorDoc, DocumentError> {
    let mut boards = layer_model::artboard::artboards(&doc.layers);
    boards.sort_by_key(|(_, b)| (b.y, b.x));
    let mut units: Vec<(PixelRect, Vec<LayerId>)> = boards
        .iter()
        .filter(|(id, _)| doc.layers.get(*id).is_some_and(|l| l.visible))
        .map(|(id, b)| (PixelRect::new(b.x, b.y, b.width, b.height), vec![*id]))
        .collect();
    if units.is_empty() {
        units.push((
            PixelRect::new(0, 0, doc.width(), doc.height()),
            doc.layers.root().to_vec(),
        ));
    }
    let mut pages = Vec::new();
    for (rect, ids) in units {
        let mut items = Vec::new();
        if stackable(doc, &ids) {
            walk(doc, tiles, &ids, 1.0, rect, &mut items)?;
        } else if let Some(img) = image_of(doc, tiles, rect)? {
            items.push(Item::Image(img));
        }
        pages.push(Page {
            width: rect.width,
            height: rect.height,
            items,
        });
    }
    Ok(VectorDoc {
        pages,
        title: doc.meta.title.clone(),
    })
}

/// Write `doc` as `format` (PDF, EMF or DXF) at `path`, atomically.
pub fn write_vector_export(
    doc: &Document,
    tiles: &MemoryTileSource,
    format: raster::ExportFormat,
    path: &Path,
) -> Result<VectorDoc, DocumentError> {
    let scene = vector_doc(doc, tiles)?;
    let bytes = match format {
        raster::ExportFormat::Pdf => ev::encode_pdf(&scene)?,
        raster::ExportFormat::Emf => ev::encode_emf(&scene)?,
        raster::ExportFormat::Dxf => ev::encode_dxf(&scene)?,
        other => {
            return Err(DocumentError::UnknownExportFormat(
                other.extension().to_string(),
            ))
        }
    };
    crate::doc::write_atomically(path, &bytes).map_err(crate::import::ImportError::from)?;
    Ok(scene)
}
