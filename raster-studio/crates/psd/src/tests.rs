//! Whole-file tests: the writer through the reader, and the reader against
//! files that have been damaged on purpose.
//!
//! The fixtures here are all built in-process. There is no checked-in `.psd`
//! binary to go stale, and the corrupt fixtures are produced by truncating and
//! byte-flipping a file this crate just wrote, so they stay in step with the
//! writer automatically.

use layer_model::BlendMode;

use crate::codec::Compression;
use crate::error::PsdError;
use crate::header::{ColorMode, Depth, PsdHeader};
use crate::limits::{ReadOptions, WriteOptions};
use crate::model::{
    Adjustment, Channel, Effects, ImageResource, MergedImage, Protection, PsdFile, PsdLayer,
    PsdMask, RealMask, Rect, TaggedBlock, TextData, CHANNEL_ALPHA,
};
use crate::resource::{resolution_info, ID_RESOLUTION_INFO};
use crate::{read, read_with, write, write_with};

// ---------------------------------------------------------------- fixtures

/// A deterministic RGBA image with both flat runs and noise, so no channel
/// encoding gets an easy ride.
fn image(width: u32, height: u32, salt: u8) -> Vec<u8> {
    let n = (width as usize) * (height as usize) * 4;
    (0..n)
        .map(|i| {
            if (i / 13) % 4 == 0 {
                salt
            } else {
                ((i * 37 + usize::from(salt)) % 251) as u8
            }
        })
        .collect()
}

fn raster(name: &str, rect: Rect, salt: u8) -> PsdLayer {
    let mut l = PsdLayer::raster(name, rect);
    l.set_rgba8(&image(rect.width(), rect.height(), salt))
        .expect("image matches the rectangle");
    l
}

/// A document exercising nearly every field this crate models.
fn rich_document() -> PsdFile {
    let mut file = PsdFile::new(PsdHeader::rgba8(16, 12));
    // Pre-seed the resolution resource the writer would otherwise synthesise,
    // so the round-trip comparison is exact.
    file.resources.push(resolution_info(72.0));
    file.resources.push(ImageResource {
        id: 1039,
        // Non-ASCII on purpose. A resource name has no `luni` counterpart
        // anywhere in the format, so this Pascal string is the only copy of it
        // there is — and when the writer and the reader disagreed about its
        // encoding, "café" came back as "cafÃ©" and got a little worse on every
        // save. An ASCII-only fixture made the `back.resources == resources`
        // assertion below vacuous on exactly the axis that was broken.
        name: "café ↔ プロファイル".into(),
        data: vec![7; 33], // odd length, to exercise the padding
    });
    file.global_mask = vec![0; 13];
    file.extra.push(TaggedBlock::new(*b"Patt", vec![1, 2, 3]));

    let mut background = raster("Background", Rect::sized(16, 12), 3);
    background.transparency_protected = true;
    background.protection = Protection {
        transparency: true,
        composite: false,
        position: true,
    };
    background.layer_id = Some(1);
    file.layers.push(background);

    let mut masked = raster("Masked ✦ layer", Rect::new(2, 1, 10, 9), 40);
    masked.blend_mode = BlendMode::VividLight;
    masked.opacity = 200;
    masked.fill_opacity = Some(64);
    masked.clipping = true;
    masked.sheet_color = Some(4);
    masked.layer_id = Some(2);
    let mut mask = PsdMask::new(Rect::new(3, 2, 9, 8), (0..36u32).map(|v| v as u8).collect());
    mask.default_color = 255;
    mask.relative_to_layer = true;
    mask.invert = true;
    mask.real = Some(RealMask {
        bounds: Rect::new(0, 0, 4, 4),
        default_color: 0,
        relative_to_layer: false,
        disabled: true,
        invert: false,
        data: (0..16u32).map(|v| (v * 3) as u8).collect(),
    });
    masked.mask = Some(mask);
    masked.blending_ranges = (0..40u32).map(|v| v as u8).collect();
    masked.effects = Some(Effects {
        key: *b"lfx2",
        data: vec![9; 25], // odd length again
    });
    masked
        .extra
        .push(TaggedBlock::new(*b"lnsr", b"bgnd".to_vec()));
    file.layers.push(masked);

    let mut adjustment = PsdLayer::raster("Levels", Rect::default());
    adjustment.adjustment = Some(Adjustment {
        key: *b"levl",
        data: vec![0, 2, 1, 2, 3],
    });
    adjustment.visible = false;
    file.layers.push(adjustment);

    let mut outer = PsdLayer::group("Outer group");
    outer.blend_mode = BlendMode::Screen;
    outer.opacity = 180;
    if let Some(g) = outer.group_data_mut() {
        g.open = false;
    }
    let mut inner = PsdLayer::group("Inner group");
    if let Some(g) = inner.group_data_mut() {
        g.pass_through = true;
    }
    inner
        .push_child(raster("Leaf A", Rect::new(0, 0, 5, 5), 77))
        .unwrap();
    inner
        .push_child(raster("Leaf B", Rect::new(1, 1, 6, 6), 91))
        .unwrap();
    outer.push_child(inner).unwrap();
    outer
        .push_child(raster("Sibling", Rect::new(4, 4, 12, 10), 120))
        .unwrap();
    file.layers.push(outer);

    file.merged = Some(MergedImage::from_rgba8(16, 12, &image(16, 12, 5)).unwrap());
    file
}

/// Compare everything a round-trip is supposed to preserve.
fn assert_round_trips(file: &PsdFile) -> PsdFile {
    let bytes = write(file).expect("write");
    let back = read(&bytes).expect("read");
    assert!(back.warnings.is_empty(), "warnings: {:?}", back.warnings);
    assert_eq!(back.header, file.header, "header");
    assert_eq!(
        back.color_mode_data, file.color_mode_data,
        "colour mode data"
    );
    assert_eq!(back.resources, file.resources, "image resources");
    assert_eq!(back.global_mask, file.global_mask, "global layer mask");
    assert_eq!(back.extra, file.extra, "document tagged blocks");
    assert_eq!(back.layers, file.layers, "layer tree");
    if let Some(m) = &file.merged {
        assert_eq!(back.merged.as_ref(), Some(m), "merged composite");
    }
    back
}

// ------------------------------------------------------------- round trips

#[test]
fn a_rich_document_round_trips_structure_and_pixels_exactly() {
    let file = rich_document();
    let back = assert_round_trips(&file);
    // Spot-check the pixels rather than trusting the derived equality alone.
    let original = image(16, 12, 3);
    assert_eq!(back.layers[0].rgba8().unwrap(), original);
    assert_eq!(back.all_layers().len(), file.all_layers().len());
}

#[test]
fn the_smallest_useful_document_round_trips() {
    let rgba = vec![1, 2, 3, 4];
    let file = crate::from_rgba8(1, 1, &rgba).unwrap();
    let bytes = write(&file).unwrap();
    let back = read(&bytes).unwrap();
    assert_eq!(back.layers.len(), 1);
    assert_eq!(back.layers[0].rgba8().unwrap(), rgba);
    assert_eq!(back.merged.unwrap().to_rgba8(1, 1).unwrap(), rgba);
}

#[test]
fn all_four_channel_encodings_produce_the_same_pixels_in_a_whole_file() {
    let file = rich_document();
    let mut decoded = Vec::new();
    for compression in Compression::ALL {
        let opts = WriteOptions {
            layer_compression: compression,
            merged_compression: compression,
            ..Default::default()
        };
        let bytes = write_with(&file, &opts).unwrap();
        let back = read(&bytes).unwrap();
        assert_eq!(
            back.layers, file.layers,
            "{compression:?} changed the layers"
        );
        decoded.push(back.merged.unwrap());
    }
    for m in &decoded[1..] {
        assert_eq!(m, &decoded[0], "encodings disagree on the merged composite");
    }
    assert_eq!(decoded.len(), 4);
}

#[test]
fn every_blend_mode_survives_a_whole_file_round_trip() {
    for mode in BlendMode::ALL {
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
        file.resources.push(resolution_info(72.0));
        let mut layer = raster("l", Rect::sized(2, 2), 1);
        layer.blend_mode = mode;
        file.layers.push(layer);
        let back = read(&write(&file).unwrap()).unwrap();
        assert_eq!(back.layers[0].blend_mode, mode, "{mode:?}");
        assert!(!back.layers[0].is_group());
    }
}

#[test]
fn a_pass_through_group_is_distinguishable_from_a_normal_one() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    let mut pass = PsdLayer::group("pass");
    pass.group_data_mut().unwrap().pass_through = true;
    let mut normal = PsdLayer::group("normal");
    normal.blend_mode = BlendMode::Multiply;
    file.layers.push(pass);
    file.layers.push(normal);

    let back = read(&write(&file).unwrap()).unwrap();
    assert!(back.layers[0].group_data().unwrap().pass_through);
    assert_eq!(back.layers[0].blend_mode, BlendMode::Normal);
    assert!(!back.layers[1].group_data().unwrap().pass_through);
    assert_eq!(back.layers[1].blend_mode, BlendMode::Multiply);
}

#[test]
fn nested_groups_keep_their_hierarchy_and_their_order() {
    let file = rich_document();
    let back = read(&write(&file).unwrap()).unwrap();

    let outer = back.layers.last().expect("the group is on top");
    assert_eq!(outer.name, "Outer group");
    let og = outer.group_data().expect("outer is a group");
    assert!(!og.open, "the collapsed state is part of the hierarchy");
    assert_eq!(og.children.len(), 2);
    // Bottom-to-top: the inner group is below the sibling.
    assert_eq!(og.children[0].name, "Inner group");
    assert_eq!(og.children[1].name, "Sibling");

    let inner = og.children[0].group_data().expect("inner is a group");
    assert!(inner.pass_through);
    let leaves: Vec<&str> = inner.children.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(leaves, vec!["Leaf A", "Leaf B"]);
    assert!(inner.children.iter().all(|l| !l.is_group()));
}

#[test]
fn five_levels_of_nesting_survive() {
    let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
    let mut node = PsdLayer::group("g5");
    node.push_child(raster("deep leaf", Rect::sized(2, 2), 8))
        .unwrap();
    for level in (1..5).rev() {
        let mut parent = PsdLayer::group(format!("g{level}"));
        parent.push_child(node).unwrap();
        node = parent;
    }
    file.layers.push(node);

    let back = read(&write(&file).unwrap()).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut cursor = &back.layers[0];
    for level in 1..=5 {
        assert_eq!(cursor.name, format!("g{level}"));
        let g = cursor.group_data().unwrap_or_else(|| panic!("g{level}"));
        assert_eq!(g.children.len(), 1);
        cursor = &g.children[0];
    }
    assert_eq!(cursor.name, "deep leaf");
    assert!(!cursor.is_group());
}

#[test]
fn an_empty_group_round_trips_as_an_empty_group_not_as_a_layer() {
    let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
    file.layers.push(PsdLayer::group("empty"));
    let back = read(&write(&file).unwrap()).unwrap();
    assert_eq!(back.layers.len(), 1);
    assert_eq!(back.layers[0].name, "empty");
    assert!(back.layers[0].children().is_empty());
    assert!(back.layers[0].is_group());
}

#[test]
fn unicode_layer_names_survive_through_the_luni_block() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    for name in [
        "日本語のレイヤー",
        "Ελληνικά",
        "emoji 🎨🖌️ layer",
        "trailing spaces   ",
        "",
    ] {
        file.layers.push(raster(name, Rect::sized(2, 2), 1));
    }
    let back = read(&write(&file).unwrap()).unwrap();
    let names: Vec<&str> = back.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "日本語のレイヤー",
            "Ελληνικά",
            "emoji 🎨🖌️ layer",
            "trailing spaces   ",
            "",
        ]
    );
}

#[test]
fn a_name_longer_than_a_pascal_string_survives_because_luni_carries_it() {
    let long: String = std::iter::repeat_n('ω', 400).collect();
    let mut file = PsdFile::new(PsdHeader::rgba8(1, 1));
    file.layers.push(raster(&long, Rect::sized(1, 1), 1));
    let back = read(&write(&file).unwrap()).unwrap();
    assert_eq!(back.layers[0].name, long);
}

#[test]
fn masks_keep_their_own_rectangle_default_colour_flags_and_pixels() {
    let file = rich_document();
    let back = read(&write(&file).unwrap()).unwrap();
    let mask = back.layers[1].mask.as_ref().expect("the mask survived");
    assert_eq!(mask.bounds, Rect::new(3, 2, 9, 8));
    assert_eq!(mask.default_color, 255);
    assert!(mask.relative_to_layer);
    assert!(mask.invert);
    assert!(!mask.disabled);
    assert_eq!(mask.data, (0..36u32).map(|v| v as u8).collect::<Vec<u8>>());
    let real = mask.real.as_ref().expect("the real mask survived");
    assert_eq!(real.bounds, Rect::new(0, 0, 4, 4));
    assert!(real.disabled);
    assert_eq!(real.data.len(), 16);
}

#[test]
fn a_layer_with_no_mask_writes_a_zero_length_mask_and_reads_back_without_one() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    file.layers.push(raster("plain", Rect::sized(2, 2), 1));
    let back = read(&write(&file).unwrap()).unwrap();
    assert!(back.layers[0].mask.is_none());
}

#[test]
fn deep_bit_depth_layers_land_in_the_lr16_and_lr32_blocks_where_photoshop_looks() {
    for (depth, key) in [(Depth::Sixteen, b"Lr16"), (Depth::ThirtyTwo, b"Lr32")] {
        let header = PsdHeader {
            channels: 4,
            width: 4,
            height: 3,
            depth,
            color_mode: ColorMode::Rgb,
        };
        let mut file = PsdFile::new(header);
        file.resources.push(resolution_info(72.0));
        let bps = depth.bytes_per_sample();
        let n = 12 * bps;
        let mut layer = PsdLayer::raster("deep", Rect::sized(4, 3));
        layer.channels = vec![
            Channel::new(CHANNEL_ALPHA, vec![0xFF; n]),
            Channel::new(0, (0..n).map(|i| (i * 7 % 251) as u8).collect()),
            Channel::new(1, (0..n).map(|i| (i * 11 % 241) as u8).collect()),
            Channel::new(2, (0..n).map(|i| (i * 13 % 239) as u8).collect()),
        ];
        file.layers.push(layer);
        // ZIP with prediction is what Photoshop itself uses at these depths.
        let opts = WriteOptions {
            layer_compression: Compression::ZipPrediction,
            merged_compression: Compression::Raw,
            ..Default::default()
        };
        let bytes = write_with(&file, &opts).unwrap();
        assert!(
            find_subsequence(&bytes, key).is_some(),
            "{depth:?}: no {} block was written",
            String::from_utf8_lossy(key)
        );
        let back = read(&bytes).unwrap();
        assert_eq!(back.layers, file.layers, "{depth:?}");
        assert!(back.extra.iter().all(|b| &b.key != key), "the block leaked");
    }
}

#[test]
fn an_eight_bit_document_keeps_its_layers_in_the_layer_info_section() {
    let file = rich_document();
    let bytes = write(&file).unwrap();
    assert!(find_subsequence(&bytes, b"Lr16").is_none());
    assert!(find_subsequence(&bytes, b"Lr32").is_none());
}

#[test]
fn a_greyscale_document_round_trips() {
    let header = PsdHeader {
        channels: 2,
        width: 4,
        height: 2,
        depth: Depth::Eight,
        color_mode: ColorMode::Grayscale,
    };
    let mut file = PsdFile::new(header);
    file.resources.push(resolution_info(72.0));
    let mut layer = PsdLayer::raster("grey", Rect::sized(4, 2));
    layer.channels = vec![
        Channel::new(CHANNEL_ALPHA, vec![255, 128, 64, 0, 255, 255, 255, 255]),
        Channel::new(0, vec![0, 32, 64, 96, 128, 160, 192, 224]),
    ];
    file.layers.push(layer);
    let back = assert_round_trips(&file);
    assert_eq!(back.header.color_mode, ColorMode::Grayscale);
    assert_eq!(back.merged.unwrap().channels.len(), 2);
}

#[test]
fn adjustment_effects_and_type_data_are_preserved_and_recognised() {
    let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));

    let mut solid = PsdLayer::raster("Solid colour", Rect::default());
    let mut body = crate::bytes::Sink::new();
    body.u32(16);
    let mut colour = crate::Descriptor::new("null");
    let mut rgb = crate::Descriptor::new("RGBC");
    rgb.push("Rd  ", crate::Value::Double(255.0)).unwrap();
    rgb.push("Grn ", crate::Value::Double(64.0)).unwrap();
    rgb.push("Bl  ", crate::Value::Double(0.0)).unwrap();
    colour.push("Clr ", crate::Value::Descriptor(rgb)).unwrap();
    colour.write(&mut body).unwrap();
    solid.adjustment = Some(Adjustment {
        key: *b"SoCo",
        data: body.into_inner(),
    });
    file.layers.push(solid);

    let mut typed = PsdLayer::raster("Type", Rect::sized(4, 4));
    typed.set_rgba8(&image(4, 4, 2)).unwrap();
    typed.text = Some(TextData {
        transform: [1.0, 0.0, 0.0, 1.0, 3.5, 9.0],
        text: Some("preserved".into()),
        raw: tysh_fixture("preserved"),
    });
    file.layers.push(typed);

    let back = read(&write(&file).unwrap()).unwrap();
    let adj = back.layers[0].adjustment.as_ref().unwrap();
    assert_eq!(adj.kind(), Some(crate::AdjustmentKey::SolidColorFill));
    assert_eq!(
        adj.solid_color_rgb(&ReadOptions::default()),
        Some([255.0, 64.0, 0.0])
    );
    let text = back.layers[1].text.as_ref().unwrap();
    assert_eq!(text.text.as_deref(), Some("preserved"));
    assert_eq!(text.transform, [1.0, 0.0, 0.0, 1.0, 3.5, 9.0]);
    assert_eq!(text.raw, tysh_fixture("preserved"));
}

#[test]
fn a_writer_that_was_given_no_composite_synthesises_one() {
    let mut file = PsdFile::new(PsdHeader::rgba8(3, 2));
    let mut rgba = image(3, 2, 9);
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let mut layer = PsdLayer::raster("only", Rect::sized(3, 2));
    layer.set_rgba8(&rgba).unwrap();
    file.layers.push(layer);
    assert!(file.merged.is_none());
    let back = read(&write(&file).unwrap()).unwrap();
    let merged = back.merged.expect("a composite was written");
    assert_eq!(merged.channels.len(), 4);
    assert!(merged.channels.iter().all(|c| c.len() == 6));
    // An opaque full-canvas layer composites to itself.
    assert_eq!(merged.to_rgba8(3, 2).unwrap(), rgba);
}

#[test]
fn a_resolution_resource_is_synthesised_only_when_the_document_has_none() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    file.layers.push(raster("l", Rect::sized(2, 2), 1));
    let back = read(&write(&file).unwrap()).unwrap();
    let res: Vec<&ImageResource> =
        crate::read::resources_with_id(&back, ID_RESOLUTION_INFO).collect();
    assert_eq!(res.len(), 1);
    assert_eq!(crate::resource::resolution_dpi(res[0]), Some(72.0));

    file.resources.push(resolution_info(300.0));
    let back = read(&write(&file).unwrap()).unwrap();
    let res: Vec<&ImageResource> =
        crate::read::resources_with_id(&back, ID_RESOLUTION_INFO).collect();
    assert_eq!(res.len(), 1, "a second one must not be added");
    assert_eq!(crate::resource::resolution_dpi(res[0]), Some(300.0));

    let opts = WriteOptions {
        synthesize_resolution: false,
        ..Default::default()
    };
    let mut bare = PsdFile::new(PsdHeader::rgba8(2, 2));
    bare.layers.push(raster("l", Rect::sized(2, 2), 1));
    let back = read(&write_with(&bare, &opts).unwrap()).unwrap();
    assert!(back.resources.is_empty());
}

#[test]
fn writing_a_document_whose_channels_do_not_match_its_bounds_is_refused() {
    let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
    let mut layer = PsdLayer::raster("wrong", Rect::sized(4, 4));
    layer.channels = vec![Channel::new(0, vec![1, 2, 3])];
    file.layers.push(layer);
    let err = write(&file).unwrap_err();
    assert!(matches!(err, PsdError::InvalidDocument(_)), "{err}");
    assert!(!err.is_file_fault(), "this is the caller's mistake");
}

#[test]
fn a_header_that_cannot_be_expressed_is_refused_before_anything_is_allocated() {
    // A four-billion-pixel canvas: without the gate this reaches the flattener
    // and aborts the process on the allocation rather than returning.
    let mut file = PsdFile::new(PsdHeader::rgba8(u32::MAX, u32::MAX));
    assert!(matches!(
        write(&file).unwrap_err(),
        PsdError::InvalidDocument(_)
    ));

    file.header = PsdHeader::rgba8(30_001, 10);
    assert!(matches!(
        write(&file).unwrap_err(),
        PsdError::InvalidDocument(_)
    ));

    file.header = PsdHeader::rgba8(0, 10);
    assert!(matches!(
        write(&file).unwrap_err(),
        PsdError::InvalidDocument(_)
    ));

    file.header = PsdHeader {
        channels: 2,
        width: 4,
        height: 4,
        depth: Depth::Eight,
        color_mode: ColorMode::Rgb,
    };
    assert!(matches!(
        write(&file).unwrap_err(),
        PsdError::InvalidDocument(_)
    ));

    // The largest canvas the format allows is still accepted, and the check
    // itself does not allocate it.
    file.header = PsdHeader::rgba8(30_000, 30_000);
    file.merged = Some(MergedImage {
        channels: vec![Vec::new(); 4],
    });
    // The merged channels are the wrong size, so this fails later, in the
    // encoder — which proves the header gate let it through.
    assert!(matches!(
        write(&file).unwrap_err(),
        PsdError::ChannelSizeMismatch { .. }
    ));
}

#[test]
fn a_non_empty_layer_with_no_channels_at_all_is_refused() {
    let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
    file.layers
        .push(PsdLayer::raster("no pixels", Rect::sized(4, 4)));
    assert!(matches!(
        write(&file).unwrap_err(),
        PsdError::InvalidDocument(_)
    ));
}

// ------------------------------------------------------- hostile input

#[test]
fn every_truncation_of_a_valid_file_errors_and_never_panics() {
    let bytes = write(&rich_document()).unwrap();
    assert!(bytes.len() > 500, "fixture is too small to be interesting");
    // The merged composite is the last section and is genuinely optional, so a
    // prefix that stops exactly where it begins is a valid layered file with no
    // composite. Every *other* prefix has to be refused.
    let mut tolerated = 0usize;
    for cut in 0..bytes.len() {
        match read(&bytes[..cut]) {
            Err(_) => {}
            Ok(partial) => {
                assert!(
                    partial.merged.is_none(),
                    "a {cut}-byte prefix of a {}-byte file produced a complete document",
                    bytes.len()
                );
                tolerated += 1;
            }
        }
    }
    assert!(
        tolerated <= 2,
        "{tolerated} prefixes parsed; only the composite boundary should"
    );
    assert!(read(&bytes).is_ok(), "the untruncated file still parses");
}

#[test]
fn flipping_any_byte_of_a_valid_file_never_panics() {
    let original = write(&rich_document()).unwrap();
    let opts = ReadOptions {
        // A small budget so a corrupted size field cannot make the test slow.
        max_decoded_bytes: 8 << 20,
        ..Default::default()
    };
    let mut parsed_anyway = 0usize;
    for i in 0..original.len() {
        let mut bytes = original.clone();
        bytes[i] ^= 0xFF;
        match read_with(&bytes, &opts) {
            Ok(_) => parsed_anyway += 1,
            Err(e) => assert!(e.is_file_fault(), "byte {i}: {e}"),
        }
    }
    // Many single-byte flips land in pixel data and are undetectable; the point
    // is only that none of them panics.
    assert!(
        parsed_anyway > 0,
        "the fixture has no tolerant bytes at all"
    );
}

#[test]
fn flipping_any_single_bit_of_the_header_and_section_lengths_never_panics() {
    let original = write(&rich_document()).unwrap();
    let scan = original.len().min(256);
    for i in 0..scan {
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[i] ^= 1 << bit;
            let _ = read(&bytes);
        }
    }
}

#[test]
fn an_absurd_layer_count_does_not_reserve_for_it() {
    let mut bytes = write(&rich_document()).unwrap();
    let at = layer_count_offset(&bytes);
    // 0x7FFF layers, in a file of a couple of kilobytes.
    bytes[at] = 0x7F;
    bytes[at + 1] = 0xFF;
    let err = read(&bytes).unwrap_err();
    assert!(
        matches!(
            err,
            PsdError::Truncated { .. } | PsdError::LimitExceeded { .. }
        ),
        "{err}"
    );
}

#[test]
fn a_layer_count_beyond_the_configured_limit_is_refused_by_name() {
    let mut bytes = write(&rich_document()).unwrap();
    let at = layer_count_offset(&bytes);
    bytes[at] = 0x7F;
    bytes[at + 1] = 0xFF;
    let opts = ReadOptions {
        max_layers: 4,
        ..Default::default()
    };
    let err = read_with(&bytes, &opts).unwrap_err();
    match err {
        PsdError::LimitExceeded { what, value, max } => {
            assert_eq!((what, value, max), ("layer count", 0x7FFF, 4));
        }
        other => panic!("wrong error: {other}"),
    }
}

#[test]
fn an_absurd_canvas_declared_in_the_header_is_refused_before_allocating() {
    let mut bytes = write(&rich_document()).unwrap();
    // Header: 4 magic, 2 version, 6 reserved, 2 channels, then height and width.
    bytes[14..18].copy_from_slice(&30_001u32.to_be_bytes());
    let err = read(&bytes).unwrap_err();
    assert!(matches!(err, PsdError::LimitExceeded { .. }), "{err}");

    bytes[14..18].copy_from_slice(&u32::MAX.to_be_bytes());
    bytes[18..22].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        read(&bytes).unwrap_err(),
        PsdError::LimitExceeded { .. }
    ));
}

#[test]
fn a_canvas_within_the_limit_but_far_larger_than_the_file_is_bounded_by_the_budget() {
    let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
    file.layers.push(raster("l", Rect::sized(4, 4), 1));
    let mut bytes = write(&file).unwrap();
    // Claim a 20 000 x 20 000 canvas: 1.6 GB of merged pixels, from a file of
    // a few hundred bytes. This must refuse, not allocate.
    bytes[14..18].copy_from_slice(&20_000u32.to_be_bytes());
    bytes[18..22].copy_from_slice(&20_000u32.to_be_bytes());
    let err = read(&bytes).unwrap_err();
    assert!(
        matches!(
            err,
            PsdError::BudgetExhausted { .. } | PsdError::Truncated { .. }
        ),
        "{err}"
    );
}

#[test]
fn a_channel_declaring_more_bytes_than_the_file_holds_is_a_truncation() {
    // Build the smallest file we can reason about, then blow up the first
    // channel length in its single layer record.
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    file.layers.push(raster("l", Rect::sized(2, 2), 1));
    let mut bytes = write(&file).unwrap();
    let at = first_channel_length_offset(&bytes);
    bytes[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    let err = read(&bytes).unwrap_err();
    assert!(matches!(err, PsdError::Truncated { .. }), "{err}");
}

#[test]
fn a_zip_bomb_in_a_channel_is_refused_by_the_channel_geometry() {
    // A tiny layer whose ZIP channel inflates to megabytes.
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    file.layers.push(raster("l", Rect::sized(2, 2), 1));
    let opts = WriteOptions {
        layer_compression: Compression::Zip,
        ..Default::default()
    };
    let honest = write_with(&file, &opts).unwrap();
    let bomb = crate::zip::deflate(&vec![0u8; 32 << 20]).unwrap();
    // Splice the bomb in where a channel's zlib stream lives. Even if the
    // splice lands somewhere unexpected the parse must still refuse rather
    // than inflate 32 MiB into a four byte channel.
    let mut bytes = honest.clone();
    let at = first_channel_length_offset(&bytes);
    bytes[at..at + 4].copy_from_slice(&((bomb.len() + 2) as u32).to_be_bytes());
    let insert = find_subsequence(&honest, &[0x78]).unwrap_or(honest.len() / 2);
    bytes.splice(insert..insert, bomb.iter().copied());
    let res = read(&bytes);
    if let Ok(file) = res {
        for layer in file.all_layers() {
            for channel in &layer.channels {
                assert!(channel.data.len() <= 4 * 4, "a bomb got through");
            }
        }
    }
}

#[test]
fn an_unbalanced_group_divider_is_repaired_with_a_warning_rather_than_rejected() {
    // Write a valid nested document, then turn the group's closing record
    // (`lsct` type 1 or 2) into an ordinary layer by rewriting the type to 0.
    let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
    let mut group = PsdLayer::group("g");
    group
        .push_child(raster("child", Rect::sized(2, 2), 4))
        .unwrap();
    file.layers.push(group);
    let mut bytes = write(&file).unwrap();

    // Find the `lsct` whose body starts with 1 (an open group) and make it 0.
    let mut patched = false;
    let mut i = 0;
    while i + 12 <= bytes.len() {
        if &bytes[i..i + 4] == b"8BIM" && &bytes[i + 4..i + 8] == b"lsct" {
            let body = i + 12;
            if body + 4 <= bytes.len() && bytes[body + 3] == 1 {
                bytes[body + 3] = 0;
                patched = true;
                break;
            }
        }
        i += 1;
    }
    assert!(patched, "no open-group divider found to damage");

    let back = read(&bytes).unwrap();
    assert!(
        !back.warnings.is_empty(),
        "the repair should have been reported"
    );
    // The child was not lost.
    let names: Vec<&str> = back.all_layers().iter().map(|l| l.name.as_str()).collect();
    assert!(names.contains(&"child"), "{names:?}");
    // Type 0 is "any other type of layer", so the record that used to close the
    // group is now an ordinary one: the group is never closed, its contents are
    // lifted to the top level, and nothing here is a group any more.
    assert!(
        back.all_layers().iter().all(|l| !l.is_group()),
        "a demoted divider must not still read as a group: {names:?}"
    );
}

/// `lsct` has four defined types and only three of them are groups. Type 0 is
/// the specification's "any other type of layer" — an ordinary layer that
/// happens to carry the block, with real pixels in its channels. Reading it as
/// a group is invisible at read time (a group with no children looks like an
/// empty group) and destructive at write time: the saved file gets a group
/// record *and* a hidden bounding divider where one ordinary layer used to be,
/// so the layer structure of somebody else's file silently changes.
///
/// The fixture is built by writing a layer that carries a four-byte `lnsr`
/// block and then renaming that key to `lsct` in place — no length anywhere in
/// the file moves, so the record is exactly a real one carrying `lsct` type 0.
#[test]
fn a_section_divider_of_type_zero_is_an_ordinary_layer_not_a_group() {
    let pixels = image(3, 2, 11);
    let mut file = PsdFile::new(PsdHeader::rgba8(3, 2));
    let mut layer = raster("Plain", Rect::sized(3, 2), 11);
    // Four bytes of body, big-endian zero: read as an `lsct` this is type 0.
    layer
        .extra
        .push(TaggedBlock::new(*b"lnsr", vec![0, 0, 0, 0]));
    file.layers.push(layer);
    let mut bytes = write(&file).unwrap();

    let at = find_subsequence(&bytes, b"8BIMlnsr").expect("the block is in the record");
    bytes[at + 4..at + 8].copy_from_slice(b"lsct");

    let back = read(&bytes).unwrap();
    assert_eq!(back.layers.len(), 1);
    let read_layer = &back.layers[0];
    assert!(
        !read_layer.is_group(),
        "lsct type 0 is 'any other type of layer', not a group"
    );
    assert!(read_layer.children().is_empty());
    assert_eq!(read_layer.name, "Plain");
    // The pixels the record carried are still on the layer, not stranded on a
    // group record.
    assert_eq!(read_layer.channels.len(), 4);
    assert_eq!(read_layer.rgba8().unwrap(), pixels);
    assert_eq!(back.record_count(), 1, "one layer is one record");

    // And saving it does not grow the document: a group would cost two records.
    let again = write(&back).unwrap();
    let back2 = read(&again).unwrap();
    assert_eq!(back2.record_count(), 1);
    assert!(!back2.layers[0].is_group());
    assert_eq!(back2.layers[0].rgba8().unwrap(), pixels);
}

/// An `lsct` block shorter than the fields it is read for.
///
/// A divider's type is a `u32`, and the blend key that may restate the group's
/// mode is two more tags after it: twelve bytes in all, of which a hostile file
/// is free to supply none.
///
/// This is a regression guard over short `lsct` bodies, **not** a test that
/// fails without the change it accompanies, and it should not be read as one.
/// The spelling it replaced took the tail as `&block.data[4..]`, but only
/// inside an `if block.data.len() >= 12` several lines above, so the slice was
/// never taken for a short block and no input could panic there. The two forms
/// are behaviourally identical for every input — old and new alike need twelve
/// bytes to reach the signature at `4..8` and the key at `8..12`. Reading the
/// tail through the cursor that already consumed the type is therefore a
/// structural change: it deletes the pairing between a guard and an offset that
/// happened to be in step, which a later edit to either one could have put out
/// of step. What this test pins is the behaviour on both sides of that change —
/// a truncated divider is a non-fatal, still-writable record rather than a
/// fatal parse — so that the structural change stays free.
#[test]
fn a_section_divider_block_shorter_than_its_own_fields_is_not_fatal() {
    for len in [0usize, 1, 2, 3, 4, 6, 8, 11] {
        let mut file = PsdFile::new(PsdHeader::rgba8(3, 2));
        let mut layer = raster("Plain", Rect::sized(3, 2), 11);
        layer.extra.push(TaggedBlock::new(*b"lnsr", vec![0; len]));
        file.layers.push(layer);
        let mut bytes = write(&file).unwrap();
        let at = find_subsequence(&bytes, b"8BIMlnsr").expect("the block is in the record");
        bytes[at + 4..at + 8].copy_from_slice(b"lsct");

        let back =
            read(&bytes).unwrap_or_else(|e| panic!("a {len}-byte `lsct` must not be fatal: {e}"));
        assert_eq!(back.all_layers().len(), 1, "len {len}");
        // A body of zeros names divider type 0 — "any other type of layer" —
        // and a body too short to name anything leaves the record ordinary too.
        assert!(!back.layers[0].is_group(), "len {len}");
        assert_eq!(back.layers[0].name, "Plain", "len {len}");
        write(&back).unwrap_or_else(|e| panic!("len {len} left an unwritable document: {e}"));
    }
}

/// A file may declare a layer mask rectangle and then never supply the `-2`
/// channel that fills it. Reading that cleanly and *then* refusing to write the
/// document blames the caller for a defect in the file, so the reader drops the
/// mask and says so instead.
#[test]
fn a_mask_record_whose_channel_never_arrives_is_dropped_and_stays_writable() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    let mut layer = raster("L", Rect::sized(2, 2), 5);
    // The mask rectangle matches the layer's, so renaming its channel id below
    // leaves a channel that still decodes — the only thing that changes is that
    // no `-2` channel exists any more.
    layer.mask = Some(PsdMask::new(Rect::sized(2, 2), vec![10, 20, 30, 40]));
    file.layers.push(layer);
    let mut bytes = write(&file).unwrap();
    assert!(
        patch_first_channel_id(&mut bytes, crate::CHANNEL_USER_MASK, 5),
        "the written record should carry a user-mask channel"
    );

    let back = read(&bytes).unwrap();
    assert!(back.layers[0].mask.is_none(), "the empty mask was kept");
    assert!(
        back.warnings.iter().any(|w| w.contains("mask")),
        "dropping the mask must be reported: {:?}",
        back.warnings
    );
    // The whole point: the document the reader handed back can be saved.
    let again = write(&back).expect("a file the reader accepted must be writable");
    let back2 = read(&again).unwrap();
    assert_eq!(back2.layers[0].name, "L");
    assert!(back2.layers[0].mask.is_none());
}

/// The same asymmetry for the second, "real" mask: the 36-byte mask field is
/// present but the `-3` channel is not.
#[test]
fn a_real_mask_whose_channel_never_arrives_is_dropped_and_stays_writable() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    let mut layer = raster("L", Rect::sized(2, 2), 5);
    let mut mask = PsdMask::new(Rect::sized(2, 2), vec![10, 20, 30, 40]);
    mask.real = Some(RealMask {
        bounds: Rect::sized(2, 2),
        default_color: 0,
        relative_to_layer: false,
        disabled: false,
        invert: false,
        data: vec![1, 2, 3, 4],
    });
    layer.mask = Some(mask);
    file.layers.push(layer);
    let mut bytes = write(&file).unwrap();
    assert!(
        patch_first_channel_id(&mut bytes, crate::CHANNEL_REAL_USER_MASK, 6),
        "the written record should carry a real-user-mask channel"
    );

    let back = read(&bytes).unwrap();
    let mask = back.layers[0]
        .mask
        .as_ref()
        .expect("the mask itself is fine");
    assert_eq!(mask.data, vec![10, 20, 30, 40]);
    assert!(mask.real.is_none(), "the empty real mask was kept");
    assert!(
        back.warnings.iter().any(|w| w.contains("real user mask")),
        "{:?}",
        back.warnings
    );
    write(&back).expect("a file the reader accepted must be writable");
}

#[test]
fn an_unknown_blend_mode_key_degrades_to_normal_with_a_warning() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    let mut layer = raster("l", Rect::sized(2, 2), 1);
    layer.blend_mode = BlendMode::Exclusion;
    file.layers.push(layer);
    let mut bytes = write(&file).unwrap();
    let at = find_subsequence(&bytes, b"8BIMsmud").expect("the blend key is in the record");
    bytes[at + 4..at + 8].copy_from_slice(b"zzzz");
    let back = read(&bytes).unwrap();
    assert_eq!(back.layers[0].blend_mode, BlendMode::Normal);
    assert!(
        back.warnings.iter().any(|w| w.contains("zzzz")),
        "{:?}",
        back.warnings
    );
}

#[test]
fn an_unsupported_colour_mode_is_refused_by_name_rather_than_read_as_rgb() {
    let mut bytes = write(&rich_document()).unwrap();
    for (code, name) in [(4u16, "CMYK"), (9, "Lab"), (2, "Indexed"), (0, "Bitmap")] {
        bytes[24..26].copy_from_slice(&code.to_be_bytes());
        match read(&bytes).unwrap_err() {
            PsdError::UnsupportedColorMode { code: c, name: n } => {
                assert_eq!((c, n), (code, name));
            }
            other => panic!("{name}: wrong error: {other}"),
        }
    }
}

#[test]
fn an_unsupported_bit_depth_is_refused() {
    let mut bytes = write(&rich_document()).unwrap();
    bytes[22..24].copy_from_slice(&1u16.to_be_bytes());
    assert!(matches!(
        read(&bytes).unwrap_err(),
        PsdError::UnsupportedDepth(1)
    ));
}

#[test]
fn an_empty_input_and_a_stub_header_both_error_cleanly() {
    assert!(read(&[]).is_err());
    assert!(read(b"8BPS").is_err());
    assert!(read(b"not a psd at all").is_err());
    assert!(read(&[0u8; 26]).is_err());
}

#[test]
fn a_file_with_no_layer_section_still_yields_its_composite() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    file.merged = Some(MergedImage::from_rgba8(2, 2, &image(2, 2, 6)).unwrap());
    let back = read(&write(&file).unwrap()).unwrap();
    assert!(back.layers.is_empty());
    assert_eq!(back.merged.unwrap().to_rgba8(2, 2).unwrap(), image(2, 2, 6));
}

#[test]
fn reading_is_deterministic() {
    let bytes = write(&rich_document()).unwrap();
    let a = read(&bytes).unwrap();
    let b = read(&bytes).unwrap();
    assert_eq!(a, b);
    // ...and writing what was read reproduces the same bytes.
    assert_eq!(write(&a).unwrap(), bytes);
}

// ------------------------------------------- reading bytes we did not write

/// A whole `.psd` assembled field by field from the published layout, so the
/// reader is checked against the *specification* and not only against this
/// crate's own writer. A writer and reader that share a field-order mistake
/// round-trip each other perfectly; this fixture is the check that catches it.
///
/// 2×1 pixels, RGB, 8-bit, one layer named `Hi` with raw channel data.
fn handwritten_psd() -> Vec<u8> {
    let mut b: Vec<u8> = Vec::new();

    // --- File header, 26 bytes -------------------------------------------
    b.extend_from_slice(b"8BPS"); // signature
    b.extend_from_slice(&1u16.to_be_bytes()); // version 1 = PSD
    b.extend_from_slice(&[0; 6]); // reserved
    b.extend_from_slice(&4u16.to_be_bytes()); // channels
    b.extend_from_slice(&1u32.to_be_bytes()); // HEIGHT first
    b.extend_from_slice(&2u32.to_be_bytes()); // then width
    b.extend_from_slice(&8u16.to_be_bytes()); // depth
    b.extend_from_slice(&3u16.to_be_bytes()); // colour mode 3 = RGB
    assert_eq!(b.len(), 26);

    // --- Colour mode data and image resources, both empty ----------------
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes());

    // --- Layer and mask information --------------------------------------
    let mut layer_info: Vec<u8> = Vec::new();
    layer_info.extend_from_slice(&1i16.to_be_bytes()); // one layer
    for v in [0i32, 0, 1, 2] {
        layer_info.extend_from_slice(&v.to_be_bytes()); // top, left, bottom, right
    }
    layer_info.extend_from_slice(&4u16.to_be_bytes()); // channel count
    for id in [-1i16, 0, 1, 2] {
        layer_info.extend_from_slice(&id.to_be_bytes());
        // 2 bytes of compression code + 2 bytes of raw samples
        layer_info.extend_from_slice(&4u32.to_be_bytes());
    }
    layer_info.extend_from_slice(b"8BIM");
    layer_info.extend_from_slice(b"norm");
    layer_info.push(255); // opacity
    layer_info.push(0); // clipping
    layer_info.push(0); // flags: visible, unprotected
    layer_info.push(0); // filler
    let extra: Vec<u8> = {
        let mut e = Vec::new();
        e.extend_from_slice(&0u32.to_be_bytes()); // no layer mask
        e.extend_from_slice(&0u32.to_be_bytes()); // no blending ranges
        e.extend_from_slice(&[2, b'H', b'i', 0]); // Pascal name padded to 4
        e
    };
    layer_info.extend_from_slice(&(extra.len() as u32).to_be_bytes());
    layer_info.extend_from_slice(&extra);
    // Channel data, in the order the channel list declared: A, R, G, B.
    for samples in [[0xFFu8, 0x80], [0x10, 0x20], [0x30, 0x40], [0x50, 0x60]] {
        layer_info.extend_from_slice(&0u16.to_be_bytes()); // raw
        layer_info.extend_from_slice(&samples);
    }
    assert_eq!(layer_info.len() % 2, 0);

    let mut lmi: Vec<u8> = Vec::new();
    lmi.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
    lmi.extend_from_slice(&layer_info);
    lmi.extend_from_slice(&0u32.to_be_bytes()); // global layer mask info
    b.extend_from_slice(&(lmi.len() as u32).to_be_bytes());
    b.extend_from_slice(&lmi);

    // --- Merged composite, raw -------------------------------------------
    b.extend_from_slice(&0u16.to_be_bytes());
    b.extend_from_slice(&[0x10, 0x20]); // R
    b.extend_from_slice(&[0x30, 0x40]); // G
    b.extend_from_slice(&[0x50, 0x60]); // B
    b.extend_from_slice(&[0xFF, 0x80]); // A
    b
}

#[test]
fn a_file_assembled_from_the_specification_reads_correctly() {
    let file = read(&handwritten_psd()).unwrap();
    assert!(file.warnings.is_empty(), "{:?}", file.warnings);
    assert_eq!(file.header.width, 2);
    assert_eq!(file.header.height, 1);
    assert_eq!(file.header.channels, 4);
    assert_eq!(file.header.depth, Depth::Eight);
    assert_eq!(file.header.color_mode, ColorMode::Rgb);

    assert_eq!(file.layers.len(), 1);
    let layer = &file.layers[0];
    assert_eq!(layer.name, "Hi");
    assert_eq!(layer.bounds, Rect::new(0, 0, 2, 1));
    assert_eq!(layer.blend_mode, BlendMode::Normal);
    assert_eq!(layer.opacity, 255);
    assert!(layer.visible);
    assert!(!layer.clipping);
    assert!(layer.mask.is_none());
    assert_eq!(
        layer.rgba8().unwrap(),
        vec![0x10, 0x30, 0x50, 0xFF, 0x20, 0x40, 0x60, 0x80]
    );

    let merged = file.merged.as_ref().unwrap();
    assert_eq!(
        merged.to_rgba8(2, 1).unwrap(),
        vec![0x10, 0x30, 0x50, 0xFF, 0x20, 0x40, 0x60, 0x80]
    );
}

#[test]
fn a_handwritten_file_survives_a_pass_through_this_crates_writer() {
    let original = read(&handwritten_psd()).unwrap();
    let again = read(&write(&original).unwrap()).unwrap();
    assert_eq!(again.layers, original.layers);
    assert_eq!(again.merged, original.merged);
}

#[test]
fn every_truncation_of_a_handwritten_file_errors_or_stops_before_the_composite() {
    let bytes = handwritten_psd();
    for cut in 0..bytes.len() {
        if let Ok(partial) = read(&bytes[..cut]) {
            assert!(partial.merged.is_none(), "prefix {cut} looked complete");
        }
    }
}

#[test]
fn tagged_blocks_padded_to_four_bytes_are_resynchronised_rather_than_abandoned() {
    // The specification pads a tagged block to an even length; Photoshop
    // sometimes pads to four. A reader that trusts only the specification loses
    // every block after the first odd one, silently.
    let mut section: Vec<u8> = Vec::new();
    for (key, body) in [(b"lnsr", &b"bgnd"[..]), (b"lyid", &b"\0\0\0\x07"[..])] {
        section.extend_from_slice(b"8BIM");
        section.extend_from_slice(key);
        // Declare an odd length and then pad by three, not one.
        section.extend_from_slice(&(body.len() as u32 - 1).to_be_bytes());
        section.extend_from_slice(&body[..body.len() - 1]);
        section.extend_from_slice(&[0, 0, 0]);
    }
    let mut warnings = Vec::new();
    let blocks = crate::read::read_tagged_blocks(
        &mut crate::bytes::Cursor::new(&section),
        &ReadOptions::default(),
        &mut warnings,
    )
    .unwrap();
    assert_eq!(blocks.len(), 2, "the second block was lost: {warnings:?}");
    assert_eq!(&blocks[0].key, b"lnsr");
    assert_eq!(&blocks[1].key, b"lyid");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_tagged_block_scan_that_cannot_resynchronise_warns_and_stops() {
    let mut section: Vec<u8> = Vec::new();
    section.extend_from_slice(b"8BIM");
    section.extend_from_slice(b"lnsr");
    section.extend_from_slice(&4u32.to_be_bytes());
    section.extend_from_slice(b"bgnd");
    section.extend_from_slice(&[0xAB; 32]); // noise, not a signature
    let mut warnings = Vec::new();
    let blocks = crate::read::read_tagged_blocks(
        &mut crate::bytes::Cursor::new(&section),
        &ReadOptions::default(),
        &mut warnings,
    )
    .unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

// ------------------------------------------- hostile nesting and geometry

/// One layer record that is nothing but an `lsct` section divider of the given
/// type: type 3 opens a group, types 1 and 2 close one.
///
/// Fifty-eight bytes, no channels, no pixels — the whole cost of one level of
/// group nesting to whoever wrote the file.
fn divider_record_bytes(kind: u32) -> Vec<u8> {
    let mut extra = crate::bytes::Sink::new();
    extra.u32(0); // no layer mask
    extra.u32(0); // no blending ranges
    extra.pascal_string("g", 4);
    extra.tag(b"8BIM");
    extra.tag(b"lsct");
    extra.u32(4);
    extra.u32(kind);
    let extra = extra.into_inner();

    let mut s = crate::bytes::Sink::new();
    for _ in 0..4 {
        s.i32(0); // an empty rectangle, as Photoshop writes for a divider
    }
    s.u16(0); // no channels
    s.tag(b"8BIM");
    s.tag(b"norm");
    s.u8(255); // opacity
    s.u8(0); // not clipping
    s.u8(0b1000); // "bit 4 is meaningful"
    s.u8(0); // filler
    s.u32(extra.len() as u32);
    s.bytes(&extra);
    s.into_inner()
}

/// A `.psd` whose entire layer section is `opens` group-opening dividers
/// followed by `opens` group-closing records — a tower of nesting and nothing
/// else.
fn divider_tower_psd(opens: usize) -> Vec<u8> {
    let mut layer_info = crate::bytes::Sink::new();
    layer_info.i16(-((opens * 2) as i16));
    for _ in 0..opens {
        layer_info.bytes(&divider_record_bytes(3));
    }
    for _ in 0..opens {
        layer_info.bytes(&divider_record_bytes(1));
    }
    let layer_info = layer_info.into_inner();

    let mut s = crate::bytes::Sink::new();
    PsdHeader::rgba8(4, 4).write(&mut s);
    s.u32(0); // colour mode data
    s.u32(0); // image resources
    let lmi = s.begin_len();
    s.u32(layer_info.len() as u32);
    s.bytes(&layer_info);
    s.u32(0); // global layer mask info
    s.end_len_even(lmi);
    s.into_inner()
}

/// A `.psd` that is nothing but a header: no colour mode data, no resources, no
/// layer section, no merged composite. Thirty-eight bytes.
fn header_only_psd(width: u32, height: u32) -> Vec<u8> {
    let mut s = crate::bytes::Sink::new();
    PsdHeader::rgba8(width, height).write(&mut s);
    s.u32(0); // colour mode data
    s.u32(0); // image resources
    s.u32(0); // layer and mask information
    s.into_inner()
}

/// A `.psd` whose single layer record declares `rect` and then lists
/// `channels` channels, each of `channel_len` bytes of payload.
///
/// With `channels: 0` this is the "declares a rectangle, supplies no pixels"
/// record: structurally perfect, every length in it correct, and nothing in it
/// to fill the rectangle with. With `channels: 4` and `channel_len: 0` it is
/// the same hole reached the other way — a channel table whose entries are too
/// short to hold even a compression code.
fn one_record_psd(rect: Rect, channels: u16, channel_len: u32) -> Vec<u8> {
    let mut extra = crate::bytes::Sink::new();
    extra.u32(0); // no layer mask
    extra.u32(0); // no blending ranges
    extra.pascal_string("nochan", 4);
    let extra = extra.into_inner();

    let mut layer_info = crate::bytes::Sink::new();
    layer_info.i16(-1); // one record; negative because the composite has alpha
    layer_info.i32(rect.top);
    layer_info.i32(rect.left);
    layer_info.i32(rect.bottom);
    layer_info.i32(rect.right);
    layer_info.u16(channels);
    for id in [CHANNEL_ALPHA, 0, 1, 2].iter().take(channels as usize) {
        layer_info.i16(*id);
        layer_info.u32(channel_len);
    }
    layer_info.tag(b"8BIM");
    layer_info.tag(b"norm");
    layer_info.u8(255); // opacity
    layer_info.u8(0); // not clipping
    layer_info.u8(0b1000); // "bit 4 is meaningful"
    layer_info.u8(0); // filler
    layer_info.u32(extra.len() as u32);
    layer_info.bytes(&extra);
    for _ in 0..channels {
        layer_info.zeros(channel_len as usize);
    }
    let layer_info = layer_info.into_inner();

    let mut s = crate::bytes::Sink::new();
    PsdHeader::rgba8(8, 8).write(&mut s);
    s.u32(0); // colour mode data
    s.u32(0); // image resources
    let lmi = s.begin_len();
    s.u32(layer_info.len() as u32);
    s.bytes(&layer_info);
    s.u32(0); // global layer mask info
    s.end_len_even(lmi);
    s.into_inner()
}

/// A record may declare a rectangle and then supply nothing to put in it. Left
/// alone that reads cleanly and *then* makes `write` refuse the whole document
/// with an `InvalidDocument`, whose `is_file_fault` is false — the crate
/// blaming its caller for a defect in somebody else's file. The reader empties
/// the rectangle instead, exactly as it drops a mask whose channel never
/// arrived.
#[test]
fn a_record_that_declares_a_rectangle_but_no_pixels_stays_writable() {
    // Both routes to the same hole: no channel table at all, and a channel
    // table whose entries are too short to hold a compression code.
    for (channels, channel_len) in [(0u16, 0u32), (4, 0), (4, 1)] {
        let bytes = one_record_psd(Rect::sized(4, 4), channels, channel_len);
        let back = read(&bytes).unwrap_or_else(|e| {
            panic!("({channels}, {channel_len}) should read: {e}");
        });
        assert_eq!(back.layers.len(), 1);
        let layer = &back.layers[0];
        assert_eq!(layer.name, "nochan");
        assert!(layer.channels.is_empty(), "({channels}, {channel_len})");
        assert!(
            layer.bounds.is_empty(),
            "({channels}, {channel_len}) kept a {}x{} rectangle it cannot fill",
            layer.bounds.width(),
            layer.bounds.height()
        );
        assert!(
            back.warnings
                .iter()
                .any(|w| w.contains("no channel data to fill it")),
            "({channels}, {channel_len}) emptied the rectangle silently: {:?}",
            back.warnings
        );

        // The whole point: the document the reader handed back can be saved.
        let again = write(&back)
            .unwrap_or_else(|e| panic!("({channels}, {channel_len}) is unwritable: {e}"));
        let back2 = read(&again).unwrap();
        assert_eq!(back2.layers.len(), 1);
        assert_eq!(back2.layers[0].name, "nochan");
        assert!(back2.layers[0].bounds.is_empty());
    }
}

/// The same rule, stated once for every damaged file the fuzzers can build:
/// **a document the reader accepts is one the writer accepts.** If `write` does
/// refuse it, the refusal must blame the file, never the caller — an
/// `InvalidDocument` here means the reader let through something it should have
/// repaired or refused itself.
///
/// Damaging one whole file is not enough to state that rule over the crate, and
/// the two repairs this test covers show why. The zero-edge canvas of
/// [`crate::header::PsdHeader::read`] is reachable by flipping bits in a file
/// this crate wrote, and mutation over `rich_document()` alone does make this
/// test red when that guard is removed. A record that declares a rectangle
/// above an **empty channel table** is not: the channel count is what says how
/// long the channel table is, so a flip that empties the count leaves the table
/// bytes behind and the record desynchronises rather than arriving empty, and
/// the read fails before `write_with` sees the layer. Measured rather than
/// assumed — with only the `rich_document()` seed the counter below reaches
/// that repair **zero** times — so the class had to be thought of and
/// hand-built (see
/// [`a_record_that_declares_a_rectangle_but_no_pixels_stays_writable`]). Those
/// records are seeded into the corpus here and mutated in turn, and the
/// coverage assertion at the end fails if a later change stops them arriving,
/// so the property covers both halves of the rule rather than looking as though
/// it does.
#[test]
fn every_corruption_the_reader_accepts_produces_a_document_the_writer_accepts() {
    // Seed one: everything the writer emits, which is where header- and
    // section-level corruption comes from. Then hand-built layer records —
    // a well-formed one (four raw 4x4 channels, two bytes of code and sixteen
    // of samples each) and the three shapes of missing channel table that
    // `empty_the_rectangle_without_pixels` repairs.
    let mut seeds = vec![write(&rich_document()).unwrap()];
    for (channels, channel_len) in [(4u16, 18u32), (0, 0), (4, 0), (4, 1)] {
        seeds.push(one_record_psd(Rect::sized(4, 4), channels, channel_len));
    }

    let opts = ReadOptions {
        // A small budget so a corrupted size field cannot make the test slow.
        max_decoded_bytes: 8 << 20,
        ..Default::default()
    };
    let mut accepted = 0usize;
    let mut written = 0usize;
    let mut repaired = 0usize;
    // Each seed undamaged, then byte flips, then single-bit flips over the
    // header and section lengths, then every truncation: four shapes of damage.
    let mut corpus: Vec<Vec<u8>> = Vec::new();
    for original in &seeds {
        corpus.push(original.clone());
        for i in 0..original.len() {
            let mut bytes = original.clone();
            bytes[i] ^= 0xFF;
            corpus.push(bytes);
        }
        for i in 0..original.len().min(96) {
            for bit in 0..8 {
                let mut bytes = original.clone();
                bytes[i] ^= 1 << bit;
                corpus.push(bytes);
            }
        }
        for cut in 0..original.len() {
            corpus.push(original[..cut].to_vec());
        }
    }

    for bytes in &corpus {
        let Ok(file) = read_with(bytes, &opts) else {
            continue;
        };
        accepted += 1;
        if file
            .warnings
            .iter()
            .any(|w| w.contains("no channel data to fill it"))
        {
            repaired += 1;
        }
        match write_with(&file, &WriteOptions::default()) {
            Ok(_) => written += 1,
            Err(e) => {
                assert!(
                    e.is_file_fault(),
                    "the reader accepted a document the writer blames the caller for: {e}"
                );
            }
        }
    }
    assert!(
        accepted > 50 && written > 50,
        "the corpus is not exercising the property: {accepted} read, {written} written"
    );
    assert!(
        repaired >= 3,
        "the corpus stopped reaching the empty-channel-table repair, so this \
         test no longer covers the record half of the rule ({repaired} reached it)"
    );
}

#[test]
fn a_tower_of_group_dividers_is_refused_by_depth_rather_than_aborting() {
    // Four thousand levels of nesting in under half a megabyte. A group costs
    // two records, so the default `max_layers` of 8192 reaches depth 4096 — the
    // *default* configuration is the reachable one. Before the depth cap this
    // parsed with zero warnings, and `write` and `flatten` on the result each
    // killed the process with STATUS_STACK_OVERFLOW, which is an abort no
    // caller can catch.
    let bytes = divider_tower_psd(4_000);
    assert!(
        bytes.len() > 400_000 && bytes.len() < 600_000,
        "{} bytes",
        bytes.len()
    );
    let err = read(&bytes).unwrap_err();
    assert!(matches!(err, PsdError::GroupTooDeep { max: 64 }), "{err}");
    assert!(err.is_file_fault(), "this is the file's fault, not ours");
}

#[test]
fn nesting_at_the_configured_limit_is_kept_and_one_level_past_it_is_refused() {
    let bytes = divider_tower_psd(4);

    let at_limit = ReadOptions {
        max_group_depth: 4,
        ..Default::default()
    };
    let file = read_with(&bytes, &at_limit).unwrap();
    assert!(file.warnings.is_empty(), "{:?}", file.warnings);
    // One chain of four groups, innermost empty.
    assert_eq!(file.layers.len(), 1);
    assert_eq!(file.all_layers().len(), 4);
    assert_eq!(file.record_count(), 8);
    let mut depth = 0;
    let mut level = file.layers.as_slice();
    while let Some(layer) = level.first() {
        assert!(layer.is_group(), "level {depth} is not a group");
        depth += 1;
        level = layer.children();
    }
    assert_eq!(depth, 4, "the hierarchy is four deep");

    let one_short = ReadOptions {
        max_group_depth: 3,
        ..Default::default()
    };
    let err = read_with(&bytes, &one_short).unwrap_err();
    assert!(matches!(err, PsdError::GroupTooDeep { max: 3 }), "{err}");

    // The default is generous enough that ordinary nesting is untouched.
    assert_eq!(ReadOptions::default().max_group_depth, 64);
    assert!(read(&bytes).is_ok());
}

#[test]
fn a_tree_deeper_than_the_reader_allows_can_still_be_written_flattened_and_dropped() {
    // `PsdFile` is public, so a caller can assemble a tree the reader would
    // never build. Every consumer of it walks with an explicit stack, so this
    // returns rather than aborting — and the file it produces is then refused
    // by the reader's depth cap rather than blowing up the next program.
    // 4000 groups is 8000 records, just inside the default `max_layers` of
    // 8192 — so the file that comes out is refused for its *depth*, which is
    // the point, rather than for its record count.
    const DEPTH: usize = 4_000;
    let mut node = PsdLayer::group("innermost");
    for i in 1..DEPTH {
        let mut parent = PsdLayer::group(format!("g{i}"));
        parent.push_child(node).unwrap();
        node = parent;
    }
    let mut file = PsdFile::new(PsdHeader::rgba8(1, 1));
    file.layers.push(node);

    // The fallback flattener recurses once per isolated group level too.
    let merged = crate::flatten(&file).unwrap();
    assert_eq!(merged.channels.len(), 4);

    let bytes = write(&file).unwrap();
    let err = read(&bytes).unwrap_err();
    assert!(matches!(err, PsdError::GroupTooDeep { max: 64 }), "{err}");
    // The drop at the end of this function is the last recursion there was.
}

#[test]
fn a_thirty_eight_byte_file_cannot_make_the_writer_allocate_its_declared_canvas() {
    // 30 000 is exactly the largest canvas edge the reader accepts, so this
    // parses. Nine hundred million pixels at sixteen bytes of working canvas
    // each is 14.4 GB — asked for by thirty-eight bytes of input, because the
    // document has no composite of its own and `write` falls back to
    // flattening. `check_header` only bounds the edge; it never bounded the
    // product.
    let bytes = header_only_psd(30_000, 30_000);
    assert_eq!(bytes.len(), 38);
    let file = read(&bytes).unwrap();
    assert!(file.merged.is_none());
    assert!(file.layers.is_empty());
    assert_eq!(file.header.width, 30_000);

    let (result, allocated) = crate::probe::bytes_allocated_by(|| write(&file));
    let err = result.unwrap_err();
    assert!(matches!(err, PsdError::BudgetExhausted { .. }), "{err}");
    assert!(
        allocated < (1 << 20),
        "the refusal still reserved {allocated} bytes"
    );

    // A canvas that fits the ceiling still writes.
    let small = read(&header_only_psd(64, 64)).unwrap();
    assert!(write(&small).is_ok());

    // And the ceiling `write` uses really is the one in `WriteOptions`: the
    // same tiny document is refused when the caller lowers it. (Asserted on a
    // 64 x 64 canvas rather than the 30 000 x 30 000 one so that a regression
    // here fails fast instead of reserving fourteen gigabytes.)
    let tight = WriteOptions {
        max_flatten_bytes: 1024,
        ..Default::default()
    };
    let err = write_with(&small, &tight).unwrap_err();
    assert!(matches!(err, PsdError::BudgetExhausted { .. }), "{err}");
}

#[test]
fn flattening_a_header_too_large_to_index_errors_instead_of_panicking() {
    // `flatten` is `pub` and re-exported at the crate root, so this is reachable
    // without any file at all: `vec![[0.0; 3]; u32::MAX * u32::MAX]` is a
    // "capacity overflow" panic in `raw_vec`, not a `Result`.
    let file = PsdFile::new(PsdHeader::rgba8(u32::MAX, u32::MAX));
    let (result, allocated) = crate::probe::bytes_allocated_by(|| crate::flatten(&file));
    let err = result.unwrap_err();
    assert!(matches!(err, PsdError::Overflow { .. }), "{err}");
    assert!(allocated < 4096, "{allocated} bytes");

    // And a size that fits `usize` but not memory is a budget refusal.
    let file = PsdFile::new(PsdHeader::rgba8(30_000, 30_000));
    let (result, allocated) = crate::probe::bytes_allocated_by(|| crate::flatten(&file));
    assert!(
        matches!(result.unwrap_err(), PsdError::BudgetExhausted { .. }),
        "a 14.4 GB canvas was not refused"
    );
    assert!(allocated < 4096, "{allocated} bytes");

    // The ceiling is a ceiling, not a blanket refusal: the same canvas at a
    // size that fits still flattens. (Deliberately not the 30 000 × 30 000 one
    // with a raised budget — asserting that would mean actually reserving and
    // touching 14.4 GB in a unit test.)
    let ordinary = PsdFile::new(PsdHeader::rgba8(256, 256));
    assert!(crate::flatten(&ordinary).is_ok());
    assert!(crate::flatten_with(&ordinary, 1 << 24).is_ok());
    assert!(matches!(
        crate::flatten_with(&ordinary, 1024).unwrap_err(),
        PsdError::BudgetExhausted { .. }
    ));
}

#[test]
fn the_flatten_budget_bounds_memory_held_at_once_not_memory_ever_used() {
    // A 100 × 100 canvas costs 160 000 bytes of working space, and each
    // isolated group needs another one. Ten groups side by side hold two at a
    // time; ten nested inside one another hold eleven. A budget between the two
    // must accept the first and refuse the second — otherwise the choice is
    // between rejecting ordinary documents and letting two hundred nested
    // groups on a 4000 × 4000 canvas ask for fifty gigabytes.
    let header = PsdHeader::rgba8(100, 100);
    let budget = 600_000;

    let mut siblings = PsdFile::new(header);
    for i in 0..10 {
        siblings.layers.push(PsdLayer::group(format!("g{i}")));
    }
    assert!(
        crate::flatten_with(&siblings, budget).is_ok(),
        "ten groups side by side hold only two canvases at once"
    );

    let mut nested = PsdFile::new(header);
    let mut node = PsdLayer::group("g0");
    for i in 1..10 {
        let mut parent = PsdLayer::group(format!("g{i}"));
        parent.push_child(node).unwrap();
        node = parent;
    }
    nested.layers.push(node);
    let err = crate::flatten_with(&nested, budget).unwrap_err();
    assert!(matches!(err, PsdError::BudgetExhausted { .. }), "{err}");

    // A pass-through group borrows its parent's canvas instead of allocating
    // one, so the same nesting costs nothing extra.
    let mut pass = PsdFile::new(header);
    let mut node = PsdLayer::group("p0");
    node.group_data_mut().unwrap().pass_through = true;
    for i in 1..10 {
        let mut parent = PsdLayer::group(format!("p{i}"));
        parent.group_data_mut().unwrap().pass_through = true;
        parent.push_child(node).unwrap();
        node = parent;
    }
    pass.layers.push(node);
    assert!(crate::flatten_with(&pass, budget).is_ok());
}

#[test]
fn a_non_ascii_image_resource_name_survives_repeated_save_cycles() {
    // A resource name has no `luni` counterpart anywhere in the format, so this
    // Pascal string is the only copy of the name. When the writer emitted UTF-8
    // and the reader decoded Latin-1, "café" came back as "cafÃ©" — and the
    // mojibake was re-encoded on the next save, so the damage compounded. Three
    // cycles make a one-shot coincidence impossible.
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    file.resources.push(resolution_info(72.0));
    for name in ["café", "日本語プロファイル", "Grüße", "plain"] {
        file.resources.push(ImageResource {
            id: 1039,
            name: name.into(),
            data: vec![1, 2, 3],
        });
    }
    file.layers.push(raster("l", Rect::sized(2, 2), 1));

    let mut current = file.clone();
    for cycle in 0..3 {
        current = read(&write(&current).unwrap()).unwrap();
        assert_eq!(
            current.resources, file.resources,
            "resource names drifted on cycle {cycle}"
        );
    }
}

// ----------------------------------------------------------------- helpers

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Offset of the `i16` layer count inside a file this crate wrote.
fn layer_count_offset(bytes: &[u8]) -> usize {
    let mut cur = crate::bytes::Cursor::new(bytes);
    cur.skip(26).unwrap();
    let cmd = cur.u32().unwrap() as usize;
    cur.skip(cmd).unwrap();
    let res = cur.u32().unwrap() as usize;
    cur.skip(res).unwrap();
    let _lmi = cur.u32().unwrap();
    let _layer_info_len = cur.u32().unwrap();
    cur.offset()
}

/// Rewrite the id of the first channel of the first layer record whose id is
/// `from`, leaving every length in the file untouched.
///
/// Renaming a channel rather than deleting it is what makes the "declared but
/// absent" fixtures possible: the payload stays where the record says it is, so
/// the file is structurally perfect and the only thing wrong with it is that
/// the channel the mask needs is no longer there.
fn patch_first_channel_id(bytes: &mut [u8], from: i16, to: i16) -> bool {
    // count(2) + rect(16), then the channel table: u16 count, then (i16, u32).
    let base = layer_count_offset(bytes) + 2 + 16;
    let n = u16::from_be_bytes([bytes[base], bytes[base + 1]]) as usize;
    for i in 0..n {
        let at = base + 2 + i * 6;
        if i16::from_be_bytes([bytes[at], bytes[at + 1]]) == from {
            bytes[at..at + 2].copy_from_slice(&to.to_be_bytes());
            return true;
        }
    }
    false
}

/// Offset of the first channel's `u32` length in the first layer record.
fn first_channel_length_offset(bytes: &[u8]) -> usize {
    // count(2) + rect(16) + channel count(2) + channel id(2)
    layer_count_offset(bytes) + 2 + 16 + 2 + 2
}

/// A `TySh` payload shaped the way Photoshop writes one.
fn tysh_fixture(text: &str) -> Vec<u8> {
    let mut s = crate::bytes::Sink::new();
    s.u16(1);
    for v in [1.0, 0.0, 0.0, 1.0, 3.5, 9.0] {
        s.f64(v);
    }
    s.u16(50);
    s.u32(16);
    let mut d = crate::Descriptor::new("TxLr");
    d.push("Txt ", crate::Value::from(text)).unwrap();
    d.push("EngineData", crate::Value::RawData(b"<< /x 1 >>".to_vec()))
        .unwrap();
    d.write(&mut s).unwrap();
    s.u16(1);
    s.u32(16);
    crate::Descriptor::new("warp").write(&mut s).unwrap();
    s.i32(0);
    s.i32(0);
    s.i32(100);
    s.i32(20);
    s.into_inner()
}

#[test]
fn the_layer_count_offset_helper_points_at_the_layer_count() {
    let file = rich_document();
    let bytes = write(&file).unwrap();
    let at = layer_count_offset(&bytes);
    let count = i16::from_be_bytes([bytes[at], bytes[at + 1]]);
    assert_eq!(
        count.unsigned_abs() as usize,
        file.record_count(),
        "the helper must point at the real count or the corruption tests are vacuous"
    );
    assert!(
        count < 0,
        "an alpha-bearing document writes a negative count"
    );
}

#[test]
fn the_first_channel_length_helper_points_at_a_plausible_length() {
    let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
    file.layers.push(raster("l", Rect::sized(2, 2), 1));
    let bytes = write(&file).unwrap();
    let at = first_channel_length_offset(&bytes);
    let len = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    // Two bytes of compression code plus a PackBits encoding of four pixels.
    assert!((3..64).contains(&len), "length looked wrong: {len}");
}
