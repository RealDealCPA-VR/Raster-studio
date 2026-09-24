//! W9-H: Photoshop style libraries (`.asl`) into the style presets, and the
//! style presets as the Layer Style dialog's Styles grid lists them.
//!
//! `asset_store::asl` reads the library's framing and each style's name;
//! the effects themselves are an action descriptor, which the `psd` crate
//! already decodes for a layer's `lfx2` block. So each style's `Lefx` item
//! is re-framed as an `lfx2` block and handed to [`psd::import_effects`]:
//! one decoder for both roads, and whatever it maps from a `.psd` it maps
//! from an `.asl`. The effects it cannot map are named back to the caller.

use std::path::Path;

use asset_store::asl::{parse_asl, AslStyle};
use asset_store::presets::PresetStore;
use layer_model::LayerEffects;

use crate::editor::Editor;

/// One style decoded from a library.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedStyle {
    pub name: String,
    pub effects: LayerEffects,
    /// The effects the style lists that this build does not map.
    pub unmapped: Vec<String>,
}

/// Decode every style of an `.asl` library. A malformed library is an
/// error naming why; a style whose effect descriptor cannot be read is
/// skipped and named in the error only when **no** style could be read.
pub fn styles_from_asl(bytes: &[u8]) -> Result<Vec<ImportedStyle>, String> {
    let library = parse_asl(bytes).map_err(|e| e.to_string())?;
    // The patterns the styles' pattern overlays name: the section holds
    // them in the same framing as a `.psd`'s `Patt` block.
    let opts = psd::ReadOptions::default();
    let mut patterns = psd::pattern::PatternLibrary::default();
    let mut budget = psd::limits::Budget::new(opts.max_decoded_bytes);
    patterns.read_block(&library.patterns, &opts, &mut budget);
    let mut out = Vec::new();
    let mut failed = Vec::new();
    for style in &library.styles {
        match decode_style(style, &patterns) {
            Some(s) => out.push(s),
            None => failed.push(style.name.clone()),
        }
    }
    if out.is_empty() && !failed.is_empty() {
        return Err(format!(
            "no style in the library could be read ({})",
            failed.join(", ")
        ));
    }
    Ok(out)
}

/// An effect block (`Lefx` descriptor) framed as a layer's `lfx2` block.
fn lfx2_block(lefx: &psd::Descriptor) -> Option<psd::Effects> {
    let mut sink = psd::bytes::Sink::new();
    sink.u32(0);
    sink.u32(16);
    lefx.write(&mut sink).ok()?;
    Some(psd::Effects {
        key: *b"lfx2",
        data: sink.into_inner(),
    })
}

/// One style's `Styl` descriptor → its `Lefx` item → the native effect
/// block, through the same `psd` decoder a `.psd` import uses (every
/// effect kind, contours and repeated effects included); the pattern
/// overlay resolves against the library's own patterns.
fn decode_style(
    style: &AslStyle,
    patterns: &psd::pattern::PatternLibrary,
) -> Option<ImportedStyle> {
    let opts = psd::ReadOptions::default();
    let mut cur = psd::bytes::Cursor::new(&style.style_descriptor);
    let _version = cur.u32().ok()?;
    let styl = psd::Descriptor::read(&mut cur, &opts).ok()?;
    let lefx = styl.descriptor("Lefx")?;
    let block = lfx2_block(lefx)?;
    // W11-B: `import_effects` runs `psd::effects::map_rest` on the block, so
    // the kinds past card 075's four (inner shadow, inner glow, bevel, satin,
    // gradient overlay, contours, repeated effects) map through the same one
    // mapping a `.psd` layer's `lfx2` block does.
    let imported = psd::import_effects(&block, &opts)?;
    let mut effects = imported.effects;
    let mut unmapped = imported.unmapped;
    photoshop::blend_options(&styl, &mut unmapped);
    if let Some(overlay) = psd::pattern::pattern_overlay(&block, &opts, patterns) {
        effects.pattern_overlay = Some(overlay);
        psd::effects::struck(&mut unmapped, "pattern overlay");
    }
    Some(ImportedStyle {
        name: style.name.clone(),
        effects,
        unmapped,
    })
}

/// W9-H: the part of a style `.asl` carries beyond its effect block. The
/// effect mapping itself lives in `psd::effects` (W11-B).
mod photoshop {
    use psd::{Descriptor, Value};

    fn num(d: &Descriptor, key: &str) -> Option<f32> {
        d.number(key).map(|v| v as f32).filter(|v| v.is_finite())
    }

    /// A style's `blendOptions` (Photoshop's Blending Options: fill
    /// opacity, opacity, blend mode, Blend If). A style preset here is an
    /// effect block only and none of them is applied, so each one that is
    /// set away from Photoshop's default is named as not imported: a style
    /// that ghosts its layer body at 0% fill says so instead of landing at
    /// 100% without a word.
    pub(super) fn blend_options(styl: &Descriptor, unmapped: &mut Vec<String>) {
        let Some(bo) = styl.descriptor("blendOptions") else {
            return;
        };
        for (key, value) in &bo.items {
            let what = match key.as_str() {
                "fillOpacity" | "Opct" => {
                    let label = if key == "Opct" {
                        "opacity"
                    } else {
                        "fill opacity"
                    };
                    match num(bo, key) {
                        Some(p) if (p - 100.0).abs() < 1.0e-3 => continue,
                        Some(p) => format!("blending options ({label} {p:.0}%)"),
                        None => format!("blending options ({label})"),
                    }
                }
                "Md  " => match value {
                    Value::Enumerated { value, .. } if value == "Nrml" => continue,
                    Value::Enumerated { value, .. } => {
                        format!("blending options (blend mode {})", value.trim())
                    }
                    _ => "blending options (blend mode)".to_string(),
                },
                "Blnd" => "blending options (Blend If)".to_string(),
                other => format!("blending options ({})", other.trim()),
            };
            if !unmapped.contains(&what) {
                unmapped.push(what);
            }
        }
    }
}

/// W9-H: the style presets as `(name, effect block)`, oldest first, for
/// the Layer Style dialog's Styles page. A preset whose JSON no longer
/// parses is left out rather than offered as an empty style.
pub fn style_presets(store: &PresetStore) -> Vec<(String, LayerEffects)> {
    store
        .styles()
        .iter()
        .filter_map(|(name, json)| {
            serde_json::from_str::<LayerEffects>(json)
                .ok()
                .map(|fx| (name.clone(), fx))
        })
        .collect()
}

impl Editor {
    /// W9-H: add every style of the `.asl` library at `path` to the style
    /// presets (a style of the same name is replaced, so re-importing a
    /// library updates it) and persist them. The message says how many
    /// landed and which effects did not map.
    pub fn import_style_library(&mut self, path: &Path) -> Result<String, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let styles = styles_from_asl(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut unmapped: Vec<String> = Vec::new();
        for style in &styles {
            let json = serde_json::to_string(&style.effects).map_err(|e| e.to_string())?;
            self.presets_mut().define_style(&style.name, json);
            for u in &style.unmapped {
                if !unmapped.contains(u) {
                    unmapped.push(u.clone());
                }
            }
        }
        let _ = self.presets().save(&self.paths().presets_file());
        let n = styles.len();
        let mut message = if n == 1 {
            "Loaded 1 style".to_string()
        } else {
            format!("Loaded {n} styles")
        };
        if !unmapped.is_empty() {
            message.push_str(&format!(" (not imported: {})", unmapped.join(", ")));
        }
        self.set_status(message.clone());
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::{BlendMode, ColorOverlayEffect, ShadowEffect};

    /// A `Styl` descriptor wrapping `effects` the way Photoshop writes a
    /// style: the effect block (written by the same `lfx2` writer a `.psd`
    /// export uses) under `Lefx`, from its version word on.
    fn styl_bytes(effects: &LayerEffects) -> Vec<u8> {
        let (lfx2, _) = psd::effects::export_effects(effects).expect("an exportable style");
        let mut cur = psd::bytes::Cursor::new(&lfx2);
        cur.u32().unwrap();
        cur.u32().unwrap();
        let lefx = psd::Descriptor::read(&mut cur, &psd::ReadOptions::default()).unwrap();
        let mut styl = psd::Descriptor::new("Styl");
        styl.push("Lefx", psd::Value::Descriptor(lefx)).unwrap();
        let mut sink = psd::bytes::Sink::new();
        sink.u32(16);
        styl.write(&mut sink).unwrap();
        sink.into_inner()
    }

    fn fixture_styles() -> (LayerEffects, LayerEffects) {
        let shadow = LayerEffects {
            drop_shadow: Some(ShadowEffect {
                blend_mode: BlendMode::Multiply,
                color: [0.0, 0.0, 0.0, 1.0],
                opacity: 0.5,
                angle_deg: 90.0,
                use_global_light: false,
                distance_px: 7.0,
                spread: 0.0,
                size_px: 4.0,
                noise: 0.0,
                knockout: true,
            }),
            ..Default::default()
        };
        let red = LayerEffects {
            color_overlay: Some(ColorOverlayEffect {
                blend_mode: BlendMode::Normal,
                color: [1.0, 0.0, 0.0, 1.0],
                opacity: 1.0,
            }),
            ..Default::default()
        };
        (shadow, red)
    }

    /// The fixture library: two styles, as an `.asl` file's bytes.
    fn fixture_asl() -> Vec<u8> {
        let (shadow, red) = fixture_styles();
        let (a, b) = (styl_bytes(&shadow), styl_bytes(&red));
        asset_store::asl::write_asl(&[("Soft Shadow", "s-1", &a), ("Red Fill", "s-2", &b)])
    }

    #[test]
    fn an_asl_fixture_imports_its_styles_with_their_effects() {
        let styles = styles_from_asl(&fixture_asl()).unwrap();
        let (shadow, red) = fixture_styles();
        assert_eq!(styles.len(), 2);
        assert_eq!(styles[0].name, "Soft Shadow");
        let got = styles[0].effects.drop_shadow.as_ref().expect("the shadow");
        let want = shadow.drop_shadow.as_ref().unwrap();
        assert_eq!(got.blend_mode, want.blend_mode);
        assert!((got.opacity - want.opacity).abs() < 1e-3);
        assert!((got.distance_px - want.distance_px).abs() < 1e-3);
        assert!((got.size_px - want.size_px).abs() < 1e-3);
        assert_eq!(styles[1].name, "Red Fill");
        assert_eq!(
            styles[1].effects.color_overlay.as_ref().map(|o| o.color),
            red.color_overlay.as_ref().map(|o| o.color)
        );
    }

    #[test]
    fn a_malformed_asl_is_an_error_not_a_panic() {
        assert!(styles_from_asl(b"").is_err());
        assert!(styles_from_asl(b"not a style library at all").is_err());
        let good = fixture_asl();
        let err = styles_from_asl(&good[..good.len() / 2]).unwrap_err();
        assert!(err.contains("ends inside"), "{err}");
        // A library whose one style carries garbage for its effects.
        let junk = asset_store::asl::write_asl(&[("Junk", "", &[0, 0, 0, 16, 0xFF, 0xFF])]);
        let err = styles_from_asl(&junk).unwrap_err();
        assert!(err.contains("Junk"), "{err}");
    }

    #[test]
    fn importing_a_library_adds_its_styles_to_the_presets_the_styles_grid_lists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.asl");
        std::fs::write(&path, fixture_asl()).unwrap();
        let mut ed = Editor::with_state(
            crate::prefs::AppPaths::rooted(dir.path().join("config")),
            crate::prefs::Preferences::default(),
            crate::recent::RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new()),
        );
        let message = ed.import_style_library(&path).unwrap();
        assert_eq!(message, "Loaded 2 styles");
        let listed = style_presets(ed.presets());
        let names: Vec<&str> = listed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Soft Shadow", "Red Fill"]);
        assert!(listed[0].1.drop_shadow.is_some());
        // Persisted with the preset store.
        let reloaded = PresetStore::load(&ed.paths().presets_file());
        assert_eq!(reloaded.styles().len(), 2);
        // And they feed the Layer Style dialog's Styles page.
        let dialog = ui::dialogs::LayerStyleDialog::new(
            layer_model::LayerId::new(),
            "Layer",
            LayerEffects::default(),
        )
        .with_styles(listed);
        assert_eq!(dialog.styles().len(), 2);
        // A malformed file changes nothing.
        let bad = dir.path().join("bad.asl");
        std::fs::write(&bad, b"8BSL?").unwrap();
        assert!(ed.import_style_library(&bad).is_err());
        assert_eq!(ed.presets().styles().len(), 2);
    }

    // ---- W9-H round 2: the routes a user actually takes ----------------

    fn screen(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn click_at(at: egui::Pos2) -> Vec<egui::Event> {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        vec![egui::Event::PointerMoved(at), button(true), button(false)]
    }

    fn enter() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// An editor over a white 8x8 picture whose File > Open picker answers
    /// `pick`.
    fn editor_picking(dir: &Path, pick: &Path) -> Editor {
        let png = dir.join("white.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 8, 8, &[255u8; 8 * 8 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            crate::prefs::AppPaths::rooted(dir.join("config")),
            crate::prefs::Preferences::default(),
            crate::recent::RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new().opening(pick)),
        );
        ed.open_path(&png).unwrap();
        ed
    }

    /// Click `action` in the menu bar, exactly as the bar's click handler
    /// does: the menu's own enablement, then the chrome's route (which asks
    /// the dialog host first).
    fn menu_click(
        ed: &mut Editor,
        action: ui::menu::MenuAction,
    ) -> (crate::chrome::Chrome, crate::chrome::ChromeOutput) {
        let mut chrome = crate::chrome::Chrome::new();
        let menu_ctx = crate::menu_bridge::context(ed, chrome.workspace());
        let intent = crate::menu_bridge::resolve_intent(action, &menu_ctx, ed)
            .unwrap_or_else(|reason| panic!("{action:?} is disabled: {reason}"));
        let mut out = crate::chrome::ChromeOutput::default();
        chrome.menu_click(intent, ed, &mut out);
        (chrome, out)
    }

    #[test]
    fn file_open_of_an_asl_adds_its_styles_and_apply_style_preset_offers_them_in_a_styles_grid() {
        let dir = tempfile::tempdir().unwrap();
        let asl = dir.path().join("library.asl");
        std::fs::write(&asl, fixture_asl()).unwrap();
        let mut ed = editor_picking(dir.path(), &asl);
        let docs_before = ed.documents().len();

        // File > Open, the `.asl` picked: its styles land in the presets and
        // no document opens (the image importer never sees the file).
        assert_eq!(
            ed.dispatch(crate::action::Action::Open),
            Ok(crate::editor::Effect::Tool)
        );
        assert_eq!(
            ed.documents().len(),
            docs_before,
            "a style library is no document"
        );
        let names: Vec<&str> = ed
            .presets()
            .styles()
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(names, ["Soft Shadow", "Red Fill"]);

        // Layer > Layer Style > Apply Style Preset opens the Layer Style
        // dialog on its Styles page, listing both.
        let (mut chrome, out) = menu_click(&mut ed, ui::menu::MenuAction::ApplyStylePreset);
        assert!(
            out.menu.is_empty(),
            "the dialog host answered: {:?}",
            out.menu
        );
        let host = chrome.dialogs_for_test();
        {
            let dialog = host.active_layer_style_dialog_for_test();
            assert_eq!(dialog.page(), ui::dialogs::layer_style::StylePage::Styles);
            let listed: Vec<&str> = dialog.styles().iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(listed, ["Soft Shadow", "Red Fill"]);
        }

        // Click the drawn "Red Fill" tile, then Enter: one command applies it.
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = crate::chrome::ChromeOutput::default();
        // A centred dialog takes a few frames to find its place: lay out
        // until the tile holds still, then press where it is drawn.
        let tile_id = ui::dialogs::layer_style::style_tile_id(1);
        let (mut previous, mut still) = (None, 0);
        for _ in 0..24 {
            let _ = ctx.run(screen(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
            let rect = ctx.read_response(tile_id).map(|r| r.rect);
            if rect.is_some() && rect == previous {
                still += 1;
                if still >= 3 {
                    break;
                }
            } else {
                still = 0;
            }
            previous = rect;
        }
        let tile = previous.expect("the Styles grid drew the second tile");
        let _ = ctx.run(screen(click_at(tile.center())), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        let _ = ctx.run(screen(vec![enter()]), |ctx| host.ui(ctx, None, &mut out));
        assert!(!host.is_open(), "Enter confirmed the dialog");
        assert_eq!(out.commands.len(), 1, "{:?}", out.commands);
        for command in out.commands {
            ed.apply_command(command);
        }
        let open = ed.active().unwrap();
        let layer = open
            .document
            .layers
            .get(open.document.active_layer().unwrap())
            .unwrap();
        let (_, red) = fixture_styles();
        assert_eq!(
            layer.effects.color_overlay.as_ref().map(|o| o.color),
            red.color_overlay.as_ref().map(|o| o.color),
            "the clicked style is the layer's style"
        );
        assert!(layer.effects.drop_shadow.is_none(), "not the other style");
    }

    #[test]
    fn file_open_of_a_malformed_asl_fails_and_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let asl = dir.path().join("broken.asl");
        let good = fixture_asl();
        std::fs::write(&asl, &good[..good.len() / 2]).unwrap();
        let mut ed = editor_picking(dir.path(), &asl);
        let docs_before = ed.documents().len();
        let err = ed.dispatch(crate::action::Action::Open).unwrap_err();
        assert!(
            matches!(err, crate::editor::ActionError::Failed { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("ends inside"), "{err}");
        assert!(ed.presets().styles().is_empty());
        assert_eq!(ed.documents().len(), docs_before);
    }

    // ---- W9-H round 2: a library in Photoshop's own layout ---------------
    //
    // Built by hand, not by `write_asl`: the name descriptor, the `Styl`
    // descriptor with `documentMode` before `Lefx`, each style padded to
    // four bytes, and a real pattern section ahead of the styles. The keys,
    // enum codes and units are the ones Photoshop-authored libraries carry
    // (checked against five of them; see
    // `photoshop_authored_libraries_import`).

    use layer_model::effects::{
        BevelDirection, BevelStyle, BevelTechnique, ContourPreset, GlowSource, GradientStyle,
    };
    use psd::{Descriptor, Value};

    fn objc(class: &str, items: Vec<(&str, Value)>) -> Descriptor {
        let mut d = Descriptor::new(class);
        for (k, v) in items {
            d.push(k, v).unwrap();
        }
        d
    }

    fn unit(code: &[u8; 4], value: f64) -> Value {
        Value::UnitFloat { unit: *code, value }
    }

    fn en(type_id: &str, value: &str) -> Value {
        Value::Enumerated {
            type_id: type_id.into(),
            value: value.into(),
        }
    }

    fn rgbc(r: f64, g: f64, b: f64) -> Value {
        Value::Descriptor(objc(
            "RGBC",
            vec![
                ("Rd  ", Value::Double(r)),
                ("Grn ", Value::Double(g)),
                ("Bl  ", Value::Double(b)),
            ],
        ))
    }

    fn shpc(name: &str, knots: &[(f64, f64)]) -> Value {
        let crv = knots
            .iter()
            .map(|(h, v)| {
                Value::Descriptor(objc(
                    "CrPt",
                    vec![("Hrzn", Value::Double(*h)), ("Vrtc", Value::Double(*v))],
                ))
            })
            .collect();
        Value::Descriptor(objc(
            "ShpC",
            vec![
                ("Nm  ", Value::Text(name.into())),
                ("Crv ", Value::List(crv)),
            ],
        ))
    }

    fn shadow_desc(class: &str, mode: &str, opacity: f64, distance: f64, contour: Value) -> Value {
        Value::Descriptor(objc(
            class,
            vec![
                ("enab", Value::Bool(true)),
                ("Md  ", en("BlnM", mode)),
                ("Clr ", rgbc(0.0, 0.0, 0.0)),
                ("Opct", unit(b"#Prc", opacity)),
                ("uglg", Value::Bool(false)),
                ("lagl", unit(b"#Ang", 120.0)),
                ("Dstn", unit(b"#Pxl", distance)),
                ("Ckmt", unit(b"#Pxl", 0.0)),
                ("blur", unit(b"#Pxl", 27.0)),
                ("Nose", unit(b"#Prc", 0.0)),
                ("AntA", Value::Bool(false)),
                ("TrnS", contour),
            ],
        ))
    }

    /// The `Lefx` of a style with every kind the `psd` importer leaves out.
    fn photoshop_lefx() -> Descriptor {
        let linear = || shpc("Linear", &[(0.0, 0.0), (255.0, 255.0)]);
        let grad = objc(
            "Grdn",
            vec![
                ("Nm  ", Value::Text("Two Color".into())),
                ("GrdF", en("GrdF", "CstS")),
                ("Intr", Value::Double(4096.0)),
                (
                    "Clrs",
                    Value::List(vec![
                        Value::Descriptor(objc(
                            "Clrt",
                            vec![
                                ("Clr ", rgbc(255.0, 0.0, 0.0)),
                                ("Type", en("Clry", "UsrS")),
                                ("Lctn", Value::Integer(0)),
                                ("Mdpn", Value::Integer(50)),
                            ],
                        )),
                        Value::Descriptor(objc(
                            "Clrt",
                            vec![
                                ("Clr ", rgbc(0.0, 0.0, 255.0)),
                                ("Type", en("Clry", "UsrS")),
                                ("Lctn", Value::Integer(4096)),
                                ("Mdpn", Value::Integer(50)),
                            ],
                        )),
                    ]),
                ),
                (
                    "Trns",
                    Value::List(vec![
                        Value::Descriptor(objc(
                            "TrnS",
                            vec![
                                ("Opct", unit(b"#Prc", 100.0)),
                                ("Lctn", Value::Integer(0)),
                                ("Mdpn", Value::Integer(50)),
                            ],
                        )),
                        Value::Descriptor(objc(
                            "TrnS",
                            vec![
                                ("Opct", unit(b"#Prc", 40.0)),
                                ("Lctn", Value::Integer(4096)),
                                ("Mdpn", Value::Integer(50)),
                            ],
                        )),
                    ]),
                ),
            ],
        );
        objc(
            "Lefx",
            vec![
                ("Scl ", unit(b"#Prc", 100.0)),
                ("masterFXSwitch", Value::Bool(true)),
                // Two drop shadows, as Photoshop CC writes repeated effects.
                (
                    "dropShadowMulti",
                    Value::List(vec![
                        shadow_desc("DrSh", "Mltp", 75.0, 5.0, shpc("Cone", &[])),
                        shadow_desc("DrSh", "Nrml", 30.0, 12.0, linear()),
                    ]),
                ),
                (
                    "IrSh",
                    shadow_desc(
                        "IrSh",
                        "Drkn",
                        28.0,
                        8.0,
                        shpc("Half Round", &[(0.0, 0.0), (128.0, 200.0), (255.0, 255.0)]),
                    ),
                ),
                (
                    "IrGl",
                    Value::Descriptor(objc(
                        "IrGl",
                        vec![
                            ("enab", Value::Bool(true)),
                            ("Md  ", en("BlnM", "Scrn")),
                            ("Clr ", rgbc(255.0, 255.0, 190.0)),
                            ("Opct", unit(b"#Prc", 55.0)),
                            ("GlwT", en("BETE", "SfBL")),
                            ("Ckmt", unit(b"#Pxl", 0.0)),
                            ("blur", unit(b"#Pxl", 12.0)),
                            ("Nose", unit(b"#Prc", 0.0)),
                            ("glwS", en("IGSr", "SrcC")),
                            ("TrnS", shpc("$$$/Contours/Defaults/Gaussian=Gaussian", &[])),
                        ],
                    )),
                ),
                (
                    "ebbl",
                    Value::Descriptor(objc(
                        "ebbl",
                        vec![
                            ("enab", Value::Bool(true)),
                            ("hglM", en("BlnM", "Scrn")),
                            ("hglC", rgbc(255.0, 255.0, 255.0)),
                            ("hglO", unit(b"#Prc", 75.0)),
                            ("sdwM", en("BlnM", "Mltp")),
                            ("sdwC", rgbc(0.0, 0.0, 0.0)),
                            ("sdwO", unit(b"#Prc", 60.0)),
                            ("bvlT", en("bvlT", "PrBL")),
                            ("bvlS", en("BESl", "Embs")),
                            ("uglg", Value::Bool(true)),
                            ("lagl", unit(b"#Ang", 120.0)),
                            ("Lald", unit(b"#Ang", 30.0)),
                            ("srgR", unit(b"#Prc", 83.0)),
                            ("blur", unit(b"#Pxl", 49.0)),
                            ("bvlD", en("BESs", "Out ")),
                            ("TrnS", shpc("Ring", &[])),
                            ("Sftn", unit(b"#Pxl", 2.0)),
                        ],
                    )),
                ),
                (
                    "ChFX",
                    Value::Descriptor(objc(
                        "ChFX",
                        vec![
                            ("enab", Value::Bool(true)),
                            ("Md  ", en("BlnM", "Mltp")),
                            ("Clr ", rgbc(0.0, 0.0, 0.0)),
                            ("Invr", Value::Bool(true)),
                            ("Opct", unit(b"#Prc", 68.0)),
                            ("lagl", unit(b"#Ang", 19.0)),
                            ("Dstn", unit(b"#Pxl", 11.0)),
                            ("blur", unit(b"#Pxl", 14.0)),
                        ],
                    )),
                ),
                (
                    "GrFl",
                    Value::Descriptor(objc(
                        "GrFl",
                        vec![
                            ("enab", Value::Bool(true)),
                            ("Md  ", en("BlnM", "Ovrl")),
                            ("Opct", unit(b"#Prc", 80.0)),
                            ("Grad", Value::Descriptor(grad)),
                            ("Angl", unit(b"#Ang", 90.0)),
                            ("Type", en("GrdT", "Rdl ")),
                            ("Rvrs", Value::Bool(true)),
                            ("Algn", Value::Bool(true)),
                            ("Scl ", unit(b"#Prc", 150.0)),
                        ],
                    )),
                ),
                (
                    "patternFill",
                    Value::Descriptor(objc(
                        "patternFill",
                        vec![
                            ("enab", Value::Bool(true)),
                            ("Md  ", en("BlnM", "Nrml")),
                            ("Opct", unit(b"#Prc", 100.0)),
                            (
                                "Ptrn",
                                Value::Descriptor(objc(
                                    "Ptrn",
                                    vec![
                                        ("Nm  ", Value::Text("Checks".into())),
                                        ("Idnt", Value::Text("ptrn-checks".into())),
                                    ],
                                )),
                            ),
                            ("Scl ", unit(b"#Prc", 100.0)),
                            ("Algn", Value::Bool(true)),
                        ],
                    )),
                ),
                // An effect switched off in the file is not drawn.
                (
                    "OrGl",
                    Value::Descriptor(objc("OrGl", vec![("enab", Value::Bool(false))])),
                ),
            ],
        )
    }

    /// Frame styles the way Photoshop does, with `patterns` (a `Patt`-style
    /// run) as the pattern section.
    fn photoshop_library(styles: &[(&str, Descriptor)], patterns: &[u8]) -> Vec<u8> {
        let styles: Vec<(&str, Descriptor, Option<Descriptor>)> =
            styles.iter().map(|(n, l)| (*n, l.clone(), None)).collect();
        photoshop_library_with_blending(&styles, patterns)
    }

    /// As [`photoshop_library`], each style optionally carrying a
    /// `blendOptions` object after its `Lefx`, where Photoshop puts it.
    fn photoshop_library_with_blending(
        styles: &[(&str, Descriptor, Option<Descriptor>)],
        patterns: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(b"8BSL");
        out.extend_from_slice(&3u16.to_be_bytes());
        out.extend_from_slice(&(patterns.len() as u32).to_be_bytes());
        out.extend_from_slice(patterns);
        out.extend_from_slice(&(styles.len() as u32).to_be_bytes());
        for (i, (name, lefx, blending)) in styles.iter().enumerate() {
            let naming = objc(
                "null",
                vec![
                    ("Nm  ", Value::Text((*name).into())),
                    ("Idnt", Value::Text(format!("style-{i}"))),
                ],
            );
            let mut styl = objc(
                "Styl",
                vec![
                    (
                        "documentMode",
                        Value::Descriptor(Descriptor::new("documentMode")),
                    ),
                    ("Lefx", Value::Descriptor(lefx.clone())),
                ],
            );
            if let Some(b) = blending {
                styl.push("blendOptions", Value::Descriptor(b.clone()))
                    .unwrap();
            }
            let mut sink = psd::bytes::Sink::new();
            sink.u32(16);
            naming.write(&mut sink).unwrap();
            sink.u32(16);
            styl.write(&mut sink).unwrap();
            let mut body = sink.into_inner();
            while !body.len().is_multiple_of(4) {
                body.push(0);
            }
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
        }
        out
    }

    fn checks_pattern() -> Vec<u8> {
        let mut rgba8 = Vec::new();
        for i in 0..4 {
            let v = if i % 3 == 0 { 255 } else { 0 };
            rgba8.extend_from_slice(&[v, v, v, 255]);
        }
        psd::pattern::encode_block(&[psd::pattern::PsdPattern {
            name: "Checks".into(),
            id: "ptrn-checks".into(),
            width: 2,
            height: 2,
            rgba8,
        }])
    }

    #[test]
    fn a_library_in_photoshops_layout_maps_every_kind_it_carries() {
        let plain = objc(
            "Lefx",
            vec![
                ("Scl ", unit(b"#Prc", 100.0)),
                ("masterFXSwitch", Value::Bool(true)),
                (
                    "SoFi",
                    Value::Descriptor(objc(
                        "SoFi",
                        vec![
                            ("enab", Value::Bool(true)),
                            ("Md  ", en("BlnM", "Nrml")),
                            ("Opct", unit(b"#Prc", 100.0)),
                            ("Clr ", rgbc(0.0, 128.0, 255.0)),
                        ],
                    )),
                ),
            ],
        );
        let bytes = photoshop_library(
            &[("Everything", photoshop_lefx()), ("Plain", plain)],
            &checks_pattern(),
        );
        let styles = styles_from_asl(&bytes).unwrap();
        assert_eq!(
            styles.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Everything", "Plain"]
        );
        let s = &styles[0];
        assert!(s.unmapped.is_empty(), "not imported: {:?}", s.unmapped);
        let fx = &s.effects;

        // Two drop shadows: the first is the primary slot, the second an
        // extra instance; each keeps its own contour.
        let ds = fx.drop_shadow.as_ref().expect("the first drop shadow");
        assert_eq!(ds.blend_mode, BlendMode::Multiply);
        assert!((ds.opacity - 0.75).abs() < 1e-4 && (ds.distance_px - 5.0).abs() < 1e-4);
        assert_eq!(fx.extras.contours.drop_shadow.preset, ContourPreset::Cone);
        assert_eq!(fx.extras.drop_shadows.len(), 1, "the second instance");
        let second = &fx.extras.drop_shadows[0];
        assert_eq!(second.effect.blend_mode, BlendMode::Normal);
        assert!((second.effect.distance_px - 12.0).abs() < 1e-4);
        assert_eq!(second.contour.preset, ContourPreset::Linear);

        // Inner shadow, with a curve no preset names kept as its knots.
        let is = fx.inner_shadow.as_ref().expect("the inner shadow");
        assert_eq!(is.blend_mode, BlendMode::Darken);
        assert!((is.opacity - 0.28).abs() < 1e-4 && (is.distance_px - 8.0).abs() < 1e-4);
        let c = &fx.extras.contours.inner_shadow;
        assert_eq!(c.preset, ContourPreset::Custom);
        assert_eq!(c.points.len(), 3);
        assert!((c.points[1][0] - 128.0 / 255.0).abs() < 1e-4);
        assert!((c.points[1][1] - 200.0 / 255.0).abs() < 1e-4);

        // Inner glow from the centre, its contour named by a localisation key.
        let ig = fx.inner_glow.as_ref().expect("the inner glow");
        assert_eq!(ig.blend_mode, BlendMode::Screen);
        assert_eq!(ig.source, GlowSource::Center);
        assert!((ig.size_px - 12.0).abs() < 1e-4);
        assert_eq!(
            fx.extras.contours.inner_glow.preset,
            ContourPreset::Gaussian
        );

        let bev = fx.bevel_emboss.as_ref().expect("the bevel");
        assert_eq!(bev.style, BevelStyle::Emboss);
        assert_eq!(bev.technique, BevelTechnique::ChiselHard);
        assert_eq!(bev.direction, BevelDirection::Down);
        assert!((bev.depth - 0.83).abs() < 1e-4);
        assert!((bev.size_px - 49.0).abs() < 1e-4 && (bev.soften_px - 2.0).abs() < 1e-4);
        assert!((bev.altitude_deg - 30.0).abs() < 1e-4);
        assert!((bev.shadow_opacity - 0.6).abs() < 1e-4);
        assert_eq!(bev.highlight_mode, BlendMode::Screen);
        assert_eq!(fx.extras.contours.bevel.preset, ContourPreset::Ring);

        let satin = fx.satin.as_ref().expect("the satin");
        assert!(satin.invert);
        assert!((satin.opacity - 0.68).abs() < 1e-4);
        assert!((satin.angle_deg - 19.0).abs() < 1e-4 && (satin.size_px - 14.0).abs() < 1e-4);

        let go = fx.gradient_overlay.as_ref().expect("the gradient overlay");
        assert_eq!(go.blend_mode, BlendMode::Overlay);
        assert_eq!(go.style, GradientStyle::Radial);
        assert!(go.reverse);
        assert!((go.scale - 1.5).abs() < 1e-4);
        assert_eq!(go.gradient.stops.len(), 2);
        assert_eq!(go.gradient.stops[0].color, [1.0, 0.0, 0.0, 1.0]);
        assert!((go.gradient.stops[1].position - 1.0).abs() < 1e-4);
        assert!((go.gradient.alpha_stops[1].color[3] - 0.4).abs() < 1e-4);

        // The pattern overlay resolves against the library's own patterns.
        let po = fx.pattern_overlay.as_ref().expect("the pattern overlay");
        let tile = po
            .pattern
            .tile
            .as_ref()
            .expect("its tile, from the section");
        assert_eq!((tile.width(), tile.height()), (2, 2));

        // The glow switched off in the file is not drawn.
        assert!(fx.outer_glow.is_none());

        // The plain style after the padded first one still reads.
        assert_eq!(
            styles[1].effects.color_overlay.as_ref().map(|o| o.color),
            Some([0.0, 128.0 / 255.0, 1.0, 1.0])
        );
    }

    /// Round 3: a style's Blending Options are not part of a style preset
    /// here, so the ones set away from the default are named, in the
    /// style's `unmapped` and in the File > Open status line; an empty or
    /// all-default `blendOptions` (as `multiple_styles.asl` carries) names
    /// nothing.
    #[test]
    fn a_styles_blending_options_are_named_as_not_imported() {
        let lefx = || {
            objc(
                "Lefx",
                vec![
                    ("Scl ", unit(b"#Prc", 100.0)),
                    (
                        "DrSh",
                        shadow_desc(
                            "DrSh",
                            "Mltp",
                            75.0,
                            5.0,
                            shpc("Linear", &[(0.0, 0.0), (255.0, 255.0)]),
                        ),
                    ),
                ],
            )
        };
        // As freebie.asl's "Freebie 5": fill opacity 0%.
        let ghost = objc("blendOptions", vec![("fillOpacity", unit(b"#Prc", 0.0))]);
        let defaults = objc(
            "blendOptions",
            vec![
                ("fillOpacity", unit(b"#Prc", 100.0)),
                ("Opct", unit(b"#Prc", 100.0)),
                ("Md  ", en("BlnM", "Nrml")),
            ],
        );
        let screened = objc(
            "blendOptions",
            vec![
                ("Md  ", en("BlnM", "Scrn")),
                ("Opct", unit(b"#Prc", 40.0)),
                ("Blnd", Value::List(vec![])),
                ("knockout", en("KnkO", "shal")),
            ],
        );
        let bytes = photoshop_library_with_blending(
            &[
                ("Ghost", lefx(), Some(ghost)),
                ("Empty", lefx(), Some(Descriptor::new("blendOptions"))),
                ("Defaults", lefx(), Some(defaults)),
                ("Screened", lefx(), Some(screened)),
                ("None", lefx(), None),
            ],
            &[],
        );
        let styles = styles_from_asl(&bytes).unwrap();
        let unmapped: Vec<(&str, Vec<&str>)> = styles
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.unmapped.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            unmapped,
            [
                ("Ghost", vec!["blending options (fill opacity 0%)"]),
                ("Empty", vec![]),
                ("Defaults", vec![]),
                (
                    "Screened",
                    vec![
                        "blending options (blend mode Scrn)",
                        "blending options (opacity 40%)",
                        "blending options (Blend If)",
                        "blending options (knockout)"
                    ]
                ),
                ("None", vec![]),
            ]
        );
        // The effects still import.
        assert!(styles.iter().all(|s| s.effects.drop_shadow.is_some()));

        // And the File > Open road's status line says it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ghost.asl");
        std::fs::write(&path, &bytes).unwrap();
        let mut ed = Editor::with_state(
            crate::prefs::AppPaths::rooted(dir.path().join("config")),
            crate::prefs::Preferences::default(),
            crate::recent::RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new()),
        );
        let message = ed.import_style_library(&path).unwrap();
        assert!(
            message.contains("not imported: blending options (fill opacity 0%)"),
            "{message}"
        );
    }

    /// The same importer over Photoshop-authored libraries. They are not in
    /// this repository (no licence to redistribute them); point
    /// `RASTER_STUDIO_ASL_DIR` at a folder of them - Krita's
    /// `sdk/tests/data/asl` has five - and run with `--ignored`.
    #[test]
    #[ignore = "needs RASTER_STUDIO_ASL_DIR: a folder of Photoshop-authored .asl files"]
    fn photoshop_authored_libraries_import() {
        let dir = std::env::var("RASTER_STUDIO_ASL_DIR").expect("RASTER_STUDIO_ASL_DIR");
        let mut seen = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !crate::dialogs::is_style_library_path(&path) {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let styles =
                styles_from_asl(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(!styles.is_empty(), "{}", path.display());
            for s in &styles {
                eprintln!(
                    "{}: {:?} not imported: {:?}",
                    path.display(),
                    s.name,
                    s.unmapped
                );
            }
            seen += 1;
        }
        assert!(seen > 0, "no .asl file in RASTER_STUDIO_ASL_DIR");
    }
}
