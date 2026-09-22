//! W1-B2: the options bar reaches the paint/retouch, shape and type tools.
//!
//! Every control the registry declares for these tools used to be either
//! refused (`Bool`/`Int`/`Choice` keys on the stroke tools, `size_px` on
//! Type, every geometry key on the shapes) or accepted by the trait default
//! and dropped (`Choice` keys: Dodge/Burn `range`, Sponge `mode`, the shapes'
//! `mode`, Type `font_family`), and the paint blend mode never reached any
//! tool at all. These tests drive the real tools through `set_setting` with
//! a NON-default value and then assert on what the gesture produced — the
//! pixels, the path, the created layer — not on a helper.

use editor_core::{Command, Selection};
use glam::Vec2;
use layer_model::LayerKind;
use raster::PixelRect;
use tools::brush::BrushSettings;
use tools::registry::{self, OptionKind};
use tools::shape::{ShapeKind, ShapeMode, ShapeTool};
use tools::stroke::{StrokeOp, StrokeTool};
use tools::text::TypeTool;
use tools::tool::{PointerEvent, Tool, ToolContext, ToolId, ToolSetting};
use tools::transform::{TransformMode, TransformTool};
use tools::{blend_mode_from_choice, BlendMode, ToolError, ToolGroup, BLEND_MODE_KEY};
use vector::PathEl;

mod common;
use common::{fixture, line, stroke};

/// The tools this finding names: every stroke tool, the seven shapes, Type
/// and Free Transform, plus Patch and Red Eye (wired by W1-B1 in edit.rs;
/// pinned here because the finding lists their keys as dead). Pen declares
/// no options and is covered by the loop trivially.
const OWNED: &[ToolId] = &[
    ToolId::SpotHealing,
    ToolId::HealingBrush,
    ToolId::Brush,
    ToolId::Pencil,
    ToolId::ColorReplacement,
    ToolId::CloneStamp,
    ToolId::PatternStamp,
    ToolId::Eraser,
    ToolId::BackgroundEraser,
    ToolId::Blur,
    ToolId::Sharpen,
    ToolId::RefineBoundary,
    ToolId::Smudge,
    ToolId::Dodge,
    ToolId::Burn,
    ToolId::Sponge,
    ToolId::Pen,
    ToolId::Type,
    ToolId::Rectangle,
    ToolId::RoundedRectangle,
    ToolId::Ellipse,
    ToolId::Polygon,
    ToolId::Star,
    ToolId::Line,
    ToolId::CustomShape,
    ToolId::FreeTransform,
    ToolId::Patch,
    ToolId::RedEye,
];

/// A value that is NOT the schema default, inside the schema range.
fn non_default(kind: &OptionKind) -> ToolSetting {
    match *kind {
        OptionKind::Float { min, max, default } => {
            ToolSetting::Float(if default < max { max } else { min })
        }
        OptionKind::Int { min, max, default } => {
            ToolSetting::Int(if default < max { max } else { min })
        }
        OptionKind::Bool { default } => ToolSetting::Bool(!default),
        OptionKind::Choice { choices, default } => {
            ToolSetting::Choice((default + 1) % choices.len())
        }
        OptionKind::Color { default } => {
            ToolSetting::Color([1.0 - default[0], default[1], default[2], default[3]])
        }
    }
}

fn multiply_index() -> usize {
    BlendMode::ALL
        .iter()
        .position(|m| *m == BlendMode::Multiply)
        .expect("Multiply is a blend mode")
}

// ---------------------------------------------------------------------------
// The contract: every declared option of every owned tool is ACCEPTED.
// ---------------------------------------------------------------------------

#[test]
fn every_option_of_every_owned_tool_accepts_a_non_default_value() {
    let mut refused = Vec::new();
    for id in OWNED {
        let info = registry::info(*id).expect("every owned tool is in the registry");
        let mut tool = registry::make(*id);
        for spec in info.options {
            if let Err(e) = tool.set_setting(spec.key, non_default(&spec.kind)) {
                refused.push(format!("{:?}/{}: {e}", id, spec.key));
            }
        }
        // The options bar offers the paint blend mode to the tools that
        // composite a stroke; every such tool here must answer it.
        if tools::composites_strokes(*id) {
            if let Err(e) = tool.set_setting(BLEND_MODE_KEY, ToolSetting::Choice(multiply_index()))
            {
                refused.push(format!("{:?}/{}: {e}", id, BLEND_MODE_KEY));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "options the registry declares were refused by their own tool:\n{}",
        refused.join("\n")
    );
}

#[test]
fn a_known_key_with_the_wrong_kind_is_a_mismatch_and_an_unknown_key_is_unknown() {
    let mut dodge = registry::make(ToolId::Dodge);
    assert!(matches!(
        dodge.set_setting("range", ToolSetting::Float(1.0)),
        Err(ToolError::OptionKindMismatch { .. })
    ));
    assert!(matches!(
        dodge.set_setting("nonesuch", ToolSetting::Float(1.0)),
        Err(ToolError::UnknownOption { .. })
    ));
    let mut star = registry::make(ToolId::Star);
    assert!(matches!(
        star.set_setting("points", ToolSetting::Float(8.0)),
        Err(ToolError::OptionKindMismatch { .. })
    ));
    // A geometry key the registry never offers this kind is unknown, not a
    // silent success.
    assert!(matches!(
        star.set_setting("sides", ToolSetting::Int(6)),
        Err(ToolError::UnknownOption { .. })
    ));
    let mut ty = registry::make(ToolId::Type);
    assert!(matches!(
        ty.set_setting("size_px", ToolSetting::Choice(1)),
        Err(ToolError::OptionKindMismatch { .. })
    ));
}

// ---------------------------------------------------------------------------
// Dodge / Burn: the range restricts the tonal band.
// ---------------------------------------------------------------------------

/// A white layer, a burn stroke across its middle, and the pixel under it.
fn burn_white_with_range(range: usize) -> [u8; 4] {
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [255, 255, 255, 255]);
    let mut tool = registry::make(ToolId::Burn);
    tool.set_setting("size", ToolSetting::Float(12.0)).unwrap();
    tool.set_setting("hardness", ToolSetting::Float(1.0))
        .unwrap();
    tool.set_setting("exposure", ToolSetting::Float(1.0))
        .unwrap();
    tool.set_setting("range", ToolSetting::Choice(range))
        .expect("the registry declares `range` for Burn");
    let cmds = stroke(
        &mut fx,
        tool.as_mut(),
        &line((16.0, 32.0), (48.0, 32.0), 16),
        common::BLACK,
        Selection::None,
    );
    fx.commit(cmds);
    fx.pixel(32, 32)
}

#[test]
fn burn_range_shadows_leaves_a_white_pixel_where_highlights_darkens_it() {
    // Registry order: 0 Shadows, 1 Midtones, 2 Highlights.
    let shadows = burn_white_with_range(0);
    let highlights = burn_white_with_range(2);
    assert!(
        shadows[0] >= 250 && shadows[1] >= 250 && shadows[2] >= 250,
        "a Shadows burn must leave white (near-)white: {shadows:?}"
    );
    assert!(
        highlights[0] < 150,
        "a Highlights burn at full exposure must darken white: {highlights:?}"
    );
    assert!(shadows[0] > highlights[0], "{shadows:?} vs {highlights:?}");
}

#[test]
fn dodge_range_shadows_leaves_a_white_pixel_unchanged() {
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [255, 255, 255, 255]);
    let mut tool = registry::make(ToolId::Dodge);
    tool.set_setting("size", ToolSetting::Float(12.0)).unwrap();
    tool.set_setting("exposure", ToolSetting::Float(1.0))
        .unwrap();
    tool.set_setting("range", ToolSetting::Choice(0))
        .expect("the registry declares `range` for Dodge");
    let cmds = stroke(
        &mut fx,
        tool.as_mut(),
        &line((16.0, 32.0), (48.0, 32.0), 16),
        common::BLACK,
        Selection::None,
    );
    fx.commit(cmds);
    assert_eq!(fx.pixel(32, 32), [255, 255, 255, 255]);
}

// ---------------------------------------------------------------------------
// Sponge: the mode choice picks the direction.
// ---------------------------------------------------------------------------

fn sponge_pink_with_mode(mode: usize) -> [u8; 4] {
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [200, 100, 100, 255]);
    let mut tool = registry::make(ToolId::Sponge);
    tool.set_setting("size", ToolSetting::Float(12.0)).unwrap();
    tool.set_setting("hardness", ToolSetting::Float(1.0))
        .unwrap();
    tool.set_setting("amount", ToolSetting::Float(1.0)).unwrap();
    tool.set_setting("mode", ToolSetting::Choice(mode))
        .expect("the registry declares `mode` for Sponge");
    let cmds = stroke(
        &mut fx,
        tool.as_mut(),
        &line((16.0, 32.0), (48.0, 32.0), 16),
        common::BLACK,
        Selection::None,
    );
    fx.commit(cmds);
    fx.pixel(32, 32)
}

#[test]
fn sponge_mode_desaturate_greys_and_saturate_pushes_the_chroma_out() {
    let before_spread = 200i32 - 100;
    let desat = sponge_pink_with_mode(0);
    let sat = sponge_pink_with_mode(1);
    let desat_spread = i32::from(desat[0]) - i32::from(desat[1]);
    let sat_spread = i32::from(sat[0]) - i32::from(sat[1]);
    assert!(
        desat_spread.abs() < 4,
        "Desaturate at full amount must grey the pixel: {desat:?}"
    );
    assert!(
        sat_spread > before_spread,
        "Saturate must widen the chroma beyond {before_spread}: {sat:?}"
    );
}

// ---------------------------------------------------------------------------
// Clone stamp / healing brush: `aligned` toggles the source behaviour.
// ---------------------------------------------------------------------------

#[test]
fn aligned_off_re_anchors_every_stroke_and_aligned_on_keeps_the_offset() {
    for op in [StrokeOp::CloneStamp, StrokeOp::Healing { softness: 4.0 }] {
        let mut tool = StrokeTool::new(ToolId::CloneStamp, BrushSettings::default(), op);
        tool.clone.aligned = true;
        tool.set_setting("aligned", ToolSetting::Bool(false))
            .expect("the registry declares `aligned`");
        assert!(!tool.clone.aligned);
        tool.clone.set_anchor(Vec2::new(100.0, 100.0));
        let first = tool.clone.begin_stroke(Vec2::new(10.0, 10.0)).unwrap();
        let second = tool.clone.begin_stroke(Vec2::new(50.0, 50.0)).unwrap();
        assert_ne!(
            first, second,
            "non-aligned: every stroke restarts at the anchor"
        );

        tool.set_setting("aligned", ToolSetting::Bool(true))
            .unwrap();
        tool.clone.set_anchor(Vec2::new(100.0, 100.0));
        let first = tool.clone.begin_stroke(Vec2::new(10.0, 10.0)).unwrap();
        let second = tool.clone.begin_stroke(Vec2::new(50.0, 50.0)).unwrap();
        assert_eq!(
            first, second,
            "aligned: the offset is locked after the first stroke"
        );
    }
    // Paint has no source, so `aligned` is not one of its keys.
    let mut brush = registry::make(ToolId::Brush);
    assert!(matches!(
        brush.set_setting("aligned", ToolSetting::Bool(false)),
        Err(ToolError::UnknownOption { .. })
    ));
}

// ---------------------------------------------------------------------------
// The paint blend mode reaches the brush and composites the dabs.
// ---------------------------------------------------------------------------

fn paint_white_over_grey(mode: Option<usize>) -> [u8; 4] {
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [128, 128, 128, 255]);
    let mut tool = registry::make(ToolId::Brush);
    tool.set_setting("size", ToolSetting::Float(12.0)).unwrap();
    tool.set_setting("hardness", ToolSetting::Float(1.0))
        .unwrap();
    if let Some(index) = mode {
        tool.set_setting(BLEND_MODE_KEY, ToolSetting::Choice(index))
            .expect("the Mode combo reaches the brush");
    }
    let cmds = stroke(
        &mut fx,
        tool.as_mut(),
        &line((16.0, 32.0), (48.0, 32.0), 16),
        [1.0, 1.0, 1.0, 1.0],
        Selection::None,
    );
    fx.commit(cmds);
    fx.pixel(32, 32)
}

#[test]
fn a_multiply_brush_leaves_the_base_where_a_normal_brush_covers_it() {
    let normal = paint_white_over_grey(None);
    assert_eq!(normal, [255, 255, 255, 255], "Normal: white covers grey");

    let multiply = paint_white_over_grey(Some(multiply_index()));
    for c in &multiply[..3] {
        assert!(
            (i32::from(*c) - 128).abs() <= 1,
            "Multiply by white is the identity, got {multiply:?}"
        );
    }
    assert_eq!(multiply[3], 255);

    // Screen with black is the identity the other way round.
    let screen = BlendMode::ALL
        .iter()
        .position(|m| *m == BlendMode::Screen)
        .unwrap();
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [255, 255, 255, 255]);
    let mut tool = registry::make(ToolId::Brush);
    tool.set_setting("hardness", ToolSetting::Float(1.0))
        .unwrap();
    tool.set_setting(BLEND_MODE_KEY, ToolSetting::Choice(screen))
        .unwrap();
    let cmds = stroke(
        &mut fx,
        tool.as_mut(),
        &line((16.0, 32.0), (48.0, 32.0), 16),
        common::BLACK,
        Selection::None,
    );
    fx.commit(cmds);
    assert_eq!(fx.pixel(32, 32), [255, 255, 255, 255], "Screen with black");
}

/// The Mode combo's offer set IS the answer set: the four stroke tools whose
/// op lays a source colour over the layer (Brush, Pencil, Clone Stamp,
/// Pattern Stamp) answer `ui.blend_mode`, and every other tool refuses it —
/// the retouching strokes because `apply_stroke` has no colour of theirs to
/// blend (a key accepted and ignored would be the dead control this finding
/// is about), Patch, Red Eye, the fills, the gradient and the magic eraser
/// because they stamp no dabs at all. Any drift between `composites_strokes`
/// and the registry's construction shows here.
#[test]
fn composites_strokes_is_exactly_the_set_that_answers_the_mode_key() {
    let mut drift = Vec::new();
    for info in registry::all() {
        let answers = registry::make(info.id)
            .set_setting(BLEND_MODE_KEY, ToolSetting::Choice(multiply_index()))
            .is_ok();
        let composites = tools::composites_strokes(info.id);
        let in_groups = matches!(info.group, ToolGroup::Paint | ToolGroup::Retouch);
        // Outside the two groups the trait default accepts any Choice, so
        // the equivalence is pinned inside them and the predicate is pinned
        // false outside.
        if (in_groups && answers != composites) || (!in_groups && composites) {
            drift.push(format!(
                "{:?}: answers={answers} composites_strokes={composites}",
                info.id
            ));
        }
    }
    assert!(drift.is_empty(), "{drift:?}");
    assert!(tools::composites_strokes(ToolId::Brush));
    assert!(tools::composites_strokes(ToolId::Pencil));
    assert!(tools::composites_strokes(ToolId::CloneStamp));
    assert!(tools::composites_strokes(ToolId::PatternStamp));
    // The retouching strokes: Lerp/Erase preparations, no source colour.
    for id in [
        ToolId::Sponge,
        ToolId::Dodge,
        ToolId::Burn,
        ToolId::Blur,
        ToolId::Sharpen,
        ToolId::Smudge,
        ToolId::Eraser,
        ToolId::BackgroundEraser,
        ToolId::ColorReplacement,
        ToolId::HealingBrush,
        ToolId::SpotHealing,
        ToolId::RefineBoundary,
    ] {
        assert!(!tools::composites_strokes(id), "{id:?}");
        assert!(
            matches!(
                registry::make(id).set_setting(BLEND_MODE_KEY, ToolSetting::Choice(1)),
                Err(ToolError::UnknownOption { .. })
            ),
            "{id:?} must refuse the Mode key its compositing never reads"
        );
    }
    assert!(!tools::composites_strokes(ToolId::Patch));
    assert!(!tools::composites_strokes(ToolId::RedEye));
    assert!(!tools::composites_strokes(ToolId::Gradient));
    assert!(!tools::composites_strokes(ToolId::PaintBucket));
    assert!(!tools::composites_strokes(ToolId::Hand));
    assert!(matches!(
        registry::make(ToolId::Patch).set_setting(BLEND_MODE_KEY, ToolSetting::Choice(1)),
        Err(ToolError::UnknownOption { .. })
    ));
}

/// The predicate the key is gated on is the one `apply_stroke` composites
/// by: exactly the ops `prepare` turns into `Blend::Over`. A Multiply held
/// by the clone stamp changes its pixels the way it changes the brush's
/// (Multiply by a white source is the identity), so the key is live for
/// every tool it is accepted by, not only the brush.
#[test]
fn a_multiply_clone_stamp_leaves_the_base_the_way_a_multiply_brush_does() {
    fn clone_white_over_grey(mode: Option<usize>) -> [u8; 4] {
        let mut fx = fixture(64, 64);
        // Left half white (the source), right half grey (the target).
        fx.paint_rect(PixelRect::new(0, 0, 32, 64), [255, 255, 255, 255]);
        fx.paint_rect(PixelRect::new(32, 0, 32, 64), [128, 128, 128, 255]);
        let mut tool = StrokeTool::new(
            ToolId::CloneStamp,
            BrushSettings::default(),
            StrokeOp::CloneStamp,
        );
        tool.set_setting("size", ToolSetting::Float(8.0)).unwrap();
        tool.set_setting("hardness", ToolSetting::Float(1.0))
            .unwrap();
        if let Some(index) = mode {
            tool.set_setting(BLEND_MODE_KEY, ToolSetting::Choice(index))
                .expect("the Mode combo reaches the clone stamp");
        }
        // The source is anchored in the white half (what an Alt-click
        // does), and the stroke lands in the grey half.
        tool.clone.set_anchor(Vec2::new(16.0, 32.0));
        let cmds = stroke(
            &mut fx,
            &mut tool,
            &line((44.0, 32.0), (52.0, 32.0), 8),
            common::BLACK,
            Selection::None,
        );
        fx.commit(cmds);
        fx.pixel(48, 32)
    }
    let normal = clone_white_over_grey(None);
    assert_eq!(
        normal,
        [255, 255, 255, 255],
        "Normal: the white source covers grey"
    );
    let multiply = clone_white_over_grey(Some(multiply_index()));
    for c in &multiply[..3] {
        assert!(
            (i32::from(*c) - 128).abs() <= 1,
            "Multiply by a white source is the identity, got {multiply:?}"
        );
    }
}

#[test]
fn the_blend_mode_choice_index_is_the_position_in_blend_mode_all() {
    assert_eq!(blend_mode_from_choice(0), Some(BlendMode::Normal));
    assert_eq!(
        blend_mode_from_choice(multiply_index()),
        Some(BlendMode::Multiply)
    );
    assert_eq!(blend_mode_from_choice(BlendMode::ALL.len()), None);
    let mut tool = StrokeTool::new(
        ToolId::Brush,
        BrushSettings::default(),
        StrokeOp::Paint {
            color: [0.0, 0.0, 0.0, 1.0],
        },
    );
    tool.set_setting(BLEND_MODE_KEY, ToolSetting::Choice(multiply_index()))
        .unwrap();
    assert_eq!(tool.blend_mode, BlendMode::Multiply);
    // Out of range clamps to the last mode, the options bar's own rule.
    tool.set_setting(BLEND_MODE_KEY, ToolSetting::Choice(usize::MAX))
        .unwrap();
    assert_eq!(tool.blend_mode, *BlendMode::ALL.last().unwrap());
    assert!(matches!(
        tool.set_setting(BLEND_MODE_KEY, ToolSetting::Float(1.0)),
        Err(ToolError::OptionKindMismatch { .. })
    ));
}

// ---------------------------------------------------------------------------
// Shapes: every geometry option shapes the path.
// ---------------------------------------------------------------------------

/// The path a drag from `a` to `b` previews, after the tool took `settings`.
fn preview_after(id: ToolId, settings: &[(&str, ToolSetting)], a: Vec2, b: Vec2) -> vector::Path {
    let mut tool = registry::make(id);
    for (key, value) in settings {
        tool.set_setting(key, *value)
            .unwrap_or_else(|e| panic!("{id:?}/{key}: {e}"));
    }
    let mut tiles = tools::MemoryTiles::new();
    let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
    tool.on_pointer_down(&mut ctx, PointerEvent::at(a.x, a.y))
        .unwrap();
    tool.on_pointer_move(&mut ctx, PointerEvent::at(b.x, b.y))
        .unwrap();
    // `preview` is inherent to ShapeTool; rebuild the same tool concretely to
    // read it — the settings went through the trait object above, so this
    // proves the dyn route, and the path below proves the geometry.
    let kind = shape_kind_after(id, settings);
    let mut concrete = ShapeTool::new(kind, ShapeMode::VectorLayer);
    for (key, value) in settings {
        concrete.set_setting(key, *value).unwrap();
    }
    concrete
        .on_pointer_down(&mut ctx, PointerEvent::at(a.x, a.y))
        .unwrap();
    concrete
        .on_pointer_move(&mut ctx, PointerEvent::at(b.x, b.y))
        .unwrap();
    concrete.preview().expect("a non-degenerate drag previews")
}

fn shape_kind_after(id: ToolId, settings: &[(&str, ToolSetting)]) -> ShapeKind {
    let mut tool = match id {
        ToolId::Rectangle => ShapeTool::new(ShapeKind::Rectangle, ShapeMode::VectorLayer),
        ToolId::RoundedRectangle => ShapeTool::new(
            ShapeKind::RoundedRectangle { radius: 8.0 },
            ShapeMode::VectorLayer,
        ),
        ToolId::Ellipse => ShapeTool::new(ShapeKind::Ellipse, ShapeMode::VectorLayer),
        ToolId::Polygon => ShapeTool::new(ShapeKind::Polygon { sides: 6 }, ShapeMode::VectorLayer),
        ToolId::Star => ShapeTool::new(
            ShapeKind::Star {
                points: 5,
                inner_ratio: 0.4,
            },
            ShapeMode::VectorLayer,
        ),
        ToolId::Line => ShapeTool::new(ShapeKind::Line { width: 2.0 }, ShapeMode::VectorLayer),
        other => panic!("{other:?} is not a shape tool"),
    };
    for (key, value) in settings {
        tool.set_setting(key, *value).unwrap();
    }
    tool.kind
}

fn vertex_count(path: &vector::Path) -> usize {
    path.elements()
        .iter()
        .filter(|e| matches!(e, PathEl::MoveTo(_) | PathEl::LineTo(_)))
        .count()
}

#[test]
fn polygon_sides_and_star_points_shape_the_path() {
    let a = Vec2::new(20.0, 20.0);
    let b = Vec2::new(120.0, 120.0);
    let hexagon = preview_after(ToolId::Polygon, &[("sides", ToolSetting::Int(6))], a, b);
    assert_eq!(vertex_count(&hexagon), 6, "{hexagon:?}");
    let octagon = preview_after(ToolId::Polygon, &[("sides", ToolSetting::Int(8))], a, b);
    assert_eq!(vertex_count(&octagon), 8, "{octagon:?}");

    let star = preview_after(ToolId::Star, &[("points", ToolSetting::Int(8))], a, b);
    assert_eq!(vertex_count(&star), 16, "8 points alternate 16 vertices");

    // The indent moves the inner vertices: a 0.9 ratio is nearly the
    // polygon, a 0.2 ratio is a spiky star, so the two paths differ.
    let shallow = preview_after(
        ToolId::Star,
        &[("inner_ratio", ToolSetting::Float(0.9))],
        a,
        b,
    );
    let deep = preview_after(
        ToolId::Star,
        &[("inner_ratio", ToolSetting::Float(0.2))],
        a,
        b,
    );
    assert_ne!(shallow.elements(), deep.elements());
    // The inner radius scales with the ratio, so the deep star's second
    // vertex sits nearer the centre than the shallow star's.
    let centre = vector::point(70.0, 70.0);
    let inner = |p: &vector::Path| match p.elements()[1] {
        PathEl::LineTo(q) => q.distance(centre),
        ref other => panic!("{other:?}"),
    };
    assert!(inner(&deep) < inner(&shallow) * 0.5);
}

#[test]
fn line_weight_and_corner_radius_reach_the_path() {
    let a = Vec2::new(20.0, 50.0);
    let b = Vec2::new(120.0, 50.0);
    let thick = preview_after(ToolId::Line, &[("width", ToolSetting::Float(10.0))], a, b);
    let bb = thick.bounds();
    assert!(
        ((bb.max.y - bb.min.y) - 10.0).abs() < 0.6,
        "a 10px weight strokes a 10px tall horizontal line: {bb:?}"
    );
    let thin = preview_after(ToolId::Line, &[("width", ToolSetting::Float(1.0))], a, b);
    let tb = thin.bounds();
    assert!((tb.max.y - tb.min.y) < 2.0, "{tb:?}");

    let c = Vec2::new(20.0, 20.0);
    let d = Vec2::new(120.0, 80.0);
    let sharp = preview_after(
        ToolId::RoundedRectangle,
        &[("radius", ToolSetting::Float(0.0))],
        c,
        d,
    );
    let round = preview_after(
        ToolId::RoundedRectangle,
        &[("radius", ToolSetting::Float(25.0))],
        c,
        d,
    );
    assert_ne!(sharp.elements(), round.elements());
    // A 25px radius never reaches the corner point itself.
    let touches_corner = round.elements().iter().any(|e| match e {
        PathEl::MoveTo(p) | PathEl::LineTo(p) => {
            (p.x - 20.0).abs() < 1e-6 && (p.y - 20.0).abs() < 1e-6
        }
        _ => false,
    });
    assert!(!touches_corner, "{round:?}");
}

#[test]
fn from_center_draws_outward_from_the_press() {
    let a = Vec2::new(50.0, 50.0);
    let b = Vec2::new(70.0, 60.0);
    // The box shapes fill the drag box, so the box itself is the assertion.
    for id in [ToolId::Rectangle, ToolId::Ellipse, ToolId::RoundedRectangle] {
        let corner = preview_after(id, &[], a, b).bounds();
        assert!((corner.min.x - 50.0).abs() < 1e-3, "{id:?} {corner:?}");
        let centred = preview_after(id, &[("from_center", ToolSetting::Bool(true))], a, b).bounds();
        assert!(
            (centred.min.x - 30.0).abs() < 1e-3 && (centred.max.x - 70.0).abs() < 1e-3,
            "{id:?}: from-centre box spans 30..70, got {centred:?}"
        );
        assert!(
            (centred.min.y - 40.0).abs() < 1e-3 && (centred.max.y - 60.0).abs() < 1e-3,
            "{id:?}: {centred:?}"
        );
    }
    // The radial shapes are inscribed in the box's inner circle: corner-to-
    // corner that circle sits at (60, 55) with radius 5; from the centre it
    // sits at the press, (50, 50), with radius 10 — twice as wide, and
    // centred on the press rather than beside it.
    for id in [ToolId::Polygon, ToolId::Star] {
        let corner = preview_after(id, &[], a, b).bounds();
        let centred = preview_after(id, &[("from_center", ToolSetting::Bool(true))], a, b).bounds();
        let mid = |b: &vector::Bounds| (b.min.x + b.max.x) * 0.5;
        assert!((mid(&corner) - 60.0).abs() < 0.6, "{id:?} {corner:?}");
        assert!((mid(&centred) - 50.0).abs() < 1.2, "{id:?} {centred:?}");
        let width = |b: &vector::Bounds| b.max.x - b.min.x;
        assert!(
            (width(&centred) - 2.0 * width(&corner)).abs() < 1e-3,
            "{id:?}: from-centre doubles the radius: {corner:?} -> {centred:?}"
        );
    }
}

#[test]
fn shape_mode_rasterize_paints_pixels_where_shape_layer_creates_a_layer() {
    for id in [
        ToolId::Rectangle,
        ToolId::RoundedRectangle,
        ToolId::Ellipse,
        ToolId::Polygon,
        ToolId::Star,
        ToolId::Line,
        ToolId::CustomShape,
    ] {
        for (mode, expect_pixels) in [(0usize, false), (1, true)] {
            let mut fx = fixture(64, 64);
            let mut tool = registry::make(id);
            tool.set_setting("mode", ToolSetting::Choice(mode))
                .unwrap_or_else(|e| panic!("{id:?}: {e}"));
            let canvas = fx.canvas();
            let layer = fx.layer;
            let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
            ctx.foreground = common::RED;
            tool.on_pointer_down(&mut ctx, PointerEvent::at(8.0, 8.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(56.0, 48.0))
                .unwrap();
            let cmds = ctx.drain();
            assert_eq!(cmds.len(), 1, "{id:?} mode {mode}: {cmds:?}");
            match (&cmds[0], expect_pixels) {
                (Command::PaintTiles { .. }, true) | (Command::CreateLayer { .. }, false) => {}
                (other, _) => panic!(
                    "{id:?} mode {mode} emitted {} (expected pixels: {expect_pixels})",
                    other.label()
                ),
            }
            if expect_pixels {
                fx.commit(cmds);
                // The drag box's centre is inside every one of these shapes.
                let p = fx.pixel(32, 28);
                assert!(
                    p[3] > 0 && p[0] > 200,
                    "{id:?} rasterised nothing at the centre: {p:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Type: size and family reach the created layer.
// ---------------------------------------------------------------------------

fn created_text_layer(settings: &[(&str, ToolSetting)]) -> layer_model::TextLayer {
    let mut tool = registry::make(ToolId::Type);
    for (key, value) in settings {
        tool.set_setting(key, *value)
            .unwrap_or_else(|e| panic!("Type/{key}: {e}"));
    }
    let mut tiles = tools::MemoryTiles::new();
    let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
    tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
        .unwrap();
    tool.on_pointer_up(&mut ctx, PointerEvent::at(10.0, 10.0))
        .unwrap();
    let cmds = ctx.drain();
    let Some(Command::CreateLayer { layer }) = cmds.first() else {
        panic!("a Type click creates a layer: {cmds:?}");
    };
    let LayerKind::Text(text) = &layer.kind else {
        panic!("a Type click creates a TEXT layer: {:?}", layer.kind);
    };
    text.clone()
}

#[test]
fn type_size_and_font_family_reach_the_created_text_layer() {
    let big = created_text_layer(&[("size_px", ToolSetting::Float(48.0))]);
    let small = created_text_layer(&[("size_px", ToolSetting::Float(12.0))]);
    assert_eq!(big.size_px, 48.0);
    assert_eq!(small.size_px, 12.0);
    // The em size IS the layer's height driver in the text engine: a 48px
    // run is four times the 12px run's line height.
    assert!(big.size_px > small.size_px);

    // Registry order: 0 sans-serif, 1 serif, 2 monospace.
    let serif = created_text_layer(&[("font_family", ToolSetting::Choice(1))]);
    assert_eq!(serif.font_family, "serif");
    let mono = created_text_layer(&[("font_family", ToolSetting::Choice(2))]);
    assert_eq!(mono.font_family, "monospace");
    let default = created_text_layer(&[]);
    assert_eq!(default.font_family, "sans-serif");
    assert_eq!(default.size_px, 24.0);

    // The tool's own view of the choice list is the registry's.
    assert_eq!(tools::text::font_family_choice(1), Some("serif"));
    assert_eq!(tools::text::font_family_choice(99), Some("monospace"));

    // Out of the registry range clamps; a NaN is refused, not stored.
    let clamped = created_text_layer(&[("size_px", ToolSetting::Float(5000.0))]);
    assert_eq!(clamped.size_px, 512.0);
    let mut tool = TypeTool::default();
    assert!(tool
        .set_setting("size_px", ToolSetting::Float(f32::NAN))
        .is_err());
    assert_eq!(tool.size_px, 24.0);
}

// ---------------------------------------------------------------------------
// Red Eye and Patch: the retouch keys the finding lists as dead.
// ---------------------------------------------------------------------------

/// A mildly red layer (linear R/G ratio about 2.4), a Red Eye box dragged
/// over it with the given threshold and darken, and the pixel inside.
fn red_eye_fix(threshold: f32, darken: f32) -> [u8; 4] {
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), [180, 120, 120, 255]);
    let mut tool = registry::make(ToolId::RedEye);
    tool.set_setting("threshold", ToolSetting::Float(threshold))
        .expect("the registry declares `threshold` for Red Eye");
    tool.set_setting("darken", ToolSetting::Float(darken))
        .expect("the registry declares `darken` for Red Eye");
    let cmds = stroke(
        &mut fx,
        tool.as_mut(),
        &[(8.0, 8.0, 1.0), (40.0, 40.0, 1.0)],
        common::BLACK,
        Selection::None,
    );
    fx.commit(cmds);
    fx.pixel(20, 20)
}

#[test]
fn red_eye_threshold_and_darken_reach_the_fix() {
    // Registry range 1.0..4.0: at the top nothing this mild counts as flash
    // red and the pixel is left alone.
    let untouched = red_eye_fix(4.0, 0.5);
    assert_eq!(untouched, [180, 120, 120, 255], "threshold 4.0 leaves it");
    // At the bottom the pixel is fully flash red and goes grey...
    let grey = red_eye_fix(1.0, 0.0);
    assert!(
        grey[0] < 170 && (i32::from(grey[0]) - i32::from(grey[1])).abs() <= 2,
        "threshold 1.0 with no darkening greys the red: {grey:?}"
    );
    // ...and Darken 1.0 takes that grey to black.
    let black = red_eye_fix(1.0, 1.0);
    assert!(
        black[0] <= 1 && black[1] <= 1 && black[2] <= 1 && black[3] == 255,
        "darken 1.0 makes the corrected pupil black: {black:?}"
    );
    assert!(grey[0] > black[0]);
}

#[test]
fn patch_softness_reaches_the_tool_and_refuses_the_wrong_kind() {
    let mut tool = tools::edit::PatchTool::default();
    assert_eq!(tool.softness, 4.0);
    tool.set_setting("softness", ToolSetting::Float(20.0))
        .unwrap();
    assert_eq!(tool.softness, 20.0, "the registry's one Patch option lands");
    // Out of the registry range clamps; a NaN is refused, not stored.
    tool.set_setting("softness", ToolSetting::Float(500.0))
        .unwrap();
    assert_eq!(tool.softness, 64.0);
    assert!(tool
        .set_setting("softness", ToolSetting::Float(f32::NAN))
        .is_err());
    assert_eq!(tool.softness, 64.0);
    assert!(matches!(
        tool.set_setting("softness", ToolSetting::Choice(1)),
        Err(ToolError::OptionKindMismatch { .. })
    ));
    assert!(matches!(
        tool.set_setting("aligned", ToolSetting::Bool(true)),
        Err(ToolError::UnknownOption { .. })
    ));
    // The same through the registry's construction, as the shell reaches it.
    let mut made = registry::make(ToolId::Patch);
    assert!(made
        .set_setting("softness", ToolSetting::Float(9.0))
        .is_ok());
    let mut red_eye = registry::make(ToolId::RedEye);
    assert!(red_eye
        .set_setting("threshold", ToolSetting::Float(2.0))
        .is_ok());
    assert!(red_eye
        .set_setting("darken", ToolSetting::Float(0.9))
        .is_ok());
    assert!(matches!(
        red_eye.set_setting("darken", ToolSetting::Int(1)),
        Err(ToolError::OptionKindMismatch { .. })
    ));
}

// ---------------------------------------------------------------------------
// Free Transform: the mode choice (already wired; pinned for the set).
// ---------------------------------------------------------------------------

#[test]
fn free_transform_mode_choice_reaches_the_tool() {
    let mut tool = TransformTool::default();
    tool.set_setting("mode", ToolSetting::Choice(5)).unwrap();
    assert_eq!(tool.mode, TransformMode::Warp);
    tool.set_setting("mode", ToolSetting::Choice(1)).unwrap();
    assert_eq!(tool.mode, TransformMode::Rotate);
}
