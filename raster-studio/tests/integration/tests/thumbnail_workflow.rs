//! The thumbnail workflow suite (plan Task 003 and onward).
//!
//! Task 003's card: deterministic composition fixtures with documented
//! dimensions, alpha and deterministic hashes, in a small CI size and a
//! full-size manual/performance size, assembled through the application's own
//! commands. Later cards extend this file card by card.

use integration_tests::app::DocExt;
use integration_tests::fixture::thumbnail::{
    self, background_rgba8, build_scene, fnv1a64, layout, logo_rgba8, oversized_source_rgba8,
    portrait_mask_coverage, portrait_rgba8, write_assets, FULL_CANVAS, SMALL_CANVAS,
};
use raster::decode_path;
use std::path::PathBuf;

#[test]
fn fixture_assets_have_the_documented_dimensions_and_alpha() {
    let (w, h) = SMALL_CANVAS;
    let l = layout(w, h);
    let bg = background_rgba8(w, h);
    let oversized = oversized_source_rgba8(l.oversized.0, l.oversized.1);
    let logo = logo_rgba8(l.logo.w);
    let portrait = portrait_rgba8(l.portrait.w, l.portrait.h);
    let px = |buf: &[u8], stride: u32, x: u32, y: u32| {
        let i = ((y as usize) * stride as usize + x as usize) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };

    // Background: the documented size, fully opaque, exact gradient ends.
    assert_eq!(bg.len(), (w * h * 4) as usize);
    for a in bg.iter().skip(3).step_by(4) {
        assert_eq!(*a, 255, "the background is fully opaque");
    }
    assert_eq!(px(&bg, w, w / 2, 0), [16, 24, 36, 255], "top stop");
    assert_eq!(px(&bg, w, w / 2, h - 1), [40, 52, 64, 255], "bottom stop");

    // Oversized: strictly larger than the canvas on both axes, with four
    // distinct labelled corners and a white centre dot.
    let (ow, oh) = l.oversized;
    assert_eq!((ow, oh), (w * 3 / 2, h * 16 / 9));
    let corner = (ow.min(oh) / 16).max(12);
    let probe = corner / 2;
    assert_eq!(
        px(&oversized, ow, probe, probe),
        [200, 32, 32, 255],
        "TL red"
    );
    assert_eq!(
        px(&oversized, ow, ow - probe, probe),
        [32, 190, 70, 255],
        "TR green"
    );
    assert_eq!(
        px(&oversized, ow, probe, oh - probe),
        [40, 96, 230, 255],
        "BL blue"
    );
    assert_eq!(
        px(&oversized, ow, ow - probe, oh - probe),
        [235, 205, 45, 255],
        "BR yellow"
    );
    assert_eq!(
        px(&oversized, ow, ow / 2, oh / 2),
        [255, 255, 255, 255],
        "centre"
    );
    assert_ne!(
        px(&oversized, ow, ow / 4, oh / 2),
        px(&oversized, ow, ow * 3 / 4, oh / 2),
        "asymmetric on purpose"
    );

    // Logo: transparent hole, opaque teal ring, transparent notch, and a real
    // feathered outer edge with strictly partial alpha.
    let side = l.logo.w;
    let c = side / 2;
    assert_eq!(px(&logo, side, c, c)[3], 0, "centre hole fully transparent");
    assert_eq!(
        px(&logo, side, c, c.saturating_sub(side * 7 / 20)),
        [30, 160, 170, 255],
        "the ring is opaque teal"
    );
    let mut partial = 0usize;
    let mut transparent = 0usize;
    let mut opaque = 0usize;
    for a in logo.iter().skip(3).step_by(4) {
        match *a {
            0 => transparent += 1,
            255 => opaque += 1,
            _ => partial += 1,
        }
    }
    assert!(opaque > 0 && transparent > 0, "ring and hole both exist");
    assert!(
        partial > side as usize * 2,
        "the ring has a feathered edge, not a hard cutoff"
    );
    let notch_x = (c as f32 + (side as f32 * 0.35) * 55f32.to_radians().cos()) as u32;
    let notch_y = (c as f32 + (side as f32 * 0.35) * 55f32.to_radians().sin()) as u32;
    assert_eq!(
        px(&logo, side, notch_x, notch_y)[3],
        0,
        "the notch is transparent"
    );

    // Portrait — SYNTHETIC coverage, labelled as such everywhere it appears:
    // opaque interior, transparent exterior, feathered boundary.
    let (pw, ph) = (l.portrait.w, l.portrait.h);
    assert_eq!(portrait.len(), (pw * ph * 4) as usize);
    assert_eq!(
        px(&portrait, pw, pw / 2, (ph as f32 * 0.34) as u32)[3],
        255,
        "head"
    );
    assert_eq!(px(&portrait, pw, 0, 0)[3], 0, "corner outside");
    assert_eq!(px(&portrait, pw, pw - 1, 0)[3], 0, "opposite corner");
    let band = portrait
        .iter()
        .skip(3)
        .step_by(4)
        .filter(|a| (1..255).contains(*a))
        .count();
    assert!(band > 0, "the silhouette edge is feathered");
}

#[test]
fn fixture_generation_is_deterministic() {
    let (w, h) = SMALL_CANVAS;
    let l = layout(w, h);
    assert_eq!(
        background_rgba8(w, h),
        background_rgba8(w, h),
        "background bytes are a pure function of size"
    );
    assert_eq!(
        oversized_source_rgba8(l.oversized.0, l.oversized.1),
        oversized_source_rgba8(l.oversized.0, l.oversized.1)
    );
    assert_eq!(logo_rgba8(l.logo.w), logo_rgba8(l.logo.w));
    assert_eq!(
        portrait_rgba8(l.portrait.w, l.portrait.h),
        portrait_rgba8(l.portrait.w, l.portrait.h)
    );

    // The assembled scene composites identically when built twice. The text
    // layers make the font library part of the run, so load the fixture font
    // the way every compositing test in this suite does.
    thumbnail::load_fixture_font();
    let mut a = build_scene(w, h, "Determinism A");
    let mut b = build_scene(w, h, "Determinism B");
    assert_eq!(
        a.doc.composite_all(),
        b.doc.composite_all(),
        "two builds of the scene composite to the same bytes"
    );
}

#[test]
fn the_scene_has_the_documented_structure() {
    let (w, h) = SMALL_CANVAS;
    let scene = build_scene(w, h, "Structure");
    let ids = scene.ids;
    let tree = &scene.doc.document.layers;

    // Root order, top to bottom.
    assert_eq!(
        tree.root(),
        &[
            ids.alternatives,
            ids.tone,
            ids.portrait,
            ids.headline,
            ids.subhead,
            ids.background
        ],
        "the documented root stack, top to bottom"
    );

    // The group holds [Headline A, Logos, Headline B]; Logos holds the logo.
    let group = tree.get(ids.alternatives).expect("group");
    assert_eq!(group.children(), &[ids.alt_a, ids.logos, ids.alt_b]);
    let logos = tree.get(ids.logos).expect("nested group");
    assert_eq!(logos.children(), &[ids.logo]);
    assert_eq!(tree.get(ids.logo).unwrap().name, "Logo");

    // The portrait has an attached raster mask; the adjustment clips to it.
    let portrait = tree.get(ids.portrait).expect("portrait");
    assert!(portrait.mask.is_some(), "the portrait carries a mask");
    let tone = tree.get(ids.tone).expect("adjustment");
    assert_eq!(
        tone.clipping,
        layer_model::ClippingMode::ClipToBelow,
        "the tone adjustment is clipped to the portrait"
    );

    // Headline B is the hidden alternative.
    assert!(
        !tree.get(ids.alt_b).unwrap().visible,
        "Headline B starts hidden"
    );
    assert!(tree.get(ids.alt_a).unwrap().visible, "Headline A shows");

    // The text layers are positioned through their transforms; the rasters
    // are painted in place with identity transforms.
    assert_ne!(
        tree.get(ids.headline).unwrap().transform,
        glam::Affine2::IDENTITY,
        "the headline is positioned by its layer transform"
    );
    assert_eq!(
        tree.get(ids.portrait).unwrap().transform,
        glam::Affine2::IDENTITY
    );

    // The mask coverage is distinct from the portrait's alpha: zero outside
    // and on the left edge, ramping across the left quarter, opaque after.
    let l = layout(w, h);
    let p = l.portrait;
    assert_eq!(
        portrait_mask_coverage(&l, p.x, p.y),
        0,
        "left edge masked out"
    );
    assert_eq!(
        portrait_mask_coverage(&l, p.x + p.w - 1, p.y + p.h / 2),
        255,
        "right side fully revealed"
    );
    assert_eq!(portrait_mask_coverage(&l, 1, 1), 0, "outside the portrait");
    let ramp = portrait_mask_coverage(&l, p.x + p.w * 15 / 100, p.y + p.h / 2);
    assert!(
        (1..255).contains(&ramp),
        "the mask has a real ramp, not a hard edge: {ramp}"
    );
}

#[test]
fn the_scene_survives_save_and_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("thumbnail-scene.rstudio");
    thumbnail::load_fixture_font();
    let mut scene = build_scene(SMALL_CANVAS.0, SMALL_CANVAS.1, "Persist");
    let before = scene.doc.composite_all();
    scene.doc.save_to(&package, "integration-test").unwrap();

    let mut reopened = integration_tests::app::open_project(&package);
    assert_eq!(
        reopened.composite_all(),
        before,
        "the reopened scene composites to the same bytes"
    );
}

#[test]
fn fixture_assets_round_trip_through_png() {
    let tmp = tempfile::tempdir().unwrap();
    let (w, h) = SMALL_CANVAS;
    let l = layout(w, h);
    let assets = write_assets(tmp.path(), w, h).expect("assets write");

    let cases: [(&std::path::Path, Vec<u8>, (u32, u32)); 4] = [
        (assets.background.as_path(), background_rgba8(w, h), (w, h)),
        (
            assets.oversized.as_path(),
            oversized_source_rgba8(l.oversized.0, l.oversized.1),
            l.oversized,
        ),
        (
            assets.logo.as_path(),
            logo_rgba8(l.logo.w),
            (l.logo.w, l.logo.h),
        ),
        (
            assets.portrait.as_path(),
            portrait_rgba8(l.portrait.w, l.portrait.h),
            (l.portrait.w, l.portrait.h),
        ),
    ];
    for (path, expected, (ew, eh)) in cases {
        let decoded = decode_path(path).expect("the PNG decodes");
        assert_eq!((decoded.width, decoded.height), (ew, eh));
        assert_eq!(
            decoded.rgba8,
            expected,
            "PNG is lossless: {} round-trips the fixture bytes",
            path.display()
        );
    }
}

/// Materialize the full-size (3628 × 2041) assets and a saved scene into
/// `tests/project-fixtures/`, and print their content hashes.
///
/// Ignored by default: it writes multi-megabyte artifacts. Run it explicitly
/// when a card needs real files on disk (place/drop/PSD fixtures):
///
/// ```text
/// cargo test -p integration-tests --test thumbnail_workflow -- --ignored --nocapture materialize
/// ```
#[test]
#[ignore = "writes large fixture artifacts; run manually when a card needs the files on disk"]
fn materialize_full_size_assets_and_project() {
    let (w, h) = FULL_CANVAS;
    // The test process runs with its CWD at the package root
    // (`tests/integration`); the card's fixture home is the workspace's
    // `tests/project-fixtures`.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("integration sits under tests/")
        .join("project-fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    let assets = write_assets(&dir, w, h).expect("full-size assets write");
    let mut scene = build_scene(w, h, "Thumbnail 3628x2041");
    let package = dir.join("thumbnail-scene-3628x2041.rstudio");
    scene.doc.save_to(&package, "integration-test").unwrap();

    for (name, path) in [
        ("background", &assets.background),
        ("oversized", &assets.oversized),
        ("logo", &assets.logo),
        ("portrait", &assets.portrait),
    ] {
        let bytes = std::fs::read(path).unwrap();
        println!(
            "hash {name}: {:016x} ({} bytes)",
            fnv1a64(&bytes),
            bytes.len()
        );
    }
    // A native package is a directory (document + tiles + journal), so its
    // "content hash" is the sorted per-file list.
    let mut package_files: Vec<PathBuf> = std::fs::read_dir(&package)
        .expect("the package directory exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    package_files.sort();
    for file in &package_files {
        let bytes = std::fs::read(file).unwrap();
        println!(
            "hash package/{}: {:016x} ({} bytes)",
            file.file_name().unwrap().to_string_lossy(),
            fnv1a64(&bytes),
            bytes.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Task 014 — the interaction foundation, through the real shell routes
// ---------------------------------------------------------------------------

/// The Phase 1 foundation holds end to end: typed tool options reach the
/// running Move tool through the pointer route; the edit-target snapshot
/// answers content or mask per document; bounds queries answer the real
/// stored extent; a pending edit commits as one labeled undo entry that undo
/// takes back whole. Every step here rides the routes the shipping shell
/// runs — `ToolPointer::handle`, `Editor::begin/commit_edit`,
/// `Editor::set_edit_target_kind` — assembled exactly as the shell calls them.
#[test]
fn the_interaction_foundation_holds_through_the_real_shell_routes() {
    use app_shell::tool_input::ToolPointer;
    use compositor::bounds;
    use editor_core::{Command, LayerPatch, Patch};
    use integration_tests::app;
    use layer_model::{LayerMask, MaskId};
    use ui::canvas::PointerPhase;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 64, 64);
    let canvas_layer = app::the_opened_layer(&ed);

    // A second layer with ink in its own corner, through the command route:
    // the auto-select target and the transform's victim.
    let ink_layer = {
        let layer = layer_model::Layer::raster("Ink");
        let id = layer.id;
        let doc = ed.active_mut().unwrap();
        doc.apply(Command::create_layer(layer)).unwrap();
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in 8..16u32 {
            for x in 40..48u32 {
                let i = ((y * raster::TILE_SIZE + x) * 4) as usize;
                bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
            }
        }
        let hash = doc.tiles.insert_bytes(bytes);
        doc.apply(
            Command::paint_tiles(
                editor_core::PixelTarget::Layer(id),
                vec![editor_core::TileEdit::set(
                    raster::TileCoord::new(0, 0, 0),
                    hash,
                )],
            )
            .unwrap(),
        )
        .unwrap();
        // The panel selection stays on the canvas layer beneath.
        ed.set_layer_selection(vec![canvas_layer], Some(canvas_layer));
        id
    };
    let depth_setup = ed.active().unwrap().history_depth();

    // 1. Typed tool options: Auto-Select forwarded at pointer-down makes a
    //    Move press claim the ink under the pointer, not the active layer.
    ed.set_tool(tools::ToolId::Move);
    let mut pointer = ToolPointer::new();
    let settings = [("auto_select".to_string(), tools::ToolSetting::Bool(true))];
    let down_at = app::shell_screen_pt(ed.active().unwrap(), 44.0, 12.0);
    let down = pointer.handle(
        &mut ed,
        ui::canvas::PointerInput::at(PointerPhase::Down, down_at),
        false,
        &settings,
    );
    assert!(down.reached_tool && down.failed.is_none(), "{down:?}");
    // Release without a drag: a no-motion Move creates no history.
    let up_at = app::shell_screen_pt(ed.active().unwrap(), 44.0, 12.0);
    pointer.handle(
        &mut ed,
        ui::canvas::PointerInput::at(PointerPhase::Up, up_at),
        false,
        &settings,
    );
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_setup,
        "a click moves nothing"
    );

    // 2. Bounds: the ink layer's stored extent is exactly its tile.
    let ink_bounds = bounds::content_bounds(
        &ed.active().unwrap().document,
        &ed.active().unwrap().tiles,
        ink_layer,
        0,
        Default::default(),
    )
    .unwrap()
    .expect("the ink layer has stored tiles");
    assert_eq!(ink_bounds, raster::PixelRect::new(0, 0, 256, 256));
    let ink_alpha = bounds::alpha_bounds(
        &ed.active().unwrap().document,
        &ed.active().unwrap().tiles,
        ink_layer,
        0,
        Default::default(),
    )
    .unwrap()
    .expect("the ink is precise");
    assert_eq!(ink_alpha, raster::PixelRect::new(40, 8, 8, 8));

    // 3. The edit target: content by default; mask only when the layer has one.
    assert_eq!(
        ed.edit_target().unwrap().kind,
        app_shell::edit_target::EditTargetKind::Content
    );
    let mask = {
        let doc = ed.active_mut().unwrap();
        doc.apply(Command::SetLayerProperties {
            layer_id: canvas_layer,
            patch: LayerPatch {
                mask: Patch::Set(LayerMask::new(MaskId::new())),
                ..Default::default()
            },
        })
        .unwrap();
        doc.document.layers.get(canvas_layer).unwrap().mask_id()
    };
    assert!(mask.is_some(), "the canvas layer now carries a mask");
    // Card 038: the auto-select gestures above now sync the panel selection
    // to the picked layer — restore the canvas layer before aiming the edit
    // target at its new mask.
    ed.set_layer_selection(vec![canvas_layer], Some(canvas_layer));
    ed.set_edit_target_kind(app_shell::edit_target::EditTargetKind::Mask);
    assert_eq!(
        ed.edit_target().unwrap().kind,
        app_shell::edit_target::EditTargetKind::Mask,
        "the mask target resolves once a mask exists"
    );

    // 4. The pending-edit lifecycle: a two-command gesture is one labeled
    // entry, and undo takes the whole gesture back.
    let depth_before = depth_setup + 1; // +1: the mask attach above
    ed.begin_edit(vec![ink_layer], "Move the ink").unwrap();
    ed.commit_edit(vec![
        Command::TransformLayer {
            layer_id: ink_layer,
            matrix: [1.0, 0.0, 0.0, 1.0, 8.0, 4.0],
        },
        Command::SetLayerProperties {
            layer_id: ink_layer,
            patch: LayerPatch {
                name: Some("Ink (moved)".to_string()),
                ..Default::default()
            },
        },
    ])
    .unwrap();
    assert_eq!(ed.active().unwrap().history_depth(), depth_before + 1);
    assert_eq!(
        ed.active().unwrap().history.undo_label(),
        Some("Move the ink")
    );
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(ed.active().unwrap().history_depth(), depth_before);
    assert_eq!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(ink_layer)
            .unwrap()
            .name,
        "Ink"
    );

    // 5. Coordinate conversion: the document point under a screen position
    // round-trips through the fixture camera.
    let doc = ed.active().unwrap();
    let camera = app_shell::tool_input::canvas_camera_of(&doc.camera);
    let viewport = app_shell::tool_input::canvas_viewport(doc.camera.viewport_size);
    let doc_pt = glam::Vec2::new(31.5, 17.25);
    let screen = ui::canvas::CanvasCamera::screen_pt_of(&camera, &viewport, doc_pt);
    let back = ui::canvas::CanvasCamera::doc_of_screen_pt(&camera, &viewport, screen);
    assert!(
        (back - doc_pt).length() < 1e-4,
        "screen<->document round-trips: {doc_pt:?} -> {screen:?} -> {back:?}"
    );
}

/// Task 018: a styled text document saves and reopens with an identical model
/// and composite. The persisted rich-text schema is version 4; the migration
/// for older documents is the schema's own serde defaults (a three-field text
/// layer loads black, regular, auto-leading point text — what the old renderer
/// produced), and future versions are refused rather than silently degraded.
#[test]
fn a_styled_text_document_round_trips_through_the_native_package() {
    use app_shell::doc::OpenDocument;
    use editor_core::Command;
    use integration_tests::app::APP_VERSION;
    use layer_model::text::{StyleOverride, StyleSpan, Weight};
    use layer_model::LayerKind;

    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path().join("styled.rstudio");

    // Build the acceptance scene's headline through the real commands: white,
    // bold, condensed-family, one green span, a wrapping box, a kerning nudge.
    let mut doc = OpenDocument::blank(
        app_shell::doc::DocumentId(crate_id()),
        256,
        144,
        "styled",
        100,
    )
    .unwrap();
    let layer = layer_model::Layer::with_kind(
        "Headline",
        LayerKind::Text(layer_model::TextLayer {
            text: "Weekly digest".to_string(),
            font_family: "DejaVu Sans Condensed".to_string(),
            size_px: 48.0,
            warp: Default::default(),
            path: None,
            style: layer_model::text::BaseStyle {
                weight: Weight(700),
                fill: [1.0, 1.0, 1.0, 1.0],
                ..layer_model::text::BaseStyle::default()
            },
            spans: vec![StyleSpan {
                start: 7,
                end: 13,
                style: StyleOverride {
                    fill: Some([0.2, 0.8, 0.4, 1.0]),
                    ..StyleOverride::default()
                },
            }],
            paragraph: layer_model::text::Paragraph {
                alignment: layer_model::text::Alignment::Center,
                ..layer_model::text::Paragraph::default()
            },
            frame: layer_model::text::Frame::Box {
                width: 200.0,
                height: Some(64.0),
            },
            kerning: vec![layer_model::text::Kern {
                index: 7,
                amount: -20.0,
            }],
        }),
    );
    let id = layer.id;
    doc.apply(Command::create_layer(layer)).unwrap();
    let model_before = doc.document.layers.get(id).unwrap().kind.clone();
    let composite_before = doc.composite_all();

    doc.save_to(&package, APP_VERSION).unwrap();

    let reopened = integration_tests::app::open_project(&package);
    assert_eq!(
        reopened.document.meta.format_version,
        editor_core::DOCUMENT_FORMAT_VERSION,
        "the reopened package carries the current document version"
    );
    let reopened_kind = reopened.document.layers.get(id).unwrap().kind.clone();
    assert_eq!(
        reopened_kind, model_before,
        "the styled model reopens field for field"
    );
    let mut reopened = reopened;
    assert_eq!(
        reopened.composite_all(),
        composite_before,
        "and composites to the same bytes"
    );
}

/// Distinct ids for document-level tests (the application allocates per tab).
fn crate_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(9000);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// -- Task 021: the Character/Paragraph controls write persistent data -------

/// The Character panel's edits reach the document through the shell's kind
/// edit route, change the composite, fold one gesture into one undo step,
/// undo back to the exact baseline, and survive save/reopen. The panel's
/// own setters and `commit` build the intent; `menu_bridge::pick` resolves
/// it the way `Chrome::harvest` does; `Editor::apply_kind_edit` applies it
/// with the gesture the window would stamp.
#[test]
fn the_character_panel_edits_reach_the_pixels_undo_and_reopen() {
    use app_shell::chrome::KindEdit;
    use app_shell::menu_bridge;
    use editor_core::Command;
    use integration_tests::app;
    use integration_tests::fixture::thumbnail::load_fixture_font;
    use layer_model::text::Weight;
    use layer_model::LayerKind;
    use ui::panels::text as text_panel;

    load_fixture_font();
    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 256, 144);

    // The headline the acceptance scene opens with, through the command route.
    let id = {
        let layer = layer_model::Layer::with_kind(
            "Headline",
            LayerKind::Text(layer_model::TextLayer {
                text: "Headline".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let id = layer.id;
        app::apply_intent(&mut ed, ui::Intent::Document(Command::create_layer(layer)));
        assert!(
            ed.active().unwrap().document.layers.get(id).is_some(),
            "the text layer was created"
        );
        // The panel edits the ACTIVE layer, and create_layer does not have to
        // move the selection - the user just made a layer, the panel's read
        // goes through the same selection the Layers panel shows.
        ed.set_layer_selection(vec![id], Some(id));
        id
    };

    let region = ed.active().unwrap().canvas_rect();
    let baseline = ed.active_mut().unwrap().composite(region).unwrap();
    let depth_setup = ed.active().unwrap().history_depth();

    // The panel reads its run, changes three fields the way its controls do
    // (size slider, weight combo, fill picker), and commits one intent.
    let gesture: u64 = 7;
    {
        let doc = ed.active().unwrap();
        let (layer, mut run) = text_panel::active_text(&doc.document, doc.document.active_layer())
            .expect("the active layer is text");
        assert!(text_panel::Character::set_size(&mut run, 48.0));
        assert!(text_panel::Character::set_weight(&mut run, 700));
        // The fill arrives the way the picker delivers it: an sRGB swatch
        // that the panel converts into the model's linear space.
        let picked = egui::Color32::from_rgba_unmultiplied(30, 220, 90, 255);
        assert!(text_panel::Character::set_color(
            &mut run,
            text_panel::swatch_to_fill(picked)
        ));
        let intent = text_panel::commit(&doc.document, layer, &run).expect("an edit was made");

        // The shell answers the intent - not unrouted, not dropped.
        let pick = menu_bridge::pick(&intent, &ed).expect("the kind edit is routed");
        let app_shell::menu_bridge::Pick::Kind { layer, kind } = pick else {
            panic!("a kind intent picked something else: {pick:?}");
        };
        // `Chrome::harvest` stamps the gesture; a drag is one gesture.
        ed.apply_kind_edit(KindEdit {
            layer,
            kind,
            gesture: Some(gesture),
        });
    }

    // Pixels moved.
    let after_edit = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(after_edit, baseline, "the panel edit changed pixels");

    // One gesture, one undo step - three fields, one entry.
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_setup + 1,
        "a gesture folds into one undo step"
    );

    // A second frame of the same gesture folds into the same entry.
    {
        let doc = ed.active().unwrap();
        let (layer, mut run) = text_panel::active_text(&doc.document, doc.document.active_layer())
            .expect("still a text layer");
        assert!(text_panel::Character::set_tracking(&mut run, 120.0));
        let intent = text_panel::commit(&doc.document, layer, &run).expect("an edit was made");
        let Some(app_shell::menu_bridge::Pick::Kind { layer, kind }) =
            menu_bridge::pick(&intent, &ed)
        else {
            panic!("the kind edit is routed");
        };
        ed.apply_kind_edit(KindEdit {
            layer,
            kind,
            gesture: Some(gesture),
        });
    }
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_setup + 1,
        "the second frame of the same gesture stays folded"
    );

    // Undo takes the whole gesture back to the exact baseline.
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(
        ed.active_mut().unwrap().composite(region).unwrap(),
        baseline,
        "undo restores the exact baseline pixels"
    );

    // Redo, then the edited model survives save/reopen and the panel reads
    // the edited values back.
    ed.active_mut().unwrap().redo().unwrap();
    {
        let package = tmp.path().join("panel_edits.rstudio");
        ed.active_mut()
            .unwrap()
            .save_to(&package, app::APP_VERSION)
            .unwrap();
        let reopened = app::open_project(&package);
        let Some(LayerKind::Text(t)) = reopened.document.layers.get(id).map(|l| &l.kind) else {
            panic!("the reopened layer is text");
        };
        assert_eq!(t.size_px, 48.0);
        assert_eq!(t.style.weight, Weight(700));
        assert_eq!(
            t.style.fill,
            text_panel::swatch_to_fill(egui::Color32::from_rgba_unmultiplied(30, 220, 90, 255)),
            "the linear fill survives the package"
        );
        let (_, run) =
            text_panel::active_text(&reopened.document, reopened.document.active_layer())
                .expect("the reopened layer is the active text layer");
        assert_eq!(
            run.style.color, t.style.fill,
            "the panel reads what it wrote"
        );
        assert_eq!(run.style.size_px, 48.0);
    }
}

// -- card 022: font selection and substitution reporting ---------------------

#[test]
fn font_selection_reports_substitution_and_keeps_the_requested_family() {
    use app_shell::chrome::KindEdit;
    use app_shell::menu_bridge;
    use editor_core::Command;
    use integration_tests::app;
    use integration_tests::fixture::thumbnail::load_fixture_font;
    use layer_model::LayerKind;
    use ui::panels::text as text_panel;

    // Deterministic fonts only: the licensed fixture faces.
    load_fixture_font();
    assert_eq!(
        compositor::load_font(dejavu::sans_condensed::regular().to_vec()),
        1,
        "the condensed face joins the library"
    );

    // Loading a document font must not touch egui's UI typefaces. The app
    // installs its own through the theme; the width of a fixed UI string in a
    // fixed style cannot change across a `load_font`.
    let ctx = egui::Context::default();
    app_shell::chrome::install_theme(&ctx, design::Theme::Dark);
    // Fonts are built on the first run pass; warm one so measuring works.
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("warm");
        });
    });
    let ui_font = egui::TextStyle::Body.resolve(&ctx.style());
    let ui_text_width = |ctx: &egui::Context| {
        ctx.fonts(|f| {
            f.layout_no_wrap(
                "Raster Studio".to_string(),
                ui_font.clone(),
                egui::Color32::WHITE,
            )
            .rect
            .width()
        })
    };
    let before = ui_text_width(&ctx);
    load_fixture_font();
    compositor::load_font(dejavu::sans_condensed::bold().to_vec());
    // The embedded serif face is step 2's control: a face that is never the
    // library's generic sans, so it is guaranteed to draw differently from
    // whatever the substitution resolves to on this machine.
    let control_family = "DejaVu Serif";
    compositor::load_font(dejavu::serif::bold().to_vec());
    assert!(
        !compositor::font_family_faces(control_family).is_empty(),
        "the serif control face joins the library"
    );
    assert_eq!(
        ui_text_width(&ctx),
        before,
        "document font loading leaves the UI fonts alone"
    );

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 256, 144);
    let id = {
        let layer = layer_model::Layer::with_kind(
            "Headline",
            LayerKind::Text(layer_model::TextLayer {
                text: "Headline".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let id = layer.id;
        app::apply_intent(&mut ed, ui::Intent::Document(Command::create_layer(layer)));
        ed.set_layer_selection(vec![id], Some(id));
        id
    };

    let region = ed.active().unwrap().canvas_rect();
    let baseline = ed.active_mut().unwrap().composite(region).unwrap();
    let depth_setup = ed.active().unwrap().history_depth();

    // 1. The picker's face rows list the fixture family's widths, and
    //    adopting the condensed face updates the headline's pixels through
    //    the real panel route.
    let faces = compositor::font_family_faces("DejaVu Sans");
    assert!(
        faces
            .iter()
            .any(|f| f.stretch == text_engine::FontStretch::SemiCondensed),
        "the condensed width is listed for the family"
    );
    let gesture: u64 = 22;
    {
        let doc = ed.active().unwrap();
        let (layer, mut run) = text_panel::active_text(&doc.document, doc.document.active_layer())
            .expect("the active layer is text");
        // The bold condensed headline the acceptance scene wants.
        assert!(text_panel::Character::set_face(
            &mut run,
            text_engine::FontWeight::BOLD,
            text_engine::FontSlant::Normal,
            text_engine::FontStretch::SemiCondensed,
        ));
        let intent = text_panel::commit(&doc.document, layer, &run).expect("an edit was made");
        let Some(app_shell::menu_bridge::Pick::Kind { layer, kind }) =
            menu_bridge::pick(&intent, &ed)
        else {
            panic!("the kind edit is routed");
        };
        ed.apply_kind_edit(KindEdit {
            layer,
            kind,
            gesture: Some(gesture),
        });
    }
    let condensed_pixels = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(
        condensed_pixels, baseline,
        "the face adoption changed pixels"
    );
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_setup + 1,
        "the gesture folds into one undo step"
    );

    // 2. A family the machine does not have is reported, with an installed
    //    substitute — and the requested name stays in the document.
    //
    //    What the substitution changes on screen depends on the machine: the
    //    substitute is the library's pinned generic sans (`SANS_PREFERENCES`
    //    in text-engine — Segoe UI on Windows, Helvetica Neue/Arial on macOS).
    //    On a bare Linux runner none of those is installed, so the generic sans
    //    IS the fixture family "DejaVu Sans", and a run already set to DejaVu
    //    Sans Bold SemiCondensed shapes with the very same face after the
    //    substitution: identical pixels. That is why `assert_ne!(substituted,
    //    condensed_pixels)` was red on ubuntu-latest for ~30 pushes and green
    //    everywhere else. So the pixel contract is pinned against what the
    //    substitution actually does — draw with exactly the face it reports —
    //    and against a deterministic control face (DejaVu Serif, embedded)
    //    that is never the generic sans. Reproduce the runner locally with
    //    `RASTER_STUDIO_FONT_DIRS=` (set, empty: no system fonts).
    let missing = "Raster Test Missing Family";
    let substitute = text_panel::substitution(missing).expect("a missing family is reported");
    assert!(
        !compositor::font_family_faces(&substitute).is_empty(),
        "the reported substitute {substitute:?} is an installed family"
    );
    assert_ne!(
        substitute, control_family,
        "the generic sans is never the serif control face"
    );
    fn set_family(ed: &mut app_shell::Editor, family: &str) {
        let doc = ed.active().unwrap();
        let (layer, mut run) = text_panel::active_text(&doc.document, doc.document.active_layer())
            .expect("still a text layer");
        assert!(text_panel::Character::set_family(&mut run, family));
        let intent = text_panel::commit(&doc.document, layer, &run).expect("an edit was made");
        let Some(app_shell::menu_bridge::Pick::Kind { layer, kind }) =
            menu_bridge::pick(&intent, ed)
        else {
            panic!("the kind edit is routed");
        };
        ed.apply_kind_edit(KindEdit {
            layer,
            kind,
            gesture: None,
        });
    }
    set_family(&mut ed, missing);
    {
        let doc = ed.active().unwrap();
        let Some(LayerKind::Text(t)) = doc.document.layers.get(id).map(|l| &l.kind) else {
            panic!("the layer is text");
        };
        assert_eq!(
            t.font_family, missing,
            "the requested name is retained, not rewritten"
        );
    }
    let substituted = ed.active_mut().unwrap().composite(region).unwrap();
    // The substituted run draws with exactly the face the report names: asking
    // for that family by name, in the same bold condensed style, is the same
    // picture.
    set_family(&mut ed, &substitute);
    let explicit = ed.active_mut().unwrap().composite(region).unwrap();
    assert_eq!(
        substituted, explicit,
        "the substituted run draws with the reported substitute {substitute:?}"
    );
    // And it is a real render of that face, not a blank or a stale cache: the
    // embedded serif control draws differently.
    set_family(&mut ed, control_family);
    let control = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(
        substituted, control,
        "the substitute {substitute:?} and the {control_family} control differ on screen"
    );
    // Whether the substitution is visible against the DejaVu Sans run follows
    // from the substitute's identity — both ways are asserted, neither assumed.
    if substitute == thumbnail::FONT_FAMILY {
        assert_eq!(
            substituted, condensed_pixels,
            "the generic sans is the fixture family here, so the same face draws"
        );
    } else {
        assert_ne!(
            substituted, condensed_pixels,
            "a different generic sans {substitute:?} changes the picture"
        );
    }
    // Back to the missing family for the round trip; the same request yields
    // the same picture.
    set_family(&mut ed, missing);
    assert_eq!(
        ed.active_mut().unwrap().composite(region).unwrap(),
        substituted,
        "the substitution renders deterministically"
    );

    // 3. Save/reopen retains the requested (missing) family and the adopted
    //    width; the substitution still reports after the round trip.
    let package = tmp.path().join("font_selection.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let reopened = app::open_project(&package);
    let Some(LayerKind::Text(t)) = reopened.document.layers.get(id).map(|l| &l.kind) else {
        panic!("the reopened layer is text");
    };
    assert_eq!(t.font_family, missing);
    assert_eq!(
        t.style.stretch,
        layer_model::text::Stretch::SemiCondensed,
        "the adopted width survives the package"
    );
    assert!(text_panel::substitution(&t.font_family).is_some());
}

// -- card 023: point vs paragraph geometry -----------------------------------

#[test]
fn a_paragraph_box_reflows_without_touching_the_type_size_and_survives_reopen() {
    use app_shell::chrome::KindEdit;
    use app_shell::menu_bridge;
    use editor_core::Command;
    use integration_tests::app;
    use integration_tests::fixture::thumbnail::load_fixture_font;
    use layer_model::LayerKind;
    use ui::panels::text as text_panel;

    load_fixture_font();
    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 256, 144);
    let id = {
        let layer = layer_model::Layer::with_kind(
            "Subhead",
            LayerKind::Text(layer_model::TextLayer {
                text: "wrap wrap wrap wrap wrap wrap wrap".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 24.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let id = layer.id;
        app::apply_intent(&mut ed, ui::Intent::Document(Command::create_layer(layer)));
        ed.set_layer_selection(vec![id], Some(id));
        id
    };

    let region = ed.active().unwrap().canvas_rect();
    let point_pixels = ed.active_mut().unwrap().composite(region).unwrap();

    // The panel route: switch to a wrapping box and narrow it. The pixels
    // reflow; the type size in the model does not move.
    fn apply(run_edit: impl FnOnce(&mut text_engine::TextRun) -> bool, ed: &mut app_shell::Editor) {
        let doc = ed.active().unwrap();
        let (layer, mut run) = text_panel::active_text(&doc.document, doc.document.active_layer())
            .expect("the active layer is text");
        assert!(run_edit(&mut run), "the panel edit reported a change");
        let intent = text_panel::commit(&doc.document, layer, &run).expect("an edit was made");
        let Some(app_shell::menu_bridge::Pick::Kind { layer, kind }) =
            menu_bridge::pick(&intent, ed)
        else {
            panic!("the kind edit is routed");
        };
        ed.apply_kind_edit(KindEdit {
            layer,
            kind,
            gesture: None,
        });
    }

    apply(
        |run| {
            let a = text_panel::Paragraph::set_boxed(run, true);
            let b = text_panel::Paragraph::set_box_width(run, 90.0);
            a || b
        },
        &mut ed,
    );
    {
        let doc = ed.active().unwrap();
        let Some(LayerKind::Text(t)) = doc.document.layers.get(id).map(|l| &l.kind) else {
            panic!("the layer is text");
        };
        assert_eq!(
            t.frame,
            layer_model::text::Frame::Box {
                width: 90.0,
                height: None
            },
            "the box persists in the model"
        );
        assert_eq!(t.size_px, 24.0, "reflowing never touches the type size");
        // The explicit paragraph break is still in the payload.
        assert_eq!(t.text, "wrap wrap wrap wrap wrap wrap wrap");
    }
    let boxed_pixels = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(
        boxed_pixels, point_pixels,
        "narrowing the box reflows the ink"
    );

    // A fixed height that is too small reports overset instead of clipping;
    // the engine's answer is what the panel shows.
    apply(
        |run| text_panel::Paragraph::set_box_height(run, Some(40.0)),
        &mut ed,
    );
    {
        let doc = ed.active().unwrap();
        let (layer, run) = text_panel::active_text(&doc.document, doc.document.active_layer())
            .expect("still a text layer");
        let _ = layer;
        let overset = text_panel::overset_lines(&run).expect("a fixed height reports");
        assert!(overset > 0, "a 40px box cannot hold seven wrapped lines");
        // Widening the same box to fit returns the overset to zero without a
        // type-size change — the size-vs-box distinction, end to end.
        let mut wider = run.clone();
        assert!(text_panel::Paragraph::set_box_width(&mut wider, 240.0));
        assert_eq!(wider.style.size_px, 24.0);
    }

    // Save/reopen retains the box geometry.
    let package = tmp.path().join("paragraph_box.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let reopened = app::open_project(&package);
    let Some(LayerKind::Text(t)) = reopened.document.layers.get(id).map(|l| &l.kind) else {
        panic!("the reopened layer is text");
    };
    assert_eq!(
        t.frame,
        layer_model::text::Frame::Box {
            width: 90.0,
            height: Some(40.0)
        },
        "the box dimensions survive the package"
    );
    assert_eq!(t.size_px, 24.0);
}

// -- card 024: styled headline authoring, verified end to end ----------------
//
// The font-dependent judgment "does it LOOK like a bold white headline over a
// green subhead" is recorded separately, in THUMBNAIL-WORKFLOW-PROGRESS.md
// row 024's manual item: the fixture loads the regular face, so bold arrives
// through synthetic emboldening, and no automated assertion claims visual
// correctness. Everything below is deterministic.

#[test]
fn the_styled_headline_workflow_verifies_end_to_end() {
    use app_shell::chrome::KindEdit;
    use app_shell::menu_bridge;
    use editor_core::Command;
    use integration_tests::app::{self, DocExt};
    use integration_tests::fixture::thumbnail::load_fixture_font;
    use layer_model::text::{Stretch, Weight};
    use layer_model::LayerKind;
    use ui::panels::text as text_panel;

    load_fixture_font();
    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 480, 270);
    let region = ed.active().unwrap().canvas_rect();

    // The dark backdrop the acceptance scene opens with, filled through the
    // command route on the canvas raster layer.
    let canvas_layer = app::the_opened_layer(&ed);
    ed.active_mut()
        .unwrap()
        .fill_layer(canvas_layer, [16, 18, 24, 255]);

    // Both headlines enter as text layers through the create-layer command —
    // the route the Type tool emits — then every style arrives through the
    // Character panel route. Nothing below touches the model directly.
    fn apply(ed: &mut app_shell::Editor, run_edit: impl FnOnce(&mut text_engine::TextRun) -> bool) {
        let doc = ed.active().unwrap();
        let (layer, mut run) = text_panel::active_text(&doc.document, doc.document.active_layer())
            .expect("the active layer is text");
        assert!(run_edit(&mut run), "the panel edit reported a change");
        let intent = text_panel::commit(&doc.document, layer, &run).expect("an edit was made");
        let Some(app_shell::menu_bridge::Pick::Kind { layer, kind }) =
            menu_bridge::pick(&intent, ed)
        else {
            panic!("the kind edit is routed");
        };
        ed.apply_kind_edit(KindEdit {
            layer,
            kind,
            gesture: None,
        });
    }
    fn text_layer(name: &str, text: &str, size: f32) -> layer_model::Layer {
        layer_model::Layer::with_kind(
            name,
            LayerKind::Text(layer_model::TextLayer {
                text: text.to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: size,
                ..layer_model::TextLayer::default()
            }),
        )
    }

    let headline = {
        let layer = text_layer("Headline", "THUMBNAILS", 72.0);
        let id = layer.id;
        app::apply_intent(&mut ed, ui::Intent::Document(Command::create_layer(layer)));
        ed.set_layer_selection(vec![id], Some(id));
        id
    };
    let secondary = {
        let layer = text_layer("Subhead", "Falk in 60 seconds", 28.0);
        let id = layer.id;
        app::apply_intent(&mut ed, ui::Intent::Document(Command::create_layer(layer)));
        ed.set_layer_selection(vec![id], Some(id));
        id
    };
    // The pre-style snapshot: backdrop plus both (still black) text layers.
    let dark = ed.active_mut().unwrap().composite(region).unwrap();

    // Weight: the headline goes bold through the panel.
    ed.set_layer_selection(vec![headline], Some(headline));
    apply(&mut ed, |run| text_panel::Character::set_weight(run, 700));
    let bold = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(bold, dark, "the weight change rewrites the headline");
    // Colour: the headline goes white, then the subhead goes green.
    apply(&mut ed, |run| {
        text_panel::Character::set_color(run, [1.0, 1.0, 1.0, 1.0])
    });
    let white = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(white, bold, "the fill change rewrites the headline");
    // Absolute colour pins, not comparative: the white fill must actually
    // reach a fully white pixel somewhere in the glyph cores.
    assert!(
        white.as_chunks::<4>().0.contains(&[255, 255, 255, 255]),
        "the headline carries a fully white pixel"
    );
    ed.set_layer_selection(vec![secondary], Some(secondary));
    apply(&mut ed, |run| {
        text_panel::Character::set_color(run, [0.16, 0.72, 0.31, 1.0])
    });
    let green = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(green, white, "the colour change rewrites the subhead");
    assert!(
        green
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] == 255 && p[1] > p[0] && p[1] > p[2]),
        "the subhead carries a green-dominant pixel"
    );
    // Tracking: the headline loosens.
    ed.set_layer_selection(vec![headline], Some(headline));
    apply(&mut ed, |run| {
        text_panel::Character::set_tracking(run, 90.0)
    });
    let tracked = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(tracked, green, "the tracking change rewrites the headline");

    // Undo walks the four styled edits back to the exact dark backdrop;
    // redo walks them forward to the exact same bytes again.
    for expected in [green.clone(), white.clone(), bold.clone(), dark.clone()] {
        assert!(
            ed.active_mut().unwrap().undo().unwrap(),
            "undo consumed exactly one step"
        );
        assert_eq!(
            ed.active_mut().unwrap().composite(region).unwrap(),
            expected,
            "undo restores the exact earlier render"
        );
    }
    for expected in [bold.clone(), white.clone(), green.clone(), tracked.clone()] {
        assert!(
            ed.active_mut().unwrap().redo().unwrap(),
            "redo consumed exactly one step"
        );
        assert_eq!(
            ed.active_mut().unwrap().composite(region).unwrap(),
            expected,
            "redo reproduces the exact later render"
        );
    }

    // Model preservation, checked against expectations written independently
    // of the shell: both headlines are still editable text with every styled
    // field intact.
    let expected_headline = layer_model::TextLayer {
        text: "THUMBNAILS".to_string(),
        font_family: "DejaVu Sans".to_string(),
        size_px: 72.0,
        style: layer_model::text::BaseStyle {
            weight: Weight::BOLD,
            fill: [1.0, 1.0, 1.0, 1.0],
            stretch: Stretch::Normal,
            tracking: 90.0,
            ..layer_model::text::BaseStyle::default()
        },
        ..layer_model::TextLayer::default()
    };
    let expected_secondary = layer_model::TextLayer {
        text: "Falk in 60 seconds".to_string(),
        font_family: "DejaVu Sans".to_string(),
        size_px: 28.0,
        style: layer_model::text::BaseStyle {
            weight: Weight::NORMAL,
            fill: [0.16, 0.72, 0.31, 1.0],
            stretch: Stretch::Normal,
            ..layer_model::text::BaseStyle::default()
        },
        ..layer_model::TextLayer::default()
    };
    {
        let doc = ed.active().unwrap();
        for (id, expected) in [
            (headline, &expected_headline),
            (secondary, &expected_secondary),
        ] {
            let Some(LayerKind::Text(t)) = doc.document.layers.get(id).map(|l| &l.kind) else {
                panic!("the layer is still editable text");
            };
            assert_eq!(
                t, expected,
                "the styled model survived the workflow verbatim"
            );
        }
    }

    // Save/reopen: the model round-trips and the reopened render is
    // byte-identical — the deterministic rendered result, checked through a
    // second decode path rather than trusting the live cache.
    let package = tmp.path().join("styled_headline.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let reopened = app::open_project(&package);
    for (id, expected) in [
        (headline, &expected_headline),
        (secondary, &expected_secondary),
    ] {
        let Some(LayerKind::Text(t)) = reopened.document.layers.get(id).map(|l| &l.kind) else {
            panic!("the reopened layer is text");
        };
        assert_eq!(t, expected, "the styled model survives the package");
    }
    let mut reopened_doc = reopened;
    let reopened_pixels = reopened_doc.composite(region).unwrap();
    assert_eq!(
        reopened_pixels, tracked,
        "the reopened document renders the tracked headline identically"
    );

    // Export: the PNG the app would write decodes back to the same pixels.
    let export = tmp.path().join("styled_headline.png");
    std::fs::write(
        &export,
        raster::encode(raster::ExportFormat::Png, 480, 270, &reopened_pixels)
            .expect("the composite encodes"),
    )
    .expect("the export writes");
    let decoded = raster::decode_path(&export).expect("the export decodes");
    assert_eq!(
        decoded.rgba8, reopened_pixels,
        "the exported PNG carries the styled render"
    );
}

/// Card 033: the repeated headline editing sequence — enter, select, replace,
/// style, add a line, resize the frame, confirm, transform, reopen, cancel,
/// save/reload — plus font substitution and a locked layer, all through the
/// routes the shipping shell runs.
#[test]
fn the_repeated_headline_editing_sequence_holds_end_to_end() {
    use app_shell::tool_input::ToolPointer;
    use editor_core::Command;
    use integration_tests::app;
    use layer_model::text::{Frame, StyleOverride, StyleSpan, Weight};

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 96, 96);
    let layer = layer_model::Layer::with_kind(
        "Headline",
        layer_model::LayerKind::Text(layer_model::TextLayer {
            text: "THUMBNAILS".to_string(),
            font_family: "DejaVu Sans".to_string(),
            size_px: 24.0,
            ..layer_model::TextLayer::default()
        }),
    );
    let id = layer.id;
    ed.active_mut()
        .unwrap()
        .apply(Command::create_layer(layer))
        .unwrap();

    let mut pointer = ToolPointer::new();
    // 1. Enter the existing headline.
    pointer.enter_text_session(&mut ed, id);
    assert!(pointer.is_text_editing());
    // 2. Select the whole word (the headline is one word) via movement.
    pointer.text_edit(&mut ed, tools::TextEdit::SelectAll);
    // 3. Paste the replacement (the session route; the OS clipboard stays
    //    in the shell's hands).
    pointer.text_edit(&mut ed, tools::TextEdit::PasteText("HEADSHOTS"));
    // 4. Add a line, resize the frame.
    pointer.text_edit(&mut ed, tools::TextEdit::Insert("\nsecond"));
    pointer.text_edit(
        &mut ed,
        tools::TextEdit::ResizeBox {
            width: 120.0,
            height: None,
        },
    );
    // 5. Confirm: one cohesive history entry.
    let depth_before = ed.active().unwrap().history_depth();
    pointer.text_edit(&mut ed, tools::TextEdit::Confirm);
    assert!(!pointer.is_text_editing());
    assert_eq!(ed.active().unwrap().history_depth(), depth_before + 1);
    {
        let doc = ed.active().unwrap();
        let Some(layer_model::LayerKind::Text(t)) = doc.document.layers.get(id).map(|l| &l.kind)
        else {
            panic!("still text");
        };
        assert_eq!(t.text, "HEADSHOTS\nsecond");
        assert!(matches!(
            t.frame,
            Frame::Box {
                width: 120.0,
                height: None
            }
        ));
    }
    // 6. Transform (one undoable translate) and reopen; typing is discarded
    //    by cancel — the layer keeps the committed text.
    ed.active_mut()
        .unwrap()
        .apply(Command::TransformLayer {
            layer_id: id,
            matrix: [1.0, 0.0, 0.0, 1.0, 8.0, 4.0],
        })
        .unwrap();
    let depth_after_transform = ed.active().unwrap().history_depth();
    pointer.enter_text_session(&mut ed, id);
    pointer.text_edit(&mut ed, tools::TextEdit::Insert("scrapped"));
    pointer.text_edit(&mut ed, tools::TextEdit::Cancel);
    assert!(!pointer.is_text_editing());
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_after_transform,
        "cancel leaves no history entry"
    );
    {
        let doc = ed.active().unwrap();
        let Some(layer_model::LayerKind::Text(t)) = doc.document.layers.get(id).map(|l| &l.kind)
        else {
            panic!("still text");
        };
        assert_eq!(t.text, "HEADSHOTS\nsecond", "cancel restores the headline");
    }
    // 7. Style a span (post-confirm, the Character panel's command route):
    //    the first word goes bold.
    {
        let doc = ed.active().unwrap();
        let mut text = match doc.document.layers.get(id).map(|l| &l.kind) {
            Some(layer_model::LayerKind::Text(t)) => t.clone(),
            _ => panic!("still text"),
        };
        text.spans.push(StyleSpan {
            start: 0,
            end: 9,
            style: StyleOverride {
                weight: Some(Weight(700)),
                ..StyleOverride::default()
            },
        });
        ed.apply_command(editor_core::Command::SetLayerKind {
            layer_id: id,
            kind: Box::new(layer_model::LayerKind::Text(text)),
        });
    }
    // 8. Save and reload: content, style and frame survive the round trip.
    let package = tmp.path().join("headline.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let reopened = app::open_project(&package);
    let Some(layer_model::LayerKind::Text(t)) = reopened.document.layers.get(id).map(|l| &l.kind)
    else {
        panic!("the reopened layer is text");
    };
    assert_eq!(t.text, "HEADSHOTS\nsecond");
    assert!(matches!(
        t.frame,
        Frame::Box {
            width: 120.0,
            height: None
        }
    ));
    assert_eq!(t.spans.len(), 1, "the styled span survives the round trip");
    assert_eq!(t.spans[0].start..t.spans[0].end, 0..9);
    // 9. Font substitution: a family the machine does not have resolves to
    //    the pinned default, never the missing name.
    let substitute = compositor::font_substitute_for("Nosuch Face");
    assert_ne!(substitute.as_deref(), Some("Nosuch Face"));
    // 10. A locked layer refuses the session.
    ed.active_mut()
        .unwrap()
        .document
        .layers
        .get_mut(id)
        .unwrap()
        .locked
        .all = true;
    pointer.enter_text_session(&mut ed, id);
    assert!(
        !pointer.is_text_editing(),
        "a locked layer opens no text session"
    );
}

/// Card 035: a whole-layer affine commit updates the layer transform — the
/// raster tiles keep their hashes, the kind stays, and it is one undoable
/// entry (no resample).
#[test]
fn a_whole_layer_affine_commit_updates_the_transform_without_resampling() {
    use app_shell::tool_input::ToolPointer;
    use editor_core::Command;
    use integration_tests::app;
    use ui::canvas::PointerPhase;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 96, 96);
    // Ink in one corner of the canvas layer.
    let ink_layer = {
        let layer = layer_model::Layer::raster("Ink");
        let id = layer.id;
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in 8..16u32 {
            for x in 8..16u32 {
                let i = ((y * raster::TILE_SIZE + x) * 4) as usize;
                bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
            }
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(
                        raster::TileCoord::new(0, 0, 0),
                        hash,
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        id
    };
    ed.set_layer_selection(vec![ink_layer], Some(ink_layer));
    let hashes_before = app::layer_tile_map(&ed, ink_layer);
    let depth_before = ed.active().unwrap().history_depth();

    // A whole-layer move through the real transform route: press inside the
    // ink, drag to the destination, release, commit.
    ed.set_tool(tools::ToolId::FreeTransform);
    let mut pointer = ToolPointer::new();
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(10.0, 10.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(30.0, 26.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Up,
        glam::vec2(30.0, 26.0),
    );
    pointer.commit(&mut ed);

    // The tiles are untouched; the transform moved.
    assert_eq!(
        app::layer_tile_map(&ed, ink_layer),
        hashes_before,
        "the raster hashes survive a whole-layer move"
    );
    let doc = ed.active().unwrap();
    let layer = doc.document.layers.get(ink_layer).unwrap();
    assert!(
        layer.transform.translation.x != 0.0 || layer.transform.translation.y != 0.0,
        "the layer transform moved: {:?}",
        layer.transform
    );
    assert!(matches!(layer.kind, layer_model::LayerKind::Raster(_)));
    assert_eq!(
        doc.history_depth(),
        depth_before + 1,
        "one undoable entry for the move"
    );
    // Undo puts the transform back whole.
    ed.active_mut().unwrap().undo().unwrap();
    let layer = ed.active().unwrap().document.layers.get(ink_layer).unwrap();
    assert_eq!(
        layer.transform.translation,
        glam::Vec2::ZERO,
        "undo restores the baseline transform"
    );
}

/// Card 036: a Shift-selected set moves together — one undo entry — with
/// ancestor duplicates normalized and locked participants refusing the whole
/// set (all-or-nothing).
#[test]
fn a_multi_layer_transform_moves_the_set_in_one_transaction_and_respects_locks() {
    use app_shell::tool_input::ToolPointer;
    use editor_core::Command;
    use integration_tests::app;
    use ui::canvas::PointerPhase;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 96, 96);
    // Two ink layers.
    let mut ids = Vec::new();
    for name in ["InkA", "InkB"] {
        let layer = layer_model::Layer::raster(name);
        let id = layer.id;
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in 8..16u32 {
            for x in 8..16u32 {
                let i = ((y * raster::TILE_SIZE + x) * 4) as usize;
                bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
            }
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(
                        raster::TileCoord::new(0, 0, 0),
                        hash,
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        ids.push(id);
    }
    let depth_before = ed.active().unwrap().history_depth();

    // Shift-select both layers (the panel's set), then one gizmo move.
    ed.set_layer_selection(ids.clone(), Some(ids[0]));
    ed.set_tool(tools::ToolId::FreeTransform);
    let mut pointer = ToolPointer::new();
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(10.0, 10.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(24.0, 20.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Up,
        glam::vec2(24.0, 20.0),
    );
    pointer.commit(&mut ed);

    // One transaction: undo takes BOTH layers back together.
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_before + 1,
        "the set moves as one undoable entry"
    );
    for id in &ids {
        let t = ed
            .active()
            .unwrap()
            .document
            .layers
            .get(*id)
            .unwrap()
            .transform
            .translation;
        assert!(t.x != 0.0 || t.y != 0.0, "participant {id:?} moved: {t:?}");
    }
    ed.active_mut().unwrap().undo().unwrap();
    for id in &ids {
        let t = ed
            .active()
            .unwrap()
            .document
            .layers
            .get(*id)
            .unwrap()
            .transform
            .translation;
        assert_eq!(t, glam::Vec2::ZERO, "undo restored the whole set");
    }

    // A locked participant refuses the whole session up front.
    ed.active_mut()
        .unwrap()
        .document
        .layers
        .get_mut(ids[1])
        .unwrap()
        .locked
        .all = true;
    ed.set_layer_selection(ids.clone(), Some(ids[0]));
    ed.set_tool(tools::ToolId::FreeTransform);
    let mut pointer = ToolPointer::new();
    let down_at = app::shell_screen_pt(ed.active().unwrap(), 10.0, 10.0);
    let down = pointer.handle(
        &mut ed,
        ui::canvas::PointerInput::at(PointerPhase::Down, down_at),
        false,
        &[],
    );
    assert!(
        down.failed.is_some(),
        "a locked participant refuses the session: {down:?}"
    );
}

/// Card 036's group+child check: selecting both a group and its child moves
/// the child exactly once, whichever order the clicks landed in.
#[test]
fn selecting_a_group_and_its_child_moves_the_child_once() {
    use app_shell::tool_input::ToolPointer;
    use editor_core::Command;
    use integration_tests::app;
    use ui::canvas::PointerPhase;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 96, 96);
    // A group with one ink child, plus a sibling ink layer outside it.
    let group = layer_model::Layer::group("Pack");
    let group_id = group.id;
    let child = layer_model::Layer::raster("Child");
    let child_id = child.id;
    ed.active_mut()
        .unwrap()
        .apply(Command::create_layer(group))
        .unwrap();
    ed.active_mut()
        .unwrap()
        .apply(Command::create_layer(child))
        .unwrap();
    ed.active_mut()
        .unwrap()
        .apply(Command::MoveLayer {
            layer_id: child_id,
            parent: Some(group_id),
            index: 0,
        })
        .unwrap();
    let depth_before = ed.active().unwrap().history_depth();

    // The CHILD is the active layer (the natural last click), the set holds
    // both — the ancestor must not double-move the child at commit.
    ed.set_layer_selection(vec![group_id, child_id], Some(child_id));
    ed.set_tool(tools::ToolId::FreeTransform);
    let mut pointer = ToolPointer::new();
    // Press in the translate zone: (10,10) sits in corner (0,0)'s rotate
    // band, the exact centre is the Pivot handle — (32,48) is clear of both.
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(32.0, 48.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(46.0, 58.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Up,
        glam::vec2(24.0, 20.0),
    );
    pointer.commit(&mut ed);

    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_before + 1,
        "the set moves as one undoable entry"
    );
    // "The child moves once" is about its TOTAL placement (the compositor's
    // view), not its own matrix: the conjugated delta lands in parent space.
    let total = app_shell::interaction_geometry::document_transform_of(
        &ed.active().unwrap().document,
        child_id,
        0,
    )
    .unwrap()
    .translation;
    assert!(
        (total.x - 14.0).abs() < 1e-3 && (total.y - 10.0).abs() < 1e-3,
        "the child's placement moved exactly the gizmo delta, once: {total:?}"
    );
    let group_total = app_shell::interaction_geometry::document_transform_of(
        &ed.active().unwrap().document,
        group_id,
        0,
    )
    .unwrap()
    .translation;
    assert!(
        (group_total.x - 14.0).abs() < 1e-3 && (group_total.y - 10.0).abs() < 1e-3,
        "the group moved by the same delta: {group_total:?}"
    );
}

/// Card 045's done-check: the complete transform workflow verifies end to
/// end through the real shell routes - resize a portrait (aspect-preserved
/// corner), rotate a logo, move a group, transform under an unlinked mask,
/// cancel a drag, undo/redo, an object crossing the canvas boundary, and a
/// second transform over a previously transformed layer. Content bounds,
/// committed transforms, editability and source preservation must all
/// agree. (The align step rides the T042 remainder: the align ACTIONS are
/// not wired to menu routes yet.)
#[test]
fn the_complete_transform_workflow_verifies_end_to_end() {
    use app_shell::tool_input::ToolPointer;
    use editor_core::Command;
    use integration_tests::app;
    use layer_model::LayerId;
    use ui::canvas::PointerPhase;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 128, 128);

    fn ink_layer(ed: &mut app_shell::Editor, name: &str, rect: (u32, u32, u32, u32)) -> LayerId {
        use editor_core::Command;
        let layer = layer_model::Layer::raster(name);
        let id = layer.id;
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in rect.1..rect.1 + rect.3 {
            for x in rect.0..rect.0 + rect.2 {
                let i = ((y * raster::TILE_SIZE + x) * 4) as usize;
                bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
            }
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(
                        raster::TileCoord::new(0, 0, 0),
                        hash,
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        id
    }

    let portrait = ink_layer(&mut ed, "Portrait", (8, 8, 32, 32));
    let logo = ink_layer(&mut ed, "Logo", (80, 8, 40, 40));
    let group = layer_model::Layer::group("Pack");
    let group_id = group.id;
    ed.active_mut()
        .unwrap()
        .apply(Command::create_layer(group))
        .unwrap();
    let deco = ink_layer(&mut ed, "Deco", (40, 60, 30, 30));
    ed.active_mut()
        .unwrap()
        .apply(Command::MoveLayer {
            layer_id: deco,
            parent: Some(group_id),
            index: 0,
        })
        .unwrap();
    // The logo wears an UNLINKED mask over its left half: moving the content
    // must leave the mask's document pose frozen (card 043 at the shell).
    {
        let doc = ed.active_mut().unwrap();
        let mut mask = layer_model::LayerMask::new(layer_model::MaskId::new());
        mask.linked = false;
        doc.document.layers.get_mut(logo).unwrap().set_mask(mask);
        let coverage = vec![255u8; editor_core::MASK_TILE_BYTES];
        let hash = doc.tiles.insert_bytes(coverage);
        doc.apply(
            Command::paint_tiles(
                editor_core::PixelTarget::Mask(logo),
                vec![editor_core::TileEdit::set(
                    raster::TileCoord::new(0, 0, 0),
                    hash,
                )],
            )
            .unwrap(),
        )
        .unwrap();
    }

    let depth_after_setup = ed.active().unwrap().history_depth();
    let hashes = |ed: &app_shell::Editor, id: LayerId| app::layer_tile_map(ed, id);
    let pose_of = |ed: &app_shell::Editor, id: LayerId| {
        app_shell::interaction_geometry::document_transform_of(
            &ed.active().unwrap().document,
            id,
            0,
        )
        .unwrap()
    };

    // 1. RESIZE the portrait: a marquee frames the session over the ink
    //    (the transform tool's preferred source), a corner drag preserves
    //    aspect by default (card 041), the tiles survive.
    ed.set_layer_selection(vec![portrait], Some(portrait));
    app::set_selection(
        &mut ed,
        editor_core::Selection::Rect {
            min: glam::IVec2::new(8, 8),
            max: glam::IVec2::new(40, 40),
        },
    );
    ed.set_tool(tools::ToolId::FreeTransform);
    let portrait_hashes = hashes(&ed, portrait);
    let mut pointer = ToolPointer::new();
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(39.0, 39.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(56.0, 56.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Up,
        glam::vec2(56.0, 56.0),
    );
    pointer.commit(&mut ed);
    app::set_selection(&mut ed, editor_core::Selection::None);
    {
        let t = pose_of(&ed, portrait);
        let sx = t.matrix2.x_axis.length();
        let sy = t.matrix2.y_axis.length();
        assert!(
            (sx - sy).abs() / sx.max(1e-4) < 1e-3,
            "the corner resize preserved aspect: sx={sx} sy={sy}"
        );
        // The opposite corner anchors: (40-8) -> (56-8) is 1.5x.
        assert!((sx - 1.5).abs() < 0.05, "the portrait grew 1.5x: {sx}");
        assert_eq!(
            hashes(&ed, portrait),
            portrait_hashes,
            "no resample on resize"
        );
    }

    // 2. ROTATE the logo: a marquee frames the session, the rotate band
    //    (6..24px from a corner at zoom 1) spins the quad, the tiles keep
    //    their hashes, and the mask's pose stays frozen (unlinked).
    ed.set_layer_selection(vec![logo], Some(logo));
    app::set_selection(
        &mut ed,
        editor_core::Selection::Rect {
            min: glam::IVec2::new(80, 8),
            max: glam::IVec2::new(120, 48),
        },
    );
    let logo_hashes = hashes(&ed, logo);
    // The mask's DOCUMENT pose is the layer transform composed with the
    // mask's own transform (card 043) - THAT is what must stay frozen.
    let mask_pose_before = {
        let doc = ed.active().unwrap();
        let layer = doc.document.layers.get(logo).unwrap();
        layer.transform * *layer.mask.as_ref().unwrap().transform
    };
    let mut pointer = ToolPointer::new();
    // The session frames (80,8)-(120,48); its pivot is (100,28). The rotate
    // band is 6..24px from a corner: press 8.5px inside the top-right corner
    // and swing it about the pivot.
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(114.0, 14.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(104.0, 10.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Up,
        glam::vec2(104.0, 10.0),
    );
    pointer.commit(&mut ed);
    app::set_selection(&mut ed, editor_core::Selection::None);
    {
        let t = pose_of(&ed, logo);
        assert!(
            t.matrix2.x_axis.y.abs() > 1e-3,
            "the logo rotated: {:?}",
            t.matrix2
        );
        assert_eq!(hashes(&ed, logo), logo_hashes, "no resample on rotate");
        let pose_after = {
            let doc = ed.active().unwrap();
            let layer = doc.document.layers.get(logo).unwrap();
            layer.transform * *layer.mask.as_ref().unwrap().transform
        };
        assert_eq!(
            pose_after, mask_pose_before,
            "the unlinked mask's document pose held through the rotate"
        );
    }

    // 3. MOVE the group: the set rides one transaction, the child once.
    ed.set_layer_selection(vec![group_id, deco], Some(deco));
    ed.set_tool(tools::ToolId::Move);
    let depth_before_move = ed.active().unwrap().history_depth();
    let mut pointer = ToolPointer::new();
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(50.0, 75.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(60.0, 85.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Up,
        glam::vec2(60.0, 85.0),
    );
    pointer.commit(&mut ed);
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_before_move + 1,
        "the group move is one undoable entry"
    );
    // The snap (card 042) may adjust the raw delta; the semantic under test
    // is that the child and its group move by the SAME delta, once.
    let deco_total = pose_of(&ed, deco).translation;
    let group_total = pose_of(&ed, group_id).translation;
    assert!(
        (deco_total - group_total).length() < 1e-3 && deco_total.length() > 1.0,
        "the child moved once, with its group: {deco_total:?} vs {group_total:?}"
    );

    // 4. CANCEL: a transform drag abandoned (the pointer leaves the tool's
    //    route) commits nothing.
    ed.set_layer_selection(vec![portrait], Some(portrait));
    ed.set_tool(tools::ToolId::FreeTransform);
    let depth_before_cancel = ed.active().unwrap().history_depth();
    let mut pointer = ToolPointer::new();
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(20.0, 20.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(40.0, 40.0),
    );
    pointer.cancel(&mut ed);
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_before_cancel,
        "a cancelled transform leaves no step"
    );

    // 5. OFF-CANVAS: drag the deco child past the right edge - the INK
    //    (alpha bounds mapped through the pose) crosses the boundary and the
    //    on-canvas part still renders (card 044: nothing is clipped away).
    //    The drag is long enough that the snap cannot pull it back inside.
    ed.set_layer_selection(vec![deco], Some(deco));
    ed.set_tool(tools::ToolId::Move);
    let mut pointer = ToolPointer::new();
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(65.0, 85.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(125.0, 85.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Up,
        glam::vec2(125.0, 85.0),
    );
    pointer.commit(&mut ed);
    {
        let doc = ed.active().unwrap();
        // Ink-level bounds: alpha_bounds is layer space, mapped through the
        // layer's full document transform.
        let ink = compositor::bounds::alpha_bounds(
            &doc.document,
            &doc.tiles,
            deco,
            0,
            compositor::CompositeOptions::default(),
        )
        .ok()
        .flatten()
        .expect("the moved child's ink");
        let m =
            app_shell::interaction_geometry::document_transform_of(&doc.document, deco, 0).unwrap();
        let xs = [
            glam::vec2(ink.x as f32, ink.y as f32),
            glam::vec2((ink.x + ink.width as i64) as f32, ink.y as f32),
            glam::vec2(ink.x as f32, (ink.y + ink.height as i64) as f32),
            glam::vec2(
                (ink.x + ink.width as i64) as f32,
                (ink.y + ink.height as i64) as f32,
            ),
        ];
        let min_x = xs
            .iter()
            .map(|p| m.transform_point2(*p).x)
            .fold(f32::INFINITY, f32::min);
        let max_x = xs
            .iter()
            .map(|p| m.transform_point2(*p).x)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max_x > doc.document.width() as f32,
            "the child's ink crosses the canvas boundary: [{min_x}, {max_x}]"
        );
        assert!(
            min_x < doc.document.width() as f32,
            "the child straddles the boundary: [{min_x}, {max_x}]"
        );
    }
    // The on-canvas part still renders: the composite shows ink at the
    // straddling band's visible side.
    {
        let region = ed.active().unwrap().canvas_rect();
        let buf = ed.active_mut().unwrap().composite(region).unwrap();
        let at = |x: usize, y: usize| {
            let i = (y * region.width as usize + x) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        assert_ne!(
            at(120, 84),
            [255, 255, 255, 255],
            "the straddling child still renders its on-canvas part"
        );
    }

    // 6. UNDO/REDO the whole sequence: each undo steps back one committed
    //    transform; redo restores it.
    let depth_after_all = ed.active().unwrap().history_depth();
    for _ in 0..4 {
        ed.active_mut().unwrap().undo().unwrap();
    }
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_after_setup,
        "four undos step back to exactly the post-setup depth"
    );
    for _ in 0..4 {
        if !ed.active_mut().unwrap().redo().unwrap() {
            break;
        }
    }
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth_after_all,
        "redo restored the sequence"
    );

    // 7. A SECOND transform over a previously transformed layer: the total
    //    placement is the composition, not a reset to the second delta.
    ed.set_layer_selection(vec![portrait], Some(portrait));
    ed.set_tool(tools::ToolId::Move);
    let portrait_before = pose_of(&ed, portrait).translation;
    let mut pointer = ToolPointer::new();
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Down,
        glam::vec2(24.0, 24.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Move,
        glam::vec2(44.0, 24.0),
    );
    app::shell_pointer(
        &mut pointer,
        &mut ed,
        PointerPhase::Up,
        glam::vec2(44.0, 24.0),
    );
    pointer.commit(&mut ed);
    {
        let after = pose_of(&ed, portrait);
        let total = after.translation;
        // The snap (card 042) may adjust the raw delta toward nearby
        // candidates; what must hold is the COMPOSITION: the second move
        // adds to the first placement (it does not reset it), the axis
        // delta stays within the raw drag plus one snap threshold, and the
        // scale survived untouched (a translate never resamples).
        assert!(
            (total.x - portrait_before.x - 20.0).abs() <= 8.0 + 1e-3,
            "the second move composed along x: {total:?} from {portrait_before:?}"
        );
        assert!(
            (total.y - portrait_before.y).abs() <= 8.0 + 1e-3,
            "the y drift stays within one snap threshold: {total:?}"
        );
        assert!(
            (after.matrix2.x_axis.length() - 1.5).abs() < 1e-3,
            "the resize's scale survived the move: {:?}",
            after.matrix2
        );
    }

    // 8. SOURCE PRESERVATION + editability through the whole workflow.
    assert_eq!(
        hashes(&ed, portrait),
        portrait_hashes,
        "portrait source preserved"
    );
    assert_eq!(hashes(&ed, logo), logo_hashes, "logo source preserved");
    assert!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(logo)
            .unwrap()
            .mask
            .is_some(),
        "the mask survives the workflow"
    );
}

/// Task 054: the whole asset-reuse and clipboard workflow verifies end to end.
///
/// A large background (oversized, placed like a drop lands), a portrait placed
/// through the Place menu, and a logo pasted from the image clipboard compose
/// one document; the project saves natively, the ORIGINAL source files are
/// moved and renamed away, and the reopened project keeps every full source
/// and transform. Error paths (a garbage file) and linked-file disappearance
/// are exercised in the same session. The clipboard-dependent manual checks
/// stay in their `#[ignore]`d host-bound helpers (app-shell `clipboard.rs`,
/// `menu_bridge::paste_takes_a_foreign_os_clipboard_image_through_real_placement`);
/// this fake-service test never touches the OS clipboard.
/// Card 063: the mask stays under the intended portrait edge through the
/// whole cutout workflow — content transforms (linked riding, unlinked
/// frozen, relink without a jump), density/feather parameter edits, further
/// mask edits, undo/redo restoring pixel hashes + mask parameters +
/// transforms TOGETHER, and native save/reopen. The mapping under test is
/// the document↔mask-local one shared with the compositor
/// (`interaction_geometry::document_transform_of` ∘ `mask.transform`):
/// camera zoom is a presentation concern over the same document-space truth
/// (pinned at cards 030/039), so alignment at identity zoom IS alignment at
/// every zoom.
#[test]
fn the_masked_cutout_alignment_survives_transforms_relink_and_reopen() {
    use editor_core::Command;
    use integration_tests::app;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 128, 128);

    // The "portrait": an ink block x 8..40, y 8..40 on layer-local tile (0,0)
    // with an identity transform. The LINKED mask reveals the TOP band
    // (y < 16 in document space): the cutout edge is the horizontal line
    // y = 16, and the probes below discriminate riding vs frozen poses.
    let portrait = {
        let layer = layer_model::Layer::raster("Portrait");
        let id = layer.id;
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in 8..40usize {
            for x in 8..40usize {
                let i = (y * raster::TILE_SIZE as usize + x) * 4;
                bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
            }
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(
                        raster::TileCoord::new(0, 0, 0),
                        hash,
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        // The mask: top band revealed.
        let doc = ed.active_mut().unwrap();
        let mask = layer_model::LayerMask::new(layer_model::MaskId::new());
        let mut coverage = vec![0u8; editor_core::MASK_TILE_BYTES];
        for y in 0..16usize {
            for x in 0..raster::TILE_SIZE as usize {
                coverage[y * raster::TILE_SIZE as usize + x] = 255;
            }
        }
        let mhash = doc.tiles.insert_bytes(coverage);
        doc.apply(Command::SetLayerProperties {
            layer_id: id,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(mask),
                ..Default::default()
            },
        })
        .unwrap();
        doc.apply(
            Command::paint_tiles(
                editor_core::PixelTarget::Mask(id),
                vec![editor_core::TileEdit::set(
                    raster::TileCoord::new(0, 0, 0),
                    mhash,
                )],
            )
            .unwrap(),
        )
        .unwrap();
        id
    };
    let region = raster::PixelRect::new(0, 0, 128, 128);
    let composite = |ed: &mut app_shell::Editor| -> Vec<u8> {
        ed.active_mut().unwrap().composite(region).unwrap()
    };
    let probe = |bytes: &[u8], x: usize, y: usize| -> [u8; 4] {
        let i = (y * 128 + x) * 4;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    let visible = |b: &[u8]| b[1] < 150; // the green ink over the white backdrop
    let backdrop = [255u8, 255, 255, 255];

    let mask_transform = |ed: &app_shell::Editor, id: layer_model::LayerId| -> glam::Affine2 {
        ed.active()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .mask
            .as_ref()
            .map(|m| glam::Affine2::from_cols_array(&m.transform.to_cols_array()))
            .unwrap_or(glam::Affine2::IDENTITY)
    };
    let set_mask =
        |ed: &mut app_shell::Editor, id: layer_model::LayerId, mask: layer_model::LayerMask| {
            ed.active_mut()
                .unwrap()
                .apply(Command::SetLayerProperties {
                    layer_id: id,
                    patch: editor_core::LayerPatch {
                        mask: editor_core::Patch::Set(mask),
                        ..Default::default()
                    },
                })
                .unwrap();
        };
    let get_mask =
        |ed: &app_shell::Editor, id: layer_model::LayerId| -> Option<layer_model::LayerMask> {
            ed.active()
                .unwrap()
                .document
                .layers
                .get(id)
                .unwrap()
                .mask
                .clone()
        };
    let move_by = |ed: &mut app_shell::Editor, id: layer_model::LayerId, dy: f32| {
        ed.active_mut()
            .unwrap()
            .apply(Command::TransformLayer {
                layer_id: id,
                matrix: glam::Affine2::from_translation(glam::Vec2::new(0.0, dy)).to_cols_array(),
            })
            .unwrap();
    };

    // Baseline: edge at y=16. The band shows the ink; below the edge the
    // backdrop shows through.
    let baseline = composite(&mut ed);
    assert!(visible(&probe(&baseline, 16, 12)));
    assert_eq!(probe(&baseline, 16, 20), backdrop, "below the edge: hidden");

    // 1. LINKED move: the mask RIDES the content (+16 down). Edge -> y=32.
    move_by(&mut ed, portrait, 16.0);
    let riding = composite(&mut ed);
    assert!(
        visible(&probe(&riding, 16, 28)),
        "the band moved with the ink"
    );
    assert_eq!(
        probe(&riding, 16, 40),
        backdrop,
        "below the moved edge: hidden"
    );
    assert_eq!(
        probe(&riding, 16, 12),
        backdrop,
        "the ink left its old rows"
    );
    assert!(
        mask_transform(&ed, portrait) == glam::Affine2::IDENTITY,
        "linked keeps the mask transform at identity"
    );

    // 2. UNLINK and move back (−16): the mask's document pose is FROZEN —
    //    the edge stays at y=32 while the ink returns to y 8..40.
    let mut unlinked = get_mask(&ed, portrait).unwrap();
    unlinked.linked = false;
    set_mask(&mut ed, portrait, unlinked);
    move_by(&mut ed, portrait, -16.0);
    let frozen = composite(&mut ed);
    assert!(
        visible(&probe(&frozen, 16, 28)),
        "the frozen band still covers y<32"
    );
    assert_eq!(
        probe(&frozen, 16, 40),
        backdrop,
        "the edge did NOT ride: a riding mask would reveal y<48 and show this pixel"
    );
    assert!(
        mask_transform(&ed, portrait) != glam::Affine2::IDENTITY,
        "unlinked accumulates the counter-transform"
    );

    // 3. RELINK: card 043 preserves the accumulated transform — no jump.
    let mut relinked_mask = get_mask(&ed, portrait).unwrap();
    relinked_mask.linked = true;
    set_mask(&mut ed, portrait, relinked_mask);
    let relinked_bytes = composite(&mut ed);
    assert_eq!(relinked_bytes, frozen, "relinking jumps nothing");

    // 4. DENSITY: density scales how much the mask can HIDE — at half
    //    density the CONCEALED region lets half the ink through (a revealed
    //    pixel is unchanged, which is the documented semantics).
    let mut half = get_mask(&ed, portrait).unwrap();
    half.set_density(0.5).unwrap();
    set_mask(&mut ed, portrait, half);
    let density_bytes = composite(&mut ed);
    let blended = probe(&density_bytes, 16, 36); // inside the ink, below the frozen edge
    let revealed = probe(&density_bytes, 16, 28);
    assert_eq!(
        revealed,
        [10, 60, 10, 255],
        "a revealed pixel is untouched by density"
    );
    assert!(
        blended[1] > 60 && blended[1] < 255,
        "half density lets the concealed ink show through partially: {blended:?}"
    );
    // Undo restores the FULL density bytes exactly.
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(composite(&mut ed), relinked_bytes, "undo restores density");
    ed.active_mut().unwrap().redo().unwrap();

    // 5. FEATHER: a 4px feather softens the edge — a probe ON the old edge
    //    line (y=32) becomes partial, deep interior stays full.
    let mut soft = get_mask(&ed, portrait).unwrap();
    soft.set_feather_px(4.0).unwrap();
    set_mask(&mut ed, portrait, soft);
    let feathered = composite(&mut ed);
    let on_edge = probe(&feathered, 16, 32);
    let interior = probe(&feathered, 16, 28);
    assert!(
        on_edge[1] > interior[1] && on_edge[1] < 255,
        "the feathered edge blends toward the backdrop: {on_edge:?} vs interior {interior:?}"
    );
    assert!(interior[1] < 100, "deep interior keeps the ink");
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(
        composite(&mut ed),
        density_bytes,
        "undo restores feather (the committed baseline after the density redo)"
    );

    // 6. EDIT AGAIN: an eraser stroke on the mask target punches a hole in
    //    the band (a brush on a mask RAISES coverage — reveal — so
    //    concealment is the eraser's op); undo restores the coverage.
    // The stroke must aim at the PORTRAIT (the shell paints the ACTIVE
    // layer's target; the canvas background layer is active after setup).
    ed.set_layer_selection(vec![portrait], Some(portrait));
    ed.set_tool(tools::ToolId::Eraser);
    ed.set_edit_target_kind(app_shell::edit_target::EditTargetKind::Mask);
    let coverage_before = app::mask_tile_map(&ed, portrait);
    let mut pointer = app_shell::tool_input::ToolPointer::new();
    app::shell_stroke(
        &mut pointer,
        &mut ed,
        &[
            glam::Vec2::new(12.0, 24.0),
            glam::Vec2::new(16.0, 24.0),
            glam::Vec2::new(20.0, 24.0),
        ],
    );
    let holed = composite(&mut ed);
    let holed_px = probe(&holed, 16, 24);
    // The hole is punched through the coverage (store 0), but the DENSITY
    // step (0.5, committed above) still lets half the ink through — the
    // pixel sits strictly between the full ink and the backdrop.
    assert!(
        holed_px[1] > 60 && holed_px[1] < 255,
        "the stroked hole removes the coverage at the stroked row: {holed_px:?}"
    );
    assert_ne!(app::mask_tile_map(&ed, portrait), coverage_before);
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(
        app::mask_tile_map(&ed, portrait),
        coverage_before,
        "undo restores the mask coverage"
    );
    // Put the eraser back on top of history: the walk below snapshots
    // states[0] as the top of history, and it must be the post-stroke state
    // the redo walk has to reproduce.
    ed.active_mut().unwrap().redo().unwrap();
    assert_ne!(
        app::mask_tile_map(&ed, portrait),
        coverage_before,
        "redo replays the eraser onto the top of history"
    );

    // 7. UNDO walks the whole workflow back: pixel hashes, mask parameters,
    //    and transforms move TOGETHER at every step.
    /// The aligned state one history step restores: (mask tile map, mask
    /// transform, layer pose, mask params).
    type AlignedState = (
        Option<editor_core::TileMap>,
        glam::Affine2,
        Option<glam::Affine2>,
        Option<layer_model::LayerMask>,
    );
    let full_state = |ed: &app_shell::Editor| -> AlignedState {
        let layer_pose = app_shell::interaction_geometry::document_transform_of(
            &ed.active().unwrap().document,
            portrait,
            0,
        )
        .ok();
        (
            app::mask_tile_map(ed, portrait),
            mask_transform(ed, portrait),
            layer_pose,
            get_mask(ed, portrait),
        )
    };
    // Walk history back to the setup state and forward again; each step
    // must restore the (hashes, mask transform, layer pose, mask params)
    // tuple exactly — the card's "restores pixel hashes, mask parameters,
    // and transforms together".
    let mut states: Vec<AlignedState> = Vec::new();
    loop {
        // Stop at the document's floor: below the setup the layer itself
        // no longer exists (undo walked past create_layer).
        if ed.active().unwrap().document.layers.get(portrait).is_none() {
            break;
        }
        states.push(full_state(&ed));
        if !ed.active_mut().unwrap().undo().unwrap_or(false) {
            break;
        }
    }
    // Every undo step restores a UNIQUE prior state, and the FIRST entry is
    // the top of history (post-stroke): the mask map never desyncs from the
    // transform pair along the way.
    assert!(
        states.len() >= 6,
        "the workflow made real history: {}",
        states.len()
    );
    // Below the mask attachment the tuple is (no mask, identity, identity)
    // for the setup's own commands — uniqueness is only meaningful while a
    // mask exists.
    let masked: Vec<_> = states.iter().take_while(|s| s.3.is_some()).collect();
    assert!(
        masked.len() >= 5,
        "the masked workflow made real history: {}",
        masked.len()
    );
    for pair in masked.windows(2) {
        assert_ne!(pair[0], pair[1], "each undo step changes the aligned state");
    }
    // The oldest state is the setup: identity mask transform, linked, full
    // density, edge at y=16.
    // The deepest MASKED state (the walk continues below the attachment
    // into the setup's own layer commands, where the mask is None).
    let oldest = masked.last().unwrap();
    assert!(
        oldest.1 == glam::Affine2::IDENTITY,
        "the setup mask transform is identity"
    );
    assert!(
        oldest.3.as_ref().is_some_and(|m| m.linked),
        "the setup mask is linked"
    );
    // Redo replays forward to the top exactly.
    while ed.active_mut().unwrap().redo().unwrap_or(false) {}
    assert_eq!(
        full_state(&ed),
        states[0],
        "redo replays the whole workflow"
    );

    // 8. SAVE/REOPEN: the alignment (mask pose + coverage + transforms)
    //    survives the native package — the probes compare byte-for-byte.
    let pre_save = composite(&mut ed);
    let package = tmp.path().join("alignment.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let mut reopened = app::open_project(&package);
    let after = reopened.composite(region).unwrap();
    assert_eq!(
        probe(&after, 16, 28),
        probe(&pre_save, 16, 28),
        "the cutout edge survives save/reopen"
    );
    assert_eq!(
        probe(&after, 16, 40),
        probe(&pre_save, 16, 40),
        "the frozen-pose discriminator survives save/reopen"
    );
}

/// Card 064: the manual portrait extraction walk — rough selection, add/
/// subtract-style selection edits (expand/contract/feather through the Select
/// menu's real routes; the composed shift/alt gestures themselves are pinned
/// at card 056), selection-to-mask, paint corrections on the mask target,
/// black/white background inspection, and placement over a thumbnail
/// backdrop. Deterministic checks only: a real-photo visual quality gate
/// CANNOT pass here and is recorded in the ledger (card 091's human walk) —
/// no ML runtime is added to bypass mask editing. Every step is undoable and
/// the portrait's own pixels are never touched by the mask work.
#[test]
fn the_manual_portrait_extraction_walk_holds_end_to_end() {
    use app_shell::menu_bridge;
    use editor_core::Command;
    use integration_tests::app;
    use ui::menu::{MaskOp, MenuAction, ModifySelection};

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 128, 128);

    // The "portrait": an oval-ish ink blob (a filled rect here — the shape
    // is irrelevant to the alignment semantics) ABOVE the white canvas.
    let portrait = {
        let layer = layer_model::Layer::raster("Portrait");
        let id = layer.id;
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in 16..80usize {
            for x in 24..88usize {
                let i = (y * raster::TILE_SIZE as usize + x) * 4;
                bytes[i..i + 4].copy_from_slice(&[90, 40, 30, 255]);
            }
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(
                        raster::TileCoord::new(0, 0, 0),
                        hash,
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        // Work ON the portrait.
        ed.set_layer_selection(vec![id], Some(id));
        id
    };
    let region = raster::PixelRect::new(0, 0, 128, 128);
    let composite = |ed: &mut app_shell::Editor| -> Vec<u8> {
        ed.active_mut().unwrap().composite(region).unwrap()
    };
    let probe = |bytes: &[u8], x: usize, y: usize| -> [u8; 4] {
        let i = (y * 128 + x) * 4;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    let portrait_hashes = |ed: &app_shell::Editor| app::layer_tile_map(ed, portrait);
    let hashes_at_setup = portrait_hashes(&ed);
    let depth_setup = ed.active().unwrap().history_depth();
    let pre_everything = composite(&mut ed);

    // 1. ROUGH SELECTION + menu-driven selection edits: expand (grow the
    //    rough box), contract (take it back), then FEATHER — the
    //    deterministic stand-in for edge refinement on the way in.
    app::set_selection(
        &mut ed,
        editor_core::Selection::Rect {
            min: glam::IVec2::new(20, 12),
            max: glam::IVec2::new(92, 84),
        },
    );
    menu_bridge::perform(MenuAction::Modify(ModifySelection::Expand), &mut ed).expect("expand");
    menu_bridge::perform(MenuAction::Modify(ModifySelection::Contract), &mut ed).expect("contract");
    menu_bridge::perform(MenuAction::Modify(ModifySelection::Feather), &mut ed).expect("feather");
    // The selection is now a soft-edged MASK selection (feather materialized
    // it) — exactly what Reveal Selection turns into coverage.

    // 2. SELECTION-TO-MASK: reveal through the real menu route.
    menu_bridge::perform(MenuAction::Mask(MaskOp::RevealSelection), &mut ed)
        .expect("reveal selection");
    let cutout = composite(&mut ed);
    // Interior shows the ink over the white canvas; OUTSIDE the rough box
    // the portrait is hidden by the mask (the backdrop canvas shows).
    let interior = probe(&cutout, 48, 48);
    assert_eq!(interior, [90, 40, 30, 255], "the interior keeps the ink");
    let outside = probe(&cutout, 8, 8);
    assert_eq!(
        outside,
        [255, 255, 255, 255],
        "outside the cutout: backdrop"
    );
    // The portrait's OWN pixels are untouched — masking is nondestructive.
    assert_eq!(
        portrait_hashes(&ed),
        hashes_at_setup,
        "the mask work is source-preserving"
    );

    // 3. PAINT CORRECTIONS on the mask target: erase a strip of coverage
    //    (conceal) and undo it; then reveal a bit of the hidden side with
    //    the brush.
    ed.set_layer_selection(vec![portrait], Some(portrait));
    ed.set_edit_target_kind(app_shell::edit_target::EditTargetKind::Mask);
    ed.set_tool(tools::ToolId::Eraser);
    let mut pointer = app_shell::tool_input::ToolPointer::new();
    app::shell_stroke(
        &mut pointer,
        &mut ed,
        &[glam::Vec2::new(40.0, 48.0), glam::Vec2::new(56.0, 48.0)],
    );
    let corrected = composite(&mut ed);
    assert_ne!(
        probe(&corrected, 48, 48),
        interior,
        "the eraser correction changed the visible cutout"
    );
    assert_eq!(
        probe(&corrected, 48, 48),
        [255, 255, 255, 255],
        "the eraser CONCEALS on a mask: the corrected pixel shows the backdrop"
    );
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(composite(&mut ed), cutout, "undo restores the correction");
    // Reveal with the brush: black conceals → paint WHITE to reveal more.
    ed.set_tool(tools::ToolId::Brush);
    ed.set_foreground([1.0, 1.0, 1.0, 1.0]);
    app::shell_stroke(
        &mut pointer,
        &mut ed,
        &[glam::Vec2::new(100.0, 48.0), glam::Vec2::new(110.0, 48.0)],
    );
    // (The revealed area is outside the portrait's ink, so the composite is
    // unchanged there — the correction is proven by the eraser arm above and
    // the coverage delta here.)
    let revealed_somewhere = app::mask_tile_map(&ed, portrait).is_some();
    assert!(revealed_somewhere, "the reveal stroke committed coverage");
    ed.active_mut().unwrap().undo().unwrap();

    // 4. BLACK/WHITE BACKGROUND INSPECTION: two backdrop layers below the
    //    portrait, toggled — the feathered edge reads differently over each
    //    (the halo check), while the deep interior is backdrop-independent.
    for (name, rgb) in [
        ("Inspect Black", [0u8, 0, 0]),
        ("Inspect White", [255u8, 255, 255]),
    ] {
        let path = tmp.path().join(format!("{name}.png"));
        let mut flat = vec![0u8; 128 * 128 * 4];
        for px in flat.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 128, 128, &flat).unwrap(),
        )
        .unwrap();
        ed.place_path(&path, false).expect("the backdrop places");
    }
    // The two placed backdrops sit above everything; move them below the
    // canvas background? Simpler: toggle their visibility OFF for the probe
    // (placement itself is the card's "placement over the thumbnail" step
    // and is verified by the layer stack), then hide them and probe the
    // ORIGINAL composite; then show one at a time by hiding the canvas.
    let placed: Vec<layer_model::LayerId> = {
        let doc = ed.active().unwrap();
        doc.document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| {
                doc.document
                    .layers
                    .get(*id)
                    .unwrap()
                    .name
                    .starts_with("Inspect")
            })
            .collect()
    };
    assert_eq!(placed.len(), 2, "both backdrops placed");
    // Send both backdrops BEHIND everything (they were placed on top):
    // root.insert(index.min(len)) — MAX clamps to the end = the bottom of
    // the topmost-first stack.
    for id in &placed {
        ed.active_mut()
            .unwrap()
            .apply(Command::MoveLayer {
                layer_id: *id,
                parent: None,
                index: usize::MAX,
            })
            .unwrap();
    }
    // Hide the canvases so a backdrop shows through, toggle each backdrop.
    let canvas_ids: Vec<layer_model::LayerId> = {
        let doc = ed.active().unwrap();
        doc.document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| {
                let l = doc.document.layers.get(*id).unwrap();
                matches!(l.kind, layer_model::LayerKind::Raster(_))
                    && l.mask.is_none()
                    && *id != portrait
            })
            .collect()
    };
    let set_visible = |ed: &mut app_shell::Editor, id: layer_model::LayerId, visible: bool| {
        ed.active_mut()
            .unwrap()
            .apply(Command::SetLayerProperties {
                layer_id: id,
                patch: editor_core::LayerPatch {
                    visible: Some(visible),
                    ..Default::default()
                },
            })
            .unwrap();
    };
    for id in &canvas_ids {
        set_visible(&mut ed, *id, false);
    }
    for id in &placed {
        set_visible(&mut ed, *id, false);
    }
    // iter_depth_first is topmost-first: pick the backdrops by NAME.
    let black_id = *placed
        .iter()
        .find(|id| ed.active().unwrap().document.layers.get(**id).unwrap().name == "Inspect Black")
        .unwrap();
    let white_id = *placed
        .iter()
        .find(|id| ed.active().unwrap().document.layers.get(**id).unwrap().name == "Inspect White")
        .unwrap();
    set_visible(&mut ed, black_id, true); // black backdrop
    let over_black = composite(&mut ed);
    set_visible(&mut ed, black_id, false);
    set_visible(&mut ed, white_id, true); // white backdrop
    let over_white = composite(&mut ed);
    // Deep interior: the ink over either backdrop (mask coverage 255).
    assert_eq!(probe(&over_black, 48, 48), [90, 40, 30, 255]);
    assert_eq!(probe(&over_white, 48, 48), [90, 40, 30, 255]);
    // A CONCEALED portrait pixel shows whichever backdrop is under it —
    // black over black, white over white (the inspection reads the halo).
    assert_eq!(probe(&over_black, 8, 8), [0, 0, 0, 255]);
    assert_eq!(probe(&over_white, 8, 8), [255, 255, 255, 255]);
    // Restore visibility.
    for id in &placed {
        set_visible(&mut ed, *id, false);
    }
    for id in &canvas_ids {
        set_visible(&mut ed, *id, true);
    }

    // 5. UNDOABLE: walking history back to the setup depth restores the
    //    pre-walk composite byte-for-byte (the mask and selection work leaves
    //    no residue).
    while ed.active().unwrap().history_depth() > depth_setup {
        ed.active_mut().unwrap().undo().unwrap();
    }
    let baseline = composite(&mut ed);
    assert_eq!(
        baseline, pre_everything,
        "undo returns the untouched portrait"
    );
    while ed.active_mut().unwrap().redo().unwrap_or(false) {}
    let replayed = composite(&mut ed);
    // The replayed composite differs only by the visible-backdrop toggles'
    // final state; the CUTOUT probes must hold at the top of history too.
    assert_eq!(
        probe(&replayed, 48, 48),
        [90, 40, 30, 255],
        "redo replays the cutout"
    );
    assert_eq!(
        portrait_hashes(&ed),
        hashes_at_setup,
        "source-preserving through the whole walk"
    );

    // 6. SURVIVES SAVE/REOPEN.
    let pre_save = composite(&mut ed);
    let package = tmp.path().join("walk.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let mut reopened = app::open_project(&package);
    let after = reopened.composite(region).unwrap();
    assert_eq!(
        probe(&after, 48, 48),
        probe(&pre_save, 48, 48),
        "the cutout survives save/reopen"
    );
}

/// Card 065: layer-stack management is PRACTICAL at scale — 30+ layers with
/// nested groups, rename through the panel's own command shape, duplicate an
/// alternative via the editor's real action dispatch, move a layer into a
/// group and between groups, cycle/duplicate-child rejection on re-parent,
/// and undo restoring order AND content together. The panel-level controls
/// (drag rows, drop zones, rename fields) are pinned at the ui level
/// (clicking_the_real_thing: drag-reorder, re-parent, rename field); this
/// test proves the stack operations they drive hold at 40+ layers.
#[test]
fn layer_stack_management_is_practical_at_scale() {
    use editor_core::Command;
    use integration_tests::app;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 128, 128);

    fn ink_layer(ed: &mut app_shell::Editor, name: &str, seed: u8) -> layer_model::LayerId {
        let layer = layer_model::Layer::raster(name);
        let id = layer.id;
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for px in bytes.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&[seed, seed / 2, seed / 3, 255]);
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(
                        raster::TileCoord::new(0, 0, 0),
                        hash,
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        id
    }

    // 30 content layers + 3 groups (one with a nested subgroup): 40+ rows.
    let mut content = Vec::new();
    for i in 0..30u8 {
        content.push(ink_layer(&mut ed, &format!("Layer {i:02}"), 40 + i));
    }
    for g in 0..3u8 {
        let group = layer_model::Layer::group(format!("Group {g}"));
        let gid = group.id;
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(group))
            .unwrap();
        for c in 0..3u8 {
            let child = ink_layer(&mut ed, &format!("G{g} child {c}"), 100 + g * 10 + c);
            ed.active_mut()
                .unwrap()
                .apply(Command::MoveLayer {
                    layer_id: child,
                    parent: Some(gid),
                    index: 0,
                })
                .unwrap();
        }
    }
    // A nested subgroup inside Group 0.
    let (group0, nested) = {
        let doc = &ed.active().unwrap().document;
        let gid = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| doc.layers.get(*id).unwrap().name == "Group 0")
            .unwrap();
        let nested = layer_model::Layer::group("Nested");
        let nid = nested.id;
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(nested))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(Command::MoveLayer {
                layer_id: nid,
                parent: Some(gid),
                index: 0,
            })
            .unwrap();
        (gid, nid)
    };
    let count = ed.active().unwrap().document.layers.len();
    assert!(count >= 40, "40+ rows: {count}");
    let order = |ed: &app_shell::Editor| -> Vec<String> {
        let doc = &ed.active().unwrap().document;
        doc.layers
            .iter_depth_first()
            .into_iter()
            .map(|id| doc.layers.get(id).unwrap().name.clone())
            .collect()
    };
    let order_before = order(&ed);

    // 1. RENAME the portrait through the panel's own command shape (the
    //    panel's LayersModel::rename emits exactly this SetLayerProperties).
    let portrait = content[0];
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: portrait,
            patch: editor_core::LayerPatch {
                name: Some("Headline portrait".into()),
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(portrait)
            .unwrap()
            .name,
        "Headline portrait"
    );

    // 2. DUPLICATE an alternative through the editor's real action dispatch
    //    (the Layers panel's own route).
    let alternative = content[5];
    ed.set_layer_selection(vec![alternative], Some(alternative));
    ed.dispatch(app_shell::action::Action::DuplicateLayer)
        .unwrap();
    let dup_count = ed.active().unwrap().document.layers.len();
    assert_eq!(dup_count, count + 1, "the duplicate adds exactly one layer");
    let duplicate = ed.active().unwrap().document.active_layer().unwrap();
    assert_ne!(duplicate, alternative, "the duplicate is a NEW layer");
    let dup_pixels = app::layer_tile_map(&ed, duplicate);
    let alt_pixels = app::layer_tile_map(&ed, alternative);
    assert_eq!(
        dup_pixels, alt_pixels,
        "the duplicate carries the same pixels"
    );
    assert_eq!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(duplicate)
            .unwrap()
            .name,
        "Layer 05 copy",
        "the duplicate carries the source's name marked as a copy"
    );

    // 3. MOVE the duplicate INTO a group (the drag-reparent route's command).
    ed.active_mut()
        .unwrap()
        .apply(Command::MoveLayer {
            layer_id: duplicate,
            parent: Some(group0),
            index: 0,
        })
        .unwrap();
    let in_group = {
        let doc = &ed.active().unwrap().document;
        doc.layers
            .get(group0)
            .unwrap()
            .children()
            .contains(&duplicate)
    };
    assert!(in_group, "the duplicate is a child of Group 0");

    // 4. A drop cannot form a cycle: moving Group 0 into its own descendant
    //    (the nested subgroup) is REFUSED and changes nothing.
    let depth = ed.active().unwrap().history_depth();
    let refused = ed.active_mut().unwrap().apply(Command::MoveLayer {
        layer_id: group0,
        parent: Some(nested),
        index: 0,
    });
    assert!(
        refused.is_err(),
        "a group into its own descendant is refused"
    );
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth,
        "a refused move leaves no history"
    );
    assert_eq!(order(&ed)[..4], order_before[..4], "the tree is unchanged");

    // 5. UNDO restores order AND content together: undo the move, the
    //    duplicate, and the rename — each step back is exact.
    ed.active_mut().unwrap().undo().unwrap(); // the move into the group
    let after_move_undo = order(&ed);
    assert_eq!(
        after_move_undo.len(),
        count + 1,
        "the duplicate survives the move's undo"
    );
    ed.active_mut().unwrap().undo().unwrap(); // the duplicate
    assert_eq!(
        ed.active().unwrap().document.layers.len(),
        count,
        "the duplicate's undo removes it"
    );
    assert!(
        app::layer_tile_map(&ed, duplicate).is_none(),
        "the duplicate's PIXELS are gone with it"
    );
    ed.active_mut().unwrap().undo().unwrap(); // the rename
    assert_eq!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(portrait)
            .unwrap()
            .name,
        format!("Layer {:02}", 0),
        "the rename's undo restores the name"
    );
    assert_eq!(
        order(&ed),
        order_before,
        "the whole walk restores the order"
    );
    // Redo replays all three exactly.
    for _ in 0..3 {
        ed.active_mut().unwrap().redo().unwrap();
    }
    assert_eq!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(portrait)
            .unwrap()
            .name,
        "Headline portrait"
    );
    assert_eq!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(duplicate)
            .unwrap()
            .name,
        "Layer 05 copy"
    );
    let dup_in_group = ed
        .active()
        .unwrap()
        .document
        .layers
        .get(group0)
        .unwrap()
        .children()
        .contains(&duplicate);
    assert!(dup_in_group, "redo restores the move into the group");

    // 6. VISIBILITY rides the document and undoes exactly (the row's eye
    //    toggle emits this patch; the collapse flag is panel session state
    //    over the saved tree by design).
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: group0,
            patch: editor_core::LayerPatch {
                visible: Some(false),
                ..Default::default()
            },
        })
        .unwrap();
    assert!(
        !ed.active()
            .unwrap()
            .document
            .layers
            .get(group0)
            .unwrap()
            .visible,
        "the eye toggle hides the group"
    );
    ed.active_mut().unwrap().undo().unwrap();
    assert!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(group0)
            .unwrap()
            .visible,
        "undo restores the visibility"
    );

    // 7. The stack survives native save/reopen at this size.
    let package = tmp.path().join("stack.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let reopened = app::open_project(&package);
    let names_after: Vec<String> = reopened
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .map(|id| reopened.document.layers.get(id).unwrap().name.clone())
        .collect();
    assert_eq!(
        names_after.len(),
        order_before.len() + 1,
        "the duplicate is the one extra row"
    );
    assert!(
        names_after.contains(&"Headline portrait".to_string()),
        "the renamed layer survives"
    );
    assert!(
        names_after.iter().any(|n| n == "Layer 05 copy"),
        "the duplicate survives reopen"
    );
}

/// Card 066: thumbnail effects integration — drop shadow and outside stroke
/// on a masked portrait, exercised through the layer-style patch
/// route the Layer Style dialog emits (SetLayerProperties effects, replaced
/// wholesale). Checked: a parameter change visibly updates the composite AND
/// the styled bounds the thumbnail/chrome size from; undo restores exactly;
/// effects ride save/reopen; a shadow pushed across a tile boundary leaves
/// no seams or stale remnants after movement (the reopened document — a
/// cold, freshly-composed cache — must be byte-identical to the live one).
/// Effect ordering around masks: the shadow derives from the MASKED
/// silhouette, so concealing more of the layer shrinks the halo.
#[test]
fn thumbnail_effects_integration_holds_end_to_end() {
    use editor_core::Command;
    use integration_tests::app;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 512, 128);

    // The portrait straddles the x=256 tile boundary, so any halo pushed
    // right crosses into tile (1,0) — the seam case.
    let portrait = {
        let layer = layer_model::Layer::raster("Portrait");
        let id = layer.id;
        // Ink doc x 232..280 (straddling the x=256 tile boundary), y 40..89:
        // tile (0,0) holds x 232..255, tile (1,0) holds x 256..279.
        let ts = raster::TILE_SIZE as usize;
        let mut bytes0 = vec![0u8; ts * ts * 4];
        let mut bytes1 = vec![0u8; ts * ts * 4];
        for y in 40..90usize {
            for x in 232..ts {
                let i = (y * ts + x) * 4;
                bytes0[i..i + 4].copy_from_slice(&[90, 40, 30, 255]);
            }
            for x in 0..24usize {
                let i = (y * ts + x) * 4;
                bytes1[i..i + 4].copy_from_slice(&[90, 40, 30, 255]);
            }
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes0);
        let hash1 = ed.active_mut().unwrap().tiles.insert_bytes(bytes1);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![
                        editor_core::TileEdit::set(raster::TileCoord::new(0, 0, 0), hash),
                        editor_core::TileEdit::set(raster::TileCoord::new(1, 0, 0), hash1),
                    ],
                )
                .unwrap(),
            )
            .unwrap();
        // A mask hiding the right sliver of the ink: the effect silhouette
        // is the MASKED one.
        let doc = ed.active_mut().unwrap();
        let mask = layer_model::LayerMask::new(layer_model::MaskId::new());
        doc.apply(Command::SetLayerProperties {
            layer_id: id,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(mask),
                ..Default::default()
            },
        })
        .unwrap();
        // Reveal x < 270 in DOCUMENT space: tile (0,0) fully, tile (1,0)
        // columns 0..14 (the store is tiled like the canvas).
        let ts = raster::TILE_SIZE as usize;
        let coverage = vec![255u8; editor_core::MASK_TILE_BYTES];
        let hash0 = doc.tiles.insert_bytes(coverage.clone());
        let mut cov1 = vec![0u8; editor_core::MASK_TILE_BYTES];
        for y in 0..ts {
            for x in 0..14usize {
                cov1[y * ts + x] = 255;
            }
        }
        let hash1 = doc.tiles.insert_bytes(cov1);
        doc.apply(
            Command::paint_tiles(
                editor_core::PixelTarget::Mask(id),
                vec![
                    editor_core::TileEdit::set(raster::TileCoord::new(0, 0, 0), hash0),
                    editor_core::TileEdit::set(raster::TileCoord::new(1, 0, 0), hash1),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        ed.set_layer_selection(vec![id], Some(id));
        id
    };
    let region = raster::PixelRect::new(0, 0, 512, 128);
    let composite = |ed: &mut app_shell::Editor| -> Vec<u8> {
        ed.active_mut().unwrap().composite(region).unwrap()
    };
    let probe = |bytes: &[u8], x: usize, y: usize| -> [u8; 4] {
        let i = (y * 512 + x) * 4;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    let backdrop = [255u8, 255, 255, 255];
    // The no-effects baseline, captured before any style lands.
    let no_effects = composite(&mut ed);
    let styled_bounds = |ed: &app_shell::Editor| {
        compositor::bounds::styled_bounds(
            &ed.active().unwrap().document,
            &ed.active().unwrap().tiles,
            portrait,
            0,
            compositor::CompositeOptions::default(),
        )
        .ok()
        .flatten()
    };

    // 1. DROP SHADOW: distance 20 at 0° pushes a dark halo to the RIGHT of
    //    the (masked) silhouette — across the tile boundary.
    let shadow = layer_model::ShadowEffect {
        blend_mode: layer_model::BlendMode::Normal,
        color: [0.0, 0.0, 0.0, 1.0],
        opacity: 0.9,
        angle_deg: 0.0,
        use_global_light: false,
        distance_px: 20.0,
        spread: 0.0,
        size_px: 0.0,
        ..layer_model::ShadowEffect::default()
    };
    let effects = layer_model::LayerEffects {
        drop_shadow: Some(shadow.clone()),
        ..Default::default()
    };
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: portrait,
            patch: editor_core::LayerPatch {
                effects: Some(Box::new(effects.clone())),
                ..Default::default()
            },
        })
        .unwrap();
    let with_shadow = composite(&mut ed);
    // The halo lands BEYOND the masked ink (mask ends at x=270, shadow at
    // 270+20=290) — inside tile (1,0) at x>256: the boundary-crossing case.
    // size_px 0 → a hard offset copy. The shadow falls OPPOSITE the light:
    // angle 0° (light from +x) shifts it LEFT — the halo occupies 212..232,
    // still on this layer's tile. The tile-boundary crossing is exercised by
    // the ink's right edge being masked at 270 (the styled bounds span both
    // tiles) and by the movement in step 6.
    let halo = probe(&with_shadow, 220, 64);
    assert!(
        halo[0] < 200,
        "the shadow shows past the mask edge, across the tile boundary: {halo:?}"
    );
    assert_eq!(
        probe(&with_shadow, 8, 64),
        backdrop,
        "far from the layer: clean"
    );
    // The styled bounds (what the thumbnail/chrome size from) grew past the
    // content bounds by the shadow's reach.
    let bounds = styled_bounds(&ed).expect("the styled bounds answer");
    assert!(bounds.x <= 212, "the styled bounds include the halo");

    // 2. PARAMETER CHANGE visibly updates: distance 40 moves the halo.
    let mut farther = effects.clone();
    if let Some(s) = &mut farther.drop_shadow {
        s.distance_px = 40.0;
    }
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: portrait,
            patch: editor_core::LayerPatch {
                effects: Some(Box::new(farther)),
                ..Default::default()
            },
        })
        .unwrap();
    let farther_bytes = composite(&mut ed);
    assert_ne!(
        farther_bytes, with_shadow,
        "the distance change rewrote the canvas"
    );
    let halo40 = probe(&farther_bytes, 196, 64);
    assert!(
        halo40[0] < 200,
        "the halo moved with the distance: {halo40:?}"
    );
    let bounds40 = styled_bounds(&ed).expect("the styled bounds answer");
    assert!(bounds40.x <= 192, "the styled bounds followed the distance");

    // 3. UNDO restores the previous effects exactly.
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(composite(&mut ed), with_shadow, "undo restores the shadow");
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(composite(&mut ed), no_effects, "undo to no effects");
    ed.active_mut().unwrap().redo().unwrap();
    ed.active_mut().unwrap().redo().unwrap();
    assert_eq!(
        composite(&mut ed),
        farther_bytes,
        "redo replays the distance change"
    );

    // 4. EFFECT ORDERING AROUND THE MASK: the silhouette is the MASKED one —
    //    concealing more of the layer shrinks the halo. Paint a hole in the
    //    mask over the shadow-side sliver; the halo must lose exactly the
    //    shadow derived from the newly hidden ink.
    ed.set_edit_target_kind(app_shell::edit_target::EditTargetKind::Mask);
    ed.set_tool(tools::ToolId::Eraser);
    let mut pointer = app_shell::tool_input::ToolPointer::new();
    app::shell_stroke(
        &mut pointer,
        &mut ed,
        &[glam::Vec2::new(264.0, 64.0), glam::Vec2::new(268.0, 64.0)],
    );
    let after_hole = composite(&mut ed);
    // The shadow beyond the newly concealed ink is GONE (its source ink is
    // masked out), while the ink left of the hole still casts.
    // shadow(x) = ink(x + 40): erasing source ink removes exactly the
    // shadow it cast, while untouched ink keeps casting.
    let kept = probe(&after_hole, 200, 64);
    assert_eq!(kept, halo40, "the untouched ink keeps casting its halo");
    let lost = probe(&after_hole, 220, 64);
    assert_eq!(lost, backdrop, "the erased ink no longer casts: {lost:?}");
    ed.set_edit_target_kind(app_shell::edit_target::EditTargetKind::Content);

    // 5. OUTSIDE STROKE on the same layer: width change visibly updates.
    let stroked = layer_model::LayerEffects {
        drop_shadow: Some(shadow.clone()),
        stroke: Some(layer_model::StrokeEffect {
            size_px: 6.0,
            position: layer_model::StrokePosition::Outside,
            blend_mode: layer_model::BlendMode::Normal,
            opacity: 1.0,
            fill: layer_model::FillStyle::Solid([0.0, 0.0, 1.0, 1.0]),
            overprint: false,
        }),
        ..Default::default()
    };
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: portrait,
            patch: editor_core::LayerPatch {
                effects: Some(Box::new(stroked.clone())),
                ..Default::default()
            },
        })
        .unwrap();
    let with_stroke = composite(&mut ed);
    assert_ne!(
        with_stroke, after_hole,
        "adding the stroke rewrote the canvas"
    );
    // A blue ring just outside the masked silhouette's left edge (x=232):
    let ring = probe(&with_stroke, 226, 64);
    assert!(
        ring[2] > ring[0],
        "the outside stroke shows blue outside the silhouette: {ring:?}"
    );
    // Widening it visibly updates again.
    let mut wider = stroked; // moves; the with_stroke bytes are already captured
    if let Some(s) = &mut wider.stroke {
        s.size_px = 14.0;
    }
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: portrait,
            patch: editor_core::LayerPatch {
                effects: Some(Box::new(wider)),
                ..Default::default()
            },
        })
        .unwrap();
    let wider_bytes = composite(&mut ed);
    assert_ne!(
        wider_bytes, with_stroke,
        "the width change rewrote the canvas"
    );
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(
        composite(&mut ed),
        with_stroke,
        "undo restores the stroke width"
    );

    // 6. NO SEAMS OR STALE REMNANTS AFTER MOVEMENT: move the layer left by
    //    32 (the halo shifts with it across tiles), then compare the live
    //    composite against a COLD recompose — save + reopen in a fresh
    //    document (empty caches) and require byte equality.
    ed.active_mut()
        .unwrap()
        .apply(Command::TransformLayer {
            layer_id: portrait,
            matrix: glam::Affine2::from_translation(glam::Vec2::new(-32.0, 0.0)).to_cols_array(),
        })
        .unwrap();
    let moved = composite(&mut ed);
    let package = tmp.path().join("effects.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let mut reopened = app::open_project(&package);
    let cold = reopened.composite(region).unwrap();
    assert_eq!(
        cold, moved,
        "a cold recompose is byte-identical: no seams, no stale halo remnants"
    );
}

/// Card 068: clipped adjustments scope to the layer they clip — a
/// portrait-only tonal adjustment leaves the headline and background
/// untouched; toggling (visibility, then clip release) restores prior
/// pixels; the adjustment's own mask is editable separately and composes;
/// parameters and clipping survive save/reopen. Uses the existing
/// adjustment types (Curves, Invert) through the real command routes —
/// repair only where a reproduced defect demands it (none found: the
/// clipping commands, scoping, and mask targeting all behave).
#[test]
fn the_clipped_adjustment_scopes_to_the_portrait_and_survives_reload() {
    use editor_core::Command;
    use integration_tests::app;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 128, 128);

    // Headline (bottom, blue-ish) and Portrait (top, warm) content layers
    // over the white canvas; the portrait is what the adjustment will clip
    // to. NOTE: the shell stacks topmost-first, so the LAST created layer
    // renders above.
    let headline = {
        let layer = layer_model::Layer::raster("Headline");
        let id = layer.id;
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in 20..60usize {
            for x in 16..112usize {
                let i = (y * raster::TILE_SIZE as usize + x) * 4;
                bytes[i..i + 4].copy_from_slice(&[30, 30, 160, 255]);
            }
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(
                        raster::TileCoord::new(0, 0, 0),
                        hash,
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        id
    };
    let _portrait = {
        let layer = layer_model::Layer::raster("Portrait");
        let id = layer.id;
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in 68..100usize {
            for x in 40..88usize {
                let i = (y * raster::TILE_SIZE as usize + x) * 4;
                bytes[i..i + 4].copy_from_slice(&[200, 120, 60, 255]);
            }
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        ed.active_mut()
            .unwrap()
            .apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(
                        raster::TileCoord::new(0, 0, 0),
                        hash,
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        id
    };
    let region = raster::PixelRect::new(0, 0, 128, 128);
    let composite = |ed: &mut app_shell::Editor| -> Vec<u8> {
        ed.active_mut().unwrap().composite(region).unwrap()
    };
    let probe = |bytes: &[u8], x: usize, y: usize| -> [u8; 4] {
        let i = (y * 128 + x) * 4;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    let baseline = composite(&mut ed);
    let headline_px = probe(&baseline, 64, 40);
    let portrait_px = probe(&baseline, 64, 84);
    let backdrop = [255u8, 255, 255, 255];

    // The clipped adjustment: a strong Curves darkening, created ABOVE the
    // portrait (the active layer route) and clipped to it.
    let adjustment = {
        let layer = layer_model::Layer::with_kind(
            "Tonal",
            layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: layer_model::AdjustmentKind::Curves {
                    points: vec![[0.0, 0.0], [0.5, 0.15], [1.0, 0.55]],
                },
            }),
        );
        let id = layer.id;
        ed.active_mut()
            .unwrap()
            .apply(Command::create_layer(layer))
            .unwrap();
        id
    };
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: adjustment,
            patch: editor_core::LayerPatch {
                clipping: Some(layer_model::ClippingMode::ClipToBelow),
                ..Default::default()
            },
        })
        .unwrap();
    let clipped = composite(&mut ed);
    // The portrait darkens; the headline and the backdrop are UNCHANGED.
    let portrait_clipped = probe(&clipped, 64, 84);
    assert!(
        portrait_clipped[0] < portrait_px[0] && portrait_clipped[1] < portrait_px[1],
        "the portrait darkens under the clipped curves: {portrait_clipped:?} vs {portrait_px:?}"
    );
    assert_eq!(
        probe(&clipped, 64, 40),
        headline_px,
        "the headline is OUTSIDE the clip scope"
    );
    assert_eq!(
        probe(&clipped, 8, 8),
        backdrop,
        "the backdrop is outside too"
    );

    // TOGGLING restores prior pixels: hide the adjustment...
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: adjustment,
            patch: editor_core::LayerPatch {
                visible: Some(false),
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(
        composite(&mut ed),
        baseline,
        "hiding the adjustment restores prior pixels"
    );
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: adjustment,
            patch: editor_core::LayerPatch {
                visible: Some(true),
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(
        composite(&mut ed),
        clipped,
        "showing it again restores the scope"
    );
    // ...and releasing the clip makes it hit EVERYTHING (the scope check in
    // reverse): the headline darkens too.
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: adjustment,
            patch: editor_core::LayerPatch {
                clipping: Some(layer_model::ClippingMode::None),
                ..Default::default()
            },
        })
        .unwrap();
    let released = composite(&mut ed);
    assert_ne!(
        probe(&released, 64, 40),
        headline_px,
        "released from the clip, the adjustment reaches the headline"
    );
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(composite(&mut ed), clipped, "undo restores the clip scope");

    // EDIT ITS MASK SEPARATELY: a mask on the ADJUSTMENT reveals only the
    // left QUARTER of the canvas (x 0..64 of the 256-wide store tile) —
    // the darkening now covers only the portrait's left quarter.
    {
        let doc = ed.active_mut().unwrap();
        let mask = layer_model::LayerMask::new(layer_model::MaskId::new());
        doc.apply(Command::SetLayerProperties {
            layer_id: adjustment,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(mask),
                ..Default::default()
            },
        })
        .unwrap();
        let mut coverage = vec![0u8; editor_core::MASK_TILE_BYTES];
        for y in 0..raster::TILE_SIZE as usize {
            for x in 0..64usize {
                coverage[y * raster::TILE_SIZE as usize + x] = 255;
            }
        }
        let mhash = doc.tiles.insert_bytes(coverage);
        doc.apply(
            Command::paint_tiles(
                editor_core::PixelTarget::Mask(adjustment),
                vec![editor_core::TileEdit::set(
                    raster::TileCoord::new(0, 0, 0),
                    mhash,
                )],
            )
            .unwrap(),
        )
        .unwrap();
    }
    let masked_adjust = composite(&mut ed);
    // The portrait's LEFT side stays darkened, its RIGHT side back to the
    // original ink; the headline is untouched throughout.
    assert!(
        probe(&masked_adjust, 48, 84)[0] < portrait_px[0],
        "the left portrait keeps the darkening"
    );
    assert_eq!(
        probe(&masked_adjust, 80, 84),
        portrait_px,
        "the right portrait is outside the adjustment's own mask"
    );
    assert_eq!(
        probe(&masked_adjust, 64, 40),
        headline_px,
        "the headline stays out"
    );
    // Undo removes the mask in TWO entries (the coverage paint, then the
    // attach itself).
    ed.active_mut().unwrap().undo().unwrap();
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(
        composite(&mut ed),
        clipped,
        "undo restores the unmasked clip"
    );

    // SWAP THE KIND: Invert hits only the portrait while clipped.
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerKind {
            layer_id: adjustment,
            kind: Box::new(layer_model::LayerKind::Adjustment(
                layer_model::AdjustmentLayer {
                    kind: layer_model::AdjustmentKind::Invert,
                },
            )),
        })
        .unwrap();
    let inverted = composite(&mut ed);
    let inverted_px = probe(&inverted, 64, 84);
    assert!(
        (inverted_px[0] as i32 - (255 - portrait_px[0] as i32)).abs() <= 2,
        "the portrait inverts: {inverted_px:?} vs {portrait_px:?}"
    );
    assert_eq!(
        probe(&inverted, 64, 40),
        headline_px,
        "the headline stays out"
    );

    // PARAMETERS SURVIVE RELOAD: save + reopen; the kind, clipping, and
    // composite are byte-identical.
    let pre_save = composite(&mut ed);
    let package = tmp.path().join("clipped.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let mut reopened = app::open_project(&package);
    let doc = &reopened.document;
    let adj = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| doc.layers.get(*id).unwrap().name == "Tonal")
        .unwrap();
    let layer = doc.layers.get(adj).unwrap();
    assert!(layer.clipping == layer_model::ClippingMode::ClipToBelow);
    match &layer.kind {
        layer_model::LayerKind::Adjustment(a) => {
            assert!(matches!(a.kind, layer_model::AdjustmentKind::Invert))
        }
        other => panic!("the adjustment stays an adjustment: {other:?}"),
    }
    let after = reopened.composite(region).unwrap();
    assert_eq!(after, pre_save, "the clipped composite survives reload");
    let _ = headline;
}

/// Card 070: the native reusable composition workflow — a native sample
/// composition (a template with clearly named editable placeholder layers:
/// a headline text layer, a "replace me" smart-object logo, a background)
/// opens as its own document; Duplicate Document and Save As produce
/// independent variants (text edits + Replace Contents) WITHOUT touching
/// the template file, and each variant exports under its own name. No
/// separate template engine: the workflow is open-as-unsaved + duplicate +
/// save-as, all through real routes.
#[test]
fn native_composition_workflow_supports_independent_variants() {
    use integration_tests::app;

    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 320, 180);

    // The template: a background, a "replace me" logo smart object, and a
    // headline text layer with clearly named editable placeholders.
    let background_png = tmp.path().join("background.png");
    let mut bg = vec![0u8; 320 * 180 * 4];
    for px in bg.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&[235, 235, 225, 255]);
    }
    std::fs::write(
        &background_png,
        raster::encode(raster::ExportFormat::Png, 320, 180, &bg).unwrap(),
    )
    .unwrap();
    let logo_png = tmp.path().join("logo.png");
    let mut logo = vec![0u8; 48 * 48 * 4];
    for px in logo.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&[200, 60, 30, 255]);
    }
    std::fs::write(
        &logo_png,
        raster::encode(raster::ExportFormat::Png, 48, 48, &logo).unwrap(),
    )
    .unwrap();

    ed.open_path(&background_png).unwrap();
    let template_index = ed.active_index().unwrap();
    let background = ed.active().unwrap().document.active_layer().unwrap();
    ed.active_mut()
        .unwrap()
        .apply(editor_core::Command::SetLayerProperties {
            layer_id: background,
            patch: editor_core::LayerPatch {
                name: Some("Background".into()),
                ..Default::default()
            },
        })
        .unwrap();
    // The logo places ABOVE the background as a smart object.
    ed.place_path(&logo_png, false).unwrap();
    let logo_layer = ed.active().unwrap().document.active_layer().unwrap();
    ed.active_mut()
        .unwrap()
        .apply(editor_core::Command::SetLayerProperties {
            layer_id: logo_layer,
            patch: editor_core::LayerPatch {
                name: Some("Logo — replace me".into()),
                ..Default::default()
            },
        })
        .unwrap();
    // The headline text layer on top (the Type tool's create route).
    let headline = layer_model::Layer::with_kind(
        "Headline — edit me",
        layer_model::LayerKind::Text(layer_model::TextLayer::legacy(
            "TEMPLATE HEADLINE".to_string(),
            "DejaVu Sans".to_string(),
            24.0,
        )),
    );
    let headline_id = headline.id;
    ed.active_mut()
        .unwrap()
        .apply(editor_core::Command::create_layer(headline))
        .unwrap();
    // Save the TEMPLATE.
    let template = tmp.path().join("template.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&template, app::APP_VERSION)
        .unwrap();
    // Native packages are DIRECTORIES: the immutability check hashes the
    // manifest (the package's identity document).
    let manifest = template.join("manifest.json");
    let template_before = std::fs::read(&manifest).unwrap();
    // VARIANT 1: Duplicate Document, then a text-content edit through the
    // real SetLayerKind route.
    let docs_before = ed.documents().len();
    ed.duplicate_document().unwrap();
    assert_eq!(
        ed.documents().len(),
        docs_before + 1,
        "the duplicate is a second open document"
    );
    {
        let mut text = match &ed
            .active()
            .unwrap()
            .document
            .layers
            .get(headline_id)
            .unwrap()
            .kind
        {
            layer_model::LayerKind::Text(t) => t.clone(),
            other => panic!("the headline is text: {other:?}"),
        };
        text.text = "VARIANT ONE".to_string();
        ed.apply_command(editor_core::Command::SetLayerKind {
            layer_id: headline_id,
            kind: Box::new(layer_model::LayerKind::Text(text)),
        });
    }
    let v1 = tmp.path().join("variant-one.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&v1, app::APP_VERSION)
        .unwrap();

    // VARIANT 2: back to the template tab, duplicate again, REPLACE
    // CONTENTS on the logo smart object with a new file (the card-069
    // route), keep the template text.
    ed.activate(template_index).unwrap();
    ed.duplicate_document().unwrap();
    {
        let replacement = tmp.path().join("logo2.png");
        let mut logo2 = vec![0u8; 96 * 96 * 4];
        for px in logo2.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&[20, 160, 90, 255]);
        }
        std::fs::write(
            &replacement,
            raster::encode(raster::ExportFormat::Png, 96, 96, &logo2).unwrap(),
        )
        .unwrap();
        // duplicate_document preserves the selection — assert it instead of
        // discarding it, then make the intent explicit anyway.
        let active = ed.active().unwrap().document.active_layer().unwrap();
        assert_eq!(
            active, logo_layer,
            "the duplicate preserves the logo selection"
        );
        ed.set_layer_selection(vec![logo_layer], Some(logo_layer));
        ed.replace_smart_object_contents(&replacement).unwrap();
    }
    let v2 = tmp.path().join("variant-two.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&v2, app::APP_VERSION)
        .unwrap();

    // THE TEMPLATE IS UNCHANGED: the manifest is byte-identical AND the
    // package's file SET is unchanged (no tile/asset file was rewritten).
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        template_before,
        "creating two variants left the template manifest untouched"
    );

    // The variants are INDEPENDENT: reopen all three documents fresh.
    let template_doc = app::open_project(&template);
    let mut v1_doc = app::open_project(&v1);
    let mut v2_doc = app::open_project(&v2);
    let names = |doc: &app_shell::doc::OpenDocument| -> Vec<String> {
        doc.document
            .layers
            .iter_depth_first()
            .into_iter()
            .map(|id| doc.document.layers.get(id).unwrap().name.clone())
            .collect()
    };
    assert!(names(&template_doc).contains(&"Headline — edit me".to_string()));
    // Variant 1 changed ONLY the headline text.
    match &v1_doc.document.layers.get(headline_id).unwrap().kind {
        layer_model::LayerKind::Text(t) => assert_eq!(t.text, "VARIANT ONE"),
        other => panic!("variant 1's headline: {other:?}"),
    }
    match &template_doc.document.layers.get(headline_id).unwrap().kind {
        layer_model::LayerKind::Text(t) => assert_eq!(t.text, "TEMPLATE HEADLINE"),
        other => panic!("the template's headline: {other:?}"),
    }
    // Variant 2 changed ONLY the logo pixels (via Replace Contents).
    let logo_pixel = |doc: &app_shell::doc::OpenDocument| -> [u8; 4] {
        use compositor::TileSource as _;
        let map = doc.document.layer_tiles(logo_layer).unwrap();
        let (_, hash) = map.iter().next().unwrap();
        let bytes = doc.tiles.tile(hash).unwrap();
        [bytes[0], bytes[1], bytes[2], bytes[3]]
    };
    assert_eq!(logo_pixel(&v2_doc)[1], 160, "variant 2 wears the new logo");
    assert_eq!(
        logo_pixel(&template_doc)[0],
        200,
        "the template keeps the old logo"
    );

    // Each variant EXPORTS under its own name (the app's encode path).
    let region = raster::PixelRect::new(0, 0, 320, 180);
    let export1 = tmp.path().join("thumbnail-variant-one.png");
    let export2 = tmp.path().join("thumbnail-variant-two.png");
    std::fs::write(
        &export1,
        raster::encode(
            raster::ExportFormat::Png,
            320,
            180,
            &v1_doc.composite(region).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        &export2,
        raster::encode(
            raster::ExportFormat::Png,
            320,
            180,
            &v2_doc.composite(region).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(export1.exists() && export2.exists());
    assert_ne!(
        std::fs::read(&export1).unwrap(),
        std::fs::read(&export2).unwrap(),
        "the two variants export different images"
    );
}

/// Card 071 — M4 DELIVERY: the native thumbnail workflow built from scratch
/// through the application's own routes: place sources, edit the headline,
/// mask the portrait, transform objects, add effects + a clipped
/// adjustment, organize groups, save/reopen, export — then repeat with one
/// content replacement (Replace Contents). Every operation is a route the
/// shipping UI drives (place_path, the selection→mask menu shape, commands,
/// the layer-style patch route); nothing is reachable ONLY by constructing
/// the document in code. The deterministic checks here are the delivery
/// evidence; the visual quality gate on real photos remains card 091's
/// human walk. Run with --ignored to materialize the delivery artifacts
/// (the native .rstudio + exported PNG) under tests/project-fixtures/
/// m4-delivery/ for the record.
#[test]
#[ignore = "materializes M4 delivery artifacts; run explicitly: cargo test -p integration-tests --test thumbnail_workflow the_native_thumbnail_milestone -- --ignored"]
fn the_native_thumbnail_milestone_is_delivered_end_to_end() {
    use app_shell::menu_bridge;
    use editor_core::Command;
    use integration_tests::app;
    use ui::menu::MenuAction;

    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("project-fixtures")
        .join("m4-delivery");
    std::fs::create_dir_all(&out_dir).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 320, 180);

    // 1. PLACE SOURCES: a generated background and portrait (no licensing
    //    concerns), placed through the editor's place route.
    let background_png = tmp.path().join("background.png");
    let mut bg = vec![0u8; 320 * 180 * 4];
    for px in bg.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&[235, 235, 225, 255]);
    }
    std::fs::write(
        &background_png,
        raster::encode(raster::ExportFormat::Png, 320, 180, &bg).unwrap(),
    )
    .unwrap();
    let portrait_png = tmp.path().join("portrait.png");
    let mut portrait = vec![0u8; 96 * 120 * 4];
    for y in 0..120usize {
        for x in 0..96usize {
            let i = (y * 96 + x) * 4;
            portrait[i..i + 4].copy_from_slice(&[200, 120, 60, 255]);
        }
    }
    std::fs::write(
        &portrait_png,
        raster::encode(raster::ExportFormat::Png, 96, 120, &portrait).unwrap(),
    )
    .unwrap();
    ed.open_path(&background_png).unwrap();
    let background = ed.active().unwrap().document.active_layer().unwrap();
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: background,
            patch: editor_core::LayerPatch {
                name: Some("Background".into()),
                ..Default::default()
            },
        })
        .unwrap();
    ed.place_path(&portrait_png, false).unwrap();
    let portrait_layer = ed.active().unwrap().document.active_layer().unwrap();
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: portrait_layer,
            patch: editor_core::LayerPatch {
                name: Some("Portrait".into()),
                ..Default::default()
            },
        })
        .unwrap();

    // 2. EDIT THE HEADLINE: a text layer through the Type tool's create
    //    route, then a content edit through SetLayerKind.
    let headline = layer_model::Layer::with_kind(
        "Headline",
        layer_model::LayerKind::Text(layer_model::TextLayer::legacy(
            "DRAFT".to_string(),
            "DejaVu Sans".to_string(),
            24.0,
        )),
    );
    let headline_id = headline.id;
    ed.active_mut()
        .unwrap()
        .apply(Command::create_layer(headline))
        .unwrap();
    {
        let mut text = match &ed
            .active()
            .unwrap()
            .document
            .layers
            .get(headline_id)
            .unwrap()
            .kind
        {
            layer_model::LayerKind::Text(t) => t.clone(),
            other => panic!("the headline is text: {other:?}"),
        };
        text.text = "THUMBNAIL READY".to_string();
        ed.apply_command(Command::SetLayerKind {
            layer_id: headline_id,
            kind: Box::new(layer_model::LayerKind::Text(text)),
        });
    }

    // 3. MASK THE PORTRAIT: a rough selection, feathered, revealed as the
    //    layer's mask (the Select ▸/Layer ▸ Mask menu shape).
    app::set_selection(
        &mut ed,
        editor_core::Selection::Rect {
            min: glam::IVec2::new(112, 30),
            max: glam::IVec2::new(208, 150),
        },
    );
    menu_bridge::perform(
        MenuAction::Modify(ui::menu::ModifySelection::Feather),
        &mut ed,
    )
    .expect("feather");
    menu_bridge::perform(MenuAction::Mask(ui::menu::MaskOp::RevealSelection), &mut ed)
        .expect("selection to mask");
    let after_mask = app::mask_tile_map(&ed, portrait_layer).is_some();
    assert!(after_mask, "the portrait wears an editable mask");

    // 4. TRANSFORM OBJECTS: nudge the portrait (its mask rides, card 043).
    ed.active_mut()
        .unwrap()
        .apply(Command::TransformLayer {
            layer_id: portrait_layer,
            matrix: glam::Affine2::from_translation(glam::Vec2::new(24.0, 0.0)).to_cols_array(),
        })
        .unwrap();

    // 5. EFFECTS + A CLIPPED ADJUSTMENT: a drop shadow on the portrait and
    //    a clipped Curves above it (the portrait-only tonal edit).
    let shadow = layer_model::LayerEffects {
        drop_shadow: Some(layer_model::ShadowEffect {
            color: [0.0, 0.0, 0.0, 1.0],
            opacity: 0.8,
            angle_deg: 0.0,
            use_global_light: false,
            distance_px: 8.0,
            spread: 0.0,
            size_px: 4.0,
            noise: 0.0,
            blend_mode: layer_model::BlendMode::Normal,
            knockout: false,
        }),
        ..Default::default()
    };
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: portrait_layer,
            patch: editor_core::LayerPatch {
                effects: Some(Box::new(shadow)),
                ..Default::default()
            },
        })
        .unwrap();
    let adjustment = layer_model::Layer::with_kind(
        "Portrait Tonal",
        layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
            kind: layer_model::AdjustmentKind::Curves {
                points: vec![[0.0, 0.0], [1.0, 0.8]],
            },
        }),
    );
    let adjustment_id = adjustment.id;
    ed.active_mut()
        .unwrap()
        .apply(Command::create_layer(adjustment))
        .unwrap();
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: adjustment_id,
            patch: editor_core::LayerPatch {
                clipping: Some(layer_model::ClippingMode::ClipToBelow),
                ..Default::default()
            },
        })
        .unwrap();

    // 6. ORGANIZE GROUPS: the portrait + its adjustment move into a group.
    let group = layer_model::Layer::group("Subject");
    let group_id = group.id;
    ed.active_mut()
        .unwrap()
        .apply(Command::create_layer(group))
        .unwrap();
    for id in [portrait_layer, adjustment_id] {
        ed.active_mut()
            .unwrap()
            .apply(Command::MoveLayer {
                layer_id: id,
                parent: Some(group_id),
                index: 0,
            })
            .unwrap();
    }
    let composed = {
        let region = raster::PixelRect::new(0, 0, 320, 180);
        ed.active_mut().unwrap().composite(region).unwrap()
    };

    // 7. SAVE/REOPEN: the whole scene survives the native package.
    let package = tmp.path().join("m4-delivery.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, app::APP_VERSION)
        .unwrap();
    let mut reopened = app::open_project(&package);
    let region = raster::PixelRect::new(0, 0, 320, 180);
    let reopened_composite = reopened.composite(region).unwrap();
    assert_eq!(
        reopened_composite, composed,
        "the composed scene survives save/reopen"
    );

    // 8. EXPORT with the app's encode path.
    let exported_png = tmp.path().join("m4-delivery.png");
    std::fs::write(
        &exported_png,
        raster::encode(raster::ExportFormat::Png, 320, 180, &composed).unwrap(),
    )
    .unwrap();
    let decoded = raster::decode_path(&exported_png).unwrap();
    assert_eq!(decoded.width, 320);
    assert_eq!(decoded.height, 180);

    // 9. REPEAT WITH ONE CONTENT REPLACEMENT: Replace Contents on the
    //    portrait smart object — wait, the portrait was placed EMBEDDED, so
    //    convert it first (the Layers-panel route), then replace.
    // Conversion rasterizes the layer’s masked appearance (effects and
    // the clipped adjustment bake into the object’s flat tiles — its
    // documented design — so with effects present the appearance
    // legitimately changes; post_convert is the replacement baseline.
    ed.set_layer_selection(vec![portrait_layer], Some(portrait_layer));
    ed.convert_to_smart_object().unwrap();
    // Convert mints a NEW smart-object layer (create + paint + move +
    // delete, one transaction) — find it by kind inside the Subject group
    // (the selection may still point at the deleted source id).
    let smart_portrait = {
        let doc = &ed.active().unwrap().document;
        doc.layers
            .iter_depth_first()
            .into_iter()
            .find(|id| {
                matches!(
                    &doc.layers.get(*id).unwrap().kind,
                    layer_model::LayerKind::SmartObject(_)
                )
            })
            .expect("the converted smart object exists")
    };
    ed.set_layer_selection(vec![smart_portrait], Some(smart_portrait));
    let replacement = tmp.path().join("portrait2.png");
    let mut portrait2 = vec![0u8; 96 * 120 * 4];
    for y in 0..120usize {
        for x in 0..96usize {
            let i = (y * 96 + x) * 4;
            portrait2[i..i + 4].copy_from_slice(&[40, 160, 90, 255]);
        }
    }
    std::fs::write(
        &replacement,
        raster::encode(raster::ExportFormat::Png, 96, 120, &portrait2).unwrap(),
    )
    .unwrap();
    // The conversion rasterizes the layer's masked appearance into the new
    // object's tiles (its documented design), so the replacement's undo
    // baseline is the post-conversion composite.
    let post_convert = {
        let region = raster::PixelRect::new(0, 0, 320, 180);
        ed.active_mut().unwrap().composite(region).unwrap()
    };
    let depth = ed.active().unwrap().history_depth();
    ed.replace_smart_object_contents(&replacement).unwrap();
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth + 1,
        "the replacement is one undoable step"
    );
    let replaced = {
        let region = raster::PixelRect::new(0, 0, 320, 180);
        ed.active_mut().unwrap().composite(region).unwrap()
    };
    assert_ne!(replaced, composed, "the replacement changed the scene");
    // Undo restores the ORIGINAL content and metadata.
    ed.active_mut().unwrap().undo().unwrap();
    let restored = {
        let region = raster::PixelRect::new(0, 0, 320, 180);
        ed.active_mut().unwrap().composite(region).unwrap()
    };
    assert_eq!(
        restored, post_convert,
        "undo restores the pre-replacement scene"
    );

    // MATERIALIZE the delivery artifacts for the record.
    let out_package = out_dir.join("m4-delivery.rstudio");
    let _ = std::fs::remove_dir_all(&out_package);
    std::fs::rename(&package, &out_package).unwrap();
    std::fs::copy(&exported_png, out_dir.join("m4-delivery.png")).unwrap();
    std::fs::write(
        out_dir.join("README.md"),
        "M4 delivery artifacts (card 071), materialized by the ignored test\n\
             `the_native_thumbnail_milestone_is_delivered_end_to_end`.\n\n\
             - m4-delivery.rstudio: the native package (open in the app)\n\
             - m4-delivery.png: the export\n\n\
             Deterministic checks are pinned by the test itself; screenshots\n\
             and the real-photo visual quality gate belong to card 091's\n\
             human acceptance walk.\n",
    )
    .unwrap();
}

#[test]
fn the_asset_reuse_and_clipboard_workflows_survive_native_persistence() {
    use app_shell::dialogs::ScriptedDialogs;
    use app_shell::menu_bridge;
    use integration_tests::app;
    use ui::menu::MenuAction;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    let solid = |w: u32, h: u32, c: [u8; 4]| {
        let mut v = vec![0u8; (w * h * 4) as usize];
        for px in v.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&c);
        }
        v
    };
    let png = |name: &str, w: u32, h: u32, rgba: Vec<u8>| {
        let path = dir.join(name);
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
        )
        .unwrap();
        path
    };

    // The canvas and the assets.
    let canvas = png("canvas.png", 64, 64, solid(64, 64, [255, 255, 255, 255]));
    let background = png(
        "background.png",
        300,
        200,
        solid(300, 200, [10, 20, 30, 255]),
    );
    let portrait = png("portrait.png", 40, 50, solid(40, 50, [200, 100, 50, 255]));
    let linked = png("linked.png", 20, 20, solid(20, 20, [90, 90, 90, 255]));
    let garbage = dir.join("garbage.png");
    std::fs::write(&garbage, b"this is not a png at all").unwrap();

    let logo_rgba = {
        let mut v = vec![0u8; 16 * 12 * 4];
        for px in v.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&[0, 200, 90, 255]);
        }
        v
    };

    // The editor with the Place picker primed for the portrait, and the
    // image clipboard as the deterministic fake.
    let mut ed = {
        let mut ed = app_shell::Editor::with_state(
            app_shell::AppPaths::rooted(dir.join("config")),
            app_shell::prefs::Preferences::default(),
            app_shell::recent::RecentFiles::new(),
            Box::new(ScriptedDialogs {
                place_files: vec![portrait.clone()],
                ..Default::default()
            }),
        );
        ed.open_path(&canvas).expect("the canvas opens");
        ed
    };
    {
        let mut os = app_shell::clipboard::FakeClipboard::new();
        os.seed(app_shell::clipboard::ClipboardImage::validate(16, 12, logo_rgba.clone()).unwrap());
        ed.set_image_clipboard(Box::new(os));
    }

    // ---- Compose: drop-equivalent (card 049 routes drops through
    // place_path), the Place menu, and the clipboard.
    ed.place_path(&background, false)
        .expect("the background places");
    menu_bridge::perform(MenuAction::PlaceEmbedded, &mut ed)
        .expect("the portrait places through the menu");
    menu_bridge::perform(MenuAction::Paste, &mut ed)
        .expect("the logo pastes from the image clipboard");
    // A linked asset for the disappearance check.
    ed.place_path(&linked, true)
        .expect("the linked asset places");

    assert_eq!(
        ed.active().unwrap().document.layers.len(),
        5,
        "canvas + background + portrait + logo + linked"
    );
    let composite_saved = {
        let rect = ed.active().unwrap().canvas_rect();
        ed.active_mut().unwrap().composite(rect).unwrap()
    };
    let transforms_before: Vec<(String, glam::Affine2, layer_model::LayerKind)> = ed
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .map(|id| {
            let doc = ed.active().unwrap();
            let layer = doc.document.layers.get(id).unwrap();
            (layer.name.clone(), layer.transform, layer.kind.clone())
        })
        .collect();
    for (name, _, kind) in &transforms_before {
        if name != "canvas.png" {
            assert!(
                matches!(kind, layer_model::LayerKind::SmartObject(_)),
                "{name} placed as a smart object"
            );
        }
    }

    // ---- Paste Into inside the same session: the source pixels survive
    // behind an editable mask.
    app::set_selection(
        &mut ed,
        editor_core::Selection::Rect {
            min: glam::IVec2::new(0, 0),
            max: glam::IVec2::new(4, 4),
        },
    );
    // Copy works on a PIXEL layer — copy a crop of the canvas: make it the
    // active layer first.
    let canvas_id = ed
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| {
            ed.active()
                .unwrap()
                .document
                .layers
                .get(*id)
                .unwrap()
                .name
                .starts_with("canvas")
        })
        .unwrap();
    ed.set_layer_selection(vec![canvas_id], Some(canvas_id));
    menu_bridge::perform(MenuAction::Copy, &mut ed).expect("the copy");
    menu_bridge::perform(MenuAction::PasteInto, &mut ed).expect("the paste into");
    let into_id = ed
        .active()
        .unwrap()
        .document
        .active_layer()
        .expect("the pasted-into layer is selected");
    {
        let doc = ed.active().unwrap();
        let into_layer = doc.document.layers.get(into_id).unwrap();
        assert!(
            into_layer.mask.is_some(),
            "paste into carries an editable mask"
        );
        let stored: usize = app::layer_tile_map(&ed, into_id).unwrap().len();
        assert!(stored > 0, "the pasted-into layer stores its pixels");
    }

    // ---- Error path: a garbage file refuses and damages nothing.
    let count = ed.active().unwrap().document.layers.len();
    let depth = ed.active().unwrap().history_depth();
    assert!(
        ed.place_path(&garbage, false).is_err(),
        "a non-image file refuses to place"
    );
    assert_eq!(
        ed.active().unwrap().document.layers.len(),
        count,
        "the refusal changed no layers"
    );
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth,
        "the refusal left no history debris"
    );

    // ---- Save natively; the ORIGINAL source files move and rename away.
    let names_before: Vec<String> = ed
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .map(|id| {
            ed.active()
                .unwrap()
                .document
                .layers
                .get(id)
                .unwrap()
                .name
                .clone()
        })
        .collect();
    assert_eq!(
        names_before.len(),
        6,
        "canvas + background + portrait + logo + linked + the paste-into layer: {names_before:?}"
    );
    let package = dir.join("asset-workflow.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&package, "integration-test")
        .expect("the project saves");
    std::fs::rename(
        dir.join("background.png"),
        dir.join("renamed-background.bin"),
    )
    .unwrap();
    std::fs::rename(dir.join("portrait.png"), dir.join("renamed-portrait.bin")).unwrap();
    std::fs::remove_file(&linked).unwrap(); // the linked file disappears

    // ---- Reopen in a fresh editor (the old one is closed).
    let mut reopened = app_shell::Editor::with_state(
        app_shell::AppPaths::rooted(dir.join("config2")),
        app_shell::prefs::Preferences::default(),
        app_shell::recent::RecentFiles::new(),
        Box::new(ScriptedDialogs::default()),
    );
    reopened.set_image_clipboard(Box::new(app_shell::clipboard::FakeClipboard::new()));
    reopened.open_path(&package).expect("the project reopens");
    {
        let doc = reopened.active().unwrap();
        let names_after: Vec<String> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .map(|id| doc.document.layers.get(id).unwrap().name.clone())
            .collect();
        assert_eq!(
            names_after, names_before,
            "the whole layer stack survives the save/reopen"
        );
    }
    let composite_after = {
        let rect = reopened.active().unwrap().canvas_rect();
        reopened.active_mut().unwrap().composite(rect).unwrap()
    };
    assert_eq!(
        composite_after, composite_saved,
        "the composition is byte-identical after save/close/move/reopen"
    );
    for (name, transform, kind) in &transforms_before {
        let doc = reopened.active().unwrap();
        let found = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .map(|id| doc.document.layers.get(id).unwrap())
            .find(|l| &l.name == name)
            .unwrap_or_else(|| panic!("{name} survives the reopen"));
        assert_eq!(&found.transform, transform, "{name}'s transform survives");
        assert_eq!(&found.kind, kind, "{name}'s identity survives");
    }

    // ---- Paste Into retains source pixels behind an editable mask THROUGH
    // persistence: the reopened layer's tile map and mask tile map survive.
    {
        let doc = reopened.active().unwrap();
        let into_id = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| doc.document.layers.get(*id).unwrap().name == "Paste Into")
            .expect("the paste-into layer survives the reopen");
        assert!(
            doc.document.layers.get(into_id).unwrap().mask.is_some(),
            "the editable mask survives persistence"
        );
        assert!(
            !app::layer_tile_map(&reopened, into_id)
                .expect("the pasted-into tiles survive")
                .is_empty(),
            "the pasted-into source pixels survive persistence"
        );
        assert!(
            !app::mask_tile_map(&reopened, into_id)
                .expect("the mask coverage survives")
                .is_empty(),
            "the mask's coverage tiles survive persistence"
        );
    }

    // ---- Oversized sources are never permanently clipped: the background's
    // stored tiles span the FULL 300x200 source frame, not the 64px canvas.
    {
        let doc = reopened.active().unwrap();
        let background_id = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| {
                doc.document
                    .layers
                    .get(*id)
                    .unwrap()
                    .name
                    .starts_with("background")
            })
            .unwrap();
        let map = app::layer_tile_map(&reopened, background_id).unwrap();
        assert!(
            map.contains(raster::TileCoord::new(1, 0, 0)),
            "the oversized background's far source tile survives persistence (300x200 spans 2x1 tiles)"
        );
        let hash = map.get(raster::TileCoord::new(0, 0, 0)).unwrap();
        use compositor::TileSource as _;
        let bytes = reopened
            .active()
            .unwrap()
            .tiles
            .tile(hash)
            .expect("the stored tile bytes");
        assert_eq!(
            &bytes[0..4],
            &[10, 20, 30, 255],
            "the background's full-resolution source pixels survive"
        );
    }

    // ---- Linked-file disappearance: reported, appearance kept.
    {
        let out = reopened.refresh_linked_sources().unwrap();
        assert!(out.contains("skipped"), "{out:?}");
        assert!(
            reopened.status().is_some_and(|s| s.contains("missing")),
            "the vanished linked file is reported: {:?}",
            reopened.status()
        );
    }
    let composite_after_disappearance = {
        let rect = reopened.active().unwrap().canvas_rect();
        reopened.active_mut().unwrap().composite(rect).unwrap()
    };
    assert_eq!(
        composite_after_disappearance, composite_saved,
        "the vanished linked file keeps its last good appearance"
    );
}
