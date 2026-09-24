//! `EngineData` — the text engine's own description of a type layer.
//!
//! A `TySh` block's text descriptor carries the string under `Txt ` and,
//! under `EngineData`, a blob in a private PostScript-like syntax that holds
//! everything about how the string is *set*: the font list, the character
//! style runs (font, size, fill, tracking, leading, faux bold/italic, caps) and
//! the paragraph runs (justification, indents, spacing).
//!
//! ```text
//! <<
//!     /EngineDict << /Editor << /Text (þÿ…UTF-16BE…) >>
//!                    /ParagraphRun << /RunArray [ … ] /RunLengthArray [ 12 ] >>
//!                    /StyleRun << /RunArray [ << /StyleSheet << /StyleSheetData
//!                                   << /Font 0 /FontSize 24 /FillColor
//!                                      << /Type 1 /Values [ 1 1 0 0 ] >> >> >> >> ]
//!                                 /RunLengthArray [ 12 ] >> … >>
//!     /ResourceDict << /FontSet [ << /Name (þÿ…) >> … ] … >>
//! >>
//! ```
//!
//! This module has three parts:
//!
//! * [`parse`] — a bounded tokenizer + recursive-descent parser into
//!   [`EdValue`]. The blob is **untrusted input**: its size, nesting depth and
//!   node count are all capped ([`MAX_ENGINE_DATA_BYTES`], [`MAX_DEPTH`],
//!   [`MAX_NODES`]) and every failure is an [`EngineDataError`], never a panic.
//! * [`extract`] / [`EngineText`] — the text, style runs and paragraph runs
//!   read out of the parsed tree, with the normal style/paragraph sheets merged
//!   underneath every run the way the text engine applies them. Run lengths are
//!   in UTF-16 code units, as in the file.
//! * [`write`] — the inverse: a Photoshop-shaped blob for an [`EngineText`],
//!   which [`crate::text::build_styled`] embeds in a `TySh` block. [`parse`]
//!   and [`extract`] read everything [`write`] produces back unchanged (the
//!   round trip the tests pin).
//!
//! [`to_text_layer`] and [`from_text_layer`] map an [`EngineText`] onto the
//! persisted `layer_model` text schema (base style + sparse spans + paragraph
//! + frame) and back, so an importer and an exporter share one mapping.

use layer_model::text::{
    Alignment, BaseStyle, Caps, Frame, Leading, Paragraph, Script, Slant, StyleOverride, StyleSpan,
    Weight,
};
use layer_model::TextLayer;

/// Largest engine-data blob this parser will look at. Real ones are a few
/// kilobytes to a few hundred; sixteen mebibytes is far past any real layer.
pub const MAX_ENGINE_DATA_BYTES: usize = 16 << 20;
/// Deepest `<< >>` / `[ ]` nesting accepted. Photoshop's own is about ten.
pub const MAX_DEPTH: usize = 64;
/// Most values (of any kind) one blob may contain.
pub const MAX_NODES: usize = 1 << 20;
/// Most style or paragraph runs read from one layer.
pub const MAX_RUNS: usize = 1 << 16;

/// The family name this crate's writer uses for the editor's generic sans
/// (an empty `font_family`). [`to_text_layer`] maps it back to empty.
pub const GENERIC_SANS_NAME: &str = "RasterStudioSans";

/// Why an engine-data blob could not be read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineDataError {
    #[error("the text engine data is {0} bytes, more than the {MAX_ENGINE_DATA_BYTES} accepted")]
    TooLarge(usize),
    #[error("the text engine data nests deeper than {MAX_DEPTH} levels")]
    TooDeep,
    #[error("the text engine data holds more than {MAX_NODES} values")]
    TooManyNodes,
    #[error("the text engine data has more than {MAX_RUNS} runs")]
    TooManyRuns,
    #[error("the text engine data ends in the middle of a value")]
    Truncated,
    #[error("the text engine data is malformed at byte {at}: {what}")]
    Syntax { at: usize, what: &'static str },
    #[error("the text engine data is inconsistent: {0}")]
    Invalid(&'static str),
}

/// One parsed engine-data value.
#[derive(Debug, Clone, PartialEq)]
pub enum EdValue {
    /// `<< /Key value … >>`, in file order.
    Dict(Vec<(String, EdValue)>),
    /// `[ value … ]`.
    Array(Vec<EdValue>),
    /// `/Name` in value position.
    Name(String),
    /// `( … )`, escapes resolved, bytes as written (see [`decode_string`]).
    Str(Vec<u8>),
    Num(f64),
    Bool(bool),
    /// Any other bare word (`null`, …).
    Word(String),
}

impl EdValue {
    /// The value under `key` when this is a dictionary.
    pub fn get(&self, key: &str) -> Option<&EdValue> {
        match self {
            EdValue::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Walk a key path through nested dictionaries.
    pub fn path(&self, keys: &[&str]) -> Option<&EdValue> {
        keys.iter().try_fold(self, |v, k| v.get(k))
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            EdValue::Num(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            EdValue::Bool(b) => Some(*b),
            EdValue::Num(n) => Some(*n != 0.0),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[EdValue]> {
        match self {
            EdValue::Array(items) => Some(items),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ parsing

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0 | 0x0c)
}

fn is_delim(b: u8) -> bool {
    is_space(b) || matches!(b, b'/' | b'[' | b']' | b'<' | b'>' | b'(' | b')')
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    nodes: usize,
}

impl<'a> Parser<'a> {
    fn skip_space(&mut self) {
        while let Some(&b) = self.src.get(self.pos) {
            if is_space(b) {
                self.pos += 1;
            } else if b == b'%' {
                // A PostScript comment runs to the end of the line.
                while let Some(&c) = self.src.get(self.pos) {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn starts_with(&self, s: &[u8]) -> bool {
        self.src
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(s))
    }

    fn syntax(&self, what: &'static str) -> EngineDataError {
        EngineDataError::Syntax { at: self.pos, what }
    }

    fn count_node(&mut self) -> Result<(), EngineDataError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(EngineDataError::TooManyNodes);
        }
        Ok(())
    }

    fn word(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(&b) = self.src.get(self.pos) {
            if is_delim(b) {
                break;
            }
            self.pos += 1;
        }
        &self.src[start..self.pos]
    }

    fn name(&mut self) -> Result<String, EngineDataError> {
        // Caller has checked the '/'.
        self.pos += 1;
        let w = self.word();
        Ok(String::from_utf8_lossy(w).into_owned())
    }

    fn string(&mut self) -> Result<Vec<u8>, EngineDataError> {
        // Caller has checked the '('.
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let b = self.peek().ok_or(EngineDataError::Truncated)?;
            self.pos += 1;
            match b {
                b')' => return Ok(out),
                b'\\' => {
                    let e = self.peek().ok_or(EngineDataError::Truncated)?;
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        // A backslash before a line break continues the line.
                        b'\n' => {}
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<EdValue, EngineDataError> {
        if depth > MAX_DEPTH {
            return Err(EngineDataError::TooDeep);
        }
        self.skip_space();
        self.count_node()?;
        let b = self.peek().ok_or(EngineDataError::Truncated)?;
        match b {
            b'<' if self.starts_with(b"<<") => {
                self.pos += 2;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.peek() {
                        None => return Err(EngineDataError::Truncated),
                        Some(b'>') if self.starts_with(b">>") => {
                            self.pos += 2;
                            return Ok(EdValue::Dict(items));
                        }
                        Some(b'/') => {
                            let key = self.name()?;
                            self.skip_space();
                            if self.starts_with(b">>") {
                                return Err(self.syntax("a key without a value"));
                            }
                            let v = self.value(depth + 1)?;
                            items.push((key, v));
                        }
                        Some(_) => return Err(self.syntax("expected a /Key in a dictionary")),
                    }
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.peek() {
                        None => return Err(EngineDataError::Truncated),
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(EdValue::Array(items));
                        }
                        Some(_) => items.push(self.value(depth + 1)?),
                    }
                }
            }
            b'/' => Ok(EdValue::Name(self.name()?)),
            b'(' => Ok(EdValue::Str(self.string()?)),
            b'<' | b'>' | b']' | b')' => Err(self.syntax("unexpected delimiter")),
            _ => {
                let at = self.pos;
                let w = self.word();
                if w.is_empty() {
                    return Err(EngineDataError::Syntax {
                        at,
                        what: "unexpected byte",
                    });
                }
                match w {
                    b"true" => Ok(EdValue::Bool(true)),
                    b"false" => Ok(EdValue::Bool(false)),
                    _ if matches!(w[0], b'-' | b'+' | b'.' | b'0'..=b'9') => {
                        let s = std::str::from_utf8(w).map_err(|_| EngineDataError::Syntax {
                            at,
                            what: "a malformed number",
                        })?;
                        s.parse::<f64>()
                            .map(EdValue::Num)
                            .map_err(|_| EngineDataError::Syntax {
                                at,
                                what: "a malformed number",
                            })
                    }
                    _ => Ok(EdValue::Word(String::from_utf8_lossy(w).into_owned())),
                }
            }
        }
    }
}

/// Parse an engine-data blob into its value tree.
///
/// The blob's outermost value is normally a `<< >>` dictionary; a blob that is
/// a bare run of `/Key value` pairs is read as one implicitly. Anything left
/// over after the outermost value is an error.
pub fn parse(bytes: &[u8]) -> Result<EdValue, EngineDataError> {
    if bytes.len() > MAX_ENGINE_DATA_BYTES {
        return Err(EngineDataError::TooLarge(bytes.len()));
    }
    let mut p = Parser {
        src: bytes,
        pos: 0,
        nodes: 0,
    };
    p.skip_space();
    let root = if p.peek() == Some(b'/') {
        let mut items = Vec::new();
        loop {
            p.skip_space();
            match p.peek() {
                None => break,
                Some(b'/') => {
                    let key = p.name()?;
                    items.push((key, p.value(1)?));
                }
                Some(_) => return Err(p.syntax("expected a /Key at the top level")),
            }
        }
        EdValue::Dict(items)
    } else {
        p.value(0)?
    };
    p.skip_space();
    if p.pos < bytes.len() {
        return Err(p.syntax("trailing bytes after the engine data"));
    }
    Ok(root)
}

/// Decode an engine-data string: `þÿ` + UTF-16BE (what Photoshop writes), or
/// UTF-8, or — when neither decodes — Latin-1. Never fails.
pub fn decode_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

// --------------------------------------------------------------- extraction

/// One character style, every field optional: an absent field means "the
/// file did not say", and the normal sheet or the importer's default applies.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CharStyle {
    /// The font's name as the file's `FontSet` gives it (usually a PostScript
    /// name such as `Montserrat-Bold`).
    pub font: Option<String>,
    pub size: Option<f64>,
    /// Straight RGBA, sRGB-encoded, 0..1.
    pub fill: Option<[f64; 4]>,
    /// 1/1000 em.
    pub tracking: Option<f64>,
    pub auto_leading: Option<bool>,
    pub leading: Option<f64>,
    pub faux_bold: Option<bool>,
    pub faux_italic: Option<bool>,
    /// 0 normal, 1 small caps, 2 all caps.
    pub caps: Option<i64>,
    /// 0 normal, 1 superscript, 2 subscript.
    pub baseline: Option<i64>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
    pub horizontal_scale: Option<f64>,
    pub vertical_scale: Option<f64>,
    pub baseline_shift: Option<f64>,
    /// Manual kerning (1/1000 em) the run sets (`Kerning`). Read so the
    /// importer can report it: the layer model keeps per-pair kerning, not
    /// per-run kerning, so it does not map.
    pub kerning: Option<f64>,
}

impl CharStyle {
    /// `self` with every field `over` sets replaced.
    fn overlay(&self, over: &CharStyle) -> CharStyle {
        CharStyle {
            font: over.font.clone().or_else(|| self.font.clone()),
            size: over.size.or(self.size),
            fill: over.fill.or(self.fill),
            tracking: over.tracking.or(self.tracking),
            auto_leading: over.auto_leading.or(self.auto_leading),
            leading: over.leading.or(self.leading),
            faux_bold: over.faux_bold.or(self.faux_bold),
            faux_italic: over.faux_italic.or(self.faux_italic),
            caps: over.caps.or(self.caps),
            baseline: over.baseline.or(self.baseline),
            underline: over.underline.or(self.underline),
            strikethrough: over.strikethrough.or(self.strikethrough),
            horizontal_scale: over.horizontal_scale.or(self.horizontal_scale),
            vertical_scale: over.vertical_scale.or(self.vertical_scale),
            baseline_shift: over.baseline_shift.or(self.baseline_shift),
            kerning: over.kerning.or(self.kerning),
        }
    }
}

/// One paragraph style, every field optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParagraphStyle {
    /// 0 left, 1 right, 2 centre, 3 justify (last left), 4 justify (last
    /// right), 5 justify (last centre), 6 justify all.
    pub justification: Option<i64>,
    pub first_line_indent: Option<f64>,
    pub start_indent: Option<f64>,
    pub end_indent: Option<f64>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
    /// The paragraph's auto-leading factor (`AutoLeading`, Photoshop's
    /// default 1.2): a run with auto leading spaces its lines by this many
    /// times the font size.
    pub auto_leading: Option<f64>,
}

impl ParagraphStyle {
    fn overlay(&self, over: &ParagraphStyle) -> ParagraphStyle {
        ParagraphStyle {
            justification: over.justification.or(self.justification),
            first_line_indent: over.first_line_indent.or(self.first_line_indent),
            start_indent: over.start_indent.or(self.start_indent),
            end_indent: over.end_indent.or(self.end_indent),
            space_before: over.space_before.or(self.space_before),
            space_after: over.space_after.or(self.space_after),
            auto_leading: over.auto_leading.or(self.auto_leading),
        }
    }
}

/// A character style covering `length` UTF-16 code units.
#[derive(Debug, Clone, PartialEq)]
pub struct StyleRun {
    pub length: usize,
    pub style: CharStyle,
}

/// A paragraph style covering `length` UTF-16 code units.
#[derive(Debug, Clone, PartialEq)]
pub struct ParagraphRun {
    pub length: usize,
    pub style: ParagraphStyle,
}

/// Point text or paragraph (box) text.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum ShapeFrame {
    #[default]
    Point,
    Box {
        width: f64,
        height: f64,
    },
}

/// What the engine data says about one type layer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineText {
    /// The editor's string, without the trailing `\r` the engine appends.
    pub text: String,
    /// Character runs, merged over the normal style sheet, in order.
    pub style_runs: Vec<StyleRun>,
    /// Paragraph runs, merged over the normal paragraph sheet, in order.
    pub paragraph_runs: Vec<ParagraphRun>,
    pub frame: ShapeFrame,
    /// A box's top-left corner (`BoxBounds` left, top) relative to the
    /// `TySh` transform's origin; zero for point text.
    pub box_origin: [f64; 2],
}

fn num(d: &EdValue, key: &str) -> Option<f64> {
    d.get(key).and_then(EdValue::as_f64)
}

fn int(d: &EdValue, key: &str) -> Option<i64> {
    num(d, key)
        .filter(|v| v.abs() < 1e12)
        .map(|v| v.round() as i64)
}

fn flag(d: &EdValue, key: &str) -> Option<bool> {
    d.get(key).and_then(EdValue::as_bool)
}

fn char_style(d: &EdValue, fonts: &[String]) -> CharStyle {
    let fill = d.get("FillColor").and_then(|c| {
        let v = c.get("Values")?.as_array()?;
        if v.len() != 4 {
            return None;
        }
        let mut argb = [0.0f64; 4];
        for (slot, x) in argb.iter_mut().zip(v) {
            *slot = x.as_f64()?.clamp(0.0, 1.0);
        }
        Some([argb[1], argb[2], argb[3], argb[0]])
    });
    CharStyle {
        font: int(d, "Font")
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| fonts.get(i).cloned()),
        size: num(d, "FontSize").filter(|s| *s > 0.0),
        fill,
        tracking: num(d, "Tracking"),
        auto_leading: flag(d, "AutoLeading"),
        leading: num(d, "Leading").filter(|l| *l > 0.0),
        faux_bold: flag(d, "FauxBold"),
        faux_italic: flag(d, "FauxItalic"),
        caps: int(d, "FontCaps"),
        baseline: int(d, "FontBaseline"),
        underline: flag(d, "Underline"),
        strikethrough: flag(d, "Strikethrough"),
        horizontal_scale: num(d, "HorizontalScale").filter(|s| *s > 0.0),
        vertical_scale: num(d, "VerticalScale").filter(|s| *s > 0.0),
        baseline_shift: num(d, "BaselineShift"),
        kerning: num(d, "Kerning"),
    }
}

fn paragraph_style(d: &EdValue) -> ParagraphStyle {
    ParagraphStyle {
        justification: int(d, "Justification"),
        first_line_indent: num(d, "FirstLineIndent"),
        start_indent: num(d, "StartIndent"),
        end_indent: num(d, "EndIndent"),
        space_before: num(d, "SpaceBefore"),
        space_after: num(d, "SpaceAfter"),
        auto_leading: num(d, "AutoLeading").filter(|f| *f > 0.0),
    }
}

/// `RunLengthArray` (or, failing that, a per-entry `RunLength`), validated.
fn run_lengths(run: &EdValue, entries: usize) -> Result<Vec<usize>, EngineDataError> {
    let lengths: Vec<&EdValue> = match run.get("RunLengthArray").and_then(EdValue::as_array) {
        Some(a) => a.iter().collect(),
        None => run
            .get("RunArray")
            .and_then(EdValue::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|e| e.get("RunLength"))
            .collect(),
    };
    if lengths.len() > MAX_RUNS {
        return Err(EngineDataError::TooManyRuns);
    }
    if lengths.len() != entries {
        return Err(EngineDataError::Invalid(
            "the run lengths and the run array disagree in length",
        ));
    }
    lengths
        .into_iter()
        .map(|v| match v.as_f64() {
            Some(n) if n >= 0.0 && n.fract() == 0.0 && n <= u32::MAX as f64 => Ok(n as usize),
            _ => Err(EngineDataError::Invalid(
                "a run length is not a non-negative whole number",
            )),
        })
        .collect()
}

/// Read the text, style runs, paragraph runs and frame out of an engine-data
/// blob.
///
/// `Ok(None)` when the blob parses but carries no `EngineDict` (nothing to
/// read, not an error); `Err` when it is malformed or exceeds a cap.
pub fn extract(bytes: &[u8]) -> Result<Option<EngineText>, EngineDataError> {
    let root = parse(bytes)?;
    let Some(engine) = root.get("EngineDict") else {
        return Ok(None);
    };
    if !matches!(engine, EdValue::Dict(_)) {
        return Err(EngineDataError::Invalid("EngineDict is not a dictionary"));
    }
    let resources = root
        .get("ResourceDict")
        .or_else(|| root.get("DocumentResources"));

    let fonts: Vec<String> = resources
        .and_then(|r| r.get("FontSet"))
        .and_then(EdValue::as_array)
        .unwrap_or(&[])
        .iter()
        .map(|f| match f.get("Name") {
            Some(EdValue::Str(s)) => decode_string(s),
            Some(EdValue::Name(n)) => n.clone(),
            _ => String::new(),
        })
        .collect();

    // The normal sheets sit under every run.
    let sheet_at = |set: &str, index: &str| -> Option<&EdValue> {
        let r = resources?;
        let i = usize::try_from(int(r, index).unwrap_or(0)).ok()?;
        r.get(set)?.as_array()?.get(i)
    };
    let normal_char = sheet_at("StyleSheetSet", "TheNormalStyleSheet")
        .and_then(|s| s.get("StyleSheetData"))
        .map(|d| char_style(d, &fonts))
        .unwrap_or_default();
    let normal_para = sheet_at("ParagraphSheetSet", "TheNormalParagraphSheet")
        .and_then(|s| s.get("Properties"))
        .map(paragraph_style)
        .unwrap_or_default();

    let mut out = EngineText {
        text: match engine.path(&["Editor", "Text"]) {
            Some(EdValue::Str(s)) => {
                let t = decode_string(s);
                t.strip_suffix('\r').map(str::to_owned).unwrap_or(t)
            }
            _ => String::new(),
        },
        ..EngineText::default()
    };

    if let Some(style) = engine.get("StyleRun") {
        let default = style
            .path(&["DefaultRunData", "StyleSheet", "StyleSheetData"])
            .map(|d| char_style(d, &fonts))
            .unwrap_or_default();
        let base = normal_char.overlay(&default);
        let entries = style
            .get("RunArray")
            .and_then(EdValue::as_array)
            .unwrap_or(&[]);
        let lengths = run_lengths(style, entries.len())?;
        for (entry, length) in entries.iter().zip(lengths) {
            let own = entry
                .path(&["StyleSheet", "StyleSheetData"])
                .map(|d| char_style(d, &fonts))
                .unwrap_or_default();
            out.style_runs.push(StyleRun {
                length,
                style: base.overlay(&own),
            });
        }
    }
    if let Some(para) = engine.get("ParagraphRun") {
        let default = para
            .path(&["DefaultRunData", "ParagraphSheet", "Properties"])
            .map(paragraph_style)
            .unwrap_or_default();
        let base = normal_para.overlay(&default);
        let entries = para
            .get("RunArray")
            .and_then(EdValue::as_array)
            .unwrap_or(&[]);
        let lengths = run_lengths(para, entries.len())?;
        for (entry, length) in entries.iter().zip(lengths) {
            let own = entry
                .path(&["ParagraphSheet", "Properties"])
                .map(paragraph_style)
                .unwrap_or_default();
            out.paragraph_runs.push(ParagraphRun {
                length,
                style: base.overlay(&own),
            });
        }
    }

    let photoshop = engine
        .path(&["Rendered", "Shapes", "Children"])
        .and_then(EdValue::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.path(&["Cookie", "Photoshop"]));
    if let Some(ps) = photoshop {
        if int(ps, "ShapeType") == Some(1) {
            if let Some(b) = ps.get("BoxBounds").and_then(EdValue::as_array) {
                let v: Vec<f64> = b.iter().filter_map(EdValue::as_f64).collect();
                if v.len() == 4 {
                    let (w, h) = (v[2] - v[0], v[3] - v[1]);
                    if w > 0.0 && h >= 0.0 && w.is_finite() && h.is_finite() {
                        out.frame = ShapeFrame::Box {
                            width: w,
                            height: h,
                        };
                        if v[0].is_finite() && v[1].is_finite() {
                            out.box_origin = [v[0].clamp(-1e6, 1e6), v[1].clamp(-1e6, 1e6)];
                        }
                    }
                }
            }
        }
    }
    Ok(Some(out))
}

// ------------------------------------------------------------------ writing

/// Format a number the way the engine writes one (no exponent, no NaN).
fn fnum(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v}");
    if s.contains('e') {
        format!("{v:.6}")
    } else {
        s
    }
}

/// A string literal: printable ASCII stays readable (escaped where the syntax
/// needs it); anything else is written as `þÿ` + UTF-16BE, as Photoshop does,
/// with `(`, `)` and `\` bytes escaped.
fn literal(s: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    let bytes: Vec<u8> = if s.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        s.as_bytes().to_vec()
    } else {
        let mut v = vec![0xfe, 0xff];
        for u in s.encode_utf16() {
            v.extend_from_slice(&u.to_be_bytes());
        }
        v
    };
    for b in bytes {
        if matches!(b, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b')');
    out
}

fn char_style_body(out: &mut Vec<u8>, s: &CharStyle, fonts: &[String], indent: &str) {
    let mut line = |k: &str, v: String| {
        out.extend_from_slice(format!("{indent}/{k} {v}\n").as_bytes());
    };
    if let Some(f) = &s.font {
        if let Some(i) = fonts.iter().position(|n| n == f) {
            line("Font", i.to_string());
        }
    }
    if let Some(v) = s.size {
        line("FontSize", fnum(v));
    }
    if let Some(v) = s.faux_bold {
        line("FauxBold", v.to_string());
    }
    if let Some(v) = s.faux_italic {
        line("FauxItalic", v.to_string());
    }
    if let Some(v) = s.auto_leading {
        line("AutoLeading", v.to_string());
    }
    if let Some(v) = s.leading {
        line("Leading", fnum(v));
    }
    if let Some(v) = s.horizontal_scale {
        line("HorizontalScale", fnum(v));
    }
    if let Some(v) = s.vertical_scale {
        line("VerticalScale", fnum(v));
    }
    if let Some(v) = s.tracking {
        line("Tracking", fnum(v));
    }
    if let Some(v) = s.baseline_shift {
        line("BaselineShift", fnum(v));
    }
    if let Some(v) = s.kerning {
        line("Kerning", fnum(v));
    }
    if let Some(v) = s.caps {
        line("FontCaps", v.to_string());
    }
    if let Some(v) = s.baseline {
        line("FontBaseline", v.to_string());
    }
    if let Some(v) = s.underline {
        line("Underline", v.to_string());
    }
    if let Some(v) = s.strikethrough {
        line("Strikethrough", v.to_string());
    }
    if let Some([r, g, b, a]) = s.fill {
        line(
            "FillColor",
            format!(
                "<< /Type 1 /Values [ {} {} {} {} ] >>",
                fnum(a),
                fnum(r),
                fnum(g),
                fnum(b)
            ),
        );
    }
}

fn paragraph_body(out: &mut Vec<u8>, p: &ParagraphStyle, indent: &str) {
    let mut line = |k: &str, v: String| {
        out.extend_from_slice(format!("{indent}/{k} {v}\n").as_bytes());
    };
    if let Some(v) = p.justification {
        line("Justification", v.to_string());
    }
    for (k, v) in [
        ("FirstLineIndent", p.first_line_indent),
        ("StartIndent", p.start_indent),
        ("EndIndent", p.end_indent),
        ("SpaceBefore", p.space_before),
        ("SpaceAfter", p.space_after),
        ("AutoLeading", p.auto_leading),
    ] {
        if let Some(v) = v {
            line(k, fnum(v));
        }
    }
}

fn resources(out: &mut Vec<u8>, key: &str, fonts: &[String]) {
    out.extend_from_slice(format!("\t/{key}\n\t<<\n\t\t/FontSet\n\t\t[\n").as_bytes());
    for f in fonts {
        out.extend_from_slice(b"\t\t\t<<\n\t\t\t\t/Name ");
        out.extend_from_slice(&literal(f));
        out.extend_from_slice(
            b"\n\t\t\t\t/Script 0\n\t\t\t\t/FontType 0\n\t\t\t\t/Synthetic 0\n\t\t\t>>\n",
        );
    }
    out.extend_from_slice(b"\t\t]\n\t\t/StyleSheetSet\n\t\t[\n\t\t\t<<\n\t\t\t\t/Name ");
    out.extend_from_slice(&literal("Normal RGB"));
    out.extend_from_slice(b"\n\t\t\t\t/StyleSheetData\n\t\t\t\t<<\n\t\t\t\t\t/Font 0\n\t\t\t\t\t/FontSize 12\n\t\t\t\t\t/AutoLeading true\n\t\t\t\t\t/Tracking 0\n\t\t\t\t\t/FillColor << /Type 1 /Values [ 1 0 0 0 ] >>\n\t\t\t\t>>\n\t\t\t>>\n\t\t]\n\t\t/ParagraphSheetSet\n\t\t[\n\t\t\t<<\n\t\t\t\t/Name ");
    out.extend_from_slice(&literal("Normal RGB"));
    out.extend_from_slice(b"\n\t\t\t\t/DefaultStyleSheet 0\n\t\t\t\t/Properties\n\t\t\t\t<<\n\t\t\t\t\t/Justification 0\n\t\t\t\t>>\n\t\t\t>>\n\t\t]\n\t\t/TheNormalStyleSheet 0\n\t\t/TheNormalParagraphSheet 0\n\t>>\n");
}

/// Write an [`EngineText`] as a Photoshop-shaped engine-data blob.
///
/// The editor text gets the engine's trailing `\r`, and the last style and
/// paragraph run are extended by one unit to cover it (the engine requires
/// the runs to cover every unit). Each style run also carries a `RunLength`
/// key naming the characters of the string it covers.
pub fn write(engine: &EngineText) -> Vec<u8> {
    let mut fonts: Vec<String> = Vec::new();
    for r in &engine.style_runs {
        if let Some(f) = &r.style.font {
            if !fonts.contains(f) {
                fonts.push(f.clone());
            }
        }
    }
    let total: usize = engine.text.encode_utf16().count() + 1;
    let fit = |runs: Vec<usize>| -> Vec<usize> {
        // Lengths as written: the last run absorbs the terminator and any
        // shortfall; runs past the end are dropped to zero.
        let mut left = total;
        let n = runs.len();
        runs.into_iter()
            .enumerate()
            .map(|(i, l)| {
                let l = if i + 1 == n { left } else { l.min(left) };
                left -= l;
                l
            })
            .collect()
    };

    let mut out: Vec<u8> =
        b"\n\n<<\n\t/EngineDict\n\t<<\n\t\t/Editor\n\t\t<<\n\t\t\t/Text ".to_vec();
    out.extend_from_slice(&literal(&format!("{}\r", engine.text)));
    out.extend_from_slice(b"\n\t\t>>\n");

    // Paragraph runs.
    let paras: Vec<ParagraphRun> = if engine.paragraph_runs.is_empty() {
        vec![ParagraphRun {
            length: total,
            style: ParagraphStyle::default(),
        }]
    } else {
        engine.paragraph_runs.clone()
    };
    let para_lengths = fit(paras.iter().map(|p| p.length).collect());
    out.extend_from_slice(b"\t\t/ParagraphRun\n\t\t<<\n\t\t\t/DefaultRunData\n\t\t\t<<\n\t\t\t\t/ParagraphSheet\n\t\t\t\t<<\n\t\t\t\t\t/DefaultStyleSheet 0\n\t\t\t\t\t/Properties\n\t\t\t\t\t<<\n\t\t\t\t\t>>\n\t\t\t\t>>\n\t\t\t\t/Adjustments\n\t\t\t\t<<\n\t\t\t\t\t/Axis [ 1 0 1 ]\n\t\t\t\t\t/XY [ 0 0 ]\n\t\t\t\t>>\n\t\t\t>>\n\t\t\t/RunArray\n\t\t\t[\n");
    for p in &paras {
        out.extend_from_slice(b"\t\t\t\t<<\n\t\t\t\t\t/ParagraphSheet\n\t\t\t\t\t<<\n\t\t\t\t\t\t/DefaultStyleSheet 0\n\t\t\t\t\t\t/Properties\n\t\t\t\t\t\t<<\n");
        paragraph_body(&mut out, &p.style, "\t\t\t\t\t\t\t");
        out.extend_from_slice(b"\t\t\t\t\t\t>>\n\t\t\t\t\t>>\n\t\t\t\t\t/Adjustments\n\t\t\t\t\t<<\n\t\t\t\t\t\t/Axis [ 1 0 1 ]\n\t\t\t\t\t\t/XY [ 0 0 ]\n\t\t\t\t\t>>\n\t\t\t\t>>\n");
    }
    out.extend_from_slice(b"\t\t\t]\n\t\t\t/RunLengthArray [");
    for l in &para_lengths {
        out.extend_from_slice(format!(" {l}").as_bytes());
    }
    out.extend_from_slice(b" ]\n\t\t\t/IsJoinable 1\n\t\t>>\n");

    // Style runs.
    let styles: Vec<StyleRun> = if engine.style_runs.is_empty() {
        vec![StyleRun {
            length: total,
            style: CharStyle::default(),
        }]
    } else {
        engine.style_runs.clone()
    };
    let style_lengths = fit(styles.iter().map(|s| s.length).collect());
    out.extend_from_slice(b"\t\t/StyleRun\n\t\t<<\n\t\t\t/DefaultRunData\n\t\t\t<<\n\t\t\t\t/StyleSheet\n\t\t\t\t<<\n\t\t\t\t\t/StyleSheetData\n\t\t\t\t\t<<\n\t\t\t\t\t>>\n\t\t\t\t>>\n\t\t\t>>\n\t\t\t/RunArray\n\t\t\t[\n");
    let text_units = total - 1;
    let mut covered = 0usize;
    for (s, l) in styles.iter().zip(&style_lengths) {
        out.extend_from_slice(b"\t\t\t\t<<\n\t\t\t\t\t/StyleSheet\n\t\t\t\t\t<<\n\t\t\t\t\t\t/StyleSheetData\n\t\t\t\t\t\t<<\n");
        char_style_body(&mut out, &s.style, &fonts, "\t\t\t\t\t\t\t");
        // The characters of the string this run covers (the terminator is
        // not a character of the string).
        let visible = (*l).min(text_units.saturating_sub(covered));
        covered += l;
        out.extend_from_slice(
            format!("\t\t\t\t\t\t>>\n\t\t\t\t\t>>\n\t\t\t\t\t/RunLength {visible}\n\t\t\t\t>>\n")
                .as_bytes(),
        );
    }
    out.extend_from_slice(b"\t\t\t]\n\t\t\t/RunLengthArray [");
    for l in &style_lengths {
        out.extend_from_slice(format!(" {l}").as_bytes());
    }
    out.extend_from_slice(
        b" ]\n\t\t\t/IsJoinable 2\n\t\t>>\n\t\t/AntiAlias 4\n\t\t/UseFractionalGlyphWidths true\n",
    );

    // The shape: point or box.
    let cookie = match engine.frame {
        ShapeFrame::Point => "/ShapeType 0\n\t\t\t\t\t\t\t\t/PointBase [ 0 0 ]".to_string(),
        ShapeFrame::Box { width, height } => {
            let [left, top] = engine.box_origin;
            format!(
                "/ShapeType 1\n\t\t\t\t\t\t\t\t/BoxBounds [ {} {} {} {} ]",
                fnum(left),
                fnum(top),
                fnum(left + width),
                fnum(top + height)
            )
        }
    };
    let shape_type = match engine.frame {
        ShapeFrame::Point => 0,
        ShapeFrame::Box { .. } => 1,
    };
    out.extend_from_slice(
        format!(
            "\t\t/Rendered\n\t\t<<\n\t\t\t/Version 1\n\t\t\t/Shapes\n\t\t\t<<\n\t\t\t\t/WritingDirection 0\n\t\t\t\t/Children\n\t\t\t\t[\n\t\t\t\t\t<<\n\t\t\t\t\t\t/ShapeType {shape_type}\n\t\t\t\t\t\t/Procession 0\n\t\t\t\t\t\t/Lines\n\t\t\t\t\t\t<<\n\t\t\t\t\t\t\t/WritingDirection 0\n\t\t\t\t\t\t\t/Children [ ]\n\t\t\t\t\t\t>>\n\t\t\t\t\t\t/Cookie\n\t\t\t\t\t\t<<\n\t\t\t\t\t\t\t/Photoshop\n\t\t\t\t\t\t\t<<\n\t\t\t\t\t\t\t\t{cookie}\n\t\t\t\t\t\t\t\t/Base\n\t\t\t\t\t\t\t\t<<\n\t\t\t\t\t\t\t\t\t/ShapeType {shape_type}\n\t\t\t\t\t\t\t\t\t/TransformPoint0 [ 1 0 ]\n\t\t\t\t\t\t\t\t\t/TransformPoint1 [ 0 1 ]\n\t\t\t\t\t\t\t\t\t/TransformPoint2 [ 0 0 ]\n\t\t\t\t\t\t\t\t>>\n\t\t\t\t\t\t\t>>\n\t\t\t\t\t\t>>\n\t\t\t\t\t>>\n\t\t\t\t]\n\t\t\t>>\n\t\t>>\n\t>>\n"
        )
        .as_bytes(),
    );
    resources(&mut out, "ResourceDict", &fonts);
    resources(&mut out, "DocumentResources", &fonts);
    out.extend_from_slice(b">>\n");
    out
}

// ----------------------------------------------------- layer-model mapping

fn srgb_to_linear(c: f64) -> f32 {
    let c = c.clamp(0.0, 1.0);
    (if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }) as f32
}

fn linear_to_srgb(c: f32) -> f64 {
    let c = f64::from(c.clamp(0.0, 1.0));
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Split a font name as a `FontSet` gives it — usually a PostScript name like
/// `Montserrat-BoldItalic` — into a family guess, a weight and an italic flag.
/// A suffix after the last `-` counts only when it is a recognised style word,
/// so a family whose name contains a hyphen survives whole.
pub fn split_font_name(name: &str) -> (String, Option<Weight>, bool) {
    let Some((family, suffix)) = name.rsplit_once('-') else {
        return (name.to_owned(), None, false);
    };
    let mut s = suffix.to_ascii_lowercase();
    if let Some(rest) = s.strip_suffix("mt") {
        s = rest.to_owned();
    }
    let mut italic = false;
    for tail in ["italic", "oblique", "it"] {
        if let Some(rest) = s.strip_suffix(tail) {
            s = rest.to_owned();
            italic = true;
            break;
        }
    }
    let weight = match s.as_str() {
        "" | "regular" | "roman" | "book" | "normal" => Some(Weight::NORMAL),
        "thin" | "hairline" => Some(Weight(100)),
        "extralight" | "ultralight" => Some(Weight(200)),
        "light" => Some(Weight(300)),
        "medium" => Some(Weight(500)),
        "semibold" | "demibold" | "demi" => Some(Weight(600)),
        "bold" => Some(Weight::BOLD),
        "extrabold" | "ultrabold" | "heavy" => Some(Weight(800)),
        "black" => Some(Weight(900)),
        _ => None,
    };
    match weight {
        Some(w) if !family.is_empty() => (family.to_owned(), Some(w), italic),
        _ => (name.to_owned(), None, false),
    }
}

/// The style word [`split_font_name`] reads back as `weight`, for the named
/// weights 100..=900; `None` for any other weight.
fn weight_word(weight: Weight) -> Option<&'static str> {
    Some(match weight.0 {
        100 => "Thin",
        200 => "ExtraLight",
        300 => "Light",
        400 => "Regular",
        500 => "Medium",
        600 => "SemiBold",
        700 => "Bold",
        800 => "ExtraBold",
        900 => "Black",
        _ => return None,
    })
}

/// The nearest named weight (100..=900 in hundreds).
fn nearest_named_weight(weight: Weight) -> Weight {
    Weight(((u32::from(weight.0) + 50) / 100 * 100).clamp(100, 900) as u16)
}

/// The `FontSet` name [`from_text_layer`] writes for a family at a weight: a
/// PostScript-style `Family-Weight` name that [`split_font_name`] reads back
/// as the same family and weight. The bare family is written for Regular
/// unless the bare name would itself split into a family and a style (a
/// family such as `Foo-Light` is written `Foo-Light-Regular`). A weight
/// between the named ones is written as the nearest named one;
/// [`unwritten_styling`] names that.
pub fn font_name_for(family: &str, weight: Weight) -> String {
    let weight = nearest_named_weight(weight);
    let bare_splits = split_font_name(family).1.is_some();
    match weight_word(weight) {
        Some(_) if weight == Weight::NORMAL && !bare_splits => family.to_owned(),
        Some(word) => format!("{family}-{word}"),
        None => family.to_owned(),
    }
}

/// What [`from_text_layer`] cannot write into the engine data, in words for
/// the export report (empty when everything is written). The layer's raster
/// fallback still shows it.
pub fn unwritten_styling(layer: &TextLayer) -> Vec<&'static str> {
    let mut out = Vec::new();
    let weights = std::iter::once(layer.style.weight)
        .chain(layer.spans.iter().filter_map(|s| s.style.weight));
    let named = |w: Weight| weight_word(w).is_some();
    if weights.into_iter().any(|w| !named(w)) {
        out.push("font weight between the named weights (written as the nearest)");
    }
    out
}

/// One run's look in the layer model's vocabulary.
#[derive(Debug, Clone, PartialEq)]
struct Look {
    family: String,
    size: f32,
    weight: Weight,
    slant: Slant,
    fill: [f32; 4],
    tracking: f32,
    underline: bool,
    strikethrough: bool,
    script: Script,
}

/// Map a byte range of `text` from a UTF-16 unit range.
fn utf16_to_bytes(text: &str) -> Vec<(usize, usize)> {
    // (utf16 offset, byte offset) at every char boundary, plus the end.
    let mut table = Vec::with_capacity(text.len() + 1);
    let mut u = 0usize;
    for (b, c) in text.char_indices() {
        table.push((u, b));
        u += c.len_utf16();
    }
    table.push((u, text.len()));
    table
}

fn byte_at(table: &[(usize, usize)], unit: usize) -> usize {
    let i = table.partition_point(|(u, _)| *u < unit);
    table
        .get(i)
        .map(|(_, b)| *b)
        .unwrap_or_else(|| table.last().map_or(0, |(_, b)| *b))
}

/// The style runs as byte ranges of the string `table` was built from,
/// empty ranges dropped.
fn style_ranges<'a>(
    engine: &'a EngineText,
    table: &[(usize, usize)],
) -> Vec<(usize, usize, &'a CharStyle)> {
    let mut ranges = Vec::new();
    let mut unit = 0usize;
    for run in &engine.style_runs {
        let start = byte_at(table, unit);
        unit = unit.saturating_add(run.length);
        let end = byte_at(table, unit);
        if end > start {
            ranges.push((start, end, &run.style));
        }
    }
    ranges
}

/// The run [`to_text_layer`] takes its layer-wide style from: the first run
/// that covers any of the string.
fn base_style_of<'a>(
    engine: &'a EngineText,
    ranges: &[(usize, usize, &'a CharStyle)],
    default_style: &'a CharStyle,
) -> &'a CharStyle {
    ranges
        .first()
        .map(|r| r.2)
        .or_else(|| engine.style_runs.first().map(|r| &r.style))
        .unwrap_or(default_style)
}

/// What [`to_text_layer`] reads but cannot carry onto the layer, in words for
/// the import report (empty when everything mapped).
///
/// The layer model holds leading, caps, horizontal/vertical scale and
/// baseline shift once per layer, so [`to_text_layer`] takes them from the
/// base run: a later run that sets a different value is named here. So are
/// manual kerning on any run (the layer model's kerning is per pair, not per
/// run) and paragraph runs after the first that differ from it (the layer
/// has one paragraph style).
pub fn unmapped_styling(engine: &EngineText, text: &str) -> Vec<&'static str> {
    let table = utf16_to_bytes(text);
    let ranges = style_ranges(engine, &table);
    let default_style = CharStyle::default();
    let base = base_style_of(engine, &ranges, &default_style);
    let leading = |s: &CharStyle| match (s.auto_leading, s.leading) {
        (Some(false), Some(l)) => Some(l),
        _ => None,
    };
    let caps = |s: &CharStyle| s.caps.filter(|c| matches!(c, 1 | 2)).unwrap_or(0);
    // The base is the first range's style (or no range at all).
    let later = || ranges.iter().skip(1).map(|r| r.2);
    let mut out = Vec::new();
    if later().any(|s| leading(s) != leading(base)) {
        out.push("leading that changes within the text");
    }
    if later().any(|s| caps(s) != caps(base)) {
        out.push("caps that change within the text");
    }
    let scale = |v: Option<f64>| v.unwrap_or(1.0);
    if later().any(|s| scale(s.horizontal_scale) != scale(base.horizontal_scale)) {
        out.push("horizontal scale that changes within the text");
    }
    if later().any(|s| scale(s.vertical_scale) != scale(base.vertical_scale)) {
        out.push("vertical scale that changes within the text");
    }
    let shift = |v: Option<f64>| v.unwrap_or(0.0);
    if later().any(|s| shift(s.baseline_shift) != shift(base.baseline_shift)) {
        out.push("baseline shift that changes within the text");
    }
    if ranges.iter().any(|r| r.2.kerning.is_some_and(|k| k != 0.0)) {
        out.push("manual kerning");
    }
    if let Some((first, rest)) = engine.paragraph_runs.split_first() {
        if rest.iter().any(|p| p.style != first.style) {
            out.push("paragraph styles after the first paragraph");
        }
    }
    out
}

/// Map an [`EngineText`] onto the persisted text schema.
///
/// `text` is the layer's string (the `Txt ` descriptor's, when there is one);
/// run lengths are mapped onto it by UTF-16 units. The first non-empty style
/// run becomes the base style (family, size, fill, …); every later run that
/// differs becomes a span. Family names pass through `resolve`, which maps a
/// family guess (see [`split_font_name`]) to the name to store — an installed
/// family's spelling when one matches. `fallback_family` / `fallback_size`
/// stand in where the file names neither.
pub fn to_text_layer(
    engine: &EngineText,
    text: &str,
    fallback_family: &str,
    fallback_size: f32,
    resolve: &mut dyn FnMut(&str) -> String,
) -> TextLayer {
    let table = utf16_to_bytes(text);
    let look_of = |s: &CharStyle, resolve: &mut dyn FnMut(&str) -> String| -> Look {
        let (family, weight, italic) = match &s.font {
            Some(name) if !name.is_empty() => {
                let (guess, w, it) = split_font_name(name);
                if guess == GENERIC_SANS_NAME {
                    (String::new(), w, it)
                } else {
                    (resolve(&guess), w, it)
                }
            }
            _ => (fallback_family.to_owned(), None, false),
        };
        let mut weight = weight.unwrap_or(Weight::NORMAL);
        if s.faux_bold == Some(true) && weight < Weight::BOLD {
            weight = Weight::BOLD;
        }
        let slant = if italic || s.faux_italic == Some(true) {
            Slant::Italic
        } else {
            Slant::Normal
        };
        let fill = s.fill.map_or([0.0, 0.0, 0.0, 1.0], |[r, g, b, a]| {
            [
                srgb_to_linear(r),
                srgb_to_linear(g),
                srgb_to_linear(b),
                a.clamp(0.0, 1.0) as f32,
            ]
        });
        Look {
            family,
            size: s
                .size
                .map_or(fallback_size, |v| v.clamp(0.1, 10_000.0) as f32),
            weight,
            slant,
            fill,
            tracking: s
                .tracking
                .map_or(0.0, |t| t.clamp(-10_000.0, 10_000.0) as f32),
            underline: s.underline.unwrap_or(false),
            strikethrough: s.strikethrough.unwrap_or(false),
            script: match s.baseline {
                Some(1) => Script::Superscript,
                Some(2) => Script::Subscript,
                _ => Script::Normal,
            },
        }
    };

    let ranges = style_ranges(engine, &table);
    let default_style = CharStyle::default();
    let base_style = base_style_of(engine, &ranges, &default_style);
    let base = look_of(base_style, resolve);

    let mut spans: Vec<StyleSpan> = Vec::new();
    for (start, end, style) in ranges.iter().skip(1) {
        let look = look_of(style, resolve);
        let o = StyleOverride {
            family: (look.family != base.family).then(|| look.family.clone()),
            size_px: (look.size != base.size).then_some(look.size),
            weight: (look.weight != base.weight).then_some(look.weight),
            slant: (look.slant != base.slant).then_some(look.slant),
            stretch: None,
            fill: (look.fill != base.fill).then_some(look.fill),
            underline: (look.underline != base.underline).then_some(look.underline),
            strikethrough: (look.strikethrough != base.strikethrough).then_some(look.strikethrough),
            script: (look.script != base.script).then_some(look.script),
            tracking: (look.tracking != base.tracking).then_some(look.tracking),
        };
        if o == StyleOverride::default() {
            continue;
        }
        match spans.last_mut() {
            Some(prev) if prev.end == *start && prev.style == o => prev.end = *end,
            _ => spans.push(StyleSpan {
                start: *start,
                end: *end,
                style: o,
            }),
        }
    }

    let para = engine
        .paragraph_runs
        .first()
        .map(|p| p.style.clone())
        .unwrap_or_default();
    let px = |v: Option<f64>| v.map_or(0.0, |v| v.clamp(-100_000.0, 100_000.0) as f32);
    // Auto leading is the paragraph's factor times the size (Photoshop's
    // default factor is 1.2), which is what `Leading::Multiple` means.
    let leading = match (base_style.auto_leading, base_style.leading) {
        (Some(false), Some(l)) => Leading::Absolute(l.clamp(0.1, 100_000.0) as f32),
        _ => Leading::Multiple(para.auto_leading.unwrap_or(1.2).clamp(0.01, 100.0) as f32),
    };

    TextLayer {
        text: text.to_owned(),
        font_family: base.family.clone(),
        size_px: base.size,
        style: BaseStyle {
            weight: base.weight,
            slant: base.slant,
            fill: base.fill,
            underline: base.underline,
            strikethrough: base.strikethrough,
            script: base.script,
            tracking: base.tracking,
            horizontal_scale: base_style
                .horizontal_scale
                .map_or(1.0, |v| v.clamp(0.01, 100.0) as f32),
            vertical_scale: base_style
                .vertical_scale
                .map_or(1.0, |v| v.clamp(0.01, 100.0) as f32),
            baseline_shift: px(base_style.baseline_shift),
            caps: match base_style.caps {
                Some(1) => Caps::SmallCaps,
                Some(2) => Caps::AllCaps,
                _ => Caps::Normal,
            },
            ..BaseStyle::default()
        },
        spans,
        paragraph: Paragraph {
            alignment: match para.justification {
                Some(1) => Alignment::Right,
                Some(2) => Alignment::Center,
                Some(3) => Alignment::Justified,
                Some(4) => Alignment::JustifyLastRight,
                Some(5) => Alignment::JustifyLastCenter,
                Some(6) => Alignment::JustifyAll,
                _ => Alignment::Left,
            },
            leading,
            first_line_indent: px(para.first_line_indent),
            space_before: px(para.space_before),
            space_after: px(para.space_after),
            left_indent: px(para.start_indent),
            right_indent: px(para.end_indent),
            vertical: false,
        },
        frame: match engine.frame {
            ShapeFrame::Point => Frame::Point,
            ShapeFrame::Box { width, height } => Frame::Box {
                width: width.clamp(1.0, 1e6) as f32,
                height: (height > 0.0).then(|| height.clamp(1.0, 1e6) as f32),
            },
        },
        kerning: Vec::new(),
        ..TextLayer::default()
    }
}

/// Map a persisted text layer onto an [`EngineText`] for [`write`]: one style
/// run per stretch of the string whose resolved style is constant (base style
/// with every covering span applied, in order), one paragraph run. An empty
/// family is written as [`GENERIC_SANS_NAME`]; each run's weight travels in
/// its font name ([`font_name_for`]); `Leading::Multiple(m)` is written as
/// auto leading with the paragraph's `AutoLeading` factor `m`, so it reads
/// back as the same multiple.
///
/// Manual kerning, stretch, ligature/kerning switches, anti-aliasing and
/// vertical type are not encoded here.
pub fn from_text_layer(layer: &TextLayer) -> EngineText {
    let text = &layer.text;
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for s in &layer.spans {
        for b in [s.start, s.end] {
            if b <= text.len() && text.is_char_boundary(b) {
                cuts.push(b);
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    let base = &layer.style;
    let style_at = |start: usize, end: usize| -> CharStyle {
        let mut family = layer.font_family.clone();
        let mut size = layer.size_px;
        let mut weight = base.weight;
        let mut slant = base.slant;
        let mut fill = base.fill;
        let mut underline = base.underline;
        let mut strike = base.strikethrough;
        let mut script = base.script;
        let mut tracking = base.tracking;
        for s in &layer.spans {
            if s.start <= start && end <= s.end {
                let o = &s.style;
                if let Some(v) = &o.family {
                    family = v.clone();
                }
                size = o.size_px.unwrap_or(size);
                weight = o.weight.unwrap_or(weight);
                slant = o.slant.unwrap_or(slant);
                fill = o.fill.unwrap_or(fill);
                underline = o.underline.unwrap_or(underline);
                strike = o.strikethrough.unwrap_or(strike);
                script = o.script.unwrap_or(script);
                tracking = o.tracking.unwrap_or(tracking);
            }
        }
        let [r, g, b, a] = fill;
        // A multiple is auto leading at the paragraph's factor (written
        // below), so it stays a multiple of each run's size on the way back.
        let (auto_leading, leading) = match layer.paragraph.leading {
            Leading::Absolute(v) => (false, Some(f64::from(v))),
            Leading::Multiple(_) => (true, None),
        };
        let family = if family.is_empty() {
            GENERIC_SANS_NAME.to_owned()
        } else {
            family
        };
        CharStyle {
            // The weight travels in the name (`Family-SemiBold`), which
            // `split_font_name` reads back exactly; faux bold stays off so
            // it cannot move the weight on the way back.
            font: Some(font_name_for(&family, weight)),
            size: Some(f64::from(size)),
            fill: Some([
                linear_to_srgb(r),
                linear_to_srgb(g),
                linear_to_srgb(b),
                f64::from(a.clamp(0.0, 1.0)),
            ]),
            tracking: Some(f64::from(tracking)),
            auto_leading: Some(auto_leading),
            leading,
            faux_bold: Some(false),
            faux_italic: Some(slant == Slant::Italic),
            caps: Some(match base.caps {
                Caps::Normal => 0,
                Caps::SmallCaps => 1,
                Caps::AllCaps => 2,
            }),
            baseline: Some(match script {
                Script::Normal => 0,
                Script::Superscript => 1,
                Script::Subscript => 2,
            }),
            underline: Some(underline),
            strikethrough: Some(strike),
            horizontal_scale: Some(f64::from(base.horizontal_scale)),
            vertical_scale: Some(f64::from(base.vertical_scale)),
            baseline_shift: Some(f64::from(base.baseline_shift)),
            kerning: None,
        }
    };

    let mut style_runs: Vec<StyleRun> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let length = text[a..b].encode_utf16().count();
        let style = style_at(a, b);
        match style_runs.last_mut() {
            Some(prev) if prev.style == style => prev.length += length,
            _ => style_runs.push(StyleRun { length, style }),
        }
    }
    if style_runs.is_empty() {
        style_runs.push(StyleRun {
            length: 0,
            style: style_at(0, 0),
        });
    }

    let p = &layer.paragraph;
    let paragraph = ParagraphStyle {
        justification: Some(match p.alignment {
            Alignment::Left => 0,
            Alignment::Right => 1,
            Alignment::Center => 2,
            Alignment::Justified => 3,
            Alignment::JustifyLastRight => 4,
            Alignment::JustifyLastCenter => 5,
            Alignment::JustifyAll => 6,
        }),
        first_line_indent: Some(f64::from(p.first_line_indent)),
        start_indent: Some(f64::from(p.left_indent)),
        end_indent: Some(f64::from(p.right_indent)),
        space_before: Some(f64::from(p.space_before)),
        space_after: Some(f64::from(p.space_after)),
        auto_leading: match p.leading {
            Leading::Multiple(m) => Some(f64::from(m)),
            Leading::Absolute(_) => None,
        },
    };

    EngineText {
        text: text.clone(),
        style_runs,
        paragraph_runs: vec![ParagraphRun {
            length: text.encode_utf16().count(),
            style: paragraph,
        }],
        frame: match layer.frame {
            Frame::Point => ShapeFrame::Point,
            Frame::Box { width, height } => ShapeFrame::Box {
                width: f64::from(width),
                height: f64::from(height.unwrap_or(0.0)),
            },
        },
        box_origin: [0.0, 0.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-crafted blob in Photoshop's own shape: UTF-16 strings with the
    /// BOM, a FontSet whose index 0 is the invisible font, two style runs over
    /// the normal sheet, a centred paragraph, and a box.
    fn photoshop_blob() -> Vec<u8> {
        let utf16 = |s: &str| -> Vec<u8> {
            let mut v = vec![b'(', 0xfe, 0xff];
            for u in s.encode_utf16() {
                for b in u.to_be_bytes() {
                    if matches!(b, b'(' | b')' | b'\\') {
                        v.push(b'\\');
                    }
                    v.push(b);
                }
            }
            v.push(b')');
            v
        };
        let mut b: Vec<u8> = Vec::new();
        b.extend_from_slice(b"\n\n<<\n\t/EngineDict\n\t<<\n\t\t/Editor\n\t\t<<\n\t\t\t/Text ");
        b.extend_from_slice(&utf16("Big (sale)\r"));
        b.extend_from_slice(
            b"\n\t\t>>\n\t\t/ParagraphRun\n\t\t<<\n\t\t\t/DefaultRunData\n\t\t\t<<\n\t\t\t\t/ParagraphSheet << /DefaultStyleSheet 0 /Properties << >> >>\n\t\t\t>>\n\t\t\t/RunArray [ << /ParagraphSheet << /DefaultStyleSheet 0 /Properties << /Justification 2 /FirstLineIndent 4.0 /StartIndent .5 >> >> >> ]\n\t\t\t/RunLengthArray [ 11 ]\n\t\t\t/IsJoinable 1\n\t\t>>\n\t\t/StyleRun\n\t\t<<\n\t\t\t/DefaultRunData << /StyleSheet << /StyleSheetData << >> >> >>\n\t\t\t/RunArray [\n\t\t\t<< /StyleSheet << /StyleSheetData << /Font 1 /FontSize 36.0 /FauxBold true /Tracking 50 /FillColor << /Type 1 /Values [ 1.0 1.0 0.0 0.0 ] >> >> >> >>\n\t\t\t<< /StyleSheet << /StyleSheetData << /Font 2 /FontSize 18.0 /AutoLeading false /Leading 30.0 /FontCaps 2 >> >> >>\n\t\t\t]\n\t\t\t/RunLengthArray [ 4 7 ]\n\t\t\t/IsJoinable 2\n\t\t>>\n\t\t/Rendered << /Version 1 /Shapes << /WritingDirection 0 /Children [ << /ShapeType 1 /Cookie << /Photoshop << /ShapeType 1 /BoxBounds [ 0.0 0.0 300.0 120.0 ] >> >> >> ] >> >>\n\t>>\n\t/ResourceDict\n\t<<\n\t\t/FontSet [\n\t\t\t<< /Name ",
        );
        b.extend_from_slice(&utf16("AdobeInvisFont"));
        b.extend_from_slice(b" /Script 0 >>\n\t\t\t<< /Name ");
        b.extend_from_slice(&utf16("Montserrat-Bold"));
        b.extend_from_slice(b" /Script 0 >>\n\t\t\t<< /Name ");
        b.extend_from_slice(&utf16("Lobster"));
        b.extend_from_slice(
            b" /Script 0 >>\n\t\t]\n\t\t/StyleSheetSet [ << /Name (Normal) /StyleSheetData << /Font 0 /FontSize 12.0 /FillColor << /Type 1 /Values [ 1.0 0.0 0.0 1.0 ] >> >> >> ]\n\t\t/TheNormalStyleSheet 0\n\t>>\n>>\n",
        );
        b
    }

    #[test]
    fn a_hand_crafted_photoshop_blob_yields_its_text_runs_and_frame() {
        let e = extract(&photoshop_blob()).unwrap().expect("an EngineDict");
        assert_eq!(e.text, "Big (sale)", "UTF-16, escaped parens, \\r stripped");
        assert_eq!(e.style_runs.len(), 2);
        let a = &e.style_runs[0];
        assert_eq!(a.length, 4);
        assert_eq!(a.style.font.as_deref(), Some("Montserrat-Bold"));
        assert_eq!(a.style.size, Some(36.0));
        assert_eq!(a.style.fill, Some([1.0, 0.0, 0.0, 1.0]), "ARGB → RGBA");
        assert_eq!(a.style.faux_bold, Some(true));
        assert_eq!(a.style.tracking, Some(50.0));
        let b = &e.style_runs[1];
        assert_eq!(b.length, 7);
        assert_eq!(b.style.font.as_deref(), Some("Lobster"));
        assert_eq!(b.style.size, Some(18.0));
        assert_eq!(
            b.style.fill,
            Some([0.0, 0.0, 1.0, 1.0]),
            "the normal sheet's blue shows through"
        );
        assert_eq!(b.style.caps, Some(2));
        assert_eq!(b.style.leading, Some(30.0));
        assert_eq!(e.paragraph_runs.len(), 1);
        assert_eq!(e.paragraph_runs[0].style.justification, Some(2));
        assert_eq!(e.paragraph_runs[0].style.start_indent, Some(0.5));
        assert_eq!(
            e.frame,
            ShapeFrame::Box {
                width: 300.0,
                height: 120.0
            }
        );

        // ...and onto the layer model: family from the PostScript name, the
        // second run a span.
        let layer = to_text_layer(&e, "Big (sale)", "sans-serif", 24.0, &mut |f| f.to_owned());
        assert_eq!(layer.font_family, "Montserrat");
        assert_eq!(layer.size_px, 36.0);
        assert_eq!(layer.style.weight, Weight::BOLD);
        assert_eq!(layer.style.fill, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(layer.style.tracking, 50.0);
        assert_eq!(layer.paragraph.alignment, Alignment::Center);
        assert_eq!(layer.paragraph.first_line_indent, 4.0);
        assert_eq!(
            layer.frame,
            Frame::Box {
                width: 300.0,
                height: Some(120.0)
            }
        );
        assert_eq!(layer.spans.len(), 1);
        let span = &layer.spans[0];
        assert_eq!((span.start, span.end), (4, 10));
        assert_eq!(span.style.family.as_deref(), Some("Lobster"));
        assert_eq!(span.style.size_px, Some(18.0));
        assert_eq!(span.style.weight, Some(Weight::NORMAL));
        assert_eq!(span.style.fill, Some([0.0, 0.0, 1.0, 1.0]));
        // The second run's leading and all-caps have nowhere to go on a span
        // (the layer holds them once, from the first run): named, not dropped.
        assert_eq!(
            unmapped_styling(&e, "Big (sale)"),
            vec![
                "leading that changes within the text",
                "caps that change within the text"
            ]
        );
    }

    /// Per-run values the layer model holds once per layer, manual kerning
    /// and later paragraph runs are each named; values equal to the base
    /// run's are not.
    #[test]
    fn styling_the_layer_model_cannot_hold_is_named() {
        let run = |length: usize, style: CharStyle| StyleRun { length, style };
        let para = |length: usize, justification: i64| ParagraphRun {
            length,
            style: ParagraphStyle {
                justification: Some(justification),
                ..ParagraphStyle::default()
            },
        };
        let base = CharStyle {
            size: Some(10.0),
            horizontal_scale: Some(1.0),
            ..CharStyle::default()
        };
        let same = EngineText {
            text: "abcd".into(),
            style_runs: vec![
                run(2, base.clone()),
                run(
                    2,
                    CharStyle {
                        size: Some(20.0),
                        ..CharStyle::default()
                    },
                ),
            ],
            paragraph_runs: vec![para(2, 2), para(2, 2)],
            ..EngineText::default()
        };
        assert!(unmapped_styling(&same, "abcd").is_empty());

        let differs = EngineText {
            style_runs: vec![
                run(2, base),
                run(
                    2,
                    CharStyle {
                        horizontal_scale: Some(1.5),
                        vertical_scale: Some(0.5),
                        baseline_shift: Some(3.0),
                        kerning: Some(-40.0),
                        ..CharStyle::default()
                    },
                ),
            ],
            paragraph_runs: vec![para(2, 2), para(2, 0)],
            ..same
        };
        assert_eq!(
            unmapped_styling(&differs, "abcd"),
            vec![
                "horizontal scale that changes within the text",
                "vertical scale that changes within the text",
                "baseline shift that changes within the text",
                "manual kerning",
                "paragraph styles after the first paragraph",
            ]
        );
        // Kerning survives the writer and the parser.
        let back = extract(&write(&differs)).unwrap().unwrap();
        assert_eq!(back.style_runs[1].style.kerning, Some(-40.0));
    }

    #[test]
    fn the_writer_round_trips_a_styled_layer() {
        let layer = TextLayer {
            text: "Hello Wörld\nline two".into(),
            font_family: "Montserrat".into(),
            size_px: 40.0,
            style: BaseStyle {
                fill: [1.0, 0.0, 0.0, 1.0],
                tracking: 25.0,
                caps: Caps::AllCaps,
                ..BaseStyle::default()
            },
            spans: vec![StyleSpan {
                start: 6,
                end: 12,
                style: StyleOverride {
                    family: Some("DejaVu Serif".into()),
                    size_px: Some(20.0),
                    weight: Some(Weight::BOLD),
                    fill: Some([0.0, 0.0, 1.0, 1.0]),
                    ..StyleOverride::default()
                },
            }],
            paragraph: Paragraph {
                alignment: Alignment::Right,
                leading: Leading::Absolute(50.0),
                first_line_indent: 3.0,
                left_indent: 2.0,
                right_indent: 1.0,
                ..Paragraph::default()
            },
            frame: Frame::Box {
                width: 400.0,
                height: Some(90.0),
            },
            ..TextLayer::default()
        };
        let engine = from_text_layer(&layer);
        let blob = write(&engine);
        let back = extract(&blob).unwrap().expect("an EngineDict");
        assert_eq!(back.text, layer.text);
        assert!(
            unmapped_styling(&back, &layer.text).is_empty(),
            "the writer's own output maps in full"
        );
        let mapped = to_text_layer(&back, &layer.text, "x", 1.0, &mut |f| f.to_owned());
        assert_eq!(mapped.font_family, "Montserrat");
        assert_eq!(mapped.size_px, 40.0);
        assert_eq!(mapped.style.caps, Caps::AllCaps);
        assert_eq!(mapped.style.tracking, 25.0);
        for (got, want) in mapped.style.fill.iter().zip(layer.style.fill) {
            assert!((got - want).abs() < 1e-5, "{got} vs {want}");
        }
        assert_eq!(mapped.paragraph, layer.paragraph);
        assert_eq!(mapped.frame, layer.frame);
        assert_eq!(mapped.spans.len(), 1);
        let s = &mapped.spans[0];
        assert_eq!((s.start, s.end), (6, 12), "\"Wörld\" in bytes");
        assert_eq!(s.style.family.as_deref(), Some("DejaVu Serif"));
        assert_eq!(s.style.size_px, Some(20.0));
        assert_eq!(s.style.weight, Some(Weight::BOLD));
        let f = s.style.fill.unwrap();
        assert!((f[2] - 1.0).abs() < 1e-5 && f[0].abs() < 1e-5);
    }

    /// Round 3: every named weight, upright and italic, on a plain family,
    /// a hyphenated family whose last part reads like a style word, and the
    /// generic sans, comes back from the writer as the same weight — with
    /// nothing reported, because nothing was lost.
    #[test]
    fn every_named_weight_round_trips_through_the_writer() {
        for family in ["Montserrat", "Foo-Light", "Bar-It", ""] {
            for w in (100..=900).step_by(100) {
                for slant in [Slant::Normal, Slant::Italic] {
                    let layer = TextLayer {
                        text: "Weighty".into(),
                        font_family: family.into(),
                        size_px: 30.0,
                        style: BaseStyle {
                            weight: Weight(w),
                            slant,
                            ..BaseStyle::default()
                        },
                        ..TextLayer::default()
                    };
                    assert!(unwritten_styling(&layer).is_empty(), "{family} {w}");
                    let back = extract(&write(&from_text_layer(&layer))).unwrap().unwrap();
                    assert!(unmapped_styling(&back, &layer.text).is_empty());
                    let mapped = to_text_layer(&back, &layer.text, "x", 1.0, &mut |f| f.to_owned());
                    assert_eq!(mapped.font_family, family, "{family} {w} {slant:?}");
                    assert_eq!(mapped.style.weight, Weight(w), "{family} {w} {slant:?}");
                    assert_eq!(mapped.style.slant, slant, "{family} {w}");
                }
            }
        }
        // A span's weight travels the same way.
        let layer = TextLayer {
            text: "Light Black".into(),
            font_family: "Inter".into(),
            size_px: 20.0,
            style: BaseStyle {
                weight: Weight(300),
                ..BaseStyle::default()
            },
            spans: vec![StyleSpan {
                start: 6,
                end: 11,
                style: StyleOverride {
                    weight: Some(Weight(900)),
                    ..StyleOverride::default()
                },
            }],
            ..TextLayer::default()
        };
        let back = extract(&write(&from_text_layer(&layer))).unwrap().unwrap();
        let mapped = to_text_layer(&back, &layer.text, "x", 1.0, &mut |f| f.to_owned());
        assert_eq!(mapped.style.weight, Weight(300));
        assert_eq!(mapped.spans.len(), 1, "{:?}", mapped.spans);
        assert_eq!(mapped.spans[0].style.weight, Some(Weight(900)));
        assert_eq!(mapped.spans[0].style.family, None, "same family");
    }

    /// A weight between the named ones cannot be spelled in a font name: it
    /// is written as the nearest named weight and the export names it.
    #[test]
    fn an_off_grid_weight_is_written_as_the_nearest_and_named() {
        let layer = TextLayer {
            text: "x".into(),
            font_family: "Inter".into(),
            size_px: 20.0,
            style: BaseStyle {
                weight: Weight(450),
                ..BaseStyle::default()
            },
            ..TextLayer::default()
        };
        assert_eq!(
            unwritten_styling(&layer),
            vec!["font weight between the named weights (written as the nearest)"]
        );
        let back = extract(&write(&from_text_layer(&layer))).unwrap().unwrap();
        let mapped = to_text_layer(&back, "x", "x", 1.0, &mut |f| f.to_owned());
        assert_eq!(mapped.style.weight, Weight(500));
        assert_eq!(font_name_for("Inter", Weight(1000)), "Inter-Black");
    }

    /// Round 3: proportional leading comes back proportional — including
    /// over runs of different sizes, with no false "did not import" note —
    /// and Photoshop's own auto leading reads as its paragraph factor
    /// (1.2 when the file does not say).
    #[test]
    fn multiple_leading_round_trips_as_a_multiple() {
        for m in [1.0_f32, 1.5, 0.8, 2.25] {
            let layer = TextLayer {
                text: "small BIG".into(),
                font_family: "Inter".into(),
                size_px: 20.0,
                spans: vec![StyleSpan {
                    start: 6,
                    end: 9,
                    style: StyleOverride {
                        size_px: Some(40.0),
                        ..StyleOverride::default()
                    },
                }],
                paragraph: Paragraph {
                    leading: Leading::Multiple(m),
                    ..Paragraph::default()
                },
                ..TextLayer::default()
            };
            let back = extract(&write(&from_text_layer(&layer))).unwrap().unwrap();
            assert!(
                unmapped_styling(&back, &layer.text).is_empty(),
                "{m}: {:?}",
                unmapped_styling(&back, &layer.text)
            );
            let mapped = to_text_layer(&back, &layer.text, "x", 1.0, &mut |f| f.to_owned());
            assert_eq!(mapped.paragraph.leading, Leading::Multiple(m), "{m}");
        }

        let blob = |props: &str| -> Vec<u8> {
            format!(
                "<< /EngineDict << /Editor << /Text (ab\r) >> /ParagraphRun << /RunArray [ << /ParagraphSheet << /Properties << {props} >> >> >> ] /RunLengthArray [ 3 ] >> /StyleRun << /RunArray [ << /StyleSheet << /StyleSheetData << /FontSize 10.0 /AutoLeading true >> >> >> ] /RunLengthArray [ 3 ] >> >> >>"
            )
            .into_bytes()
        };
        for (props, want) in [("", 1.2_f32), ("/AutoLeading 1.5", 1.5)] {
            let e = extract(&blob(props)).unwrap().unwrap();
            let mapped = to_text_layer(&e, "ab", "x", 1.0, &mut |f| f.to_owned());
            assert_eq!(mapped.paragraph.leading, Leading::Multiple(want), "{props}");
        }
    }

    /// A box's `BoxBounds` top-left is kept (the importer offsets the layer
    /// by it) and written back where it was read.
    #[test]
    fn a_box_origin_is_read_and_written() {
        let blob = b"<< /EngineDict << /Editor << /Text (ab\r) >> /Rendered << /Shapes << /Children [ << /Cookie << /Photoshop << /ShapeType 1 /BoxBounds [ 10.0 -5.0 310.0 115.0 ] >> >> >> ] >> >> >> >>";
        let e = extract(blob).unwrap().unwrap();
        assert_eq!(
            e.frame,
            ShapeFrame::Box {
                width: 300.0,
                height: 120.0
            }
        );
        assert_eq!(e.box_origin, [10.0, -5.0]);
        let again = extract(&write(&e)).unwrap().unwrap();
        assert_eq!(again.frame, e.frame);
        assert_eq!(again.box_origin, [10.0, -5.0]);
    }

    #[test]
    fn the_generic_sans_round_trips_as_an_empty_family() {
        let layer = TextLayer::legacy("x", "", 12.0);
        let back = extract(&write(&from_text_layer(&layer))).unwrap().unwrap();
        let mapped = to_text_layer(&back, "x", "fallback", 1.0, &mut |f| f.to_owned());
        assert_eq!(mapped.font_family, "");
        assert_eq!(mapped.size_px, 12.0);
    }

    #[test]
    fn postscript_names_split_into_family_weight_and_slant() {
        assert_eq!(
            split_font_name("Montserrat-BoldItalic"),
            ("Montserrat".into(), Some(Weight::BOLD), true)
        );
        assert_eq!(
            split_font_name("Arial-BoldMT"),
            ("Arial".into(), Some(Weight::BOLD), false)
        );
        assert_eq!(split_font_name("ArialMT"), ("ArialMT".into(), None, false));
        assert_eq!(
            split_font_name("Noto-Sans"),
            ("Noto-Sans".into(), None, false),
            "a hyphen before a non-style word stays in the family"
        );
    }

    #[test]
    fn a_blob_without_an_engine_dict_is_not_an_error() {
        assert_eq!(extract(b"<< /x 1 >>"), Ok(None));
    }

    #[test]
    fn malformed_blobs_are_errors_not_panics() {
        for bad in [
            &b"<< /EngineDict"[..],
            b"<< /EngineDict << /Editor << /Text (abc",
            b"<< /EngineDict >>",
            b"<< /a 1.2.3 >>",
            b"<< /a 1 >> junk",
            b">>",
            b"<< 12 >>",
            b"<< /EngineDict << /StyleRun << /RunArray [ << >> ] /RunLengthArray [ -3 ] >> >> >>",
            b"<< /EngineDict << /StyleRun << /RunArray [ << >> ] /RunLengthArray [ 1 2 ] >> >> >>",
            b"<< /EngineDict 5 >>",
        ] {
            assert!(extract(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn nesting_past_the_cap_is_refused_without_overflowing_the_stack() {
        let deep = "[".repeat(100_000);
        assert_eq!(parse(deep.as_bytes()), Err(EngineDataError::TooDeep));
        let mut ok = "[".repeat(MAX_DEPTH);
        ok.push_str(&"]".repeat(MAX_DEPTH));
        assert!(parse(ok.as_bytes()).is_ok());
    }

    #[test]
    fn size_and_node_caps_are_enforced() {
        let big = vec![b' '; MAX_ENGINE_DATA_BYTES + 1];
        assert_eq!(
            parse(&big),
            Err(EngineDataError::TooLarge(MAX_ENGINE_DATA_BYTES + 1))
        );
        let mut many = String::from("[");
        for _ in 0..=MAX_NODES {
            many.push_str(" 1");
        }
        many.push(']');
        assert_eq!(parse(many.as_bytes()), Err(EngineDataError::TooManyNodes));
    }

    /// Fuzz-style: every truncation and a spread of single-byte corruptions of
    /// a real blob return (Ok or Err) — never a panic.
    #[test]
    fn every_truncation_and_corruption_returns() {
        let blob = photoshop_blob();
        for cut in 0..blob.len() {
            let _ = extract(&blob[..cut]);
        }
        let mut seed = 0x2545_f491_u32;
        for i in 0..blob.len() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let mut m = blob.clone();
            m[i] = (seed & 0xff) as u8;
            if let Ok(Some(e)) = extract(&m) {
                let _ = to_text_layer(&e, &e.text, "f", 12.0, &mut |f| f.to_owned());
            }
        }
    }

    /// Run lengths that overrun or fall short of the string map onto it
    /// without slicing mid-character.
    #[test]
    fn run_lengths_that_disagree_with_the_string_are_clamped() {
        let e = EngineText {
            text: "é🎨x".into(),
            style_runs: vec![
                StyleRun {
                    length: 2, // splits the emoji's surrogate pair
                    style: CharStyle {
                        size: Some(10.0),
                        ..CharStyle::default()
                    },
                },
                StyleRun {
                    length: 999,
                    style: CharStyle {
                        size: Some(20.0),
                        ..CharStyle::default()
                    },
                },
            ],
            ..EngineText::default()
        };
        let l = to_text_layer(&e, "é🎨x", "f", 12.0, &mut |f| f.to_owned());
        assert_eq!(l.size_px, 10.0);
        assert_eq!(l.spans.len(), 1);
        assert!(l.text.is_char_boundary(l.spans[0].start));
        assert_eq!(l.spans[0].end, l.text.len());
    }
}
