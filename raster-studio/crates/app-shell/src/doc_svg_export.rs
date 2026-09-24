//! W10-F: SVG export that keeps vectors vectors.
//!
//! [`OpenDocument::vector_svg`] writes the document as an SVG. It walks the
//! layer tree bottom to top the way the compositor does and writes each
//! visible layer as the most faithful element SVG has for it:
//!
//! * a **group** is a `<g opacity>` around its children when its opacity
//!   (times its fill opacity) is below 100%, so the opacity applies once to
//!   the group's finished content, not to each child. That is what an
//!   isolated group composites to, and for a Normal pass-through group with
//!   only source-over children it is the same picture;
//! * a **shape layer** with a solid fill (or none) and a centred stroke is a
//!   `<path>`: its own path data, its transform as `matrix(...)`, fill,
//!   fill rule, stroke colour / width / cap / join / miter limit / dashes,
//!   and its own opacity;
//! * a **text layer** in one style (no style runs, no warp, horizontal) is a
//!   `<text>` with one `<tspan>` per line: family, size, weight, slant,
//!   fill, alignment and leading. Glyph positions are the viewer's: the first
//!   baseline sits at 0.8 em and lines advance by 1.2 em x the leading
//!   multiple (or the absolute leading), which approximates, not reproduces,
//!   this build's shaper;
//! * a **clipping run** (a base layer and the layers clipped to it, found
//!   the way the compositor finds it) is one compositor render of the base
//!   with its clipped layers, embedded as a PNG `<image>` at canvas size.
//!   SVG has no Porter-Duff atop to say it with;
//! * **everything else** (raster, smart object, fill layers, and any shape
//!   or text layer the rules above do not cover: effects, a mask, a
//!   gradient fill, an inside / outside stroke, style runs, warp) is that
//!   one layer rendered by the compositor on its own and embedded as a PNG
//!   `<image>` at canvas size. Its enclosing groups' opacity is left to
//!   their `<g>`, so it is not applied twice.
//!
//! A document the stack cannot express as SVG source-over - an adjustment
//! layer, a blend mode other than Normal, a group with effects, a mask, a
//! transform of its own or an artboard - is written as one embedded image
//! of the whole composite, which is what the codec's own `ExportFormat::Svg`
//! writes; the report says so. Photopea does the same for what it cannot
//! vectorise.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use compositor::MemoryTileSource;
use editor_core::Document;
use layer_model::{BlendMode, ClippingMode, Layer, LayerId, LayerKind};

use super::{DocumentError, OpenDocument};

/// What an SVG export wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SvgExport {
    /// The document text.
    pub svg: String,
    /// Layers written as `<path>`.
    pub paths: usize,
    /// Layers written as `<text>`.
    pub texts: usize,
    /// Layers (or, flattened, the whole document) written as `<image>`.
    pub images: usize,
    /// `true` when the document could not be stacked and went out as one
    /// image.
    pub flattened: bool,
}

/// `true` when this destination asks for an SVG.
pub fn exports_as_svg(path: &Path) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("svg"))
        .unwrap_or(false)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not allowed in XML 1.0.
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// Straight RGBA already encoded in the document's colour space (a shape
/// layer's fill and stroke colour, which the compositor decodes with
/// `to_linear`) to `(#rrggbb, alpha)`. The channels are written as they are,
/// the same encoded values the embedded PNGs carry: encoding them again
/// would brighten every colour that is not 0 or 1.
fn encoded_hex(c: [f32; 4]) -> (String, f32) {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (
        format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2])),
        c[3].clamp(0.0, 1.0),
    )
}

/// Linear straight RGBA (a text layer's fill) encoded into the document's
/// colour space, as the composite's own bytes are, then as
/// [`encoded_hex`].
fn linear_hex(space: &color::ColorSpace, c: [f32; 4]) -> (String, f32) {
    let rgb = [c[0], c[1], c[2]].map(|v| v.clamp(0.0, 1.0));
    let enc = color::from_linear(space, rgb);
    encoded_hex([enc[0], enc[1], enc[2], c[3]])
}

/// A number written without float noise.
fn num(v: f32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

fn matrix(layer: &Layer) -> String {
    let m = layer.transform.matrix2;
    let t = layer.transform.translation;
    format!(
        "matrix({} {} {} {} {} {})",
        num(m.x_axis.x),
        num(m.x_axis.y),
        num(m.y_axis.x),
        num(m.y_axis.y),
        num(t.x),
        num(t.y)
    )
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A layer that stacks as plain source-over in SVG.
fn plain(layer: &Layer) -> bool {
    layer.blend_mode == BlendMode::Normal
        && layer.clipping == ClippingMode::None
        && layer.mask.is_none()
        && layer.effects.is_empty()
}

/// The `<path>` for a shape layer, or `None` when SVG cannot say it.
fn shape_path(layer: &Layer, opacity: f32) -> Option<String> {
    let LayerKind::Shape(shape) = &layer.kind else {
        return None;
    };
    if !plain(layer) || !shape.fill_paint.is_solid() || shape.path_svg.trim().is_empty() {
        return None;
    }
    let mut out = format!(
        "<path d=\"{}\" transform=\"{}\"",
        escape(&shape.path_svg),
        matrix(layer)
    );
    match shape.fill {
        Some(fill) => {
            let (hex, alpha) = encoded_hex(fill);
            let rule = match shape.fill_rule {
                layer_model::ShapeFillRule::NonZero => "nonzero",
                layer_model::ShapeFillRule::EvenOdd => "evenodd",
            };
            let _ = write!(
                out,
                " fill=\"{hex}\" fill-opacity=\"{}\" fill-rule=\"{rule}\"",
                num(alpha)
            );
        }
        None => out.push_str(" fill=\"none\""),
    }
    if let Some(stroke) = &shape.stroke {
        if stroke.align != layer_model::ShapeStrokeAlign::Center {
            return None;
        }
        let (hex, alpha) = encoded_hex(stroke.color);
        let cap = match stroke.cap {
            layer_model::ShapeCap::Butt => "butt",
            layer_model::ShapeCap::Round => "round",
            layer_model::ShapeCap::Square => "square",
        };
        let join = match stroke.join {
            layer_model::ShapeJoin::Miter => "miter",
            layer_model::ShapeJoin::Round => "round",
            layer_model::ShapeJoin::Bevel => "bevel",
        };
        // Widths and dashes are document pixels, whatever the transform.
        let _ = write!(
            out,
            " stroke=\"{hex}\" stroke-opacity=\"{}\" stroke-width=\"{}\" \
             stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\" stroke-miterlimit=\"{}\" \
             vector-effect=\"non-scaling-stroke\"",
            num(alpha),
            num(stroke.width_px.max(0.0)),
            num(stroke.miter_limit.max(1.0))
        );
        if !stroke.dash.is_empty() {
            let dashes: Vec<String> = stroke.dash.iter().map(|d| num(d.max(0.0))).collect();
            let _ = write!(
                out,
                " stroke-dasharray=\"{}\" stroke-dashoffset=\"{}\"",
                dashes.join(" "),
                num(stroke.dash_offset)
            );
        }
    }
    // The compositor draws the stroke over the fill and only then fades the
    // whole layer by opacity x fill opacity, so fill opacity goes on the
    // element, not on each paint: folded into fill-opacity and
    // stroke-opacity, a translucent fill would show through the stroke.
    let _ = write!(
        out,
        " opacity=\"{}\"/>",
        num(opacity * layer.effective_fill_opacity())
    );
    Some(out)
}

/// The `<text>` for a text layer, or `None` when SVG cannot say it.
fn text_element(layer: &Layer, opacity: f32, space: &color::ColorSpace) -> Option<String> {
    let LayerKind::Text(text) = &layer.kind else {
        return None;
    };
    if !plain(layer)
        || !text.spans.is_empty()
        || !text.warp.is_default()
        || text.paragraph.vertical
        || text.text.is_empty()
    {
        return None;
    }
    let size = text.size_px.max(0.1);
    let step = match text.paragraph.leading {
        layer_model::text::Leading::Multiple(m) => 1.2 * size * m.max(0.0),
        layer_model::text::Leading::Absolute(px) => px.max(0.0),
    };
    let width = match text.frame {
        layer_model::text::Frame::Box { width, .. } => width,
        layer_model::text::Frame::Point => 0.0,
    };
    let (anchor, x) = match text.paragraph.alignment {
        layer_model::text::Alignment::Center => ("middle", width / 2.0),
        layer_model::text::Alignment::Right => ("end", width),
        _ => ("start", 0.0),
    };
    let (hex, alpha) = linear_hex(space, text.style.fill);
    let mut out = format!(
        "<text transform=\"{}\" font-family=\"{}\" font-size=\"{}\" fill=\"{hex}\" \
         fill-opacity=\"{}\" text-anchor=\"{anchor}\" xml:space=\"preserve\"",
        matrix(layer),
        escape(&text.font_family),
        num(size),
        num(alpha * layer.effective_fill_opacity()),
    );
    if text.style.weight.0 != 400 {
        let _ = write!(out, " font-weight=\"{}\"", text.style.weight.0);
    }
    if text.style.slant == layer_model::text::Slant::Italic {
        out.push_str(" font-style=\"italic\"");
    }
    let _ = write!(out, " opacity=\"{}\">", num(opacity));
    let normalised = text.text.replace("\r\n", "\n").replace('\r', "\n");
    for (i, line) in normalised.split('\n').enumerate() {
        let _ = write!(
            out,
            "<tspan x=\"{}\" y=\"{}\">{}</tspan>",
            num(x),
            num(0.8 * size + i as f32 * step),
            escape(line)
        );
    }
    out.push_str("</text>");
    Some(out)
}

impl OpenDocument {
    /// The document as an SVG; see the module docs for what becomes what.
    pub fn vector_svg(&self) -> Result<SvgExport, DocumentError> {
        vector_svg(&self.document, &self.tiles)
    }

    /// Write [`OpenDocument::vector_svg`] to `path`, atomically.
    pub fn export_svg_to(&self, path: &Path) -> Result<SvgExport, DocumentError> {
        write_vector_svg(&self.document, &self.tiles, path)
    }
}

/// Write [`vector_svg`] of a snapshot to `path`, atomically: the one writer
/// both `OpenDocument::export_to` and File > Export's worker
/// (`jobs::run_file_export`) call.
pub fn write_vector_svg(
    document: &Document,
    tiles: &MemoryTileSource,
    path: &Path,
) -> Result<SvgExport, DocumentError> {
    let report = vector_svg(document, tiles)?;
    super::write_atomically(path, report.svg.as_bytes())
        .map_err(crate::import::ImportError::from)?;
    Ok(report)
}

/// A copy of `document` in which only `unit` - one layer, or a clipping
/// base followed by the layers clipped to it - is drawn. Every other layer
/// is hidden except the unit's ancestors, which stay visible with their
/// opacity and fill opacity at 100%: the SVG applies a group's opacity once,
/// as the `<g opacity>` around everything inside it. The unit's subtrees and
/// its clipped layers keep their own visibility, so a hidden clipper stays
/// hidden.
fn staged_unit(document: &Document, unit: &[LayerId]) -> Document {
    let mut staged = document.clone();
    let mut ancestors = HashSet::new();
    let mut cursor = unit.first().and_then(|id| staged.layers.parent_of(*id));
    while let Some(id) = cursor {
        ancestors.insert(id);
        cursor = staged.layers.parent_of(id);
    }
    let mut kept = HashSet::new();
    let mut stack: Vec<LayerId> = unit.to_vec();
    while let Some(id) = stack.pop() {
        if kept.insert(id) {
            if let Some(l) = staged.layers.get(id) {
                stack.extend(l.children().iter().copied());
            }
        }
    }
    for id in staged.layers.iter_depth_first() {
        if kept.contains(&id) {
            continue;
        }
        if let Some(l) = staged.layers.get_mut(id) {
            if ancestors.contains(&id) {
                l.visible = true;
                l.opacity = 1.0;
                l.fill_opacity = 1.0;
            } else {
                l.visible = false;
            }
        }
    }
    staged
}

/// `true` when SVG source-over, `<g opacity>` and per-unit images can
/// reproduce the stack; see the module docs.
fn stackable(document: &Document) -> bool {
    let layers = &document.layers;
    layers.iter_depth_first().into_iter().all(|id| {
        layers.get(id).is_some_and(|l| {
            !l.visible
                || (!matches!(l.kind, LayerKind::Adjustment(_))
                    && l.blend_mode == BlendMode::Normal
                    && match &l.kind {
                        LayerKind::Group(_) => {
                            l.effects.is_empty()
                                && l.mask.is_none()
                                && layer_model::artboard::artboard_of(layers, id).is_none()
                                && l.transform == glam::Affine2::IDENTITY
                        }
                        _ => true,
                    })
        })
    })
}

/// The stacking walk. It mirrors the compositor's own traversal
/// (`composite_ids`), bottom to top, so a clipping run is found exactly
/// where the compositor finds it.
struct Writer<'a> {
    document: &'a Document,
    tiles: &'a MemoryTileSource,
    body: String,
    report: SvgExport,
}

impl<'a> Writer<'a> {
    /// One sibling list, top-most first as the tree stores it.
    fn list(&mut self, ids: &'a [LayerId]) -> Result<(), DocumentError> {
        let layers = &self.document.layers;
        let mut i = ids.len();
        while i > 0 {
            i -= 1;
            let base_id = ids[i];
            let Some(base) = layers.get(base_id) else {
                continue;
            };
            if base.clipping == ClippingMode::ClipToBelow {
                // Nothing non-clipping lies beneath it in this list, so the
                // compositor draws it as a plain layer; so does the SVG.
                self.layer(base_id)?;
                continue;
            }
            let mut clipped = Vec::new();
            while i > 0
                && layers
                    .get(ids[i - 1])
                    .is_some_and(|l| l.clipping == ClippingMode::ClipToBelow)
            {
                i -= 1;
                clipped.push(ids[i]);
            }
            // A hidden base hides everything clipped to it.
            if base.is_noop() {
                continue;
            }
            let any_clipper_drawn = clipped
                .iter()
                .any(|id| layers.get(*id).is_some_and(|l| !l.is_noop()));
            if any_clipper_drawn {
                // SVG has no Porter-Duff atop: the base and its clipped run
                // go out as one compositor render.
                let mut unit = vec![base_id];
                unit.extend(clipped);
                self.image(&unit)?;
            } else {
                self.layer(base_id)?;
            }
        }
        Ok(())
    }

    /// One layer that is not drawn as part of a clipping run.
    fn layer(&mut self, id: LayerId) -> Result<(), DocumentError> {
        let document = self.document;
        let Some(layer) = document.layers.get(id) else {
            return Ok(());
        };
        if layer.is_noop() {
            return Ok(());
        }
        let opacity = layer.effective_opacity();
        match &layer.kind {
            LayerKind::Group(group) => {
                // Applied once, to the group's finished content, as the
                // compositor does for an isolated group. A Normal
                // pass-through group lerps (children over the backdrop) with
                // the backdrop by its opacity, which for source-over
                // children is the same picture.
                let group_opacity = opacity * layer.effective_fill_opacity();
                let wrap = group_opacity < 1.0;
                if wrap {
                    let _ = writeln!(self.body, "<g opacity=\"{}\">", num(group_opacity));
                }
                self.list(&group.children)?;
                if wrap {
                    self.body.push_str("</g>\n");
                }
            }
            _ => {
                if let Some(path) = shape_path(layer, opacity) {
                    self.body.push_str(&path);
                    self.body.push('\n');
                    self.report.paths += 1;
                } else if let Some(text) = text_element(layer, opacity, &document.meta.color_space)
                {
                    self.body.push_str(&text);
                    self.body.push('\n');
                    self.report.texts += 1;
                } else {
                    self.image(&[id])?;
                }
            }
        }
        Ok(())
    }

    /// A unit rendered by the compositor on its own and embedded at canvas
    /// size; nothing is written when the render is fully transparent.
    fn image(&mut self, unit: &[LayerId]) -> Result<(), DocumentError> {
        let (w, h) = (self.document.width(), self.document.height());
        let staged = staged_unit(self.document, unit);
        let rgba = compositor::composite_region(
            &staged,
            self.tiles,
            raster::PixelRect::new(0, 0, w, h),
            0,
            compositor::CompositeOptions::default(),
        )?
        .to_rgba8(&self.document.meta.color_space);
        if rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            return Ok(());
        }
        self.body.push_str(&image_element(w, h, &rgba)?);
        self.body.push('\n');
        self.report.images += 1;
        Ok(())
    }
}

/// A snapshot (document + its pixels) as an SVG; see the module docs for
/// what becomes what.
pub fn vector_svg(
    document: &Document,
    tiles: &MemoryTileSource,
) -> Result<SvgExport, DocumentError> {
    let (w, h) = (document.width(), document.height());
    let (body, mut report) = if stackable(document) {
        let mut writer = Writer {
            document,
            tiles,
            body: String::new(),
            report: SvgExport::default(),
        };
        writer.list(document.layers.root())?;
        (writer.body, writer.report)
    } else {
        let rgba = compositor::composite_region(
            document,
            tiles,
            raster::PixelRect::new(0, 0, w, h),
            0,
            compositor::CompositeOptions::default(),
        )?
        .to_rgba8(&document.meta.color_space);
        let mut body = image_element(w, h, &rgba)?;
        body.push('\n');
        let report = SvgExport {
            images: 1,
            flattened: true,
            ..SvgExport::default()
        };
        (body, report)
    };
    report.svg = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{w}\" height=\"{h}\" \
         viewBox=\"0 0 {w} {h}\">\n{body}</svg>\n"
    );
    Ok(report)
}

fn image_element(w: u32, h: u32, rgba: &[u8]) -> Result<String, DocumentError> {
    let png = raster::encode(raster::ExportFormat::Png, w, h, rgba)?;
    Ok(format!(
        "<image width=\"{w}\" height=\"{h}\" preserveAspectRatio=\"none\" \
         xlink:href=\"data:image/png;base64,{}\"/>",
        base64(&png)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::DocumentId;
    use editor_core::Command;

    fn solid_rect_shape(fill: [f32; 4]) -> Layer {
        let mut layer = Layer::with_kind(
            "Rect",
            LayerKind::Shape(layer_model::ShapeLayer {
                path_svg: "M 0 0 L 10 0 L 10 6 L 0 6 Z".to_string(),
                fill: Some(fill),
                ..layer_model::ShapeLayer::default()
            }),
        );
        layer.transform = glam::Affine2::from_translation(glam::Vec2::new(4.0, 2.0));
        layer
    }

    /// A document with a grey raster background, a red rectangle shape on
    /// it and a line of text on top, exported through `export_to` (the
    /// by-name export route): the shape is a real `<path>`, the text a real
    /// `<text>`, the background an embedded PNG, and rendering the SVG back
    /// (resvg, through the codec's SVG import) reproduces the composite
    /// wherever there is no text.
    #[test]
    fn shapes_become_paths_text_becomes_text_and_rasters_become_images() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = OpenDocument::blank(DocumentId(7001), 32, 24, "svg", 32).unwrap();
        // The blank document's background is one raster layer; paint it.
        let imported = crate::import::document_from_image(
            &crate::import::DecodedImage {
                width: 32,
                height: 24,
                rgba8: [90u8, 90, 90, 255].repeat(32 * 24),
                color_space: color::ColorSpace::Srgb,
                icc_profile: None,
            },
            "svg",
            32,
        )
        .unwrap();
        doc = OpenDocument::from_import(doc.id(), imported);
        doc.apply(Command::create_layer(solid_rect_shape([
            1.0, 0.0, 0.0, 1.0,
        ])))
        .unwrap();
        let mut text = Layer::with_kind(
            "Caption",
            LayerKind::Text(layer_model::TextLayer {
                text: "Hi & <bye>\nline two".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 5.0,
                ..layer_model::TextLayer::default()
            }),
        );
        // Below the rectangle, so the samples below never meet its glyphs.
        text.transform = glam::Affine2::from_translation(glam::Vec2::new(0.0, 14.0));
        doc.apply(Command::create_layer(text)).unwrap();

        let out = dir.path().join("vector.svg");
        doc.export_to(&out).unwrap();
        let svg = std::fs::read_to_string(&out).unwrap();
        assert!(
            svg.contains("<path d=\"M 0 0 L 10 0 L 10 6 L 0 6 Z\" transform=\"matrix(1 0 0 1 4 2)\" fill=\"#ff0000\""),
            "{svg}"
        );
        assert!(svg.contains("<text "), "{svg}");
        assert!(svg.contains(">Hi &amp; &lt;bye&gt;</tspan>"), "{svg}");
        assert!(svg.contains(">line two</tspan>"), "{svg}");
        assert_eq!(
            svg.matches("<image ").count(),
            1,
            "only the background is raster"
        );
        let report = doc.vector_svg().unwrap();
        assert_eq!((report.paths, report.texts, report.images), (1, 1, 1));
        assert!(!report.flattened);

        // Render it back: the rectangle and the background match the
        // document's own composite outside the text's rows.
        let rendered = raster::decode_surface_path(&out, raster::ImportLimits::default())
            .unwrap()
            .into_decoded_image();
        assert_eq!((rendered.width, rendered.height), (32, 24));
        let rect = doc.canvas_rect();
        let composite = doc.composite(rect).unwrap();
        // Inside the rectangle, on the background around it, clear of the
        // text (rows 14 and down).
        for (x, y) in [
            (6u32, 4u32),
            (12, 6),
            (4, 2),
            (1, 1),
            (20, 10),
            (30, 3),
            (15, 12),
        ] {
            let i = ((y * 32 + x) * 4) as usize;
            let (a, b) = (&rendered.rgba8[i..i + 4], &composite[i..i + 4]);
            for c in 0..4 {
                assert!(
                    a[c].abs_diff(b[c]) <= 2,
                    "({x},{y}): svg {a:?} vs doc {b:?}"
                );
            }
        }
        // The shape's interior really is the shape's red, from the path.
        let i = ((5 * 32 + 8) * 4) as usize;
        assert_eq!(&rendered.rgba8[i..i + 3], &[255, 0, 0]);
    }

    /// A layer SVG cannot stack (a Multiply blend) sends the whole document
    /// out as one image, and says so.
    #[test]
    fn a_blend_mode_svg_cannot_stack_flattens_to_one_image() {
        let mut doc = OpenDocument::blank(DocumentId(7002), 8, 8, "svg", 32).unwrap();
        let mut shape = solid_rect_shape([0.0, 0.0, 1.0, 1.0]);
        shape.blend_mode = BlendMode::Multiply;
        doc.apply(Command::create_layer(shape)).unwrap();
        let report = doc.vector_svg().unwrap();
        assert!(report.flattened);
        assert_eq!((report.paths, report.images), (0, 1));
        assert!(!report.svg.contains("<path"));
    }

    /// A shape layer covering the whole canvas.
    fn canvas_shape(name: &str, fill: [f32; 4], w: u32, h: u32) -> Layer {
        Layer::with_kind(
            name,
            LayerKind::Shape(layer_model::ShapeLayer {
                path_svg: format!("M 0 0 L {w} 0 L {w} {h} L 0 {h} Z"),
                fill: Some(fill),
                ..layer_model::ShapeLayer::default()
            }),
        )
    }

    /// Export `document` through `export_to` (the by-name export route),
    /// render the SVG back with resvg (the codec's SVG import) and assert
    /// every pixel matches the document's own composite. Returns the report.
    fn exported_svg_matches_the_composite(id: u64, document: Document) -> SvgExport {
        let dir = tempfile::tempdir().unwrap();
        let first = document.layers.root()[0];
        let mut doc = OpenDocument::from_import(
            DocumentId(id),
            crate::import::ImportedDocument {
                document,
                history: editor_core::History::new(),
                tiles: MemoryTileSource::new(),
                layer: first,
            },
        );
        let out = dir.path().join("probe.svg");
        doc.export_to(&out).unwrap();
        let svg = std::fs::read_to_string(&out).unwrap();
        let rendered = raster::decode_surface_path(&out, raster::ImportLimits::default())
            .unwrap()
            .into_decoded_image();
        let rect = doc.canvas_rect();
        let composite = doc.composite(rect).unwrap();
        assert_eq!(rendered.rgba8.len(), composite.len());
        let width = rect.width as usize;
        for (i, (a, b)) in rendered
            .rgba8
            .as_chunks::<4>()
            .0
            .iter()
            .zip(composite.as_chunks::<4>().0)
            .enumerate()
        {
            let close = (0..4).all(|c| a[c].abs_diff(b[c]) <= 3);
            assert!(
                close,
                "({},{}): svg {a:?} vs doc {b:?}\n{svg}",
                i % width,
                i / width
            );
        }
        doc.vector_svg().unwrap()
    }

    /// Round 3, defect 1: a layer clipped to the one below survives SVG
    /// export. A red rectangle is the base and a canvas-sized blue shape is
    /// clipped to it: the exported SVG renders blue inside the rectangle and
    /// nothing outside it, as the composite does, and the pair goes out as
    /// one image.
    #[test]
    fn a_layer_clipped_to_the_one_below_is_written_with_its_base() {
        let mut document = Document::new(20, 12, "clip");
        document
            .layers
            .push_root(solid_rect_shape([1.0, 0.0, 0.0, 1.0]))
            .unwrap();
        let mut clipped = canvas_shape("Blue", [0.0, 0.0, 1.0, 1.0], 20, 12);
        clipped.clipping = ClippingMode::ClipToBelow;
        document.layers.push_root(clipped).unwrap();
        let report = exported_svg_matches_the_composite(7004, document.clone());
        assert_eq!((report.paths, report.images), (0, 1), "{}", report.svg);
        assert!(!report.flattened);

        // The composite really is blue inside the base and clear outside.
        let tiles = MemoryTileSource::new();
        let composite = compositor::composite_region(
            &document,
            &tiles,
            raster::PixelRect::new(0, 0, 20, 12),
            0,
            compositor::CompositeOptions::default(),
        )
        .unwrap()
        .to_rgba8(&document.meta.color_space);
        let at = |x: usize, y: usize| &composite[(y * 20 + x) * 4..(y * 20 + x) * 4 + 4];
        assert_eq!(at(6, 4), &[0, 0, 255, 255]);
        assert_eq!(at(1, 1)[3], 0);
    }

    /// A group at 50% holding two overlapping, fully opaque shapes.
    fn half_opaque_group(blending: layer_model::GroupBlending) -> Document {
        let mut document = Document::new(12, 8, "group");
        let mut group = Layer::group("G");
        group.opacity = 0.5;
        if let LayerKind::Group(g) = &mut group.kind {
            g.blending = blending;
        }
        let gid = document.layers.push_root(group).unwrap();
        // Children are listed top-most first: red at the bottom, blue on
        // top of it.
        document
            .layers
            .insert_at(
                canvas_shape("Red", [1.0, 0.0, 0.0, 1.0], 12, 8),
                Some(gid),
                0,
            )
            .unwrap();
        document
            .layers
            .insert_at(
                canvas_shape("Blue", [0.0, 0.0, 1.0, 1.0], 12, 8),
                Some(gid),
                0,
            )
            .unwrap();
        document
    }

    /// Round 3, defect 2: a group's opacity applies once, to the group as a
    /// whole, not to each child. With the red child fully covered by the
    /// blue one, the composite is half-transparent pure blue; folding 50%
    /// into each child lets red show through and comes out too opaque.
    #[test]
    fn a_group_opacity_applies_once_to_the_whole_group() {
        for blending in [
            layer_model::GroupBlending::Isolated,
            layer_model::GroupBlending::PassThrough,
        ] {
            let report = exported_svg_matches_the_composite(7005, half_opaque_group(blending));
            assert_eq!(report.paths, 2, "{blending:?}");
            assert!(
                report.svg.contains("<g opacity=\"0.5\">"),
                "{blending:?}: {}",
                report.svg
            );
        }
    }

    /// Both together: a clipping run inside a 50% group. The run's image is
    /// rendered without the group's opacity (the `<g>` applies it), so it
    /// is not faded twice.
    #[test]
    fn a_clipping_run_inside_a_translucent_group_is_faded_once() {
        let mut document = Document::new(20, 12, "both");
        let mut group = Layer::group("G");
        group.opacity = 0.5;
        let gid = document.layers.push_root(group).unwrap();
        document
            .layers
            .insert_at(solid_rect_shape([1.0, 0.0, 0.0, 1.0]), Some(gid), 0)
            .unwrap();
        let mut clipped = canvas_shape("Blue", [0.0, 0.0, 1.0, 1.0], 20, 12);
        clipped.clipping = ClippingMode::ClipToBelow;
        document.layers.insert_at(clipped, Some(gid), 0).unwrap();
        let report = exported_svg_matches_the_composite(7006, document);
        assert_eq!((report.paths, report.images), (0, 1), "{}", report.svg);
    }

    /// The rectangle of [`solid_rect_shape`] with a centred stroke.
    fn stroked_rect(fill: [f32; 4], stroke: [f32; 4], width_px: f32) -> Layer {
        let mut layer = solid_rect_shape(fill);
        if let LayerKind::Shape(s) = &mut layer.kind {
            s.stroke = Some(layer_model::ShapeStroke {
                color: stroke,
                width_px,
                align: layer_model::ShapeStrokeAlign::Center,
                ..layer_model::ShapeStroke::default()
            });
        }
        layer
    }

    /// Final-round defect 1: shape fill and stroke colours are encoded values
    /// in the document's space (the compositor decodes them), so the SVG
    /// writes them as they are. Mid-tone colours, not 0/1 primaries, so a
    /// second encoding would show: rendered back with resvg, every pixel
    /// matches the composite.
    #[test]
    fn mid_tone_shape_fill_and_stroke_colours_export_at_the_composite_brightness() {
        let mut document = Document::new(20, 12, "tones");
        document
            .layers
            .push_root(stroked_rect(
                [0.2, 0.4, 0.6, 1.0],
                [0.8, 0.3, 0.1, 1.0],
                2.0,
            ))
            .unwrap();
        let report = exported_svg_matches_the_composite(7007, document);
        assert_eq!((report.paths, report.images), (1, 0), "{}", report.svg);
        assert!(report.svg.contains("fill=\"#336699\""), "{}", report.svg);
        assert!(report.svg.contains("stroke=\"#cc4d1a\""), "{}", report.svg);
    }

    /// Final-round defect 2: fill opacity fades the stroked shape as a whole
    /// (the compositor draws the stroke over the fill, then multiplies the
    /// layer by opacity x fill opacity), so the fill does not show through
    /// the stroke where they overlap.
    #[test]
    fn fill_opacity_fades_a_stroked_shape_as_a_whole() {
        let mut document = Document::new(20, 12, "fill-opacity");
        let mut layer = stroked_rect([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], 4.0);
        layer.fill_opacity = 0.5;
        document.layers.push_root(layer).unwrap();
        let report = exported_svg_matches_the_composite(7008, document.clone());
        assert_eq!(report.paths, 1, "{}", report.svg);
        assert!(report.svg.contains("opacity=\"0.5\"/>"), "{}", report.svg);

        // The composite really is half-transparent pure blue where the
        // stroke covers the fill's edge.
        let tiles = MemoryTileSource::new();
        let composite = compositor::composite_region(
            &document,
            &tiles,
            raster::PixelRect::new(0, 0, 20, 12),
            0,
            compositor::CompositeOptions::default(),
        )
        .unwrap()
        .to_rgba8(&document.meta.color_space);
        let i = (2 * 20 + 4) * 4;
        assert_eq!(&composite[i..i + 4], &[0, 0, 255, 128]);
    }

    #[test]
    fn a_gradient_shape_and_an_outside_stroke_fall_back_to_an_image_of_that_layer() {
        let mut doc = OpenDocument::blank(DocumentId(7003), 16, 16, "svg", 32).unwrap();
        let mut shape = solid_rect_shape([0.0, 1.0, 0.0, 1.0]);
        if let LayerKind::Shape(s) = &mut shape.kind {
            s.stroke = Some(layer_model::ShapeStroke {
                align: layer_model::ShapeStrokeAlign::Outside,
                ..layer_model::ShapeStroke::default()
            });
        }
        doc.apply(Command::create_layer(shape)).unwrap();
        let report = doc.vector_svg().unwrap();
        assert_eq!(report.paths, 0);
        assert!(report.images >= 1);
        assert!(!report.flattened);
    }
}
