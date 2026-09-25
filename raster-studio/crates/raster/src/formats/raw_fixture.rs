//! W13-C: a synthetic DNG writer for tests, here and in `app-shell`. Not an
//! exporter: it writes exactly the files the tests describe, with an
//! independent colour model so a decode can be checked against the scene it
//! was built from.
//!
//! The scene is given in linear sRGB. The camera is modelled by
//! [`Spec::color_matrix`] (XYZ to camera, the DNG `ColorMatrix` convention):
//! each raw sample is `black + (white - black) * cam / cam_white_max`, where
//! `cam = ColorMatrix * sRGB_to_XYZ * scene` and `cam_white_max` is the
//! largest channel of the camera's response to sRGB white. `AsShotNeutral`
//! is that response, normalised. A correct develop therefore returns the
//! scene itself, sRGB-encoded.

/// How the raw samples are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// One strip, 16-bit samples in the file's byte order.
    Uncompressed16,
    /// One strip, 12-bit samples packed MSB-first.
    Packed12,
    /// Lossless-JPEG tiles of `tile` x `tile` samples, each coded as a
    /// two-component frame `tile / 2` wide (as DNG writers do), with a
    /// restart marker after every row when `restart` is set.
    LosslessJpegTiles { tile: u32, restart: bool },
}

/// What to write.
#[derive(Debug, Clone)]
pub struct Spec {
    pub width: u32,
    pub height: u32,
    /// 2x2 CFA colour codes (0 red, 1 green, 2 blue), row-major; ignored for
    /// linear raw.
    pub pattern: [u8; 4],
    /// Three samples per pixel, photometric `LinearRaw`, instead of a CFA.
    pub linear_raw: bool,
    pub storage: Storage,
    pub big_endian: bool,
    pub black: u16,
    pub white: u16,
    pub orientation: u16,
    /// XYZ to camera, row-major.
    pub color_matrix: [f64; 9],
    /// Write `AsShotNeutral` (otherwise the reader derives it).
    pub write_neutral: bool,
    /// `ActiveArea` `(top, left, bottom, right)`; the samples outside it are
    /// written as `white` (so a reader that ignores it shows it).
    pub active_area: Option<[u32; 4]>,
    /// `DefaultCropOrigin` `(x, y)` and `DefaultCropSize` `(w, h)`, inside
    /// the active area.
    pub crop: Option<[u32; 4]>,
}

/// A Canon-like `ColorMatrix` (XYZ to camera): invertible, far from the
/// identity, with a strongly green-weighted white.
pub const CAMERA_MATRIX: [f64; 9] = [
    0.6461, -0.0907, -0.0882, -0.4300, 1.2184, 0.2378, -0.0819, 0.1944, 0.5931,
];

impl Default for Spec {
    fn default() -> Self {
        Spec {
            width: 32,
            height: 32,
            pattern: [0, 1, 1, 2],
            linear_raw: false,
            storage: Storage::Uncompressed16,
            big_endian: false,
            black: 256,
            white: 4095,
            orientation: 1,
            color_matrix: CAMERA_MATRIX,
            write_neutral: true,
            active_area: None,
            crop: None,
        }
    }
}

const SRGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];

fn camera(m: &[f64; 9], rgb: [f64; 3]) -> [f64; 3] {
    let xyz = [0, 1, 2].map(|i| (0..3).map(|k| SRGB_TO_XYZ[i][k] * rgb[k]).sum::<f64>());
    [0, 1, 2].map(|i| (0..3).map(|k| m[i * 3 + k] * xyz[k]).sum::<f64>())
}

/// The sRGB encoding of a linear value, as 16-bit, for expected colours.
pub fn srgb16(v: f64) -> u16 {
    let e = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (e.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

struct Tiff {
    be: bool,
    buf: Vec<u8>,
}

enum Value {
    Short(Vec<u16>),
    Long(Vec<u32>),
    Byte(Vec<u8>),
    Ascii(&'static str),
    SRational(Vec<f64>),
    Rational(Vec<f64>),
}

impl Tiff {
    fn u16(&self, v: u16) -> [u8; 2] {
        if self.be {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    }

    fn u32(&self, v: u32) -> [u8; 4] {
        if self.be {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    }

    fn encode(&self, v: &Value) -> (u16, u32, Vec<u8>) {
        match v {
            Value::Short(s) => (
                3,
                s.len() as u32,
                s.iter().flat_map(|&x| self.u16(x)).collect(),
            ),
            Value::Long(s) => (
                4,
                s.len() as u32,
                s.iter().flat_map(|&x| self.u32(x)).collect(),
            ),
            Value::Byte(s) => (1, s.len() as u32, s.clone()),
            Value::Ascii(s) => {
                let mut b = s.as_bytes().to_vec();
                b.push(0);
                (2, b.len() as u32, b)
            }
            Value::SRational(s) => (
                10,
                s.len() as u32,
                s.iter()
                    .flat_map(|&x| {
                        let mut b = self.u32((x * 10_000.0).round() as i32 as u32).to_vec();
                        b.extend(self.u32(10_000));
                        b
                    })
                    .collect(),
            ),
            Value::Rational(s) => (
                5,
                s.len() as u32,
                s.iter()
                    .flat_map(|&x| {
                        let mut b = self.u32((x * 10_000.0).round() as u32).to_vec();
                        b.extend(self.u32(10_000));
                        b
                    })
                    .collect(),
            ),
        }
    }

    /// Append an IFD (entries sorted by tag) and its out-of-line values;
    /// return its offset.
    fn ifd(&mut self, mut entries: Vec<(u16, Value)>) -> u32 {
        entries.sort_by_key(|e| e.0);
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
        let at = self.buf.len();
        let n = entries.len();
        let mut extra_at = at + 2 + n * 12 + 4;
        let mut table = self.u16(n as u16).to_vec();
        let mut extra = Vec::new();
        for (tag, value) in &entries {
            let (kind, count, bytes) = self.encode(value);
            table.extend(self.u16(*tag));
            table.extend(self.u16(kind));
            table.extend(self.u32(count));
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
}

/// Bits out MSB-first, with JPEG byte stuffing.
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
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

    /// Pad the last byte with ones.
    fn flush(&mut self) {
        while self.n != 0 {
            self.put(1, 1);
        }
    }
}

/// A lossless JPEG (predictor 1, 16-bit precision) of `samples`, `width` x
/// `height` pixels of `comps` interleaved components. One Huffman table:
/// every difference category 0-16 has a five-bit code equal to itself.
pub fn lossless_jpeg(
    samples: &[u16],
    width: u32,
    height: u32,
    comps: u32,
    restart: bool,
) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    // DHT: class 0, id 0, seventeen codes of length five.
    let mut counts = [0u8; 16];
    counts[4] = 17;
    let mut dht = vec![0x00];
    dht.extend(counts);
    dht.extend(0u8..=16);
    out.extend([0xFF, 0xC4]);
    out.extend(((dht.len() + 2) as u16).to_be_bytes());
    out.extend(dht);
    // SOF3.
    let mut sof = vec![16];
    sof.extend((height as u16).to_be_bytes());
    sof.extend((width as u16).to_be_bytes());
    sof.push(comps as u8);
    for c in 0..comps {
        sof.extend([c as u8, 0x11, 0]);
    }
    out.extend([0xFF, 0xC3]);
    out.extend(((sof.len() + 2) as u16).to_be_bytes());
    out.extend(sof);
    if restart {
        out.extend([0xFF, 0xDD, 0, 4]);
        out.extend((width as u16).to_be_bytes());
    }
    // SOS: every component on table 0, predictor 1, no point transform.
    let mut sos = vec![comps as u8];
    for c in 0..comps {
        sos.extend([c as u8, 0x00]);
    }
    sos.extend([1, 0, 0]);
    out.extend([0xFF, 0xDA]);
    out.extend(((sos.len() + 2) as u16).to_be_bytes());
    out.extend(sos);
    let (w, h, n) = (width as usize, height as usize, comps as usize);
    let row = w * n;
    let mut bits = BitWriter {
        out: Vec::new(),
        acc: 0,
        n: 0,
    };
    for y in 0..h {
        let first_line = y == 0 || restart;
        if restart && y > 0 {
            bits.flush();
            bits.out.extend([0xFF, 0xD0 + ((y - 1) % 8) as u8]);
        }
        for x in 0..w {
            for c in 0..n {
                let i = y * row + x * n + c;
                let pred = if first_line && x == 0 {
                    1i32 << 15
                } else if x == 0 {
                    i32::from(samples[i - row])
                } else {
                    i32::from(samples[i - n])
                };
                let mut d = (i32::from(samples[i]) - pred) & 0xFFFF;
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
    bits.flush();
    out.extend(bits.out);
    out.extend([0xFF, 0xD9]);
    out
}

/// Write a DNG of `scene` (linear sRGB, `0..=1`, at pixel `(x, y)` of the
/// active area) as [`Spec`] says.
pub fn dng(spec: &Spec, scene: impl Fn(u32, u32) -> [f64; 3]) -> Vec<u8> {
    let (w, h) = (spec.width, spec.height);
    let white_resp = camera(&spec.color_matrix, [1.0; 3]);
    let max = white_resp.iter().copied().fold(f64::MIN, f64::max);
    let [top, left, bottom, right] = spec.active_area.unwrap_or([0, 0, h, w]);
    let spp: u32 = if spec.linear_raw { 3 } else { 1 };
    let range = f64::from(spec.white - spec.black);
    let mut samples = vec![spec.white; (w * h * spp) as usize];
    for y in top..bottom {
        for x in left..right {
            let cam = camera(&spec.color_matrix, scene(x - left, y - top));
            let code = |c: usize| {
                (f64::from(spec.black) + range * (cam[c] / max).clamp(0.0, 1.0)).round() as u16
            };
            let i = ((y * w + x) * spp) as usize;
            if spec.linear_raw {
                for c in 0..3 {
                    samples[i + c] = code(c);
                }
            } else {
                // The pattern's phase is relative to the active area.
                let p = spec.pattern[(((y - top) % 2) * 2 + (x - left) % 2) as usize];
                samples[i] = code(usize::from(p));
            }
        }
    }
    let mut t = Tiff {
        be: spec.big_endian,
        buf: if spec.big_endian {
            b"MM\0*\0\0\0\0".to_vec()
        } else {
            b"II*\0\0\0\0\0".to_vec()
        },
    };
    // Pixel data first.
    let (bits, layout): (u16, Vec<(u16, Value)>) = match spec.storage {
        Storage::Uncompressed16 => {
            let at = t.buf.len() as u32;
            let bytes: Vec<u8> = samples.iter().flat_map(|&s| t.u16(s)).collect();
            let len = bytes.len() as u32;
            t.buf.extend(bytes);
            (
                16,
                vec![
                    (273, Value::Long(vec![at])),
                    (278, Value::Long(vec![h])),
                    (279, Value::Long(vec![len])),
                    (259, Value::Short(vec![1])),
                ],
            )
        }
        Storage::Packed12 => {
            let at = t.buf.len();
            let per_row = (w * spp) as usize;
            for row in samples.chunks(per_row) {
                // TIFF has no byte stuffing: the bits go in as they are, and
                // every row starts on a byte.
                let mut acc: u64 = 0;
                let mut n = 0u32;
                for &s in row {
                    acc = (acc << 12) | u64::from(s & 0x0FFF);
                    n += 12;
                    while n >= 8 {
                        n -= 8;
                        t.buf.push((acc >> n) as u8);
                    }
                }
                if n > 0 {
                    t.buf.push((acc << (8 - n)) as u8);
                }
            }
            let len = (t.buf.len() - at) as u32;
            (
                12,
                vec![
                    (273, Value::Long(vec![at as u32])),
                    (278, Value::Long(vec![h])),
                    (279, Value::Long(vec![len])),
                    (259, Value::Short(vec![1])),
                ],
            )
        }
        Storage::LosslessJpegTiles { tile, restart } => {
            let (across, down) = (w.div_ceil(tile), h.div_ceil(tile));
            let mut offsets = Vec::new();
            let mut counts = Vec::new();
            for ty in 0..down {
                for tx in 0..across {
                    let mut block = Vec::with_capacity((tile * tile * spp) as usize);
                    for y in 0..tile {
                        for x in 0..tile * spp {
                            // Past the image edge the tile is padding, which
                            // a reader must ignore: zero.
                            let (sy, sx) = (ty * tile + y, tx * tile + x / spp);
                            block.push(if sy < h && sx < w {
                                samples[((sy * w + sx) * spp + x % spp) as usize]
                            } else {
                                0
                            });
                        }
                    }
                    let jpeg = lossless_jpeg(&block, tile * spp / 2, tile, 2, restart);
                    offsets.push(t.buf.len() as u32);
                    counts.push(jpeg.len() as u32);
                    t.buf.extend(jpeg);
                }
            }
            (
                16,
                vec![
                    (322, Value::Long(vec![tile])),
                    (323, Value::Long(vec![tile])),
                    (324, Value::Long(offsets)),
                    (325, Value::Long(counts)),
                    (259, Value::Short(vec![7])),
                ],
            )
        }
    };
    let mut raw = vec![
        (254, Value::Long(vec![0])),
        (256, Value::Long(vec![w])),
        (257, Value::Long(vec![h])),
        (258, Value::Short(vec![bits; spp as usize])),
        (
            262,
            Value::Short(vec![if spec.linear_raw { 34892 } else { 32803 }]),
        ),
        (277, Value::Short(vec![spp as u16])),
        (50714, Value::Long(vec![u32::from(spec.black)])),
        (50717, Value::Long(vec![u32::from(spec.white)])),
    ];
    if !spec.linear_raw {
        raw.push((33421, Value::Short(vec![2, 2])));
        raw.push((33422, Value::Byte(spec.pattern.to_vec())));
    }
    if let Some(a) = spec.active_area {
        raw.push((50829, Value::Long(a.to_vec())));
    }
    if let Some([x, y, cw, ch]) = spec.crop {
        raw.push((50719, Value::Long(vec![x, y])));
        raw.push((50720, Value::Long(vec![cw, ch])));
    }
    raw.extend(layout);
    let raw_at = t.ifd(raw);
    let mut ifd0 = vec![
        (254, Value::Long(vec![1])),
        (256, Value::Long(vec![1])),
        (257, Value::Long(vec![1])),
        (271, Value::Ascii("Synthetic")),
        (274, Value::Short(vec![spec.orientation])),
        (330, Value::Long(vec![raw_at])),
        (50706, Value::Byte(vec![1, 4, 0, 0])),
        (50721, Value::SRational(spec.color_matrix.to_vec())),
        (50778, Value::Short(vec![21])),
    ];
    if spec.write_neutral {
        ifd0.push((
            50728,
            Value::Rational(white_resp.iter().map(|v| v / max).collect()),
        ));
    }
    let ifd0_at = t.ifd(ifd0);
    let first = t.u32(ifd0_at);
    t.buf[4..8].copy_from_slice(&first);
    t.buf
}
