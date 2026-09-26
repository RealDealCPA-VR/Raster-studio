//! W16-I: an SVG read as **layers**, the way Photopea opens one: groups as
//! groups, vector shapes as shape layers (path, solid fill, solid stroke),
//! plain text as text layers (string, font, size, colour) and raster images
//! as raster layers.
//!
//! A child module of [`super`] (the SVG importer), declared there with
//! `#[path]`. The SVG is parsed exactly as the flat importer parses it (the
//! same size bound, gzip cap, `data:`-only image resolver and panic guard);
//! the `usvg` tree is then walked into the format-neutral
//! [`DesignDocument`] the Sketch / XD / Figma reader produces, so
//! `app_shell::import_design` maps it onto the document model with the same
//! shape and text mapping.
//!
//! The EPS importer (its PostScript interpreter writes what it paints as an
//! SVG display list: see `formats::postscript::display_list`) and the PDF
//! page reader (`formats::pdf::layers`, which writes a page's paths and text
//! as the same kind of display list) come through [`read_layers`] too.
//!
//! # What maps, and what is flattened
//!
//! | usvg node | Opens as |
//! | --- | --- |
//! | `Group` (every `<g>`, and each element usvg wraps for its transform or opacity) | a layer group with the group's opacity; an unnamed wrapper around a single element is dropped and the element stands in its place |
//! | `Path` with a solid fill and / or a solid, undashed stroke | a shape layer: the path in its own space, placed by its whole transform |
//! | `Path` filled with a padded linear gradient, or a padded radial gradient that is circular on the canvas with its focal point at its centre | a shape layer with a **live gradient fill** ([`ShapeGradient`], in [`VectorLayers::gradients`]): the same stops and the same ramp geometry, re-expressed as the editor's gradient (angle, scale and offset from the shape's filled bounds) by `app_shell` |
//! | `Text` that is one run of one style, anchored at its start, with a solid fill and no stroke, decoration, spacing, per-glyph positioning or text path, placed without skew or mirroring | a text layer (string, first font family, size, weight, italic, colour); the baseline is placed an estimated 0.8 em below the layer's top |
//! | any other `Text` | its outlines, as shape layers in a group named after the text |
//! | `Image` | a raster layer |
//!
//! What cannot be carried as a live layer is **flattened**: the element (a
//! group with a clip path, mask, filter or non-normal blend mode, a path
//! with a pattern fill, a reflected or repeated gradient, an elliptical or
//! off-centre radial gradient, a gradient or pattern stroke or a dashed
//! stroke) is drawn by `resvg`
//! on its own, at its place on the canvas, into a raster layer, and the
//! import report says how many elements were flattened and why
//! ([`DesignDocument::notes`]). Stroke caps and joins other than butt /
//! miter open as butt / miter and the report says so. A live gradient's
//! ramp is blended the way the editor blends every gradient (in linear
//! light), where an SVG viewer blends in sRGB, so its midtones differ
//! slightly from the file's; the ends and the geometry are the file's.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use resvg::{tiny_skia, usvg};

use super::{guarded, parse, pixel_size, CodecError, ImportFormat, ImportLimits};
use crate::codec::formats::vector_docs::design_files::{
    Affine, DesignDocument, DesignKind, DesignNode, DesignStroke, StrokeAlign, MAX_LAYER_DEPTH,
    MAX_NODES,
};

/// Where a text layer's baseline sits below its top, in em: an estimate of
/// a typical font's ascent (the SVG gives the baseline, the layer its top).
pub const ASCENT_EM: f64 = 0.8;

/// An SVG (or an EPS / PDF display list) read as layers.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorLayers {
    /// The layer tree; `format` is the file's own format.
    pub design: DesignDocument,
    /// The canvas: the drawing's own size, rounded up to whole pixels.
    pub width: u32,
    pub height: u32,
    /// How many elements opened as pixels (see the module docs).
    pub flattened: usize,
    /// W16-I: the live gradient fills, each keyed by its shape node's index
    /// in [`DesignDocument::walk`] order (that node's `fill` is the first
    /// stop's colour, what the shape shows where gradients are not drawn).
    pub gradients: Vec<(usize, ShapeGradient)>,
}

/// The geometry of a [`ShapeGradient`], in the shape node's own path space
/// (the space of its `path_svg`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientGeometry {
    /// The ramp runs from `from` (position 0) to `to` (position 1) and is
    /// constant across that direction; padded beyond both ends.
    Linear { from: (f64, f64), to: (f64, f64) },
    /// The ramp runs from `centre` (position 0) out to `radius` (position
    /// 1); padded beyond.
    Radial { centre: (f64, f64), radius: f64 },
}

/// W16-I: a shape's gradient fill as the file paints it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeGradient {
    pub geometry: GradientGeometry,
    /// `(position 0..=1, straight sRGB RGBA)`, in order; the fill's own
    /// opacity is folded into each stop's alpha. Never empty.
    pub stops: Vec<(f32, [f32; 4])>,
}

/// The marker a gradient shape's name carries while the tree is built,
/// before [`VectorLayers::gradients`] is keyed (a NUL cannot be in an XML
/// name).
const GRADIENT_TAG: char = '\u{0}';

/// Strip the gradient tags from `nodes` (pre-order), answering each tagged
/// node's walk index with its gradient.
fn key_gradients(
    nodes: &mut [DesignNode],
    pending: &mut [Option<ShapeGradient>],
    index: &mut usize,
    out: &mut Vec<(usize, ShapeGradient)>,
) {
    for node in nodes {
        if let Some(at) = node.name.find(GRADIENT_TAG) {
            let slot = node.name[at + 1..].parse::<usize>().ok();
            node.name.truncate(at);
            if let Some(g) = slot.and_then(|i| pending.get_mut(i)).and_then(Option::take) {
                out.push((*index, g));
            }
        }
        *index += 1;
        match &mut node.kind {
            DesignKind::Group { children } | DesignKind::Artboard { children, .. } => {
                key_gradients(children, pending, index, out)
            }
            _ => {}
        }
    }
}

/// `paint` (a fill with opacity `opacity`) as a live gradient on a path
/// whose node is shifted by `(-dx, -dy)`, or why it must be flattened.
/// `extent` is the path's longer side.
fn shape_gradient(
    paint: &usvg::Paint,
    opacity: f32,
    (dx, dy): (f64, f64),
    extent: f64,
) -> Result<ShapeGradient, &'static str> {
    let degenerate = "a gradient with a degenerate transform";
    let (base, geometry): (&usvg::BaseGradient, _) = match paint {
        usvg::Paint::LinearGradient(g) => {
            // Position t(g) = ((g - p1) . d) / |d|^2 in gradient space;
            // gradient space is `transform` of the path's own space.
            let inv = g.transform().invert().ok_or(degenerate)?;
            let (p1, p2) = (
                (f64::from(g.x1()), f64::from(g.y1())),
                (f64::from(g.x2()), f64::from(g.y2())),
            );
            let d = (p2.0 - p1.0, p2.1 - p1.1);
            let dd = d.0 * d.0 + d.1 * d.1;
            if !(dd.is_finite() && dd > 1e-12) {
                return Err(degenerate);
            }
            let (a, b, c, e) = (
                f64::from(inv.sx),
                f64::from(inv.ky),
                f64::from(inv.kx),
                f64::from(inv.sy),
            );
            let (tx, ty) = (f64::from(inv.tx), f64::from(inv.ty));
            // t(u) = n . u + k for u in the path's own space.
            let n = ((a * d.0 + b * d.1) / dd, (c * d.0 + e * d.1) / dd);
            let k = ((tx - p1.0) * d.0 + (ty - p1.1) * d.1) / dd;
            let nn = n.0 * n.0 + n.1 * n.1;
            if !(nn.is_finite() && nn > 1e-18) {
                return Err(degenerate);
            }
            // In the node's shifted space q = u - (dx, dy).
            let t0 = n.0 * dx + n.1 * dy + k;
            let from = (-t0 * n.0 / nn, -t0 * n.1 / nn);
            let to = (from.0 + n.0 / nn, from.1 + n.1 / nn);
            let len = (1.0 / nn).sqrt();
            if extent > 0.0 && !(0.02..=500.0).contains(&(len / extent)) {
                return Err("a gradient far longer or shorter than its shape");
            }
            (&**g, GradientGeometry::Linear { from, to })
        }
        usvg::Paint::RadialGradient(g) => {
            if (g.fx() - g.cx()).abs() > 1e-4 || (g.fy() - g.cy()).abs() > 1e-4 {
                return Err("a radial gradient with its focal point off its centre");
            }
            let t = g.transform();
            let (sx, ky, kx, sy) = (
                f64::from(t.sx),
                f64::from(t.ky),
                f64::from(t.kx),
                f64::from(t.sy),
            );
            let (l1, l2) = (sx * sx + ky * ky, kx * kx + sy * sy);
            let s = l1.sqrt();
            if !(s.is_finite() && s > 1e-9)
                || (l1 - l2).abs() > 1e-3 * l1
                || (sx * kx + ky * sy).abs() > 1e-3 * l1
            {
                return Err("an elliptical radial gradient");
            }
            let (cx, cy) = (f64::from(g.cx()), f64::from(g.cy()));
            let centre = (
                sx * cx + kx * cy + f64::from(t.tx) - dx,
                ky * cx + sy * cy + f64::from(t.ty) - dy,
            );
            let radius = f64::from(g.r().get()) * s;
            if extent > 0.0 && !(0.01..=250.0).contains(&(radius / extent)) {
                return Err("a gradient far longer or shorter than its shape");
            }
            (&**g, GradientGeometry::Radial { centre, radius })
        }
        _ => return Err("a pattern fill"),
    };
    if base.spread_method() != usvg::SpreadMethod::Pad {
        return Err("a reflected or repeated gradient");
    }
    let stops: Vec<(f32, [f32; 4])> = base
        .stops()
        .iter()
        .map(|s| {
            (
                s.offset().get(),
                rgba(s.color(), s.opacity().get() * opacity.clamp(0.0, 1.0)),
            )
        })
        .collect();
    if stops.is_empty() {
        return Err("a gradient with no stops");
    }
    Ok(ShapeGradient { geometry, stops })
}

/// Read `data` (an SVG, gzip-compressed or not) as layers. `format` names
/// the file the SVG came from (it is `Svg` for an SVG; the EPS and PDF
/// readers pass their own) and `opened` what part of it this is (for the
/// status line, for example `the drawing`).
pub fn read_layers(
    data: &[u8],
    limits: ImportLimits,
    format: ImportFormat,
    opened: &str,
) -> Result<VectorLayers, CodecError> {
    let tree = parse(data)?;
    let (width, height) = pixel_size(&tree, limits)?;
    guarded("layer reader", || {
        let mut walk = Walk {
            limits,
            canvas: (width, height),
            nodes: 0,
            held: 0,
            flattened: BTreeMap::new(),
            caps: 0,
            gradients: Vec::new(),
        };
        let root = tree.root();
        let mut nodes = walk.children(root, 0)?;
        let mut gradients = Vec::new();
        key_gradients(&mut nodes, &mut walk.gradients, &mut 0, &mut gradients);
        let mut notes = Vec::new();
        for (why, n) in &walk.flattened {
            notes.push(if *n == 1 {
                format!("1 element with {why} opened as pixels (a raster layer)")
            } else {
                format!("{n} elements with {why} opened as pixels (a raster layer each)")
            });
        }
        if walk.caps > 0 {
            notes.push(format!(
                "{} stroke{} with round or square caps or round or bevel joins opened with butt caps and miter joins",
                walk.caps,
                if walk.caps == 1 { "" } else { "s" }
            ));
        }
        Ok(VectorLayers {
            design: DesignDocument {
                format,
                nodes,
                notes,
                opened: opened.to_string(),
            },
            width,
            height,
            flattened: walk.flattened.values().sum(),
            gradients,
        })
    })
}

fn affine(t: tiny_skia::Transform) -> Affine {
    Affine([
        f64::from(t.sx),
        f64::from(t.ky),
        f64::from(t.kx),
        f64::from(t.sy),
        f64::from(t.tx),
        f64::from(t.ty),
    ])
}

fn rgba(c: usvg::Color, opacity: f32) -> [f32; 4] {
    [
        f32::from(c.red) / 255.0,
        f32::from(c.green) / 255.0,
        f32::from(c.blue) / 255.0,
        opacity.clamp(0.0, 1.0),
    ]
}

/// `path` as SVG path data, moved by `(-dx, -dy)`.
fn path_data(path: &tiny_skia::Path, dx: f32, dy: f32) -> String {
    let mut d = String::new();
    let p = |pt: tiny_skia::Point| (pt.x - dx, pt.y - dy);
    for seg in path.segments() {
        match seg {
            tiny_skia::PathSegment::MoveTo(a) => {
                let (x, y) = p(a);
                let _ = write!(d, "M{x} {y}");
            }
            tiny_skia::PathSegment::LineTo(a) => {
                let (x, y) = p(a);
                let _ = write!(d, "L{x} {y}");
            }
            tiny_skia::PathSegment::QuadTo(a, b) => {
                let ((x1, y1), (x, y)) = (p(a), p(b));
                let _ = write!(d, "Q{x1} {y1} {x} {y}");
            }
            tiny_skia::PathSegment::CubicTo(a, b, c) => {
                let ((x1, y1), (x2, y2), (x, y)) = (p(a), p(b), p(c));
                let _ = write!(d, "C{x1} {y1} {x2} {y2} {x} {y}");
            }
            tiny_skia::PathSegment::Close => d.push('Z'),
        }
    }
    d
}

struct Walk {
    limits: ImportLimits,
    canvas: (u32, u32),
    nodes: usize,
    /// Bytes of flattened pixels held so far.
    held: u64,
    /// Elements flattened, by reason.
    flattened: BTreeMap<&'static str, usize>,
    /// Strokes whose caps / joins did not carry over.
    caps: usize,
    /// W16-I: live gradient fills, by the tag their shape's name carries.
    gradients: Vec<Option<ShapeGradient>>,
}

impl Walk {
    /// The layers of `group`'s children, bottom first.
    fn children(
        &mut self,
        group: &usvg::Group,
        depth: usize,
    ) -> Result<Vec<DesignNode>, CodecError> {
        let parent = group.abs_transform();
        let mut out = Vec::new();
        for child in group.children() {
            if let Some(node) = self.node(child, parent, depth)? {
                out.push(node);
            }
        }
        Ok(out)
    }

    fn node(
        &mut self,
        node: &usvg::Node,
        parent: tiny_skia::Transform,
        depth: usize,
    ) -> Result<Option<DesignNode>, CodecError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(CodecError::LimitExceeded(format!(
                "the drawing has more than {MAX_NODES} elements"
            )));
        }
        let id = node.id();
        let named = |fallback: &str| {
            if id.is_empty() {
                fallback.to_string()
            } else {
                id.to_string()
            }
        };
        match node {
            usvg::Node::Group(g) => {
                let why = if g.clip_path().is_some() {
                    Some("a clipping path")
                } else if g.mask().is_some() {
                    Some("a mask")
                } else if !g.filters().is_empty() {
                    Some("a filter")
                } else if g.blend_mode() != usvg::BlendMode::Normal {
                    Some("a blend mode")
                } else if depth >= MAX_LAYER_DEPTH {
                    Some("nesting deeper than the layer tree reads")
                } else {
                    None
                };
                if let Some(why) = why {
                    return self.flatten(node, parent, named("Group"), why);
                }
                let mut children = self.children(g, depth + 1)?;
                let opacity = g.opacity().get();
                if children.is_empty() {
                    return Ok(None);
                }
                if id.is_empty() && children.len() == 1 && opacity >= 1.0 {
                    return Ok(children.pop());
                }
                Ok(Some(DesignNode {
                    name: named("Group"),
                    visible: true,
                    opacity,
                    transform: Affine::IDENTITY,
                    width: 0.0,
                    height: 0.0,
                    kind: DesignKind::Group { children },
                }))
            }
            usvg::Node::Path(p) => {
                if !p.is_visible() {
                    return Ok(None);
                }
                let bounds = p.data().bounds();
                let (x, y) = (bounds.left(), bounds.top());
                let mut gradient = None;
                let fill = match p.fill() {
                    None => None,
                    Some(f) => match f.paint() {
                        usvg::Paint::Color(c) => Some((rgba(*c, f.opacity().get()), f.rule())),
                        paint => {
                            // W16-I: a gradient the editor's shape gradient
                            // draws the same stays live.
                            let extent = f64::from(bounds.width().max(bounds.height()));
                            match shape_gradient(
                                paint,
                                f.opacity().get(),
                                (f64::from(x), f64::from(y)),
                                extent,
                            ) {
                                Ok(g) => {
                                    let first = g.stops[0].1;
                                    gradient = Some(g);
                                    Some((first, f.rule()))
                                }
                                Err(why) => return self.flatten(node, parent, named("Shape"), why),
                            }
                        }
                    },
                };
                let stroke = match p.stroke() {
                    None => None,
                    Some(s) => match s.paint() {
                        usvg::Paint::Color(c) if s.dasharray().is_none() => {
                            if s.linecap() != usvg::LineCap::Butt
                                || s.linejoin() != usvg::LineJoin::Miter
                            {
                                self.caps += 1;
                            }
                            Some(DesignStroke {
                                color: rgba(*c, s.opacity().get()),
                                width: s.width().get(),
                                align: StrokeAlign::Center,
                            })
                        }
                        usvg::Paint::Color(_) => {
                            return self.flatten(node, parent, named("Shape"), "a dashed stroke")
                        }
                        _ => {
                            return self.flatten(
                                node,
                                parent,
                                named("Shape"),
                                "a gradient or pattern stroke",
                            )
                        }
                    },
                };
                if fill.is_none() && stroke.is_none() {
                    return Ok(None);
                }
                let mut name = named("Shape");
                if let Some(g) = gradient {
                    name = format!("{name}{GRADIENT_TAG}{}", self.gradients.len());
                    self.gradients.push(Some(g));
                }
                Ok(Some(DesignNode {
                    name,
                    visible: true,
                    opacity: 1.0,
                    transform: affine(p.abs_transform())
                        .then(Affine::translate(f64::from(x), f64::from(y))),
                    width: f64::from(bounds.width()),
                    height: f64::from(bounds.height()),
                    kind: DesignKind::Shape {
                        path_svg: path_data(p.data(), x, y),
                        fill: fill.map(|f| f.0),
                        stroke,
                        even_odd: fill.is_some_and(|f| f.1 == usvg::FillRule::EvenOdd),
                    },
                }))
            }
            usvg::Node::Text(t) => {
                if let Some(text) = plain_text(t) {
                    return Ok(Some(text));
                }
                // Outlines: the glyphs as shapes, in a group named after the
                // text.
                let label: String = t.chunks().iter().map(|c| c.text()).collect();
                let label = label.trim().chars().take(64).collect::<String>();
                let children = self.children(t.flattened(), depth + 1)?;
                if children.is_empty() {
                    return Ok(None);
                }
                Ok(Some(DesignNode {
                    name: if id.is_empty() { label } else { id.to_string() },
                    visible: true,
                    opacity: 1.0,
                    transform: Affine::IDENTITY,
                    width: 0.0,
                    height: 0.0,
                    kind: DesignKind::Group { children },
                }))
            }
            usvg::Node::Image(_) => self.flatten(node, parent, named("Image"), ""),
        }
    }

    /// `node` drawn on its own, at its place on the canvas, as a raster
    /// layer. `why` is the report's reason (empty for an image, which is
    /// pixels anyway).
    fn flatten(
        &mut self,
        node: &usvg::Node,
        parent: tiny_skia::Transform,
        name: String,
        why: &'static str,
    ) -> Result<Option<DesignNode>, CodecError> {
        let Some(bbox) = node.abs_layer_bounding_box() else {
            return Ok(None);
        };
        let (cw, ch) = (self.canvas.0 as f32, self.canvas.1 as f32);
        let x0 = bbox.x().floor().clamp(0.0, cw);
        let y0 = bbox.y().floor().clamp(0.0, ch);
        let x1 = bbox.right().ceil().clamp(0.0, cw);
        let y1 = bbox.bottom().ceil().clamp(0.0, ch);
        if x1 <= x0 || y1 <= y0 {
            return Ok(None);
        }
        let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
        self.limits.check_dimensions(w, h)?;
        self.held = self.held.saturating_add(u64::from(w) * u64::from(h) * 8);
        self.limits.check_alloc(self.held)?;
        let mut pixmap = tiny_skia::Pixmap::new(w, h)
            .ok_or_else(|| CodecError::LimitExceeded(format!("cannot allocate {w}x{h}")))?;
        // `render_node` pre-translates by the bounding box in the node's own
        // space; undo that so the node lands where it sits on the canvas.
        let transform = tiny_skia::Transform::from_translate(-x0, -y0)
            .pre_concat(parent)
            .pre_translate(bbox.x(), bbox.y());
        resvg::render_node(node, transform, &mut pixmap.as_mut());
        let mut out = Vec::with_capacity(pixmap.pixels().len() * 4);
        for px in pixmap.pixels() {
            let c = px.demultiply();
            out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        // Trimmed to what was drawn (a clip or mask can leave most of the
        // bounding box empty).
        let (mut tx0, mut ty0, mut tx1, mut ty1) = (w, h, 0u32, 0u32);
        for (i, px) in out.as_chunks::<4>().0.iter().enumerate() {
            if px[3] != 0 {
                let (x, y) = (i as u32 % w, i as u32 / w);
                tx0 = tx0.min(x);
                ty0 = ty0.min(y);
                tx1 = tx1.max(x + 1);
                ty1 = ty1.max(y + 1);
            }
        }
        if tx1 <= tx0 || ty1 <= ty0 {
            return Ok(None);
        }
        let (tw, th) = (tx1 - tx0, ty1 - ty0);
        let mut trimmed = Vec::with_capacity(tw as usize * th as usize * 4);
        for y in ty0..ty1 {
            let row = (y * w + tx0) as usize * 4;
            trimmed.extend_from_slice(&out[row..row + tw as usize * 4]);
        }
        let (out, w, h) = (trimmed, tw, th);
        let (x0, y0) = (x0 + tx0 as f32, y0 + ty0 as f32);
        if !why.is_empty() {
            *self.flattened.entry(why).or_default() += 1;
        }
        Ok(Some(DesignNode {
            name,
            visible: true,
            opacity: 1.0,
            transform: Affine::translate(f64::from(x0), f64::from(y0)),
            width: f64::from(w),
            height: f64::from(h),
            kind: DesignKind::Bitmap {
                width: w,
                height: h,
                rgba: out,
            },
        }))
    }
}

/// `t` as a text layer when it is plain (see the module docs); `None` when
/// it must open as outlines.
fn plain_text(t: &usvg::Text) -> Option<DesignNode> {
    let [chunk] = t.chunks() else {
        return None;
    };
    let [span] = chunk.spans() else {
        return None;
    };
    let zero = |v: &[f32]| v.iter().all(|x| x.abs() < 1e-6);
    let decorated = span.decoration().underline().is_some()
        || span.decoration().overline().is_some()
        || span.decoration().line_through().is_some();
    if chunk.anchor() != usvg::TextAnchor::Start
        || !matches!(chunk.text_flow(), usvg::TextFlow::Linear)
        || t.writing_mode() != usvg::WritingMode::LeftToRight
        || !zero(t.dx())
        || !zero(t.dy())
        || !zero(t.rotate())
        || span.start() != 0
        || span.end() != chunk.text().len()
        || span.stroke().is_some()
        || decorated
        || !span.is_visible()
        || span.letter_spacing().abs() > 1e-6
        || span.word_spacing().abs() > 1e-6
        || !span.baseline_shift().is_empty()
        || span.text_length().is_some()
        || span.small_caps()
    {
        return None;
    }
    let fill = span.fill()?;
    let usvg::Paint::Color(color) = fill.paint() else {
        return None;
    };
    let abs = t.abs_transform();
    let (a, b, c, d) = (
        f64::from(abs.sx),
        f64::from(abs.ky),
        f64::from(abs.kx),
        f64::from(abs.sy),
    );
    let det = a * d - b * c;
    if !(det.is_finite() && det > 1e-12) {
        return None;
    }
    let s = det.sqrt();
    // A rotation and a uniform scale only (no skew, no mirroring).
    if (a - d).abs() > 1e-3 * s || (b + c).abs() > 1e-3 * s {
        return None;
    }
    let text = chunk.text().to_string();
    if text.trim().is_empty() || text.contains('\n') {
        return None;
    }
    let font = span.font();
    let family = match font.families().first() {
        Some(usvg::FontFamily::Named(name)) => name.clone(),
        Some(generic) => generic.to_string(),
        None => "sans-serif".to_string(),
    };
    let em = f64::from(span.font_size().get());
    let (x, y) = (
        f64::from(chunk.x().unwrap_or(0.0)),
        f64::from(chunk.y().unwrap_or(0.0)),
    );
    let size = em * s;
    let transform = affine(abs)
        .then(Affine::translate(x, y - ASCENT_EM * em))
        .then(Affine([1.0 / s, 0.0, 0.0, 1.0 / s, 0.0, 0.0]));
    let bbox = t.bounding_box();
    let name = text.trim().chars().take(64).collect::<String>();
    Some(DesignNode {
        name: if t.id().is_empty() {
            name
        } else {
            t.id().to_string()
        },
        visible: true,
        opacity: 1.0,
        transform,
        width: (f64::from(bbox.width()) * s).max(1.0),
        height: (em * 1.2 * s).max(1.0),
        kind: DesignKind::Text {
            text,
            font_family: family,
            bold: font.weight() >= 600,
            italic: font.style() != usvg::FontStyle::Normal,
            size: size as f32,
            color: rgba(*color, fill.opacity().get()),
            box_width: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(nodes: &[DesignNode]) -> Vec<String> {
        nodes
            .iter()
            .map(|n| match &n.kind {
                DesignKind::Group { children } => {
                    format!("{}:group[{}]", n.name, kinds(children).join(","))
                }
                DesignKind::Shape { .. } => format!("{}:shape", n.name),
                DesignKind::Text { text, .. } => format!("{}:text({text})", n.name),
                DesignKind::Bitmap { .. } => format!("{}:bitmap", n.name),
                DesignKind::Artboard { .. } => format!("{}:artboard", n.name),
            })
            .collect()
    }

    pub(crate) const FOUR: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
  <rect id="box" x="10" y="20" width="30" height="40" fill="#ff0000" stroke="#0000ff" stroke-width="2"/>
  <circle id="dot" cx="100" cy="50" r="20" fill="#00ff00"/>
  <text id="label" x="20" y="90" font-family="Arial" font-size="16" fill="#000080">Hello</text>
  <g id="pair" opacity="0.5">
    <rect x="150" y="10" width="10" height="10" fill="#101010"/>
    <rect x="170" y="10" width="10" height="10" fill="#202020"/>
  </g>
</svg>"##;

    #[test]
    fn a_rect_a_circle_text_and_a_group_open_as_those_layers() {
        let v = read_layers(
            FOUR.as_bytes(),
            ImportLimits::default(),
            ImportFormat::Svg,
            "the drawing",
        )
        .unwrap();
        assert_eq!((v.width, v.height), (200, 100));
        assert_eq!(
            kinds(&v.design.nodes),
            vec![
                "box:shape",
                "dot:shape",
                "label:text(Hello)",
                "pair:group[Shape:shape,Shape:shape]"
            ]
        );
        assert_eq!(v.flattened, 0);
        assert!(v.design.notes.is_empty(), "{:?}", v.design.notes);
        let DesignKind::Shape { fill, stroke, .. } = &v.design.nodes[0].kind else {
            unreachable!()
        };
        assert_eq!(*fill, Some([1.0, 0.0, 0.0, 1.0]));
        let stroke = stroke.as_ref().expect("the rect's stroke");
        assert_eq!((stroke.color, stroke.width), ([0.0, 0.0, 1.0, 1.0], 2.0));
        // The rect sits where the SVG put it.
        assert_eq!(v.design.nodes[0].corners()[0], (10.0, 20.0));
        let DesignKind::Text {
            size,
            color,
            font_family,
            ..
        } = &v.design.nodes[2].kind
        else {
            unreachable!()
        };
        assert_eq!((*size, font_family.as_str()), (16.0, "Arial"));
        assert!((color[2] - 128.0 / 255.0).abs() < 1e-6);
        // The baseline (y = 90) sits 0.8 em below the layer's top.
        let (_, top) = v.design.nodes[2].transform.apply(0.0, 0.0);
        assert!((top - (90.0 - 0.8 * 16.0)).abs() < 1e-3, "{top}");
        assert!((v.design.nodes[3].opacity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn what_does_not_map_is_flattened_and_reported() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
  <defs><linearGradient id="g" spreadMethod="repeat" x2="0.5"><stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f"/></linearGradient>
  <clipPath id="c"><rect width="10" height="10"/></clipPath></defs>
  <rect id="grad" width="20" height="20" fill="url(#g)"/>
  <g id="clipped" clip-path="url(#c)"><rect width="40" height="40" fill="#0f0"/></g>
  <text id="spaced" x="2" y="38" font-size="8" letter-spacing="3" fill="#000">ab</text>
</svg>"##;
        let v = read_layers(
            svg.as_bytes(),
            ImportLimits::default(),
            ImportFormat::Svg,
            "the drawing",
        )
        .unwrap();
        let k = kinds(&v.design.nodes);
        assert_eq!(k[0], "grad:bitmap");
        assert_eq!(k[1], "clipped:bitmap");
        assert!(k[2].starts_with("spaced:group["), "outlines: {k:?}");
        assert_eq!(v.flattened, 2);
        assert_eq!(v.design.notes.len(), 2, "{:?}", v.design.notes);
        // The clipped group was drawn clipped, at its place.
        let DesignKind::Bitmap { width, height, .. } = &v.design.nodes[1].kind else {
            unreachable!()
        };
        assert_eq!((*width, *height), (10, 10));
    }

    #[test]
    fn padded_linear_and_circular_radial_gradients_stay_live_with_their_geometry() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
  <defs>
    <linearGradient id="h"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff" stop-opacity="0.5"/></linearGradient>
    <linearGradient id="v" x2="0" y2="1"><stop offset="0" stop-color="#000"/><stop offset="1" stop-color="#fff"/></linearGradient>
    <radialGradient id="r"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#000"/></radialGradient>
    <radialGradient id="off" fx="0.2"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#000"/></radialGradient>
  </defs>
  <rect id="across" x="10" y="10" width="80" height="20" fill="url(#h)" fill-opacity="0.5"/>
  <g id="set"><rect id="down" x="100" y="10" width="20" height="60" fill="url(#v)"/></g>
  <circle id="glow" cx="160" cy="50" r="30" fill="url(#r)"/>
  <circle id="focal" cx="40" cy="70" r="20" fill="url(#off)"/>
</svg>"##;
        let v = read_layers(
            svg.as_bytes(),
            ImportLimits::default(),
            ImportFormat::Svg,
            "the drawing",
        )
        .unwrap();
        assert_eq!(
            kinds(&v.design.nodes),
            vec![
                "across:shape",
                "set:group[down:shape]",
                "glow:shape",
                "focal:bitmap"
            ],
            "the tags are stripped from the names"
        );
        assert_eq!(v.flattened, 1);
        assert!(
            v.design.notes[0].contains("focal point off its centre"),
            "{:?}",
            v.design.notes
        );
        // Keyed by walk index: across 0, set 1, down 2, glow 3.
        let keys: Vec<usize> = v.gradients.iter().map(|(i, _)| *i).collect();
        assert_eq!(keys, vec![0, 2, 3]);
        let close =
            |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        // The horizontal ramp spans the rect's width in its own space.
        let (_, across) = &v.gradients[0];
        let GradientGeometry::Linear { from, to } = across.geometry else {
            panic!("{across:?}")
        };
        assert!(
            close(from, (0.0, 0.0)) && close(to, (80.0, 0.0)),
            "{from:?} {to:?}"
        );
        assert_eq!(across.stops[0], (0.0, [1.0, 0.0, 0.0, 0.5]));
        assert_eq!(across.stops[1], (1.0, [0.0, 0.0, 1.0, 0.25]));
        let DesignKind::Shape { fill, .. } = &v.design.nodes[0].kind else {
            unreachable!()
        };
        assert_eq!(*fill, Some([1.0, 0.0, 0.0, 0.5]), "the first stop");
        // The vertical one runs down the rect's height.
        let GradientGeometry::Linear { from, to } = v.gradients[1].1.geometry else {
            panic!()
        };
        assert!(
            close(from, (0.0, 0.0)) && close(to, (0.0, 60.0)),
            "{from:?} {to:?}"
        );
        // The radial one is centred in the circle's box, radius r.
        let GradientGeometry::Radial { centre, radius } = v.gradients[2].1.geometry else {
            panic!()
        };
        assert!(
            close(centre, (30.0, 30.0)) && (radius - 30.0).abs() < 1e-3,
            "{centre:?} {radius}"
        );
    }

    pub(crate) const TWO_FILLS_EPS: &str = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 50\n%%EndComments\n1 0 0 setrgbcolor 10 10 30 20 rectfill\n0 0 1 setrgbcolor newpath 60 10 moveto 90 10 lineto 75 40 lineto closepath fill\n%%EOF\n";

    #[test]
    fn an_eps_with_two_fills_opens_as_two_shape_layers() {
        let (v, note) = crate::codec::formats::postscript::layers(
            TWO_FILLS_EPS.as_bytes(),
            ImportLimits::default(),
        )
        .unwrap();
        assert!(note.starts_with("EPS:"), "{note}");
        assert_eq!((v.width, v.height), (100, 50));
        assert_eq!(v.design.format, ImportFormat::Eps);
        assert_eq!(kinds(&v.design.nodes), vec!["Shape:shape", "Shape:shape"]);
        let fills: Vec<_> = v
            .design
            .nodes
            .iter()
            .map(|n| match &n.kind {
                DesignKind::Shape { fill, .. } => *fill,
                _ => None,
            })
            .collect();
        assert_eq!(
            fills,
            vec![Some([1.0, 0.0, 0.0, 1.0]), Some([0.0, 0.0, 1.0, 1.0])]
        );
        // PostScript y runs up: the red rect (y 10..30) is 20..40 from the top.
        let (x, y) = v.design.nodes[0].corners()[0];
        assert!(
            (x - 10.0).abs() < 1e-3 && (y - 20.0).abs() < 1e-3,
            "{x},{y}"
        );
    }

    #[test]
    fn an_eps_show_opens_as_a_text_layer() {
        let eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 50\n%%EndComments\n/Helvetica findfont 12 scalefont setfont 0 0 1 setrgbcolor 10 20 moveto (Hi) show\n%%EOF\n";
        let (v, _) =
            crate::codec::formats::postscript::layers(eps.as_bytes(), ImportLimits::default())
                .unwrap();
        assert_eq!(kinds(&v.design.nodes), vec!["Hi:text(Hi)"]);
        let DesignKind::Text { size, color, .. } = &v.design.nodes[0].kind else {
            unreachable!()
        };
        assert!((size - 12.0).abs() < 1e-3, "{size}");
        assert_eq!(*color, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn hostile_svgs_error_and_never_panic() {
        for bytes in [
            &b""[..],
            b"<svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#,
        ] {
            assert!(read_layers(bytes, ImportLimits::default(), ImportFormat::Svg, "x").is_err());
        }
        for cut in 0..FOUR.len() {
            let _ = read_layers(
                &FOUR.as_bytes()[..cut],
                ImportLimits::default(),
                ImportFormat::Svg,
                "x",
            );
        }
    }
}
