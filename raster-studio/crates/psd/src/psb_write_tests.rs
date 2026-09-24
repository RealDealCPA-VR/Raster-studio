//! W11-H: `.psb` (version 2) writing.
//!
//! The reader's `.psb` road is proven against hand-assembled files in
//! `psb_tests`, independently of this writer. So a document written by
//! [`write_psb`] that reads back to the same layers, blocks and composite as
//! its `.psd` twin proves every widened field was written at the width the
//! format asks for; and the byte walk below pins the widths themselves.

use crate::model::{Channel, MergedImage, PsdFile, PsdLayer, PsdMask, Rect, TaggedBlock};
use crate::{
    is_psb, read, write, write_psb, write_psb_with, ColorMode, Compression, Depth, PsdHeader,
    WriteOptions,
};

fn plane(n: usize, seed: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 7 + seed * 31) % 251) as u8).collect()
}

/// Two layers (one masked, one in a group) and a long-key document block.
fn document(depth: Depth) -> PsdFile {
    let header = PsdHeader {
        channels: 4,
        width: 6,
        height: 5,
        depth,
        color_mode: ColorMode::Rgb,
    };
    let mut file = PsdFile::new(header);
    let bps = depth.bytes_per_sample();
    let mut back = PsdLayer::raster("back", Rect::sized(6, 5));
    back.channels = (-1i16..3)
        .map(|id| Channel::new(id, plane(30 * bps, (id + 1) as usize)))
        .collect();
    back.mask = Some(PsdMask::new(Rect::new(1, 1, 4, 3), plane(6 * bps, 9)));
    let mut top = PsdLayer::raster("top", Rect::new(2, 1, 5, 4));
    top.channels = (-1i16..3)
        .map(|id| Channel::new(id, plane(9 * bps, (id + 5) as usize)))
        .collect();
    let mut group = PsdLayer::group("set");
    group.push_child(top).unwrap();
    file.layers = vec![back, group];
    // `lnk2` is one of the keys whose length is 64-bit in a `.psb`.
    file.extra
        .push(TaggedBlock::new(*b"lnk2", vec![1, 2, 3, 4, 5]));
    file
}

#[test]
fn a_psb_reads_back_as_the_same_document_as_its_psd_twin() {
    for depth in [Depth::Eight, Depth::Sixteen, Depth::ThirtyTwo] {
        let file = document(depth);
        let psd = write(&file).unwrap();
        let psb = write_psb(&file).unwrap();
        assert!(!is_psb(&psd) && is_psb(&psb), "{depth:?}");
        let (a, b) = (read(&psd).unwrap(), read(&psb).unwrap());
        assert_eq!(b.header, a.header, "{depth:?}");
        assert_eq!(b.layers, a.layers, "{depth:?}");
        assert_eq!(b.extra, a.extra, "{depth:?}");
        assert_eq!(b.merged, a.merged, "{depth:?}");
        assert!(b.warnings.is_empty(), "{depth:?}: {:?}", b.warnings);
        assert_eq!(b.layers.len(), 2);
    }
}

/// Walk the `.psb` by hand: 64-bit section lengths and 32-bit row counts,
/// where a `.psd` has 32 and 16.
#[test]
fn the_psb_field_widths_are_the_widened_ones() {
    let file = document(Depth::Eight);
    let opts = WriteOptions {
        layer_compression: Compression::Rle,
        merged_compression: Compression::Rle,
        ..Default::default()
    };
    let bytes = write_psb_with(&file, &opts).unwrap();
    let be32 = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let be64 = |at: usize| u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap()) as usize;
    assert_eq!(&bytes[4..6], &[0, 2], "version 2");
    let mut at = 26;
    at += 4 + be32(at); // colour mode data
    at += 4 + be32(at); // image resources
    let lmi_len = be64(at);
    let lmi = at + 8;
    let merged = lmi + lmi_len;
    // The layer-info length is 64-bit, and its first field is the count.
    let li_len = be64(lmi);
    assert_eq!(
        i16::from_be_bytes([bytes[lmi + 8], bytes[lmi + 9]]).unsigned_abs(),
        4,
        "two layers, a group record and its divider"
    );
    // Inside the first record: 16 bytes of rectangle, a channel count, then
    // (id, 64-bit length) pairs.
    let rec = lmi + 10;
    assert_eq!(u16::from_be_bytes([bytes[rec + 16], bytes[rec + 17]]), 5);
    let first_len = be64(rec + 18 + 2);
    assert!(first_len > 2 && first_len < 1000, "{first_len}");
    assert!(lmi + 8 + li_len <= merged);
    // The merged composite: RLE, then one 32-bit count per row per channel.
    assert_eq!(u16::from_be_bytes([bytes[merged], bytes[merged + 1]]), 1);
    let counts = (0..5 * 4).map(|i| be32(merged + 2 + i * 4));
    let packed: usize = counts.sum();
    assert_eq!(merged + 2 + 5 * 4 * 4 + packed, bytes.len());
}

#[test]
fn a_canvas_past_30000_pixels_is_written_as_a_psb() {
    let (w, h) = (30_001u32, 2u32);
    let mut file = PsdFile::new(PsdHeader::rgba8(w, h));
    let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i % 253) as u8).collect();
    file.merged = Some(MergedImage::from_rgba8(w, h, &rgba).unwrap());
    let bytes = write(&file).unwrap();
    assert!(is_psb(&bytes), "a 30001 px canvas has no .psd encoding");
    let back = read(&bytes).unwrap();
    assert_eq!((back.header.width, back.header.height), (w, h));
    assert_eq!(back.merged.unwrap().to_rgba8(w, h).unwrap(), rgba);
    // A .psb has its own ceiling.
    let file = PsdFile::new(PsdHeader::rgba8(300_001, 1));
    assert!(write_psb(&file).is_err());
}
