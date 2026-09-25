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
    /// `para` parametric curve. The seven floats are `g, a, b, c, d, e, f`
    /// (ICC.1:2010 table 68); a kind with fewer parameters leaves the rest 0.
    Parametric { kind: u16, params: [f32; 7] },
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
    /// W13-F: the `wtpt` tag, the media white in PCS XYZ (D50 when the
    /// profile carries none), which Absolute Colorimetric scales by.
    media_white: [f32; 3],
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
fn decode_param(kind: u16, p: &[f32; 7], enc: f32) -> f32 {
    let x = enc.clamp(0.0, 1.0);
    let [g, a, b, c, d, e, f] = *p;
    let y = match kind {
        0 => x.powf(g),
        // W13-F: kinds 1 and 2 as ICC.1:2010 table 68 has them (they used
        // to read `c` as a gain and drop kind 2's offset): (aX + b)^g from
        // the break -b/a up, plus c for kind 2; below the break 0, or c.
        1 | 2 => {
            let brk = if a.abs() > 0.0 {
                (-b / a).max(0.0)
            } else {
                0.0
            };
            let offset = if kind == 2 { c } else { 0.0 };
            if x >= brk {
                (a * x + b).max(0.0).powf(g) + offset
            } else {
                offset
            }
        }
        // ICC.1:2010 table 68: Y = (aX + b)^g at and above d, cX below it.
        3 => {
            if x >= d {
                (a * x + b).max(0.0).powf(g)
            } else {
                c * x
            }
        }
        // ICC.1:2010 table 68: Y = (aX + b)^g + e at and above d, cX + f
        // below it. W13-F: this used to add c above the break and use d as
        // the slope below it, dropping e and f.
        _ => {
            if x >= d {
                (a * x + b).max(0.0).powf(g) + e
            } else {
                c * x + f
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
        // ICC.1:2010 7.2.9: the profile file signature sits at byte 36.
        // W13-F: this used to read byte 4 (the CMM type field), which refused
        // every real profile; byte 4 is still accepted for the hand-built
        // profiles written against that reading.
        let signed = |at: usize| sig_at(bytes, at).is_some_and(|s| is_sig(s, b"acsp"));
        if !signed(36) && !signed(4) {
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
        let mut media_white = D50_WHITE;

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
                // W13-F: a white with no luminance is no white; keep D50.
                b"wtpt" => {
                    let w = read_xyz(tag)?;
                    if w.iter().all(|v| v.is_finite() && *v > 0.0) {
                        media_white = w;
                    }
                }
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
            media_white,
            trc: [
                trc[0].take().unwrap_or(Curve::Identity),
                trc[1].take().unwrap_or(Curve::Identity),
                trc[2].take().unwrap_or(Curve::Identity),
            ],
        })
    }

    /// W13-F: the profile's media white (`wtpt`) in PCS XYZ.
    pub fn media_white(&self) -> [f32; 3] {
        self.media_white
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

    /// Whether this profile maps encoded RGB to linear sRGB the same way the
    /// standard sRGB space does, sampled over primaries, secondaries and grey.
    ///
    /// Used to answer "is this tag worth keeping?": a profile that is
    /// measurably sRGB needs no separate colour space — treating its pixels
    /// as sRGB is exact, not an approximation. The comparison runs through
    /// the profile's own decode, so primaries *and* tone curves are both
    /// exercised (an sRGB-matrix profile with an identity curve is NOT
    /// sRGB-equivalent and must not claim to be). The tolerance is a quarter
    /// of an 8-bit step, far below anything an untagged-vs-tagged mistake
    /// would produce.
    pub fn is_srgb_equivalent(&self) -> bool {
        const SAMPLES: [[f32; 3]; 9] = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 1.0],
            [0.5, 0.5, 0.5],
            [0.25, 0.75, 0.5],
            [0.18, 0.18, 0.18],
        ];
        const TOLERANCE: f32 = 0.25 / 255.0;
        for rgb in SAMPLES {
            let got = self.to_linear_srgb(rgb);
            let expected = crate::transfer::srgb_to_linear3(rgb);
            if got
                .iter()
                .zip(expected)
                .any(|(a, b)| (a - b).abs() > TOLERANCE)
            {
                return false;
            }
        }
        true
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
            // ICC.1:2010 10.18 (table 67): 1, 3, 4, 5 and 7 parameters for
            // kinds 0..=4, each an s15Fixed16Number after the 12-byte head.
            // W13-F: these were read as IEEE floats with the wrong counts,
            // so every real `para` curve decoded to nonsense.
            let n = match kind {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => return Err(IccError::Unsupported("parametric curve kind")),
            };
            if tag.len() < 12 + n * 4 {
                return Err(IccError::Truncated);
            }
            let param = |i: usize| {
                if i < n {
                    s15fixed16_at(tag, 12 + i * 4).unwrap_or(0.0)
                } else {
                    0.0
                }
            };
            // Absent parameters read 0: kinds 0..=2 have no 'd' segment
            // (kind 2's 4th parameter is its `c`), only kind 4 has e and f.
            Ok(Curve::Parametric {
                kind,
                params: std::array::from_fn(param),
            })
        }
        _ => Err(IccError::Unsupported("TRC type (not curv/para)")),
    }
}

// ---------------------------------------------------------------------------
// W13-F: writing matrix-shaper profiles (Edit > Assign / Convert to Profile)
// ---------------------------------------------------------------------------

/// Adobe RGB (1998)'s colorants, adapted to the D50 PCS white, as its
/// published ICC profile carries them (columns sum to D50).
pub const ADOBE_RGB_1998_COLORANTS: [[f32; 3]; 3] = [
    [0.609_741_2, 0.311_111_4, 0.019_470_2],
    [0.205_276_5, 0.625_671_4, 0.060_867_3],
    [0.149_185_2, 0.063_217_2, 0.744_567_9],
];

/// ProPhoto RGB (ROMM RGB)'s colorants. ROMM is defined on D50, so they need
/// no adaptation.
pub const PROPHOTO_RGB_COLORANTS: [[f32; 3]; 3] = [
    [0.797_668_5, 0.288_040_2, 0.0],
    [0.135_192_9, 0.711_883_5, 0.0],
    [0.031_341_6, 0.000_091_6, 0.824_905_4],
];

/// Entries in the `curv` table [`matrix_shaper_profile`] writes.
const WRITTEN_CURVE_SAMPLES: usize = 1024;

/// A version 2.1 display-class RGB matrix-shaper ICC profile: `colorants`
/// (`rXYZ gXYZ bXYZ`, D50-adapted) and one tone curve shared by the three
/// channels, `decode` mapping an encoded value in `0..=1` to linear light,
/// sampled into a 1024-entry `curv` table. The bytes are a spec-conformant
/// profile (signature at byte 36, `desc`, `cprt`, `wtpt`), so other
/// applications read a file exported with it, and [`MatrixShaper::parse`]
/// reads it back.
///
/// `media_white` is the `wtpt` tag: the white the colorants were adapted
/// from, as a version 2 profile records it (D65 for Adobe RGB, D50 for
/// ProPhoto), which Absolute Colorimetric reads.
pub fn matrix_shaper_profile(
    description: &str,
    colorants: [[f32; 3]; 3],
    media_white: [f32; 3],
    decode: impl Fn(f32) -> f32,
) -> Vec<u8> {
    fn s15(out: &mut Vec<u8>, v: f32) {
        out.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
    }
    fn xyz(v: [f32; 3]) -> Vec<u8> {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        for c in v {
            s15(&mut t, c);
        }
        t
    }
    let ascii: Vec<u8> = description
        .bytes()
        .filter(|b| b.is_ascii() && !b.is_ascii_control())
        .collect();
    // textDescriptionType (v2): the ASCII name, an empty Unicode record and an
    // empty 67-byte ScriptCode record.
    let mut desc = b"desc\0\0\0\0".to_vec();
    desc.extend_from_slice(&(ascii.len() as u32 + 1).to_be_bytes());
    desc.extend_from_slice(&ascii);
    desc.push(0);
    desc.extend_from_slice(&[0; 8]);
    desc.extend_from_slice(&[0; 3 + 67]);
    let mut cprt = b"text\0\0\0\0".to_vec();
    cprt.extend_from_slice(b"No copyright, use freely");
    cprt.push(0);
    let mut trc = b"curv\0\0\0\0".to_vec();
    trc.extend_from_slice(&(WRITTEN_CURVE_SAMPLES as u32).to_be_bytes());
    for i in 0..WRITTEN_CURVE_SAMPLES {
        let lin = decode(i as f32 / (WRITTEN_CURVE_SAMPLES - 1) as f32).clamp(0.0, 1.0);
        trc.extend_from_slice(&((lin * 65535.0).round() as u16).to_be_bytes());
    }
    let bodies: [(&[u8; 4], Vec<u8>); 6] = [
        (b"desc", desc),
        (b"cprt", cprt),
        (b"wtpt", xyz(media_white)),
        (b"rXYZ", xyz(colorants[0])),
        (b"gXYZ", xyz(colorants[1])),
        (b"bXYZ", xyz(colorants[2])),
    ];
    // The three TRC tags share one body, which ICC allows.
    let count = bodies.len() + 3;
    let mut offset = 128 + 4 + count * 12;
    let mut table = Vec::new();
    let mut data = Vec::new();
    let mut place = |sig: &[u8; 4], body: &[u8], table: &mut Vec<u8>, data: &mut Vec<u8>| {
        let at = offset;
        table.extend_from_slice(sig);
        table.extend_from_slice(&(at as u32).to_be_bytes());
        table.extend_from_slice(&(body.len() as u32).to_be_bytes());
        data.extend_from_slice(body);
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
        offset = 128 + 4 + count * 12 + data.len();
        at
    };
    for (sig, body) in &bodies {
        place(sig, body, &mut table, &mut data);
    }
    let trc_at = place(b"rTRC", &trc, &mut table, &mut data);
    for sig in [b"gTRC", b"bTRC"] {
        table.extend_from_slice(sig);
        table.extend_from_slice(&(trc_at as u32).to_be_bytes());
        table.extend_from_slice(&(trc.len() as u32).to_be_bytes());
    }
    let mut out = vec![0u8; 128];
    out[8..12].copy_from_slice(&0x0210_0000u32.to_be_bytes());
    out[12..16].copy_from_slice(b"mntr");
    out[16..20].copy_from_slice(b"RGB ");
    out[20..24].copy_from_slice(b"XYZ ");
    out[36..40].copy_from_slice(b"acsp");
    let mut illuminant = Vec::new();
    for c in D50_WHITE {
        s15(&mut illuminant, c);
    }
    out[68..80].copy_from_slice(&illuminant);
    out.extend_from_slice(&(count as u32).to_be_bytes());
    out.extend_from_slice(&table);
    out.extend_from_slice(&data);
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&size.to_be_bytes());
    out
}

/// Adobe RGB (1998): its colorants and a pure 563/256 (about 2.2) gamma.
pub fn adobe_rgb_1998_profile() -> Vec<u8> {
    matrix_shaper_profile(
        "Adobe RGB (1998) compatible",
        ADOBE_RGB_1998_COLORANTS,
        MEDIA_WHITE_D65,
        |e| e.powf(563.0 / 256.0),
    )
}

/// ProPhoto RGB: ROMM's colorants and its 1.8 gamma with the linear toe
/// below 1/512 of linear light (16 x slope, ISO 22028-2).
pub fn prophoto_rgb_profile() -> Vec<u8> {
    matrix_shaper_profile(
        "ProPhoto RGB compatible",
        PROPHOTO_RGB_COLORANTS,
        D50_WHITE,
        |e| {
            if e < 16.0 / 512.0 {
                e / 16.0
            } else {
                e.powf(1.8)
            }
        },
    )
}

/// W13-F: the D65 media white a version 2 profile of a D65 space (sRGB,
/// Display P3, Adobe RGB) records in its `wtpt` tag.
pub const MEDIA_WHITE_D65: [f32; 3] = D65_WHITE;

/// W13-F: ICC Absolute Colorimetric between two media whites, on a linear
/// sRGB (D65) value: into the D50 PCS, each XYZ component scaled by
/// `from_white / to_white` (ICC.1:2010 annex D, the media-relative to
/// ICC-absolute step and back), and out again. Equal whites change nothing;
/// a D65 source into a D50 destination keeps its bluer white.
pub fn absolute_colorimetric(
    lin_srgb: [f32; 3],
    from_white: [f32; 3],
    to_white: [f32; 3],
) -> [f32; 3] {
    let xyz_d65 = mat3_mul_vec3(&LINEAR_SRGB_TO_XYZ_D65, lin_srgb);
    let pcs = mat3_mul_vec3(&bradford(D65_WHITE, D50_WHITE), xyz_d65);
    let scaled = [0, 1, 2].map(|i| {
        if to_white[i].abs() > 1e-9 {
            pcs[i] * from_white[i] / to_white[i]
        } else {
            pcs[i]
        }
    });
    let back = mat3_mul_vec3(&bradford(D50_WHITE, D65_WHITE), scaled);
    crate::space::xyz_to_linear_srgb(back)
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

    /// A profile with sRGB primaries and the true sRGB tone curve — the
    /// sample-table spelling of it. This is what "sRGB-equivalent" means.
    fn srgb_profile(n: usize) -> Vec<u8> {
        let mut b = gamma_profile(SRGB_PRIMARIES, 2.2, n);
        // Patch the three TRC tables (they sit after the 60 bytes of
        // colourant XYZ) with the real sRGB curve.
        let trc_start = 128 + 4 + 7 * 12 + 60;
        let trc_sz = 12 + n * 2;
        for c in 0..3 {
            let at = trc_start + c * trc_sz + 12;
            for i in 0..n {
                // An ICC `curv` table maps the ENCODED device value (the
                // index) to linear, so the entries are the sRGB decode.
                let enc = i as f32 / (n - 1) as f32;
                let lin = crate::transfer::srgb_to_linear3([enc; 3])[0] * 65535.0;
                let v = (lin.round() as u16).to_be_bytes();
                b[at + i * 2..at + i * 2 + 2].copy_from_slice(&v);
            }
        }
        b
    }

    #[test]
    fn an_srgb_curve_with_srgb_primaries_is_srgb_equivalent_but_gamma_2_2_is_not() {
        let srgb = MatrixShaper::parse(&srgb_profile(256)).unwrap();
        assert!(srgb.is_srgb_equivalent());
        // Same primaries, a 2.2 curve: measurably not the sRGB transfer, so
        // equivalence must not be claimed.
        let g22 = MatrixShaper::parse(&gamma_profile(SRGB_PRIMARIES, 2.2, 256)).unwrap();
        assert!(!g22.is_srgb_equivalent());
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
            (0u16, [2.2f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            (1, [2.2, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            (2, [2.2, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            (3, [2.2, 1.0 / 1.055, 0.055 / 1.055, 0.0, 0.04045, 0.0, 0.0]),
            (4, [2.2, 1.0 / 1.055, 0.055 / 1.055, 0.0, 0.2, 0.0, 0.0]),
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
            params: [2.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        };
        assert!((g.decode(0.5) - 0.5f32.powf(2.2)).abs() < 1e-5);
    }

    /// A `para` tag as ICC writes it: s15Fixed16 parameters, as many as the
    /// kind has.
    fn para_tag(kind: u16, params: &[f32]) -> Vec<u8> {
        let mut t = b"para\0\0\0\0".to_vec();
        t.u16be(kind);
        t.u16be(0);
        for p in params {
            t.s15(*p);
        }
        t
    }

    #[test]
    fn w13f_para_curves_read_s15fixed16_parameters_and_the_spec_counts() {
        // Kind 0 is one parameter: a 16-byte tag.
        let g = read_trc(&para_tag(0, &[2.2])).unwrap();
        assert!((g.decode(0.5) - 0.5f32.powf(2.2)).abs() < 1e-3, "{g:?}");
        // Kind 3 with the sRGB constants decodes the sRGB curve.
        let srgb = read_trc(&para_tag(
            3,
            &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045],
        ))
        .unwrap();
        for e in [0.02f32, 0.2, 0.5, 0.9] {
            let want = crate::transfer::srgb_to_linear3([e; 3])[0];
            assert!(
                (srgb.decode(e) - want).abs() < 1e-3,
                "e={e}: {} vs {want}",
                srgb.decode(e)
            );
        }
    }

    #[test]
    fn w13f_para_kind_4_offsets_e_above_and_cx_plus_f_below_the_break() {
        // ICC.1:2010 table 68, kind 4: Y = (aX + b)^g + e for X >= d,
        // Y = cX + f below it. g 1, a 0.5, b 0, c 0.25, d 0.4, e 0.3, f 0.05.
        let curve = read_trc(&para_tag(4, &[1.0, 0.5, 0.0, 0.25, 0.4, 0.3, 0.05])).unwrap();
        for (x, want) in [(0.2f32, 0.25 * 0.2 + 0.05), (0.8, 0.5 * 0.8 + 0.3)] {
            assert!(
                (curve.decode(x) - want).abs() < 1e-3,
                "x={x}: {} vs {want}",
                curve.decode(x)
            );
        }
    }

    #[test]
    fn w13f_a_profile_signed_at_byte_36_parses() {
        let mut bytes = srgb_profile(256);
        bytes[4..8].copy_from_slice(b"lcms");
        bytes[36..40].copy_from_slice(b"acsp");
        assert!(MatrixShaper::parse(&bytes).unwrap().is_srgb_equivalent());
    }

    #[test]
    fn w13f_written_profiles_parse_and_carry_their_gamuts() {
        for (bytes, name) in [
            (adobe_rgb_1998_profile(), "Adobe RGB"),
            (prophoto_rgb_profile(), "ProPhoto"),
        ] {
            assert_eq!(&bytes[36..40], b"acsp", "{name}: signed where ICC says");
            assert_eq!(
                u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize,
                bytes.len(),
                "{name}: the header declares the whole profile"
            );
            let p = MatrixShaper::parse(&bytes).unwrap();
            assert!(!p.is_srgb_equivalent(), "{name} is not sRGB");
            // White stays white and grey stays grey.
            let white = p.to_linear_srgb([1.0; 3]);
            for c in white {
                assert!((c - 1.0).abs() < 0.01, "{name} white {white:?}");
            }
            let grey = p.to_linear_srgb([0.5; 3]);
            assert!((grey[0] - grey[1]).abs() < 0.01 && (grey[1] - grey[2]).abs() < 0.01);
            // Both spaces hold a green sRGB cannot: full device green lands
            // outside the sRGB cube.
            let green = p.to_linear_srgb([0.0, 1.0, 0.0]);
            assert!(
                green.iter().any(|c| *c < -0.01 || *c > 1.01),
                "{name} green {green:?}"
            );
            // The encode inverts the decode.
            let back = p.from_linear_srgb(p.to_linear_srgb([0.2, 0.6, 0.9]));
            for (b, want) in back.iter().zip([0.2, 0.6, 0.9]) {
                assert!((b - want).abs() < 2e-3, "{name} {back:?}");
            }
        }
    }

    #[test]
    fn w13f_the_media_white_is_read_from_wtpt_and_absolute_scales_by_it() {
        let adobe = MatrixShaper::parse(&adobe_rgb_1998_profile()).unwrap();
        let prophoto = MatrixShaper::parse(&prophoto_rgb_profile()).unwrap();
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-3);
        assert!(
            close(adobe.media_white(), MEDIA_WHITE_D65),
            "{:?}",
            adobe.media_white()
        );
        assert!(
            close(prophoto.media_white(), D50_WHITE),
            "{:?}",
            prophoto.media_white()
        );
        // Equal whites: nothing moves.
        let grey = [0.4, 0.4, 0.4];
        assert!(close(
            absolute_colorimetric(grey, D65_WHITE, D65_WHITE),
            grey
        ));
        // A D65 white into a D50 medium stays bluer than the medium's white.
        let out = absolute_colorimetric(grey, D65_WHITE, D50_WHITE);
        assert!(out[2] > out[0] + 0.02, "{out:?}");
        // And back again is the identity.
        let back = absolute_colorimetric(out, D50_WHITE, D65_WHITE);
        assert!(close(back, grey), "{back:?}");
    }
}
