//! W18-H: a Pixlr document (`.pxz`), read as Photopea reads it.
//!
//! A `.pxz` is a ZIP archive holding `manifest.json` — the canvas `width`,
//! `height` and `name`, and the `stack` of layers, bottom-most first — and
//! the files the layers name. Each stack entry has a `type`, `name`,
//! `visible`, `opacity` (0-1), a `rect` (`x`, `y`, `w`, `h` and a rotation
//! `r` in degrees about its centre), a `format` and a `content`:
//!
//! | `type` | Opens as |
//! |---|---|
//! | `image` | a raster layer: `content` names a PNG / JPEG / WebP in the archive, placed at the rect's corner at its own size (as Photopea places it). A rotated image, or a `shape` whose `variant` is `svg` (`content` names an SVG in the archive), is drawn into its rect as pixels and the rotation is dropped (listed in the report) |
//! | `shape` | a shape layer: `variant` `rectangle` / `rounded` (`radii`), `ellipse`, `line` (along the rect's top edge) or `path` (`content` is SVG path data, scaled to fill the rect, as Photopea scales it); `format.fill` a `color` or a linear `gradient` (`value.stops`, `value.direction`); `style.outline` a stroke (`size`, `color`); the rotation kept |
//! | `text` | a text layer: `content` the string, `format.font.name` the family, `format.size`, `format.bold`, the fill colour (a gradient's first stop), wrapping in the rect's width; the rotation kept |
//!
//! What does not map is listed, one sentence each, in [`PxzDocument`]'s
//! notes: an embedded font file (`format.font.content`; the family name is
//! used), text alignment and line spacing, and any other `type`,
//! `variant` or fill kind.
//!
//! No published description of the format exists; the layout above is the
//! one Photopea's own reader expects, and this reader is checked against
//! archives built to it, not against files saved by Pixlr.
//!
//! # Untrusted input
//!
//! Entries are inflated through the bounded ZIP reader with a size cap; the
//! manifest is parsed by the bounded JSON parser; the layer count, every
//! image's pixels (against [`ImportLimits`]) and the canvas are checked
//! before anything is allocated for them.

use super::vector_docs::design_files::{
    ellipse_path, parse_json, rect_path, Affine, DesignDocument, DesignKind, DesignNode,
    DesignStroke, Json, StrokeAlign, MAX_JSON_BYTES, MAX_NODES,
};
use super::vector_docs::zip_entry;
use crate::codec::svg_import::layers::{GradientGeometry, ShapeGradient, VectorLayers};
use crate::codec::{decode_surface_bytes, CodecError, ImportFormat, ImportLimits};
use resvg::usvg;

const NAME: &str = "Pixlr PXZ";

fn malformed(what: impl std::fmt::Display) -> CodecError {
    super::malformed(NAME, what)
}

/// `true` when `zip` is a ZIP archive holding a `manifest.json` whose
/// `stack` is a list (Photopea's test is the entry alone; the `stack`
/// keeps an unrelated archive with a manifest from being taken for one).
pub fn looks_like_pxz(zip: &[u8]) -> bool {
    if !zip.starts_with(b"PK\x03\x04") {
        return false;
    }
    match zip_entry(zip, "manifest.json", MAX_JSON_BYTES, NAME) {
        Ok(Some(bytes)) => parse_json(&bytes, "manifest.json")
            .is_ok_and(|m| matches!(m.get("stack"), Some(Json::Arr(_)))),
        _ => false,
    }
}

/// A `.pxz`, as vector layers.
#[derive(Debug, Clone, PartialEq)]
pub struct PxzDocument {
    /// The document's name from the manifest.
    pub name: String,
    /// The layers, on the manifest's canvas; the design's notes list what
    /// did not map.
    pub layers: VectorLayers,
}

/// Straight-alpha sRGB from a CSS colour: `#rgb`, `#rrggbb`, `#rrggbbaa`,
/// `rgb(...)` / `rgba(...)`.
fn css_colour(text: &str) -> Option<[f32; 4]> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix('#') {
        let digit = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok();
        let pair = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        let [r, g, b, a] = match hex.len() {
            3 => [digit(0)? * 17, digit(1)? * 17, digit(2)? * 17, 255],
            6 => [pair(0)?, pair(2)?, pair(4)?, 255],
            8 => [pair(0)?, pair(2)?, pair(4)?, pair(6)?],
            _ => return None,
        };
        return Some([r, g, b, a].map(|v| f32::from(v) / 255.0));
    }
    let lower = t.to_ascii_lowercase();
    let inner = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<f32> = inner
        .split(',')
        .map(|p| p.trim().parse::<f32>().ok().filter(|v| v.is_finite()))
        .collect::<Option<_>>()?;
    let rgb = |v: f32| (v / 255.0).clamp(0.0, 1.0);
    match parts.as_slice() {
        [r, g, b] => Some([rgb(*r), rgb(*g), rgb(*b), 1.0]),
        [r, g, b, a] => Some([rgb(*r), rgb(*g), rgb(*b), a.clamp(0.0, 1.0)]),
        _ => None,
    }
}

fn num(j: &Json, key: &str) -> Option<f64> {
    j.get(key).and_then(Json::as_f64)
}

/// Rotate by `degrees` about `(cx, cy)`, the way Photopea applies a
/// Pixlr rotation.
fn rotation(degrees: f64, cx: f64, cy: f64) -> Affine {
    let (s, c) = (-degrees.to_radians()).sin_cos();
    Affine::translate(cx, cy)
        .then(Affine([c, s, -s, c, 0.0, 0.0]))
        .then(Affine::translate(-cx, -cy))
}

/// The bounds of SVG path data, through `usvg` (a one-path document).
fn path_bounds(d: &str) -> Option<(f64, f64, f64, f64)> {
    if d.len() > 1 << 20 || d.contains(['"', '<', '>', '&']) {
        return None;
    }
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"><path d=\"{d}\"/></svg>"
    );
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let b = tree.root().abs_bounding_box();
    let (x, y, w, h) = (
        f64::from(b.x()),
        f64::from(b.y()),
        f64::from(b.width()),
        f64::from(b.height()),
    );
    (w.is_finite() && h.is_finite()).then_some((x, y, w, h))
}

/// Nearest-neighbour resample of `rgba` (`sw` x `sh`) to `tw` x `th`.
fn resample(rgba: &[u8], sw: u32, sh: u32, tw: u32, th: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(tw as usize * th as usize * 4);
    for y in 0..th {
        let sy = (u64::from(y) * u64::from(sh) / u64::from(th)) as usize;
        for x in 0..tw {
            let sx = (u64::from(x) * u64::from(sw) / u64::from(tw)) as usize;
            let i = (sy * sw as usize + sx) * 4;
            out.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    out
}

struct Reader<'a> {
    zip: &'a [u8],
    limits: ImportLimits,
    pixel_bytes: u64,
    notes: Vec<String>,
    gradients: Vec<(usize, ShapeGradient)>,
}

impl Reader<'_> {
    fn note(&mut self, note: String) {
        if self.notes.len() < 200 && !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    /// The image in entry `wanted`, at `size` when given (else its own).
    fn image(
        &mut self,
        wanted: &str,
        layer: &str,
        size: Option<(u32, u32)>,
    ) -> Result<Option<(u32, u32, Vec<u8>)>, CodecError> {
        let Some(bytes) = zip_entry(self.zip, wanted, self.limits.max_alloc_bytes, NAME)? else {
            self.note(format!(
                "layer {layer:?} names {wanted:?}, which the archive does not hold; it was left out"
            ));
            return Ok(None);
        };
        let surface = decode_surface_bytes(&bytes, self.limits)?;
        let (sw, sh) = (surface.width, surface.height);
        let (tw, th) = size.unwrap_or((sw, sh));
        let cost = (u64::from(sw) * u64::from(sh) + u64::from(tw) * u64::from(th)) * 4;
        self.pixel_bytes = self.pixel_bytes.saturating_add(cost);
        if self.pixel_bytes > self.limits.max_alloc_bytes
            || u64::from(tw) * u64::from(th) > self.limits.max_pixels
        {
            return Err(CodecError::LimitExceeded(format!(
                "the images in this {NAME} file need more than {} bytes",
                self.limits.max_alloc_bytes
            )));
        }
        let rgba = surface.pixels.into_rgba8();
        if (tw, th) == (sw, sh) {
            return Ok(Some((sw, sh, rgba)));
        }
        Ok(Some((tw, th, resample(&rgba, sw, sh, tw, th))))
    }

    /// A shape's fill: a colour, or (recorded against the shape's walk
    /// `index`) a linear gradient across its `w` x `h` box at the file's
    /// direction (Photopea: angle `270 - direction`).
    fn fill(
        &mut self,
        fill: Option<&Json>,
        layer: &str,
        index: usize,
        (w, h): (f64, f64),
    ) -> Option<[f32; 4]> {
        let fill = fill.filter(|f| !matches!(f, Json::Null))?;
        match fill.get("type").and_then(Json::as_str) {
            Some("color") => fill
                .get("value")
                .and_then(Json::as_str)
                .and_then(css_colour),
            Some("gradient") => {
                let value = fill.get("value")?;
                let stops: Vec<(f32, [f32; 4])> = value
                    .get("stops")
                    .map(Json::as_arr)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|s| {
                        let colour = s.get("color").and_then(Json::as_str).and_then(css_colour)?;
                        let at = num(s, "position").unwrap_or(0.0).clamp(0.0, 1.0) as f32;
                        Some((at, colour))
                    })
                    .collect();
                let first = stops.first()?.1;
                let angle = (270.0 - num(value, "direction").unwrap_or(0.0)).to_radians();
                let (dx, dy) = (angle.cos(), -angle.sin());
                let half = (w * dx.abs() + h * dy.abs()) / 2.0;
                let (cx, cy) = (w / 2.0, h / 2.0);
                self.gradients.push((
                    index,
                    ShapeGradient {
                        geometry: GradientGeometry::Linear {
                            from: (cx - dx * half, cy - dy * half),
                            to: (cx + dx * half, cy + dy * half),
                        },
                        stops,
                    },
                ));
                Some(first)
            }
            other => {
                self.note(format!(
                    "layer {layer:?} has a {} fill, which has no equivalent here; it opened unfilled",
                    other.unwrap_or("typeless")
                ));
                None
            }
        }
    }
}

/// Read a `.pxz`.
pub fn read(zip: &[u8], limits: ImportLimits) -> Result<PxzDocument, CodecError> {
    let manifest = zip_entry(zip, "manifest.json", MAX_JSON_BYTES, NAME)?
        .ok_or_else(|| malformed("the archive holds no manifest.json"))?;
    let manifest = parse_json(&manifest, "manifest.json")?;
    let dimension = |key: &str| {
        num(&manifest, key)
            .filter(|v| *v >= 1.0 && *v <= f64::from(u32::MAX))
            .map(|v| v.round() as u32)
            .ok_or_else(|| malformed(format!("the manifest has no usable {key}")))
    };
    let (width, height) = (dimension("width")?, dimension("height")?);
    limits.check_dimensions(width, height)?;
    let name = manifest
        .get("name")
        .and_then(Json::as_str)
        .unwrap_or("Untitled")
        .to_string();
    let Some(Json::Arr(stack)) = manifest.get("stack") else {
        return Err(malformed("the manifest has no layer stack"));
    };
    if stack.len() > MAX_NODES {
        return Err(CodecError::LimitExceeded(format!(
            "this {NAME} file has more than {MAX_NODES} layers"
        )));
    }
    let mut r = Reader {
        zip,
        limits,
        pixel_bytes: 0,
        notes: Vec::new(),
        gradients: Vec::new(),
    };
    let mut nodes = Vec::with_capacity(stack.len());
    for (index, entry) in stack.iter().enumerate() {
        let walk_index = nodes.len();
        if let Some(node) = layer(&mut r, entry, index, walk_index)? {
            nodes.push(node);
        }
    }
    if nodes.is_empty() {
        return Err(CodecError::Unsupported(format!(
            "this {NAME} file holds no layers this build reads"
        )));
    }
    let Reader {
        notes, gradients, ..
    } = r;
    Ok(PxzDocument {
        name,
        layers: VectorLayers {
            design: DesignDocument {
                // `ImportFormat` has no Pixlr variant; the format is only
                // named in the mapping's messages, and this reader has
                // already refused the two cases those messages cover (no
                // layers, a canvas past the limits).
                format: ImportFormat::Png,
                nodes,
                notes,
                opened: "the document".into(),
            },
            width,
            height,
            flattened: 0,
            gradients,
        },
    })
}

fn layer(
    r: &mut Reader<'_>,
    entry: &Json,
    index: usize,
    walk_index: usize,
) -> Result<Option<DesignNode>, CodecError> {
    let name = entry
        .get("name")
        .and_then(Json::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("Layer {}", index + 1));
    let kind = entry.get("type").and_then(Json::as_str).unwrap_or("");
    let rect = entry.get("rect");
    let field = |key: &str| rect.and_then(|rc| num(rc, key));
    let (x, y) = (field("x").unwrap_or(0.0), field("y").unwrap_or(0.0));
    let (w, h) = (
        field("w").unwrap_or(0.0).max(0.0),
        field("h").unwrap_or(0.0).max(0.0),
    );
    let turn = field("r").unwrap_or(0.0);
    let format = entry.get("format");
    let content = entry.get("content").and_then(Json::as_str).unwrap_or("");
    let visible = entry.get("visible").and_then(Json::as_bool).unwrap_or(true);
    let opacity = num(entry, "opacity").unwrap_or(1.0).clamp(0.0, 1.0) as f32;
    let place = Affine::translate(x, y);
    let turned = rotation(turn, x + w / 2.0, y + h / 2.0).then(place);
    let variant = format
        .and_then(|f| f.get("variant"))
        .and_then(Json::as_str)
        .unwrap_or("");
    let node = |kind: DesignKind, transform: Affine, width: f64, height: f64| DesignNode {
        name: name.clone(),
        visible,
        opacity,
        transform,
        width,
        height,
        kind,
    };
    // Pictures: an image, or an SVG shape (Photopea's smart objects).
    let rotated_picture = kind == "image" && turn != 0.0;
    if rotated_picture || (kind == "shape" && variant == "svg") {
        let size = (w.round() >= 1.0 && h.round() >= 1.0).then(|| {
            (
                w.round().min(f64::from(r.limits.max_width)) as u32,
                h.round().min(f64::from(r.limits.max_height)) as u32,
            )
        });
        if turn != 0.0 {
            r.note(format!(
                "layer {name:?} is rotated {turn} degrees as a picture; it opened unrotated in its box"
            ));
        }
        let Some((bw, bh, rgba)) = r.image(content, &name, size)? else {
            return Ok(None);
        };
        return Ok(Some(node(
            DesignKind::Bitmap {
                width: bw,
                height: bh,
                rgba,
            },
            place,
            f64::from(bw),
            f64::from(bh),
        )));
    }
    match kind {
        "image" => {
            let Some((bw, bh, rgba)) = r.image(content, &name, None)? else {
                return Ok(None);
            };
            Ok(Some(node(
                DesignKind::Bitmap {
                    width: bw,
                    height: bh,
                    rgba,
                },
                place,
                f64::from(bw),
                f64::from(bh),
            )))
        }
        "shape" => {
            let (path_svg, transform) = match variant {
                "rectangle" | "rounded" => {
                    let radius = format
                        .and_then(|f| f.get("radii"))
                        .and_then(|v| v.as_f64().or_else(|| v.as_arr().first()?.as_f64()))
                        .unwrap_or(0.0);
                    (rect_path(w, h, radius), turned)
                }
                "ellipse" => (ellipse_path(0.0, 0.0, w, h), turned),
                "line" => (format!("M0 0 L{w} 0"), turned),
                "path" => {
                    let Some((bx, by, bw, bh)) = path_bounds(content) else {
                        r.note(format!(
                            "shape {name:?}'s path data could not be read; it was left out"
                        ));
                        return Ok(None);
                    };
                    let (sx, sy) = (
                        if bw > 0.0 { w / bw } else { 1.0 },
                        if bh > 0.0 { h / bh } else { 1.0 },
                    );
                    let fit = Affine([sx, 0.0, 0.0, sy, -bx * sx, -by * sy]);
                    (content.to_string(), turned.then(fit))
                }
                other => {
                    r.note(format!(
                        "shape {name:?} is a {other:?} shape, which this build does not read; it was left out"
                    ));
                    return Ok(None);
                }
            };
            let fill = if variant == "line" {
                None
            } else {
                r.fill(
                    format.and_then(|f| f.get("fill")),
                    &name,
                    walk_index,
                    (w, h),
                )
            };
            let stroke = entry
                .get("style")
                .and_then(|s| s.get("outline"))
                .and_then(|o| {
                    Some(DesignStroke {
                        color: css_colour(o.get("color")?.as_str()?)?,
                        width: num(o, "size")?.clamp(0.0, 10_000.0) as f32,
                        align: StrokeAlign::Center,
                    })
                });
            let (width, height) = if variant == "path" {
                (w, h)
            } else {
                (w, h.max(if variant == "line" { 1.0 } else { 0.0 }))
            };
            Ok(Some(node(
                DesignKind::Shape {
                    path_svg,
                    fill,
                    stroke,
                    even_odd: false,
                },
                transform,
                width,
                height,
            )))
        }
        "text" => {
            let font = format.and_then(|f| f.get("font"));
            let family = font
                .and_then(|f| f.get("name"))
                .and_then(Json::as_str)
                .unwrap_or("Arial")
                .to_string();
            if font
                .and_then(|f| f.get("content"))
                .and_then(Json::as_str)
                .is_some_and(|c| !c.is_empty())
            {
                r.note(format!(
                    "text {name:?} embeds its font file; the family {family:?} is used instead"
                ));
            }
            let get = |key: &str| format.and_then(|f| f.get(key));
            let size = get("size")
                .and_then(Json::as_f64)
                .unwrap_or(24.0)
                .clamp(0.1, 10_000.0)
                .round() as f32;
            let bold = get("bold").and_then(Json::as_bool).unwrap_or(false);
            let italic = get("italic").and_then(Json::as_bool).unwrap_or(false);
            let fill = get("fill");
            let color = match fill.and_then(|f| f.get("type")).and_then(Json::as_str) {
                Some("color") => fill
                    .and_then(|f| f.get("value"))
                    .and_then(Json::as_str)
                    .and_then(css_colour),
                Some("gradient") => fill
                    .and_then(|f| f.at(&["value", "stops"]))
                    .and_then(|s| s.as_arr().first())
                    .and_then(|s| s.get("color"))
                    .and_then(Json::as_str)
                    .and_then(css_colour),
                _ => None,
            }
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let align = get("align").and_then(Json::as_str).unwrap_or("left");
            if align != "left" {
                r.note(format!(
                    "text {name:?} is aligned {align}; it opened left-aligned"
                ));
            }
            if get("linespace").is_some_and(|v| !matches!(v, Json::Null)) {
                r.note(format!(
                    "text {name:?} sets its own line spacing; it opened with the default leading"
                ));
            }
            Ok(Some(node(
                DesignKind::Text {
                    text: content.to_string(),
                    font_family: family,
                    bold,
                    italic,
                    size,
                    color,
                    box_width: (w >= 1.0).then_some(w as f32),
                },
                turned,
                w,
                h,
            )))
        }
        other => {
            r.note(format!(
                "layer {name:?} is a {other:?} layer, which this build does not read; it was left out"
            ));
            Ok(None)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stored (uncompressed) ZIP of `entries`.
    pub(crate) fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (name, data) in entries {
            let local = out.len() as u32;
            let crc = crc32(data);
            out.extend(b"PK\x03\x04");
            out.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend(crc.to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend_from_slice(data);
            dir.extend(b"PK\x01\x02");
            dir.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            dir.extend(crc.to_le_bytes());
            dir.extend((data.len() as u32).to_le_bytes());
            dir.extend((data.len() as u32).to_le_bytes());
            dir.extend((name.len() as u16).to_le_bytes());
            dir.extend([0u8; 12]);
            dir.extend(local.to_le_bytes());
            dir.extend(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend(b"PK\x05\x06\0\0\0\0");
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((dir.len() as u32).to_le_bytes());
        out.extend(at.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for b in data {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A 2x2 PNG of one colour.
    pub(crate) fn png(rgba: [u8; 4]) -> Vec<u8> {
        crate::codec::encode(crate::codec::ExportFormat::Png, 2, 2, &rgba.repeat(4)).unwrap()
    }

    /// A Pixlr document of an image, a red rounded rectangle with a
    /// stroke, a gradient ellipse, a path and a text, bottom to top.
    pub(crate) fn sample() -> Vec<u8> {
        let manifest = br##"{
          "width": 120, "height": 80, "name": "Poster",
          "stack": [
            {"type": "image", "name": "Photo", "visible": true, "opacity": 1,
             "rect": {"x": 4, "y": 6, "w": 2, "h": 2}, "content": "img/photo.png"},
            {"type": "shape", "name": "Box", "visible": true, "opacity": 0.5,
             "rect": {"x": 10, "y": 20, "w": 40, "h": 30, "r": 0},
             "format": {"variant": "rounded", "radii": 5,
                        "fill": {"type": "color", "value": "#ff0000"}},
             "style": {"outline": {"size": 3, "color": "rgb(0, 0, 255)"}}},
            {"type": "shape", "name": "Blob", "visible": false, "opacity": 1,
             "rect": {"x": 60, "y": 10, "w": 20, "h": 20, "r": 90},
             "format": {"variant": "ellipse",
                        "fill": {"type": "gradient", "value": {"direction": 90,
                          "stops": [{"color": "#000", "position": 0},
                                    {"color": "#fff", "position": 1}]}}}},
            {"type": "shape", "name": "Tri", "rect": {"x": 0, "y": 40, "w": 20, "h": 10},
             "format": {"variant": "path", "fill": {"type": "color", "value": "#00ff00"}},
             "content": "M100 100 L140 100 L120 120 Z"},
            {"type": "text", "name": "Title", "rect": {"x": 5, "y": 60, "w": 100, "h": 20},
             "format": {"font": {"name": "Georgia", "content": "fonts/g.ttf"}, "size": 18,
                        "bold": true, "align": "center",
                        "fill": {"type": "color", "value": "#112233"}},
             "content": "Hello"}
          ]
        }"##;
        stored_zip(&[
            ("manifest.json", manifest.as_slice()),
            ("img/photo.png", &png([9, 8, 7, 255])),
        ])
    }

    #[test]
    fn a_pxz_reads_its_layers_bottom_first() {
        let bytes = sample();
        assert!(looks_like_pxz(&bytes));
        let doc = read(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(doc.name, "Poster");
        let l = &doc.layers;
        assert_eq!((l.width, l.height), (120, 80));
        let names: Vec<&str> = l.design.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Photo", "Box", "Blob", "Tri", "Title"]);
        let [photo, rect, blob, tri, title] = &l.design.nodes[..] else {
            panic!()
        };
        assert!(
            matches!(&photo.kind, DesignKind::Bitmap { width: 2, height: 2, rgba } if rgba[..4] == [9, 8, 7, 255])
        );
        assert_eq!(photo.transform, Affine::translate(4.0, 6.0));
        let DesignKind::Shape { fill, stroke, .. } = &rect.kind else {
            panic!("{rect:?}")
        };
        assert_eq!(*fill, Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(stroke.as_ref().unwrap().color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(stroke.as_ref().unwrap().width, 3.0);
        assert_eq!(rect.opacity, 0.5);
        assert!((rect.width, rect.height) == (40.0, 30.0));
        assert_eq!(rect.transform.apply(0.0, 0.0), (10.0, 20.0));
        // The ellipse is hidden, turned 90 degrees about its centre, and
        // carries a live gradient keyed by its walk index.
        assert!(!blob.visible);
        let (cx, cy) = blob.transform.apply(10.0, 10.0);
        assert!((cx - 70.0).abs() < 1e-9 && (cy - 20.0).abs() < 1e-9);
        let (ox, oy) = blob.transform.apply(0.0, 0.0);
        assert!(
            (ox - 60.0).abs() < 1e-9 && (oy - 30.0).abs() < 1e-9,
            "{ox},{oy}"
        );
        assert_eq!(l.gradients.len(), 1);
        assert_eq!(l.gradients[0].0, 2);
        assert_eq!(l.gradients[0].1.stops.len(), 2);
        // Direction 90: angle 180, right to left across the box.
        let GradientGeometry::Linear { from, to } = l.gradients[0].1.geometry else {
            panic!()
        };
        assert!(
            (from.0 - 20.0).abs() < 1e-9 && (to.0).abs() < 1e-9,
            "{from:?} {to:?}"
        );
        // The path is fitted to its rect: (100, 100) -> (0, 40).
        let (px, py) = tri.transform.apply(100.0, 100.0);
        assert!(
            (px - 0.0).abs() < 1e-6 && (py - 40.0).abs() < 1e-6,
            "{px},{py}"
        );
        let (qx, qy) = tri.transform.apply(140.0, 120.0);
        assert!(
            (qx - 20.0).abs() < 1e-6 && (qy - 50.0).abs() < 1e-6,
            "{qx},{qy}"
        );
        let DesignKind::Text {
            text,
            font_family,
            bold,
            size,
            color,
            box_width,
            ..
        } = &title.kind
        else {
            panic!()
        };
        assert_eq!(text, "Hello");
        assert_eq!(font_family, "Georgia");
        assert!(*bold);
        assert_eq!(*size, 18.0);
        assert_eq!(*box_width, Some(100.0));
        assert!((color[0] - 17.0 / 255.0).abs() < 1e-6);
        // What did not map: the embedded font and the alignment.
        let notes = &l.design.notes;
        assert!(
            notes.iter().any(|n| n.contains("embeds its font")),
            "{notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("aligned center")),
            "{notes:?}"
        );
    }

    #[test]
    fn unknown_layers_are_noted_and_missing_images_left_out() {
        let manifest = br#"{"width": 10, "height": 10, "stack": [
            {"type": "image", "name": "Gone", "content": "nope.png", "rect": {"x":0,"y":0,"w":1,"h":1}},
            {"type": "sticker", "name": "S"},
            {"type": "shape", "name": "Star", "format": {"variant": "star"}},
            {"type": "shape", "name": "Sq", "rect": {"x":1,"y":1,"w":4,"h":4},
             "format": {"variant": "rectangle", "fill": {"type": "pattern"}}}
        ]}"#;
        let zip = stored_zip(&[("manifest.json", manifest.as_slice())]);
        let doc = read(&zip, ImportLimits::default()).unwrap();
        let names: Vec<&str> = doc
            .layers
            .design
            .nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(names, ["Sq"]);
        let notes = &doc.layers.design.notes;
        assert_eq!(notes.len(), 4, "{notes:?}");
        // A document whose layers all fail is refused.
        let manifest = br#"{"width": 10, "height": 10, "stack": [{"type": "sticker"}]}"#;
        let zip = stored_zip(&[("manifest.json", manifest.as_slice())]);
        assert!(read(&zip, ImportLimits::default()).is_err());
    }

    #[test]
    fn css_colours_parse() {
        assert_eq!(css_colour("#fff"), Some([1.0; 4]));
        assert_eq!(css_colour("#00000080").unwrap()[3], 128.0 / 255.0);
        assert_eq!(
            css_colour("rgba(255, 0, 0, 0.5)"),
            Some([1.0, 0.0, 0.0, 0.5])
        );
        assert_eq!(css_colour("teal"), None);
        assert_eq!(css_colour("#12"), None);
    }

    #[test]
    fn malformed_archives_error_and_never_panic() {
        let good = sample();
        for cut in (0..good.len()).step_by(7) {
            let _ = read(&good[..cut], ImportLimits::default());
        }
        for i in (0..good.len()).step_by(3) {
            let mut f = good.clone();
            f[i] ^= 0x5A;
            let _ = read(&f, ImportLimits::default());
        }
        for manifest in [
            &b"{}"[..],
            b"[]",
            b"{\"width\": 0, \"height\": 5, \"stack\": []}",
            b"{\"width\": 1e12, \"height\": 5, \"stack\": []}",
            b"{\"width\": 5, \"height\": 5, \"stack\": {}}",
            b"{\"width\": 5, \"height\": 5, \"stack\": [{\"type\": \"shape\", \"format\": {\"variant\": \"path\"}, \"content\": \"\\\"/><x\"}]}",
            b"not json",
        ] {
            let zip = stored_zip(&[("manifest.json", manifest)]);
            assert!(read(&zip, ImportLimits::default()).is_err());
        }
        assert!(read(b"PK", ImportLimits::default()).is_err());
        assert!(!looks_like_pxz(b"PK\x03\x04 not a zip"));
        // A zip with no manifest, or a manifest without a stack.
        let zip = stored_zip(&[("other.txt", b"hi")]);
        assert!(!looks_like_pxz(&zip) && read(&zip, ImportLimits::default()).is_err());
        let zip = stored_zip(&[("manifest.json", br#"{"version": 1}"#)]);
        assert!(!looks_like_pxz(&zip));
    }
}
