//! W13X-8 test fixtures: a synthetic Sketch, Adobe XD and Figma file, each
//! one artboard "Board" at (100, 50), 200x100, white, holding (bottom to
//! top) a red 40x20 rectangle "Box" at (10, 10) with 50% opacity, the text
//! "Hello" "Title" in Helvetica Bold 24 blue at (60, 10), and a 4x3 image
//! "Photo" at (10, 40), plus a component instance "Button" that does not
//! map. Sketch and XD also give the box a 2 px blue inside border and a
//! drop shadow.
//!
//! Self-contained (std only), so both `raster`'s tests and `app-shell`'s
//! (which include this file by path) build the same bytes. The caller
//! supplies the image's PNG bytes and, for Figma, the chunk compressor;
//! [`stored_deflate`] is a raw DEFLATE stream of stored blocks.

#![allow(dead_code)]

/// A stored (method 0) ZIP of `entries`, first entry first.
pub fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let local = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&local.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let dir_at = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&dir_at.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out
}

/// `data` as a raw DEFLATE stream of stored (uncompressed) blocks.
pub fn stored_deflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunks = data.chunks(0xFFFF).peekable();
    if chunks.peek().is_none() {
        return vec![1, 0, 0, 0xFF, 0xFF];
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let n = chunk.len() as u16;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(!n).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

const RECT_POINTS: &str = r#"[
  {"_class":"curvePoint","point":"{0, 0}","curveFrom":"{0, 0}","curveTo":"{0, 0}","hasCurveFrom":false,"hasCurveTo":false},
  {"_class":"curvePoint","point":"{1, 0}","curveFrom":"{1, 0}","curveTo":"{1, 0}","hasCurveFrom":false,"hasCurveTo":false},
  {"_class":"curvePoint","point":"{1, 1}","curveFrom":"{1, 1}","curveTo":"{1, 1}","hasCurveFrom":false,"hasCurveTo":false},
  {"_class":"curvePoint","point":"{0, 1}","curveFrom":"{0, 1}","curveTo":"{0, 1}","hasCurveFrom":false,"hasCurveTo":false}
]"#;

/// The Sketch file; `image` is the 4x3 PNG.
pub fn sketch_file(image: &[u8]) -> Vec<u8> {
    let page = format!(
        r#"{{"_class":"page","name":"Page 1","layers":[
  {{"_class":"artboard","name":"Board","frame":{{"x":100,"y":50,"width":200,"height":100}},
    "hasBackgroundColor":true,"backgroundColor":{{"red":1,"green":1,"blue":1,"alpha":1}},
    "layers":[
      {{"_class":"rectangle","name":"Box","isVisible":true,
        "frame":{{"x":10,"y":10,"width":40,"height":20}},"isClosed":true,"points":{RECT_POINTS},
        "style":{{"fills":[{{"isEnabled":true,"fillType":0,"color":{{"red":1,"green":0,"blue":0,"alpha":1}}}}],
                 "borders":[{{"isEnabled":true,"fillType":0,"color":{{"red":0,"green":0,"blue":1,"alpha":1}},"thickness":2,"position":1}}],
                 "shadows":[{{"isEnabled":true}}],
                 "contextSettings":{{"opacity":0.5}}}}}},
      {{"_class":"text","name":"Title","frame":{{"x":60,"y":10,"width":120,"height":30}},"textBehaviour":0,
        "attributedString":{{"_class":"attributedString","string":"Hello",
          "attributes":[{{"location":0,"length":5,"attributes":{{
            "MSAttributedStringFontAttribute":{{"_class":"fontDescriptor","attributes":{{"name":"Helvetica-Bold","size":24}}}},
            "MSAttributedStringColorAttribute":{{"_class":"color","red":0,"green":0,"blue":1,"alpha":1}}}}}}]}}}},
      {{"_class":"bitmap","name":"Photo","frame":{{"x":10,"y":40,"width":4,"height":3}},
        "image":{{"_class":"MSJSONFileReference","_ref":"images/abc.png"}}}},
      {{"_class":"symbolInstance","name":"Button","frame":{{"x":0,"y":0,"width":1,"height":1}}}}
    ]}}
]}}"#
    );
    build_zip(&[
        (
            "document.json",
            br#"{"_class":"document","pages":[{"_class":"MSJSONFileReference","_ref_class":"MSImmutablePage","_ref":"pages/P1"}]}"#,
        ),
        ("pages/P1.json", page.as_bytes()),
        ("images/abc.png", image),
        ("previews/preview.png", image),
    ])
}

/// The Adobe XD file; `image` is the 4x3 PNG.
pub fn xd_file(image: &[u8]) -> Vec<u8> {
    let artwork = r#"{"version":"1.5.0","children":[
  {"type":"artboard","id":"a1","name":"Board",
   "style":{"fill":{"type":"solid","color":{"mode":"RGB","value":{"r":255,"g":255,"b":255}}}},
   "artboard":{"ref":"artboard-1","children":[
     {"type":"shape","name":"Box","transform":{"a":1,"b":0,"c":0,"d":1,"tx":10,"ty":10},
      "shape":{"type":"rect","x":0,"y":0,"width":40,"height":20},
      "style":{"opacity":0.5,
               "fill":{"type":"solid","color":{"mode":"RGB","value":{"r":255,"g":0,"b":0}}},
               "stroke":{"type":"solid","color":{"mode":"RGB","value":{"r":0,"g":0,"b":255}},"width":2,"align":"inside"},
               "filters":[{"type":"dropShadow","visible":true}]}},
     {"type":"text","name":"Title","transform":{"a":1,"b":0,"c":0,"d":1,"tx":60,"ty":34},
      "text":{"rawText":"Hello","frame":{"type":"positioned"}},
      "style":{"font":{"family":"Helvetica","style":"Bold","size":24,"postscriptName":"Helvetica-Bold"},
               "fill":{"type":"solid","color":{"mode":"RGB","value":{"r":0,"g":0,"b":255}}}}},
     {"type":"shape","name":"Photo","transform":{"a":1,"b":0,"c":0,"d":1,"tx":10,"ty":40},
      "shape":{"type":"rect","x":0,"y":0,"width":4,"height":3},
      "style":{"fill":{"type":"pattern","pattern":{"width":4,"height":3,"meta":{"ux":{"uid":"img1"}}}}}},
     {"type":"syncRef","name":"Button"}
   ]}}
]}"#;
    let resources = r#"{"resources":{},"artboards":{"artboard-1":{"x":100,"y":50,"width":200,"height":100,"name":"Board"}}}"#;
    build_zip(&[
        ("mimetype", b"application/vnd.adobe.sparkler.project+dcxucf"),
        ("manifest", br#"{"name":"t","children":[]}"#),
        (
            "resources/graphics/graphicContent.agc",
            resources.as_bytes(),
        ),
        (
            "artwork/artboard-1/graphics/graphicContent.agc",
            artwork.as_bytes(),
        ),
        ("resources/img1", image),
        ("preview.png", image),
    ])
}

// -------------------------------------------------------------------- Figma

pub fn var_uint(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let b = (v & 127) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 128);
    }
}

fn var_int(out: &mut Vec<u8>, v: i32) {
    var_uint(out, ((v << 1) ^ (v >> 31)) as u32);
}

/// Kiwi's float: the bits rotated so the exponent comes first; a zero
/// exponent byte is written as a lone zero.
pub fn float(out: &mut Vec<u8>, v: f32) {
    let bits = v.to_bits().rotate_right(23);
    if bits & 255 == 0 {
        out.push(0);
    } else {
        out.extend_from_slice(&bits.to_le_bytes());
    }
}

fn string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// A kiwi schema: `(name, kind, [(field, type, array, value)])`, with
/// types named (`"uint"`, `"float"`, …, another definition's name, or `""`
/// for an enum's values).
/// One schema field: `(name, type, is array, value)`.
pub type FieldSpec<'a> = (&'a str, &'a str, bool, u32);
/// One schema definition: `(name, kind, fields)`.
pub type DefSpec<'a> = (&'a str, u8, &'a [FieldSpec<'a>]);

pub fn schema(defs: &[DefSpec]) -> Vec<u8> {
    let builtin = [
        "bool", "byte", "int", "uint", "float", "string", "int64", "uint64",
    ];
    let mut out = Vec::new();
    var_uint(&mut out, defs.len() as u32);
    for (name, kind, fields) in defs {
        string(&mut out, name);
        out.push(*kind);
        var_uint(&mut out, fields.len() as u32);
        for (field, ty, array, value) in *fields {
            string(&mut out, field);
            let t = match builtin.iter().position(|b| b == ty) {
                Some(i) => -(i as i32) - 1,
                None if ty.is_empty() => 0,
                None => defs.iter().position(|d| d.0 == *ty).unwrap() as i32,
            };
            var_int(&mut out, t);
            out.push(u8::from(*array));
            var_uint(&mut out, *value);
        }
    }
    out
}

/// The subset of Figma's schema the fixture uses, under Figma's names.
pub fn fig_schema() -> Vec<u8> {
    schema(&[
        (
            "NodeType",
            0,
            &[
                ("DOCUMENT", "", false, 1),
                ("CANVAS", "", false, 2),
                ("FRAME", "", false, 3),
                ("RECTANGLE", "", false, 4),
                ("TEXT", "", false, 5),
                ("INSTANCE", "", false, 6),
            ],
        ),
        (
            "GUID",
            1,
            &[
                ("sessionID", "uint", false, 1),
                ("localID", "uint", false, 2),
            ],
        ),
        (
            "ParentIndex",
            1,
            &[("guid", "GUID", false, 1), ("position", "string", false, 2)],
        ),
        (
            "Vector",
            1,
            &[("x", "float", false, 1), ("y", "float", false, 2)],
        ),
        (
            "Matrix",
            1,
            &[
                ("m00", "float", false, 1),
                ("m01", "float", false, 2),
                ("m02", "float", false, 3),
                ("m10", "float", false, 4),
                ("m11", "float", false, 5),
                ("m12", "float", false, 6),
            ],
        ),
        (
            "Color",
            1,
            &[
                ("r", "float", false, 1),
                ("g", "float", false, 2),
                ("b", "float", false, 3),
                ("a", "float", false, 4),
            ],
        ),
        (
            "PaintType",
            0,
            &[
                ("SOLID", "", false, 0),
                ("GRADIENT_LINEAR", "", false, 1),
                ("IMAGE", "", false, 5),
            ],
        ),
        (
            "Image",
            2,
            &[("hash", "byte", true, 1), ("name", "string", false, 2)],
        ),
        (
            "Paint",
            2,
            &[
                ("type", "PaintType", false, 1),
                ("color", "Color", false, 2),
                ("opacity", "float", false, 3),
                ("visible", "bool", false, 4),
                ("image", "Image", false, 5),
            ],
        ),
        ("TextData", 2, &[("characters", "string", false, 1)]),
        (
            "FontName",
            1,
            &[
                ("family", "string", false, 1),
                ("style", "string", false, 2),
                ("postscript", "string", false, 3),
            ],
        ),
        (
            "NodeChange",
            2,
            &[
                ("guid", "GUID", false, 1),
                ("parentIndex", "ParentIndex", false, 2),
                ("type", "NodeType", false, 3),
                ("name", "string", false, 4),
                ("visible", "bool", false, 5),
                ("opacity", "float", false, 6),
                ("size", "Vector", false, 7),
                ("transform", "Matrix", false, 8),
                ("fillPaints", "Paint", true, 9),
                ("textData", "TextData", false, 10),
                ("fontSize", "float", false, 11),
                ("fontName", "FontName", false, 12),
                ("strokePaints", "Paint", true, 13),
                ("strokeWeight", "float", false, 14),
            ],
        ),
        ("Message", 2, &[("nodeChanges", "NodeChange", true, 1)]),
    ])
}

fn guid_bytes(out: &mut Vec<u8>, local: u32) {
    var_uint(out, 0);
    var_uint(out, local);
}

fn solid(out: &mut Vec<u8>, rgba: [f32; 4]) {
    var_uint(out, 1);
    var_uint(out, 0); // SOLID
    var_uint(out, 2);
    for c in rgba {
        float(out, c);
    }
    out.push(0);
}

#[derive(Clone, Copy)]
struct FigNode<'a> {
    local: u32,
    parent: Option<(u32, &'a str)>,
    ty: u32,
    name: &'a str,
    size: Option<(f32, f32)>,
    at: Option<(f32, f32)>,
    fill: Option<[f32; 4]>,
    image: Option<&'a [u8]>,
    text: Option<(&'a str, &'a str, &'a str, f32)>,
    opacity: Option<f32>,
}

fn node_change(out: &mut Vec<u8>, n: &FigNode) {
    var_uint(out, 1);
    guid_bytes(out, n.local);
    if let Some((parent, position)) = n.parent {
        var_uint(out, 2);
        guid_bytes(out, parent);
        string(out, position);
    }
    var_uint(out, 3);
    var_uint(out, n.ty);
    var_uint(out, 4);
    string(out, n.name);
    if let Some(o) = n.opacity {
        var_uint(out, 6);
        float(out, o);
    }
    if let Some((w, h)) = n.size {
        var_uint(out, 7);
        float(out, w);
        float(out, h);
    }
    if let Some((x, y)) = n.at {
        var_uint(out, 8);
        for v in [1.0, 0.0, x, 0.0, 1.0, y] {
            float(out, v);
        }
    }
    if let Some(c) = n.fill {
        var_uint(out, 9);
        var_uint(out, 1);
        solid(out, c);
    }
    if let Some(hash) = n.image {
        var_uint(out, 9); // fillPaints
        var_uint(out, 1); // one paint
        var_uint(out, 1); // .type
        var_uint(out, 5); // IMAGE
        var_uint(out, 5); // .image
        var_uint(out, 1); // .hash
        var_uint(out, hash.len() as u32);
        out.extend_from_slice(hash);
        out.push(0); // end of Image
        out.push(0); // end of Paint
    }
    if let Some((chars, family, style, size)) = n.text {
        var_uint(out, 10);
        var_uint(out, 1);
        string(out, chars);
        out.push(0);
        var_uint(out, 11);
        float(out, size);
        var_uint(out, 12);
        string(out, family);
        string(out, style);
        string(out, "");
    }
    out.push(0);
}

/// The image's hash; the archive holds it as `images/abcd01`.
pub const FIG_IMAGE_HASH: [u8; 3] = [0xab, 0xcd, 0x01];

/// The Figma message (its node tree), encoded with [`fig_schema`].
pub fn fig_message() -> Vec<u8> {
    let base = FigNode {
        local: 0,
        parent: None,
        ty: 1,
        name: "Document",
        size: None,
        at: None,
        fill: None,
        image: None,
        text: None,
        opacity: None,
    };
    let nodes = [
        base,
        FigNode {
            local: 1,
            parent: Some((0, "!")),
            ty: 2,
            name: "Page 1",
            ..base
        },
        FigNode {
            local: 2,
            parent: Some((1, "!")),
            ty: 3,
            name: "Board",
            size: Some((200.0, 100.0)),
            at: Some((100.0, 50.0)),
            fill: Some([1.0, 1.0, 1.0, 1.0]),
            ..base
        },
        // Listed out of order: the positions put the text second.
        FigNode {
            local: 4,
            parent: Some((2, "b")),
            ty: 5,
            name: "Title",
            size: Some((120.0, 30.0)),
            at: Some((60.0, 10.0)),
            fill: Some([0.0, 0.0, 1.0, 1.0]),
            text: Some(("Hello", "Helvetica", "Bold", 24.0)),
            ..base
        },
        FigNode {
            local: 3,
            parent: Some((2, "a")),
            ty: 4,
            name: "Box",
            size: Some((40.0, 20.0)),
            at: Some((10.0, 10.0)),
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            opacity: Some(0.5),
            ..base
        },
        FigNode {
            local: 5,
            parent: Some((2, "c")),
            ty: 4,
            name: "Photo",
            size: Some((4.0, 3.0)),
            at: Some((10.0, 40.0)),
            image: Some(&FIG_IMAGE_HASH),
            ..base
        },
        FigNode {
            local: 6,
            parent: Some((2, "d")),
            ty: 6,
            name: "Button",
            ..base
        },
    ];
    let mut out = Vec::new();
    var_uint(&mut out, 1);
    var_uint(&mut out, nodes.len() as u32);
    for n in &nodes {
        node_change(&mut out, n);
    }
    out.push(0);
    out
}

/// A `fig-kiwi` canvas: the schema as stored DEFLATE, the message through
/// `compress_message`.
pub fn fig_canvas(compress_message: &dyn Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let schema = stored_deflate(&fig_schema());
    let data = compress_message(&fig_message());
    let mut out = b"fig-kiwi".to_vec();
    out.extend_from_slice(&48u32.to_le_bytes());
    for chunk in [&schema, &data] {
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// A `.fig` archive: the canvas, its image (`image`, the 4x3 PNG) and a
/// thumbnail.
pub fn fig_file(image: &[u8], compress_message: &dyn Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    build_zip(&[
        ("canvas.fig", &fig_canvas(compress_message)),
        ("images/abcd01", image),
        ("thumbnail.png", image),
    ])
}
