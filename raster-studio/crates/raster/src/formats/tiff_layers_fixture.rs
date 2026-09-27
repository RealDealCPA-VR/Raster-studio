//! W18-H: a layered TIFF built by hand, for the tests of
//! `tiff_layers_w18` here and of the app-shell open route (which includes
//! this file with `#[path]`). No crate-internal dependencies.

/// Integers in the file's byte order.
struct W {
    le: bool,
    out: Vec<u8>,
}

impl W {
    fn u16(&mut self, v: u16) {
        let b = if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        self.out.extend_from_slice(&b);
    }
    fn u32(&mut self, v: u32) {
        let b = if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        self.out.extend_from_slice(&b);
    }
    fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }
    /// A four-character code, reversed in a little-endian file.
    fn code(&mut self, c: &[u8; 4]) {
        if self.le {
            self.out.extend(c.iter().rev());
        } else {
            self.out.extend_from_slice(c);
        }
    }
}

/// The canvas: 4 x 2.
pub const WIDTH: u32 = 4;
pub const HEIGHT: u32 = 2;

/// The layer data (`Layr` block payload): "Background" (4 x 2 opaque red,
/// raw channels) and "Blue dot" (2 x 1 blue at (1, 0), opacity 128, RLE
/// channels, with a `luni` name "Blue dot").
fn layer_info(le: bool) -> Vec<u8> {
    let mut w = W {
        le,
        out: Vec::new(),
    };
    w.u16(2);
    // (name, rect top/left/bottom/right, opacity, planes per channel -1,0,1,2, rle)
    type Spec<'a> = (&'a str, [i32; 4], u8, [u8; 4], bool);
    let layers: [Spec<'_>; 2] = [
        ("Background", [0, 0, 2, 4], 255, [255, 255, 0, 0], false),
        ("Blue dot", [0, 1, 1, 3], 128, [255, 0, 0, 255], true),
    ];
    let mut data = Vec::new();
    for (name, rect, opacity, px, rle) in layers {
        let n = ((rect[2] - rect[0]) * (rect[3] - rect[1])) as usize;
        let rows = (rect[2] - rect[0]) as usize;
        let cols = (rect[3] - rect[1]) as usize;
        for v in rect {
            w.i32(v);
        }
        w.u16(4);
        let mut channel_bytes = Vec::new();
        for (id, value) in [-1i16, 0, 1, 2].iter().zip(px) {
            let mut c = W {
                le,
                out: Vec::new(),
            };
            if rle {
                c.u16(1);
                // One packbits run per row: repeat `value` `cols` times.
                for _ in 0..rows {
                    c.u16(2);
                }
                for _ in 0..rows {
                    c.out.push((257 - cols) as u8);
                    c.out.push(value);
                }
            } else {
                c.u16(0);
                c.out.extend(std::iter::repeat_n(value, n));
            }
            w.u16(*id as u16);
            w.u32(c.out.len() as u32);
            channel_bytes.push(c.out);
        }
        w.code(b"8BIM");
        w.code(b"norm");
        w.out.extend_from_slice(&[opacity, 0, 0, 0]);
        let mut extra = W {
            le,
            out: Vec::new(),
        };
        extra.u32(0); // mask
        extra.u32(0); // blending ranges
        let mut pascal = vec![name.len() as u8];
        pascal.extend_from_slice(name.as_bytes());
        while !pascal.len().is_multiple_of(4) {
            pascal.push(0);
        }
        extra.out.extend(pascal);
        extra.code(b"8BIM");
        extra.code(b"luni");
        let units: Vec<u16> = name.encode_utf16().collect();
        extra.u32(4 + 2 * units.len() as u32);
        extra.u32(units.len() as u32);
        for u in units {
            extra.u16(u);
        }
        // A block the little-endian conversion does not carry.
        extra.code(b"8BIM");
        extra.code(b"lyid");
        extra.u32(4);
        extra.u32(7);
        w.u32(extra.out.len() as u32);
        w.out.extend(extra.out);
        data.extend(channel_bytes.concat());
    }
    w.out.extend(data);
    w.out
}

/// The composite, RGB: red, then blue blended at half over red in (1, 0)
/// and (2, 0).
pub fn composite() -> Vec<u8> {
    let mut rgb = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if y == 0 && (1..3).contains(&x) {
                rgb.extend_from_slice(&[127, 0, 128]);
            } else {
                rgb.extend_from_slice(&[255, 0, 0]);
            }
        }
    }
    rgb
}

/// A layered TIFF in either byte order; `bits` is the BitsPerSample it
/// declares (the strip always holds 8-bit samples).
pub fn layered_tiff(le: bool, bits: u16) -> Vec<u8> {
    let mut w = W {
        le,
        out: Vec::new(),
    };
    w.out.extend_from_slice(if le { b"II" } else { b"MM" });
    w.u16(42);
    w.u32(8);
    let strip = composite();
    let mut source = b"Adobe Photoshop Document Data Block\0".to_vec();
    let info = layer_info(le);
    let mut block = W {
        le,
        out: Vec::new(),
    };
    block.code(b"8BIM");
    block.code(b"Layr");
    block.u32(info.len() as u32);
    block.out.extend(info);
    while !block.out.len().is_multiple_of(4) {
        block.out.push(0);
    }
    source.extend(block.out);
    // IFD at 8: 11 entries, then the next-IFD offset; data after.
    let entries = 11u32;
    let data_at = 8 + 2 + entries * 12 + 4;
    let bps_at = data_at;
    let strip_at = bps_at + 6;
    let source_at = strip_at + strip.len() as u32;
    w.u16(entries as u16);
    let entry = |w: &mut W, tag: u16, kind: u16, count: u32, value: u32| {
        w.u16(tag);
        w.u16(kind);
        w.u32(count);
        if kind == 3 && count == 1 {
            w.u16(value as u16);
            w.u16(0);
        } else {
            w.u32(value);
        }
    };
    entry(&mut w, 256, 3, 1, WIDTH);
    entry(&mut w, 257, 3, 1, HEIGHT);
    entry(&mut w, 258, 3, 3, bps_at);
    entry(&mut w, 259, 3, 1, 1);
    entry(&mut w, 262, 3, 1, 2);
    entry(&mut w, 273, 4, 1, strip_at);
    entry(&mut w, 277, 3, 1, 3);
    entry(&mut w, 278, 3, 1, HEIGHT);
    entry(&mut w, 279, 4, 1, strip.len() as u32);
    entry(&mut w, 284, 3, 1, 1);
    entry(&mut w, 37724, 7, source.len() as u32, source_at);
    w.u32(0);
    for _ in 0..3 {
        w.u16(bits);
    }
    w.out.extend(strip);
    w.out.extend(source);
    w.out
}
