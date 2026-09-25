//! W15-E: a bounded PostScript interpreter for EPS artwork.
//!
//! [`render`] runs the PostScript of an EPS file and draws what it paints.
//! The painted marks are written as an SVG document (in device pixels, one
//! pixel per point, the page cut to the file's `%%HiResBoundingBox` or
//! `%%BoundingBox`), which the existing `resvg` importer
//! ([`crate::codec::svg_import::rasterize`]) draws under the caller's
//! [`ImportLimits`].
//!
//! # What is interpreted
//!
//! The subset design tools emit (Illustrator, Inkscape / cairo, CorelDRAW,
//! matplotlib, Ghostscript's `eps2write`):
//!
//! * the operand stack, dictionaries (`def`, `bind`, `load`, `store`,
//!   `where`, `known`, `begin` / `end`, `<< >>`), procedures and control
//!   flow (`exec`, `if`, `ifelse`, `repeat`, `for`, `loop`, `exit`, `forall`,
//!   `stopped`, `stop`), arithmetic, relational and bitwise operators,
//!   arrays, strings, `token`, `cvs` / `cvn` / `cvi` / `cvr` / `cvx`;
//! * the graphics state: `gsave` / `grestore` / `save` / `restore`, `concat`,
//!   `translate`, `scale`, `rotate` and the matrix operators, `setgray`,
//!   `setrgbcolor`, `setcmykcolor`, `sethsbcolor`, `setcolorspace` /
//!   `setcolor` (DeviceGray / RGB / CMYK, ICCBased, Indexed, Separation and
//!   DeviceN through their tint transform), `setcustomcolor`, line width,
//!   cap, join, miter limit and dash;
//! * paths: `moveto`, `lineto`, `curveto` and their relative forms, `arc`,
//!   `arcn`, `arct`, `arcto`, `closepath`, `rectfill`, `rectstroke`,
//!   `rectclip`, `pathbbox`, `currentpoint`, and painting with `fill`,
//!   `eofill`, `stroke`, `clip`, `eoclip`;
//! * `image`, `colorimage` and `imagemask` (the Level 1 operand forms and
//!   the Level 2 dictionary form, 1 / 2 / 4 / 8 / 12 / 16 bits), reading
//!   their samples from a string, a procedure (`currentfile picstr
//!   readhexstring pop`) or a file, through the `ASCIIHexDecode`,
//!   `ASCII85Decode`, `RunLengthDecode`, `FlateDecode`, `DCTDecode` and
//!   `SubFileDecode` filters;
//! * text through `show`, `ashow`, `widthshow`, `awidthshow`, `xshow`,
//!   `yshow` and `xyshow`, set in a **fallback font** (a system font chosen
//!   from the font's name: serif, monospace or sans-serif), with an
//!   estimated advance of 0.55 em a character. An embedded Type 1 font's
//!   encrypted section (`eexec`) is skipped, and its text is drawn in the
//!   fallback font; the note says so.
//!
//! # What is not
//!
//! Smooth shading (`shfill`), patterns, `charpath`, `strokepath`,
//! `kshow` / `cshow`, `ImageType` 3 / 4 masked images, and the file system
//! (`file`, `run`, `deletefile`) are not drawn or run: each is **reported**
//! in the note, not silently dropped. An operator this interpreter does not
//! know is reported by name and skipped (inside `stopped` it raises
//! `undefined`, as PostScript would, so a feature probe still sees it
//! missing).
//!
//! # Bounds (untrusted input)
//!
//! The bounds: an operation count and a wall-clock budget ([`Budget`]), the
//! operand stack depth ([`MAX_OPERAND_STACK`]), the dictionary stack,
//! execution nesting ([`MAX_EXEC_DEPTH`]) and procedure nesting in the
//! scanner, the filters stacked over one file ([`MAX_FILTER_CHAIN`]),
//! colour spaces nested in colour spaces ([`MAX_CSPACE_NESTING`]), a memory
//! budget that every string, array, dictionary, decoded filter byte and
//! image sample counts against, the points in one path, and the size of
//! the SVG it writes ([`MAX_SVG_BYTES`], [`MAX_SVG_ELEMENTS`]). Integer
//! arithmetic stays in 32 bits (overflow becomes a real, as in PostScript)
//! and every index is checked. The operation count is taken per executed
//! object, per element `bind` walks and per colour space it parses; the
//! clock is read every 1 024 operations, so one operation's own work (a
//! string copy or `search` over strings as large as the memory budget,
//! linear in their length) is not interrupted and can run past the time
//! budget by that much.
//!
//! Data may nest as deep as the memory budget allows (`[ exch ]` in a loop
//! builds a chain a million deep): arrays, dictionaries and files (a filter
//! keeps the file or procedure it reads alive) are freed by an iterative
//! `Drop` (a work list, not the recursive drop glue). `bind` walks a
//! procedure with a work list and visits each array once. When a run ends
//! every array and dictionary it made is emptied, so the cycles PostScript
//! builds (`systemdict` holds itself; a program may put an array into
//! itself) are freed, not leaked, after each open. Tested for the malformed
//! and byte-flipped inputs and the deep, self-referencing and cyclic data in
//! `postscript_tests` on a 1 MiB stack; that is what the tests show, not a
//! proof that no input can overflow the native stack or panic.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::Write as _;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use crate::codec::{
    decode_surface_bytes_as, encode, svg_import, CodecError, DecodedSurface, ExportFormat,
    ImportFormat, ImportLimits, SurfacePixels,
};

/// The deepest the operand stack may grow.
pub const MAX_OPERAND_STACK: usize = 100_000;
/// The deepest the dictionary stack may grow.
pub const MAX_DICT_STACK: usize = 1_000;
/// The deepest procedures may nest while they run.
pub const MAX_EXEC_DEPTH: usize = 120;
/// The deepest `{ }` may nest in the source.
pub const MAX_PROC_NESTING: usize = 200;
/// The most decoding filters one file may be read through (each filter
/// reads the one under it, so the chain is bounded like the call depth).
pub const MAX_FILTER_CHAIN: usize = 32;
/// The deepest a colour space may name another as its base or alternative
/// (PostScript allows one level; a program could otherwise make a colour
/// space array that holds itself).
pub const MAX_CSPACE_NESTING: usize = 4;
/// The deepest `gsave` may nest.
pub const MAX_GSAVE: usize = 1_000;
/// The most points one path may hold.
pub const MAX_PATH_POINTS: usize = 2_000_000;
/// The largest SVG the interpreter writes.
pub const MAX_SVG_BYTES: usize = 96 << 20;
/// The most painted elements the SVG may hold.
pub const MAX_SVG_ELEMENTS: usize = 400_000;
/// The most errors one run may recover from before it gives up.
pub const MAX_ERRORS: usize = 2_000;
/// Estimated advance of one character of the fallback font, in em.
const ADVANCE_EM: f64 = 0.55;
/// The font dictionary key that keeps the FontMatrix a font was defined
/// with (so a scaled copy still knows its glyph units).
const BASE_MATRIX: &str = ".RasterStudioBaseFontMatrix";

/// The work one EPS may do.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// Objects executed (operators, names, procedure elements, loop turns).
    pub max_ops: u64,
    /// Wall-clock time.
    pub time: Duration,
    /// Bytes of strings, arrays, dictionaries, decoded filter output and
    /// image samples.
    pub max_vm_bytes: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            max_ops: 30_000_000,
            time: Duration::from_secs(15),
            max_vm_bytes: 512 << 20,
        }
    }
}

/// Run `ps` (the PostScript of an EPS) under the default [`Budget`] and draw
/// it, with the sentence that says what was drawn and what was not.
pub fn render(ps: &[u8], limits: ImportLimits) -> Result<(DecodedSurface, String), CodecError> {
    render_with(ps, limits, Budget::default())
}

/// [`render`] under an explicit budget.
pub fn render_with(
    ps: &[u8],
    limits: ImportLimits,
    budget: Budget,
) -> Result<(DecodedSurface, String), CodecError> {
    let bbox = bounding_box(ps).ok_or_else(|| {
        CodecError::Unsupported("the PostScript has no usable %%BoundingBox".into())
    })?;
    let (llx, lly, urx, ury) = bbox;
    let (w, h) = ((urx - llx).ceil(), (ury - lly).ceil());
    if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0) {
        return Err(CodecError::Unsupported(
            "the PostScript's %%BoundingBox has no area".into(),
        ));
    }
    if w > f64::from(limits.max_width) || h > f64::from(limits.max_height) {
        return Err(CodecError::LimitExceeded(format!(
            "a {w}x{h} point EPS page is larger than the import limit"
        )));
    }
    let mut it = Interp::new(ps, [1.0, 0.0, 0.0, -1.0, -llx, ury], (w, h), limits, budget);
    let outcome = it.run_main();
    if let Err(stop) = outcome {
        return Err(match stop {
            Stop::Budget(why) => CodecError::LimitExceeded(format!("the PostScript {why}")),
            Stop::Malformed(why) => CodecError::Unsupported(format!("malformed PostScript: {why}")),
        });
    }
    if !it.painted {
        let why = match &it.first_error {
            Some(e) => format!("the PostScript drew nothing (first error: {e})"),
            None => "the PostScript drew nothing".to_string(),
        };
        return Err(CodecError::Unsupported(why));
    }
    let note = it.note();
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\n{}</svg>\n",
        it.svg
    );
    let mut surface = svg_import::rasterize(svg.as_bytes(), limits)?;
    surface.source_format = ImportFormat::Eps;
    Ok((surface, note))
}

/// The page box: `%%HiResBoundingBox` over `%%BoundingBox`; the header's
/// value first, a trailer's when the header says `(atend)`.
pub fn bounding_box(ps: &[u8]) -> Option<(f64, f64, f64, f64)> {
    let find = |key: &[u8]| -> Option<(f64, f64, f64, f64)> {
        let mut atend = false;
        let mut last = None;
        for line in ps.split(|b| *b == b'\n' || *b == b'\r') {
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            let text = String::from_utf8_lossy(rest);
            if text.trim() == "(atend)" {
                atend = true;
                continue;
            }
            let v: Vec<f64> = text
                .split_whitespace()
                .take(4)
                .filter_map(|t| t.parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .collect();
            if let [a, b, c, d] = v[..] {
                if c > a && d > b {
                    if !atend {
                        return Some((a, b, c, d));
                    }
                    last = Some((a, b, c, d));
                }
            }
        }
        last
    };
    find(b"%%HiResBoundingBox:").or_else(|| find(b"%%BoundingBox:"))
}

// ------------------------------------------------------------------ objects

type Res<T = ()> = Result<T, Flow>;
type OpFn = fn(&mut Interp) -> Res;
type DictRef = Rc<RefCell<Dict>>;
type FileRef = Rc<RefCell<FileState>>;
type Matrix = [f64; 6];

struct Builtin {
    name: &'static str,
    f: OpFn,
}

/// Why execution left the normal path.
enum Flow {
    Exit,
    Stop,
    Quit,
    Error(&'static str, String),
    Fatal(Stop),
}

/// Why the whole run ended early.
enum Stop {
    Budget(String),
    Malformed(String),
}

#[derive(Clone)]
enum Obj {
    Null,
    Mark,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Rc<str>, bool),
    Str(PsStr),
    Arr(PsArr),
    Dict(DictRef),
    Op(&'static Builtin),
    File(FileRef),
    Save(usize),
}

#[derive(Clone)]
struct PsStr {
    buf: Rc<RefCell<Vec<u8>>>,
    start: usize,
    len: usize,
    exec: bool,
}

impl PsStr {
    fn bytes(&self) -> Vec<u8> {
        let b = self.buf.borrow();
        b.get(self.start..self.start + self.len)
            .map(<[u8]>::to_vec)
            .unwrap_or_default()
    }
}

#[derive(Clone)]
struct PsArr {
    buf: Rc<RefCell<Vec<Obj>>>,
    start: usize,
    len: usize,
    exec: bool,
}

impl PsArr {
    fn get(&self, i: usize) -> Option<Obj> {
        if i >= self.len {
            return None;
        }
        self.buf.borrow().get(self.start + i).cloned()
    }
    fn items(&self) -> Vec<Obj> {
        let b = self.buf.borrow();
        b.get(self.start..self.start + self.len)
            .map(<[Obj]>::to_vec)
            .unwrap_or_default()
    }
}

/// Arrays and dictionaries nest as deep as a program makes them (`[ exch ]`
/// in a loop builds a chain a million deep inside every budget), and the
/// derived drop glue would free that chain recursively: a native stack
/// overflow, which no error path catches. The last owner of an array or a
/// dictionary instead empties it into a work list ([`release`]), so freeing
/// any nesting takes a bounded stack.
impl Drop for PsArr {
    fn drop(&mut self) {
        if Rc::strong_count(&self.buf) != 1 {
            return;
        }
        let items = match self.buf.try_borrow_mut() {
            Ok(mut b) if !b.is_empty() => std::mem::take(&mut *b),
            _ => return,
        };
        release(items, false);
    }
}

impl Drop for Dict {
    fn drop(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.index.clear();
        let items = self.entries.drain(..).map(|e| e.1).collect();
        release(items, false);
    }
}

/// Free `work` without recursion: an array or dictionary about to be freed
/// (its last owner is in the list, or `all`, when a run is torn down) is
/// emptied into the list first, so dropping it is shallow. Emptying a
/// container twice is a no-op, so a cycle ends too.
fn release(mut work: Vec<Obj>, all: bool) {
    while let Some(o) = work.pop() {
        match &o {
            Obj::Arr(a) if all || Rc::strong_count(&a.buf) == 1 => {
                if let Ok(mut b) = a.buf.try_borrow_mut() {
                    work.append(&mut b);
                }
            }
            Obj::Dict(d) if all || Rc::strong_count(d) == 1 => {
                if let Ok(mut b) = d.try_borrow_mut() {
                    b.index.clear();
                    work.extend(b.entries.drain(..).map(|e| e.1));
                }
            }
            Obj::File(f) if all || Rc::strong_count(f) == 1 => {
                if let Ok(mut st) = f.try_borrow_mut() {
                    work.extend(st.take_children());
                }
            }
            _ => {}
        }
        drop(o);
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Name(Rc<str>),
    Int(i64),
    Real(u64),
    Bool(bool),
    Ptr(usize),
}

#[derive(Default)]
struct Dict {
    index: HashMap<Key, usize>,
    entries: Vec<(Key, Obj)>,
}

impl Dict {
    fn get(&self, k: &Key) -> Option<Obj> {
        self.index
            .get(k)
            .and_then(|i| self.entries.get(*i))
            .map(|e| e.1.clone())
    }
    fn put(&mut self, k: Key, v: Obj) -> bool {
        if let Some(i) = self.index.get(&k) {
            if let Some(e) = self.entries.get_mut(*i) {
                e.1 = v;
            }
            false
        } else {
            self.index.insert(k.clone(), self.entries.len());
            self.entries.push((k, v));
            true
        }
    }
    fn remove(&mut self, k: &Key) {
        if let Some(i) = self.index.remove(k) {
            self.entries.swap_remove(i);
            if let Some((moved, _)) = self.entries.get(i) {
                self.index.insert(moved.clone(), i);
            }
        }
    }
}

fn key_obj(k: &Key) -> Obj {
    match k {
        Key::Name(n) => Obj::Name(n.clone(), false),
        Key::Int(i) => Obj::Int(*i),
        Key::Real(r) => Obj::Real(f64::from_bits(*r)),
        Key::Bool(b) => Obj::Bool(*b),
        Key::Ptr(_) => Obj::Null,
    }
}

fn int_or_real(v: i64) -> Obj {
    if i64::from(i32::MIN) <= v && v <= i64::from(i32::MAX) {
        Obj::Int(v)
    } else {
        Obj::Real(v as f64)
    }
}

// -------------------------------------------------------------------- files

enum Src {
    /// Bytes in memory: the program itself, or a string.
    Mem { data: Rc<Vec<u8>>, pos: usize },
    /// A string read over and over (a Level 1 image data string).
    Repeat { data: Vec<u8>, pos: usize },
    /// A procedure called for each string it returns.
    Proc {
        proc_: Obj,
        out: VecDeque<u8>,
        done: bool,
    },
    /// A decoding filter over another file.
    Filter {
        kind: FilterKind,
        src: FileRef,
        out: VecDeque<u8>,
        eod: bool,
    },
}

enum FilterKind {
    Hex {
        high: Option<u8>,
    },
    A85 {
        group: Vec<u8>,
    },
    RunLength,
    Flate(Box<flate2::Decompress>),
    SubFile {
        count: i64,
        eod: Vec<u8>,
        pending: Vec<u8>,
        left: i64,
    },
    Dct,
}

struct FileState {
    src: Src,
    closed: bool,
    /// Bytes pushed back (a Flate decoder's unconsumed input).
    back: VecDeque<u8>,
}

impl FileState {
    /// The objects this file keeps alive (a procedure data source, or the
    /// file a filter reads), moved out: the file is left an empty string.
    fn take_children(&mut self) -> Option<Obj> {
        let empty = Src::Repeat {
            data: Vec::new(),
            pos: 0,
        };
        match std::mem::replace(&mut self.src, empty) {
            Src::Proc { proc_, .. } => Some(proc_),
            Src::Filter { src, .. } => Some(Obj::File(src)),
            other => {
                self.src = other;
                None
            }
        }
    }
}

/// A file is a container too: a filter keeps the file (or the procedure)
/// it reads alive, and that procedure may hold another filter, so
/// `file -> procedure -> array -> file` chains are as deep as a program
/// makes them. The last owner hands them to [`release`] like an array.
impl Drop for FileState {
    fn drop(&mut self) {
        if let Some(child) = self.take_children() {
            release(vec![child], false);
        }
    }
}

fn mem_file(data: Vec<u8>) -> FileRef {
    Rc::new(RefCell::new(FileState {
        src: Src::Mem {
            data: Rc::new(data),
            pos: 0,
        },
        closed: false,
        back: VecDeque::new(),
    }))
}

fn new_file(src: Src) -> FileRef {
    Rc::new(RefCell::new(FileState {
        src,
        closed: false,
        back: VecDeque::new(),
    }))
}

// ------------------------------------------------------------------ scanner

enum Tok {
    Int(i64),
    Real(f64),
    Name(String, NameKind),
    Str(Vec<u8>),
    ProcOpen,
    ProcClose,
}

#[derive(PartialEq)]
enum NameKind {
    Exec,
    Literal,
    Immediate,
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// The next token of `data` from `*pos`, or `None` at the end.
fn scan_token(data: &[u8], pos: &mut usize) -> Result<Option<Tok>, String> {
    loop {
        let Some(&b) = data.get(*pos) else {
            return Ok(None);
        };
        if is_ws(b) {
            *pos += 1;
        } else if b == b'%' {
            while let Some(&c) = data.get(*pos) {
                if c == b'\n' || c == b'\r' {
                    break;
                }
                *pos += 1;
            }
        } else {
            break;
        }
    }
    let b = data[*pos];
    *pos += 1;
    match b {
        b'{' => Ok(Some(Tok::ProcOpen)),
        b'}' => Ok(Some(Tok::ProcClose)),
        b'[' => Ok(Some(Tok::Name("[".into(), NameKind::Exec))),
        b']' => Ok(Some(Tok::Name("]".into(), NameKind::Exec))),
        b'(' => scan_string(data, pos).map(|s| Some(Tok::Str(s))),
        b')' => Err("an unbalanced ')'".into()),
        b'<' => match data.get(*pos) {
            Some(b'<') => {
                *pos += 1;
                Ok(Some(Tok::Name("<<".into(), NameKind::Exec)))
            }
            Some(b'~') => {
                *pos += 1;
                let start = *pos;
                let end = find(data, start, b"~>").ok_or("an unterminated <~ string")?;
                *pos = end + 2;
                let mut out = Vec::new();
                let mut group = Vec::new();
                for &c in &data[start..end] {
                    a85_push(&mut group, &mut out, c)?;
                }
                a85_finish(&mut group, &mut out)?;
                Ok(Some(Tok::Str(out)))
            }
            _ => {
                let mut out = Vec::new();
                let mut high: Option<u8> = None;
                loop {
                    let Some(&c) = data.get(*pos) else {
                        return Err("an unterminated hex string".into());
                    };
                    *pos += 1;
                    if c == b'>' {
                        break;
                    }
                    if is_ws(c) {
                        continue;
                    }
                    let v = hex_val(c).ok_or("a stray byte in a hex string")?;
                    match high.take() {
                        Some(h) => out.push(h << 4 | v),
                        None => high = Some(v),
                    }
                }
                if let Some(h) = high {
                    out.push(h << 4);
                }
                Ok(Some(Tok::Str(out)))
            }
        },
        b'>' => {
            if data.get(*pos) == Some(&b'>') {
                *pos += 1;
                Ok(Some(Tok::Name(">>".into(), NameKind::Exec)))
            } else {
                Err("a stray '>'".into())
            }
        }
        b'/' => {
            let kind = if data.get(*pos) == Some(&b'/') {
                *pos += 1;
                NameKind::Immediate
            } else {
                NameKind::Literal
            };
            let start = *pos;
            while let Some(&c) = data.get(*pos) {
                if is_ws(c) || is_delim(c) {
                    break;
                }
                *pos += 1;
            }
            let name = String::from_utf8_lossy(&data[start..*pos]).into_owned();
            eat_one_ws(data, pos);
            Ok(Some(Tok::Name(name, kind)))
        }
        _ if b >= 0x80 && !(0xA0..=0xFF).contains(&b) => {
            Err("a binary token (binary-encoded PostScript is not read)".into())
        }
        _ => {
            let start = *pos - 1;
            while let Some(&c) = data.get(*pos) {
                if is_ws(c) || is_delim(c) {
                    break;
                }
                *pos += 1;
            }
            let text = &data[start..*pos];
            eat_one_ws(data, pos);
            Ok(Some(number(text).unwrap_or_else(|| {
                Tok::Name(String::from_utf8_lossy(text).into_owned(), NameKind::Exec)
            })))
        }
    }
}

/// PostScript consumes the one whitespace byte that ends a token (a CR LF
/// pair counts as one), so binary data that follows starts exactly after it.
fn eat_one_ws(data: &[u8], pos: &mut usize) {
    match data.get(*pos) {
        Some(b'\r') => {
            *pos += 1;
            if data.get(*pos) == Some(&b'\n') {
                *pos += 1;
            }
        }
        Some(&c) if is_ws(c) => *pos += 1,
        _ => {}
    }
}

/// The first `needle` in `data` at or after `from`, in linear time and
/// constant extra space (memchr's two-way search), so a hostile
/// `(aaaa...) (aa...ab)` pair is neither quadratic nor a hidden allocation.
fn find(data: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let hay = data.get(from..)?;
    memchr::memmem::find(hay, needle).map(|i| from + i)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn number(text: &[u8]) -> Option<Tok> {
    let s = std::str::from_utf8(text).ok()?;
    if let Some((base, digits)) = s.split_once('#') {
        let base: u32 = base.parse().ok()?;
        if !(2..=36).contains(&base) || digits.is_empty() {
            return None;
        }
        let v = u64::from_str_radix(digits, base).ok()?;
        if v > u64::from(u32::MAX) {
            return Some(Tok::Real(v as f64));
        }
        // A radix number is a 32-bit pattern.
        return Some(Tok::Int(i64::from(v as u32 as i32)));
    }
    let first = s.as_bytes().first()?;
    if !(first.is_ascii_digit() || matches!(first, b'+' | b'-' | b'.')) {
        return None;
    }
    if !s.bytes().any(|c| c.is_ascii_digit())
        || !s
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.' | b'e' | b'E'))
    {
        return None;
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(if i32::try_from(i).is_ok() {
            Tok::Int(i)
        } else {
            Tok::Real(i as f64)
        });
    }
    s.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(Tok::Real)
}

fn scan_string(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut depth = 1usize;
    loop {
        let Some(&c) = data.get(*pos) else {
            return Err("an unterminated string".into());
        };
        *pos += 1;
        match c {
            b'(' => {
                depth += 1;
                out.push(c);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(out);
                }
                out.push(c);
            }
            b'\\' => {
                let Some(&e) = data.get(*pos) else {
                    return Err("an unterminated string".into());
                };
                *pos += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'\r' => {
                        if data.get(*pos) == Some(&b'\n') {
                            *pos += 1;
                        }
                    }
                    b'\n' => {}
                    b'0'..=b'7' => {
                        let mut v = u32::from(e - b'0');
                        for _ in 0..2 {
                            match data.get(*pos) {
                                Some(&d @ b'0'..=b'7') => {
                                    v = v * 8 + u32::from(d - b'0');
                                    *pos += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push((v & 0xFF) as u8);
                    }
                    other => out.push(other),
                }
            }
            b'\r' => {
                // An end of line inside a string reads as one LF.
                if data.get(*pos) == Some(&b'\n') {
                    *pos += 1;
                }
                out.push(b'\n');
            }
            _ => out.push(c),
        }
    }
}

fn a85_push(group: &mut Vec<u8>, out: &mut Vec<u8>, c: u8) -> Result<(), String> {
    if is_ws(c) {
        return Ok(());
    }
    if c == b'z' && group.is_empty() {
        out.extend_from_slice(&[0; 4]);
        return Ok(());
    }
    if !(b'!'..=b'u').contains(&c) {
        return Err("a stray byte in ASCII85 data".into());
    }
    group.push(c - b'!');
    if group.len() == 5 {
        let mut v: u64 = 0;
        for d in group.iter() {
            v = v * 85 + u64::from(*d);
        }
        if v > u64::from(u32::MAX) {
            return Err("an ASCII85 group out of range".into());
        }
        out.extend_from_slice(&(v as u32).to_be_bytes());
        group.clear();
    }
    Ok(())
}

fn a85_finish(group: &mut Vec<u8>, out: &mut Vec<u8>) -> Result<(), String> {
    let n = group.len();
    if n == 0 {
        return Ok(());
    }
    if n == 1 {
        return Err("a one-byte ASCII85 final group".into());
    }
    let mut v: u64 = 0;
    for i in 0..5 {
        v = v * 85 + u64::from(*group.get(i).unwrap_or(&84));
    }
    if v > u64::from(u32::MAX) {
        return Err("an ASCII85 group out of range".into());
    }
    out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
    group.clear();
    Ok(())
}

// ------------------------------------------------------------------ geometry

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` then `b`.
fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn apply(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn dapply(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y, m[1] * x + m[3] * y)
}

fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    Some([
        m[3] / det,
        -m[1] / det,
        -m[2] / det,
        m[0] / det,
        (m[2] * m[5] - m[3] * m[4]) / det,
        (m[1] * m[4] - m[0] * m[5]) / det,
    ])
}

#[derive(Clone, Copy)]
enum Seg {
    M(f64, f64),
    L(f64, f64),
    C(f64, f64, f64, f64, f64, f64),
    Z,
}

#[derive(Clone)]
enum CSpace {
    Gray,
    Rgb,
    Cmyk,
    Indexed {
        base: Box<CSpace>,
        hival: i64,
        lookup: Rc<Vec<u8>>,
    },
    Tint {
        n: usize,
        alt: Box<CSpace>,
        proc_: Obj,
    },
    Pattern,
}

impl CSpace {
    fn ncomp(&self) -> usize {
        match self {
            CSpace::Gray | CSpace::Indexed { .. } | CSpace::Pattern => 1,
            CSpace::Rgb => 3,
            CSpace::Cmyk => 4,
            CSpace::Tint { n, .. } => *n,
        }
    }
}

fn cmyk_rgb(c: f64, m: f64, y: f64, k: f64) -> [f64; 3] {
    [
        (1.0 - c) * (1.0 - k),
        (1.0 - m) * (1.0 - k),
        (1.0 - y) * (1.0 - k),
    ]
}

fn hsb_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    let h = (h.clamp(0.0, 1.0) * 6.0) % 6.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

#[derive(Clone)]
struct Font {
    name: String,
    matrix: Matrix,
    /// Glyph-space units per em: 1000 for a Type 1 font (FontMatrix
    /// 0.001), 1 for a Type 42 font (identity), from the matrix the font was
    /// defined with.
    units: f64,
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    /// `None` while a pattern is the colour (not drawn, reported).
    color: Option<[f64; 3]>,
    comps: Vec<f64>,
    cspace: CSpace,
    line_width: f64,
    cap: i64,
    join: i64,
    miter: f64,
    dash: (Vec<f64>, f64),
    path: Vec<Seg>,
    cp: Option<(f64, f64)>,
    start: Option<(f64, f64)>,
    clip: Vec<usize>,
    font: Option<Font>,
    save_level: usize,
}

impl GState {
    fn new(ctm: Matrix) -> Self {
        GState {
            ctm,
            color: Some([0.0; 3]),
            comps: vec![0.0],
            cspace: CSpace::Gray,
            line_width: 1.0,
            cap: 0,
            join: 0,
            miter: 10.0,
            dash: (Vec::new(), 0.0),
            path: Vec::new(),
            cp: None,
            start: None,
            clip: Vec::new(),
            font: None,
            save_level: 0,
        }
    }
}

fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let r = (v * 1000.0).round() / 1000.0;
    if r == r.trunc() && r.abs() < 1e15 {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

fn hex_color(c: [f64; 3]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

// -------------------------------------------------------------- interpreter

struct Interp {
    ostack: Vec<Obj>,
    dstack: Vec<DictRef>,
    g: GState,
    gstack: Vec<GState>,
    systemdict: DictRef,
    userdict: DictRef,
    fontdir: DictRef,
    files: Vec<FileRef>,
    main: FileRef,
    initial_ctm: Matrix,
    page: (f64, f64),
    limits: ImportLimits,
    budget: Budget,
    started: Instant,
    ops: u64,
    vm: u64,
    depth: usize,
    stopped_depth: usize,
    svg: String,
    elements: usize,
    next_clip: usize,
    painted: bool,
    text_drawn: bool,
    unknown: BTreeSet<String>,
    skipped: BTreeSet<&'static str>,
    type1_skipped: usize,
    errors: usize,
    first_error: Option<String>,
    rand: u32,
    /// Every array and dictionary this run made, so tearing the run down
    /// empties each one: the cycles PostScript makes (`systemdict` holds
    /// itself, a program may put an array into itself) are freed, not
    /// leaked, after every open.
    arrays: Vec<Weak<RefCell<Vec<Obj>>>>,
    dicts: Vec<Weak<RefCell<Dict>>>,
}

impl Drop for Interp {
    fn drop(&mut self) {
        let mut work: Vec<Obj> = std::mem::take(&mut self.ostack);
        work.extend(self.dstack.drain(..).map(Obj::Dict));
        for d in [&self.systemdict, &self.userdict, &self.fontdir] {
            work.push(Obj::Dict(d.clone()));
        }
        for w in std::mem::take(&mut self.dicts) {
            if let Some(d) = w.upgrade() {
                work.push(Obj::Dict(d));
            }
        }
        for w in std::mem::take(&mut self.arrays) {
            if let Some(buf) = w.upgrade() {
                if let Ok(mut b) = buf.try_borrow_mut() {
                    work.append(&mut b);
                }
            }
        }
        release(work, true);
    }
}

impl Interp {
    fn new(ps: &[u8], ctm: Matrix, page: (f64, f64), limits: ImportLimits, budget: Budget) -> Self {
        let systemdict: DictRef = Rc::default();
        let userdict: DictRef = Rc::default();
        let fontdir: DictRef = Rc::default();
        let main = mem_file(ps.to_vec());
        let mut it = Interp {
            ostack: Vec::new(),
            dstack: vec![systemdict.clone(), userdict.clone()],
            g: GState::new(ctm),
            gstack: Vec::new(),
            systemdict,
            userdict,
            fontdir,
            files: Vec::new(),
            main,
            initial_ctm: ctm,
            page,
            limits,
            budget,
            started: Instant::now(),
            ops: 0,
            vm: ps.len() as u64,
            depth: 0,
            stopped_depth: 0,
            svg: String::new(),
            elements: 0,
            next_clip: 0,
            painted: false,
            text_drawn: false,
            unknown: BTreeSet::new(),
            skipped: BTreeSet::new(),
            type1_skipped: 0,
            errors: 0,
            first_error: None,
            rand: 1,
            arrays: Vec::new(),
            dicts: Vec::new(),
        };
        for d in [
            it.systemdict.clone(),
            it.userdict.clone(),
            it.fontdir.clone(),
        ] {
            it.track_dict(&d);
        }
        it.install_systemdict();
        it
    }

    /// Remember a new dictionary for the teardown (see [`Interp::dicts`]).
    fn track_dict(&mut self, d: &DictRef) {
        if self.dicts.len() == self.dicts.capacity() && self.dicts.len() >= 4096 {
            self.dicts.retain(|w| w.strong_count() > 0);
        }
        self.dicts.push(Rc::downgrade(d));
    }

    /// Remember a new array for the teardown (see [`Interp::arrays`]).
    fn track_arr(&mut self, buf: &Rc<RefCell<Vec<Obj>>>) {
        if self.arrays.len() == self.arrays.capacity() && self.arrays.len() >= 4096 {
            self.arrays.retain(|w| w.strong_count() > 0);
        }
        self.arrays.push(Rc::downgrade(buf));
    }

    /// A new, empty dictionary, remembered for the teardown.
    fn new_dict(&mut self) -> DictRef {
        let d: DictRef = Rc::default();
        self.track_dict(&d);
        d
    }

    fn install_systemdict(&mut self) {
        let extra: Vec<DictRef> = (0..4).map(|_| self.new_dict()).collect();
        let encodings: Vec<Rc<RefCell<Vec<Obj>>>> = (0..2)
            .map(|_| {
                let buf = Rc::new(RefCell::new(vec![Obj::Name(".notdef".into(), false); 256]));
                self.track_arr(&buf);
                buf
            })
            .collect();
        let systemdict = self.systemdict.clone();
        let mut sd = systemdict.borrow_mut();
        for b in BUILTINS {
            sd.put(Key::Name(b.name.into()), Obj::Op(b));
        }
        let named = |n: &str| Key::Name(n.into());
        sd.put(named("systemdict"), Obj::Dict(self.systemdict.clone()));
        sd.put(named("userdict"), Obj::Dict(self.userdict.clone()));
        sd.put(named("globaldict"), Obj::Dict(self.userdict.clone()));
        sd.put(named("FontDirectory"), Obj::Dict(self.fontdir.clone()));
        sd.put(
            named("GlobalFontDirectory"),
            Obj::Dict(self.fontdir.clone()),
        );
        for (d, dict) in ["statusdict", "errordict", "$error", "serverdict"]
            .into_iter()
            .zip(extra)
        {
            sd.put(named(d), Obj::Dict(dict));
        }
        let encoding = |buf: &Rc<RefCell<Vec<Obj>>>| {
            Obj::Arr(PsArr {
                buf: buf.clone(),
                start: 0,
                len: 256,
                exec: false,
            })
        };
        sd.put(named("StandardEncoding"), encoding(&encodings[0]));
        sd.put(named("ISOLatin1Encoding"), encoding(&encodings[1]));
        sd.put(named("true"), Obj::Bool(true));
        sd.put(named("false"), Obj::Bool(false));
        sd.put(named("null"), Obj::Null);
    }

    fn note(&self) -> String {
        let mut note = "EPS: the PostScript artwork, drawn by the built-in PostScript interpreter at one pixel per point".to_string();
        if self.text_drawn {
            note.push_str("; its text is set in a fallback system font with estimated spacing");
        }
        let mut not: Vec<String> = Vec::new();
        if self.type1_skipped > 0 {
            not.push(format!(
                "{} embedded Type 1 font(s) (skipped; the fallback font draws their text)",
                self.type1_skipped
            ));
        }
        for s in &self.skipped {
            not.push((*s).to_string());
        }
        if !self.unknown.is_empty() {
            let names: Vec<&str> = self.unknown.iter().take(12).map(String::as_str).collect();
            let more = self.unknown.len().saturating_sub(12);
            not.push(format!(
                "unknown operator(s) {}{}",
                names.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            ));
        }
        if self.errors > 0 {
            not.push(format!(
                "{} PostScript error(s) recovered from (first: {})",
                self.errors,
                self.first_error.as_deref().unwrap_or("?")
            ));
        }
        if !not.is_empty() {
            let _ = write!(note, "; not drawn: {}", not.join("; "));
        }
        note
    }

    // ---- budgets

    fn tick(&mut self) -> Res {
        self.ops += 1;
        if self.ops > self.budget.max_ops {
            return Err(Flow::Fatal(Stop::Budget(format!(
                "ran past its operation budget ({} operations)",
                self.budget.max_ops
            ))));
        }
        if self.ops & 0x3FF == 0 && self.started.elapsed() > self.budget.time {
            return Err(Flow::Fatal(Stop::Budget(format!(
                "ran past its time budget ({} s)",
                self.budget.time.as_secs_f64()
            ))));
        }
        if self.ostack.len() > MAX_OPERAND_STACK {
            self.ostack.truncate(MAX_OPERAND_STACK);
            return Err(err("stackoverflow", "the operand stack is full"));
        }
        Ok(())
    }

    fn alloc(&mut self, bytes: u64) -> Res {
        self.vm = self.vm.saturating_add(bytes);
        if self.vm > self.budget.max_vm_bytes || self.vm > self.limits.max_alloc_bytes.max(64 << 20)
        {
            return Err(Flow::Fatal(Stop::Budget(format!(
                "ran past its memory budget ({} bytes)",
                self.budget
                    .max_vm_bytes
                    .min(self.limits.max_alloc_bytes.max(64 << 20))
            ))));
        }
        Ok(())
    }

    fn new_str(&mut self, bytes: Vec<u8>) -> Res<Obj> {
        self.alloc(bytes.len() as u64 + 32)?;
        let len = bytes.len();
        Ok(Obj::Str(PsStr {
            buf: Rc::new(RefCell::new(bytes)),
            start: 0,
            len,
            exec: false,
        }))
    }

    fn new_arr(&mut self, items: Vec<Obj>, exec: bool) -> Res<Obj> {
        self.alloc(items.len() as u64 * 40 + 32)?;
        let len = items.len();
        let buf = Rc::new(RefCell::new(items));
        self.track_arr(&buf);
        Ok(Obj::Arr(PsArr {
            buf,
            start: 0,
            len,
            exec,
        }))
    }

    fn matrix_obj(&mut self, m: Matrix) -> Res<Obj> {
        self.new_arr(m.iter().map(|v| Obj::Real(*v)).collect(), false)
    }

    // ---- stack helpers

    fn push(&mut self, o: Obj) {
        self.ostack.push(o);
    }

    fn pop(&mut self) -> Res<Obj> {
        self.ostack
            .pop()
            .ok_or_else(|| err("stackunderflow", "the operand stack is empty"))
    }

    fn top(&self) -> Option<&Obj> {
        self.ostack.last()
    }

    fn pop_num(&mut self) -> Res<f64> {
        match self.pop()? {
            Obj::Int(i) => Ok(i as f64),
            Obj::Real(r) => Ok(r),
            _ => Err(err("typecheck", "a number was expected")),
        }
    }

    fn pop_int(&mut self) -> Res<i64> {
        match self.pop()? {
            Obj::Int(i) => Ok(i),
            Obj::Real(r) if r == r.trunc() && r.abs() < 2.2e9 => Ok(r as i64),
            _ => Err(err("typecheck", "an integer was expected")),
        }
    }

    fn pop_bool(&mut self) -> Res<bool> {
        match self.pop()? {
            Obj::Bool(b) => Ok(b),
            _ => Err(err("typecheck", "a boolean was expected")),
        }
    }

    fn pop_dict(&mut self) -> Res<DictRef> {
        match self.pop()? {
            Obj::Dict(d) => Ok(d),
            _ => Err(err("typecheck", "a dictionary was expected")),
        }
    }

    fn pop_arr(&mut self) -> Res<PsArr> {
        match self.pop()? {
            Obj::Arr(a) => Ok(a),
            _ => Err(err("typecheck", "an array was expected")),
        }
    }

    fn pop_str(&mut self) -> Res<PsStr> {
        match self.pop()? {
            Obj::Str(s) => Ok(s),
            _ => Err(err("typecheck", "a string was expected")),
        }
    }

    fn pop_proc(&mut self) -> Res<Obj> {
        let o = self.pop()?;
        match &o {
            Obj::Arr(_) | Obj::Op(_) | Obj::Name(..) | Obj::Str(_) => Ok(o),
            _ => Err(err("typecheck", "a procedure was expected")),
        }
    }

    fn pop_matrix(&mut self) -> Res<Matrix> {
        let a = self.pop_arr()?;
        arr_matrix(&a)
    }

    fn pop_point(&mut self) -> Res<(f64, f64)> {
        let y = self.pop_num()?;
        let x = self.pop_num()?;
        if !(x.is_finite() && y.is_finite()) {
            return Err(err("undefinedresult", "a coordinate is not finite"));
        }
        Ok((x, y))
    }

    fn key(&self, o: &Obj) -> Res<Key> {
        Ok(match o {
            Obj::Name(n, _) => Key::Name(n.clone()),
            Obj::Str(s) => Key::Name(String::from_utf8_lossy(&s.bytes()).as_ref().into()),
            Obj::Int(i) => Key::Int(*i),
            Obj::Real(r) if *r == r.trunc() && r.abs() < 2.2e9 => Key::Int(*r as i64),
            Obj::Real(r) => Key::Real(r.to_bits()),
            Obj::Bool(b) => Key::Bool(*b),
            Obj::Dict(d) => Key::Ptr(Rc::as_ptr(d) as *const u8 as usize),
            Obj::Arr(a) => Key::Ptr(Rc::as_ptr(&a.buf) as *const u8 as usize),
            Obj::Op(b) => Key::Name(b.name.into()),
            Obj::File(f) => Key::Ptr(Rc::as_ptr(f) as *const u8 as usize),
            Obj::Null | Obj::Mark | Obj::Save(_) => {
                return Err(err("typecheck", "not a dictionary key"))
            }
        })
    }

    fn lookup(&self, k: &Key) -> Option<Obj> {
        self.dstack.iter().rev().find_map(|d| d.borrow().get(k))
    }

    fn def(&mut self, k: Key, v: Obj) -> Res {
        let d = self
            .dstack
            .last()
            .cloned()
            .unwrap_or_else(|| self.userdict.clone());
        if d.borrow_mut().put(k, v) {
            self.alloc(64)?;
        }
        Ok(())
    }

    // ---- execution

    fn run_main(&mut self) -> Result<(), Stop> {
        let main = self.main.clone();
        self.files.push(main.clone());
        let result = loop {
            match self.run_file(&main) {
                Ok(()) | Err(Flow::Quit) => break Ok(()),
                Err(Flow::Fatal(stop)) => break Err(stop),
                Err(Flow::Exit) | Err(Flow::Stop) => {}
                Err(Flow::Error(name, cmd)) => {
                    self.record_error(name, &cmd);
                    if self.errors > MAX_ERRORS {
                        break Err(Stop::Malformed(format!(
                            "more than {MAX_ERRORS} errors (first: {})",
                            self.first_error.as_deref().unwrap_or("?")
                        )));
                    }
                }
            }
        };
        self.files.pop();
        result
    }

    fn record_error(&mut self, name: &str, cmd: &str) {
        self.errors += 1;
        if self.first_error.is_none() {
            self.first_error = Some(format!("{name} in {cmd}"));
        }
    }

    /// Read and run `file`'s tokens to its end. At the top level an error
    /// returns to [`Self::run_main`], which records it and carries on with
    /// the next token.
    fn run_file(&mut self, file: &FileRef) -> Res {
        loop {
            let Some(o) = self.read_object(file, 0)? else {
                return Ok(());
            };
            self.tick()?;
            match o {
                Obj::Name(n, true) => self.exec_name(&n)?,
                other => self.push(other),
            }
        }
    }

    /// The next object from `file`: a whole procedure for `{`.
    fn read_object(&mut self, file: &FileRef, nest: usize) -> Res<Option<Obj>> {
        let tok = {
            let mut f = file.borrow_mut();
            let Src::Mem { data, pos } = &mut f.src else {
                return Err(err("ioerror", "only a program or a string can be run"));
            };
            let data = data.clone();
            scan_token(&data, pos).map_err(|e| Flow::Fatal(Stop::Malformed(e)))?
        };
        let Some(tok) = tok else {
            return Ok(None);
        };
        Ok(Some(match tok {
            Tok::Int(i) => Obj::Int(i),
            Tok::Real(r) => Obj::Real(r),
            Tok::Str(s) => self.new_str(s)?,
            Tok::Name(n, NameKind::Exec) => Obj::Name(n.into(), true),
            Tok::Name(n, NameKind::Literal) => Obj::Name(n.into(), false),
            Tok::Name(n, NameKind::Immediate) => {
                let n: Rc<str> = n.into();
                match self.lookup(&Key::Name(n.clone())) {
                    Some(v) => v,
                    None => {
                        self.unknown.insert(n.to_string());
                        Obj::Name(n, true)
                    }
                }
            }
            Tok::ProcClose => {
                return Err(Flow::Fatal(Stop::Malformed("an unbalanced '}'".into())));
            }
            Tok::ProcOpen => {
                if nest >= MAX_PROC_NESTING {
                    return Err(Flow::Fatal(Stop::Malformed(format!(
                        "procedures nest deeper than {MAX_PROC_NESTING}"
                    ))));
                }
                let mut items = Vec::new();
                loop {
                    // A `}` ends this procedure; anything else is an element.
                    let close = {
                        let mut f = file.borrow_mut();
                        let Src::Mem { data, pos } = &mut f.src else {
                            return Err(err("ioerror", "only a program can be run"));
                        };
                        let mut p = *pos;
                        let data = data.clone();
                        match scan_token(&data, &mut p) {
                            Ok(Some(Tok::ProcClose)) => {
                                *pos = p;
                                true
                            }
                            Ok(None) => {
                                return Err(Flow::Fatal(Stop::Malformed(
                                    "an unterminated procedure".into(),
                                )))
                            }
                            _ => false,
                        }
                    };
                    if close {
                        break;
                    }
                    match self.read_object(file, nest + 1)? {
                        Some(o) => items.push(o),
                        None => {
                            return Err(Flow::Fatal(Stop::Malformed(
                                "an unterminated procedure".into(),
                            )))
                        }
                    }
                }
                self.new_arr(items, true)?
            }
        }))
    }

    fn exec_name(&mut self, n: &Rc<str>) -> Res {
        match self.lookup(&Key::Name(n.clone())) {
            Some(v) => self.exec_value(v),
            None => {
                self.unknown.insert(n.to_string());
                if self.stopped_depth > 0 {
                    Err(err("undefined", n.to_string()))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Run a value found by name, or given to `exec`.
    fn exec_value(&mut self, v: Obj) -> Res {
        match v {
            Obj::Op(b) => (b.f)(self).map_err(|e| match e {
                Flow::Error(name, cmd) if cmd.is_empty() => Flow::Error(name, b.name.into()),
                other => other,
            }),
            Obj::Arr(a) if a.exec => self.run_proc(&a),
            Obj::Name(n, true) => {
                self.enter()?;
                let r = self.exec_name(&n);
                self.depth -= 1;
                r
            }
            Obj::Str(s) if s.exec => {
                self.enter()?;
                let f = mem_file(s.bytes());
                self.files.push(f.clone());
                let r = self.run_file(&f).map_err(syntax_in_string);
                self.files.pop();
                self.depth -= 1;
                r
            }
            other => {
                self.push(other);
                Ok(())
            }
        }
    }

    fn enter(&mut self) -> Res {
        if self.depth >= MAX_EXEC_DEPTH {
            return Err(err(
                "execstackoverflow",
                format!("procedures nest deeper than {MAX_EXEC_DEPTH}"),
            ));
        }
        self.depth += 1;
        Ok(())
    }

    fn run_proc(&mut self, a: &PsArr) -> Res {
        self.enter()?;
        let r = (|| {
            for i in 0..a.len {
                self.tick()?;
                let Some(o) = a.get(i) else { break };
                match o {
                    Obj::Name(n, true) => self.exec_name(&n)?,
                    Obj::Op(b) => self.exec_value(Obj::Op(b))?,
                    other => self.push(other),
                }
            }
            Ok(())
        })();
        self.depth -= 1;
        r
    }

    /// Run a procedure operand (`if`, `repeat`, ...).
    fn call(&mut self, p: &Obj) -> Res {
        match p {
            Obj::Arr(a) => self.run_proc(a),
            other => self.exec_value(other.clone()),
        }
    }

    // ---- files

    fn read_byte(&mut self, f: &FileRef) -> Res<Option<u8>> {
        loop {
            {
                let mut st = f.borrow_mut();
                if let Some(b) = st.back.pop_front() {
                    return Ok(Some(b));
                }
                if st.closed {
                    return Ok(None);
                }
                match &mut st.src {
                    Src::Mem { data, pos } => {
                        let b = data.get(*pos).copied();
                        if b.is_some() {
                            *pos += 1;
                        }
                        return Ok(b);
                    }
                    Src::Repeat { data, pos } => {
                        if data.is_empty() {
                            return Ok(None);
                        }
                        let b = data[*pos % data.len()];
                        *pos = (*pos + 1) % data.len();
                        return Ok(Some(b));
                    }
                    Src::Proc { out, done, .. } => {
                        if let Some(b) = out.pop_front() {
                            return Ok(Some(b));
                        }
                        if *done {
                            return Ok(None);
                        }
                    }
                    Src::Filter { out, eod, .. } => {
                        if let Some(b) = out.pop_front() {
                            return Ok(Some(b));
                        }
                        if *eod {
                            return Ok(None);
                        }
                    }
                }
            }
            self.refill(f)?;
        }
    }

    fn read_bytes(&mut self, f: &FileRef, n: usize) -> Res<Vec<u8>> {
        let mut out = Vec::with_capacity(n.min(1 << 20));
        // A fast path for bytes in memory.
        {
            let mut st = f.borrow_mut();
            if st.back.is_empty() && !st.closed {
                if let Src::Mem { data, pos } = &mut st.src {
                    let end = pos.saturating_add(n).min(data.len());
                    out.extend_from_slice(data.get(*pos..end).unwrap_or(&[]));
                    *pos = end;
                    return Ok(out);
                }
            }
        }
        while out.len() < n {
            match self.read_byte(f)? {
                Some(b) => out.push(b),
                None => break,
            }
        }
        Ok(out)
    }

    fn unread(f: &FileRef, bytes: &[u8]) {
        let mut st = f.borrow_mut();
        for b in bytes.iter().rev() {
            st.back.push_front(*b);
        }
    }

    /// Produce more output for a procedure source or a filter.
    fn refill(&mut self, f: &FileRef) -> Res {
        let proc_ = match &f.borrow().src {
            Src::Proc { proc_, .. } => Some(proc_.clone()),
            _ => None,
        };
        if let Some(p) = proc_ {
            self.tick()?;
            self.call(&p)?;
            let s = match self.pop()? {
                Obj::Str(s) => s.bytes(),
                _ => return Err(err("typecheck", "a data procedure must return a string")),
            };
            let mut st = f.borrow_mut();
            if let Src::Proc { out, done, .. } = &mut st.src {
                if s.is_empty() {
                    *done = true;
                }
                out.extend(s);
            }
            return Ok(());
        }
        let src = match &f.borrow().src {
            Src::Filter { src, .. } => src.clone(),
            _ => return Ok(()),
        };
        let is_flate = matches!(
            &f.borrow().src,
            Src::Filter {
                kind: FilterKind::Flate(_),
                ..
            }
        );
        let is_dct = matches!(
            &f.borrow().src,
            Src::Filter {
                kind: FilterKind::Dct,
                ..
            }
        );
        if is_flate {
            return self.refill_flate(f, &src);
        }
        if is_dct {
            return self.refill_dct(f, &src);
        }
        // Byte-at-a-time decoders: feed one source byte, see what comes out.
        let mut fed = 0usize;
        loop {
            let b = self.read_byte(&src)?;
            fed += 1;
            let mut st = f.borrow_mut();
            let Src::Filter { kind, out, eod, .. } = &mut st.src else {
                return Ok(());
            };
            let Some(b) = b else {
                // The source ended: flush what a decoder holds.
                match kind {
                    FilterKind::Hex { high } => {
                        if let Some(h) = high.take() {
                            out.push_back(h << 4);
                        }
                    }
                    FilterKind::A85 { group } => {
                        let mut v = Vec::new();
                        a85_finish(group, &mut v).map_err(|e| err("ioerror", e))?;
                        out.extend(v);
                    }
                    FilterKind::SubFile { pending, .. } => out.extend(pending.drain(..)),
                    _ => {}
                }
                *eod = true;
                return Ok(());
            };
            match kind {
                FilterKind::Hex { high } => {
                    if b == b'>' {
                        if let Some(h) = high.take() {
                            out.push_back(h << 4);
                        }
                        *eod = true;
                    } else if let Some(v) = hex_val(b) {
                        match high.take() {
                            Some(h) => out.push_back(h << 4 | v),
                            None => *high = Some(v),
                        }
                    } else if !is_ws(b) {
                        return Err(err("ioerror", "a stray byte in ASCIIHexDecode data"));
                    }
                }
                FilterKind::A85 { group } => {
                    if b == b'~' {
                        let mut v = Vec::new();
                        a85_finish(group, &mut v).map_err(|e| err("ioerror", e))?;
                        out.extend(v);
                        *eod = true;
                        drop(st);
                        // The `>` of `~>`.
                        if let Some(c) = self.read_byte(&src)? {
                            if c != b'>' {
                                Self::unread(&src, &[c]);
                            }
                        }
                        return Ok(());
                    }
                    let mut v = Vec::new();
                    a85_push(group, &mut v, b).map_err(|e| err("ioerror", e))?;
                    out.extend(v);
                }
                FilterKind::RunLength => {
                    drop(st);
                    if b == 128 {
                        if let Src::Filter { eod, .. } = &mut f.borrow_mut().src {
                            *eod = true;
                        }
                        return Ok(());
                    }
                    let run = if b < 128 {
                        self.read_bytes(&src, usize::from(b) + 1)?
                    } else {
                        let v = self.read_byte(&src)?.unwrap_or(0);
                        vec![v; 257 - usize::from(b)]
                    };
                    self.alloc(run.len() as u64)?;
                    if let Src::Filter { out, .. } = &mut f.borrow_mut().src {
                        out.extend(run);
                    }
                    return Ok(());
                }
                FilterKind::SubFile {
                    count,
                    eod: marker,
                    pending,
                    left,
                } => {
                    if marker.is_empty() {
                        out.push_back(b);
                        *left -= 1;
                        if *left <= 0 {
                            *eod = true;
                        }
                    } else {
                        pending.push(b);
                        if marker.starts_with(pending) {
                            if pending.len() == marker.len() {
                                *count -= 1;
                                if *count <= 0 {
                                    *eod = true;
                                    pending.clear();
                                } else {
                                    out.extend(pending.drain(..));
                                }
                            }
                        } else {
                            // Emit up to the longest suffix that is still
                            // a prefix of the marker.
                            let mut keep = pending.len();
                            while keep > 0 && !marker.starts_with(&pending[pending.len() - keep..])
                            {
                                keep -= 1;
                            }
                            let emit = pending.len() - keep;
                            out.extend(pending.drain(..emit));
                        }
                    }
                }
                FilterKind::Flate(_) | FilterKind::Dct => {}
            }
            if !out.is_empty() || *eod {
                let n = out.len() as u64;
                drop(st);
                return self.alloc(n);
            }
            if fed > (1 << 26) {
                return Err(err("ioerror", "a filter produced nothing"));
            }
        }
    }

    fn refill_flate(&mut self, f: &FileRef, src: &FileRef) -> Res {
        let input = self.read_bytes(src, 4096)?;
        let mut st = f.borrow_mut();
        let Src::Filter {
            kind: FilterKind::Flate(z),
            out,
            eod,
            ..
        } = &mut st.src
        else {
            return Ok(());
        };
        let mut buf = vec![0u8; 64 * 1024];
        let before_in = z.total_in();
        let before_out = z.total_out();
        let status = z
            .decompress(&input, &mut buf, flate2::FlushDecompress::None)
            .map_err(|_| err("ioerror", "damaged FlateDecode data"))?;
        let used = (z.total_in() - before_in) as usize;
        let made = (z.total_out() - before_out) as usize;
        out.extend(buf.get(..made).unwrap_or(&[]));
        let ended = status == flate2::Status::StreamEnd || (input.is_empty() && made == 0);
        if ended {
            *eod = true;
        }
        drop(st);
        if used < input.len() {
            Self::unread(src, &input[used..]);
        }
        if !ended && made == 0 && used == 0 {
            if let Src::Filter { eod, .. } = &mut f.borrow_mut().src {
                *eod = true;
            }
        }
        self.alloc(made as u64)
    }

    fn refill_dct(&mut self, f: &FileRef, src: &FileRef) -> Res {
        // Collect the whole JPEG stream (to its EOI marker), then decode it.
        let mut data = Vec::new();
        let mut prev = 0u8;
        while let Some(b) = self.read_byte(src)? {
            data.push(b);
            if data.len().is_multiple_of(4096) {
                self.alloc(4096)?;
            }
            if prev == 0xFF && b == 0xD9 {
                break;
            }
            prev = b;
        }
        let ncomp = jpeg_components(&data).unwrap_or(3);
        let decoded = decode_surface_bytes_as(&data, self.limits, ImportFormat::Jpeg)
            .map_err(|e| err("ioerror", format!("DCTDecode: {e}")))?;
        let SurfacePixels::Rgba8(px) = decoded.pixels else {
            return Err(err("ioerror", "DCTDecode: an unexpected sample depth"));
        };
        let mut bytes = Vec::with_capacity(px.len() / 4 * ncomp);
        for p in px.as_chunks::<4>().0 {
            match ncomp {
                1 => bytes.push(p[0]),
                4 => bytes.extend_from_slice(&[255 - p[0], 255 - p[1], 255 - p[2], 0]),
                _ => bytes.extend_from_slice(&p[..3]),
            }
        }
        self.alloc(bytes.len() as u64)?;
        if let Src::Filter { out, eod, .. } = &mut f.borrow_mut().src {
            out.extend(bytes);
            *eod = true;
        }
        Ok(())
    }

    /// A data source operand as a file.
    fn source_file(&mut self, o: Obj) -> Res<FileRef> {
        Ok(match o {
            Obj::File(f) => f,
            Obj::Str(s) => new_file(Src::Repeat {
                data: s.bytes(),
                pos: 0,
            }),
            p @ (Obj::Arr(_) | Obj::Op(_) | Obj::Name(..)) => new_file(Src::Proc {
                proc_: p,
                out: VecDeque::new(),
                done: false,
            }),
            _ => return Err(err("typecheck", "not a data source")),
        })
    }

    // ---- painting

    fn room(&mut self, bytes: usize) -> Res {
        self.elements += 1;
        if self.elements > MAX_SVG_ELEMENTS || self.svg.len() + bytes > MAX_SVG_BYTES {
            return Err(Flow::Fatal(Stop::Budget(format!(
                "draws more than the interpreter's output bound ({MAX_SVG_ELEMENTS} elements, {MAX_SVG_BYTES} bytes)"
            ))));
        }
        Ok(())
    }

    fn emit(&mut self, el: &str) -> Res {
        self.room(el.len() + self.g.clip.len() * 40)?;
        for id in &self.g.clip {
            let _ = write!(self.svg, "<g clip-path=\"url(#c{id})\">");
        }
        self.svg.push_str(el);
        for _ in &self.g.clip {
            self.svg.push_str("</g>");
        }
        self.svg.push('\n');
        self.painted = true;
        Ok(())
    }

    fn path_d(segs: &[Seg], m: Option<&Matrix>) -> String {
        let t = |x: f64, y: f64| match m {
            Some(m) => apply(m, x, y),
            None => (x, y),
        };
        let mut d = String::new();
        for s in segs {
            match *s {
                Seg::M(x, y) => {
                    let (x, y) = t(x, y);
                    let _ = write!(d, "M{} {}", num(x), num(y));
                }
                Seg::L(x, y) => {
                    let (x, y) = t(x, y);
                    let _ = write!(d, "L{} {}", num(x), num(y));
                }
                Seg::C(a, b, c, e, x, y) => {
                    let (a, b) = t(a, b);
                    let (c, e) = t(c, e);
                    let (x, y) = t(x, y);
                    let _ = write!(
                        d,
                        "C{} {} {} {} {} {}",
                        num(a),
                        num(b),
                        num(c),
                        num(e),
                        num(x),
                        num(y)
                    );
                }
                Seg::Z => d.push('Z'),
            }
        }
        d
    }

    fn paint_color(&mut self) -> Option<String> {
        match self.g.color {
            Some(c) => Some(hex_color(c)),
            None => {
                self.skipped.insert("pattern fills and strokes");
                None
            }
        }
    }

    fn fill(&mut self, even_odd: bool) -> Res {
        let segs = std::mem::take(&mut self.g.path);
        self.g.cp = None;
        self.fill_segs(&segs, even_odd)
    }

    fn fill_segs(&mut self, segs: &[Seg], even_odd: bool) -> Res {
        if segs.is_empty() {
            return Ok(());
        }
        let Some(color) = self.paint_color() else {
            return Ok(());
        };
        let d = Self::path_d(segs, None);
        let el = format!(
            "<path d=\"{d}\" fill=\"{color}\"{}/>",
            if even_odd {
                " fill-rule=\"evenodd\""
            } else {
                ""
            }
        );
        self.emit(&el)
    }

    fn stroke(&mut self) -> Res {
        let segs = std::mem::take(&mut self.g.path);
        self.g.cp = None;
        self.stroke_segs(&segs)
    }

    fn stroke_segs(&mut self, segs: &[Seg]) -> Res {
        if segs.is_empty() {
            return Ok(());
        }
        let Some(color) = self.paint_color() else {
            return Ok(());
        };
        let ctm = self.g.ctm;
        let Some(inv) = invert(&ctm) else {
            return Ok(());
        };
        let d = Self::path_d(segs, Some(&inv));
        let mut width = self.g.line_width.abs();
        if width == 0.0 {
            // The thinnest line the device draws: one pixel.
            let det = (ctm[0] * ctm[3] - ctm[1] * ctm[2]).abs().sqrt();
            width = if det > 0.0 { 1.0 / det } else { 1.0 };
        }
        let cap = ["butt", "round", "square"][self.g.cap.clamp(0, 2) as usize];
        let join = ["miter", "round", "bevel"][self.g.join.clamp(0, 2) as usize];
        let mut el = format!(
            "<path d=\"{d}\" transform=\"matrix({} {} {} {} {} {})\" fill=\"none\" stroke=\"{color}\" stroke-width=\"{}\" stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\" stroke-miterlimit=\"{}\"",
            num6(ctm[0]),
            num6(ctm[1]),
            num6(ctm[2]),
            num6(ctm[3]),
            num(ctm[4]),
            num(ctm[5]),
            num6(width),
            num(self.g.miter.max(1.0)),
        );
        let (dash, offset) = &self.g.dash;
        if !dash.is_empty()
            && dash.iter().all(|v| v.is_finite() && *v >= 0.0)
            && dash.iter().sum::<f64>() > 0.0
        {
            let list: Vec<String> = dash.iter().map(|v| num6(*v)).collect();
            let _ = write!(
                el,
                " stroke-dasharray=\"{}\" stroke-dashoffset=\"{}\"",
                list.join(" "),
                num6(*offset)
            );
        }
        el.push_str("/>");
        self.emit(&el)
    }

    fn clip(&mut self, even_odd: bool) -> Res {
        let segs = self.g.path.clone();
        self.clip_segs(&segs, even_odd)
    }

    fn clip_segs(&mut self, segs: &[Seg], even_odd: bool) -> Res {
        let id = self.next_clip;
        self.next_clip += 1;
        let d = Self::path_d(segs, None);
        let el = format!(
            "<clipPath id=\"c{id}\" clipPathUnits=\"userSpaceOnUse\"><path d=\"{}\"{}/></clipPath>\n",
            if d.is_empty() { "M0 0Z" } else { &d },
            if even_odd {
                " clip-rule=\"evenodd\""
            } else {
                ""
            }
        );
        self.room(el.len())?;
        self.svg.push_str(&el);
        self.g.clip.push(id);
        Ok(())
    }

    // ---- path construction

    fn add_seg(&mut self, s: Seg) -> Res {
        if self.g.path.len() >= MAX_PATH_POINTS {
            return Err(err(
                "limitcheck",
                format!("a path holds more than {MAX_PATH_POINTS} points"),
            ));
        }
        if self.g.path.len().is_multiple_of(1024) {
            self.alloc(1024 * 56)?;
        }
        self.g.path.push(s);
        Ok(())
    }

    fn moveto_dev(&mut self, x: f64, y: f64) -> Res {
        if let Some(Seg::M(..)) = self.g.path.last() {
            self.g.path.pop();
        }
        self.add_seg(Seg::M(x, y))?;
        self.g.cp = Some((x, y));
        self.g.start = Some((x, y));
        Ok(())
    }

    fn lineto_dev(&mut self, x: f64, y: f64) -> Res {
        if self.g.cp.is_none() {
            return Err(err("nocurrentpoint", ""));
        }
        self.add_seg(Seg::L(x, y))?;
        self.g.cp = Some((x, y));
        Ok(())
    }

    fn curveto_user(&mut self, p: [(f64, f64); 3]) -> Res {
        if self.g.cp.is_none() {
            return Err(err("nocurrentpoint", ""));
        }
        let ctm = self.g.ctm;
        let (a, b) = apply(&ctm, p[0].0, p[0].1);
        let (c, d) = apply(&ctm, p[1].0, p[1].1);
        let (x, y) = apply(&ctm, p[2].0, p[2].1);
        self.add_seg(Seg::C(a, b, c, d, x, y))?;
        self.g.cp = Some((x, y));
        Ok(())
    }

    fn current_user(&self) -> Res<(f64, f64)> {
        let (x, y) = self.g.cp.ok_or_else(|| err("nocurrentpoint", ""))?;
        let inv = invert(&self.g.ctm).ok_or_else(|| err("undefinedresult", "a singular matrix"))?;
        Ok(apply(&inv, x, y))
    }

    fn arc(&mut self, cx: f64, cy: f64, r: f64, a1: f64, a2: f64, ccw: bool) -> Res {
        let (mut a1, mut a2) = (a1, a2);
        if !(cx.is_finite() && cy.is_finite() && r.is_finite() && a1.is_finite() && a2.is_finite())
        {
            return Err(err("undefinedresult", "an arc is not finite"));
        }
        a1 = a1.clamp(-1e7, 1e7);
        a2 = a2.clamp(-1e7, 1e7);
        if ccw {
            while a2 < a1 {
                a2 += 360.0;
            }
        } else {
            while a2 > a1 {
                a2 -= 360.0;
            }
        }
        let start = (
            cx + r * a1.to_radians().cos(),
            cy + r * a1.to_radians().sin(),
        );
        let (sx, sy) = apply(&self.g.ctm, start.0, start.1);
        if self.g.cp.is_some() {
            self.lineto_dev(sx, sy)?;
        } else {
            self.moveto_dev(sx, sy)?;
        }
        let sweep = a2 - a1;
        let n = (sweep.abs() / 90.0).ceil().clamp(1.0, 4096.0) as usize;
        let step = (sweep / n as f64).to_radians();
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let mut a = a1.to_radians();
        for _ in 0..n {
            let b = a + step;
            let (ca, sa, cb, sb) = (a.cos(), a.sin(), b.cos(), b.sin());
            self.curveto_user([
                (cx + r * (ca - k * sa), cy + r * (sa + k * ca)),
                (cx + r * (cb + k * sb), cy + r * (sb - k * cb)),
                (cx + r * cb, cy + r * sb),
            ])?;
            a = b;
        }
        Ok(())
    }

    /// `arct` / `arcto`: returns the two tangent points.
    fn arct(&mut self, p1: (f64, f64), p2: (f64, f64), r: f64) -> Res<[f64; 4]> {
        let p0 = self.current_user()?;
        let (v1x, v1y) = (p0.0 - p1.0, p0.1 - p1.1);
        let (v2x, v2y) = (p2.0 - p1.0, p2.1 - p1.1);
        let (l1, l2) = (v1x.hypot(v1y), v2x.hypot(v2y));
        let cross = v1x * v2y - v1y * v2x;
        if l1 == 0.0 || l2 == 0.0 || cross.abs() < 1e-12 || r == 0.0 {
            let (x, y) = apply(&self.g.ctm, p1.0, p1.1);
            self.lineto_dev(x, y)?;
            return Ok([p1.0, p1.1, p1.0, p1.1]);
        }
        let (u1, u2) = ((v1x / l1, v1y / l1), (v2x / l2, v2y / l2));
        let cos = (u1.0 * u2.0 + u1.1 * u2.1).clamp(-1.0, 1.0);
        let theta = cos.acos();
        let dist = r / (theta / 2.0).tan();
        let t1 = (p1.0 + u1.0 * dist, p1.1 + u1.1 * dist);
        let t2 = (p1.0 + u2.0 * dist, p1.1 + u2.1 * dist);
        let (x, y) = apply(&self.g.ctm, t1.0, t1.1);
        self.lineto_dev(x, y)?;
        let phi = std::f64::consts::PI - theta;
        let k = 4.0 / 3.0 * (phi / 4.0).tan() * r;
        self.curveto_user([
            (t1.0 - u1.0 * k, t1.1 - u1.1 * k),
            (t2.0 - u2.0 * k, t2.1 - u2.1 * k),
            t2,
        ])?;
        Ok([t1.0, t1.1, t2.0, t2.1])
    }

    fn rect_segs(&self, x: f64, y: f64, w: f64, h: f64) -> Vec<Seg> {
        let m = &self.g.ctm;
        let p = [
            apply(m, x, y),
            apply(m, x + w, y),
            apply(m, x + w, y + h),
            apply(m, x, y + h),
        ];
        vec![
            Seg::M(p[0].0, p[0].1),
            Seg::L(p[1].0, p[1].1),
            Seg::L(p[2].0, p[2].1),
            Seg::L(p[3].0, p[3].1),
            Seg::Z,
        ]
    }

    /// The rectangles of `rectfill` / `rectstroke` / `rectclip`: four
    /// numbers, or an array of them.
    fn pop_rects(&mut self) -> Res<Vec<[f64; 4]>> {
        if let Some(Obj::Arr(_)) = self.top() {
            let a = self.pop_arr()?;
            let v: Vec<f64> = a.items().iter().filter_map(obj_num).collect();
            return Ok(v.as_chunks::<4>().0.to_vec());
        }
        let h = self.pop_num()?;
        let w = self.pop_num()?;
        let y = self.pop_num()?;
        let x = self.pop_num()?;
        Ok(vec![[x, y, w, h]])
    }

    // ---- colour

    fn set_comps(&mut self, comps: Vec<f64>) -> Res {
        let cs = self.g.cspace.clone();
        self.g.color = self.rgb_of(&cs, &comps)?;
        self.g.comps = comps;
        Ok(())
    }

    fn rgb_of(&mut self, cs: &CSpace, c: &[f64]) -> Res<Option<[f64; 3]>> {
        let at = |i: usize| c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        Ok(Some(match cs {
            CSpace::Gray => [at(0); 3],
            CSpace::Rgb => [at(0), at(1), at(2)],
            CSpace::Cmyk => cmyk_rgb(at(0), at(1), at(2), at(3)),
            CSpace::Pattern => return Ok(None),
            CSpace::Indexed {
                base,
                hival,
                lookup,
            } => {
                let i = c
                    .first()
                    .copied()
                    .unwrap_or(0.0)
                    .round()
                    .clamp(0.0, *hival as f64) as usize;
                let n = base.ncomp();
                let v: Vec<f64> = (0..n)
                    .map(|k| f64::from(*lookup.get(i * n + k).unwrap_or(&0)) / 255.0)
                    .collect();
                let base = (**base).clone();
                return self.rgb_of(&base, &v);
            }
            CSpace::Tint { alt, proc_, .. } => {
                let depth = self.ostack.len();
                for v in c {
                    self.push(Obj::Real(*v));
                }
                self.call(proc_)?;
                let n = alt.ncomp();
                if self.ostack.len() < depth + n {
                    return Err(err("rangecheck", "a tint transform returned too little"));
                }
                let out: Vec<f64> = self
                    .ostack
                    .split_off(self.ostack.len() - n)
                    .iter()
                    .map(|o| obj_num(o).unwrap_or(0.0))
                    .collect();
                self.ostack.truncate(depth);
                let alt = (**alt).clone();
                return self.rgb_of(&alt, &out);
            }
        }))
    }

    fn parse_cspace(&mut self, o: &Obj) -> Res<CSpace> {
        self.parse_cspace_at(o, 0)
    }

    fn parse_cspace_at(&mut self, o: &Obj, nest: usize) -> Res<CSpace> {
        if nest > MAX_CSPACE_NESTING {
            return Err(err(
                "limitcheck",
                format!("colour spaces nested more than {MAX_CSPACE_NESTING} deep"),
            ));
        }
        self.tick()?;
        let (family, arr) = match o {
            Obj::Name(n, _) => (n.to_string(), None),
            Obj::Arr(a) => match a.get(0) {
                Some(Obj::Name(n, _)) => (n.to_string(), Some(a.clone())),
                _ => return Err(err("typecheck", "a colour space array has no family")),
            },
            _ => return Err(err("typecheck", "not a colour space")),
        };
        let arg = |i: usize| arr.as_ref().and_then(|a| a.get(i));
        Ok(match family.as_str() {
            "DeviceGray" | "CIEBasedA" | "CalGray" | "G" => CSpace::Gray,
            "DeviceRGB" | "CIEBasedABC" | "CalRGB" | "Lab" | "CIEBasedDEF" | "RGB" => CSpace::Rgb,
            "DeviceCMYK" | "CIEBasedDEFG" | "CMYK" => CSpace::Cmyk,
            "Pattern" => CSpace::Pattern,
            "ICCBased" => {
                let n = match arg(1) {
                    Some(Obj::Dict(d)) => d
                        .borrow()
                        .get(&Key::Name("N".into()))
                        .and_then(|v| obj_num(&v))
                        .unwrap_or(3.0),
                    _ => 3.0,
                };
                match n as i64 {
                    1 => CSpace::Gray,
                    4 => CSpace::Cmyk,
                    _ => CSpace::Rgb,
                }
            }
            "Indexed" | "I" => {
                let base = self.parse_cspace_at(
                    &arg(1).ok_or_else(|| err("rangecheck", "Indexed"))?,
                    nest + 1,
                )?;
                let hival = arg(2)
                    .and_then(|v| obj_num(&v))
                    .unwrap_or(0.0)
                    .clamp(0.0, 4095.0) as i64;
                let lookup = match arg(3) {
                    Some(Obj::Str(s)) => s.bytes(),
                    _ => {
                        self.skipped
                            .insert("an Indexed colour table given as a procedure");
                        Vec::new()
                    }
                };
                CSpace::Indexed {
                    base: Box::new(base),
                    hival,
                    lookup: Rc::new(lookup),
                }
            }
            "Separation" | "DeviceN" => {
                let n = if family == "DeviceN" {
                    match arg(1) {
                        Some(Obj::Arr(a)) => a.len.max(1),
                        _ => 1,
                    }
                } else {
                    1
                };
                let alt = self.parse_cspace_at(
                    &arg(2).ok_or_else(|| err("rangecheck", "Separation"))?,
                    nest + 1,
                )?;
                let proc_ = arg(3).ok_or_else(|| err("rangecheck", "Separation"))?;
                CSpace::Tint {
                    n,
                    alt: Box::new(alt),
                    proc_,
                }
            }
            _ => {
                self.skipped
                    .insert("an unknown colour space (drawn as grey)");
                CSpace::Gray
            }
        })
    }

    // ---- text

    fn font(&self) -> Res<Font> {
        self.g
            .font
            .clone()
            .ok_or_else(|| err("invalidfont", "no current font"))
    }

    /// Draw `bytes` at the current point; `extra` is added (in user space)
    /// after each character, `per` gives each character's own advance.
    fn show(&mut self, bytes: &[u8], extra: (f64, f64), per: Option<&[(f64, f64)]>) -> Res {
        let chars: Vec<char> = bytes.iter().map(|b| char::from(*b)).collect();
        self.show_chars(&chars, extra, per)
    }

    fn show_chars(&mut self, chars: &[char], extra: (f64, f64), per: Option<&[(f64, f64)]>) -> Res {
        let font = self.font()?;
        let (cx, cy) = self.g.cp.ok_or_else(|| err("nocurrentpoint", ""))?;
        let color = self.paint_color();
        let lin = |m: &Matrix| [m[0], m[1], m[2], m[3], 0.0, 0.0];
        let ctm_lin = lin(&self.g.ctm);
        let flip: Matrix = [1.0, 0.0, 0.0, -1.0, 0.0, 0.0];
        // SVG text is set at 1000 units an em; the font's own glyph space
        // has `units` an em.
        let k = font.units / 1000.0;
        let em: Matrix = [k, 0.0, 0.0, k, 0.0, 0.0];
        let glyph_to_dev = mul(&mul(&mul(&em, &flip), &font.matrix), &ctm_lin);
        let (fam, weight, style) = font_css(&font.name);
        let (adv_ux, adv_uy) = dapply(&font.matrix, font.units * ADVANCE_EM, 0.0);
        let mut pos = (cx, cy);
        let runs: Vec<(Vec<char>, (f64, f64))> = if per.is_some() || extra != (0.0, 0.0) {
            chars.iter().map(|c| (vec![*c], (0.0, 0.0))).collect()
        } else {
            vec![(chars.to_vec(), (0.0, 0.0))]
        };
        for (i, (run, _)) in runs.iter().enumerate() {
            let text = svg_text(run);
            if let (Some(color), false) = (&color, text.trim().is_empty()) {
                let m = [
                    glyph_to_dev[0],
                    glyph_to_dev[1],
                    glyph_to_dev[2],
                    glyph_to_dev[3],
                    pos.0,
                    pos.1,
                ];
                let el = format!(
                    "<text transform=\"matrix({} {} {} {} {} {})\" font-family=\"{fam}\" font-size=\"1000\"{weight}{style} fill=\"{color}\" xml:space=\"preserve\">{text}</text>",
                    num6(m[0]),
                    num6(m[1]),
                    num6(m[2]),
                    num6(m[3]),
                    num(m[4]),
                    num(m[5]),
                );
                self.emit(&el)?;
                self.text_drawn = true;
            }
            let n = run.len() as f64;
            let (ux, uy) = match per.and_then(|p| p.get(i)) {
                Some(d) => *d,
                None => (adv_ux * n + extra.0 * n, adv_uy * n + extra.1 * n),
            };
            let (dx, dy) = dapply(&self.g.ctm, ux, uy);
            pos = (pos.0 + dx, pos.1 + dy);
        }
        self.g.cp = Some(pos);
        Ok(())
    }

    fn make_font(&mut self, name: &str) -> Res<Obj> {
        let d = self.new_dict();
        let fm_buf = Rc::new(RefCell::new(
            [0.001, 0.0, 0.0, 0.001, 0.0, 0.0]
                .iter()
                .map(|v| Obj::Real(*v))
                .collect(),
        ));
        let bbox_buf = Rc::new(RefCell::new(vec![
            Obj::Int(0),
            Obj::Int(-200),
            Obj::Int(1000),
            Obj::Int(900),
        ]));
        self.track_arr(&fm_buf);
        self.track_arr(&bbox_buf);
        {
            let mut b = d.borrow_mut();
            b.put(Key::Name("FontName".into()), Obj::Name(name.into(), false));
            b.put(Key::Name("FontType".into()), Obj::Int(1));
            b.put(
                Key::Name("FontMatrix".into()),
                Obj::Arr(PsArr {
                    buf: fm_buf,
                    start: 0,
                    len: 6,
                    exec: false,
                }),
            );
            let enc = self
                .systemdict
                .borrow()
                .get(&Key::Name("StandardEncoding".into()))
                .unwrap_or(Obj::Null);
            b.put(Key::Name("Encoding".into()), enc);
            b.put(Key::Name("FID".into()), Obj::Int(0));
            b.put(
                Key::Name("FontBBox".into()),
                Obj::Arr(PsArr {
                    buf: bbox_buf,
                    start: 0,
                    len: 4,
                    exec: false,
                }),
            );
        }
        self.alloc(512)?;
        self.fontdir
            .borrow_mut()
            .put(Key::Name(name.into()), Obj::Dict(d.clone()));
        Ok(Obj::Dict(d))
    }

    fn find_font(&mut self, name: &Obj) -> Res<Obj> {
        let key = self.key(name)?;
        if let Some(f) = self.fontdir.borrow().get(&key) {
            return Ok(f);
        }
        let n = match &key {
            Key::Name(n) => n.to_string(),
            _ => "Helvetica".into(),
        };
        self.make_font(&n)
    }

    fn font_of(&self, d: &DictRef) -> Font {
        let b = d.borrow();
        let name = match b.get(&Key::Name("FontName".into())) {
            Some(Obj::Name(n, _)) => n.to_string(),
            Some(Obj::Str(s)) => String::from_utf8_lossy(&s.bytes()).into_owned(),
            _ => "Helvetica".into(),
        };
        let matrix = match b.get(&Key::Name("FontMatrix".into())) {
            Some(Obj::Arr(a)) => arr_matrix(&a).unwrap_or([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]),
            _ => [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
        };
        let base = match b.get(&Key::Name(BASE_MATRIX.into())) {
            Some(Obj::Arr(a)) => arr_matrix(&a).ok(),
            _ => None,
        }
        .unwrap_or(match b.get(&Key::Name("FontType".into())) {
            Some(Obj::Int(42)) => IDENTITY,
            _ => [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
        });
        let det = (base[0] * base[3] - base[1] * base[2]).abs().sqrt();
        let units = if det.is_finite() && det > 1e-9 {
            (1.0 / det).clamp(1e-3, 1e6)
        } else {
            1000.0
        };
        Font {
            name,
            matrix,
            units,
        }
    }

    fn scaled_font(&mut self, d: &DictRef, m: &Matrix) -> Res<Obj> {
        let copy = self.new_dict();
        {
            let src = d.borrow();
            let mut c = copy.borrow_mut();
            for (k, v) in &src.entries {
                c.put(k.clone(), v.clone());
            }
        }
        self.alloc(d.borrow().entries.len() as u64 * 64)?;
        let base = self.font_of(d).matrix;
        let fm = self.matrix_obj(mul(&base, m))?;
        copy.borrow_mut().put(Key::Name("FontMatrix".into()), fm);
        Ok(Obj::Dict(copy))
    }

    // ---- images

    #[allow(clippy::too_many_arguments)]
    fn image(
        &mut self,
        w: i64,
        h: i64,
        bpc: i64,
        matrix: Matrix,
        sources: Vec<Obj>,
        multi: bool,
        cs: CSpace,
        decode: Option<Vec<f64>>,
        mask: Option<bool>,
    ) -> Res {
        if w <= 0 || h <= 0 || w > i64::from(u32::MAX) || h > i64::from(u32::MAX) {
            return Err(err("rangecheck", "an image has no area"));
        }
        if !matches!(bpc, 1 | 2 | 4 | 8 | 12 | 16) {
            return Err(err("rangecheck", format!("{bpc} bits per component")));
        }
        let (w, h) = (w as u32, h as u32);
        let too_big = |e: CodecError| Flow::Fatal(Stop::Budget(format!("has an image that {e}")));
        check_image_dims(self.limits, w, h).map_err(too_big)?;
        let ncomp = if mask.is_some() { 1 } else { cs.ncomp() };
        let bpc = bpc as usize;
        let pixels = u64::from(w) * u64::from(h);
        self.alloc(pixels.saturating_mul(4 + ncomp as u64 * 2))?;
        let per_src_comps = if multi { 1 } else { ncomp };
        let row_bytes = (w as usize * per_src_comps * bpc).div_ceil(8);
        let need = row_bytes * h as usize;
        let files: Vec<FileRef> = sources
            .into_iter()
            .map(|s| self.source_file(s))
            .collect::<Res<_>>()?;
        if files.is_empty() || (multi && files.len() < ncomp) {
            return Err(err("stackunderflow", "an image has too few data sources"));
        }
        let mut planes: Vec<Vec<u8>> = vec![Vec::with_capacity(need); files.len()];
        // Read round-robin, one row at a time, so procedures that share one
        // file (`currentfile ... readhexstring`) see their data in order.
        for _ in 0..h {
            for (i, f) in files.iter().enumerate() {
                let mut row = self.read_bytes(f, row_bytes)?;
                row.resize(row_bytes, 0);
                planes[i].extend_from_slice(&row);
            }
            self.tick()?;
        }
        let max = ((1u32 << bpc) - 1) as f64;
        let dec: Vec<f64> = decode.unwrap_or_else(|| match (&cs, mask) {
            (_, Some(_)) => vec![0.0, 1.0],
            (CSpace::Indexed { .. }, None) => vec![0.0, max],
            _ => (0..ncomp).flat_map(|_| [0.0, 1.0]).collect(),
        });
        let sample = |plane: &[u8], idx: usize| -> u32 {
            match bpc {
                8 => u32::from(*plane.get(idx).unwrap_or(&0)),
                16 => {
                    u32::from(*plane.get(idx * 2).unwrap_or(&0)) << 8
                        | u32::from(*plane.get(idx * 2 + 1).unwrap_or(&0))
                }
                _ => {
                    let bit = idx * bpc;
                    let mut v = 0u32;
                    for k in 0..bpc {
                        let b = bit + k;
                        let byte = *plane.get(b / 8).unwrap_or(&0);
                        v = v << 1 | u32::from((byte >> (7 - b % 8)) & 1);
                    }
                    v
                }
            }
        };
        // A table for spaces a procedure converts, one entry per sample.
        let mut lut: Option<Vec<[f64; 3]>> = None;
        if let (CSpace::Tint { .. } | CSpace::Indexed { .. }, None, true) =
            (&cs, mask, ncomp == 1 && bpc <= 8)
        {
            let mut t = Vec::with_capacity(1 << bpc);
            for v in 0..(1u32 << bpc) {
                let d = dec.first().copied().unwrap_or(0.0)
                    + f64::from(v)
                        * (dec.get(1).copied().unwrap_or(1.0)
                            - dec.first().copied().unwrap_or(0.0))
                        / max;
                t.push(self.rgb_of(&cs, &[d])?.unwrap_or([0.5; 3]));
            }
            lut = Some(t);
        } else if let (CSpace::Tint { .. }, None) = (&cs, mask) {
            self.skipped.insert("a DeviceN image (drawn as grey)");
        }
        let paint = self.g.color.unwrap_or([0.0; 3]);
        let mut rgba = Vec::with_capacity(pixels as usize * 4);
        let wu = w as usize;
        for y in 0..h as usize {
            for x in 0..wu {
                let comp = |c: usize| -> (f64, u32) {
                    let (plane, idx) = if multi {
                        (&planes[c], y * row_bytes * 8 / bpc + x)
                    } else {
                        (&planes[0], y * row_bytes * 8 / bpc + x * ncomp + c)
                    };
                    let v = sample(plane, idx);
                    let d0 = dec.get(c * 2).copied().unwrap_or(0.0);
                    let d1 = dec.get(c * 2 + 1).copied().unwrap_or(1.0);
                    (d0 + f64::from(v) * (d1 - d0) / max, v)
                };
                if let Some(_polarity) = mask {
                    let (v, _) = comp(0);
                    // With Decode [0 1] a 0 sample paints.
                    let on = v < 0.5;
                    let c = hex_bytes(paint);
                    rgba.extend_from_slice(&[c[0], c[1], c[2], if on { 255 } else { 0 }]);
                    continue;
                }
                let rgb = if let Some(t) = &lut {
                    let (_, raw) = comp(0);
                    t.get(raw as usize).copied().unwrap_or([0.0; 3])
                } else {
                    let c: Vec<f64> = (0..ncomp).map(|c| comp(c).0).collect();
                    let at = |i: usize| c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                    match &cs {
                        CSpace::Rgb => [at(0), at(1), at(2)],
                        CSpace::Cmyk => cmyk_rgb(at(0), at(1), at(2), at(3)),
                        _ => [at(0); 3],
                    }
                };
                let c = hex_bytes(rgb);
                rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        let png = encode(ExportFormat::Png, w, h, &rgba)
            .map_err(|e| err("ioerror", format!("an image could not be encoded: {e}")))?;
        let inv =
            invert(&matrix).ok_or_else(|| err("undefinedresult", "a singular image matrix"))?;
        let t = mul(&inv, &self.g.ctm);
        let b64 = super::vector_docs::base64_encode(&png);
        let el = format!(
            "<image x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" preserveAspectRatio=\"none\" image-rendering=\"optimizeSpeed\" transform=\"matrix({} {} {} {} {} {})\" href=\"data:image/png;base64,{b64}\"/>",
            num6(t[0]),
            num6(t[1]),
            num6(t[2]),
            num6(t[3]),
            num(t[4]),
            num(t[5]),
        );
        self.emit(&el)
    }

    /// The Level 2 dictionary form of `image` / `imagemask`.
    fn image_dict(&mut self, d: &DictRef, mask: Option<bool>) -> Res {
        let get = |k: &str| d.borrow().get(&Key::Name(k.into()));
        let int = |k: &str| get(k).and_then(|v| obj_num(&v)).map(|v| v as i64);
        let image_type = int("ImageType").unwrap_or(1);
        if image_type != 1 {
            self.skipped.insert("masked images (ImageType 3 / 4)");
            return Ok(());
        }
        let w = int("Width").ok_or_else(|| err("rangecheck", "an image has no /Width"))?;
        let h = int("Height").ok_or_else(|| err("rangecheck", "an image has no /Height"))?;
        let bpc = if mask.is_some() {
            1
        } else {
            int("BitsPerComponent").unwrap_or(8)
        };
        let matrix = match get("ImageMatrix") {
            Some(Obj::Arr(a)) => arr_matrix(&a)?,
            _ => [w as f64, 0.0, 0.0, -(h as f64), 0.0, h as f64],
        };
        let decode = match get("Decode") {
            Some(Obj::Arr(a)) => Some(a.items().iter().filter_map(obj_num).collect()),
            _ => None,
        };
        let multi = matches!(get("MultipleDataSources"), Some(Obj::Bool(true)));
        let src =
            get("DataSource").ok_or_else(|| err("rangecheck", "an image has no /DataSource"))?;
        let sources = match (&src, multi) {
            (Obj::Arr(a), true) => a.items(),
            _ => vec![src],
        };
        let cs = self.g.cspace.clone();
        self.image(w, h, bpc, matrix, sources, multi, cs, decode, mask)
    }
}

fn check_image_dims(limits: ImportLimits, w: u32, h: u32) -> Result<(), CodecError> {
    if w > limits.max_width
        || h > limits.max_height
        || u64::from(w) * u64::from(h) > limits.max_pixels
    {
        return Err(CodecError::LimitExceeded(format!(
            "is {w}x{h}, larger than the import limit"
        )));
    }
    Ok(())
}

fn num6(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let r = (v * 1e6).round() / 1e6;
    format!("{r}")
}

fn hex_bytes(c: [f64; 3]) -> [u8; 3] {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [b(c[0]), b(c[1]), b(c[2])]
}

/// The number of components in a JPEG's frame header.
fn jpeg_components(data: &[u8]) -> Option<usize> {
    let mut i = 2;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = data[i + 1];
        if matches!(m, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            return Some(usize::from(data[i + 9]));
        }
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) || m == 0xFF {
            i += if m == 0xFF { 1 } else { 2 };
            continue;
        }
        let len = usize::from(u16::from_be_bytes([data[i + 2], data[i + 3]]));
        i += 2 + len;
    }
    None
}

fn font_css(name: &str) -> (String, &'static str, &'static str) {
    let lower = name.to_ascii_lowercase();
    let generic = if lower.contains("courier") || lower.contains("mono") {
        "'Courier New', Courier, monospace"
    } else if lower.contains("times")
        || (lower.contains("serif") && !lower.contains("sans"))
        || lower.contains("georgia")
        || lower.contains("garamond")
        || lower.contains("minion")
    {
        "'Times New Roman', Times, serif"
    } else {
        "Helvetica, Arial, sans-serif"
    };
    let family: String = name
        .split('-')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
        .take(64)
        .collect();
    let fam = if family.is_empty() {
        generic.to_string()
    } else {
        format!("'{family}', {generic}")
    };
    let weight = if lower.contains("bold") || lower.contains("black") || lower.contains("heavy") {
        " font-weight=\"bold\""
    } else {
        ""
    };
    let style = if lower.contains("italic") || lower.contains("oblique") {
        " font-style=\"italic\""
    } else {
        ""
    };
    (fam, weight, style)
}

/// Latin-1 bytes as escaped SVG text.
fn svg_text(chars: &[char]) -> String {
    let mut s = String::new();
    for &c in chars {
        match c {
            '&' => s.push_str("&amp;"),
            '<' => s.push_str("&lt;"),
            '>' => s.push_str("&gt;"),
            '"' => s.push_str("&quot;"),
            c if c.is_control() => {}
            c => s.push(c),
        }
    }
    s
}

/// The character an Adobe glyph name stands for (`glyphshow`): the names
/// matplotlib and cairo write for Latin text, and `uniXXXX`.
fn glyph_char(name: &str) -> Option<char> {
    let mut it = name.chars();
    if let (Some(c), None) = (it.next(), it.next()) {
        if c.is_ascii_alphabetic() {
            return Some(c);
        }
    }
    if let Some(hex) = name.strip_prefix("uni") {
        return u32::from_str_radix(hex.get(..4)?, 16)
            .ok()
            .and_then(char::from_u32);
    }
    const NAMES: &[(&str, char)] = &[
        ("space", ' '),
        ("zero", '0'),
        ("one", '1'),
        ("two", '2'),
        ("three", '3'),
        ("four", '4'),
        ("five", '5'),
        ("six", '6'),
        ("seven", '7'),
        ("eight", '8'),
        ("nine", '9'),
        ("period", '.'),
        ("comma", ','),
        ("colon", ':'),
        ("semicolon", ';'),
        ("hyphen", '-'),
        ("minus", '\u{2212}'),
        ("plus", '+'),
        ("equal", '='),
        ("parenleft", '('),
        ("parenright", ')'),
        ("bracketleft", '['),
        ("bracketright", ']'),
        ("braceleft", '{'),
        ("braceright", '}'),
        ("slash", '/'),
        ("backslash", '\\'),
        ("exclam", '!'),
        ("question", '?'),
        ("quotedbl", '"'),
        ("quotesingle", '\''),
        ("quoteleft", '\u{2018}'),
        ("quoteright", '\u{2019}'),
        ("numbersign", '#'),
        ("dollar", '$'),
        ("percent", '%'),
        ("ampersand", '&'),
        ("asterisk", '*'),
        ("less", '<'),
        ("greater", '>'),
        ("at", '@'),
        ("underscore", '_'),
        ("asciicircum", '^'),
        ("asciitilde", '~'),
        ("bar", '|'),
        ("grave", '`'),
        ("degree", '\u{b0}'),
        ("endash", '\u{2013}'),
        ("emdash", '\u{2014}'),
        ("mu", '\u{3bc}'),
        ("multiply", '\u{d7}'),
    ];
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

fn obj_num(o: &Obj) -> Option<f64> {
    match o {
        Obj::Int(i) => Some(*i as f64),
        Obj::Real(r) => Some(*r),
        _ => None,
    }
}

fn arr_matrix(a: &PsArr) -> Res<Matrix> {
    if a.len != 6 {
        return Err(err("rangecheck", "a matrix has six numbers"));
    }
    let mut m = [0.0; 6];
    for (i, v) in m.iter_mut().enumerate() {
        *v = a
            .get(i)
            .as_ref()
            .and_then(obj_num)
            .filter(|v| v.is_finite())
            .ok_or_else(|| err("typecheck", "a matrix holds numbers"))?;
    }
    Ok(m)
}

/// A scan error in a string (`token`, an executed string) is a
/// `syntaxerror` the program can catch; only the program's own text ends
/// the run.
fn syntax_in_string(f: Flow) -> Flow {
    match f {
        Flow::Fatal(Stop::Malformed(why)) => Flow::Error("syntaxerror", why),
        other => other,
    }
}

fn err(name: &'static str, cmd: impl Into<String>) -> Flow {
    Flow::Error(name, cmd.into())
}

// ---------------------------------------------------------------- operators

macro_rules! ops {
    ($($name:literal => $f:expr),* $(,)?) => {
        static BUILTINS: &[Builtin] = &[$(Builtin { name: $name, f: $f }),*];
    };
}

fn arith(it: &mut Interp, fi: fn(i64, i64) -> Option<i64>, fr: fn(f64, f64) -> f64) -> Res {
    let b = it.pop()?;
    let a = it.pop()?;
    let r = match (&a, &b) {
        (Obj::Int(x), Obj::Int(y)) => match fi(*x, *y) {
            Some(v) => int_or_real(v),
            None => Obj::Real(fr(*x as f64, *y as f64)),
        },
        _ => match (obj_num(&a), obj_num(&b)) {
            (Some(x), Some(y)) => Obj::Real(fr(x, y)),
            _ => return Err(err("typecheck", "")),
        },
    };
    it.push(r);
    Ok(())
}

fn unary(it: &mut Interp, fi: fn(i64) -> i64, fr: fn(f64) -> f64) -> Res {
    let r = match it.pop()? {
        Obj::Int(i) => int_or_real(fi(i)),
        Obj::Real(r) => Obj::Real(fr(r)),
        _ => return Err(err("typecheck", "")),
    };
    it.push(r);
    Ok(())
}

fn real_op(it: &mut Interp, f: fn(f64) -> f64) -> Res {
    let v = f(it.pop_num()?);
    if !v.is_finite() {
        return Err(err("undefinedresult", ""));
    }
    it.push(Obj::Real(v));
    Ok(())
}

fn obj_eq(a: &Obj, b: &Obj) -> bool {
    match (a, b) {
        (Obj::Int(_) | Obj::Real(_), Obj::Int(_) | Obj::Real(_)) => obj_num(a) == obj_num(b),
        (Obj::Bool(x), Obj::Bool(y)) => x == y,
        (Obj::Null, Obj::Null) | (Obj::Mark, Obj::Mark) => true,
        (Obj::Name(..) | Obj::Str(_), Obj::Name(..) | Obj::Str(_)) => text_of(a) == text_of(b),
        (Obj::Arr(x), Obj::Arr(y)) => {
            Rc::ptr_eq(&x.buf, &y.buf) && x.start == y.start && x.len == y.len
        }
        (Obj::Dict(x), Obj::Dict(y)) => Rc::ptr_eq(x, y),
        (Obj::Op(x), Obj::Op(y)) => x.name == y.name,
        (Obj::File(x), Obj::File(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

fn text_of(o: &Obj) -> Vec<u8> {
    match o {
        Obj::Name(n, _) => n.as_bytes().to_vec(),
        Obj::Str(s) => s.bytes(),
        _ => Vec::new(),
    }
}

fn compare(it: &mut Interp, f: fn(std::cmp::Ordering) -> bool) -> Res {
    let b = it.pop()?;
    let a = it.pop()?;
    let ord = match (&a, &b) {
        (Obj::Str(x), Obj::Str(y)) => x.bytes().cmp(&y.bytes()),
        _ => match (obj_num(&a), obj_num(&b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
            _ => return Err(err("typecheck", "")),
        },
    };
    it.push(Obj::Bool(f(ord)));
    Ok(())
}

fn logic(it: &mut Interp, fb: fn(bool, bool) -> bool, fi: fn(i64, i64) -> i64) -> Res {
    let b = it.pop()?;
    let a = it.pop()?;
    let r = match (a, b) {
        (Obj::Bool(x), Obj::Bool(y)) => Obj::Bool(fb(x, y)),
        (Obj::Int(x), Obj::Int(y)) => int_or_real(i64::from(fi(x, y) as i32)),
        _ => return Err(err("typecheck", "")),
    };
    it.push(r);
    Ok(())
}

fn type_name(o: &Obj) -> &'static str {
    match o {
        Obj::Null => "nulltype",
        Obj::Mark => "marktype",
        Obj::Bool(_) => "booleantype",
        Obj::Int(_) => "integertype",
        Obj::Real(_) => "realtype",
        Obj::Name(..) => "nametype",
        Obj::Str(_) => "stringtype",
        Obj::Arr(_) => "arraytype",
        Obj::Dict(_) => "dicttype",
        Obj::Op(_) => "operatortype",
        Obj::File(_) => "filetype",
        Obj::Save(_) => "savetype",
    }
}

fn cvs_text(o: &Obj) -> Vec<u8> {
    match o {
        Obj::Int(i) => i.to_string().into_bytes(),
        Obj::Real(r) => {
            if *r == r.trunc() && r.abs() < 1e15 {
                format!("{r:.1}").into_bytes()
            } else {
                format!("{r}").into_bytes()
            }
        }
        Obj::Bool(b) => b.to_string().into_bytes(),
        Obj::Name(n, _) => n.as_bytes().to_vec(),
        Obj::Str(s) => s.bytes(),
        Obj::Op(b) => b.name.as_bytes().to_vec(),
        _ => b"--nostringval--".to_vec(),
    }
}

fn set_exec(o: Obj, exec: bool) -> Obj {
    match o {
        Obj::Name(n, _) => Obj::Name(n, exec),
        Obj::Arr(mut a) => {
            a.exec = exec;
            Obj::Arr(a)
        }
        Obj::Str(mut s) => {
            s.exec = exec;
            Obj::Str(s)
        }
        other => other,
    }
}

/// `bind` over a procedure and every procedure inside it. A work list, not
/// a recursion, and each array is walked once however often it is nested
/// (a procedure that holds itself twice would otherwise be a walk of 2^depth
/// steps); every element counts against the operation and time budget.
fn bind_proc(it: &mut Interp, a: &PsArr) -> Res {
    let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut work = vec![a.clone()];
    while let Some(a) = work.pop() {
        if !seen.insert((Rc::as_ptr(&a.buf) as usize, a.start)) {
            continue;
        }
        for i in 0..a.len {
            it.tick()?;
            let Some(o) = a.get(i) else { break };
            match o {
                Obj::Name(n, true) => {
                    if let Some(Obj::Op(b)) = it.lookup(&Key::Name(n)) {
                        if let Some(slot) = a.buf.borrow_mut().get_mut(a.start + i) {
                            *slot = Obj::Op(b);
                        }
                    }
                }
                Obj::Arr(inner) if inner.exec => work.push(inner),
                _ => {}
            }
        }
    }
    Ok(())
}

fn op_get(it: &mut Interp) -> Res {
    let k = it.pop()?;
    let c = it.pop()?;
    let v = match c {
        Obj::Dict(d) => {
            let key = it.key(&k)?;
            let v = d.borrow().get(&key);
            v.ok_or_else(|| {
                err(
                    "undefined",
                    String::from_utf8_lossy(&cvs_text(&k)).into_owned(),
                )
            })?
        }
        Obj::Arr(a) => {
            let i = obj_num(&k).ok_or_else(|| err("typecheck", ""))?;
            if i < 0.0 {
                return Err(err("rangecheck", ""));
            }
            a.get(i as usize).ok_or_else(|| err("rangecheck", ""))?
        }
        Obj::Str(s) => {
            let i = obj_num(&k).ok_or_else(|| err("typecheck", ""))?;
            if i < 0.0 || i as usize >= s.len {
                return Err(err("rangecheck", ""));
            }
            Obj::Int(i64::from(
                *s.buf.borrow().get(s.start + i as usize).unwrap_or(&0),
            ))
        }
        _ => return Err(err("typecheck", "")),
    };
    it.push(v);
    Ok(())
}

fn op_put(it: &mut Interp) -> Res {
    let v = it.pop()?;
    let k = it.pop()?;
    match it.pop()? {
        Obj::Dict(d) => {
            let key = it.key(&k)?;
            if d.borrow_mut().put(key, v) {
                it.alloc(64)?;
            }
        }
        Obj::Arr(a) => {
            let i = obj_num(&k).ok_or_else(|| err("typecheck", ""))?;
            if i < 0.0 || i as usize >= a.len {
                return Err(err("rangecheck", ""));
            }
            if let Some(slot) = a.buf.borrow_mut().get_mut(a.start + i as usize) {
                *slot = v;
            }
        }
        Obj::Str(s) => {
            let i = obj_num(&k).ok_or_else(|| err("typecheck", ""))?;
            let b = obj_num(&v).ok_or_else(|| err("typecheck", ""))?;
            if i < 0.0 || i as usize >= s.len {
                return Err(err("rangecheck", ""));
            }
            if let Some(slot) = s.buf.borrow_mut().get_mut(s.start + i as usize) {
                *slot = (b as i64 & 0xFF) as u8;
            }
        }
        _ => return Err(err("typecheck", "")),
    }
    Ok(())
}

fn op_getinterval(it: &mut Interp) -> Res {
    let n = it.pop_int()?;
    let i = it.pop_int()?;
    let o = it.pop()?;
    let check = |len: usize| -> Res<(usize, usize)> {
        if i < 0 || n < 0 || (i + n) as usize > len {
            return Err(err("rangecheck", ""));
        }
        Ok((i as usize, n as usize))
    };
    let r = match o {
        Obj::Arr(a) => {
            let (i, n) = check(a.len)?;
            Obj::Arr(PsArr {
                buf: a.buf.clone(),
                start: a.start + i,
                len: n,
                exec: a.exec,
            })
        }
        Obj::Str(s) => {
            let (i, n) = check(s.len)?;
            Obj::Str(PsStr {
                start: s.start + i,
                len: n,
                ..s
            })
        }
        _ => return Err(err("typecheck", "")),
    };
    it.push(r);
    Ok(())
}

fn op_putinterval(it: &mut Interp) -> Res {
    let src = it.pop()?;
    let i = it.pop_int()?;
    let dst = it.pop()?;
    match (dst, src) {
        (Obj::Arr(d), Obj::Arr(s)) => {
            let items = s.items();
            if i < 0 || i as usize + items.len() > d.len {
                return Err(err("rangecheck", ""));
            }
            let mut b = d.buf.borrow_mut();
            for (k, v) in items.into_iter().enumerate() {
                if let Some(slot) = b.get_mut(d.start + i as usize + k) {
                    *slot = v;
                }
            }
        }
        (Obj::Str(d), Obj::Str(s)) => {
            let bytes = s.bytes();
            if i < 0 || i as usize + bytes.len() > d.len {
                return Err(err("rangecheck", ""));
            }
            let mut b = d.buf.borrow_mut();
            for (k, v) in bytes.into_iter().enumerate() {
                if let Some(slot) = b.get_mut(d.start + i as usize + k) {
                    *slot = v;
                }
            }
        }
        _ => return Err(err("typecheck", "")),
    }
    Ok(())
}

fn op_length(it: &mut Interp) -> Res {
    let n = match it.pop()? {
        Obj::Arr(a) => a.len,
        Obj::Str(s) => s.len,
        Obj::Dict(d) => d.borrow().entries.len(),
        Obj::Name(n, _) => n.len(),
        _ => return Err(err("typecheck", "")),
    };
    it.push(int_or_real(n as i64));
    Ok(())
}

fn op_copy(it: &mut Interp) -> Res {
    match it.top() {
        Some(Obj::Int(_)) => {
            let n = it.pop_int()?;
            if n < 0 || n as usize > it.ostack.len() {
                return Err(err("rangecheck", ""));
            }
            it.alloc(n as u64 * 8)?;
            let from = it.ostack.len() - n as usize;
            let copy: Vec<Obj> = it.ostack[from..].to_vec();
            it.ostack.extend(copy);
            Ok(())
        }
        Some(Obj::Arr(_)) => {
            let d = it.pop_arr()?;
            let s = it.pop_arr()?;
            let items = s.items();
            if items.len() > d.len {
                return Err(err("rangecheck", ""));
            }
            {
                let mut b = d.buf.borrow_mut();
                for (k, v) in items.iter().enumerate() {
                    if let Some(slot) = b.get_mut(d.start + k) {
                        *slot = v.clone();
                    }
                }
            }
            it.push(Obj::Arr(PsArr {
                buf: d.buf.clone(),
                start: d.start,
                len: items.len(),
                exec: d.exec,
            }));
            Ok(())
        }
        Some(Obj::Str(_)) => {
            let d = it.pop_str()?;
            let s = it.pop_str()?;
            let bytes = s.bytes();
            if bytes.len() > d.len {
                return Err(err("rangecheck", ""));
            }
            {
                let mut b = d.buf.borrow_mut();
                for (k, v) in bytes.iter().enumerate() {
                    if let Some(slot) = b.get_mut(d.start + k) {
                        *slot = *v;
                    }
                }
            }
            it.push(Obj::Str(PsStr {
                len: bytes.len(),
                ..d
            }));
            Ok(())
        }
        Some(Obj::Dict(_)) => {
            let d = it.pop_dict()?;
            let s = it.pop_dict()?;
            let entries: Vec<(Key, Obj)> = s.borrow().entries.clone();
            it.alloc(entries.len() as u64 * 64)?;
            {
                let mut b = d.borrow_mut();
                for (k, v) in entries {
                    b.put(k, v);
                }
            }
            it.push(Obj::Dict(d));
            Ok(())
        }
        _ => Err(err("typecheck", "")),
    }
}

fn op_forall(it: &mut Interp) -> Res {
    let p = it.pop_proc()?;
    let c = it.pop()?;
    let r = (|| -> Res {
        match c {
            Obj::Arr(a) => {
                for i in 0..a.len {
                    it.tick()?;
                    let Some(v) = a.get(i) else { break };
                    it.push(v);
                    it.call(&p)?;
                }
            }
            Obj::Str(s) => {
                for b in s.bytes() {
                    it.tick()?;
                    it.push(Obj::Int(i64::from(b)));
                    it.call(&p)?;
                }
            }
            Obj::Dict(d) => {
                let entries: Vec<(Key, Obj)> = d.borrow().entries.clone();
                for (k, v) in entries {
                    it.tick()?;
                    it.push(key_obj(&k));
                    it.push(v);
                    it.call(&p)?;
                }
            }
            _ => return Err(err("typecheck", "")),
        }
        Ok(())
    })();
    loop_end(r)
}

/// `exit` ends the loop it is in.
fn loop_end(r: Res) -> Res {
    match r {
        Err(Flow::Exit) => Ok(()),
        other => other,
    }
}

fn op_for(it: &mut Interp) -> Res {
    let p = it.pop_proc()?;
    let limit = it.pop()?;
    let inc = it.pop()?;
    let init = it.pop()?;
    let all_int = matches!((&init, &inc), (Obj::Int(_), Obj::Int(_)));
    let (mut v, step, lim) = (
        obj_num(&init).ok_or_else(|| err("typecheck", ""))?,
        obj_num(&inc).ok_or_else(|| err("typecheck", ""))?,
        obj_num(&limit).ok_or_else(|| err("typecheck", ""))?,
    );
    let r = (|| -> Res {
        loop {
            it.tick()?;
            if (step >= 0.0 && v > lim) || (step < 0.0 && v < lim) {
                return Ok(());
            }
            it.push(if all_int {
                int_or_real(v as i64)
            } else {
                Obj::Real(v)
            });
            it.call(&p)?;
            v += step;
        }
    })();
    loop_end(r)
}

fn op_repeat(it: &mut Interp) -> Res {
    let p = it.pop_proc()?;
    let n = it.pop_int()?;
    if n < 0 {
        return Err(err("rangecheck", ""));
    }
    let r = (|| -> Res {
        for _ in 0..n {
            it.tick()?;
            it.call(&p)?;
        }
        Ok(())
    })();
    loop_end(r)
}

fn op_loop(it: &mut Interp) -> Res {
    let p = it.pop_proc()?;
    let r = (|| -> Res {
        loop {
            it.tick()?;
            it.call(&p)?;
        }
    })();
    loop_end(r)
}

fn op_stopped(it: &mut Interp) -> Res {
    let p = it.pop()?;
    let depth = it.depth;
    it.stopped_depth += 1;
    let r = it.exec_value(p);
    it.stopped_depth -= 1;
    it.depth = depth;
    match r {
        Ok(()) => it.push(Obj::Bool(false)),
        Err(Flow::Stop) | Err(Flow::Error(..)) | Err(Flow::Exit) => it.push(Obj::Bool(true)),
        Err(other) => return Err(other),
    }
    Ok(())
}

fn op_if(it: &mut Interp) -> Res {
    let p = it.pop_proc()?;
    if it.pop_bool()? {
        it.call(&p)?;
    }
    Ok(())
}

fn op_ifelse(it: &mut Interp) -> Res {
    let b = it.pop_proc()?;
    let a = it.pop_proc()?;
    if it.pop_bool()? {
        it.call(&a)
    } else {
        it.call(&b)
    }
}

fn op_exec(it: &mut Interp) -> Res {
    let o = it.pop()?;
    it.exec_value(o)
}

fn op_def(it: &mut Interp) -> Res {
    let v = it.pop()?;
    let k = it.pop()?;
    let key = it.key(&k)?;
    it.def(key, v)
}

fn op_load(it: &mut Interp) -> Res {
    let k = it.pop()?;
    let key = it.key(&k)?;
    let v = it.lookup(&key).ok_or_else(|| {
        err(
            "undefined",
            String::from_utf8_lossy(&cvs_text(&k)).into_owned(),
        )
    })?;
    it.push(v);
    Ok(())
}

fn op_store(it: &mut Interp) -> Res {
    let v = it.pop()?;
    let k = it.pop()?;
    let key = it.key(&k)?;
    let target = it
        .dstack
        .iter()
        .rev()
        .find(|d| d.borrow().index.contains_key(&key))
        .cloned();
    match target {
        Some(d) => {
            d.borrow_mut().put(key, v);
            Ok(())
        }
        None => it.def(key, v),
    }
}

fn op_where(it: &mut Interp) -> Res {
    let k = it.pop()?;
    let key = it.key(&k)?;
    let found = it
        .dstack
        .iter()
        .rev()
        .find(|d| d.borrow().index.contains_key(&key))
        .cloned();
    match found {
        Some(d) => {
            it.push(Obj::Dict(d));
            it.push(Obj::Bool(true));
        }
        None => it.push(Obj::Bool(false)),
    }
    Ok(())
}

fn op_known(it: &mut Interp) -> Res {
    let k = it.pop()?;
    let d = it.pop_dict()?;
    let key = it.key(&k)?;
    let known = d.borrow().index.contains_key(&key);
    it.push(Obj::Bool(known));
    Ok(())
}

fn op_undef(it: &mut Interp) -> Res {
    let k = it.pop()?;
    let d = it.pop_dict()?;
    let key = it.key(&k)?;
    d.borrow_mut().remove(&key);
    Ok(())
}

fn op_dict(it: &mut Interp) -> Res {
    let n = it.pop_int()?;
    if n < 0 {
        return Err(err("rangecheck", ""));
    }
    it.alloc(64)?;
    let d = it.new_dict();
    it.push(Obj::Dict(d));
    Ok(())
}

fn op_begin(it: &mut Interp) -> Res {
    let d = it.pop_dict()?;
    if it.dstack.len() >= MAX_DICT_STACK {
        return Err(err("dictstackoverflow", ""));
    }
    it.dstack.push(d);
    Ok(())
}

fn op_end(it: &mut Interp) -> Res {
    if it.dstack.len() <= 2 {
        return Err(err("dictstackunderflow", ""));
    }
    it.dstack.pop();
    Ok(())
}

fn op_currentdict(it: &mut Interp) -> Res {
    let d = it
        .dstack
        .last()
        .cloned()
        .unwrap_or_else(|| it.userdict.clone());
    it.push(Obj::Dict(d));
    Ok(())
}

fn op_array(it: &mut Interp) -> Res {
    let n = it.pop_int()?;
    if !(0..=1 << 24).contains(&n) {
        return Err(err("rangecheck", ""));
    }
    let a = it.new_arr(vec![Obj::Null; n as usize], false)?;
    it.push(a);
    Ok(())
}

fn op_string(it: &mut Interp) -> Res {
    let n = it.pop_int()?;
    if !(0..=1 << 26).contains(&n) {
        return Err(err("rangecheck", ""));
    }
    let s = it.new_str(vec![0; n as usize])?;
    it.push(s);
    Ok(())
}

fn op_close_array(it: &mut Interp) -> Res {
    let at = it
        .ostack
        .iter()
        .rposition(|o| matches!(o, Obj::Mark))
        .ok_or_else(|| err("unmatchedmark", ""))?;
    let items = it.ostack.split_off(at + 1);
    it.ostack.pop();
    let a = it.new_arr(items, false)?;
    it.push(a);
    Ok(())
}

fn op_close_dict(it: &mut Interp) -> Res {
    let at = it
        .ostack
        .iter()
        .rposition(|o| matches!(o, Obj::Mark))
        .ok_or_else(|| err("unmatchedmark", ""))?;
    let items = it.ostack.split_off(at + 1);
    it.ostack.pop();
    if !items.len().is_multiple_of(2) {
        return Err(err("rangecheck", "a dictionary needs key-value pairs"));
    }
    it.alloc(items.len() as u64 * 32 + 64)?;
    let d = it.new_dict();
    for pair in items.as_chunks::<2>().0 {
        let k = it.key(&pair[0])?;
        d.borrow_mut().put(k, pair[1].clone());
    }
    it.push(Obj::Dict(d));
    Ok(())
}

fn op_aload(it: &mut Interp) -> Res {
    let a = it.pop_arr()?;
    it.alloc(a.len as u64 * 8)?;
    let items = a.items();
    it.ostack.extend(items);
    it.push(Obj::Arr(a));
    Ok(())
}

fn op_astore(it: &mut Interp) -> Res {
    let a = it.pop_arr()?;
    if it.ostack.len() < a.len {
        return Err(err("stackunderflow", ""));
    }
    let items = it.ostack.split_off(it.ostack.len() - a.len);
    {
        let mut b = a.buf.borrow_mut();
        for (k, v) in items.into_iter().enumerate() {
            if let Some(slot) = b.get_mut(a.start + k) {
                *slot = v;
            }
        }
    }
    it.push(Obj::Arr(a));
    Ok(())
}

fn op_index(it: &mut Interp) -> Res {
    let n = it.pop_int()?;
    if n < 0 || n as usize >= it.ostack.len() {
        return Err(err("rangecheck", ""));
    }
    let v = it.ostack[it.ostack.len() - 1 - n as usize].clone();
    it.push(v);
    Ok(())
}

fn op_roll(it: &mut Interp) -> Res {
    let j = it.pop_int()?;
    let n = it.pop_int()?;
    if n < 0 || n as usize > it.ostack.len() {
        return Err(err("rangecheck", ""));
    }
    if n == 0 {
        return Ok(());
    }
    let from = it.ostack.len() - n as usize;
    let k = j.rem_euclid(n) as usize;
    it.ostack[from..].rotate_right(k);
    Ok(())
}

fn op_counttomark(it: &mut Interp) -> Res {
    let at = it
        .ostack
        .iter()
        .rposition(|o| matches!(o, Obj::Mark))
        .ok_or_else(|| err("unmatchedmark", ""))?;
    let n = it.ostack.len() - at - 1;
    it.push(int_or_real(n as i64));
    Ok(())
}

fn op_cleartomark(it: &mut Interp) -> Res {
    let at = it
        .ostack
        .iter()
        .rposition(|o| matches!(o, Obj::Mark))
        .ok_or_else(|| err("unmatchedmark", ""))?;
    it.ostack.truncate(at);
    Ok(())
}

fn op_cvs(it: &mut Interp) -> Res {
    let dst = it.pop_str()?;
    let o = it.pop()?;
    let text = cvs_text(&o);
    if text.len() > dst.len {
        return Err(err("rangecheck", ""));
    }
    {
        let mut b = dst.buf.borrow_mut();
        for (k, v) in text.iter().enumerate() {
            if let Some(slot) = b.get_mut(dst.start + k) {
                *slot = *v;
            }
        }
    }
    it.push(Obj::Str(PsStr {
        len: text.len(),
        ..dst
    }));
    Ok(())
}

fn op_cvi(it: &mut Interp) -> Res {
    let v = match it.pop()? {
        Obj::Str(s) => match number(&s.bytes()) {
            Some(Tok::Int(i)) => i as f64,
            Some(Tok::Real(r)) => r,
            _ => return Err(err("typecheck", "")),
        },
        o => obj_num(&o).ok_or_else(|| err("typecheck", ""))?,
    };
    let t = v.trunc();
    if !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&t) {
        return Err(err("rangecheck", ""));
    }
    it.push(Obj::Int(t as i64));
    Ok(())
}

fn op_cvr(it: &mut Interp) -> Res {
    let v = match it.pop()? {
        Obj::Str(s) => match number(&s.bytes()) {
            Some(Tok::Int(i)) => i as f64,
            Some(Tok::Real(r)) => r,
            _ => return Err(err("typecheck", "")),
        },
        o => obj_num(&o).ok_or_else(|| err("typecheck", ""))?,
    };
    it.push(Obj::Real(v));
    Ok(())
}

fn op_cvn(it: &mut Interp) -> Res {
    let s = it.pop_str()?;
    let n: Rc<str> = String::from_utf8_lossy(&s.bytes()).as_ref().into();
    it.push(Obj::Name(n, s.exec));
    Ok(())
}

fn op_token(it: &mut Interp) -> Res {
    let s = it.pop_str()?;
    let bytes = s.bytes();
    let f = mem_file(bytes);
    match it.read_object(&f, 0).map_err(syntax_in_string)? {
        Some(o) => {
            let used = match &f.borrow().src {
                Src::Mem { pos, .. } => *pos,
                _ => 0,
            };
            let rest = Obj::Str(PsStr {
                start: s.start + used.min(s.len),
                len: s.len - used.min(s.len),
                ..s
            });
            it.push(rest);
            it.push(o);
            it.push(Obj::Bool(true));
        }
        None => it.push(Obj::Bool(false)),
    }
    Ok(())
}

fn op_search(it: &mut Interp, anchor: bool) -> Res {
    let seek = it.pop_str()?;
    let s = it.pop_str()?;
    // Searched in place: no copy of either string and no table sized by the
    // needle (memchr's two-way search needs constant extra space), so a
    // search costs nothing against the memory budget it does not charge.
    let (at, seek_len, hay_len) = {
        let hay_buf = s.buf.borrow();
        let seek_buf = seek.buf.borrow();
        let hay = hay_buf.get(s.start..s.start + s.len).unwrap_or(&[]);
        let needle = seek_buf
            .get(seek.start..seek.start + seek.len)
            .unwrap_or(&[]);
        let at = if anchor {
            hay.starts_with(needle).then_some(0)
        } else {
            memchr::memmem::find(hay, needle)
        };
        (at, needle.len(), hay.len())
    };
    match at {
        Some(i) => {
            let part = |start: usize, len: usize| {
                Obj::Str(PsStr {
                    buf: s.buf.clone(),
                    start: s.start + start,
                    len,
                    exec: false,
                })
            };
            if anchor {
                it.push(part(seek_len, hay_len - seek_len));
                it.push(part(0, seek_len));
            } else {
                it.push(part(i + seek_len, hay_len - i - seek_len));
                it.push(part(i, seek_len));
                it.push(part(0, i));
            }
            it.push(Obj::Bool(true));
        }
        None => {
            it.push(Obj::Str(s));
            it.push(Obj::Bool(false));
        }
    }
    Ok(())
}

fn op_readhexstring(it: &mut Interp) -> Res {
    let s = it.pop_str()?;
    let f = match it.pop()? {
        Obj::File(f) => f,
        _ => return Err(err("typecheck", "")),
    };
    let mut got = Vec::with_capacity(s.len);
    let mut high: Option<u8> = None;
    while got.len() < s.len {
        let Some(c) = it.read_byte(&f)? else { break };
        let Some(v) = hex_val(c) else { continue };
        match high.take() {
            Some(h) => got.push(h << 4 | v),
            None => high = Some(v),
        }
    }
    let full = got.len() == s.len;
    fill_str(&s, &got);
    it.push(Obj::Str(PsStr {
        len: got.len(),
        ..s
    }));
    it.push(Obj::Bool(full));
    Ok(())
}

fn fill_str(s: &PsStr, bytes: &[u8]) {
    let mut b = s.buf.borrow_mut();
    for (k, v) in bytes.iter().enumerate() {
        if let Some(slot) = b.get_mut(s.start + k) {
            *slot = *v;
        }
    }
}

fn op_readstring(it: &mut Interp) -> Res {
    let s = it.pop_str()?;
    let f = match it.pop()? {
        Obj::File(f) => f,
        _ => return Err(err("typecheck", "")),
    };
    let got = it.read_bytes(&f, s.len)?;
    let full = got.len() == s.len;
    fill_str(&s, &got);
    it.push(Obj::Str(PsStr {
        len: got.len(),
        ..s
    }));
    it.push(Obj::Bool(full));
    Ok(())
}

fn op_readline(it: &mut Interp) -> Res {
    let s = it.pop_str()?;
    let f = match it.pop()? {
        Obj::File(f) => f,
        _ => return Err(err("typecheck", "")),
    };
    let mut got = Vec::new();
    let mut ended = false;
    while let Some(c) = it.read_byte(&f)? {
        if c == b'\n' {
            ended = true;
            break;
        }
        if c == b'\r' {
            ended = true;
            if let Some(n) = it.read_byte(&f)? {
                if n != b'\n' {
                    Interp::unread(&f, &[n]);
                }
            }
            break;
        }
        if got.len() >= s.len {
            return Err(err("rangecheck", "a line is longer than its string"));
        }
        got.push(c);
    }
    fill_str(&s, &got);
    it.push(Obj::Str(PsStr {
        len: got.len(),
        ..s
    }));
    it.push(Obj::Bool(ended));
    Ok(())
}

fn op_read(it: &mut Interp) -> Res {
    let f = match it.pop()? {
        Obj::File(f) => f,
        _ => return Err(err("typecheck", "")),
    };
    match it.read_byte(&f)? {
        Some(b) => {
            it.push(Obj::Int(i64::from(b)));
            it.push(Obj::Bool(true));
        }
        None => it.push(Obj::Bool(false)),
    }
    Ok(())
}

fn op_filter(it: &mut Interp) -> Res {
    let name = match it.pop()? {
        Obj::Name(n, _) => n,
        _ => return Err(err("typecheck", "")),
    };
    let kind = match &*name {
        "ASCIIHexDecode" | "AHx" => FilterKind::Hex { high: None },
        "ASCII85Decode" | "A85" => FilterKind::A85 { group: Vec::new() },
        "RunLengthDecode" | "RL" => FilterKind::RunLength,
        "FlateDecode" | "Fl" => FilterKind::Flate(Box::new(flate2::Decompress::new(true))),
        "DCTDecode" | "DCT" => FilterKind::Dct,
        "SubFileDecode" => {
            let (count, marker) = if let Some(Obj::Dict(_)) = it.top() {
                let d = it.pop_dict()?;
                let b = d.borrow();
                let count = b
                    .get(&Key::Name("EODCount".into()))
                    .and_then(|v| obj_num(&v))
                    .unwrap_or(0.0) as i64;
                let marker = b
                    .get(&Key::Name("EODString".into()))
                    .map(|v| text_of(&v))
                    .unwrap_or_default();
                (count, marker)
            } else {
                let marker = it.pop_str()?.bytes();
                (it.pop_int()?, marker)
            };
            FilterKind::SubFile {
                count: count.max(1),
                left: count,
                eod: marker,
                pending: Vec::new(),
            }
        }
        other => {
            it.skipped
                .insert("a data filter this build does not decode");
            return Err(err("undefined", format!("the {other} filter")));
        }
    };
    if let (false, Some(Obj::Dict(_))) = (matches!(kind, FilterKind::SubFile { .. }), it.top()) {
        it.pop()?;
    }
    let src = it.pop()?;
    let src = match src {
        Obj::Str(s) => mem_file(s.bytes()),
        other => it.source_file(other)?,
    };
    // Each filter reads the one under it (a recursion as deep as the
    // chain), so the chain is bounded like the call depth.
    let mut chain = 1usize;
    let mut cur = src.clone();
    loop {
        let next = match &cur.borrow().src {
            Src::Filter { src, .. } => Some(src.clone()),
            _ => None,
        };
        let Some(next) = next else { break };
        chain += 1;
        if chain > MAX_FILTER_CHAIN {
            return Err(err(
                "limitcheck",
                format!("more than {MAX_FILTER_CHAIN} filters over one file"),
            ));
        }
        cur = next;
    }
    it.alloc(256)?;
    it.push(Obj::File(new_file(Src::Filter {
        kind,
        src,
        out: VecDeque::new(),
        eod: false,
    })));
    Ok(())
}

fn op_eexec(it: &mut Interp) -> Res {
    let f = match it.pop()? {
        Obj::File(f) => f,
        Obj::Str(_) => {
            it.type1_skipped += 1;
            return Ok(());
        }
        _ => return Err(err("typecheck", "")),
    };
    // The encrypted section runs to the cleartext `cleartomark` after its
    // 512 zeros; it is skipped, and the font dictionary its cleartext part
    // left on the stack is registered under its name (drawn in the
    // fallback font).
    {
        let mut st = f.borrow_mut();
        if let Src::Mem { data, pos } = &mut st.src {
            *pos = match find(data, *pos, b"cleartomark") {
                Some(at) => at + b"cleartomark".len(),
                None => data.len(),
            };
        }
    }
    it.type1_skipped += 1;
    if let Some(Obj::Dict(d)) = it.top().cloned() {
        it.pop()?;
        let name = d.borrow().get(&Key::Name("FontName".into()));
        if let Some(n @ Obj::Name(..)) = name {
            let k = it.key(&n)?;
            it.fontdir.borrow_mut().put(k, Obj::Dict(d));
        }
    }
    Ok(())
}

fn op_startdata(it: &mut Interp) -> Res {
    // CFF font data: `(Binary) len StartData <bytes>`: skipped.
    let n = it.pop_int()?;
    it.pop()?;
    if let Some(f) = it.files.last().cloned() {
        it.read_bytes(&f, n.max(0) as usize)?;
    }
    it.type1_skipped += 1;
    Ok(())
}

fn transform_op(it: &mut Interp, inverse: bool, delta: bool) -> Res {
    let m = if let Some(Obj::Arr(_)) = it.top() {
        it.pop_matrix()?
    } else {
        it.g.ctm
    };
    let (x, y) = it.pop_point()?;
    let m = if inverse {
        invert(&m).ok_or_else(|| err("undefinedresult", ""))?
    } else {
        m
    };
    let (a, b) = if delta {
        dapply(&m, x, y)
    } else {
        apply(&m, x, y)
    };
    it.push(Obj::Real(a));
    it.push(Obj::Real(b));
    Ok(())
}

/// `translate` / `scale` / `rotate`: on the CTM, or, given a matrix, into it.
fn ctm_op(it: &mut Interp, make: fn(&mut Interp) -> Res<Matrix>) -> Res {
    if let Some(Obj::Arr(_)) = it.top() {
        let a = it.pop_arr()?;
        let m = make(it)?;
        {
            let mut b = a.buf.borrow_mut();
            for (k, v) in m.iter().enumerate() {
                if let Some(slot) = b.get_mut(a.start + k) {
                    *slot = Obj::Real(*v);
                }
            }
        }
        it.push(Obj::Arr(a));
        return Ok(());
    }
    let m = make(it)?;
    it.g.ctm = mul(&m, &it.g.ctm);
    Ok(())
}

fn set_matrix_into(a: &PsArr, m: &Matrix) -> Res {
    if a.len != 6 {
        return Err(err("rangecheck", ""));
    }
    let mut b = a.buf.borrow_mut();
    for (k, v) in m.iter().enumerate() {
        if let Some(slot) = b.get_mut(a.start + k) {
            *slot = Obj::Real(*v);
        }
    }
    Ok(())
}

fn op_image(it: &mut Interp) -> Res {
    if let Some(Obj::Dict(_)) = it.top() {
        let d = it.pop_dict()?;
        return it.image_dict(&d, None);
    }
    let src = it.pop()?;
    let m = it.pop_matrix()?;
    let bpc = it.pop_int()?;
    let h = it.pop_int()?;
    let w = it.pop_int()?;
    it.image(w, h, bpc, m, vec![src], false, CSpace::Gray, None, None)
}

fn op_colorimage(it: &mut Interp) -> Res {
    let ncomp = it.pop_int()?;
    let multi = it.pop_bool()?;
    let cs = match ncomp {
        1 => CSpace::Gray,
        3 => CSpace::Rgb,
        4 => CSpace::Cmyk,
        _ => return Err(err("rangecheck", "colorimage takes 1, 3 or 4 components")),
    };
    let n = if multi { ncomp as usize } else { 1 };
    if it.ostack.len() < n + 4 {
        return Err(err("stackunderflow", ""));
    }
    let sources = it.ostack.split_off(it.ostack.len() - n);
    let m = it.pop_matrix()?;
    let bpc = it.pop_int()?;
    let h = it.pop_int()?;
    let w = it.pop_int()?;
    it.image(w, h, bpc, m, sources, multi, cs, None, None)
}

fn op_imagemask(it: &mut Interp) -> Res {
    if let Some(Obj::Dict(_)) = it.top() {
        let d = it.pop_dict()?;
        return it.image_dict(&d, Some(false));
    }
    let src = it.pop()?;
    let m = it.pop_matrix()?;
    let polarity = it.pop_bool()?;
    let h = it.pop_int()?;
    let w = it.pop_int()?;
    // Polarity true paints the 1 bits: the Decode array [1 0].
    let decode = Some(if polarity {
        vec![1.0, 0.0]
    } else {
        vec![0.0, 1.0]
    });
    it.image(
        w,
        h,
        1,
        m,
        vec![src],
        false,
        CSpace::Gray,
        decode,
        Some(polarity),
    )
}

fn op_show_family(it: &mut Interp, kind: u8) -> Res {
    // 0 show, 1 ashow, 2 widthshow, 3 awidthshow
    let s = it.pop_str()?.bytes();
    let (mut extra, mut special) = ((0.0, 0.0), None);
    match kind {
        1 => {
            let (ax, ay) = it.pop_point()?;
            extra = (ax, ay);
        }
        2 => {
            let c = it.pop_int()?;
            let (cx, cy) = it.pop_point()?;
            special = Some((c, cx, cy));
        }
        3 => {
            let (ax, ay) = it.pop_point()?;
            let c = it.pop_int()?;
            let (cx, cy) = it.pop_point()?;
            extra = (ax, ay);
            special = Some((c, cx, cy));
        }
        _ => {}
    }
    match special {
        None => it.show(&s, extra, None),
        Some((c, cx, cy)) => {
            let font = it.font()?;
            let (ux, uy) = dapply(&font.matrix, font.units * ADVANCE_EM, 0.0);
            let per: Vec<(f64, f64)> = s
                .iter()
                .map(|b| {
                    let mut d = (ux + extra.0, uy + extra.1);
                    if i64::from(*b) == c {
                        d = (d.0 + cx, d.1 + cy);
                    }
                    d
                })
                .collect();
            it.show(&s, (0.0, 0.0), Some(&per))
        }
    }
}

fn op_xyshow(it: &mut Interp, which: u8) -> Res {
    // 0 xshow, 1 yshow, 2 xyshow
    let nums = it.pop()?;
    let s = it.pop_str()?.bytes();
    let list: Vec<f64> = match nums {
        Obj::Arr(a) => a.items().iter().filter_map(obj_num).collect(),
        _ => return Err(err("typecheck", "")),
    };
    let per: Vec<(f64, f64)> = (0..s.len())
        .map(|i| match which {
            0 => (list.get(i).copied().unwrap_or(0.0), 0.0),
            1 => (0.0, list.get(i).copied().unwrap_or(0.0)),
            _ => (
                list.get(2 * i).copied().unwrap_or(0.0),
                list.get(2 * i + 1).copied().unwrap_or(0.0),
            ),
        })
        .collect();
    it.show(&s, (0.0, 0.0), Some(&per))
}

fn noop(_: &mut Interp) -> Res {
    Ok(())
}

fn pop1(it: &mut Interp) -> Res {
    it.pop().map(|_| ())
}

fn pop2(it: &mut Interp) -> Res {
    it.pop()?;
    it.pop().map(|_| ())
}

fn skip_report(it: &mut Interp, what: &'static str, operands: usize) -> Res {
    it.skipped.insert(what);
    for _ in 0..operands {
        it.pop()?;
    }
    Ok(())
}

fn op_setcolorspace(it: &mut Interp) -> Res {
    let o = it.pop()?;
    let cs = it.parse_cspace(&o)?;
    let init: Vec<f64> = match &cs {
        CSpace::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
        CSpace::Tint { n, .. } => vec![1.0; *n],
        other => vec![0.0; other.ncomp()],
    };
    it.g.cspace = cs;
    if matches!(it.g.cspace, CSpace::Pattern) {
        it.g.color = None;
        return Ok(());
    }
    it.set_comps(init)
}

fn op_setcolor(it: &mut Interp) -> Res {
    if matches!(it.g.cspace, CSpace::Pattern) {
        // A pattern dictionary (and, for uncoloured patterns, components).
        while let Some(o) = it.top() {
            if matches!(o, Obj::Dict(_)) {
                it.pop()?;
                break;
            }
            it.pop()?;
        }
        it.g.color = None;
        it.skipped.insert("pattern fills and strokes");
        return Ok(());
    }
    let n = it.g.cspace.ncomp();
    let mut c = vec![0.0; n];
    for i in (0..n).rev() {
        c[i] = it.pop_num()?;
    }
    it.set_comps(c)
}

fn op_setgray(it: &mut Interp) -> Res {
    let g = it.pop_num()?;
    it.g.cspace = CSpace::Gray;
    it.set_comps(vec![g])
}

fn op_setrgbcolor(it: &mut Interp) -> Res {
    let b = it.pop_num()?;
    let g = it.pop_num()?;
    let r = it.pop_num()?;
    it.g.cspace = CSpace::Rgb;
    it.set_comps(vec![r, g, b])
}

fn op_setcmykcolor(it: &mut Interp) -> Res {
    let k = it.pop_num()?;
    let y = it.pop_num()?;
    let m = it.pop_num()?;
    let c = it.pop_num()?;
    it.g.cspace = CSpace::Cmyk;
    it.set_comps(vec![c, m, y, k])
}

fn op_sethsbcolor(it: &mut Interp) -> Res {
    let b = it.pop_num()?;
    let s = it.pop_num()?;
    let h = it.pop_num()?;
    let rgb = hsb_rgb(h, s.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
    it.g.cspace = CSpace::Rgb;
    it.set_comps(rgb.to_vec())
}

fn op_setcustomcolor(it: &mut Interp) -> Res {
    let tint = it.pop_num()?;
    let a = it.pop_arr()?;
    let v: Vec<f64> = a.items().iter().take(4).filter_map(obj_num).collect();
    if v.len() < 4 {
        return Err(err("rangecheck", ""));
    }
    it.g.cspace = CSpace::Cmyk;
    it.set_comps(v.iter().map(|c| c * tint).collect())
}

fn op_findcmykcustomcolor(it: &mut Interp) -> Res {
    let name = it.pop()?;
    let k = it.pop()?;
    let y = it.pop()?;
    let m = it.pop()?;
    let c = it.pop()?;
    let a = it.new_arr(vec![c, m, y, k, name], false)?;
    it.push(a);
    Ok(())
}

fn op_currentcolor_rgb(it: &mut Interp) -> Res {
    let c = it.g.color.unwrap_or([0.0; 3]);
    for v in c {
        it.push(Obj::Real(v));
    }
    Ok(())
}

fn op_currentgray(it: &mut Interp) -> Res {
    let c = it.g.color.unwrap_or([0.0; 3]);
    it.push(Obj::Real(0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]));
    Ok(())
}

fn op_currentcmyk(it: &mut Interp) -> Res {
    let c = it.g.color.unwrap_or([0.0; 3]);
    let k = 1.0 - c[0].max(c[1]).max(c[2]);
    let f = |v: f64| {
        if k >= 1.0 {
            0.0
        } else {
            (1.0 - v - k) / (1.0 - k)
        }
    };
    for v in [f(c[0]), f(c[1]), f(c[2]), k] {
        it.push(Obj::Real(v));
    }
    Ok(())
}

fn op_gsave(it: &mut Interp) -> Res {
    if it.gstack.len() >= MAX_GSAVE {
        return Err(err("limitcheck", "gsave nests too deep"));
    }
    it.alloc(256)?;
    let g = it.g.clone();
    it.gstack.push(g);
    Ok(())
}

fn op_grestore(it: &mut Interp) -> Res {
    if let Some(top) = it.gstack.last() {
        // `grestore` does not pop the state `save` pushed.
        if top.save_level > 0 && it.gstack.len() == top.save_level {
            it.g = top.clone();
            return Ok(());
        }
    }
    if let Some(g) = it.gstack.pop() {
        it.g = g;
    }
    Ok(())
}

fn op_save(it: &mut Interp) -> Res {
    if it.gstack.len() >= MAX_GSAVE {
        return Err(err("limitcheck", "save nests too deep"));
    }
    let mut g = it.g.clone();
    g.save_level = it.gstack.len() + 1;
    it.gstack.push(g);
    it.push(Obj::Save(it.gstack.len()));
    Ok(())
}

fn op_restore(it: &mut Interp) -> Res {
    let level = match it.pop()? {
        Obj::Save(l) => l,
        _ => return Err(err("typecheck", "")),
    };
    if level == 0 || level > it.gstack.len() {
        return Err(err("invalidrestore", ""));
    }
    it.gstack.truncate(level);
    if let Some(mut g) = it.gstack.pop() {
        g.save_level = 0;
        it.g = g;
    }
    Ok(())
}

fn op_setdash(it: &mut Interp) -> Res {
    let off = it.pop_num()?;
    let a = it.pop_arr()?;
    let list: Vec<f64> = a.items().iter().filter_map(obj_num).collect();
    it.g.dash = (list, off);
    Ok(())
}

fn op_currentdash(it: &mut Interp) -> Res {
    let (list, off) = it.g.dash.clone();
    let a = it.new_arr(list.into_iter().map(Obj::Real).collect(), false)?;
    it.push(a);
    it.push(Obj::Real(off));
    Ok(())
}

fn op_pathbbox(it: &mut Interp) -> Res {
    let inv = invert(&it.g.ctm).ok_or_else(|| err("undefinedresult", ""))?;
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut add = |x: f64, y: f64| {
        let (ux, uy) = apply(&inv, x, y);
        b[0] = b[0].min(ux);
        b[1] = b[1].min(uy);
        b[2] = b[2].max(ux);
        b[3] = b[3].max(uy);
    };
    for s in &it.g.path {
        match *s {
            Seg::M(x, y) | Seg::L(x, y) => add(x, y),
            Seg::C(a, c, d, e, x, y) => {
                add(a, c);
                add(d, e);
                add(x, y);
            }
            Seg::Z => {}
        }
    }
    if !b[0].is_finite() {
        return Err(err("nocurrentpoint", ""));
    }
    for v in b {
        it.push(Obj::Real(v));
    }
    Ok(())
}

fn op_pathforall(it: &mut Interp) -> Res {
    let close = it.pop_proc()?;
    let curve = it.pop_proc()?;
    let line = it.pop_proc()?;
    let mv = it.pop_proc()?;
    let inv = invert(&it.g.ctm).ok_or_else(|| err("undefinedresult", ""))?;
    let segs = it.g.path.clone();
    let r = (|| -> Res {
        for s in segs {
            it.tick()?;
            match s {
                Seg::M(x, y) | Seg::L(x, y) => {
                    let (ux, uy) = apply(&inv, x, y);
                    it.push(Obj::Real(ux));
                    it.push(Obj::Real(uy));
                    it.call(if matches!(s, Seg::M(..)) { &mv } else { &line })?;
                }
                Seg::C(a, b, c, d, x, y) => {
                    for (px, py) in [(a, b), (c, d), (x, y)] {
                        let (ux, uy) = apply(&inv, px, py);
                        it.push(Obj::Real(ux));
                        it.push(Obj::Real(uy));
                    }
                    it.call(&curve)?;
                }
                Seg::Z => it.call(&close)?,
            }
        }
        Ok(())
    })();
    loop_end(r)
}

fn op_clippath(it: &mut Interp) -> Res {
    let (w, h) = it.page;
    it.g.path = vec![
        Seg::M(0.0, 0.0),
        Seg::L(w, 0.0),
        Seg::L(w, h),
        Seg::L(0.0, h),
        Seg::Z,
    ];
    it.g.cp = Some((0.0, 0.0));
    Ok(())
}

fn op_findresource(it: &mut Interp) -> Res {
    let cat = it.pop()?;
    let key = it.pop()?;
    let cat = String::from_utf8_lossy(&text_of(&cat)).into_owned();
    match cat.as_str() {
        "Font" => {
            let f = it.find_font(&key)?;
            it.push(f);
        }
        "Encoding" => {
            let e = it
                .systemdict
                .borrow()
                .get(&Key::Name("StandardEncoding".into()))
                .unwrap_or(Obj::Null);
            it.push(e);
        }
        "ColorSpace" | "Pattern" | "Form" | "Halftone" => {
            return Err(err("undefinedresource", cat));
        }
        _ => {
            let stored = {
                let k = it.key(&key)?;
                it.userdict
                    .borrow()
                    .get(&Key::Name(format!(".resource.{cat}").into()))
                    .and_then(|o| match o {
                        Obj::Dict(d) => d.borrow().get(&k),
                        _ => None,
                    })
            };
            match stored {
                Some(v) => it.push(v),
                None => {
                    // A ProcSet this interpreter has not got (the CFF
                    // `FontSetInit`): an empty dictionary stands in.
                    it.alloc(64)?;
                    let d = it.new_dict();
                    it.push(Obj::Dict(d));
                }
            }
        }
    }
    Ok(())
}

fn op_defineresource(it: &mut Interp) -> Res {
    let cat = it.pop()?;
    let inst = it.pop()?;
    let key = it.pop()?;
    let cat = String::from_utf8_lossy(&text_of(&cat)).into_owned();
    if cat == "Font" {
        let k = it.key(&key)?;
        it.fontdir.borrow_mut().put(k, inst.clone());
    } else {
        let slot = Key::Name(format!(".resource.{cat}").into());
        let existing = it.userdict.borrow().get(&slot);
        let d = match existing {
            Some(Obj::Dict(d)) => d,
            _ => {
                let d = it.new_dict();
                it.userdict.borrow_mut().put(slot, Obj::Dict(d.clone()));
                d
            }
        };
        let k = it.key(&key)?;
        d.borrow_mut().put(k, inst.clone());
        it.alloc(64)?;
    }
    it.push(inst);
    Ok(())
}

fn op_resourcestatus(it: &mut Interp) -> Res {
    it.pop()?;
    it.pop()?;
    it.push(Obj::Bool(false));
    Ok(())
}

fn push_matrix(it: &mut Interp, m: Matrix) -> Res {
    let a = it.pop_arr()?;
    set_matrix_into(&a, &m)?;
    it.push(Obj::Arr(a));
    Ok(())
}

ops! {
    // stack
    "pop" => pop1,
    "exch" => |it| { let b = it.pop()?; let a = it.pop()?; it.push(b); it.push(a); Ok(()) },
    "dup" => |it| { let a = it.top().cloned().ok_or_else(|| err("stackunderflow", ""))?; it.push(a); Ok(()) },
    "copy" => op_copy,
    "index" => op_index,
    "roll" => op_roll,
    "clear" => |it| { it.ostack.clear(); Ok(()) },
    "count" => |it| { let n = it.ostack.len() as i64; it.push(int_or_real(n)); Ok(()) },
    "mark" => |it| { it.push(Obj::Mark); Ok(()) },
    "[" => |it| { it.push(Obj::Mark); Ok(()) },
    "<<" => |it| { it.push(Obj::Mark); Ok(()) },
    "]" => op_close_array,
    ">>" => op_close_dict,
    "cleartomark" => op_cleartomark,
    "counttomark" => op_counttomark,
    // arithmetic
    "add" => |it| arith(it, |a, b| Some(a + b), |a, b| a + b),
    "sub" => |it| arith(it, |a, b| Some(a - b), |a, b| a - b),
    "mul" => |it| arith(it, |a, b| Some(a * b), |a, b| a * b),
    "div" => |it| {
        let b = it.pop_num()?; let a = it.pop_num()?;
        if b == 0.0 { return Err(err("undefinedresult", "div by zero")); }
        it.push(Obj::Real(a / b)); Ok(())
    },
    "idiv" => |it| {
        let b = it.pop_int()?; let a = it.pop_int()?;
        if b == 0 { return Err(err("undefinedresult", "idiv by zero")); }
        it.push(int_or_real(a / b)); Ok(())
    },
    "mod" => |it| {
        let b = it.pop_int()?; let a = it.pop_int()?;
        if b == 0 { return Err(err("undefinedresult", "mod by zero")); }
        it.push(int_or_real(a % b)); Ok(())
    },
    "neg" => |it| unary(it, |a| -a, |a| -a),
    "abs" => |it| unary(it, |a| a.abs(), f64::abs),
    "ceiling" => |it| unary(it, |a| a, f64::ceil),
    "floor" => |it| unary(it, |a| a, f64::floor),
    "round" => |it| unary(it, |a| a, |a| (a + 0.5).floor()),
    "truncate" => |it| unary(it, |a| a, f64::trunc),
    "sqrt" => |it| real_op(it, f64::sqrt),
    "ln" => |it| real_op(it, f64::ln),
    "log" => |it| real_op(it, f64::log10),
    "sin" => |it| real_op(it, |a| a.to_radians().sin()),
    "cos" => |it| real_op(it, |a| a.to_radians().cos()),
    "atan" => |it| {
        let den = it.pop_num()?; let n = it.pop_num()?;
        if n == 0.0 && den == 0.0 { return Err(err("undefinedresult", "")); }
        it.push(Obj::Real(n.atan2(den).to_degrees().rem_euclid(360.0))); Ok(())
    },
    "exp" => |it| {
        let e = it.pop_num()?; let b = it.pop_num()?;
        let v = b.powf(e);
        if !v.is_finite() { return Err(err("undefinedresult", "")); }
        it.push(Obj::Real(v)); Ok(())
    },
    "rand" => |it| {
        it.rand = it.rand.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let v = i64::from(it.rand >> 1);
        it.push(Obj::Int(v)); Ok(())
    },
    "srand" => |it| { it.rand = it.pop_int()? as u32; Ok(()) },
    "rrand" => |it| { let v = i64::from(it.rand >> 1); it.push(Obj::Int(v)); Ok(()) },
    "cvi" => op_cvi,
    "cvr" => op_cvr,
    // relational / logical
    "eq" => |it| { let b = it.pop()?; let a = it.pop()?; it.push(Obj::Bool(obj_eq(&a, &b))); Ok(()) },
    "ne" => |it| { let b = it.pop()?; let a = it.pop()?; it.push(Obj::Bool(!obj_eq(&a, &b))); Ok(()) },
    "gt" => |it| compare(it, |o| o.is_gt()),
    "ge" => |it| compare(it, |o| o.is_ge()),
    "lt" => |it| compare(it, |o| o.is_lt()),
    "le" => |it| compare(it, |o| o.is_le()),
    "and" => |it| logic(it, |a, b| a && b, |a, b| a & b),
    "or" => |it| logic(it, |a, b| a || b, |a, b| a | b),
    "xor" => |it| logic(it, |a, b| a ^ b, |a, b| a ^ b),
    "not" => |it| {
        let r = match it.pop()? {
            Obj::Bool(b) => Obj::Bool(!b),
            Obj::Int(i) => int_or_real(i64::from(!(i as i32))),
            _ => return Err(err("typecheck", "")),
        };
        it.push(r); Ok(())
    },
    "bitshift" => |it| {
        let s = it.pop_int()?; let v = it.pop_int()? as i32 as u32;
        let r = if s >= 32 || s <= -32 { 0 } else if s >= 0 { v << s } else { v >> -s };
        it.push(Obj::Int(i64::from(r as i32))); Ok(())
    },
    // control
    "exec" => op_exec,
    "if" => op_if,
    "ifelse" => op_ifelse,
    "for" => op_for,
    "repeat" => op_repeat,
    "loop" => op_loop,
    "forall" => op_forall,
    "exit" => |_| Err(Flow::Exit),
    "stop" => |_| Err(Flow::Stop),
    "stopped" => op_stopped,
    "quit" => |_| Err(Flow::Quit),
    "countexecstack" => |it| { let d = it.depth as i64; it.push(Obj::Int(d)); Ok(()) },
    // types
    "type" => |it| { let o = it.pop()?; it.push(Obj::Name(type_name(&o).into(), true)); Ok(()) },
    "cvx" => |it| { let o = it.pop()?; it.push(set_exec(o, true)); Ok(()) },
    "cvlit" => |it| { let o = it.pop()?; it.push(set_exec(o, false)); Ok(()) },
    "xcheck" => |it| {
        let o = it.pop()?;
        let x = matches!(o, Obj::Name(_, true) | Obj::Op(_)) || matches!(&o, Obj::Arr(a) if a.exec) || matches!(&o, Obj::Str(s) if s.exec);
        it.push(Obj::Bool(x)); Ok(())
    },
    "rcheck" => |it| { it.pop()?; it.push(Obj::Bool(true)); Ok(()) },
    "wcheck" => |it| { it.pop()?; it.push(Obj::Bool(true)); Ok(()) },
    "readonly" => noop,
    "executeonly" => noop,
    "noaccess" => noop,
    "cvn" => op_cvn,
    "cvs" => op_cvs,
    "cvrs" => |it| {
        let dst = it.pop_str()?; let radix = it.pop_int()?; let n = it.pop_num()?;
        let text = if radix == 10 { cvs_text(&Obj::Real(n)) } else {
            let v = n as i64 as u32;
            match radix { 2 => format!("{v:b}"), 8 => format!("{v:o}"), 16 => format!("{v:X}"), _ => v.to_string() }.into_bytes()
        };
        if text.len() > dst.len { return Err(err("rangecheck", "")); }
        fill_str(&dst, &text);
        it.push(Obj::Str(PsStr { len: text.len(), ..dst })); Ok(())
    },
    "token" => op_token,
    // dictionaries
    "dict" => op_dict,
    "begin" => op_begin,
    "end" => op_end,
    "def" => op_def,
    "load" => op_load,
    "store" => op_store,
    "where" => op_where,
    "known" => op_known,
    "undef" => op_undef,
    "currentdict" => op_currentdict,
    "countdictstack" => |it| { let n = it.dstack.len() as i64; it.push(Obj::Int(n)); Ok(()) },
    "maxlength" => |it| { let d = it.pop_dict()?; let n = d.borrow().entries.len() as i64 + 16; it.push(int_or_real(n)); Ok(()) },
    "bind" => |it| {
        if let Some(Obj::Arr(a)) = it.top().cloned() { bind_proc(it, &a)?; }
        Ok(())
    },
    // arrays and strings
    "array" => op_array,
    "packedarray" => |it| {
        let n = it.pop_int()?;
        if n < 0 || n as usize > it.ostack.len() { return Err(err("rangecheck", "")); }
        let items = it.ostack.split_off(it.ostack.len() - n as usize);
        let a = it.new_arr(items, false)?; it.push(a); Ok(())
    },
    "string" => op_string,
    "length" => op_length,
    "get" => op_get,
    "put" => op_put,
    "getinterval" => op_getinterval,
    "putinterval" => op_putinterval,
    "aload" => op_aload,
    "astore" => op_astore,
    "search" => |it| op_search(it, false),
    "anchorsearch" => |it| op_search(it, true),
    // files
    "currentfile" => |it| {
        let f = it.files.last().cloned().unwrap_or_else(|| it.main.clone());
        it.push(Obj::File(f)); Ok(())
    },
    "readhexstring" => op_readhexstring,
    "readstring" => op_readstring,
    "readline" => op_readline,
    "read" => op_read,
    "closefile" => |it| {
        if let Obj::File(f) = it.pop()? {
            let is_main = Rc::ptr_eq(&f, &it.main);
            if !is_main { f.borrow_mut().closed = true; }
        }
        Ok(())
    },
    "flushfile" => |it| {
        // An input filter is read to its end (cairo flushes its ASCII85
        // image data this way); the program itself is left where it is.
        if let Obj::File(f) = it.pop()? {
            if !Rc::ptr_eq(&f, &it.main) && !matches!(f.borrow().src, Src::Mem { .. }) {
                while it.read_byte(&f)?.is_some() { it.tick()?; }
            }
        }
        Ok(())
    },
    "status" => |it| {
        let open = match it.pop()? { Obj::File(f) => !f.borrow().closed, _ => false };
        it.push(Obj::Bool(open)); Ok(())
    },
    "filter" => op_filter,
    "eexec" => op_eexec,
    "StartData" => op_startdata,
    "file" => |it| { it.skipped.insert("file access (refused)"); Err(err("invalidfileaccess", "file")) },
    "run" => |it| { it.skipped.insert("file access (refused)"); Err(err("invalidfileaccess", "run")) },
    "deletefile" => |it| { it.skipped.insert("file access (refused)"); Err(err("invalidfileaccess", "deletefile")) },
    "renamefile" => |it| { it.skipped.insert("file access (refused)"); Err(err("invalidfileaccess", "renamefile")) },
    "print" => pop1,
    "=" => pop1,
    "==" => pop1,
    "=only" => pop1,
    "==only" => pop1,
    "pstack" => noop,
    "stack" => noop,
    "flush" => noop,
    // environment
    "languagelevel" => |it| { it.push(Obj::Int(2)); Ok(()) },
    "version" => |it| { let s = it.new_str(b"3010".to_vec())?; it.push(s); Ok(()) },
    "product" => |it| { let s = it.new_str(b"Raster Studio EPS".to_vec())?; it.push(s); Ok(()) },
    "revision" => |it| { it.push(Obj::Int(1)); Ok(()) },
    "serialnumber" => |it| { it.push(Obj::Int(0)); Ok(()) },
    "usertime" => |it| { let t = it.started.elapsed().as_millis() as i64; it.push(int_or_real(t)); Ok(()) },
    "realtime" => |it| { let t = it.started.elapsed().as_millis() as i64; it.push(int_or_real(t)); Ok(()) },
    "vmstatus" => |it| { let v = (it.vm.min(1 << 30)) as i64; it.push(Obj::Int(0)); it.push(Obj::Int(v)); it.push(Obj::Int(1 << 30)); Ok(()) },
    "vmreclaim" => pop1,
    "setvmthreshold" => pop1,
    "setglobal" => pop1,
    "currentglobal" => |it| { it.push(Obj::Bool(false)); Ok(()) },
    "gcheck" => |it| { it.pop()?; it.push(Obj::Bool(false)); Ok(()) },
    "setpacking" => pop1,
    "currentpacking" => |it| { it.push(Obj::Bool(false)); Ok(()) },
    "setshared" => pop1,
    "setobjectformat" => pop1,
    "setsystemparams" => pop1,
    "setuserparams" => pop1,
    "currentuserparams" => |it| { let d = it.new_dict(); it.push(Obj::Dict(d)); Ok(()) },
    "currentsystemparams" => |it| { let d = it.new_dict(); it.push(Obj::Dict(d)); Ok(()) },
    "setpagedevice" => pop1,
    "currentpagedevice" => |it| { let d = it.new_dict(); it.push(Obj::Dict(d)); Ok(()) },
    "showpage" => noop,
    "copypage" => noop,
    "erasepage" => noop,
    "pdfmark" => op_cleartomark,
    "findresource" => op_findresource,
    "defineresource" => op_defineresource,
    "undefineresource" => pop2,
    "resourcestatus" => op_resourcestatus,
    "resourceforall" => |it| { for _ in 0..4 { it.pop()?; } Ok(()) },
    "findencoding" => |it| { it.pop()?; let e = it.systemdict.borrow().get(&Key::Name("StandardEncoding".into())).unwrap_or(Obj::Null); it.push(e); Ok(()) },
    "save" => op_save,
    "restore" => op_restore,
    // graphics state
    "gsave" => op_gsave,
    "grestore" => op_grestore,
    "grestoreall" => |it| {
        while !it.gstack.is_empty() { op_grestore(it)?; if it.gstack.last().is_some_and(|g| g.save_level > 0) { break; } }
        Ok(())
    },
    "initgraphics" => |it| { let ctm = it.initial_ctm; let font = it.g.font.take(); it.g = GState::new(ctm); it.g.font = font; Ok(()) },
    "gstate" => |it| { let d = it.new_dict(); it.push(Obj::Dict(d)); Ok(()) },
    "currentgstate" => noop,
    "setgstate" => pop1,
    "setlinewidth" => |it| { it.g.line_width = it.pop_num()?; Ok(()) },
    "currentlinewidth" => |it| { let w = it.g.line_width; it.push(Obj::Real(w)); Ok(()) },
    "setlinecap" => |it| { it.g.cap = it.pop_int()?; Ok(()) },
    "currentlinecap" => |it| { let c = it.g.cap; it.push(Obj::Int(c)); Ok(()) },
    "setlinejoin" => |it| { it.g.join = it.pop_int()?; Ok(()) },
    "currentlinejoin" => |it| { let c = it.g.join; it.push(Obj::Int(c)); Ok(()) },
    "setmiterlimit" => |it| { it.g.miter = it.pop_num()?; Ok(()) },
    "currentmiterlimit" => |it| { let m = it.g.miter; it.push(Obj::Real(m)); Ok(()) },
    "setdash" => op_setdash,
    "currentdash" => op_currentdash,
    "setflat" => pop1,
    "currentflat" => |it| { it.push(Obj::Real(1.0)); Ok(()) },
    "setstrokeadjust" => pop1,
    "currentstrokeadjust" => |it| { it.push(Obj::Bool(false)); Ok(()) },
    "setoverprint" => pop1,
    "currentoverprint" => |it| { it.push(Obj::Bool(false)); Ok(()) },
    "setsmoothness" => pop1,
    "setscreen" => |it| { for _ in 0..3 { it.pop()?; } Ok(()) },
    "currentscreen" => |it| { it.push(Obj::Real(60.0)); it.push(Obj::Real(45.0)); let p = it.new_arr(Vec::new(), true)?; it.push(p); Ok(()) },
    "setcolorscreen" => |it| { for _ in 0..12 { it.pop()?; } Ok(()) },
    "sethalftone" => pop1,
    "currenthalftone" => |it| { let d = it.new_dict(); it.push(Obj::Dict(d)); Ok(()) },
    "settransfer" => pop1,
    "currenttransfer" => |it| { let p = it.new_arr(Vec::new(), true)?; it.push(p); Ok(()) },
    "setcolortransfer" => |it| { for _ in 0..4 { it.pop()?; } Ok(()) },
    "setblackgeneration" => pop1,
    "setundercolorremoval" => pop1,
    "setcolorrendering" => pop1,
    "setrenderingintent" => pop1,
    // colour
    "setgray" => op_setgray,
    "setrgbcolor" => op_setrgbcolor,
    "setcmykcolor" => op_setcmykcolor,
    "sethsbcolor" => op_sethsbcolor,
    "setcolorspace" => op_setcolorspace,
    "setcolor" => op_setcolor,
    "setpattern" => |it| { it.pop()?; it.g.cspace = CSpace::Pattern; it.g.color = None; it.skipped.insert("pattern fills and strokes"); Ok(()) },
    "makepattern" => |it| { it.pop()?; Ok(()) },
    "setcustomcolor" => op_setcustomcolor,
    "findcmykcustomcolor" => op_findcmykcustomcolor,
    "currentgray" => op_currentgray,
    "currentrgbcolor" => op_currentcolor_rgb,
    "currentcmykcolor" => op_currentcmyk,
    "currentcolorspace" => |it| { let a = it.new_arr(vec![Obj::Name("DeviceRGB".into(), false)], false)?; it.push(a); Ok(()) },
    "currentcolor" => op_currentcolor_rgb,
    // matrices
    "matrix" => |it| { let m = it.matrix_obj(IDENTITY)?; it.push(m); Ok(()) },
    "identmatrix" => |it| push_matrix(it, IDENTITY),
    "currentmatrix" => |it| { let m = it.g.ctm; push_matrix(it, m) },
    "defaultmatrix" => |it| { let m = it.initial_ctm; push_matrix(it, m) },
    "setmatrix" => |it| { it.g.ctm = it.pop_matrix()?; Ok(()) },
    "initmatrix" => |it| { it.g.ctm = it.initial_ctm; Ok(()) },
    "concat" => |it| { let m = it.pop_matrix()?; it.g.ctm = mul(&m, &it.g.ctm); Ok(()) },
    "concatmatrix" => |it| {
        let dst = it.pop_arr()?; let b = it.pop_matrix()?; let a = it.pop_matrix()?;
        set_matrix_into(&dst, &mul(&a, &b))?; it.push(Obj::Arr(dst)); Ok(())
    },
    "invertmatrix" => |it| {
        let dst = it.pop_arr()?; let a = it.pop_matrix()?;
        let inv = invert(&a).ok_or_else(|| err("undefinedresult", ""))?;
        set_matrix_into(&dst, &inv)?; it.push(Obj::Arr(dst)); Ok(())
    },
    "translate" => |it| ctm_op(it, |it| { let (x, y) = it.pop_point()?; Ok([1.0, 0.0, 0.0, 1.0, x, y]) }),
    "scale" => |it| ctm_op(it, |it| { let (x, y) = it.pop_point()?; Ok([x, 0.0, 0.0, y, 0.0, 0.0]) }),
    "rotate" => |it| ctm_op(it, |it| {
        let a = it.pop_num()?.to_radians();
        Ok([a.cos(), a.sin(), -a.sin(), a.cos(), 0.0, 0.0])
    }),
    "transform" => |it| transform_op(it, false, false),
    "itransform" => |it| transform_op(it, true, false),
    "dtransform" => |it| transform_op(it, false, true),
    "idtransform" => |it| transform_op(it, true, true),
    // paths
    "newpath" => |it| { it.g.path.clear(); it.g.cp = None; Ok(()) },
    "moveto" => |it| { let (x, y) = it.pop_point()?; let (dx, dy) = apply(&it.g.ctm, x, y); it.moveto_dev(dx, dy) },
    "rmoveto" => |it| {
        let (x, y) = it.pop_point()?;
        let (cx, cy) = it.g.cp.ok_or_else(|| err("nocurrentpoint", ""))?;
        let (dx, dy) = dapply(&it.g.ctm, x, y);
        it.moveto_dev(cx + dx, cy + dy)
    },
    "lineto" => |it| { let (x, y) = it.pop_point()?; let (dx, dy) = apply(&it.g.ctm, x, y); it.lineto_dev(dx, dy) },
    "rlineto" => |it| {
        let (x, y) = it.pop_point()?;
        let (cx, cy) = it.g.cp.ok_or_else(|| err("nocurrentpoint", ""))?;
        let (dx, dy) = dapply(&it.g.ctm, x, y);
        it.lineto_dev(cx + dx, cy + dy)
    },
    "curveto" => |it| {
        let p3 = it.pop_point()?; let p2 = it.pop_point()?; let p1 = it.pop_point()?;
        it.curveto_user([p1, p2, p3])
    },
    "rcurveto" => |it| {
        let p3 = it.pop_point()?; let p2 = it.pop_point()?; let p1 = it.pop_point()?;
        let (ux, uy) = it.current_user()?;
        it.curveto_user([(ux + p1.0, uy + p1.1), (ux + p2.0, uy + p2.1), (ux + p3.0, uy + p3.1)])
    },
    "arc" => |it| {
        let a2 = it.pop_num()?; let a1 = it.pop_num()?; let r = it.pop_num()?; let (x, y) = it.pop_point()?;
        it.arc(x, y, r, a1, a2, true)
    },
    "arcn" => |it| {
        let a2 = it.pop_num()?; let a1 = it.pop_num()?; let r = it.pop_num()?; let (x, y) = it.pop_point()?;
        it.arc(x, y, r, a1, a2, false)
    },
    "arct" => |it| {
        let r = it.pop_num()?; let p2 = it.pop_point()?; let p1 = it.pop_point()?;
        it.arct(p1, p2, r).map(|_| ())
    },
    "arcto" => |it| {
        let r = it.pop_num()?; let p2 = it.pop_point()?; let p1 = it.pop_point()?;
        let t = it.arct(p1, p2, r)?;
        for v in t { it.push(Obj::Real(v)); }
        Ok(())
    },
    "closepath" => |it| {
        if it.g.cp.is_some() && !it.g.path.is_empty() {
            it.add_seg(Seg::Z)?;
            it.g.cp = it.g.start;
        }
        Ok(())
    },
    "currentpoint" => |it| { let (x, y) = it.current_user()?; it.push(Obj::Real(x)); it.push(Obj::Real(y)); Ok(()) },
    "pathbbox" => op_pathbbox,
    "pathforall" => op_pathforall,
    "flattenpath" => noop,
    "reversepath" => noop,
    "strokepath" => |it| skip_report(it, "strokepath (the outline of a stroke)", 0),
    "clippath" => op_clippath,
    "initclip" => |it| { it.g.clip.clear(); Ok(()) },
    "setbbox" => |it| { for _ in 0..4 { it.pop()?; } Ok(()) },
    "ucache" => noop,
    // painting
    "fill" => |it| it.fill(false),
    "eofill" => |it| it.fill(true),
    "stroke" => |it| it.stroke(),
    "clip" => |it| it.clip(false),
    "eoclip" => |it| it.clip(true),
    "rectfill" => |it| {
        for r in it.pop_rects()? { let s = it.rect_segs(r[0], r[1], r[2], r[3]); it.fill_segs(&s, false)?; }
        Ok(())
    },
    "rectstroke" => |it| {
        // `x y w h matrix rectstroke`: a trailing six-number array after
        // four numbers is a matrix (not applied: reported).
        let n = it.ostack.len();
        let matrix_form = n >= 5
            && matches!(it.ostack.last(), Some(Obj::Arr(a)) if a.len == 6)
            && matches!(it.ostack[n - 2], Obj::Int(_) | Obj::Real(_));
        if matrix_form {
            it.pop()?;
            it.skipped.insert("the matrix operand of rectstroke");
        }
        for r in it.pop_rects()? { let s = it.rect_segs(r[0], r[1], r[2], r[3]); it.stroke_segs(&s)?; }
        Ok(())
    },
    "rectclip" => |it| {
        let mut segs = Vec::new();
        for r in it.pop_rects()? { segs.extend(it.rect_segs(r[0], r[1], r[2], r[3])); }
        it.clip_segs(&segs, false)?;
        it.g.path.clear(); it.g.cp = None; Ok(())
    },
    "shfill" => |it| skip_report(it, "smooth shading (shfill)", 1),
    "image" => op_image,
    "colorimage" => op_colorimage,
    "imagemask" => op_imagemask,
    "execform" => |it| skip_report(it, "forms (execform)", 1),
    // fonts and text
    "findfont" => |it| { let n = it.pop()?; let f = it.find_font(&n)?; it.push(f); Ok(()) },
    "definefont" => |it| {
        let f = it.pop()?; let n = it.pop()?;
        let k = it.key(&n)?;
        if let Obj::Dict(d) = &f {
            let has_name = d.borrow().index.contains_key(&Key::Name("FontName".into()));
            if !has_name { d.borrow_mut().put(Key::Name("FontName".into()), key_obj(&k)); }
            d.borrow_mut().put(Key::Name("FID".into()), Obj::Int(0));
            let base = d.borrow().get(&Key::Name("FontMatrix".into()));
            let has_base = d.borrow().index.contains_key(&Key::Name(BASE_MATRIX.into()));
            if let (Some(Obj::Arr(a)), false) = (base, has_base) {
                let copy = it.new_arr(a.items(), false)?;
                d.borrow_mut().put(Key::Name(BASE_MATRIX.into()), copy);
            }
        }
        it.fontdir.borrow_mut().put(k, f.clone());
        it.push(f); Ok(())
    },
    "undefinefont" => pop1,
    "scalefont" => |it| {
        let s = it.pop_num()?; let d = it.pop_dict()?;
        let f = it.scaled_font(&d, &[s, 0.0, 0.0, s, 0.0, 0.0])?; it.push(f); Ok(())
    },
    "makefont" => |it| {
        let m = it.pop_matrix()?; let d = it.pop_dict()?;
        let f = it.scaled_font(&d, &m)?; it.push(f); Ok(())
    },
    "setfont" => |it| { let d = it.pop_dict()?; let f = it.font_of(&d); it.g.font = Some(f); Ok(()) },
    "selectfont" => |it| {
        let m = match it.pop()? {
            Obj::Arr(a) => arr_matrix(&a)?,
            o => { let s = obj_num(&o).ok_or_else(|| err("typecheck", ""))?; [s, 0.0, 0.0, s, 0.0, 0.0] }
        };
        let n = it.pop()?;
        let f = it.find_font(&n)?;
        let Obj::Dict(d) = f else { return Err(err("invalidfont", "")) };
        if let Obj::Dict(scaled) = it.scaled_font(&d, &m)? { let f = it.font_of(&scaled); it.g.font = Some(f); }
        Ok(())
    },
    "currentfont" => |it| {
        let f = it.g.font.clone();
        let name = f.as_ref().map(|f| f.name.clone()).unwrap_or_else(|| "Helvetica".into());
        let d = match it.find_font(&Obj::Name(name.as_str().into(), false))? { Obj::Dict(d) => d, _ => it.new_dict() };
        let m = f.map(|f| f.matrix).unwrap_or([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);
        let copy = it.scaled_font(&d, &IDENTITY)?;
        if let Obj::Dict(c) = &copy { let fm = it.matrix_obj(m)?; c.borrow_mut().put(Key::Name("FontMatrix".into()), fm); }
        it.push(copy); Ok(())
    },
    "rootfont" => |it| {
        let name = it.g.font.as_ref().map(|f| f.name.clone()).unwrap_or_else(|| "Helvetica".into());
        let f = it.find_font(&Obj::Name(name.as_str().into(), false))?; it.push(f); Ok(())
    },
    "show" => |it| op_show_family(it, 0),
    "ashow" => |it| op_show_family(it, 1),
    "widthshow" => |it| op_show_family(it, 2),
    "awidthshow" => |it| op_show_family(it, 3),
    "xshow" => |it| op_xyshow(it, 0),
    "yshow" => |it| op_xyshow(it, 1),
    "xyshow" => |it| op_xyshow(it, 2),
    "glyphshow" => |it| {
        let name = String::from_utf8_lossy(&text_of(&it.pop()?)).into_owned();
        match glyph_char(&name) {
            Some(c) => it.show_chars(&[c], (0.0, 0.0), None),
            None => { it.skipped.insert("glyphshow of a glyph name with no known character"); Ok(()) }
        }
    },
    "kshow" => |it| skip_report(it, "kshow text", 2),
    "cshow" => |it| skip_report(it, "cshow text", 2),
    "charpath" => |it| skip_report(it, "charpath (text as a path)", 2),
    "stringwidth" => |it| {
        let s = it.pop_str()?;
        let font = it.font()?;
        let (x, y) = dapply(&font.matrix, font.units * ADVANCE_EM * s.len as f64, 0.0);
        it.push(Obj::Real(x)); it.push(Obj::Real(y)); Ok(())
    },
    "setcachedevice" => |it| { for _ in 0..6 { it.pop()?; } Ok(()) },
    "setcachedevice2" => |it| { for _ in 0..10 { it.pop()?; } Ok(()) },
    "setcharwidth" => pop2,
    "setcachelimit" => pop1,
}

#[cfg(test)]
#[path = "postscript_tests.rs"]
mod tests;
