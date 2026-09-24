//! W9-M: the live (editable) PSD mappings — vector shape layers, placed smart
//! objects, pattern overlays and 16-bit documents — on both sides of the
//! exchange.
//!
//! A child module of [`crate::import`] so it shares that module's tile
//! helpers; the `psd` crate owns the byte layouts ([`psd::shape`],
//! [`psd::placed`], [`psd::pattern`]) and this file only maps them onto the
//! document model.
//!
//! * **Shapes.** A [`ShapeLayer`] is written as a real shape layer: its path
//!   (through the layer transform, so the file's path is in canvas pixels) in
//!   `vmsk`, its fill as `SoCo`, its outline as `vstk`, and a `vogk` live
//!   rectangle when the path is an axis-aligned rectangle. The rendered
//!   appearance still rides in the channels. On the way in a layer with a path
//!   and a solid fill becomes a shape layer again, path in canvas pixels and
//!   an identity transform. What a `.psd` shape cannot say — a translucent
//!   fill (a `.psd` fill has no alpha), a stroke under a non-uniform transform,
//!   a path that does not parse — keeps the card-078 raster fallback, named.
//! * **Smart objects.** An embedded, unfiltered [`SmartObjectLayer`] is
//!   written as `SoLd` naming its asset, with the asset's own bytes in the
//!   document's `lnk2` block and the transform as the four canvas corners of
//!   the source rectangle. On the way in `SoLd`/`PlLd` plus a matching
//!   `lnk2`/`lnk3`/`lnkD` entry becomes a smart object whose source is the
//!   decoded file and whose transform maps it onto those corners. Linked
//!   objects (whose bytes live elsewhere) and filtered ones (whose filters a
//!   `.psd` cannot carry) keep the raster fallback.
//! * **16 bits.** A 16-bit document is written as a 16-bit `.psd` (raster
//!   layers at their stored 16-bit samples; rendered fallbacks and masks
//!   widened exactly from 8 bits) and a 16-bit `.psd` opens as a 16-bit
//!   document whose layer tiles keep every sample.

use std::collections::HashMap;

use super::*;
use layer_model::{
    AssetId, AssetOrigin, AssetRecord, ShapeCap, ShapeFillPaint, ShapeFillRule, ShapeJoin,
    ShapeLayer, ShapeStroke, ShapeStrokeAlign, SmartObjectLayer,
};
use psd::placed::{LinkedFile, LinkedFiles, PlacedLayer};
use psd::shape::{Knot, LineAlign, LineCap, LineJoin, ShapeData, StrokeStyle, SubPath, VectorPath};

/// What a document export collects beside its layer records.
#[derive(Debug, Default)]
pub(super) struct PsdExportExtras {
    /// Embedded smart-object sources, for the document's `lnk2` block.
    pub linked: Vec<LinkedFile>,
    /// Patterns the layers' pattern overlays name, for the `Patt` block.
    pub patterns: Vec<psd::pattern::PsdPattern>,
    /// The document is 16-bit and is written as a 16-bit `.psd`.
    pub deep: bool,
}

impl PsdExportExtras {
    /// Put the collected document-level blocks into `file`.
    pub fn finish(self, file: &mut psd::PsdFile) {
        if !self.linked.is_empty() {
            file.extra.push(psd::TaggedBlock::new(
                *b"lnk2",
                psd::placed::encode_linked_files(&self.linked),
            ));
        }
        if !self.patterns.is_empty() {
            file.extra.push(psd::TaggedBlock::new(
                *b"Patt",
                psd::pattern::encode_block(&self.patterns),
            ));
        }
    }
}

/// An affine applied in `f64`, so a path's coordinates are not rounded to
/// `f32` on the way out.
fn apply(t: glam::Affine2, x: f64, y: f64) -> [f64; 2] {
    let (a, b, o) = (t.matrix2.x_axis, t.matrix2.y_axis, t.translation);
    [
        f64::from(a.x) * x + f64::from(b.x) * y + f64::from(o.x),
        f64::from(a.y) * x + f64::from(b.y) * y + f64::from(o.y),
    ]
}

/// The uniform scale of `t`'s linear part, or `None` when it stretches or
/// shears (a stroke width then has no single value in canvas pixels).
fn uniform_scale(t: glam::Affine2) -> Option<f64> {
    let (a, b) = (t.matrix2.x_axis, t.matrix2.y_axis);
    let (la, lb) = (f64::from(a.length()), f64::from(b.length()));
    let dot = f64::from(a.dot(b));
    ((la - lb).abs() <= 1e-4 * la.max(1.0) && dot.abs() <= 1e-4 * la.max(1.0) * lb.max(1.0))
        .then_some(la)
}

fn unit_to_255(c: f32) -> f64 {
    f64::from(c) * 255.0
}

/// A shape layer's `.psd` blocks: the fill-layer payload (`SoCo`, `GdFl` or
/// `PtFl`) for the adjustment slot and the tagged blocks for the record's
/// extras. A pattern fill's pattern is added to `extras`. `None` when the
/// shape cannot be expressed exactly (see the module docs) — the caller then
/// keeps the raster fallback and names it.
pub(super) fn shape_blocks(
    shape: &ShapeLayer,
    transform: glam::Affine2,
    width: u32,
    height: u32,
    extras: &mut PsdExportExtras,
) -> Option<(psd::Adjustment, Vec<psd::TaggedBlock>)> {
    if !transform.is_finite() {
        return None;
    }
    let translation_only = transform.matrix2 == glam::Mat2::IDENTITY;
    // A gradient is fitted to the shape's own bounds, which a translation
    // moves but does not reshape; a pattern is anchored in canvas space.
    let paint = match &shape.fill_paint {
        ShapeFillPaint::Solid => None,
        ShapeFillPaint::Gradient(g) if translation_only && shape.fill.is_some() => {
            Some(psd::fill::encode_gradient_fill(g))
        }
        ShapeFillPaint::Pattern(p)
            if transform == glam::Affine2::IDENTITY && shape.fill.is_some() =>
        {
            let (adjustment, pattern) = psd::fill::encode_pattern_fill(p)?;
            if !extras.patterns.iter().any(|known| known.id == pattern.id) {
                extras.patterns.push(pattern);
            }
            Some(adjustment)
        }
        _ => return None,
    };
    if shape
        .fill
        .is_some_and(|f| (f[3] - 1.0).abs() > 1e-4 || f.iter().any(|c| !c.is_finite()))
    {
        return None;
    }
    let stroke = match &shape.stroke {
        Some(s) => {
            let scale = uniform_scale(transform)?;
            if !(s.width_px.is_finite() && s.width_px > 0.0) {
                return None;
            }
            let width_px = f64::from(s.width_px) * scale;
            let per_width = |v: f32| f64::from(v) * scale / width_px;
            Some(StrokeStyle {
                stroke_enabled: true,
                fill_enabled: shape.fill.is_some(),
                width_px,
                color: [
                    unit_to_255(s.color[0]),
                    unit_to_255(s.color[1]),
                    unit_to_255(s.color[2]),
                ],
                opacity: f64::from(s.color[3]).clamp(0.0, 1.0),
                cap: match s.cap {
                    ShapeCap::Butt => LineCap::Butt,
                    ShapeCap::Round => LineCap::Round,
                    ShapeCap::Square => LineCap::Square,
                },
                join: match s.join {
                    ShapeJoin::Miter => LineJoin::Miter,
                    ShapeJoin::Round => LineJoin::Round,
                    ShapeJoin::Bevel => LineJoin::Bevel,
                },
                align: match s.align {
                    ShapeStrokeAlign::Inside => LineAlign::Inside,
                    ShapeStrokeAlign::Center => LineAlign::Center,
                    ShapeStrokeAlign::Outside => LineAlign::Outside,
                },
                miter_limit: f64::from(s.miter_limit),
                dash: s.dash.iter().map(|d| per_width(*d)).collect(),
                dash_offset: per_width(s.dash_offset),
            })
        }
        None => None,
    };

    let operation = match shape.fill_rule {
        ShapeFillRule::EvenOdd => 0,
        ShapeFillRule::NonZero => 1,
    };
    let mut subpaths = Vec::new();
    for (closed, local) in svg_path::knots(&shape.path_svg)? {
        if local.is_empty() {
            continue;
        }
        let at = |p: [f64; 2]| apply(transform, p[0], p[1]);
        let knots: Vec<Knot> = local
            .iter()
            .map(|k| Knot {
                before: at(k.before),
                anchor: at(k.anchor),
                after: at(k.after),
                linked: k.linked,
            })
            .collect();
        if knots.iter().any(|k| {
            [k.before, k.anchor, k.after]
                .iter()
                .flatten()
                .any(|v| !v.is_finite())
        }) {
            return None;
        }
        subpaths.push(SubPath {
            closed,
            operation,
            knots,
        });
    }
    if subpaths.is_empty() {
        return None;
    }
    let rect = axis_aligned_rect(&subpaths);
    let data = ShapeData {
        path: VectorPath {
            subpaths,
            ..VectorPath::default()
        },
        fill: shape
            .fill
            .map(|f| [unit_to_255(f[0]), unit_to_255(f[1]), unit_to_255(f[2])]),
        stroke,
    };
    let (soco, mut extra) = data.blocks(width, height);
    if let Some([l, t, r, b]) = rect {
        extra.push(psd::TaggedBlock::new(
            *b"vogk",
            psd::shape::encode_rect_origination(l, t, r, b),
        ));
    }
    let fill = paint.unwrap_or(psd::Adjustment {
        key: *b"SoCo",
        data: soco,
    });
    Some((fill, extra))
}

/// `[left, top, right, bottom]` when the path is one closed, handle-free,
/// four-corner, axis-aligned rectangle — the one live shape this writer
/// records an origination for.
fn axis_aligned_rect(subpaths: &[SubPath]) -> Option<[f64; 4]> {
    let [sp] = subpaths else { return None };
    if !sp.closed || sp.knots.len() != 4 {
        return None;
    }
    if sp
        .knots
        .iter()
        .any(|k| k.before != k.anchor || k.after != k.anchor)
    {
        return None;
    }
    let xs: Vec<f64> = sp.knots.iter().map(|k| k.anchor[0]).collect();
    let ys: Vec<f64> = sp.knots.iter().map(|k| k.anchor[1]).collect();
    let (l, r) = (
        xs.iter().cloned().fold(f64::MAX, f64::min),
        xs.iter().cloned().fold(f64::MIN, f64::max),
    );
    let (t, b) = (
        ys.iter().cloned().fold(f64::MAX, f64::min),
        ys.iter().cloned().fold(f64::MIN, f64::max),
    );
    let on_edge = |v: f64, a: f64, c: f64| (v - a).abs() < 1e-6 || (v - c).abs() < 1e-6;
    let corners = sp
        .knots
        .iter()
        .all(|k| on_edge(k.anchor[0], l, r) && on_edge(k.anchor[1], t, b));
    // Consecutive corners must share an x or a y — a rectangle, not a bow tie.
    let sides = (0..4).all(|i| {
        let (p, q) = (sp.knots[i].anchor, sp.knots[(i + 1) % 4].anchor);
        (p[0] - q[0]).abs() < 1e-6 || (p[1] - q[1]).abs() < 1e-6
    });
    (corners && sides && r > l && b > t).then_some([l, t, r, b])
}

/// The shape layer a `.psd` layer record describes, in canvas pixels.
pub(super) fn shape_from_psd(
    source: &psd::PsdLayer,
    width: u32,
    height: u32,
    patterns: &psd::pattern::PatternLibrary,
) -> Option<ShapeLayer> {
    let opts = psd::ReadOptions::default();
    let data = ShapeData::of(source, width, height, &opts)?;
    // A gradient or pattern fill layer paints the interior; a solid one is
    // the flat `fill` colour.
    let fill_paint = match source.adjustment.as_ref() {
        Some(a) if a.key == *b"GdFl" => match psd::fill::fill_source(a, &opts)? {
            layer_model::FillSource::Gradient(g) => ShapeFillPaint::Gradient(g),
            _ => return None,
        },
        Some(a) if a.key == *b"PtFl" => {
            ShapeFillPaint::Pattern(psd::pattern::pattern_fill_layer(a, &opts, patterns)?)
        }
        _ => ShapeFillPaint::Solid,
    };
    let path_svg = svg_path::from_knots(&data.path.subpaths)?;
    let fill_rule = if data.path.subpaths.first().is_some_and(|s| s.operation == 0) {
        ShapeFillRule::EvenOdd
    } else {
        ShapeFillRule::NonZero
    };
    let style = data.stroke.clone().unwrap_or_default();
    let to_unit = |c: [f64; 3], a: f64| -> layer_model::Rgba {
        [
            (c[0] / 255.0).clamp(0.0, 1.0) as f32,
            (c[1] / 255.0).clamp(0.0, 1.0) as f32,
            (c[2] / 255.0).clamp(0.0, 1.0) as f32,
            a.clamp(0.0, 1.0) as f32,
        ]
    };
    let fill = style.fill_enabled.then(|| {
        if fill_paint.is_solid() {
            data.fill.map(|c| to_unit(c, 1.0))
        } else {
            // The paint decides the colour; `fill` only says "filled".
            Some([0.0, 0.0, 0.0, 1.0])
        }
    });
    let fill = fill.flatten();
    let stroke = (style.stroke_enabled && style.width_px > 0.0).then(|| ShapeStroke {
        color: to_unit(style.color, style.opacity),
        width_px: style.width_px as f32,
        cap: match style.cap {
            LineCap::Butt => ShapeCap::Butt,
            LineCap::Round => ShapeCap::Round,
            LineCap::Square => ShapeCap::Square,
        },
        join: match style.join {
            LineJoin::Miter => ShapeJoin::Miter,
            LineJoin::Round => ShapeJoin::Round,
            LineJoin::Bevel => ShapeJoin::Bevel,
        },
        align: match style.align {
            LineAlign::Inside => ShapeStrokeAlign::Inside,
            LineAlign::Center => ShapeStrokeAlign::Center,
            LineAlign::Outside => ShapeStrokeAlign::Outside,
        },
        miter_limit: style.miter_limit as f32,
        dash: style
            .dash
            .iter()
            .map(|d| (d * style.width_px) as f32)
            .collect(),
        dash_offset: (style.dash_offset * style.width_px) as f32,
    });
    Some(ShapeLayer {
        path_svg,
        fill,
        fill_rule,
        stroke,
        fill_paint,
    })
}

/// A smart object's `SoLd` and `PlLd` blocks, registering its source in
/// `extras`. `None` when the object cannot travel live (see the module docs).
pub(super) fn smart_blocks(
    document: &Document,
    object: &SmartObjectLayer,
    transform: glam::Affine2,
    extras: &mut PsdExportExtras,
) -> Option<[psd::TaggedBlock; 2]> {
    if object.linked || !object.filters.is_empty() || !transform.is_finite() {
        return None;
    }
    let AssetOrigin::Embedded { name, bytes } = document.asset_origin(object.asset)? else {
        return None;
    };
    let (w, h) = match document.asset_source_size(object.asset) {
        Some(size) => size,
        None => {
            let decoded = DecodedImage::decode_bytes(bytes).ok()?;
            (decoded.width, decoded.height)
        }
    };
    if w == 0 || h == 0 {
        return None;
    }
    let id = object.asset.to_string();
    if !extras.linked.iter().any(|f| f.id == id) {
        extras.linked.push(LinkedFile::embedded(
            id.clone(),
            name.clone(),
            bytes.clone(),
        ));
    }
    let (fw, fh) = (f64::from(w), f64::from(h));
    let mut corners = [0.0f64; 8];
    for (i, (x, y)) in [(0.0, 0.0), (fw, 0.0), (fw, fh), (0.0, fh)]
        .into_iter()
        .enumerate()
    {
        let p = apply(transform, x, y);
        corners[i * 2] = p[0];
        corners[i * 2 + 1] = p[1];
    }
    let placed = PlacedLayer {
        id,
        corners,
        size: Some((fw, fh)),
    };
    Some([placed.to_block(), placed.to_legacy_block()])
}

/// A placed layer, ready to insert: its kind, transform, source pixels and
/// the asset row it names.
pub(super) struct PlacedImport {
    pub kind: LayerKind,
    pub transform: glam::Affine2,
    pub source: DecodedImage,
    pub asset: AssetRecord,
}

/// What a `.psd` layer record is, beyond its pixels.
pub(super) enum Live {
    /// Nothing live: map it the ordinary way.
    None,
    Shape(ShapeLayer),
    Smart(Box<PlacedImport>),
    /// A placed layer whose source could not be recovered, and why; its
    /// pixels are imported instead.
    SmartFailed(String),
}

/// Classify `source` (a non-group record) against the document's placed
/// files. `assets` maps a placed-file id onto the asset already made for it,
/// so two layers placing the same file share one source.
pub(super) fn live_kind(
    source: &psd::PsdLayer,
    linked: &LinkedFiles,
    patterns: &psd::pattern::PatternLibrary,
    width: u32,
    height: u32,
    assets: &mut HashMap<String, AssetId>,
) -> Live {
    if PlacedLayer::is_placed(source) {
        return match smart_from_psd(source, linked, assets) {
            Ok(placed) => Live::Smart(Box::new(placed)),
            Err(why) => Live::SmartFailed(why),
        };
    }
    match shape_from_psd(source, width, height, patterns) {
        Some(shape) => Live::Shape(shape),
        None => Live::None,
    }
}

fn smart_from_psd(
    source: &psd::PsdLayer,
    linked: &LinkedFiles,
    assets: &mut HashMap<String, AssetId>,
) -> Result<PlacedImport, String> {
    let placed = PlacedLayer::of(source, &psd::ReadOptions::default())
        .ok_or("its placed-layer block could not be read")?;
    let file = linked
        .find(&placed.id)
        .ok_or("the file it places is not in the document")?;
    if !file.embedded {
        return Err("it links to a file outside the document".into());
    }
    let decoded = DecodedImage::decode_bytes(&file.data)
        .map_err(|e| format!("its source did not decode: {e}"))?;
    let (w, h) = (decoded.width, decoded.height);
    if w == 0 || h == 0 || !editor_core::canvas_size_is_supported(w, h) {
        return Err(format!("its {w}x{h} source is outside the supported size"));
    }
    let c = placed.corners;
    let (fw, fh) = (f64::from(w), f64::from(h));
    let x_axis = [(c[2] - c[0]) / fw, (c[3] - c[1]) / fw];
    let y_axis = [(c[6] - c[0]) / fh, (c[7] - c[1]) / fh];
    let transform = glam::Affine2::from_cols_array(&[
        x_axis[0] as f32,
        x_axis[1] as f32,
        y_axis[0] as f32,
        y_axis[1] as f32,
        c[0] as f32,
        c[1] as f32,
    ]);
    if !transform.is_finite() || transform.matrix2.determinant().abs() < 1e-12 {
        return Err("its placement is degenerate".into());
    }
    let asset = *assets.entry(placed.id.clone()).or_default();
    Ok(PlacedImport {
        kind: LayerKind::SmartObject(SmartObjectLayer {
            asset,
            linked: false,
            filters: Vec::new(),
        }),
        transform,
        asset: AssetRecord {
            id: asset,
            origin: AssetOrigin::Embedded {
                name: file.name.clone(),
                bytes: file.data.clone(),
            },
            source_size: Some((w, h)),
        },
        source: decoded,
    })
}

// ------------------------------------------------------------- 16 bits

/// The layer's stored samples over `area` (document space, the layer's own
/// space offset by `(dx, dy)`), at 16 bits, cropped to its ink, written into
/// `record` as big-endian 16-bit planes. A record with no ink is left as it
/// is (empty).
pub(super) fn set_deep_pixels(
    record: &mut psd::PsdLayer,
    map: &TileMap,
    tiles: &MemoryTileSource,
    area: DocRect,
    dx: i64,
    dy: i64,
) {
    let source = area.offset(-dx, -dy);
    let (w, h) = (source.width() as usize, source.height() as usize);
    if w == 0 || h == 0 {
        return;
    }
    let mut out = vec![0u16; w * h * 4];
    let ts = i64::from(TILE_SIZE);
    let stride = TILE_SIZE as usize * 4;
    for (coord, hash) in map.iter() {
        if coord.level != 0 {
            continue;
        }
        let Some(samples) = tiles.tile(hash).and_then(raster::depth::rgba16_samples) else {
            continue;
        };
        if samples.len() < stride * TILE_SIZE as usize {
            continue;
        }
        let (ox, oy) = coord.pixel_origin();
        let (cx0, cx1) = (source.x0.max(ox), source.x1.min(ox + ts));
        let (cy0, cy1) = (source.y0.max(oy), source.y1.min(oy + ts));
        for y in cy0..cy1 {
            if cx1 <= cx0 {
                break;
            }
            let src = ((y - oy) as usize) * stride + ((cx0 - ox) as usize) * 4;
            let dst = (((y - source.y0) as usize) * w + (cx0 - source.x0) as usize) * 4;
            let n = ((cx1 - cx0) as usize) * 4;
            out[dst..dst + n].copy_from_slice(&samples[src..src + n]);
        }
    }
    // Crop to the ink at 16 bits, so a faint edge is not lost to 8-bit
    // rounding of its alpha.
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if out[(y * w + x) * 4 + 3] != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut planes: Vec<Vec<u8>> = (0..4).map(|_| Vec::with_capacity(cw * ch * 2)).collect();
    for y in y0..y1 {
        for x in x0..x1 {
            let px = &out[(y * w + x) * 4..(y * w + x) * 4 + 4];
            for (c, plane) in planes.iter_mut().enumerate() {
                plane.extend_from_slice(&px[c].to_be_bytes());
            }
        }
    }
    record.bounds = DocRect {
        x0: area.x0 + x0 as i64,
        y0: area.y0 + y0 as i64,
        x1: area.x0 + x1 as i64,
        y1: area.y0 + y1 as i64,
    }
    .to_psd();
    let alpha = planes.pop().unwrap_or_default();
    record.channels = vec![psd::Channel::new(psd::CHANNEL_ALPHA, alpha)];
    for (i, plane) in planes.into_iter().enumerate() {
        record.channels.push(psd::Channel::new(i as i16, plane));
    }
}

/// Widen every 8-bit channel and mask in `layers` (the rendered fallbacks and
/// mask coverage, which are produced at 8 bits) to 16 bits by bit repetition,
/// which is exact. Channels already at 16 bits are left alone. Iterative:
/// the tree can be as deep as the export allows.
pub(super) fn widen_to_sixteen(layers: &mut [psd::PsdLayer]) {
    fn widen(data: &mut Vec<u8>) {
        *data = data
            .iter()
            .flat_map(|b| raster::depth::widen_sample(*b).to_be_bytes())
            .collect();
    }
    let mut stack: Vec<&mut psd::PsdLayer> = layers.iter_mut().collect();
    while let Some(layer) = stack.pop() {
        let n = layer.bounds.width() as usize * layer.bounds.height() as usize;
        for channel in &mut layer.channels {
            if n > 0 && channel.data.len() == n {
                widen(&mut channel.data);
            }
        }
        if let Some(mask) = &mut layer.mask {
            let m = mask.bounds.width() as usize * mask.bounds.height() as usize;
            if m > 0 && mask.data.len() == m {
                widen(&mut mask.data);
            }
        }
        if let psd::LayerKind::Group(group) = &mut layer.kind {
            stack.extend(group.children.iter_mut());
        }
    }
}

/// A 16-bit document's merged composite from its 8-bit one, widened exactly.
pub(super) fn merged_sixteen(width: u32, height: u32, rgba8: &[u8]) -> psd::MergedImage {
    let n = width as usize * height as usize;
    let mut channels: Vec<Vec<u8>> = (0..4).map(|_| Vec::with_capacity(n * 2)).collect();
    for px in rgba8.as_chunks::<4>().0 {
        for (c, plane) in channels.iter_mut().enumerate() {
            plane.extend_from_slice(&raster::depth::widen_sample(px[c]).to_be_bytes());
        }
    }
    psd::MergedImage { channels }
}

/// A 16-bit `.psd` layer's pixels as tile edits that keep every sample: a
/// tile whose samples are all exact 8-bit codes (multiples of 257) is stored
/// as RGBA8 — the same convention a freshly opened 16-bit PNG follows — and
/// any other tile as RGBA16. `None` when the layer has no decodable pixels.
pub(super) fn deep_tile_edits(
    layer: &psd::PsdLayer,
    header: &psd::PsdHeader,
    tiles: &mut MemoryTileSource,
) -> Option<Vec<TileEdit>> {
    if header.depth != psd::Depth::Sixteen {
        return None;
    }
    let (w, h) = (
        layer.bounds.width() as usize,
        layer.bounds.height() as usize,
    );
    if w == 0 || h == 0 {
        return None;
    }
    let n = w.checked_mul(h)?;
    let plane = |id: i16| -> Option<&[u8]> {
        layer
            .channel(id)
            .map(|c| c.data.as_slice())
            .filter(|d| d.len() == n * 2)
    };
    let ids = header.color_mode.channel_ids();
    let color: Vec<&[u8]> = ids.iter().map(|id| plane(*id)).collect::<Option<_>>()?;
    let alpha = match layer.channel(psd::CHANNEL_ALPHA) {
        Some(_) => Some(plane(psd::CHANNEL_ALPHA)?),
        None => None,
    };
    let sample = |p: &[u8], i: usize| u16::from_be_bytes([p[i * 2], p[i * 2 + 1]]);

    let rect = layer.bounds;
    let area = DocRect::from_psd(rect);
    let ts = i64::from(TILE_SIZE);
    let mut out = Vec::new();
    for coord in area.tiles() {
        let (ox, oy) = coord.pixel_origin();
        let (cx0, cx1) = (area.x0.max(ox), area.x1.min(ox + ts));
        let (cy0, cy1) = (area.y0.max(oy), area.y1.min(oy + ts));
        let mut data = vec![0u16; TILE_SIZE as usize * TILE_SIZE as usize * 4];
        for y in cy0..cy1 {
            for x in cx0..cx1 {
                let i = ((y - area.y0) as usize) * w + (x - area.x0) as usize;
                let d = (((y - oy) as usize) * TILE_SIZE as usize + (x - ox) as usize) * 4;
                let rgb = match color.as_slice() {
                    [g] => [sample(g, i); 3],
                    [r, g, b, ..] => [sample(r, i), sample(g, i), sample(b, i)],
                    _ => return None,
                };
                data[d..d + 3].copy_from_slice(&rgb);
                data[d + 3] = alpha.map_or(u16::MAX, |a| sample(a, i));
            }
        }
        if data.iter().all(|v| *v == 0) {
            continue;
        }
        let bytes = if data.iter().all(|v| v % 257 == 0) {
            data.iter().map(|v| (v / 257) as u8).collect()
        } else {
            raster::rgba16_to_tile_bytes(&data)
        };
        let hash = tiles.insert_bytes(bytes);
        out.push(TileEdit::set(coord, hash));
    }
    Some(out)
}

/// SVG path data to and from `.psd` knots.
///
/// A shape's `path_svg` is standard SVG path data (the `vector` crate's
/// `to_svg` writes it); a `.psd` path is anchors with a preceding and a
/// leaving control point. The conversion is exact for lines and cubics, and a
/// quadratic is carried as its exact cubic. Elliptical arcs (`A`) have no
/// knot form and decline, which keeps the shape's raster fallback.
pub(super) mod svg_path {
    use psd::shape::{Knot, SubPath};

    /// One cubic (or straight) segment from the previous end point.
    #[derive(Debug, Clone, Copy)]
    struct Seg {
        c1: [f64; 2],
        c2: [f64; 2],
        end: [f64; 2],
    }

    struct Sub {
        start: [f64; 2],
        segs: Vec<Seg>,
        closed: bool,
    }

    /// The most path commands one shape may carry before it declines.
    const MAX_COMMANDS: usize = 1 << 20;

    fn numbers(d: &str) -> Option<Vec<Result<char, f64>>> {
        let b = d.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let c = b[i] as char;
            if c.is_ascii_whitespace() || c == ',' {
                i += 1;
            } else if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
                out.push(Ok(c));
                i += 1;
            } else {
                let start = i;
                if b[i] == b'+' || b[i] == b'-' {
                    i += 1;
                }
                let mut dot = false;
                while i < b.len() && (b[i].is_ascii_digit() || (b[i] == b'.' && !dot)) {
                    dot |= b[i] == b'.';
                    i += 1;
                }
                if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                    i += 1;
                    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                        i += 1;
                    }
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if i == start {
                    return None;
                }
                let v: f64 = d.get(start..i)?.parse().ok()?;
                if !v.is_finite() {
                    return None;
                }
                out.push(Err(v));
            }
            if out.len() > MAX_COMMANDS {
                return None;
            }
        }
        Some(out)
    }

    fn parse(d: &str) -> Option<Vec<Sub>> {
        let tokens = numbers(d)?;
        let mut subs: Vec<Sub> = Vec::new();
        let mut cur = [0.0f64; 2];
        let mut start = [0.0f64; 2];
        // The last cubic / quadratic control, for S and T reflection.
        let mut last_c2: Option<[f64; 2]> = None;
        let mut last_q: Option<[f64; 2]> = None;
        let mut cmd = ' ';
        let mut i = 0;
        let take = |i: &mut usize| -> Option<f64> {
            match tokens.get(*i)? {
                Err(v) => {
                    *i += 1;
                    Some(*v)
                }
                Ok(_) => None,
            }
        };
        while i < tokens.len() {
            if let Ok(c) = tokens[i] {
                cmd = c;
                i += 1;
                if matches!(c, 'Z' | 'z') {
                    if let Some(sub) = subs.last_mut() {
                        sub.closed = true;
                    }
                    cur = start;
                    last_c2 = None;
                    last_q = None;
                    continue;
                }
            }
            let rel = cmd.is_ascii_lowercase();
            let base = if rel { cur } else { [0.0, 0.0] };
            let pt = |i: &mut usize| -> Option<[f64; 2]> {
                let x = take(i)?;
                let y = take(i)?;
                Some([base[0] + x, base[1] + y])
            };
            let line = |cur: [f64; 2], end: [f64; 2]| Seg {
                c1: cur,
                c2: end,
                end,
            };
            let push = |subs: &mut Vec<Sub>, seg: Seg| -> Option<()> {
                subs.last_mut()?.segs.push(seg);
                Some(())
            };
            match cmd.to_ascii_uppercase() {
                'M' => {
                    let p = pt(&mut i)?;
                    subs.push(Sub {
                        start: p,
                        segs: Vec::new(),
                        closed: false,
                    });
                    cur = p;
                    start = p;
                    // Further pairs after a move are implicit lines.
                    cmd = if rel { 'l' } else { 'L' };
                    last_c2 = None;
                    last_q = None;
                    continue;
                }
                'L' => {
                    let p = pt(&mut i)?;
                    push(&mut subs, line(cur, p))?;
                    cur = p;
                    last_c2 = None;
                    last_q = None;
                }
                'H' => {
                    let x = take(&mut i)? + if rel { cur[0] } else { 0.0 };
                    let p = [x, cur[1]];
                    push(&mut subs, line(cur, p))?;
                    cur = p;
                    last_c2 = None;
                    last_q = None;
                }
                'V' => {
                    let y = take(&mut i)? + if rel { cur[1] } else { 0.0 };
                    let p = [cur[0], y];
                    push(&mut subs, line(cur, p))?;
                    cur = p;
                    last_c2 = None;
                    last_q = None;
                }
                'C' | 'S' => {
                    let c1 = if cmd.eq_ignore_ascii_case(&'C') {
                        pt(&mut i)?
                    } else {
                        last_c2.map_or(cur, |c| [2.0 * cur[0] - c[0], 2.0 * cur[1] - c[1]])
                    };
                    let c2 = pt(&mut i)?;
                    let end = pt(&mut i)?;
                    push(&mut subs, Seg { c1, c2, end })?;
                    cur = end;
                    last_c2 = Some(c2);
                    last_q = None;
                }
                'Q' | 'T' => {
                    let q = if cmd.eq_ignore_ascii_case(&'Q') {
                        pt(&mut i)?
                    } else {
                        last_q.map_or(cur, |c| [2.0 * cur[0] - c[0], 2.0 * cur[1] - c[1]])
                    };
                    let end = pt(&mut i)?;
                    // The exact cubic of a quadratic.
                    let c1 = [
                        cur[0] + 2.0 / 3.0 * (q[0] - cur[0]),
                        cur[1] + 2.0 / 3.0 * (q[1] - cur[1]),
                    ];
                    let c2 = [
                        end[0] + 2.0 / 3.0 * (q[0] - end[0]),
                        end[1] + 2.0 / 3.0 * (q[1] - end[1]),
                    ];
                    push(&mut subs, Seg { c1, c2, end })?;
                    cur = end;
                    last_q = Some(q);
                    last_c2 = None;
                }
                // Arcs and anything unknown have no knot form.
                _ => return None,
            }
        }
        Some(subs)
    }

    fn near(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() <= 1e-9 && (a[1] - b[1]).abs() <= 1e-9
    }

    /// `(closed, knots)` per subpath of SVG path data, in the path's own
    /// space. `None` when the data does not parse or uses an arc.
    pub fn knots(d: &str) -> Option<Vec<(bool, Vec<Knot>)>> {
        let mut out = Vec::new();
        for sub in parse(d)? {
            let mut knots = vec![Knot {
                before: sub.start,
                anchor: sub.start,
                after: sub.start,
                linked: false,
            }];
            let count = sub.segs.len();
            for (k, seg) in sub.segs.iter().enumerate() {
                if let Some(last) = knots.last_mut() {
                    last.after = seg.c1;
                }
                // A closed ring's last segment arriving back at the start
                // shapes the first knot's incoming control instead of
                // adding a duplicate knot on top of it.
                if sub.closed && k + 1 == count && near(seg.end, sub.start) {
                    knots[0].before = seg.c2;
                } else {
                    knots.push(Knot {
                        before: seg.c2,
                        anchor: seg.end,
                        after: seg.end,
                        linked: false,
                    });
                }
            }
            for k in &mut knots {
                let (i, o) = (
                    [k.before[0] - k.anchor[0], k.before[1] - k.anchor[1]],
                    [k.after[0] - k.anchor[0], k.after[1] - k.anchor[1]],
                );
                // Smooth: both handles present and pointing opposite ways.
                let cross = i[0] * o[1] - i[1] * o[0];
                let dot = i[0] * o[0] + i[1] * o[1];
                k.linked = i != [0.0, 0.0] && o != [0.0, 0.0] && cross.abs() < 1e-6 && dot < 0.0;
            }
            out.push((sub.closed, knots));
        }
        Some(out)
    }

    fn num(v: f64) -> String {
        let r = (v * 10_000.0).round() / 10_000.0;
        let r = if r == 0.0 { 0.0 } else { r };
        format!("{r}")
    }

    fn segment(out: &mut String, a: &Knot, b: &Knot) {
        if near(a.after, a.anchor) && near(b.before, b.anchor) {
            out.push_str(&format!(" L {} {}", num(b.anchor[0]), num(b.anchor[1])));
        } else {
            out.push_str(&format!(
                " C {} {} {} {} {} {}",
                num(a.after[0]),
                num(a.after[1]),
                num(b.before[0]),
                num(b.before[1]),
                num(b.anchor[0]),
                num(b.anchor[1])
            ));
        }
    }

    /// SVG path data for `.psd` subpaths. `None` when there is nothing to
    /// draw.
    pub fn from_knots(subpaths: &[SubPath]) -> Option<String> {
        let mut out = String::new();
        for sp in subpaths {
            let Some(first) = sp.knots.first() else {
                continue;
            };
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&format!(
                "M {} {}",
                num(first.anchor[0]),
                num(first.anchor[1])
            ));
            for pair in sp.knots.windows(2) {
                segment(&mut out, &pair[0], &pair[1]);
            }
            if sp.closed {
                if let Some(last) = sp.knots.last() {
                    // A curved closing segment is spelled out; a straight
                    // one is what `Z` draws.
                    if sp.knots.len() > 1
                        && !(near(last.after, last.anchor) && near(first.before, first.anchor))
                    {
                        segment(&mut out, last, first);
                    }
                }
                out.push_str(" Z");
            }
        }
        (!out.is_empty()).then_some(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn relative_lines_curves_and_quads_become_knots() {
            let k = knots("M 8 8 h 24 v 16 h -24 Z").unwrap();
            assert_eq!(k.len(), 1);
            let (closed, ring) = &k[0];
            assert!(closed);
            let anchors: Vec<[f64; 2]> = ring.iter().map(|k| k.anchor).collect();
            assert_eq!(
                anchors,
                [[8.0, 8.0], [32.0, 8.0], [32.0, 24.0], [8.0, 24.0]]
            );

            // A quadratic is its exact cubic; S reflects the last control.
            let k = knots("M0,0 Q 3 0 3 3 S 6 6 6 3").unwrap();
            let ring = &k[0].1;
            assert_eq!(ring[0].after, [2.0, 0.0]);
            assert_eq!(ring[1].anchor, [3.0, 3.0]);
            assert_eq!(ring.len(), 3);
            // Arcs decline.
            assert!(knots("M 0 0 A 5 5 0 0 1 10 0").is_none());
            assert!(knots("M 0 0 L nonsense").is_none());
        }

        #[test]
        fn knots_written_back_as_svg_parse_to_the_same_knots() {
            let d = "M 1 2 C 3 4 5 6 7 8 L 9 10 C 11 12 -1 2 1 2 Z M 20 20 L 30 30";
            let a = knots(d).unwrap();
            let subs: Vec<SubPath> = a
                .iter()
                .map(|(closed, knots)| SubPath {
                    closed: *closed,
                    operation: 1,
                    knots: knots.clone(),
                })
                .collect();
            let back = knots(&from_knots(&subs).unwrap()).unwrap();
            assert_eq!(a.len(), back.len());
            for ((ca, ka), (cb, kb)) in a.iter().zip(&back) {
                assert_eq!(ca, cb);
                assert_eq!(ka.len(), kb.len());
                for (x, y) in ka.iter().zip(kb) {
                    assert_eq!((x.before, x.anchor, x.after), (y.before, y.anchor, y.after));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 96;
    const H: u32 = 64;

    /// 16-bit codes off the 8-bit grid (a multiple of 257 is an exact 8-bit
    /// code), so any trip through 8 bits shows.
    fn busy(x: u32, y: u32) -> [u16; 4] {
        [
            (20_001 + x * 37 + y * 5) as u16,
            (1_003 + y * 601 + x) as u16,
            (65_000 - x * 513 - y * 3) as u16,
            65_535,
        ]
    }

    fn find(doc: &Document, name: &str) -> LayerId {
        doc.layers
            .iter_depth_first()
            .into_iter()
            .find(|id| doc.layers.get(*id).is_some_and(|l| l.name == name))
            .unwrap_or_else(|| panic!("no layer called {name}"))
    }

    fn rendered(doc: &Document, tiles: &MemoryTileSource, id: LayerId) -> Vec<u8> {
        compositor::composite_subtree(
            doc,
            tiles,
            id,
            raster::PixelRect::new(0, 0, W, H),
            0,
            compositor::CompositeOptions::default(),
        )
        .expect("the layer renders")
        .to_rgba8(&doc.meta.color_space)
    }

    /// A 16-bit document holding a 16-bit raster layer, a stroked shape layer
    /// under a translation and an embedded smart object under a scale.
    fn live_document() -> (Document, MemoryTileSource) {
        let mut doc = Document::new(W, H, "live");
        doc.meta.bit_depth = 16;
        let mut tiles = MemoryTileSource::new();

        let deep = doc.layers.push_root(Layer::raster("Deep")).unwrap();
        let ts = TILE_SIZE as usize;
        let mut samples = vec![0u16; ts * ts * 4];
        for y in 0..H {
            for x in 0..W {
                let i = (y as usize * ts + x as usize) * 4;
                samples[i..i + 4].copy_from_slice(&busy(x, y));
            }
        }
        let hash = tiles.insert_bytes(raster::rgba16_to_tile_bytes(&samples));
        let delta = TileDelta::new(vec![TileEdit::set(TileCoord::new(0, 0, 0), hash)]).unwrap();
        doc.pixels.apply(PixelKey::Layer(deep), &delta);

        let badge = doc
            .layers
            .push_root(Layer::with_kind(
                "Badge",
                LayerKind::Shape(ShapeLayer {
                    path_svg: "M 8 8 h 24 v 16 h -24 Z".into(),
                    fill: Some([1.0, 0.0, 0.0, 1.0]),
                    fill_rule: ShapeFillRule::EvenOdd,
                    stroke: Some(ShapeStroke {
                        color: [0.0, 0.0, 1.0, 1.0],
                        width_px: 2.0,
                        join: ShapeJoin::Round,
                        ..ShapeStroke::default()
                    }),
                    ..ShapeLayer::default()
                }),
            ))
            .unwrap();
        doc.layers.get_mut(badge).unwrap().transform =
            glam::Affine2::from_translation(glam::vec2(12.0, 10.0));

        let source: Vec<u8> = (0..64u32)
            .flat_map(|i| [200, (i * 4) as u8, 10, 255])
            .collect();
        let png = raster::encode(raster::ExportFormat::Png, 8, 8, &source).unwrap();
        let asset = AssetId::new();
        doc.set_asset_origin(AssetRecord {
            id: asset,
            origin: AssetOrigin::Embedded {
                name: "logo.png".into(),
                bytes: png,
            },
            source_size: Some((8, 8)),
        });
        let logo = doc
            .layers
            .push_root(Layer::with_kind(
                "Logo",
                LayerKind::SmartObject(SmartObjectLayer {
                    asset,
                    linked: false,
                    filters: Vec::new(),
                }),
            ))
            .unwrap();
        doc.layers.get_mut(logo).unwrap().transform =
            glam::Affine2::from_scale(glam::Vec2::new(2.0, 2.0))
                * glam::Affine2::from_translation(glam::vec2(30.0, 12.0));
        let edits = tile_edits_for_rgba(&source, psd::Rect::sized(8, 8), &mut tiles);
        doc.pixels
            .apply(PixelKey::Layer(logo), &TileDelta::new(edits).unwrap());
        (doc, tiles)
    }

    /// The finding's round trip: a document with a shape layer, a smart
    /// object and 16-bit pixels, saved as a `.psd` and opened again, keeps
    /// every layer's KIND and its PIXELS — and the file itself is a 16-bit
    /// file whose shape and smart object are real shape / placed layers.
    #[test]
    fn shape_smart_object_and_sixteen_bit_layers_survive_a_psd_round_trip() {
        let (doc, tiles) = live_document();
        let composite = vec![0u8; (W * H * 4) as usize];
        let (bytes, notes) = psd_from_document(&doc, &tiles, &composite).unwrap();
        assert!(notes.is_empty(), "nothing falls back: {notes:?}");

        // The file, read by the crate's own reader.
        let file = psd::read(&bytes).unwrap();
        assert_eq!(file.header.depth, psd::Depth::Sixteen);
        let record = |name: &str| file.layers.iter().find(|l| l.name == name).unwrap();
        let badge = record("Badge");
        assert_eq!(badge.adjustment.as_ref().map(|a| a.key), Some(*b"SoCo"));
        assert!(badge.extra.iter().any(|b| b.key == *b"vmsk"));
        assert!(badge.extra.iter().any(|b| b.key == *b"vstk"));
        assert!(
            badge.extra.iter().any(|b| b.key == *b"vogk"),
            "a rectangle is a live rectangle"
        );
        assert!(psd::placed::PlacedLayer::is_placed(record("Logo")));
        assert!(file.extra.iter().any(|b| b.key == *b"lnk2"));

        // ...and opened again.
        let back = document_from_psd(&bytes, "back", 10).unwrap();
        let (bdoc, btiles) = (&back.imported.document, &back.imported.tiles);
        assert_eq!(bdoc.meta.bit_depth, 16);
        assert!(back.notes.is_empty(), "{:?}", back.notes);

        // 16-bit pixels: every sample exactly.
        let deep = find(bdoc, "Deep");
        let map = bdoc.layer_tiles(deep).expect("the deep layer has pixels");
        let hash = map
            .get(TileCoord::new(0, 0, 0))
            .expect("its tile is stored");
        let stored =
            raster::depth::rgba16_samples(compositor::TileSource::tile(btiles, hash).unwrap())
                .unwrap();
        for y in 0..H {
            for x in 0..W {
                let i = (y as usize * TILE_SIZE as usize + x as usize) * 4;
                assert_eq!(stored[i..i + 4], busy(x, y), "sample at ({x}, {y})");
            }
        }

        // The shape: still a shape, the path in canvas pixels, same paint.
        let LayerKind::Shape(shape) = &bdoc.layers.get(find(bdoc, "Badge")).unwrap().kind else {
            panic!("the shape came back as something else");
        };
        let ring = &svg_path::knots(&shape.path_svg).unwrap()[0].1;
        let anchors: Vec<[f64; 2]> = ring
            .iter()
            .map(|k| [k.anchor[0].round(), k.anchor[1].round()])
            .collect();
        assert_eq!(
            anchors,
            [[20.0, 18.0], [44.0, 18.0], [44.0, 34.0], [20.0, 34.0]]
        );
        assert_eq!(shape.fill, Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(shape.fill_rule, ShapeFillRule::EvenOdd);
        let stroke = shape.stroke.as_ref().expect("the stroke travelled");
        assert_eq!((stroke.width_px, stroke.join), (2.0, ShapeJoin::Round));
        assert_eq!(stroke.color, [0.0, 0.0, 1.0, 1.0]);

        // The smart object: still a smart object over the same source bytes.
        let logo = find(bdoc, "Logo");
        let LayerKind::SmartObject(object) = &bdoc.layers.get(logo).unwrap().kind else {
            panic!("the smart object came back as something else");
        };
        let original = match &doc.layers.get(find(&doc, "Logo")).unwrap().kind {
            LayerKind::SmartObject(o) => doc.asset_origin(o.asset),
            _ => unreachable!(),
        };
        assert_eq!(bdoc.asset_origin(object.asset), original);
        assert_eq!(bdoc.asset_source_size(object.asset), Some((8, 8)));

        // Pixels: each live layer renders as it did before the round trip.
        for name in ["Badge", "Logo", "Deep"] {
            let before = rendered(&doc, &tiles, find(&doc, name));
            let after = rendered(bdoc, btiles, find(bdoc, name));
            let worst = before
                .iter()
                .zip(&after)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(worst <= 1, "{name} differs by {worst}");
            assert!(before.iter().any(|v| *v != 0), "{name} draws something");
        }
    }

    #[test]
    fn a_pattern_overlay_travels_with_its_pattern() {
        let mut doc = Document::new(16, 16, "fx");
        let mut tiles = MemoryTileSource::new();
        let layer = doc.layers.push_root(Layer::raster("Tiled")).unwrap();
        let edits = tile_edits_for_rgba(
            &[9u8, 9, 9, 255].repeat(64),
            psd::Rect::sized(8, 8),
            &mut tiles,
        );
        doc.pixels
            .apply(PixelKey::Layer(layer), &TileDelta::new(edits).unwrap());
        let tile = layer_model::effects::PatternTile::new(
            "Checks",
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        )
        .unwrap();
        doc.layers.get_mut(layer).unwrap().effects.pattern_overlay =
            Some(layer_model::PatternOverlayEffect {
                opacity: 0.5,
                pattern: layer_model::effects::PatternFill {
                    tile: Some(tile.clone()),
                    scale: 2.0,
                    ..Default::default()
                },
                ..Default::default()
            });
        let (bytes, notes) = psd_from_document(&doc, &tiles, &[0u8; 16 * 16 * 4]).unwrap();
        assert!(
            notes.is_empty(),
            "the overlay is not named as dropped: {notes:?}"
        );
        let back = document_from_psd(&bytes, "back", 10).unwrap();
        let bdoc = &back.imported.document;
        let effects = &bdoc.layers.get(find(bdoc, "Tiled")).unwrap().effects;
        let overlay = effects
            .pattern_overlay
            .as_ref()
            .expect("the overlay came back");
        assert!((overlay.opacity - 0.5).abs() < 1e-6);
        assert!((overlay.pattern.scale - 2.0).abs() < 1e-6);
        let came = overlay.pattern.tile.as_ref().expect("with its pixels");
        assert_eq!(came.rgba8(), tile.rgba8());
    }

    /// A placed layer whose file is missing opens as its pixels, and says so.
    #[test]
    fn a_smart_object_whose_file_is_missing_opens_as_pixels_and_is_named() {
        let (doc, tiles) = live_document();
        let composite = vec![0u8; (W * H * 4) as usize];
        let (bytes, _) = psd_from_document(&doc, &tiles, &composite).unwrap();
        let mut file = psd::read(&bytes).unwrap();
        file.extra.retain(|b| b.key != *b"lnk2");
        let back = document_from_psd(&psd::write(&file).unwrap(), "cut", 10).unwrap();
        let bdoc = &back.imported.document;
        assert!(matches!(
            bdoc.layers.get(find(bdoc, "Logo")).unwrap().kind,
            LayerKind::Raster(_)
        ));
        let told = back.notes.summary().unwrap();
        assert!(told.contains("Logo") && told.contains("pixels"), "{told}");
        let report = back
            .notes
            .layers()
            .iter()
            .find(|l| l.name == "Logo")
            .unwrap();
        assert_eq!(report.outcome, PsdLayerOutcome::RasterFallback);
    }

    /// Writes the round-trip fixture for an independent reader (psd-tools):
    /// `RS_W9M_PSD_OUT=path cargo test -p app-shell --lib -- --ignored w9m_fixture`.
    #[test]
    #[ignore]
    fn w9m_fixture_for_independent_readers() {
        let Ok(path) = std::env::var("RS_W9M_PSD_OUT") else {
            return;
        };
        let (doc, tiles) = live_document();
        let composite = vec![0u8; (W * H * 4) as usize];
        let (bytes, _) = psd_from_document(&doc, &tiles, &composite).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
