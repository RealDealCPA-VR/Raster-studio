# Thumbnail workflow baseline — Task 001

Recorded 2026-09-07 against commit `9ba62c1` ("C14: real-NVDA attempt recorded and rolled
back; the listening stays human"). This is the M0 baseline for
`docs/THUMBNAIL-WORKFLOW-IMPLEMENTATION-PLAN.md`: what the application actually does today,
recorded separately from source-traced predictions (E01–E14). Nothing was fixed in this card.

## Environment and pre-change checks

| Item | Value |
|---|---|
| Commit | `9ba62c1`, branch `main`, clean tracked tree (untracked: the three plan docs, `.pi/`) |
| OS / host | Windows 11, GPU available (`--shot` evidence pipeline works) |
| Toolchain | started on cargo/rustc 1.98.0; **repaired to 1.98.1** (see the toolchain note below) |
| `cargo fmt --all --check` | PASS |
| `cargo check --locked --workspace --all-targets` | PASS (exit 0) |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | PASS (exit 0, no warnings) |
| `cargo test --locked --workspace` | recorded below in "Test suite" |

## How the findings were produced

Every gap claim E01–E14 was re-traced against the current checkout (line numbers in the plan
still resolve; drift noted where a function sits a few lines off its citation). Source-traced
verdicts are marked **[src]**. Where an existing test demonstrates the behavior, it was run
and is marked **[observed]** with the test name and result. Operations that need a human at
the window (real IME, OS clipboard against other apps, hardware) are named as not exercisable
headlessly rather than claimed.

### Toolchain repair (recorded, not silent)

The local stable toolchain was missing `bin/rustdoc.exe` (only its `.pdb` remained), so
`cargo test --doc` failed even for a fresh probe crate — a corrupt rustup component, not a
project defect. Repair sequence, executed and recorded 2026-09-07: stub the missing file so
component removal could proceed → `rustup self update` (1.29.0 → 1.29.1) →
`rustup toolchain uninstall stable` → `rustup toolchain install stable --profile default`
→ **rustc/rustdoc 1.98.1 (48a229cea 2026-09-01)**. A patch bump from 1.98.0; `Cargo.lock`
unchanged, every gate rerun on 1.98.1.

## E01–E14: predicted vs observed

| ID | Plan claim | Source-traced verdict (evidence, current lines) | Observed outcome |
|---|---|---|---|
| E01 | Text styling lost at document boundary | **CONFIRMED.** `layer-model/src/layer.rs:437` `TextLayer { text, font_family, size_px }` — nothing else. `text-engine/src/model.rs:198` `From<&TextLayer> for TextRun` resets style via `..CharStyle::default()`; `:218` `From<&TextRun> for TextLayer` keeps only 3 fields. `ui/src/panels/text.rs:97` `commit` collapses through that lossy impl; a weight-only or color-only edit produces `next == current` and emits **no intent at all**. Compositor `RunKey` (`compositor/src/text.rs:66-76`) keys only text/family/size/generation; `run_image` (`:152`) composites black default style. Module doc (`compositor/src/text.rs:21-25`) admits verbatim: "Per-run colour, weight and slant exist in `text_engine::TextRun` but have nowhere to be *stored*". | `cargo test -p ui alignment_survives_the_round_trip_through_the_document` passes and asserts only `size_px` survives the round trip — the panel's own test name is the observed symptom. |
| E02 | Canvas text editing rudimentary (append-only, no caret/range/enter-existing) | **CONFIRMED.** `tools/src/text.rs:27-33` module doc confesses: "It is not a text editor. There is no selection, no word wrap, no click-to-place-caret…". `TextSession` (`:57-64`) holds only `layer, origin, text` — no caret/anchor. `insert` (`:107`) `push_str`s at the end; `backspace` (`:127`) pops the last char; `on_pointer_down` (`:203`) *finishes* any open session instead of entering the layer under the pointer; `on_pointer_up` (`:236-262`) always mints a new empty `TextLayer`. `TextEdit` protocol (`tools/src/tool.rs:476-481`) has only `Insert`/`Backspace`. | Existing tools test `a_second_click_makes_a_second_layer_rather_than_moving_the_first` pins the wrong behavior as correct. text-engine already ships `ShapedText::hit_test`, `caret_stops`, `caret_rect`, `selection_rects` (`text-engine/src/edit.rs`) — consumed only by text-engine's own tests. |
| E03 | Free Transform resamples pixels from canvas/selection bounds | **CONFIRMED.** `tools/src/transform.rs:791 commit` loads a `ColorPatch`, resamples destructively, commits `Command::PaintTiles` (`:843-866`). Start bounds (`:903-919`): `ctx.selection.bounds()` else **whole canvas** — never object content bounds; `compositor::content_bounds` (`compositor/src/composite.rs:661`) exists but the tool never calls it. Un-rasterized text/shape/smart layers have no tiles → absent tiles read transparent (`tools/src/patch.rs:231`) → transform silently no-ops or rasterizes over. The non-destructive stack (`Layer.transform`, `level_transform`, `Command::TransformLayer` with invert-first undo at `editor-core/src/command.rs:904-921`) exists and works — Move uses it; Free Transform bypasses it. | Operation sequence reproducible; observed no-op on a text layer is implied by tile-absence (not separately exercised this card — covered by Task 004 reproducer). |
| E04 | Move options/picking unwired; raw-tile picking; multi-selection not in gestures | **CONFIRMED.** `tools/src/edit.rs:66` `MoveTool`, `auto_select: false` (`:80`); `layer_under` (`:92-104`) samples a raw 1×1 tile at document coords, alpha only — no layer transform, no mask, no visibility ( `iter_depth_first` at `layer-model/src/tree.rs:714` walks hidden layers too), no shape-path/text hit. Registry declares Move options as Bool (`registry.rs:231-232`) but the shell forwards **Choice options only**: `chrome.rs:538` `tool_choices` filters `OptionKind::Choice`; applied in `tool_input.rs:782-784` at pointer-down only. No `auto_select` reference exists anywhere in app-shell. `Document.layer_selection` exists (`editor-core/src/document.rs:201`) but `tools` has zero references to it; `ToolContext` carries only `active_layer`. | Toggle on the Auto-Select checkbox changes nothing in the running tool — reproducible by construction (no code path consumes the boolean). |
| E05 | Live overlays not wired: `CanvasSessions` populated only in tests | **CONFIRMED (stronger).** `ui/src/canvas/workspace.rs:72` `CanvasSessions` read by `CanvasHost::central_panel` — but production never calls `central_panel`: `chrome.rs:841-843` says "CanvasHost::central_panel is never called"; sole caller is `ui::Workspace::ui`, which "this application does not call" (`chrome.rs:3212`). Every `sessions.` assignment lives in `#[cfg(test)]` (`workspace.rs:377+`). | Symptom observed at source level; Task 012 will publish real tool geometry and capture a frame. |
| E06 | Place clips source before storing tiles; drop always opens a document | **CONFIRMED.** `app-shell/src/editor.rs:1442-1443` `let w = image.width.min(cw); let h = image.height.min(ch);` then rows cropped and only clipped tiles inserted into `doc.tiles` (`:1466-1470`) — while full file bytes are kept in the asset table (`:1431-1435`). Same min-clip in linked refresh (`:1537-1538`). `shell.rs:1774-1778` `DroppedFile` → `open_paths` unconditionally opens a new tab (image or project). | Reproducer: place a 4096-wide source into a 64-wide canvas; stored tile grid covers 64 columns only (verifiable from `place_path` code; behavioral reproducer lands in Task 004). |
| E07 | No OS bitmap clipboard; internal canvas-sized paste | **CONFIRMED.** `menu_bridge.rs:1889-1901` `paste` reads the in-process buffer, allocates canvas-sized `rgba`, clips with `min(h)/min(w)`. `editor.rs:551-559` doc: "**In-process, not the system clipboard.**". arboard 3.6.1 is transitive only (via egui-winit), and its Cargo.lock dependency list has **no `image` crate** — the `image-data` feature is not enabled; no workspace member declares arboard. | A real screenshot→app paste cannot work by construction; OS-level manual check recorded as the Task 051 gate. |
| E08 | Mask property selection doesn't pick a paint target | **CONFIRMED.** Only two `PaintTarget` assignments outside quick-mask: `tool_input.rs:523` and `:829`, both `ctx.paint_target = PaintTarget::Layer;` (comment at `:818-820`: "Nothing in this shell selects a mask for editing yet"). `PropertyFocus::Mask` (`ui/src/panels/properties.rs:24-29`) changes only which Properties subject is displayed; consumed only by a dock tab-index. `PaintTarget::Mask` is reachable solely via quick-mask. | Selecting a mask in Properties and painting writes layer pixels — observed at source; reproducer in Task 004. |
| E09 | Four mask-creation variants resolve to the same patch | **CONFIRMED.** `ui/src/menu.rs:2118-2129`: `RevealAll | HideAll | RevealSelection | HideSelection =>` identical `Command::SetLayerProperties { mask: Set(LayerMask::new(MaskId::new())) }`. `LayerMask::new` (`layer-model/src/mask.rs:117-127`) is "fully-enabled… full density" — so even *Hide All* reveals everything. The four ops ride through as `Intent::Document(cmd)` (`menu_bridge.rs:221`); only Toggle/ToggleLink/Apply have shell routes (`menu_bridge.rs:1060-1062`). menu.rs's comment "which the application rasterises" is aspirational — no such route exists. | Existing menu test `adding_a_mask_is_a_command_and_a_second_one_is_refused` (`menu.rs:3126`) asserts command emission, not coverage — consistent with the gap. |
| E10 | No complete portrait-refinement workflow | **CONFIRMED.** Morphology exists (`selection/src/modify.rs`: `expand` :337, `contract` :344, `feather` :356, `smooth` :434, `invert` :530). Mask painting exists only inside quick-mask (scratch layer, `editor.rs:967-987`). No selection→layer-mask seeding (E09), no direct mask paint target (E08), no mask edge-preview/commit-cancel surface. Parity matrix: "Select Subject … no segmentation model ships" (`docs/parity-matrix.md:66`) — Select Subject stays deferred per plan; the manual chain is what's missing. | Not exercisable end-to-end today; M6 (055–064) owns the fix. |
| E11 | PSD import lossy: type→pixels; effects reported, not mapped | **CONFIRMED.** `import.rs:900-902` effects only `tally.effects.push(name)`; `:1012-1018` text layers `wants_pixels = true` + `tally.type_layers.push(name)`; notes at `:463/:467` say "imported as pixels; the text is no longer editable" / "were not imported". `psd/src/text.rs:20-26`: deliberately no TySh writer. | Existing interchange test `a_layered_psd_file_round_trips_through_the_psd_crate_with_structure_and_pixels` (`tests/integration/tests/interchange_and_recovery.rs:184`) covers structure/pixels only — a self-round-trip, not independent evidence. |
| E12 | PSD export: good merged preview, incomplete editable layers | **CONFIRMED.** `import.rs:1325-1328` Text/Shape/SmartObject → `tally.no_pixels` (written "as empty layers", `:484`); only Invert serialized (`:1317-1324`); effects tallied, never serialized (`:1294-1295`); `translation_of` (`:658-669`) marks anything beyond integral translation inexpressible. `doc.rs:1202-1207` `export_psd_to` uses the real compositor for the merged preview — the exact asymmetry claimed. | Same interchange test; merged preview correctness does not imply layer editability (plan's rule). |
| E13 | Docs overstate readiness | **CONFIRMED.** `docs/parity-matrix.md:35` "guides … are not saved" — but `Guide` is "Persisted with the document" (`editor-core/src/document.rs:113-120`), serialized (`:384`), undoable (`command.rs:466`), tested (`:1749`). `:119` "Smart objects ⬜ … nothing renders it" — but compositor renders SmartObject via `fill_layer` (`composite.rs:663-665, 1061`) and placement fills its tiles (`editor.rs:1466-1476`). `:115` rich text "✅" contradicts E01. Stale comment also at `composite.rs:444-445`. | Documentation reconciliation is Task 092; these three rows are the known contradictions to fix there. |
| E14 | Full composition performance unvalidated | **CONFIRMED.** `tests/integration/tests/performance.rs` measures 10 solid-color raster layers on a 4096×4096 canvas with cache-ratio and exact-recompute assertions. No text, portrait, mask, effect, transform, or 3628×2041 scene exists in any benchmark. | Task 086 owns the real-scene measurement; no numbers invented here. |

## Operation walkthroughs (reproducible sequences)

Each row: the operation the acceptance sequence needs, today's reproducible sequence through
the real UI routes, and the observed outcome. "Not separately exercised" rows are honest:
the behavior is pinned at source level and the behavioral reproducer is Task 004's deliverable.

1. **Create thumbnail document** — File ▸ New (1280 × 720 preset exists) → works; canvas size honored. *(Existing preset verified in menus; no defect.)*
2. **Change text weight/color via Character panel** — select text layer, set weight/color → commit collapses the run to three fields; weight/color edits emit **no document change** (E01). Observed by `alignment_survives_the_round_trip_through_the_document` (only `size_px` round-trips).
3. **Re-edit the headline** — click Type on the text → a second layer is created instead of entering the first (E02; pinned by existing tools test `a_second_click_makes_a_second_layer_rather_than_moving_the_first`).
4. **Place an oversized image** — File ▸ Place → source decoded, full bytes retained as asset, but only `min(cw) × min(ch)` upper-left tiles stored (E06). Behavioral reproducer assigned to Task 004.
5. **Move/scale it** — Move tool emits `Command::TransformLayer` (non-destructive, works for raster layers with tiles); Free Transform resamples pixels from canvas/selection bounds (E03); auto-select checkbox does nothing (E04).
6. **Create the four raster-mask variants** — each menu item emits the identical reveal-everything `LayerMask::new` patch (E09). Existing test `adding_a_mask_is_a_command_and_a_second_one_is_refused` verifies emission only.
7. **Paint a mask** — impossible outside quick-mask (E08); quick-mask paints a scratch layer instead of the layer's mask.
8. **Import/export layered PSD** — works with losses reported honestly in `PsdNotes` (E11/E12): type as pixels, effects dropped, text/shape/smart-object layers exported without pixel records, non-Invert adjustments dropped, non-translation transforms inexpressible.
9. **OS clipboard bitmap paste** — unavailable; internal buffer only (E07).
10. **IME composition against a text session** — not exercisable headlessly on this host without a real IME; Task 029 owns the synthetic tests + manual check.

## Test suite and application evidence

- `cargo test --locked --workspace` — **PASS, exit 0** (2026-09-07/09 host clock; unit + integration + doc tests, including `interchange_and_recovery.rs` and `performance.rs`).
- Real application launch: `cargo run --locked -p studio-desktop -- --shot ../shot-baseline-start.png` — the shell initialized wgpu, rendered the first frame, and captured it (log: "captured screenshot to ../shot-baseline-start.png"). The capture lives at the repository root as launch evidence; headless text-recording of its pixels was not performed because this executor has no image viewer, and no completion claim rests on the image's content.
- Not exercisable on this host: a real IME session, OS-clipboard exchange with other applications, pen/tablet input. These are named in the walkthrough above and owned by their cards (029, 051).

## Fixtures available today

- `tests/integration/src/fixture.rs`: procedural `photo_rgba8` generators, image writers, pixel comparison helpers (145 LOC).
- `tests/integration/src/app.rs`: headless document constructors (`blank`, `linear`, `open_image`, `open_project`) and `DocTiles` adapter (389 LOC).
- `tests/project-fixtures/` and `tests/golden-images/`: empty (READMEs only) — no committed composition/PSD fixtures exist yet (Task 003 / Task 073).
- Fonts: the licensed test font fixture referenced by text-engine tests (see `crates/text-engine` font loading); no installed-font dependency.

## Task 004 — shell-route regression harness and retained reproducers (2026-09-09)

Task 004 extended the integration harness (`tests/integration/src/app.rs`) with the real
shell routes the reproducers drive, and added four retained failing reproducers in
`tests/integration/tests/thumbnail_reproducers.rs`. Every reproducer drives the product's
own routes — `app_shell::Editor`, the `ToolPointer` route `shell.rs` feeds, the menu
resolution a frame builds through `ui::Workspace::menu_context`, and
`ui::panels::text::commit` exactly as the Character panel calls it — and each fails for
the intended behavior, not for a label.

### Harness additions (`tests/integration/src/app.rs`)

- `shell_editor(dir, w, h)` — an `Editor` holding one white `w × h` canvas opened through
  `Editor::open_path`, camera at 100% centred over a 400 × 300 viewport (the same setup
  app-shell's own pointer-route tests use), plus `shell_screen_pt`/`shell_pointer`/
  `shell_stroke` driving `ToolPointer::handle` with real `PointerInput` samples.
- `menu_intent(workspace, editor, action)` — the intent a menu item emits, resolved
  through the frame's real `MenuContext`; `apply_intent` applies it through the editor's
  history like `ChromeOutput::commands` are applied.
- `layer_tile_map` / `mask_tile_map` (before/after identity checks), `set_selection`
  (the marquee's field write), `the_opened_layer`.
- Dependency note: `ui` joined integration-tests' regular dependencies and `text-engine`
  its dev-dependencies (Cargo.lock +3 lines, offline-resolvable edges only).

### The four reproducer runs (exact intended failures, `-- --ignored --nocapture`)

```text
test a_weight_only_character_edit_survives_the_panel_commit_route ... FAILED
a weight-only Character edit must emit an edit intent (E01: the run collapses to the three
legacy fields), got None
test a_placed_oversized_source_keeps_every_source_tile ... FAILED
E06: the placed layer must store all 22 source tile columns (its canvas is 128 wide, the
source 5442); clipping the store loses the far corners forever
  left: 1
  right: 22
test painting_with_the_mask_selected_edits_mask_coverage_not_layer_pixels ... FAILED
E08: painting with the mask selected must edit the mask's coverage — today nothing
connects the Properties mask focus to a paint target, so the mask is untouched
  left: None
  right: None
test the_four_mask_creation_ops_produce_four_distinct_coverages ... FAILED
Reveal All must reveal (0,0) with real coverage (E09: today the op is a bare
LayerMask::new with no coverage tiles, identical to the other three)
  left: 0
  right: 255
```

Each reproducer also carries non-label evidence that the route itself works: E08 asserts
the stroke reached the Brush tool (`outcomes…reached_tool`) and committed one history
step before asserting the mask did not change; E09 composites the layer before the op
(alpha 255) so the only variable is the attached mask; E06 checks the bottom-right
source pixel once the extent assertion can pass; E01 carries a positive control (a size
edit through the same `commit` emits).

### Runtime refinement of E09's prediction

The baseline's E09 row predicted the identical `LayerMask::new` patch "reveals
everything". The runtime shows the sharper truth: `LayerMask::new` carries **no coverage
tiles**, and `composite.rs::fill_mask` reads absent mask tiles as zero coverage — so at
the baseline **all four ops composite as fully hidden** (the pre-op composite shows the
layer, alpha 255; after any of the four ops it is alpha 0 everywhere). The four ops are
indistinguishable *and* none performs its named operation. Card 057's implementation
must write real coverage (including outside-tile defaults), which is exactly what the
reproducer's composite assertions demand.

### Retention rule

The four tests stay `#[ignore]`d (each `#[ignore]` reason names its owning card:
E01→016+021, E06→046, E08→055, E09→057). They are not part of the green suite; they run
explicitly via `cargo test --locked -p integration-tests --test thumbnail_reproducers --
--ignored --nocapture`. When an owning card lands fix and test together, its
`#[ignore]` is removed, the test must pass through the same route, and the flip is
recorded in the progress ledger.

### Harness hygiene found on the way

`crates/app-shell/src/tool_input.rs` printed `ROUTED in_gesture=… route=…` with
`eprintln!` on **every routed pointer sample** — pre-existing debug noise (last touched
in 741761a) that polluted every reproducer's recorded output. Removed in this card
(one line, no behavior change); app-shell's own suite covers the pointer route.

## Task 001 conclusion

All fourteen predictions E01–E14 are CONFIRMED against commit `9ba62c1`; none were refuted.
The application builds clean and every existing gate is green, so the plan's premise holds:
the gaps are real integration gaps, not stale analysis. No code was changed in this card.

## Task 005 — reuse and dependency inventory (2026-09-09)

Every subsystem the thumbnail workflow needs already has an owner in the workspace; the
plan's proposed modules are extraction seams, not rebuilds. Verified against the current
checkout (HEAD `9ba62c1` + this goal's uncommitted work).

### 1. Text shaping + caret geometry
**Owner:** `text-engine` + `compositor/src/text.rs`.
- `text-engine/src/layout.rs:255` `shape(library, &TextRun) -> ShapedText` (cosmic-text 0.17.2); `ShapedText` fields `layout.rs:185-204`. Production consumer: `compositor::text::run_image` (`compositor/src/text.rs:160`); also `ui/src/canvas/text_overlay.rs:75 TextLayout::from_shaped`.
- `text-engine/src/edit.rs`: `CaretStop` (:29), `caret_stops` (:49), `line_at_y` (:111), `line_of_index` (:125), `hit_test` (:137), `caret_rect` (:160), `selection_rects` (:189) — complete caret/selection geometry, **tests-only today**; cards 025-030 reuse these instead of growing `text_overlay.rs`.
- `text-engine/src/raster.rs`: `rasterize` (:433, `CoverageMask`), `render_linear` (:584), `fill_linear` (:567), `GlyphRasterCache` (:224), `CoverageMask::ink_bounds` (:129).
- Compositor cache: `RunKey {text, family, size_bits, generation}` (`text.rs:58`, max 64), `engine()` mutex (:76), `load_font` (:100), `font_families` (:112), `ink_bounds` (:171). `RunKey` omits style fields — card 020 extends it.
- Gap: no text session owner — proposed `app-shell/src/text_session.rs` (plan table).

### 2. Layer content bounds
**Owner:** `compositor/src/composite.rs` (private `Ctx` methods; card 008 promotes to `compositor/src/bounds.rs`).
- `Ctx::content_bounds` (:661) — pre-transform ink extent per layer kind; group children mapped through their own transforms.
- `Ctx::document_bounds` (:486) — content bounds through `level_transform` (:1133); effect reach is separate (`style_reach` :446, `style_rect` :460).
- `Ctx::tile_map_bounds` (:695); `compositor::shape::ink_bounds` (`shape.rs:137`).
- Affine rect helpers (:1298-1550): `is_identity`, `expand_rect`, `preimage_rect`, `image_rect`, `rect_from_bounds`, `intersect_rects`, `clip_to`, `union_rects`, `centre_window` — NaN/saturation-safe; reuse, do not re-derive.

### 3. Transform commit paths
- **Non-destructive commit (reuse for cards 034-045):** `Command::TransformLayer {layer_id, matrix}` (`editor-core/src/command.rs:311`, pre-multiplied delta); apply arm :904-919 computes the inverse FIRST (`invert_delta` :1124) so undo never stores NaN; refuses locked layers. Absolute form: `LayerPatch.transform` (:111-139). Multi-step: `Command::Transaction`.
- **Destructive resample (explicit bakes only):** `TransformTool::commit` (`tools/src/transform.rs:791`), `resample` (:590), `ColorPatch`/`CoveragePatch` (`tools/src/patch.rs:213/:402`), `quad_affine` (:718), `selection::transform_selection` (`selection/src/transform.rs:340`).
- Live gizmo: `TransformState` (`transform.rs:261`: `source_corners`, `handles`, `hit_test`, `drag`, `dest_bounds`), `Homography::from_quads` (:158), `WarpMesh` (:107).

### 4. Masks
- `layer-model/src/mask.rs`: `LayerMask` (:58: id/kind/linked/enabled/density/feather_px/inverted), `coverage(sample)` (:199), `set_density`/`set_feather_px` (:152/:168), `affects_composite` (:185), `new`/`vector` (:96/:108).
- Pixel edits: `PixelTarget::Mask(LayerId)` (`editor-core/src/pixels.rs:103`; `NoMask` refusal at apply), `Command::PaintTiles` (:332/:590), `FillRegion` + `MaskCoverage {HIDDEN, REVEALED}` (:348, :509-540), `MASK_TILE_BYTES` (:501); **absent mask tile = zero coverage** (the E09 runtime finding).
- Compositor: `active_mask` (:729, vector-without-tiles renders unmasked), `mask_coverage` (:970), `mask_sample` (:1009, feather clamp), `fill_mask` (:1097), Gaussian `blur`/`boxes_for_gauss` (:1626/:1649).
- Query: `Document::mask_tiles` (`editor-core/src/document.rs:567`).
- Selection→mask seeding pieces: `selection::channel::selection_to_mask_tiles` (`selection/src/channel.rs:76`, sparse store-format tiles) + `mask_tiles_to_selection` (:122), `boolean::to_mask` (`boolean.rs:117`). No app-shell route yet — cards 055/057; proposed `app-shell/src/mask_edit.rs`.

### 5. Selection
**Owner:** `selection` crate + `editor-core/src/selection.rs`.
- `SelectionMask` (:67: `coverage_at` :175, tight O(1) `bounds` :190); `Selection` enum (:296: None=ALL pixels, `coverage_at` :372, `is_empty` :331).
- `selection/src/modify.rs`: `expand` (:337), `contract` (:344), `feather` (:356), `smooth` (:434), `border` (:512), `invert` (:530).
- Marching ants: `selection/src/outline.rs::outline(&SelectionMask, threshold) -> Vec<Polyline>` (:62) — already consumed by `app-shell/src/presenter.rs:690` and `ui/src/canvas/ants.rs`.
- Also: `CoverageBuf` (`buf.rs:163`), `Rect::of_selection_bounds` (`rect.rs:121`), `boolean::{combine, to_mask}` (`boolean.rs:86/:117`), marquee/lasso/wand generators, `transform` (`transform.rs:181`) + `ResampleFilter` (:47), channel I/O (`channel.rs:34-122`).

### 6. Effects
- Schema `layer-model/src/effects.rs`: `LayerEffects` (:48, 10 slots), `ShadowEffect` (:152), `GlowEffect` (:232), `FillStyle` (:217), `BevelEffect` (:303), `SatinEffect` (:358), `ColorOverlayEffect` (:387), `GradientOverlayEffect` (:474), `PatternOverlayEffect` (:542), `StrokeEffect` (:570 + `StrokePosition` :561).
- Rendering `compositor/src/effects.rs`: `reach` (:103, clamped `MAX_REACH=256`), `render` (:175, behind→interior→stroke), `drop_shadow` (:521), `stroke` (:780, inside/outside/center), `bevel` (:818), `signed_distance` (:349), `silhouette/grow` (:427), `blur` (:460). Stroke+glow+overlays exist; pattern fills draw nothing (no asset store); contours/glow-jitter/overprint/bevel-technique not implemented (module docs admit).
- Effect reach participates in `Ctx::tile_input_key` (`composite.rs:1148`); tiles cached by `TileCompositor` (`cache.rs:53`).

### 7. Export
- `raster/src/codec.rs`: `ExportFormat` (:967: Png, Jpeg(u8 1..=100), WebP lossless, Tiff, Gif, Bmp; `supports_16_bit` :1051 PNG/TIFF), `EncodedPixels::{Rgba8, Rgba16}` (:1087, 16→8 is an error not a silent downconvert), `encode` (:1281), `encode_to_path` (:1373), `encode_pdf` (`pdf.rs:24`).
- Color-correct conversion: `compositor/src/canvas.rs::Canvas::to_rgba8(&ColorSpace)` (:154 — unpremultiply → `color::from_linear` → quantize), `to_rgba16` (:173), `to_straight` (:143); `raster::export::{rgba8_from_linear :423, rgba16_from_linear :428, linear_from_rgba8 :456}`.
- Compositing: `composite_region/rect/subtree` (`composite.rs:161/:202/:219`); cached `TileCompositor::composite_region` (`cache.rs:87`).
- Routes: `Editor::export_layers` (`editor.rs:1144`), `export_diagnostics` (:1204), `flatten_all_layers` (:1349); `OpenDocument::composite_rgba16` (`doc.rs:751`); package preview `project-format/src/preview.rs:71`; per-layer thumbnails `OpenDocument::layer_thumbnail` (`doc.rs:770`) + `box_downscale` (:678).

### 8. Fonts
- `compositor::load_font(Vec<u8>) -> usize` (`text.rs:100`, bumps generation + clears caches); `text_engine::FontLibrary` (`text-engine/src/font.rs:115`): `with_system_fonts()` (:137, fontdb; engine seeded at `text.rs:76-81`), `load_bytes` (:147), `families()` (:169), `resolve(family, weight, slant) -> Option<FaceMatch>` (:215, reports faked matches), `face_metrics` (:239).
- Fixture font: `dejavu` 2.37 (`dejavu::sans::regular()`); loaders `compositor::testkit::text_fixture_family` (cfg(test)) and `fixture::thumbnail::load_fixture_font` (`fixture/thumbnail.rs:54`).

### 9. Test fixtures
- `tests/integration/src/fixture.rs`: `photo_rgba8` (:16), `photo_rgba8_with_alpha` (:36), `photo_rgba8_channels_cycled` (:56), `write_image` (:68), `pixel_at` (:86), `differing_pixels` (:95), `max_channel_diff` (:107), `mean_channel_diff` (:117), `srgb8_of_linear` (:134), `linear8` (:145).
- `fixture/thumbnail.rs`: `SMALL_CANVAS (256×144)` :39, `FULL_CANVAS (3628×2041)` :43, `load_fixture_font` :54, `fnv1a64` :62, `layout()` :113, generators `background_rgba8` :145 / `oversized_source_rgba8` :170 / `logo_rgba8` :200 / `portrait_rgba8` :235 / `portrait_mask_coverage` :274, `write_assets` :300, `build_scene` :388 (root: Alternatives group → clipped Portrait-tone adjustment → masked Portrait → Headline/Subhead text → Background; nested Logos group; hidden Headline B).
- `src/app.rs`: `DocExt` (:282), `blank` (:65), `open_image/open_project` (:122/:127), shell routes (`shell_editor` :154, `shell_pointer` :191, `shell_stroke` :203, `menu_intent` :229, `apply_intent` :241), tile-map readers (:258/:265), `set_selection` (:274).
- `tests/project-fixtures/`: 4 committed PNGs + `thumbnail-scene-3628x2041.rstudio/` (hashes in README); `tests/golden-images/`: README only, no goldens yet.
- Suites: `end_to_end.rs` (11), `interchange_and_recovery.rs` (10), `performance.rs` (6), `thumbnail_workflow.rs` (5+1 ignored), `thumbnail_reproducers.rs` (4 ignored reproducers).

### 10. Clipboard
- `Editor.clipboard: Option<Clipboard>` (`app-shell/src/editor.rs:559`); `Clipboard {width, height, rgba8}` (:564) — in-process only (docs :549-557); `paste(editor, into)` (`menu_bridge.rs:1889`) mints a raster layer in one transaction; Paste Into masks via `pixels::mask_by_selection` (`menu_bridge.rs:818`); `ui::ClipboardState` (`ui/src/intent.rs:310`).
- **arboard 3.6.1 is transitive only** (via egui-winit 0.29.1; `Cargo.lock:233-247`); no workspace member declares it; `image-data` off. Card 051 promotes it to a direct `app-shell` dependency — a manifest edit, not a new dependency; proposed owner `app-shell/src/clipboard.rs`.

### 11. Interaction geometry
**Owner:** `ui/src/canvas`. `CanvasCamera` (`camera.rs:76`): `screen_pt_of/screen_px_of` (:260/:265), `doc_of_screen_pt/px` (:271/:318), matrix forms (:231-255), `visible_doc_rect` (:504), `fit_rect/fill_document/zoom_about_screen_pt` (:440/:455/:387), view-state round-trip (:534/:545); `Viewport` (`viewport.rs`, `content_bounds_pt` :229). `InputRouter` (`input.rs:238`), `PointerInput`/`PointerPhase` (:51/:43), egui bridge (`pointer.rs` :34/:95). Hit tests: `handles.rs:181`, `crop.rs:279`. Snapping: `snapping.rs` (`collect_candidates` :217, `snap_point` :374, `snap_rect` :417, `tool_snaps` :190); guides `rulers.rs` (:424/:468); ants `ants.rs` (:75/:116). Gap: no shell-owned conversion layer — card 009 proposes `app-shell/src/interaction_geometry.rs`.

### 12. Layer tree operations
**Owner:** `layer-model/src/tree.rs`: `push_root` :333, `insert_at` :348, `remove`→`DetachedSubtree` (:392/:120), `reinsert` :438 (orphan/cycle validation), `move_layer` :477 (`WouldCycle` guard :487/:567 via `is_descendant_of` :623), `group_layers` :531, `clipping_group` :660, `iter_depth_first` :714, `subtree_ids` :725, `validate` :736. `Layer` (`layer.rs:46`): `transform` :64, `mask` :78, `clipping` :79, `linked` :88, `effective_opacity` :179, `LockState::blocks_{pixel_edit,transform}` (:202-222). `BlendMode::ALL` = 27 modes (`blend.rs:54`, pinned count).

### Dependency facts (verified in `Cargo.lock`)
- `cosmic-text` **0.17.2** locked; declared `0.17` (no default features; `std`+`swash`) by `text-engine` only — the single shaper; no duplicate shaper exists or is planned.
- `arboard` **3.6.1** transitive via `egui-winit 0.29.1`; no workspace member declares it; `image-data` feature off.
- `dejavu` **2.37.0**; `glam` **0.29.3** (workspace `0.29` + serde).
- **No new core dependencies required by the plan's phases.** Card 051 promotes arboard from transitive to direct (manifest edit only). Everything else reuses existing crates.

## Task 006 — frozen acceptance scene and verification matrix (2026-09-09)

### The frozen scene

The acceptance scene is the Task 003 fixture scene (`fixture::thumbnail::build_scene`)
**plus** the two elements later cards deliver through the application. Layer order
(root, top to bottom):

| # | Layer | Kind | Frozen facts | Delivered by |
|---|---|---|---|---|
| 1 | `Placed background` | SmartObject (oversized source) | `thumbnail-oversized.png` (5442×3628), full source tiles retained, fit-by-transform | cards 046–048 (E06 reproducer flips) |
| 2 | `Alternatives` (group) | Group | holds 1a–1c below | exists in fixture |
| 1a | `Headline A` | Text | DejaVu Sans, styled (bold + tracking), visible | fixture `build_scene`; styled via cards 015–024 |
| 1b | `Logos` (group) → `Logo` | SmartObject w/ transparency | `thumbnail-logo.png` (512², hole + notch + feather) — placed graphic #1 | fixture; re-placed via app in card 046 |
| 1c | `Headline B` | Text | hidden alternative — toggled in step 12 | fixture (hidden) |
| 3 | `Headline shadow` effect | LayerEffect on the big headline | drop shadow + stroke outline, editable via Layer Style dialog | cards 066–067 (effects render exists: `compositor/src/effects.rs`) |
| 4 | `Portrait tone` | Adjustment (Curves/Levels), `ClipToBelow` | clipped to the portrait only | fixture; verified card 068 |
| 5 | `Portrait` | Raster + raster mask | `thumbnail-portrait.png` (1361×1814, soft edges); mask coverage = horizontal ramp, deliberately ≠ alpha; editable mask | fixture; mask workflow cards 055–063 |
| 6 | `Subhead` | Text (green, smaller) | second editable headline, family/weight/size/tracking via real controls | cards 015–024 (E01 reproducer flips) |
| 7 | `Background` | Raster | `thumbnail-background.png` (3628×2041 gradient, exact stops) | fixture |

Two placed graphics: the oversized background (#1) and the Logo (#1b); a third arrives
if paste routes are exercised (step 8 "place or paste two logos" uses Logo + a second
paste of the same asset — the same `logo_rgba8` fixture, exercising asset reuse).
Synthetic-portrait rule from Task 003 still applies: fixture coverage proves mechanics,
not real-hair extraction.

### Sizes, zooms, scaling

| Canvas size | Fixture support | Notes |
|---|---|---|
| 1280 × 720 | `File ▸ New` preset (verified in baseline walkthrough #1); export target of step 15 | small acceptance run |
| 1920 × 1080 | `NEW_DOCUMENT_SIZE` default (`app-shell/src/editor.rs:182`) | default document |
| 3628 × 2041 | `FULL_CANVAS` fixture + committed `thumbnail-scene-3628x2041.rstudio` | the milestone scene |

| Zoom | Automated evidence | Manual |
|---|---|---|
| 33.33% | card 030 overlays, card 042 centering at fit zoom, card 084 export | — |
| 100% | `shell_editor` camera default (harness), pointer-route tests | — |
| 200% | card 030, card 042, step 18 | — |

| Windows display scaling | Evidence |
|---|---|
| 100% | automated where the host allows; recorded per card |
| 150% | **manual hardware verification** (marked; host-bound like C14) |
| 200% | **manual hardware verification** (marked) |

Zoom × scaling combinations: steps 18's grid is executed at 100% scaling headlessly
(zooms are camera state, already exercised by `shell_editor`/`CanvasCamera`); the
150%/200% scaling rows need the physical display session and are recorded honestly in
cards 082/091, not claimed.

### Acceptance sequence → fixtures/tests/cards map

| Step | Maps to | Fixture / test / card |
|---|---|---|
| 1 | New 3628×2041 + Fit/33.33%; 1280×720 preset | `FULL_CANVAS`; presets (verified); card 082 zoom readouts |
| 2 | Oversized place, no crop | `thumbnail-oversized.png`; reproducer `a_placed_oversized_source_keeps_every_source_tile`; card 046 |
| 3 | Portrait + selection → mask + paint | `thumbnail-portrait.png`, `portrait_mask_coverage`; cards 055–058 (E08/E09 reproducers flip) |
| 4 | Mask views; source pixels intact | card 059; `mask_tile_map` readers |
| 5 | Two styled headlines | cards 015–024; `load_fixture_font`; E01 reproducer flips |
| 6 | Re-enter, replace word, restyle range | cards 025–033 |
| 7 | Z-order arrangement | card 065 |
| 8 | Two logos, content-bounds select/resize | cards 046–054 + 034–042; `logo_rgba8`; bounds card 008 |
| 9 | Aspect/rotation/numeric/snap/align/group/multi-move | cards 039–043; `snapping.rs` candidates exist |
| 10 | Cancel transform/edit; clean history | cards 011, 031, 039 |
| 11 | Shadow + outline styles; clipped adjustment | cards 066–068; `effects.rs` render paths |
| 12 | 30–50 layers, alternatives, rename, replace contents | cards 065, 069, 070; fixture Alternatives/hidden Headline B |
| 13 | Native save/reopen preserves all | card 088; `thumbnail_workflow.rs` round-trip + card 018 migration |
| 14 | Duplicate variant; template unchanged | card 070 |
| 15 | Export 1280×720 JPEG/PNG with policies | card 084; `ExportFormat` + `Canvas::to_rgba8` |
| 16 | PSD fixture → report → edit → export → independent check | cards 072–081 |
| 17 | Crash recovery | card 089; `interchange_and_recovery.rs` recovery tests extend |
| 18 | Zoom × scaling grid | automated at 100% scaling (camera tests); 150/200% manual — marked |
| 19 | Release perf/memory, cancellable long ops | cards 086–087; ratios not wall-clock thresholds |

### Frozen fix order

First five fixes, foundation work first (frozen per plan): **E01** (cards 015–021) →
**E03/E05** (cards 034–039 with 007–013 foundation) → **E06** (card 046) → **E08/E09**
(cards 055, 057) → **E04** (card 038). Each fix flips its retained reproducer
(`thumbnail_reproducers.rs`) in the same card that implements it.

### Verification matrix disciplines

- A synthetic fixture validates mechanics only; portrait edge quality (real photos) and
  independent PSD compatibility get their own references (cards 064, 073).
- Manual-hardware gates named, never claimed: display-scaling 150/200% rows, OS clipboard
  against other applications (card 051's manual check), real IME (card 029), NVDA hardware
  pass (C14, separate doc, intentionally open).
- No merged-preview-only PSD claims (review rules); performance numbers are ratios on the
  real composition (card 086), never invented.
