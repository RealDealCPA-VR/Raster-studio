//! W16-C: every float option on the options bar is shown in Photopea's unit
//! (Tolerance 0-255, Opacity / Flow / Hardness / Exposure / Spacing in %,
//! sizes in px, the tip angle in degrees), and the Paint Bucket's Fill
//! source is an option the tool honours.

use super::*;
use crate::bucket::{FillSource, FILL_SOURCE_KEY};
use crate::tool::ToolSetting;

/// Every (tool, float option) pair the registry declares.
fn floats() -> Vec<(ToolId, OptionSpec, FloatDisplay)> {
    let mut out = Vec::new();
    for tool in all() {
        for spec in tool.options {
            if let Some(display) = spec.float_display() {
                out.push((tool.id, *spec, display));
            }
        }
    }
    out
}

fn display_of(tool: ToolId, key: &str) -> FloatDisplay {
    info(tool)
        .and_then(|i| i.options.iter().find(|s| s.key == key))
        .and_then(OptionSpec::float_display)
        .unwrap_or_else(|| panic!("{tool:?} has no float option {key:?}"))
}

/// The finding itself: no float stored as a 0-1 fraction reaches the bar as
/// a raw fraction. Each one is either a 0-255 level (Tolerance) or a
/// percentage, so typing Photopea's numbers cannot select everything.
#[test]
fn no_float_option_is_shown_as_a_raw_fraction() {
    let mut raw = Vec::new();
    for (tool, spec, display) in floats() {
        let OptionKind::Float { min, max, .. } = spec.kind else {
            unreachable!()
        };
        if min >= 0.0 && max <= 1.0 {
            let ok = (spec.key == "tolerance" && display.scale == 255.0)
                || (display.unit == FloatUnit::Percent && display.scale == 100.0);
            if !ok {
                raw.push(format!("{tool:?}.{} {display:?}", spec.key));
            }
        }
    }
    assert!(raw.is_empty(), "raw 0-1 fractions on the bar: {raw:#?}");
}

#[test]
fn tolerance_is_a_level_from_0_to_255_on_every_tool_that_has_one() {
    let mut seen = 0;
    for (tool, spec, display) in floats() {
        if spec.key != "tolerance" {
            continue;
        }
        seen += 1;
        assert_eq!(display.scale, 255.0, "{tool:?}");
        assert_eq!(display.unit, FloatUnit::Plain, "{tool:?}");
        assert_eq!(display.decimals, 0, "{tool:?}");
    }
    assert!(seen >= 4, "the wand, bucket and erasers carry a Tolerance");
    let bucket = display_of(ToolId::PaintBucket, "tolerance");
    // Typing 32 stores 32/255 and the default 32/255 shows 32.
    assert_eq!(bucket.stored(32.0), 32.0 / 255.0);
    assert_eq!(bucket.shown(32.0 / 255.0).round(), 32.0);
}

#[test]
fn opacity_flow_hardness_exposure_and_spacing_are_percentages() {
    for (tool, key) in [
        (ToolId::Brush, "opacity"),
        (ToolId::Brush, "flow"),
        (ToolId::Brush, "hardness"),
        (ToolId::Brush, "spacing"),
        (ToolId::Brush, "smoothing"),
        (ToolId::Brush, "roundness"),
        (ToolId::PaintBucket, "opacity"),
        (ToolId::Gradient, "opacity"),
        (ToolId::Dodge, "exposure"),
    ] {
        let d = display_of(tool, key);
        assert_eq!(d.unit, FloatUnit::Percent, "{tool:?}.{key}");
        assert_eq!(d.scale, 100.0, "{tool:?}.{key}");
    }
    assert_eq!(display_of(ToolId::Brush, "opacity").shown(1.0), 100.0);
    assert_eq!(display_of(ToolId::Brush, "spacing").shown(0.25), 25.0);
}

#[test]
fn sizes_are_pixels_and_the_tip_angle_is_degrees() {
    for (tool, key) in [
        (ToolId::Brush, "size"),
        (ToolId::Eraser, "size"),
        (ToolId::RectMarquee, "feather"),
        (ToolId::Crop, "width"),
    ] {
        let d = display_of(tool, key);
        assert_eq!(d.unit, FloatUnit::Pixels, "{tool:?}.{key}");
        assert_eq!(d.scale, 1.0, "{tool:?}.{key}");
    }
    let angle = display_of(ToolId::Brush, "angle");
    assert_eq!(angle.unit, FloatUnit::Degrees);
    assert!((angle.shown(std::f32::consts::FRAC_PI_2) - 90.0).abs() < 1e-3);
    // Free Transform's angle is already stored in degrees: not scaled again.
    let t = display_of(ToolId::FreeTransform, crate::transform::keys::ANGLE);
    assert_eq!((t.unit, t.scale), (FloatUnit::Degrees, 1.0));
}

#[test]
fn the_paint_bucket_offers_foreground_or_pattern_and_the_tool_takes_it() {
    let spec = info(ToolId::PaintBucket)
        .unwrap()
        .options
        .iter()
        .find(|s| s.key == FILL_SOURCE_KEY)
        .expect("the bucket's options carry a Fill source");
    assert_eq!(
        spec.kind,
        OptionKind::Choice {
            choices: &["Foreground", "Pattern"],
            default: 0,
        }
    );
    let mut tool = crate::bucket::PaintBucketTool::default();
    tool.set_setting(FILL_SOURCE_KEY, ToolSetting::Choice(1))
        .unwrap();
    assert_eq!(tool.content, FillSource::Pattern.content());
    assert_eq!(
        tool.id(),
        ToolId::PaintBucket,
        "a bucket filling a pattern is still the Paint Bucket"
    );
    tool.set_setting(FILL_SOURCE_KEY, ToolSetting::Choice(0))
        .unwrap();
    assert_eq!(tool.content, crate::bucket::FillContent::Foreground);
    assert!(tool
        .set_setting(FILL_SOURCE_KEY, ToolSetting::Bool(true))
        .is_err());
}
