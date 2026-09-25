//! W13-C: lossless JPEG (ITU T.81 process 14, `SOF3`), the compression
//! (TIFF `Compression = 7`) nearly every DNG writer uses for its raw data.
//!
//! Only what a DNG tile or strip holds is read: one lossless frame, one
//! interleaved scan over every component, sampling factors of one, predictors
//! 1-7, a point transform, Huffman tables and restart intervals. Everything
//! is bounds-checked: a damaged stream is an error, never a panic, and the
//! sample buffer is sized from the frame header only after the caller's cap
//! has accepted it.

use crate::codec::CodecError;

use super::malformed;

const NAME: &str = "lossless JPEG";

/// A canonical Huffman table (T.81 Annex C), with a 9-bit fast path.
struct Huffman {
    maxcode: [i32; 17],
    mincode: [i32; 17],
    valptr: [i32; 17],
    values: Vec<u8>,
    /// `(length << 8) | symbol` for every 9-bit prefix that completes a code
    /// of at most nine bits; zero otherwise.
    fast: Vec<u16>,
}

const FAST_BITS: u32 = 9;

impl Huffman {
    fn build(counts: &[u8; 16], values: Vec<u8>) -> Result<Self, CodecError> {
        let mut maxcode = [-1i32; 17];
        let mut mincode = [0i32; 17];
        let mut valptr = [0i32; 17];
        let mut fast = vec![0u16; 1 << FAST_BITS];
        let mut code: i32 = 0;
        let mut k = 0usize;
        for len in 1..=16usize {
            let n = usize::from(counts[len - 1]);
            if n > 0 {
                valptr[len] = k as i32;
                mincode[len] = code;
                for _ in 0..n {
                    if code >= (1i32 << len) {
                        return Err(malformed(NAME, "a Huffman table has too many codes"));
                    }
                    let symbol = *values
                        .get(k)
                        .ok_or_else(|| malformed(NAME, "a Huffman table is short"))?;
                    if len as u32 <= FAST_BITS {
                        let shift = FAST_BITS - len as u32;
                        let first = (code as usize) << shift;
                        fast[first..first + (1usize << shift)]
                            .fill(((len as u16) << 8) | u16::from(symbol));
                    }
                    code += 1;
                    k += 1;
                }
                maxcode[len] = code - 1;
            }
            code <<= 1;
        }
        Ok(Huffman {
            maxcode,
            mincode,
            valptr,
            values,
            fast,
        })
    }

    fn decode(&self, bits: &mut Bits) -> Result<u8, CodecError> {
        let entry = self.fast[bits.peek(FAST_BITS) as usize];
        if entry != 0 {
            bits.skip(u32::from(entry >> 8));
            return Ok(entry as u8);
        }
        let mut code: i32 = 0;
        for len in 1..=16usize {
            code = (code << 1) | bits.take(1) as i32;
            if code <= self.maxcode[len] {
                let at = self.valptr[len] + code - self.mincode[len];
                return usize::try_from(at)
                    .ok()
                    .and_then(|at| self.values.get(at).copied())
                    .ok_or_else(|| malformed(NAME, "a Huffman code has no symbol"));
            }
        }
        Err(malformed(NAME, "an invalid Huffman code"))
    }
}

/// An MSB-first reader over entropy-coded bytes: `FF 00` is a data `FF`, any
/// other `FF xx` is a marker, where the reader stops and feeds zero bits.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    count: u32,
    at_marker: bool,
    /// Zero bytes fed past a marker or the end of the data.
    padding: u64,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Bits {
            data,
            pos,
            acc: 0,
            count: 0,
            at_marker: false,
            padding: 0,
        }
    }

    fn fill(&mut self) {
        while self.count <= 56 {
            let byte = if self.at_marker {
                None
            } else {
                match self.data.get(self.pos) {
                    Some(0xFF) => match self.data.get(self.pos + 1) {
                        Some(0x00) => {
                            self.pos += 2;
                            Some(0xFF)
                        }
                        _ => None,
                    },
                    Some(&b) => {
                        self.pos += 1;
                        Some(b)
                    }
                    None => None,
                }
            };
            let byte = byte.unwrap_or_else(|| {
                self.at_marker = true;
                self.padding += 1;
                0
            });
            self.acc |= u64::from(byte) << (56 - self.count);
            self.count += 8;
        }
    }

    fn peek(&mut self, n: u32) -> u32 {
        self.fill();
        (self.acc >> (64 - n)) as u32
    }

    fn skip(&mut self, n: u32) {
        self.acc <<= n;
        self.count -= n;
    }

    fn take(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.skip(n);
        v
    }

    /// Whether any bit consumed so far was padding rather than data.
    fn overran(&self) -> bool {
        self.padding.saturating_mul(8) > u64::from(self.count)
    }

    /// Drop the rest of the interval's bits and step over an `RSTn` marker.
    fn restart(&mut self) -> Result<(), CodecError> {
        if self.overran() {
            return Err(malformed(NAME, "the entropy-coded data is truncated"));
        }
        self.acc = 0;
        self.count = 0;
        self.padding = 0;
        self.at_marker = false;
        while self.data.get(self.pos) == Some(&0xFF) && self.data.get(self.pos + 1) == Some(&0xFF) {
            self.pos += 1;
        }
        match (self.data.get(self.pos), self.data.get(self.pos + 1)) {
            (Some(0xFF), Some(0xD0..=0xD7)) => {
                self.pos += 2;
                Ok(())
            }
            _ => Err(malformed(NAME, "a restart marker is missing")),
        }
    }
}

fn be16(d: &[u8], at: usize) -> Result<usize, CodecError> {
    match d.get(at..at + 2) {
        Some(b) => Ok(usize::from(u16::from_be_bytes([b[0], b[1]]))),
        None => Err(malformed(NAME, "a marker segment is truncated")),
    }
}

struct FrameHeader {
    precision: u32,
    height: usize,
    width: usize,
    ids: Vec<u8>,
}

/// Decode one lossless-JPEG frame into its samples, row by row with the
/// components interleaved, refusing one whose header declares more than
/// `max_samples` samples before any sample buffer exists.
pub(crate) fn decode(data: &[u8], max_samples: usize) -> Result<Vec<u16>, CodecError> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err(malformed(NAME, "no start-of-image marker"));
    }
    let mut pos = 2usize;
    let mut tables: [Option<Huffman>; 4] = [None, None, None, None];
    let mut frame: Option<FrameHeader> = None;
    let mut restart_interval = 0usize;
    loop {
        if data.get(pos) != Some(&0xFF) {
            return Err(malformed(NAME, "expected a marker"));
        }
        while data.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let marker = *data
            .get(pos)
            .ok_or_else(|| malformed(NAME, "the stream ends before its scan"))?;
        pos += 1;
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 => return Err(malformed(NAME, "the stream has no scan")),
            _ => {}
        }
        let len = be16(data, pos)?;
        let segment = data
            .get(pos + 2..pos + len.max(2))
            .ok_or_else(|| malformed(NAME, "a marker segment is truncated"))?;
        match marker {
            0xC4 => {
                let mut s = segment;
                while !s.is_empty() {
                    let class_id = s[0];
                    let id = usize::from(class_id & 0x0F);
                    if class_id >> 4 != 0 || id > 3 {
                        return Err(malformed(NAME, "a Huffman table id is out of range"));
                    }
                    let counts: [u8; 16] = s
                        .get(1..17)
                        .and_then(|c| c.try_into().ok())
                        .ok_or_else(|| malformed(NAME, "a Huffman table is truncated"))?;
                    let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
                    let values = s
                        .get(17..17 + total)
                        .ok_or_else(|| malformed(NAME, "a Huffman table is truncated"))?
                        .to_vec();
                    tables[id] = Some(Huffman::build(&counts, values)?);
                    s = &s[17 + total..];
                }
            }
            0xC3 => {
                if segment.len() < 6 {
                    return Err(malformed(NAME, "the frame header is truncated"));
                }
                let precision = u32::from(segment[0]);
                let height = usize::from(u16::from_be_bytes([segment[1], segment[2]]));
                let width = usize::from(u16::from_be_bytes([segment[3], segment[4]]));
                let n = usize::from(segment[5]);
                if !(2..=16).contains(&precision) || !(1..=4).contains(&n) {
                    return Err(malformed(NAME, "unsupported precision or component count"));
                }
                if width == 0 || height == 0 {
                    return Err(malformed(NAME, "the frame has no pixels"));
                }
                let mut ids = Vec::with_capacity(n);
                for c in 0..n {
                    let spec = segment
                        .get(6 + c * 3..9 + c * 3)
                        .ok_or_else(|| malformed(NAME, "the frame header is truncated"))?;
                    if spec[1] != 0x11 {
                        return Err(malformed(NAME, "subsampled components are not supported"));
                    }
                    ids.push(spec[0]);
                }
                frame = Some(FrameHeader {
                    precision,
                    height,
                    width,
                    ids,
                });
            }
            0xC0..=0xCF => {
                return Err(malformed(
                    NAME,
                    format!("frame type SOF{} is not lossless", marker - 0xC0),
                ))
            }
            0xDD => restart_interval = be16(data, pos + 2)?,
            0xDA => {
                let header = frame
                    .as_ref()
                    .ok_or_else(|| malformed(NAME, "the scan comes before the frame header"))?;
                let start = pos + len;
                return decode_scan(
                    data,
                    start,
                    segment,
                    header,
                    &tables,
                    restart_interval,
                    max_samples,
                );
            }
            _ => {}
        }
        pos += len.max(2);
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(
    data: &[u8],
    start: usize,
    sos: &[u8],
    header: &FrameHeader,
    tables: &[Option<Huffman>; 4],
    restart_interval: usize,
    max_samples: usize,
) -> Result<Vec<u16>, CodecError> {
    let n = header.ids.len();
    let ns = usize::from(
        *sos.first()
            .ok_or_else(|| malformed(NAME, "empty scan header"))?,
    );
    if ns != n {
        return Err(malformed(NAME, "a scan must interleave every component"));
    }
    let mut comp_tables: Vec<&Huffman> = Vec::with_capacity(n);
    for c in 0..n {
        let spec = sos
            .get(1 + c * 2..3 + c * 2)
            .ok_or_else(|| malformed(NAME, "the scan header is truncated"))?;
        if spec[0] != header.ids[c] {
            return Err(malformed(NAME, "scan components are out of order"));
        }
        let table = tables
            .get(usize::from(spec[1] >> 4))
            .and_then(|t| t.as_ref())
            .ok_or_else(|| malformed(NAME, "the scan names a missing Huffman table"))?;
        comp_tables.push(table);
    }
    let tail = sos
        .get(1 + n * 2..4 + n * 2)
        .ok_or_else(|| malformed(NAME, "the scan header is truncated"))?;
    let predictor = tail[0];
    let point = u32::from(tail[2] & 0x0F);
    if !(1..=7).contains(&predictor) || point >= header.precision {
        return Err(malformed(NAME, "unsupported predictor or point transform"));
    }
    let (w, h) = (header.width, header.height);
    let total = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(n))
        .filter(|&t| t <= max_samples)
        .ok_or_else(|| malformed(NAME, "the frame is larger than its tile"))?;
    let mut out = vec![0u16; total];
    let initial = 1i32 << (header.precision - point - 1);
    let row = w * n;
    let mut bits = Bits::new(data, start);
    let mut mcu = 0usize;
    // Where the current restart interval began: its row uses first-line
    // prediction from its first sample on.
    let (mut reset_y, mut reset_x) = (0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if restart_interval > 0 && mcu > 0 && mcu.is_multiple_of(restart_interval) {
                bits.restart()?;
                (reset_y, reset_x) = (y, x);
            }
            mcu += 1;
            for (c, table) in comp_tables.iter().enumerate() {
                let i = y * row + x * n + c;
                let ra = || i32::from(out[i - n]);
                let rb = || i32::from(out[i - row]);
                let pred = if y == reset_y && x == reset_x {
                    initial
                } else if y == reset_y {
                    ra()
                } else if x == 0 {
                    rb()
                } else {
                    let rc = i32::from(out[i - row - n]);
                    match predictor {
                        1 => ra(),
                        2 => rb(),
                        3 => rc,
                        4 => ra() + rb() - rc,
                        5 => ra() + ((rb() - rc) >> 1),
                        6 => rb() + ((ra() - rc) >> 1),
                        _ => (ra() + rb()) >> 1,
                    }
                };
                let ssss = table.decode(&mut bits)?;
                let diff = match ssss {
                    0 => 0,
                    16 => 32768,
                    1..=15 => {
                        let t = u32::from(ssss);
                        let v = bits.take(t) as i32;
                        if v < (1 << (t - 1)) {
                            v - (1 << t) + 1
                        } else {
                            v
                        }
                    }
                    _ => return Err(malformed(NAME, "a difference category above 16")),
                };
                out[i] = (pred + diff) as u16;
            }
        }
    }
    if bits.overran() {
        return Err(malformed(NAME, "the entropy-coded data is truncated"));
    }
    if point > 0 {
        for v in &mut out {
            *v <<= point;
        }
    }
    Ok(out)
}
