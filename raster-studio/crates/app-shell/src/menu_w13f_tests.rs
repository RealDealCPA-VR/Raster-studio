//! W13-F through the menu route: each row found in the menu bar, resolved
//! against the live context, clicked through the chrome's own menu handler
//! (and, for the three dialogs, confirmed in the chrome's own dialog host)
//! and applied the way the shell applies a frame's output. The Slice
//! tool's button and Pattern Preview are driven through whole chrome
//! frames.

use std::path::Path;

use super::*;
use crate::action::Action;
use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use ui::menu::{Entry, Menu};
use ui::Workspace;

fn editor(dir: &Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

/// An opaque test card: a gradient, hard black/white edges, a dark corner
/// and deterministic noise — the content a decomposition finds hardest.
fn card(w: u32, h: u32) -> Vec<u8> {
    let mut seed = 0x2545_f491_u32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let px = if x < w / 4 && y < h / 4 {
                // The dark corner: codes 0..24, where linear light is finest.
                [(x * 3) as u8, (y * 2) as u8, ((x + y) % 25) as u8]
            } else if (x / 6 + y / 6) % 2 == 0 && y > h / 2 {
                // A checkerboard of pure black and white.
                if (x / 3) % 2 == 0 {
                    [0, 0, 0]
                } else {
                    [255, 255, 255]
                }
            } else {
                let n = noise();
                [
                    ((x * 255) / w) as u8 ^ (n as u8 & 0x1f),
                    ((y * 255) / h) as u8,
                    (n >> 8) as u8,
                ]
            };
            rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    rgba
}

fn opened(dir: &Path, w: u32, h: u32, rgba: &[u8]) -> Editor {
    let mut ed = editor(dir);
    let png = dir.join("card.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, w, h, rgba).unwrap(),
    )
    .unwrap();
    ed.open_path(&png).unwrap();
    ed
}

/// Every action `menu` offers, submenus included.
fn offers(menus: &[Menu], title: &str, action: MenuAction) -> bool {
    fn walk(entries: &[Entry], action: MenuAction) -> bool {
        entries.iter().any(|e| match e {
            Entry::Item(a) => *a == action,
            Entry::Separator => false,
            Entry::Submenu { entries, .. } => walk(entries, action),
        })
    }
    menus
        .iter()
        .any(|m| m.title == title && walk(&m.entries, action))
}

fn apply(ed: &mut Editor, out: ChromeOutput) -> Result<(), String> {
    for command in out.commands {
        ed.apply_command(command);
    }
    for picked in out.menu {
        crate::menu_bridge::perform(picked, ed)?;
    }
    for a in out.actions {
        ed.dispatch(a).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Click `action` in `title`'s menu through the chrome's click handler and
/// hand back the chrome and what the click produced. Panics when the row is
/// not in that menu or is disabled.
fn press(ed: &mut Editor, title: &str, action: MenuAction) -> (Chrome, ChromeOutput) {
    assert!(
        offers(&crate::menu_bridge::menus(ed), title, action),
        "{action:?} is not in the {title} menu"
    );
    let mut chrome = Chrome::new();
    let menu_ctx = crate::menu_bridge::context(ed, chrome.workspace());
    let intent = crate::menu_bridge::resolve_intent(action, &menu_ctx, ed)
        .unwrap_or_else(|reason| panic!("{action:?} is disabled: {reason}"));
    let mut out = ChromeOutput::default();
    chrome.menu_click(intent, ed, &mut out);
    (chrome, out)
}

/// A row with no dialog: click it and apply what it produced.
fn click(ed: &mut Editor, title: &str, action: MenuAction) -> Result<(), String> {
    let (chrome, out) = press(ed, title, action);
    assert!(!chrome.dialog_open(), "{action:?} opened a dialog");
    apply(ed, out)
}

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    }
}

/// Enter in the chrome's open dialog, through the host's own `ui`: a settle
/// frame, then the key. Returns what that produced.
fn enter(chrome: &mut Chrome) -> ChromeOutput {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let mut out = ChromeOutput::default();
    let host = chrome.dialogs_for_test();
    let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
    let key = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    };
    let _ = ctx.run(raw_input(vec![key]), |ctx| host.ui(ctx, None, &mut out));
    out
}

/// Click a dialog row, let `set` fill the dialog in, press Enter and apply.
fn through_dialog(
    ed: &mut Editor,
    title: &str,
    action: MenuAction,
    set: impl FnOnce(&mut W13fDialog),
) -> Result<(), String> {
    let (mut chrome, out) = press(ed, title, action);
    assert!(chrome.dialog_open(), "{action:?} asks first");
    assert!(out.is_empty(), "opening {action:?} edits nothing");
    set(chrome.dialogs_for_test().active_w13f_for_test());
    let out = enter(&mut chrome);
    assert!(!chrome.dialog_open(), "Enter confirms {action:?}");
    assert_eq!(out.menu, vec![action]);
    apply(ed, out)
}

fn convert_spec(target: ProfileChoice, intent: RenderingIntent) -> impl FnOnce(&mut W13fDialog) {
    move |d| match d {
        W13fDialog::Convert(d) => d.set_spec(ConvertProfileSpec {
            target,
            intent,
            black_point: true,
        }),
        other => panic!("{other:?}"),
    }
}

fn disabled_reason(ed: &mut Editor, action: MenuAction) -> Option<String> {
    let menu_ctx = crate::menu_bridge::context(ed, &Workspace::new());
    crate::menu_bridge::resolve_intent(action, &menu_ctx, ed).err()
}

fn depth(ed: &Editor) -> usize {
    ed.active().unwrap().history_depth()
}

fn tag(ed: &Editor) -> ColorSpace {
    ed.active().unwrap().document.meta.color_space.clone()
}

fn active_pixels(ed: &Editor) -> Vec<u8> {
    let doc = ed.active().unwrap();
    pixels::read_layer(doc, doc.document.active_layer().unwrap())
}

fn composite(ed: &mut Editor) -> Vec<u8> {
    let doc = ed.active_mut().unwrap();
    let rect = doc.canvas_rect();
    doc.composite(rect).unwrap()
}

#[test]
fn every_row_sits_in_its_menu_and_says_why_when_it_cannot_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut empty = editor(dir.path());
    let menus = crate::menu_bridge::menus(&empty);
    for p in ProfileChoice::ALL {
        assert!(offers(&menus, "Edit", MenuAction::AssignProfile(*p)));
    }
    let pattern = MenuAction::ToggleView(ui::ViewFlag::PatternPreview);
    for (title, a) in [
        ("Edit", MenuAction::ConvertToProfile),
        ("Image", MenuAction::ReduceColors),
        ("Image", MenuAction::WaveletDecompose),
        ("View", MenuAction::ClearSlices),
        ("View", MenuAction::SlicesFromGuides),
        ("View", pattern),
    ] {
        assert!(offers(&menus, title, a), "{a:?} in {title}");
    }
    // Every label is the catalogue's, not a literal.
    for (a, key) in [
        (
            MenuAction::ConvertToProfile,
            "ui.w13f.menu.convert_to_profile",
        ),
        (MenuAction::ReduceColors, "ui.w13f.menu.reduce_colors"),
        (MenuAction::WaveletDecompose, "ui.w13f.menu.wavelet"),
        (MenuAction::ClearSlices, "ui.w13f.menu.clear_slices"),
        (
            MenuAction::SlicesFromGuides,
            "ui.w13f.menu.slices_from_guides",
        ),
        (pattern, "ui.w13f.menu.pattern_preview"),
        (
            MenuAction::AssignProfile(ProfileChoice::FromFile),
            "ui.w13f.profile.from_file",
        ),
    ] {
        assert!(!tr(key).is_empty(), "{key} is in the catalogue");
        assert_eq!(a.label(), tr(key), "{a:?}");
    }
    for a in [
        MenuAction::AssignProfile(ProfileChoice::Srgb),
        MenuAction::ConvertToProfile,
        MenuAction::ReduceColors,
        MenuAction::WaveletDecompose,
        MenuAction::ClearSlices,
        MenuAction::SlicesFromGuides,
        pattern,
    ] {
        assert_eq!(
            disabled_reason(&mut empty, a).as_deref(),
            Some("No document is open"),
            "{a:?}"
        );
    }

    let mut ed = opened(dir.path(), 16, 16, &card(16, 16));
    assert_eq!(
        disabled_reason(&mut ed, MenuAction::SlicesFromGuides).as_deref(),
        Some(tr("ui.w13f.why.no_guides"))
    );
    assert_eq!(
        disabled_reason(&mut ed, MenuAction::ClearSlices).as_deref(),
        Some(tr("ui.w13f.why.no_slices"))
    );
    // The sRGB document's own profile is ticked and greyed.
    assert_eq!(
        disabled_reason(&mut ed, MenuAction::AssignProfile(ProfileChoice::Srgb)).as_deref(),
        Some(ui::menu::profile_already())
    );
    let menu_ctx = crate::menu_bridge::context(&mut ed, &Workspace::new());
    assert_eq!(
        MenuAction::AssignProfile(ProfileChoice::Srgb).checked(&menu_ctx),
        Some(true)
    );
    assert_eq!(
        MenuAction::AssignProfile(ProfileChoice::AdobeRgb).checked(&menu_ctx),
        Some(false)
    );
    assert_eq!(pattern.checked(&menu_ctx), Some(false));
    for a in [
        MenuAction::AssignProfile(ProfileChoice::AdobeRgb),
        MenuAction::ConvertToProfile,
        MenuAction::ReduceColors,
        MenuAction::WaveletDecompose,
        pattern,
    ] {
        assert_eq!(disabled_reason(&mut ed, a), None, "{a:?} is enabled");
    }
    // Once tagged Adobe RGB, that row is the greyed one.
    perform(MenuAction::AssignProfile(ProfileChoice::AdobeRgb), &mut ed).unwrap();
    assert_eq!(
        disabled_reason(&mut ed, MenuAction::AssignProfile(ProfileChoice::AdobeRgb)).as_deref(),
        Some(ui::menu::profile_already())
    );
    assert_eq!(
        disabled_reason(&mut ed, MenuAction::AssignProfile(ProfileChoice::Srgb)),
        None
    );
    // Wavelet Decompose solves against the sRGB decode, so a document
    // tagged with any other profile greys it (and says why), while Reduce
    // Colors, which works on the numbers, stays enabled.
    assert_eq!(
        disabled_reason(&mut ed, MenuAction::WaveletDecompose).as_deref(),
        Some(ui::menu::wavelet_needs_srgb())
    );
    assert_eq!(disabled_reason(&mut ed, MenuAction::ReduceColors), None);
    for p in [ProfileChoice::DisplayP3, ProfileChoice::ProPhotoRgb] {
        perform(MenuAction::AssignProfile(p), &mut ed).unwrap();
        assert_eq!(
            disabled_reason(&mut ed, MenuAction::WaveletDecompose).as_deref(),
            Some(ui::menu::wavelet_needs_srgb()),
            "{p:?}"
        );
    }
    perform(MenuAction::AssignProfile(ProfileChoice::Srgb), &mut ed).unwrap();
    assert_eq!(disabled_reason(&mut ed, MenuAction::WaveletDecompose), None);
}

#[test]
fn assign_profile_retags_in_one_undo_step_and_leaves_every_number_alone() {
    let dir = tempfile::tempdir().unwrap();
    let rgba = card(24, 20);
    let mut ed = opened(dir.path(), 24, 20, &rgba);
    assert_eq!(tag(&ed), ColorSpace::Srgb);
    let before = active_pixels(&ed);
    let shown_before = composite(&mut ed);
    let steps = depth(&ed);

    click(
        &mut ed,
        "Edit",
        MenuAction::AssignProfile(ProfileChoice::AdobeRgb),
    )
    .unwrap();

    assert_eq!(depth(&ed), steps + 1, "Assign is one undo step");
    let ColorSpace::IccProfile { profile, .. } = tag(&ed) else {
        panic!("tagged {:?}", tag(&ed));
    };
    assert_eq!(profile, color::icc::adobe_rgb_1998_profile());
    assert!(
        ed.active().unwrap().document.is_dirty(),
        "a new tag is an unsaved change"
    );
    assert_eq!(active_pixels(&ed), before, "Assign changes no number");
    // The same numbers under a wider profile: the picture shows differently.
    let decoded = |space: &ColorSpace, px: [u8; 3]| {
        SpaceTransform::new(space)
            .unwrap()
            .decode(px.map(|c| f32::from(c) / 255.0))
    };
    let (a, b) = (
        decoded(&ColorSpace::Srgb, [0, 255, 0]),
        decoded(&tag(&ed), [0, 255, 0]),
    );
    assert!((a[0] - b[0]).abs() > 0.05, "{a:?} vs {b:?}");
    // The composite is written in the document's own space, so its numbers
    // are the layer's numbers still, up to the rounding of the compositor's
    // decode / encode round trip through the profile.
    let shown_after = composite(&mut ed);
    let drift = shown_before
        .iter()
        .zip(&shown_after)
        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
        .max()
        .unwrap();
    assert!(drift <= 1, "the composite moved {drift} codes");

    // Undo takes the tag back; Redo puts it on again.
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(tag(&ed), ColorSpace::Srgb, "Undo restores the old tag");
    assert_eq!(depth(&ed), steps);
    ed.dispatch(Action::Redo).unwrap();
    assert!(matches!(tag(&ed), ColorSpace::IccProfile { .. }));

    // A second pick of the same profile says so.
    assert_eq!(
        perform(MenuAction::AssignProfile(ProfileChoice::AdobeRgb), &mut ed),
        Err("The document is already tagged Adobe RGB (1998)".to_string())
    );
}

#[test]
fn convert_to_profile_asks_then_rewrites_numbers_and_tag_in_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let rgba = card(24, 20);
    let mut ed = opened(dir.path(), 24, 20, &rgba);
    let before = active_pixels(&ed);
    let steps = depth(&ed);

    // The dialog opens aimed at another profile than the document's.
    let (mut chrome, _) = press(&mut ed, "Edit", MenuAction::ConvertToProfile);
    match chrome.dialogs_for_test().active_w13f_for_test() {
        W13fDialog::Convert(d) => {
            assert_eq!(d.spec().target, ProfileChoice::AdobeRgb);
            assert_eq!(d.spec().intent, RenderingIntent::RelativeColorimetric);
        }
        other => panic!("{other:?}"),
    }

    through_dialog(
        &mut ed,
        "Edit",
        MenuAction::ConvertToProfile,
        convert_spec(
            ProfileChoice::AdobeRgb,
            RenderingIntent::RelativeColorimetric,
        ),
    )
    .unwrap();

    let after = active_pixels(&ed);
    assert_ne!(after, before, "Convert changes the numbers");
    assert_eq!(depth(&ed), steps + 1, "one undo step");
    let tagged = tag(&ed);
    assert!(matches!(tagged, ColorSpace::IccProfile { .. }));
    // Every new number is the old colour (the same linear light) written in
    // Adobe RGB and rounded to the nearest code: sRGB fits inside Adobe RGB,
    // so nothing clips, and the 8-bit rounding is all that moves.
    let srgb = SpaceTransform::new(&ColorSpace::Srgb).unwrap();
    let adobe = SpaceTransform::new(&tagged).unwrap();
    let unit = |p: &[u8]| [p[0], p[1], p[2]].map(|c| f32::from(c) / 255.0);
    let mut worst = (0.0f32, [0u8; 4], [0u8; 4]);
    for (old, new) in before
        .as_chunks::<4>()
        .0
        .iter()
        .zip(after.as_chunks::<4>().0)
    {
        let ideal = adobe.encode(srgb.decode(unit(old)));
        for c in 0..3 {
            let miss = (ideal[c] * 255.0 - f32::from(new[c])).abs();
            if miss > worst.0 {
                worst = (miss, *old, *new);
            }
        }
    }
    assert!(worst.0 <= 0.5 + 1e-3, "not the nearest code: {worst:?}");
    // The published figure: sRGB's green is (144, 255, 60) in Adobe RGB.
    let green = adobe.convert(&srgb, [0.0, 1.0, 0.0]);
    let codes = green.map(|c| (c * 255.0).round() as i32);
    assert!(
        (codes[0] - 144).abs() <= 1 && codes[1] == 255 && (codes[2] - 60).abs() <= 1,
        "{codes:?}"
    );

    // One undo puts back the numbers AND the tag they belong to, so a second
    // conversion starts from the right profile.
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(active_pixels(&ed), before);
    assert_eq!(tag(&ed), ColorSpace::Srgb, "Undo restores the old tag");
    ed.dispatch(Action::Redo).unwrap();
    assert_eq!(active_pixels(&ed), after);
    assert_eq!(tag(&ed), tagged);
}

#[test]
fn the_rendering_intent_and_black_point_change_the_numbers_as_the_icc_defines() {
    let srgb = || SpaceTransform::new(&ColorSpace::Srgb).unwrap();
    let space = |bytes: Vec<u8>| raster::codec::icc_profile_space(&bytes);
    let prophoto = || SpaceTransform::new(&space(color::icc::prophoto_rgb_profile())).unwrap();
    let adobe = || SpaceTransform::new(&space(color::icc::adobe_rgb_1998_profile())).unwrap();
    let spec = |intent, black_point| ConvertProfileSpec {
        target: ProfileChoice::ProPhotoRgb,
        intent,
        black_point,
    };
    let white = [1.0; 3];
    // Relative: media white to media white.
    let relative = Conversion::new(
        srgb(),
        prophoto(),
        &spec(RenderingIntent::RelativeColorimetric, true),
    );
    let w = relative.convert(white);
    assert!(w.iter().all(|c| (c - 1.0).abs() < 2e-3), "{w:?}");
    // Perceptual and Saturation: the matrix-shaper fallback, relative.
    for intent in [RenderingIntent::Perceptual, RenderingIntent::Saturation] {
        let c = Conversion::new(srgb(), prophoto(), &spec(intent, true));
        for probe in [[0.2, 0.5, 0.9], [0.9, 0.1, 0.3], white] {
            assert_eq!(c.convert(probe), relative.convert(probe), "{intent:?}");
        }
    }
    // Absolute: sRGB's D65 white kept as the bluer white it is inside
    // ProPhoto's D50 one.
    let absolute = Conversion::new(
        srgb(),
        prophoto(),
        &spec(RenderingIntent::AbsoluteColorimetric, true),
    );
    let grey = [0.6; 3];
    let (r, a) = (relative.convert(grey), absolute.convert(grey));
    assert!(
        (r[0] - r[2]).abs() < 2e-3,
        "relative keeps grey neutral: {r:?}"
    );
    assert!(a[2] > a[0] + 0.02, "a D65 grey is bluer than D50: {a:?}");
    let w = absolute.convert(white);
    assert!(
        w[0] < 0.99,
        "nor is the white the destination's white: {w:?}"
    );
    // Between two D65 profiles the two colorimetric intents agree.
    let rel = Conversion::new(
        srgb(),
        adobe(),
        &spec(RenderingIntent::RelativeColorimetric, false),
    );
    let abs = Conversion::new(
        srgb(),
        adobe(),
        &spec(RenderingIntent::AbsoluteColorimetric, false),
    );
    for probe in [[0.2, 0.5, 0.9], white] {
        let (a, b) = (rel.convert(probe), abs.convert(probe));
        assert!(
            a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 2e-3),
            "{a:?} vs {b:?}"
        );
    }

    // Black point compensation: into a profile whose black is lifted, the
    // shadows are scaled up onto its black instead of crushing into code 0.
    let lifted = color::icc::matrix_shaper_profile(
        "lifted black",
        color::icc::ADOBE_RGB_1998_COLORANTS,
        color::icc::MEDIA_WHITE_D65,
        |e| 0.05 + 0.95 * e.powf(2.2),
    );
    let to = || SpaceTransform::new(&space(lifted.clone())).unwrap();
    let distinct = |bpc: bool| {
        let c = Conversion::new(
            srgb(),
            to(),
            &spec(RenderingIntent::RelativeColorimetric, bpc),
        );
        (0..64u8)
            .map(|g| (c.convert([f32::from(g) / 255.0; 3])[1] * 255.0).round() as u8)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    let (with, without) = (distinct(true), distinct(false));
    assert!(
        with > without + 10,
        "black point compensation keeps the shadows apart: {with} vs {without} codes"
    );
}

#[test]
fn a_profile_from_a_file_is_assigned_and_converted_to() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = opened(dir.path(), 8, 8, &card(8, 8));
    let file = dir.path().join("wide.icc");
    std::fs::write(&file, color::icc::prophoto_rgb_profile()).unwrap();

    SCRIPTED_PROFILE.with(|s| *s.borrow_mut() = Some(file.clone()));
    let before = active_pixels(&ed);
    through_dialog(
        &mut ed,
        "Edit",
        MenuAction::ConvertToProfile,
        convert_spec(
            ProfileChoice::FromFile,
            RenderingIntent::RelativeColorimetric,
        ),
    )
    .unwrap();
    let ColorSpace::IccProfile { profile, .. } = tag(&ed) else {
        panic!("not tagged");
    };
    assert_eq!(profile, std::fs::read(&file).unwrap());
    assert_ne!(active_pixels(&ed), before);

    // No file picked: nothing happens, and the status says why.
    assert_eq!(
        perform(MenuAction::AssignProfile(ProfileChoice::FromFile), &mut ed),
        Err("No profile file was chosen".to_string())
    );
    // A file that is not a profile is refused by name.
    let junk = dir.path().join("junk.icc");
    std::fs::write(&junk, b"not a profile at all, just some bytes").unwrap();
    SCRIPTED_PROFILE.with(|s| *s.borrow_mut() = Some(junk));
    assert!(perform(MenuAction::AssignProfile(ProfileChoice::FromFile), &mut ed).is_err());
}

#[test]
fn reduce_colors_asks_then_puts_the_layer_on_that_palette_in_one_undo_step() {
    use color::quantize::{Dither, PaletteKind};
    let dir = tempfile::tempdir().unwrap();
    let rgba = card(32, 24);
    let mut ed = opened(dir.path(), 32, 24, &rgba);
    let before = active_pixels(&ed);
    let distinct = |px: &[u8]| {
        px.as_chunks::<4>()
            .0
            .iter()
            .map(|p| [p[0], p[1], p[2]])
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    assert!(distinct(&before) > 64);
    let steps = depth(&ed);
    let reduce = |spec: IndexedSpec| {
        move |d: &mut W13fDialog| match d {
            W13fDialog::Reduce(d) => d.set_spec(spec),
            other => panic!("{other:?}"),
        }
    };

    // A palette size no fixed row offered: five colours, not dithered.
    through_dialog(
        &mut ed,
        "Image",
        MenuAction::ReduceColors,
        reduce(IndexedSpec {
            palette: PaletteKind::Adaptive,
            colors: 5,
            dither: Dither::None,
        }),
    )
    .unwrap();
    let after = active_pixels(&ed);
    assert!(distinct(&after) <= 5, "{} colours", distinct(&after));
    assert!(distinct(&after) >= 3, "{} colours", distinct(&after));
    assert_eq!(depth(&ed), steps + 1);

    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(active_pixels(&ed), before);

    // The web palette, dithered: every colour a multiple of 51.
    through_dialog(
        &mut ed,
        "Image",
        MenuAction::ReduceColors,
        reduce(IndexedSpec {
            palette: PaletteKind::Web,
            colors: 256,
            dither: Dither::Diffusion,
        }),
    )
    .unwrap();
    assert!(active_pixels(&ed)
        .as_chunks::<4>()
        .0
        .iter()
        .all(|p| p[..3].iter().all(|c| c % 51 == 0)));
}

#[test]
fn wavelet_decompose_asks_then_splits_the_layer_into_a_stack_that_recomposites_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (48, 40);
    // The card, and white noise over the whole code range in every channel:
    // the steepest details there are.
    let mut seed = 0x9e37_79b9_u32;
    let noise: Vec<u8> = (0..w * h * 4)
        .map(|i| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if i % 4 == 3 {
                255
            } else {
                (seed >> 24) as u8
            }
        })
        .collect();
    for (rgba, scales) in [
        (card(w, h), 2u8),
        (card(w, h), 5),
        (card(w, h), 7),
        (noise.clone(), 3),
        (noise, 6),
    ] {
        let mut ed = opened(dir.path(), w, h, &rgba);
        let shown = composite(&mut ed);
        let source = ed.active().unwrap().document.active_layer().unwrap();
        let steps = depth(&ed);

        through_dialog(
            &mut ed,
            "Image",
            MenuAction::WaveletDecompose,
            |d| match d {
                W13fDialog::Wavelet(d) => d.set_spec(WaveletSpec { scales }),
                other => panic!("{other:?}"),
            },
        )
        .unwrap();

        assert_eq!(depth(&ed), steps + 1, "one undo step");
        let doc = ed.active().unwrap();
        let root = doc.document.layers.root().to_vec();
        assert_eq!(root.len(), 1 + 1 + usize::from(scales), "{root:?}");
        assert_eq!(
            *root.last().unwrap(),
            source,
            "the source stays at the bottom"
        );
        assert!(
            !doc.document.layers.get(source).unwrap().visible,
            "and is hidden"
        );
        let names: Vec<String> = root
            .iter()
            .map(|id| doc.document.layers.get(*id).unwrap().name.clone())
            .collect();
        assert!(names[0].ends_with("Scale 1"), "{names:?}");
        assert!(names[root.len() - 2].ends_with("Residual"), "{names:?}");
        for id in &root[..usize::from(scales)] {
            assert_eq!(
                doc.document.layers.get(*id).unwrap().blend_mode,
                BlendMode::LinearLight
            );
        }
        assert_eq!(
            doc.document
                .layers
                .get(root[usize::from(scales)])
                .unwrap()
                .blend_mode,
            BlendMode::Normal
        );
        assert_eq!(doc.document.active_layer(), Some(root[0]));
        // The detail layers are not blank: the image really was split.
        let finest = pixels::read_layer(doc, root[0]);
        assert!(finest.as_chunks::<4>().0.iter().any(|p| p[0] != finest[0]));

        let recomposed = composite(&mut ed);
        let worst = shown
            .iter()
            .zip(&recomposed)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap();
        assert!(worst <= 1, "{scales} scales recomposite {worst}/255 off");

        ed.dispatch(Action::Undo).unwrap();
        assert_eq!(ed.active().unwrap().document.layers.root(), &[source]);
        assert_eq!(composite(&mut ed), shown);
    }
}

fn guided(dir: &Path) -> Editor {
    let mut ed = opened(dir, 40, 30, &card(40, 30));
    // Two vertical guides and one horizontal: a 3 x 2 grid. A guide off the
    // canvas and a repeat of one change nothing.
    let guide = |axis, doc: f32| editor_core::Guide {
        axis,
        doc,
        ..editor_core::Guide::default()
    };
    ed.apply_command(Command::SetGuides {
        guides: editor_core::Guides {
            list: vec![
                guide(editor_core::GuideAxis::Vertical, 10.0),
                guide(editor_core::GuideAxis::Vertical, 25.4),
                guide(editor_core::GuideAxis::Vertical, 10.2),
                guide(editor_core::GuideAxis::Vertical, 90.0),
                guide(editor_core::GuideAxis::Horizontal, 12.0),
            ],
            ..editor_core::Guides::default()
        },
    });
    ed
}

const GRID: [PixelRect; 6] = [
    PixelRect::new(0, 0, 10, 12),
    PixelRect::new(10, 0, 15, 12),
    PixelRect::new(25, 0, 15, 12),
    PixelRect::new(0, 12, 10, 18),
    PixelRect::new(10, 12, 15, 18),
    PixelRect::new(25, 12, 15, 18),
];

#[test]
fn clear_slices_and_slices_from_guides_replace_the_set_one_undo_step_each() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = opened(dir.path(), 40, 30, &card(40, 30));
    assert_eq!(
        perform(MenuAction::ClearSlices, &mut ed),
        Err("There are no slices to clear".to_string())
    );
    let mut ed = guided(dir.path());
    let id = ed.active().unwrap().id();
    let steps = depth(&ed);
    click(&mut ed, "View", MenuAction::SlicesFromGuides).unwrap();
    assert_eq!(depth(&ed), steps + 1);
    assert_eq!(ed.slices.get(id).to_vec(), GRID.to_vec());
    assert_eq!(ed.active().unwrap().document.slices.len(), 6);

    click(&mut ed, "View", MenuAction::ClearSlices).unwrap();
    assert!(ed.slices.get(id).is_empty());
    assert!(ed.active().unwrap().document.slices.is_empty());
    assert_eq!(depth(&ed), steps + 2);

    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(ed.active().unwrap().document.slices.len(), 6);
}

/// One chrome frame over `ed` with `events`, and what it produced and drew.
fn chrome_frame(
    ctx: &egui::Context,
    chrome: &mut Chrome,
    ed: &mut Editor,
    events: Vec<egui::Event>,
) -> (ChromeOutput, Vec<egui::Shape>) {
    let mut out = ChromeOutput::default();
    let full = ctx.run(raw_input(events), |ctx| {
        out = chrome.ui(ctx, ed);
    });
    let shapes = full.shapes.into_iter().map(|c| c.shape).collect();
    (out, shapes)
}

fn chrome_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    ctx
}

#[test]
fn the_slice_tools_options_bar_cuts_slices_from_the_guides() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = guided(dir.path());
    let doc_id = ed.active().unwrap().id();
    let ctx = chrome_ctx();
    let mut chrome = Chrome::new();
    let button = ui::view::ids::tool_option(tools::ToolId::Slice, "slices_from_guides");

    // Not the Brush's bar.
    ed.set_tool(tools::ToolId::Brush);
    for _ in 0..2 {
        chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    }
    assert!(
        ctx.read_response(button).is_none(),
        "the Brush has no such button"
    );

    ed.set_tool(tools::ToolId::Slice);
    for _ in 0..3 {
        chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    }
    let rect = ctx
        .read_response(button)
        .expect("the Slice tool's bar draws Slices From Guides")
        .rect;
    let at = rect.center();
    let steps = depth(&ed);
    let press = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    let (out, _) = chrome_frame(
        &ctx,
        &mut chrome,
        &mut ed,
        vec![egui::Event::PointerMoved(at), press(true), press(false)],
    );
    assert_eq!(out.menu, vec![MenuAction::SlicesFromGuides]);
    apply(&mut ed, out).unwrap();
    assert_eq!(ed.slices.get(doc_id).to_vec(), GRID.to_vec());
    assert_eq!(depth(&ed), steps + 1, "one undo step");
}

/// The meshes a frame drew with `texture`.
fn meshes_with(shapes: &[egui::Shape], texture: egui::TextureId) -> Vec<egui::Rect> {
    fn walk(shape: &egui::Shape, texture: egui::TextureId, out: &mut Vec<egui::Rect>) {
        match shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == texture => {
                out.push(mesh.calc_bounds());
            }
            egui::Shape::Vec(inner) => {
                for s in inner {
                    walk(s, texture, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in shapes {
        walk(s, texture, &mut out);
    }
    out
}

#[test]
fn pattern_preview_tiles_the_canvas_around_itself_and_unticks() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (32u32, 24u32);
    let rgba = card(w, h);
    let mut ed = opened(dir.path(), w, h, &rgba);
    {
        let doc = ed.active_mut().unwrap();
        doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
        doc.camera.zoom = 2.0;
        doc.camera.center = glam::Vec2::new(w as f32 / 2.0, h as f32 / 2.0);
    }
    let ctx = chrome_ctx();
    let mut chrome = Chrome::new();
    for _ in 0..2 {
        chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    }
    assert!(pattern_preview_image(&ctx).is_none(), "off by default");

    // Tick it through the View menu's own route.
    let pattern = MenuAction::ToggleView(ui::ViewFlag::PatternPreview);
    let menu_ctx = crate::menu_bridge::context(&mut ed, chrome.workspace());
    let intent = crate::menu_bridge::resolve_intent(pattern, &menu_ctx, &ed).unwrap();
    // What the menu bar does with a clicked row's intent: the chrome routes
    // it in its next frame.
    chrome.emit(intent);
    let (out, _) = chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    assert!(out.commands.is_empty(), "a view setting, not an edit");
    // The tick lands as the frame's intents are absorbed; the next frame
    // paints with it.
    let (_, shapes) = chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    assert!(chrome
        .workspace()
        .view_flags
        .get(ui::ViewFlag::PatternPreview));

    // The picture the copies draw is the document's composite.
    let image = pattern_preview_image(&ctx).expect("built once ticked");
    assert_eq!(image.size, [w as usize, h as usize]);
    let shown = composite(&mut ed);
    for (i, px) in image.pixels.iter().enumerate() {
        let want = &shown[i * 4..i * 4 + 4];
        assert_eq!(
            px.to_srgba_unmultiplied(),
            [want[0], want[1], want[2], want[3]]
        );
    }

    // Copies all around the canvas, each exactly one document away.
    let texture: egui::TextureId = ctx
        .data(|d| d.get_temp::<(PreviewKey, egui::TextureHandle)>(preview_id()))
        .expect("the texture is held")
        .1
        .id();
    let copies = meshes_with(&shapes, texture);
    assert!(copies.len() >= 8, "{} copies", copies.len());
    let doc = ed.active().unwrap();
    let camera = crate::tool_input::canvas_camera_of(&doc.camera);
    let viewport = crate::tool_input::canvas_viewport(&doc.camera);
    let screen = |x: f32, y: f32| {
        let s = camera.screen_pt_of(&viewport, glam::Vec2::new(x, y));
        egui::pos2(s.x, s.y)
    };
    let (fw, fh) = (w as f32, h as f32);
    for (i, j) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, -1)] {
        let want = egui::Rect::from_two_pos(
            screen(i as f32 * fw, j as f32 * fh),
            screen((i + 1) as f32 * fw, (j + 1) as f32 * fh),
        );
        assert!(
            copies
                .iter()
                .any(|r| (r.min - want.min).length() < 0.5 && (r.max - want.max).length() < 0.5),
            "no copy at ({i}, {j}): {want:?} not in {copies:?}"
        );
    }
    let canvas = egui::Rect::from_two_pos(screen(0.0, 0.0), screen(fw, fh));
    let overlap = |r: &egui::Rect| {
        let w = (r.max.x.min(canvas.max.x) - r.min.x.max(canvas.min.x)).max(0.0);
        let h = (r.max.y.min(canvas.max.y) - r.min.y.max(canvas.min.y)).max(0.0);
        w * h
    };
    assert!(
        copies.iter().all(|r| overlap(r) < 0.5),
        "no copy covers the canvas itself"
    );

    // Unticked, the copies are gone.
    let menu_ctx = crate::menu_bridge::context(&mut ed, chrome.workspace());
    let intent = crate::menu_bridge::resolve_intent(pattern, &menu_ctx, &ed).unwrap();
    chrome.emit(intent);
    chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let (_, shapes) = chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    assert!(!chrome
        .workspace()
        .view_flags
        .get(ui::ViewFlag::PatternPreview));
    assert!(meshes_with(&shapes, texture).is_empty());
}

// ---------------------------------------------------------------------------
// W13X-2: the canvas is colour-managed, and Pattern Preview draws its bytes
// ---------------------------------------------------------------------------

/// What the canvas texture receives for the whole document: the presenter's
/// own upload path ([`crate::presenter::CanvasPresenter::composite_masked`],
/// which `sync` runs for every full and per-tile upload).
fn presented(presenter: &crate::presenter::CanvasPresenter, ed: &mut Editor) -> Vec<u8> {
    let doc = ed.active_mut().unwrap();
    let rect = doc.canvas_rect();
    presenter.composite_masked(doc, rect).unwrap()
}

/// The largest per-channel difference between two RGBA8 buffers.
fn worst(a: &[u8], b: &[u8]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())
        .max()
        .unwrap_or(0)
}

#[test]
fn the_canvas_shows_the_documents_profile_assign_changes_it_and_convert_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (24u32, 20u32);
    let mut ed = opened(dir.path(), w, h, &card(w, h));
    let mut presenter = crate::presenter::CanvasPresenter::new();
    assert!(presenter.follow_display_space(ed.active().unwrap()));
    assert!(!presenter.follow_display_space(ed.active().unwrap()));

    // sRGB: the texture receives the composite as it is.
    let layer = active_pixels(&ed);
    let numbers = composite(&mut ed);
    let srgb_shown = presented(&presenter, &mut ed);
    assert_eq!(srgb_shown, numbers);

    // Assign Adobe RGB: not one number moves, the screen does.
    click(
        &mut ed,
        "Edit",
        MenuAction::AssignProfile(ProfileChoice::AdobeRgb),
    )
    .unwrap();
    assert_eq!(active_pixels(&ed), layer, "Assign changes no number");
    assert!(
        presenter.follow_display_space(ed.active().unwrap()),
        "a new tag re-sends the whole canvas (it dirties no tile)"
    );
    // The composite, encoded in the new profile (the same numbers to the
    // compositor's linear round trip).
    let numbers = composite(&mut ed);
    let adobe_shown = presented(&presenter, &mut ed);
    assert!(
        worst(&adobe_shown, &srgb_shown) > 20,
        "Assign must change what the canvas shows"
    );
    // Each presented pixel is its Adobe RGB numbers decoded and written as
    // sRGB, clipped: the nearest code of the exact float transform.
    let adobe = SpaceTransform::new(&tag(&ed)).unwrap();
    let srgb = SpaceTransform::new(&ColorSpace::Srgb).unwrap();
    for (n, s) in numbers
        .as_chunks::<4>()
        .0
        .iter()
        .zip(adobe_shown.as_chunks::<4>().0)
    {
        let ideal = srgb.convert(&adobe, [n[0], n[1], n[2]].map(|c| f32::from(c) / 255.0));
        for c in 0..3 {
            let miss = (ideal[c] * 255.0 - f32::from(s[c])).abs();
            assert!(miss <= 0.5 + 0.05, "{n:?} shows {s:?}, ideal {ideal:?}");
        }
        assert_eq!(n[3], s[3], "alpha untouched");
    }

    // Convert the Adobe RGB document to sRGB: the numbers move and the
    // screen keeps every pixel (the new sRGB numbers are the nearest codes of
    // what the canvas showed; clipped colours clip the same way).
    through_dialog(
        &mut ed,
        "Edit",
        MenuAction::ConvertToProfile,
        convert_spec(ProfileChoice::Srgb, RenderingIntent::RelativeColorimetric),
    )
    .unwrap();
    assert_eq!(tag(&ed), ColorSpace::Srgb);
    assert_ne!(active_pixels(&ed), layer, "Convert changes the numbers");
    assert!(presenter.follow_display_space(ed.active().unwrap()));
    let moved = worst(&presented(&presenter, &mut ed), &adobe_shown);
    assert!(
        moved <= 2,
        "Convert to sRGB moved the screen by {moved} codes"
    );

    // Undo twice: the sRGB tag and the first picture are back.
    ed.dispatch(Action::Undo).unwrap();
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(tag(&ed), ColorSpace::Srgb);
    assert_eq!(active_pixels(&ed), layer);
    assert!(
        !presenter.follow_display_space(ed.active().unwrap()),
        "sRGB again, as after the Convert: nothing to re-send for the tag"
    );
    assert_eq!(presented(&presenter, &mut ed), srgb_shown);

    // Convert sRGB to Adobe RGB: sRGB fits inside Adobe RGB, so the screen
    // keeps its colours to the 8-bit rounding of the new numbers: within 2
    // codes, except where one Adobe RGB code spans more sRGB codes than
    // that (an sRGB channel low in its steep toe beside a strong other
    // channel, e.g. red 12 beside green 229). Each pixel is held to its own
    // bound: the farthest the screen moves when each new number rounds by
    // half a code, plus the display's own rounding.
    through_dialog(
        &mut ed,
        "Edit",
        MenuAction::ConvertToProfile,
        convert_spec(
            ProfileChoice::AdobeRgb,
            RenderingIntent::RelativeColorimetric,
        ),
    )
    .unwrap();
    let adobe = SpaceTransform::new(&tag(&ed)).unwrap();
    assert!(presenter.follow_display_space(ed.active().unwrap()));
    let converted_shown = presented(&presenter, &mut ed);
    let (mut within_two, mut channels) = (0usize, 0usize);
    for (after, before) in converted_shown
        .as_chunks::<4>()
        .0
        .iter()
        .zip(srgb_shown.as_chunks::<4>().0)
    {
        let old = [before[0], before[1], before[2]].map(|c| f32::from(c) / 255.0);
        let exact = adobe.convert(&srgb, old).map(|v| v * 255.0);
        let mut bound = [0.0f32; 3];
        for corner in 0..8 {
            let mut a = exact;
            for (k, v) in a.iter_mut().enumerate() {
                let dir = if corner & (1 << k) == 0 { -0.5 } else { 0.5 };
                *v = (*v + dir).clamp(0.0, 255.0) / 255.0;
            }
            let shown = srgb.convert(&adobe, a);
            for c in 0..3 {
                bound[c] = bound[c].max((shown[c] * 255.0 - f32::from(before[c])).abs());
            }
        }
        for c in 0..3 {
            channels += 1;
            let moved = (i32::from(after[c]) - i32::from(before[c])).abs();
            assert!(
                moved as f32 <= (bound[c] + 1.0).max(2.0),
                "{before:?} now shows {after:?} (bound {bound:?})"
            );
            within_two += usize::from(moved <= 2);
        }
    }
    assert!(
        within_two * 100 >= channels * 97,
        "{within_two} of {channels} channels within 2 codes"
    );
}

#[test]
fn pattern_preview_on_an_adobe_rgb_document_draws_the_canvas_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (32u32, 24u32);
    let mut ed = opened(dir.path(), w, h, &card(w, h));
    click(
        &mut ed,
        "Edit",
        MenuAction::AssignProfile(ProfileChoice::AdobeRgb),
    )
    .unwrap();
    {
        let doc = ed.active_mut().unwrap();
        doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
        doc.camera.zoom = 2.0;
        doc.camera.center = glam::Vec2::new(w as f32 / 2.0, h as f32 / 2.0);
    }
    let ctx = chrome_ctx();
    let mut chrome = Chrome::new();
    chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let pattern = MenuAction::ToggleView(ui::ViewFlag::PatternPreview);
    let menu_ctx = crate::menu_bridge::context(&mut ed, chrome.workspace());
    let intent = crate::menu_bridge::resolve_intent(pattern, &menu_ctx, &ed).unwrap();
    chrome.emit(intent);
    chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let image = pattern_preview_image(&ctx).expect("built once ticked");
    assert_eq!(image.size, [w as usize, h as usize]);
    let copy: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|p| p.to_srgba_unmultiplied())
        .collect();

    let mut presenter = crate::presenter::CanvasPresenter::new();
    presenter.follow_display_space(ed.active().unwrap());
    let canvas = presented(&presenter, &mut ed);
    // Every tile edge: where a copy's column/row meets the canvas's opposite
    // one, both sides are the canvas texture's own bytes.
    let at = |buf: &[u8], x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };
    for y in 0..h {
        for x in [0, w - 1] {
            assert_eq!(at(&copy, x, y), at(&canvas, x, y), "column {x}, row {y}");
        }
    }
    for x in 0..w {
        for y in [0, h - 1] {
            assert_eq!(at(&copy, x, y), at(&canvas, x, y), "row {y}, column {x}");
        }
    }
    assert_eq!(copy, canvas, "the whole copy is the canvas texture's bytes");
    assert_ne!(
        canvas,
        composite(&mut ed),
        "an Adobe RGB document's numbers are not what the screen shows"
    );

    // The GPU texture the canvas samples, where an adapter exists.
    let gpu = match render::GpuContext::headless_blocking() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("SKIP GPU half: no adapter ({e:#})");
            return;
        }
    };
    let mut presenter = crate::presenter::CanvasPresenter::new();
    presenter.sync(&gpu, ed.active_mut().unwrap()).unwrap();
    let back = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();
    assert_eq!(
        back.as_rgba8(),
        &copy[..],
        "the texture holds the copy's bytes"
    );

    // Assign sRGB: the live presenter re-sends the whole canvas without a
    // dirty tile, and the texture now holds the numbers themselves.
    click(
        &mut ed,
        "Edit",
        MenuAction::AssignProfile(ProfileChoice::Srgb),
    )
    .unwrap();
    ed.active_mut().unwrap().take_dirty();
    let report = presenter.sync(&gpu, ed.active_mut().unwrap()).unwrap();
    assert_eq!(report.full_uploads, 1, "{report:?}");
    let back = presenter.texture().unwrap().read_level(&gpu, 0).unwrap();
    assert_eq!(back.as_rgba8(), &composite(&mut ed)[..]);
}

#[test]
fn a_profile_the_engine_cannot_transform_shows_the_numbers_on_canvas_and_in_pattern_preview() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (32u32, 24u32);
    let mut ed = opened(dir.path(), w, h, &card(w, h));
    // An ICC tag kept on import (PNG/JPEG/PSD) whose bytes are not a
    // matrix-shaper profile: LUT, Lab-PCS, gray or unparseable alike.
    let unsupported = ColorSpace::IccProfile {
        asset_hash: "not-a-matrix-shaper".into(),
        profile: vec![0u8; 200],
    };
    assert!(!unsupported.is_transform_supported());
    {
        let doc = ed.active_mut().unwrap();
        doc.document.meta.color_space = unsupported.clone();
        doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
        doc.camera.zoom = 2.0;
        doc.camera.center = glam::Vec2::new(w as f32 / 2.0, h as f32 / 2.0);
    }
    assert_eq!(tag(&ed), unsupported);

    // The canvas: the numbers as they are, not washed out by treating the
    // encoded numbers as linear light.
    let mut presenter = crate::presenter::CanvasPresenter::new();
    assert!(presenter.follow_display_space(ed.active().unwrap()));
    let numbers = composite(&mut ed);
    let canvas = presented(&presenter, &mut ed);
    assert_eq!(worst(&canvas, &numbers), 0, "the canvas shows the numbers");

    // Pattern Preview: the same bytes at every tile edge and everywhere.
    let ctx = chrome_ctx();
    let mut chrome = Chrome::new();
    chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let pattern = MenuAction::ToggleView(ui::ViewFlag::PatternPreview);
    let menu_ctx = crate::menu_bridge::context(&mut ed, chrome.workspace());
    let intent = crate::menu_bridge::resolve_intent(pattern, &menu_ctx, &ed).unwrap();
    chrome.emit(intent);
    chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    chrome_frame(&ctx, &mut chrome, &mut ed, Vec::new());
    let image = pattern_preview_image(&ctx).expect("built once ticked");
    let copy: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|p| p.to_srgba_unmultiplied())
        .collect();
    assert_eq!(copy, canvas, "Pattern Preview draws the canvas bytes");
}
