//! W18-D: synthetic vendor RAW writers for tests, here and in `app-shell`.
//! Not exporters: each writes exactly the container its test describes,
//! from the same public descriptions the readers follow, so a decode can be
//! checked against the scene it was built from.
//!
//! The camera model: the scene is linear sRGB; the sensor's response to
//! channel `c` is `scene[c] * SENSITIVITY[c]`, coded as `black + (white -
//! black) * response`, clipped at `white`. Files that record an as-shot
//! balance record `1 / SENSITIVITY`; the reader's generic (identity) matrix
//! then returns the scene. Every scene handed to these writers by the tests
//! blows out a band (a clipped highlight), which is where a reader with no
//! white tag finds the saturation level.

/// The synthetic camera's response per channel (red, green, blue).
pub const SENSITIVITY: [f64; 3] = [0.5, 1.0, 0.7];

/// The standard X-Trans 6x6 layout (0 red, 1 green, 2 blue).
pub const XTRANS: [u8; 36] = [
    1, 1, 0, 1, 1, 2, //
    1, 1, 2, 1, 1, 0, //
    2, 0, 1, 0, 2, 1, //
    1, 1, 2, 1, 1, 0, //
    1, 1, 0, 1, 1, 2, //
    0, 2, 1, 2, 0, 1,
];

/// A TIFF value.
pub enum V {
    S(Vec<u16>),
    L(Vec<u32>),
    B(Vec<u8>),
    A(&'static str),
    /// Unsigned rationals `(numerator, denominator)`.
    R(Vec<(u32, u32)>),
    /// `UNDEFINED` bytes.
    U(Vec<u8>),
}

/// A little TIFF writer: IFDs appended with their values, file-relative
/// offsets.
pub struct T {
    pub le: bool,
    pub buf: Vec<u8>,
}

impl T {
    pub fn new(le: bool, header: &[u8]) -> Self {
        T {
            le,
            buf: header.to_vec(),
        }
    }

    pub fn u16(&self, v: u16) -> [u8; 2] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }

    pub fn u32(&self, v: u32) -> [u8; 4] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }

    fn align(&mut self) {
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
    }

    /// Append bytes; return where they start.
    pub fn blob(&mut self, bytes: &[u8]) -> u32 {
        self.align();
        let at = self.buf.len() as u32;
        self.buf.extend_from_slice(bytes);
        at
    }

    /// Write a u32 at `at`.
    pub fn put32(&mut self, at: usize, v: u32) {
        let b = self.u32(v);
        self.buf[at..at + 4].copy_from_slice(&b);
    }

    /// Append an IFD (sorted by tag) and its out-of-line values; return its
    /// offset. The next-IFD pointer is 0 ([`T::set_next`] changes it).
    pub fn ifd(&mut self, mut entries: Vec<(u16, V)>) -> u32 {
        entries.sort_by_key(|e| e.0);
        self.align();
        let at = self.buf.len();
        let n = entries.len();
        let mut extra_at = at + 2 + n * 12 + 4;
        let mut table = self.u16(n as u16).to_vec();
        let mut extra = Vec::new();
        for (tag, value) in &entries {
            let (kind, count, bytes): (u16, usize, Vec<u8>) = match value {
                V::S(s) => (3, s.len(), s.iter().flat_map(|&x| self.u16(x)).collect()),
                V::L(s) => (4, s.len(), s.iter().flat_map(|&x| self.u32(x)).collect()),
                V::B(s) => (1, s.len(), s.clone()),
                V::U(s) => (7, s.len(), s.clone()),
                V::A(s) => {
                    let mut b = s.as_bytes().to_vec();
                    b.push(0);
                    (2, b.len(), b)
                }
                V::R(s) => (
                    5,
                    s.len(),
                    s.iter()
                        .flat_map(|&(a, b)| {
                            let mut v = self.u32(a).to_vec();
                            v.extend(self.u32(b));
                            v
                        })
                        .collect(),
                ),
            };
            table.extend(self.u16(*tag));
            table.extend(self.u16(kind));
            table.extend(self.u32(count as u32));
            if bytes.len() <= 4 {
                let mut inline = bytes.clone();
                inline.resize(4, 0);
                table.extend(inline);
            } else {
                table.extend(self.u32(extra_at as u32));
                extra.extend(&bytes);
                extra_at += bytes.len();
                if extra_at % 2 == 1 {
                    extra.push(0);
                    extra_at += 1;
                }
            }
        }
        table.extend(self.u32(0));
        self.buf.extend(table);
        self.buf.extend(extra);
        at as u32
    }

    /// Point IFD `at`'s next-IFD link at `next`.
    pub fn set_next(&mut self, at: u32, next: u32) {
        let at = at as usize;
        let n = usize::from(if self.le {
            u16::from_le_bytes([self.buf[at], self.buf[at + 1]])
        } else {
            u16::from_be_bytes([self.buf[at], self.buf[at + 1]])
        });
        self.put32(at + 2 + n * 12, next);
    }
}

/// What every writer shares: the full raw size, the active area inside it
/// (`top, left, bottom, right`), black per 2x2 cell, white, orientation.
#[derive(Debug, Clone)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub active: [u32; 4],
    pub black: [u16; 4],
    pub white: u16,
    pub orientation: u16,
}

impl Shot {
    pub fn new(width: u32, height: u32, black: u16, white: u16) -> Self {
        Shot {
            width,
            height,
            active: [0, 0, height, width],
            black: [black; 4],
            white,
            orientation: 1,
        }
    }
}

/// The raw codes of `scene` (linear sRGB at active-area coordinates) under
/// the CFA `cfa(x, y)` (raw coordinates); outside the active area, black.
pub fn mosaic(
    shot: &Shot,
    cfa: impl Fn(u32, u32) -> u8,
    scene: impl Fn(u32, u32) -> [f64; 3],
) -> Vec<u16> {
    let [top, left, bottom, right] = shot.active;
    let mut out = Vec::with_capacity((shot.width * shot.height) as usize);
    for y in 0..shot.height {
        for x in 0..shot.width {
            let black = shot.black[((y % 2) * 2 + x % 2) as usize];
            if y < top || y >= bottom || x < left || x >= right {
                out.push(black);
                continue;
            }
            let c = usize::from(cfa(x, y));
            let response = scene(x - left, y - top)[c] * SENSITIVITY[c];
            let range = f64::from(shot.white - black);
            let v = f64::from(black) + range * response.clamp(0.0, 1.0);
            out.push(v.round().min(f64::from(shot.white)) as u16);
        }
    }
    out
}

/// Red-green / green-blue at the origin.
pub fn rggb(x: u32, y: u32) -> u8 {
    [0u8, 1, 1, 2][((y % 2) * 2 + x % 2) as usize]
}

// ------------------------------------------------------ lossless JPEG ----

struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                if self.acc == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }
}

/// A reference lossless-JPEG encoder (ITU T.81 process 14): one frame of
/// `width x height` pixels of `comps` interleaved components at
/// `precision` bits, one scan with `predictor` (1-7), no point transform,
/// one Huffman table in which difference category `k` (0-16) has the
/// five-bit code `k`.
pub fn ljpeg(
    samples: &[u16],
    width: u32,
    height: u32,
    comps: u32,
    precision: u32,
    predictor: u8,
) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    let mut counts = [0u8; 16];
    counts[4] = 17;
    let mut dht = vec![0x00];
    dht.extend(counts);
    dht.extend(0u8..=16);
    out.extend([0xFF, 0xC4]);
    out.extend(((dht.len() + 2) as u16).to_be_bytes());
    out.extend(dht);
    let mut sof = vec![precision as u8];
    sof.extend((height as u16).to_be_bytes());
    sof.extend((width as u16).to_be_bytes());
    sof.push(comps as u8);
    for c in 0..comps {
        sof.extend([c as u8 + 1, 0x11, 0]);
    }
    out.extend([0xFF, 0xC3]);
    out.extend(((sof.len() + 2) as u16).to_be_bytes());
    out.extend(sof);
    let mut sos = vec![comps as u8];
    for c in 0..comps {
        sos.extend([c as u8 + 1, 0x00]);
    }
    sos.extend([predictor, 0, 0]);
    out.extend([0xFF, 0xDA]);
    out.extend(((sos.len() + 2) as u16).to_be_bytes());
    out.extend(sos);
    let (w, h, n) = (width as usize, height as usize, comps as usize);
    let row = w * n;
    let mut bits = Bits {
        out: Vec::new(),
        acc: 0,
        n: 0,
    };
    for y in 0..h {
        for x in 0..w {
            for c in 0..n {
                let i = y * row + x * n + c;
                let s = |k: usize| i32::from(samples[k]);
                let pred = if y == 0 && x == 0 {
                    1i32 << (precision - 1)
                } else if y == 0 {
                    s(i - n)
                } else if x == 0 {
                    s(i - row)
                } else {
                    let (ra, rb, rc) = (s(i - n), s(i - row), s(i - row - n));
                    match predictor {
                        1 => ra,
                        2 => rb,
                        3 => rc,
                        4 => ra + rb - rc,
                        5 => ra + ((rb - rc) >> 1),
                        6 => rb + ((ra - rc) >> 1),
                        _ => (ra + rb) >> 1,
                    }
                };
                let mut d = (s(i) - pred) & 0xFFFF;
                if d >= 32768 {
                    d -= 65536;
                }
                let ssss = if d == -32768 {
                    16
                } else {
                    32 - d.unsigned_abs().leading_zeros()
                };
                bits.put(ssss, 5);
                if (1..16).contains(&ssss) {
                    let v = if d > 0 { d } else { d + (1 << ssss) - 1 };
                    bits.put(v as u32, ssss);
                }
            }
        }
    }
    while bits.n != 0 {
        bits.put(1, 1);
    }
    out.extend(bits.out);
    out.extend([0xFF, 0xD9]);
    out
}

// ---------------------------------------------------------------- CR2 ----

/// A Canon CR2.
#[derive(Debug, Clone)]
pub struct Cr2 {
    pub shot: Shot,
    /// Lossless-JPEG components (2 or 4).
    pub comps: u32,
    /// `cr2_slice`, or none (one slice).
    pub slices: Option<[u16; 3]>,
    pub precision: u32,
    pub predictor: u8,
    /// Write the maker note's `SensorInfo` and `ColorBalance`.
    pub maker_note: bool,
    /// Start the CFA one row off (green-blue / red-green at the origin), as
    /// a 5D Mark II file does.
    pub row_shift: bool,
}

impl Default for Cr2 {
    fn default() -> Self {
        let mut shot = Shot::new(72, 44, 0, 16383);
        shot.active = [4, 8, 44, 72];
        shot.black = [2040, 2050, 2046, 2056];
        Cr2 {
            shot,
            comps: 2,
            slices: Some([2, 24, 24]),
            precision: 14,
            predictor: 1,
            maker_note: true,
            row_shift: false,
        }
    }
}

/// Write a CR2 of `scene`.
pub fn cr2(spec: &Cr2, scene: impl Fn(u32, u32) -> [f64; 3]) -> Vec<u8> {
    let s = &spec.shot;
    let shift = u32::from(spec.row_shift);
    let samples = mosaic(s, |x, y| rggb(x, y + shift), scene);
    let (w, h) = (s.width as usize, s.height as usize);
    // Slice the image into the stream order: each slice top to bottom.
    let widths: Vec<usize> = match spec.slices {
        Some([n, sw, last]) => std::iter::repeat_n(usize::from(sw), usize::from(n))
            .chain(std::iter::once(usize::from(last)))
            .collect(),
        None => vec![w],
    };
    assert_eq!(
        widths.iter().sum::<usize>(),
        w,
        "slices must cover the width"
    );
    let mut stream = Vec::with_capacity(w * h);
    let mut x0 = 0;
    for sw in widths {
        for y in 0..h {
            stream.extend_from_slice(&samples[y * w + x0..y * w + x0 + sw]);
        }
        x0 += sw;
    }
    let frame_w = s.width / spec.comps;
    let jpeg = ljpeg(
        &stream,
        frame_w,
        s.height,
        spec.comps,
        spec.precision,
        spec.predictor,
    );
    let mut t = T::new(true, b"II*\0\0\0\0\0CR\x02\0\0\0\0\0");
    let data_at = t.blob(&jpeg);
    let [top, left, bottom, right] = s.active;
    let note_at = if spec.maker_note {
        let info: Vec<u16> = vec![
            34,
            s.width as u16,
            s.height as u16,
            0,
            0,
            left as u16,
            top as u16,
            right as u16 - 1,
            bottom as u16 - 1,
            2,
            top as u16,
            left as u16 - 1,
            bottom as u16 - 1,
            0,
            0,
            0,
            0,
        ];
        let mut balance = vec![0u16; 1273];
        let level = |c: usize| (1024.0 / SENSITIVITY[c]).round() as u16;
        balance[63..67].copy_from_slice(&[level(0), 1024, 1024, level(2)]);
        Some(t.ifd(vec![(0x00E0, V::S(info)), (0x4001, V::S(balance))]))
    } else {
        None
    };
    let note_len = t.buf.len() as u32 - note_at.unwrap_or(0);
    let exif = note_at.map(|at| {
        let note = t.buf[at as usize..].to_vec();
        let _ = note_len;
        t.ifd(vec![(37500, V::U(note))])
    });
    let mut ifd0 = vec![
        (256, V::L(vec![s.width])),
        (257, V::L(vec![s.height])),
        (271, V::A("Canon")),
        (274, V::S(vec![s.orientation])),
    ];
    if let Some(e) = exif {
        ifd0.push((34665, V::L(vec![e])));
    }
    let ifd0 = t.ifd(ifd0);
    let ifd1 = t.ifd(vec![(254, V::L(vec![1]))]);
    let ifd2 = t.ifd(vec![(254, V::L(vec![1]))]);
    let mut raw = vec![
        (259, V::S(vec![6])),
        (273, V::L(vec![data_at])),
        (279, V::L(vec![jpeg.len() as u32])),
        (0xC5D8, V::L(vec![1])),
    ];
    if let Some(sl) = spec.slices {
        raw.push((0xC640, V::S(sl.to_vec())));
    }
    let ifd3 = t.ifd(raw);
    t.set_next(ifd0, ifd1);
    t.set_next(ifd1, ifd2);
    t.set_next(ifd2, ifd3);
    t.put32(4, ifd0);
    t.put32(12, ifd3);
    t.buf
}

// ------------------------------------------------ TIFF-shaped vendors ----

/// How uncompressed samples are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    /// Two bytes per sample in the file's byte order.
    Words,
    /// Rows packed MSB-first at `bits`.
    Packed,
}

fn pack(samples: &[u16], w: usize, bits: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for row in samples.chunks(w) {
        let mut acc: u64 = 0;
        let mut n = 0u32;
        for &s in row {
            acc = (acc << bits) | u64::from(s & ((1 << bits) - 1) as u16);
            n += bits;
            while n >= 8 {
                n -= 8;
                out.push((acc >> n) as u8);
            }
        }
        if n > 0 {
            out.push((acc << (8 - n)) as u8);
        }
    }
    out
}

fn words(t: &T, samples: &[u16]) -> Vec<u8> {
    samples.iter().flat_map(|&s| t.u16(s)).collect()
}

/// A Nikon NEF: big-endian, the CFA raw in a `SubIFD`, the maker note's
/// black and as-shot balance.
pub fn nef(shot: &Shot, bits: u32, store: Store, scene: impl Fn(u32, u32) -> [f64; 3]) -> Vec<u8> {
    let samples = mosaic(shot, rggb, scene);
    let mut t = T::new(false, b"MM\0*\0\0\0\0");
    let data = match store {
        Store::Words => words(&t, &samples),
        Store::Packed => pack(&samples, shot.width as usize, bits),
    };
    let data_at = t.blob(&data);
    let raw = t.ifd(vec![
        (254, V::L(vec![0])),
        (256, V::L(vec![shot.width])),
        (257, V::L(vec![shot.height])),
        (258, V::S(vec![bits as u16])),
        (259, V::S(vec![1])),
        (262, V::S(vec![32803])),
        (273, V::L(vec![data_at])),
        (277, V::S(vec![1])),
        (278, V::L(vec![shot.height])),
        (279, V::L(vec![data.len() as u32])),
        (33421, V::S(vec![2, 2])),
        (33422, V::B(vec![0, 1, 1, 2])),
    ]);
    // The maker note: "Nikon\0", version, two bytes, then its own TIFF.
    let mut inner = T::new(false, b"MM\0*\0\0\0\x08");
    let rb = |c: usize| ((1.0 / SENSITIVITY[c]) * 10_000.0).round() as u32;
    let inner_ifd = inner.ifd(vec![
        (
            0x000C,
            V::R(vec![(rb(0), 10_000), (rb(2), 10_000), (1, 1), (1, 1)]),
        ),
        (0x003D, V::S(shot.black.to_vec())),
    ]);
    inner.put32(4, inner_ifd);
    let mut note = b"Nikon\0\x02\x10\0\0".to_vec();
    note.extend(inner.buf);
    let exif = t.ifd(vec![(37500, V::U(note))]);
    let ifd0 = t.ifd(vec![
        (254, V::L(vec![1])),
        (256, V::L(vec![160])),
        (257, V::L(vec![120])),
        (271, V::A("NIKON CORPORATION")),
        (274, V::S(vec![shot.orientation])),
        (330, V::L(vec![raw])),
        (34665, V::L(vec![exif])),
    ]);
    t.put32(4, ifd0);
    t.buf
}

/// The four `SonyToneCurve` thresholds the tests use.
pub const SONY_CURVE: [u16; 4] = [8000, 10400, 12900, 14100];

/// The ARW 2 tone curve over doubled 11-bit codes, from the thresholds.
pub fn sony_curve(t: [u16; 4]) -> Vec<u32> {
    let mut knots = [0usize, 0, 0, 0, 0, 4095];
    for i in 0..4 {
        knots[i + 1] = usize::from((t[i] >> 2) & 0xFFF);
    }
    let mut curve: Vec<u32> = (0..4096).collect();
    for i in 0..5 {
        for j in knots[i] + 1..=knots[i + 1] {
            curve[j] = curve[j - 1] + (1 << i);
        }
    }
    curve
}

/// Encode one ARW 2 block of sixteen 11-bit codes, returning the block and
/// the values a decoder reconstructs.
pub fn arw2_block(p: &[u16; 16]) -> ([u8; 16], [u16; 16]) {
    let max = *p.iter().max().unwrap();
    let min = *p.iter().min().unwrap();
    let imax = p.iter().position(|&v| v == max).unwrap();
    let imin = (0..16)
        .find(|&i| p[i] == min && i != imax)
        .unwrap_or(if imax == 0 { 1 } else { 0 });
    let mut sh = 0u32;
    while sh < 4 && (0x80u32 << sh) <= u32::from(max - min) {
        sh += 1;
    }
    let mut v: u128 = u128::from(max) | (u128::from(min) << 11);
    v |= (imax as u128) << 22 | (imin as u128) << 26;
    let mut back = [0u16; 16];
    let mut bit = 30;
    for i in 0..16 {
        back[i] = if i == imax {
            max
        } else if i == imin {
            min
        } else {
            let d = (u32::from(p[i].saturating_sub(min)) >> sh).min(0x7F);
            v |= u128::from(d) << bit;
            bit += 7;
            ((d << sh) as u16 + min).min(0x7FF)
        };
    }
    (v.to_le_bytes(), back)
}

/// The 11-bit code whose doubled index maps at or just below `value`.
pub fn sony_code(curve: &[u32], value: u32) -> u16 {
    (0..2048u16)
        .rev()
        .find(|&p| curve[usize::from(p) * 2] <= value)
        .unwrap_or(0)
}

/// ARW 2 rows (one byte per pixel) of 11-bit `codes`, `w` a multiple of 32;
/// also the codes a decoder reconstructs.
pub fn arw2_rows(codes: &[u16], w: usize) -> (Vec<u8>, Vec<u16>) {
    let mut out = Vec::with_capacity(codes.len());
    let mut back = vec![0u16; codes.len()];
    for (y, row) in codes.chunks(w).enumerate() {
        for col in (0..w).step_by(32) {
            for half in 0..2 {
                let p: [u16; 16] = std::array::from_fn(|i| row[col + half + 2 * i]);
                let (block, got) = arw2_block(&p);
                out.extend(block);
                for i in 0..16 {
                    back[y * w + col + half + 2 * i] = got[i];
                }
            }
        }
    }
    (out, back)
}

/// A Sony ARW, little-endian. `compressed`: ARW 2 (the shot's levels are
/// on the curve's output scale), else 14-bit words. Returns the file and
/// the CFA values a decoder must produce.
pub fn arw(
    shot: &Shot,
    compressed: bool,
    scene: impl Fn(u32, u32) -> [f64; 3],
) -> (Vec<u8>, Vec<u16>) {
    let samples = mosaic(shot, rggb, scene);
    let mut t = T::new(true, b"II*\0\0\0\0\0");
    let curve = sony_curve(SONY_CURVE);
    let (data, expect) = if compressed {
        let codes: Vec<u16> = samples
            .iter()
            .map(|&v| sony_code(&curve, u32::from(v)))
            .collect();
        let (rows, back) = arw2_rows(&codes, shot.width as usize);
        let expect = back
            .iter()
            .map(|&p| curve[usize::from(p) * 2] as u16)
            .collect();
        (rows, expect)
    } else {
        (words(&t, &samples), samples.clone())
    };
    let data_at = t.blob(&data);
    let raw = t.ifd(vec![
        (254, V::L(vec![0])),
        (256, V::L(vec![shot.width])),
        (257, V::L(vec![shot.height])),
        (258, V::S(vec![if compressed { 8 } else { 14 }])),
        (259, V::S(vec![if compressed { 32767 } else { 1 }])),
        (262, V::S(vec![32803])),
        (273, V::L(vec![data_at])),
        (277, V::S(vec![1])),
        (279, V::L(vec![data.len() as u32])),
        (33421, V::S(vec![2, 2])),
        (33422, V::B(vec![0, 1, 1, 2])),
        (0x7010, V::S(SONY_CURVE.to_vec())),
    ]);
    let ifd0 = t.ifd(vec![
        (254, V::L(vec![1])),
        (271, V::A("SONY")),
        (274, V::S(vec![shot.orientation])),
        (330, V::L(vec![raw])),
    ]);
    t.put32(4, ifd0);
    (t.buf, expect)
}

/// An Olympus ORF (`IIRO`): 16-bit words in IFD0.
pub fn orf(shot: &Shot, scene: impl Fn(u32, u32) -> [f64; 3]) -> Vec<u8> {
    let samples = mosaic(shot, rggb, scene);
    let mut t = T::new(true, b"IIRO\0\0\0\0");
    let data = words(&t, &samples);
    let data_at = t.blob(&data);
    let ifd0 = t.ifd(vec![
        (256, V::L(vec![shot.width])),
        (257, V::L(vec![shot.height])),
        (258, V::S(vec![12])),
        (259, V::S(vec![1])),
        (271, V::A("OLYMPUS IMAGING CORP.")),
        (273, V::L(vec![data_at])),
        (277, V::S(vec![1])),
        (279, V::L(vec![data.len() as u32])),
        (50714, V::L(vec![u32::from(shot.black[0])])),
    ]);
    t.put32(4, ifd0);
    t.buf
}

/// A Panasonic RW2 (`IIU\0`): the Panasonic IFD0 tags, 16-bit words at
/// `RawDataOffset`; `pattern` is the `CFAPattern` code (1-4).
pub fn rw2(shot: &Shot, pattern: u16, scene: impl Fn(u32, u32) -> [f64; 3]) -> Vec<u8> {
    let codes: [u8; 4] = match pattern {
        2 => [1, 0, 2, 1],
        3 => [1, 2, 0, 1],
        4 => [2, 1, 1, 0],
        _ => [0, 1, 1, 2],
    };
    let samples = mosaic(shot, |x, y| codes[((y % 2) * 2 + x % 2) as usize], scene);
    let mut t = T::new(true, b"IIU\0\0\0\0\0");
    let data = words(&t, &samples);
    let [top, left, bottom, right] = shot.active;
    let level = |c: usize| (1024.0 / SENSITIVITY[c]).round() as u16;
    let black = shot.black[0];
    // RawDataOffset is written after the IFD: patch it.
    let ifd0 = t.ifd(vec![
        (0x02, V::S(vec![shot.width as u16])),
        (0x03, V::S(vec![shot.height as u16])),
        (0x04, V::S(vec![top as u16])),
        (0x05, V::S(vec![left as u16])),
        (0x06, V::S(vec![bottom as u16])),
        (0x07, V::S(vec![right as u16])),
        (0x09, V::S(vec![pattern])),
        (0x0A, V::S(vec![12])),
        (0x0B, V::S(vec![34316])),
        (0x1C, V::S(vec![black])),
        (0x1D, V::S(vec![black])),
        (0x1E, V::S(vec![black])),
        (0x24, V::S(vec![level(0)])),
        (0x25, V::S(vec![1024])),
        (0x26, V::S(vec![level(2)])),
        (0x10F, V::A("Panasonic")),
        (0x112, V::S(vec![shot.orientation])),
        (0x118, V::L(vec![0])),
    ]);
    let data_at = t.blob(&data);
    // Find the 0x118 entry and point it at the data.
    let n = usize::from(u16::from_le_bytes([
        t.buf[ifd0 as usize],
        t.buf[ifd0 as usize + 1],
    ]));
    for k in 0..n {
        let e = ifd0 as usize + 2 + k * 12;
        if u16::from_le_bytes([t.buf[e], t.buf[e + 1]]) == 0x118 {
            t.put32(e + 8, data_at);
        }
    }
    t.put32(4, ifd0);
    t.buf
}

/// A Fujifilm RAF: header, metadata records (crop, X-Trans layout when
/// `xtrans`, GRGB balance) and a CFA section holding a TIFF with the
/// `FujiIFD` and 16-bit words in that TIFF's byte order (`le`).
pub fn raf(shot: &Shot, xtrans: bool, le: bool, scene: impl Fn(u32, u32) -> [f64; 3]) -> Vec<u8> {
    let cfa = |x: u32, y: u32| {
        if xtrans {
            XTRANS[((y % 6) * 6 + x % 6) as usize]
        } else {
            rggb(x, y)
        }
    };
    let samples = mosaic(shot, cfa, scene);
    // The CFA section's TIFF.
    let mut t = T::new(
        le,
        if le {
            b"II*\0\0\0\0\0"
        } else {
            b"MM\0*\0\0\0\0"
        },
    );
    let data = words(&t, &samples);
    let data_at = t.blob(&data);
    let level = |c: usize| (1024.0 / SENSITIVITY[c]).round() as u16;
    let fuji = t.ifd(vec![
        (0xF001, V::L(vec![shot.width])),
        (0xF002, V::L(vec![shot.height])),
        (0xF003, V::L(vec![14])),
        (0xF007, V::L(vec![data_at])),
        (0xF008, V::L(vec![data.len() as u32])),
        (
            0xF00A,
            V::L(shot.black.iter().map(|&b| u32::from(b)).collect()),
        ),
        (
            0xF00E,
            V::L(vec![1024, u32::from(level(0)), u32::from(level(2))]),
        ),
    ]);
    let ifd0 = t.ifd(vec![(0xF000, V::L(vec![fuji]))]);
    t.put32(4, ifd0);
    let section = t.buf;
    // The metadata container.
    let [top, left, bottom, right] = shot.active;
    let mut meta = Vec::new();
    let mut records: Vec<(u16, Vec<u8>)> = vec![
        (
            0x0110,
            [(top as u16).to_be_bytes(), (left as u16).to_be_bytes()].concat(),
        ),
        (
            0x0111,
            [
                ((bottom - top) as u16).to_be_bytes(),
                ((right - left) as u16).to_be_bytes(),
            ]
            .concat(),
        ),
    ];
    if xtrans {
        // Stored last site first.
        records.push((0x0131, XTRANS.iter().rev().copied().collect()));
    }
    meta.extend((records.len() as u32).to_be_bytes());
    for (tag, data) in records {
        meta.extend(tag.to_be_bytes());
        meta.extend((data.len() as u16).to_be_bytes());
        meta.extend(data);
    }
    let mut out = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
    let mut camera = b"X-Synthetic".to_vec();
    camera.resize(32, 0);
    out.extend(camera);
    out.extend(b"0100");
    out.extend([0u8; 20]);
    let meta_at = 0x100u32;
    let cfa_at = meta_at + meta.len() as u32 + 16;
    for v in [
        0u32,
        0,
        meta_at,
        meta.len() as u32,
        cfa_at,
        section.len() as u32,
    ] {
        out.extend(v.to_be_bytes());
    }
    out.resize(meta_at as usize, 0);
    out.extend(meta);
    out.resize(cfa_at as usize, 0);
    out.extend(section);
    out
}
