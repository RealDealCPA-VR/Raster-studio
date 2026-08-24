//! A self-contained, conservative ICC *matrix-shaper* decoder.
//!
//! This is the engine half of S1.7: it turns the tags of a classic ICC
//! v2/v4 **DeviceRGB �  PCS XYZ** profile (`rXYZ gXYZ bXYZ` colorants plus
//! `rTRC gTRC bTRC` one-dimensional tone curves) into a real
//! encoded-RGB � linear-sRGB transform, applying Bradford chromatic
//! adaptation from the D50 PCS to the D65 working white.
//!
//! It deliberately supports only the matrix-shaper subset:
//!
//! * RGB device colour space, XYZ connection space (matrix-shaper). Lab PCS,
//!   CMYK and every LUT tag set (`A2B0`/`B2A0`) are rejected as
//!   [`IccError::Unsupported`].
//! * `curv` (16-bit sampled) and `para` (parametric v4 types 0..=4) TRCs.
//!   `mAB`/`mBA` tone curves are not device TRCs and are not consumed here.
//! * Matrix profiles whose `rXYZ/gXYZ/bXYZ` sum to a non-finite white are
//!   rejected rather than approximated.
//!
//! No I/O happens here: the caller hands over the raw profile bytes (from the
//! asset store keyed by hash). Threading those bytes from the document through
//! [`crate::ColorSpace::IccProfile`] into the compositor and export path is
//! the remaining architectural step beyond this engine.
//!
//! Numeric conventions follow ICC.1:2010 §10.14 (s15Fixed16), §10.15
//! (`curvType`) and §10.17 (`paraCurveType`). Encoding is the numeric inverse
//! of decoding, so `decode` and `encode` are exact round-trips of each other
//! for the monotone TRCs this engine accepts.

use crate::space::{mat3_mul_vec3, Mat3, LINEAR_SRGB_TO_XYZ_D65};

/// Why a byte stream is not a usable matrix-shaper profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IccError {
    /// Shorter than the 128-byte header or the 12-byte tag table.
    Truncated,
    /// The `acsp` signature or a required tag signature is wrong / missing.
    BadSignature(&'static str),
    /// A supported signature points beyond the buffer (a hostile profile).
    OutOfBounds(&'static str),
    /// A matrix-shaper-unfriendly profile: wrong device class, not RGB/XYZ,
    /// or a Lab PCS / LUT tag set this engine does not implement.
    Unsupported(&'static str),
    /// A parsed value is not finite or a matrix is singular.
    NonFinite,
}

impl std::fmt::Display for IccError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IccError::Truncated => write!(f, "ICC profile is truncated"),
            IccError::BadSignature(s) => write!(f, "bad ICC signature: {s}"),
            IccError::OutOfBounds(s) => write!(f, "ICC tag {s} lies outside the buffer"),
            IccError::Unsupported(s) => write!(f, "unsupported ICC profile: {s}"),
            IccError::NonFinite => write!(f, "ICC profile contains a non-finite value"),
        }
    }
}

impl std::error::Error for IccError {}

/// A one-dimensional tone reproduction curve (TRC).
#[derive(Debug, Clone)]
pub enum Curve {
    /// No tag present / raw identity curve.
    Identity,
    /// `curv` with a zero-length table: an actual gamma of 1.0.
    Gamma(f32),
    /// `curv` with a sampled 16-bit table.
    Sampled(Vec<u16>),
    /// `para` parametric curve. The five floats are `g, a, b, c, d`.
    Parametric { kind: u16, params: [f32; 5] },
}

/// A parsed matrix-shaper RGB profile.
#[derive(Debug, Clone)]
pub struct MatrixShaper {
    /// Columns are `rXYZ gXYZ bXYZ`; maps linear device RGB to PCS XYZ (D50).
    rgb_to_xyz_d50: Mat3,
    /// Column-wise inverse of [`Self::rgb_to_xyz_d50`]; PCS XYZ back to device.
    xyz_d50_to_rgb: Mat3,
    /// The per-channel tone curves, applied before / after the matrix.
    trc: [Curve; 3],
}

// ---------------------------------------------------------------------------
// Byte readers (all big-endian, as ICC is)
// ---------------------------------------------------------------------------

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}
fn f32_at(b: &[u8], at: usize) -> Option<f32> {
    Some(f32::from_bits(u32_at(b, at)?))
}
fn sig_at(b: &[u8], at: usize) -> Option<[u8; 4]> {
    Some([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ])
}
fn s15fixed16_at(b: &[u8], at: usize) -> Option<f32> {
    u32_at(b, at).map(|u| (u as i32) as f32 / 65536.0)
}

fn is_sig(s: [u8; 4], want: &[u8; 4]) -> bool {
    &s == want
}

fn inv3(m: &Mat3) -> Option<Mat3> {
    let a = m[0][0];
    let b = m[0][1];
    let c = m[0][2];
    let d = m[1][0];
    let e = m[1][1];
    let f = m[1][2];
    let g = m[2][0];
    let h = m[2][1];
    let i = m[2][2];
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (e * i - f * h) * inv,
            (c * h - b * i) * inv,
            (b * f - c * e) * inv,
        ],
        [
            (f * g - d * i) * inv,
            (a * i - c * g) * inv,
            (c * d - a * f) * inv,
        ],
        [
            (d * h - e * g) * inv,
            (b * g - a * h) * inv,
            (a * e - b * d) * inv,
        ],
    ])
}

// ---------------------------------------------------------------------------
// TRC decoding / encoding
// ---------------------------------------------------------------------------

impl Curve {
    /// Encoded device value in `0..=1` to linear intensity in `0..=1`.
    pub fn decode(&self, enc: f32) -> f32 {
        match self {
            Curve::Identity => enc.clamp(0.0, 1.0),
            Curve::Gamma(g) => enc.clamp(0.0, 1.0).powf(*g),
            Curve::Sampled(tab) => decode_sampled(tab, enc),
            Curve::Parametric { kind, params } => decode_param(*kind, params, enc),
        }
    }

    /// Linear intensity in `0..=1` to encoded device value in `0..=1`.
    ///
    /// The numeric inverse of [`Curve::decode`] (bisection on the monotone
    /// decode), so `decode(encode(x)) == x` to solver tolerance.
    pub fn encode(&self, lin: f32) -> f32 {
        let lin = lin.clamp(0.0, 1.0);
        let d = |x| self.decode(x);
        // A tiny in-range probe that brackets every accepted curve's shape;
        // decode is monotone non-decreasing, so bisection lands on lin.
        let mut lo = 0.0f32;
        let mut hi = 1.0f32;
        for _ in 0..48 {
            let mid = 0.5 * (lo + hi);
            if d(mid) < lin {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }
}

fn decode_sampled(tab: &[u16], enc: f32) -> f32 {
    let enc = enc.clamp(0.0, 1.0);
    let n = tab.len();
    if n == 0 {
        return enc;
    }
    let f = enc * (n - 1) as f32;
    let i = f.floor() as usize;
    if i >= n - 1 {
        return tab[n - 1] as f32 / 65535.0;
    }
    let t = f - i as f32;
    let a = tab[i] as f32 / 65535.0;
    let b = tab[i + 1] as f32 / 65535.0;
    a + t * (b - a)
}

/// Decode a `para` tone curve per ICC.1:2010 §10.17.
fn decode_param(kind: u16, p: &[f32; 5], enc: f32) -> f32 {
    let x = enc.clamp(0.0, 1.0);
    let [g, a, b, c, d] = *p;
    let y = match kind {
        0 => x.powf(g),
        1 => c * x.powf(g),
        2 => {
            let brk = if g.abs() > 0.0 {
                (-b / a).max(0.0)
            } else {
                0.0
            };
            if x >= brk {
                (a * x + b).max(0.0).powf(g)
            } else {
                0.0
            }
        }
        3 => {
            if x >= d {
                (a * x + b).max(0.0).powf(g) + c
            } else {
                c * x
            }
        }
        _ => {
            if x >= d {
                (a * x + b).max(0.0).powf(g) + c
            } else {
                d * x
            }
        }
    };
    y.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Bradford chromatic adaptation D50 (PCS) <-> D65 (working white)
// ---------------------------------------------------------------------------

fn bradford(src_white: [f32; 3], dst_white: [f32; 3]) -> Mat3 {
    // Bradford cone response matrix and its inverse (rounded constants).
    const M: Mat3 = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    const M_INV: Mat3 = [
        [0.986_992_9, -0.147_054_3, 0.159_962_7],
        [0.432_305_3, 0.518_360_3, 0.049_291_2],
        [-0.008_528_7, 0.040_042_8, 0.968_486_7],
    ];
    let s = mat3_mul_vec3(&M, src_white);
    let d = mat3_mul_vec3(&M, dst_white);
    let mut adapted = [[0.0f32; 3]; 3];
    for row in 0..3 {
        for col in 0..3 {
            let mut acc = 0.0;
            for k in 0..3 {
                let scale = if s[k].abs() > 1e-9 { d[k] / s[k] } else { 1.0 };
                acc += M_INV[row][k] * scale * M[k][col];
            }
            adapted[row][col] = acc;
        }
    }
    adapted
}

const D50_WHITE: [f32; 3] = [0.9642, 1.0, 0.8249];
const D65_WHITE: [f32; 3] = [0.95047, 1.0, 1.08883];

// ---------------------------------------------------------------------------
// Profile parsing
// ---------------------------------------------------------------------------

const SIG_RGB: [u8; 4] = *b"RGB ";
const SIG_XYZ: [u8; 4] = *b"XYZ ";
const SIG_LAB: [u8; 4] = *b"Lab ";

impl MatrixShaper {
    /// Parse an ICC profile and validate it is a matrix-shaper this engine can
    /// transform. Zero I/O: the caller supplies the raw bytes.
    pub fn parse(bytes: &[u8]) -> Result<MatrixShaper, IccError> {
        if bytes.len() < 128 + 12 {
            return Err(IccError::Truncated);
        }
        if !is_sig(sig_at(bytes, 4).ok_or(IccError::Truncated)?, b"acsp") {
            return Err(IccError::BadSignature("acsp"));
        }
        let class = sig_at(bytes, 12).ok_or(IccError::Truncated)?;
        let colour = sig_at(bytes, 16).ok_or(IccError::Truncated)?;
        let pcs = sig_at(bytes, 20).ok_or(IccError::Truncated)?;
        // Matrix-shaper needs an input/display/output RGB profile in XYZ PCS.
        let class_ok = is_sig(class, b"scnr") || is_sig(class, b"mntr") || is_sig(class, b"prtr");
        if !class_ok {
            return Err(IccError::Unsupported(
                "device class is not input/display/output",
            ));
        }
        if !is_sig(colour, &SIG_RGB) {
            return Err(IccError::Unsupported("device colour space is not RGB"));
        }
        if is_sig(pcs, &SIG_LAB) {
            return Err(IccError::Unsupported("Lab PCS (needs a LUT engine)"));
        }
        if !is_sig(pcs, &SIG_XYZ) {
            return Err(IccError::Unsupported("connection space is not XYZ"));
        }

        let count = u32_at(bytes, 128).ok_or(IccError::Truncated)? as usize;
        if bytes.len() < 132 + count * 12 {
            return Err(IccError::Truncated);
        }

        let mut xyz: Option<[f32; 3]> = None;
        let mut gxyz = None;
        let mut bxyz = None;
        let mut trc: [Option<Curve>; 3] = [None, None, None];

        for i in 0..count {
            let base = 132 + i * 12;
            let sig = sig_at(bytes, base).ok_or(IccError::Truncated)?;
            let off = u32_at(bytes, base + 4).ok_or(IccError::Truncated)? as usize;
            let size = u32_at(bytes, base + 8).ok_or(IccError::Truncated)? as usize;
            if off + size > bytes.len() {
                return Err(IccError::OutOfBounds("tag"));
            }
            let tag = bytes
                .get(off..off + size)
                .ok_or(IccError::OutOfBounds("tag body"))?;
            match &sig {
                b"rXYZ" => xyz = Some(read_xyz(tag)?),
                b"gXYZ" => gxyz = Some(read_xyz(tag)?),
                b"bXYZ" => bxyz = Some(read_xyz(tag)?),
                b"rTRC" => trc[0] = Some(read_trc(tag)?),
                b"gTRC" => trc[1] = Some(read_trc(tag)?),
                b"bTRC" => trc[2] = Some(read_trc(tag)?),
                _ => {}
            }
        }

        let (r, g, b) = match (xyz, gxyz, bxyz) {
            (Some(r), Some(g), Some(b)) => (r, g, b),
            _ => return Err(IccError::BadSignature("colourant tags rXYZ/gXYZ/bXYZ")),
        };
        // Columns are the three primaries' XYZ.
        let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        // The white the full-scale device RGB maps to: must be finite & near the
        // D50 PCS white the matrix-shaper assumes, else reject.
        let white = mat3_mul_vec3(&m, [1.0, 1.0, 1.0]);
        if !white.iter().all(|v| v.is_finite())
            || (white[1] - 1.0).abs() > 0.15
            || (white[0] - 0.9642).abs() > 0.3
        {
            return Err(IccError::NonFinite);
        }
        let inv = inv3(&m).ok_or(IccError::NonFinite)?;
        Ok(MatrixShaper {
            rgb_to_xyz_d50: m,
            xyz_d50_to_rgb: inv,
            trc: [
                trc[0].take().unwrap_or(Curve::Identity),
                trc[1].take().unwrap_or(Curve::Identity),
                trc[2].take().unwrap_or(Curve::Identity),
            ],
        })
    }

    /// Decode an encoded RGB triple in this profile into linear sRGB (D65).
    pub fn to_linear_srgb(&self, rgb: [f32; 3]) -> [f32; 3] {
        let lin = [
            self.trc[0].decode(rgb[0]),
            self.trc[1].decode(rgb[1]),
            self.trc[2].decode(rgb[2]),
        ];
        let xyz_d50 = mat3_mul_vec3(&self.rgb_to_xyz_d50, lin);
        let xyz_d65 = mat3_mul_vec3(&bradford(D50_WHITE, D65_WHITE), xyz_d50);
        crate::space::xyz_to_linear_srgb(xyz_d65)
    }

    /// Encode a linear sRGB (D65) triple into this profile's device encoding.
    pub fn from_linear_srgb(&self, lin_srgb: [f32; 3]) -> [f32; 3] {
        let xyz_d65 = mat3_mul_vec3(&LINEAR_SRGB_TO_XYZ_D65, lin_srgb);
        let xyz_d50 = mat3_mul_vec3(&bradford(D65_WHITE, D50_WHITE), xyz_d65);
        let lin_dev = mat3_mul_vec3(&self.xyz_d50_to_rgb, xyz_d50);
        [
            self.trc[0].encode(lin_dev[0]),
            self.trc[1].encode(lin_dev[1]),
            self.trc[2].encode(lin_dev[2]),
        ]
    }
}

fn read_xyz(tag: &[u8]) -> Result<[f32; 3], IccError> {
    if tag.len() < 12 || !is_sig(sig_at(tag, 0).ok_or(IccError::Truncated)?, b"XYZ ") {
        return Err(IccError::BadSignature("XYZ tag body"));
    }
    let x = s15fixed16_at(tag, 8).ok_or(IccError::Truncated)?;
    let y = s15fixed16_at(tag, 12).ok_or(IccError::Truncated)?;
    let z = s15fixed16_at(tag, 16).ok_or(IccError::Truncated)?;
    if ![x, y, z].iter().all(|v| v.is_finite()) {
        return Err(IccError::NonFinite);
    }
    Ok([x, y, z])
}

fn read_trc(tag: &[u8]) -> Result<Curve, IccError> {
    if tag.len() < 8 {
        return Err(IccError::Truncated);
    }
    let ty = sig_at(tag, 0).ok_or(IccError::Truncated)?;
    match &ty {
        b"curv" => {
            let count = u32_at(tag, 8).ok_or(IccError::Truncated)? as usize;
            if tag.len() < 12 + count * 2 {
                return Err(IccError::OutOfBounds("curv table"));
            }
            if count == 0 {
                return Ok(Curve::Gamma(1.0));
            }
            let mut tab = Vec::with_capacity(count);
            for i in 0..count {
                tab.push(u16_at(tag, 12 + i * 2).ok_or(IccError::Truncated)?);
            }
            Ok(Curve::Sampled(tab))
        }
        b"para" => {
            let kind = u16_at(tag, 8).ok_or(IccError::Truncated)?;
            // 6 floats for kinds 0..=2, 7 for 3, 9 for 4 (after the 12-byte head).
            let n = match kind {
                0 | 1 => 6,
                2 | 3 => 7,
                4 => 9,
                _ => return Err(IccError::Unsupported("parametric curve kind")),
            };
            if tag.len() < 12 + n * 4 {
                return Err(IccError::Truncated);
            }
            let g = f32_at(tag, 12).ok_or(IccError::Truncated)?;
            let a = f32_at(tag, 16).ok_or(IccError::Truncated)?;
            let b = f32_at(tag, 20).ok_or(IccError::Truncated)?;
            let c = f32_at(tag, 24).ok_or(IccError::Truncated)?;
            let d = f32_at(tag, 28).ok_or(IccError::Truncated)?;
            if kind <= 2 {
                // kinds 0..=2 have no 'd' segment; c holds the 4th param.
                Ok(Curve::Parametric {
                    kind,
                    params: [g, a, b, c, 0.0],
                })
            } else {
                Ok(Curve::Parametric {
                    kind,
                    params: [g, a, b, c, d],
                })
            }
        }
        _ => Err(IccError::Unsupported("TRC type (not curv/para)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Push a 4-byte big-endian value onto a builder.
    trait Push {
        fn u16be(&mut self, v: u16);
        fn u32be(&mut self, v: u32);
        fn s15(&mut self, v: f32);
        fn sig(&mut self, s: &[u8; 4]);
    }
    impl Push for Vec<u8> {
        fn u16be(&mut self, v: u16) {
            self.extend_from_slice(&v.to_be_bytes());
        }
        fn u32be(&mut self, v: u32) {
            self.extend_from_slice(&v.to_be_bytes());
        }
        fn s15(&mut self, v: f32) {
            self.u32be(((v * 65536.0) as i32) as u32);
        }
        fn sig(&mut self, s: &[u8; 4]) {
            self.extend_from_slice(s);
        }
    }

    /// Build a valid matrix-shaper RGB profile with the given colourants and a
    /// sampled TRC (each entry = linear^(1/gamma), i.e. a gamma-encoded device).
    fn gamma_profile(primary: [[f32; 3]; 3], gamma: f32, n: usize) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        // Header.
        b.u32be(0); // size placeholder
        b.sig(b"acsp");
        b.u32be(0x04_00_00_00);
        b.sig(b"mntr");
        b.sig(b"RGB ");
        b.sig(b"XYZ ");
        // Pad the rest of the 128-byte header (date-time, signature, platform,
        // flags, mfr/model, attributes, intent, creator, profile id) with zeros;
        // the PCS illuminant we do honour lives at 68..80, which is inside this.
        while b.len() < 80 {
            b.push(0);
        }
        b.s15(0.9642);
        b.s15(1.0);
        b.s15(0.8249);
        while b.len() < 128 {
            b.push(0);
        }
        // Tag table base (128) + 4-byte count + 7*12-byte entries.
        let base = 128u32;
        let nf = n as u32;
        let trc_sz = 12 + nf * 2; // curv: 12-byte head + 16-bit samples
        let tags = 7u32;
        let r_off = base + 4 + tags * 12;
        b.u32be(tags);
        // helper to write a tag triplet
        macro_rules! tag {
            ($sig:expr, $off:expr, $sz:expr) => {{
                b.sig($sig);
                b.u32be($off);
                b.u32be($sz);
            }};
        }
        tag!(b"rXYZ", r_off, 20);
        tag!(b"gXYZ", r_off + 20, 20);
        tag!(b"bXYZ", r_off + 40, 20);
        tag!(b"rTRC", r_off + 60, trc_sz);
        tag!(b"gTRC", r_off + 60 + trc_sz, trc_sz);
        tag!(b"bTRC", r_off + 60 + trc_sz * 2, trc_sz);
        tag!(b"wtpt", r_off + 60 + trc_sz * 3, 20);
        // colourant bodies (XYZ type, 20 bytes).
        for p in primary {
            b.sig(b"XYZ ");
            b.u32be(0);
            b.s15(p[0]);
            b.s15(p[1]);
            b.s15(p[2]);
        }
        // three identical sampled TRCs.
        let mut table = Vec::new();
        for i in 0..n {
            let lin = i as f32 / (n - 1) as f32;
            let enc = 65535.0 * lin.powf(1.0 / gamma);
            table.push(enc.round() as u16);
        }
        for _ in 0..3 {
            b.sig(b"curv");
            b.u32be(0);
            b.u32be(n as u32);
            for v in &table {
                b.u16be(*v);
            }
        }
        // wtpt (XYZ body).
        b.sig(b"XYZ ");
        b.u32be(0);
        b.s15(0.9642);
        b.s15(1.0);
        b.s15(0.8249);
        // Patch the size (up to 128-byte header start).
        let size = b.len() as u32;
        b[0..4].copy_from_slice(&size.to_be_bytes());
        b
    }

    // sRGB primaries (D65), scaled so full white sums to (0.9642,1.0,0.8249).
    const SRGB_PRIMARIES: [[f32; 3]; 3] = [
        [0.436_041, 0.222_485, 0.013_916],
        [0.385_113, 0.716_909, 0.097_107],
        [0.143_046, 0.060_607, 0.713_913],
    ];

    #[test]
    fn a_gamma_2_2_profile_round_trips_to_linear_srgb() {
        let bytes = gamma_profile(SRGB_PRIMARIES, 2.2, 256);
        let p = MatrixShaper::parse(&bytes).unwrap();
        for v in [0.0, 0.1, 0.4, 0.75, 1.0] {
            let enc = [v, v, v];
            let lin = p.to_linear_srgb(enc);
            // Grey in = grey out (the matrix sums to white; gamma preserves it),
            // and the value is the 2.2 decode of an sRGB-encoded sample ~ but
            // scaled: decoded 2.2 gamma is v^2.2 here because the table was
            // built with the *device* gamma from linear probes.
            let back = p.from_linear_srgb(lin);
            for k in 0..3 {
                assert!(
                    (back[k] - enc[k]).abs() < 2e-3,
                    "round-trip broke at enc={v}: {back:?} vs {enc:?}"
                );
            }
        }
    }

    #[test]
    fn the_colourant_matrix_reproduces_the_srgb_primaries() {
        let bytes = gamma_profile(SRGB_PRIMARIES, 2.2, 256);
        let p = MatrixShaper::parse(&bytes).unwrap();
        // A pure red primary, fully on. decode(1.0)=1.0, so lin RGB is (1,0,0)
        // and the matrix column is rXYZ -> to D65 then to linear sRGB: the red
        // primary projects to ~(1,0,0) in linear sRGB.
        let lin = p.to_linear_srgb([1.0, 0.0, 0.0]);
        assert!(
            lin[0] > 0.9 && lin[1].abs() < 0.08 && lin[2].abs() < 0.08,
            "{lin:?}"
        );
        let green = p.to_linear_srgb([0.0, 1.0, 0.0]);
        assert!(
            green[1] > 0.9 && green[0].abs() < 0.08 && green[2].abs() < 0.08,
            "{green:?}"
        );
    }

    #[test]
    fn an_out_of_gamut_colorant_set_is_rejected() {
        // A red primary whose column plus the others leaves a finite white is
        // fine; a matrix that fails the near-D50 white probe (here a broken
        // blue column that pushes the sum far off the PCS white) is refused.
        let bad = gamma_profile(
            [
                [0.436, 0.222, 0.013],
                [0.385, 0.716, 0.097],
                [1.8, 1.5, 6.0],
            ],
            2.2,
            16,
        );
        assert_eq!(MatrixShaper::parse(&bad).unwrap_err(), IccError::NonFinite);
    }

    #[test]
    fn bad_signatures_and_truncation_are_errors_not_panics() {
        assert_eq!(MatrixShaper::parse(&[]).unwrap_err(), IccError::Truncated);
        let mut bytes = gamma_profile(SRGB_PRIMARIES, 2.2, 16);
        bytes[4..8].copy_from_slice(b"nope");
        assert_eq!(
            MatrixShaper::parse(&bytes).unwrap_err(),
            IccError::BadSignature("acsp")
        );
        // Truncate inside the tag table region.
        let short = &gamma_profile(SRGB_PRIMARIES, 2.2, 16)[..140];
        assert!(MatrixShaper::parse(short).is_err());
    }

    #[test]
    fn parametric_kinds_0_to_4_round_trip_and_decode_gamma() {
        // A short, well-formed profile exercising each para kind in the TRC.
        for (kind, params) in [
            (0u16, [2.2f32, 0.0, 0.0, 0.0, 0.0]),
            (1, [2.2, 0.0, 0.0, 1.0, 0.0]),
            (2, [2.2, 1.0, 0.0, 0.0, 0.0]),
            (3, [2.2, 1.0 / 1.055, 0.055 / 1.055, 0.0, 0.04045]),
            (4, [2.2, 1.0 / 1.055, 0.055 / 1.055, 0.0, 0.2]),
        ] {
            let curve = Curve::Parametric { kind, params };
            // decode(1.0) == 1.0, decode is monotone, and encode inverts it.
            assert!((curve.decode(1.0) - 1.0).abs() < 1e-4, "kind {kind}");
            for x in [0.0, 0.3, 0.6, 1.0] {
                let y = curve.decode(x);
                assert!(
                    y.is_finite() && (0.0..=1.0).contains(&y),
                    "kind {kind} x={x}"
                );
                let back = curve.encode(y);
                assert!((back - x).abs() < 5e-3, "kind {kind} round-trip x={x}");
            }
        }
        // Kind 0 with gamma 2.2 decodes exactly x^2.2.
        let g = Curve::Parametric {
            kind: 0,
            params: [2.2, 0.0, 0.0, 0.0, 0.0],
        };
        assert!((g.decode(0.5) - 0.5f32.powf(2.2)).abs() < 1e-5);
    }
}
