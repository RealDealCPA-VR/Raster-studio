//! W18-H: a **layered TIFF** — the Photoshop layers a TIFF carries in its
//! `ImageSourceData` tag (37724) — handed to the application as a `.psd`.
//!
//! Photoshop's "Save as TIFF" with layers writes, besides the flattened
//! image every TIFF reader shows, the tag 37724: the text
//! `Adobe Photoshop Document Data Block` and a NUL, then `8BIM` blocks. The
//! `Layr` block holds the layers exactly as a `.psd`'s layer info section
//! does (count, records, channel data). [`psd_bytes`] rebuilds a `.psd` from
//! it — the TIFF's own composite as the merged image, the `Layr` payload as
//! the layer info — so the application opens it through its `.psd` reader
//! (every layer's name, bounds, opacity, blend mode, visibility and pixels,
//! and in a big-endian file every additional layer record the `.psd` reader
//! knows). The app-shell open route (`import_layered_w16`) asks
//! [`has_layers`] and opens the result; File > Revert does the same.
//!
//! # Little-endian files
//!
//! A TIFF in Intel byte order (`II`, Photoshop's default on Windows) writes
//! the block in that order too: signatures and keys reversed (`MIB8`,
//! `ryaL`), every integer little-endian. The records are converted field by
//! field: the rectangle, channels, blend mode, opacity, flags, name and the
//! Unicode name (`luni`) are carried; a layer mask (with its channel),
//! blending ranges and every other additional record (effects, adjustment
//! and type data, whose descriptors this build cannot re-order) are left
//! out, and [`LayeredTiff::notes`] says so. RLE row counts are re-ordered;
//! raw and ZIP 8-bit channel data are the same in either order.
//!
//! # What is not read
//!
//! A TIFF whose samples are not 8-bit RGB (16- or 32-bit, greyscale, CMYK,
//! Lab) or a BigTIFF keeps opening as its flattened image, and
//! [`has_layers`] says `false` for it: the `.psd` this builds is 8-bit RGB.
//!
//! # Untrusted input
//!
//! Every IFD entry, block and record is read through bounds-checked reads;
//! counts (layers, channels, blocks) are bounded before they are walked; the
//! output only ever copies bytes of the input, so its size is bounded by the
//! file's.

use crate::codec::{CodecError, ImportFormat, ImportLimits};

const NAME: &str = "layered TIFF";
const TAG_IMAGE_SOURCE_DATA: u16 = 37724;
const MAGIC: &[u8] = b"Adobe Photoshop Document Data Block\0";
/// More layer records than a `.psd` can address.
const MAX_LAYERS: usize = 16_384;
/// More `8BIM` blocks than a real data block holds.
const MAX_BLOCKS: usize = 4096;

fn malformed(what: impl std::fmt::Display) -> CodecError {
    super::malformed(NAME, what)
}

/// The layers a TIFF carries, as a `.psd`.
#[derive(Debug, Clone, PartialEq)]
pub struct LayeredTiff {
    /// A version-1 `.psd`, 8-bit RGB: the TIFF's composite and its layers.
    pub psd: Vec<u8>,
    /// How many layer records it holds (group dividers count).
    pub records: usize,
    /// What the conversion left out, one sentence each.
    pub notes: Vec<String>,
}

/// A bounds-checked reader in the file's byte order.
struct R<'a> {
    b: &'a [u8],
    pos: usize,
    le: bool,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or_else(|| malformed("the layer data ends early"))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        let s = self.take(2)?;
        let a = [s[0], s[1]];
        Ok(if self.le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        let s = self.take(4)?;
        let a = [s[0], s[1], s[2], s[3]];
        Ok(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }
    /// A four-character code, un-reversed.
    fn code(&mut self) -> Result<[u8; 4], CodecError> {
        let s = self.take(4)?;
        Ok(if self.le {
            [s[3], s[2], s[1], s[0]]
        } else {
            [s[0], s[1], s[2], s[3]]
        })
    }
}

/// What the TIFF's first IFD says: the byte order, the 37724 payload, and
/// whether its samples are 8-bit RGB.
struct Ifd<'a> {
    le: bool,
    source: Option<&'a [u8]>,
    rgb8: bool,
}

fn ifd(bytes: &[u8]) -> Result<Ifd<'_>, CodecError> {
    let le = match bytes.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return Err(malformed("not a classic TIFF")),
    };
    let mut r = R {
        b: bytes,
        pos: 4,
        le,
    };
    r.pos = r.u32()? as usize;
    let count = usize::from(r.u16()?);
    let mut source = None;
    let (mut bits_ok, mut spp, mut photometric) = (false, 1u32, u32::MAX);
    for _ in 0..count {
        let tag = r.u16()?;
        let kind = r.u16()?;
        let n = r.u32()? as usize;
        let field = r.take(4)?;
        let mut f = R {
            b: field,
            pos: 0,
            le,
        };
        let small = |f: &mut R<'_>| -> Result<u32, CodecError> {
            if kind == 3 {
                f.u16().map(u32::from)
            } else {
                f.u32()
            }
        };
        match tag {
            258 => {
                // BitsPerSample: every sample 8.
                bits_ok = if n <= 2 && kind == 3 {
                    (0..n).all(|_| f.u16().is_ok_and(|v| v == 8))
                } else if kind == 3 {
                    let at = f.u32()? as usize;
                    let mut a = R {
                        b: bytes,
                        pos: at,
                        le,
                    };
                    let mut ok = true;
                    for _ in 0..n.min(16) {
                        ok &= a.u16()? == 8;
                    }
                    ok
                } else {
                    false
                };
            }
            262 => photometric = small(&mut f)?,
            277 => spp = small(&mut f)?,
            TAG_IMAGE_SOURCE_DATA if matches!(kind, 1 | 7) => {
                source = if n <= 4 {
                    Some(&field[..n])
                } else {
                    let at = f.u32()? as usize;
                    Some(
                        at.checked_add(n)
                            .and_then(|end| bytes.get(at..end))
                            .ok_or_else(|| malformed("tag 37724 runs past the file"))?,
                    )
                };
            }
            _ => {}
        }
    }
    Ok(Ifd {
        le,
        source,
        rgb8: bits_ok && photometric == 2 && (3..=4).contains(&spp),
    })
}

/// The `Layr` block's payload inside a 37724 data block.
fn layr_block(source: &[u8], le: bool) -> Result<Option<&[u8]>, CodecError> {
    let Some(mut rest) = source.strip_prefix(MAGIC) else {
        return Err(malformed("tag 37724 is not a Photoshop data block"));
    };
    for _ in 0..MAX_BLOCKS {
        if rest.len() < 12 {
            return Ok(None);
        }
        let mut r = R {
            b: rest,
            pos: 0,
            le,
        };
        let sig = r.code()?;
        if &sig != b"8BIM" && &sig != b"8B64" {
            return Err(malformed("a data block has no 8BIM signature"));
        }
        let key = r.code()?;
        let len = r.u32()? as usize;
        let data = r.take(len)?;
        if &key == b"Layr" {
            return Ok(Some(data));
        }
        // The next block, past up to three bytes of padding.
        let mut next = r.pos;
        let sigs: [&[u8]; 2] = [b"8BIM", b"MIB8"];
        while next < rest.len()
            && next < r.pos + 4
            && !sigs.iter().any(|s| rest[next..].starts_with(s))
        {
            next += 1;
        }
        rest = &rest[next.min(rest.len())..];
    }
    Ok(None)
}

/// Whether `bytes` is a TIFF whose layers [`psd_bytes`] can hand over.
pub fn has_layers(bytes: &[u8]) -> bool {
    matches!(ifd(bytes), Ok(Ifd { source: Some(s), rgb8: true, le }) if matches!(layr_block(s, le), Ok(Some(_))))
}

/// A big-endian writer.
#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
}

/// One record's channel list: `(id, data length)`.
type Channels = Vec<(i16, u32)>;

/// Rewrite a little-endian layer info payload in the `.psd`'s big-endian
/// order (see the module docs), noting what was left out.
fn big_endian_layer_info(data: &[u8], notes: &mut Vec<String>) -> Result<Vec<u8>, CodecError> {
    let mut r = R {
        b: data,
        pos: 0,
        le: true,
    };
    let count = r.u16()? as i16;
    let n = usize::from(count.unsigned_abs());
    if n > MAX_LAYERS {
        return Err(CodecError::LimitExceeded(format!(
            "the TIFF lists {n} layers, more than {MAX_LAYERS}"
        )));
    }
    let mut out = W::default();
    out.u16(count as u16);
    let mut plan: Vec<(Channels, Vec<bool>, u32)> = Vec::with_capacity(n);
    let (mut masks, mut ranges, mut others) = (0usize, 0usize, Vec::<String>::new());
    for _ in 0..n {
        let rect: Vec<u32> = (0..4).map(|_| r.u32()).collect::<Result<_, _>>()?;
        let rows = (rect[2] as i32).saturating_sub(rect[0] as i32).max(0) as u32;
        let channels = usize::from(r.u16()?);
        if channels > 56 {
            return Err(malformed(format!("a layer has {channels} channels")));
        }
        let list: Channels = (0..channels)
            .map(|_| Ok((r.u16()? as i16, r.u32()?)))
            .collect::<Result<_, CodecError>>()?;
        // A mask channel (-2, -3) is left out with its mask.
        let keep: Vec<bool> = list.iter().map(|(id, _)| *id >= -1).collect();
        masks += keep.iter().filter(|k| !**k).count();
        let sig = r.code()?;
        if &sig != b"8BIM" {
            return Err(malformed("a layer record has no blend signature"));
        }
        let blend = r.code()?;
        let fixed = r.take(4)?;
        let extra_len = r.u32()? as usize;
        let extra = r.take(extra_len)?;
        let mut e = R {
            b: extra,
            pos: 0,
            le: true,
        };
        let mask_len = e.u32()? as usize;
        e.take(mask_len)?;
        let ranges_len = e.u32()? as usize;
        if ranges_len > 0 {
            ranges += 1;
        }
        e.take(ranges_len)?;
        let name_len = usize::from(*e.take(1)?.first().unwrap_or(&0));
        let name = e.take(name_len)?;
        let pad = (4 - (1 + name_len) % 4) % 4;
        e.take(pad.min(e.b.len() - e.pos))?;
        let mut luni: Option<Vec<u16>> = None;
        while e.b.len() - e.pos >= 12 {
            let bsig = e.code()?;
            if &bsig != b"8BIM" && &bsig != b"8B64" {
                break;
            }
            let key = e.code()?;
            let len = e.u32()? as usize;
            let body = e.take(len.min(e.b.len() - e.pos))?;
            if &key == b"luni" {
                let mut u = R {
                    b: body,
                    pos: 0,
                    le: true,
                };
                let chars = (u.u32()? as usize).min(body.len() / 2);
                luni = Some((0..chars).map(|_| u.u16()).collect::<Result<_, _>>()?);
            } else {
                let k = String::from_utf8_lossy(&key).into_owned();
                if !others.contains(&k) {
                    others.push(k);
                }
            }
        }
        // The record, big-endian.
        for v in &rect {
            out.u32(*v);
        }
        let kept: Channels = list
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(c, _)| *c)
            .collect();
        out.u16(kept.len() as u16);
        for (id, len) in &kept {
            out.u16(*id as u16);
            out.u32(*len);
        }
        out.0.extend_from_slice(b"8BIM");
        out.0.extend_from_slice(&blend);
        out.0.extend_from_slice(fixed);
        let mut x = W::default();
        x.u32(0);
        x.u32(0);
        x.0.push(name_len as u8);
        x.0.extend_from_slice(name);
        x.0.resize(x.0.len() + pad, 0);
        if let Some(units) = luni {
            x.0.extend_from_slice(b"8BIMluni");
            x.u32(4 + 2 * units.len() as u32);
            x.u32(units.len() as u32);
            for u in units {
                x.u16(u);
            }
        }
        out.u32(x.0.len() as u32);
        out.0.extend(x.0);
        plan.push((list, keep, rows));
    }
    // Channel image data, in record order.
    for (list, keep, rows) in plan {
        for ((_, len), keep) in list.iter().zip(keep) {
            let bytes = r.take(*len as usize)?;
            if !keep {
                continue;
            }
            if bytes.len() < 2 {
                return Err(malformed("a channel has no compression code"));
            }
            let compression = u16::from_le_bytes([bytes[0], bytes[1]]);
            out.u16(compression);
            let body = &bytes[2..];
            if compression == 1 {
                // RLE: a row-count table, then PackBits (byte data).
                let table = (rows as usize) * 2;
                if body.len() < table {
                    return Err(malformed("an RLE channel's row table is short"));
                }
                for c in body[..table].as_chunks::<2>().0 {
                    out.0.extend_from_slice(&[c[1], c[0]]);
                }
                out.0.extend_from_slice(&body[table..]);
            } else {
                out.0.extend_from_slice(body);
            }
        }
    }
    if masks > 0 {
        notes.push(format!(
            "{masks} layer mask channel(s) of this little-endian TIFF were left out"
        ));
    }
    if ranges > 0 {
        notes.push(format!(
            "the blending ranges of {ranges} layer(s) of this little-endian TIFF were left out"
        ));
    }
    if !others.is_empty() {
        notes.push(format!(
            "layer records this little-endian TIFF stores in its own byte order were left out \
             ({})",
            others.join(", ")
        ));
    }
    Ok(out.0)
}

/// The layers `bytes` (a TIFF) carries, as a `.psd` (see the module docs).
pub fn psd_bytes(bytes: &[u8], limits: ImportLimits) -> Result<LayeredTiff, CodecError> {
    let parsed = ifd(bytes)?;
    let source = parsed
        .source
        .ok_or_else(|| malformed("the TIFF has no Photoshop layer data (tag 37724)"))?;
    if !parsed.rgb8 {
        return Err(CodecError::Unsupported(
            "the TIFF's layers are read from 8-bit RGB files only".into(),
        ));
    }
    let layr = layr_block(source, parsed.le)?
        .ok_or_else(|| malformed("the Photoshop data block has no Layr block"))?;
    if layr.len() < 2 {
        return Err(malformed("the Layr block is empty"));
    }
    let mut notes = Vec::new();
    let info = if parsed.le {
        big_endian_layer_info(layr, &mut notes)?
    } else {
        layr.to_vec()
    };
    let count = i16::from_be_bytes([info[0], info[1]]);
    let records = usize::from(count.unsigned_abs());
    if records > MAX_LAYERS {
        return Err(CodecError::LimitExceeded(format!(
            "the TIFF lists {records} layers, more than {MAX_LAYERS}"
        )));
    }

    // The composite, from the TIFF itself.
    let surface = crate::codec::decode_surface_bytes_as(bytes, limits, ImportFormat::Tiff)?;
    let (w, h) = (surface.width, surface.height);
    let rgba = surface.pixels.into_rgba8();
    let alpha = count < 0 || rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255);
    let planes = if alpha { 4 } else { 3 };

    let mut out = W::default();
    out.0.extend_from_slice(b"8BPS");
    out.u16(1);
    out.0.extend_from_slice(&[0; 6]);
    out.u16(planes as u16);
    out.u32(h);
    out.u32(w);
    out.u16(8);
    out.u16(3);
    out.u32(0); // colour mode data
    out.u32(0); // image resources
    let padded = info.len().div_ceil(4) * 4;
    let section = 4 + padded + 4;
    out.u32(u32::try_from(section).map_err(|_| malformed("the layer data is too large"))?);
    out.u32(padded as u32);
    out.0.extend_from_slice(&info);
    out.0.resize(out.0.len() + padded - info.len(), 0);
    out.u32(0); // global layer mask
    out.u16(0); // raw composite
    for c in 0..planes {
        out.0.extend(rgba.as_chunks::<4>().0.iter().map(|p| p[c]));
    }
    Ok(LayeredTiff {
        psd: out.0,
        records,
        notes,
    })
}

#[cfg(test)]
#[path = "tiff_layers_fixture.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use super::fixture;
    use super::*;

    #[test]
    fn both_byte_orders_hand_over_the_same_layers() {
        let be = fixture::layered_tiff(false, 8);
        let le = fixture::layered_tiff(true, 8);
        assert!(has_layers(&be) && has_layers(&le));
        let a = psd_bytes(&be, ImportLimits::default()).unwrap();
        let b = psd_bytes(&le, ImportLimits::default()).unwrap();
        assert_eq!(a.records, 2);
        assert!(a.notes.is_empty(), "{:?}", a.notes);
        assert!(
            b.notes.iter().any(|n| n.contains("lyid")),
            "the little-endian file names what it left out: {:?}",
            b.notes
        );
        // The same header, composite and records; only the dropped `lyid`
        // records differ, so the little-endian `.psd` is the shorter.
        assert_eq!(&a.psd[..26], &b.psd[..26]);
        assert_eq!(&a.psd[..4], b"8BPS");
        let tail = (fixture::WIDTH * fixture::HEIGHT * 3) as usize;
        assert_eq!(&a.psd[a.psd.len() - tail..], &b.psd[b.psd.len() - tail..]);
        assert_eq!(
            &a.psd[a.psd.len() - tail..][..3],
            &[255, 127, 127],
            "the composite's red plane"
        );
        assert!(b.psd.len() < a.psd.len());
    }

    #[test]
    fn deep_or_plain_tiffs_are_not_layered_and_damage_is_an_error() {
        let deep = fixture::layered_tiff(true, 16);
        assert!(!has_layers(&deep));
        assert!(psd_bytes(&deep, ImportLimits::default()).is_err());
        assert!(!has_layers(b"II*\0\x08\0\0\0\0\0"));
        assert!(!has_layers(b"not a tiff"));
        for le in [false, true] {
            let good = fixture::layered_tiff(le, 8);
            for n in 0..good.len() {
                let _ = has_layers(&good[..n]);
                let _ = psd_bytes(&good[..n], ImportLimits::default());
            }
            for i in 0..good.len() {
                let mut bad = good.clone();
                bad[i] ^= 0x41;
                let _ = psd_bytes(&bad, ImportLimits::default());
            }
        }
    }
}
