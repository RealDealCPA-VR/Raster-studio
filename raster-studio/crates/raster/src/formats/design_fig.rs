//! W13X-8: a Figma `.fig` read as layers.
//!
//! A `.fig` saved from Figma ("Save local copy") is a ZIP holding
//! `canvas.fig`, the images (`images/<sha1 hex>`) and a thumbnail; the
//! canvas itself is Figma's `fig-kiwi` container:
//!
//! * `fig-kiwi` (8 bytes), a little-endian `u32` version, then chunks, each a
//!   little-endian `u32` length and that many bytes;
//! * chunk 0 is the **schema** in the [kiwi](https://github.com/evanw/kiwi)
//!   binary schema format, chunk 1 the **message** encoded with it; each is
//!   raw DEFLATE or, in newer files, Zstandard (recognised by its magic
//!   number).
//!
//! The schema travels with the file, so the message is decoded generically
//! against it ([`decode_kiwi`]) into [`Json`] values, and the node tree is
//! then read by field *name* (`nodeChanges`, `guid`, `parentIndex`, `type`,
//! `size`, `transform`, `fillPaints`, `textData`, …), the names Figma's
//! schema uses. A field this reader does not look at costs nothing; a
//! property it does not map is written to the notes.
//!
//! The first page (`CANVAS`) that is not Figma's internal-only canvas opens;
//! a `FRAME` placed directly on it becomes an artboard, `GROUP`, `SECTION`,
//! `SYMBOL` and nested frames become groups, the geometric primitives
//! become shape layers (their `fillGeometry` path when the file carries
//! one, else the primitive drawn from the node's size), `TEXT` becomes a
//! text layer and a rectangle filled with an `IMAGE` paint becomes a raster
//! layer. Component instances are not expanded, and say so.
//!
//! # Untrusted input
//!
//! Both chunks inflate through readers capped at [`MAX_CHUNK_BYTES`]; the
//! schema is held to [`MAX_DEFINITIONS`] definitions of at most
//! [`MAX_FIELDS`] fields; the decoder nests at most [`MAX_KIWI_DEPTH`] deep
//! and produces at most [`MAX_KIWI_VALUES`] values (a struct with no fields
//! costs no bytes, so an array's declared length alone cannot bound it);
//! every read checks the bytes remain.
//!
//! The value count alone does not bound memory: every decoded field carries
//! its schema name as its key, so one byte of message can stand for a long
//! name. Schema names are held to [`MAX_NAME_BYTES`], and everything the
//! decoder builds (each value's slot, each key, enum name, string and byte
//! array) is charged, by size, against [`MAX_KIWI_DECODED_BYTES`], and so is
//! the spare capacity an array or a message's field list gains each time it
//! grows. Not charged: the allocator's per-allocation overhead and the old
//! buffer that lives briefly while a growing list is copied. An array
//! reserves at most [`MAX_KIWI_RESERVE`] slots up front and grows as it is
//! charged.

use std::collections::HashMap;
use std::io::Read;

use super::super::malformed;
use super::design_files::{
    ellipse_path, rect_path, Affine, DesignDocument, DesignKind, DesignNode, DesignStroke, Json,
    Reader, StrokeAlign,
};
use crate::codec::{CodecError, ImportFormat, ImportLimits};

/// Largest chunk inflated.
pub const MAX_CHUNK_BYTES: u64 = 256 << 20;
/// Most schema definitions read.
pub const MAX_DEFINITIONS: usize = 4096;
/// Most fields one definition may declare.
pub const MAX_FIELDS: usize = 1024;
/// Deepest nesting the decoder follows.
pub const MAX_KIWI_DEPTH: usize = 64;
/// Most values one message may decode to.
pub const MAX_KIWI_VALUES: usize = 8_000_000;
/// Longest definition or field name a schema may declare (kiwi names are
/// identifiers; Figma's longest are a few dozen bytes).
pub const MAX_NAME_BYTES: usize = 256;
/// Approximate most memory one decoded message may take: value slots,
/// keys, enum names, strings and byte arrays together, counted by their
/// sizes, plus the spare capacity of every list that grows. The allocator's
/// per-allocation overhead and the old buffer alive while a growing list is
/// copied are not counted, so the real peak is somewhat larger.
pub const MAX_KIWI_DECODED_BYTES: usize = 512 << 20;
/// Most slots an array reserves before its items are decoded.
pub const MAX_KIWI_RESERVE: usize = 4096;

const NAME: &str = "Figma";
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

// ---------------------------------------------------------------- container

/// Inflate one chunk: Zstandard when it starts with the Zstandard magic
/// number, raw DEFLATE otherwise.
fn inflate(chunk: &[u8], what: &str) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    let result = if chunk.starts_with(&ZSTD_MAGIC) {
        let decoder = ruzstd::decoding::StreamingDecoder::new(chunk)
            .map_err(|e| malformed(NAME, format!("the {what} does not decompress: {e}")))?;
        decoder
            .take(MAX_CHUNK_BYTES + 1)
            .read_to_end(&mut out)
            .map(|_| ())
    } else {
        flate2::read::DeflateDecoder::new(chunk)
            .take(MAX_CHUNK_BYTES + 1)
            .read_to_end(&mut out)
            .map(|_| ())
    };
    result.map_err(|e| malformed(NAME, format!("the {what} does not decompress: {e}")))?;
    if out.len() as u64 > MAX_CHUNK_BYTES {
        return Err(CodecError::LimitExceeded(format!(
            "the Figma {what} inflates past {MAX_CHUNK_BYTES} bytes"
        )));
    }
    Ok(out)
}

/// The schema and message chunks of a `fig-kiwi` canvas, inflated.
pub fn kiwi_chunks(canvas: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CodecError> {
    if !canvas.starts_with(b"fig-kiwi") {
        return Err(malformed(NAME, "the canvas does not start with fig-kiwi"));
    }
    let mut at = 12usize;
    let mut chunks = Vec::new();
    while chunks.len() < 2 {
        let len = canvas
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(|| malformed(NAME, "the fig-kiwi canvas ends before its chunks"))?;
        let data = canvas
            .get(at + 4..(at + 4).saturating_add(len))
            .ok_or_else(|| malformed(NAME, "a fig-kiwi chunk runs past the file"))?;
        chunks.push(data);
        at += 4 + len;
    }
    Ok((
        inflate(chunks[0], "schema")?,
        inflate(chunks[1], "canvas data")?,
    ))
}

// --------------------------------------------------------------------- kiwi

struct Bytes<'a> {
    b: &'a [u8],
    i: usize,
}

impl Bytes<'_> {
    fn end() -> CodecError {
        malformed(NAME, "the kiwi data ends early")
    }

    fn byte(&mut self) -> Result<u8, CodecError> {
        let v = *self.b.get(self.i).ok_or_else(Self::end)?;
        self.i += 1;
        Ok(v)
    }

    fn var_uint(&mut self) -> Result<u32, CodecError> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let b = self.byte()?;
            value |= u32::from(b & 127) << shift;
            if b & 128 == 0 {
                break;
            }
        }
        Ok(value)
    }

    fn var_int(&mut self) -> Result<i32, CodecError> {
        let v = self.var_uint()?;
        Ok(if v & 1 != 0 {
            !((v >> 1) as i32)
        } else {
            (v >> 1) as i32
        })
    }

    fn var_uint64(&mut self) -> Result<u64, CodecError> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            if shift == 56 {
                value |= u64::from(b) << shift;
                break;
            }
            value |= u64::from(b & 127) << shift;
            if b & 128 == 0 {
                break;
            }
            shift += 7;
        }
        Ok(value)
    }

    fn var_float(&mut self) -> Result<f32, CodecError> {
        let first = self.byte()?;
        if first == 0 {
            return Ok(0.0);
        }
        let rest = self.b.get(self.i..self.i + 3).ok_or_else(Self::end)?;
        self.i += 3;
        let bits = u32::from(first)
            | u32::from(rest[0]) << 8
            | u32::from(rest[1]) << 16
            | u32::from(rest[2]) << 24;
        Ok(f32::from_bits(bits.rotate_left(23)))
    }

    fn string(&mut self) -> Result<String, CodecError> {
        let rest = &self.b[self.i.min(self.b.len())..];
        let len = rest.iter().position(|b| *b == 0).ok_or_else(Self::end)?;
        let s = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.i += len + 1;
        Ok(s)
    }
}

/// A field's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KiwiType {
    Bool,
    Byte,
    Int,
    Uint,
    Float,
    Str,
    Int64,
    Uint64,
    Def(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KiwiKind {
    Enum,
    Struct,
    Message,
}

#[derive(Debug, Clone)]
pub struct KiwiField {
    pub name: String,
    pub ty: KiwiType,
    pub array: bool,
    pub value: u32,
}

#[derive(Debug, Clone)]
pub struct KiwiDef {
    pub name: String,
    pub kind: KiwiKind,
    pub fields: Vec<KiwiField>,
}

/// A binary kiwi schema.
pub fn decode_schema(bytes: &[u8]) -> Result<Vec<KiwiDef>, CodecError> {
    let mut b = Bytes { b: bytes, i: 0 };
    let count = b.var_uint()? as usize;
    if count > MAX_DEFINITIONS {
        return Err(CodecError::LimitExceeded(format!(
            "the Figma schema declares {count} definitions"
        )));
    }
    let mut defs = Vec::with_capacity(count);
    let named = |name: String| -> Result<String, CodecError> {
        if name.len() > MAX_NAME_BYTES {
            return Err(CodecError::LimitExceeded(format!(
                "a Figma schema name is {} bytes long (the most read is {MAX_NAME_BYTES})",
                name.len()
            )));
        }
        Ok(name)
    };
    for _ in 0..count {
        let name = named(b.string()?)?;
        let kind = match b.byte()? {
            0 => KiwiKind::Enum,
            1 => KiwiKind::Struct,
            2 => KiwiKind::Message,
            k => return Err(malformed(NAME, format!("schema definition kind {k}"))),
        };
        let fields_n = b.var_uint()? as usize;
        if fields_n > MAX_FIELDS {
            return Err(CodecError::LimitExceeded(format!(
                "a Figma schema definition declares {fields_n} fields"
            )));
        }
        let mut fields = Vec::with_capacity(fields_n);
        for _ in 0..fields_n {
            let name = named(b.string()?)?;
            let ty = b.var_int()?;
            let array = b.byte()? & 1 != 0;
            let value = b.var_uint()?;
            let ty = match ty {
                -1 => KiwiType::Bool,
                -2 => KiwiType::Byte,
                -3 => KiwiType::Int,
                -4 => KiwiType::Uint,
                -5 => KiwiType::Float,
                -6 => KiwiType::Str,
                -7 => KiwiType::Int64,
                -8 => KiwiType::Uint64,
                t if t >= 0 && (t as usize) < count => KiwiType::Def(t as usize),
                t => return Err(malformed(NAME, format!("schema field type {t}"))),
            };
            fields.push(KiwiField {
                name,
                ty,
                array,
                value,
            });
        }
        defs.push(KiwiDef { name, kind, fields });
    }
    Ok(defs)
}

struct Decoder<'a> {
    b: Bytes<'a>,
    defs: &'a [KiwiDef],
    values: usize,
    /// Bytes the decoded value takes so far (see [`MAX_KIWI_DECODED_BYTES`]).
    bytes: usize,
    /// The most `bytes` may reach.
    budget: usize,
    /// Byte arrays, kept aside: a `byte[]` field decodes to
    /// `{"$bytes": index}`.
    blobs: Vec<Vec<u8>>,
}

impl Decoder<'_> {
    fn tick(&mut self) -> Result<(), CodecError> {
        self.values += 1;
        if self.values > MAX_KIWI_VALUES {
            return Err(CodecError::LimitExceeded(format!(
                "the Figma canvas holds more than {MAX_KIWI_VALUES} values"
            )));
        }
        self.charge(std::mem::size_of::<(String, Json)>())
    }

    /// Make room for one more item in `v`, charging the spare capacity a
    /// doubling adds (the slot the item fills is charged by [`Self::tick`]),
    /// so a growing array or message cannot hold uncharged slack.
    fn room<T>(&mut self, v: &mut Vec<T>) -> Result<(), CodecError> {
        if v.len() == v.capacity() {
            let extra = v.capacity().max(4);
            self.charge(extra.saturating_mul(std::mem::size_of::<T>()))?;
            v.reserve_exact(extra);
        }
        Ok(())
    }

    /// Count `n` bytes of decoded memory against the budget.
    fn charge(&mut self, n: usize) -> Result<(), CodecError> {
        self.bytes = self.bytes.saturating_add(n);
        if self.bytes > self.budget {
            return Err(CodecError::LimitExceeded(format!(
                "the Figma canvas decodes to more than {} bytes",
                self.budget
            )));
        }
        Ok(())
    }

    /// A copy of a schema name, charged.
    fn name(&mut self, name: &str) -> Result<String, CodecError> {
        self.charge(name.len())?;
        Ok(name.to_owned())
    }

    fn one(&mut self, ty: KiwiType, depth: usize) -> Result<Json, CodecError> {
        self.tick()?;
        Ok(match ty {
            KiwiType::Bool => Json::Bool(self.b.byte()? != 0),
            KiwiType::Byte => Json::Num(f64::from(self.b.byte()?)),
            KiwiType::Int => Json::Num(f64::from(self.b.var_int()?)),
            KiwiType::Uint => Json::Num(f64::from(self.b.var_uint()?)),
            KiwiType::Float => Json::Num(f64::from(self.b.var_float()?)),
            KiwiType::Str => {
                let s = self.b.string()?;
                self.charge(s.len())?;
                Json::Str(s)
            }
            KiwiType::Int64 | KiwiType::Uint64 => Json::Num(self.b.var_uint64()? as f64),
            KiwiType::Def(d) => self.def(d, depth + 1)?,
        })
    }

    fn field(&mut self, f: &KiwiField, depth: usize) -> Result<Json, CodecError> {
        if !f.array {
            return self.one(f.ty, depth);
        }
        let len = self.b.var_uint()? as usize;
        if f.ty == KiwiType::Byte {
            self.charge(len)?;
            let bytes = self
                .b
                .b
                .get(self.b.i..self.b.i.saturating_add(len))
                .ok_or_else(Bytes::end)?
                .to_vec();
            self.b.i += len;
            self.blobs.push(bytes);
            self.tick()?;
            return Ok(Json::Obj(vec![(
                "$bytes".into(),
                Json::Num((self.blobs.len() - 1) as f64),
            )]));
        }
        let mut items = Vec::with_capacity(len.min(MAX_KIWI_RESERVE));
        for _ in 0..len {
            let item = self.one(f.ty, depth)?;
            self.room(&mut items)?;
            items.push(item);
        }
        Ok(Json::Arr(items))
    }

    fn def(&mut self, d: usize, depth: usize) -> Result<Json, CodecError> {
        if depth > MAX_KIWI_DEPTH {
            return Err(CodecError::LimitExceeded(format!(
                "the Figma canvas nests more than {MAX_KIWI_DEPTH} deep"
            )));
        }
        let def = &self.defs[d];
        match def.kind {
            KiwiKind::Enum => {
                let v = self.b.var_uint()?;
                let name = match def.fields.iter().find(|f| f.value == v) {
                    Some(f) => self.name(&f.name)?,
                    None => {
                        let name = format!("{}#{v}", def.name);
                        self.charge(name.len())?;
                        name
                    }
                };
                Ok(Json::Str(name))
            }
            KiwiKind::Struct => {
                let mut out = Vec::with_capacity(def.fields.len().min(MAX_KIWI_RESERVE));
                for f in &def.fields {
                    let key = self.name(&f.name)?;
                    let v = self.field(f, depth)?;
                    out.push((key, v));
                }
                Ok(Json::Obj(out))
            }
            KiwiKind::Message => {
                let mut out = Vec::new();
                loop {
                    let tag = self.b.var_uint()?;
                    if tag == 0 {
                        break;
                    }
                    let f = def.fields.iter().find(|f| f.value == tag).ok_or_else(|| {
                        malformed(
                            NAME,
                            format!("field {tag} is not in the schema's {}", def.name),
                        )
                    })?;
                    let key = self.name(&f.name)?;
                    let v = self.field(f, depth)?;
                    self.room(&mut out)?;
                    out.push((key, v));
                }
                Ok(Json::Obj(out))
            }
        }
    }
}

/// Decode `message` against `schema`, rooted at the definition `root`.
/// Returns the value and the byte arrays it refers to by index.
pub fn decode_kiwi(
    schema: &[KiwiDef],
    root: &str,
    message: &[u8],
) -> Result<(Json, Vec<Vec<u8>>), CodecError> {
    decode_kiwi_within(schema, root, message, MAX_KIWI_DECODED_BYTES)
}

/// [`decode_kiwi`] with its own memory budget in place of
/// [`MAX_KIWI_DECODED_BYTES`] (never above it).
pub fn decode_kiwi_within(
    schema: &[KiwiDef],
    root: &str,
    message: &[u8],
    budget: usize,
) -> Result<(Json, Vec<Vec<u8>>), CodecError> {
    let d = schema
        .iter()
        .position(|d| d.name == root)
        .ok_or_else(|| malformed(NAME, format!("the schema has no {root}")))?;
    let mut decoder = Decoder {
        b: Bytes { b: message, i: 0 },
        defs: schema,
        values: 0,
        bytes: 0,
        budget: budget.min(MAX_KIWI_DECODED_BYTES),
        blobs: Vec::new(),
    };
    let value = decoder.def(d, 0)?;
    Ok((value, decoder.blobs))
}

// -------------------------------------------------------------------- nodes

type Guid = (u64, u64);

fn guid(j: Option<&Json>) -> Option<Guid> {
    let j = j?;
    Some((
        j.get("sessionID")?.as_f64()? as u64,
        j.get("localID")?.as_f64()? as u64,
    ))
}

fn fnum(j: &Json, k: &str, d: f64) -> f64 {
    j.get(k).and_then(Json::as_f64).unwrap_or(d)
}

fn color(j: Option<&Json>, opacity: f64) -> Option<[f32; 4]> {
    let j = j?;
    let c = |k: &str, d: f64| fnum(j, k, d).clamp(0.0, 1.0) as f32;
    Some([
        c("r", 0.0),
        c("g", 0.0),
        c("b", 0.0),
        (fnum(j, "a", 1.0) * opacity).clamp(0.0, 1.0) as f32,
    ])
}

/// A command blob (`fillGeometry[].commandsBlob`) as SVG path data:
/// `0` close, `1` move, `2` line, `3` quadratic, `4` cubic, each followed
/// by its little-endian `f32` coordinates.
pub fn commands_to_svg(blob: &[u8]) -> Option<String> {
    let mut out = String::new();
    let mut i = 0usize;
    let f = |i: usize| -> Option<f32> {
        let b = blob.get(i..i + 4)?;
        let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        v.is_finite().then_some(v)
    };
    while i < blob.len() {
        let (cmd, n) = match blob[i] {
            0 => ("Z", 0),
            1 => ("M", 2),
            2 => ("L", 2),
            3 => ("Q", 4),
            4 => ("C", 6),
            _ => return None,
        };
        i += 1;
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(cmd);
        for k in 0..n {
            let v = f(i)?;
            i += 4;
            if k > 0 {
                out.push(' ');
            }
            out.push_str(&format!("{}", (v * 1000.0).round() / 1000.0));
        }
    }
    (!out.is_empty()).then_some(out)
}

struct Fig<'a, 'z> {
    r: &'a mut Reader<'z>,
    nodes: HashMap<Guid, &'a Json>,
    children: HashMap<Guid, Vec<(String, Guid)>>,
    blobs: &'a [Vec<u8>],
    msg_blobs: &'a [Json],
}

impl Fig<'_, '_> {
    fn blob(&self, j: Option<&Json>) -> Option<&[u8]> {
        let i = j?.get("$bytes")?.as_f64()? as usize;
        self.blobs.get(i).map(Vec::as_slice)
    }

    /// The node's own `fillGeometry` (or `strokeGeometry` when unfilled)
    /// path, from the message's blob table.
    fn geometry(&self, node: &Json) -> Option<String> {
        let paths: Vec<String> = node
            .get("fillGeometry")
            .map(Json::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|g| {
                let index = g.get("commandsBlob")?.as_f64()? as usize;
                let bytes = self.blob(self.msg_blobs.get(index)?.get("bytes"))?;
                commands_to_svg(bytes)
            })
            .collect();
        (!paths.is_empty()).then(|| paths.join(" "))
    }

    /// Node and paint blend modes and masks: none is kept, each is named.
    fn blend_and_mask_notes(&mut self, node: &Json, name: &str) {
        let blend = |j: &Json| {
            j.get("blendMode")
                .and_then(Json::as_str)
                .filter(|m| *m != "NORMAL" && *m != "PASS_THROUGH")
                .map(|m| m.to_ascii_lowercase().replace('_', " "))
        };
        if let Some(mode) = blend(node) {
            self.r.note(format!(
                "layer {name:?} has the {mode} blend mode, which was not kept (it opened as Normal)"
            ));
        }
        for (key, what) in [("fillPaints", "fill"), ("strokePaints", "stroke")] {
            for paint in Self::paints(node, key) {
                if let Some(mode) = blend(&paint) {
                    self.r.note(format!(
                        "layer {name:?} has a {what} with the {mode} blend mode, which was not \
                         kept (it opened as Normal)"
                    ));
                }
            }
        }
        if node.get("isMask").and_then(Json::as_bool).unwrap_or(false) {
            self.r.note(format!(
                "layer {name:?} is a mask, which was not kept: it opened as an ordinary \
                 layer and the layers above it in its group opened unclipped"
            ));
        }
    }

    fn paints(node: &Json, key: &str) -> Vec<Json> {
        node.get(key)
            .map(Json::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter(|p| p.get("visible").and_then(Json::as_bool).unwrap_or(true))
            .cloned()
            .collect()
    }

    fn solid(&mut self, paints: &[Json], what: &str, name: &str) -> Option<[f32; 4]> {
        if paints.len() > 1 {
            self.r.note(format!(
                "layer {name:?} has {} {what}s; only the top-most was kept",
                paints.len()
            ));
        }
        let p = paints.last()?;
        match p.get("type").and_then(Json::as_str).unwrap_or("SOLID") {
            "SOLID" => color(p.get("color"), fnum(p, "opacity", 1.0)),
            other => {
                self.r.note(format!(
                    "layer {name:?} has a {} {what}, which was not kept (only flat colours are)",
                    other.to_ascii_lowercase().replace('_', " ")
                ));
                None
            }
        }
    }

    fn stroke(&mut self, node: &Json, name: &str) -> Option<DesignStroke> {
        let paints = Self::paints(node, "strokePaints");
        let color = self.solid(&paints, "stroke", name)?;
        Some(DesignStroke {
            color,
            width: fnum(node, "strokeWeight", 1.0).clamp(0.0, 10_000.0) as f32,
            align: match node.get("strokeAlign").and_then(Json::as_str) {
                Some("INSIDE") => StrokeAlign::Inside,
                Some("OUTSIDE") => StrokeAlign::Outside,
                _ => StrokeAlign::Center,
            },
        })
    }

    fn kids(&mut self, id: Guid, at: Affine, depth: usize) -> Result<Vec<DesignNode>, CodecError> {
        let mut list = self.children.get(&id).cloned().unwrap_or_default();
        list.sort();
        let mut out = Vec::new();
        for (_, child) in list {
            if let Some(n) = self.node(child, at, depth + 1)? {
                out.push(n);
            }
        }
        Ok(out)
    }

    fn node(
        &mut self,
        id: Guid,
        parent: Affine,
        depth: usize,
    ) -> Result<Option<DesignNode>, CodecError> {
        self.r.count(depth)?;
        let Some(&node) = self.nodes.get(&id) else {
            return Ok(None);
        };
        let ty = node.get("type").and_then(Json::as_str).unwrap_or("");
        let name = node
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or(ty)
            .to_string();
        let size = node.get("size");
        let w = size
            .map(|s| fnum(s, "x", 0.0))
            .unwrap_or(0.0)
            .clamp(0.0, 1e6);
        let h = size
            .map(|s| fnum(s, "y", 0.0))
            .unwrap_or(0.0)
            .clamp(0.0, 1e6);
        let local = node
            .get("transform")
            .map(|t| {
                Affine([
                    fnum(t, "m00", 1.0),
                    fnum(t, "m10", 0.0),
                    fnum(t, "m01", 0.0),
                    fnum(t, "m11", 1.0),
                    fnum(t, "m02", 0.0),
                    fnum(t, "m12", 0.0),
                ])
            })
            .filter(|a| a.0.iter().all(|v| v.is_finite() && v.abs() < 1e7))
            .unwrap_or(Affine::IDENTITY);
        let transform = parent.then(local);
        self.blend_and_mask_notes(node, &name);
        if node
            .get("effects")
            .map(Json::as_arr)
            .unwrap_or(&[])
            .iter()
            .any(|e| e.get("visible").and_then(Json::as_bool).unwrap_or(true))
        {
            self.r.note(format!(
                "layer {name:?} has effects (shadows / blurs), which were not kept"
            ));
        }
        let fills = Self::paints(node, "fillPaints");
        let kind = match ty {
            "FRAME" | "SYMBOL" if depth == 0 => {
                let background = self.solid(&fills, "fill", &name);
                if fnum(node, "cornerRadius", 0.0) > 0.0 {
                    self.r.note(format!(
                        "frame {name:?} has rounded corners; its artboard is square"
                    ));
                }
                DesignKind::Artboard {
                    background,
                    children: self.kids(id, transform, depth)?,
                }
            }
            "FRAME" | "GROUP" | "SECTION" | "SYMBOL" => {
                if ty == "FRAME" && !fills.is_empty() {
                    self.r.note(format!(
                        "nested frame {name:?} has a fill, which was not kept (it opened as a group)"
                    ));
                }
                DesignKind::Group {
                    children: self.kids(id, transform, depth)?,
                }
            }
            "BOOLEAN_OPERATION" => match self.geometry(node) {
                Some(path_svg) => {
                    let fill = self.solid(&fills, "fill", &name);
                    let stroke = self.stroke(node, &name);
                    DesignKind::Shape {
                        path_svg,
                        fill,
                        stroke,
                        even_odd: false,
                    }
                }
                None => {
                    self.r.note(format!(
                        "boolean shape {name:?} carries no combined outline; its parts opened as a group"
                    ));
                    DesignKind::Group {
                        children: self.kids(id, transform, depth)?,
                    }
                }
            },
            "RECTANGLE" | "ROUNDED_RECTANGLE" | "ELLIPSE" | "LINE" | "REGULAR_POLYGON" | "STAR"
            | "VECTOR" => {
                if let Some(image) = fills
                    .last()
                    .filter(|p| p.get("type").and_then(Json::as_str) == Some("IMAGE"))
                {
                    let hash = self
                        .blob(image.at(&["image", "hash"]))
                        .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>());
                    if self.r.zip.is_empty() {
                        self.r.note(format!(
                            "image {name:?} was left out: a bare fig-kiwi canvas does not hold \
                             its images (they travel beside it in the .fig archive)"
                        ));
                        return Ok(None);
                    }
                    let Some(hash) = hash.filter(|h| !h.is_empty()) else {
                        self.r
                            .note(format!("image {name:?} names no image; it was left out"));
                        return Ok(None);
                    };
                    if !transform.is_translation() {
                        self.r.note(format!(
                            "image layer {name:?} is rotated or scaled; it was placed unrotated"
                        ));
                    }
                    if ty != "RECTANGLE" && ty != "ROUNDED_RECTANGLE" {
                        self.r.note(format!(
                            "image {name:?} fills a non-rectangular shape; its bounding box was used"
                        ));
                    }
                    let Some((bw, bh, rgba)) =
                        self.r.bitmap(&format!("images/{hash}"), &name, (w, h))?
                    else {
                        return Ok(None);
                    };
                    DesignKind::Bitmap {
                        width: bw,
                        height: bh,
                        rgba,
                    }
                } else {
                    let path_svg = match self.geometry(node) {
                        Some(p) => p,
                        None => match ty {
                            "ELLIPSE" => ellipse_path(0.0, 0.0, w, h),
                            "LINE" => format!("M0 0 L{w} 0"),
                            "RECTANGLE" | "ROUNDED_RECTANGLE" => {
                                rect_path(w, h, fnum(node, "cornerRadius", 0.0))
                            }
                            _ => {
                                self.r.note(format!(
                                    "{} {name:?} carries no outline in the file; its bounding box was drawn",
                                    ty.to_ascii_lowercase().replace('_', " ")
                                ));
                                rect_path(w, h, 0.0)
                            }
                        },
                    };
                    let fill = self.solid(&fills, "fill", &name);
                    let stroke = self.stroke(node, &name);
                    DesignKind::Shape {
                        path_svg,
                        fill,
                        stroke,
                        even_odd: false,
                    }
                }
            }
            "TEXT" => {
                let text = node
                    .at(&["textData", "characters"])
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string();
                let family = node
                    .at(&["fontName", "family"])
                    .and_then(Json::as_str)
                    .unwrap_or("Inter")
                    .to_string();
                let style = node
                    .at(&["fontName", "style"])
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if node
                    .at(&["textData", "styleOverrideTable"])
                    .is_some_and(|t| !t.as_arr().is_empty())
                {
                    self.r.note(format!(
                        "text layer {name:?} has more than one styled range; its base font, \
                         size and colour were applied to all of it"
                    ));
                }
                let color = self
                    .solid(&fills, "fill", &name)
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]);
                let point =
                    node.get("textAutoResize").and_then(Json::as_str) == Some("WIDTH_AND_HEIGHT");
                DesignKind::Text {
                    text,
                    font_family: family,
                    bold: ["bold", "black", "heavy", "semibold"]
                        .iter()
                        .any(|s| style.contains(s)),
                    italic: style.contains("italic"),
                    size: fnum(node, "fontSize", 12.0).clamp(0.5, 4000.0) as f32,
                    color,
                    box_width: (!point).then_some(w as f32),
                }
            }
            "INSTANCE" => {
                self.r.note(format!(
                    "component instance {name:?} was not expanded (instances are not read); it was left out"
                ));
                return Ok(None);
            }
            "SLICE" => {
                self.r.note(format!(
                    "slice {name:?} is an export marker with nothing to draw; it was left out"
                ));
                return Ok(None);
            }
            other => {
                self.r.note(format!(
                    "layer {name:?} is a Figma {other:?} node, which this reader does not know; it was left out"
                ));
                return Ok(None);
            }
        };
        Ok(Some(DesignNode {
            name,
            visible: node.get("visible").and_then(Json::as_bool).unwrap_or(true),
            opacity: fnum(node, "opacity", 1.0).clamp(0.0, 1.0) as f32,
            transform,
            width: w,
            height: h,
            kind,
        }))
    }
}

/// Read the node tree of a decoded Figma message.
pub fn read_message(
    message: &Json,
    blobs: &[Vec<u8>],
    zip: &[u8],
    limits: ImportLimits,
) -> Result<DesignDocument, CodecError> {
    let mut r = Reader::new(zip, ImportFormat::Fig, limits);
    let changes = message.get("nodeChanges").map(Json::as_arr).unwrap_or(&[]);
    let mut nodes = HashMap::new();
    let mut children: HashMap<Guid, Vec<(String, Guid)>> = HashMap::new();
    for change in changes {
        let Some(id) = guid(change.get("guid")) else {
            continue;
        };
        if change.get("phase").and_then(Json::as_str) == Some("REMOVED") {
            continue;
        }
        nodes.insert(id, change);
        if let Some(parent) = guid(change.at(&["parentIndex", "guid"])) {
            let position = change
                .at(&["parentIndex", "position"])
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            children.entry(parent).or_default().push((position, id));
        }
    }
    let document = changes
        .iter()
        .find(|c| c.get("type").and_then(Json::as_str) == Some("DOCUMENT"))
        .and_then(|c| guid(c.get("guid")))
        .ok_or_else(|| malformed(NAME, "the canvas has no DOCUMENT node"))?;
    let mut pages = children.get(&document).cloned().unwrap_or_default();
    pages.sort();
    let pages: Vec<Guid> = pages
        .into_iter()
        .map(|(_, g)| g)
        .filter(|g| {
            nodes.get(g).is_some_and(|n| {
                n.get("type").and_then(Json::as_str) == Some("CANVAS")
                    && !n
                        .get("internalOnly")
                        .and_then(Json::as_bool)
                        .unwrap_or(false)
            })
        })
        .collect();
    let first = *pages
        .first()
        .ok_or_else(|| CodecError::Unsupported("this Figma file has no pages".into()))?;
    let page_name = nodes
        .get(&first)
        .and_then(|n| n.get("name"))
        .and_then(Json::as_str)
        .unwrap_or("Page 1")
        .to_string();
    if pages.len() > 1 {
        r.note(format!(
            "only the first page ({page_name:?}) was opened; the file's {} other page(s) were not",
            pages.len() - 1
        ));
    }
    let msg_blobs = message.get("blobs").map(Json::as_arr).unwrap_or(&[]);
    let mut fig = Fig {
        r: &mut r,
        nodes,
        children,
        blobs,
        msg_blobs,
    };
    let mut list = fig.children.get(&first).cloned().unwrap_or_default();
    list.sort();
    let mut top = Vec::new();
    for (_, id) in list {
        if let Some(n) = fig.node(id, Affine::IDENTITY, 0)? {
            top.push(n);
        }
    }
    Ok(DesignDocument {
        format: ImportFormat::Fig,
        nodes: top,
        notes: r.notes,
        opened: format!("page {page_name:?}"),
    })
}

/// A `.fig` (a ZIP holding `canvas.fig`, or a bare `fig-kiwi` canvas) as
/// layers.
pub fn read_fig(bytes: &[u8], limits: ImportLimits) -> Result<DesignDocument, CodecError> {
    let (canvas, zip): (std::borrow::Cow<[u8]>, &[u8]) = if bytes.starts_with(b"fig-kiwi") {
        (std::borrow::Cow::Borrowed(bytes), &[])
    } else if bytes.starts_with(b"PK\x03\x04") {
        let canvas =
            super::zip_entry(bytes, "canvas.fig", MAX_CHUNK_BYTES, NAME)?.ok_or_else(|| {
                CodecError::Unsupported("this Figma archive has no canvas.fig".into())
            })?;
        (std::borrow::Cow::Owned(canvas), bytes)
    } else {
        return Err(malformed(
            NAME,
            "neither a fig-kiwi canvas nor a ZIP archive",
        ));
    };
    let (schema, data) = kiwi_chunks(&canvas)?;
    let schema = decode_schema(&schema)?;
    let (message, blobs) = decode_kiwi(&schema, "Message", &data)?;
    read_message(&message, &blobs, zip, limits)
}
