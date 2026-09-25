//! W13X-8: Sketch, XD and Figma files read as layers.

use std::io::Write;

use super::*;
use crate::codec::{encode, ExportFormat};

/// The shared fixtures (`app-shell`'s tests include the same file).
#[path = "design_fixtures.rs"]
pub mod fixtures;

use fixtures::build_zip;

/// A 4x3 solid PNG.
pub(crate) fn png(w: u32, h: u32, c: [u8; 4]) -> Vec<u8> {
    let px: Vec<u8> = (0..w * h).flat_map(|_| c).collect();
    encode(ExportFormat::Png, w, h, &px).unwrap()
}

fn image() -> Vec<u8> {
    png(4, 3, [0, 200, 0, 255])
}

fn sketch_file() -> Vec<u8> {
    fixtures::sketch_file(&image())
}

fn xd_file() -> Vec<u8> {
    fixtures::xd_file(&image())
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

fn zstd(bytes: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(bytes, ruzstd::encoding::CompressionLevel::Fastest)
}

/// The Figma archive; its message chunk Zstandard-compressed when `zstd`.
fn fig_file(zstd_chunk: bool) -> Vec<u8> {
    if zstd_chunk {
        fixtures::fig_file(&image(), &zstd)
    } else {
        fixtures::fig_file(&image(), &deflate)
    }
}

fn fig_canvas() -> Vec<u8> {
    fixtures::fig_canvas(&deflate)
}

/// The one artboard, and its three children in paint order.
fn board_of_three(doc: &DesignDocument) -> (&DesignNode, [&DesignNode; 3]) {
    assert_eq!(doc.nodes.len(), 1, "one top-level node: {:?}", doc.nodes);
    let board = &doc.nodes[0];
    let DesignKind::Artboard {
        background,
        children,
    } = &board.kind
    else {
        panic!("an artboard, got {:?}", board.kind)
    };
    assert_eq!(*background, Some([1.0, 1.0, 1.0, 1.0]));
    assert_eq!(board.name, "Board");
    assert_eq!(board.transform, Affine::translate(100.0, 50.0));
    assert_eq!((board.width, board.height), (200.0, 100.0));
    assert_eq!(children.len(), 3, "rectangle, text, image: {children:?}");
    (board, [&children[0], &children[1], &children[2]])
}

fn assert_three_layers(doc: &DesignDocument, text_origin: (f64, f64)) {
    let (_, [rect, text, image]) = board_of_three(doc);

    assert_eq!(rect.name, "Box");
    assert_eq!(rect.transform.apply(0.0, 0.0), (110.0, 60.0));
    assert_eq!(rect.opacity, 0.5);
    let DesignKind::Shape {
        path_svg,
        fill,
        stroke,
        ..
    } = &rect.kind
    else {
        panic!("a shape, got {:?}", rect.kind)
    };
    assert_eq!(*fill, Some([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(
        stroke.as_ref().map(|s| (s.color, s.width, s.align)),
        Some(([0.0, 0.0, 1.0, 1.0], 2.0, StrokeAlign::Inside))
    );
    // The path spans the 40x20 box.
    for corner in ["0 0", "40 0", "40 20", "0 20"] {
        assert!(path_svg.contains(corner), "{path_svg} lacks {corner}");
    }

    assert_eq!(text.name, "Title");
    let (tx, ty) = text.transform.apply(0.0, 0.0);
    assert!(
        (tx - text_origin.0).abs() < 1e-6 && (ty - text_origin.1).abs() < 1e-6,
        "text at ({tx}, {ty})"
    );
    let DesignKind::Text {
        text: string,
        font_family,
        bold,
        size,
        color,
        ..
    } = &text.kind
    else {
        panic!("text, got {:?}", text.kind)
    };
    assert_eq!(
        (string.as_str(), font_family.as_str(), *bold, *size, *color),
        ("Hello", "Helvetica", true, 24.0, [0.0, 0.0, 1.0, 1.0])
    );

    assert_eq!(image.name, "Photo");
    assert_eq!(image.transform.apply(0.0, 0.0), (110.0, 90.0));
    let DesignKind::Bitmap {
        width,
        height,
        rgba,
    } = &image.kind
    else {
        panic!("a bitmap, got {:?}", image.kind)
    };
    assert_eq!((*width, *height), (4, 3));
    assert_eq!(&rgba[..4], &[0, 200, 0, 255]);
}

#[test]
fn a_sketch_file_opens_as_a_rectangle_a_text_and_an_image_inside_an_artboard() {
    let doc = read_design(
        ImportFormat::Sketch,
        &sketch_file(),
        ImportLimits::default(),
    )
    .expect("the Sketch file reads");
    assert_eq!(doc.format, ImportFormat::Sketch);
    assert_eq!(doc.opened, "page \"Page 1\"");
    assert_three_layers(&doc, (160.0, 60.0));
    // What did not map is reported, not dropped silently.
    let notes = doc.notes.join("\n");
    assert!(notes.contains("\"Box\" has a drop shadow"), "{notes}");
    assert!(
        notes.contains("symbol instance \"Button\" was not expanded"),
        "{notes}"
    );
}

#[test]
fn an_xd_file_opens_as_a_rectangle_a_text_and_an_image_inside_an_artboard() {
    let doc = read_design(ImportFormat::Xd, &xd_file(), ImportLimits::default())
        .expect("the XD file reads");
    assert_eq!(doc.format, ImportFormat::Xd);
    // XD places point text on its first baseline (y 34 in the board); the
    // layout box starts 0.8 em (19.2 px) above it.
    assert_three_layers(&doc, (160.0, 50.0 + 34.0 - 19.2));
    let notes = doc.notes.join("\n");
    assert!(notes.contains("\"Box\" has a dropShadow effect"), "{notes}");
    assert!(
        notes.contains("component \"Button\" was not expanded"),
        "{notes}"
    );
}

#[test]
fn sketch_gradients_rotation_and_extra_pages_are_reported() {
    let page = r#"{"_class":"page","name":"First","layers":[
      {"_class":"oval","name":"Dot","frame":{"x":0,"y":0,"width":10,"height":10},
       "style":{"fills":[{"isEnabled":true,"fillType":1},{"isEnabled":true,"fillType":0,"color":{"red":0,"green":1,"blue":0,"alpha":1}}]}},
      {"_class":"group","name":"Stack","frame":{"x":5,"y":5,"width":10,"height":10},"layers":[
        {"_class":"weirdLayer","name":"Mystery"}]}]}"#;
    let zip = build_zip(&[
        (
            "document.json",
            br#"{"pages":[{"_ref":"pages/A"},{"_ref":"pages/B"}]}"#,
        ),
        ("pages/A.json", page.as_bytes()),
        ("pages/B.json", br#"{"layers":[]}"#),
    ]);
    let doc = read_sketch(&zip, ImportLimits::default()).unwrap();
    assert_eq!(doc.nodes.len(), 2);
    let DesignKind::Shape { fill, path_svg, .. } = &doc.nodes[0].kind else {
        panic!("{:?}", doc.nodes[0].kind)
    };
    // The top-most (last) fill is the flat green one.
    assert_eq!(*fill, Some([0.0, 1.0, 0.0, 1.0]));
    assert!(
        path_svg.starts_with('M') && path_svg.contains('C'),
        "{path_svg}"
    );
    let notes = doc.notes.join("\n");
    assert!(notes.contains("\"Dot\" has 2 fills"), "{notes}");
    assert!(notes.contains("only the first page (\"First\")"), "{notes}");
    assert!(
        notes.contains("\"Mystery\" is a Sketch \"weirdLayer\""),
        "{notes}"
    );
}

#[test]
fn malformed_design_files_error_and_never_panic() {
    let limits = ImportLimits::default();
    // Malformed JSON is an error, by name.
    for bad in [
        &b"{\"pages\": ["[..],
        b"{\"a\" 1}",
        b"[1, 2,]",
        b"\"\\u12\"",
        b"{} trailing",
        b"",
    ] {
        assert!(parse_json(bad, "document.json").is_err(), "{bad:?}");
    }
    let deep = "[".repeat(10_000);
    assert!(parse_json(deep.as_bytes(), "deep").is_err());
    let broken = build_zip(&[("document.json", b"{\"pages\": [")]);
    let err = read_design(ImportFormat::Sketch, &broken, limits).unwrap_err();
    assert!(
        err.to_string().contains("document.json is not valid JSON"),
        "{err}"
    );
    let broken_xd = build_zip(&[
        ("mimetype", b"application/vnd.adobe.sparkler.project+dcxucf"),
        (
            "artwork/artboard-1/graphics/graphicContent.agc",
            b"{\"children\": [}",
        ),
    ]);
    assert!(read_design(ImportFormat::Xd, &broken_xd, limits).is_err());
    // Every truncation and a sweep of flipped bytes: errors or documents,
    // never a panic.
    for (format, bytes) in [
        (ImportFormat::Sketch, sketch_file()),
        (ImportFormat::Xd, xd_file()),
        (ImportFormat::Fig, fig_file(false)),
    ] {
        for cut in (0..bytes.len()).step_by(7) {
            let _ = read_design(format, &bytes[..cut], limits);
        }
        let mut flipped = bytes.clone();
        for i in (0..flipped.len()).step_by(5) {
            flipped[i] ^= 0x5A;
            let _ = read_design(format, &flipped, limits);
            flipped[i] ^= 0x5A;
        }
    }
    // Absurd nesting is refused by the layer-depth ceiling.
    let mut nested = String::from(r#"{"_class":"rectangle","name":"x"}"#);
    for _ in 0..(MAX_LAYER_DEPTH + 2) {
        nested = format!(r#"{{"_class":"group","name":"g","layers":[{nested}]}}"#);
    }
    let page = format!(r#"{{"layers":[{nested}]}}"#);
    let zip = build_zip(&[
        ("document.json", br#"{"pages":[{"_ref":"pages/A"}]}"#),
        ("pages/A.json", page.as_bytes()),
    ]);
    assert!(matches!(
        read_sketch(&zip, limits),
        Err(CodecError::LimitExceeded(_))
    ));
}

#[test]
fn a_figma_file_opens_as_a_rectangle_a_text_and_an_image_inside_an_artboard() {
    // Raw DEFLATE (older files), Zstandard (newer) and stored DEFLATE
    // blocks (what app-shell's tests write) all read.
    let stored = fixtures::fig_file(&image(), &fixtures::stored_deflate);
    for bytes in [fig_file(false), fig_file(true), stored] {
        let doc = read_design(ImportFormat::Fig, &bytes, ImportLimits::default())
            .expect("the Figma file reads");
        assert_eq!(doc.format, ImportFormat::Fig);
        assert_eq!(doc.opened, "page \"Page 1\"");
        // This file gives the rectangle no stroke (the Sketch / XD files'
        // inside border), so the three layers are checked here.
        let (_, [rect, text, image]) = board_of_three(&doc);
        let DesignKind::Shape { path_svg, .. } = &rect.kind else {
            panic!("{:?}", rect.kind)
        };
        assert!(path_svg.contains("40 20"), "{path_svg}");
        assert_eq!(rect.opacity, 0.5);
        assert_eq!(rect.transform.apply(0.0, 0.0), (110.0, 60.0));
        assert!(matches!(
            &rect.kind,
            DesignKind::Shape {
                fill: Some([1.0, 0.0, 0.0, 1.0]),
                stroke: None,
                ..
            }
        ));
        assert_eq!(text.transform.apply(0.0, 0.0), (160.0, 60.0));
        assert!(matches!(
            &text.kind,
            DesignKind::Text { text, font_family, bold: true, size, color: [0.0, 0.0, 1.0, 1.0], .. }
                if text == "Hello" && font_family == "Helvetica" && *size == 24.0
        ));
        assert_eq!(image.transform.apply(0.0, 0.0), (110.0, 90.0));
        assert!(matches!(
            &image.kind,
            DesignKind::Bitmap { width: 4, height: 3, rgba } if rgba[..4] == [0, 200, 0, 255]
        ));
        let notes = doc.notes.join("\n");
        assert!(
            notes.contains("component instance \"Button\" was not expanded"),
            "{notes}"
        );
    }
    // A bare canvas reads too; its image is not in it, and it says so.
    let doc = read_design(ImportFormat::Fig, &fig_canvas(), ImportLimits::default()).unwrap();
    let board = &doc.nodes[0];
    assert_eq!(board.children().len(), 2);
    let notes = doc.notes.join("\n");
    // The whole sentence, so a broken string continuation shows.
    assert!(
        doc.notes.iter().any(|n| n
            == "image \"Photo\" was left out: a bare fig-kiwi canvas does not hold its images \
                (they travel beside it in the .fig archive)"),
        "{notes}"
    );
}

#[test]
fn sketch_and_xd_blend_modes_and_masks_are_reported_not_dropped() {
    let page = r#"{"_class":"page","name":"First","layers":[
      {"_class":"rectangle","name":"Mask","hasClippingMask":true,
       "frame":{"x":0,"y":0,"width":10,"height":10}},
      {"_class":"rectangle","name":"Shade","frame":{"x":0,"y":0,"width":10,"height":10},
       "style":{"contextSettings":{"blendMode":2,"opacity":1}}},
      {"_class":"rectangle","name":"Tint","frame":{"x":0,"y":0,"width":10,"height":10},
       "style":{"contextSettings":{"blendMode":0,"opacity":1},
                "fills":[{"isEnabled":true,"fillType":0,"color":{"red":1,"green":0,"blue":0,"alpha":1},
                          "contextSettings":{"blendMode":5,"opacity":1}}]}},
      {"_class":"rectangle","name":"Plain","frame":{"x":0,"y":0,"width":10,"height":10},
       "style":{"contextSettings":{"blendMode":0,"opacity":1}}}]}"#;
    let zip = build_zip(&[
        ("document.json", br#"{"pages":[{"_ref":"pages/A"}]}"#),
        ("pages/A.json", page.as_bytes()),
    ]);
    let doc = read_sketch(&zip, ImportLimits::default()).unwrap();
    assert_eq!(doc.nodes.len(), 4);
    let notes = doc.notes.join("\n");
    for sentence in [
        "layer \"Mask\" is a clipping mask, which was not kept: it opened as an ordinary layer \
         and the layers above it in its group opened unclipped",
        "layer \"Shade\" has the multiply blend mode, which was not kept (it opened as Normal)",
        "layer \"Tint\" has a fill with the screen blend mode, which was not kept (it opened as \
         Normal)",
    ] {
        assert!(
            doc.notes.iter().any(|n| n == sentence),
            "{sentence}\n--\n{notes}"
        );
    }
    assert!(!notes.contains("\"Plain\""), "{notes}");
    assert_eq!(doc.notes.len(), 3, "{notes}");

    let artwork = r#"{"version":"1.5.0","children":[
      {"type":"group","name":"Clipped","mask":{"type":"shape","shape":{"type":"rect","x":0,"y":0,"width":5,"height":5}},
       "group":{"children":[
         {"type":"shape","name":"Shade","style":{"blendMode":"multiply"},
          "shape":{"type":"rect","x":0,"y":0,"width":4,"height":3}}]}}]}"#;
    let xd = build_zip(&[
        ("mimetype", b"application/vnd.adobe.sparkler.project+dcxucf"),
        ("manifest", br#"{"name":"t","children":[]}"#),
        (
            "resources/graphics/graphicContent.agc",
            br#"{"resources":{},"artboards":{}}"#,
        ),
        (
            "artwork/pasteboard/graphics/graphicContent.agc",
            artwork.as_bytes(),
        ),
    ]);
    let doc = read_xd(&xd, ImportLimits::default()).unwrap();
    let notes = doc.notes.join("\n");
    assert!(
        doc.notes
            .iter()
            .any(|n| n
                == "group \"Clipped\" is masked, which was not kept: its layers opened unclipped"),
        "{notes}"
    );
    assert!(
        doc.notes
            .iter()
            .any(|n| n == "layer \"Shade\" has a blend mode, which opened as Normal"),
        "{notes}"
    );
}

#[test]
fn figma_blend_modes_and_masks_are_reported_not_dropped() {
    use super::super::design_fig::read_message;
    let g = |l: u32| format!(r#"{{"sessionID":0,"localID":{l}}}"#);
    let child = |l: u32, pos: &str, rest: &str| {
        format!(
            r#"{{"guid":{},"parentIndex":{{"guid":{},"position":"{pos}"}},{rest}}}"#,
            g(l),
            g(1)
        )
    };
    let message = format!(
        r#"{{"nodeChanges":[
          {{"guid":{},"type":"DOCUMENT","name":"Document"}},
          {{"guid":{},"parentIndex":{{"guid":{},"position":"!"}},"type":"CANVAS","name":"Page 1"}},
          {},{},{},{}]}}"#,
        g(0),
        g(1),
        g(0),
        child(
            2,
            "a",
            r#""type":"RECTANGLE","name":"Mask","isMask":true,"size":{"x":10,"y":10}"#
        ),
        child(
            3,
            "b",
            r#""type":"RECTANGLE","name":"Shade","blendMode":"MULTIPLY","size":{"x":10,"y":10}"#
        ),
        child(
            4,
            "c",
            r#""type":"RECTANGLE","name":"Tint","size":{"x":10,"y":10},"fillPaints":[{"type":"SOLID","color":{"r":1,"g":0,"b":0,"a":1},"blendMode":"COLOR_DODGE"}]"#
        ),
        child(
            5,
            "d",
            r#""type":"GROUP","name":"Folder","blendMode":"PASS_THROUGH""#
        ),
    );
    let message = parse_json(message.as_bytes(), "message").unwrap();
    let doc = read_message(&message, &[], &[], ImportLimits::default()).unwrap();
    assert_eq!(doc.nodes.len(), 4);
    let notes = doc.notes.join("\n");
    for sentence in [
        "layer \"Mask\" is a mask, which was not kept: it opened as an ordinary layer and the \
         layers above it in its group opened unclipped",
        "layer \"Shade\" has the multiply blend mode, which was not kept (it opened as Normal)",
        "layer \"Tint\" has a fill with the color dodge blend mode, which was not kept (it opened \
         as Normal)",
    ] {
        assert!(
            doc.notes.iter().any(|n| n == sentence),
            "{sentence}\n--\n{notes}"
        );
    }
    // Pass-through is a group's ordinary mode: nothing to report.
    assert!(!notes.contains("\"Folder\""), "{notes}");
    assert_eq!(doc.notes.len(), 3, "{notes}");
}

/// A kiwi schema whose one struct has a single `bool` field named with
/// `name_len` bytes, and a message holding `count` of those structs: each
/// one byte of message, each decoding to a key of `name_len` bytes.
fn kiwi_probe(name_len: usize, count: u32) -> (Vec<u8>, Vec<u8>) {
    let name = "n".repeat(name_len);
    let fields: [fixtures::FieldSpec; 1] = [(name.as_str(), "bool", false, 1)];
    let schema = fixtures::schema(&[
        ("S", 1, &fields),
        ("Message", 2, &[("items", "S", true, 1)]),
    ]);
    let mut message = Vec::new();
    fixtures::var_uint(&mut message, 1);
    fixtures::var_uint(&mut message, count);
    message.extend(std::iter::repeat_n(1u8, count as usize));
    message.push(0);
    (schema, message)
}

#[test]
fn a_figma_canvas_cannot_decode_to_more_memory_than_its_budget() {
    use super::super::design_fig::{
        decode_kiwi, decode_kiwi_within, decode_schema, read_fig, MAX_NAME_BYTES,
    };
    // The round-1 review's probe: a 4096-byte field name and 20,000 structs
    // (72 bytes deflated) decoded to 81,920,000 bytes of keys. Opened the
    // way File > Open opens it, it is now refused at the schema.
    let (schema, message) = kiwi_probe(4096, 20_000);
    let mut canvas = b"fig-kiwi".to_vec();
    canvas.extend_from_slice(&48u32.to_le_bytes());
    for chunk in [deflate(&schema), deflate(&message)] {
        canvas.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        canvas.extend_from_slice(&chunk);
    }
    assert!(canvas.len() < 200, "{} bytes", canvas.len());
    let err = read_fig(&canvas, ImportLimits::default()).unwrap_err();
    assert!(
        matches!(&err, CodecError::LimitExceeded(m) if m.contains("4096 bytes long")),
        "{err}"
    );
    // At the longest name allowed, every key is charged to the budget:
    // 20,000 keys of 256 bytes are 5,120,000 bytes, past a 4 MiB budget ...
    let (schema, message) = kiwi_probe(MAX_NAME_BYTES, 20_000);
    let defs = decode_schema(&schema).unwrap();
    let err = decode_kiwi_within(&defs, "Message", &message, 4 << 20).unwrap_err();
    assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
    // ... within an 8 MiB one, and within the real one.
    assert!(decode_kiwi_within(&defs, "Message", &message, 8 << 20).is_ok());
    let (value, _) = decode_kiwi(&defs, "Message", &message).unwrap();
    assert_eq!(value.get("items").map(|v| v.as_arr().len()), Some(20_000));
}

#[test]
fn a_figma_array_declaring_billions_of_items_reserves_a_bounded_slot_count() {
    use super::super::design_fig::{decode_kiwi_within, decode_schema};
    // The round-2 review's probe: a zero-field struct (each item takes no
    // message bytes) and an array declaring u32::MAX of them, about 8 bytes
    // of message. Reserving the declared length up front asks the allocator
    // for ~137 GB and aborts the process; the reserve is held to
    // MAX_KIWI_RESERVE and the budget stops the decode instead.
    let schema = fixtures::schema(&[("S", 1, &[]), ("Message", 2, &[("items", "S", true, 1)])]);
    let mut message = Vec::new();
    fixtures::var_uint(&mut message, 1);
    fixtures::var_uint(&mut message, u32::MAX);
    message.push(0);
    assert!(message.len() <= 8, "{} bytes", message.len());
    let defs = decode_schema(&schema).unwrap();
    let err = decode_kiwi_within(&defs, "Message", &message, 1 << 20).unwrap_err();
    assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
}

/// W13X-8 review: a message repeating a one-byte-named field grows its field
/// list by doubling; the spare capacity each doubling adds is charged, so
/// one field past a power of two costs about another power of two of slots.
#[test]
fn a_growing_kiwi_field_list_charges_its_spare_capacity() {
    use super::super::design_fig::{decode_kiwi_within, decode_schema};
    let fields: [fixtures::FieldSpec; 1] = [("b", "bool", false, 1)];
    let defs = decode_schema(&fixtures::schema(&[("Message", 2, &fields)])).unwrap();
    let message = |count: usize| {
        let mut m = Vec::with_capacity(count * 2 + 1);
        for _ in 0..count {
            m.extend_from_slice(&[1, 1]);
        }
        m.push(0);
        m
    };
    // The smallest budget the decode fits in, by bisection.
    let least = |count: usize| {
        let m = message(count);
        let (mut lo, mut hi) = (0usize, 64 << 20);
        assert!(decode_kiwi_within(&defs, "Message", &m, hi).is_ok());
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if decode_kiwi_within(&defs, "Message", &m, mid).is_ok() {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    };
    let slot = std::mem::size_of::<(String, Json)>();
    let at_power = least(1 << 16);
    let one_past = least((1 << 16) + 1);
    assert!(
        one_past - at_power >= (1 << 16) * slot,
        "one more field cost {} bytes, the doubling adds {}",
        one_past - at_power,
        (1 << 16) * slot
    );
}
