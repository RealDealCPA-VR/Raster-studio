//! Bounds-checked big-endian reading and writing.
//!
//! Every multi-byte field in a `.psd` is big-endian, and every one of them is
//! read through [`Cursor`], which cannot be advanced past the end of its slice.
//! That is the single reason the parser cannot panic on a truncated file: there
//! is no indexing anywhere else in the crate.
//!
//! [`Cursor::sub`] is the other half of the safety story. A section declares
//! its own length, so the parser carves a sub-cursor of exactly that many bytes
//! and parses the section through it. A section that lies about its contents
//! can then only corrupt itself — it cannot read a neighbouring section, and it
//! cannot run off the end of the file.

use crate::error::{tag_name, PsdError, PsdResult};

/// Decode the bytes of a Pascal string.
///
/// [`Sink::pascal_string`] writes UTF-8, so UTF-8 is tried first and a string
/// this crate wrote comes back byte for byte. Photoshop writes MacRoman, which
/// is rarely valid UTF-8 above ASCII, so anything that is not UTF-8 falls back
/// to Latin-1 — never fails, and keeps a legacy name readable instead of
/// turning it into replacement characters.
///
/// The two directions **must** agree. They did not before: the writer emitted
/// UTF-8 and the reader decoded Latin-1, so an image resource named `café` came
/// back as `cafÃ©`, and because the mojibake was re-encoded as UTF-8 on the next
/// save the damage compounded with every open/save cycle. Layer names hid this
/// because they are overridden by the `luni` block; resource names have no such
/// second copy.
pub fn decode_pascal(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// A read cursor over a byte slice that reports absolute file offsets.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    /// Absolute offset of `data[0]` within the whole file.
    base: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Cursor {
            data,
            pos: 0,
            base: 0,
        }
    }

    /// Absolute offset of the next unread byte.
    pub fn offset(&self) -> usize {
        self.base + self.pos
    }

    /// Offset of the next unread byte relative to the start of this cursor.
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Bytes not yet read, without consuming them.
    pub fn peek_rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    fn need(&self, n: usize) -> PsdResult<()> {
        if n > self.remaining() {
            return Err(PsdError::Truncated {
                needed: n,
                available: self.remaining(),
                at: self.offset(),
            });
        }
        Ok(())
    }

    /// Borrow the next `n` bytes.
    ///
    /// This never allocates, so an absurd `n` is refused by the bounds check
    /// rather than by the allocator.
    pub fn take(&mut self, n: usize) -> PsdResult<&'a [u8]> {
        self.need(n)?;
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Carve off a bounded view of the next `n` bytes and advance past them.
    pub fn sub(&mut self, n: usize) -> PsdResult<Cursor<'a>> {
        let base = self.offset();
        let slice = self.take(n)?;
        Ok(Cursor {
            data: slice,
            pos: 0,
            base,
        })
    }

    pub fn skip(&mut self, n: usize) -> PsdResult<()> {
        self.take(n).map(|_| ())
    }

    /// Skip whatever is left in this (sub-)cursor.
    pub fn skip_rest(&mut self) {
        self.pos = self.data.len();
    }

    pub fn u8(&mut self) -> PsdResult<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn i8(&mut self) -> PsdResult<i8> {
        Ok(self.u8()? as i8)
    }

    pub fn u16(&mut self) -> PsdResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn i16(&mut self) -> PsdResult<i16> {
        Ok(self.u16()? as i16)
    }

    pub fn u32(&mut self) -> PsdResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> PsdResult<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn f64(&mut self) -> PsdResult<f64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(f64::from_be_bytes(a))
    }

    pub fn tag(&mut self) -> PsdResult<[u8; 4]> {
        let b = self.take(4)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    /// Peek a four-byte tag without consuming it. `None` when fewer than four
    /// bytes remain.
    pub fn peek_tag(&self) -> Option<[u8; 4]> {
        let r = self.peek_rest();
        if r.len() < 4 {
            None
        } else {
            Some([r[0], r[1], r[2], r[3]])
        }
    }

    /// Peek a four-byte tag `ahead` bytes past the cursor.
    pub fn peek_tag_at(&self, ahead: usize) -> Option<[u8; 4]> {
        let r = self.peek_rest();
        let end = ahead.checked_add(4)?;
        if r.len() < end {
            None
        } else {
            Some([r[ahead], r[ahead + 1], r[ahead + 2], r[ahead + 3]])
        }
    }

    pub fn expect_tag(&mut self, want: &'static [u8; 4], what: &'static str) -> PsdResult<()> {
        let at = self.offset();
        let got = self.tag()?;
        if &got != want {
            return Err(PsdError::BadSignature {
                expected: what,
                found: tag_name(got),
                at,
            });
        }
        Ok(())
    }

    /// Advance so that the number of bytes consumed *from this cursor* is a
    /// multiple of `align`. Padding past the end is clamped rather than
    /// treated as truncation, because trailing padding is routinely omitted.
    pub fn align_to(&mut self, align: usize) -> PsdResult<()> {
        debug_assert!(align.is_power_of_two());
        let over = self.pos % align;
        if over == 0 {
            return Ok(());
        }
        let pad = align - over;
        let pad = pad.min(self.remaining());
        self.skip(pad)
    }

    /// A Pascal string: one length byte, then that many bytes, then padding so
    /// the whole field is a multiple of `align`.
    ///
    /// See [`decode_pascal`] for the encoding, which has to match
    /// [`Sink::pascal_string`] exactly — an image resource's name has no
    /// Unicode counterpart anywhere in the format, so a mismatch corrupts it a
    /// little more on every save.
    pub fn pascal_string(&mut self, align: usize) -> PsdResult<String> {
        let start = self.pos;
        let len = self.u8()? as usize;
        let bytes = self.take(len)?;
        let s = decode_pascal(bytes);
        let consumed = self.pos - start;
        let over = consumed % align;
        if over != 0 {
            let pad = (align - over).min(self.remaining());
            self.skip(pad)?;
        }
        Ok(s)
    }

    /// A Photoshop Unicode string: a `u32` count of UTF-16 code units followed
    /// by that many big-endian code units.
    ///
    /// `max_units` bounds the allocation before it happens. Unpaired surrogates
    /// decode lossily rather than failing, so a corrupt name cannot abort a
    /// parse.
    pub fn unicode_string(&mut self, max_units: usize) -> PsdResult<String> {
        let count = self.u32()? as usize;
        if count > max_units {
            return Err(PsdError::LimitExceeded {
                what: "unicode string length",
                value: count as u64,
                max: max_units as u64,
            });
        }
        let bytes = self.take(count.checked_mul(2).ok_or(PsdError::Overflow {
            what: "unicode string byte length",
        })?)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        let mut s = String::from_utf16_lossy(&units);
        // Photoshop terminates these with a NUL code unit; drop it.
        if s.ends_with('\0') {
            s.pop();
        }
        Ok(s)
    }
}

/// The offset of a length field that will be filled in once its section is
/// finished. Returned by [`Sink::begin_len`], consumed by [`Sink::end_len`].
#[derive(Debug, Clone, Copy)]
#[must_use = "a length slot that is never ended leaves a zero length in the file"]
pub struct LenSlot {
    at: usize,
}

/// A growable big-endian byte sink.
#[derive(Debug, Default, Clone)]
pub struct Sink {
    buf: Vec<u8>,
}

impl Sink {
    pub fn new() -> Self {
        Sink { buf: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i16(&mut self, v: i16) {
        self.u16(v as u16);
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }

    pub fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    pub fn tag(&mut self, v: &[u8; 4]) {
        self.buf.extend_from_slice(v);
    }

    pub fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }

    /// Pad with zeros until the length is a multiple of `align`.
    pub fn align_to(&mut self, align: usize) {
        debug_assert!(align.is_power_of_two());
        let over = self.buf.len() % align;
        if over != 0 {
            self.zeros(align - over);
        }
    }

    /// A Pascal string padded so the whole field is a multiple of `align`.
    ///
    /// The string is written as UTF-8 truncated at a character boundary to fit
    /// 255 bytes, and [`decode_pascal`] reads UTF-8 back, so the pair is the
    /// identity for any string that fits. For a *layer* name this field is
    /// legacy — readers that understand `luni` take the name from there, and
    /// this crate always writes `luni` too — but an image resource's name has
    /// no `luni`, so this is the only copy of it there is.
    pub fn pascal_string(&mut self, s: &str, align: usize) {
        let start = self.buf.len();
        let mut end = s.len().min(255);
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let bytes = &s.as_bytes()[..end];
        self.u8(bytes.len() as u8);
        self.bytes(bytes);
        let over = (self.buf.len() - start) % align;
        if over != 0 {
            self.zeros(align - over);
        }
    }

    /// A Photoshop Unicode string: `u32` code-unit count then UTF-16BE, with
    /// the trailing NUL Photoshop expects included in the count.
    pub fn unicode_string(&mut self, s: &str) {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.u32(units.len() as u32 + 1);
        for u in units {
            self.u16(u);
        }
        self.u16(0);
    }

    /// Reserve a `u32` length field to be filled in by [`Sink::end_len`].
    pub fn begin_len(&mut self) -> LenSlot {
        let at = self.buf.len();
        self.u32(0);
        LenSlot { at }
    }

    /// Bytes written since `slot` was opened, or `None` if the slot did not
    /// come from this sink.
    ///
    /// `LenSlot` is `Copy` and both it and this type are public, so a slot from
    /// a *different*, longer sink can reach us. Establishing the bound here
    /// rather than trusting [`Sink::begin_len`] keeps every public entry point
    /// on this type total — which matters because the release profile sets
    /// `panic = "abort"`, so a panic here would take the process down rather
    /// than surface as an error a caller could handle.
    fn span_of(&self, slot: LenSlot) -> Option<u32> {
        let body = self.buf.len().checked_sub(slot.at)?.checked_sub(4)?;
        u32::try_from(body).ok()
    }

    /// Fill in a slot with the number of bytes written since it was opened.
    ///
    /// A slot that did not come from this sink writes nothing.
    pub fn end_len(&mut self, slot: LenSlot) {
        let Some(len) = self.span_of(slot) else {
            return;
        };
        let Some(dst) = self.buf.get_mut(slot.at..slot.at + 4) else {
            return;
        };
        dst.copy_from_slice(&len.to_be_bytes());
    }

    /// Fill in a slot after padding the section to an even length, which is
    /// what the format asks for in most places that carry a length.
    pub fn end_len_even(&mut self, slot: LenSlot) {
        if self.span_of(slot).is_some_and(|len| len % 2 != 0) {
            self.u8(0);
        }
        self.end_len(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_past_the_end_reports_the_absolute_offset_and_does_not_panic() {
        let data = [0u8; 10];
        let mut c = Cursor::new(&data);
        c.skip(6).unwrap();
        let err = c.take(9).unwrap_err();
        match err {
            PsdError::Truncated {
                needed,
                available,
                at,
            } => {
                assert_eq!((needed, available, at), (9, 4, 6));
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn sub_cursor_cannot_read_past_its_own_section() {
        let data: Vec<u8> = (0u8..20).collect();
        let mut c = Cursor::new(&data);
        c.skip(4).unwrap();
        let mut section = c.sub(4).unwrap();
        assert_eq!(section.take(4).unwrap(), &[4, 5, 6, 7]);
        assert!(section.u8().is_err());
        // The outer cursor advanced past the section, not into it.
        assert_eq!(c.offset(), 8);
        // Absolute offsets survive the carve.
        assert_eq!(section.offset(), 8);
    }

    #[test]
    fn unicode_string_length_is_bounded_before_it_allocates() {
        // Declares 0xFFFF_FFFF code units in a four-byte file.
        let data = [0xFF, 0xFF, 0xFF, 0xFF];
        let mut c = Cursor::new(&data);
        let err = c.unicode_string(4096).unwrap_err();
        assert!(matches!(err, PsdError::LimitExceeded { .. }), "{err}");
    }

    #[test]
    fn unicode_round_trip_including_astral_characters() {
        let mut s = Sink::new();
        s.unicode_string("Grüße 🌊 layer");
        let buf = s.into_inner();
        let mut c = Cursor::new(&buf);
        assert_eq!(c.unicode_string(4096).unwrap(), "Grüße 🌊 layer");
    }

    #[test]
    fn lone_surrogate_decodes_lossily_instead_of_failing() {
        // count = 2, then a high surrogate with no low surrogate.
        let data = [0, 0, 0, 2, 0xD8, 0x00, 0x00, 0x41];
        let mut c = Cursor::new(&data);
        let s = c.unicode_string(4096).unwrap();
        assert!(s.ends_with('A'), "{s:?}");
    }

    #[test]
    fn pascal_string_pads_the_whole_field_to_the_alignment() {
        let mut s = Sink::new();
        s.pascal_string("abc", 4);
        assert_eq!(s.len(), 4);
        s.pascal_string("abcd", 4);
        assert_eq!(s.len(), 12);

        let buf = s.into_inner();
        let mut c = Cursor::new(&buf);
        assert_eq!(c.pascal_string(4).unwrap(), "abc");
        assert_eq!(c.pascal_string(4).unwrap(), "abcd");
        assert!(c.is_empty());
    }

    #[test]
    fn a_pascal_string_round_trips_unchanged_when_it_is_not_ascii() {
        // The writer emits UTF-8; the reader must read UTF-8 back. Decoding as
        // Latin-1 turns "café" into "cafÃ©" — and re-encoding *that* as UTF-8
        // on the next save compounds the damage, which is why this has to be an
        // exact identity rather than "close enough".
        for name in [
            "café",
            "日本語",
            "Ελληνικά",
            "plain ascii",
            "",
            "Grüße — ok",
        ] {
            let mut s = Sink::new();
            s.pascal_string(name, 2);
            let buf = s.into_inner();
            assert_eq!(buf.len() % 2, 0, "{name:?} left the field unpadded");
            let mut c = Cursor::new(&buf);
            assert_eq!(c.pascal_string(2).unwrap(), name, "{name:?}");
            assert!(c.is_empty(), "{name:?} left {} bytes", c.remaining());
        }
    }

    #[test]
    fn a_macroman_name_that_is_not_utf8_still_decodes_readably() {
        // 0x8E is "é" in MacRoman and an invalid UTF-8 lead byte, so this is
        // the fallback path: Latin-1, which never fails.
        let buf = [4u8, b'c', b'a', b'f', 0x8E];
        let mut c = Cursor::new(&buf);
        let s = c.pascal_string(1).unwrap();
        assert_eq!(s.chars().count(), 4, "{s:?}");
        assert!(s.starts_with("caf"), "{s:?}");
        assert!(!s.contains('\u{FFFD}'), "no replacement characters: {s:?}");
    }

    #[test]
    fn a_pascal_string_is_truncated_on_a_character_boundary() {
        // 90 three-byte characters is 270 bytes, past the 255 the length byte
        // can describe; the cut must not land inside a character. 255 is a
        // multiple of three, so this one lands exactly on a boundary.
        let long: String = std::iter::repeat_n('日', 90).collect();
        let mut s = Sink::new();
        s.pascal_string(&long, 1);
        let buf = s.into_inner();
        assert_eq!(buf[0], 255, "85 characters of three bytes each");
        let mut c = Cursor::new(&buf);
        let back = c.pascal_string(1).unwrap();
        assert_eq!(back.chars().count(), 85);
        assert!(long.starts_with(&back));

        // A two-byte character makes the 255-byte cut fall mid-character, so
        // the writer has to back up: 128 × 2 = 256, one over.
        let two: String = std::iter::repeat_n('é', 130).collect();
        let mut s = Sink::new();
        s.pascal_string(&two, 1);
        let buf = s.into_inner();
        assert_eq!(buf[0], 254, "127 characters of two bytes each");
        let mut c = Cursor::new(&buf);
        let back = c.pascal_string(1).unwrap();
        assert_eq!(back.chars().count(), 127);
        assert!(two.starts_with(&back));
    }

    #[test]
    fn length_slots_are_backfilled() {
        let mut s = Sink::new();
        let slot = s.begin_len();
        s.bytes(&[1, 2, 3]);
        s.end_len_even(slot);
        let buf = s.into_inner();
        assert_eq!(&buf[..4], &[0, 0, 0, 4]);
        assert_eq!(buf.len(), 8);
    }

    #[test]
    fn a_length_slot_from_another_sink_is_ignored_rather_than_fatal() {
        // `LenSlot` is `Copy` and every type here is public, so a slot can
        // reach a sink it did not come from. The release profile aborts on
        // panic, so this has to be an ignored write, not a crash.
        let mut long = Sink::new();
        long.zeros(100);
        let foreign = long.begin_len();

        let mut short = Sink::new();
        short.u16(0xABCD);
        short.end_len(foreign);
        short.end_len_even(foreign);

        // Nothing was written, and the sink is otherwise untouched.
        assert_eq!(short.into_inner(), vec![0xAB, 0xCD]);
    }

    #[test]
    fn a_slot_from_this_sink_still_measures_its_own_span() {
        let mut s = Sink::new();
        let slot = s.begin_len();
        s.zeros(7);
        s.end_len(slot);
        let buf = s.into_inner();
        assert_eq!(&buf[..4], &[0, 0, 0, 7]);
    }
}
