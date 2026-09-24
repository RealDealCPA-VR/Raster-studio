//! Writers for the non-RGB file layouts a document's colour mode asks for
//! (W7-D): CMYK JPEG, CMYK TIFF and palette (PNG-8) PNG.
//!
//! The `image` crate's encoders write RGB only (its JPEG encoder has no CMYK
//! path and its PNG encoder no palette path), so these three are written here,
//! small and self-contained:
//!
//! * **CMYK JPEG** — baseline sequential DCT, four components at 1x1
//!   sampling, one interleaved scan, the Annex K luminance tables for every
//!   component (IJG quality scaling), and an Adobe APP14 marker with
//!   transform 0. Samples are stored inverted (`255 - ink`), the convention
//!   Photoshop writes and every Adobe-aware decoder expects.
//! * **CMYK TIFF** — baseline, uncompressed, chunky, one strip,
//!   `PhotometricInterpretation = 5` (separated), `InkSet = 1` (CMYK).
//! * **PNG-8** — colour type 3 with a `PLTE` (and a `tRNS` when any entry is
//!   not opaque). Lossless when the image has at most 256 distinct RGBA
//!   colours; otherwise re-quantised with 1-bit alpha (see
//!   [`encode_indexed_png`]), so an Indexed document always exports.
//!
//! The separation is `color::cmyk` — the documented naive ink model, not an
//! ICC press profile — so the file, the mode conversion and the soft proof
//! agree about every colour.

use std::collections::HashMap;
use std::io::Write;

use color::cmyk::{rgb8_to_cmyk, Cmyk};

/// Separate straight RGBA8 into CMYK8 (one byte per ink, 255 = full ink),
/// flattening alpha onto `background` first. Colours repeat heavily in real
/// images, so each distinct one is solved once.
pub fn separate_rgba8(rgba: &[u8], background: [u8; 3]) -> Vec<u8> {
    let mut cache: HashMap<[u8; 3], [u8; 4]> = HashMap::new();
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.as_chunks::<4>().0 {
        let a = u32::from(px[3]);
        let rgb: [u8; 3] = std::array::from_fn(|c| {
            ((u32::from(px[c]) * a + u32::from(background[c]) * (255 - a) + 127) / 255) as u8
        });
        let inks = *cache.entry(rgb).or_insert_with(|| {
            let Cmyk { c, m, y, k } = rgb8_to_cmyk(rgb);
            [c, m, y, k].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        });
        out.extend_from_slice(&inks);
    }
    out
}

// ------------------------------------------------------------------ JPEG ---

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Annex K.1 luminance quantisation table, natural (row-major) order.
const LUMA_QUANT: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Annex K.3 luminance DC and AC Huffman specifications.
const DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// Code and length per symbol (Annex C).
fn huffman_codes(bits: &[u8; 16], vals: &[u8]) -> [(u16, u8); 256] {
    let mut table = [(0u16, 0u8); 256];
    let mut code = 0u16;
    let mut k = 0;
    for (len_minus_one, &count) in bits.iter().enumerate() {
        for _ in 0..count {
            table[usize::from(vals[k])] = (code, len_minus_one as u8 + 1);
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    table
}

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, code: u16, len: u8) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | u32::from((code >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                let byte = self.acc as u8;
                self.out.push(byte);
                if byte == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        while self.n != 0 {
            self.put(1, 1);
        }
        self.out
    }
}

/// Magnitude category and the value's bits, per Annex F.
fn category(v: i32) -> (u8, u16) {
    let mag = v.unsigned_abs();
    let size = 32 - mag.leading_zeros();
    let raw = if v < 0 { v - 1 } else { v };
    let bits = (raw as u32) & ((1u32 << size) - 1);
    (size as u8, bits as u16)
}

fn fdct(block: &[f32; 64]) -> [f32; 64] {
    use std::f32::consts::PI;
    let c = |u: usize| {
        if u == 0 {
            std::f32::consts::FRAC_1_SQRT_2
        } else {
            1.0
        }
    };
    let mut cos = [[0.0f32; 8]; 8];
    for (x, row) in cos.iter_mut().enumerate() {
        for (u, v) in row.iter_mut().enumerate() {
            *v = ((2 * x + 1) as f32 * u as f32 * PI / 16.0).cos();
        }
    }
    let mut tmp = [0.0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            let s: f32 = (0..8).map(|x| block[y * 8 + x] * cos[x][u]).sum();
            tmp[y * 8 + u] = 0.5 * c(u) * s;
        }
    }
    let mut out = [0.0f32; 64];
    for u in 0..8 {
        for v in 0..8 {
            let s: f32 = (0..8).map(|y| tmp[y * 8 + u] * cos[y][v]).sum();
            out[v * 8 + u] = 0.5 * c(v) * s;
        }
    }
    out
}

/// Encode CMYK8 samples (255 = full ink) as a baseline Adobe CMYK JPEG.
///
/// `quality` is `1..=100` (IJG scaling of the Annex K table).
pub fn encode_cmyk_jpeg(width: u32, height: u32, cmyk: &[u8], quality: u8) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    debug_assert_eq!(cmyk.len(), w * h * 4);
    let q = i32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let quant: [u16; 64] =
        LUMA_QUANT.map(|b| ((i32::from(b) * scale + 50) / 100).clamp(1, 255) as u16);

    let mut out = vec![0xFF, 0xD8];
    // APP14 Adobe: version 100, flags 0, transform 0 (no colour transform).
    out.extend_from_slice(&[0xFF, 0xEE, 0, 14]);
    out.extend_from_slice(b"Adobe");
    out.extend_from_slice(&[0, 100, 0, 0, 0, 0, 0]);
    // DQT, table 0, zig-zag order.
    out.extend_from_slice(&[0xFF, 0xDB, 0, 67, 0]);
    out.extend(ZIGZAG.iter().map(|&i| quant[i] as u8));
    // SOF0: 8-bit, four components, 1x1 sampling, quant table 0.
    out.extend_from_slice(&[0xFF, 0xC0, 0, 20, 8]);
    out.extend_from_slice(&(height as u16).to_be_bytes());
    out.extend_from_slice(&(width as u16).to_be_bytes());
    out.push(4);
    for id in 1..=4u8 {
        out.extend_from_slice(&[id, 0x11, 0]);
    }
    // DHT: DC table 0 and AC table 0.
    let dht = |out: &mut Vec<u8>, class_id: u8, bits: &[u8; 16], vals: &[u8]| {
        let len = 2 + 1 + 16 + vals.len();
        out.extend_from_slice(&[0xFF, 0xC4]);
        out.extend_from_slice(&(len as u16).to_be_bytes());
        out.push(class_id);
        out.extend_from_slice(bits);
        out.extend_from_slice(vals);
    };
    dht(&mut out, 0x00, &DC_BITS, &DC_VALS);
    dht(&mut out, 0x10, &AC_BITS, &AC_VALS);
    // SOS: four components, all on tables 0/0, full spectral range.
    out.extend_from_slice(&[0xFF, 0xDA, 0, 14, 4]);
    for id in 1..=4u8 {
        out.extend_from_slice(&[id, 0x00]);
    }
    out.extend_from_slice(&[0, 63, 0]);

    let dc = huffman_codes(&DC_BITS, &DC_VALS);
    let ac = huffman_codes(&AC_BITS, &AC_VALS);
    let mut bits = BitWriter {
        out: Vec::new(),
        acc: 0,
        n: 0,
    };
    let mut pred = [0i32; 4];
    let blocks_x = w.div_ceil(8);
    let blocks_y = h.div_ceil(8);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            for (comp, pred) in pred.iter_mut().enumerate() {
                let mut block = [0.0f32; 64];
                for y in 0..8 {
                    for x in 0..8 {
                        // Edge replication past the right and bottom edges.
                        let sx = (bx * 8 + x).min(w - 1);
                        let sy = (by * 8 + y).min(h - 1);
                        // Stored inverted: 255 is no ink.
                        let ink = cmyk[(sy * w + sx) * 4 + comp];
                        block[y * 8 + x] = f32::from(255 - ink) - 128.0;
                    }
                }
                let coeffs = fdct(&block);
                let mut zz = [0i32; 64];
                for (k, &natural) in ZIGZAG.iter().enumerate() {
                    zz[k] = (coeffs[natural] / f32::from(quant[natural])).round() as i32;
                }
                let diff = zz[0] - *pred;
                *pred = zz[0];
                let (size, value) = category(diff);
                let (code, len) = dc[usize::from(size)];
                bits.put(code, len);
                if size > 0 {
                    bits.put(value, size);
                }
                let mut run = 0u8;
                for &v in &zz[1..] {
                    if v == 0 {
                        run += 1;
                        continue;
                    }
                    while run >= 16 {
                        let (code, len) = ac[0xF0];
                        bits.put(code, len);
                        run -= 16;
                    }
                    let (size, value) = category(v);
                    let (code, len) = ac[usize::from((run << 4) | size)];
                    bits.put(code, len);
                    bits.put(value, size);
                    run = 0;
                }
                if run > 0 {
                    let (code, len) = ac[0x00];
                    bits.put(code, len);
                }
            }
        }
    }
    out.extend_from_slice(&bits.finish());
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

// ------------------------------------------------------------------ TIFF ---

/// Encode CMYK8 samples as a baseline, uncompressed, separated TIFF.
pub fn encode_cmyk_tiff(width: u32, height: u32, cmyk: &[u8]) -> Vec<u8> {
    let data_len = cmyk.len() as u32;
    let data_at = 8u32;
    let bps_at = data_at + data_len;
    let xres_at = bps_at + 8;
    let yres_at = xres_at + 8;
    let ifd_at = yres_at + 8;
    let mut out = Vec::with_capacity(ifd_at as usize + 200);
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&ifd_at.to_le_bytes());
    out.extend_from_slice(cmyk);
    for _ in 0..4 {
        out.extend_from_slice(&8u16.to_le_bytes());
    }
    for _ in 0..2 {
        out.extend_from_slice(&72u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    // (tag, type, count, value-or-offset); SHORT = 3, LONG = 4, RATIONAL = 5.
    let entries: [(u16, u16, u32, u32); 14] = [
        (256, 4, 1, width),
        (257, 4, 1, height),
        (258, 3, 4, bps_at),
        (259, 3, 1, 1),
        (262, 3, 1, 5),
        (273, 4, 1, data_at),
        (277, 3, 1, 4),
        (278, 4, 1, height),
        (279, 4, 1, data_len),
        (282, 5, 1, xres_at),
        (283, 5, 1, yres_at),
        (284, 3, 1, 1),
        (296, 3, 1, 2),
        (332, 3, 1, 1),
    ];
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, ty, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&ty.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        if ty == 3 && count == 1 {
            // A single SHORT sits in the first two bytes of the field.
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        } else {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

// ------------------------------------------------------------------- PNG ---

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for bytes in chunks {
        for &b in *bytes {
            crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    crc ^ 0xFFFF_FFFF
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Alpha at or above which a pixel is opaque in a re-quantised palette PNG
/// (Photoshop's Indexed Color keeps 1-bit transparency).
pub const INDEXED_ALPHA_THRESHOLD: u8 = 128;

/// Encode straight RGBA8 as a palette PNG (colour type 3). Always succeeds:
///
/// * When the image has at most 256 distinct RGBA colours the palette is
///   exactly those colours in first-seen order (lossless), with `tRNS`
///   carrying alpha only when some entry is not opaque.
/// * Otherwise — typically a soft-edged stroke painted after Image > Mode >
///   Indexed Color, whose antialiased alpha multiplies the palette — the
///   image is re-quantised the way Photoshop's Indexed mode stores it:
///   alpha is thresholded to 1 bit at [`INDEXED_ALPHA_THRESHOLD`] (one fully
///   transparent entry, index 0), and the opaque colours become the image's
///   own colours when they fit, or an adaptive (median-cut) palette of the
///   remaining entries otherwise, each pixel mapped to its nearest entry.
pub fn encode_indexed_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let (palette, indices) = exact_palette(rgba).unwrap_or_else(|| thresholded_palette(rgba));
    let w = (width as usize).max(1);
    let mut raw = Vec::with_capacity((w + 1) * height as usize);
    for (i, &at) in indices.iter().enumerate() {
        if i % w == 0 {
            raw.push(0); // filter: none
        }
        raw.push(at);
    }
    let mut palette = palette;
    if palette.is_empty() {
        palette.push([0, 0, 0, 0]);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing into a `Vec` cannot fail.
    let idat = z
        .write_all(&raw)
        .and_then(|()| z.finish())
        .unwrap_or_default();

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    let plte: Vec<u8> = palette.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    png_chunk(&mut out, b"PLTE", &plte);
    if let Some(last) = palette.iter().rposition(|p| p[3] != 255) {
        // Entries past the end of `tRNS` are opaque.
        let trns: Vec<u8> = palette[..=last].iter().map(|p| p[3]).collect();
        png_chunk(&mut out, b"tRNS", &trns);
    }
    png_chunk(&mut out, b"IDAT", &idat);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// The image's own RGBA colours as a palette plus one index per pixel, or
/// `None` when there are more than 256 of them.
fn exact_palette(rgba: &[u8]) -> Option<(Vec<[u8; 4]>, Vec<u8>)> {
    let mut index: HashMap<[u8; 4], u8> = HashMap::new();
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut indices = Vec::with_capacity(rgba.len() / 4);
    for px in rgba.as_chunks::<4>().0 {
        let at = match index.get(px) {
            Some(&at) => at,
            None => {
                if palette.len() == 256 {
                    return None;
                }
                let at = palette.len() as u8;
                palette.push(*px);
                index.insert(*px, at);
                at
            }
        };
        indices.push(at);
    }
    Some((palette, indices))
}

/// Photoshop-style re-quantisation: 1-bit alpha, at most 256 entries.
fn thresholded_palette(rgba: &[u8]) -> (Vec<[u8; 4]>, Vec<u8>) {
    use color::quantize::{build_palette, nearest, Histogram, PaletteKind, MAX_COLORS};
    let pixels = rgba.as_chunks::<4>().0;
    let opaque = |px: &[u8; 4]| px[3] >= INDEXED_ALPHA_THRESHOLD;
    let transparent = pixels.iter().any(|px| !opaque(px));
    let mut histogram = Histogram::new();
    let visible: Vec<u8> = pixels
        .iter()
        .filter(|px| opaque(px))
        .flat_map(|px| [px[0], px[1], px[2], 255])
        .collect();
    histogram.add_rgba8(&visible);
    let slots = MAX_COLORS - u16::from(transparent);
    let colours: Vec<[u8; 3]> = if histogram.distinct() == 0 {
        Vec::new()
    } else {
        build_palette(&histogram, PaletteKind::Exact, slots)
            .or_else(|_| build_palette(&histogram, PaletteKind::Adaptive, slots))
            .unwrap_or_default()
    };
    let base = u8::from(transparent);
    let mut palette: Vec<[u8; 4]> = Vec::with_capacity(colours.len() + 1);
    if transparent {
        palette.push([0, 0, 0, 0]);
    }
    palette.extend(colours.iter().map(|c| [c[0], c[1], c[2], 255]));
    let mut cache: HashMap<[u8; 3], u8> = HashMap::new();
    let indices = pixels
        .iter()
        .map(|px| {
            if !opaque(px) || colours.is_empty() {
                return 0;
            }
            let rgb = [px[0], px[1], px[2]];
            *cache
                .entry(rgb)
                .or_insert_with(|| base + nearest(&colours, rgb.map(i32::from)) as u8)
        })
        .collect();
    (palette, indices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageDecoder;

    /// The SOF0 component count and whether an Adobe APP14 marker is present.
    fn jpeg_components(bytes: &[u8]) -> (u8, bool) {
        let mut i = 2;
        let mut adobe = false;
        while i + 4 < bytes.len() {
            assert_eq!(bytes[i], 0xFF, "marker expected at {i}");
            let marker = bytes[i + 1];
            let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
            if marker == 0xEE && &bytes[i + 4..i + 9] == b"Adobe" {
                adobe = true;
            }
            if marker == 0xC0 {
                return (bytes[i + 9], adobe);
            }
            i += 2 + len;
        }
        panic!("no SOF0");
    }

    #[test]
    fn a_cmyk_jpeg_has_four_components_and_decodes_to_the_proofed_colour() {
        // A red and a mid-grey half, 16x8.
        let mut rgba = Vec::new();
        for _y in 0..8 {
            for x in 0..16 {
                rgba.extend_from_slice(if x < 8 {
                    &[255, 0, 0, 255]
                } else {
                    &[128, 128, 128, 255]
                });
            }
        }
        let cmyk = separate_rgba8(&rgba, [255; 3]);
        let bytes = encode_cmyk_jpeg(16, 8, &cmyk, 95);
        assert_eq!(jpeg_components(&bytes), (4, true));
        // A real decoder reads it, as colour. `image` composes CMYK back
        // naively (`(1 - ink) * (1 - k)`), so that is what the inks written
        // must come back as — which pins the inverted-sample convention: the
        // other convention decodes red as cyan.
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
            .expect("decodes")
            .to_rgb8();
        let red = decoded.get_pixel(3, 3).0;
        let grey = decoded.get_pixel(12, 3).0;
        let inks = &cmyk[..4];
        let naive: [u8; 3] = std::array::from_fn(|c| {
            ((255 - u32::from(inks[c])) * (255 - u32::from(inks[3])) / 255) as u8
        });
        for c in 0..3 {
            assert!(red[c].abs_diff(naive[c]) <= 8, "red {red:?} vs {naive:?}");
            assert!(grey[c].abs_diff(128) <= 8, "grey {grey:?}");
        }
        assert!(red[0] > 200 && red[1] < 40 && red[2] < 40, "red {red:?}");
    }

    #[test]
    fn a_cmyk_tiff_decodes_as_cmyk8() {
        let rgba = [0u8, 255, 0, 255, 128, 128, 128, 255].repeat(2);
        let cmyk = separate_rgba8(&rgba, [255; 3]);
        let bytes = encode_cmyk_tiff(2, 2, &cmyk);
        let decoder =
            image::codecs::tiff::TiffDecoder::new(std::io::Cursor::new(&bytes)).expect("parses");
        assert_eq!(
            decoder.original_color_type(),
            image::ExtendedColorType::Cmyk8
        );
        assert_eq!(decoder.dimensions(), (2, 2));
    }

    #[test]
    fn an_indexed_png_is_colour_type_3_and_round_trips() {
        let rgba = [
            10u8, 20, 30, 255, 200, 100, 0, 255, 10, 20, 30, 255, 0, 0, 0, 0,
        ];
        let bytes = encode_indexed_png(2, 2, &rgba);
        // IHDR: bit depth 8, colour type 3.
        assert_eq!(&bytes[12..16], b"IHDR");
        assert_eq!((bytes[24], bytes[25]), (8, 3));
        let back = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(back.as_raw().as_slice(), &rgba);
    }

    #[test]
    fn the_exporter_writes_the_layout_the_preset_ink_asks_for() {
        use crate::export::{export, linear_from_rgba8, ExportInk, ExportMetadata, ExportPreset};
        use crate::ExportFormat;
        let rgba = [0u8, 200, 30, 255, 90, 90, 90, 255].repeat(8);
        let image = linear_from_rgba8(4, 4, &rgba, &color::ColorSpace::Srgb).unwrap();
        let meta = ExportMetadata::default();
        let cmyk_jpeg = export(
            &image,
            &ExportPreset::new("c", ExportFormat::Jpeg(90)).with_ink(ExportInk::Cmyk),
            &meta,
        )
        .unwrap();
        assert_eq!(jpeg_components(&cmyk_jpeg.bytes), (4, true));
        let rgb_jpeg = export(
            &image,
            &ExportPreset::new("r", ExportFormat::Jpeg(90)),
            &meta,
        )
        .unwrap();
        assert_eq!(
            jpeg_components(&rgb_jpeg.bytes).0,
            3,
            "an RGB preset stays RGB"
        );
        let cmyk_tiff = export(
            &image,
            &ExportPreset::new("t", ExportFormat::Tiff).with_ink(ExportInk::Cmyk),
            &meta,
        )
        .unwrap();
        let decoder =
            image::codecs::tiff::TiffDecoder::new(std::io::Cursor::new(&cmyk_tiff.bytes)).unwrap();
        assert_eq!(
            decoder.original_color_type(),
            image::ExtendedColorType::Cmyk8
        );
        let png8 = export(
            &image,
            &ExportPreset::new("p", ExportFormat::Png).with_ink(ExportInk::Indexed),
            &meta,
        )
        .unwrap();
        assert_eq!(png8.bytes[25], 3, "colour type 3");
        // A container that cannot carry CMYK is written as RGB.
        assert!(!ExportInk::Cmyk.carried_by(ExportFormat::Png));
        let png = export(
            &image,
            &ExportPreset::new("q", ExportFormat::Png).with_ink(ExportInk::Cmyk),
            &meta,
        )
        .unwrap();
        assert_eq!(png.bytes[25], 6, "RGBA PNG");
        assert_eq!(ExportInk::for_color_mode(3), ExportInk::Cmyk);
        assert_eq!(ExportInk::for_color_mode(4), ExportInk::Indexed);
        assert_eq!(ExportInk::for_color_mode(0), ExportInk::Rgb);
    }

    /// The palette entry count and the `tRNS` length of a palette PNG.
    fn png_palette(bytes: &[u8]) -> (usize, Option<usize>) {
        let mut i = 8;
        let (mut plte, mut trns) = (0, None);
        while i + 8 <= bytes.len() {
            let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
            match &bytes[i + 4..i + 8] {
                b"PLTE" => plte = len / 3,
                b"tRNS" => trns = Some(len),
                _ => {}
            }
            i += 12 + len;
        }
        (plte, trns)
    }

    #[test]
    fn more_than_256_colours_is_requantised_with_one_bit_alpha() {
        // 300 opaque colours: an adaptive palette of at most 256.
        let rgba: Vec<u8> = (0..300u32)
            .flat_map(|i| [(i % 256) as u8, (i / 256) as u8 * 200, 0, 255])
            .collect();
        let bytes = encode_indexed_png(300, 1, &rgba);
        assert_eq!(bytes[25], 3, "colour type 3");
        let (entries, trns) = png_palette(&bytes);
        assert!(entries <= 256, "{entries} entries");
        assert_eq!(trns, None, "an all-opaque image has no tRNS");
        let back = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        for (want, got) in rgba.as_chunks::<4>().0.iter().zip(back.pixels()) {
            for c in 0..3 {
                assert!(want[c].abs_diff(got.0[c]) <= 16, "{want:?} vs {got:?}");
            }
            assert_eq!(got.0[3], 255);
        }
    }

    #[test]
    fn a_soft_alpha_stroke_over_a_palette_still_writes_a_palette_png() {
        // Four palette colours crossed with every alpha step of an
        // antialiased edge: 4 x 256 distinct RGBA values.
        let palette = [
            [200u8, 30, 30],
            [30, 200, 30],
            [30, 30, 200],
            [250, 250, 250],
        ];
        let rgba: Vec<u8> = (0..1024u32)
            .flat_map(|i| {
                let p = palette[(i % 4) as usize];
                [p[0], p[1], p[2], (i / 4) as u8]
            })
            .collect();
        let bytes = encode_indexed_png(64, 16, &rgba);
        assert_eq!(bytes[25], 3, "colour type 3");
        let (entries, trns) = png_palette(&bytes);
        assert_eq!(entries, 5, "the four colours plus one transparent entry");
        assert_eq!(trns, Some(1), "only the transparent entry is in tRNS");
        let back = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        for (want, got) in rgba.as_chunks::<4>().0.iter().zip(back.pixels()) {
            if want[3] >= INDEXED_ALPHA_THRESHOLD {
                assert_eq!(got.0, [want[0], want[1], want[2], 255]);
            } else {
                assert_eq!(got.0[3], 0, "{want:?} -> {got:?}");
            }
        }
    }
}
