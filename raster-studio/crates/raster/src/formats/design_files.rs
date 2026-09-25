//! W13X-8: Sketch, Adobe XD and Figma documents read as their **layers**
//! (artboards, groups, vector shapes, text, bitmaps), the way Photopea opens
//! them, not as the embedded preview [`super`] falls back to.
//!
//! This module parses; `app_shell::import_design` maps the result onto the
//! document model. What it returns is a format-neutral tree
//! ([`DesignDocument`]): every node carries its absolute placement as an
//! affine ([`Affine`], node space to document space), its size, name,
//! visibility and opacity, and one of five kinds ([`DesignKind`]).
//!
//! | Format | Read from | Artboards | Groups | Shapes | Text | Bitmaps |
//! | --- | --- | --- | --- | --- | --- | --- |
//! | Sketch | `document.json` + `pages/<id>.json` (first page) | `artboard`, `symbolMaster` | `group`; a `shapeGroup` becomes one shape (its paths united) | `rectangle`, `oval`, `shapePath`, `triangle`, `star`, `polygon` (their Bezier points) | `text` (string, font, size, colour of the first run) | `bitmap` (its `images/…` entry) |
//! | Adobe XD | `artwork/*/graphicContent.agc` (+ `resources/graphics/graphicContent.agc` for artboard bounds) | `artboard` items | `group` | `rect`, `ellipse`, `circle`, `line`, `path`, `compound`, `polygon` | `text` (`rawText`, font, size, fill) | a shape filled with an image `pattern` (its `resources/<uid>` entry) |
//! | Figma | `canvas.fig` (inside the ZIP) or a bare `fig-kiwi` file: see [`super::design_fig`] | `FRAME` directly on a page | `GROUP`, nested `FRAME` | `RECTANGLE`, `ROUNDED_RECTANGLE`, `ELLIPSE`, `LINE`, `REGULAR_POLYGON`, `STAR`, `VECTOR` (as its bounding box) | `TEXT` | an `IMAGE` fill (`images/<hash>` in the ZIP) |
//!
//! # What is not read, and says so
//!
//! Every property the mapping recognises and drops is written to
//! [`DesignDocument::notes`] (the import report), never skipped silently:
//! layer and per-fill blend modes (they open as Normal), clipping masks
//! (the masked layers open unclipped), gradient / image fills other
//! than the bitmap case, more than one fill or border, shadows and blurs,
//! boolean operations other than union, symbol instances (not expanded),
//! rotated or scaled bitmaps (placed unrotated at their frame size), text
//! with more than one style run (the first run's style is used), pages
//! other than the first, and any layer class this reader does not know.
//!
//! # Untrusted input
//!
//! Every entry is inflated through the capped ZIP reader of [`super`]; each
//! JSON entry is at most [`MAX_JSON_BYTES`], the parser nests at most
//! [`MAX_JSON_DEPTH`] deep, the layer tree at most [`MAX_LAYER_DEPTH`] deep
//! and [`MAX_NODES`] nodes in all, and bitmaps are decoded under the
//! caller's [`ImportLimits`] with their total pixel bytes held to its
//! allocation ceiling. Malformed JSON is an error, never a panic.

use super::super::malformed;
use crate::codec::{decode_surface_bytes, CodecError, ImportFormat, ImportLimits};

/// Largest JSON entry read, inflated.
pub const MAX_JSON_BYTES: u64 = 64 << 20;
/// Deepest JSON nesting the parser follows.
pub const MAX_JSON_DEPTH: usize = 256;
/// Deepest layer nesting read.
pub const MAX_LAYER_DEPTH: usize = 64;
/// Most nodes one document may hold.
pub const MAX_NODES: usize = 20_000;

// --------------------------------------------------------------------- model

/// A 2D affine `[a, b, c, d, e, f]`: `x' = a x + c y + e`, `y' = b x + d y + f`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine(pub [f64; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn translate(x: f64, y: f64) -> Affine {
        Affine([1.0, 0.0, 0.0, 1.0, x, y])
    }

    /// `self` after `inner`: a point goes through `inner` first.
    pub fn then(self, inner: Affine) -> Affine {
        let [a1, b1, c1, d1, e1, f1] = self.0;
        let [a2, b2, c2, d2, e2, f2] = inner.0;
        Affine([
            a1 * a2 + c1 * b2,
            b1 * a2 + d1 * b2,
            a1 * c2 + c1 * d2,
            b1 * c2 + d1 * d2,
            a1 * e2 + c1 * f2 + e1,
            b1 * e2 + d1 * f2 + f1,
        ])
    }

    /// `(x, y)` mapped.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    /// `true` when the linear part is the identity (a pure translation).
    pub fn is_translation(&self) -> bool {
        let [a, b, c, d, ..] = self.0;
        (a - 1.0).abs() < 1e-9 && b.abs() < 1e-9 && c.abs() < 1e-9 && (d - 1.0).abs() < 1e-9
    }

    fn is_finite(&self) -> bool {
        self.0.iter().all(|v| v.is_finite())
    }
}

/// Where a stroke sits relative to its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrokeAlign {
    Center,
    Inside,
    Outside,
}

/// A solid outline.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignStroke {
    /// Straight-alpha sRGB, 0..1.
    pub color: [f32; 4],
    pub width: f32,
    pub align: StrokeAlign,
}

/// What a node is.
#[derive(Debug, Clone, PartialEq)]
pub enum DesignKind {
    /// A board: its rect is `(0, 0, width, height)` in node space.
    Artboard {
        /// Straight-alpha sRGB; `None` is transparent.
        background: Option<[f32; 4]>,
        /// Paint order: the first child is the bottom-most.
        children: Vec<DesignNode>,
    },
    Group {
        /// Paint order: the first child is the bottom-most.
        children: Vec<DesignNode>,
    },
    /// A vector shape, its path SVG path data in node space.
    Shape {
        path_svg: String,
        /// Straight-alpha sRGB; `None` is unfilled.
        fill: Option<[f32; 4]>,
        stroke: Option<DesignStroke>,
        even_odd: bool,
    },
    /// Text whose layout box starts at the node origin.
    Text {
        text: String,
        font_family: String,
        bold: bool,
        italic: bool,
        size: f32,
        /// Straight-alpha sRGB.
        color: [f32; 4],
        /// `Some(width)` for text that wraps in a fixed-width box.
        box_width: Option<f32>,
    },
    /// Pixels, `width` x `height` RGBA8, drawn at the node origin.
    Bitmap {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

/// One layer of the design.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignNode {
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    /// Node space to document space.
    pub transform: Affine,
    pub width: f64,
    pub height: f64,
    pub kind: DesignKind,
}

impl DesignNode {
    /// The children of an artboard or group; empty otherwise.
    pub fn children(&self) -> &[DesignNode] {
        match &self.kind {
            DesignKind::Artboard { children, .. } | DesignKind::Group { children } => children,
            _ => &[],
        }
    }

    /// The node's frame corners in document space.
    pub fn corners(&self) -> [(f64, f64); 4] {
        let t = &self.transform;
        [
            t.apply(0.0, 0.0),
            t.apply(self.width, 0.0),
            t.apply(0.0, self.height),
            t.apply(self.width, self.height),
        ]
    }
}

/// A design file read as layers.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignDocument {
    pub format: ImportFormat,
    /// Top-level nodes, in paint order (first is bottom-most).
    pub nodes: Vec<DesignNode>,
    /// What did not map, one sentence each, in the order noticed.
    pub notes: Vec<String>,
    /// Which part of the file was opened (for example `page "Page 1"`).
    pub opened: String,
}

impl DesignDocument {
    /// Every node, depth first.
    pub fn walk(&self) -> Vec<&DesignNode> {
        let mut out = Vec::new();
        let mut stack: Vec<&DesignNode> = self.nodes.iter().rev().collect();
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(n.children().iter().rev());
        }
        out
    }
}

// ---------------------------------------------------------------------- JSON

/// A parsed JSON value. Objects keep their key order.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Follow `keys` through nested objects.
    pub fn at(&self, keys: &[&str]) -> Option<&Json> {
        keys.iter().try_fold(self, |j, k| j.get(k))
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            Json::Num(n) => Some(*n != 0.0),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => &[],
        }
    }

    fn num(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Json::as_f64)
    }

    fn num_or(&self, key: &str, default: f64) -> f64 {
        self.num(key).unwrap_or(default)
    }

    fn str_of(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Json::as_str)
    }

    fn arr(&self, key: &str) -> &[Json] {
        self.get(key).map(Json::as_arr).unwrap_or(&[])
    }

    fn flag(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(Json::as_bool).unwrap_or(default)
    }
}

/// Parse `bytes` as JSON. `what` names the entry in messages.
pub fn parse_json(bytes: &[u8], what: &str) -> Result<Json, CodecError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut p = JsonParser {
        b: bytes,
        i: 0,
        what,
    };
    let value = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("trailing bytes after the value"));
    }
    Ok(value)
}

struct JsonParser<'a> {
    b: &'a [u8],
    i: usize,
    what: &'a str,
}

impl JsonParser<'_> {
    fn err(&self, why: &str) -> CodecError {
        malformed(
            "JSON",
            format!("{} is not valid JSON at byte {}: {why}", self.what, self.i),
        )
    }

    fn ws(&mut self) {
        while self
            .b
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &[u8]) -> bool {
        if self.b[self.i..].starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, CodecError> {
        if depth > MAX_JSON_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        self.ws();
        let Some(&c) = self.b.get(self.i) else {
            return Err(self.err("unexpected end"));
        };
        match c {
            b'{' => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.eat(b"}") {
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err(self.err("expected a key"));
                    }
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(b":") {
                        return Err(self.err("expected ':'"));
                    }
                    let v = self.value(depth + 1)?;
                    fields.push((key, v));
                    self.ws();
                    if self.eat(b",") {
                        continue;
                    }
                    if self.eat(b"}") {
                        return Ok(Json::Obj(fields));
                    }
                    return Err(self.err("expected ',' or '}'"));
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(b"]") {
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b",") {
                        continue;
                    }
                    if self.eat(b"]") {
                        return Ok(Json::Arr(items));
                    }
                    return Err(self.err("expected ',' or ']'"));
                }
            }
            b'"' => self.string().map(Json::Str),
            b't' if self.eat(b"true") => Ok(Json::Bool(true)),
            b'f' if self.eat(b"false") => Ok(Json::Bool(false)),
            b'n' if self.eat(b"null") => Ok(Json::Null),
            b'-' | b'0'..=b'9' => {
                let start = self.i;
                while self
                    .b
                    .get(self.i)
                    .is_some_and(|c| matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
                {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[start..self.i])
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(Json::Num)
                    .ok_or_else(|| self.err("a malformed number"))
            }
            _ => Err(self.err("unexpected character")),
        }
    }

    fn hex4(&mut self) -> Result<u32, CodecError> {
        let s = self
            .b
            .get(self.i..self.i + 4)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .ok_or_else(|| self.err("a malformed \\u escape"))?;
        self.i += 4;
        Ok(s)
    }

    fn string(&mut self) -> Result<String, CodecError> {
        self.i += 1; // the opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return Err(self.err("an unterminated string"));
            };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.b.get(self.i) else {
                        return Err(self.err("an unterminated escape"));
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) && self.eat(b"\\u") {
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    0xFFFD
                                }
                            } else {
                                hi
                            };
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        }
                        _ => return Err(self.err("an unknown escape")),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.err("a string that is not UTF-8"))
    }
}

// ----------------------------------------------------------------------- ZIP

/// Every entry name in the archive's central directory (at most 65 535).
pub fn zip_names(zip: &[u8], name: &str) -> Result<Vec<String>, CodecError> {
    let le16 = |at: usize| -> Result<usize, CodecError> {
        zip.get(at..at + 2)
            .map(|s| usize::from(u16::from_le_bytes([s[0], s[1]])))
            .ok_or_else(|| malformed(name, "the archive ends inside a record"))
    };
    let le32 = |at: usize| -> Result<usize, CodecError> {
        zip.get(at..at + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
            .ok_or_else(|| malformed(name, "the archive ends inside a record"))
    };
    if zip.len() < 22 {
        return Err(malformed(name, "too short to be a ZIP archive"));
    }
    let floor = zip.len().saturating_sub(22 + 0xFFFF);
    let end = (floor..=zip.len() - 22)
        .rev()
        .find(|&i| &zip[i..i + 4] == b"PK\x05\x06")
        .ok_or_else(|| malformed(name, "no ZIP end-of-directory record"))?;
    let count = le16(end + 10)?;
    let mut at = le32(end + 16)?;
    let mut names = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        if zip.get(at..at + 4) != Some(b"PK\x01\x02".as_slice()) {
            return Err(malformed(name, "a central directory record is damaged"));
        }
        let name_len = le16(at + 28)?;
        let skip = name_len + le16(at + 30)? + le16(at + 32)?;
        let entry = zip
            .get(at + 46..at + 46 + name_len)
            .ok_or_else(|| malformed(name, "an entry name runs past the file"))?;
        names.push(String::from_utf8_lossy(entry).into_owned());
        at += 46 + skip;
    }
    Ok(names)
}

/// The shared state of one read: the archive, the budgets, the notes.
pub(super) struct Reader<'a> {
    pub zip: &'a [u8],
    pub format: ImportFormat,
    pub limits: ImportLimits,
    pub notes: Vec<String>,
    pub nodes: usize,
    pub pixel_bytes: u64,
}

impl<'a> Reader<'a> {
    pub fn new(zip: &'a [u8], format: ImportFormat, limits: ImportLimits) -> Self {
        Reader {
            zip,
            format,
            limits,
            notes: Vec::new(),
            nodes: 0,
            pixel_bytes: 0,
        }
    }

    fn name(&self) -> &'static str {
        self.format.name()
    }

    pub fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        if self.notes.len() < 200 && !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    pub fn entry(&self, wanted: &str) -> Result<Option<Vec<u8>>, CodecError> {
        super::zip_entry(self.zip, wanted, MAX_JSON_BYTES, self.name())
    }

    pub fn json(&self, wanted: &str) -> Result<Option<Json>, CodecError> {
        match self.entry(wanted)? {
            Some(bytes) => parse_json(&bytes, wanted).map(Some),
            None => Ok(None),
        }
    }

    /// Count one more node against [`MAX_NODES`], and check the depth.
    pub fn count(&mut self, depth: usize) -> Result<(), CodecError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(CodecError::LimitExceeded(format!(
                "this {} file has more than {MAX_NODES} layers",
                self.name()
            )));
        }
        if depth > MAX_LAYER_DEPTH {
            return Err(CodecError::LimitExceeded(format!(
                "this {} file nests layers more than {MAX_LAYER_DEPTH} deep",
                self.name()
            )));
        }
        Ok(())
    }

    /// Decode the image in entry `wanted`, resampled (nearest) to the frame
    /// `(w, h)` when that differs; `None` (with a note) when it is missing.
    pub fn bitmap(
        &mut self,
        wanted: &str,
        layer: &str,
        (w, h): (f64, f64),
    ) -> Result<Option<(u32, u32, Vec<u8>)>, CodecError> {
        let Some(bytes) =
            super::zip_entry(self.zip, wanted, self.limits.max_alloc_bytes, self.name())?
        else {
            self.note(format!(
                "image layer {layer:?} names {wanted}, which the file does not hold; it was left out"
            ));
            return Ok(None);
        };
        let surface = decode_surface_bytes(&bytes, self.limits)?;
        let (sw, sh) = (surface.width, surface.height);
        let rgba = surface.pixels.into_rgba8();
        let (tw, th) = (w.round(), h.round());
        let (tw, th) = if tw >= 1.0 && th >= 1.0 {
            (
                tw.min(f64::from(self.limits.max_width)) as u32,
                th.min(f64::from(self.limits.max_height)) as u32,
            )
        } else {
            (sw, sh)
        };
        let bytes_needed = u64::from(tw) * u64::from(th) * 4 + u64::from(sw) * u64::from(sh) * 4;
        self.pixel_bytes = self.pixel_bytes.saturating_add(bytes_needed);
        if self.pixel_bytes > self.limits.max_alloc_bytes
            || u64::from(tw) * u64::from(th) > self.limits.max_pixels
        {
            return Err(CodecError::LimitExceeded(format!(
                "the images in this {} file need more than {} bytes",
                self.name(),
                self.limits.max_alloc_bytes
            )));
        }
        if (tw, th) == (sw, sh) {
            return Ok(Some((sw, sh, rgba)));
        }
        let mut out = Vec::with_capacity(tw as usize * th as usize * 4);
        for y in 0..th {
            let sy = (u64::from(y) * u64::from(sh) / u64::from(th)) as usize;
            for x in 0..tw {
                let sx = (u64::from(x) * u64::from(sw) / u64::from(tw)) as usize;
                let i = (sy * sw as usize + sx) * 4;
                out.extend_from_slice(&rgba[i..i + 4]);
            }
        }
        Ok(Some((tw, th, out)))
    }
}

// ------------------------------------------------------------------ geometry

fn num(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == 0.0 {
        "0".into()
    } else {
        format!("{r}")
    }
}

/// A `w` x `h` rectangle with corner radius `r` (clamped to fit).
pub fn rect_path(w: f64, h: f64, r: f64) -> String {
    let r = r.max(0.0).min(w / 2.0).min(h / 2.0);
    if r <= 0.0 {
        return format!("M0 0 L{} 0 L{} {} L0 {} Z", num(w), num(w), num(h), num(h));
    }
    let k = r * (1.0 - 0.552_284_75);
    let (w_, h_) = (num(w), num(h));
    format!(
        "M{r0} 0 L{wr} 0 C{wk} 0 {w_} {k0} {w_} {r0} L{w_} {hr} C{w_} {hk} {wk} {h_} {wr} {h_} \
         L{r0} {h_} C{k0} {h_} 0 {hk} 0 {hr} L0 {r0} C0 {k0} {k0} 0 {r0} 0 Z",
        r0 = num(r),
        wr = num(w - r),
        wk = num(w - k),
        k0 = num(k),
        hr = num(h - r),
        hk = num(h - k),
    )
}

/// An ellipse inscribed in `(x, y, w, h)`, as four cubic arcs.
pub fn ellipse_path(x: f64, y: f64, w: f64, h: f64) -> String {
    let (rx, ry) = (w / 2.0, h / 2.0);
    let (cx, cy) = (x + rx, y + ry);
    let (kx, ky) = (rx * 0.552_284_75, ry * 0.552_284_75);
    format!(
        "M{} {} C{} {} {} {} {} {} C{} {} {} {} {} {} C{} {} {} {} {} {} C{} {} {} {} {} {} Z",
        num(cx + rx),
        num(cy),
        num(cx + rx),
        num(cy + ky),
        num(cx + kx),
        num(cy + ry),
        num(cx),
        num(cy + ry),
        num(cx - kx),
        num(cy + ry),
        num(cx - rx),
        num(cy + ky),
        num(cx - rx),
        num(cy),
        num(cx - rx),
        num(cy - ky),
        num(cx - kx),
        num(cy - ry),
        num(cx),
        num(cy - ry),
        num(cx + kx),
        num(cy - ry),
        num(cx + rx),
        num(cy - ky),
        num(cx + rx),
        num(cy),
    )
}

// -------------------------------------------------------------------- Sketch

/// `"{0.5, 0.25}"` as a point.
fn sketch_point(s: Option<&str>) -> Option<(f64, f64)> {
    let s = s?.trim().trim_start_matches('{').trim_end_matches('}');
    let (x, y) = s.split_once(',')?;
    let (x, y) = (x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?);
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// Sketch's `{red, green, blue, alpha}` colour.
fn sketch_color(j: Option<&Json>) -> Option<[f32; 4]> {
    let j = j?;
    let c = |k: &str, d: f64| j.num_or(k, d).clamp(0.0, 1.0) as f32;
    j.get("red")?;
    Some([
        c("red", 0.0),
        c("green", 0.0),
        c("blue", 0.0),
        c("alpha", 1.0),
    ])
}

/// The path of a Sketch shape layer's `points`, in node space.
fn sketch_points_path(layer: &Json, w: f64, h: f64, offset: (f64, f64)) -> Option<String> {
    struct P {
        p: (f64, f64),
        from: (f64, f64),
        to: (f64, f64),
        curve_from: bool,
        curve_to: bool,
    }
    let pts: Vec<P> = layer
        .arr("points")
        .iter()
        .filter_map(|pt| {
            let p = sketch_point(pt.str_of("point"))?;
            let from = sketch_point(pt.str_of("curveFrom")).unwrap_or(p);
            let to = sketch_point(pt.str_of("curveTo")).unwrap_or(p);
            let map = |(x, y): (f64, f64)| (offset.0 + x * w, offset.1 + y * h);
            Some(P {
                p: map(p),
                from: map(from),
                to: map(to),
                curve_from: pt.flag("hasCurveFrom", from != p),
                curve_to: pt.flag("hasCurveTo", to != p),
            })
        })
        .collect();
    if pts.len() < 2 {
        return None;
    }
    let mut d = format!("M{} {}", num(pts[0].p.0), num(pts[0].p.1));
    let seg = |d: &mut String, a: &P, b: &P| {
        if a.curve_from || b.curve_to {
            d.push_str(&format!(
                " C{} {} {} {} {} {}",
                num(a.from.0),
                num(a.from.1),
                num(b.to.0),
                num(b.to.1),
                num(b.p.0),
                num(b.p.1)
            ));
        } else {
            d.push_str(&format!(" L{} {}", num(b.p.0), num(b.p.1)));
        }
    };
    for i in 1..pts.len() {
        seg(&mut d, &pts[i - 1], &pts[i]);
    }
    if layer.flag("isClosed", true) {
        seg(&mut d, &pts[pts.len() - 1], &pts[0]);
        d.push_str(" Z");
    }
    Some(d)
}

/// The node-space transform of a Sketch layer inside `parent`.
fn sketch_transform(layer: &Json, parent: Affine) -> Affine {
    let frame = layer.get("frame");
    let f = |k: &str| frame.and_then(|f| f.num(k)).unwrap_or(0.0);
    let (x, y, w, h) = (f("x"), f("y"), f("width"), f("height"));
    let mut t = parent.then(Affine::translate(x, y));
    let rotation = layer.num_or("rotation", 0.0);
    let flip_h = layer.flag("isFlippedHorizontal", false);
    let flip_v = layer.flag("isFlippedVertical", false);
    if rotation.abs() > 1e-6 || flip_h || flip_v {
        // Sketch turns a layer about its frame's centre; a positive angle
        // turns it anticlockwise on screen (y grows down).
        let (s, c) = (-rotation.to_radians()).sin_cos();
        let (sx, sy) = (
            if flip_h { -1.0 } else { 1.0 },
            if flip_v { -1.0 } else { 1.0 },
        );
        let about = Affine::translate(w / 2.0, h / 2.0)
            .then(Affine([c * sx, s * sx, -s * sy, c * sy, 0.0, 0.0]))
            .then(Affine::translate(-w / 2.0, -h / 2.0));
        t = t.then(about);
    }
    t
}

/// Fill and stroke from a Sketch `style`, noting what does not map.
fn sketch_style(
    r: &mut Reader,
    layer: &Json,
    name: &str,
) -> (Option<[f32; 4]>, Option<DesignStroke>) {
    let style = layer.get("style");
    let enabled = |k: &str| -> Vec<&Json> {
        style
            .map(|s| s.arr(k))
            .unwrap_or(&[])
            .iter()
            .filter(|f| f.flag("isEnabled", true))
            .collect()
    };
    let fills = enabled("fills");
    let borders = enabled("borders");
    if fills.len() > 1 {
        r.note(format!(
            "layer {name:?} has {} fills; only the top-most was kept",
            fills.len()
        ));
    }
    if borders.len() > 1 {
        r.note(format!(
            "layer {name:?} has {} borders; only the top-most was kept",
            borders.len()
        ));
    }
    let paint_opacity = |f: &Json| f.at(&["contextSettings", "opacity"]).and_then(Json::as_f64);
    let fill =
        fills
            .last()
            .and_then(|f| match f.num_or("fillType", 0.0) as i64 {
                0 => sketch_color(f.get("color")).map(|mut c| {
                    c[3] *= paint_opacity(f).unwrap_or(1.0).clamp(0.0, 1.0) as f32;
                    c
                }),
                kind => {
                    r.note(format!(
                "layer {name:?} has a {} fill, which was not kept (only flat colour fills are)",
                if kind == 1 { "gradient" } else { "pattern / image" }
            ));
                    None
                }
            });
    let stroke = borders.last().and_then(|b| {
        if b.num_or("fillType", 0.0) as i64 != 0 {
            r.note(format!(
                "layer {name:?} has a gradient border, which was not kept"
            ));
            return None;
        }
        let color = sketch_color(b.get("color"))?;
        Some(DesignStroke {
            color,
            width: b.num_or("thickness", 1.0).clamp(0.0, 10_000.0) as f32,
            align: match b.num_or("position", 0.0) as i64 {
                1 => StrokeAlign::Inside,
                2 => StrokeAlign::Outside,
                _ => StrokeAlign::Center,
            },
        })
    });
    for (key, what) in [
        ("shadows", "a drop shadow"),
        ("innerShadows", "an inner shadow"),
    ] {
        if !enabled(key).is_empty() {
            r.note(format!("layer {name:?} has {what}, which was not kept"));
        }
    }
    if style
        .and_then(|s| s.get("blur"))
        .is_some_and(|b| b.flag("isEnabled", false))
    {
        r.note(format!("layer {name:?} has a blur, which was not kept"));
    }
    (fill, stroke)
}

/// A PostScript font name split into family and bold / italic.
fn split_font_name(name: &str) -> (String, bool, bool) {
    let (family, style) = name.split_once('-').unwrap_or((name, ""));
    let style = style.to_ascii_lowercase();
    let bold = ["bold", "black", "heavy", "semibold", "demibold"]
        .iter()
        .any(|s| style.contains(s));
    let italic = style.contains("italic") || style.contains("oblique");
    (family.to_string(), bold, italic)
}

fn sketch_text(r: &mut Reader, layer: &Json, name: &str, w: f64) -> DesignKind {
    let attributed = layer.get("attributedString");
    let text = attributed
        .and_then(|a| a.str_of("string"))
        .unwrap_or_default()
        .to_string();
    let runs = attributed.map(|a| a.arr("attributes")).unwrap_or(&[]);
    let first = runs
        .first()
        .and_then(|run| run.get("attributes"))
        .or_else(|| layer.at(&["style", "textStyle", "encodedAttributes"]));
    let font = first.and_then(|a| a.at(&["MSAttributedStringFontAttribute", "attributes"]));
    let family = font
        .and_then(|f| f.str_of("name"))
        .unwrap_or("Helvetica")
        .to_string();
    let size = font
        .and_then(|f| f.num("size"))
        .unwrap_or(12.0)
        .clamp(0.5, 4000.0) as f32;
    let color = first
        .and_then(|a| sketch_color(a.get("MSAttributedStringColorAttribute")))
        .unwrap_or([0.0, 0.0, 0.0, 1.0]);
    if runs.len() > 1 {
        let distinct = runs
            .iter()
            .map(|run| run.get("attributes"))
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w[0] != w[1]);
        if distinct {
            r.note(format!(
                "text layer {name:?} has {} differently styled runs; the first run's font, \
                 size and colour were applied to all of it",
                runs.len()
            ));
        }
    }
    let (font_family, bold, italic) = split_font_name(&family);
    DesignKind::Text {
        text,
        font_family,
        bold,
        italic,
        size,
        color,
        box_width: (layer.num_or("textBehaviour", 0.0) as i64 != 0).then_some(w as f32),
    }
}

/// Sketch's blend modes (`contextSettings.blendMode`), by number.
const SKETCH_BLEND_MODES: [&str; 18] = [
    "normal",
    "darken",
    "multiply",
    "color burn",
    "lighten",
    "screen",
    "color dodge",
    "overlay",
    "soft light",
    "hard light",
    "difference",
    "exclusion",
    "hue",
    "saturation",
    "color",
    "luminosity",
    "plus darker",
    "plus lighter",
];

/// The name of a Sketch blend mode number.
fn sketch_blend_name(mode: i64) -> String {
    usize::try_from(mode)
        .ok()
        .and_then(|m| SKETCH_BLEND_MODES.get(m))
        .map(|m| (*m).to_string())
        .unwrap_or_else(|| format!("number {mode}"))
}

/// Layer blend modes, per-fill blend modes and clipping masks: none of them
/// is kept, each is named in the report.
fn sketch_blend_and_mask_notes(r: &mut Reader, layer: &Json, name: &str) {
    let mode = layer
        .at(&["style", "contextSettings", "blendMode"])
        .and_then(Json::as_f64)
        .unwrap_or(0.0) as i64;
    if mode != 0 {
        r.note(format!(
            "layer {name:?} has the {} blend mode, which was not kept (it opened as Normal)",
            sketch_blend_name(mode)
        ));
    }
    for key in ["fills", "borders"] {
        let paints = layer.at(&["style", key]).map(Json::as_arr).unwrap_or(&[]);
        for paint in paints.iter().filter(|p| p.flag("isEnabled", true)) {
            let mode = paint
                .at(&["contextSettings", "blendMode"])
                .and_then(Json::as_f64)
                .unwrap_or(0.0) as i64;
            if mode != 0 {
                r.note(format!(
                    "layer {name:?} has a {} with the {} blend mode, which was not kept \
                     (it opened as Normal)",
                    if key == "fills" { "fill" } else { "border" },
                    sketch_blend_name(mode)
                ));
            }
        }
    }
    if layer.flag("hasClippingMask", false) {
        r.note(format!(
            "layer {name:?} is a clipping mask, which was not kept: it opened as an \
             ordinary layer and the layers above it in its group opened unclipped"
        ));
    }
}

fn sketch_layer(
    r: &mut Reader,
    layer: &Json,
    parent: Affine,
    depth: usize,
) -> Result<Option<DesignNode>, CodecError> {
    r.count(depth)?;
    let class = layer.str_of("_class").unwrap_or("");
    let name = layer.str_of("name").unwrap_or(class).to_string();
    sketch_blend_and_mask_notes(r, layer, &name);
    let frame = layer.get("frame");
    let f = |k: &str| frame.and_then(|f| f.num(k)).unwrap_or(0.0).clamp(0.0, 1e6);
    let (w, h) = (f("width"), f("height"));
    let transform = sketch_transform(layer, parent);
    if !transform.is_finite() {
        return Err(malformed(
            r.name(),
            format!("layer {name:?} has a non-finite frame"),
        ));
    }
    let children = |r: &mut Reader, inner: Affine| -> Result<Vec<DesignNode>, CodecError> {
        let mut out = Vec::new();
        for child in layer.arr("layers") {
            if let Some(n) = sketch_layer(r, child, inner, depth + 1)? {
                out.push(n);
            }
        }
        Ok(out)
    };
    let kind = match class {
        "artboard" | "symbolMaster" if depth == 0 => {
            let background = if layer.flag("hasBackgroundColor", false) {
                sketch_color(layer.get("backgroundColor"))
            } else {
                Some([1.0, 1.0, 1.0, 1.0])
            };
            DesignKind::Artboard {
                background,
                children: children(r, transform)?,
            }
        }
        "artboard" | "symbolMaster" | "group" => DesignKind::Group {
            children: children(r, transform)?,
        },
        "shapeGroup" => {
            let mut paths = Vec::new();
            for child in layer.arr("layers") {
                r.count(depth + 1)?;
                let cf = child.get("frame");
                let g = |k: &str| cf.and_then(|f| f.num(k)).unwrap_or(0.0);
                if child.num_or("rotation", 0.0).abs() > 1e-6 {
                    r.note(format!(
                        "a path inside {name:?} is rotated; it was combined unrotated"
                    ));
                }
                if child.num_or("booleanOperation", -1.0) as i64 > 0 {
                    r.note(format!(
                        "shape {name:?} combines its paths with a subtract / intersect / \
                         difference operation; they were combined as a union"
                    ));
                }
                if let Some(p) = sketch_shape_path(
                    child,
                    g("width").clamp(0.0, 1e6),
                    g("height").clamp(0.0, 1e6),
                    (g("x"), g("y")),
                ) {
                    paths.push(p);
                }
            }
            let (fill, stroke) = sketch_style(r, layer, &name);
            DesignKind::Shape {
                path_svg: paths.join(" "),
                fill,
                stroke,
                even_odd: layer.num_or("windingRule", 1.0) as i64 == 1,
            }
        }
        "rectangle" | "oval" | "shapePath" | "triangle" | "star" | "polygon" => {
            let (fill, stroke) = sketch_style(r, layer, &name);
            let has_radius = layer
                .arr("points")
                .iter()
                .any(|p| p.num_or("cornerRadius", 0.0) > 0.0);
            if has_radius && class != "rectangle" {
                r.note(format!(
                    "shape {name:?} has rounded corners, which were drawn sharp"
                ));
            }
            DesignKind::Shape {
                path_svg: sketch_shape_path(layer, w, h, (0.0, 0.0)).unwrap_or_default(),
                fill,
                stroke,
                even_odd: layer.num_or("windingRule", 1.0) as i64 == 1,
            }
        }
        "text" => sketch_text(r, layer, &name, w),
        "bitmap" => {
            let Some(reference) = layer.at(&["image", "_ref"]).and_then(Json::as_str) else {
                r.note(format!(
                    "image layer {name:?} names no image; it was left out"
                ));
                return Ok(None);
            };
            let mut wanted = reference.to_string();
            if r.entry(&wanted)?.is_none() && !wanted.contains('.') {
                wanted.push_str(".png");
            }
            if !transform.is_translation() {
                r.note(format!(
                    "image layer {name:?} is rotated or flipped; it was placed unrotated"
                ));
            }
            let Some((bw, bh, rgba)) = r.bitmap(&wanted, &name, (w, h))? else {
                return Ok(None);
            };
            DesignKind::Bitmap {
                width: bw,
                height: bh,
                rgba,
            }
        }
        "symbolInstance" => {
            r.note(format!(
                "symbol instance {name:?} was not expanded (symbols are not read); it was left out"
            ));
            return Ok(None);
        }
        "slice" | "MSImmutableHotspotLayer" | "hotspot" => {
            r.note(format!(
                "{} {name:?} is an export / prototyping marker with nothing to draw; it was left out",
                if class == "slice" { "slice" } else { "hotspot" }
            ));
            return Ok(None);
        }
        other => {
            r.note(format!(
                "layer {name:?} is a Sketch {other:?}, which this reader does not know; it was left out"
            ));
            return Ok(None);
        }
    };
    let opacity = layer
        .at(&["style", "contextSettings", "opacity"])
        .and_then(Json::as_f64)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0) as f32;
    Ok(Some(DesignNode {
        name,
        visible: layer.flag("isVisible", true),
        opacity,
        transform,
        width: w,
        height: h,
        kind,
    }))
}

/// The path of one Sketch shape (its points, or its frame for a rectangle
/// or oval that has none), offset by `offset`.
fn sketch_shape_path(layer: &Json, w: f64, h: f64, offset: (f64, f64)) -> Option<String> {
    let class = layer.str_of("_class").unwrap_or("");
    if class == "rectangle" {
        let radius = layer.num("fixedRadius").unwrap_or_else(|| {
            layer
                .arr("points")
                .first()
                .map(|p| p.num_or("cornerRadius", 0.0))
                .unwrap_or(0.0)
        });
        if radius > 0.0 || layer.arr("points").is_empty() {
            let path = rect_path(w, h, radius);
            return Some(if offset == (0.0, 0.0) {
                path
            } else {
                offset_path(&path, offset)
            });
        }
    }
    if let Some(p) = sketch_points_path(layer, w, h, offset) {
        return Some(p);
    }
    match class {
        "rectangle" => Some(offset_path(&rect_path(w, h, 0.0), offset)),
        "oval" => Some(ellipse_path(offset.0, offset.1, w, h)),
        _ => None,
    }
}

/// `path` (absolute commands only, numbers in x y pairs) moved by `offset`.
fn offset_path(path: &str, (dx, dy): (f64, f64)) -> String {
    if dx == 0.0 && dy == 0.0 {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 16);
    let mut is_x = true;
    for token in path.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let (cmd, rest) = match token.chars().next() {
            Some(c) if c.is_ascii_alphabetic() => (Some(c), &token[1..]),
            _ => (None, token),
        };
        if let Some(c) = cmd {
            out.push(c);
            is_x = true;
        }
        if let Ok(v) = rest.parse::<f64>() {
            out.push_str(&num(if is_x { v + dx } else { v + dy }));
            is_x = !is_x;
        }
    }
    out
}

/// A Sketch document's first page, as layers.
pub fn read_sketch(zip: &[u8], limits: ImportLimits) -> Result<DesignDocument, CodecError> {
    let mut r = Reader::new(zip, ImportFormat::Sketch, limits);
    let document = r
        .json("document.json")?
        .ok_or_else(|| malformed("Sketch", "the archive has no document.json"))?;
    let mut pages: Vec<String> = document
        .arr("pages")
        .iter()
        .filter_map(|p| p.str_of("_ref"))
        .map(|p| {
            if p.ends_with(".json") {
                p.to_string()
            } else {
                format!("{p}.json")
            }
        })
        .collect();
    if pages.is_empty() {
        let mut names: Vec<String> = zip_names(zip, "Sketch")?
            .into_iter()
            .filter(|n| n.starts_with("pages/") && n.ends_with(".json"))
            .collect();
        names.sort();
        pages = names;
    }
    let first = pages
        .first()
        .ok_or_else(|| CodecError::Unsupported("this Sketch file has no pages".into()))?;
    let page = r
        .json(first)?
        .ok_or_else(|| malformed("Sketch", format!("{first} is named but missing")))?;
    let page_name = page.str_of("name").unwrap_or("Page 1").to_string();
    if pages.len() > 1 {
        r.note(format!(
            "only the first page ({page_name:?}) was opened; the file's {} other page(s) were not",
            pages.len() - 1
        ));
    }
    let mut nodes = Vec::new();
    for layer in page.arr("layers") {
        if let Some(n) = sketch_layer(&mut r, layer, Affine::IDENTITY, 0)? {
            nodes.push(n);
        }
    }
    Ok(DesignDocument {
        format: ImportFormat::Sketch,
        nodes,
        notes: r.notes,
        opened: format!("page {page_name:?}"),
    })
}

// ------------------------------------------------------------------------ XD

/// XD's `{mode, value: {r, g, b}, alpha}` colour (0-255 channels).
fn xd_color(r: &mut Reader, j: Option<&Json>, name: &str) -> Option<[f32; 4]> {
    let j = j?;
    let mode = j.str_of("mode").unwrap_or("RGB");
    if mode != "RGB" {
        r.note(format!(
            "layer {name:?} uses a {mode} colour, which was read as RGB"
        ));
    }
    let v = j.get("value")?;
    let c = |k: &str| (v.num_or(k, 0.0) / 255.0).clamp(0.0, 1.0) as f32;
    Some([
        c("r"),
        c("g"),
        c("b"),
        j.num_or("alpha", 1.0).clamp(0.0, 1.0) as f32,
    ])
}

fn xd_transform(j: &Json) -> Affine {
    let Some(t) = j.get("transform") else {
        return Affine::IDENTITY;
    };
    let a = Affine([
        t.num_or("a", 1.0),
        t.num_or("b", 0.0),
        t.num_or("c", 0.0),
        t.num_or("d", 1.0),
        t.num_or("tx", 0.0),
        t.num_or("ty", 0.0),
    ]);
    if a.is_finite() {
        a
    } else {
        Affine::IDENTITY
    }
}

fn xd_style_notes(r: &mut Reader, item: &Json, name: &str) {
    let style = item.get("style");
    if let Some(filters) = style.and_then(|s| s.get("filters")) {
        for f in filters.as_arr() {
            if f.flag("visible", true) {
                let what = f.str_of("type").unwrap_or("effect");
                r.note(format!(
                    "layer {name:?} has a {what} effect, which was not kept"
                ));
            }
        }
    }
    if style
        .and_then(|s| s.get("blendMode"))
        .and_then(Json::as_str)
        .is_some_and(|m| m != "normal" && m != "pass-through")
    {
        r.note(format!(
            "layer {name:?} has a blend mode, which opened as Normal"
        ));
    }
    if item.get("mask").is_some() {
        r.note(format!(
            "group {name:?} is masked, which was not kept: its layers opened unclipped"
        ));
    }
}

fn xd_paint(r: &mut Reader, item: &Json, name: &str) -> (Option<[f32; 4]>, Option<DesignStroke>) {
    let style = item.get("style");
    let fill = style.and_then(|s| s.get("fill"));
    let fill = match fill.and_then(|f| f.str_of("type")) {
        None | Some("none") => None,
        Some("solid") => xd_color(r, fill.and_then(|f| f.get("color")), name),
        Some(other) => {
            r.note(format!(
                "layer {name:?} has a {other} fill, which was not kept (only flat colour fills are)"
            ));
            None
        }
    };
    let stroke = style.and_then(|s| s.get("stroke"));
    let stroke = match stroke.and_then(|s| s.str_of("type")) {
        Some("solid") => {
            let s = stroke.unwrap_or(&Json::Null);
            xd_color(r, s.get("color"), name).map(|color| DesignStroke {
                color,
                width: s.num_or("width", 1.0).clamp(0.0, 10_000.0) as f32,
                align: match s.str_of("align") {
                    Some("inside") => StrokeAlign::Inside,
                    Some("outside") => StrokeAlign::Outside,
                    _ => StrokeAlign::Center,
                },
            })
        }
        None | Some("none") => None,
        Some(other) => {
            r.note(format!(
                "layer {name:?} has a {other} stroke, which was not kept"
            ));
            None
        }
    };
    (fill, stroke)
}

/// The shape's path in node space and its box.
fn xd_shape_path(shape: &Json, r: &mut Reader, name: &str) -> Option<(String, f64, f64)> {
    let n = |k: &str| shape.num_or(k, 0.0).clamp(-1e6, 1e6);
    match shape.str_of("type")? {
        "rect" => {
            let (w, h) = (n("width").max(0.0), n("height").max(0.0));
            let radii: Vec<f64> = shape
                .arr("r")
                .iter()
                .filter_map(Json::as_f64)
                .chain(shape.num("r"))
                .collect();
            if radii.windows(2).any(|p| (p[0] - p[1]).abs() > 1e-6) {
                r.note(format!(
                    "rectangle {name:?} has differing corner radii; the first was used for all"
                ));
            }
            let path = rect_path(w, h, radii.first().copied().unwrap_or(0.0));
            Some((offset_path(&path, (n("x"), n("y"))), w, h))
        }
        "ellipse" => {
            let (rx, ry) = (n("rx").abs(), n("ry").abs());
            Some((
                ellipse_path(n("cx") - rx, n("cy") - ry, rx * 2.0, ry * 2.0),
                rx * 2.0,
                ry * 2.0,
            ))
        }
        "circle" => {
            let rr = n("r").abs();
            Some((
                ellipse_path(n("cx") - rr, n("cy") - rr, rr * 2.0, rr * 2.0),
                rr * 2.0,
                rr * 2.0,
            ))
        }
        "line" => Some((
            format!(
                "M{} {} L{} {}",
                num(n("x1")),
                num(n("y1")),
                num(n("x2")),
                num(n("y2"))
            ),
            (n("x2") - n("x1")).abs(),
            (n("y2") - n("y1")).abs(),
        )),
        "polygon" | "polyline" => {
            let pts: Vec<(f64, f64)> = shape
                .arr("points")
                .iter()
                .filter_map(|p| Some((p.num("x")?, p.num("y")?)))
                .collect();
            if pts.len() < 2 {
                return None;
            }
            let mut d = format!("M{} {}", num(pts[0].0), num(pts[0].1));
            for p in &pts[1..] {
                d.push_str(&format!(" L{} {}", num(p.0), num(p.1)));
            }
            if shape.str_of("type") == Some("polygon") {
                d.push_str(" Z");
            }
            let (mx, my) = pts
                .iter()
                .fold((0f64, 0f64), |(a, b), p| (a.max(p.0), b.max(p.1)));
            Some((d, mx, my))
        }
        "path" | "compound" => {
            let d = shape.str_of("path")?.to_string();
            if d.len() > 4 << 20 {
                return None;
            }
            Some((d, n("width").max(0.0), n("height").max(0.0)))
        }
        other => {
            r.note(format!(
                "shape {name:?} is an XD {other:?} shape, which this reader does not know; it was left out"
            ));
            None
        }
    }
}

fn xd_item(
    r: &mut Reader,
    item: &Json,
    parent: Affine,
    depth: usize,
) -> Result<Option<DesignNode>, CodecError> {
    r.count(depth)?;
    let kind_name = item.str_of("type").unwrap_or("");
    let name = item.str_of("name").unwrap_or(kind_name).to_string();
    let transform = parent.then(xd_transform(item));
    xd_style_notes(r, item, &name);
    let opacity = item
        .at(&["style", "opacity"])
        .and_then(Json::as_f64)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0) as f32;
    let visible = item.flag("visible", true);
    let node = |kind, width, height| DesignNode {
        name: name.clone(),
        visible,
        opacity,
        transform,
        width,
        height,
        kind,
    };
    match kind_name {
        "group" => {
            let mut children = Vec::new();
            let list = item
                .at(&["group", "children"])
                .or_else(|| item.get("children"))
                .map(Json::as_arr)
                .unwrap_or(&[]);
            for child in list {
                if let Some(n) = xd_item(r, child, transform, depth + 1)? {
                    children.push(n);
                }
            }
            Ok(Some(node(DesignKind::Group { children }, 0.0, 0.0)))
        }
        "shape" => {
            let shape = item.get("shape").unwrap_or(&Json::Null);
            let fill = item.at(&["style", "fill"]);
            if fill.and_then(|f| f.str_of("type")) == Some("pattern") {
                let pattern = fill.and_then(|f| f.get("pattern"));
                let uid = pattern
                    .and_then(|p| p.at(&["meta", "ux", "uid"]))
                    .and_then(Json::as_str)
                    .map(|u| format!("resources/{u}"))
                    .or_else(|| {
                        pattern
                            .and_then(|p| p.str_of("href"))
                            .map(|h| h.trim_start_matches('/').to_string())
                    });
                let Some(wanted) = uid else {
                    r.note(format!(
                        "image {name:?} names no image resource; it was left out"
                    ));
                    return Ok(None);
                };
                let n = |k: &str| shape.num_or(k, 0.0).clamp(-1e6, 1e6);
                let (w, h) = (n("width").max(0.0), n("height").max(0.0));
                if shape.str_of("type") != Some("rect") {
                    r.note(format!(
                        "image {name:?} fills a non-rectangular shape; its bounding box was used"
                    ));
                }
                let transform = transform.then(Affine::translate(n("x"), n("y")));
                if !transform.is_translation() {
                    r.note(format!(
                        "image layer {name:?} is rotated or scaled; it was placed unrotated"
                    ));
                }
                let Some((bw, bh, rgba)) = r.bitmap(&wanted, &name, (w, h))? else {
                    return Ok(None);
                };
                let mut n = node(
                    DesignKind::Bitmap {
                        width: bw,
                        height: bh,
                        rgba,
                    },
                    w,
                    h,
                );
                n.transform = transform;
                return Ok(Some(n));
            }
            let Some((path_svg, w, h)) = xd_shape_path(shape, r, &name) else {
                return Ok(None);
            };
            let (fill, stroke) = xd_paint(r, item, &name);
            Ok(Some(node(
                DesignKind::Shape {
                    path_svg,
                    fill,
                    stroke,
                    even_odd: item
                        .at(&["style", "fill", "fillRule"])
                        .and_then(Json::as_str)
                        == Some("evenodd"),
                },
                w,
                h,
            )))
        }
        "text" => {
            let text = item.get("text").unwrap_or(&Json::Null);
            let raw = text.str_of("rawText").unwrap_or_default().to_string();
            let font = item.at(&["style", "font"]);
            let size = font
                .and_then(|f| f.num("size"))
                .unwrap_or(12.0)
                .clamp(0.5, 4000.0);
            let family = font
                .and_then(|f| f.str_of("family"))
                .unwrap_or("Helvetica")
                .to_string();
            let style = font
                .and_then(|f| f.str_of("style"))
                .unwrap_or("")
                .to_ascii_lowercase();
            let (fill, _) = xd_paint(r, item, &name);
            let area = text.at(&["frame", "type"]).and_then(Json::as_str) == Some("area");
            let (w, h) = (
                text.at(&["frame", "width"])
                    .and_then(Json::as_f64)
                    .unwrap_or(0.0),
                text.at(&["frame", "height"])
                    .and_then(Json::as_f64)
                    .unwrap_or(0.0),
            );
            if text.arr("paragraphs").len() > 1
                || text
                    .arr("paragraphs")
                    .iter()
                    .any(|p| p.arr("lines").iter().any(|l| l.as_arr().len() > 1))
            {
                r.note(format!(
                    "text layer {name:?} may have more than one styled range; its base \
                     font, size and colour were applied to all of it"
                ));
            }
            // XD places point text by its first baseline; the layout box here
            // starts at the top of the line, about 0.8 em above it.
            let transform = if area {
                transform
            } else {
                transform.then(Affine::translate(0.0, -0.8 * size))
            };
            let mut n = node(
                DesignKind::Text {
                    text: raw,
                    font_family: family,
                    bold: ["bold", "black", "heavy", "semibold"]
                        .iter()
                        .any(|s| style.contains(s)),
                    italic: style.contains("italic") || style.contains("oblique"),
                    size: size as f32,
                    color: fill.unwrap_or([0.0, 0.0, 0.0, 1.0]),
                    box_width: area.then_some(w as f32),
                },
                w,
                h,
            );
            n.transform = transform;
            Ok(Some(n))
        }
        "syncRef" | "symbol" => {
            r.note(format!(
                "component {name:?} was not expanded (XD components are not read); it was left out"
            ));
            Ok(None)
        }
        other => {
            r.note(format!(
                "layer {name:?} is an XD {other:?} item, which this reader does not know; it was left out"
            ));
            Ok(None)
        }
    }
}

/// An Adobe XD document's artboards (and pasteboard items), as layers.
pub fn read_xd(zip: &[u8], limits: ImportLimits) -> Result<DesignDocument, CodecError> {
    let mut r = Reader::new(zip, ImportFormat::Xd, limits);
    let mut contents: Vec<String> = zip_names(zip, "Adobe XD")?
        .into_iter()
        .filter(|n| n.starts_with("artwork/") && n.ends_with("graphicContent.agc"))
        .collect();
    contents.sort();
    if contents.is_empty() {
        return Err(CodecError::Unsupported(
            "this Adobe XD file has no artwork (artwork/*/graphicContent.agc)".into(),
        ));
    }
    let resources = r.json("resources/graphics/graphicContent.agc")?;
    let manifest = r.json("manifest")?;
    let bounds_of = |id: &str| -> Option<(f64, f64, f64, f64)> {
        let from_resources = resources
            .as_ref()
            .and_then(|res| res.at(&["artboards", id]))
            .map(|a| {
                (
                    a.num_or("x", 0.0),
                    a.num_or("y", 0.0),
                    a.num_or("width", 0.0),
                    a.num_or("height", 0.0),
                )
            });
        from_resources.or_else(|| {
            let mut stack: Vec<&Json> = manifest.iter().collect();
            let mut seen = 0;
            while let Some(j) = stack.pop() {
                seen += 1;
                if seen > 10_000 {
                    return None;
                }
                if j.str_of("path") == Some(id) {
                    let b = j.get("uxdesign#bounds")?;
                    return Some((
                        b.num_or("x", 0.0),
                        b.num_or("y", 0.0),
                        b.num_or("width", 0.0),
                        b.num_or("height", 0.0),
                    ));
                }
                stack.extend(j.arr("children"));
            }
            None
        })
    };
    let mut nodes = Vec::new();
    let mut boards = 0usize;
    for entry in &contents {
        let dir = entry
            .trim_start_matches("artwork/")
            .split('/')
            .next()
            .unwrap_or("")
            .to_string();
        let agc = r
            .json(entry)?
            .ok_or_else(|| malformed("Adobe XD", format!("{entry} is listed but missing")))?;
        for item in agc.arr("children") {
            if item.str_of("type") != Some("artboard") {
                if let Some(n) = xd_item(&mut r, item, Affine::IDENTITY, 0)? {
                    nodes.push(n);
                }
                continue;
            }
            r.count(0)?;
            let reference = item
                .at(&["artboard", "ref"])
                .and_then(Json::as_str)
                .unwrap_or(&dir)
                .to_string();
            let name = item.str_of("name").unwrap_or(&reference).to_string();
            let Some((x, y, w, h)) = bounds_of(&reference).or_else(|| bounds_of(&dir)) else {
                r.note(format!(
                    "artboard {name:?} has no bounds in the file; its contents opened as a group"
                ));
                let mut children = Vec::new();
                for child in item
                    .at(&["artboard", "children"])
                    .map(Json::as_arr)
                    .unwrap_or(&[])
                {
                    if let Some(n) = xd_item(&mut r, child, Affine::IDENTITY, 1)? {
                        children.push(n);
                    }
                }
                nodes.push(DesignNode {
                    name,
                    visible: true,
                    opacity: 1.0,
                    transform: Affine::IDENTITY,
                    width: 0.0,
                    height: 0.0,
                    kind: DesignKind::Group { children },
                });
                continue;
            };
            let clamp = |v: f64| {
                if v.is_finite() {
                    v.clamp(-1e7, 1e7)
                } else {
                    0.0
                }
            };
            let (x, y, w, h) = (clamp(x), clamp(y), clamp(w).max(0.0), clamp(h).max(0.0));
            let at = Affine::translate(x, y);
            let background = xd_paint(&mut r, item, &name).0;
            let mut children = Vec::new();
            for child in item
                .at(&["artboard", "children"])
                .map(Json::as_arr)
                .unwrap_or(&[])
            {
                if let Some(n) = xd_item(&mut r, child, at, 1)? {
                    children.push(n);
                }
            }
            boards += 1;
            nodes.push(DesignNode {
                name,
                visible: item.flag("visible", true),
                opacity: 1.0,
                transform: at,
                width: w,
                height: h,
                kind: DesignKind::Artboard {
                    background,
                    children,
                },
            });
        }
    }
    Ok(DesignDocument {
        format: ImportFormat::Xd,
        nodes,
        notes: r.notes,
        opened: format!("{boards} artboard(s)"),
    })
}

// -------------------------------------------------------------------- entry

/// Read a Sketch, XD or Figma file as layers.
///
/// `Err` when the file is damaged (malformed JSON included) or holds no
/// document this reader can use; the caller then falls back to the
/// embedded preview, saying why.
pub fn read_design(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DesignDocument, CodecError> {
    match format {
        ImportFormat::Sketch => read_sketch(bytes, limits),
        ImportFormat::Xd => read_xd(bytes, limits),
        ImportFormat::Fig => super::design_fig::read_fig(bytes, limits),
        other => Err(CodecError::Unsupported(format!(
            "{} is not a design-file format",
            other.name()
        ))),
    }
}

#[cfg(test)]
#[path = "design_files_tests.rs"]
pub(crate) mod tests;
