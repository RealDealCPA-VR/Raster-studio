//! W9-G: a layer's vector mask — the `vmsk`/`vsms` path plus the vector
//! half of the mask-parameter block — survives a write and a read.

use crate::model::{PsdFile, PsdLayer, PsdMask, Rect, TaggedBlock};
use crate::shape::{Knot, SubPath, VectorPath};
use crate::{read, write, PsdHeader};

fn triangle() -> VectorPath {
    let k = |x: f64, y: f64| Knot {
        before: [x, y],
        anchor: [x, y],
        after: [x, y],
        linked: false,
    };
    VectorPath {
        subpaths: vec![SubPath {
            closed: true,
            operation: 1,
            knots: vec![k(0.0, 0.0), k(16.0, 0.0), k(0.0, 16.0)],
        }],
        invert: false,
        not_linked: false,
        disabled: true,
    }
}

#[test]
fn the_vector_mask_parameter_pair_round_trips() {
    let mut file = PsdFile::new(PsdHeader::rgba8(16, 16));
    let mut layer = PsdLayer::raster("masked", Rect::sized(16, 16));
    layer.set_rgba8(&[255; 16 * 16 * 4]).unwrap();
    let mut mask = PsdMask::new(Rect::sized(16, 16), vec![255; 256]);
    mask.vector_density = Some(128);
    mask.vector_feather_px = Some(3.5);
    layer.mask = Some(mask);
    layer
        .extra
        .push(TaggedBlock::new(*b"vmsk", triangle().encode(16, 16)));
    file.layers.push(layer);

    let back = read(&write(&file).unwrap()).unwrap();
    let l = &back.layers[0];
    let mask = l.mask.as_ref().expect("the mask record survived");
    assert_eq!(mask.vector_density, Some(128));
    assert_eq!(mask.vector_feather_px, Some(3.5));
    assert_eq!(mask.density, 255, "the user-mask pair is untouched");
    assert_eq!(mask.feather_px, 0.0);
    let block = l
        .extra
        .iter()
        .find(|b| &b.key == b"vmsk")
        .expect("the vmsk block survived");
    assert_eq!(VectorPath::decode(&block.data, 16, 16), Some(triangle()));
}

#[test]
fn a_mask_without_vector_parameters_reads_none() {
    let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
    let mut layer = PsdLayer::raster("plain", Rect::sized(4, 4));
    layer.set_rgba8(&[255; 64]).unwrap();
    layer.mask = Some(PsdMask::new(Rect::sized(4, 4), vec![200; 16]));
    file.layers.push(layer);
    let back = read(&write(&file).unwrap()).unwrap();
    let mask = back.layers[0].mask.as_ref().unwrap();
    assert_eq!((mask.vector_density, mask.vector_feather_px), (None, None));
}
