//! W11-I: the last three Photopea tool options — Dodge/Burn "Protect Tones",
//! Sponge "Vibrance" and the Gradient's "Transparency" toggle. Each test
//! builds the tool the way the app does (`registry::make`), reaches the
//! option through the key the options bar sends (declared by the registry),
//! and asserts on the pixels the gesture committed.

use raster::PixelRect;
use tools::gradient::{ColorStop, GradientRamp, OpacityStop, GRADIENT_TRANSPARENCY_KEY};
use tools::registry::{self, OptionKind};
use tools::stroke::{PROTECT_TONES_KEY, VIBRANCE_KEY};
use tools::tool::{PointerEvent, ToolContext, ToolId, ToolSetting};

// Shared with the other suites; this one uses only part of it.
#[allow(dead_code)]
mod common;
use common::{fixture, stroke, BLACK};

/// The registry declares `key` for `id` as a Bool defaulting to `default`:
/// the options bar draws its checkbox from exactly this row.
fn declared_bool(id: ToolId, key: &str) -> bool {
    let info = registry::info(id).expect("registered tool");
    let spec = info
        .options
        .iter()
        .find(|o| o.key == key)
        .unwrap_or_else(|| panic!("{id:?} does not declare {key}"));
    match spec.kind {
        OptionKind::Bool { default } => default,
        ref other => panic!("{key} is not a Bool: {other:?}"),
    }
}

/// Encoded HSL saturation and lightness of an sRGB8 pixel.
fn sat_light(p: [u8; 4]) -> (f32, f32) {
    let [_, s, l] = color::rgb_to_hsl([
        p[0] as f32 / 255.0,
        p[1] as f32 / 255.0,
        p[2] as f32 / 255.0,
    ]);
    (s, l)
}

/// One stroke of the registry-built `id` over a canvas of `base`, with
/// `key` set to `on`; the pixel at the dab's centre afterwards.
fn retouch(id: ToolId, base: [u8; 4], setup: &[(&str, ToolSetting)]) -> [u8; 4] {
    let mut fx = fixture(64, 64);
    fx.paint_rect(PixelRect::new(0, 0, 64, 64), base);
    let mut tool = registry::make(id);
    tool.set_setting("size", ToolSetting::Float(40.0)).unwrap();
    for (k, v) in setup {
        tool.set_setting(k, *v).unwrap();
    }
    let path = [(32.0, 32.0, 1.0), (32.0, 32.0, 1.0), (32.0, 32.0, 1.0)];
    let cmds = stroke(
        &mut fx,
        tool.as_mut(),
        &path,
        BLACK,
        editor_core::Selection::None,
    );
    fx.commit(cmds);
    fx.pixel(32, 32)
}

#[test]
fn dodge_and_burn_protect_tones_keeps_the_saturation_and_moves_only_lightness() {
    assert!(declared_bool(ToolId::Dodge, PROTECT_TONES_KEY));
    assert!(declared_bool(ToolId::Burn, PROTECT_TONES_KEY));
    let base = [180, 60, 60, 255];
    let (s0, l0) = sat_light(base);
    for id in [ToolId::Dodge, ToolId::Burn] {
        let exposure = ("exposure", ToolSetting::Float(0.8));
        let protected = retouch(
            id,
            base,
            &[exposure, (PROTECT_TONES_KEY, ToolSetting::Bool(true))],
        );
        let plain = retouch(
            id,
            base,
            &[exposure, (PROTECT_TONES_KEY, ToolSetting::Bool(false))],
        );
        let (sp, lp) = sat_light(protected);
        let (sn, ln) = sat_light(plain);
        // Both move the lightness the tool's way.
        if id == ToolId::Dodge {
            assert!(
                lp > l0 + 0.05 && ln > l0 + 0.05,
                "{id:?}: {lp} {ln} vs {l0}"
            );
        } else {
            assert!(
                lp < l0 - 0.05 && ln < l0 - 0.05,
                "{id:?}: {lp} {ln} vs {l0}"
            );
        }
        // Protected: saturation within a couple of 8-bit levels of the
        // original. Unprotected: visibly washed out (dodge) or changed.
        assert!(
            (sp - s0).abs() < 0.03,
            "{id:?} with Protect Tones moved saturation {s0} -> {sp} ({protected:?})"
        );
        assert!(
            (sn - s0).abs() > 0.05,
            "{id:?} without Protect Tones kept saturation {s0} -> {sn} ({plain:?}); \
             the two settings must differ"
        );
    }
}

#[test]
fn sponge_vibrance_spares_an_already_saturated_colour() {
    assert!(declared_bool(ToolId::Sponge, VIBRANCE_KEY));
    let saturate = ("mode", ToolSetting::Choice(1));
    let flow = ("amount", ToolSetting::Float(1.0));
    // A vivid colour: Vibrance boosts it far less than a plain Saturate.
    let vivid = [200, 70, 70, 255];
    let (s0, _) = sat_light(vivid);
    let with = retouch(
        ToolId::Sponge,
        vivid,
        &[saturate, flow, (VIBRANCE_KEY, ToolSetting::Bool(true))],
    );
    let without = retouch(
        ToolId::Sponge,
        vivid,
        &[saturate, flow, (VIBRANCE_KEY, ToolSetting::Bool(false))],
    );
    let (sw, _) = sat_light(with);
    let (so, _) = sat_light(without);
    assert!(so > s0 + 0.05, "plain Saturate did nothing: {s0} -> {so}");
    assert!(
        sw - s0 < (so - s0) * 0.6,
        "Vibrance did not spare the vivid colour: +{} vs +{}",
        sw - s0,
        so - s0
    );
    // A dull colour still gains under Vibrance.
    let dull = [140, 120, 120, 255];
    let (d0, _) = sat_light(dull);
    let dull_with = retouch(
        ToolId::Sponge,
        dull,
        &[saturate, flow, (VIBRANCE_KEY, ToolSetting::Bool(true))],
    );
    assert!(
        sat_light(dull_with).0 > d0 + 0.05,
        "Vibrance left a dull colour alone"
    );
}

/// Drag the registry-built Gradient across a 64x8 canvas with a ramp that
/// fades from opaque to fully transparent; the committed pixels at both ends.
fn gradient_ends(transparency: Option<bool>) -> ([u8; 4], [u8; 4]) {
    let mut fx = fixture(64, 8);
    let mut tool = registry::make(ToolId::Gradient);
    tool.set_setting("dither", ToolSetting::Bool(false))
        .unwrap();
    if let Some(on) = transparency {
        tool.set_setting(GRADIENT_TRANSPARENCY_KEY, ToolSetting::Bool(on))
            .unwrap();
    }
    let layer = fx.layer;
    let canvas = fx.canvas();
    let cmds = {
        let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
        ctx.ramp = GradientRamp::new(
            vec![
                ColorStop {
                    position: 0.0,
                    color: [1.0, 0.0, 0.0],
                },
                ColorStop {
                    position: 1.0,
                    color: [0.0, 0.0, 1.0],
                },
            ],
            vec![
                OpacityStop {
                    position: 0.0,
                    opacity: 1.0,
                },
                OpacityStop {
                    position: 1.0,
                    opacity: 0.0,
                },
            ],
        )
        .unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(0.0, 4.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(64.0, 4.0))
            .unwrap();
        ctx.drain()
    };
    fx.commit(cmds);
    (fx.pixel(1, 4), fx.pixel(62, 4))
}

#[test]
fn gradient_transparency_off_ignores_the_ramps_opacity_stops() {
    assert!(declared_bool(ToolId::Gradient, GRADIENT_TRANSPARENCY_KEY));
    // The default (on) and explicit on: the ramp fades out.
    for setting in [None, Some(true)] {
        let (_, right) = gradient_ends(setting);
        assert!(
            right[3] < 20,
            "{setting:?}: the fade did not apply: {right:?}"
        );
    }
    // Off: the same ramp is opaque end to end, colours unchanged.
    let (left, right) = gradient_ends(Some(false));
    assert_eq!(left[3], 255, "{left:?}");
    assert_eq!(
        right[3], 255,
        "the opacity stops were not ignored: {right:?}"
    );
    assert!(left[0] > 200 && right[2] > 200, "{left:?} {right:?}");
}
