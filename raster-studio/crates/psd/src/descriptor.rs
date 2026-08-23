//! Action descriptors — the key/value trees Photoshop stores inside tagged
//! blocks.
//!
//! Every structured thing a modern `.psd` carries that is not pixels is a
//! descriptor: the text of a type layer, the parameters of a solid-colour fill,
//! the settings of a layer effect. The encoding is a tagged tree:
//!
//! ```text
//! descriptor := unicode-name  key(class-id)  u32 item-count
//!               item*
//! item       := key  ostype(4)  value
//! ```
//!
//! A *key* is a `u32` length followed by that many ASCII bytes, except that a
//! length of zero means "four bytes follow" — the four-character-code case,
//! which is by far the common one. Reading a zero length as "no bytes" walks
//! the parser four bytes behind for the rest of the file — and by the same
//! token an *empty* key cannot be written at all, so [`Descriptor::push`] and
//! [`Descriptor::write`] refuse one instead of emitting a stream nothing can
//! read back.
//!
//! # Untrusted input
//!
//! Descriptors nest, and nothing in the encoding stops them nesting a million
//! deep. Parsing is therefore explicitly depth-limited
//! ([`ReadOptions::max_descriptor_depth`]) rather than relying on the native
//! stack, because a stack overflow is an abort, not an error — it cannot be
//! caught, and it takes the host application with it. Item counts are bounded
//! before the `Vec` that holds them is reserved.

use crate::bytes::{Cursor, Sink};
use crate::error::{tag_name, PsdError, PsdResult};
use crate::limits::{check_limit, ReadOptions};

/// A descriptor: a class id, an optional display name, and ordered items.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Descriptor {
    /// Display name. Almost always empty in files Photoshop writes.
    pub name: String,
    /// The class this descriptor describes, e.g. `TxLr` for a text layer.
    pub class_id: String,
    pub items: Vec<(String, Value)>,
}

impl Descriptor {
    pub fn new(class_id: &str) -> Self {
        Descriptor {
            name: String::new(),
            class_id: class_id.to_string(),
            items: Vec::new(),
        }
    }

    /// First item with this key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The string value of a `TEXT` item.
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The numeric value of a `doub`, `UntF` or `long` item.
    pub fn number(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            Value::Double(v) => Some(*v),
            Value::UnitFloat { value, .. } => Some(*value),
            Value::Integer(v) => Some(f64::from(*v)),
            Value::LargeInteger(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// The child descriptor of an `Objc` item.
    pub fn descriptor(&self, key: &str) -> Option<&Descriptor> {
        match self.get(key)? {
            Value::Descriptor(d) => Some(d),
            _ => None,
        }
    }

    /// Add an item, refusing a key the format cannot express.
    ///
    /// See [`write_key`]: an empty key has no encoding, and writing one
    /// desynchronises every reader from that point on. Refusing here means the
    /// mistake is reported where it is made rather than becoming a file nobody
    /// can open.
    pub fn push(&mut self, key: &str, value: Value) -> PsdResult<()> {
        check_key(key)?;
        self.items.push((key.to_string(), value));
        Ok(())
    }

    /// Parse a descriptor. The caller has already consumed any version word.
    pub fn read(cur: &mut Cursor<'_>, opts: &ReadOptions) -> PsdResult<Self> {
        read_descriptor(cur, opts, 0)
    }

    /// Serialise, or refuse if any key in the tree cannot be encoded.
    ///
    /// [`Descriptor::items`] and [`Descriptor::class_id`] are public, so
    /// [`Descriptor::push`] is not the only way an unwritable key can get in.
    /// This is the check that actually makes one impossible to *emit*.
    pub fn write(&self, sink: &mut Sink) -> PsdResult<()> {
        sink.unicode_string(&self.name);
        write_key(sink, &self.class_id)?;
        sink.u32(self.items.len() as u32);
        for (key, value) in &self.items {
            write_key(sink, key)?;
            value.write(sink)?;
        }
        Ok(())
    }
}

/// One descriptor value.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// `Objc` and `GlbO`, which have the same shape.
    Descriptor(Descriptor),
    /// `VlLs`
    List(Vec<Value>),
    /// `doub`
    Double(f64),
    /// `UntF` — a double plus a unit code such as `#Pxl` or `#Prc`.
    UnitFloat { unit: [u8; 4], value: f64 },
    /// `TEXT`
    Text(String),
    /// `enum`
    Enumerated { type_id: String, value: String },
    /// `long`
    Integer(i32),
    /// `comp`
    LargeInteger(i64),
    /// `bool`
    Bool(bool),
    /// `type` and `GlbC`
    Class { name: String, class_id: String },
    /// `alis` — an opaque platform file reference, preserved verbatim.
    Alias(Vec<u8>),
    /// `tdta` — opaque bytes, most famously the text engine data.
    RawData(Vec<u8>),
    /// `obj ` — a reference chain.
    Reference(Vec<RefItem>),
}

/// One link in an `obj ` reference chain.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RefItem {
    Property {
        name: String,
        class_id: String,
        key_id: String,
    },
    Class {
        name: String,
        class_id: String,
    },
    Enumerated {
        name: String,
        class_id: String,
        type_id: String,
        value: String,
    },
    Offset {
        name: String,
        class_id: String,
        value: u32,
    },
    Identifier(u32),
    Index(u32),
    Name {
        name: String,
        class_id: String,
        value: String,
    },
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::Text(s.to_string())
    }
}

impl Value {
    fn ostype(&self) -> [u8; 4] {
        match self {
            Value::Descriptor(_) => *b"Objc",
            Value::List(_) => *b"VlLs",
            Value::Double(_) => *b"doub",
            Value::UnitFloat { .. } => *b"UntF",
            Value::Text(_) => *b"TEXT",
            Value::Enumerated { .. } => *b"enum",
            Value::Integer(_) => *b"long",
            Value::LargeInteger(_) => *b"comp",
            Value::Bool(_) => *b"bool",
            Value::Class { .. } => *b"type",
            Value::Alias(_) => *b"alis",
            Value::RawData(_) => *b"tdta",
            Value::Reference(_) => *b"obj ",
        }
    }

    pub fn write(&self, sink: &mut Sink) -> PsdResult<()> {
        sink.tag(&self.ostype());
        self.write_body(sink)
    }

    fn write_body(&self, sink: &mut Sink) -> PsdResult<()> {
        match self {
            Value::Descriptor(d) => d.write(sink)?,
            Value::List(items) => {
                sink.u32(items.len() as u32);
                for v in items {
                    v.write(sink)?;
                }
            }
            Value::Double(v) => sink.f64(*v),
            Value::UnitFloat { unit, value } => {
                sink.tag(unit);
                sink.f64(*value);
            }
            Value::Text(s) => sink.unicode_string(s),
            Value::Enumerated { type_id, value } => {
                write_key(sink, type_id)?;
                write_key(sink, value)?;
            }
            Value::Integer(v) => sink.i32(*v),
            Value::LargeInteger(v) => {
                sink.u32((*v as u64 >> 32) as u32);
                sink.u32(*v as u32);
            }
            Value::Bool(v) => sink.u8(u8::from(*v)),
            Value::Class { name, class_id } => {
                sink.unicode_string(name);
                write_key(sink, class_id)?;
            }
            Value::Alias(bytes) | Value::RawData(bytes) => {
                sink.u32(bytes.len() as u32);
                sink.bytes(bytes);
            }
            Value::Reference(items) => {
                sink.u32(items.len() as u32);
                for item in items {
                    write_ref_item(sink, item)?;
                }
            }
        }
        Ok(())
    }
}

/// Refuse a key the encoding cannot represent.
///
/// The encoding is "a `u32` length, then that many bytes, except that zero
/// means four bytes follow". A zero-length key therefore has *no* encoding at
/// all: writing `u32(0)` and no bytes is indistinguishable from a
/// four-character code, and a reader consumes the four bytes that come next —
/// which are the value's ostype — and is four bytes out of step for the rest of
/// the stream. There is nothing to fix on the writing side, so the only correct
/// answer is to refuse.
fn check_key(key: &str) -> PsdResult<()> {
    if key.is_empty() {
        return Err(PsdError::InvalidDocument(
            "a descriptor key may not be empty: the format encodes a zero \
             length as 'four bytes follow', so an empty key cannot be \
             distinguished from a four-character code"
                .to_string(),
        ));
    }
    Ok(())
}

fn write_key(sink: &mut Sink, key: &str) -> PsdResult<()> {
    check_key(key)?;
    let b = key.as_bytes();
    if b.len() == 4 {
        // The four-character-code form: a zero length, then exactly four bytes.
        sink.u32(0);
    } else {
        sink.u32(b.len() as u32);
    }
    sink.bytes(b);
    Ok(())
}

fn read_key(cur: &mut Cursor<'_>, opts: &ReadOptions) -> PsdResult<String> {
    let len = cur.u32()?;
    let n = if len == 0 { 4 } else { len as usize };
    check_limit(
        "descriptor key length",
        n as u64,
        opts.max_name_units as u64,
    )?;
    let bytes = cur.take(n)?;
    Ok(bytes.iter().map(|&b| b as char).collect())
}

fn read_descriptor(
    cur: &mut Cursor<'_>,
    opts: &ReadOptions,
    depth: usize,
) -> PsdResult<Descriptor> {
    if depth > opts.max_descriptor_depth {
        return Err(PsdError::DescriptorTooDeep {
            max: opts.max_descriptor_depth,
        });
    }
    let name = cur.unicode_string(opts.max_name_units)?;
    let class_id = read_key(cur, opts)?;
    let count = cur.u32()? as usize;
    check_limit(
        "descriptor item count",
        count as u64,
        opts.max_descriptor_items as u64,
    )?;
    // Each item is at least 8 bytes (a four-cc key plus an ostype), so a count
    // that cannot fit in what is left is a lie worth refusing before reserving.
    if count.saturating_mul(8) > cur.remaining() {
        return Err(PsdError::Truncated {
            needed: count * 8,
            available: cur.remaining(),
            at: cur.offset(),
        });
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let key = read_key(cur, opts)?;
        let value = read_value(cur, opts, depth + 1)?;
        items.push((key, value));
    }
    Ok(Descriptor {
        name,
        class_id,
        items,
    })
}

fn read_value(cur: &mut Cursor<'_>, opts: &ReadOptions, depth: usize) -> PsdResult<Value> {
    if depth > opts.max_descriptor_depth {
        return Err(PsdError::DescriptorTooDeep {
            max: opts.max_descriptor_depth,
        });
    }
    let ostype = cur.tag()?;
    match &ostype {
        b"Objc" | b"GlbO" => Ok(Value::Descriptor(read_descriptor(cur, opts, depth)?)),
        b"VlLs" => {
            let count = cur.u32()? as usize;
            check_limit(
                "list length",
                count as u64,
                opts.max_descriptor_items as u64,
            )?;
            if count.saturating_mul(4) > cur.remaining() {
                return Err(PsdError::Truncated {
                    needed: count * 4,
                    available: cur.remaining(),
                    at: cur.offset(),
                });
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(read_value(cur, opts, depth + 1)?);
            }
            Ok(Value::List(items))
        }
        b"doub" => Ok(Value::Double(cur.f64()?)),
        b"UntF" => {
            let unit = cur.tag()?;
            Ok(Value::UnitFloat {
                unit,
                value: cur.f64()?,
            })
        }
        // A text run is the one string that legitimately runs to hundreds of
        // thousands of characters, so it is bounded by the enclosing block's
        // size rather than by the name limit. The `take` inside
        // `unicode_string` happens before any `Vec` is built, so the section
        // bound is what actually caps the allocation either way.
        b"TEXT" => Ok(Value::Text(
            cur.unicode_string(opts.max_tagged_block_bytes / 2 + 1)?,
        )),
        b"enum" => Ok(Value::Enumerated {
            type_id: read_key(cur, opts)?,
            value: read_key(cur, opts)?,
        }),
        b"long" => Ok(Value::Integer(cur.i32()?)),
        b"comp" => {
            let hi = u64::from(cur.u32()?);
            let lo = u64::from(cur.u32()?);
            Ok(Value::LargeInteger(((hi << 32) | lo) as i64))
        }
        b"bool" => Ok(Value::Bool(cur.u8()? != 0)),
        b"type" | b"GlbC" => Ok(Value::Class {
            name: cur.unicode_string(opts.max_name_units)?,
            class_id: read_key(cur, opts)?,
        }),
        b"alis" | b"tdta" => {
            let len = cur.u32()? as usize;
            // No pre-check needed beyond the cursor's: `take` borrows, so a
            // huge length fails on bounds rather than on allocation.
            let bytes = cur.take(len)?.to_vec();
            if &ostype == b"alis" {
                Ok(Value::Alias(bytes))
            } else {
                Ok(Value::RawData(bytes))
            }
        }
        b"obj " => {
            let count = cur.u32()? as usize;
            check_limit(
                "reference length",
                count as u64,
                opts.max_descriptor_items as u64,
            )?;
            if count.saturating_mul(4) > cur.remaining() {
                return Err(PsdError::Truncated {
                    needed: count * 4,
                    available: cur.remaining(),
                    at: cur.offset(),
                });
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(read_ref_item(cur, opts)?);
            }
            Ok(Value::Reference(items))
        }
        other => Err(PsdError::UnknownDescriptorType(tag_name(*other))),
    }
}

fn read_ref_item(cur: &mut Cursor<'_>, opts: &ReadOptions) -> PsdResult<RefItem> {
    let kind = cur.tag()?;
    match &kind {
        b"prop" => Ok(RefItem::Property {
            name: cur.unicode_string(opts.max_name_units)?,
            class_id: read_key(cur, opts)?,
            key_id: read_key(cur, opts)?,
        }),
        b"Clss" => Ok(RefItem::Class {
            name: cur.unicode_string(opts.max_name_units)?,
            class_id: read_key(cur, opts)?,
        }),
        b"Enmr" => Ok(RefItem::Enumerated {
            name: cur.unicode_string(opts.max_name_units)?,
            class_id: read_key(cur, opts)?,
            type_id: read_key(cur, opts)?,
            value: read_key(cur, opts)?,
        }),
        b"rele" => Ok(RefItem::Offset {
            name: cur.unicode_string(opts.max_name_units)?,
            class_id: read_key(cur, opts)?,
            value: cur.u32()?,
        }),
        b"Idnt" => Ok(RefItem::Identifier(cur.u32()?)),
        b"indx" => Ok(RefItem::Index(cur.u32()?)),
        b"name" => Ok(RefItem::Name {
            name: cur.unicode_string(opts.max_name_units)?,
            class_id: read_key(cur, opts)?,
            value: cur.unicode_string(opts.max_name_units)?,
        }),
        other => Err(PsdError::UnknownDescriptorType(tag_name(*other))),
    }
}

fn write_ref_item(sink: &mut Sink, item: &RefItem) -> PsdResult<()> {
    match item {
        RefItem::Property {
            name,
            class_id,
            key_id,
        } => {
            sink.tag(b"prop");
            sink.unicode_string(name);
            write_key(sink, class_id)?;
            write_key(sink, key_id)?;
        }
        RefItem::Class { name, class_id } => {
            sink.tag(b"Clss");
            sink.unicode_string(name);
            write_key(sink, class_id)?;
        }
        RefItem::Enumerated {
            name,
            class_id,
            type_id,
            value,
        } => {
            sink.tag(b"Enmr");
            sink.unicode_string(name);
            write_key(sink, class_id)?;
            write_key(sink, type_id)?;
            write_key(sink, value)?;
        }
        RefItem::Offset {
            name,
            class_id,
            value,
        } => {
            sink.tag(b"rele");
            sink.unicode_string(name);
            write_key(sink, class_id)?;
            sink.u32(*value);
        }
        RefItem::Identifier(v) => {
            sink.tag(b"Idnt");
            sink.u32(*v);
        }
        RefItem::Index(v) => {
            sink.tag(b"indx");
            sink.u32(*v);
        }
        RefItem::Name {
            name,
            class_id,
            value,
        } => {
            sink.tag(b"name");
            sink.unicode_string(name);
            write_key(sink, class_id)?;
            sink.unicode_string(value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Descriptor {
        let mut inner = Descriptor::new("Clr ");
        inner.push("Rd  ", Value::Double(255.0)).unwrap();
        inner.push("Grn ", Value::Double(12.5)).unwrap();
        inner.push("Bl  ", Value::Double(0.0)).unwrap();

        let mut d = Descriptor::new("TxLr");
        d.name = "a name".into();
        d.push("Txt ", Value::from("Hello, ✎ world")).unwrap();
        d.push("TextIndex", Value::Integer(-3)).unwrap();
        d.push("big", Value::LargeInteger(-1)).unwrap();
        d.push("on", Value::Bool(true)).unwrap();
        d.push("off", Value::Bool(false)).unwrap();
        d.push("Clr ", Value::Descriptor(inner)).unwrap();
        d.push(
            "Sz  ",
            Value::UnitFloat {
                unit: *b"#Pxl",
                value: 36.0,
            },
        )
        .unwrap();
        d.push(
            "Ornt",
            Value::Enumerated {
                type_id: "Ornt".into(),
                value: "Hrzn".into(),
            },
        )
        .unwrap();
        d.push(
            "list",
            Value::List(vec![
                Value::Integer(1),
                Value::Text("two".into()),
                Value::Double(3.0),
            ]),
        )
        .unwrap();
        d.push("EngineData", Value::RawData(vec![1, 2, 3, 4, 5]))
            .unwrap();
        d.push(
            "cls",
            Value::Class {
                name: String::new(),
                class_id: "Lyr ".into(),
            },
        )
        .unwrap();
        d.push(
            "ref",
            Value::Reference(vec![
                RefItem::Identifier(9),
                RefItem::Index(2),
                RefItem::Property {
                    name: String::new(),
                    class_id: "Lyr ".into(),
                    key_id: "Bckg".into(),
                },
                RefItem::Enumerated {
                    name: String::new(),
                    class_id: "Lyr ".into(),
                    type_id: "Ordn".into(),
                    value: "Trgt".into(),
                },
                RefItem::Name {
                    name: String::new(),
                    class_id: "Lyr ".into(),
                    value: "the layer".into(),
                },
                RefItem::Offset {
                    name: String::new(),
                    class_id: "Lyr ".into(),
                    value: 4,
                },
                RefItem::Class {
                    name: String::new(),
                    class_id: "Lyr ".into(),
                },
            ]),
        )
        .unwrap();
        d
    }

    #[test]
    fn every_value_type_round_trips_through_bytes() {
        let d = sample();
        let mut sink = Sink::new();
        d.write(&mut sink).unwrap();
        let buf = sink.into_inner();
        let mut cur = Cursor::new(&buf);
        let back = Descriptor::read(&mut cur, &ReadOptions::default()).unwrap();
        assert_eq!(back, d);
        assert!(cur.is_empty(), "{} bytes left over", cur.remaining());
    }

    #[test]
    fn writing_the_parsed_form_reproduces_the_same_bytes() {
        let d = sample();
        let mut a = Sink::new();
        d.write(&mut a).unwrap();
        let first = a.into_inner();
        let back = Descriptor::read(&mut Cursor::new(&first), &ReadOptions::default()).unwrap();
        let mut b = Sink::new();
        back.write(&mut b).unwrap();
        assert_eq!(b.into_inner(), first);
    }

    #[test]
    fn accessors_find_values_by_key() {
        let d = sample();
        assert_eq!(d.text("Txt "), Some("Hello, ✎ world"));
        assert_eq!(d.number("Sz  "), Some(36.0));
        assert_eq!(d.number("TextIndex"), Some(-3.0));
        assert_eq!(d.descriptor("Clr ").unwrap().number("Grn "), Some(12.5));
        assert_eq!(d.get("nope"), None);
        assert_eq!(d.text("Sz  "), None, "the wrong type must not coerce");
    }

    #[test]
    fn a_four_character_key_uses_the_zero_length_form() {
        let mut sink = Sink::new();
        write_key(&mut sink, "Txt ").unwrap();
        assert_eq!(sink.as_slice(), &[0, 0, 0, 0, b'T', b'x', b't', b' ']);

        let mut sink = Sink::new();
        write_key(&mut sink, "TextIndex").unwrap();
        assert_eq!(&sink.as_slice()[..4], &[0, 0, 0, 9]);
    }

    #[test]
    fn keys_of_every_length_round_trip_and_the_empty_key_is_refused() {
        // One, four and nine characters exercise the short form, the
        // four-character-code form, and the long form. Each is written into a
        // descriptor with a value *after* it, so a key that consumed the wrong
        // number of bytes would take the following ostype with it and the read
        // would fail rather than merely disagree.
        for key in ["k", "Txt ", "TextIndex"] {
            let mut d = Descriptor::new("root");
            d.push(key, Value::Integer(7)).unwrap();
            d.push("tail", Value::Bool(true)).unwrap();
            let mut sink = Sink::new();
            d.write(&mut sink).unwrap();
            let buf = sink.into_inner();
            let mut cur = Cursor::new(&buf);
            let back = Descriptor::read(&mut cur, &ReadOptions::default()).unwrap();
            assert_eq!(back, d, "{key:?}");
            assert!(cur.is_empty(), "{key:?} left {} bytes", cur.remaining());
        }

        // The empty key has no encoding at all, so it must be refused rather
        // than written as the four-character-code form.
        let mut d = Descriptor::new("root");
        let err = d.push("", Value::Integer(7)).unwrap_err();
        assert!(matches!(err, PsdError::InvalidDocument(_)), "{err}");
        assert!(d.items.is_empty(), "the refused item must not be stored");

        // `items` is public, so pushing past `push` still cannot produce a
        // stream this crate (or Photoshop) would read back wrongly.
        d.items.push((String::new(), Value::Integer(7)));
        let mut sink = Sink::new();
        let err = d.write(&mut sink).unwrap_err();
        assert!(matches!(err, PsdError::InvalidDocument(_)), "{err}");

        // Same for an empty class id.
        let empty_class = Descriptor::new("");
        let mut sink = Sink::new();
        assert!(matches!(
            empty_class.write(&mut sink).unwrap_err(),
            PsdError::InvalidDocument(_)
        ));
    }

    #[test]
    fn a_zero_length_key_reads_exactly_four_bytes() {
        let buf = [0u8, 0, 0, 0, b'a', b'b', b'c', b'd', 0xFF];
        let mut cur = Cursor::new(&buf);
        assert_eq!(read_key(&mut cur, &ReadOptions::default()).unwrap(), "abcd");
        assert_eq!(cur.remaining(), 1);
    }

    #[test]
    fn nesting_deeper_than_the_limit_is_an_error_and_not_a_stack_overflow() {
        // Build a descriptor nested far deeper than the default limit. If the
        // parser recursed without a limit this test would abort the process.
        let mut d = Descriptor::new("root");
        for _ in 0..400 {
            let mut outer = Descriptor::new("wrap");
            outer.push("in  ", Value::Descriptor(d)).unwrap();
            d = outer;
        }
        let mut sink = Sink::new();
        d.write(&mut sink).unwrap();
        let buf = sink.into_inner();
        let err = Descriptor::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap_err();
        assert!(
            matches!(err, PsdError::DescriptorTooDeep { max: 32 }),
            "{err}"
        );
    }

    #[test]
    fn an_absurd_item_count_is_refused_before_the_vec_is_reserved() {
        let mut sink = Sink::new();
        sink.unicode_string("");
        write_key(&mut sink, "root").unwrap();
        sink.u32(u32::MAX); // four billion items in a twenty byte file
        let buf = sink.into_inner();
        let err = Descriptor::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap_err();
        assert!(matches!(err, PsdError::LimitExceeded { .. }), "{err}");
    }

    #[test]
    fn an_item_count_that_cannot_fit_the_remaining_bytes_is_refused() {
        let mut sink = Sink::new();
        sink.unicode_string("");
        write_key(&mut sink, "root").unwrap();
        sink.u32(4000); // under the limit, but the file has nothing left
        let buf = sink.into_inner();
        let err = Descriptor::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap_err();
        assert!(matches!(err, PsdError::Truncated { .. }), "{err}");
    }

    #[test]
    fn an_unknown_value_type_is_named_rather_than_skipped() {
        let mut sink = Sink::new();
        sink.unicode_string("");
        write_key(&mut sink, "root").unwrap();
        sink.u32(1);
        write_key(&mut sink, "key ").unwrap();
        sink.tag(b"zzzz");
        sink.zeros(64);
        let buf = sink.into_inner();
        let err = Descriptor::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap_err();
        match err {
            PsdError::UnknownDescriptorType(t) => assert_eq!(t, "zzzz"),
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn glbo_and_glbc_are_accepted_as_their_plain_equivalents() {
        let mut sink = Sink::new();
        sink.unicode_string("");
        write_key(&mut sink, "root").unwrap();
        sink.u32(1);
        write_key(&mut sink, "k   ").unwrap();
        sink.tag(b"GlbO");
        Descriptor::new("Clr ").write(&mut sink).unwrap();
        let buf = sink.into_inner();
        let d = Descriptor::read(&mut Cursor::new(&buf), &ReadOptions::default()).unwrap();
        assert_eq!(
            d.get("k   "),
            Some(&Value::Descriptor(Descriptor::new("Clr ")))
        );
    }

    #[test]
    fn truncating_a_descriptor_anywhere_is_an_error_and_never_a_panic() {
        let mut sink = Sink::new();
        sample().write(&mut sink).unwrap();
        let buf = sink.into_inner();
        for cut in 0..buf.len() {
            let res = Descriptor::read(&mut Cursor::new(&buf[..cut]), &ReadOptions::default());
            assert!(res.is_err(), "a {cut}-byte prefix parsed as a whole tree");
        }
    }

    #[test]
    fn a_str_converts_into_a_text_value() {
        assert_eq!(Value::from("x"), Value::Text("x".to_string()));
    }
}
