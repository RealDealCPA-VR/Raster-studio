//! End-to-end tests for the path the crate exists to provide: a gesture goes
//! in, one undoable command comes out, and applying that command's inverse puts
//! the document back byte for byte.
//!
//! These run the real `editor_core::Document`, the real command apply/inverse,
//! and a real content-addressed tile store, because the property under test is
//! precisely that those three agree with each other.

use editor_core::{Command, PixelKey, PixelStore, PixelTarget, Selection};
use glam::{IVec2, Vec2};
use raster::PixelRect;
use selection::BooleanOp;
use tools::brush::BrushSettings;
use tools::bucket::{FillContent, FillSettings, PaintBucketTool, PatternFillTool};
use tools::edit::{
    Alignment, CropTool, EyedropperTool, MagicEraserTool, MoveTool, RedEyeTool, SliceTool,
};
use tools::gradient::{
    stop_from_srgb8, GradientRamp, GradientSettings, GradientShape, GradientTool,
};
use tools::registry;
use tools::select::{LassoKind, LassoTool, MarqueeShape, MarqueeTool, WandKind, WandTool};
use tools::shape::{ShapeKind, ShapeMode, ShapeTool};
use tools::stroke::{StrokeOp, StrokeTool};
use tools::tool::{Modifiers, Pattern, PointerEvent, Tool, ToolContext, ToolId, ToolRequest};
use tools::transform::{Handle, TransformMode, TransformTool};

mod common;
use common::{fixture, line, stroke, Fixture, BLACK, RED};

// ---------------------------------------------------------------------------

#[test]
fn a_stroke_of_many_dabs_emits_exactly_one_command_covering_every_tile() {
    let mut fx = fixture(1024, 256);
    let mut brush = StrokeTool::new(
        ToolId::Brush,
        BrushSettings {
            size: 20.0,
            spacing: 0.05, // a dab every pixel
            hardness: 1.0,
            ..Default::default()
        },
        StrokeOp::Paint { color: BLACK },
    );
    let path = line((10.0, 128.0), (1000.0, 128.0), 40);
    let cmds = stroke(&mut fx, &mut brush, &path, RED, Selection::None);

    assert_eq!(cmds.len(), 1, "a stroke must be one undoable command");
    let Command::PaintTiles { target, delta } = &cmds[0] else {
        panic!("expected PaintTiles, got {:?}", cmds[0].label());
    };
    assert_eq!(*target, PixelTarget::Layer(fx.layer));
    // 990 px of stroke at 256 px per tile crosses four tile columns.
    assert_eq!(
        delta.len(),
        4,
        "one command should carry every touched tile, got {}",
        delta.len()
    );
}

#[test]
fn the_stroke_commands_inverse_restores_the_exact_prior_tiles() {
    let mut fx = fixture(512, 128);
    // Pre-existing content, so undo has something non-trivial to restore.
    fx.paint_rect(PixelRect::new(0, 0, 512, 128), [20, 40, 60, 255]);
    let before_store = fx.doc.pixels.clone();
    let before_pixels: Vec<[u8; 4]> = (0..512).step_by(7).map(|x| fx.pixel(x, 64)).collect();

    let mut brush = StrokeTool::new(
        ToolId::Brush,
        BrushSettings {
            size: 24.0,
            spacing: 0.1,
            ..Default::default()
        },
        StrokeOp::Paint { color: BLACK },
    );
    let path = line((20.0, 64.0), (480.0, 64.0), 30);
    let cmds = stroke(&mut fx, &mut brush, &path, RED, Selection::None);
    assert_eq!(cmds.len(), 1);

    let inverses = fx.commit(cmds);
    assert_ne!(fx.doc.pixels, before_store, "the stroke changed nothing");
    assert_eq!(fx.pixel(250, 64)[0], 255, "the stroke did not paint red");

    // Undo.
    fx.commit(inverses);
    assert_eq!(
        fx.doc.pixels, before_store,
        "undo did not restore the tile references exactly"
    );
    let after_pixels: Vec<[u8; 4]> = (0..512).step_by(7).map(|x| fx.pixel(x, 64)).collect();
    assert_eq!(
        after_pixels, before_pixels,
        "undo did not restore the exact prior pixels"
    );
}

#[test]
fn a_stroke_over_empty_tiles_undoes_back_to_an_empty_store() {
    let mut fx = fixture(256, 256);
    assert_eq!(fx.doc.pixels, PixelStore::default());
    let mut brush = StrokeTool::new(
        ToolId::Brush,
        BrushSettings::default(),
        StrokeOp::Paint { color: BLACK },
    );
    let cmds = stroke(
        &mut fx,
        &mut brush,
        &line((40.0, 40.0), (200.0, 200.0), 20),
        RED,
        Selection::None,
    );
    let inverses = fx.commit(cmds);
    assert_eq!(fx.doc.pixels.tile_count(), 1);
    fx.commit(inverses);
    assert_eq!(
        fx.doc.pixels,
        PixelStore::default(),
        "undoing the first stroke must leave no tile behind at all"
    );
}

#[test]
fn overlapping_dabs_in_one_stroke_do_not_double_darken() {
    let mut fx = fixture(128, 128);
    // Half-opacity black over white. However many times the stroke crosses
    // itself, the result must be the *same* half-opacity grey.
    fx.paint_rect(PixelRect::new(0, 0, 128, 128), [255, 255, 255, 255]);

    let settings = BrushSettings {
        size: 30.0,
        hardness: 1.0,
        spacing: 0.05,
        opacity: 0.5,
        flow: 1.0,
        size_pressure: false,
        ..Default::default()
    };

    // One short stroke, then a scrubbing one over the same spot.
    let mut single = StrokeTool::new(ToolId::Brush, settings, StrokeOp::Paint { color: BLACK });
    let cmds = stroke(
        &mut fx,
        &mut single,
        &[(64.0, 64.0, 1.0), (64.0, 64.0, 1.0)],
        BLACK,
        Selection::None,
    );
    let undo = fx.commit(cmds);
    let one_dab = fx.pixel(64, 64);
    fx.commit(undo);
    assert_eq!(fx.pixel(64, 64), [255, 255, 255, 255], "undo failed");

    let mut scrub = StrokeTool::new(ToolId::Brush, settings, StrokeOp::Paint { color: BLACK });
    let mut path = Vec::new();
    for _ in 0..6 {
        path.extend(line((50.0, 64.0), (78.0, 64.0), 14));
        path.extend(line((78.0, 64.0), (50.0, 64.0), 14));
    }
    let cmds = stroke(&mut fx, &mut scrub, &path, BLACK, Selection::None);
    fx.commit(cmds);
    let scrubbed = fx.pixel(64, 64);

    assert_eq!(
        scrubbed, one_dab,
        "scrubbing over one spot darkened it: {scrubbed:?} vs {one_dab:?}"
    );
    // And that value really is the half-opacity result, not an accident.
    assert!(
        (scrubbed[0] as i32 - 188).abs() <= 2,
        "50% black over white should be ~188 sRGB, got {}",
        scrubbed[0]
    );
}

#[test]
fn pressure_narrows_the_painted_band() {
    let mut fx = fixture(256, 128);
    let settings = BrushSettings {
        size: 40.0,
        hardness: 1.0,
        spacing: 0.05,
        size_pressure: true,
        min_size_ratio: 0.1,
        ..Default::default()
    };

    let width_at = |fx: &mut Fixture, pressure: f32| -> i64 {
        let mut brush = StrokeTool::new(ToolId::Brush, settings, StrokeOp::Paint { color: BLACK });
        let path: Vec<(f32, f32, f32)> = line((40.0, 64.0), (200.0, 64.0), 40)
            .into_iter()
            .map(|(x, y, _)| (x, y, pressure))
            .collect();
        let cmds = stroke(fx, &mut brush, &path, BLACK, Selection::None);
        let undo = fx.commit(cmds);
        let mut hits = 0;
        for y in 0..128 {
            if fx.pixel(120, y)[3] > 0 {
                hits += 1;
            }
        }
        fx.commit(undo);
        hits
    };

    let full = width_at(&mut fx, 1.0);
    let light = width_at(&mut fx, 0.2);
    assert!((39..=42).contains(&full), "full-pressure band was {full}px");
    assert!(
        light < full / 2,
        "light pressure should be much narrower: {light} vs {full}"
    );
    assert!(light > 0, "light pressure painted nothing");
}

#[test]
fn painting_is_clipped_by_the_selection() {
    let mut fx = fixture(128, 128);
    let sel = Selection::Rect {
        min: IVec2::new(0, 0),
        max: IVec2::new(64, 128),
    };
    let mut brush = StrokeTool::new(
        ToolId::Brush,
        BrushSettings {
            size: 20.0,
            hardness: 1.0,
            spacing: 0.05,
            size_pressure: false,
            ..Default::default()
        },
        StrokeOp::Paint { color: BLACK },
    );
    let cmds = stroke(
        &mut fx,
        &mut brush,
        &line((10.0, 64.0), (110.0, 64.0), 25),
        RED,
        sel,
    );
    fx.commit(cmds);
    assert_eq!(
        fx.pixel(30, 64)[0],
        255,
        "inside the selection stayed empty"
    );
    assert_eq!(
        fx.pixel(100, 64),
        [0, 0, 0, 0],
        "paint escaped the selection"
    );
}

#[test]
fn an_eraser_removes_coverage_and_undo_brings_it_back() {
    let mut fx = fixture(128, 128);
    fx.paint_rect(PixelRect::new(0, 0, 128, 128), [10, 200, 30, 255]);
    let before = fx.doc.pixels.clone();

    let mut eraser = StrokeTool::new(
        ToolId::Eraser,
        BrushSettings {
            size: 24.0,
            hardness: 1.0,
            spacing: 0.05,
            size_pressure: false,
            ..Default::default()
        },
        StrokeOp::Erase,
    );
    let cmds = stroke(
        &mut fx,
        &mut eraser,
        &line((20.0, 64.0), (110.0, 64.0), 25),
        BLACK,
        Selection::None,
    );
    assert_eq!(cmds.len(), 1);
    let undo = fx.commit(cmds);
    assert_eq!(fx.pixel(64, 64)[3], 0, "the eraser left coverage behind");
    assert_eq!(fx.pixel(64, 10)[3], 255, "the eraser reached too far");
    fx.commit(undo);
    assert_eq!(fx.doc.pixels, before);
    assert_eq!(fx.pixel(64, 64), [10, 200, 30, 255]);
}

#[test]
fn a_clone_stamp_copies_from_its_source_offset() {
    let mut fx = fixture(256, 128);
    // A distinctive block on the left, empty on the right.
    fx.paint_rect(PixelRect::new(10, 40, 40, 40), [240, 30, 120, 255]);

    let mut clone = StrokeTool::new(
        ToolId::CloneStamp,
        BrushSettings {
            size: 30.0,
            hardness: 1.0,
            spacing: 0.05,
            size_pressure: false,
            ..Default::default()
        },
        StrokeOp::CloneStamp,
    );
    clone.clone.set_anchor(Vec2::new(30.0, 60.0));

    let layer = fx.layer;
    let canvas = fx.canvas();
    {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        clone
            .on_pointer_down(&mut ctx, PointerEvent::at(160.0, 60.0))
            .unwrap();
        clone
            .on_pointer_move(&mut ctx, PointerEvent::at(165.0, 60.0))
            .unwrap();
        clone
            .on_pointer_up(&mut ctx, PointerEvent::at(170.0, 60.0))
            .unwrap();
        let cmds = ctx.drain();
        assert_eq!(cmds.len(), 1, "a clone stroke is one command");
        for c in cmds {
            c.apply(&mut fx.doc).unwrap();
        }
    }
    fx.tiles.sync_from(&fx.doc.pixels);
    assert_eq!(
        fx.pixel(162, 60),
        [240, 30, 120, 255],
        "the clone did not copy the source colour"
    );
    // The source itself is untouched.
    assert_eq!(fx.pixel(30, 60), [240, 30, 120, 255]);
}

#[test]
fn flood_fill_respects_tolerance_and_the_contiguous_flag() {
    // Three bands: dark, light, dark. The two dark bands are the same colour
    // but are not connected to each other.
    let build = || {
        let mut fx = fixture(48, 8);
        fx.paint_rect(PixelRect::new(0, 0, 12, 8), [60, 60, 60, 255]);
        fx.paint_rect(PixelRect::new(12, 0, 24, 8), [200, 200, 200, 255]);
        fx.paint_rect(PixelRect::new(36, 0, 12, 8), [60, 60, 60, 255]);
        fx
    };

    let run = |fx: &mut Fixture, settings: FillSettings| {
        let mut bucket = PaintBucketTool::new(settings, FillContent::Foreground);
        let layer = fx.layer;
        let canvas = fx.canvas();
        let cmds = {
            let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
            ctx.foreground = RED;
            bucket
                .on_pointer_down(&mut ctx, PointerEvent::at(2.0, 4.0))
                .unwrap();
            bucket
                .on_pointer_up(&mut ctx, PointerEvent::at(2.0, 4.0))
                .unwrap();
            ctx.drain()
        };
        assert_eq!(cmds.len(), 1, "a bucket click is one command");
        fx.commit(cmds);
    };

    // Tight tolerance, contiguous: only the band that was clicked.
    let mut fx = build();
    run(
        &mut fx,
        FillSettings {
            tolerance: 10.0 / 255.0,
            contiguous: true,
            antialias: false,
            ..Default::default()
        },
    );
    assert_eq!(fx.pixel(2, 4)[0], 255, "the clicked band was not filled");
    assert_eq!(fx.pixel(24, 4)[0], 200, "the light band was filled");
    assert_eq!(
        fx.pixel(40, 4)[0],
        60,
        "a contiguous fill jumped to the far band"
    );

    // Same tolerance, global: the disconnected band of the same colour too.
    let mut fx = build();
    run(
        &mut fx,
        FillSettings {
            tolerance: 10.0 / 255.0,
            contiguous: false,
            antialias: false,
            ..Default::default()
        },
    );
    assert_eq!(fx.pixel(2, 4)[0], 255);
    assert_eq!(fx.pixel(40, 4)[0], 255, "a global fill missed the far band");
    assert_eq!(fx.pixel(24, 4)[0], 200, "the light band was still too far");

    // Wide tolerance: everything, even across the light band.
    let mut fx = build();
    run(
        &mut fx,
        FillSettings {
            tolerance: 1.0,
            contiguous: true,
            antialias: false,
            ..Default::default()
        },
    );
    assert_eq!(
        fx.pixel(24, 4)[0],
        255,
        "a wide tolerance stopped too early"
    );
    assert_eq!(fx.pixel(40, 4)[0], 255);
}

#[test]
fn a_gradient_produces_a_monotone_ramp() {
    let mut fx = fixture(64, 8);
    let mut tool = GradientTool::new(GradientSettings {
        shape: GradientShape::Linear,
        ramp: GradientRamp::two([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
        dither: false,
        reverse: false,
        opacity: 1.0,
    });
    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        ctx.ramp = GradientRamp::two([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(0.0, 4.0))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(30.0, 4.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(64.0, 4.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a gradient is one command");
    fx.commit(cmds);

    let row: Vec<u8> = (0..64).map(|x| fx.pixel(x, 4)[0]).collect();
    for w in row.windows(2) {
        assert!(w[1] >= w[0], "the ramp fell: {row:?}");
    }
    // The ramp is sampled at pixel centres, so the first and last pixel sit
    // half a step inside the endpoints rather than exactly on them.
    assert!(row[0] < 30, "the ramp started at {}", row[0]);
    assert!(row[63] > 240, "the ramp ended at {}", row[63]);
    assert!(row[32] > 100 && row[32] < 200, "midpoint was {}", row[32]);
    // Every pixel is opaque: the gradient covered its whole rect.
    assert!((0..64).all(|x| fx.pixel(x, 4)[3] == 255));
}

#[test]
fn dithering_breaks_up_the_bands_a_shallow_gradient_would_otherwise_show() {
    // A ramp spanning only four 8-bit levels across 256 pixels: without
    // dithering that is four hard steps, which is exactly the artefact.
    let transitions = |dither: bool| -> usize {
        let mut fx = fixture(256, 16);
        let mut tool = GradientTool::new(GradientSettings {
            shape: GradientShape::Linear,
            ramp: GradientRamp::two(stop_from_srgb8([0, 0, 0]), stop_from_srgb8([4, 4, 4])),
            dither,
            reverse: false,
            opacity: 1.0,
        });
        let layer = fx.layer;
        let canvas = fx.canvas();
        let cmds = {
            let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
            // The ramp is the *context's* now: the application threads the
            // dialog's confirmed ramp in per gesture, so the test does too.
            ctx.ramp = GradientRamp::two(stop_from_srgb8([0, 0, 0]), stop_from_srgb8([4, 4, 4]));
            tool.on_pointer_down(&mut ctx, PointerEvent::at(0.0, 8.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(256.0, 8.0))
                .unwrap();
            ctx.drain()
        };
        fx.commit(cmds);
        let row: Vec<u8> = (0..256).map(|x| fx.pixel(x, 8)[0]).collect();
        row.windows(2).filter(|w| w[0] != w[1]).count()
    };

    let plain = transitions(false);
    let dithered = transitions(true);
    assert!(
        plain <= 6,
        "the undithered ramp should be a handful of hard steps, got {plain}"
    );
    assert!(
        dithered > plain * 4,
        "dithering did not break the bands: {dithered} transitions vs {plain}"
    );
}

#[test]
fn a_transform_with_a_singular_matrix_returns_not_invertible_instead_of_writing_nan() {
    let mut fx = fixture(128, 128);
    fx.paint_rect(PixelRect::new(16, 16, 96, 96), [12, 34, 56, 255]);
    let before = fx.doc.pixels.clone();

    let mut tool = TransformTool::default();
    tool.mode = TransformMode::Distort;
    tool.begin(PixelRect::new(16, 16, 96, 96)).unwrap();
    // Drag every corner onto the same point: the quad has no area left.
    let state = tool.state.as_mut().unwrap();
    state.corners = [Vec2::new(50.0, 50.0); 4];

    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
    let err = tool.commit(&mut ctx).unwrap_err();
    assert!(
        err.is_not_invertible(),
        "expected NotInvertible, got {err:?}"
    );
    assert!(
        ctx.commands().is_empty(),
        "a refused transform still emitted"
    );
    drop(ctx);
    assert_eq!(fx.doc.pixels, before, "a refused transform changed pixels");
    // And nothing NaN reached the store.
    for x in (16..112).step_by(7) {
        assert_eq!(fx.pixel(x, 64), [12, 34, 56, 255]);
    }
}

#[test]
fn a_transform_moves_pixels_and_emits_one_command() {
    let mut fx = fixture(256, 256);
    fx.paint_rect(PixelRect::new(20, 20, 40, 40), [200, 10, 10, 255]);

    let mut tool = TransformTool::default();
    tool.mode = TransformMode::Scale;
    tool.begin(PixelRect::new(20, 20, 40, 40)).unwrap();
    // Slide the whole box 100 px right and 60 px down.
    {
        let state = tool.state.as_mut().unwrap();
        state.drag(
            TransformMode::Scale,
            Handle::Inside,
            Vec2::new(40.0, 40.0),
            Vec2::new(140.0, 100.0),
        );
    }

    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        tool.commit(&mut ctx).unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a transform commit is one command");
    let undo = fx.commit(cmds);

    assert_eq!(
        fx.pixel(140, 100),
        [200, 10, 10, 255],
        "content did not arrive"
    );
    assert_eq!(fx.pixel(40, 40), [0, 0, 0, 0], "content did not leave");
    for x in 100..200 {
        let p = fx.pixel(x, 100);
        assert!(p[0] == 0 || p[0] == 200, "resampling produced {p:?}");
    }
    fx.commit(undo);
    assert_eq!(fx.pixel(40, 40), [200, 10, 10, 255], "undo did not restore");
}

#[test]
fn a_marquee_emits_a_selection_edit_carrying_the_modifier_op() {
    let mut fx = fixture(128, 128);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
    let mut tool = MarqueeTool::new(MarqueeShape::Rect);

    tool.on_pointer_down(
        &mut ctx,
        PointerEvent::at(10.0, 10.0).with_modifiers(Modifiers::shift()),
    )
    .unwrap();
    tool.on_pointer_move(&mut ctx, PointerEvent::at(40.0, 30.0))
        .unwrap();
    tool.on_pointer_up(
        &mut ctx,
        PointerEvent::at(40.0, 30.0).with_modifiers(Modifiers::shift()),
    )
    .unwrap();

    assert!(ctx.commands().is_empty(), "a selection is not a pixel edit");
    let edits = ctx.drain_selection();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].op, BooleanOp::Add, "shift must mean add");

    // Folding it onto an existing selection keeps both regions. Shift also
    // constrained the drag to a square, so the box is 10..40 in *both* axes
    // even though the pointer only travelled 20px vertically.
    let base = Selection::Rect {
        min: IVec2::new(90, 90),
        max: IVec2::new(120, 120),
    };
    let canvas_rect = selection::Rect::from_xywh(0, 0, 128, 128);
    let folded = edits[0].apply(canvas_rect, &base).unwrap();
    assert!(
        folded.coverage_at(IVec2::new(20, 35)) > 0.5,
        "new box missing"
    );
    assert!(
        folded.coverage_at(IVec2::new(100, 100)) > 0.5,
        "old box lost"
    );
    assert_eq!(folded.coverage_at(IVec2::new(70, 70)), 0.0);

    // Without a modifier the gesture replaces instead.
    let mut tool = MarqueeTool::new(MarqueeShape::Rect);
    tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
        .unwrap();
    tool.on_pointer_up(&mut ctx, PointerEvent::at(40.0, 30.0))
        .unwrap();
    let edits = ctx.drain_selection();
    assert_eq!(edits[0].op, BooleanOp::Replace);
    let folded = edits[0].apply(canvas_rect, &base).unwrap();
    assert!(folded.coverage_at(IVec2::new(20, 20)) > 0.5);
    assert_eq!(
        folded.coverage_at(IVec2::new(100, 100)),
        0.0,
        "replace kept the old selection"
    );
}

#[test]
fn a_shape_makes_a_layer_in_vector_mode_and_paints_in_raster_mode() {
    let mut fx = fixture(128, 128);
    let layer = fx.layer;
    let canvas = fx.canvas();

    // Vector mode: a new shape layer, no pixels touched.
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        let mut tool = ShapeTool::new(ShapeKind::Ellipse, ShapeMode::VectorLayer);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(20.0, 20.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(100.0, 80.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1);
    assert!(matches!(cmds[0], Command::CreateLayer { .. }));
    fx.commit(cmds);
    assert_eq!(fx.pixel(60, 50), [0, 0, 0, 0], "vector mode painted pixels");

    // Rasterise mode: pixels, no new layer.
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        ctx.foreground = RED;
        let mut tool = ShapeTool::new(ShapeKind::Ellipse, ShapeMode::Rasterize);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(20.0, 20.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(100.0, 80.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1);
    assert!(matches!(cmds[0], Command::PaintTiles { .. }));
    fx.commit(cmds);
    assert_eq!(fx.pixel(60, 50), [255, 0, 0, 255]);
    assert_eq!(fx.pixel(5, 5), [0, 0, 0, 0], "the ellipse leaked");
}

#[test]
fn the_eyedropper_averages_over_its_sample_radius() {
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [0, 0, 0, 255]);
    fx.paint_rect(PixelRect::new(30, 30, 4, 4), [255, 255, 255, 255]);

    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
    ctx.sample_from = Some(PixelKey::Layer(layer));

    let mut single = EyedropperTool::new(0, false);
    single
        .on_pointer_down(&mut ctx, PointerEvent::at(31.0, 31.0))
        .unwrap();
    let point = ctx.picked().unwrap();
    assert!(point[0] > 0.99, "a single-pixel read should be white");

    let mut wide = EyedropperTool::new(8, false);
    wide.on_pointer_down(&mut ctx, PointerEvent::at(31.0, 31.0))
        .unwrap();
    let avg = ctx.picked().unwrap();
    assert!(
        avg[0] < point[0] && avg[0] > 0.0,
        "a 17x17 average of mostly black should sit between: {avg:?}"
    );
    // 16 white pixels out of 289 in linear light.
    assert!(
        (avg[0] - 16.0 / 289.0).abs() < 0.01,
        "average was {} not {}",
        avg[0],
        16.0 / 289.0
    );
}

#[test]
fn the_move_tool_emits_one_transform_and_a_click_that_moved_nothing_emits_none() {
    let mut fx = fixture(128, 128);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
    let mut tool = MoveTool::default();

    tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
        .unwrap();
    tool.on_pointer_up(&mut ctx, PointerEvent::at(10.0, 10.0))
        .unwrap();
    assert!(
        ctx.commands().is_empty(),
        "a click that moved nothing must not enter history"
    );

    tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
        .unwrap();
    tool.on_pointer_move(&mut ctx, PointerEvent::at(40.0, 25.0))
        .unwrap();
    tool.on_pointer_up(&mut ctx, PointerEvent::at(40.0, 25.0))
        .unwrap();
    let cmds = ctx.drain();
    assert_eq!(cmds.len(), 1);
    match &cmds[0] {
        Command::TransformLayer { layer_id, matrix } => {
            assert_eq!(*layer_id, layer);
            assert_eq!(&matrix[4..], &[30.0, 15.0]);
        }
        other => panic!("expected TransformLayer, got {}", other.label()),
    }
}

#[test]
fn every_registry_tool_survives_a_full_gesture_on_a_real_document() {
    for id in ToolId::ALL {
        let mut fx = fixture(64, 64);
        fx.paint_rect(PixelRect::new(0, 0, 64, 64), [128, 128, 128, 255]);
        let layer = fx.layer;
        let canvas = fx.canvas();
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        ctx.pattern = Some(tools::tool::Pattern::solid([9, 9, 9, 255]));
        ctx.sample_from = Some(PixelKey::Layer(layer));
        let mut tool = registry::make(*id);

        // A tool may legitimately refuse (a clone with no source, a zero-length
        // gradient). It may not panic, and whatever it emits must apply.
        let _ = tool.on_pointer_down(&mut ctx, PointerEvent::at(12.0, 12.0));
        let _ = tool.on_pointer_move(&mut ctx, PointerEvent::at(30.0, 26.0));
        let _ = tool.on_pointer_up(&mut ctx, PointerEvent::at(48.0, 40.0));
        let cmds = ctx.drain();
        drop(ctx);
        for c in cmds {
            let inv = c
                .apply(&mut fx.doc)
                .unwrap_or_else(|e| panic!("{id:?} emitted a command the document refused: {e}"));
            inv.apply(&mut fx.doc)
                .unwrap_or_else(|e| panic!("{id:?}'s inverse was refused: {e}"));
        }
        tool.cancel(&mut ToolContext::new(&mut fx.tiles, canvas));
        assert!(!tool.is_active(), "{id:?} stayed active after cancel");
    }
}

/// The pencil's whole promise is "one pixel per sample, no anti-aliasing", and
/// a one-pixel pencil has a radius of 0.5 — so every pixel centre it can reach
/// is *exactly* 0.5 away when the stroke runs along integer document
/// coordinates, which is the ordinary mouse-driven case. An inclusive rim test
/// accepted both of the two rows straddling such a stroke and drew a 2 px line;
/// nudge the same drag half a pixel and it drew 1 px. A tool's line width may
/// not depend on the sub-pixel phase of the gesture.
#[test]
fn the_pencil_draws_a_one_pixel_line_whatever_the_sub_pixel_phase() {
    /// Drag the registry's pencil in a straight horizontal line and report
    /// every pixel it actually marked.
    fn drag(x0: f32, x1: f32, y: f32) -> Vec<(i64, i64)> {
        let mut fx = fixture(64, 64);
        let mut tool = registry::make(ToolId::Pencil);
        let path = line((x0, y), (x1, y), 20);
        let cmds = stroke(&mut fx, &mut *tool, &path, BLACK, Selection::None);
        assert_eq!(cmds.len(), 1, "a pencil stroke is one command");
        fx.commit(cmds);
        let mut marked = Vec::new();
        for py in 0..64 {
            for px in 0..64 {
                if fx.pixel(px, py)[3] != 0 {
                    marked.push((px, py));
                }
            }
        }
        marked
    }

    // Same 20 px run, drawn at two different sub-pixel phases *across* the
    // stroke. The cross-axis phase is the one that decided the line width: at
    // y = 10.0 the stroke sits exactly on the boundary between rows 9 and 10,
    // which is where the inclusive rim test claimed both of them.
    //
    // The run spans x ∈ [10, 30), which is 20 whole pixel cells. (Shifting the
    // run *along* its own axis genuinely changes that count — a half-integer
    // start clips a 21st cell — which is why the two cases below share their x
    // phase and differ only in y.)
    let on_boundary = drag(10.0, 30.0, 10.0);
    let in_center = drag(10.0, 30.0, 10.5);

    for (name, marked) in [
        ("on the row boundary", &on_boundary),
        ("mid-row", &in_center),
    ] {
        assert_eq!(
            marked.len(),
            20,
            "{name}: pencil painted {} pixels, not 20: {marked:?}",
            marked.len()
        );
        let row = marked[0].1;
        assert!(
            marked.iter().all(|(_, y)| *y == row),
            "{name}: pencil painted more than one row: {marked:?}"
        );
        let xs: Vec<i64> = marked.iter().map(|(x, _)| *x).collect();
        assert_eq!(xs.first(), Some(&10), "{name}: line started late: {xs:?}");
        assert_eq!(xs.last(), Some(&29), "{name}: line stopped early: {xs:?}");
        assert!(
            xs.windows(2).all(|w| w[1] == w[0] + 1),
            "{name}: the line has a gap: {xs:?}"
        );
    }
    assert_eq!(
        on_boundary.len(),
        in_center.len(),
        "the pencil's width depends on the stroke's sub-pixel phase"
    );

    // Shifting along the stroke's own axis moves the line by at most one cell
    // at each end, and never widens it.
    let along = drag(10.5, 30.5, 10.5);
    assert!(
        (20..=21).contains(&along.len()),
        "a half-pixel shift along the stroke changed the count to {}: {along:?}",
        along.len()
    );
    let row = along[0].1;
    assert!(
        along.iter().all(|(_, y)| *y == row),
        "a half-pixel shift along the stroke widened the line: {along:?}"
    );

    // And every marked pixel is fully opaque: the pencil never anti-aliases.
    let mut fx = fixture(64, 64);
    let mut tool = registry::make(ToolId::Pencil);
    let cmds = stroke(
        &mut fx,
        &mut *tool,
        &line((10.0, 10.0), (30.0, 10.0), 20),
        BLACK,
        Selection::None,
    );
    fx.commit(cmds);
    for x in 10..30 {
        assert_eq!(
            fx.pixel(x, 10)[3],
            255,
            "the pencil left a partial pixel at x={x}"
        );
    }
}

// ---------------------------------------------------------------------------
// The rest of the palette: one behavioural assertion per tool, so a regression
// in any of them is a red test rather than a silently different picture.
// ---------------------------------------------------------------------------

#[test]
fn a_crop_with_an_aspect_lock_reports_a_ratio_correct_rect_and_its_straighten() {
    let mut fx = fixture(256, 256);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);

    let mut tool = CropTool::default();
    tool.aspect = Some(2.0);
    tool.straighten = 0.3;
    tool.delete_cropped = true;

    tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
        .unwrap();
    tool.on_pointer_move(&mut ctx, PointerEvent::at(110.0, 90.0))
        .unwrap();
    tool.on_pointer_up(&mut ctx, PointerEvent::at(110.0, 90.0))
        .unwrap();

    // Releasing only sets the box. The crop itself waits for Enter.
    assert!(ctx.requests().is_empty(), "the release cropped on its own");
    let boxed = tool.box_rect.expect("the release left no box");
    assert_eq!(
        (boxed.width, boxed.height),
        (160, 80),
        "the 100x80 drag was not widened to the 2:1 lock: {boxed:?}"
    );
    assert_eq!((boxed.x, boxed.y), (10, 10));

    tool.commit(&mut ctx).unwrap();
    let reqs = ctx.drain_requests();
    assert_eq!(reqs.len(), 1);
    match &reqs[0] {
        ToolRequest::Crop(c) => {
            assert_eq!(c.rect, boxed);
            assert!(
                (c.rect.width as f32 / c.rect.height as f32 - 2.0).abs() < 1e-6,
                "the reported rect is not 2:1: {:?}",
                c.rect
            );
            assert_eq!(c.straighten, 0.3, "the straighten angle was dropped");
            assert!(c.delete_cropped);
        }
        other => panic!("expected a crop request, got {other:?}"),
    }
    // Committing with nothing pending is a refusal, not a second crop.
    assert!(tool.commit(&mut ctx).is_err());
    assert!(ctx.requests().is_empty());

    // An unconstrained drag keeps the shape the hand made.
    let mut plain = CropTool::default();
    plain
        .on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
        .unwrap();
    plain
        .on_pointer_up(&mut ctx, PointerEvent::at(110.0, 90.0))
        .unwrap();
    let r = plain.box_rect.unwrap();
    assert_eq!((r.width, r.height), (100, 80));
}

#[test]
fn three_slice_drags_publish_one_request_carrying_all_three() {
    let mut fx = fixture(256, 256);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
    let mut tool = SliceTool::default();

    let drags = [
        ((0.0, 0.0), (10.0, 10.0)),
        ((20.0, 20.0), (40.0, 44.0)),
        ((50.0, 50.0), (80.0, 60.0)),
    ];
    for (i, (from, to)) in drags.iter().enumerate() {
        tool.on_pointer_down(&mut ctx, PointerEvent::at(from.0, from.1))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(to.0, to.1))
            .unwrap();
        assert_eq!(tool.slices().len(), i + 1);
        assert!(
            ctx.requests().is_empty(),
            "release {} published a slice set on its own; three drags would \
             leave three overlapping sets in the outbox",
            i + 1
        );
    }

    tool.commit(&mut ctx);
    let reqs = ctx.drain_requests();
    assert_eq!(reqs.len(), 1, "commit must publish exactly one set");
    match &reqs[0] {
        ToolRequest::Slices(s) => {
            assert_eq!(s.len(), 3, "the set lost or duplicated slices: {s:?}");
            assert_eq!(s[0].rect, PixelRect::new(0, 0, 10, 10));
            assert_eq!(s[1].rect, PixelRect::new(20, 20, 20, 24));
            assert_eq!(s[2].rect, PixelRect::new(50, 50, 30, 10));
            assert_eq!(s[0].name, "slice_01");
            assert_eq!(s[2].name, "slice_03");
        }
        other => panic!("expected slices, got {other:?}"),
    }
    // The set is consumed, so a second commit does not re-export it.
    tool.commit(&mut ctx);
    assert!(
        ctx.requests().is_empty(),
        "commit republished the same slices"
    );
    assert!(tool.slices().is_empty());
}

#[test]
fn red_eye_neutralises_the_pupil_and_leaves_skin_and_grey_alone() {
    // Skin is [0.6, 0.45, 0.4] in linear light — redder than its neighbours but
    // nowhere near the ratio a flash pupil has.
    const SKIN: [u8; 4] = [203, 193, 170, 255];
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), SKIN);
    fx.paint_rect(PixelRect::new(28, 28, 8, 8), [200, 20, 20, 255]);
    fx.paint_rect(PixelRect::new(42, 42, 6, 6), [128, 128, 128, 255]);

    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        let mut tool = RedEyeTool::default();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(24.0, 24.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(52.0, 52.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a red-eye box is one command");
    fx.commit(cmds);

    let pupil = fx.pixel(32, 32);
    assert!(
        (pupil[0] as i32 - pupil[1] as i32).abs() <= 2
            && (pupil[1] as i32 - pupil[2] as i32).abs() <= 2,
        "the pupil is still coloured: {pupil:?}"
    );
    assert!(
        pupil[0] < 120,
        "the pupil should have gone dark and neutral, got {pupil:?}"
    );
    assert_eq!(pupil[3], 255, "red-eye ate the coverage");
    // Skin inside the box, and grey inside the box, are both left alone.
    assert_eq!(fx.pixel(38, 30), SKIN, "red-eye desaturated the skin");
    assert_eq!(
        fx.pixel(44, 44),
        [128, 128, 128, 255],
        "red-eye touched a neutral grey"
    );
    // And nothing outside the box moved at all.
    assert_eq!(fx.pixel(4, 4), SKIN);
}

#[test]
fn the_magic_eraser_clears_the_region_it_was_clicked_on_and_nothing_else() {
    let mut fx = fixture(48, 8);
    fx.paint_rect(PixelRect::new(0, 0, 12, 8), [60, 60, 60, 255]);
    fx.paint_rect(PixelRect::new(12, 0, 24, 8), [200, 200, 200, 255]);
    fx.paint_rect(PixelRect::new(36, 0, 12, 8), [60, 60, 60, 255]);

    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        let mut tool = MagicEraserTool::default();
        tool.antialias = false;
        tool.on_pointer_down(&mut ctx, PointerEvent::at(2.0, 4.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(2.0, 4.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a magic-eraser click is one command");
    let undo = fx.commit(cmds);

    assert_eq!(fx.pixel(2, 4)[3], 0, "the clicked band was not erased");
    assert_eq!(fx.pixel(11, 0)[3], 0, "the band was only partly erased");
    assert_eq!(
        fx.pixel(24, 4),
        [200, 200, 200, 255],
        "the eraser crossed into the light band"
    );
    assert_eq!(
        fx.pixel(40, 4),
        [60, 60, 60, 255],
        "a contiguous erase jumped to the disconnected band"
    );
    fx.commit(undo);
    assert_eq!(fx.pixel(2, 4), [60, 60, 60, 255], "undo did not restore");
}

/// A 2x2 pattern with four distinguishable corners.
fn checker() -> Pattern {
    Pattern::new(
        2,
        2,
        vec![
            255, 0, 0, 255, // (0, 0)
            0, 255, 0, 255, // (1, 0)
            0, 0, 255, 255, // (0, 1)
            255, 255, 0, 255, // (1, 1)
        ],
    )
    .unwrap()
}

#[test]
fn the_pattern_stamp_lays_the_active_pattern_down_in_document_space() {
    let mut fx = fixture(64, 64);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let pattern = checker();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        ctx.pattern = Some(pattern.clone());
        let mut tool = StrokeTool::new(
            ToolId::PatternStamp,
            BrushSettings {
                size: 20.0,
                hardness: 1.0,
                spacing: 0.05,
                size_pressure: false,
                ..Default::default()
            },
            StrokeOp::PatternStamp,
        );
        tool.on_pointer_down(&mut ctx, PointerEvent::at(32.0, 32.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(32.0, 32.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a pattern stamp stroke is one command");
    fx.commit(cmds);

    // The pattern is anchored to the document, not to the dab.
    for (x, y) in [(32, 32), (33, 32), (32, 33), (33, 33), (30, 30)] {
        assert_eq!(
            fx.pixel(x, y),
            pattern.sample(x, y),
            "the stamp did not lay the pattern at ({x}, {y})"
        );
    }
    assert_eq!(
        fx.pixel(2, 2),
        [0, 0, 0, 0],
        "the stamp reached outside the dab"
    );
}

#[test]
fn pattern_fill_tiles_the_pattern_across_the_selection_only() {
    let mut fx = fixture(64, 64);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let pattern = checker();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        ctx.pattern = Some(pattern.clone());
        ctx.selection = Selection::Rect {
            min: IVec2::new(10, 10),
            max: IVec2::new(30, 30),
        };
        let mut tool = PatternFillTool::default();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(20.0, 20.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(20.0, 20.0))
            .unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a pattern fill is one command");
    fx.commit(cmds);

    for (x, y) in [(10, 10), (11, 10), (10, 11), (29, 29), (20, 21)] {
        assert_eq!(
            fx.pixel(x, y),
            pattern.sample(x, y),
            "the fill missed ({x}, {y})"
        );
    }
    assert_eq!(
        fx.pixel(31, 20),
        [0, 0, 0, 0],
        "the fill escaped the selection"
    );
    assert_eq!(
        fx.pixel(20, 9),
        [0, 0, 0, 0],
        "the fill escaped the selection"
    );
}

#[test]
fn move_auto_select_takes_the_topmost_layer_with_an_opaque_pixel_under_the_cursor() {
    let mut fx = fixture(64, 64);
    let active = fx.layer;
    let top = layer_model::Layer::raster("top").id;
    let bottom = layer_model::Layer::raster("bottom").id;
    // The top layer has a hole where the bottom one is opaque.
    fx.tiles
        .put_pixel(PixelKey::Layer(bottom), 10, 10, [0, 0, 255, 255]);
    fx.tiles
        .put_pixel(PixelKey::Layer(top), 40, 40, [255, 0, 0, 255]);
    fx.tiles
        .put_pixel(PixelKey::Layer(bottom), 40, 40, [0, 0, 255, 255]);

    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(active);
    ctx.layer_stack = vec![top, bottom];

    fn picked(ctx: &mut ToolContext<'_>, at: (f32, f32), auto: bool) -> layer_model::LayerId {
        let mut tool = MoveTool::default();
        tool.auto_select = auto;
        tool.on_pointer_down(ctx, PointerEvent::at(at.0, at.1))
            .unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(at.0 + 12.0, at.1 + 4.0))
            .unwrap();
        match ctx.drain().pop().expect("the move emitted nothing") {
            Command::TransformLayer { layer_id, matrix } => {
                assert_eq!(&matrix[4..], &[12.0, 4.0]);
                layer_id
            }
            other => panic!("expected TransformLayer, got {}", other.label()),
        }
    }

    assert_eq!(
        picked(&mut ctx, (40.0, 40.0), true),
        top,
        "auto-select did not take the topmost opaque layer"
    );
    assert_eq!(
        picked(&mut ctx, (10.0, 10.0), true),
        bottom,
        "auto-select claimed a layer that is transparent there"
    );
    // Nothing opaque anywhere under the cursor falls back to the active layer.
    assert_eq!(picked(&mut ctx, (60.0, 60.0), true), active);
    // With auto-select off the panel's choice wins even over an opaque pixel.
    assert_eq!(
        picked(&mut ctx, (40.0, 40.0), false),
        active,
        "auto-select was off and the tool still grabbed another layer"
    );
}

#[test]
fn move_align_shifts_only_the_layers_that_are_not_already_aligned() {
    let mut fx = fixture(128, 128);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);

    let a = layer_model::Layer::raster("a").id;
    let b = layer_model::Layer::raster("b").id;
    let target = PixelRect::new(0, 0, 100, 100);
    MoveTool::align(
        &mut ctx,
        &[
            (a, PixelRect::new(10, 10, 20, 40)),
            // Already flush with the target's left edge: nothing to do.
            (b, PixelRect::new(0, 50, 20, 20)),
        ],
        target,
        Alignment::Left,
    );
    let cmds = ctx.drain();
    assert_eq!(cmds.len(), 1, "an already-aligned layer must not be moved");
    match &cmds[0] {
        Command::TransformLayer { layer_id, matrix } => {
            assert_eq!(*layer_id, a);
            assert_eq!(&matrix[4..], &[-10.0, 0.0]);
        }
        other => panic!("expected TransformLayer, got {}", other.label()),
    }

    // Centring moves on the axis asked for and no other.
    MoveTool::align(
        &mut ctx,
        &[(a, PixelRect::new(10, 10, 20, 40))],
        target,
        Alignment::VerticalCenter,
    );
    match &ctx.drain()[0] {
        Command::TransformLayer { matrix, .. } => assert_eq!(&matrix[4..], &[0.0, 20.0]),
        other => panic!("expected TransformLayer, got {}", other.label()),
    }
}

#[test]
fn every_lasso_emits_a_mask_covering_the_outline_it_drew() {
    // A dark square on a light field, so the magnetic lasso has an edge to snap
    // to and the other two have something to sit on top of.
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [230, 230, 230, 255]);
    fx.paint_rect(PixelRect::new(20, 20, 24, 24), [20, 20, 20, 255]);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
    ctx.sample_from = Some(PixelKey::Layer(layer));

    let outline = [(18.0, 18.0), (46.0, 18.0), (46.0, 46.0), (18.0, 46.0)];

    // Freehand: the drag *is* the path.
    let mut free = LassoTool::new(LassoKind::Freehand);
    free.on_pointer_down(
        &mut ctx,
        PointerEvent::at(outline[0].0, outline[0].1).with_modifiers(Modifiers::shift()),
    )
    .unwrap();
    for (x, y) in &outline[1..] {
        free.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
            .unwrap();
    }
    free.on_pointer_up(&mut ctx, PointerEvent::at(outline[0].0, outline[0].1))
        .unwrap();
    let edits = ctx.drain_selection();
    assert_eq!(edits.len(), 1, "the freehand lasso emitted nothing");
    assert_eq!(edits[0].op, BooleanOp::Add, "shift must mean add");
    assert!(
        edits[0].incoming.coverage_at(IVec2::new(32, 32)) > 0.5,
        "the freehand outline did not enclose its own centre"
    );
    assert_eq!(edits[0].incoming.coverage_at(IVec2::new(4, 4)), 0.0);

    // Polygonal: one click per corner, and a click back on the first closes it.
    let mut poly = LassoTool::new(LassoKind::Polygonal);
    for (x, y) in &outline {
        poly.on_pointer_down(&mut ctx, PointerEvent::at(*x, *y))
            .unwrap();
        poly.on_pointer_up(&mut ctx, PointerEvent::at(*x, *y))
            .unwrap();
    }
    assert!(
        ctx.selection_edits().is_empty(),
        "an unclosed polygon emitted a selection"
    );
    poly.on_pointer_down(&mut ctx, PointerEvent::at(outline[0].0 + 2.0, outline[0].1))
        .unwrap();
    let edits = ctx.drain_selection();
    assert_eq!(edits.len(), 1, "clicking the first vertex did not close");
    assert_eq!(edits[0].op, BooleanOp::Replace);
    assert!(edits[0].incoming.coverage_at(IVec2::new(32, 32)) > 0.5);
    assert_eq!(edits[0].incoming.coverage_at(IVec2::new(60, 60)), 0.0);
    assert!(!poly.is_active(), "the closed polygon kept its path");

    // Magnetic: sparse anchors, and the path snapped onto the contrast edge.
    let mut mag = LassoTool::new(LassoKind::Magnetic);
    mag.on_pointer_down(
        &mut ctx,
        PointerEvent::at(outline[0].0, outline[0].1).with_modifiers(Modifiers::alt()),
    )
    .unwrap();
    for (x, y) in &outline[1..] {
        mag.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y))
            .unwrap();
    }
    mag.on_pointer_up(&mut ctx, PointerEvent::at(outline[0].0, outline[0].1))
        .unwrap();
    let edits = ctx.drain_selection();
    assert_eq!(edits.len(), 1, "the magnetic lasso emitted nothing");
    assert_eq!(edits[0].op, BooleanOp::Subtract, "alt must mean subtract");
    assert!(
        edits[0].incoming.coverage_at(IVec2::new(32, 32)) > 0.5,
        "the magnetic outline lost the region it was drawn around"
    );
}

#[test]
fn the_wand_and_quick_select_pick_the_region_under_the_pointer() {
    let mut fx = fixture(48, 16);
    fx.paint_rect(PixelRect::new(0, 0, 16, 16), [40, 40, 40, 255]);
    fx.paint_rect(PixelRect::new(16, 0, 16, 16), [220, 220, 220, 255]);
    fx.paint_rect(PixelRect::new(32, 0, 16, 16), [40, 40, 40, 255]);
    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
    ctx.sample_from = Some(PixelKey::Layer(layer));

    let mut wand = WandTool::new(WandKind::Magic);
    wand.wand.contiguous = true;
    wand.wand.antialias = 0.0;
    wand.on_pointer_down(
        &mut ctx,
        PointerEvent::at(4.0, 8.0).with_modifiers(Modifiers::shift()),
    )
    .unwrap();
    wand.on_pointer_up(&mut ctx, PointerEvent::at(4.0, 8.0))
        .unwrap();
    let edits = ctx.drain_selection();
    assert_eq!(edits.len(), 1, "the wand emitted nothing");
    assert_eq!(edits[0].op, BooleanOp::Add);
    assert!(edits[0].incoming.coverage_at(IVec2::new(8, 8)) > 0.5);
    assert_eq!(
        edits[0].incoming.coverage_at(IVec2::new(24, 8)),
        0.0,
        "the wand crossed into the light band"
    );
    assert_eq!(
        edits[0].incoming.coverage_at(IVec2::new(40, 8)),
        0.0,
        "a contiguous wand jumped to the disconnected band"
    );

    // Quick select scrubs: the stroke's own colours decide what is included.
    let mut quick = WandTool::new(WandKind::Quick);
    quick
        .on_pointer_down(&mut ctx, PointerEvent::at(2.0, 8.0))
        .unwrap();
    for x in 3..14 {
        quick
            .on_pointer_move(&mut ctx, PointerEvent::at(x as f32, 8.0))
            .unwrap();
    }
    quick
        .on_pointer_up(&mut ctx, PointerEvent::at(14.0, 8.0))
        .unwrap();
    let edits = ctx.drain_selection();
    assert_eq!(edits.len(), 1, "quick select emitted nothing");
    assert_eq!(edits[0].op, BooleanOp::Replace);
    assert!(
        edits[0].incoming.coverage_at(IVec2::new(8, 8)) > 0.5,
        "quick select missed the band it was scrubbed over"
    );
    let light = edits[0].incoming.coverage_at(IVec2::new(28, 8));
    assert!(
        light < 0.3,
        "quick select claimed the light band it was never scrubbed over: {light}"
    );
    // Quick select spreads by colour rather than by connectivity, so the far
    // band of the same dark grey comes with it. That is the difference between
    // it and the contiguous wand above, and it is worth pinning.
    assert!(
        edits[0].incoming.coverage_at(IVec2::new(40, 8)) > 0.5,
        "quick select is colour-driven and should have taken the far dark band"
    );
}

/// `TransformState::dest_bounds` used to consult the warp mesh whenever one
/// existed, while `commit` and `resample` gate it on `TransformMode::Warp`.
/// Entering warp mode is the only way a mesh is created, and switching modes
/// does not throw it away — it cannot, or switching back would lose the user's
/// warp — so a scale performed after a visit to warp mode was bounded by the
/// stale mesh box and the result was silently truncated to it. The patch is
/// tile-aligned to 256 px, which hides the truncation for small drags; this one
/// goes well past that.
#[test]
fn a_scale_after_a_visit_to_warp_mode_is_not_clipped_to_the_stale_mesh() {
    let mut fx = fixture(512, 512);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [255, 0, 0, 255]);

    let mut tool = TransformTool::with_mode(TransformMode::Warp);
    tool.begin(PixelRect::new(0, 0, 64, 64)).unwrap();
    // Touching a warp handle is what creates the mesh. A zero-length drag
    // leaves it at the identity, so the picture is untouched and all the visit
    // leaves behind is the mesh itself.
    tool.state.as_mut().unwrap().drag(
        TransformMode::Warp,
        Handle::Mesh(1, 1),
        Vec2::new(21.0, 21.0),
        Vec2::new(21.0, 21.0),
    );
    assert!(
        tool.state.as_ref().unwrap().mesh.is_some(),
        "the warp visit did not create a mesh, so this test proves nothing"
    );

    // Back to scale, and blow the box out to 500x500 — far outside the mesh.
    tool.mode = TransformMode::Scale;
    tool.state.as_mut().unwrap().drag(
        TransformMode::Scale,
        Handle::Corner(2),
        Vec2::new(64.0, 64.0),
        Vec2::new(500.0, 500.0),
    );

    {
        let state = tool.state.as_ref().unwrap();
        let scaled = state
            .dest_bounds(fx.canvas(), TransformMode::Scale)
            .expect("a 500x500 destination has bounds");
        assert!(
            scaled.width > 400 && scaled.height > 400,
            "dest_bounds took the destination from the stale mesh: {scaled:?}"
        );
        // ...and in warp mode the mesh is still exactly what bounds it.
        let warped = state
            .dest_bounds(fx.canvas(), TransformMode::Warp)
            .expect("the mesh has bounds");
        assert!(
            warped.width <= 65 && warped.height <= 65,
            "warp mode stopped using the mesh: {warped:?}"
        );
    }

    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        tool.commit(&mut ctx).unwrap();
        ctx.drain()
    };
    assert_eq!(cmds.len(), 1, "a transform commit is one command");
    fx.commit(cmds);

    assert_eq!(
        fx.pixel(400, 400),
        [255, 0, 0, 255],
        "the scaled result was truncated to the stale mesh box"
    );
}
