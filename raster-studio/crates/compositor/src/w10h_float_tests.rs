//! W10-H: a 32-bit document's `f32` layer tiles composite at full `f32`
//! precision, HDR headroom included, through the free compositor and the
//! cached one the live view draws with.

use color::{srgb_to_linear, ColorSpace};
use raster::depth32::rgbaf32_to_tile_bytes;
use raster::{PixelRect, TileCoord, TILE_SIZE};

use crate::testkit::TestDoc;
use crate::{composite_region, CompositeOptions, TileCompositor};

#[test]
fn an_f32_tile_composites_at_f32_precision_and_keeps_values_above_one() {
    let w = TILE_SIZE;
    let mut t = TestDoc::new(w, 1);
    let id = t.push_raster("float");
    // Pixel 0 is HDR (4.0 red); pixel 1 sits between two 16-bit codes; the
    // rest of row 0 is a ramp no 8-bit step could hold.
    let mut samples = vec![0.0f32; (TILE_SIZE * TILE_SIZE * 4) as usize];
    for x in 0..w as usize {
        let v = 0.1 + x as f32 * 0.001_37;
        samples[x * 4..x * 4 + 4].copy_from_slice(&[v, v / 2.0, 1.0 - v, 1.0]);
    }
    samples[..4].copy_from_slice(&[4.0, 0.5, 0.0, 1.0]);
    samples[4..8].copy_from_slice(&[0.123_456_7, 0.25, 0.75, 1.0]);
    let hash = t.src.insert_bytes(rgbaf32_to_tile_bytes(&samples));
    t.set_tile_hash(id, TileCoord::new(0, 0, 0), hash);
    let (doc, src) = t.finish();

    let region = PixelRect::new(0, 0, w, 1);
    let free = composite_region(&doc, &src, region, 0, CompositeOptions::default()).unwrap();
    let live = TileCompositor::new()
        .composite_region(&doc, &src, region, 0, CompositeOptions::default())
        .unwrap();
    for canvas in [free, live] {
        let straight = canvas.to_straight();
        // HDR: the red of pixel 0 decodes to far above 1.0 and stays there.
        assert!(
            straight[0][0] > 20.0,
            "HDR red was clipped: {:?}",
            straight[0]
        );
        assert!((straight[0][0] - srgb_to_linear(4.0)).abs() < 1e-3);
        // Precision: the between-codes red decodes to its own value.
        assert!((straight[1][0] - srgb_to_linear(0.123_456_7)).abs() < 1e-6);
        // The ramp keeps every distinct step at 16 bits.
        let out = canvas.to_rgba16(&ColorSpace::Srgb);
        let mut reds: Vec<u16> = (2..w as usize).map(|x| out[x * 4]).collect();
        reds.dedup();
        assert_eq!(reds.len(), w as usize - 2, "the f32 ramp was banded");
        let odd = reds.iter().filter(|c| **c % 257 != 0).count();
        assert!(odd > reds.len() / 2, "the ramp went through 8 bits");
    }
}
