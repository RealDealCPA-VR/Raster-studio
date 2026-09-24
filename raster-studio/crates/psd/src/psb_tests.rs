//! W10-F: `.psb` (version 2) reading.
//!
//! One hand-assembled document is written twice by [`build`] - once as a
//! `.psd`, once as a `.psb` - differing only where the format says they
//! differ: 64-bit layer-and-mask, layer-info and channel lengths, a 64-bit
//! length on the long-key `FMsk` block (a short-key `lyid` block keeps its
//! 32-bit one), and 32-bit RLE row counts. The `.psd` is read by the
//! established version-1 reader, so a `.psb` that reads back to the same
//! [`PsdFile`] proves every one of those widths was honoured.

use crate::model::PsdFile;
use crate::{read, read_with, PsdError, ReadOptions};

const W: u32 = 5;
const H: u32 = 4;

struct Sink {
    out: Vec<u8>,
    psb: bool,
}

impl Sink {
    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_be_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.out.extend_from_slice(&v.to_be_bytes());
    }
    fn tag(&mut self, t: &[u8; 4]) {
        self.out.extend_from_slice(t);
    }
    /// A section or channel length: 4 bytes in a PSD, 8 in a PSB.
    fn len(&mut self, v: usize) {
        if self.psb {
            self.out.extend_from_slice(&(v as u64).to_be_bytes());
        } else {
            self.u32(v as u32);
        }
    }
    /// An RLE row count: 2 bytes in a PSD, 4 in a PSB.
    fn count(&mut self, v: usize) {
        if self.psb {
            self.u32(v as u32);
        } else {
            self.u16(v as u16);
        }
    }
    fn bytes(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
    }
}

/// PackBits of one row as a single literal run.
fn packbits(row: &[u8]) -> Vec<u8> {
    let mut v = vec![(row.len() - 1) as u8];
    v.extend_from_slice(row);
    v
}

/// One channel's data: compression code, then (RLE) the row table and rows.
fn channel(psb: bool, rle: bool, w: usize, plane: &[u8]) -> Vec<u8> {
    let mut s = Sink {
        out: Vec::new(),
        psb,
    };
    if rle {
        s.u16(1);
        let rows: Vec<Vec<u8>> = plane.chunks(w).map(packbits).collect();
        for r in &rows {
            s.count(r.len());
        }
        for r in &rows {
            s.bytes(r);
        }
    } else {
        s.u16(0);
        s.bytes(plane);
    }
    s.out
}

/// The sample channel `c` holds at `(x, y)` of a layer.
fn sample(layer: usize, c: usize, x: usize, y: usize) -> u8 {
    (layer * 70 + c * 50 + x * 9 + y * 13) as u8
}

/// The document, as a `.psd` (`psb == false`) or a `.psb`.
pub(crate) fn build(psb: bool) -> Vec<u8> {
    let mut s = Sink {
        out: Vec::new(),
        psb,
    };
    s.tag(b"8BPS");
    s.u16(if psb { 2 } else { 1 });
    s.bytes(&[0; 6]);
    s.u16(4); // RGBA composite
    s.u32(H);
    s.u32(W);
    s.u16(8);
    s.u16(3); // RGB
    s.u32(0); // colour mode data
    s.u32(0); // image resources

    // Two layers, bottom first: an RLE one covering the canvas and a raw
    // 2x2 one at (2, 1).
    let layers: [(&str, [i32; 4], bool); 2] = [
        ("back", [0, 0, H as i32, W as i32], true),
        ("top", [1, 2, 3, 4], false),
    ];
    let mut records = Sink {
        out: Vec::new(),
        psb,
    };
    let mut data = Vec::new();
    records.u16(layers.len() as u16);
    for (index, (name, rect, rle)) in layers.iter().enumerate() {
        let (w, h) = ((rect[3] - rect[1]) as usize, (rect[2] - rect[0]) as usize);
        let channels: Vec<(i16, Vec<u8>)> = [-1i16, 0, 1, 2]
            .iter()
            .map(|id| {
                let c = (*id + 1) as usize;
                let plane: Vec<u8> = (0..w * h).map(|i| sample(index, c, i % w, i / w)).collect();
                (*id, channel(psb, *rle, w, &plane))
            })
            .collect();
        for v in rect {
            records.i32(*v);
        }
        records.u16(channels.len() as u16);
        for (id, bytes) in &channels {
            records.u16(*id as u16);
            records.len(bytes.len());
            data.extend_from_slice(bytes);
        }
        records.tag(b"8BIM");
        records.tag(b"norm");
        records.bytes(&[255, 0, 0, 0]);
        let mut extra = Sink {
            out: Vec::new(),
            psb,
        };
        extra.u32(0); // mask
        extra.u32(0); // blending ranges
        let mut pascal = vec![name.len() as u8];
        pascal.extend_from_slice(name.as_bytes());
        while pascal.len() % 4 != 0 {
            pascal.push(0);
        }
        extra.bytes(&pascal);
        // A short-key block: 32-bit length in both variants.
        extra.tag(b"8BIM");
        extra.tag(b"lyid");
        extra.u32(4);
        extra.u32(100 + index as u32);
        records.u32(extra.out.len() as u32);
        records.bytes(&extra.out);
    }
    let mut layer_info = records.out;
    layer_info.extend_from_slice(&data);

    let mut lmi = Sink {
        out: Vec::new(),
        psb,
    };
    lmi.len(layer_info.len());
    lmi.bytes(&layer_info);
    lmi.u32(0); // global layer mask
                // A long-key block: 64-bit length in a PSB.
    lmi.tag(b"8BIM");
    lmi.tag(b"FMsk");
    lmi.len(12);
    lmi.bytes(&[7; 12]);

    s.len(lmi.out.len());
    s.bytes(&lmi.out);

    // The merged composite, RLE, one row table for all four channels.
    s.u16(1);
    let planes: Vec<Vec<u8>> = (0..4)
        .map(|c| {
            (0..(W * H) as usize)
                .map(|i| sample(9, c, i % W as usize, i / W as usize))
                .collect()
        })
        .collect();
    let rows: Vec<Vec<u8>> = planes
        .iter()
        .flat_map(|p| p.chunks(W as usize).map(packbits).collect::<Vec<_>>())
        .collect();
    for r in &rows {
        s.count(r.len());
    }
    for r in &rows {
        s.bytes(r);
    }
    s.out
}

fn summary(file: &PsdFile) -> String {
    format!(
        "{:?} {:?} {:?} {:?}",
        file.header,
        file.layers,
        file.extra,
        file.merged.as_ref().map(|m| &m.channels)
    )
}

#[test]
fn a_psb_reads_to_the_same_document_as_its_psd_twin() {
    let psd = read(&build(false)).expect("the .psd twin reads");
    let psb = read(&build(true)).expect("the .psb reads");
    assert_eq!(psd.layers.len(), 2);
    assert_eq!(psd.layers[1].name, "top");
    assert_eq!(psd.extra.len(), 1, "{:?}", psd.extra);
    assert_eq!(&psd.extra[0].key, b"FMsk");
    assert!(psd.warnings.is_empty(), "{:?}", psd.warnings);
    // Everything, pixels included, is identical.
    assert_eq!(summary(&psb), summary(&psd));
    assert!(psb.warnings.is_empty(), "{:?}", psb.warnings);
    // ...and the pixels are the ones written.
    let top = psb.layers[1].rgba8().expect("the top layer has pixels");
    assert_eq!(
        top[0..4],
        [
            sample(1, 1, 0, 0),
            sample(1, 2, 0, 0),
            sample(1, 3, 0, 0),
            sample(1, 0, 0, 0)
        ]
    );
    let merged = &psb.merged.as_ref().unwrap().channels;
    assert_eq!(merged[2][7], sample(9, 2, 2, 1));
}

#[test]
fn a_psb_canvas_may_exceed_the_psd_ceiling_but_not_its_own() {
    let mut bytes = build(true);
    // Declare a 40 000 px wide canvas: past a .psd's 30 000, within a
    // .psb's 300 000. It then fails on the (now too short) data, not on the
    // header.
    bytes[18..22].copy_from_slice(&40_000u32.to_be_bytes());
    let err = read(&bytes).unwrap_err();
    assert!(!matches!(err, PsdError::LimitExceeded { .. }), "{err}");
    let mut v1 = build(false);
    v1[18..22].copy_from_slice(&40_000u32.to_be_bytes());
    assert!(matches!(read(&v1), Err(PsdError::LimitExceeded { .. })));
    // A .psb past its own ceiling is refused by the header.
    let opts = ReadOptions {
        max_psb_dimension: 1_000,
        ..ReadOptions::default()
    };
    bytes[18..22].copy_from_slice(&2_000u32.to_be_bytes());
    assert!(matches!(
        read_with(&bytes, &opts),
        Err(PsdError::LimitExceeded { .. })
    ));
}

#[test]
fn a_malformed_psb_errors_and_never_panics() {
    let good = build(true);
    for n in 0..good.len() {
        let _ = read(&good[..n]);
    }
    for i in 0..good.len() {
        let mut bad = good.clone();
        bad[i] ^= 0xff;
        let _ = read(&bad);
    }
    // A 64-bit layer-and-mask length far past the file is refused.
    let mut huge = good.clone();
    huge[34..42].copy_from_slice(&u64::MAX.to_be_bytes());
    assert!(read(&huge).is_err());
    // An unknown version is still refused by name.
    let mut v3 = good;
    v3[4..6].copy_from_slice(&3u16.to_be_bytes());
    assert!(matches!(read(&v3), Err(PsdError::UnsupportedVersion(3))));
}

/// `testdata/two_layers.psb` is exactly this builder's `.psb`, so the
/// application-level test that opens it (app-shell `doc_formats_w10f_tests`)
/// opens the document this file proves reads correctly.
#[test]
fn the_committed_fixture_is_this_builders_psb() {
    let fixture: &[u8] = include_bytes!("testdata/two_layers.psb");
    assert_eq!(fixture, &build(true)[..]);
}
