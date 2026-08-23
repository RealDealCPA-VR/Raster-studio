//! The product, end to end: open, paint, stack, save, reopen, filter, undo.
//!
//! Every test here drives [`app_shell::doc::OpenDocument`] — the engine the
//! application runs — through the calls it makes, in the order it makes them.
//! Nothing is stubbed and nothing is approximated: where a value can be
//! computed by hand it is computed by hand, and where two pipelines must agree
//! the assertion is byte equality.
//!
//! In particular every composite here goes through [`OpenDocument::composite`],
//! which draws through the same [`compositor::TileCompositor`] cache the canvas
//! is painted from. A stale-tile or missed-invalidation bug in that cache is a
//! bug the user sees, so it is a bug these tests have to see too.

use app_shell::doc::OpenDocument;
use color::{linear_to_srgb, srgb8_to_linear, srgb_to_linear, ColorSpace};
use editor_core::{
    Command, LayerPatch, Patch, PixelKey, PixelTarget, Selection, SelectionMask, MASK_TILE_BYTES,
};
use glam::IVec2;
use integration_tests::app::{self, DocExt, APP_VERSION};
use integration_tests::fixture::{
    self, differing_pixels, linear8, max_channel_diff, photo_rgba8, pixel_at, write_image,
};
use layer_model::{
    AdjustmentKind, AdjustmentLayer, BlendMode, ClippingMode, Layer, LayerEffects, LayerId,
    LayerKind, MaskId, ShadowEffect,
};
use raster::{ExportFormat, PixelFormat, Tile, TileCoord, TileGrid, TILE_SIZE};
use tools::bucket::{fill_masked, FillContent};
use tools::{
    BrushSettings, ColorPatch, PointerEvent, StrokeOp, StrokeTool, Tool, ToolContext, ToolId,
};

// ---------------------------------------------------------------------------
// 1. Opening an image file
// ---------------------------------------------------------------------------

/// A picture the user opens becomes a real raster layer, not a texture beside
/// the document: its tiles *are* the file's pixels, and the composite of the
/// document *is* the file.
#[test]
fn opening_an_image_makes_a_raster_layer_whose_tiles_are_the_source_pixels() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("holiday.png");

    // Deliberately not a multiple of TILE_SIZE. Edge tiles are padded, and the
    // padding must never be mistaken for image content.
    let (w, h) = (300u32, 200u32);
    let source = photo_rgba8(w, h);
    write_image(&path, ExportFormat::Png, w, h, &source).unwrap();

    // --- exactly what File ▸ Open runs ---
    let mut doc = app::open_image(&path);

    assert_eq!((doc.document.width(), doc.document.height()), (w, h));
    assert_eq!(doc.title(), "holiday.png");
    assert!(!doc.is_dirty(), "a just-opened file is not unsaved work");
    assert_eq!(doc.source_path(), Some(path.as_path()));

    // --- the image is in the document ---
    assert_eq!(doc.document.layers.len(), 1, "one layer, not zero");
    let layer_id = doc.document.active_layer().expect("the layer is active");
    let layer = doc.document.layers.get(layer_id).unwrap();
    assert!(
        matches!(layer.kind, LayerKind::Raster(_)),
        "the image must be a raster layer, got {:?}",
        layer.kind
    );
    assert_eq!(layer.name, "holiday.png");

    let grid = TileGrid::from_rgba8(w, h, &source).unwrap();
    let tiles = doc
        .document
        .layer_tiles(layer_id)
        .expect("the layer has pixels");
    assert_eq!(
        tiles.len(),
        grid.len(),
        "every tile the grid produced is referenced"
    );
    assert_eq!(tiles.len(), 2, "300x200 is two tiles wide and one tall");

    // --- and the tiles hold the source pixels, byte for byte ---
    for y in 0..h {
        for x in 0..w {
            let coord = TileCoord::new((x / TILE_SIZE) as i32, (y / TILE_SIZE) as i32, 0);
            let hash = tiles.get(coord).expect("a tile covering every pixel");
            let bytes = doc.tile_bytes(hash).expect("the store holds it");
            let i = ((y % TILE_SIZE) as usize * TILE_SIZE as usize + (x % TILE_SIZE) as usize) * 4;
            assert_eq!(
                &bytes[i..i + 4],
                &source[((y as usize * w as usize) + x as usize) * 4..][..4],
                "tile pixel ({x}, {y})"
            );
        }
    }

    // --- and the composite of the document is the file ---
    assert_eq!(
        doc.composite_all(),
        source,
        "what the user sees is the picture they opened"
    );

    // Opening is not an undoable step: undoing it would leave a canvas the
    // user never asked for, so `import` clears the history. That is a decision
    // rather than an accident, and it is pinned here because the tab's Edit
    // menu enablement reads exactly this answer.
    assert!(!doc.undo().unwrap(), "there is nothing before the file");
    assert_eq!(doc.document.layers.len(), 1);
    assert_eq!(doc.history_depth(), 0);
}

// ---------------------------------------------------------------------------
// 2. Painting a stroke, and taking it back
// ---------------------------------------------------------------------------

/// Distance from a point to the segment `a`-`b`, for stating where a stroke is
/// allowed to have changed anything.
fn distance_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
    let (wx, wy) = (p[0] - a[0], p[1] - a[1]);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= 0.0 {
        0.0
    } else {
        ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0)
    };
    let (dx, dy) = (wx - t * vx, wy - t * vy);
    (dx * dx + dy * dy).sqrt()
}

#[test]
fn a_brush_stroke_changes_only_the_stroked_region_and_undo_restores_it_exactly() {
    const SIZE: f32 = 21.0;
    const RADIUS: f32 = SIZE / 2.0;
    const START: [f32; 2] = [100.0, 100.0];
    const END: [f32; 2] = [160.0, 100.0];
    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    let mut doc = app::blank(512, 512, "Canvas");
    let layer = doc
        .document
        .active_layer()
        .expect("File ▸ New makes a layer");
    doc.fill_layer(layer, [90, 110, 130, 255]);

    let before_doc = doc.document.clone();
    let before = doc.composite_all();
    let steps_before = doc.history_depth();
    // What the presenter does after it has uploaded a frame: everything before
    // the stroke is already on screen.
    doc.take_dirty();

    // --- the gesture ---
    //
    // Driven through `tools` directly because `app-shell` does not route
    // pointer events into a tool yet; see `integration_tests::app::DocTiles`.
    // The tiles the tool reads and writes are the document's own, and the
    // command it produces goes through `OpenDocument::apply` like any other.
    let canvas = doc.canvas_rect();
    let commands = {
        let mut tiles = doc.tool_tiles();
        let mut ctx = ToolContext::new(&mut tiles, canvas)
            .with_layer(layer)
            .with_foreground(RED);
        let mut brush = StrokeTool::new(
            ToolId::Brush,
            BrushSettings {
                size: SIZE,
                hardness: 1.0,
                spacing: 0.1,
                smoothing: 0.0,
                size_pressure: false,
                ..BrushSettings::default()
            },
            StrokeOp::Paint { color: RED },
        );
        brush
            .on_pointer_down(&mut ctx, PointerEvent::at(START[0], START[1]))
            .unwrap();
        for step in 1..=6 {
            let x = START[0] + (END[0] - START[0]) * (step as f32 / 6.0);
            brush
                .on_pointer_move(&mut ctx, PointerEvent::at(x, START[1]))
                .unwrap();
        }
        brush
            .on_pointer_up(&mut ctx, PointerEvent::at(END[0], END[1]))
            .unwrap();
        assert!(!brush.is_active(), "the gesture ended");
        ctx.drain()
    };
    assert_eq!(
        commands.len(),
        1,
        "a whole stroke is one command, however many dabs it took"
    );
    doc.apply(commands[0].clone()).unwrap();
    assert_eq!(
        doc.history_depth(),
        steps_before + 1,
        "...and therefore one undo step"
    );

    // --- the stroke landed ---
    let after = doc.composite_all();
    let changed = differing_pixels(&before, &after, 512);
    assert!(!changed.is_empty(), "the brush painted nothing at all");

    // The cached frame the canvas draws must equal an uncached composite of the
    // same document. A cache that handed back a tile from before the stroke
    // would still produce a plausible-looking picture; it would not produce
    // this one.
    let space = doc.document.meta.color_space.clone();
    assert_eq!(
        doc.composite_uncached(canvas).to_rgba8(&space),
        after,
        "the tile cache handed back a frame the document does not describe"
    );

    // Nowhere outside the stroke moved. The tolerance is the antialiased edge
    // of the outermost dab, not a fudge factor: a dab's coverage reaches zero
    // within a pixel of its radius.
    for &(x, y) in &changed {
        let d = distance_to_segment([x as f32, y as f32], START, END);
        assert!(
            d <= RADIUS + 2.0,
            "pixel ({x}, {y}) changed but lies {d:.2}px from the stroke"
        );
    }

    // The core of the stroke is the paint colour, not a blend of it.
    for x in 105u32..=155 {
        assert_eq!(
            pixel_at(&after, 512, x, 100),
            [255, 0, 0, 255],
            "the middle of the stroke at x = {x}"
        );
    }
    // A pixel outside the brush is untouched.
    assert_eq!(
        pixel_at(&after, 512, 130, 100 + (RADIUS as u32) + 4),
        pixel_at(&before, 512, 130, 100 + (RADIUS as u32) + 4)
    );
    assert!(
        changed.len() > 600,
        "a 60px stroke {SIZE}px wide covered only {} pixels",
        changed.len()
    );

    // ...and the presenter is told to upload exactly the tile the stroke
    // touched, not the whole canvas.
    let dirty = doc.take_dirty();
    assert!(
        !dirty.is_all(),
        "a small stroke is not a full-canvas upload"
    );
    assert_eq!(
        dirty.tiles().collect::<Vec<_>>(),
        vec![TileCoord::new(0, 0, 0)],
        "the stroke lies entirely inside the first tile"
    );

    // --- and one undo puts every pixel back ---
    assert!(doc.undo().unwrap());
    assert_eq!(
        doc.composite_all(),
        before,
        "undo must restore the pixels byte for byte, not approximately"
    );
    assert_eq!(
        doc.document, before_doc,
        "and the document itself is the one that was there"
    );

    // Redo puts it back again — the stroke was not lost, only reversed.
    assert!(doc.redo().unwrap());
    assert_eq!(doc.composite_all(), after);
}

// ---------------------------------------------------------------------------
// 3. A layered document, against values computed by hand
// ---------------------------------------------------------------------------

// Stored 8-bit codes. The document below works in *linear* sRGB, so a code `v`
// decodes to exactly `v / 255` and every reference value here is arithmetic
// anyone can check on paper.
const BACKDROP: u8 = 102; // 0.4
const GROUP_LOWER: u8 = 204; // 0.8
const GROUP_UPPER: u8 = 153; // 0.6
const CLIP_BASE: u8 = 201;
const CLIP_TOP: u8 = 51; // 0.2
/// Group-mask coverage, one value per canvas column.
const MASK_COLUMN: [u8; 4] = [0, 85, 170, 255];
/// The clipping base's own alpha, one value per canvas row — its *shape*.
const BASE_ALPHA_ROW: [u8; 4] = [255, 170, 85, 0];
/// One stop down: the adjustment layer halves the light beneath it.
const EXPOSURE_STOPS: f32 = -1.0;

/// The document under test, with every part addressable.
struct Layered {
    doc: OpenDocument,
    group: LayerId,
    group_mask: MaskId,
    clip_base: LayerId,
}

fn code(v: u8) -> f32 {
    v as f32 / 255.0
}

/// A group of two differently blended layers under a mask, a clipping pair, and
/// an adjustment over the lot — built entirely through commands.
fn layered_document() -> Layered {
    let mut doc = app::linear(4, 4, "Layered");
    let coords = doc.canvas_tiles();

    // Bottom of the stack: the layer File ▸ New already made.
    let backdrop = doc
        .document
        .active_layer()
        .expect("File ▸ New makes a layer");
    doc.fill_layer(backdrop, [BACKDROP, BACKDROP, BACKDROP, 255]);

    // An isolated group of two layers with different blend modes.
    let group = doc.add_layer(Layer::group("Group"));
    let lower = doc.add_child(group, Layer::raster("Lower"));
    doc.fill_layer(lower, [GROUP_LOWER, GROUP_LOWER, GROUP_LOWER, 255]);
    let upper = doc.add_child(group, Layer::raster("Upper"));
    doc.fill_layer(upper, [GROUP_UPPER, GROUP_UPPER, GROUP_UPPER, 255]);
    doc.set_props(
        upper,
        LayerPatch {
            blend_mode: Some(BlendMode::Multiply),
            ..Default::default()
        },
    );

    // ...under a layer mask that varies across the canvas.
    let group_mask = doc.attach_mask(group);
    doc.paint_mask(group, &coords, &|_, x, _| {
        MASK_COLUMN[(x as usize).min(MASK_COLUMN.len() - 1)]
    });

    // A clipping pair: a base whose alpha is its shape, and a layer clipped to
    // it that may recolour that shape but never extend it.
    let clip_base = doc.add_layer(Layer::raster("Clip base"));
    doc.paint_layer(clip_base, &coords, &|_, _, y| {
        let a = BASE_ALPHA_ROW[(y as usize).min(BASE_ALPHA_ROW.len() - 1)];
        [CLIP_BASE, CLIP_BASE, CLIP_BASE, a]
    });
    let clip_top = doc.add_layer(Layer::raster("Clipped"));
    doc.fill_layer(clip_top, [CLIP_TOP, CLIP_TOP, CLIP_TOP, 255]);
    doc.set_props(
        clip_top,
        LayerPatch {
            blend_mode: Some(BlendMode::Screen),
            clipping: Some(ClippingMode::ClipToBelow),
            ..Default::default()
        },
    );

    // ...and an adjustment layer over everything.
    doc.add_layer(Layer::with_kind(
        "Exposure",
        LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Exposure {
                stops: EXPOSURE_STOPS,
            },
        }),
    ));

    Layered {
        doc,
        group,
        group_mask,
        clip_base,
    }
}

// The same stack, at the size and in the working space the product actually
// runs in. `layered_document` above is 4x4 and linear because its whole job is
// to be checkable on paper; that makes it the wrong fixture for anything about
// *geometry* or *encoding*:
//
//   * 4x4 lives inside a single tile, and 99.98% of the one tile blob it stores
//     is off-canvas padding — so a package writer that dropped an edge tile,
//     transposed a row stride or lost a shard would round-trip it perfectly;
//   * every layer in it is a flat fill, and `fixture.rs` argues at length that
//     "a flat colour survives almost any mistake ... so a fixture that is flat
//     proves nothing about geometry";
//   * and linear sRGB is a working space no application path can produce (see
//     `app::linear`), so `Canvas::to_rgba8` collapses to `round(v * 255)` and
//     the transfer curve the product always applies is never run.
//
// This one is 600x400: three tiles across and two down, a multiple of TILE_SIZE
// in neither axis, so both edge tiles are partial. Every layer, the mask and the
// clipping base's shape are two-axis functions or real fixture pixels. The
// working space is whatever `File ▸ New` gives, which is sRGB.
const PHOTO_W: u32 = 600;
const PHOTO_H: u32 = 400;

/// One pixel of a full-canvas RGBA8 fixture buffer.
fn canvas_sample(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y as usize * PHOTO_W as usize) + x as usize) * 4;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// The same stack as [`layered_document`] — group, blend modes, layer mask,
/// clipping pair, adjustment — built out of real picture content on a canvas
/// that spans several tiles, in the product's own sRGB working space.
fn photo_layered_document() -> Layered {
    let mut doc = app::blank(PHOTO_W, PHOTO_H, "Photo stack");

    // Bottom of the stack: the picture, opaque, varying on both axes and in all
    // three channels.
    let backdrop = doc
        .document
        .active_layer()
        .expect("File ▸ New makes a layer");
    let photo = photo_rgba8(PHOTO_W, PHOTO_H);
    doc.paint_canvas(backdrop, &|x, y| canvas_sample(&photo, x, y));

    // An isolated group of two differently blended layers. The lower one
    // carries partial alpha, so the group's own buffer is not opaque.
    let group = doc.add_layer(Layer::group("Group"));
    let lower = doc.add_child(group, Layer::raster("Lower"));
    let translucent = fixture::photo_rgba8_with_alpha(PHOTO_W, PHOTO_H);
    doc.paint_canvas(lower, &|x, y| canvas_sample(&translucent, x, y));
    // Every generated channel here is taken modulo a value that is *not* 256.
    // A function of `x % 256` repeats exactly once per tile, so a layer built
    // from one would store the same blob in every full tile — which is the flat
    // fixture problem wearing a pattern. `the_persistence_fixture_is_multi_tile_
    // srgb_and_is_not_flat` checks that it did not happen.
    let upper = doc.add_child(group, Layer::raster("Upper"));
    doc.paint_canvas(upper, &|x, y| {
        [
            ((x * 7 + y * 3) % 251) as u8,
            ((y * 5 + x / 3) % 241) as u8,
            ((x * y / 11) % 239) as u8,
            255,
        ]
    });
    doc.set_props(
        upper,
        LayerPatch {
            blend_mode: Some(BlendMode::Multiply),
            ..Default::default()
        },
    );

    // ...under a layer mask whose coverage varies on both axes, so a mask tile
    // that came back transposed or off by a shard is a visible difference.
    let group_mask = doc.attach_mask(group);
    doc.paint_canvas_mask(group, &|x, y| (((x * 3) ^ (y * 5)) % 251) as u8);

    // A clipping pair: a base whose alpha is its shape — here a diagonal ramp
    // rather than four flat rows — and a layer clipped to it.
    let clip_base = doc.add_layer(Layer::raster("Clip base"));
    doc.paint_canvas(clip_base, &|x, y| {
        let [r, g, b, _] = canvas_sample(&photo, x, y);
        [r, g, b, ((x + y * 2) % 251) as u8]
    });
    let clip_top = doc.add_layer(Layer::raster("Clipped"));
    doc.paint_canvas(clip_top, &|x, y| {
        [
            (251 - (x % 251)) as u8,
            ((x * 3 + y) % 253) as u8,
            ((y * 11 + x) % 247) as u8,
            255,
        ]
    });
    doc.set_props(
        clip_top,
        LayerPatch {
            blend_mode: Some(BlendMode::Screen),
            clipping: Some(ClippingMode::ClipToBelow),
            ..Default::default()
        },
    );

    // ...and an adjustment layer over everything.
    doc.add_layer(Layer::with_kind(
        "Exposure",
        LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Exposure {
                stops: EXPOSURE_STOPS,
            },
        }),
    ));

    Layered {
        doc,
        group,
        group_mask,
        clip_base,
    }
}

/// The fixture's own premises, so a test built on it cannot be quietly weakened
/// by the fixture shrinking back to one flat tile.
#[test]
fn the_persistence_fixture_is_multi_tile_srgb_and_is_not_flat() {
    let mut l = photo_layered_document();

    assert_eq!(
        l.doc.document.meta.color_space,
        ColorSpace::Srgb,
        "the persistence fixture must be in the space the product ships in, \
         or the sRGB transfer curve goes untested"
    );

    let tiles = l.doc.canvas_tiles();
    assert_eq!(tiles.len(), 6, "600x400 is three tiles across and two down");
    assert_ne!(
        PHOTO_W % TILE_SIZE,
        0,
        "the right-hand tiles must be partial"
    );
    assert_ne!(PHOTO_H % TILE_SIZE, 0, "the bottom tiles must be partial");

    // Every layer that carries pixels stores a distinct blob per tile: a fixture
    // whose layers were flat would store *one* blob and reference it six times,
    // and a package writer that lost five of six tiles would still round-trip.
    for name in ["Layer 1", "Lower", "Upper", "Clip base", "Clipped"] {
        let id = l
            .doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| l.doc.document.layers.get(*id).unwrap().name == name)
            .unwrap_or_else(|| panic!("no layer named `{name}`"));
        let map = l
            .doc
            .document
            .layer_tiles(id)
            .unwrap_or_else(|| panic!("`{name}` has no pixels"));
        let hashes: std::collections::HashSet<_> = tiles.iter().map(|c| map.get(*c)).collect();
        assert_eq!(
            hashes.len(),
            tiles.len(),
            "`{name}` stores the same blob in more than one tile, so it is flat"
        );
    }

    // The mask is on the same hook: a mask that stored one blob six times would
    // survive a package writer that dropped five of them.
    let group = l.group;
    let mask_map = l
        .doc
        .document
        .mask_tiles(group)
        .expect("the group's mask has coverage tiles");
    let mask_hashes: std::collections::HashSet<_> =
        tiles.iter().map(|c| mask_map.get(*c)).collect();
    assert_eq!(
        mask_hashes.len(),
        tiles.len(),
        "the group mask stores the same blob in more than one tile, so it is flat"
    );

    // ...and the composite that comes out of it is a picture, not a wash.
    let frame = l.doc.composite_all();
    assert_eq!(frame.len(), (PHOTO_W * PHOTO_H * 4) as usize);
    let distinct: std::collections::BTreeSet<&[u8]> = frame.chunks_exact(4).collect();
    assert!(
        distinct.len() > 10_000,
        "the composite collapsed to {} distinct colours",
        distinct.len()
    );
}

/// What the pixel at `(x, y)` must be, derived from the documented compositing
/// rules rather than from the compositor.
///
/// Every layer here is grey, so one channel is the whole answer, and every
/// layer covers the whole canvas at full opacity, so the accumulator's alpha is
/// 1 throughout and premultiplied colour equals straight colour.
fn reference_pixel(x: usize, y: usize) -> f32 {
    let mask = code(MASK_COLUMN[x]);
    let shape = code(BASE_ALPHA_ROW[y]);

    // The isolated group renders its children into its own buffer: `Lower` over
    // nothing, then `Upper` multiplied onto it.
    let group_rgb = code(GROUP_LOWER) * code(GROUP_UPPER);
    // The group buffer is blended down through its mask, which scales the
    // source alpha and therefore mixes with the backdrop.
    let after_group = group_rgb * mask + code(BACKDROP) * (1.0 - mask);

    // The clipping group: `Clipped` is composited *atop* the base's shape, so
    // `Cs' = (1 - ab) * Cs + ab * Screen(Cb, Cs)` and the alpha stays the
    // base's...
    let base_rgb = code(CLIP_BASE);
    let top_rgb = code(CLIP_TOP);
    let screen = base_rgb + top_rgb - base_rgb * top_rgb;
    let clipped_rgb = (1.0 - shape) * top_rgb + shape * screen;
    // ...and the finished buffer blends down with the *base's* alpha, mode and
    // opacity.
    let after_clip = shape * clipped_rgb + after_group * (1.0 - shape);

    // The adjustment rewrites the backdrop beneath it. Exposure is defined on
    // light, so one stop down is exactly a halving.
    after_clip * 2.0f32.powf(EXPOSURE_STOPS)
}

#[test]
fn a_layered_document_composites_to_the_values_the_model_says_it_should() {
    let mut l = layered_document();
    let rect = l.doc.canvas_rect();

    // The frame the canvas draws, through the tile cache, encoded for the
    // screen. Compared byte for byte against the reference: the document works
    // in linear sRGB, so encoding a reference value is `round(v * 255)` and
    // nothing about the comparison is approximate.
    let frame = l.doc.composite_all();
    let mut want_bytes = Vec::with_capacity(4 * 16);
    for y in 0..4usize {
        for x in 0..4usize {
            let v = linear8(reference_pixel(x, y));
            want_bytes.extend_from_slice(&[v, v, v, 255]);
        }
    }
    assert_eq!(
        frame, want_bytes,
        "the composited frame is not what the layer model says it should be"
    );

    // The same document with no cache in the way, in full `f32` precision — so
    // the reference is checked to a hundredth of an 8-bit code, and the cached
    // frame above is checked to be the same picture.
    let canvas = l.doc.composite_uncached(rect);
    for y in 0..4usize {
        for x in 0..4usize {
            let want = reference_pixel(x, y);
            let got = canvas.get(x as i64, y as i64);
            for (c, channel) in got.iter().take(3).enumerate() {
                assert!(
                    (channel - want).abs() < 1e-5,
                    "({x}, {y}) channel {c}: got {channel}, reference {want}"
                );
            }
            assert!(
                (got[3] - 1.0).abs() < 1e-6,
                "({x}, {y}) alpha: an opaque backdrop under everything stays opaque, got {}",
                got[3]
            );
        }
    }

    // The fixture would be worthless if every pixel came out the same.
    let distinct: std::collections::BTreeSet<u8> = (0..4)
        .flat_map(|y| (0..4).map(move |x| linear8(reference_pixel(x, y))))
        .collect();
    assert!(
        distinct.len() >= 10,
        "the reference collapsed to {} distinct values",
        distinct.len()
    );
}

#[test]
fn each_part_of_the_stack_is_load_bearing() {
    // A composite that does not change when a layer is switched off proves
    // nothing about that layer. Each of these is the same document with one
    // piece disabled, and each must look different.
    let baseline = layered_document().doc.composite_all();

    let mut without_mask = layered_document();
    let group = without_mask.group;
    without_mask.doc.set_props(
        group,
        LayerPatch {
            mask: Patch::Clear,
            ..Default::default()
        },
    );
    assert_ne!(
        without_mask.doc.composite_all(),
        baseline,
        "the group mask changes the picture"
    );

    let mut without_clip_base = layered_document();
    let clip_base = without_clip_base.clip_base;
    without_clip_base.doc.set_props(
        clip_base,
        LayerPatch {
            visible: Some(false),
            ..Default::default()
        },
    );
    assert_ne!(
        without_clip_base.doc.composite_all(),
        baseline,
        "hiding a clipping base hides the whole clipping group"
    );
}

// ---------------------------------------------------------------------------
// 4. Save, close, reopen — the single most important test here
// ---------------------------------------------------------------------------

/// Where the round-tripped selection sits: astride the boundary between the
/// first and second tile column, so the mask is not trivially inside one tile.
const SELECTION_ORIGIN: IVec2 = IVec2::new(250, 180);
const SELECTION_SIZE: (u32, u32) = (12, 8);

#[test]
fn a_saved_document_reopens_and_composites_to_byte_identical_output() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("Layered.rstudio");

    // The multi-tile, content-rich, sRGB fixture — not the 4x4 linear one. A
    // package writer only has multi-tile paths (several blobs per layer, shard
    // directories, partial edge tiles, row strides over content that varies in
    // both axes) if the document handed to it has more than one tile of more
    // than one colour. `the_persistence_fixture_is_multi_tile_srgb_and_is_not_
    // flat` pins those premises.
    let mut l = photo_layered_document();

    // Give the document the rest of what has to survive: a layer style and a
    // selection. (A selection is a field on the document rather than a command
    // target — `editor-core` has no selection command yet, and `app-shell`
    // writes it in place for the same reason — so it is set in place here too.)
    let shadow = ShadowEffect {
        distance_px: 12.0,
        size_px: 7.5,
        spread: 0.25,
        ..ShadowEffect::default()
    };
    let clip_base = l.clip_base;
    l.doc.set_props(
        clip_base,
        LayerPatch {
            effects: Some(Box::new(LayerEffects {
                drop_shadow: Some(shadow.clone()),
                ..LayerEffects::default()
            })),
            ..Default::default()
        },
    );
    let (sw, sh) = SELECTION_SIZE;
    let coverage: Vec<u8> = (0..sw * sh).map(|i| (255 - (i % 256)) as u8).collect();
    l.doc.document.selection =
        Selection::Mask(SelectionMask::new(SELECTION_ORIGIN, sw, sh, coverage.clone()).unwrap());

    let before = l.doc.composite_all();
    let group_mask_before = l.group_mask;

    // --- save ---
    l.doc.save_to(&package, APP_VERSION).unwrap();
    assert!(!l.doc.is_dirty(), "a save clears the unsaved-work flag");
    let doc_before = l.doc.document.clone();

    // --- close and reopen: nothing of the session survives but the package ---
    drop(l);
    let mut back = app::open_project(&package);

    // The document that came back *is* the document that went in.
    assert_eq!(
        back.document, doc_before,
        "content, layer tree, pixel references, selection and active layer"
    );
    assert!(!back.is_dirty());
    assert_eq!(back.project_path(), Some(package.as_path()));

    // And so are its pixels — composited through the cache, as the canvas will.
    assert_eq!(
        back.composite_all(),
        before,
        "a reopened document must composite byte-identically"
    );

    // Spell out the parts, so a failure says which one was lost.
    let named = |name: &str| -> LayerId {
        back.document
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| back.document.layers.get(*id).unwrap().name == name)
            .unwrap_or_else(|| panic!("`{name}` did not survive the round trip"))
    };
    let group = named("Group");
    assert_eq!(
        back.document.layers.get(group).unwrap().mask_id(),
        Some(group_mask_before),
        "the layer mask survived, under the same identity"
    );

    // Every tile of every layer and of the mask came back, and the package
    // actually holds the bytes each one names. "The map is `Some`" would pass on
    // a package that wrote one blob and referenced it six times; this does not.
    let canvas_tiles = back.canvas_tiles();
    assert_eq!(
        canvas_tiles.len(),
        6,
        "the reopened canvas is still six tiles"
    );
    let mask_map = back
        .document
        .mask_tiles(group)
        .expect("and so did the mask's coverage tiles");
    for coord in &canvas_tiles {
        let hash = mask_map
            .get(*coord)
            .unwrap_or_else(|| panic!("the mask lost its tile at {coord:?}"));
        assert_eq!(
            back.tile_bytes(hash).map(<[u8]>::len),
            Some(MASK_TILE_BYTES),
            "the package holds no coverage bytes for the mask tile at {coord:?}"
        );
    }
    for name in ["Layer 1", "Lower", "Upper", "Clip base", "Clipped"] {
        let map = back
            .document
            .layer_tiles(named(name))
            .unwrap_or_else(|| panic!("`{name}` came back with no pixels"));
        for coord in &canvas_tiles {
            let hash = map
                .get(*coord)
                .unwrap_or_else(|| panic!("`{name}` lost its tile at {coord:?}"));
            assert_eq!(
                back.tile_bytes(hash).map(<[u8]>::len),
                Some(Tile::byte_len(PixelFormat::Rgba8)),
                "the package holds no pixels for `{name}` at {coord:?}"
            );
        }
    }
    let clip_base = named("Clip base");
    assert_eq!(
        back.document
            .layers
            .get(clip_base)
            .unwrap()
            .effects
            .drop_shadow,
        Some(shadow),
        "the layer style survived"
    );
    assert!(
        matches!(back.document.selection, Selection::Mask(_)),
        "the selection survived"
    );
    for (i, want) in coverage.iter().enumerate() {
        let p = IVec2::new(
            SELECTION_ORIGIN.x + (i as u32 % sw) as i32,
            SELECTION_ORIGIN.y + (i as u32 / sw) as i32,
        );
        assert_eq!(
            back.document.selection.coverage_at(p),
            *want as f32 / 255.0,
            "...with its coverage intact, at {p:?}"
        );
    }
    assert_eq!(
        back.document
            .selection
            .coverage_at(SELECTION_ORIGIN - IVec2::ONE),
        0.0,
        "...and nothing outside it selected"
    );

    // And it is still editable: the point of the whole format is "save, close,
    // reopen, and carry on".
    let extra = back.add_layer(Layer::raster("Added after reopening"));
    back.fill_layer(extra, [255, 255, 255, 255]);
    assert_ne!(back.composite_all(), before, "the new layer is visible");
    assert!(back.undo().unwrap());
    assert!(back.undo().unwrap());
    assert_eq!(
        back.composite_all(),
        before,
        "editing after a reopen is undoable back to what was loaded"
    );
}

// ---------------------------------------------------------------------------
// 5. Working through a feathered selection
// ---------------------------------------------------------------------------

/// The straight-alpha sRGB8 code a linear channel value is stored as.
///
/// The same two steps `tools::patch` and `compositor::Canvas` both take on the
/// way out: transfer curve, then round.
fn store_code(v: f32) -> u8 {
    (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8
}

/// The feathered marquee both tests below work through.
///
/// A rectangle refined by `selection::modify::feather`, so its edge carries
/// every coverage between 0 and 255 rather than only the two ends.
fn feathered_marquee() -> SelectionMask {
    let shape =
        selection::marquee::rectangle(selection::Rect::from_xywh(64, 64, 128, 128)).unwrap();
    selection::modify::feather(&shape, 8.0).unwrap()
}

/// A pixel on the feathered edge, and the coverage byte the selection pipeline
/// stores for it.
///
/// Pinned as a constant so the expectations below are arithmetic on known
/// numbers rather than on whatever the mask happens to say. If `feather`
/// changes, this is the assertion that says so first.
const EDGE_PIXEL: IVec2 = IVec2::new(64, 128);
const EDGE_COVERAGE: u8 = 147;

#[test]
fn the_feathered_edge_carries_the_partial_coverage_the_tests_below_rely_on() {
    let mask = feathered_marquee();
    assert_eq!(
        mask.coverage_at(EDGE_PIXEL),
        EDGE_COVERAGE,
        "the pinned edge pixel no longer has the coverage the expectations use"
    );
    assert_eq!(mask.coverage_at(IVec2::new(128, 128)), 255, "the interior");
    assert_eq!(mask.coverage_at(IVec2::new(8, 8)), 0, "well outside");

    // ...and the edge really is a ramp, not a step.
    let partial = (56..72)
        .map(|x| mask.coverage_at(IVec2::new(x, 128)))
        .filter(|c| *c > 0 && *c < 255)
        .count();
    assert!(partial >= 8, "only {partial} partially covered edge pixels");
}

/// A **production** operation applied through the feathered selection.
///
/// [`tools::bucket::fill_masked`] is what the paint bucket, the pattern fill
/// and Edit ▸ Fill all run, and its doc comment states the property this test
/// checks: "a fill through a feathered selection lands identically however it
/// was invoked". The law is source-over at `a = source alpha * coverage *
/// opacity`, so with an opaque fill at full opacity over an opaque layer the
/// result is exactly `original + (fill - original) * coverage` in linear light
/// — which is computed here from the fill colour, the layer's flat colour and
/// the mask's stored byte, none of which comes from the write path.
#[test]
fn a_fill_through_a_feathered_selection_blends_by_coverage_in_production_code() {
    // Flat, opaque, and a value with no special structure, so every expected
    // number below is one multiplication away from constants stated here.
    const BASE: [u8; 4] = [64, 64, 64, 255];
    /// The fill, as straight-alpha **linear** RGBA — what `FillContent::Color`
    /// takes.
    const FILL: [f32; 4] = [0.75, 0.25, 0.5, 1.0];

    let mut doc = app::blank(TILE_SIZE, TILE_SIZE, "Filled");
    let layer = doc.document.active_layer().unwrap();
    doc.fill_layer(layer, BASE);
    let before = doc.composite_all();

    let mask = feathered_marquee();
    doc.document.selection = Selection::Mask(mask.clone());

    let key = PixelKey::Layer(layer);
    let region = doc.canvas_rect();
    let delta = {
        let mut tiles = doc.tool_tiles();
        let mut patch = ColorPatch::load(&tiles, key, region).unwrap();
        fill_masked(
            &mut patch,
            &mask,
            &FillContent::Color(FILL),
            [0.0; 4],
            None,
            1.0,
        )
        .unwrap();
        patch.commit(&mut tiles, key).unwrap()
    };
    assert!(!delta.is_empty(), "the fill changed nothing");
    doc.apply(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta,
    })
    .unwrap();
    let after = doc.composite_all();

    // --- nothing outside the selection moved ---
    for (x, y) in differing_pixels(&before, &after, TILE_SIZE) {
        assert!(
            mask.coverage_at(IVec2::new(x as i32, y as i32)) > 0,
            "pixel ({x}, {y}) changed but has zero coverage"
        );
    }

    // --- the pinned edge pixel, computed by hand ---
    //
    // base 64/255 sRGB decodes to a known linear value; coverage is
    // EDGE_COVERAGE (147) of 255;
    // the result must lie exactly that fraction of the way to the fill.
    let base_lin = srgb8_to_linear(BASE[0]);
    let c = EDGE_COVERAGE as f32 / 255.0;
    let want_edge = [
        store_code(base_lin + (FILL[0] - base_lin) * c),
        store_code(base_lin + (FILL[1] - base_lin) * c),
        store_code(base_lin + (FILL[2] - base_lin) * c),
        255,
    ];
    let got_edge = pixel_at(&after, TILE_SIZE, EDGE_PIXEL.x as u32, EDGE_PIXEL.y as u32);
    for ch in 0..4 {
        assert!(
            got_edge[ch].abs_diff(want_edge[ch]) <= 1,
            "edge pixel {EDGE_PIXEL:?} channel {ch}: got {}, want {} \
             (base {base_lin}, fill {}, coverage {c})",
            got_edge[ch],
            want_edge[ch],
            FILL[ch.min(2)]
        );
    }
    // The fully covered interior is the fill itself, and the outside is the
    // layer — so the ramp above really is a ramp between two different things.
    assert_eq!(
        pixel_at(&after, TILE_SIZE, 128, 128),
        [
            store_code(FILL[0]),
            store_code(FILL[1]),
            store_code(FILL[2]),
            255
        ],
        "the fully selected interior is the fill"
    );
    assert_eq!(
        pixel_at(&after, TILE_SIZE, 8, 8),
        BASE,
        "outside is untouched"
    );

    // --- and every partially covered pixel obeys the same law ---
    let mut partial = 0usize;
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let cov = mask.coverage_at(IVec2::new(x as i32, y as i32));
            if cov == 0 || cov == 255 {
                continue;
            }
            partial += 1;
            let c = cov as f32 / 255.0;
            let got = pixel_at(&after, TILE_SIZE, x, y);
            for ch in 0..3 {
                let want = store_code(base_lin + (FILL[ch] - base_lin) * c);
                assert!(
                    got[ch].abs_diff(want) <= 1,
                    "({x}, {y}) channel {ch} at coverage {cov}: got {}, want {want}",
                    got[ch]
                );
            }
            assert_eq!(
                got[3], 255,
                "an opaque fill on an opaque layer stays opaque"
            );
        }
    }
    assert!(
        partial > 500,
        "the feathered edge produced only {partial} partially covered pixels"
    );

    // ...and one undo takes it all back.
    assert!(doc.undo().unwrap());
    assert_eq!(doc.composite_all(), before);
}

/// `filters::stylize::solarize`, computed from its documented contract rather
/// than read back out of the buffer the write path used.
///
/// The filter's own doc comment states the rule, and states that it is defined
/// on the **gamma-encoded** value rather than on linear light: encode the
/// channel to sRGB, reflect everything from mid-tone up (`e -> 1 - e`), decode
/// back. A channel stored as the 8-bit code `v` decodes and re-encodes to
/// exactly `v / 255`, so for the opaque sRGB8 layer below the whole fold is
/// arithmetic on the stored byte — no `FilterBuffer` anywhere in it.
///
/// This is what makes the expectations below an oracle instead of an echo. If
/// `solarize` stops folding the way it says it does, this function does not
/// follow it, and the comparison goes red.
fn solarized_linear(code: u8) -> f32 {
    let e = code as f32 / 255.0;
    let folded = if e < 0.5 { e } else { 1.0 - e };
    srgb_to_linear(folded)
}

/// Pixels on the feathered edge, with the coverage byte `selection::modify::
/// feather` stores for each.
///
/// Constants, for the same reason [`EDGE_COVERAGE`] is one: the filter test's
/// write loop reads coverage out of the very mask its expectation would
/// otherwise read, so a defect in `feather` would move both sides of the
/// comparison by the same amount and cancel. Pinning the bytes here breaks that
/// symmetry — a `feather` that changes its falloff fails this list first, and
/// the per-pixel expectations built on it second.
///
/// The rectangle is `(64, 64, 128, 128)` feathered by 8, so these walk the left
/// edge and the top edge from nearly clear to nearly solid, plus the corner
/// where the two falloffs multiply.
const EDGE_SAMPLES: &[(IVec2, u8)] = &[
    (IVec2::new(60, 128), 24),
    (IVec2::new(62, 128), 73),
    (IVec2::new(64, 128), 147),
    (IVec2::new(66, 128), 211),
    (IVec2::new(68, 128), 244),
    (IVec2::new(128, 60), 24),
    (IVec2::new(128, 64), 147),
    (IVec2::new(128, 68), 244),
    (IVec2::new(64, 64), 84),
    (IVec2::new(66, 66), 175),
];

/// A **filter** applied through the same feathered selection.
///
/// # The gap this test is honest about
///
/// There is no production "apply this filter through the selection" call
/// anywhere in the workspace. `filters` transforms whole buffers and knows
/// nothing about selections; `tools` blends through a selection but only for
/// the operations its own tools perform (fill, red-eye, patch, stroke). So the
/// per-pixel blend below is written *here*, and this test cannot claim to
/// exercise a product path that does not exist. It is filed as a product gap.
///
/// # Why the expectation is not the write expression read back
///
/// An earlier version of this test computed each expected pixel from
/// `filtered.get(x, y)` — the buffer the write loop had just blended in — and
/// from `mask.coverage_at(p)` — the mask the write loop had just read. Both
/// sides then moved together: breaking `solarize`'s fold or `feather`'s falloff
/// changed the written pixel and the expectation by the same amount and the
/// test stayed green. Both oracles are now independent of the write:
///
/// * the original is the source code this test painted, `source_code(x, y)` —
///   a number the test chose rather than one it measured. The pre-edit
///   composite `before` is not the source of it; it is checked *against* it,
///   pixel by pixel, by the premise loop below, and is otherwise used only to
///   prove the filter did something and that undo restores the frame,
/// * the filtered value comes from [`solarized_linear`], which is the filter's
///   documented fold applied to that original byte,
/// * the coverage at the pinned pixels comes from [`EDGE_SAMPLES`], which are
///   constants,
/// * and the result is read out of the composited frame **after** the edit,
///   through `ColorPatch`'s encode and the compositor's decode.
///
/// So a defect in `selection::modify::feather`, in `filters::stylize::solarize`,
/// in `tools::patch`'s encode/decode, in `Command::PaintTiles` or in the tile
/// cache fails this test; only the blending law itself is the test's own.
#[test]
fn a_filter_runs_only_inside_the_selection_and_fades_across_its_feather() {
    // The source codes are stated here rather than measured, so every
    // expectation below is arithmetic on numbers this test chose. The canvas is
    // exactly one tile, so a tile-local coordinate is a document coordinate.
    fn source_code(x: u32, y: u32) -> [u8; 4] {
        [x as u8, y as u8, ((x + y) / 2) as u8, 255]
    }

    let mut doc = app::blank(TILE_SIZE, TILE_SIZE, "Filtered");
    let layer = doc.document.active_layer().unwrap();
    doc.paint_canvas(layer, &source_code);

    let mask = feathered_marquee();
    doc.document.selection = Selection::Mask(mask.clone());
    let selection = doc.document.selection.clone();

    let before = doc.composite_all();
    // The layer went in as sRGB8 and comes back as sRGB8: if the composite did
    // not return the codes that were painted, every hand-computed value below
    // would be measuring the wrong input.
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            assert_eq!(
                pixel_at(&before, TILE_SIZE, x, y),
                source_code(x, y),
                "the unedited composite at ({x}, {y}) is not what was painted"
            );
        }
    }

    let key = PixelKey::Layer(layer);
    let region = doc.canvas_rect();

    // --- what "Filter ▸ Stylize ▸ Solarize" would do to a selection ---
    let delta = {
        let mut tiles = doc.tool_tiles();
        let mut patch = ColorPatch::load(&tiles, key, region).unwrap();
        let filtered = filters::stylize::solarize(patch.buffer());
        let origin = patch.origin();
        let (pw, ph) = (patch.width(), patch.height());
        for y in 0..ph {
            for x in 0..pw {
                let p = IVec2::new(origin.x + x as i32, origin.y + y as i32);
                let coverage = selection.coverage_at(p);
                if coverage <= 0.0 {
                    continue;
                }
                let src = patch.get(p);
                let dst = filtered.get(x, y);
                let mut mixed = [0.0f32; 4];
                for (i, m) in mixed.iter_mut().enumerate() {
                    *m = src[i] + (dst[i] - src[i]) * coverage;
                }
                patch.set(p, mixed);
            }
        }
        patch.commit(&mut tiles, key).unwrap()
    };
    assert!(!delta.is_empty(), "the filter changed nothing");
    doc.apply(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta,
    })
    .unwrap();
    let after = doc.composite_all();

    // --- only the selection moved ---
    for (x, y) in differing_pixels(&before, &after, TILE_SIZE) {
        assert!(
            mask.coverage_at(IVec2::new(x as i32, y as i32)) > 0,
            "pixel ({x}, {y}) changed but is not selected at all"
        );
    }

    // --- the pinned pixels: both halves of the expectation are constants ---
    //
    // This is the block that bites when `feather`'s falloff moves: the coverage
    // it uses is [`EDGE_SAMPLES`], not the mask.
    for &(p, coverage) in EDGE_SAMPLES {
        assert_eq!(
            mask.coverage_at(p),
            coverage,
            "the pinned coverage at {p:?} no longer matches what `feather` \
             produces, so the expectations built on it are stale"
        );
        assert!(
            coverage > 0 && coverage < 255,
            "{p:?} is not on the feathered edge at all"
        );
        let orig = source_code(p.x as u32, p.y as u32);
        let c = coverage as f32 / 255.0;
        let got = pixel_at(&after, TILE_SIZE, p.x as u32, p.y as u32);
        for ch in 0..3 {
            let o = srgb8_to_linear(orig[ch]);
            let want = store_code(o + (solarized_linear(orig[ch]) - o) * c);
            assert!(
                got[ch].abs_diff(want) <= 1,
                "pinned pixel {p:?} channel {ch} at coverage {coverage}: \
                 got {}, want {want} (source code {})",
                got[ch],
                orig[ch]
            );
        }
    }

    // --- and at every pixel the result lies `coverage` of the way from the
    //     original to the filter's documented fold of it, along each channel ---
    let mut partial = 0usize;
    let mut fully = 0usize;
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let cov = mask.coverage_at(IVec2::new(x as i32, y as i32));
            let c = cov as f32 / 255.0;
            let orig = source_code(x, y);
            let got = pixel_at(&after, TILE_SIZE, x, y);
            if cov == 0 {
                assert_eq!(got, orig, "unselected pixel ({x}, {y}) must be untouched");
                continue;
            }
            for ch in 0..3 {
                // Straight-alpha linear light: the layer is opaque throughout,
                // so the premultiplied plane the filter works in and the
                // straight values read back agree channel for channel.
                let o = srgb8_to_linear(orig[ch]);
                let want = store_code(o + (solarized_linear(orig[ch]) - o) * c);
                assert!(
                    got[ch].abs_diff(want) <= 1,
                    "({x}, {y}) channel {ch} at coverage {cov}: got {}, want {want}",
                    got[ch]
                );
            }
            assert_eq!(got[3], 255, "an opaque layer stays opaque");
            if cov == 255 {
                fully += 1;
            } else {
                partial += 1;
            }
        }
    }
    assert!(
        partial > 500,
        "the feathered edge produced only {partial} partially covered pixels"
    );
    assert!(fully > 5000, "only {fully} fully selected pixels");

    // The filter has to be doing something, or the whole comparison is vacuous.
    let centre = pixel_at(&before, TILE_SIZE, 128, 128);
    assert_ne!(
        pixel_at(&after, TILE_SIZE, 128, 128),
        centre,
        "solarize is the identity here, so nothing above was measured"
    );

    // ...and one undo takes it all back.
    assert!(doc.undo().unwrap());
    assert_eq!(doc.composite_all(), before);
}

// ---------------------------------------------------------------------------
// 6. Transform, then undo
// ---------------------------------------------------------------------------

#[test]
fn transforming_a_layer_is_undoable_to_the_exact_document_and_pixels() {
    // The sRGB, multi-tile fixture: a transform is about geometry, and a 4x4
    // canvas inside one tile has almost none. Sliding a layer here moves content
    // across a tile boundary, and the byte comparisons below go through the
    // product's own transfer curve rather than linear's `round(v * 255)`.
    let mut l = photo_layered_document();
    let before_doc = l.doc.document.clone();
    let before = l.doc.composite_all();

    // A one-pixel slide down the y axis, in document space, of the layer whose
    // alpha varies with the row — so the picture cannot help but change.
    l.doc
        .apply(Command::TransformLayer {
            layer_id: l.clip_base,
            matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 1.0],
        })
        .unwrap();
    let moved = l.doc.composite_all();
    assert_ne!(moved, before, "the transform did not move anything");
    assert_ne!(l.doc.document, before_doc);

    assert!(l.doc.undo().unwrap());
    assert_eq!(
        l.doc.document, before_doc,
        "undo restores the document exactly, not approximately"
    );
    assert_eq!(l.doc.composite_all(), before, "...and the pixels with it");

    assert!(l.doc.redo().unwrap());
    assert_eq!(l.doc.composite_all(), moved);
}

// A last guard on the fixtures themselves: `max_channel_diff` and
// `fixture::photo_rgba8_with_alpha` are used by the interchange tests, and
// `srgb8_of_linear` by both. Keeping one assertion here means a broken helper
// fails loudly rather than silently weakening a comparison.
#[test]
fn the_comparison_helpers_do_what_they_say() {
    let a = photo_rgba8(8, 8);
    let mut b = a.clone();
    assert_eq!(max_channel_diff(&a, &b), 0);
    assert!(differing_pixels(&a, &b, 8).is_empty());
    b[4 * (3 * 8 + 2)] = a[4 * (3 * 8 + 2)].wrapping_add(9);
    assert_eq!(differing_pixels(&a, &b, 8), vec![(2, 3)]);
    assert!(max_channel_diff(&a, &b) > 0);
    assert_eq!(fixture::srgb8_of_linear(0.0), 0);
    assert_eq!(fixture::srgb8_of_linear(1.0), 255);
    assert_eq!(linear8(0.0), 0);
    assert_eq!(linear8(1.0), 255);
    assert_eq!(linear8(0.4), 102);
    let alpha = fixture::photo_rgba8_with_alpha(8, 8);
    assert_eq!(alpha[3], 0, "the ramp starts transparent");
    assert!(alpha[4 * 7 + 3] > 200, "and ends near opaque");
}
