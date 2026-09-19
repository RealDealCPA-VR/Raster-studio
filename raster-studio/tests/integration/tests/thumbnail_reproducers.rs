//! Task 004's retained failing reproducers: E01, E06, E08, E09.
//!
//! Each test drives the **real shell routes** — `app_shell::Editor`, the
//! `ToolPointer` route `shell.rs` feeds, the menu resolution a frame builds
//! through `ui::Workspace::menu_context`, and `ui::panels::text::commit`
//! exactly as the Character panel calls it (`ui/src/view/docks.rs:1798`).
//! On the baseline code each one fails for the intended behavior — a pixel,
//! coverage or emitted-edit fact — not for a missing menu label, and the
//! intended-failure output is recorded in `docs/THUMBNAIL-BASELINE.md`.
//!
//! They stay `#[ignore]`d **until their owning implementation card lands fix
//! and test together** (plan Task 004: "Do not leave future-feature failures
//! or ignored regressions in the normal suite and then claim a green phase
//! gate"). The normal suite stays green; these run explicitly:
//!
//! ```text
//! cargo test --locked -p integration-tests --test thumbnail_reproducers -- --ignored --nocapture
//! ```
//!
//! | Test | Gap | Owning card |
//! |---|---|---|
//! | `a_weight_only_character_edit_survives_the_panel_commit_route` | E01 | 016 (+ 021) |
//! | `a_placed_oversized_source_keeps_every_source_tile` | E06 | 046 |
//! | `painting_with_the_mask_selected_edits_mask_coverage_not_layer_pixels` | E08 | 055 — FLIPPED GREEN 2026-09-09 (the edit target routes painting; the `#[ignore]` is removed) |
//! | `the_four_mask_creation_ops_produce_four_distinct_coverages` | E09 | 057 — FLIPPED GREEN 2026-09-09 (the four ops attach real coverage; the `#[ignore]` is removed) |
//!
//! When an owning card lands, remove its `#[ignore]`, make it pass through the
//! same route, and record the flip in the progress ledger.

use app_shell::editor::Editor;
use app_shell::tool_input::ToolPointer;
use compositor::TileSource;
use editor_core::{Command, LayerPatch, Patch, Selection};
use glam::{IVec2, Vec2};
use integration_tests::app;
use layer_model::{LayerKind, LayerMask, MaskId};
use raster::{decode_path, PixelRect, TileCoord, TILE_SIZE};
use ui::menu::{MaskOp, MenuAction};

/// The committed Task 003 fixture the acceptance scene places.
fn oversized_fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("project-fixtures")
        .join("thumbnail-oversized.png")
}

/// The alpha byte of a composited RGBA8 canvas pixel.
fn alpha_at(comp: &[u8], w: u32, x: u32, y: u32) -> u8 {
    comp[((y * w + x) * 4 + 3) as usize]
}

/// E01 — "Text styling lost at the document boundary" (plan cards 016 + 021).
///
/// The Character panel edits a `TextRun` and commits through
/// `ui::panels::text::commit`. `TextLayer::from(run)` keeps only
/// text/family/size (`layer-model/src/layer.rs:437`), so a weight-only change
/// collapses to `next == current` and the panel emits **no intent at all**.
/// The intended behavior: the edit survives the document boundary, so the
/// commit emits the layer-kind edit.
/// Card 016 landed the lossless conversion (2026-09-09): the reproducer
/// passes through the same panel commit route and now runs in the normal
/// suite, per the Task 004 retention rule.
#[test]
fn a_weight_only_character_edit_survives_the_panel_commit_route() {
    // No file system: `blank` builds the document the shell would have.
    let mut doc = app::blank(256, 144, "E01");

    // A text layer through the real command route.
    let text = layer_model::TextLayer::legacy("Headline", "DejaVu Sans", 48.0);
    let layer = layer_model::Layer::with_kind("Headline", LayerKind::Text(text));
    let id = layer.id;
    doc.apply(Command::create_layer(layer)).expect("create");

    // The panel route: read the run out of the document, change one style
    // field, commit exactly as `ui/src/view/docks.rs` does.
    let (id, mut run) =
        ui::panels::text::active_text(&doc.document, Some(id)).expect("the text layer is active");
    assert!(
        ui::panels::text::Character::set_weight(&mut run, 700),
        "the weight change is a change"
    );
    // Intended: the edit the panel would send exists, because the weight the
    // user set survives the conversion into the document's text layer.
    match ui::panels::text::commit(&doc.document, id, &run) {
        Some(ui::intent::Intent::EditLayerKind { layer: got, .. }) => assert_eq!(got, id),
        other => panic!(
            "a weight-only Character edit must emit an edit intent (E01: the run collapses to \
             the three legacy fields), got {other:?}"
        ),
    }

    // Positive control, same route: a field that *does* survive round-tripping
    // emits. This pins the failure to the lossy conversion, not to the route.
    assert!(
        ui::panels::text::Character::set_size(&mut run, 64.0),
        "the size change is a change"
    );
    assert!(
        ui::panels::text::commit(&doc.document, id, &run).is_some(),
        "the same commit route emits when the field survives (control)"
    );
}

/// E06 — "Place clips the source before storing tiles" (plan card 046).
///
/// `Editor::place_path` stores only `min(source, canvas)` columns/rows
/// (`app-shell/src/editor.rs:1442-1443`): the source's far corners never reach
/// the pixel store, and no later fit-by-transform can bring them back. The
/// intended behavior: the placed layer's tiles span the **whole source**, and
/// the source's bottom-right pixel is recoverable from the store byte for byte.
#[test]
#[ignore = "E06 reproducer — fails until card 046 lands: placing an oversized source must keep every source tile"]
fn a_placed_oversized_source_keeps_every_source_tile() {
    let tmp = tempfile::tempdir().unwrap();
    // A canvas far smaller than the source, on both axes.
    let mut ed = app::shell_editor(tmp.path(), 128, 72);

    let fixture = oversized_fixture();
    let source = decode_path(&fixture).expect("the committed fixture decodes");
    let (ow, oh) = (source.width, source.height);
    assert!(ow > 128 && oh > 72, "the fixture is oversized: {ow}x{oh}");

    ed.place_path(&fixture, false).expect("the place runs");

    let doc = ed.active().unwrap();
    let placed = doc
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| {
            matches!(
                doc.document.layers.get(*id).map(|l| &l.kind),
                Some(LayerKind::SmartObject(_))
            )
        })
        .expect("placing created a smart-object layer");
    let tiles = doc
        .document
        .layer_tiles(placed)
        .expect("the layer has tiles");

    // The stored extent must cover the whole source.
    let want_cols = ow.div_ceil(TILE_SIZE);
    let want_rows = oh.div_ceil(TILE_SIZE);
    let mut min_tx = i32::MAX;
    let mut max_tx = i32::MIN;
    let mut min_ty = i32::MAX;
    let mut max_ty = i32::MIN;
    for (c, _) in tiles.iter() {
        min_tx = min_tx.min(c.x);
        max_tx = max_tx.max(c.x);
        min_ty = min_ty.min(c.y);
        max_ty = max_ty.max(c.y);
    }
    assert_eq!(
        (max_tx - min_tx + 1) as u32,
        want_cols,
        "E06: the placed layer must store all {want_cols} source tile columns (its canvas is \
         128 wide, the source {ow}); clipping the store loses the far corners forever"
    );
    assert_eq!(
        (max_ty - min_ty + 1) as u32,
        want_rows,
        "E06: the placed layer must store all {want_rows} source tile rows"
    );

    // And the source's bottom-right pixel is recoverable, byte for byte.
    let br = tiles
        .get(TileCoord::new(
            want_cols as i32 - 1,
            want_rows as i32 - 1,
            0,
        ))
        .and_then(|h| TileSource::tile(&doc.tiles, h))
        .expect("the bottom-right source tile is in the store");
    let (lx, ly) = ((ow - 1) % TILE_SIZE, (oh - 1) % TILE_SIZE);
    let off = ((ly * TILE_SIZE + lx) * 4) as usize;
    let last = ((oh as usize * ow as usize) - 1) * 4;
    assert_eq!(
        &br[off..off + 4],
        &source.rgba8[last..last + 4],
        "the source's bottom-right pixel survives placement"
    );
}

/// E08 — "Mask property selection doesn't pick a paint target" (plan card 055).
///
/// The Properties panel's Layer/Mask segmented control writes
/// `Workspace::property_focus` (`ui/src/view/docks.rs:1147-1155`) and nothing
/// consumes that state for painting: `tool_input.rs` sets
/// `paint_target = PaintTarget::Layer` unconditionally outside quick-mask
/// (`:523`, `:829`). The user story — select the mask, paint, coverage changes
/// — cannot happen. The intended behavior: with the mask focused, a stroke
/// edits the mask's coverage and leaves the layer pixels alone.
///
/// The click itself (control → field) is the widget connection the panel owns
/// and is pinned at the lines above; this reproducer starts from the state
/// that click produces and exercises everything the shell does afterwards.
#[test]
fn painting_with_the_mask_selected_edits_mask_coverage_not_layer_pixels() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ed = app::shell_editor(tmp.path(), 64, 64);
    let layer = app::the_opened_layer(&ed);

    // A mask to target, attached through the real command route.
    let mask = LayerMask::new(MaskId::new());
    ed.active_mut()
        .unwrap()
        .apply(Command::SetLayerProperties {
            layer_id: layer,
            patch: LayerPatch {
                mask: Patch::Set(mask),
                ..Default::default()
            },
        })
        .expect("the mask attaches");

    // The user clicked "Mask" — the Properties control OR the row's mask
    // thumbnail (card 055). The click emits `Intent::SetEditTarget`; the
    // chrome harvest resolves it to `Pick::EditTarget` and the shell applies
    // it (shell.rs apply_chrome → set_edit_target_kind) — exactly what the
    // harness replays here. The ui half (click → intent) is pinned by the
    // ui crate's thumbnail tests; this reproducer owns everything after.
    let mut workspace = ui::Workspace::new();
    workspace.property_focus = ui::panels::properties::PropertyFocus::Mask;
    let intent = ui::Intent::SetEditTarget { mask: true };
    match intent {
        ui::Intent::SetEditTarget { mask } => {
            ed.set_edit_target_kind(app_shell::edit_target::EditTargetKind::from_focus(mask))
        }
        other => panic!("the mask click emitted {other:?}"),
    }
    assert!(ed.edit_target_is_mask(), "the editor aims at the mask");

    // The active tool is the brush, as the tool palette would have set it.
    ed.set_tool(tools::ToolId::Brush);

    let layer_before = app::layer_tile_map(&ed, layer);
    let mask_before = app::mask_tile_map(&ed, layer);

    // Paint through the real pointer route.
    let mut pointer = ToolPointer::new();
    let outcomes = app::shell_stroke(
        &mut pointer,
        &mut ed,
        &[Vec2::new(16.0, 32.0), Vec2::new(48.0, 32.0)],
    );
    assert!(
        outcomes.iter().any(|o| o.reached_tool),
        "the stroke reached the tool: {outcomes:?}"
    );
    assert!(
        ed.active().unwrap().history_depth() >= 1,
        "the stroke committed (the route works end to end)"
    );

    // Intended: the mask gains the painted coverage…
    let mask_after = app::mask_tile_map(&ed, layer);
    assert_ne!(
        mask_after, mask_before,
        "E08: painting with the mask selected must edit the mask's coverage — today nothing \
         connects the Properties mask focus to a paint target, so the mask is untouched"
    );
    // …and the layer's own pixels are untouched.
    let layer_after = app::layer_tile_map(&ed, layer);
    assert_eq!(
        layer_after, layer_before,
        "E08: the stroke must not paint the layer's pixels while the mask is the target"
    );
}

/// E09 — "The four mask-creation variants resolve to the same patch" (card 057).
///
/// `MenuAction::Mask(op).resolve(&ctx)` answers the identical bare
/// `LayerMask::new(..)` for Reveal All, Hide All, Reveal Selection and Hide
/// Selection (`ui/src/menu.rs:2118-2129`) — a mask with **no coverage tiles**.
/// The compositor reads absent mask tiles as zero coverage
/// (`composite.rs::fill_mask` starts from 0 and only fills where tiles exist),
/// so at the baseline all four ops composite as *fully hidden* — the runtime
/// symptom is even stronger than the source-traced one, and it is what this
/// reproducer records.
///
/// The intended behavior, composited: Reveal All shows the layer everywhere,
/// Hide All hides it everywhere, and the two selection ops divide the canvas
/// at the selection edge — four distinct coverages.
#[test]
fn the_four_mask_creation_ops_produce_four_distinct_coverages() {
    const W: u32 = 64;
    const H: u32 = 64;
    // The selection ops get an explicit half-canvas rect. (`Selection::None`
    // means *every* pixel, not no pixels — see `Selection::is_empty`.)
    let left_half = Selection::Rect {
        min: IVec2::new(0, 0),
        max: IVec2::new((W / 2) as i32, H as i32),
    };

    let mut alphas: Vec<(&'static str, Vec<u8>)> = Vec::new();
    for (op, needs_selection) in [
        (MaskOp::RevealAll, false),
        (MaskOp::HideAll, false),
        (MaskOp::RevealSelection, true),
        (MaskOp::HideSelection, true),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let mut ed = app::shell_editor(tmp.path(), W, H);
        if needs_selection {
            app::set_selection(&mut ed, left_half.clone());
        }

        // The layer shows before the op runs — the only change below is the
        // mask the op attaches.
        let before = ed
            .active_mut()
            .unwrap()
            .composite(PixelRect::new(0, 0, W, H))
            .expect("the canvas composites");
        assert_eq!(
            alpha_at(&before, W, 0, 0),
            255,
            "the canvas.png layer shows before the mask op ({op:?})"
        );

        // The real menu route (card 057): the menu gates and routes the op
        // to the application, which rasterises the coverage and attaches
        // mask + coverage atomically through the shell's history.
        let workspace = ui::Workspace::new();
        // The menu still gates: the op must resolve as ENABLED (an action
        // intent) before the app performs it.
        match app::menu_intent(&workspace, &ed, MenuAction::Mask(op)) {
            Some(ui::Intent::Action(MenuAction::Mask(_))) => {}
            other => panic!("{op:?} did not resolve as an enabled action: {other:?}"),
        }
        app_shell::menu_bridge::perform(MenuAction::Mask(op), &mut ed)
            .unwrap_or_else(|e| panic!("{op:?} failed through the app route: {e}"));

        let comp = ed
            .active_mut()
            .unwrap()
            .composite(PixelRect::new(0, 0, W, H))
            .expect("the canvas composites");
        alphas.push((op.label(), comp));
    }

    // Reveal All: everything visible. At the baseline the op attaches a bare
    // no-tile mask, which composites as fully hidden — the intended failure.
    let (_, reveal) = &alphas[0];
    for y in 0..H {
        for x in 0..W {
            assert_eq!(
                alpha_at(reveal, W, x, y),
                255,
                "Reveal All must reveal ({x},{y}) with real coverage (E09: today the op is a \
                 bare LayerMask::new with no coverage tiles, identical to the other three)"
            );
        }
    }
    // Hide All: everything hidden. Today it is the identical reveal-everything
    // patch, so the layer stays fully visible — the intended failure.
    let (_, hide) = &alphas[1];
    for y in 0..H {
        for x in 0..W {
            assert_eq!(
                alpha_at(hide, W, x, y),
                0,
                "Hide All must hide ({x},{y}); today all four ops attach the same \
                 reveal-everything mask (E09)"
            );
        }
    }
    // Reveal Selection: only the left half shows.
    let (_, reveal_sel) = &alphas[2];
    assert_eq!(
        alpha_at(reveal_sel, W, 0, 0),
        255,
        "inside the selection shows"
    );
    assert_eq!(
        alpha_at(reveal_sel, W, W - 1, 0),
        0,
        "outside the selection is hidden"
    );
    // Hide Selection: the mirror image.
    let (_, hide_sel) = &alphas[3];
    assert_eq!(alpha_at(hide_sel, W, 0, 0), 0, "the selection is hidden");
    assert_eq!(
        alpha_at(hide_sel, W, W - 1, 0),
        255,
        "outside the selection shows"
    );

    // And the four outcomes are pairwise distinct — the plan's own card check.
    for i in 0..alphas.len() {
        for j in i + 1..alphas.len() {
            assert_ne!(
                alphas[i].1, alphas[j].1,
                "the four mask-creation ops must produce four distinct coverages (E09)"
            );
        }
    }
}

/// Silence the unused-import lint while the reproducer suite is ignored-only.
#[allow(dead_code)]
fn touches(_: &Editor) {}
