//! W13-D: Windows metafiles, WMF and EMF, read by translating their common
//! drawing records into an SVG document that the `resvg` importer
//! ([`crate::codec::svg_import::rasterize`]) draws, under the caller's
//! [`ImportLimits`].
//!
//! # What is drawn
//!
//! * Objects: pens (solid; width, colour, the null pen), brushes (solid and
//!   hatched brushes as their solid colour, the null brush), fonts (height,
//!   weight, italic, face name, escapement), EMF stock objects; select and
//!   delete; the DC stack (save / restore).
//! * Coordinates: window and viewport origin and extent, EMF map modes
//!   (text, isotropic, anisotropic and the metric / English / twips modes)
//!   and the EMF world transform (set and modify).
//! * Shapes: move-to / line-to, rectangles, rounded rectangles, ellipses,
//!   polygons, polylines, poly-polygons, poly-polylines, Béziers (EMF), with
//!   the polygon fill mode; EMF path brackets (begin / end / close figure,
//!   fill, stroke, stroke-and-fill).
//! * Text: WMF `TextOut` / `ExtTextOut`, EMF `ExtTextOutW`, with the text
//!   colour and horizontal alignment, drawn in a system font.
//! * Bitmaps: WMF `StretchDIB` / `DIBStretchBlt` and EMF `StretchDIBits`, the
//!   whole DIB drawn into the destination rectangle (decoded by the BMP
//!   reader).
//!
//! # What is not
//!
//! Arcs, chords and pies; pen dash styles, caps and joins (every pen is a
//! solid round-joined line); pattern and DIB pattern brushes (they leave
//! the current brush as it was); clipping regions and clip paths; raster
//! operations other than a plain copy; the source sub-rectangle of a
//! bitmap blit; EMF+ records (an EMF+ "dual" file's GDI records are drawn,
//! an EMF+-only file draws nothing). A file of only those records opens
//! blank or is refused as empty.
//!
//! # Untrusted input
//!
//! Every record's length is checked against the file before it is read, the
//! walk stops at the end-of-file record or at [`MAX_RECORDS`], counts inside
//! a record are checked against the record, and the SVG is capped at
//! [`MAX_SVG_ELEMENTS`] elements. Embedded bitmaps go through the BMP reader
//! with the caller's limits, and their total is bounded by the limits'
//! allocation ceiling.

use std::fmt::Write as _;

use super::malformed;
use crate::codec::{
    decode_surface_bytes_as, encode, svg_import, CodecError, DecodedSurface, ExportFormat,
    ImageInfo, ImportFormat, ImportLimits,
};

/// The most records one file may hold before the walk stops.
pub const MAX_RECORDS: usize = 1_000_000;
/// The most SVG elements a metafile turns into.
pub const MAX_SVG_ELEMENTS: usize = 200_000;
/// The pixel size a WMF with no placeable header and no window opens at.
const FALLBACK_SIDE: f64 = 512.0;

const WMF_PLACEABLE: [u8; 4] = [0xD7, 0xCD, 0xC6, 0x9A];

/// `true` for a placeable WMF (`D7 CD C6 9A`) or a bare WMF header (type 1
/// or 2, a 9-word header, version 1.0 or 3.0).
pub fn looks_like_wmf(head: &[u8]) -> bool {
    if head.starts_with(&WMF_PLACEABLE) {
        return true;
    }
    head.len() >= 6
        && matches!(head[0..2], [1, 0] | [2, 0])
        && head[2..4] == [9, 0]
        && matches!(head[4..6], [0, 1] | [0, 3])
}

/// `true` for an EMF: an `EMR_HEADER` record with the ` EMF` signature.
pub fn looks_like_emf(head: &[u8]) -> bool {
    head.len() >= 44 && head[0..4] == [1, 0, 0, 0] && &head[40..44] == b" EMF"
}

// ------------------------------------------------------------------ reading

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at.checked_add(2)?)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn i16_at(b: &[u8], at: usize) -> Option<f64> {
    u16_at(b, at).map(|v| f64::from(v as i16))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at.checked_add(4)?)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn i32_at(b: &[u8], at: usize) -> Option<f64> {
    u32_at(b, at).map(|v| f64::from(v as i32))
}
fn f32_at(b: &[u8], at: usize) -> Option<f64> {
    u32_at(b, at).map(|v| f64::from(f32::from_bits(v)))
}
fn colorref(b: &[u8], at: usize) -> Option<[u8; 3]> {
    b.get(at..at.checked_add(3)?).map(|s| [s[0], s[1], s[2]])
}

// ------------------------------------------------------------------- canvas

/// An affine transform `[a, b, c, d, e, f]`: `x' = a x + c y + e`,
/// `y' = b x + d y + f` (the EMF `XFORM` order).
type Affine = [f64; 6];
const IDENTITY: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn apply(m: &Affine, (x, y): (f64, f64)) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// `outer` after `inner`.
fn compose(outer: &Affine, inner: &Affine) -> Affine {
    [
        outer[0] * inner[0] + outer[2] * inner[1],
        outer[1] * inner[0] + outer[3] * inner[1],
        outer[0] * inner[2] + outer[2] * inner[3],
        outer[1] * inner[2] + outer[3] * inner[3],
        outer[0] * inner[4] + outer[2] * inner[5] + outer[4],
        outer[1] * inner[4] + outer[3] * inner[5] + outer[5],
    ]
}

#[derive(Clone, Copy, Debug)]
struct Pen {
    color: Option<[u8; 3]>,
    width: f64,
}

#[derive(Clone, Copy, Debug)]
struct Brush {
    color: Option<[u8; 3]>,
}

#[derive(Clone, Debug)]
struct Font {
    height: f64,
    weight: i32,
    italic: bool,
    face: String,
    escapement: f64,
}

impl Default for Font {
    fn default() -> Self {
        Font {
            height: 12.0,
            weight: 400,
            italic: false,
            face: String::new(),
            escapement: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
enum Obj {
    Pen(Pen),
    Brush(Brush),
    Font(Font),
    /// A palette, region or pattern brush: holds a slot, draws nothing.
    Other,
}

/// The device-context state the records change.
#[derive(Clone, Debug)]
struct Dc {
    pen: Pen,
    brush: Brush,
    font: Font,
    text_color: [u8; 3],
    text_align: u32,
    even_odd: bool,
    window_org: (f64, f64),
    window_ext: (f64, f64),
    viewport_org: (f64, f64),
    viewport_ext: (f64, f64),
    map_mode: u32,
    world: Affine,
    current: (f64, f64),
}

impl Dc {
    fn new() -> Self {
        Dc {
            pen: Pen {
                color: Some([0, 0, 0]),
                width: 0.0,
            },
            brush: Brush {
                color: Some([255, 255, 255]),
            },
            font: Font::default(),
            text_color: [0, 0, 0],
            text_align: 0,
            even_odd: true,
            window_org: (0.0, 0.0),
            window_ext: (1.0, 1.0),
            viewport_org: (0.0, 0.0),
            viewport_ext: (1.0, 1.0),
            map_mode: 1,
            world: IDENTITY,
            current: (0.0, 0.0),
        }
    }
}

/// Where the output goes: SVG elements in device (pixel) coordinates.
struct Canvas {
    body: String,
    elements: usize,
    images: u64,
    image_budget: u64,
    limits: ImportLimits,
}

impl Canvas {
    fn new(limits: ImportLimits) -> Self {
        Canvas {
            body: String::new(),
            elements: 0,
            images: 0,
            image_budget: limits.max_alloc_bytes,
            limits,
        }
    }

    fn room(&mut self) -> bool {
        if self.elements >= MAX_SVG_ELEMENTS {
            return false;
        }
        self.elements += 1;
        true
    }

    /// Emit `d` (device coordinates) filled and/or stroked.
    fn path(&mut self, d: &str, fill: Option<[u8; 3]>, even_odd: bool, stroke: Option<(Pen, f64)>) {
        if d.is_empty() || (fill.is_none() && stroke.is_none_or(|(p, _)| p.color.is_none())) {
            return;
        }
        if !self.room() {
            return;
        }
        let _ = write!(self.body, "<path d=\"{d}\"");
        match fill {
            Some(c) => {
                let _ = write!(self.body, " fill=\"{}\"", hex(c));
                if even_odd {
                    self.body.push_str(" fill-rule=\"evenodd\"");
                }
            }
            None => self.body.push_str(" fill=\"none\""),
        }
        if let Some((
            Pen {
                color: Some(c),
                width,
            },
            scale,
        )) = stroke
        {
            let w = if width <= 0.0 {
                1.0
            } else {
                (width * scale).max(1.0)
            };
            let _ = write!(
                self.body,
                " stroke=\"{}\" stroke-width=\"{}\" stroke-linejoin=\"round\" stroke-linecap=\"round\"",
                hex(c),
                num(w)
            );
        }
        self.body.push_str("/>\n");
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        at: (f64, f64),
        text: &str,
        font: &Font,
        size: f64,
        color: [u8; 3],
        align: u32,
        angle: f64,
    ) {
        let text = text.trim_end_matches('\0');
        if text.trim().is_empty() || !size.is_finite() || size <= 0.0 || !self.room() {
            return;
        }
        let anchor = match align & 6 {
            6 => "middle",
            2 => "end",
            _ => "start",
        };
        // TA_BASELINE (24) is the baseline; TA_BOTTOM (8) the descent line;
        // TA_TOP (0), the default, the cell's top.
        let baseline = match align & 24 {
            24 => "alphabetic",
            8 => "text-after-edge",
            _ => "text-before-edge",
        };
        let _ = write!(
            self.body,
            "<text x=\"{}\" y=\"{}\" font-size=\"{}\" fill=\"{}\" text-anchor=\"{anchor}\" dominant-baseline=\"{baseline}\"",
            num(at.0),
            num(at.1),
            num(size),
            hex(color)
        );
        if !font.face.is_empty() {
            let _ = write!(
                self.body,
                " font-family=\"{}, sans-serif\"",
                escape(&font.face)
            );
        }
        if font.weight >= 600 {
            self.body.push_str(" font-weight=\"bold\"");
        }
        if font.italic {
            self.body.push_str(" font-style=\"italic\"");
        }
        if angle.abs() > 0.01 && angle.is_finite() {
            let _ = write!(
                self.body,
                " transform=\"rotate({} {} {})\"",
                num(-angle),
                num(at.0),
                num(at.1)
            );
        }
        let _ = writeln!(self.body, ">{}</text>", escape(text));
    }

    /// Draw a packed DIB (`bmi` then `bits`) into the device rectangle
    /// spanned by `a` and `b`.
    fn dib(
        &mut self,
        bmi: &[u8],
        bits: &[u8],
        a: (f64, f64),
        b: (f64, f64),
    ) -> Result<(), CodecError> {
        let Some(bmp) = bmp_file(bmi, bits) else {
            return Ok(());
        };
        let surface = match decode_surface_bytes_as(&bmp, self.limits, ImportFormat::Bmp) {
            Ok(s) => s,
            Err(CodecError::LimitExceeded(e)) => return Err(CodecError::LimitExceeded(e)),
            // A bitmap the BMP reader cannot read is left out, as a
            // renderer skips a record it does not understand.
            Err(_) => return Ok(()),
        };
        let (w, h) = (surface.width, surface.height);
        let mut rgba = surface.pixels.into_rgba8();
        // A 32-bit BI_RGB DIB leaves its fourth byte zero: opaque, not
        // invisible.
        if rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            for p in rgba.as_chunks_mut::<4>().0 {
                p[3] = 255;
            }
        }
        let png = encode(ExportFormat::Png, w, h, &rgba)?;
        self.images = self.images.saturating_add(png.len() as u64 * 2);
        if self.images > self.image_budget {
            return Err(CodecError::LimitExceeded(
                "the metafile's embedded bitmaps are larger than the import limit".into(),
            ));
        }
        if !self.room() {
            return Ok(());
        }
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        let flip_x = b.0 < a.0;
        let flip_y = b.1 < a.1;
        let _ = write!(
            self.body,
            "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\"",
            num(x0),
            num(y0),
            num((x1 - x0).max(0.0)),
            num((y1 - y0).max(0.0))
        );
        if flip_x || flip_y {
            let _ = write!(
                self.body,
                " transform=\"translate({} {}) scale({} {}) translate({} {})\"",
                num(x0 + x1),
                num(y0 + y1),
                if flip_x { -1 } else { 1 },
                if flip_y { -1 } else { 1 },
                num(if flip_x { 0.0 } else { -(x0 + x1) }),
                num(if flip_y { 0.0 } else { -(y0 + y1) })
            );
        }
        let _ = writeln!(
            self.body,
            " href=\"data:image/png;base64,{}\"/>",
            super::vector_docs::base64_encode(&png)
        );
        Ok(())
    }

    fn finish(
        self,
        origin: (f64, f64),
        size: (f64, f64),
        format: ImportFormat,
    ) -> Result<DecodedSurface, CodecError> {
        let (w, h) = (size.0.ceil(), size.1.ceil());
        if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0) {
            return Err(malformed(format.name(), "the picture has no area"));
        }
        if w > f64::from(self.limits.max_width) || h > f64::from(self.limits.max_height) {
            return Err(CodecError::LimitExceeded(format!(
                "a {w}x{h} picture is larger than the import limit"
            )));
        }
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"{} {} {} {}\">\n{}</svg>\n",
            num(w),
            num(h),
            num(origin.0),
            num(origin.1),
            num(w),
            num(h),
            self.body
        );
        let mut surface = svg_import::rasterize(svg.as_bytes(), self.limits)?;
        surface.source_format = format;
        Ok(surface)
    }
}

fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// A number for SVG: finite, and at most three decimals.
fn num(v: f64) -> String {
    let v = if v.is_finite() {
        v.clamp(-1e7, 1e7)
    } else {
        0.0
    };
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// Path data for a polyline / polygon of device points.
fn poly_d(points: &[(f64, f64)], close: bool) -> String {
    let mut d = String::new();
    for (i, p) in points.iter().enumerate() {
        let _ = write!(
            d,
            "{}{} {} ",
            if i == 0 { 'M' } else { 'L' },
            num(p.0),
            num(p.1)
        );
    }
    if close && !d.is_empty() {
        d.push('Z');
    }
    d
}

/// The four cubic Béziers of an ellipse inscribed in `(l, t, r, b)`, in
/// logical space, mapped through `m`.
fn ellipse_d(m: &Affine, l: f64, t: f64, r: f64, b: f64) -> String {
    let (cx, cy, rx, ry) = ((l + r) / 2.0, (t + b) / 2.0, (r - l) / 2.0, (b - t) / 2.0);
    rounded_d(m, cx - rx, cy - ry, cx + rx, cy + ry, rx.abs(), ry.abs())
}

/// A rectangle with corner radii `rx`, `ry` (zero is square), as path data.
fn rounded_d(m: &Affine, l: f64, t: f64, r: f64, b: f64, rx: f64, ry: f64) -> String {
    let (l, r) = (l.min(r), l.max(r));
    let (t, b) = (t.min(b), t.max(b));
    let rx = rx.min((r - l) / 2.0).max(0.0);
    let ry = ry.min((b - t) / 2.0).max(0.0);
    let k = 0.552_284_75;
    let p = |x: f64, y: f64| {
        let (x, y) = apply(m, (x, y));
        format!("{} {}", num(x), num(y))
    };
    if rx == 0.0 || ry == 0.0 {
        return format!("M{} L{} L{} L{} Z", p(l, t), p(r, t), p(r, b), p(l, b));
    }
    let (ox, oy) = (rx * k, ry * k);
    format!(
        "M{} L{} C{} {} {} L{} C{} {} {} L{} C{} {} {} L{} C{} {} {} Z",
        p(l + rx, t),
        p(r - rx, t),
        p(r - rx + ox, t),
        p(r, t + ry - oy),
        p(r, t + ry),
        p(r, b - ry),
        p(r, b - ry + oy),
        p(r - rx + ox, b),
        p(r - rx, b),
        p(l + rx, b),
        p(l + rx - ox, b),
        p(l, b - ry + oy),
        p(l, b - ry),
        p(l, t + ry),
        p(l, t + ry - oy),
        p(l + rx - ox, t),
        p(l + rx, t)
    )
}

/// A BMP file around a packed DIB, so the BMP reader decodes it.
fn bmp_file(bmi: &[u8], bits: &[u8]) -> Option<Vec<u8>> {
    let header = u32_at(bmi, 0)? as usize;
    if header < 12 || header > bmi.len() {
        return None;
    }
    let total = 14usize.checked_add(bmi.len())?.checked_add(bits.len())?;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&u32::try_from(total).ok()?.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&u32::try_from(14 + bmi.len()).ok()?.to_le_bytes());
    out.extend_from_slice(bmi);
    out.extend_from_slice(bits);
    Some(out)
}

/// A packed DIB's header-and-palette length, from the header itself.
fn packed_dib_split(dib: &[u8]) -> Option<usize> {
    let header = u32_at(dib, 0)? as usize;
    let (bpp, compression, used, entry) = if header == 12 {
        (u16_at(dib, 10)?, 0, 0, 3)
    } else if header >= 40 {
        (
            u16_at(dib, 14)?,
            u32_at(dib, 16)?,
            u32_at(dib, 32)? as usize,
            4,
        )
    } else {
        return None;
    };
    let colors = if bpp <= 8 {
        if used == 0 {
            1usize << bpp
        } else {
            used.min(256)
        }
    } else {
        used.min(256)
    };
    // BI_BITFIELDS with a 40-byte header keeps its three masks after it.
    let masks = if header == 40 && compression == 3 {
        12
    } else {
        0
    };
    let split = header + masks + colors * entry;
    (split <= dib.len()).then_some(split)
}

fn object_slot(objects: &mut Vec<Option<Obj>>, obj: Obj) {
    if let Some(free) = objects.iter_mut().find(|o| o.is_none()) {
        *free = Some(obj);
    } else if objects.len() < 65_535 {
        objects.push(Some(obj));
    }
}

fn select(dc: &mut Dc, obj: &Obj) {
    match obj {
        Obj::Pen(p) => dc.pen = *p,
        Obj::Brush(b) => dc.brush = *b,
        Obj::Font(f) => dc.font = f.clone(),
        Obj::Other => {}
    }
}

fn pen(style: u32, width: f64, color: [u8; 3]) -> Pen {
    Pen {
        color: (style & 0x0F != 5).then_some(color),
        width,
    }
}

fn brush(style: u32, color: [u8; 3]) -> Obj {
    match style {
        // BS_SOLID, BS_HATCHED (drawn as its colour).
        0 | 2 => Obj::Brush(Brush { color: Some(color) }),
        1 => Obj::Brush(Brush { color: None }),
        _ => Obj::Other,
    }
}

fn restore(stack: &mut Vec<Dc>, dc: &mut Dc, which: i32) {
    let target = if which < 0 {
        stack.len().checked_sub(which.unsigned_abs() as usize)
    } else {
        (which as usize).checked_sub(1)
    };
    if let Some(t) = target.filter(|t| *t < stack.len()) {
        *dc = stack[t].clone();
        stack.truncate(t);
    }
}

// ---------------------------------------------------------------------- WMF

struct Wmf {
    pixels: (f64, f64),
}

impl Wmf {
    /// Logical -> device for the current window: the window maps onto the
    /// picture's pixel rectangle.
    fn transform(&self, dc: &Dc) -> Affine {
        let (sx, sy) = (
            self.pixels.0 / nonzero(dc.window_ext.0),
            self.pixels.1 / nonzero(dc.window_ext.1),
        );
        // The window's origin lands on the picture's top-left pixel.
        [
            sx,
            0.0,
            0.0,
            sy,
            -dc.window_org.0 * sx,
            -dc.window_org.1 * sy,
        ]
    }
}

fn nonzero(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        1.0
    } else {
        v
    }
}

fn scale_of(m: &Affine) -> f64 {
    (m[0] * m[3] - m[1] * m[2]).abs().sqrt()
}

/// Render a WMF.
pub(super) fn decode_wmf(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    const NAME: &str = "WMF";
    let bad = |what: &str| malformed(NAME, what);
    let (mut at, placeable) = if bytes.starts_with(&WMF_PLACEABLE) {
        let l = i16_at(bytes, 6).ok_or_else(|| bad("the placeable header is cut short"))?;
        let t = i16_at(bytes, 8).ok_or_else(|| bad("the placeable header is cut short"))?;
        let r = i16_at(bytes, 10).ok_or_else(|| bad("the placeable header is cut short"))?;
        let b = i16_at(bytes, 12).ok_or_else(|| bad("the placeable header is cut short"))?;
        let inch =
            f64::from(u16_at(bytes, 14).ok_or_else(|| bad("the placeable header is cut short"))?);
        (
            22usize,
            Some((l, t, r, b, if inch > 0.0 { inch } else { 1440.0 })),
        )
    } else {
        (0usize, None)
    };
    let header_words = u16_at(bytes, at + 2).ok_or_else(|| bad("no metafile header"))?;
    if header_words != 9 || !matches!(u16_at(bytes, at), Some(1 | 2)) {
        return Err(bad("the metafile header is not a WMF header"));
    }
    at += 18;

    // Pass 1: the first window, for a file with no placeable header.
    let (mut win_org, mut win_ext) = (None, None);
    {
        let mut p = at;
        for _ in 0..MAX_RECORDS {
            let Some(words) = u32_at(bytes, p) else { break };
            let Some(func) = u16_at(bytes, p + 4) else {
                break;
            };
            let len = (words as usize).saturating_mul(2);
            if len < 6 || p.saturating_add(len) > bytes.len() || func == 0 {
                break;
            }
            match func {
                0x020B if win_org.is_none() => {
                    win_org = i16_at(bytes, p + 8).zip(i16_at(bytes, p + 6));
                }
                0x020C if win_ext.is_none() => {
                    win_ext = i16_at(bytes, p + 8).zip(i16_at(bytes, p + 6));
                }
                _ => {}
            }
            if win_org.is_some() && win_ext.is_some() {
                break;
            }
            p += len;
        }
    }
    let (frame, pixels) = match placeable {
        Some((l, t, r, b, inch)) => {
            let (w, h) = ((r - l).abs(), (b - t).abs());
            (
                (l.min(r), t.min(b), w, h),
                (w * 96.0 / inch, h * 96.0 / inch),
            )
        }
        None => {
            let (ox, oy) = win_org.unwrap_or((0.0, 0.0));
            let (ex, ey) = win_ext.unwrap_or((FALLBACK_SIDE, FALLBACK_SIDE));
            ((ox, oy, ex, ey), (ex.abs(), ey.abs()))
        }
    };
    let wmf = Wmf { pixels };
    let mut dc = Dc::new();
    dc.window_org = (frame.0, frame.1);
    dc.window_ext = (
        if placeable.is_some() {
            frame.2
        } else {
            win_ext.map_or(frame.2, |e| e.0)
        },
        if placeable.is_some() {
            frame.3
        } else {
            win_ext.map_or(frame.3, |e| e.1)
        },
    );
    let mut stack: Vec<Dc> = Vec::new();
    let mut objects: Vec<Option<Obj>> = Vec::new();
    let mut canvas = Canvas::new(limits);
    let mut drew = false;

    for _ in 0..MAX_RECORDS {
        let Some(words) = u32_at(bytes, at) else {
            break;
        };
        let Some(func) = u16_at(bytes, at + 4) else {
            break;
        };
        let len = (words as usize).saturating_mul(2);
        if func == 0 {
            break;
        }
        if len < 6 || at.saturating_add(len) > bytes.len() {
            return Err(bad("a record runs past the end of the file"));
        }
        let rec = &bytes[at..at + len];
        at += len;
        let p = |i: usize| i16_at(rec, 6 + i * 2).unwrap_or(0.0);
        let m = wmf.transform(&dc);
        let s = scale_of(&m);
        match func {
            0x001E => stack.push(dc.clone()),
            0x0127 => restore(&mut stack, &mut dc, p(0) as i32),
            0x0106 => dc.even_odd = p(0) as i32 != 2,
            0x0209 => dc.text_color = colorref(rec, 6).unwrap_or([0, 0, 0]),
            0x012E => dc.text_align = p(0) as u16 as u32,
            0x020B => dc.window_org = (p(1), p(0)),
            0x020C => dc.window_ext = (p(1), p(0)),
            0x02FA => {
                let style = u32::from(u16_at(rec, 6).unwrap_or(0));
                let obj = Obj::Pen(pen(
                    style,
                    p(1).abs(),
                    colorref(rec, 12).unwrap_or([0, 0, 0]),
                ));
                object_slot(&mut objects, obj);
            }
            0x02FC => {
                let style = u32::from(u16_at(rec, 6).unwrap_or(0));
                let obj = brush(style, colorref(rec, 8).unwrap_or([0, 0, 0]));
                object_slot(&mut objects, obj);
            }
            0x02FB => {
                let face_bytes = rec.get(24..).unwrap_or(&[]);
                let face: String = face_bytes
                    .iter()
                    .take(32)
                    .take_while(|b| **b != 0)
                    .map(|b| char::from(*b))
                    .collect();
                let obj = Obj::Font(Font {
                    height: p(0),
                    weight: p(4) as i32,
                    italic: rec.get(16).is_some_and(|b| *b != 0),
                    face,
                    escapement: p(2) / 10.0,
                });
                object_slot(&mut objects, obj);
            }
            0x00F7 | 0x01F9 | 0x06FF | 0x0142 => object_slot(&mut objects, Obj::Other),
            0x012D => {
                let i = u16_at(rec, 6).unwrap_or(u16::MAX) as usize;
                if let Some(Some(obj)) = objects.get(i) {
                    let obj = obj.clone();
                    select(&mut dc, &obj);
                }
            }
            0x01F0 => {
                let i = u16_at(rec, 6).unwrap_or(u16::MAX) as usize;
                if let Some(slot) = objects.get_mut(i) {
                    *slot = None;
                }
            }
            0x0214 => dc.current = (p(1), p(0)),
            0x0213 => {
                let to = (p(1), p(0));
                let d = poly_d(&[apply(&m, dc.current), apply(&m, to)], false);
                canvas.path(&d, None, false, Some((dc.pen, s)));
                dc.current = to;
                drew = true;
            }
            0x041B | 0x0418 | 0x061C => {
                let (b, r, t, l, rh, rw) = if func == 0x061C {
                    (p(2), p(3), p(4), p(5), p(0), p(1))
                } else {
                    (p(0), p(1), p(2), p(3), 0.0, 0.0)
                };
                let d = match func {
                    0x0418 => ellipse_d(&m, l, t, r, b),
                    _ => rounded_d(&m, l, t, r, b, rw.abs() / 2.0, rh.abs() / 2.0),
                };
                canvas.path(&d, dc.brush.color, dc.even_odd, Some((dc.pen, s)));
                drew = true;
            }
            0x0324 | 0x0325 => {
                let n = u16_at(rec, 6).unwrap_or(0) as usize;
                let pts = points16(rec, 8, n)
                    .ok_or_else(|| bad("a polygon's points run past its record"))?;
                let pts: Vec<_> = pts.into_iter().map(|q| apply(&m, q)).collect();
                if func == 0x0324 {
                    canvas.path(
                        &poly_d(&pts, true),
                        dc.brush.color,
                        dc.even_odd,
                        Some((dc.pen, s)),
                    );
                } else {
                    canvas.path(&poly_d(&pts, false), None, false, Some((dc.pen, s)));
                }
                drew = true;
            }
            0x0538 => {
                let polys = u16_at(rec, 6).unwrap_or(0) as usize;
                let mut counts = Vec::with_capacity(polys.min(4096));
                for i in 0..polys {
                    counts.push(
                        u16_at(rec, 8 + i * 2).ok_or_else(|| bad("a poly-polygon is cut short"))?
                            as usize,
                    );
                }
                let mut off = 8 + polys * 2;
                let mut d = String::new();
                for n in counts {
                    let pts = points16(rec, off, n)
                        .ok_or_else(|| bad("a poly-polygon's points run past its record"))?;
                    off += n * 4;
                    let pts: Vec<_> = pts.into_iter().map(|q| apply(&m, q)).collect();
                    d.push_str(&poly_d(&pts, true));
                }
                canvas.path(&d, dc.brush.color, dc.even_odd, Some((dc.pen, s)));
                drew = true;
            }
            0x0521 => {
                let n = u16_at(rec, 6).unwrap_or(0) as usize;
                let padded = n + (n & 1);
                let text = rec.get(8..8 + n).map(latin1).unwrap_or_default();
                let y = i16_at(rec, 8 + padded).unwrap_or(0.0);
                let x = i16_at(rec, 10 + padded).unwrap_or(0.0);
                wmf_text(&mut canvas, &dc, &m, (x, y), &text);
                drew = true;
            }
            0x0A32 => {
                let y = p(0);
                let x = p(1);
                let n = u16_at(rec, 10).unwrap_or(0) as usize;
                let opts = u16_at(rec, 12).unwrap_or(0);
                let start = if opts & 0x0006 != 0 { 22 } else { 14 };
                let text = rec.get(start..start + n).map(latin1).unwrap_or_default();
                wmf_text(&mut canvas, &dc, &m, (x, y), &text);
                drew = true;
            }
            0x0F43 | 0x0B41 => {
                // StretchDIB carries a colour-usage word after the raster op.
                let base = if func == 0x0F43 { 12 } else { 10 };
                let with_bitmap = func == 0x0F43 || words as usize != (usize::from(func >> 8) + 3);
                if with_bitmap {
                    let q = |i: usize| i16_at(rec, base + i * 2).unwrap_or(0.0);
                    let (dh, dw, dy, dx) = (q(4), q(5), q(6), q(7));
                    let dib = rec.get(base + 16..).unwrap_or(&[]);
                    if let Some(split) = packed_dib_split(dib) {
                        canvas.dib(
                            &dib[..split],
                            &dib[split..],
                            apply(&m, (dx, dy)),
                            apply(&m, (dx + dw, dy + dh)),
                        )?;
                        drew = true;
                    }
                }
            }
            _ => {}
        }
    }
    if !drew {
        return Err(bad("it holds no drawing records this build reads"));
    }
    canvas.finish((0.0, 0.0), wmf.pixels, ImportFormat::Wmf)
}

fn wmf_text(canvas: &mut Canvas, dc: &Dc, m: &Affine, at: (f64, f64), text: &str) {
    let size = dc.font.height.abs() * m[3].abs().max(m[0].abs());
    canvas.text(
        apply(m, at),
        text,
        &dc.font,
        size,
        dc.text_color,
        dc.text_align,
        dc.font.escapement,
    );
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|c| char::from(*c)).collect()
}

fn points16(rec: &[u8], at: usize, n: usize) -> Option<Vec<(f64, f64)>> {
    let end = at.checked_add(n.checked_mul(4)?)?;
    if end > rec.len() {
        return None;
    }
    Some(
        (0..n)
            .map(|i| {
                let o = at + i * 4;
                (
                    i16_at(rec, o).unwrap_or(0.0),
                    i16_at(rec, o + 2).unwrap_or(0.0),
                )
            })
            .collect(),
    )
}

fn points32(rec: &[u8], at: usize, n: usize) -> Option<Vec<(f64, f64)>> {
    let end = at.checked_add(n.checked_mul(8)?)?;
    if end > rec.len() {
        return None;
    }
    Some(
        (0..n)
            .map(|i| {
                let o = at + i * 8;
                (
                    i32_at(rec, o).unwrap_or(0.0),
                    i32_at(rec, o + 4).unwrap_or(0.0),
                )
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------- EMF

struct Emf {
    px_per_mm: (f64, f64),
}

impl Emf {
    /// Logical (after the world transform) -> device, for the map mode.
    fn page(&self, dc: &Dc) -> Affine {
        let (sx, sy) = match dc.map_mode {
            // MM_ISOTROPIC, MM_ANISOTROPIC.
            7 | 8 => (
                dc.viewport_ext.0 / nonzero(dc.window_ext.0),
                dc.viewport_ext.1 / nonzero(dc.window_ext.1),
            ),
            // MM_LOMETRIC, MM_HIMETRIC, MM_LOENGLISH, MM_HIENGLISH, MM_TWIPS:
            // fixed units, y up.
            2..=6 => {
                let mm = match dc.map_mode {
                    2 => 0.1,
                    3 => 0.01,
                    4 => 0.254,
                    5 => 0.0254,
                    _ => 25.4 / 1440.0,
                };
                (mm * self.px_per_mm.0, -mm * self.px_per_mm.1)
            }
            _ => (1.0, 1.0),
        };
        [
            sx,
            0.0,
            0.0,
            sy,
            dc.viewport_org.0 - dc.window_org.0 * sx,
            dc.viewport_org.1 - dc.window_org.1 * sy,
        ]
    }

    fn transform(&self, dc: &Dc) -> Affine {
        compose(&self.page(dc), &dc.world)
    }
}

const STOCK: u32 = 0x8000_0000;

fn stock(i: u32) -> Option<Obj> {
    Some(match i & !STOCK {
        0 => Obj::Brush(Brush {
            color: Some([255; 3]),
        }),
        1 => Obj::Brush(Brush {
            color: Some([192; 3]),
        }),
        2 => Obj::Brush(Brush {
            color: Some([128; 3]),
        }),
        3 => Obj::Brush(Brush {
            color: Some([64; 3]),
        }),
        4 => Obj::Brush(Brush {
            color: Some([0; 3]),
        }),
        5 => Obj::Brush(Brush { color: None }),
        6 => Obj::Pen(Pen {
            color: Some([255; 3]),
            width: 0.0,
        }),
        7 => Obj::Pen(Pen {
            color: Some([0; 3]),
            width: 0.0,
        }),
        8 => Obj::Pen(Pen {
            color: None,
            width: 0.0,
        }),
        _ => return None,
    })
}

/// Render an EMF.
pub(super) fn decode_emf(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    const NAME: &str = "EMF";
    let bad = |what: &str| malformed(NAME, what);
    if !looks_like_emf(bytes) {
        return Err(bad("no EMF header"));
    }
    let header_len = u32_at(bytes, 4).ok_or_else(|| bad("no EMF header"))? as usize;
    let l = i32_at(bytes, 8).ok_or_else(|| bad("the header is cut short"))?;
    let t = i32_at(bytes, 12).ok_or_else(|| bad("the header is cut short"))?;
    let r = i32_at(bytes, 16).ok_or_else(|| bad("the header is cut short"))?;
    let b = i32_at(bytes, 20).ok_or_else(|| bad("the header is cut short"))?;
    let dev = (
        i32_at(bytes, 72).unwrap_or(0.0),
        i32_at(bytes, 76).unwrap_or(0.0),
    );
    let mm = (
        i32_at(bytes, 80).unwrap_or(0.0),
        i32_at(bytes, 84).unwrap_or(0.0),
    );
    let ratio = |d: f64, m: f64| {
        if d > 0.0 && m > 0.0 {
            d / m
        } else {
            96.0 / 25.4
        }
    };
    let emf = Emf {
        px_per_mm: (ratio(dev.0, mm.0), ratio(dev.1, mm.1)),
    };
    if header_len < 88 || header_len > bytes.len() {
        return Err(bad("the header record's length is wrong"));
    }
    if r < l || b < t {
        return Err(bad("the picture's bounds are empty"));
    }
    let size = (r - l + 1.0, b - t + 1.0);

    let mut dc = Dc::new();
    let mut stack: Vec<Dc> = Vec::new();
    let mut objects: std::collections::HashMap<u32, Obj> = std::collections::HashMap::new();
    let mut canvas = Canvas::new(limits);
    let mut drew = false;
    // The path bracket: `Some` between BeginPath and EndPath / a fill.
    let mut path: Option<String> = None;
    let mut bracket_done: Option<String> = None;
    let mut at = header_len;

    for _ in 0..MAX_RECORDS {
        let Some(kind) = u32_at(bytes, at) else { break };
        let Some(len) = u32_at(bytes, at + 4) else {
            break;
        };
        let len = len as usize;
        if len < 8 || !len.is_multiple_of(4) || at.saturating_add(len) > bytes.len() {
            return Err(bad("a record runs past the end of the file"));
        }
        let rec = &bytes[at..at + len];
        at += len;
        if kind == 14 {
            break;
        }
        let m = emf.transform(&dc);
        let s = scale_of(&m);
        let i = |o: usize| i32_at(rec, o).unwrap_or(0.0);
        let u = |o: usize| u32_at(rec, o).unwrap_or(0);
        let fill = |canvas: &mut Canvas, d: &str, dc: &Dc, closed: bool| {
            if closed {
                canvas.path(d, dc.brush.color, dc.even_odd, Some((dc.pen, s)));
            } else {
                canvas.path(d, None, false, Some((dc.pen, s)));
            }
        };
        match kind {
            9 => dc.window_ext = (i(8), i(12)),
            10 => dc.window_org = (i(8), i(12)),
            11 => dc.viewport_ext = (i(8), i(12)),
            12 => dc.viewport_org = (i(8), i(12)),
            17 => dc.map_mode = u(8),
            19 => dc.even_odd = u(8) != 2,
            22 => dc.text_align = u(8),
            24 => dc.text_color = colorref(rec, 8).unwrap_or([0; 3]),
            33 => stack.push(dc.clone()),
            34 => restore(&mut stack, &mut dc, i(8) as i32),
            35 | 36 => {
                let x = [
                    f32_at(rec, 8),
                    f32_at(rec, 12),
                    f32_at(rec, 16),
                    f32_at(rec, 20),
                    f32_at(rec, 24),
                    f32_at(rec, 28),
                ];
                let x: Affine = [
                    x[0].unwrap_or(1.0),
                    x[1].unwrap_or(0.0),
                    x[2].unwrap_or(0.0),
                    x[3].unwrap_or(1.0),
                    x[4].unwrap_or(0.0),
                    x[5].unwrap_or(0.0),
                ];
                if x.iter().all(|v| v.is_finite()) {
                    dc.world = match (kind, u(32)) {
                        (35, _) | (36, 4) => x,
                        (36, 1) => IDENTITY,
                        // MWT_LEFTMULTIPLY: the new transform first.
                        (36, 2) => compose(&dc.world, &x),
                        (36, 3) => compose(&x, &dc.world),
                        _ => dc.world,
                    };
                }
            }
            37 => {
                let h = u(8);
                let obj = if h & STOCK != 0 {
                    stock(h)
                } else {
                    objects.get(&h).cloned()
                };
                if let Some(obj) = obj {
                    select(&mut dc, &obj);
                }
            }
            38 => {
                let obj = Obj::Pen(pen(u(12), i(16).abs(), colorref(rec, 24).unwrap_or([0; 3])));
                objects.insert(u(8), obj);
            }
            95 => {
                let obj = Obj::Pen(pen(
                    u(28),
                    f64::from(u(32)),
                    colorref(rec, 40).unwrap_or([0; 3]),
                ));
                objects.insert(u(8), obj);
            }
            39 => {
                objects.insert(u(8), brush(u(12), colorref(rec, 16).unwrap_or([0; 3])));
            }
            93 | 94 | 48 | 49 => {
                objects.insert(u(8), Obj::Other);
            }
            82 => {
                let face: String = char::decode_utf16(
                    (0..32).map_while(|k| u16_at(rec, 40 + k * 2).filter(|c| *c != 0)),
                )
                .map(|c| c.unwrap_or('\u{FFFD}'))
                .collect();
                let font = Font {
                    height: i(12),
                    weight: i(28) as i32,
                    italic: rec.get(32).is_some_and(|b| *b != 0),
                    face,
                    escapement: i(20) / 10.0,
                };
                objects.insert(u(8), Obj::Font(font));
            }
            40 => {
                objects.remove(&u(8));
            }
            27 => {
                dc.current = (i(8), i(12));
                if let Some(p) = path.as_mut() {
                    let q = apply(&m, dc.current);
                    let _ = write!(p, "M{} {} ", num(q.0), num(q.1));
                }
            }
            54 => {
                let to = (i(8), i(12));
                match path.as_mut() {
                    Some(p) => {
                        let q = apply(&m, to);
                        let _ = write!(p, "L{} {} ", num(q.0), num(q.1));
                    }
                    None => {
                        let d = poly_d(&[apply(&m, dc.current), apply(&m, to)], false);
                        fill(&mut canvas, &d, &dc, false);
                        drew = true;
                    }
                }
                dc.current = to;
            }
            42..=44 => {
                let (l, t, r, b) = (i(8), i(12), i(16), i(20));
                let d = match kind {
                    42 => ellipse_d(&m, l, t, r, b),
                    43 => rounded_d(&m, l, t, r, b, 0.0, 0.0),
                    _ => rounded_d(&m, l, t, r, b, i(24).abs() / 2.0, i(28).abs() / 2.0),
                };
                match path.as_mut() {
                    Some(p) => p.push_str(&d),
                    None => {
                        fill(&mut canvas, &d, &dc, true);
                        drew = true;
                    }
                }
            }
            2..=6 | 85..=89 => {
                let wide = kind < 85;
                let n = u(24) as usize;
                let pts = if wide {
                    points32(rec, 28, n)
                } else {
                    points16(rec, 28, n)
                }
                .ok_or_else(|| bad("a shape's points run past its record"))?;
                let pts: Vec<_> = pts.into_iter().map(|q| apply(&m, q)).collect();
                let base = if wide { kind } else { kind - 83 };
                // 2 PolyBezier, 3 Polygon, 4 Polyline, 5 PolyBezierTo, 6 PolylineTo.
                let mut d = String::new();
                let (to, bezier) = (matches!(base, 5 | 6), matches!(base, 2 | 5));
                let mut rest: &[(f64, f64)] = &pts;
                if !to {
                    if let Some((first, tail)) = pts.split_first() {
                        let _ = write!(d, "M{} {} ", num(first.0), num(first.1));
                        rest = tail;
                    }
                } else if path.is_none() {
                    let c = apply(&m, dc.current);
                    let _ = write!(d, "M{} {} ", num(c.0), num(c.1));
                }
                if bezier {
                    for c in rest.as_chunks::<3>().0 {
                        let _ = write!(
                            d,
                            "C{} {} {} {} {} {} ",
                            num(c[0].0),
                            num(c[0].1),
                            num(c[1].0),
                            num(c[1].1),
                            num(c[2].0),
                            num(c[2].1)
                        );
                    }
                } else {
                    for q in rest {
                        let _ = write!(d, "L{} {} ", num(q.0), num(q.1));
                    }
                }
                if base == 3 {
                    d.push('Z');
                }
                if to {
                    // The current position moves to the last point (logical).
                    let raw = if wide {
                        points32(rec, 28, n)
                    } else {
                        points16(rec, 28, n)
                    };
                    if let Some(last) = raw.and_then(|v| v.last().copied()) {
                        dc.current = last;
                    }
                }
                match path.as_mut() {
                    Some(p) => p.push_str(&d),
                    None => {
                        fill(&mut canvas, &d, &dc, base == 3);
                        drew = true;
                    }
                }
            }
            7 | 8 | 90 | 91 => {
                let wide = kind < 90;
                let polys = u(24) as usize;
                let total = u(28) as usize;
                if polys > len / 4 {
                    return Err(bad("a poly-polygon's counts run past its record"));
                }
                let counts: Vec<usize> = (0..polys).map(|k| u(32 + k * 4) as usize).collect();
                let pts_at = 32 + polys * 4;
                let pts = if wide {
                    points32(rec, pts_at, total)
                } else {
                    points16(rec, pts_at, total)
                }
                .ok_or_else(|| bad("a poly-polygon's points run past its record"))?;
                let closed = matches!(kind, 8 | 91);
                let mut d = String::new();
                let mut k = 0usize;
                for n in counts {
                    let Some(slice) = pts.get(k..k.saturating_add(n)) else {
                        return Err(bad("a poly-polygon's counts exceed its points"));
                    };
                    k += n;
                    let dev: Vec<_> = slice.iter().map(|q| apply(&m, *q)).collect();
                    d.push_str(&poly_d(&dev, closed));
                }
                match path.as_mut() {
                    Some(p) => p.push_str(&d),
                    None => {
                        fill(&mut canvas, &d, &dc, closed);
                        drew = true;
                    }
                }
            }
            59 => path = Some(String::new()),
            60 => bracket_done = path.take(),
            61 => {
                if let Some(p) = path.as_mut() {
                    p.push_str("Z ");
                }
            }
            68 => {
                path = None;
                bracket_done = None;
            }
            62..=64 => {
                if let Some(d) = bracket_done.take().or_else(|| path.take()) {
                    let fill_color = if kind == 64 { None } else { dc.brush.color };
                    let stroke = if kind == 62 { None } else { Some((dc.pen, s)) };
                    canvas.path(&d, fill_color, dc.even_odd, stroke);
                    drew = true;
                }
            }
            81 => {
                let (dx, dy) = (i(24), i(28));
                let (off_bmi, cb_bmi, off_bits, cb_bits) = (
                    u(48) as usize,
                    u(52) as usize,
                    u(56) as usize,
                    u(60) as usize,
                );
                let (cx, cy) = (i(72), i(76));
                let bmi = rec.get(off_bmi..off_bmi.saturating_add(cb_bmi));
                let bits = rec.get(off_bits..off_bits.saturating_add(cb_bits));
                if let (Some(bmi), Some(bits)) = (bmi, bits) {
                    canvas.dib(
                        bmi,
                        bits,
                        apply(&m, (dx, dy)),
                        apply(&m, (dx + cx, dy + cy)),
                    )?;
                    drew = true;
                }
            }
            84 => {
                let (rx, ry) = (i(36), i(40));
                let n = u(44) as usize;
                let off = u(48) as usize;
                let units: Vec<u16> = (0..n.min(len / 2))
                    .map_while(|k| u16_at(rec, off + k * 2))
                    .collect();
                let text: String = char::decode_utf16(units)
                    .map(|c| c.unwrap_or('\u{FFFD}'))
                    .collect();
                let size = dc.font.height.abs() * s;
                canvas.text(
                    apply(&m, (rx, ry)),
                    &text,
                    &dc.font,
                    size,
                    dc.text_color,
                    dc.text_align,
                    dc.font.escapement,
                );
                drew = true;
            }
            _ => {}
        }
    }
    if !drew {
        return Err(bad(
            "it holds no drawing records this build reads (an EMF+-only file draws through records this build does not interpret)",
        ));
    }
    canvas.finish((l, t), size, ImportFormat::Emf)
}

/// Header facts: a metafile is rendered to learn its size exactly.
pub(super) fn probe(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<ImageInfo, CodecError> {
    let s = decode(format, bytes, limits)?;
    Ok(super::info(s.width, s.height, format, false))
}

/// Render a WMF or an EMF.
pub(super) fn decode(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    match format {
        ImportFormat::Emf => decode_emf(bytes, limits),
        _ => decode_wmf(bytes, limits),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codec::{decode_surface_bytes, decode_surface_bytes_as, SurfacePixels};

    fn px(s: &DecodedSurface, x: u32, y: u32) -> [u8; 4] {
        let SurfacePixels::Rgba8(p) = &s.pixels else {
            panic!("8-bit expected")
        };
        let i = ((y * s.width + x) * 4) as usize;
        [p[i], p[i + 1], p[i + 2], p[i + 3]]
    }

    /// A WMF record: size in words, function, parameters.
    fn wrec(func: u16, params: &[i16]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&((3 + params.len()) as u32).to_le_bytes());
        v.extend_from_slice(&func.to_le_bytes());
        for p in params {
            v.extend_from_slice(&p.to_le_bytes());
        }
        v
    }

    fn colour(r: u8, g: u8, b: u8) -> [i16; 2] {
        [i16::from_le_bytes([r, g]), i16::from_le_bytes([b, 0])]
    }

    /// A placeable WMF of `records`, 1440 units per inch over
    /// `(0, 0, w, h)`, so `w` units are `w * 96 / 1440` pixels.
    pub(crate) fn build_wmf(w: i16, h: i16, inch: u16, records: &[Vec<u8>]) -> Vec<u8> {
        let mut v = WMF_PLACEABLE.to_vec();
        v.extend_from_slice(&[0, 0]);
        for c in [0i16, 0, w, h] {
            v.extend_from_slice(&c.to_le_bytes());
        }
        v.extend_from_slice(&inch.to_le_bytes());
        v.extend_from_slice(&[0; 4]);
        v.extend_from_slice(&[0; 2]);
        // META_HEADER
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&9u16.to_le_bytes());
        v.extend_from_slice(&0x0300u16.to_le_bytes());
        v.extend_from_slice(&[0; 12]);
        for r in records {
            v.extend_from_slice(r);
        }
        v.extend_from_slice(&wrec(0, &[]));
        v
    }

    /// A WMF picture: a red box (null pen) on the left half and a blue
    /// polygon on the right half of a 40 x 20 px frame.
    pub(crate) fn sample_wmf() -> Vec<u8> {
        let red = colour(255, 0, 0);
        let blue = colour(0, 0, 255);
        build_wmf(
            400,
            200,
            960,
            &[
                wrec(0x020B, &[0, 0]),
                wrec(0x020C, &[200, 400]),
                wrec(0x02FA, &[5, 0, 0, 0, 0]), // null pen -> slot 0
                wrec(0x012D, &[0]),
                wrec(0x02FC, &[0, red[0], red[1], 0]), // slot 1
                wrec(0x012D, &[1]),
                wrec(0x041B, &[200, 200, 0, 0]), // bottom right top left
                wrec(0x02FC, &[0, blue[0], blue[1], 0]), // slot 2
                wrec(0x012D, &[2]),
                wrec(0x0324, &[4, 200, 0, 400, 0, 400, 200, 200, 200]),
            ],
        )
    }

    #[test]
    fn a_wmf_draws_its_shapes_at_its_placeable_size() {
        let s = decode_surface_bytes(&sample_wmf(), ImportLimits::default()).unwrap();
        // 400 units at 960 per inch, 96 px per inch: 40 px.
        assert_eq!(
            (s.width, s.height, s.source_format),
            (40, 20, ImportFormat::Wmf)
        );
        assert_eq!(px(&s, 5, 10), [255, 0, 0, 255], "the red rectangle");
        assert_eq!(px(&s, 35, 10), [0, 0, 255, 255], "the blue polygon");
    }

    #[test]
    fn a_wmf_ellipse_and_a_stroked_line_draw() {
        let green = colour(0, 200, 0);
        let wmf = build_wmf(
            300,
            300,
            1440,
            &[
                wrec(0x02FC, &[0, green[0], green[1], 0]),
                wrec(0x012D, &[0]),
                wrec(0x02FA, &[5, 0, 0, 0, 0]),
                wrec(0x012D, &[1]),
                wrec(0x0418, &[300, 300, 0, 0]),
            ],
        );
        let s = decode_surface_bytes(&wmf, ImportLimits::default()).unwrap();
        assert_eq!((s.width, s.height), (20, 20));
        assert_eq!(px(&s, 10, 10), [0, 200, 0, 255], "inside the ellipse");
        assert_eq!(px(&s, 0, 0)[3], 0, "the corner is outside it");
    }

    /// An EMF record.
    fn erec(kind: u32, params: &[u32]) -> Vec<u8> {
        let mut v = kind.to_le_bytes().to_vec();
        v.extend_from_slice(&((8 + params.len() * 4) as u32).to_le_bytes());
        for p in params {
            v.extend_from_slice(&p.to_le_bytes());
        }
        v
    }

    /// An EMF with device bounds `(0, 0, w - 1, h - 1)` holding `records`.
    pub(crate) fn build_emf(w: i32, h: i32, records: &[Vec<u8>]) -> Vec<u8> {
        let mut hdr = vec![0u8; 88];
        hdr[0..4].copy_from_slice(&1u32.to_le_bytes());
        hdr[4..8].copy_from_slice(&88u32.to_le_bytes());
        hdr[16..20].copy_from_slice(&(w - 1).to_le_bytes());
        hdr[20..24].copy_from_slice(&(h - 1).to_le_bytes());
        hdr[40..44].copy_from_slice(b" EMF");
        hdr[72..76].copy_from_slice(&1920i32.to_le_bytes());
        hdr[76..80].copy_from_slice(&1080i32.to_le_bytes());
        hdr[80..84].copy_from_slice(&508i32.to_le_bytes());
        hdr[84..88].copy_from_slice(&286i32.to_le_bytes());
        let mut v = hdr;
        for r in records {
            v.extend_from_slice(r);
        }
        v.extend_from_slice(&erec(14, &[0, 0, 0]));
        v
    }

    fn cref(r: u8, g: u8, b: u8) -> u32 {
        u32::from_le_bytes([r, g, b, 0])
    }

    pub(crate) fn sample_emf() -> Vec<u8> {
        build_emf(
            30,
            20,
            &[
                erec(37, &[STOCK | 8]), // NULL_PEN
                erec(39, &[1, 0, cref(255, 128, 0), 0]),
                erec(37, &[1]),
                erec(43, &[0, 0, 15, 20]), // rectangle, left half
                erec(39, &[2, 0, cref(0, 0, 255), 0]),
                erec(37, &[2]),
                // Polygon16: bounds, count 3, a triangle on the right.
                {
                    let mut r = erec(86, &[0, 0, 0, 0, 3]);
                    for (x, y) in [(15i16, 0i16), (30, 0), (30, 20)] {
                        r.extend_from_slice(&x.to_le_bytes());
                        r.extend_from_slice(&y.to_le_bytes());
                    }
                    let n = r.len() as u32;
                    r[4..8].copy_from_slice(&n.to_le_bytes());
                    r
                },
            ],
        )
    }

    #[test]
    fn an_emf_draws_its_shapes_in_device_bounds() {
        let s = decode_surface_bytes(&sample_emf(), ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (30, 20, ImportFormat::Emf)
        );
        assert_eq!(px(&s, 5, 10), [255, 128, 0, 255], "the orange rectangle");
        assert_eq!(px(&s, 28, 3), [0, 0, 255, 255], "the blue triangle");
        assert_eq!(px(&s, 17, 18)[3], 0, "below the triangle's diagonal");
    }

    #[test]
    fn an_emf_world_transform_and_path_bracket_apply() {
        // Scale by 2 through SetWorldTransform, then fill a 0..5 square
        // through a path bracket: it covers 0..10 px.
        let f = |v: f32| v.to_bits();
        let emf = build_emf(
            20,
            20,
            &[
                erec(35, &[f(2.0), f(0.0), f(0.0), f(2.0), f(0.0), f(0.0)]),
                erec(37, &[STOCK | 8]),
                erec(37, &[STOCK | 4]), // BLACK_BRUSH
                erec(59, &[]),
                erec(27, &[0, 0]),
                erec(54, &[5, 0]),
                erec(54, &[5, 5]),
                erec(54, &[0, 5]),
                erec(61, &[]),
                erec(60, &[]),
                erec(62, &[0, 0, 0, 0]),
            ],
        );
        let s = decode_surface_bytes(&emf, ImportLimits::default()).unwrap();
        assert_eq!(px(&s, 8, 8), [0, 0, 0, 255], "inside the scaled square");
        assert_eq!(px(&s, 14, 14)[3], 0, "outside it");
    }

    #[test]
    fn a_stretchdibits_bitmap_is_drawn() {
        // A 2x1 24-bit DIB (red, green), stretched onto 10x10 px.
        let mut bmi = vec![0u8; 40];
        bmi[0..4].copy_from_slice(&40u32.to_le_bytes());
        bmi[4..8].copy_from_slice(&2i32.to_le_bytes());
        bmi[8..12].copy_from_slice(&1i32.to_le_bytes());
        bmi[12..14].copy_from_slice(&1u16.to_le_bytes());
        bmi[14..16].copy_from_slice(&24u16.to_le_bytes());
        let bits = vec![0, 0, 255, 0, 255, 0, 0, 0]; // BGR BGR + row pad
        let mut rec = erec(81, &[0; 18]);
        let off_bmi = rec.len() as u32;
        rec.extend_from_slice(&bmi);
        let off_bits = rec.len() as u32;
        rec.extend_from_slice(&bits);
        let n = rec.len() as u32;
        rec[4..8].copy_from_slice(&n.to_le_bytes());
        let put =
            |r: &mut Vec<u8>, at: usize, v: u32| r[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut rec, 48, off_bmi);
        put(&mut rec, 52, 40);
        put(&mut rec, 56, off_bits);
        put(&mut rec, 60, bits.len() as u32);
        put(&mut rec, 72, 10);
        put(&mut rec, 76, 10);
        let emf = build_emf(10, 10, &[rec]);
        let s = decode_surface_bytes(&emf, ImportLimits::default()).unwrap();
        assert_eq!(px(&s, 1, 5), [255, 0, 0, 255], "the DIB's left pixel");
        assert_eq!(px(&s, 8, 5), [0, 255, 0, 255], "its right pixel");
    }

    #[test]
    fn malformed_metafiles_error_and_never_panic() {
        let limits = ImportLimits::default();
        let wmf = sample_wmf();
        let emf = sample_emf();
        for cut in 0..wmf.len() {
            let _ = decode_surface_bytes_as(&wmf[..cut], limits, ImportFormat::Wmf);
        }
        for cut in 0..emf.len() {
            let _ = decode_surface_bytes_as(&emf[..cut], limits, ImportFormat::Emf);
        }
        // A record claiming to run past the file.
        let mut long = sample_emf();
        long[88 + 4..88 + 8].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(decode_emf(&long, limits).is_err());
        // A polygon whose count exceeds its record.
        let lying = build_wmf(100, 100, 1440, &[wrec(0x0324, &[30000, 0, 0])]);
        assert!(decode_wmf(&lying, limits).is_err());
        // Nothing drawable.
        let empty = build_emf(10, 10, &[]);
        let err = decode_emf(&empty, limits).unwrap_err();
        assert!(err.to_string().contains("no drawing records"), "{err}");
        // A picture past the limits.
        let huge = build_emf(60000, 60000, &[erec(43, &[0, 0, 5, 5])]);
        assert!(matches!(
            decode_emf(&huge, limits),
            Err(CodecError::LimitExceeded(_))
        ));
        // Byte noise after a valid header.
        let mut noise = build_emf(10, 10, &[]);
        noise.truncate(88);
        noise.extend((0..400u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
        let _ = decode_emf(&noise, limits);
    }
}
