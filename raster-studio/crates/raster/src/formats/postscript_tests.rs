//! W15-E: the PostScript interpreter, from EPS files written here, through
//! the same entry points File > Open uses (`decode_surface_bytes`, which
//! sniffs the file, and `vector_docs::decode_described`, which app-shell's
//! open route calls and whose note becomes the status line).

use std::io::Write as _;
use std::time::{Duration, Instant};

use super::*;
use crate::codec::{decode_surface_bytes, formats::vector_docs, SurfacePixels};

fn eps(bbox: &str, body: &str) -> Vec<u8> {
    format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: {bbox}\n%%EndComments\n{body}\nshowpage\n%%EOF\n"
    )
    .into_bytes()
}

fn px(s: &DecodedSurface, x: u32, y: u32) -> [u8; 4] {
    let SurfacePixels::Rgba8(p) = &s.pixels else {
        panic!("8-bit expected")
    };
    let i = ((y * s.width + x) * 4) as usize;
    [p[i], p[i + 1], p[i + 2], p[i + 3]]
}

/// Draw through the codec facade (the File > Open import path), and read
/// the note through the route app-shell calls.
fn open(file: &[u8]) -> (DecodedSurface, String) {
    let s = decode_surface_bytes(file, ImportLimits::default()).expect("the EPS draws");
    assert_eq!(s.source_format, ImportFormat::Eps);
    let (_, note) =
        vector_docs::decode_described(ImportFormat::Eps, file, ImportLimits::default()).unwrap();
    (s, note)
}

const RED: [u8; 4] = [255, 0, 0, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

#[test]
fn a_filled_rectangle_draws_at_its_place_on_the_page() {
    // PostScript's y axis points up: the rectangle at the bottom-left of a
    // 20 x 10 page is the left half of the image.
    let file = eps(
        "0 0 20 10",
        "1 0 0 setrgbcolor newpath 0 0 moveto 10 0 lineto 10 10 lineto 0 10 lineto closepath fill",
    );
    let (s, note) = open(&file);
    assert_eq!((s.width, s.height), (20, 10));
    assert_eq!(px(&s, 2, 2), RED);
    assert_eq!(px(&s, 8, 8), RED);
    assert_eq!(px(&s, 15, 5), CLEAR, "outside the path");
    assert!(note.contains("PostScript artwork"), "{note}");
    assert!(!note.contains("preview"), "{note}");
}

#[test]
fn a_stroked_bezier_paints_its_outline_not_its_inside() {
    let file = eps(
        "0 0 100 100",
        "0 0 1 setrgbcolor 4 setlinewidth newpath 10 10 moveto 10 90 90 90 90 10 curveto stroke",
    );
    let (s, _) = open(&file);
    // B(0.5) = (50, 70) in PostScript space: row 100 - 70 = 30.
    assert_eq!(px(&s, 50, 30), [0, 0, 255, 255], "on the curve");
    assert_eq!(px(&s, 50, 29)[2], 255, "within the 4 pt line");
    assert_eq!(
        px(&s, 50, 60),
        CLEAR,
        "inside the curve: stroked, not filled"
    );
    assert_eq!(px(&s, 50, 24), CLEAR, "past the line's width");
    // The ends: (10, 10) and (90, 10), row 90.
    assert_eq!(px(&s, 10, 88), [0, 0, 255, 255]);
    assert_eq!(px(&s, 90, 88), [0, 0, 255, 255]);
}

#[test]
fn gsave_scale_grestore_scales_only_what_is_between() {
    let file = eps(
        "0 0 60 20",
        "0 setgray \
         gsave 2 2 scale newpath 0 0 moveto 5 0 lineto 5 5 lineto 0 5 lineto closepath fill grestore \
         newpath 30 0 moveto 35 0 lineto 35 5 lineto 30 5 lineto closepath fill",
    );
    let (s, _) = open(&file);
    // The scaled square covers 0..10 x 0..10 (rows 10..20).
    assert_eq!(px(&s, 8, 12), [0, 0, 0, 255], "scaled 2x inside gsave");
    assert_eq!(px(&s, 12, 12), CLEAR);
    // After grestore the second square is 5 x 5 again.
    assert_eq!(px(&s, 33, 17), [0, 0, 0, 255]);
    assert_eq!(
        px(&s, 38, 17),
        CLEAR,
        "the scale did not leak past grestore"
    );
    assert_eq!(px(&s, 33, 12), CLEAR);
}

#[test]
fn the_image_operator_places_its_samples_through_the_matrix() {
    // A 2 x 2 grey image, first row at the top, scaled to 20 x 20.
    let file = eps(
        "0 0 20 20",
        "20 20 scale 2 2 8 [2 0 0 -2 0 2] {<00ff8040>} image",
    );
    let (s, _) = open(&file);
    assert_eq!(px(&s, 5, 5), [0, 0, 0, 255]);
    assert_eq!(px(&s, 15, 5), [255, 255, 255, 255]);
    assert_eq!(px(&s, 5, 15), [128, 128, 128, 255]);
    assert_eq!(px(&s, 15, 15), [64, 64, 64, 255]);
}

#[test]
fn colorimage_reads_hex_rows_from_currentfile_the_way_illustrator_writes_them() {
    let file = eps(
        "0 0 20 10",
        "/picstr 6 string def\n20 10 scale\n\
         2 1 8 [2 0 0 -1 0 1] {currentfile picstr readhexstring pop} false 3 colorimage\n\
         ff0000 00ff00\n",
    );
    let (s, _) = open(&file);
    assert_eq!(px(&s, 5, 5), RED);
    assert_eq!(px(&s, 15, 5), [0, 255, 0, 255]);
}

#[test]
fn a_level_2_image_dictionary_reads_through_ascii85_and_flate() {
    // 2 x 1 RGB, compressed and ASCII85-encoded, from currentfile.
    let raw = [0u8, 0, 255, 255, 255, 0];
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).unwrap();
    let a85 = ascii85(&z.finish().unwrap());
    let body = format!(
        "/DeviceRGB setcolorspace 20 10 scale\n\
         << /ImageType 1 /Width 2 /Height 1 /BitsPerComponent 8 /Decode [0 1 0 1 0 1]\n\
            /ImageMatrix [2 0 0 -1 0 1]\n\
            /DataSource currentfile /ASCII85Decode filter /FlateDecode filter >> image\n\
         {a85}~>\n0 1 0 setrgbcolor 1 1 1 1 rectfill"
    );
    let (s, _) = open(&eps("0 0 40 20", &body));
    assert_eq!(px(&s, 5, 15), [0, 0, 255, 255]);
    assert_eq!(px(&s, 15, 15), [255, 255, 0, 255]);
    // After the image data the program carried on.
    assert_eq!(px(&s, 30, 5), [0, 255, 0, 255], "the fill after the image");
}

fn ascii85(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(4) {
        let mut b = [0u8; 4];
        b[..chunk.len()].copy_from_slice(chunk);
        let mut v = u32::from_be_bytes(b);
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = (v % 85) as u8 + b'!';
            v /= 85;
        }
        out.push_str(std::str::from_utf8(&digits[..chunk.len() + 1]).unwrap());
    }
    out
}

#[test]
fn imagemask_paints_the_current_colour_where_the_mask_is_set() {
    // 2 x 1 mask 10000000: with polarity true the first bit paints.
    let file = eps(
        "0 0 20 10",
        "1 0 0 setrgbcolor 20 10 scale 2 1 true [2 0 0 -1 0 1] {<80>} imagemask",
    );
    let (s, _) = open(&file);
    assert_eq!(px(&s, 5, 5), RED);
    assert_eq!(px(&s, 15, 5)[3], 0);
}

#[test]
fn a_procedure_defined_with_def_draws_each_time_it_is_called() {
    let file = eps(
        "0 0 40 10",
        "/box { newpath moveto 0 10 rlineto 10 0 rlineto 0 -10 rlineto closepath fill } bind def\n\
         /green { 0 1 0 setrgbcolor } def\n\
         green 0 0 box 20 0 box",
    );
    let (s, _) = open(&file);
    assert_eq!(px(&s, 5, 5), [0, 255, 0, 255]);
    assert_eq!(px(&s, 15, 5), CLEAR);
    assert_eq!(px(&s, 25, 5), [0, 255, 0, 255]);
}

#[test]
fn control_flow_clip_arcs_and_colour_spaces_draw() {
    let file = eps(
        "0 0 100 20",
        // A for loop of four squares, CMYK cyan.
        "1 0 0 0 setcmykcolor 0 1 3 { 10 mul 0 5 5 rectfill } for\n\
         % ifelse / repeat / loop-exit\n\
         1 2 lt { 0 0 1 setrgbcolor } { 1 0 0 setrgbcolor } ifelse\n\
         /n 0 def { /n n 1 add def n 3 ge { exit } if } loop\n\
         n 3 eq { 40 0 5 5 rectfill } if\n\
         % a clipped fill: only x < 55 paints\n\
         gsave 50 0 5 20 rectclip 0 0 1 setrgbcolor 50 0 20 20 rectfill grestore\n\
         % an arc filled as a disc, and an even-odd donut\n\
         0 setgray newpath 80 10 8 0 360 arc closepath fill\n\
         % a Separation colour through its tint transform\n\
         [/Separation (Spot) /DeviceRGB {dup 0 exch}] setcolorspace 1 setcolor 60 0 5 5 rectfill",
    );
    let (s, _) = open(&file);
    for x in [2, 12, 22, 32] {
        assert_eq!(px(&s, x, 17), [0, 255, 255, 255], "square at {x}");
    }
    assert_eq!(
        px(&s, 42, 17),
        [0, 0, 255, 255],
        "drawn after the loop exited at 3"
    );
    assert_eq!(px(&s, 52, 5), [0, 0, 255, 255], "inside the clip");
    assert_eq!(px(&s, 58, 5), CLEAR, "outside the clip");
    assert_eq!(px(&s, 80, 10), [0, 0, 0, 255], "the disc's centre");
    assert_eq!(px(&s, 73, 3), CLEAR, "outside the disc");
    assert_eq!(px(&s, 62, 17), [255, 0, 255, 255], "tint 1 -> (1 0 1)");
}

#[test]
fn stopped_catches_an_undefined_name_and_unknown_operators_are_reported() {
    let file = eps(
        "0 0 20 10",
        "{ nosuchoperator } stopped { 1 0 0 setrgbcolor 0 0 10 10 rectfill } if\n\
         alsounknown 10 0 10 10 rectfill",
    );
    let (s, note) = open(&file);
    assert_eq!(px(&s, 5, 5), RED);
    assert_eq!(
        px(&s, 15, 5),
        RED,
        "the unknown name did not stop the program"
    );
    assert!(note.contains("nosuchoperator"), "{note}");
    assert!(note.contains("alsounknown"), "{note}");
}

#[test]
fn the_hires_bounding_box_sets_the_page_and_the_origin() {
    let file = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 100 200 131 211\n\
%%HiResBoundingBox: 100.5 200.5 130.5 210.5\n%%EndComments\n\
1 0 0 setrgbcolor 100.5 200.5 10 10 rectfill\n";
    let (s, _) = open(file);
    assert_eq!((s.width, s.height), (30, 10));
    assert_eq!(px(&s, 5, 5), RED);
    assert_eq!(px(&s, 15, 5), CLEAR);
}

#[test]
fn text_is_drawn_in_a_fallback_font_and_an_embedded_type_1_font_is_skipped() {
    let file = eps(
        "0 0 120 40",
        "%%BeginResource: font Fancy\n\
         12 dict begin /FontName /Fancy def /FontType 1 def\n\
         /FontMatrix [0.001 0 0 0.001 0 0] readonly def\n\
         currentdict end currentfile eexec\n\
         d9d66f633b846a989b9974b0179fc6cc445bc2c03103c68570a7b354a4a280ae\n\
         0000000000000000000000000000000000000000000000000000000000000000\n\
         cleartomark\n%%EndResource\n\
         /Fancy findfont 24 scalefont setfont 0 setgray 4 8 moveto (Hello) show\n\
         currentpoint pop 60 gt { 1 0 0 setrgbcolor 110 30 5 5 rectfill } if",
    );
    let (s, note) = open(&file);
    assert!(note.contains("fallback system font"), "{note}");
    assert!(note.contains("1 embedded Type 1 font(s)"), "{note}");
    // `show` advanced the current point (5 characters, 0.55 em each).
    assert_eq!(px(&s, 112, 7), RED);
}

#[test]
fn an_infinite_loop_ends_on_the_operation_budget_and_the_time_budget() {
    let file = eps("0 0 10 10", "0 0 5 5 rectfill { } loop");
    let tight = Budget {
        max_ops: 200_000,
        ..Budget::default()
    };
    let err = render_with(&file, ImportLimits::default(), tight).unwrap_err();
    assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
    assert!(err.to_string().contains("operation budget"), "{err}");

    let timed = Budget {
        max_ops: u64::MAX,
        time: Duration::from_millis(200),
        ..Budget::default()
    };
    let t = Instant::now();
    let err = render_with(&file, ImportLimits::default(), timed).unwrap_err();
    assert!(err.to_string().contains("time budget"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());

    // The default budget ends it too, on the File > Open route, and the
    // preview-less file is refused with the reason.
    let t = Instant::now();
    let err = decode_surface_bytes(&file, ImportLimits::default()).unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(60), "{:?}", t.elapsed());
}

#[test]
fn runaway_stacks_recursion_and_memory_stop_inside_their_bounds() {
    // Unbounded recursion: an error, not a stack overflow of the process.
    let file = eps("0 0 10 10", "/f { f } def f 0 0 5 5 rectfill");
    let (_, note) = open(&file);
    assert!(note.contains("execstackoverflow"), "{note}");
    // An operand stack pushed without end is cut at its bound: the loop
    // ends in `stackoverflow` and the program carries on.
    let file = eps("0 0 10 10", "{ 1 } loop clear 0 0 5 5 rectfill");
    let (_, note) = open(&file);
    assert!(note.contains("stackoverflow"), "{note}");
    // Strings allocated without end hit the memory budget.
    let file = eps("0 0 10 10", "{ 1000000 string } loop");
    let err = render(&file, ImportLimits::default()).unwrap_err();
    assert!(err.to_string().contains("memory budget"), "{err}");
    // An image larger than the import limits is refused before it exists.
    let file = eps("0 0 10 10", "100000 100000 8 [1 0 0 1 0 0] {<00>} image");
    let err = render(&file, ImportLimits::default()).unwrap_err();
    assert!(err.to_string().contains("import limit"), "{err}");
}

#[test]
fn malformed_postscript_errors_and_never_panics() {
    let cases: &[&[u8]] = &[
        b"",
        b"%!PS\n",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 0 0\n0 0 1 1 rectfill\n",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n(unterminated",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n{ { { 0 0 1 1 rectfill",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n} 0 0 1 1 rectfill",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n<zz> 0 0 1 1 rectfill",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n\x80\x81\x82",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\npop pop exch roll 1 0 div",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n1e308 1e308 mul 2147483647 1 add -1 99999 roll",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n0 0 moveto 1e400 5 lineto stroke",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n[1 2 3] 5 get 3 array -1 1 put () 9 get",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n2 2 8 [0 0 0 0 0 0] {<00>} image",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n1 1 8 matrix currentfile /FlateDecode filter image garbage",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n1 1 8 matrix currentfile /DCTDecode filter image \xff\xd8\xff\xd9",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\nrestore save restore restore grestoreall end end",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n/a << /b 1 >> def a /b get 0 0 moveto 1 1 lineto 5 5 arcto clip",
    ];
    for case in cases {
        let _ = render(case, ImportLimits::default());
        let _ = decode_surface_bytes(case, ImportLimits::default());
    }
    // Every truncation and a sweep of byte flips of a real sample.
    let sample = eps(
        "0 0 40 20",
        "/b { newpath moveto 0 10 rlineto 10 0 rlineto closepath } bind def\n\
         gsave 0.5 0.5 scale 1 0 0 setrgbcolor 0 0 b fill grestore\n\
         [2 1] 0 setdash 3 setlinewidth 10 10 b stroke\n\
         /s 3 string def 2 1 8 [2 0 0 -1 0 1] {currentfile s readhexstring pop} image\n\
         00ff\n\
         << /A [1 2 3] >> /A get { pop } forall 0 1 5 { pop } for\n\
         /Helvetica findfont 9 scalefont setfont 2 2 moveto (x) show",
    );
    let quick = Budget {
        max_ops: 100_000,
        ..Budget::default()
    };
    for cut in 0..sample.len() {
        let _ = render_with(&sample[..cut], ImportLimits::default(), quick);
    }
    let mut seed = 0x2545_f491_u32;
    for _ in 0..3000 {
        let mut v = sample.clone();
        for _ in 0..3 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let at = seed as usize % v.len();
            v[at] = (seed >> 8) as u8;
        }
        let _ = render_with(&v, ImportLimits::default(), quick);
    }
}

#[test]
fn a_dos_eps_draws_its_postscript_and_falls_back_to_its_preview_with_the_reason() {
    let tiff_px: Vec<u8> = (0..3 * 2).flat_map(|_| [20u8, 90, 200, 255]).collect();
    let tiff = encode(ExportFormat::Tiff, 3, 2, &tiff_px).unwrap();
    let build = |ps: &[u8]| {
        let mut v = vec![0xC5, 0xD0, 0xD3, 0xC6];
        let ps_at = 30u32;
        let tiff_at = ps_at + ps.len() as u32;
        for x in [ps_at, ps.len() as u32, 0, 0, tiff_at, tiff.len() as u32] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&[0xFF, 0xFF]);
        v.extend_from_slice(ps);
        v.extend_from_slice(&tiff);
        v
    };
    // Drawable PostScript: the artwork, not the preview.
    let drawn = build(&eps("0 0 6 4", "1 0 0 setrgbcolor 0 0 6 4 rectfill"));
    let (s, note) = open(&drawn);
    assert_eq!((s.width, s.height, px(&s, 1, 1)), (6, 4, RED));
    assert!(note.contains("PostScript artwork"), "{note}");
    // PostScript that runs away: the preview, with why.
    let runaway = build(&eps("0 0 6 4", "{ } loop"));
    let s = decode_surface_bytes(&runaway, ImportLimits::default()).unwrap();
    assert_eq!(
        (s.width, s.height, px(&s, 1, 1)),
        (3, 2, [20, 90, 200, 255])
    );
    let (_, note) =
        vector_docs::decode_described(ImportFormat::Eps, &runaway, ImportLimits::default())
            .unwrap();
    assert!(note.contains("embedded TIFF preview"), "{note}");
    assert!(note.contains("budget"), "{note}");
}

/// The SVG the interpreter writes for `body` on a `w` x `h` page.
fn svg_of(w: f64, h: f64, body: &str) -> String {
    let mut it = Interp::new(
        body.as_bytes(),
        [1.0, 0.0, 0.0, -1.0, 0.0, h],
        (w, h),
        ImportLimits::default(),
        Budget::default(),
    );
    assert!(it.run_main().is_ok());
    std::mem::take(&mut it.svg)
}

#[test]
fn text_is_sized_by_the_font_units_type_1_and_type_42_alike() {
    // A Type 1 font at 12 pt: 1000 units an em.
    let svg = svg_of(
        100.0,
        40.0,
        "/Helvetica findfont 12 scalefont setfont 0 0 moveto (Ab) show",
    );
    assert!(
        svg.contains("<text transform=\"matrix(0.012 0 0 0.012 0 40)\""),
        "{svg}"
    );
    assert!(svg.contains(">Ab</text>"), "{svg}");
    // A Type 42 font (cairo's): identity FontMatrix, 1 unit an em; selected
    // at 20 pt the way cairo's `Tf` does, and its advance is 0.55 em a
    // character, not 1000 times that.
    let t42 = "/F42 << /FontType 42 /FontMatrix [1 0 0 1 0 0] /FontName /F42 >> definefont pop\n\
               /F42 [20 0 0 20 0 0] selectfont 0 0 moveto (AB) show\n\
               currentpoint pop 22 sub abs 0.01 lt { 0 0 1 1 rectfill } if";
    let svg = svg_of(100.0, 40.0, t42);
    assert!(
        svg.contains("<text transform=\"matrix(0.02 0 0 0.02 0 40)\""),
        "{svg}"
    );
    assert!(svg.contains("<path"), "the advance was 22 pt: {svg}");
    // matplotlib sets each glyph by name.
    let svg = svg_of(100.0, 40.0, "/Helvetica findfont 10 scalefont setfont 5 5 moveto /four glyphshow /minus glyphshow /uni00B5 glyphshow");
    assert!(
        svg.contains(">4</text>") && svg.contains(">\u{2212}</text>"),
        "{svg}"
    );
    assert!(svg.contains(">\u{b5}</text>"), "{svg}");
}

#[test]
fn a_cairo_shaped_eps_draws_with_its_image_flush_and_no_errors() {
    // The operators and prolog shape cairo 1.18 writes (Inkscape's EPS
    // export): `re`, `cm`, an ASCII85 + Flate image through `cairo_image`,
    // which asks the filter's `status` and `flushfile`s it.
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&[255, 255, 0]).unwrap();
    let a85 = ascii85(&z.finish().unwrap());
    let body = format!(
        "50 dict begin\n\
         /q {{ gsave }} bind def /Q {{ grestore }} bind def\n\
         /cm {{ 6 array astore concat }} bind def\n\
         /re {{ exch dup neg 3 1 roll 5 3 roll moveto 0 rlineto\n\
               0 exch rlineto 0 rlineto closepath }} bind def\n\
         /f {{ fill }} bind def /rg {{ setrgbcolor }} bind def\n\
         /cairo_flush_ascii85_file {{ cairo_ascii85_file status {{ cairo_ascii85_file flushfile }} if }} def\n\
         /cairo_image {{ image cairo_flush_ascii85_file }} def\n\
         q 1 0 0 -1 0 20 cm\n\
         1 0 0 rg 0 0 10 10 re f\n\
         q [ 10 0 0 -10 12 10 ] concat\n\
         /cairo_ascii85_file currentfile /ASCII85Decode filter def\n\
         /DeviceRGB setcolorspace\n\
         << /ImageType 1 /Width 1 /Height 1 /Interpolate true /BitsPerComponent 8\n\
            /Decode [ 0 1 0 1 0 1 ] /DataSource cairo_ascii85_file /FlateDecode filter\n\
            /ImageMatrix [ 1 0 0 -1 0 1 ] >>\n\
         cairo_image\n {a85}~>\n\
         Q\n\
         0 0 1 rg 30 0 10 10 re f\n\
         Q end"
    );
    let (s, note) = open(&eps("0 0 40 20", &body));
    // cairo's `cm` flips the page: y grows down from the top.
    assert_eq!(px(&s, 5, 5), RED);
    assert_eq!(px(&s, 15, 5), [255, 255, 0, 255], "the image");
    assert_eq!(px(&s, 35, 5), [0, 0, 255, 255], "drawn after the image");
    assert_eq!(px(&s, 5, 15), CLEAR);
    assert!(!note.contains("not drawn"), "{note}");
}

/// Run `f` on a thread with a 1 MiB stack, the Windows main thread's
/// default: a recursion as deep as the data would overflow it and abort the
/// whole test process.
fn on_a_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(f)
        .expect("a thread")
        .join()
        .expect("no panic")
}

#[test]
fn deep_nesting_is_freed_without_a_native_stack_overflow() {
    // Each stays inside every budget: a million-object chain is well under
    // the operation and memory bounds.
    let bodies = [
        // An array chain left on the operand stack (freed with the run).
        "[] 0 1 300000 { pop [ exch ] } for 0 0 5 5 rectfill",
        // An array chain freed mid-run by `pop`.
        "[] 0 1 300000 { pop [ exch ] } for pop 0 0 5 5 rectfill",
        // A dictionary chain freed mid-run, and one kept in userdict.
        "<< >> 0 1 300000 { pop 1 dict dup /n 4 -1 roll put } for pop 0 0 5 5 rectfill",
        "<< >> 0 1 300000 { pop 1 dict dup /n 4 -1 roll put } for /chain exch def 0 0 5 5 rectfill",
        // Filters stacked without end: the chain stops at its bound.
        "/f (00) def 0 1 100000 { pop f /ASCIIHexDecode filter /f exch def } for f read pop pop 0 0 5 5 rectfill",
    ];
    for body in bodies {
        let file = eps("0 0 10 10", body);
        let (w, note) = on_a_small_stack(move || {
            let (s, note) = render(&file, ImportLimits::default()).expect("the EPS draws");
            (s.width, note)
        });
        assert_eq!(w, 10, "{body}");
        if body.contains("filter") {
            assert!(note.contains("limitcheck"), "{note}");
        }
    }
}

#[test]
fn a_filter_chain_through_procedures_is_freed_without_a_native_stack_overflow() {
    // file -> procedure -> array -> file, as deep as the loop runs: the
    // filter-chain bound does not see it (each filter reads a procedure),
    // so freeing it must not recurse. Freed mid-run (`/x null def`) and at
    // teardown, through the File > Open facade on a 1 MiB stack.
    for body in [
        "/x null def 0 1 300000 { pop [ x ] cvx /ASCIIHexDecode filter /x exch def } for /x null def 0 0 5 5 rectfill",
        "/x null def 0 1 300000 { pop [ x ] cvx /ASCIIHexDecode filter /x exch def } for 0 0 5 5 rectfill",
    ] {
        let file = eps("0 0 10 10", body);
        let w = on_a_small_stack(move || {
            decode_surface_bytes(&file, ImportLimits::default())
                .expect("the EPS draws")
                .width
        });
        assert_eq!(w, 10, "{body}");
    }
}

#[test]
fn bind_over_a_procedure_that_holds_itself_twice_ends_quickly() {
    // A walk of every path would be 2^depth steps: bind visits each
    // procedure once and counts against the budget.
    let file = eps(
        "0 0 10 10",
        "[ null null ] cvx dup dup 0 exch put dup dup 1 exch put bind pop 0 0 5 5 rectfill",
    );
    let t = Instant::now();
    let w = on_a_small_stack(move || {
        decode_surface_bytes(&file, ImportLimits::default())
            .expect("the EPS draws")
            .width
    });
    assert_eq!(w, 10);
    assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());

    // A procedure too big to bind inside the operation budget stops on it
    // (one `array` operation makes it; only bind walks its elements).
    let big = eps("0 0 10 10", "100000 array cvx bind pop 0 0 5 5 rectfill");
    let tight = Budget {
        max_ops: 50_000,
        ..Budget::default()
    };
    let err = render_with(&big, ImportLimits::default(), tight).unwrap_err();
    assert!(err.to_string().contains("operation budget"), "{err}");
}

#[test]
fn a_colour_space_that_names_itself_is_an_error_not_a_native_stack_overflow() {
    // `[/Indexed <itself> 0 ()]` and `[/Separation /x <itself> {}]` would
    // recurse without end; the nesting is capped and reported.
    for body in [
        "/a [ /Indexed null 0 () ] def a 1 a put { a setcolorspace } stopped pop 0 0 5 5 rectfill",
        "/a [ /Separation /x null { } ] def a 2 a put a setcolorspace 0 0 5 5 rectfill",
    ] {
        let file = eps("0 0 10 10", body);
        let (w, note) = on_a_small_stack(move || {
            let (s, note) = render(&file, ImportLimits::default()).expect("the EPS draws");
            (s.width, note)
        });
        assert_eq!(w, 10, "{body}");
        if !body.contains("stopped") {
            assert!(note.contains("limitcheck"), "{note}");
        }
    }
}

#[test]
fn search_is_linear_in_its_strings() {
    // A 4 MiB string of zeros searched for 2 MiB of zeros ending in 1: quadratic would be
    // about 4e12 byte compares; linear is milliseconds.
    let file = eps(
        "0 0 10 10",
        "/h 4194304 string def /n 2097152 string def n 2097151 1 put          h n search { stop } if pop 0 0 5 5 rectfill",
    );
    let t = Instant::now();
    let (s, _) = render(&file, ImportLimits::default()).expect("the EPS draws");
    assert_eq!(px(&s, 1, 8), [0, 0, 0, 255]);
    assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    assert_eq!(find(b"abcabd", 0, b"abd"), Some(3));
    assert_eq!(find(b"aab", 1, b"ab"), Some(1));
    assert_eq!(find(b"ab", 0, b""), Some(0));
    assert_eq!(find(b"ab", 0, b"abc"), None);
}

#[test]
fn a_finished_run_frees_every_array_and_dictionary_it_made_cycles_included() {
    let ps = eps(
        "0 0 10 10",
        // systemdict holds itself; `a` holds itself; `d` holds itself; and
        // an array that holds itself is left unreachable.
        "/a [ 1 2 3 ] def a 0 a put /d 1 dict def d /self d put [ null ] dup 0 1 index put pop \
         0 0 5 5 rectfill",
    );
    let mut it = Interp::new(
        &ps,
        [1.0, 0.0, 0.0, -1.0, 0.0, 10.0],
        (10.0, 10.0),
        ImportLimits::default(),
        Budget::default(),
    );
    assert!(it.run_main().is_ok());
    assert!(it.painted);
    let arrays = it.arrays.clone();
    let mut dicts = it.dicts.clone();
    dicts.push(Rc::downgrade(&it.systemdict));
    // Anti-vacuity: the run made the containers, and they are alive now.
    let live_arrays = arrays.iter().filter(|w| w.strong_count() > 0).count();
    let live_dicts = dicts.iter().filter(|w| w.strong_count() > 0).count();
    assert!(live_arrays >= 4, "{live_arrays}");
    assert!(live_dicts >= 6, "{live_dicts}");
    drop(it);
    let leaked_arrays = arrays.iter().filter(|w| w.strong_count() > 0).count();
    let leaked_dicts = dicts.iter().filter(|w| w.strong_count() > 0).count();
    assert_eq!(
        (leaked_arrays, leaked_dicts),
        (0, 0),
        "containers outlived the run"
    );
}
