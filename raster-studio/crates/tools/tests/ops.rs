//! What each retouching op actually does to pixels, and the two surfaces a
//! stroke can land on.
//!
//! `painting.rs` pins the *command* path — one gesture, one command, an exact
//! inverse. These pin the *pixel* path: that dodge lightens, that blur softens,
//! that smudge carries colour, that an identity warp is genuinely the identity,
//! and that a tool with no meaning on a coverage mask says so instead of
//! quietly editing the layer behind it.

use editor_core::{Command, Document, PixelKey, PixelTarget, Selection, MASK_TILE_BYTES};
use layer_model::{Layer, LayerId, LayerMask, MaskId};
use raster::{PixelRect, TileCoord, TILE_SIZE};
use tools::brush::BrushSettings;
use tools::bucket::{FillContent, FillSettings, PaintBucketTool, PatternFillTool};
use tools::edit::{MagicEraserTool, PatchTool, RedEyeTool};
use tools::gradient::{GradientRamp, GradientSettings, GradientShape, GradientTool};
use tools::shape::{ShapeKind, ShapeMode, ShapeTool};
use tools::stroke::{SpongeMode, StrokeOp, StrokeTool, ToneRange};
use tools::tiles::{MemoryTiles, TileAccess};
use tools::tool::{PaintTarget, Pattern, PointerEvent, Tool, ToolContext, ToolId};
use tools::transform::{Handle, TransformMode, TransformTool, WarpMesh};
use tools::ToolError;

mod common;
use common::{fixture, line, stroke, Fixture, BLACK, RED};

/// Run one hard-edged stroke of `op` and apply what it emitted.
fn run_op(fx: &mut Fixture, op: StrokeOp, size: f32, path: &[(f32, f32, f32)]) {
    let mut tool = StrokeTool::new(
        ToolId::Brush,
        BrushSettings {
            size,
            hardness: 1.0,
            spacing: 0.05,
            size_pressure: false,
            ..Default::default()
        },
        op,
    );
    let cmds = stroke(fx, &mut tool, path, BLACK, Selection::None);
    assert!(cmds.len() <= 1, "an op stroke must be at most one command");
    fx.commit(cmds);
}

#[test]
fn dodge_lightens_and_burn_darkens_the_range_they_are_pointed_at() {
    let base = [128, 128, 128, 255];

    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), base);
    run_op(
        &mut fx,
        StrokeOp::Dodge {
            exposure: 0.6,
            range: ToneRange::Midtones,
        },
        20.0,
        &[(32.0, 32.0, 1.0), (32.0, 32.0, 1.0)],
    );
    let dodged = fx.pixel(32, 32)[0];
    assert!(dodged > base[0], "dodge darkened: {dodged}");
    assert_eq!(fx.pixel(2, 2), base, "dodge reached outside the dab");

    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), base);
    run_op(
        &mut fx,
        StrokeOp::Burn {
            exposure: 0.6,
            range: ToneRange::Midtones,
        },
        20.0,
        &[(32.0, 32.0, 1.0), (32.0, 32.0, 1.0)],
    );
    let burned = fx.pixel(32, 32)[0];
    assert!(burned < base[0], "burn lightened: {burned}");
}

#[test]
fn the_shadows_range_barely_touches_a_highlight() {
    let bright = [240, 240, 240, 255];
    let effect = |range: ToneRange| -> i32 {
        let mut fx = fixture(64, 64);
        fx.paint_rect(PixelRect::new(0, 0, 64, 64), bright);
        run_op(
            &mut fx,
            StrokeOp::Burn {
                exposure: 0.8,
                range,
            },
            20.0,
            &[(32.0, 32.0, 1.0), (32.0, 32.0, 1.0)],
        );
        bright[0] as i32 - fx.pixel(32, 32)[0] as i32
    };
    let on_highlights = effect(ToneRange::Highlights);
    let on_shadows = effect(ToneRange::Shadows);
    assert!(on_highlights > 0, "burning a highlight did nothing");
    assert!(
        on_shadows * 4 < on_highlights,
        "the shadows range hit a highlight nearly as hard as the highlights range: \
         {on_shadows} vs {on_highlights}"
    );
}

#[test]
fn the_sponge_pulls_colour_toward_grey_without_crossing_it() {
    let vivid = [220, 40, 40, 255];
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), vivid);
    run_op(
        &mut fx,
        StrokeOp::Sponge {
            amount: 0.8,
            mode: SpongeMode::Desaturate,
        },
        20.0,
        &[(32.0, 32.0, 1.0), (32.0, 32.0, 1.0)],
    );
    let p = fx.pixel(32, 32);
    let before = vivid[0] as i32 - vivid[1] as i32;
    let after = p[0] as i32 - p[1] as i32;
    assert!(
        after < before,
        "desaturating did not narrow the channel spread: {after} vs {before}"
    );
    assert!(after > 0, "it went past grey and inverted the hue");
}

#[test]
fn blur_softens_a_hard_edge_and_sharpen_does_not() {
    let edge = |op: Option<StrokeOp>| -> i32 {
        let mut fx = fixture(64, 64);
        fx.paint_rect(PixelRect::new(0, 0, 32, 64), [0, 0, 0, 255]);
        fx.paint_rect(PixelRect::new(32, 0, 32, 64), [255, 255, 255, 255]);
        if let Some(op) = op {
            run_op(&mut fx, op, 24.0, &[(32.0, 32.0, 1.0), (32.0, 32.0, 1.0)]);
        }
        fx.pixel(33, 32)[0] as i32 - fx.pixel(30, 32)[0] as i32
    };

    let untouched = edge(None);
    assert_eq!(
        untouched, 255,
        "the fixture edge was not hard to begin with"
    );

    let blurred = edge(Some(StrokeOp::Blur { radius: 4.0 }));
    assert!(
        blurred < untouched,
        "the blur tool did not soften the edge: {blurred}"
    );

    // Sharpening an already-maximal edge cannot raise the contrast further,
    // but softening it would be a real bug, and that is what this catches.
    let sharpened = edge(Some(StrokeOp::Sharpen {
        amount: 1.0,
        radius: 1.5,
    }));
    assert!(
        sharpened >= untouched - 1,
        "the sharpen tool softened the edge: {sharpened}"
    );
}

#[test]
fn smudge_drags_colour_along_the_stroke() {
    let mut fx = fixture(64, 32);
    fx.paint_rect(PixelRect::new(0, 0, 20, 32), [230, 20, 20, 255]);
    fx.paint_rect(PixelRect::new(20, 0, 44, 32), [255, 255, 255, 255]);
    run_op(
        &mut fx,
        StrokeOp::Smudge { strength: 0.9 },
        14.0,
        &line((10.0, 16.0), (36.0, 16.0), 6),
    );
    let dragged = fx.pixel(30, 16);
    assert!(
        dragged[0] as i32 > dragged[1] as i32 + 10,
        "no red was carried into the white: {dragged:?}"
    );
    assert!(
        dragged[1] < 250,
        "the white was untouched, so nothing was smudged: {dragged:?}"
    );
    // Well past the end of the stroke it is still pure white.
    assert_eq!(fx.pixel(60, 16), [255, 255, 255, 255]);
}

/// A tile is 256 px square, so the patch a stroke loads extends well past a
/// small document on every side. Smudge is the one op that walks raw dab bounds
/// instead of the canvas-clipped stroke buffer, and it used to guard only on
/// "is this point inside the loaded patch?" — which is true for up to
/// `TILE_SIZE - 1` px of empty margin. It therefore painted outside the
/// document: invisible today, but hashed into the committed tile, and visible
/// the moment the canvas grows or the layer is translated.
#[test]
fn a_smudge_that_overhangs_the_canvas_writes_nothing_outside_it() {
    const W: i64 = 64;
    const H: i64 = 32;

    // Two strokes, each running off a different edge with a 10 px radius: one
    // off the right edge, one off the bottom.
    let mut fx = fixture(W as u32, H as u32);
    fx.paint_rect(PixelRect::new(0, 0, W as u32, H as u32), [230, 20, 20, 255]);
    run_op(
        &mut fx,
        StrokeOp::Smudge { strength: 0.9 },
        20.0,
        &line((40.0, 16.0), (62.0, 16.0), 6),
    );
    run_op(
        &mut fx,
        StrokeOp::Smudge { strength: 0.9 },
        20.0,
        &line((20.0, 30.0), (44.0, 30.0), 6),
    );

    // Inside the canvas the strokes really did run: the fixture is uniform, so
    // this only proves the dabs landed at all, which is what makes the
    // out-of-bounds probe below meaningful rather than vacuous.
    assert_ne!(fx.pixel(W - 1, 16), [0, 0, 0, 0]);
    assert_ne!(fx.pixel(30, H - 1), [0, 0, 0, 0]);

    // Everything past the right edge and past the bottom edge, out to a full
    // dab radius plus slack, must still be untouched.
    for x in W..W + 24 {
        for y in 0..H + 24 {
            assert_eq!(
                fx.pixel(x, y),
                [0, 0, 0, 0],
                "smudge wrote past the right edge at ({x}, {y})"
            );
        }
    }
    for y in H..H + 24 {
        for x in 0..W {
            assert_eq!(
                fx.pixel(x, y),
                [0, 0, 0, 0],
                "smudge wrote past the bottom edge at ({x}, {y})"
            );
        }
    }

    // Negative coordinates are the same story on the other two edges.
    let mut fx2 = fixture(W as u32, H as u32);
    fx2.paint_rect(PixelRect::new(0, 0, W as u32, H as u32), [230, 20, 20, 255]);
    run_op(
        &mut fx2,
        StrokeOp::Smudge { strength: 0.9 },
        20.0,
        &line((24.0, 2.0), (2.0, 2.0), 6),
    );
    for y in -24..0 {
        for x in -24..W {
            assert_eq!(
                fx2.pixel(x, y),
                [0, 0, 0, 0],
                "smudge wrote above the top edge at ({x}, {y})"
            );
        }
    }
    for x in -24..0 {
        for y in 0..H {
            assert_eq!(
                fx2.pixel(x, y),
                [0, 0, 0, 0],
                "smudge wrote left of the canvas at ({x}, {y})"
            );
        }
    }
}

#[test]
fn a_warp_with_the_identity_mesh_leaves_the_image_where_it_was() {
    let mut fx = fixture(128, 128);
    fx.paint_rect(PixelRect::new(30, 30, 40, 40), [10, 180, 90, 255]);
    let source = PixelRect::new(20, 20, 64, 64);

    let mut tool = TransformTool::with_mode(TransformMode::Warp);
    tool.begin(source).unwrap();
    tool.state.as_mut().unwrap().mesh = Some(WarpMesh::identity(source));

    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        tool.commit(&mut ctx).unwrap();
        ctx.drain()
    };
    fx.commit(cmds);

    for (x, y) in [(35, 35), (50, 50), (65, 65), (32, 60)] {
        let p = fx.pixel(x, y);
        assert!(
            (p[1] as i32 - 180).abs() <= 3 && p[3] == 255,
            "identity warp moved ({x},{y}): {p:?}"
        );
    }
    assert_eq!(fx.pixel(25, 25)[3], 0, "the warp smeared past the content");
}

#[test]
fn a_bent_warp_mesh_actually_moves_pixels() {
    let mut fx = fixture(128, 128);
    fx.paint_rect(PixelRect::new(20, 20, 64, 64), [10, 180, 90, 255]);
    let source = PixelRect::new(20, 20, 64, 64);

    let mut tool = TransformTool::with_mode(TransformMode::Warp);
    tool.begin(source).unwrap();
    let mut mesh = WarpMesh::identity(source);
    // Pull the two right-hand columns of control points outward.
    for row in mesh.points.iter_mut() {
        row[3].x += 30.0;
        row[2].x += 20.0;
    }
    tool.state.as_mut().unwrap().mesh = Some(mesh);

    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        tool.commit(&mut ctx).unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a warp commit is one command");
    fx.commit(cmds);
    assert_eq!(
        fx.pixel(100, 50)[3],
        255,
        "the stretched edge did not reach its new position"
    );
}

/// A document whose single layer carries a mask, plus its store.
fn masked() -> (Document, MemoryTiles, layer_model::LayerId, MaskId) {
    let mut doc = Document::new(64, 64, "masked");
    let mut layer = Layer::raster("masked");
    let mask_id = MaskId::new();
    layer.set_mask(LayerMask::new(mask_id));
    let layer_id = layer.id;
    Command::create_layer(layer).apply(&mut doc).unwrap();
    (doc, MemoryTiles::new(), layer_id, mask_id)
}

#[test]
fn painting_on_a_mask_writes_coverage_rather_than_colour() {
    let (mut doc, mut tiles, layer_id, mask_id) = masked();
    let mut brush = StrokeTool::new(
        ToolId::Brush,
        BrushSettings {
            size: 20.0,
            hardness: 1.0,
            spacing: 0.05,
            size_pressure: false,
            ..Default::default()
        },
        StrokeOp::Paint {
            color: [1.0, 1.0, 1.0, 1.0],
        },
    );

    let cmds = {
        let mut ctx =
            ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64)).with_layer(layer_id);
        ctx.active_mask = Some(mask_id);
        ctx.paint_target = PaintTarget::Mask;
        ctx.foreground = [1.0, 1.0, 1.0, 1.0];
        brush
            .on_pointer_down(&mut ctx, PointerEvent::at(32.0, 32.0))
            .unwrap();
        brush
            .on_pointer_up(&mut ctx, PointerEvent::at(32.0, 32.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1);
    match &cmds[0] {
        Command::PaintTiles { target, .. } => assert_eq!(*target, PixelTarget::Mask(layer_id)),
        other => panic!("expected PaintTiles on the mask, got {}", other.label()),
    }
    for c in cmds {
        c.apply(&mut doc).unwrap();
    }
    tiles.sync_from(&doc.pixels);

    // A mask tile is one byte per pixel, not four, and the stroke revealed
    // its centre.
    let bytes = tiles
        .tile_bytes(PixelKey::Mask(mask_id), TileCoord::new(0, 0, 0))
        .expect("no mask tile was stored");
    assert_eq!(bytes.len(), editor_core::MASK_TILE_BYTES);
    let at = |x: usize, y: usize| bytes[y * TILE_SIZE as usize + x];
    assert_eq!(at(32, 32), 255, "the mask centre was not revealed");
    assert_eq!(at(2, 2), 0, "the stroke reached the whole tile");
    assert!(
        doc.pixels.tiles(PixelKey::Layer(layer_id)).is_none(),
        "painting a mask wrote to the layer's own pixels"
    );
}

#[test]
fn a_dodge_on_a_mask_is_refused_rather_than_retargeted_at_the_layer() {
    let (_doc, mut tiles, layer_id, mask_id) = masked();
    let mut tool = StrokeTool::new(
        ToolId::Dodge,
        BrushSettings::default(),
        StrokeOp::Dodge {
            exposure: 0.5,
            range: ToneRange::Midtones,
        },
    );
    let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64)).with_layer(layer_id);
    ctx.active_mask = Some(mask_id);
    ctx.paint_target = PaintTarget::Mask;
    tool.on_pointer_down(&mut ctx, PointerEvent::at(32.0, 32.0))
        .unwrap();
    let err = tool
        .on_pointer_up(&mut ctx, PointerEvent::at(32.0, 32.0))
        .unwrap_err();
    assert!(
        matches!(err, ToolError::UnsupportedOnMask),
        "expected UnsupportedOnMask, got {err:?}"
    );
    assert!(ctx.commands().is_empty());
}

#[test]
fn colour_replacement_repaints_only_pixels_near_the_colour_it_first_touched() {
    let mut fx = fixture(64, 64);
    // A blue field with a green stripe through it.
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [30, 60, 200, 255]);
    fx.paint_rect(PixelRect::new(0, 28, 64, 8), [30, 200, 60, 255]);

    let mut tool = StrokeTool::new(
        ToolId::ColorReplacement,
        BrushSettings {
            size: 40.0,
            hardness: 1.0,
            spacing: 0.05,
            size_pressure: false,
            ..Default::default()
        },
        StrokeOp::ColorReplacement {
            color: RED,
            tolerance: 40.0 / 255.0,
        },
    );
    // The stroke starts on the blue, so blue is what gets replaced.
    let cmds = stroke(
        &mut fx,
        &mut tool,
        &line((10.0, 10.0), (54.0, 54.0), 20),
        RED,
        Selection::None,
    );
    assert_eq!(cmds.len(), 1);
    fx.commit(cmds);

    let on_blue = fx.pixel(32, 12);
    assert!(
        on_blue[0] > on_blue[2],
        "the blue under the stroke was not recoloured: {on_blue:?}"
    );
    let on_stripe = fx.pixel(32, 31);
    assert!(
        on_stripe[1] > on_stripe[0],
        "the green stripe was recoloured despite being outside the tolerance: {on_stripe:?}"
    );
    // Blue far from the stroke is untouched.
    assert_eq!(fx.pixel(60, 4), [30, 60, 200, 255]);
}

// ------------------------------------------------------------- healing ----
//
// The property that separates a healing brush from a soft smudge: the
// destination's low-frequency term is taken from *outside* the region being
// repaired. Take it from under the dab and the blemish is blurred back into its
// own repair, and anything wider than a couple of sigma survives as a ghost.

/// A light field with a dark square in the middle of it.
fn blemished(field: u8, spot: u8, spot_size: u32) -> Fixture {
    let mut fx = fixture(128, 128);
    fx.paint_rect(PixelRect::new(0, 0, 128, 128), [field, field, field, 255]);
    let half = (spot_size / 2) as i64;
    fx.paint_rect(
        PixelRect::new(64 - half, 64 - half, spot_size, spot_size),
        [spot, spot, spot, 255],
    );
    fx
}

#[test]
fn a_healing_brush_clears_a_blemish_several_times_wider_than_its_softness() {
    // 24 px of blemish against a softness of 3: eight times the radius, which
    // is precisely the case a blur-the-destination heal cannot fix.
    let mut fx = blemished(200, 10, 24);
    let mut tool = StrokeTool::new(
        ToolId::HealingBrush,
        BrushSettings {
            size: 40.0,
            hardness: 1.0,
            spacing: 0.05,
            size_pressure: false,
            ..Default::default()
        },
        StrokeOp::Healing { softness: 3.0 },
    );
    // Sample from clean pixels well away from the spot.
    tool.clone.set_anchor(glam::Vec2::new(20.0, 20.0));

    let cmds = stroke(
        &mut fx,
        &mut tool,
        &[(64.0, 64.0, 1.0), (64.0, 64.0, 1.0)],
        BLACK,
        Selection::None,
    );
    assert_eq!(cmds.len(), 1, "a heal stroke is one command");
    fx.commit(cmds);

    for (x, y) in [(64, 64), (58, 64), (64, 70), (70, 58)] {
        let px = fx.pixel(x, y);
        assert!(
            (px[0] as i32 - 200).abs() <= 8,
            "the blemish survived the heal at ({x}, {y}): {px:?}"
        );
        assert_eq!(px[3], 255, "the heal ate the coverage at ({x}, {y})");
    }
    // Outside the dab the field is untouched.
    assert_eq!(fx.pixel(10, 100), [200, 200, 200, 255]);
}

#[test]
fn spot_healing_diffuses_the_surroundings_in_rather_than_the_spot_out() {
    let mut fx = blemished(180, 10, 10);
    let mut tool = StrokeTool::new(
        ToolId::SpotHealing,
        BrushSettings {
            size: 30.0,
            hardness: 1.0,
            spacing: 0.05,
            size_pressure: false,
            ..Default::default()
        },
        StrokeOp::SpotHealing,
    );
    let cmds = stroke(
        &mut fx,
        &mut tool,
        &[(64.0, 64.0, 1.0), (64.0, 64.0, 1.0)],
        BLACK,
        Selection::None,
    );
    assert_eq!(cmds.len(), 1);
    fx.commit(cmds);

    let px = fx.pixel(64, 64);
    assert!(
        (px[0] as i32 - 180).abs() <= 6,
        "spot healing left a ghost of the spot: {px:?}"
    );
    assert_eq!(
        fx.pixel(4, 4),
        [180, 180, 180, 255],
        "the dab reached too far"
    );
}

#[test]
fn the_patch_tool_heals_the_region_it_lassoed_from_where_it_was_dragged() {
    let mut fx = blemished(200, 10, 20);
    let mut tool = PatchTool::default();
    tool.softness = 3.0;

    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        // Lasso a box around the spot...
        let outline = [
            (50.0, 50.0),
            (78.0, 50.0),
            (78.0, 78.0),
            (50.0, 78.0),
            (50.0, 50.0),
        ];
        tool.on_pointer_down(&mut ctx, PointerEvent::at(outline[0].0, outline[0].1))
            .unwrap();
        for (x, y) in &outline[1..outline.len() - 1] {
            tool.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
                .unwrap();
        }
        tool.on_pointer_up(&mut ctx, PointerEvent::at(outline[0].0, outline[0].1))
            .unwrap();
        assert!(tool.region().is_some(), "the lasso did not close");
        assert!(
            ctx.commands().is_empty(),
            "drawing the outline edited pixels"
        );

        // ...then drag it onto clean pixels 40 px up and left.
        tool.on_pointer_down(&mut ctx, PointerEvent::at(64.0, 64.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(24.0, 24.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a patch drag is one command");
    fx.commit(cmds);

    for (x, y) in [(64, 64), (56, 64), (64, 72)] {
        let px = fx.pixel(x, y);
        assert!(
            (px[0] as i32 - 200).abs() <= 8,
            "the patch left the blemish at ({x}, {y}): {px:?}"
        );
    }
    assert_eq!(fx.pixel(100, 100), [200, 200, 200, 255]);
}

// ------------------------------------------------------ the mask target ----
//
// `Command::PaintTiles` carries content *hashes* and validates no byte format
// (`CommandError::FillValueMismatch` guards `FillRegion`, a different command),
// so a tool that loads a `ColorPatch` while the paint target is the mask
// commits a 262144-byte RGBA tile into a slot the compositor reads as 65536
// bytes of coverage — and everything downstream accepts it. The assertion that
// catches that is the tile's *length*, so every test below makes it.

/// A document with one masked layer, plus everything needed to drive a tool at
/// the mask and read the coverage back.
struct MaskFixture {
    doc: Document,
    tiles: MemoryTiles,
    layer: LayerId,
    mask: MaskId,
}

impl MaskFixture {
    const CANVAS: PixelRect = PixelRect {
        x: 0,
        y: 0,
        width: 64,
        height: 64,
    };

    fn new() -> Self {
        let (doc, tiles, layer, mask) = masked();
        Self {
            doc,
            tiles,
            layer,
            mask,
        }
    }

    /// A context that paints the *mask*, with a white foreground — which on a
    /// mask means "fully revealed".
    fn ctx(&mut self) -> ToolContext<'_> {
        let (layer, mask) = (self.layer, self.mask);
        let mut ctx = ToolContext::new(&mut self.tiles, Self::CANVAS).with_layer(layer);
        ctx.active_mask = Some(mask);
        ctx.paint_target = PaintTarget::Mask;
        ctx.foreground = [1.0, 1.0, 1.0, 1.0];
        ctx
    }

    /// Put coverage into the mask the way an application would: bytes in the
    /// store, a hash in the document.
    fn seed(&mut self, bytes: Vec<u8>) {
        assert_eq!(bytes.len(), MASK_TILE_BYTES);
        let coord = TileCoord::new(0, 0, 0);
        let hash = self.tiles.put(PixelKey::Mask(self.mask), coord, bytes);
        Command::paint_tiles(
            PixelTarget::Mask(self.layer),
            vec![editor_core::TileEdit::set(coord, hash)],
        )
        .unwrap()
        .apply(&mut self.doc)
        .unwrap();
        self.tiles.sync_from(&self.doc.pixels);
    }

    /// Apply what a gesture queued, asserting it was one command aimed at the
    /// mask, and refresh the store mirror.
    fn apply(&mut self, cmds: Vec<Command>) {
        assert_eq!(cmds.len(), 1, "a mask gesture is one command");
        match &cmds[0] {
            Command::PaintTiles { target, .. } => {
                assert_eq!(*target, PixelTarget::Mask(self.layer))
            }
            other => panic!("expected PaintTiles on the mask, got {}", other.label()),
        }
        for c in cmds {
            c.apply(&mut self.doc).unwrap();
        }
        self.tiles.sync_from(&self.doc.pixels);
        assert!(
            self.doc.pixels.tiles(PixelKey::Layer(self.layer)).is_none(),
            "a mask edit wrote to the layer's own pixels"
        );
    }

    /// The mask's only tile — and the length assertion this whole section is
    /// about.
    fn tile(&self) -> &[u8] {
        let bytes = self
            .tiles
            .tile_bytes(PixelKey::Mask(self.mask), TileCoord::new(0, 0, 0))
            .expect("no mask tile was stored");
        assert_eq!(
            bytes.len(),
            MASK_TILE_BYTES,
            "the mask holds {} bytes: a colour tile was committed into a coverage slot",
            bytes.len()
        );
        bytes
    }

    fn coverage(&self, x: usize, y: usize) -> u8 {
        self.tile()[y * TILE_SIZE as usize + x]
    }
}

#[test]
fn a_gradient_dragged_across_a_mask_writes_a_coverage_ramp() {
    let mut fx = MaskFixture::new();
    let mut tool = GradientTool::new(GradientSettings {
        shape: GradientShape::Linear,
        ramp: GradientRamp::two([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
        dither: false,
        reverse: false,
        opacity: 1.0,
    });
    let cmds = {
        let mut ctx = fx.ctx();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(0.0, 0.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(63.0, 0.0))
            .unwrap();
        ctx.drain()
    };
    fx.apply(cmds);

    // Black on the left conceals, white on the right reveals, and the middle is
    // genuinely partial rather than either extreme.
    assert!(fx.coverage(1, 10) < 16, "left end: {}", fx.coverage(1, 10));
    assert!(
        fx.coverage(62, 10) > 240,
        "right end: {}",
        fx.coverage(62, 10)
    );
    let mid = fx.coverage(32, 10);
    assert!((100..160).contains(&mid), "midpoint coverage was {mid}");
}

#[test]
fn a_paint_bucket_on_a_mask_fills_only_the_region_it_was_clicked_in() {
    let mut fx = MaskFixture::new();
    // Left half half-revealed, right half hidden. The flood's tolerance has to
    // be judged against *coverage*: reading the mask as RGBA finds no tiles at
    // all, which floods the entire canvas.
    let mut seed = vec![0u8; MASK_TILE_BYTES];
    for y in 0..64usize {
        for x in 0..32usize {
            seed[y * TILE_SIZE as usize + x] = 128;
        }
    }
    fx.seed(seed);

    let mut tool = PaintBucketTool::new(
        FillSettings {
            tolerance: 0.1,
            contiguous: true,
            antialias: false,
            opacity: 1.0,
            sample_merged: false,
        },
        FillContent::Foreground,
    );
    let cmds = {
        let mut ctx = fx.ctx();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        ctx.drain()
    };
    fx.apply(cmds);

    assert_eq!(
        fx.coverage(10, 10),
        255,
        "the clicked region was not revealed"
    );
    assert_eq!(
        fx.coverage(31, 63),
        255,
        "the fill stopped short of its region"
    );
    assert_eq!(
        fx.coverage(40, 10),
        0,
        "the fill leaked past the coverage edge into the hidden half"
    );
}

#[test]
fn a_pattern_fill_on_a_mask_stencils_the_pattern_luminance() {
    let mut fx = MaskFixture::new();
    let mut tool = PatternFillTool::default();
    let cmds = {
        let mut ctx = fx.ctx();
        ctx.pattern = Some(Pattern::new(2, 1, vec![255, 255, 255, 255, 0, 0, 0, 255]).unwrap());
        tool.on_pointer_down(&mut ctx, PointerEvent::at(1.0, 1.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(1.0, 1.0))
            .unwrap();
        ctx.drain()
    };
    fx.apply(cmds);

    assert_eq!(fx.coverage(0, 0), 255, "the white column did not reveal");
    assert_eq!(fx.coverage(1, 0), 0, "the black column did not conceal");
    assert_eq!(fx.coverage(2, 7), 255, "the pattern did not tile");
}

#[test]
fn a_rasterised_shape_on_a_mask_stencils_coverage() {
    let mut fx = MaskFixture::new();
    let mut tool = ShapeTool::new(ShapeKind::Rectangle, ShapeMode::Rasterize);
    let cmds = {
        let mut ctx = fx.ctx();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(8.0, 8.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(40.0, 40.0))
            .unwrap();
        ctx.drain()
    };
    fx.apply(cmds);

    assert_eq!(
        fx.coverage(20, 20),
        255,
        "the shape did not reveal its inside"
    );
    assert_eq!(fx.coverage(50, 50), 0, "the shape reached outside its box");
}

#[test]
fn a_transform_on_a_mask_moves_coverage_and_leaves_a_coverage_tile() {
    let mut fx = MaskFixture::new();
    let mut seed = vec![0u8; MASK_TILE_BYTES];
    for y in 8..24usize {
        for x in 8..24usize {
            seed[y * TILE_SIZE as usize + x] = 255;
        }
    }
    fx.seed(seed);

    let mut tool = TransformTool::with_mode(TransformMode::Scale);
    tool.begin(PixelRect::new(8, 8, 16, 16)).unwrap();
    tool.state.as_mut().unwrap().drag(
        TransformMode::Scale,
        Handle::Inside,
        glam::Vec2::new(16.0, 16.0),
        glam::Vec2::new(40.0, 40.0),
    );
    let cmds = {
        let mut ctx = fx.ctx();
        tool.commit(&mut ctx).unwrap();
        ctx.drain()
    };
    fx.apply(cmds);

    assert_eq!(fx.coverage(40, 40), 255, "the coverage did not arrive");
    assert_eq!(fx.coverage(16, 16), 0, "the coverage did not leave");
}

#[test]
fn the_colour_only_tools_refuse_a_mask_rather_than_committing_rgba_into_it() {
    let mut fx = MaskFixture::new();

    // Red-eye: a coverage plane has no red channel to find a pupil in.
    let mut red_eye = RedEyeTool::default();
    {
        let mut ctx = fx.ctx();
        red_eye
            .on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        let err = red_eye
            .on_pointer_up(&mut ctx, PointerEvent::at(30.0, 30.0))
            .unwrap_err();
        assert!(
            matches!(err, ToolError::UnsupportedOnMask),
            "red-eye on a mask gave {err:?}"
        );
        assert!(ctx.commands().is_empty());
    }

    // Magic eraser: erasing to transparency is an alpha operation on colour.
    let mut eraser = MagicEraserTool::default();
    {
        let mut ctx = fx.ctx();
        eraser
            .on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        let err = eraser
            .on_pointer_up(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap_err();
        assert!(
            matches!(err, ToolError::UnsupportedOnMask),
            "the magic eraser on a mask gave {err:?}"
        );
        assert!(ctx.commands().is_empty());
    }

    // Patch: a frequency split needs colour and shading, and a mask has neither.
    let mut patch = PatchTool::default();
    {
        let mut ctx = fx.ctx();
        let outline = [
            (20.0, 20.0),
            (36.0, 20.0),
            (36.0, 36.0),
            (20.0, 36.0),
            (20.0, 20.0),
        ];
        patch
            .on_pointer_down(&mut ctx, PointerEvent::at(outline[0].0, outline[0].1))
            .unwrap();
        for (x, y) in &outline[1..outline.len() - 1] {
            patch
                .on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
                .unwrap();
        }
        patch
            .on_pointer_up(&mut ctx, PointerEvent::at(outline[0].0, outline[0].1))
            .unwrap();
        assert!(patch.region().is_some(), "the lasso did not close");
        patch
            .on_pointer_down(&mut ctx, PointerEvent::at(28.0, 28.0))
            .unwrap();
        let err = patch
            .on_pointer_up(&mut ctx, PointerEvent::at(12.0, 12.0))
            .unwrap_err();
        assert!(
            matches!(err, ToolError::UnsupportedOnMask),
            "the patch tool on a mask gave {err:?}"
        );
        assert!(ctx.commands().is_empty());
    }

    // Nothing reached either surface.
    assert!(fx.doc.pixels.tiles(PixelKey::Mask(fx.mask)).is_none());
    assert!(fx.doc.pixels.tiles(PixelKey::Layer(fx.layer)).is_none());
}
