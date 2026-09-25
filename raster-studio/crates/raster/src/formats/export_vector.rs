//! W16-K: File > Export As > PDF, EMF and DXF: vector writers.
//!
//! The application walks its layer tree and hands this module a neutral
//! [`VectorDoc`]: pages (one per artboard, or the canvas), each a bottom-to-top
//! list of [`Item`]s in page pixels (y down). A shape or text layer arrives
//! as a [`VectorPath`] (its outline, its fill and stroke), anything else as a
//! [`PlacedImage`] (that layer rendered on its own). The writers then say it
//! in each format's own vocabulary:
//!
//! * **PDF** ([`encode_pdf`]): one page per [`Page`], one PDF point per
//!   document pixel (72 ppi). Paths are PDF path operators (`m l c h`,
//!   `f`/`f*`/`S`/`B`), their alpha an `ExtGState`; images are Flate-coded
//!   `DeviceRGB` XObjects with a `DeviceGray` soft mask for their alpha.
//! * **EMF** ([`encode_emf`]): the first page, in GDI records: path brackets
//!   (`BEGINPATH`, `MOVETOEX`, `LINETO`, `POLYBEZIERTO`, `CLOSEFIGURE`,
//!   `ENDPATH`, `FILLPATH` / `STROKEPATH` / `STROKEANDFILLPATH`) with solid
//!   brushes and pens, at 1/16 pixel precision through an anisotropic map
//!   mode; images are `STRETCHDIBITS` of a 32-bit DIB. GDI has no alpha for a
//!   brush, a pen or a `StretchDIBits`, so paint alpha is dropped and an
//!   image's translucent pixels are flattened onto white.
//! * **DXF** ([`encode_dxf`]): the first page as AutoCAD R12 (`AC1009`)
//!   ASCII `POLYLINE` entities, one per subpath, Beziers flattened to 16
//!   segments, y flipped up, the fill (else the stroke) colour as the nearest
//!   AutoCAD Color Index. DXF has no raster entity that carries its pixels
//!   inline, so images are not written; a DXF of a page with no vectors is a
//!   valid, empty drawing.

use std::fmt::Write as _;
use std::io::Write as _;

use crate::codec::CodecError;

/// A point in page pixels, y down.
pub type Pt = [f64; 2];

/// One path command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    Move(Pt),
    Line(Pt),
    /// A cubic Bezier: two controls, the end.
    Cubic(Pt, Pt, Pt),
    Close,
}

/// A solid paint: 8-bit sRGB and a straight alpha in `0..=1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    pub rgb: [u8; 3],
    pub alpha: f32,
}

/// A centred stroke.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub paint: Paint,
    /// Width in page pixels.
    pub width: f64,
}

/// A filled and / or stroked outline.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorPath {
    pub segs: Vec<Seg>,
    pub fill: Option<Paint>,
    /// The even-odd rule (else non-zero winding).
    pub even_odd: bool,
    pub stroke: Option<Stroke>,
}

/// Straight-alpha RGBA8 pixels placed at `(x, y)` in page pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedImage {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One thing drawn on a page, bottom to top.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Path(VectorPath),
    Image(PlacedImage),
}

/// One page, `width` x `height` pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub width: u32,
    pub height: u32,
    pub items: Vec<Item>,
}

impl Page {
    /// A page that is one image of `rgba` (the codec's own fallback for a
    /// composite with no layer structure).
    pub fn raster(width: u32, height: u32, rgba: &[u8]) -> Page {
        Page {
            width,
            height,
            items: vec![Item::Image(PlacedImage {
                x: 0,
                y: 0,
                width,
                height,
                rgba: rgba.to_vec(),
            })],
        }
    }

    /// How many items are paths and how many images.
    pub fn counts(&self) -> (usize, usize) {
        let paths = self
            .items
            .iter()
            .filter(|i| matches!(i, Item::Path(_)))
            .count();
        (paths, self.items.len() - paths)
    }
}

/// A whole export: its pages in order, and a title for the PDF's Info.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VectorDoc {
    pub pages: Vec<Page>,
    pub title: String,
}

fn invalid(what: &str) -> CodecError {
    CodecError::InvalidParameter(what.to_string())
}

fn check_page(page: &Page) -> Result<(), CodecError> {
    if page.width == 0 || page.height == 0 {
        return Err(invalid("a vector export page needs a non-zero size"));
    }
    for item in &page.items {
        if let Item::Image(img) = item {
            let need = img.width as usize * img.height as usize * 4;
            if img.rgba.len() != need {
                return Err(invalid("a placed image does not hold width x height RGBA"));
            }
        }
    }
    Ok(())
}

/// A number with at most three decimals and no float noise.
fn num(v: f64) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing into a Vec cannot fail.
    let _ = enc.write_all(bytes);
    enc.finish().unwrap_or_default()
}

fn pdf_string(text: &str) -> String {
    let mut out = String::from("(");
    for c in text.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
            _ => out.push('?'),
        }
    }
    out.push(')');
    out
}

// ---------------------------------------------------------------------------
// PDF
// ---------------------------------------------------------------------------

/// Write `doc` as a PDF, one page per [`Page`].
pub fn encode_pdf(doc: &VectorDoc) -> Result<Vec<u8>, CodecError> {
    if doc.pages.is_empty() {
        return Err(invalid("a PDF needs at least one page"));
    }
    for page in &doc.pages {
        check_page(page)?;
    }
    // Objects are numbered from 1; `objects[n - 1]` is object n's body.
    let mut objects: Vec<Vec<u8>> = vec![Vec::new(), Vec::new()];
    let mut page_ids = Vec::new();
    for page in &doc.pages {
        let mut content = String::new();
        let mut xobjects = String::new();
        let mut states = String::new();
        let _ = writeln!(content, "q 1 0 0 -1 0 {} cm", page.height);
        for item in &page.items {
            match item {
                Item::Path(path) => {
                    if path.fill.is_none() && path.stroke.is_none() {
                        continue;
                    }
                    content.push_str("q\n");
                    let ca = path.fill.map_or(1.0, |p| p.alpha);
                    let big_ca = path.stroke.map_or(1.0, |s| s.paint.alpha);
                    if ca < 1.0 || big_ca < 1.0 {
                        let name = format!("G{}", states.matches("/Type").count());
                        let _ = write!(
                            states,
                            "/{name} << /Type /ExtGState /ca {} /CA {} >> ",
                            num(f64::from(ca.clamp(0.0, 1.0))),
                            num(f64::from(big_ca.clamp(0.0, 1.0)))
                        );
                        let _ = writeln!(content, "/{name} gs");
                    }
                    if let Some(fill) = path.fill {
                        let [r, g, b] = fill.rgb.map(|c| num(f64::from(c) / 255.0));
                        let _ = writeln!(content, "{r} {g} {b} rg");
                    }
                    if let Some(stroke) = path.stroke {
                        let [r, g, b] = stroke.paint.rgb.map(|c| num(f64::from(c) / 255.0));
                        let _ =
                            writeln!(content, "{r} {g} {b} RG {} w", num(stroke.width.max(0.0)));
                    }
                    for seg in &path.segs {
                        let _ = match *seg {
                            Seg::Move(p) => writeln!(content, "{} {} m", num(p[0]), num(p[1])),
                            Seg::Line(p) => writeln!(content, "{} {} l", num(p[0]), num(p[1])),
                            Seg::Cubic(a, b, p) => writeln!(
                                content,
                                "{} {} {} {} {} {} c",
                                num(a[0]),
                                num(a[1]),
                                num(b[0]),
                                num(b[1]),
                                num(p[0]),
                                num(p[1])
                            ),
                            Seg::Close => writeln!(content, "h"),
                        };
                    }
                    let op = match (path.fill.is_some(), path.stroke.is_some(), path.even_odd) {
                        (true, true, false) => "B",
                        (true, true, true) => "B*",
                        (true, false, false) => "f",
                        (true, false, true) => "f*",
                        _ => "S",
                    };
                    let _ = writeln!(content, "{op}\nQ");
                }
                Item::Image(img) => {
                    if img.width == 0 || img.height == 0 {
                        continue;
                    }
                    let pixels = img.width as usize * img.height as usize;
                    let mut rgb = Vec::with_capacity(pixels * 3);
                    let mut alpha = Vec::with_capacity(pixels);
                    for px in img.rgba.as_chunks::<4>().0 {
                        rgb.extend_from_slice(&px[..3]);
                        alpha.push(px[3]);
                    }
                    let smask_id = objects.len() + 1;
                    objects.push(stream_object(
                        &format!(
                            "/Type /XObject /Subtype /Image /Width {} /Height {} \
                             /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode",
                            img.width, img.height
                        ),
                        &deflate(&alpha),
                    ));
                    let image_id = objects.len() + 1;
                    objects.push(stream_object(
                        &format!(
                            "/Type /XObject /Subtype /Image /Width {} /Height {} \
                             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode \
                             /SMask {smask_id} 0 R",
                            img.width, img.height
                        ),
                        &deflate(&rgb),
                    ));
                    let name = format!("Im{image_id}");
                    let _ = write!(xobjects, "/{name} {image_id} 0 R ");
                    let _ = writeln!(
                        content,
                        "q {} 0 0 {} {} {} cm /{name} Do Q",
                        img.width,
                        -i64::from(img.height),
                        img.x,
                        img.y + i64::from(img.height)
                    );
                }
            }
        }
        content.push_str("Q\n");
        let content_id = objects.len() + 1;
        objects.push(stream_object(
            "/Filter /FlateDecode",
            &deflate(content.as_bytes()),
        ));
        let page_id = objects.len() + 1;
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] \
                 /Resources << /XObject << {xobjects}>> /ExtGState << {states}>> >> \
                 /Contents {content_id} 0 R >>",
                page.width, page.height
            )
            .into_bytes(),
        );
        page_ids.push(page_id);
    }
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
    let kids: Vec<String> = page_ids.iter().map(|id| format!("{id} 0 R")).collect();
    objects[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        page_ids.len()
    )
    .into_bytes();
    let info_id = objects.len() + 1;
    let mut info = String::from("<< /Producer (Raster Studio)");
    if !doc.title.is_empty() {
        let _ = write!(info, " /Title {}", pdf_string(&doc.title));
    }
    info.push_str(" >>");
    objects.push(info.into_bytes());

    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info {info_id} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Ok(out)
}

fn stream_object(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

// ---------------------------------------------------------------------------
// EMF
// ---------------------------------------------------------------------------

/// Logical units per page pixel in an EMF (see the module docs).
pub const EMF_SUBPIXELS: i32 = 16;

struct Emf {
    out: Vec<u8>,
    records: u32,
}

impl Emf {
    fn record(&mut self, kind: u32, body: &[u8]) {
        let size = 8 + body.len() as u32;
        self.out.extend_from_slice(&kind.to_le_bytes());
        self.out.extend_from_slice(&size.to_le_bytes());
        self.out.extend_from_slice(body);
        self.records += 1;
    }
}

fn le(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn colorref(rgb: [u8; 3]) -> i32 {
    i32::from(rgb[0]) | (i32::from(rgb[1]) << 8) | (i32::from(rgb[2]) << 16)
}

fn logical(p: Pt) -> [i32; 2] {
    let s = f64::from(EMF_SUBPIXELS);
    let clamp = |v: f64| {
        let v = if v.is_finite() { v * s } else { 0.0 };
        v.round().clamp(-1.0e9, 1.0e9) as i32
    };
    [clamp(p[0]), clamp(p[1])]
}

const EMR_HEADER: u32 = 1;
const EMR_POLYBEZIERTO: u32 = 5;
const EMR_SETWINDOWEXTEX: u32 = 9;
const EMR_SETVIEWPORTEXTEX: u32 = 11;
const EMR_EOF: u32 = 14;
const EMR_SETMAPMODE: u32 = 17;
const EMR_SETPOLYFILLMODE: u32 = 19;
const EMR_MOVETOEX: u32 = 27;
const EMR_SELECTOBJECT: u32 = 37;
const EMR_CREATEPEN: u32 = 38;
const EMR_CREATEBRUSHINDIRECT: u32 = 39;
const EMR_DELETEOBJECT: u32 = 40;
const EMR_LINETO: u32 = 54;
const EMR_BEGINPATH: u32 = 59;
const EMR_ENDPATH: u32 = 60;
const EMR_CLOSEFIGURE: u32 = 61;
const EMR_FILLPATH: u32 = 62;
const EMR_STROKEANDFILLPATH: u32 = 63;
const EMR_STROKEPATH: u32 = 64;
const EMR_STRETCHDIBITS: u32 = 81;
const NULL_BRUSH: i32 = 0x8000_0005_u32 as i32;
const NULL_PEN: i32 = 0x8000_0008_u32 as i32;

/// Write the first page of `doc` as an Enhanced Metafile.
pub fn encode_emf(doc: &VectorDoc) -> Result<Vec<u8>, CodecError> {
    let page = doc
        .pages
        .first()
        .ok_or_else(|| invalid("an EMF needs a page"))?;
    check_page(page)?;
    let (w, h) = (page.width as i32, page.height as i32);
    let mut emf = Emf {
        out: Vec::new(),
        records: 0,
    };
    // The header is written last, once the size and count are known; a
    // placeholder of its size goes first.
    const HEADER_BODY: usize = 100;
    emf.out.resize(8 + HEADER_BODY, 0);
    emf.records = 1;
    emf.record(EMR_SETMAPMODE, &le(&[8])); // MM_ANISOTROPIC
    emf.record(
        EMR_SETWINDOWEXTEX,
        &le(&[w * EMF_SUBPIXELS, h * EMF_SUBPIXELS]),
    );
    emf.record(EMR_SETVIEWPORTEXTEX, &le(&[w, h]));
    for item in &page.items {
        match item {
            Item::Path(path) => emf_path(&mut emf, path),
            Item::Image(img) => emf_image(&mut emf, img),
        }
    }
    emf.record(EMR_EOF, &le(&[0, 16, 20]));
    // The header: bounds (device pixels, inclusive), frame (0.01 mm at 96
    // dpi), signature, version, bytes, records, handles, the reference
    // device and its millimetres, then the two extensions (no pixel format,
    // no OpenGL; micrometres).
    let bytes = emf.out.len() as u32;
    let mm = |px: i32| (f64::from(px) * 25.4 / 96.0).round() as i32;
    let mut header = le(&[
        0,
        0,
        w - 1,
        h - 1,
        0,
        0,
        (f64::from(w) * 2540.0 / 96.0).round() as i32 - 1,
        (f64::from(h) * 2540.0 / 96.0).round() as i32 - 1,
        0x464D_4520,
        0x0001_0000,
    ]);
    header.extend_from_slice(&bytes.to_le_bytes());
    header.extend_from_slice(&emf.records.to_le_bytes());
    // Handles: the one brush and one pen a path creates at a time, plus the
    // reserved zero index.
    header.extend_from_slice(&3u16.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&le(&[0, 0, 0, w, h, mm(w), mm(h), 0, 0, 0]));
    header.extend_from_slice(&le(&[mm(w) * 1000, mm(h) * 1000]));
    debug_assert_eq!(header.len(), HEADER_BODY);
    let mut head = EMR_HEADER.to_le_bytes().to_vec();
    head.extend_from_slice(&((8 + HEADER_BODY) as u32).to_le_bytes());
    head.extend_from_slice(&header);
    emf.out[..8 + HEADER_BODY].copy_from_slice(&head);
    Ok(emf.out)
}

fn emf_path(emf: &mut Emf, path: &VectorPath) {
    if path.fill.is_none() && path.stroke.is_none() {
        return;
    }
    let brush = path.fill.map(|fill| {
        emf.record(EMR_CREATEBRUSHINDIRECT, &le(&[1, 0, colorref(fill.rgb), 0]));
        emf.record(EMR_SELECTOBJECT, &le(&[1]));
        1
    });
    if brush.is_none() {
        emf.record(EMR_SELECTOBJECT, &le(&[NULL_BRUSH]));
    }
    let pen = path.stroke.map(|stroke| {
        let width = (stroke.width.max(0.0) * f64::from(EMF_SUBPIXELS)).round() as i32;
        emf.record(
            EMR_CREATEPEN,
            &le(&[2, 0, width.max(1), 0, colorref(stroke.paint.rgb)]),
        );
        emf.record(EMR_SELECTOBJECT, &le(&[2]));
        2
    });
    if pen.is_none() {
        emf.record(EMR_SELECTOBJECT, &le(&[NULL_PEN]));
    }
    emf.record(
        EMR_SETPOLYFILLMODE,
        &le(&[if path.even_odd { 1 } else { 2 }]),
    );
    emf.record(EMR_BEGINPATH, &[]);
    let mut min = [i32::MAX; 2];
    let mut max = [i32::MIN; 2];
    let mut grow = |p: [i32; 2]| {
        for k in 0..2 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    };
    for seg in &path.segs {
        match *seg {
            Seg::Move(p) => {
                let p = logical(p);
                grow(p);
                emf.record(EMR_MOVETOEX, &le(&p));
            }
            Seg::Line(p) => {
                let p = logical(p);
                grow(p);
                emf.record(EMR_LINETO, &le(&p));
            }
            Seg::Cubic(a, b, p) => {
                let pts = [logical(a), logical(b), logical(p)];
                let lo = [0, 1].map(|k| pts.iter().map(|q| q[k]).min().unwrap_or(0));
                let hi = [0, 1].map(|k| pts.iter().map(|q| q[k]).max().unwrap_or(0));
                pts.iter().for_each(|q| grow(*q));
                let mut body = le(&[lo[0], lo[1], hi[0], hi[1], 3]);
                for q in pts {
                    body.extend_from_slice(&le(&q));
                }
                emf.record(EMR_POLYBEZIERTO, &body);
            }
            Seg::Close => emf.record(EMR_CLOSEFIGURE, &[]),
        }
    }
    emf.record(EMR_ENDPATH, &[]);
    if min[0] > max[0] {
        min = [0, 0];
        max = [0, 0];
    }
    let bounds = le(&[min[0], min[1], max[0], max[1]]);
    let kind = match (brush.is_some(), pen.is_some()) {
        (true, true) => EMR_STROKEANDFILLPATH,
        (true, false) => EMR_FILLPATH,
        _ => EMR_STROKEPATH,
    };
    emf.record(kind, &bounds);
    if brush.is_some() {
        emf.record(EMR_SELECTOBJECT, &le(&[NULL_BRUSH]));
        emf.record(EMR_DELETEOBJECT, &le(&[1]));
    }
    if pen.is_some() {
        emf.record(EMR_SELECTOBJECT, &le(&[NULL_PEN]));
        emf.record(EMR_DELETEOBJECT, &le(&[2]));
    }
}

fn emf_image(emf: &mut Emf, img: &PlacedImage) {
    if img.width == 0 || img.height == 0 {
        return;
    }
    let (w, h) = (img.width as i32, img.height as i32);
    // A 32-bit bottom-up BGR DIB, each pixel flattened onto white.
    let mut bits = Vec::with_capacity(img.rgba.len());
    for row in img.rgba.chunks_exact(img.width as usize * 4).rev() {
        for px in row.as_chunks::<4>().0 {
            let a = u32::from(px[3]);
            let over = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
            bits.extend_from_slice(&[over(px[2]), over(px[1]), over(px[0]), 255]);
        }
    }
    let s = EMF_SUBPIXELS;
    let x = (img.x as i32).saturating_mul(s);
    let y = (img.y as i32).saturating_mul(s);
    let (cw, ch) = (w.saturating_mul(s), h.saturating_mul(s));
    let mut body = le(&[x, y, x + cw - 1, y + ch - 1]);
    body.extend_from_slice(&le(&[x, y, 0, 0, w, h]));
    // offBmi, cbBmi, offBits, cbBits (offsets from the record's start).
    body.extend_from_slice(&le(&[80, 40, 120, bits.len() as i32]));
    body.extend_from_slice(&le(&[0, 0x00CC_0020, cw, ch]));
    // BITMAPINFOHEADER.
    body.extend_from_slice(&le(&[40, w, h]));
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&32u16.to_le_bytes());
    body.extend_from_slice(&le(&[0, bits.len() as i32, 3780, 3780, 0, 0]));
    body.extend_from_slice(&bits);
    emf.record(EMR_STRETCHDIBITS, &body);
}

// ---------------------------------------------------------------------------
// DXF
// ---------------------------------------------------------------------------

/// Segments a DXF flattens each Bezier into.
pub const DXF_CURVE_STEPS: usize = 16;

/// The AutoCAD Color Index entries a DXF chooses from, with their sRGB.
const ACI: [(u8, [u8; 3]); 9] = [
    (1, [255, 0, 0]),
    (2, [255, 255, 0]),
    (3, [0, 255, 0]),
    (4, [0, 255, 255]),
    (5, [0, 0, 255]),
    (6, [255, 0, 255]),
    (7, [0, 0, 0]),
    (8, [128, 128, 128]),
    (9, [192, 192, 192]),
];

/// The nearest ACI index to `rgb` (7, "black/white", for black and white).
pub fn nearest_aci(rgb: [u8; 3]) -> u8 {
    let d = |c: [u8; 3]| {
        (0..3)
            .map(|k| (i32::from(c[k]) - i32::from(rgb[k])).pow(2))
            .sum::<i32>()
    };
    if d([255, 255, 255]) < d([192, 192, 192]) {
        return 7;
    }
    ACI.iter().min_by_key(|(_, c)| d(*c)).map_or(7, |(i, _)| *i)
}

fn cubic_at(p0: Pt, a: Pt, b: Pt, p: Pt, t: f64) -> Pt {
    let u = 1.0 - t;
    let k = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    [
        k[0] * p0[0] + k[1] * a[0] + k[2] * b[0] + k[3] * p[0],
        k[0] * p0[1] + k[1] * a[1] + k[2] * b[1] + k[3] * p[1],
    ]
}

/// A path's subpaths as polylines: `(points, closed)`.
pub fn polylines(segs: &[Seg]) -> Vec<(Vec<Pt>, bool)> {
    let mut out: Vec<(Vec<Pt>, bool)> = Vec::new();
    let mut current: Vec<Pt> = Vec::new();
    let mut start: Option<Pt> = None;
    let flush = |current: &mut Vec<Pt>, closed: bool, out: &mut Vec<(Vec<Pt>, bool)>| {
        if current.len() >= 2 {
            out.push((std::mem::take(current), closed));
        } else {
            current.clear();
        }
    };
    for seg in segs {
        match *seg {
            Seg::Move(p) => {
                flush(&mut current, false, &mut out);
                current.push(p);
                start = Some(p);
            }
            Seg::Line(p) => current.push(p),
            Seg::Cubic(a, b, p) => {
                let p0 = current.last().copied().or(start).unwrap_or(a);
                for i in 1..=DXF_CURVE_STEPS {
                    current.push(cubic_at(p0, a, b, p, i as f64 / DXF_CURVE_STEPS as f64));
                }
            }
            Seg::Close => {
                let restart = start;
                flush(&mut current, true, &mut out);
                if let Some(s) = restart {
                    current.push(s);
                }
            }
        }
    }
    flush(&mut current, false, &mut out);
    out
}

/// Write the first page of `doc`'s vector paths as an R12 DXF.
pub fn encode_dxf(doc: &VectorDoc) -> Result<Vec<u8>, CodecError> {
    let page = doc
        .pages
        .first()
        .ok_or_else(|| invalid("a DXF needs a page"))?;
    check_page(page)?;
    let h = f64::from(page.height);
    let mut out = String::new();
    let _ = write!(
        out,
        "0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1009\n9\n$INSUNITS\n70\n0\n\
         9\n$EXTMIN\n10\n0\n20\n0\n30\n0\n9\n$EXTMAX\n10\n{}\n20\n{}\n30\n0\n0\nENDSEC\n\
         0\nSECTION\n2\nENTITIES\n",
        page.width, page.height
    );
    for item in &page.items {
        let Item::Path(path) = item else { continue };
        let paint = path.fill.or(path.stroke.map(|s| s.paint));
        let Some(paint) = paint else { continue };
        let aci = nearest_aci(paint.rgb);
        for (points, closed) in polylines(&path.segs) {
            let _ = write!(
                out,
                "0\nPOLYLINE\n8\n0\n62\n{aci}\n66\n1\n70\n{}\n10\n0\n20\n0\n30\n0\n",
                u8::from(closed)
            );
            for p in points {
                let _ = write!(
                    out,
                    "0\nVERTEX\n8\n0\n10\n{}\n20\n{}\n30\n0\n",
                    num(p[0]),
                    num(h - p[1])
                );
            }
            out.push_str("0\nSEQEND\n8\n0\n");
        }
    }
    out.push_str("0\nENDSEC\n0\nEOF\n");
    Ok(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::formats::pdf;
    use crate::ImportLimits;

    fn square(x: f64, y: f64, s: f64, rgb: [u8; 3]) -> Item {
        Item::Path(VectorPath {
            segs: vec![
                Seg::Move([x, y]),
                Seg::Line([x + s, y]),
                Seg::Line([x + s, y + s]),
                Seg::Line([x, y + s]),
                Seg::Close,
            ],
            fill: Some(Paint { rgb, alpha: 1.0 }),
            even_odd: false,
            stroke: None,
        })
    }

    fn two_pages() -> VectorDoc {
        VectorDoc {
            title: "Board (1)".into(),
            pages: vec![
                Page {
                    width: 40,
                    height: 20,
                    items: vec![square(0.0, 0.0, 10.0, [255, 0, 0])],
                },
                Page {
                    width: 30,
                    height: 30,
                    items: vec![
                        square(10.0, 10.0, 10.0, [0, 0, 255]),
                        Item::Image(PlacedImage {
                            x: 0,
                            y: 20,
                            width: 2,
                            height: 2,
                            rgba: [0, 255, 0, 255].repeat(4),
                        }),
                    ],
                },
            ],
        }
    }

    fn rgba_at(surface: &crate::DecodedSurface, x: u32, y: u32) -> [u8; 4] {
        let rgba = surface.pixels.clone().into_rgba8();
        let i = ((y * surface.width + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn a_two_page_pdf_parses_back_with_two_pages_and_its_vectors_draw() {
        let bytes = encode_pdf(&two_pages()).unwrap();
        assert_eq!(pdf::page_count(&bytes).unwrap(), 2);
        let pages = pdf::render_pages(&bytes, ImportLimits::default()).unwrap();
        let first = &pages.pages[0];
        assert_eq!((first.width, first.height), (40, 20));
        // The red square sits at the top left (y is flipped into PDF space).
        let red = rgba_at(first, 5, 5);
        assert!(red[0] > 200 && red[1] < 60 && red[2] < 60, "{red:?}");
        let off = rgba_at(first, 30, 15);
        assert!(off[0] > 200 && off[1] > 200, "outside the square: {off:?}");
        let second = &pages.pages[1];
        let blue = rgba_at(second, 15, 15);
        assert!(blue[2] > 200 && blue[0] < 60, "{blue:?}");
        let green = rgba_at(second, 1, 21);
        assert!(
            green[1] > 200 && green[0] < 60,
            "the placed image: {green:?}"
        );
    }

    #[test]
    fn an_emf_draws_its_paths_and_image_through_the_metafile_reader() {
        let doc = VectorDoc {
            pages: vec![two_pages().pages.remove(1)],
            ..VectorDoc::default()
        };
        let bytes = encode_emf(&doc).unwrap();
        assert_eq!(&bytes[40..44], b" EMF");
        let size = u32::from_le_bytes(bytes[48..52].try_into().unwrap());
        assert_eq!(size as usize, bytes.len());
        let surface = crate::codec::decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        let blue = rgba_at(&surface, 15, 15);
        assert!(blue[2] > 200 && blue[0] < 60, "{blue:?}");
        let green = rgba_at(&surface, 1, 21);
        assert!(green[1] > 200 && green[0] < 60, "{green:?}");
    }

    #[test]
    fn a_dxf_writes_one_closed_polyline_per_subpath_with_y_up() {
        let bytes = encode_dxf(&two_pages()).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("0\nSECTION\n2\nHEADER\n"));
        assert!(text.ends_with("0\nEOF\n"));
        assert_eq!(text.matches("\nPOLYLINE\n").count(), 1);
        assert_eq!(text.matches("\nVERTEX\n").count(), 4);
        assert!(text.contains("62\n1\n"), "red is ACI 1");
        // (0, 0) at the top left is (0, 20) with y up.
        assert!(text.contains("10\n0\n20\n20\n30\n0\n"), "{text}");
    }

    #[test]
    fn a_cubic_flattens_into_sixteen_steps() {
        let lines = polylines(&[
            Seg::Move([0.0, 0.0]),
            Seg::Cubic([0.0, 10.0], [10.0, 10.0], [10.0, 0.0]),
        ]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0.len(), 1 + DXF_CURVE_STEPS);
        assert_eq!(lines[0].0.last(), Some(&[10.0, 0.0]));
    }

    #[test]
    fn a_page_with_a_short_image_is_refused() {
        let mut doc = two_pages();
        if let Item::Image(img) = &mut doc.pages[1].items[1] {
            img.rgba.pop();
        }
        assert!(encode_pdf(&doc).is_err());
        assert!(encode_pdf(&VectorDoc::default()).is_err());
    }
}
