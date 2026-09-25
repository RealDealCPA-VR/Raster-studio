//! W13X-7: the layers of a Paint.NET `.pdn`.
//!
//! A `.pdn` is `PDN3`, a 24-bit little-endian length, an XML header (which
//! carries the flattened thumbnail [`super::decode_described`] opens when
//! nothing better can be read), the two bytes `00 01`, and then the
//! document as a .NET `BinaryFormatter` stream ([MS-NRBF]): a `Document`
//! whose `layers` list holds `BitmapLayer` objects, each with its
//! properties (name, visibility, opacity, background flag), its blend
//! operation, and a `Surface` whose pixels live in a `MemoryBlock`. The
//! blocks are serialised **deferred**: after the stream's `MessageEnd`
//! record, each block's bytes follow in the order the blocks' records
//! appear, as a format byte (`0` gzip chunks, `1` raw chunks), a chunk size
//! and then the chunks, each `chunk number, byte count, bytes` (big-endian
//! 32-bit numbers), in any order. The pixels are BGRA with straight alpha,
//! row by row at the surface's stride.
//!
//! [`read_layers`] parses the record stream generically (every MS-NRBF
//! record type a `BinaryFormatter` writes for a plain object graph), then
//! finds the document, its layers and their blocks by the members they
//! carry, not by assembly-qualified class names, so a class renamed between
//! Paint.NET versions is still found when its members are the same.
//!
//! What this was checked against: synthetic files written by
//! [`fixture::pdn_file`] to that layout. No file saved by Paint.NET itself
//! was available to this build's tests. A file this reader cannot follow
//! (the older layout whose whole stream is gzip-wrapped, a block stored
//! inline rather than deferred, a class shape it does not recognise) is an
//! error that names what was not understood; the caller then falls back to
//! the thumbnail and says why.
//!
//! # Blend modes
//!
//! Paint.NET 3.x names each layer's blend operation by class
//! (`UserBlendOps+MultiplyBlendOp`); a `blendMode` enum member is read by
//! its value in the same order (Normal, Multiply, Additive, Color Burn,
//! Color Dodge, Reflect, Glow, Overlay, Difference, Negation, Lighten,
//! Darken, Screen, Xor). [`PdnBlend`] is that list; the caller maps it onto
//! its own modes.
//!
//! # Untrusted input
//!
//! Every length and count is checked against the bytes that remain before
//! anything is allocated for it; the record nesting is limited to
//! [`MAX_DEPTH`], the values held for the whole graph to [`MAX_VALUES`] and
//! the layers to [`MAX_LAYERS`]. What the parsed graph holds in memory is
//! capped at [`MAX_GRAPH_BYTES`]: every string kept, every member or element
//! value and every object is charged (by its size in memory) before it is
//! stored, and a class's name and member names are stored once and shared
//! by every object of that class (a `ClassWithId` record copies none of
//! them), so a file of many small records naming one long class costs no
//! more than the records themselves. A kept pixel block may be no longer
//! than its layer's `stride * height` (a longer one is refused as
//! malformed), and each block is turned into its layers' RGBA and dropped
//! as soon as it is read, so at most one block is held at a time; before a
//! block's buffer exists, all the layers' pixels together plus that block
//! are checked against the caller's [`ImportLimits`]. Gzip chunks inflate
//! straight into the block's buffer (no second copy), and a chunk that
//! inflates to more or fewer bytes than its place is refused. Malformed
//! input is an error, never a panic.
//!
//! [MS-NRBF]: https://learn.microsoft.com/openspecs/windows_protocols/ms-nrbf

use std::collections::HashMap;
use std::io::Read;
use std::rc::Rc;

use super::super::{check_decode, malformed};
use super::looks_like_pdn;
use crate::codec::{CodecError, ImportLimits};

const NAME: &str = "Paint.NET";

/// The most layers one file may carry.
pub const MAX_LAYERS: usize = 1000;
/// The deepest record nesting the parser follows.
pub const MAX_DEPTH: usize = 64;
/// The most member and element values held for one object graph.
pub const MAX_VALUES: usize = 1 << 20;
/// The most memory the parsed object graph may hold, in bytes: the strings
/// it keeps, its values and its objects, each charged by its size in memory.
pub const MAX_GRAPH_BYTES: usize = 64 << 20;

/// A layer's blend operation, in Paint.NET's own list order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdnBlend {
    Normal,
    Multiply,
    Additive,
    ColorBurn,
    ColorDodge,
    Reflect,
    Glow,
    Overlay,
    Difference,
    Negation,
    Lighten,
    Darken,
    Screen,
    Xor,
    /// A blend operation this reader does not know, by its name or number.
    Other(String),
}

impl PdnBlend {
    const ORDER: [PdnBlend; 14] = [
        PdnBlend::Normal,
        PdnBlend::Multiply,
        PdnBlend::Additive,
        PdnBlend::ColorBurn,
        PdnBlend::ColorDodge,
        PdnBlend::Reflect,
        PdnBlend::Glow,
        PdnBlend::Overlay,
        PdnBlend::Difference,
        PdnBlend::Negation,
        PdnBlend::Lighten,
        PdnBlend::Darken,
        PdnBlend::Screen,
        PdnBlend::Xor,
    ];

    /// The class-name stem Paint.NET 3.x gives the operation
    /// (`UserBlendOps+<stem>BlendOp`).
    pub fn stem(&self) -> String {
        match self {
            PdnBlend::Other(name) => name.clone(),
            other => format!("{other:?}"),
        }
    }

    fn from_index(index: i64) -> Self {
        usize::try_from(index)
            .ok()
            .and_then(|i| Self::ORDER.get(i).cloned())
            .unwrap_or_else(|| PdnBlend::Other(format!("blend mode {index}")))
    }

    fn from_class(class: &str) -> Self {
        let short = class.rsplit(['+', '.']).next().unwrap_or(class);
        let stem = short.strip_suffix("BlendOp").unwrap_or(short);
        Self::ORDER
            .iter()
            .find(|b| b.stem() == stem)
            .cloned()
            .unwrap_or_else(|| PdnBlend::Other(short.to_string()))
    }
}

/// One layer, as Paint.NET stored it.
#[derive(Debug, Clone)]
pub struct PdnLayer {
    pub name: String,
    pub visible: bool,
    /// 0 (clear) to 255 (opaque).
    pub opacity: u8,
    pub blend: PdnBlend,
    pub is_background: bool,
    /// The document-sized pixels, RGBA8 with straight alpha.
    pub rgba: Vec<u8>,
}

/// A `.pdn`'s layers, **bottom first** (Paint.NET's own order).
#[derive(Debug)]
pub struct PdnDocument {
    pub width: u32,
    pub height: u32,
    pub layers: Vec<PdnLayer>,
}

// ------------------------------------------------------------------ reading

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| malformed(NAME, "the object graph runs past the end of the file"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn i32(&mut self) -> Result<i32, CodecError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn u32_be(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    /// A count or length: non-negative.
    fn count(&mut self) -> Result<usize, CodecError> {
        let n = self.i32()?;
        usize::try_from(n).map_err(|_| malformed(NAME, format!("a negative count ({n})")))
    }

    /// A length-prefixed string: a 7-bit-encoded length, then UTF-8.
    fn string(&mut self) -> Result<String, CodecError> {
        let mut len = 0usize;
        for shift in (0..35).step_by(7) {
            let b = self.u8()?;
            len |= usize::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                let bytes = self.take(len)?;
                return Ok(String::from_utf8_lossy(bytes).into_owned());
            }
        }
        Err(malformed(NAME, "a string length longer than five bytes"))
    }
}

/// A member or element value.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// An object, by id.
    Ref(i32),
    /// A value this reader skipped (a decimal, a char).
    Other,
}

/// An object the graph holds.
#[derive(Debug)]
enum Node {
    Class {
        /// Shared with every object of the class.
        class: Rc<str>,
        /// Each name shared with the class's other objects.
        members: Vec<(Rc<str>, Val)>,
    },
    Array(Vec<Val>),
    Str(String),
    /// A primitive array: skipped, never needed.
    Opaque,
}

#[derive(Debug, Clone, PartialEq)]
enum BinaryType {
    Primitive(u8),
    Other,
}

#[derive(Debug, Clone)]
struct Meta {
    class: Rc<str>,
    names: Vec<Rc<str>>,
    types: Option<Vec<BinaryType>>,
}

/// What one record was.
enum Rec {
    Value(Val),
    Nulls(usize),
    End,
}

struct Parser<'a> {
    c: Cursor<'a>,
    objects: HashMap<i32, Node>,
    /// Each class's shape, shared (not copied) by every object that uses it.
    metas: HashMap<i32, Rc<Meta>>,
    /// Class objects, in the order their records start.
    order: Vec<i32>,
    values: usize,
    /// The bytes charged so far against `max_bytes`.
    bytes: usize,
    /// [`MAX_GRAPH_BYTES`], lower in tests.
    max_bytes: usize,
}

/// What one stored object costs besides its values: its map entry and its
/// place in `order`.
const OBJECT_COST: usize = std::mem::size_of::<(i32, Node)>() + 2 * std::mem::size_of::<i32>();
/// A class record's id, name and member names.
type ClassInfo = (i32, Rc<str>, Vec<Rc<str>>);
/// What one member value costs.
const MEMBER_COST: usize = std::mem::size_of::<(Rc<str>, Val)>();

/// The size of a fixed-width primitive, `None` for the variable ones.
fn primitive_size(t: u8) -> Option<usize> {
    Some(match t {
        1 | 2 | 10 => 1,
        7 | 14 => 2,
        8 | 11 | 15 => 4,
        6 | 9 | 12 | 13 | 16 => 8,
        17 => 0,
        _ => return None,
    })
}

impl<'a> Parser<'a> {
    fn budget(&mut self, n: usize) -> Result<(), CodecError> {
        self.values = self.values.saturating_add(n);
        if self.values > MAX_VALUES {
            return Err(CodecError::LimitExceeded(format!(
                "the Paint.NET object graph holds more than {MAX_VALUES} values"
            )));
        }
        Ok(())
    }

    /// Charge `n` bytes of memory the graph is about to hold.
    fn charge(&mut self, n: usize) -> Result<(), CodecError> {
        self.bytes = self.bytes.saturating_add(n);
        if self.bytes > self.max_bytes {
            return Err(CodecError::LimitExceeded(format!(
                "the Paint.NET object graph needs more than {} bytes of memory",
                self.max_bytes
            )));
        }
        Ok(())
    }

    /// A string the graph keeps, charged before it is stored.
    fn kept_string(&mut self) -> Result<String, CodecError> {
        let s = self.c.string()?;
        self.charge(s.len())?;
        Ok(s)
    }

    fn insert(&mut self, id: i32, node: Node) -> Result<(), CodecError> {
        self.charge(OBJECT_COST)?;
        if self.objects.insert(id, node).is_some() {
            return Err(malformed(NAME, format!("object {id} is defined twice")));
        }
        Ok(())
    }

    fn primitive(&mut self, t: u8) -> Result<Val, CodecError> {
        let c = &mut self.c;
        Ok(match t {
            1 => Val::Bool(c.u8()? != 0),
            2 => Val::Int(i64::from(c.u8()?)),
            3 => {
                // One UTF-8 character: its lead byte says how long it is.
                let lead = c.u8()?;
                let extra = match lead {
                    0x00..=0x7F => 0,
                    0xC0..=0xDF => 1,
                    0xE0..=0xEF => 2,
                    _ => 3,
                };
                c.take(extra)?;
                Val::Other
            }
            5 => {
                c.string()?;
                Val::Other
            }
            6 => Val::Float(f64::from_le_bytes(c.array()?)),
            7 => Val::Int(i64::from(i16::from_le_bytes(c.array()?))),
            8 => Val::Int(i64::from(c.i32()?)),
            9 | 12 | 13 => Val::Int(i64::from_le_bytes(c.array()?)),
            10 => Val::Int(i64::from(c.u8()? as i8)),
            11 => Val::Float(f64::from(f32::from_le_bytes(c.array()?))),
            14 => Val::Int(i64::from(u16::from_le_bytes(c.array()?))),
            15 => Val::Int(i64::from(u32::from_le_bytes(c.array()?))),
            16 => Val::Int(u64::from_le_bytes(c.array()?).min(i64::MAX as u64) as i64),
            17 => Val::Null,
            18 => {
                let s = c.string()?;
                self.charge(s.len())?;
                Val::Str(s)
            }
            other => return Err(malformed(NAME, format!("unknown primitive type {other}"))),
        })
    }

    /// A member-type list: `n` binary types, then their extra information.
    fn member_types(&mut self, n: usize) -> Result<Vec<BinaryType>, CodecError> {
        let kinds = self.c.take(n)?.to_vec();
        let mut out = Vec::with_capacity(n);
        for kind in kinds {
            out.push(self.binary_type_info(kind)?);
        }
        Ok(out)
    }

    fn binary_type_info(&mut self, kind: u8) -> Result<BinaryType, CodecError> {
        Ok(match kind {
            0 => BinaryType::Primitive(self.c.u8()?),
            1 | 2 | 5 | 6 => BinaryType::Other,
            3 => {
                self.c.string()?;
                BinaryType::Other
            }
            4 => {
                self.c.string()?;
                self.c.i32()?;
                BinaryType::Other
            }
            7 => {
                self.c.u8()?;
                BinaryType::Other
            }
            other => return Err(malformed(NAME, format!("unknown member type {other}"))),
        })
    }

    /// A class's name and member names.
    fn class_info(&mut self) -> Result<ClassInfo, CodecError> {
        let id = self.c.i32()?;
        let class = Rc::from(self.kept_string()?);
        let n = self.c.count()?;
        if n > self.c.remaining() {
            return Err(malformed(
                NAME,
                "a class declares more members than the file holds",
            ));
        }
        self.budget(n)?;
        self.charge(n.saturating_mul(std::mem::size_of::<Rc<str>>()))?;
        let mut names = Vec::with_capacity(n);
        for _ in 0..n {
            names.push(Rc::from(self.kept_string()?));
        }
        Ok((id, class, names))
    }

    /// A class object's member values, per `meta`.
    fn class_body(&mut self, id: i32, meta: Rc<Meta>, depth: usize) -> Result<Val, CodecError> {
        self.charge(meta.names.len().saturating_mul(MEMBER_COST))?;
        self.order.push(id);
        let mut members = Vec::with_capacity(meta.names.len());
        let mut nulls = 0usize;
        for (i, name) in meta.names.iter().enumerate() {
            if nulls > 0 {
                nulls -= 1;
                members.push((Rc::clone(name), Val::Null));
                continue;
            }
            let value = match meta.types.as_ref().and_then(|t| t.get(i)) {
                Some(BinaryType::Primitive(t)) => self.primitive(*t)?,
                _ => match self.record(depth + 1)? {
                    Rec::Value(v) => v,
                    Rec::Nulls(n) => {
                        nulls = n.saturating_sub(1);
                        Val::Null
                    }
                    Rec::End => return Err(malformed(NAME, "the stream ended inside an object")),
                },
            };
            members.push((Rc::clone(name), value));
        }
        let class = Rc::clone(&meta.class);
        self.metas.insert(id, meta);
        self.insert(id, Node::Class { class, members })?;
        Ok(Val::Ref(id))
    }

    /// `n` elements of an object array.
    fn elements(&mut self, n: usize, t: &BinaryType, depth: usize) -> Result<Vec<Val>, CodecError> {
        self.budget(n)?;
        self.charge(n.saturating_mul(std::mem::size_of::<Val>()))?;
        let mut out = Vec::with_capacity(n.min(self.c.remaining()));
        while out.len() < n {
            match t {
                BinaryType::Primitive(p) => out.push(self.primitive(*p)?),
                BinaryType::Other => match self.record(depth + 1)? {
                    Rec::Value(v) => out.push(v),
                    Rec::Nulls(k) => {
                        if k > n - out.len() {
                            return Err(malformed(NAME, "more nulls than the array has elements"));
                        }
                        out.extend(std::iter::repeat_n(Val::Null, k));
                    }
                    Rec::End => return Err(malformed(NAME, "the stream ended inside an array")),
                },
            }
        }
        Ok(out)
    }

    /// Skip `n` primitives of type `t`.
    fn skip_primitives(&mut self, n: usize, t: u8) -> Result<(), CodecError> {
        match primitive_size(t) {
            Some(size) => {
                let total = n
                    .checked_mul(size)
                    .ok_or_else(|| malformed(NAME, "an array larger than memory"))?;
                self.c.take(total)?;
            }
            None => {
                if n > self.c.remaining() {
                    return Err(malformed(NAME, "an array longer than the file"));
                }
                for _ in 0..n {
                    self.primitive(t)?;
                }
            }
        }
        Ok(())
    }

    /// One record.
    fn record(&mut self, depth: usize) -> Result<Rec, CodecError> {
        if depth > MAX_DEPTH {
            return Err(CodecError::LimitExceeded(format!(
                "the Paint.NET object graph nests deeper than {MAX_DEPTH}"
            )));
        }
        loop {
            let tag = self.c.u8()?;
            return Ok(match tag {
                // SerializationHeader: root id, header id, major, minor.
                0 => {
                    self.c.take(16)?;
                    continue;
                }
                // ClassWithId: an object reusing another's class.
                1 => {
                    let id = self.c.i32()?;
                    let meta_id = self.c.i32()?;
                    let meta = self.metas.get(&meta_id).cloned().ok_or_else(|| {
                        malformed(NAME, format!("object {id} reuses unknown class {meta_id}"))
                    })?;
                    self.budget(meta.names.len())?;
                    Rec::Value(self.class_body(id, meta, depth)?)
                }
                // SystemClassWithMembers / ClassWithMembers: no types.
                2 | 3 => {
                    let (id, class, names) = self.class_info()?;
                    if tag == 3 {
                        self.c.i32()?;
                    }
                    let meta = Rc::new(Meta {
                        class,
                        names,
                        types: None,
                    });
                    Rec::Value(self.class_body(id, meta, depth)?)
                }
                // SystemClassWithMembersAndTypes / ClassWithMembersAndTypes.
                4 | 5 => {
                    let (id, class, names) = self.class_info()?;
                    let types = self.member_types(names.len())?;
                    if tag == 5 {
                        self.c.i32()?;
                    }
                    let meta = Rc::new(Meta {
                        class,
                        names,
                        types: Some(types),
                    });
                    Rec::Value(self.class_body(id, meta, depth)?)
                }
                // BinaryObjectString.
                6 => {
                    let id = self.c.i32()?;
                    let s = self.kept_string()?;
                    self.charge(s.len())?;
                    self.insert(id, Node::Str(s.clone()))?;
                    Rec::Value(Val::Str(s))
                }
                // BinaryArray.
                7 => {
                    let id = self.c.i32()?;
                    let shape = self.c.u8()?;
                    let rank = self.c.count()?;
                    if rank == 0 || rank > 32 {
                        return Err(malformed(NAME, format!("an array of rank {rank}")));
                    }
                    let mut n = 1usize;
                    for _ in 0..rank {
                        n = n
                            .checked_mul(self.c.count()?)
                            .ok_or_else(|| malformed(NAME, "an array larger than memory"))?;
                    }
                    if matches!(shape, 3..=5) {
                        self.c.take(rank * 4)?;
                    }
                    let kind = self.c.u8()?;
                    let t = self.binary_type_info(kind)?;
                    if let BinaryType::Primitive(p) = t {
                        self.skip_primitives(n, p)?;
                        self.insert(id, Node::Opaque)?;
                    } else {
                        let items = self.elements(n, &t, depth)?;
                        self.insert(id, Node::Array(items))?;
                    }
                    Rec::Value(Val::Ref(id))
                }
                // MemberPrimitiveTyped.
                8 => {
                    let t = self.c.u8()?;
                    Rec::Value(self.primitive(t)?)
                }
                // MemberReference.
                9 => Rec::Value(Val::Ref(self.c.i32()?)),
                // ObjectNull.
                10 => Rec::Value(Val::Null),
                // MessageEnd.
                11 => Rec::End,
                // BinaryLibrary: precedes the record that names it.
                12 => {
                    self.c.i32()?;
                    self.c.string()?;
                    continue;
                }
                // ObjectNullMultiple256 / ObjectNullMultiple.
                13 => Rec::Nulls(usize::from(self.c.u8()?)),
                14 => Rec::Nulls(self.c.count()?),
                // ArraySinglePrimitive.
                15 => {
                    let id = self.c.i32()?;
                    let n = self.c.count()?;
                    let t = self.c.u8()?;
                    self.skip_primitives(n, t)?;
                    self.insert(id, Node::Opaque)?;
                    Rec::Value(Val::Ref(id))
                }
                // ArraySingleObject / ArraySingleString.
                16 | 17 => {
                    let id = self.c.i32()?;
                    let n = self.c.count()?;
                    let items = self.elements(n, &BinaryType::Other, depth)?;
                    self.insert(id, Node::Array(items))?;
                    Rec::Value(Val::Ref(id))
                }
                other => {
                    return Err(malformed(
                        NAME,
                        format!("record type {other} is not part of an object graph"),
                    ))
                }
            });
        }
    }
}

/// The parsed graph and the bytes after it (the deferred blocks).
struct Graph<'a> {
    objects: HashMap<i32, Node>,
    order: Vec<i32>,
    rest: &'a [u8],
}

fn parse_graph(stream: &[u8]) -> Result<Graph<'_>, CodecError> {
    parse_graph_capped(stream, MAX_GRAPH_BYTES)
}

fn parse_graph_capped(stream: &[u8], max_bytes: usize) -> Result<Graph<'_>, CodecError> {
    let mut p = Parser {
        c: Cursor {
            bytes: stream,
            at: 0,
        },
        objects: HashMap::new(),
        metas: HashMap::new(),
        order: Vec::new(),
        values: 0,
        bytes: 0,
        max_bytes,
    };
    if p.c.u8()? != 0 {
        return Err(malformed(
            NAME,
            "the object graph has no serialization header",
        ));
    }
    p.c.take(16)?;
    loop {
        if let Rec::End = p.record(0)? {
            break;
        }
    }
    Ok(Graph {
        objects: p.objects,
        order: p.order,
        rest: &stream[p.c.at..],
    })
}

type Members = [(Rc<str>, Val)];

impl Graph<'_> {
    fn class(&self, v: &Val) -> Option<(&str, &Members)> {
        match v {
            Val::Ref(id) => match self.objects.get(id)? {
                Node::Class { class, members } => Some((&**class, members.as_slice())),
                _ => None,
            },
            _ => None,
        }
    }

    fn class_by_id(&self, id: i32) -> Option<(&str, &Members)> {
        self.class(&Val::Ref(id))
    }

    fn string(&self, v: &Val) -> Option<String> {
        match v {
            Val::Str(s) => Some(s.clone()),
            Val::Ref(id) => match self.objects.get(id)? {
                Node::Str(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn array(&self, v: &Val) -> Option<&[Val]> {
        match v {
            Val::Ref(id) => match self.objects.get(id)? {
                Node::Array(items) => Some(items.as_slice()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The first member of `members` that is a class object with every
    /// member in `wanted` (by name, see [`member`]).
    fn part<'s>(&'s self, members: &'s Members, wanted: &[&str]) -> Option<&'s Members> {
        members.iter().find_map(|(_, v)| {
            let (_, m) = self.class(v)?;
            wanted.iter().all(|w| member(m, w).is_some()).then_some(m)
        })
    }
}

/// A member by its name, or by `<Class>+<name>` for an inherited field.
fn member<'m>(members: &'m Members, name: &str) -> Option<&'m Val> {
    members
        .iter()
        .find(|(n, _)| &**n == name)
        .or_else(|| {
            members
                .iter()
                .find(|(n, _)| n.rsplit_once('+').is_some_and(|(_, tail)| tail == name))
        })
        .map(|(_, v)| v)
}

fn int(members: &Members, name: &str) -> Option<i64> {
    match member(members, name)? {
        Val::Int(v) => Some(*v),
        _ => None,
    }
}

fn boolean(members: &Members, name: &str) -> Option<bool> {
    match member(members, name)? {
        Val::Bool(b) => Some(*b),
        _ => None,
    }
}

fn dimension(members: &Members, name: &str, what: &str) -> Result<u32, CodecError> {
    int(members, name)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| malformed(NAME, format!("{what} has no usable {name}")))
}

/// A layer before its pixels are read.
struct Pending {
    name: String,
    visible: bool,
    opacity: u8,
    blend: PdnBlend,
    is_background: bool,
    stride: usize,
    block: i32,
}

/// Read one deferred block's chunks from `c` into a buffer of `len` bytes;
/// `keep` false skips them (a block no layer uses).
fn read_block(c: &mut Cursor<'_>, len: usize, keep: bool) -> Result<Vec<u8>, CodecError> {
    let format = c.u8()?;
    if format > 1 {
        return Err(malformed(
            NAME,
            format!("unknown pixel-block format {format}"),
        ));
    }
    let chunk = c.u32_be()? as usize;
    if chunk == 0 {
        return Err(malformed(NAME, "a pixel block with zero-sized chunks"));
    }
    let count = len.div_ceil(chunk);
    // Every chunk has an 8-byte header: a count the bytes cannot hold is
    // refused before anything is sized by it.
    if count > c.remaining() / 8 {
        return Err(malformed(
            NAME,
            "a pixel block has more chunks than the file holds",
        ));
    }
    let mut out = if keep { vec![0u8; len] } else { Vec::new() };
    let mut seen = vec![false; count];
    for _ in 0..count {
        let number = c.u32_be()? as usize;
        let size = c.u32_be()? as usize;
        let data = c.take(size)?;
        if number >= count || std::mem::replace(&mut seen[number], true) {
            return Err(malformed(
                NAME,
                format!("pixel chunk {number} is out of place"),
            ));
        }
        let start = number * chunk;
        let expected = chunk.min(len - start);
        if !keep {
            continue;
        }
        let target = &mut out[start..start + expected];
        if format == 1 {
            if size != expected {
                return Err(malformed(NAME, "a raw pixel chunk of the wrong size"));
            }
            target.copy_from_slice(data);
        } else {
            // Inflate straight into the chunk's place: no second buffer.
            let mut z = flate2::read::GzDecoder::new(data);
            z.read_exact(target).map_err(|e| {
                malformed(
                    NAME,
                    format!("a pixel chunk does not inflate to its size: {e}"),
                )
            })?;
            let mut extra = [0u8; 1];
            let more = z
                .read(&mut extra)
                .map_err(|e| malformed(NAME, format!("a pixel chunk does not inflate: {e}")))?;
            if more != 0 {
                return Err(malformed(NAME, "a pixel chunk inflates past its size"));
            }
        }
    }
    Ok(out)
}

/// W13X-7: the layers of a `.pdn` (see the module docs), bottom first.
pub fn read_layers(bytes: &[u8], limits: ImportLimits) -> Result<PdnDocument, CodecError> {
    if !looks_like_pdn(bytes) || bytes.len() < 7 {
        return Err(malformed(NAME, "no PDN3 header"));
    }
    let len = usize::from(bytes[4]) | usize::from(bytes[5]) << 8 | usize::from(bytes[6]) << 16;
    let body = bytes
        .get(7 + len..)
        .ok_or_else(|| malformed(NAME, "the header runs past the end of the file"))?;
    match body {
        [0x00, 0x01, stream @ ..] => read_graph(stream, limits),
        [0x1F, 0x8B, ..] => Err(CodecError::Unsupported(
            "this Paint.NET file uses the older layout whose whole object graph is \
             gzip-compressed, which this build does not read the layers of"
                .into(),
        )),
        _ => Err(malformed(NAME, "no object graph follows the header")),
    }
}

fn read_graph(stream: &[u8], limits: ImportLimits) -> Result<PdnDocument, CodecError> {
    let graph = parse_graph(stream)?;
    // The document: the first object with a size and a layer list.
    let doc = graph
        .order
        .iter()
        .find_map(|id| {
            let (_, m) = graph.class_by_id(*id)?;
            (member(m, "layers").is_some() && int(m, "width").is_some()).then_some(m)
        })
        .ok_or_else(|| malformed(NAME, "no document object (width, height, layers)"))?;
    let (width, height) = (
        dimension(doc, "width", "the document")?,
        dimension(doc, "height", "the document")?,
    );
    check_decode(limits, width, height, 4, 0)?;
    let list = member(doc, "layers").expect("found by this member");
    // An ArrayList (`_items`, `_size`), or a plain array.
    let items: Vec<&Val> = match graph.class(list) {
        Some((_, m)) => {
            let items = member(m, "_items")
                .and_then(|v| graph.array(v))
                .ok_or_else(|| malformed(NAME, "the layer list has no items"))?;
            let size = int(m, "_size")
                .and_then(|s| usize::try_from(s).ok())
                .unwrap_or(items.len())
                .min(items.len());
            items[..size].iter().collect()
        }
        None => graph
            .array(list)
            .ok_or_else(|| malformed(NAME, "the layer list is neither a list nor an array"))?
            .iter()
            .collect(),
    };
    let items: Vec<&Val> = items.into_iter().filter(|v| **v != Val::Null).collect();
    if items.is_empty() {
        return Err(malformed(NAME, "the document has no layers"));
    }
    if items.len() > MAX_LAYERS {
        return Err(CodecError::LimitExceeded(format!(
            "the file has {} layers; at most {MAX_LAYERS} are read",
            items.len()
        )));
    }
    let layer_bytes = u64::from(width) * u64::from(height) * 4;
    limits.check_alloc(layer_bytes.saturating_mul(items.len() as u64))?;

    let mut pending = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let what = format!("layer {}", index + 1);
        let (class, m) = graph
            .class(item)
            .ok_or_else(|| malformed(NAME, format!("{what} is not an object")))?;
        let surface = graph
            .part(m, &["scan0", "stride", "width", "height"])
            .ok_or_else(|| {
                CodecError::Unsupported(format!(
                    "{what} ({class}) has no bitmap surface; only bitmap layers are read"
                ))
            })?;
        if dimension(surface, "width", &what)? != width
            || dimension(surface, "height", &what)? != height
        {
            return Err(malformed(
                NAME,
                format!("{what} is not the document's size"),
            ));
        }
        let stride = int(surface, "stride")
            .and_then(|s| usize::try_from(s).ok())
            // Paint.NET surfaces are tightly packed BGRA rows, so the
            // stride is exactly a row; a larger one would let a forged file
            // declare a huge block for a tiny layer.
            .filter(|s| *s == width as usize * 4)
            .ok_or_else(|| malformed(NAME, format!("{what} has a stride that is not one row")))?;
        let block = match member(surface, "scan0") {
            Some(Val::Ref(id)) => *id,
            _ => return Err(malformed(NAME, format!("{what} has no pixel block"))),
        };
        let props = graph.part(m, &["name", "opacity"]);
        let name = props
            .and_then(|p| member(p, "name"))
            .and_then(|v| graph.string(v))
            .unwrap_or_else(|| format!("Layer {}", index + 1));
        let opacity = props
            .and_then(|p| int(p, "opacity"))
            .map_or(255, |o| o.clamp(0, 255) as u8);
        let visible = props.and_then(|p| boolean(p, "visible")).unwrap_or(true);
        let is_background = props
            .and_then(|p| boolean(p, "isBackground"))
            .unwrap_or(false);
        let blend = match graph
            .part(m, &["blendOp"])
            .or_else(|| graph.part(m, &["blendMode"]))
        {
            None => PdnBlend::Normal,
            Some(bp) => match member(bp, "blendOp").or_else(|| member(bp, "blendMode")) {
                Some(Val::Int(i)) => PdnBlend::from_index(*i),
                Some(v) => match graph.class(v) {
                    Some((_, e)) if int(e, "value__").is_some() => {
                        PdnBlend::from_index(int(e, "value__").unwrap_or(0))
                    }
                    Some((class, _)) => PdnBlend::from_class(class),
                    None => PdnBlend::Normal,
                },
                None => PdnBlend::Normal,
            },
        };
        pending.push(Pending {
            name,
            visible,
            opacity,
            blend,
            is_background,
            stride,
            block,
        });
    }

    // The deferred blocks, in the order their records appear. Each kept
    // block becomes its layers' RGBA as soon as it is read and is then
    // dropped, so at most one block is held beside the layers' pixels.
    let (w, h) = (width as usize, height as usize);
    let all_layers = layer_bytes.saturating_mul(pending.len() as u64);
    let mut done: Vec<Option<Vec<u8>>> = vec![None; pending.len()];
    let mut c = Cursor {
        bytes: graph.rest,
        at: 0,
    };
    for id in &graph.order {
        let Some((_, m)) = graph.class_by_id(*id) else {
            continue;
        };
        if boolean(m, "hasParent") == Some(true) {
            return Err(CodecError::Unsupported(
                "a pixel block of this file points into another block, which this build \
                 does not read"
                    .into(),
            ));
        }
        let Some(len) = int(m, "length64").or_else(|| int(m, "length")) else {
            continue;
        };
        if boolean(m, "deferred") != Some(true) {
            if pending.iter().any(|p| p.block == *id) {
                return Err(CodecError::Unsupported(
                    "this file stores its pixels inline rather than after the object graph, \
                     which this build does not read"
                        .into(),
                ));
            }
            continue;
        }
        let len =
            usize::try_from(len).map_err(|_| malformed(NAME, "a pixel block of negative size"))?;
        // The longest a used block may be: its layer's `stride * height`.
        let longest = pending
            .iter()
            .filter(|p| p.block == *id)
            .map(|p| p.stride.saturating_mul(h))
            .max();
        let keep = longest.is_some();
        if let Some(longest) = longest {
            if len > longest {
                return Err(malformed(
                    NAME,
                    format!("a pixel block of {len} bytes is longer than its layer ({longest})"),
                ));
            }
            // Every layer's pixels plus this one block, before it exists.
            limits.check_alloc(all_layers.saturating_add(len as u64))?;
        }
        let data = read_block(&mut c, len, keep)?;
        for (i, p) in pending.iter().enumerate() {
            if !keep || p.block != *id {
                continue;
            }
            let need = p
                .stride
                .checked_mul(h - 1)
                .and_then(|v| v.checked_add(w * 4));
            if need.is_none_or(|need| need > data.len()) {
                return Err(malformed(
                    NAME,
                    format!("layer {:?}'s pixels are short", p.name),
                ));
            }
            let mut rgba = Vec::with_capacity(w * h * 4);
            for row in data.chunks(p.stride).take(h) {
                for px in row[..w * 4].as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            }
            done[i] = Some(rgba);
        }
    }

    let mut layers = Vec::with_capacity(pending.len());
    for (p, rgba) in pending.into_iter().zip(done) {
        let rgba =
            rgba.ok_or_else(|| malformed(NAME, format!("layer {:?} has no pixel data", p.name)))?;
        layers.push(PdnLayer {
            name: p.name,
            visible: p.visible,
            opacity: p.opacity,
            blend: p.blend,
            is_background: p.is_background,
            rgba,
        });
    }
    Ok(PdnDocument {
        width,
        height,
        layers,
    })
}

// ------------------------------------------------------------------ fixture

/// A synthetic `.pdn` writer for tests, here and in `app-shell`. Not an
/// exporter: it writes the layout the module docs describe, and nothing
/// Paint.NET itself was used to check.
#[doc(hidden)]
pub mod fixture {
    use std::io::Write;

    /// One layer to write.
    pub struct Layer<'a> {
        pub name: &'a str,
        pub visible: bool,
        pub opacity: u8,
        /// The blend operation's class stem, `Normal`, `Multiply`, ...
        pub blend: &'a str,
        pub is_background: bool,
        /// RGBA8, straight alpha, document-sized.
        pub rgba: &'a [u8],
    }

    /// How the file is laid out.
    #[derive(Clone, Copy, Default)]
    pub struct Options {
        /// Store the pixel chunks raw instead of gzip-compressed.
        pub raw: bool,
        /// Write the pixel blocks' records last layer first (their data
        /// follows in record order).
        pub reverse_blocks: bool,
        /// A thumbnail PNG for the XML header.
        pub thumb_png: Option<&'static [u8]>,
        /// Declare every pixel block this many bytes long and store it as
        /// one gzip chunk of that many zeros (a hostile file's shape).
        pub declared_block_len: Option<i64>,
        /// Write this stride instead of `width * 4` (a forged file).
        pub stride: Option<i32>,
    }

    fn lps(out: &mut Vec<u8>, s: &str) {
        let mut n = s.len();
        loop {
            let b = (n & 0x7F) as u8;
            n >>= 7;
            if n == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
        out.extend_from_slice(s.as_bytes());
    }

    fn i32(out: &mut Vec<u8>, v: i32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    /// Member type: 0 primitive(+type), 1 string, 2 object, 3 system class,
    /// 4 class (+name, library), 5 object array.
    enum T<'a> {
        Prim(u8),
        Str,
        SysClass(&'a str),
        Class(&'a str),
        ObjArray,
    }

    const LIB: i32 = 2;

    fn class(out: &mut Vec<u8>, id: i32, name: &str, members: &[(&str, T)]) {
        out.push(5);
        i32(out, id);
        lps(out, name);
        i32(out, members.len() as i32);
        for (m, _) in members {
            lps(out, m);
        }
        for (_, t) in members {
            out.push(match t {
                T::Prim(_) => 0,
                T::Str => 1,
                T::SysClass(_) => 3,
                T::Class(_) => 4,
                T::ObjArray => 5,
            });
        }
        for (_, t) in members {
            match t {
                T::Prim(p) => out.push(*p),
                T::SysClass(n) => lps(out, n),
                T::Class(n) => {
                    lps(out, n);
                    i32(out, LIB);
                }
                _ => {}
            }
        }
        i32(out, LIB);
    }

    fn reference(out: &mut Vec<u8>, id: i32) {
        out.push(9);
        i32(out, id);
    }

    /// A `.pdn` of `layers` (bottom first), `width` x `height`.
    pub fn pdn_file(width: u32, height: u32, layers: &[Layer], options: Options) -> Vec<u8> {
        let (w, h) = (width as i32, height as i32);
        let n = layers.len() as i32;
        // Ids: 1 document, 3 list, 4 items, then per layer i a block of 10.
        let base = |i: usize| 10 + 10 * i as i32;
        let mut g = Vec::new();
        g.push(0);
        i32(&mut g, 1);
        i32(&mut g, -1);
        i32(&mut g, 1);
        i32(&mut g, 0);
        g.push(12);
        i32(&mut g, LIB);
        lps(&mut g, "PaintDotNet.Data, Version=3.36.0.0");
        class(
            &mut g,
            1,
            "PaintDotNet.Document",
            &[
                ("layers", T::Class("PaintDotNet.LayerList")),
                ("width", T::Prim(8)),
                ("height", T::Prim(8)),
            ],
        );
        reference(&mut g, 3);
        i32(&mut g, w);
        i32(&mut g, h);
        class(
            &mut g,
            3,
            "PaintDotNet.LayerList",
            &[
                ("ArrayList+_items", T::ObjArray),
                ("ArrayList+_size", T::Prim(8)),
                ("ArrayList+_version", T::Prim(8)),
                ("parent", T::Class("PaintDotNet.Document")),
            ],
        );
        reference(&mut g, 4);
        i32(&mut g, n);
        i32(&mut g, 0);
        reference(&mut g, 1);
        // The items: the layers, then two empty slots of capacity.
        g.push(16);
        i32(&mut g, 4);
        i32(&mut g, n + 2);
        for i in 0..layers.len() {
            reference(&mut g, base(i));
        }
        g.push(13);
        g.push(2);
        // The layers: the first defines the class, the rest reuse it.
        for i in 0..layers.len() {
            let id = base(i);
            if i == 0 {
                class(
                    &mut g,
                    id,
                    "PaintDotNet.BitmapLayer",
                    &[
                        (
                            "properties",
                            T::Class("PaintDotNet.BitmapLayer+BitmapLayerProperties"),
                        ),
                        ("surface", T::Class("PaintDotNet.Surface")),
                        ("Layer+isDisposed", T::Prim(1)),
                        ("Layer+width", T::Prim(8)),
                        ("Layer+height", T::Prim(8)),
                        (
                            "Layer+properties",
                            T::Class("PaintDotNet.Layer+LayerProperties"),
                        ),
                    ],
                );
            } else {
                g.push(1);
                i32(&mut g, id);
                i32(&mut g, base(0));
            }
            reference(&mut g, id + 1);
            reference(&mut g, id + 2);
            g.push(0);
            i32(&mut g, w);
            i32(&mut g, h);
            reference(&mut g, id + 3);
        }
        for (i, layer) in layers.iter().enumerate() {
            let id = base(i);
            // BitmapLayerProperties, pointing at its blend op object.
            class(
                &mut g,
                id + 1,
                "PaintDotNet.BitmapLayer+BitmapLayerProperties",
                &[("blendOp", T::Class("PaintDotNet.UserBlendOp"))],
            );
            reference(&mut g, id + 5);
            class(
                &mut g,
                id + 2,
                "PaintDotNet.Surface",
                &[
                    ("width", T::Prim(8)),
                    ("height", T::Prim(8)),
                    ("stride", T::Prim(8)),
                    ("scan0", T::Class("PaintDotNet.MemoryBlock")),
                ],
            );
            i32(&mut g, w);
            i32(&mut g, h);
            i32(&mut g, options.stride.unwrap_or(w * 4));
            reference(&mut g, id + 4);
            class(
                &mut g,
                id + 3,
                "PaintDotNet.Layer+LayerProperties",
                &[
                    ("name", T::Str),
                    (
                        "userMetaData",
                        T::SysClass("System.Collections.Specialized.NameValueCollection"),
                    ),
                    ("visible", T::Prim(1)),
                    ("isBackground", T::Prim(1)),
                    ("opacity", T::Prim(2)),
                ],
            );
            g.push(6);
            i32(&mut g, id + 6);
            lps(&mut g, layer.name);
            g.push(10);
            g.push(u8::from(layer.visible));
            g.push(u8::from(layer.is_background));
            g.push(layer.opacity);
            class(
                &mut g,
                id + 5,
                &format!("PaintDotNet.UserBlendOps+{}BlendOp", layer.blend),
                &[],
            );
        }
        let order: Vec<usize> = if options.reverse_blocks {
            (0..layers.len()).rev().collect()
        } else {
            (0..layers.len()).collect()
        };
        let len = options
            .declared_block_len
            .unwrap_or(i64::from(w) * i64::from(h) * 4);
        for &i in &order {
            class(
                &mut g,
                base(i) + 4,
                "PaintDotNet.MemoryBlock",
                &[
                    ("length64", T::Prim(9)),
                    ("hasParent", T::Prim(1)),
                    ("deferred", T::Prim(1)),
                ],
            );
            g.extend_from_slice(&len.to_le_bytes());
            g.push(0);
            g.push(1);
        }
        g.push(11);
        // The deferred blocks, in record order: small chunks, written last
        // chunk first.
        for &i in &order {
            if let Some(declared) = options.declared_block_len {
                let zeros = vec![0u8; declared as usize];
                let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
                z.write_all(&zeros).expect("in memory");
                let stored = z.finish().expect("in memory");
                g.push(0);
                g.extend_from_slice(&(declared as u32).to_be_bytes());
                g.extend_from_slice(&0u32.to_be_bytes());
                g.extend_from_slice(&(stored.len() as u32).to_be_bytes());
                g.extend_from_slice(&stored);
                continue;
            }
            let bgra: Vec<u8> = layers[i]
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect();
            let chunk = 64usize;
            g.push(u8::from(options.raw));
            g.extend_from_slice(&(chunk as u32).to_be_bytes());
            let chunks: Vec<&[u8]> = bgra.chunks(chunk).collect();
            for (number, data) in chunks.iter().enumerate().rev() {
                let stored = if options.raw {
                    data.to_vec()
                } else {
                    let mut z =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                    z.write_all(data).expect("in memory");
                    z.finish().expect("in memory")
                };
                g.extend_from_slice(&(number as u32).to_be_bytes());
                g.extend_from_slice(&(stored.len() as u32).to_be_bytes());
                g.extend_from_slice(&stored);
            }
        }
        let thumb = options
            .thumb_png
            .map(|png| {
                format!(
                    "<custom><thumb png=\"{}\" /></custom>",
                    super::super::base64_encode(png)
                )
            })
            .unwrap_or_default();
        let xml = format!(
            "<pdnImage width=\"{width}\" height=\"{height}\" layers=\"{n}\">{thumb}</pdnImage>"
        );
        let mut out = b"PDN3".to_vec();
        out.extend_from_slice(&(xml.len() as u32).to_le_bytes()[..3]);
        out.extend_from_slice(xml.as_bytes());
        out.extend_from_slice(&[0x00, 0x01]);
        out.extend_from_slice(&g);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{pdn_file, Layer, Options};
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        (0..w * h).flat_map(|_| px).collect()
    }

    fn two_layers(options: Options) -> Vec<u8> {
        let bottom = solid(5, 3, [200, 10, 20, 255]);
        let mut top = solid(5, 3, [0, 0, 0, 0]);
        top[4..8].copy_from_slice(&[10, 220, 30, 128]);
        pdn_file(
            5,
            3,
            &[
                Layer {
                    name: "Background",
                    visible: true,
                    opacity: 255,
                    blend: "Normal",
                    is_background: true,
                    rgba: &bottom,
                },
                Layer {
                    name: "Ink",
                    visible: false,
                    opacity: 128,
                    blend: "Multiply",
                    is_background: false,
                    rgba: &top,
                },
            ],
            options,
        )
    }

    #[test]
    fn a_two_layer_pdn_reads_both_layers_with_their_properties() {
        for options in [
            Options::default(),
            Options {
                raw: true,
                ..Options::default()
            },
            Options {
                reverse_blocks: true,
                ..Options::default()
            },
        ] {
            let doc = read_layers(&two_layers(options), ImportLimits::default()).unwrap();
            assert_eq!((doc.width, doc.height, doc.layers.len()), (5, 3, 2));
            let (bottom, top) = (&doc.layers[0], &doc.layers[1]);
            assert_eq!(bottom.name, "Background");
            assert!(bottom.visible && bottom.is_background);
            assert_eq!((bottom.opacity, &bottom.blend), (255, &PdnBlend::Normal));
            assert_eq!(&bottom.rgba[..4], &[200, 10, 20, 255]);
            assert_eq!(top.name, "Ink");
            assert!(!top.visible && !top.is_background);
            assert_eq!((top.opacity, &top.blend), (128, &PdnBlend::Multiply));
            assert_eq!(&top.rgba[..4], &[0, 0, 0, 0]);
            assert_eq!(&top.rgba[4..8], &[10, 220, 30, 128]);
            assert_eq!(top.rgba.len(), 5 * 3 * 4);
        }
    }

    #[test]
    fn blend_names_and_numbers_map_in_paint_dot_nets_order() {
        assert_eq!(
            PdnBlend::from_class("PaintDotNet.UserBlendOps+ScreenBlendOp"),
            PdnBlend::Screen
        );
        assert_eq!(PdnBlend::from_index(2), PdnBlend::Additive);
        assert_eq!(PdnBlend::from_index(13), PdnBlend::Xor);
        assert_eq!(
            PdnBlend::from_index(40),
            PdnBlend::Other("blend mode 40".into())
        );
        assert_eq!(
            PdnBlend::from_class("X+WeirdBlendOp"),
            PdnBlend::Other("WeirdBlendOp".into())
        );
    }

    #[test]
    fn what_this_reader_cannot_follow_is_an_error_that_says_so() {
        let limits = ImportLimits::default();
        let good = two_layers(Options::default());
        let head = 7 + (usize::from(good[4]) | usize::from(good[5]) << 8);
        let mut gz = good[..head].to_vec();
        gz.extend_from_slice(&[0x1F, 0x8B, 8, 0]);
        let err = read_layers(&gz, limits).unwrap_err();
        assert!(err.to_string().contains("gzip-compressed"), "{err}");
        // Pixel data cut short.
        let err = read_layers(&good[..good.len() - 5], limits).unwrap_err();
        assert!(err.to_string().contains("past the end"), "{err}");
        // A document larger than the limits is refused before any pixel.
        let tight = ImportLimits {
            max_alloc_bytes: 16,
            ..limits
        };
        assert!(matches!(
            read_layers(&good, tight),
            Err(CodecError::LimitExceeded(_))
        ));
    }

    #[test]
    fn malformed_pdns_error_and_never_panic() {
        let limits = ImportLimits::default();
        let good = two_layers(Options::default());
        for cut in 0..good.len() {
            let _ = read_layers(&good[..cut], limits);
        }
        let mut flipped = good.clone();
        for i in 0..flipped.len() {
            for bit in [0x01u8, 0x80, 0xFF] {
                flipped[i] ^= bit;
                let _ = read_layers(&flipped, limits);
                flipped[i] ^= bit;
            }
        }
        // A huge declared array, member count and null run.
        for tail in [
            &[16u8, 9, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x7F][..],
            &[5, 9, 0, 0, 0, 1, b'x', 0xFF, 0xFF, 0xFF, 0x7F][..],
            &[16, 9, 0, 0, 0, 2, 0, 0, 0, 14, 0xFF, 0xFF, 0xFF, 0x7F][..],
            &[
                7, 9, 0, 0, 0, 2, 2, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0, 0, 8,
            ][..],
        ] {
            let mut v = b"PDN3\x00\x00\x00\x00\x01\x00".to_vec();
            v.extend_from_slice(&[0; 16]);
            v.extend_from_slice(tail);
            assert!(read_layers(&v, limits).is_err());
        }
        // Deep nesting stops at the depth limit, not the stack.
        let mut deep = b"PDN3\x00\x00\x00\x00\x01\x00".to_vec();
        deep.extend_from_slice(&[0; 16]);
        for i in 0..200i32 {
            deep.push(16);
            deep.extend_from_slice(&(i + 1).to_le_bytes());
            deep.extend_from_slice(&1i32.to_le_bytes());
        }
        assert!(read_layers(&deep, limits).is_err());
    }

    /// A graph stream: the header, one class (`ClassWithMembersAndTypes`)
    /// with a single byte member whose name is `name_len` bytes long, then
    /// `copies` `ClassWithId` records reusing it (10 bytes each).
    fn many_objects_of_one_long_class(name_len: usize, copies: i32) -> Vec<u8> {
        let mut g = vec![0u8];
        g.extend_from_slice(&[0; 16]);
        g.push(5);
        g.extend_from_slice(&1i32.to_le_bytes());
        g.extend_from_slice(&[1, b'C']);
        g.extend_from_slice(&1i32.to_le_bytes());
        let mut n = name_len;
        loop {
            let b = (n & 0x7F) as u8;
            n >>= 7;
            if n == 0 {
                g.push(b);
                break;
            }
            g.push(b | 0x80);
        }
        g.extend(std::iter::repeat_n(b'm', name_len));
        g.extend_from_slice(&[0, 2]);
        g.extend_from_slice(&2i32.to_le_bytes());
        g.push(7);
        for id in 0..copies {
            g.push(1);
            g.extend_from_slice(&(id + 100).to_le_bytes());
            g.extend_from_slice(&1i32.to_le_bytes());
            g.push(9);
        }
        g.push(11);
        g
    }

    #[test]
    fn objects_of_one_class_share_its_names_instead_of_copying_them() {
        let stream = many_objects_of_one_long_class(4000, 2000);
        let graph = parse_graph(&stream).unwrap();
        let (class, first) = graph.class_by_id(1).unwrap();
        assert_eq!(class, "C");
        let Some(Node::Class { class: shared, .. }) = graph.objects.get(&1) else {
            panic!("object 1 is a class object")
        };
        for id in 100..2100 {
            let Some(Node::Class { class, members }) = graph.objects.get(&id) else {
                panic!("object {id} is a class object")
            };
            assert!(
                Rc::ptr_eq(class, shared),
                "object {id} copied its class name"
            );
            assert!(
                Rc::ptr_eq(&members[0].0, &first[0].0),
                "object {id} copied its 4000-byte member name"
            );
            assert_eq!(members[0].1, Val::Int(9));
        }
    }

    #[test]
    fn the_graph_s_memory_is_capped_and_says_so() {
        // 2000 objects of one 4000-byte-named class: shared names cost each
        // object its value and its entry, far under a 1 MiB cap (copied
        // names alone would be 8 MB).
        let stream = many_objects_of_one_long_class(4000, 2000);
        assert!(parse_graph_capped(&stream, 1 << 20).is_ok());
        // Past the cap the parse stops with a limit error, not an
        // allocation failure.
        let stream = many_objects_of_one_long_class(4000, 20_000);
        assert!(matches!(
            parse_graph_capped(&stream, 1 << 20),
            Err(CodecError::LimitExceeded(_))
        ));
        // Kept strings are charged too.
        let mut g = vec![0u8];
        g.extend_from_slice(&[0; 16]);
        for id in 0..64i32 {
            g.push(6);
            g.extend_from_slice(&(id + 1).to_le_bytes());
            g.extend_from_slice(&[0x80, 0x40]);
            g.extend(std::iter::repeat_n(b's', 0x2000));
        }
        g.push(11);
        let err = parse_graph_capped(&g, 512 << 10)
            .err()
            .expect("over the cap");
        assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
        assert!(parse_graph_capped(&g, 4 << 20).is_ok());
    }

    /// W13X-7 round 3: a 1x1, three-layer file whose blocks each declare
    /// 6 MiB (one gzip chunk of zeros, ~6 KB on disk): a block longer than
    /// its layer's `stride * height` is refused before its buffer exists.
    #[test]
    fn a_pixel_block_longer_than_its_layer_is_refused() {
        let px = [1u8, 2, 3, 4];
        let layer = |name| Layer {
            name,
            visible: true,
            opacity: 255,
            blend: "Normal",
            is_background: false,
            rgba: &px,
        };
        let file = pdn_file(
            1,
            1,
            &[layer("a"), layer("b"), layer("c")],
            Options {
                declared_block_len: Some(6 << 20),
                ..Options::default()
            },
        );
        assert!(file.len() < 64 << 10, "{} bytes", file.len());
        for limits in [
            ImportLimits {
                max_alloc_bytes: 8 << 20,
                ..ImportLimits::default()
            },
            ImportLimits::default(),
        ] {
            let err = read_layers(&file, limits).expect_err("refused");
            assert!(err.to_string().contains("longer than its layer"), "{err}");
        }
    }

    /// W13X-7 review: a forged stride cannot stretch the block cap: a 1x1
    /// layer that claims a 64 MiB stride is refused before its block is read.
    #[test]
    fn a_forged_stride_is_refused() {
        let px = [1u8, 2, 3, 4];
        let layer = Layer {
            name: "a",
            visible: true,
            opacity: 255,
            blend: "Normal",
            is_background: false,
            rgba: &px,
        };
        let file = pdn_file(
            1,
            1,
            &[layer],
            Options {
                declared_block_len: Some(64 << 20),
                stride: Some(64 << 20),
                ..Options::default()
            },
        );
        let err = read_layers(&file, ImportLimits::default()).expect_err("refused");
        assert!(err.to_string().contains("stride"), "{err}");
    }

    /// W13X-7 round 3: the block being read is charged on top of every
    /// layer's pixels, not on its own.
    #[test]
    fn a_block_is_charged_on_top_of_all_the_layers_pixels() {
        // 5x3, two layers: 60 bytes a layer, 120 for both, 60 a block.
        let file = two_layers(Options::default());
        let at = |max_alloc_bytes| ImportLimits {
            max_alloc_bytes,
            ..ImportLimits::default()
        };
        assert!(matches!(
            read_layers(&file, at(179)),
            Err(CodecError::LimitExceeded(_))
        ));
        assert_eq!(read_layers(&file, at(180)).unwrap().layers.len(), 2);
    }

    /// W13X-7 round 3: a gzip chunk that inflates past its place is
    /// refused (it inflates into the block's buffer, never beyond it).
    #[test]
    fn a_gzip_chunk_that_inflates_past_its_place_is_refused() {
        let px = [1u8, 2, 3, 4];
        let layer = Layer {
            name: "a",
            visible: true,
            opacity: 255,
            blend: "Normal",
            is_background: false,
            rgba: &px,
        };
        // A 4-byte block whose chunk size says 4 but whose one gzip chunk
        // holds 5 bytes: build a good file, then swap its one chunk.
        let good = pdn_file(1, 1, std::slice::from_ref(&layer), Options::default());
        let doc = read_layers(&good, ImportLimits::default()).unwrap();
        assert_eq!(doc.layers[0].rgba, px);
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut z, &[3, 2, 1, 4, 9]).unwrap();
        let five = z.finish().unwrap();
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut z, &[3, 2, 1, 4]).unwrap();
        let four = z.finish().unwrap();
        let at = good
            .windows(four.len())
            .rposition(|w| w == four.as_slice())
            .expect("the chunk is in the file");
        let mut bad = good[..at - 4].to_vec();
        bad.extend_from_slice(&(five.len() as u32).to_be_bytes());
        bad.extend_from_slice(&five);
        bad.extend_from_slice(&good[at + four.len()..]);
        let err = read_layers(&bad, ImportLimits::default()).expect_err("refused");
        assert!(err.to_string().contains("past its size"), "{err}");
    }
}
