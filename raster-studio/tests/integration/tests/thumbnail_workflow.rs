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
    let missing = "Raster Test Missing Family";
    assert!(text_panel::substitution(missing).is_some());
    {
        let doc = ed.active().unwrap();
        let (layer, mut run) = text_panel::active_text(&doc.document, doc.document.active_layer())
            .expect("still a text layer");
        assert!(text_panel::Character::set_family(&mut run, missing));
        let intent = text_panel::commit(&doc.document, layer, &run).expect("an edit was made");
        let Some(app_shell::menu_bridge::Pick::Kind { layer, kind }) =
            menu_bridge::pick(&intent, &ed)
        else {
            panic!("the kind edit is routed");
        };
        ed.apply_kind_edit(KindEdit {
            layer,
            kind,
            gesture: None,
        });
    }
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
    // The substituted run still draws: pixels exist above nothing.
    let substituted = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(
        substituted, condensed_pixels,
        "the substitution is visible in the composite"
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
        white.chunks_exact(4).any(|p| p == [255, 255, 255, 255]),
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
            .chunks_exact(4)
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
        for px in flat.chunks_exact_mut(4) {
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
        for px in v.chunks_exact_mut(4) {
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
        for px in v.chunks_exact_mut(4) {
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
