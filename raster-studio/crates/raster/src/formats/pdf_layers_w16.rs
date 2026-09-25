//! W16-I: a PDF (or PDF-compatible `.ai`) page read as **layers**: its
//! filled and stroked paths as shape layers and its text as text layers, as
//! Photopea opens a PDF.
//!
//! A child module of [`super`] (the PDF reader), declared there with
//! `#[path]`.
//!
//! # How
//!
//! `hayro-interpret` 0.4 has a `Device` trait (paths, glyphs, images, clips
//! and transparency groups as callbacks), but this crate does not depend on
//! `hayro-interpret` directly (only on `hayro`, which does not re-export
//! it, and `hayro-syntax`), and this wave does not change the crate's
//! dependencies. So the page's content stream is read here with
//! `hayro-syntax`'s operator iterator, and a small interpreter keeps the
//! graphics state (`q` / `Q` / `cm`, line width, cap, join, miter limit,
//! dash, `gs` opacity and line width, DeviceGray / RGB / CMYK colour) and
//! the text state (`BT` / `ET`, `Tf`, `Td`, `TD`, `Tm`, `T*`, `Tc`, `Tw`,
//! `Tz`, `TL`, `Ts`, `Tr`), and writes each fill, stroke and text show as
//! one element of an SVG display list in page pixels (one pixel per point,
//! the page's own orientation). [`super::super::super::svg_import::layers`]
//! maps that list onto layers, as it does for an SVG.
//!
//! # What does not map (the page opens flattened)
//!
//! A page that uses anything this interpreter cannot carry as a live layer
//! is refused here with the reason, and File > Open opens that page as one
//! picture (hayro's render) and says why: images and form XObjects (`Do`),
//! inline images, smooth shading (`sh`), clipping paths other than a
//! rectangle that covers the whole page, colour spaces other than
//! DeviceGray / RGB / CMYK, composite (Type0) and Type3 fonts, text render
//! modes other than fill and invisible, soft masks and blend modes other
//! than Normal in `gs`. CMYK is converted naively (`(1 - c)(1 - k)`), as
//! the flat route's renderer does without an output profile. Text is
//! decoded as Latin-1 (the simple fonts' standard and WinAnsi encodings
//! agree with it on printable ASCII); `TJ` kerning adjustments smaller than
//! a word space are dropped and larger ones become a space. The page opens
//! on transparency: PDF paper is not a layer.

use std::fmt::Write as _;

use hayro_syntax::content::UntypedIter;
use hayro_syntax::object::{Dict, Name, Object};

use super::{guarded, load, malformed, pixel_size, NAME};
use crate::codec::svg_import::layers::{read_layers, VectorLayers};
use crate::codec::{CodecError, ImportFormat, ImportLimits};

/// Largest display list one page may write.
pub const MAX_LIST_BYTES: usize = 32 << 20;

type M = [f64; 6];
const ID: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `outer` after `inner` (a point goes through `inner` first).
fn then(outer: &M, inner: &M) -> M {
    let [a1, b1, c1, d1, e1, f1] = *outer;
    let [a2, b2, c2, d2, e2, f2] = *inner;
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

fn apply(m: &M, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn hex(c: [f64; 3]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Space {
    Gray,
    Rgb,
    Cmyk,
    Other,
}

impl Space {
    fn color(self, v: &[f64]) -> Option<[f64; 3]> {
        match (self, v) {
            (Space::Gray, [g]) => Some([*g; 3]),
            (Space::Rgb, [r, g, b]) => Some([*r, *g, *b]),
            (Space::Cmyk, [c, m, y, k]) => Some([
                (1.0 - c) * (1.0 - k),
                (1.0 - m) * (1.0 - k),
                (1.0 - y) * (1.0 - k),
            ]),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Font {
    family: String,
    bold: bool,
    italic: bool,
    first_char: i64,
    widths: Vec<f64>,
}

impl Font {
    /// Advance of byte `b` in text space units per 1 of font size.
    fn advance(&self, b: u8) -> f64 {
        let i = i64::from(b) - self.first_char;
        usize::try_from(i)
            .ok()
            .and_then(|i| self.widths.get(i))
            .copied()
            .unwrap_or(500.0)
            / 1000.0
    }
}

#[derive(Clone)]
struct Gs {
    ctm: M,
    fill_space: Space,
    stroke_space: Space,
    fill: Option<[f64; 3]>,
    stroke: Option<[f64; 3]>,
    fill_alpha: f64,
    stroke_alpha: f64,
    lw: f64,
    cap: i64,
    join: i64,
    miter: f64,
    dash: Vec<f64>,
    dash_off: f64,
    font: Option<Font>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
    render: i64,
}

enum Seg {
    M(f64, f64),
    L(f64, f64),
    C(f64, f64, f64, f64, f64, f64),
    Z,
}

struct Interp<'p, 'a> {
    page: &'p hayro_syntax::page::Page<'a>,
    base: M,
    size: (f64, f64),
    gs: Gs,
    stack: Vec<Gs>,
    path: Vec<Seg>,
    cur: (f64, f64),
    start: (f64, f64),
    clip_pending: bool,
    tm: M,
    tlm: M,
    svg: String,
}

type Res<T = ()> = Result<T, String>;

impl Interp<'_, '_> {
    fn device(&self) -> M {
        then(&self.base, &self.gs.ctm)
    }

    fn emit(&mut self, el: &str) -> Res {
        if self.svg.len() + el.len() > MAX_LIST_BYTES {
            return Err(format!("more than {MAX_LIST_BYTES} bytes of drawing"));
        }
        self.svg.push_str(el);
        self.svg.push('\n');
        Ok(())
    }

    fn d(&self, m: Option<&M>) -> String {
        let t = |x: f64, y: f64| match m {
            Some(m) => apply(m, x, y),
            None => (x, y),
        };
        let mut d = String::new();
        for s in &self.path {
            match *s {
                Seg::M(x, y) => {
                    let (x, y) = t(x, y);
                    let _ = write!(d, "M{x} {y}");
                }
                Seg::L(x, y) => {
                    let (x, y) = t(x, y);
                    let _ = write!(d, "L{x} {y}");
                }
                Seg::C(a, b, c, e, x, y) => {
                    let ((a, b), (c, e), (x, y)) = (t(a, b), t(c, e), t(x, y));
                    let _ = write!(d, "C{a} {b} {c} {e} {x} {y}");
                }
                Seg::Z => d.push('Z'),
            }
        }
        d
    }

    /// The pending clip is harmless when the path is one rectangle that
    /// covers the whole page.
    fn clip_covers_page(&self) -> bool {
        let dev = self.device();
        let mut pts = Vec::new();
        for s in &self.path {
            match *s {
                Seg::M(x, y) | Seg::L(x, y) => pts.push(apply(&dev, x, y)),
                Seg::C(..) => return false,
                Seg::Z => {}
            }
        }
        if pts.len() < 4 || pts.len() > 5 {
            return false;
        }
        let axis = pts
            .windows(2)
            .all(|w| (w[0].0 - w[1].0).abs() < 1e-3 || (w[0].1 - w[1].1).abs() < 1e-3);
        let (x0, x1) = pts
            .iter()
            .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
        let (y0, y1) = pts
            .iter()
            .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
        axis && x0 <= 0.5 && y0 <= 0.5 && x1 >= self.size.0 - 0.5 && y1 >= self.size.1 - 0.5
    }

    fn end_path(&mut self) -> Res {
        if self.clip_pending {
            self.clip_pending = false;
            if !self.clip_covers_page() {
                return Err("a clipping path".into());
            }
        }
        self.path.clear();
        Ok(())
    }

    fn paint(&mut self, fill: bool, stroke: bool, even_odd: bool) -> Res {
        if self.path.is_empty() {
            return self.end_path();
        }
        if fill {
            let color = self
                .gs
                .fill
                .ok_or("a colour space other than DeviceGray / RGB / CMYK")?;
            let d = self.d(Some(&self.device()));
            let el = format!(
                "<path d=\"{d}\" fill=\"{}\" fill-opacity=\"{}\"{}/>",
                hex(color),
                self.gs.fill_alpha,
                if even_odd {
                    " fill-rule=\"evenodd\""
                } else {
                    ""
                }
            );
            self.emit(&el)?;
        }
        if stroke {
            let color = self
                .gs
                .stroke
                .ok_or("a colour space other than DeviceGray / RGB / CMYK")?;
            let m = self.device();
            let d = self.d(None);
            let mut width = self.gs.lw.abs();
            if width == 0.0 {
                let det = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
                width = if det > 0.0 { 1.0 / det } else { 1.0 };
            }
            let cap = ["butt", "round", "square"][self.gs.cap.clamp(0, 2) as usize];
            let join = ["miter", "round", "bevel"][self.gs.join.clamp(0, 2) as usize];
            let mut el = format!(
                "<path d=\"{d}\" transform=\"matrix({} {} {} {} {} {})\" fill=\"none\" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"{width}\" stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\" stroke-miterlimit=\"{}\"",
                m[0],
                m[1],
                m[2],
                m[3],
                m[4],
                m[5],
                hex(color),
                self.gs.stroke_alpha,
                self.gs.miter.max(1.0)
            );
            if !self.gs.dash.is_empty()
                && self.gs.dash.iter().all(|v| v.is_finite() && *v >= 0.0)
                && self.gs.dash.iter().sum::<f64>() > 0.0
            {
                let list: Vec<String> = self.gs.dash.iter().map(|v| v.to_string()).collect();
                let _ = write!(
                    el,
                    " stroke-dasharray=\"{}\" stroke-dashoffset=\"{}\"",
                    list.join(" "),
                    self.gs.dash_off
                );
            }
            el.push_str("/>");
            self.emit(&el)?;
        }
        self.end_path()
    }

    fn font(&self, name: Name) -> Res<Font> {
        let dict: Dict = self
            .page
            .resources()
            .get_font(name, Box::new(|_| None), Box::new(Some))
            .ok_or("a font the page does not define")?;
        let subtype = dict
            .get::<Name>(b"Subtype".as_slice())
            .map(|n| n.as_str().to_string())
            .unwrap_or_default();
        match subtype.as_str() {
            "Type0" => return Err("a composite (Type0) font".into()),
            "Type3" => return Err("a Type3 font".into()),
            _ => {}
        }
        let base = dict
            .get::<Name>(b"BaseFont".as_slice())
            .map(|n| n.as_str().to_string())
            .unwrap_or_default();
        // A subset's `ABCDEF+` tag is not part of the name.
        let base = match base.split_once('+') {
            Some((tag, rest)) if tag.len() == 6 => rest.to_string(),
            _ => base,
        };
        let family_part = base.split([',', '-']).next().unwrap_or("").to_string();
        let lower = base.to_ascii_lowercase();
        let family = match family_part.as_str() {
            "Helvetica" | "ArialMT" | "Arial" => "Arial".to_string(),
            "Times" | "TimesNewRomanPSMT" | "TimesNewRoman" => "Times New Roman".to_string(),
            "Courier" | "CourierNewPSMT" | "CourierNew" => "Courier New".to_string(),
            "" => "Arial".to_string(),
            other => other.to_string(),
        };
        let first_char = dict
            .get::<Object>(b"FirstChar".as_slice())
            .and_then(|o| o.into_f32())
            .map_or(0, |v| v as i64);
        let widths = dict
            .get::<hayro_syntax::object::Array>(b"Widths".as_slice())
            .map(|a| {
                a.iter::<Object>()
                    .map(|o| o.into_f32().map_or(500.0, f64::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Font {
            family,
            bold: lower.contains("bold") || lower.contains("black") || lower.contains("heavy"),
            italic: lower.contains("italic") || lower.contains("oblique"),
            first_char,
            widths,
        })
    }

    fn ext_g_state(&mut self, name: Name) -> Res {
        let Some(dict): Option<Dict> =
            self.page
                .resources()
                .get_ext_g_state(name, Box::new(|_| None), Box::new(Some))
        else {
            return Ok(());
        };
        if let Some(bm) = dict.get::<Name>(b"BM".as_slice()) {
            if !matches!(bm.as_str(), "Normal" | "Compatible") {
                return Err("a blend mode".into());
            }
        }
        if let Some(mask) = dict.get::<Object>(b"SMask".as_slice()) {
            if !matches!(&mask, Object::Name(n) if n.as_str() == "None") {
                return Err("a soft mask".into());
            }
        }
        let num = |key: &[u8]| {
            dict.get::<Object>(key)
                .and_then(|o| o.into_f32())
                .map(f64::from)
        };
        if let Some(v) = num(b"ca") {
            self.gs.fill_alpha = v.clamp(0.0, 1.0);
        }
        if let Some(v) = num(b"CA") {
            self.gs.stroke_alpha = v.clamp(0.0, 1.0);
        }
        if let Some(v) = num(b"LW") {
            self.gs.lw = v;
        }
        Ok(())
    }

    /// Show `bytes` at the text matrix and advance it.
    fn show(&mut self, bytes: &[u8]) -> Res {
        let font = self.gs.font.clone().ok_or("text with no font set")?;
        let text: String = bytes.iter().map(|&b| char::from(b)).collect();
        let size = self.gs.size;
        let th = self.gs.th / 100.0;
        if self.gs.render != 3 && !text.trim().is_empty() {
            if self.gs.render != 0 {
                return Err("a text render mode other than fill".into());
            }
            let color = self
                .gs
                .fill
                .ok_or("a colour space other than DeviceGray / RGB / CMYK")?;
            // Text space to page pixels, y flipped so the glyphs stand up
            // in the SVG's y-down space.
            let trm = then(
                &then(&self.device(), &self.tm),
                &[size * th, 0.0, 0.0, -size, 0.0, self.gs.rise],
            );
            let el = format!(
                "<text transform=\"matrix({} {} {} {} {} {})\" font-family=\"{}\" font-size=\"1\"{}{} fill=\"{}\" fill-opacity=\"{}\" xml:space=\"preserve\">{}</text>",
                trm[0],
                trm[1],
                trm[2],
                trm[3],
                trm[4],
                trm[5],
                xml_escape(&font.family),
                if font.bold { " font-weight=\"bold\"" } else { "" },
                if font.italic { " font-style=\"italic\"" } else { "" },
                hex(color),
                self.gs.fill_alpha,
                xml_escape(&text)
            );
            self.emit(&el)?;
        }
        let mut tx = 0.0;
        for &b in bytes {
            tx += (font.advance(b) * size + self.gs.tc + if b == b' ' { self.gs.tw } else { 0.0 })
                * th;
        }
        self.tm = then(&self.tm, &[1.0, 0.0, 0.0, 1.0, tx, 0.0]);
        Ok(())
    }

    fn next_line(&mut self, tx: f64, ty: f64) {
        self.tlm = then(&self.tlm, &[1.0, 0.0, 0.0, 1.0, tx, ty]);
        self.tm = self.tlm;
    }

    fn run(&mut self, content: &[u8]) -> Res {
        for instr in UntypedIter::new(content) {
            let op: Vec<u8> = instr.operator.to_vec();
            let args: Vec<Object> = instr.operands().collect();
            let n = |i: usize| -> f64 {
                args.get(i)
                    .and_then(|o| o.clone().into_f32())
                    .map_or(0.0, f64::from)
            };
            let nums = || -> Vec<f64> {
                args.iter()
                    .filter_map(|o| o.clone().into_f32().map(f64::from))
                    .collect()
            };
            let name = |i: usize| args.get(i).and_then(|o| o.clone().into_name());
            match op.as_slice() {
                b"q" => {
                    if self.stack.len() > 1_000 {
                        return Err("graphics states nested past 1000".into());
                    }
                    self.stack.push(self.gs.clone());
                }
                b"Q" => {
                    if let Some(g) = self.stack.pop() {
                        self.gs = g;
                    }
                }
                b"cm" => {
                    let m = [n(0), n(1), n(2), n(3), n(4), n(5)];
                    self.gs.ctm = then(&self.gs.ctm, &m);
                }
                b"w" => self.gs.lw = n(0),
                b"J" => self.gs.cap = n(0) as i64,
                b"j" => self.gs.join = n(0) as i64,
                b"M" => self.gs.miter = n(0),
                b"d" => {
                    self.gs.dash = match args.first() {
                        Some(Object::Array(a)) => a
                            .iter::<Object>()
                            .filter_map(|o| o.into_f32().map(f64::from))
                            .collect(),
                        _ => Vec::new(),
                    };
                    self.gs.dash_off = n(1);
                }
                b"gs" => {
                    if let Some(nm) = name(0) {
                        self.ext_g_state(nm)?;
                    }
                }
                b"m" => {
                    self.cur = (n(0), n(1));
                    self.start = self.cur;
                    self.path.push(Seg::M(n(0), n(1)));
                }
                b"l" => {
                    self.cur = (n(0), n(1));
                    self.path.push(Seg::L(n(0), n(1)));
                }
                b"c" => {
                    self.cur = (n(4), n(5));
                    self.path.push(Seg::C(n(0), n(1), n(2), n(3), n(4), n(5)));
                }
                b"v" => {
                    let (x, y) = self.cur;
                    self.cur = (n(2), n(3));
                    self.path.push(Seg::C(x, y, n(0), n(1), n(2), n(3)));
                }
                b"y" => {
                    self.cur = (n(2), n(3));
                    self.path.push(Seg::C(n(0), n(1), n(2), n(3), n(2), n(3)));
                }
                b"h" => {
                    self.path.push(Seg::Z);
                    self.cur = self.start;
                }
                b"re" => {
                    let (x, y, w, h) = (n(0), n(1), n(2), n(3));
                    self.path.push(Seg::M(x, y));
                    self.path.push(Seg::L(x + w, y));
                    self.path.push(Seg::L(x + w, y + h));
                    self.path.push(Seg::L(x, y + h));
                    self.path.push(Seg::Z);
                    self.cur = (x, y);
                    self.start = (x, y);
                }
                b"S" => self.paint(false, true, false)?,
                b"s" => {
                    self.path.push(Seg::Z);
                    self.paint(false, true, false)?;
                }
                b"f" | b"F" => self.paint(true, false, false)?,
                b"f*" => self.paint(true, false, true)?,
                b"B" => self.paint(true, true, false)?,
                b"B*" => self.paint(true, true, true)?,
                b"b" => {
                    self.path.push(Seg::Z);
                    self.paint(true, true, false)?;
                }
                b"b*" => {
                    self.path.push(Seg::Z);
                    self.paint(true, true, true)?;
                }
                b"n" => self.end_path()?,
                b"W" | b"W*" => self.clip_pending = true,
                b"g" => {
                    self.gs.fill_space = Space::Gray;
                    self.gs.fill = Space::Gray.color(&[n(0)]);
                }
                b"G" => {
                    self.gs.stroke_space = Space::Gray;
                    self.gs.stroke = Space::Gray.color(&[n(0)]);
                }
                b"rg" => {
                    self.gs.fill_space = Space::Rgb;
                    self.gs.fill = Space::Rgb.color(&[n(0), n(1), n(2)]);
                }
                b"RG" => {
                    self.gs.stroke_space = Space::Rgb;
                    self.gs.stroke = Space::Rgb.color(&[n(0), n(1), n(2)]);
                }
                b"k" => {
                    self.gs.fill_space = Space::Cmyk;
                    self.gs.fill = Space::Cmyk.color(&[n(0), n(1), n(2), n(3)]);
                }
                b"K" => {
                    self.gs.stroke_space = Space::Cmyk;
                    self.gs.stroke = Space::Cmyk.color(&[n(0), n(1), n(2), n(3)]);
                }
                b"cs" | b"CS" => {
                    let space = match name(0).as_ref().map(|n| n.as_str()) {
                        Some("DeviceGray") => Space::Gray,
                        Some("DeviceRGB") => Space::Rgb,
                        Some("DeviceCMYK") => Space::Cmyk,
                        _ => Space::Other,
                    };
                    let black = match space {
                        Space::Gray => Some([0.0; 3]),
                        Space::Rgb => Some([0.0; 3]),
                        Space::Cmyk => Some([0.0; 3]),
                        Space::Other => None,
                    };
                    if op == b"cs" {
                        self.gs.fill_space = space;
                        self.gs.fill = black;
                    } else {
                        self.gs.stroke_space = space;
                        self.gs.stroke = black;
                    }
                }
                b"sc" | b"scn" => self.gs.fill = self.gs.fill_space.color(&nums()),
                b"SC" | b"SCN" => self.gs.stroke = self.gs.stroke_space.color(&nums()),
                b"BT" => {
                    self.tm = ID;
                    self.tlm = ID;
                }
                b"Tf" => {
                    if let Some(nm) = name(0) {
                        self.gs.font = Some(self.font(nm)?);
                    }
                    self.gs.size = n(1);
                }
                b"Tc" => self.gs.tc = n(0),
                b"Tw" => self.gs.tw = n(0),
                b"Tz" => self.gs.th = n(0),
                b"TL" => self.gs.tl = n(0),
                b"Ts" => self.gs.rise = n(0),
                b"Tr" => self.gs.render = n(0) as i64,
                b"Td" => self.next_line(n(0), n(1)),
                b"TD" => {
                    self.gs.tl = -n(1);
                    self.next_line(n(0), n(1));
                }
                b"Tm" => {
                    self.tlm = [n(0), n(1), n(2), n(3), n(4), n(5)];
                    self.tm = self.tlm;
                }
                b"T*" => self.next_line(0.0, -self.gs.tl),
                b"Tj" | b"'" | b"\"" => {
                    if op == b"\"" {
                        self.gs.tw = n(0);
                        self.gs.tc = n(1);
                    }
                    if op != b"Tj" {
                        self.next_line(0.0, -self.gs.tl);
                    }
                    if let Some(Object::String(s)) = args.last() {
                        let bytes = s.get().to_vec();
                        self.show(&bytes)?;
                    }
                }
                b"TJ" => {
                    let Some(Object::Array(a)) = args.first() else {
                        continue;
                    };
                    // One element for the whole array: kerning smaller
                    // than a word space is dropped, a larger gap is a space.
                    let mut bytes = Vec::new();
                    let mut shift = 0.0;
                    for o in a.iter::<Object>() {
                        match o {
                            Object::String(s) => bytes.extend_from_slice(&s.get()),
                            Object::Number(v) => {
                                let v = v.as_f64();
                                if v <= -250.0 && bytes.last() != Some(&b' ') {
                                    bytes.push(b' ');
                                    shift += -v / 1000.0 - 0.25;
                                } else {
                                    shift += -v / 1000.0;
                                }
                            }
                            _ => {}
                        }
                    }
                    self.show(&bytes)?;
                    let th = self.gs.th / 100.0;
                    let dx = shift * self.gs.size * th;
                    self.tm = then(&self.tm, &[1.0, 0.0, 0.0, 1.0, dx, 0.0]);
                }
                b"Do" => return Err("an image or form XObject".into()),
                b"BI" | b"ID" | b"EI" => return Err("an inline image".into()),
                b"sh" => return Err("smooth shading".into()),
                _ => {}
            }
        }
        Ok(())
    }
}

/// Page `index` (from 0) of the PDF in `bytes` as layers; an error naming
/// what cannot be mapped when the page uses it (see the module docs).
pub fn page_layers(
    bytes: &[u8],
    index: usize,
    limits: ImportLimits,
) -> Result<VectorLayers, CodecError> {
    let svg = guarded(|| {
        let pdf = load(bytes)?;
        let (w, h) = pixel_size(&pdf, index, super::PIXELS_PER_POINT, limits)?;
        let pages = pdf.pages();
        let page = &pages[index];
        let content = page.page_stream().unwrap_or(&[]);
        let mut it = Interp {
            page,
            base: page.initial_transform(true).as_coeffs(),
            size: (f64::from(w), f64::from(h)),
            gs: Gs {
                ctm: ID,
                fill_space: Space::Gray,
                stroke_space: Space::Gray,
                fill: Some([0.0; 3]),
                stroke: Some([0.0; 3]),
                fill_alpha: 1.0,
                stroke_alpha: 1.0,
                lw: 1.0,
                cap: 0,
                join: 0,
                miter: 10.0,
                dash: Vec::new(),
                dash_off: 0.0,
                font: None,
                size: 0.0,
                tc: 0.0,
                tw: 0.0,
                th: 100.0,
                tl: 0.0,
                rise: 0.0,
                render: 0,
            },
            stack: Vec::new(),
            path: Vec::new(),
            cur: (0.0, 0.0),
            start: (0.0, 0.0),
            clip_pending: false,
            tm: ID,
            tlm: ID,
            svg: String::new(),
        };
        it.run(content).map_err(|why| {
            CodecError::Unsupported(format!(
                "page {} uses {why}, which does not open as layers",
                index + 1
            ))
        })?;
        if it.svg.is_empty() {
            return Err(malformed(NAME, format!("page {} draws nothing", index + 1)));
        }
        Ok(format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\n{}</svg>\n",
            it.svg
        ))
    })?;
    read_layers(
        svg.as_bytes(),
        limits,
        ImportFormat::Pdf,
        &format!("page {}", index + 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::formats::vector_docs::design_files::DesignKind;

    /// A one-page PDF whose resources name Helvetica as `/F1`.
    pub(crate) fn pdf_with_font(w: u32, h: u32, content: &str) -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            ),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len() + 1
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn a_page_of_paths_and_text_opens_as_shape_and_text_layers() {
        let pdf = pdf_with_font(
            200,
            100,
            "1 0 0 rg 10 10 30 20 re f\n0 0 1 RG 3 w 50 50 m 150 50 l S\nBT /F1 12 Tf 20 80 Td 0 0.5 0 rg (Hi there) Tj ET",
        );
        let v = page_layers(&pdf, 0, ImportLimits::default()).unwrap();
        assert_eq!((v.width, v.height), (200, 100));
        assert_eq!(v.design.format, ImportFormat::Pdf);
        let n = &v.design.nodes;
        assert_eq!(n.len(), 3, "{n:?}");
        let DesignKind::Shape { fill, .. } = &n[0].kind else {
            panic!("the rect: {:?}", n[0].kind)
        };
        assert_eq!(*fill, Some([1.0, 0.0, 0.0, 1.0]));
        // PDF y runs up: the rect's top-left is at y = 100 - 30 = 70.
        let (x, y) = n[0].corners()[0];
        assert!(
            (x - 10.0).abs() < 1e-3 && (y - 70.0).abs() < 1e-3,
            "{x},{y}"
        );
        let DesignKind::Shape { stroke, fill, .. } = &n[1].kind else {
            panic!("the line")
        };
        assert!(fill.is_none());
        let s = stroke.as_ref().unwrap();
        assert_eq!((s.color, s.width), ([0.0, 0.0, 1.0, 1.0], 3.0));
        let DesignKind::Text {
            text,
            font_family,
            bold,
            size,
            ..
        } = &n[2].kind
        else {
            panic!("the text: {:?}", n[2].kind)
        };
        assert_eq!(
            (text.as_str(), font_family.as_str(), *bold),
            ("Hi there", "Arial", true)
        );
        assert!((size - 12.0).abs() < 1e-3, "{size}");
    }

    #[test]
    fn an_image_on_the_page_is_refused_with_the_reason() {
        let pdf = pdf_with_font(20, 20, "q 20 0 0 20 0 0 cm /Im1 Do Q");
        let e = page_layers(&pdf, 0, ImportLimits::default()).unwrap_err();
        assert!(e.to_string().contains("an image or form XObject"), "{e}");
        // A clip that covers the page is harmless; a smaller one is not.
        let ok = pdf_with_font(20, 20, "0 0 20 20 re W n 1 0 0 rg 0 0 5 5 re f");
        assert!(page_layers(&ok, 0, ImportLimits::default()).is_ok());
        let clipped = pdf_with_font(20, 20, "0 0 10 10 re W n 1 0 0 rg 0 0 5 5 re f");
        let e = page_layers(&clipped, 0, ImportLimits::default()).unwrap_err();
        assert!(e.to_string().contains("clipping path"), "{e}");
    }
}
