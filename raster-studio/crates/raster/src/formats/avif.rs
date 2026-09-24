//! AVIF (`.avif`): written through `image`'s AVIF encoder (`ravif` over
//! `rav1e`, both pure Rust, `image` builds them without assembly); **read:
//! refused by name**.
//!
//! # Why there is no reader
//!
//! W10-F built one - an ISOBMFF walk plus `rav1d` 1.1 (the pure-Rust port of
//! dav1d, BSD-2-Clause) - and its bit-flip test aborted the test process:
//! `rav1d` only exposes dav1d's C API (`extern "C"` functions), and on a
//! damaged frame header it reaches `Option::unwrap()` on `None`
//! (`rav1d-1.1.0/src/decode.rs:4997`) inside one of those functions, which
//! Rust turns into "panic in a function that cannot unwind" - a process
//! abort that no `catch_unwind` can stop (and the release profile is
//! `panic = "abort"` anyway). A malformed `.avif` would therefore take the
//! whole editor down with every open document. The only other pure-Rust AV1
//! decoders on crates.io are 0.0.x releases this wave did not trust, and
//! `image`'s `avif-native` decoder binds libdav1d (C). So a `.avif` is
//! recognised by content and refused with that reason ([`refusal`]) rather
//! than risked; `docs/parity-matrix.md` records the gap. That reader, its
//! bit-flip test and the `rav1d` dependency were removed together, so
//! nothing in this tree reproduces the abort any more; the tests below
//! cover the encoder, the refusal and the brand sniff.
//!
//! # What is written
//!
//! [`encode`]: 8-bit RGBA, 4:4:4, at a quality `1..=100` (100 is visually
//! lossless, not bit-exact), with an alpha item when any pixel is not
//! opaque.

use image::ImageEncoder;

use crate::codec::CodecError;

/// `true` when `head` opens with an ISOBMFF `ftyp` box whose major brand is
/// `avif` (a still image) or `avis` (a sequence), or whose major brand is the
/// generic HEIF `mif1` / `msf1` with `avif` / `avis` among the compatible
/// brands (the ones `head` holds; the box's own size bounds the list).
pub fn looks_like_avif(head: &[u8]) -> bool {
    if head.len() < 12 || &head[4..8] != b"ftyp" {
        return false;
    }
    let is_avif = |brand: &[u8]| matches!(brand, b"avif" | b"avis");
    if is_avif(&head[8..12]) {
        return true;
    }
    if !matches!(&head[8..12], b"mif1" | b"msf1") {
        return false;
    }
    let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    // Brands start after the major brand (8..12) and minor version (12..16).
    let end = size.min(head.len());
    head.get(16..end.max(16))
        .unwrap_or(&[])
        .as_chunks::<4>()
        .0
        .iter()
        .any(|brand| is_avif(brand))
}

/// The refusal every `.avif` read gets, naming why.
pub fn refusal() -> CodecError {
    CodecError::Unsupported(
        "opening AVIF is not supported: rav1d, the pure-Rust AV1 decoder this build \
         evaluated, aborts the process on a damaged file instead of reporting an error \
         (the other pure-Rust ones are 0.0.x releases), so this build does not risk it; AVIF export works (File > Export As > AVIF), and an AVIF can be opened \
         after converting it to PNG or JPEG"
            .into(),
    )
}

/// Encode straight RGBA8 as an 8-bit 4:4:4 AVIF at `quality` (`1..=100`).
pub fn encode(width: u32, height: u32, rgba: &[u8], quality: u8) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    // Speed 6 of 10: rav1e's balanced preset.
    image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut out, 6, quality).write_image(
        rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An independent encoder's file (libaom through ffmpeg, 4:2:0) is
    /// recognised by content and refused by name, as is our own output.
    const LIBAOM_420: &[u8] = include_bytes!("testdata/quadrants_16x12_libaom_420.avif");

    #[test]
    fn our_encoder_writes_a_real_avif_and_every_avif_read_is_refused_by_name() {
        let (w, h) = (16u32, 12u32);
        let mut px = Vec::new();
        for i in 0..w * h {
            px.extend_from_slice(&[(i * 7) as u8, 90, 200, if i % 5 == 0 { 0 } else { 255 }]);
        }
        let file = encode(w, h, &px, 80).unwrap();
        assert!(looks_like_avif(&file), "{:?}", &file[..16]);
        // ISOBMFF: ftyp, then a meta box with an av01 item and an alpha
        // auxiliary (some pixels are transparent), then the coded data.
        let has = |tag: &[u8]| file.windows(tag.len()).any(|w| w == tag);
        assert!(has(b"meta") && has(b"av01") && has(b"mdat") && has(b"auxC"));
        for bytes in [&file[..], LIBAOM_420] {
            assert!(looks_like_avif(bytes));
            let err = crate::codec::decode_surface_bytes(bytes, Default::default()).unwrap_err();
            let text = err.to_string();
            assert!(text.contains("AVIF") && text.contains("rav1d"), "{text}");
        }
        // Out-of-range quality is refused before encoding.
        assert!(crate::codec::ExportFormat::Avif(0).validate().is_err());
        assert!(crate::codec::ExportFormat::Avif(101).validate().is_err());
    }

    /// A `mif1`-major file that lists `avif` among its compatible brands is
    /// an AVIF (refused with the AVIF reason), not a HEIC; one listing
    /// `heic` is a HEIC; a brand past the `ftyp` box's own size is not read.
    #[test]
    fn a_mif1_file_is_named_by_its_compatible_brands() {
        let ftyp = |major: &[u8; 4], brands: &[&[u8; 4]], size_override: Option<u32>| {
            let size = 16 + 4 * brands.len() as u32;
            let mut b = size_override.unwrap_or(size).to_be_bytes().to_vec();
            b.extend_from_slice(b"ftyp");
            b.extend_from_slice(major);
            b.extend_from_slice(&[0, 0, 0, 0]);
            for brand in brands {
                b.extend_from_slice(*brand);
            }
            // Something after the box, as a real file has.
            b.extend_from_slice(&[0, 0, 0, 8]);
            b.extend_from_slice(b"meta");
            b
        };
        let avif = ftyp(b"mif1", &[b"mif1", b"miaf", b"MA1B", b"avif"], None);
        assert!(looks_like_avif(&avif));
        assert!(!super::super::looks_like_heic(&avif));
        let text = crate::codec::decode_surface_bytes(&avif, Default::default())
            .unwrap_err()
            .to_string();
        assert!(text.contains("AVIF") && !text.contains("HEIC"), "{text}");

        let heic = ftyp(b"mif1", &[b"mif1", b"heic"], None);
        assert!(!looks_like_avif(&heic));
        assert!(super::super::looks_like_heic(&heic));
        let text = crate::codec::decode_surface_bytes(&heic, Default::default())
            .unwrap_err()
            .to_string();
        assert!(text.contains("HEIC"), "{text}");

        // The box says it ends before the `avif` brand: not AVIF.
        let short = ftyp(b"mif1", &[b"mif1", b"avif"], Some(20));
        assert!(!looks_like_avif(&short));
        // Truncated heads never panic.
        for n in 0..avif.len() {
            let _ = looks_like_avif(&avif[..n]);
            let _ = super::super::looks_like_heic(&avif[..n]);
        }
    }
}
