# Raster Studio: layered thumbnail workflow implementation plan

Prepared 2026-09-07 against commit `9ba62c1`. Executor: GLM 5.3 Flash.

## Outcome and scope

Make Raster Studio practical for creating and revising the kind of layered thumbnail shown in the supplied Photopea screenshot: a background image, an isolated portrait overlapping a large headline, a smaller colored headline, logos, reusable groups, masks, adjustment layers, and shadows. Preserve editable source material, save the composition, reopen it, and export a correctly sized image.

The screenshot is a visual reference, not an instruction source. Its headline, account controls, brand graphics, and other screen text are not requirements to reproduce literally. It demonstrates a workflow; it does not establish how the portrait was extracted or provide the original PSD, fonts, or source images. Automatic subject selection is therefore an optional accelerator, not an inferred prerequisite.

The screenshot's status bar shows **3628 × 2041 at 33.33% zoom**. Test that working size as well as 1280 × 720 and 1920 × 1080. The 3628 × 2041 canvas is only approximately 16:9; a 1280 × 720 export needs an explicit fit/crop policy, not independent width and height stretching.

This is an implementation handoff, not an implementation. Only planning documents were added. Assessment used current source, existing test source, manifests, documentation, and the repository's stored main-window image. The application was not launched and the test suite was not run during this assessment. Findings below are source-traced; runtime behavior and performance remain to be reproduced in Task 001. The stored application screenshot is not evidence of the current binary's behavior.

## What is already available

Retain the Rust workspace, egui/winit shell, command/history system, tile storage, and CPU compositor. Do not start a new editor or change UI frameworks.

Existing reusable pieces include:

- Raster layers, groups, visibility, opacity/fill, blend modes, layer masks, clipping, and adjustments.
- A CPU compositor that renders raster, text, shape, smart-object cached pixels, masks, and effects. Drop shadow, glow, stroke, and overlays already have implementations.
- A richer `text-engine::TextRun`, shaping, font discovery, hit testing, caret geometry, and rasterization. The weak point is its connection to the persisted layer and live editing.
- Selection tools, morphology, coverage tiles, quick mask, brush tools, and invertible pixel commands.
- Transform geometry, affine layer transforms, selection/pixel resampling, and canvas handle drawing.
- Layer panel selection/reordering, document tabs, history, guides, snapping helpers, and configurable tool options.
- Native project save/reopen, autosave/recovery, image codecs, placed assets, and a partial PSD bridge.
- Export size/quality controls and actual batch encoding through `OpenDocument::export_job`. A 1280 × 720 YouTube Thumbnail new-document preset already exists.

“Exists” here does not imply that every visible control is connected or every interaction is correct. Verify the integration, then reuse the component.

## Missing or incomplete capabilities, with code evidence

Paths in the task cards below are relative to `C:\Users\VR\Projects\Raster-studio\raster-studio`. Line references describe the assessed commit and will drift after edits. Prefer the named symbols when locating code.

| ID | Gap and consequence for this workflow | Source evidence | Priority |
|---|---|---|---|
| E01 | **Text styling is lost at the document boundary.** Weight, slant, color, tracking, paragraph settings, style ranges, and frame geometry cannot survive conversion into the current three-field text layer. A Character control can change a temporary run and produce no document change. | `layer-model/src/layer.rs:437` (`TextLayer`); `text-engine/src/model.rs` (both `From` conversions); `ui/src/panels/text.rs` (`commit`); `compositor/src/text.rs` (`RunKey`, `run_image`). All crate paths start with `crates/`. | P0 |
| E02 | **Canvas text editing is rudimentary.** `TypeTool` creates a new layer and appends/deletes at the end; it does not enter existing text, select a range, or move the caret. Existing text geometry should supply those missing interactions. | `crates/tools/src/text.rs:1`, `TextSession`, `insert`, `backspace`, pointer handlers; `crates/text-engine/src/edit.rs`. | P0 |
| E03 | **Free Transform uses pixel resampling and canvas/selection bounds.** It does not provide the expected object-bounds, editable-type/shape/group transform workflow. It reads tile bytes, while text and shapes are rendered from their model. Repeated raster resampling also undermines smart-object quality. | `crates/tools/src/transform.rs:790` (`commit`) and `:906` (start bounds); `crates/compositor/src/composite.rs` (`source_bounds`, `render_source`, `level_transform`); `Layer::transform` already exists. | P0 |
| E04 | **Move options and picking need actual wiring.** `auto_select` defaults false; the shell forwards choice options, not the Move boolean. Raw-tile picking samples document coordinates without the layer transform, masks, text/shape rendering, or ancestor visibility. Multi-selection must reach gestures too. | `crates/tools/src/edit.rs:66` (`MoveTool`, `layer_under`); `crates/tools/src/registry.rs:231`; `crates/app-shell/src/chrome.rs:538` (`tool_choices`); `crates/app-shell/src/tool_input.rs:780`. | P0 |
| E05 | **Overlay helpers are not proof of live overlays.** `CanvasSessions` has transform, text, and snap fields, but the inspected production shell has no corresponding session-population route; uses assigning transform data are in UI tests. Live pixel previews also need an explicit integration path. | `crates/ui/src/canvas/workspace.rs:72` (`CanvasSessions`, `central_panel`); production `crates/app-shell/src/chrome.rs`, `shell.rs`, `tool_input.rs`. Reproduce the visible symptom in Task 001. | P0 |
| E06 | **Place clips the source before storing its renderable tiles.** A large source placed in a smaller document retains only its upper-left visible portion in the tile cache, despite retaining embedded file bytes. Dropped files always open documents. | `crates/app-shell/src/editor.rs:1411` (`place_path`, `min(cw)`, `min(ch)`); `crates/app-shell/src/shell.rs:1774` (`DroppedFile`). | P0 |
| E07 | **Bitmap clipboard interchange is missing.** Image copy/paste is an internal editor buffer; paste allocates canvas-sized pixels and clips to the canvas. It is not the OS image clipboard route needed for a screenshot or browser image. | `crates/app-shell/src/editor.rs` (`Clipboard`); `crates/app-shell/src/menu_bridge.rs:1889` (`paste`); `menu_context` derives availability from the internal buffer. `arboard` already exists transitively in `Cargo.lock`, without image dependencies in the inspected entry. | P1 |
| E08 | **Selecting mask properties does not select a paint target.** Both pointer and off-pointer paths force ordinary edits to `PaintTarget::Layer`; only quick mask redirects to a mask. Portrait edge cleanup needs direct mask painting. | `crates/app-shell/src/tool_input.rs:465` (`off_pointer`) and `:814` (pointer context); `crates/ui/src/panels/properties.rs` (`PropertyFocus`). | P0 |
| E09 | **Mask creation variants need end-to-end correction.** Reveal All, Hide All, Reveal Selection, and Hide Selection currently resolve to the same new `LayerMask` property patch; the inspected shell routes do not initialize their distinct coverage. A menu label alone does not make a portrait mask. | `crates/ui/src/menu.rs:2118` (`resolve_mask`); `crates/app-shell/src/menu_bridge.rs` mask routes. Task 057 must reproduce each operation through the real menu and assert coverage, not just mask presence. | P0 |
| E10 | **There is no complete portrait-refinement workflow.** Morphology and brushes exist, but a usable selection-to-mask, mask-paint, edge-preview, commit/cancel sequence is missing. Select Subject is explicitly deferred. | `crates/selection/src/modify.rs`, `crates/tools/src/select.rs`, mask paths above, `docs/parity-matrix.md` Select Subject row. | P0 manual; P2 automatic |
| E11 | **PSD import is lossy for this particular composition.** Type is imported as raster pixels; layer effects are reported but not mapped. Missing appearance cannot be recreated from a flattened screenshot. | `crates/app-shell/src/import.rs:900` (`layer_common` effects); `:1016` (`source.text`); `Tally`, `PsdNotes`; `crates/psd/src/text.rs`, `descriptor.rs`. | P1; P0 if migrating existing PSDs |
| E12 | **PSD export can have a correct merged preview but incomplete editable layers.** Text/shape/smart-object variants are reported as not having exported pixel records; most adjustments and effects are not serialized, and nontranslation transforms are not faithfully lowered. | `crates/app-shell/src/import.rs:1294` (`psd_layers_for`, `tally.no_pixels`, `expressible`); `OpenDocument::export_psd_to`. | P1; P0 if PSD round-trip is required |
| E13 | **Documentation and tests can overstate readiness.** For example, the parity table says smart objects do not render and guides are not saved, but current code renders cached smart-object tiles and persists guides. Conversely, rich text is marked complete despite E01. | `docs/parity-matrix.md`, `docs/architecture.md`; `crates/editor-core/src/document.rs:99` (`Guide`); compositor smart-object branches; E01. | P0 verification |
| E14 | **The full composition has not been validated here.** Existing performance tests use ten simple raster layers and cache ratios. That does not establish responsiveness for a large portrait, text, transformed sources, masks, and effect halos. | `tests/integration/tests/performance.rs`; `crates/app-shell/src/presenter.rs`; `crates/compositor/src/cache.rs`. | P1 |

The reference behavior is consistent with Photopea's documented [editable text workflow](https://www.photopea.com/learn/text), [content-bound Free Transform](https://www.photopea.com/learn/free-transform), and [mask target selection](https://www.photopea.com/learn/masks). These links explain the expected interaction; the repository evidence establishes what Raster Studio currently lacks.

## Implementation boundaries

1. **Native thumbnail editing is the first milestone.** Finish text, placement, transforms, masks, and composition before adding more filters or visual decoration.
2. **Preserve editability.** Native text/shape transforms alter model transforms. Placed sources retain original pixels outside the canvas. Rasterization is an explicit operation, never a hidden repair.
3. **One authoritative image pipeline.** Reuse the CPU compositor for committed rendering, preview rendering, thumbnails, and export. A temporary preview can use a document snapshot/overlay, but must not establish a second pixel implementation.
4. **One persistent text model.** Move the needed serializable text vocabulary into the leaf `layer-model` crate. `text-engine` consumes/re-exports it. Do not make `layer-model` depend on `text-engine` or encode JSON inside `TextLayer.text`.
5. **One interaction target contract.** Selected layers, active layer, content-vs-mask target, coordinate spaces, and pending gesture ownership must agree between panels, tools, history, and rendering.
6. **History contains committed edits.** Hover, caret blinking, viewport changes, and drag previews do not generate journal entries. A committed gesture creates one labeled entry. Cancel restores the pre-gesture result.
7. **Compatibility is deliberate.** The inspected document version is 3; manifest version is 2. Version new document semantics explicitly, add old-format fixtures, and preserve journal recovery. Do not bump the package manifest just because layer data changes unless its package contract changes too.
8. **Stay local.** No cloud service or image upload is needed. An optional subject model must run locally and be separately scoped. Do not rewrite the repository's local-first policy merely to add a convenience feature.
9. **Do not expand the task into full Photopea parity.** CMYK, RAW, video, licensing, an updater, collaborative editing, and OS print spooling do not enable the requested thumbnail workflow.

## How GLM 5.3 Flash should execute this plan

Use one task card per working session by default. A larger schema change may need mechanical caller updates in several crates to keep the workspace compiling; keep those mechanical changes separate from new behavior. There is no claim here about GLM's benchmark performance or context limits. The small tasks, concrete checks, and restart protocol are intended to reduce ambiguity for the named executor.

- Read this overview, the current card, its prerequisite handoff entries, and only the named source paths first.
- Trace the current code before editing. If a capability has since landed, demonstrate it with the required behavioral check and mark the card verified; do not implement a duplicate.
- Record baseline `git status` and preserve unrelated changes. Do not reset the checkout or replace existing plans.
- Write or extend a focused regression that exposes the missing behavior. For visual gestures, a command-only test is insufficient: include the actual shell/UI route at the integration step.
- Implement the smallest cohesive change. Prefer new focused modules over growing `editor.rs`, `menu_bridge.rs`, or `tool_input.rs` further.
- Run the card's checks, then inspect the diff. Do not weaken tests to accept a placeholder, fabricated preview, or silent no-op.
- Record exact commands and results, files changed, remaining limitation, and the next ready task in the progress log. A blocked check is not a pass.
- Never mark a phase complete because its menu entries exist or its documentation says complete.
- Stop at a task boundary if context becomes scarce; leave a precise handoff. Do not start a second unrelated phase in the remaining context.

Task status is initially **not started** for every card. On execution, keep one ledger in `docs/THUMBNAIL-WORKFLOW-PROGRESS.md` with columns: task ID, status, commit/diff, checks, visual evidence, remaining issue. This file is proposed for Task 002, not an already-existing file.

### Verification commands

Run commands from the Cargo workspace, not the outer Git root:

```powershell
Set-Location 'C:\Users\VR\Projects\Raster-studio\raster-studio'
cargo fmt --all --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

For a card, run `cargo test --locked -p <actual-package-name> <test-name>` or that affected package's tests. The integration package is **`integration-tests`**. New targeted integration files should use `cargo test --locked -p integration-tests --test thumbnail_workflow`. Run the full workspace gate at phase completion and after cross-crate schema changes, not after every tiny edit. Run existing CI audit checks at final release verification. Dependency additions must update the lockfile intentionally before returning to `--locked` commands.

If build dependencies or a GPU are unavailable, record the actual limitation. Do not silently install system tooling, change package versions, or treat a skipped GPU check as a verified visual interaction.

### Milestones and dependency order

| Milestone | Tasks | Exit condition |
|---|---|---|
| M0: reproducible baseline | 001–006 | Known failures and reusable fixture, no unsupported completion claims. |
| M1: interaction foundation | 007–014 | Consistent targets, bounds, options, and preview/commit semantics. |
| M2: reliable typography | 015–033 | Styled text saves, reopens, edits in place, and remains editable. |
| M3: object composition | 034–054 | Move/transform/place/paste operate on full content with visible feedback. |
| M4: thumbnail production | 055–071 | A portrait mask, text, images, effects, and groups form a usable native composition; existing export works. |
| M5: PSD interchange | 072–081 | Explicit fidelity contract and tested import/export of the supported composition. |
| M6: daily-use completion | 082–092 | Workspace polish, performance evidence, recovery, and full acceptance walk. |
| Optional automation | 093–098 | Local subject extraction and reusable templates, after the manual workflow works. |

The listed order is deliberately serial. Individual prerequisites are below. If PSD migration is the immediate use case, investigate Task 072 immediately after M0, but do not bypass text/mask foundations for its implementation. M4 is a useful native-editor delivery boundary; it is not a claim that the entire plan is complete.

## Phase 0 — Establish the real baseline

### Task 001 — Reproduce the gaps through the application

**Depends on:** none. **Targets:** existing shell/UI, test harness, `docs/THUMBNAIL-BASELINE.md` (new).

Build the current checkout and record toolchain, OS, commit, and existing checks. Walk through creating a thumbnail, changing text weight/color, editing a headline again, placing an oversized image, moving/scaling it, creating all four raster-mask variants, painting a mask, and importing/exporting a layered PSD fixture. Use existing suitable fixtures where available; a missing independent PSD fixture is recorded for Task 073, not a reason to stop all baseline work. Record observed behavior separately from source predictions E01–E14. Capture real app evidence where available. Do not fix code in this card.

**Done/check:** each operation has a reproducible sequence and observed outcome or a named reason it could not be exercised. No existing screenshot is presented as a fresh run.

### Task 002 — Create the progress and evidence ledger

**Depends on:** 001. **Targets:** `docs/THUMBNAIL-WORKFLOW-PROGRESS.md` (new).

Create entries 001–098 with statuses and prerequisites. Add a short “current task / next task / exact checks / remaining failure” handoff template. Link baseline evidence. Use `not started`, `in progress`, `verified`, `blocked`, and `optional deferred`; never collapse the last two into done.

**Done/check:** another session can identify the next ready card without rereading the conversation. Task 001 is verified only to the extent its baseline was actually recorded.

### Task 003 — Build deterministic composition fixtures

**Depends on:** 001. **Targets:** `tests/integration/src/fixture.rs`; `tests/project-fixtures/`; `tests/integration/tests/thumbnail_workflow.rs` (new).

Create small deterministic fixtures: background, asymmetric oversized image with labeled colored corners, RGBA logo, portrait-shaped cutout with soft edges, text, nested groups, mask, and an adjustment. Use generated test geometry and the existing licensed font fixture; do not depend on personal photos or installed fonts. Make a small CI version and full-size manual/performance version at 3628 × 2041.

**Done/check:** fixture assets have documented dimensions/alpha and deterministic hashes. Tests clearly label synthetic portrait coverage; it is not evidence of real hair extraction quality.

### Task 004 — Add shell-route regression harness helpers

**Depends on:** 003. **Targets:** `tests/integration/src/app.rs`; existing `crates/app-shell` headless tests; new thumbnail test module.

Extend existing helpers to invoke menu actions, set tool options, select a layer/target, route pointer/key events, inspect history, and composite the result. Keep UI click tests where actual widget connection matters. Avoid a parallel fake editor. Demonstrate failing regressions for E01, E06, E08, and E09, then retain their reproducer patches and outputs in the baseline report until the owning implementation card lands test and fix together. Do not leave future-feature failures or ignored regressions in the normal suite and then claim a green phase gate.

**Done/check:** each baseline regression fails for the intended behavior on the old code, and does not merely assert the presence of a command or menu label.

### Task 005 — Inventory reuse and dependency constraints

**Depends on:** 003. **Targets:** manifests; text-engine, compositor, selection, tools, UI helpers; baseline document.

Record the precise functions to reuse for text shaping/caret geometry, layer content bounds, transform resampling, masks, effects, export, fonts, and test fixtures. Inspect locked versions before proposing dependencies. `cosmic-text` is currently declared at 0.17; `arboard` 3.6.1 is already in the lockfile transitively. Establish no-new-dependency defaults for the core phases.

**Done/check:** every major subsystem has an existing owner or one proposed module. No dependency cycle or duplicate shaper/compositor is planned.

### Task 006 — Freeze the acceptance scene and matrix

**Depends on:** 003–005. **Targets:** baseline and progress documents.

Define the scene: background, two editable headlines, portrait with editable mask, at least two placed graphics, clipped adjustment, shadow, and grouped alternatives. Specify 1280 × 720, 1920 × 1080, and 3628 × 2041; zoom 33.33%, 100%, and 200%; Windows display scaling 100%, 150%, and 200%. Mark which combinations need manual hardware verification.

**Done/check:** the acceptance script at the end of this plan maps to actual fixture assets and test names. The first five fixes are E01, E03/E05, E06, E08/E09, and E04, with foundation work first.

## Phase 1 — Unify interaction contracts

### Task 007 — Define an explicit editor selection and target snapshot

**Depends on:** 006. **Targets:** `crates/app-shell/src/editor.rs`; `crates/ui/src/intent.rs`; proposed `crates/app-shell/src/edit_target.rs`.

Represent active layer, selected layer IDs, and content-vs-raster-mask target in one shell-owned state. The UI emits changes; tools receive a validated snapshot. Keep caret/range selection separate from layer selection. Reconcile missing/deleted IDs, tab switches, undo, and masks removed while selected. Reuse the existing layer-panel selection instead of creating competing lists.

**Done/check:** selecting a mask and then deleting it safely returns to content; switching documents restores that document's valid target. No pixel mutation occurs just by selecting.

### Task 008 — Add reusable bounds queries

**Depends on:** 007. **Targets:** `crates/compositor/src/composite.rs`, `text.rs`, `shape.rs`; proposed `bounds.rs`.

Expose bounded queries for source content bounds and transformed visible geometry of raster, smart-object, text, shape, and group layers. Reuse `source_bounds`/ink bounds. Distinguish content bounds from effect-expanded bounds and document clipping. Cache raster alpha bounds by tile hashes; do not scan a whole source every pointer frame. Define empty-layer behavior.

**Done/check:** asymmetric fixtures return correct local/document bounds, including negative positions and nested transforms; empty/hidden cases do not create NaN or unbounded allocations.

### Task 009 — Establish coordinate conversion helpers

**Depends on:** 008. **Targets:** proposed `crates/app-shell/src/interaction_geometry.rs`; existing canvas camera/viewport helpers.

Centralize screen points ↔ document pixels ↔ layer local pixels and mask space. Include parent transforms, zoom, pan, and DPI. Reject singular transforms. Document whether the existing layer tree stores transforms relative to parent or document and preserve its actual compositor convention. Never apply both a group transform and its child delta twice.

**Done/check:** point round-trips and pointer targeting work at three zooms and three scale factors with a moved/scaled nested layer. Include a linked mask and a document-space unlinked mask.

### Task 010 — Forward typed tool options

**Depends on:** 007. **Targets:** `crates/app-shell/src/chrome.rs`, `tool_input.rs`; `crates/tools/src/tool.rs`, `registry.rs`.

Replace the choice-only forwarding seam with typed tool settings for the relevant Move/Transform/Type/selection controls. Keep UI types out of tools; use a tools-owned settings value or explicit setters. Forward boolean, numeric, enum, and text values intentionally. Pin immutable gesture settings at start; define which appearance settings may update live.

**Done/check:** changing Auto-Select reaches the running Move tool; unknown keys fail visibly in development checks. Existing brush and transform choices remain functional.

### Task 011 — Add a pending-edit lifecycle

**Depends on:** 007, 009. **Targets:** proposed `crates/app-shell/src/edit_session.rs`; `editor.rs`; `editor-core/src/history.rs` only if required.

Introduce begin/update/commit/cancel state carrying document ID, target IDs, baseline values, and a generation. Provide one history transaction on commit; cancel drops the preview. Specify behavior for focus loss, tool/tab changes, save/export, and closing. Capture target IDs once so releasing a drag cannot edit a newly selected layer. Preserve autosave/recovery of committed state; pending edits must never be half-journaled.

**Done/check:** a long drag yields one undo entry; cancel yields none; tab switching cannot retarget the gesture. Failed commits leave document and history unchanged.

### Task 012 — Publish live tool geometry to canvas sessions

**Depends on:** 008–011. **Targets:** `crates/app-shell/src/tool_input.rs`, `chrome.rs`; `crates/ui/src/canvas/workspace.rs`.

Provide a production route from the live tool/session to `CanvasSessions`: transform quad/active handle, text caret/range geometry, and other-layer snap bounds. Clear the published state when the gesture ends or its document closes. Reuse existing overlay painters. Do not populate sessions only in a test helper.

**Done/check:** a shell-driven gesture produces visible handles in a captured real frame; cancel removes them. Tests distinguish geometry publication from manually filling the UI field.

### Task 013 — Render temporary content previews

**Depends on:** 011–012. **Targets:** `crates/app-shell/src/doc.rs`, `presenter.rs`; proposed preview module; compositor interfaces only as needed.

Render temporary transforms/text/mask parameters through the same compositor using an immutable document snapshot or explicit preview overrides. Use a preview generation in cache identity; invalidate old and new bounds including effects. Avoid copying full-resolution image buffers on every pointer sample. Keep the committed document/history untouched during preview.

**Done/check:** the object pixels move as the handles move; cancel restores exact baseline pixels; commit matches the preview at settled full quality. Another open document cannot reuse its preview cache.

### Task 014 — Verify the complete interaction foundation

**Depends on:** 007–013. **Targets:** shell integration and UI click tests.

Exercise typed options, layer/mask selection, coordinate conversion, geometry overlays, preview, commit, cancel, focus loss, and undo together. Include dragging across a panel and releasing outside the canvas. Keep the pointer capture behavior coherent with the existing router.

**Done/check:** actual input paths, pixel output, and history assertions pass. Run the workspace gate and record a real preview/commit/cancel demonstration before starting typography.

## Phase 2 — Make styled text persistent and renderable

### Task 015 — Define the persisted text schema

**Depends on:** 005, 014. **Targets:** proposed `crates/layer-model/src/text.rs`; `crates/text-engine/src/model.rs`, `style.rs`.

Specify one data representation for base style, style ranges, paragraph settings, point/box frame, and kerning. Preserve the existing `text`, `font_family`, and `size_px` compatibility fields or provide explicit legacy deserialization. Define defaults that preserve current black, regular text. Assign a single authority for family/size so aliases cannot disagree.

**Done/check:** a schema example includes white bold condensed headline text and green secondary text, and represents existing legacy layers without changing their pixels. This card designs types/tests; no dependency cycle is introduced.

### Task 016 — Implement text model conversion without data loss

**Depends on:** 015. **Targets:** layer-model text/layer exports; text-engine model/style exports; mechanical workspace callers.

Move or adapt the shared serializable vocabulary into layer-model; make TextRun conversion preserve every supported field. Update all struct literals located with `rg`, defaults, and imports in the same buildable change. Keep text-engine shaping-only responsibilities in text-engine.

**Done/check:** full styled runs round-trip through `TextLayer` with equal text, styles, frame, paragraph settings, and kerning. Package tests and workspace check pass, including old three-field input.

### Task 017 — Validate text payloads and editing ranges

**Depends on:** 016. **Targets:** layer-model text validation; editor-core command validation; text-engine editing tests.

Reject non-finite sizes/colors/tracking, impossible frame dimensions, out-of-bounds ranges, and ranges cutting a UTF-8 code point. Normalize valid overlapping style ranges deterministically. Preserve bounded allocation behavior for imported text. Treat grapheme navigation separately from UTF-8 validity.

**Done/check:** corrupt payloads cannot panic shaping or create an enormous unbounded raster; valid multilingual and styled text survives command apply/undo.

### Task 018 — Version and migrate native text documents

**Depends on:** 016–017. **Targets:** `crates/editor-core/src/document.rs`; `crates/project-format/src/migrate.rs`, `package.rs`, tests; `docs/file-format.md`.

Bump the document version for newly written rich-text semantics. Add deterministic migration/defaults for supported old documents and journals. Verify command serialization after SetLayerKind changes. Preserve the current manifest unless package structure changes. Ensure older readers cannot silently open a new document and discard rich style.

**Done/check:** old-format fixtures load unchanged; a new styled project reopens with identical model and composite. Future-version rejection remains explicit. Recovery can replay a styled text edit after a save marker.

### Task 019 — Render complete text styles

**Depends on:** 016–018. **Targets:** `crates/compositor/src/text.rs`; `crates/text-engine/src/layout.rs`, `raster.rs` if needed.

Pass the full persisted run to the existing shaper/rasterizer. Honor fill color/alpha, weight, slant, tracking, leading, alignment, style spans, and box width supported by text-engine. Preserve text transforms and layer effects; do not use a color-overlay effect as the only way to make white text.

**Done/check:** deterministic renders visibly distinguish regular/bold, black/white/green, different tracking, and multiline alignment; semitransparent edges composite correctly.

### Task 020 — Correct text cache identity and invalidation

**Depends on:** 019. **Targets:** `crates/compositor/src/text.rs` (`RunKey`); `composite.rs` (`hash_layer`); `cache.rs`; `app-shell/src/dirty.rs`.

Include every pixel-affecting text field and font generation in cache keys/hashes. Reuse a canonical text fingerprint, not two incomplete field lists. Keep floating-point hashing consistent with validation. Invalidate old and new ink/effect bounds after edits.

**Done/check:** render, change only color/weight/box width, and render again with caches warm; output changes correctly. Undo restores the original image and thumbnails refresh.

### Task 021 — Connect Character and Paragraph controls

**Depends on:** 019–020. **Targets:** `crates/ui/src/panels/text.rs`; `crates/ui/src/view/docks.rs`; shell kind-edit path.

Make existing controls read/write the persistent data. Add a direct text fill color control if the current surface lacks it. During text-range editing apply style to the selected range; otherwise apply to the whole layer. Coalesce a slider gesture using the established session/history contract.

**Done/check:** actual panel changes affect pixels, undo correctly, and survive reopen. Controls that cannot yet affect the model are disabled with an exact reason until their task lands.

### Task 022 — Implement useful font selection and substitution reporting

**Depends on:** 021. **Targets:** `crates/ui/src/view/docks.rs`; `crates/text-engine/src/font.rs`; compositor font facade; shell font loading seam.

Use the existing font list for searchable family and available face selection, including condensed faces when installed. Report a missing requested font and the chosen substitute; retain the requested name in the document. Add a user-chosen font-file load route only if needed, reusing `load_font` and cache invalidation. Do not silently download or redistribute fonts.

**Done/check:** changing family updates the headline; a missing family is clearly reported; deterministic tests use the existing licensed font fixture. Font loading does not replace UI fonts unexpectedly.

### Task 023 — Connect point and paragraph text geometry

**Depends on:** 019–022. **Targets:** text model/engine; UI text properties; tools Type defaults.

Expose point text vs wrapping-box text and persist box width/height. Preserve explicit line breaks. Distinguish changing font size, changing the paragraph box, and transforming the whole layer. Show overset text status where the engine reports it.

**Done/check:** resizing a paragraph frame changes wrapping while preserving font size; point text remains unwrapped except at explicit breaks. Save/reopen retains the result.

### Task 024 — Verify styled headline authoring end to end

**Depends on:** 015–023. **Targets:** thumbnail workflow test; UI click tests; fixtures.

Create a large white bold headline and smaller green text over a dark background through the shell/panels. Change only weight/color/tracking, save/reopen, export, and undo/redo. Check model preservation and a deterministic rendered result independently.

**Done/check:** no temporary-run conversion drops styling; both headlines retain editable text. Full workspace gate passes. Record any font-dependent manual comparison separately from deterministic assertions.

## Phase 3 — Make text editable on the canvas

### Task 025 — Replace append-only text-session state

**Depends on:** 024, 011. **Targets:** `crates/tools/src/text.rs`; proposed shell text-session module.

Store original rich payload, draft payload, target document/layer, caret, selection anchor, and composition state. Route draft rendering through pending-edit previews. Decide new-layer creation/cancel semantics explicitly: canceling new empty text must not leave a stray layer. Reuse existing text editing geometry.

**Done/check:** session state can represent a caret in the middle and a selected range; cancel restores the exact original layer and styles without history debris.

### Task 026 — Enter an existing text layer

**Depends on:** 025, 008–009. **Targets:** Type pointer routing; layer-panel double-click intent; shell text session.

Click with Type or double-click a text thumbnail to edit the existing layer. Convert the click through its transform and use shaped hit testing to place the caret. Clicking empty canvas creates point text. Respect locks and nested groups.

**Done/check:** clicking the middle of a transformed headline edits that same LayerId, not a new layer. Overlapping portrait/text hit policy is defined and testable.

### Task 027 — Implement caret movement and range selection

**Depends on:** 026. **Targets:** text session; `crates/text-engine/src/edit.rs`; `crates/ui/src/canvas/text_overlay.rs`.

Support arrows, Home/End, word movement, Shift selection, drag selection, and select-all inside the text session. Navigate grapheme boundaries and use visual caret geometry for bidi text; do not confuse bytes, code points, and glyph clusters. Reuse or extend the existing hit-test/caret methods.

**Done/check:** accented characters, emoji sequences, ligatures, and a short bidi fixture do not split UTF-8 or leave invalid selections. Caret position agrees with rendering after transform and zoom.

### Task 028 — Implement insertion, deletion, and text clipboard routing

**Depends on:** 027. **Targets:** shell key/text routing; Type session operations.

Insert at the caret, replace a selected range, support forward/backward and word deletion, and copy/cut/paste text through the existing OS text input integration. Preserve and shift style ranges with edits. While text editing is active, Ctrl+A/C/X/V and tool-letter keys belong to text.

**Done/check:** replacing a selected word preserves the surrounding styles; pasted multiline text stays in the layer. An image clipboard payload does not unexpectedly create a layer while typing.

### Task 029 — Handle IME composition and focus safely

**Depends on:** 028. **Targets:** `crates/app-shell/src/shell.rs`; text input/session module.

Handle winit/egui committed text and IME preedit without double insertion. Show temporary composition state and position the IME caret window from the same geometry. Define focus-loss behavior explicitly and ensure keyboard shortcuts do not interrupt an active composition.

**Done/check:** synthetic preedit/commit tests insert once; perform a manual IME check where available. Mark device/OS input verification pending if unavailable, not complete based only on synthetic events.

### Task 030 — Render accurate caret and selection overlays

**Depends on:** 027–029, 012. **Targets:** canvas text overlay painter; text-session geometry publisher.

Publish caret and range rectangles from the exact shaped run used for pixels. Apply the full layer transform, viewport, DPI, and clipping. Schedule caret blinking without causing full-document recomposition. Include empty lines and paragraph boxes.

**Done/check:** caret and highlighted ranges align at 33.33%, 100%, and 200% zoom; the old canvas-origin approximation is gone. Idle caret ticks do not change document dirty state.

### Task 031 — Wire text confirm, cancel, and history

**Depends on:** 025–030. **Targets:** Type options UI; shell pending-edit lifecycle; history integration.

Provide visible confirm/cancel controls. Enter inserts a line break; Ctrl+Enter confirms; Escape cancels. Define tool-switch/tab-close/save/export behavior consistently with Task 011. Commit one cohesive text-edit transaction while retaining ordinary editing undo within the active draft if supported.

**Done/check:** typing a sentence does not require dozens of document undos; Escape restores the previous headline; save/export cannot silently omit or partially commit a visible draft.

### Task 032 — Create and resize paragraph text with gestures

**Depends on:** 023, 031. **Targets:** Type tool; canvas frame handles; text session.

Drag with Type to create a paragraph box. Resize that box while editing to reflow text without changing the layer's affine scale. Keep whole-layer transform handles distinct from text-frame handles, with unambiguous cursor/controls.

**Done/check:** click creates point text, drag creates paragraph text, and box resizing is one undoable edit. The text remains selectable and editable after a frame change.

### Task 033 — Verify repeated headline editing

**Depends on:** 025–032. **Targets:** shell/UI tests and thumbnail fixture.

Enter existing text, select a word, paste a replacement, style a span, add a line, resize the frame, confirm, transform, reopen editing, cancel, save, and reload. Exercise font substitution and a locked text layer.

**Done/check:** content, styles, caret placement, rendering, and history all match the expected sequence. Capture an actual editor frame with a caret/range over the headline; run the phase gate.

## Phase 4 — Make Move and Free Transform object-aware

### Task 034 — Start transforms from selected content bounds

**Depends on:** 008–014, 033. **Targets:** `crates/tools/src/transform.rs`; shell transform session; bounds queries.

Start the transform when the action is invoked, with handles around selected content rather than waiting for a canvas click and using canvas bounds. Distinguish whole-layer transform, explicitly selected raster pixels, selection-only geometry, and mask-target transform. Record target IDs, baseline transforms, bounds, and pivot once.

**Done/check:** a small logo's initial box surrounds the logo; a text layer's box surrounds the text; an explicit pixel-selection transform still uses its selection. No selection means whole-layer transform, not whole-canvas pixel rewriting.

### Task 035 — Commit affine transforms without resampling sources

**Depends on:** 034. **Targets:** shell transform controller; `editor-core::Command::TransformLayer` / absolute transform patch; compositor integration tests.

For whole-layer move, scale, rotate, flip, and affine skew, update the existing layer transform. Preserve Text/Shape/SmartObject kinds and original raster tile hashes. Compute deltas against the recorded baseline using the compositor's parent convention; prefer explicit absolute before/after values when that avoids repeated composition errors.

**Done/check:** a 50% then 200% scale restores raster content quality without rewriting its source tiles; text is still editable; a moved parent and child are not transformed twice. Undo restores exact transforms.

### Task 036 — Implement multi-layer transform and selection synchronization

**Depends on:** 007, 035. **Targets:** layer panel selection intents; shell transform/Move controller; command transactions.

Use the selected set, normalize ancestor/descendant duplicates, and transform multiple objects around a shared pivot. Respect position/all locks; choose and document an all-or-nothing refusal for mixed locked selections. Update active layer on canvas selection without losing the rest of a Shift-selected set.

**Done/check:** headline, logo, and portrait move together in one undo entry; selecting both a group and its child moves the child once; a locked participant prevents partial commits.

### Task 037 — Implement rendered-content hit testing

**Depends on:** 008–009, 036. **Targets:** proposed shell hit-test module; compositor bounds/coverage query; Move/Type routes.

Replace raw document-coordinate tile sampling with a bounded visible-content test. Consider inverse transforms, ancestor visibility, locks policy, opacity, alpha, and masks; include text, shapes, and smart objects. Exclude effect-only shadow pixels by default, and exclude adjustment layers from ordinary object picking. Make group-vs-layer selection explicit. Cache bounds and sample only candidates under the cursor.

**Done/check:** a transparent logo hole selects the object beneath, a hidden group cannot be picked, a moved portrait is picked at its displayed location, and a text glyph can be picked without raster tiles.

### Task 038 — Wire Move options and on-canvas selection

**Depends on:** 010, 037. **Targets:** tools Move settings; UI Move options; shell selection routing.

Connect Auto-Select, Layer/Group selection mode, and Show Transform Controls to real behavior. With auto-select off, drag the selected set; with it on, click/drag the visible hit. Define empty-canvas clicks and modifier selection consistently. Ensure enabling transform controls displays a box without starting an edit.

**Done/check:** toggling each option changes the real shell behavior. A canvas click updates the highlighted layer row, and a no-motion click creates no transform history entry.

### Task 039 — Show a live Move and transform preview

**Depends on:** 013, 035–038. **Targets:** transform/Move sessions; presenter; canvas overlays.

Publish geometry and content preview together for move/resize/rotate. Draw a stable pivot and use screen-point handle hit regions. Show the committed result while idle and remove all stale previews after commit/cancel/tab switch.

**Done/check:** a dragged portrait visibly follows the pointer before release and stays aligned with the handles. Previewing at 33.33% does not alter saved source pixels or leave a ghost at the original location.

### Task 040 — Make edit tools respect transformed content coordinates

**Depends on:** 009, 035, 039. **Targets:** `crates/app-shell/src/tool_input.rs`; `DocumentTiles` adapter or focused target adapter; pixel-tool tests.

Route supported painting/erasing and sample operations into the selected layer's local pixel space instead of writing document coordinates into transformed source tiles. Define brush radius behavior under nonuniform scale and apply document selections in the correct space. For pixel operations on text/shape/smart objects, provide explicit rasterize or edit-source behavior rather than storing invisible tiles on the wrong layer kind.

**Done/check:** painting on a moved/scaled raster layer marks the displayed pointer location; a selection constrains it correctly. Text painting cannot silently create unused pixel data. Undo restores the original source hashes.

### Task 041 — Add predictable scale/rotate modifiers and numeric fields

**Depends on:** 039. **Targets:** transform geometry; UI tool options; shell transform controller.

Add X/Y, W/H or scale percentage, rotation, pivot, and aspect lock. Default corner scaling to preserve aspect; Shift temporarily changes that constraint, Alt scales around center, and Escape/Enter cancel/confirm. Ensure numeric edits and dragging share the same draft model. Reject non-finite/zero sizes without committing invalid matrices.

**Done/check:** numeric 50% scaling matches the drag result; rotation around a moved pivot is correct; the modifier convention is shown in tool help and tested. Repeated field changes form one committed transform.

### Task 042 — Connect snapping and alignment to real layer bounds

**Depends on:** 008, 036, 041. **Targets:** `crates/ui/src/canvas/snapping.rs`; canvas session publisher; Move/transform controller; alignment actions.

Reuse existing guides/grid/canvas candidates and feed actual other-layer edges/centers. Snap the moving geometry, not merely a decorative cursor point. Use screen-point thresholds and exclude selected descendants from candidates. Add align/distribute commands only where current routes are missing; reuse `MoveTool::align` math where valid.

**Done/check:** the logo centers on the canvas at multiple zooms; two headlines align left; snap indicators describe the adjustment actually applied. One align/distribute action is one undoable transaction.

### Task 043 — Respect linked layers and mask movement

**Depends on:** 036, 040–042. **Targets:** layer linkage resolution; mask transform handling; editor-core validation as needed.

Consume the existing layer link IDs when moving linked content, with deduplication and cycle-safe traversal. Preserve linked-mask behavior and define unlinked-mask movement in document space. If independent mask transforms need new persisted state, add it with defaults and version/migration tests before exposing controls. Do not treat a link icon as working behavior by itself.

**Done/check:** linked portrait and decoration move once together; an unlinked mask stays put while content moves; relinking does not jump the visible mask. Save/reopen and undo preserve the relationship.

### Task 044 — Keep pixel-selection and non-affine transforms explicit

**Depends on:** 035, 040, 043. **Targets:** `crates/tools/src/transform.rs`; shell transform mode resolver; UI mode availability.

Retain the existing selection-only and pixel-patch resampler for deliberate pixel edits. Correct source/destination spaces and avoid clipping stored content merely because it is outside the canvas. Whole-layer perspective/distort/warp on parametric text/shape/smart objects needs either a separately modeled non-affine transform or an explicit rasterization choice; for this milestone, clearly disable those unsupported combinations and keep affine transforms editable.

**Done/check:** raster selection transforms still work; an unsupported text warp cannot silently rasterize or corrupt the layer. Do not claim full non-affine editable-object parity when only the raster path is implemented.

### Task 045 — Verify the complete transform workflow

**Depends on:** 034–044. **Targets:** thumbnail integration tests; shell input tests; real application capture.

Place existing fixture layers around a headline, resize a portrait, rotate a logo, move a group, align text, transform a mask, cancel a drag, and undo/redo. Include objects crossing the canvas boundary and images with transparent margins. Test both untransformed and previously transformed layers.

**Done/check:** content bounds, picking, visible previews, committed transforms, editability, and source preservation all agree. Run full phase checks and capture an actual transform in progress.

## Phase 5 — Place full-resolution assets and exchange clipboard images

### Task 046 — Add a reusable full-source placement builder

**Depends on:** 018, 035, 045. **Targets:** proposed `crates/app-shell/src/placement.rs`; `import.rs`; asset/document model if required.

Build a placement result containing validated source dimensions, full tile data, asset metadata, and a new layer. Preserve all decoded source pixels, including those outside the canvas. Reuse bounded decode and content-addressed tiles. Explicitly retain source origin/dimensions so transparent padding does not make its intended size unknowable.

**Done/check:** a large asymmetric source placed on a tiny canvas retains its far-right and bottom content in stored tiles. Placement refuses malformed dimensions before allocation and does not allocate a canvas-sized copy unnecessarily.

### Task 047 — Define fit/center placement and color conversion

**Depends on:** 046. **Targets:** placement builder; color conversion helpers; import tests.

Default placement to aspect-preserving fit within the canvas, centered; do not enlarge smaller assets unless selected by the user. Represent fitting as a layer transform. Convert decoded pixels into the document's working color space using existing profile handling while retaining original embedded bytes and source-profile metadata. Define the fallback/error for an unsupported profile instead of silently treating it as sRGB.

**Done/check:** the four labeled source corners are visible after fit; scaling back up exposes the original detail. Tagged placement into a different working space matches the agreed reference conversion.

### Task 048 — Make placement atomic and active-layer aware

**Depends on:** 046–047. **Targets:** `editor.rs` (`place_path`); placement commands; document asset registration/history.

Replace the current clipping path with the builder. Insert above the active layer or into the intended group, select the placed layer, and enter a transform preview. Treat asset registration and content placement as a coherent operation: undo cannot leave live references to a removed asset, and cancel does not leave stray layers. Retain unreachable immutable blobs only under a defined storage policy.

**Done/check:** Place → confirm → undo removes the whole placed object; redo restores it. Cancel or failed decode changes neither layer stack nor dirty/history state. Native save/reopen works without the original file for embedded placement.

### Task 049 — Make drag-and-drop context-sensitive

**Depends on:** 048. **Targets:** `crates/app-shell/src/shell.rs` dropped-file handling; placement service; dialog/menu routes.

Dropping a supported image on an open canvas places it. Dropping a native project/PSD or dropping when no document exists opens it. Provide an explicit Open action for users who want a separate image document. For multiple dropped images, preserve order and define one batch transaction or clearly documented per-file steps; report failures without losing successful sources or blocking the UI indefinitely.

**Done/check:** dropping a portrait into a composition adds a layer instead of a second image tab. Dropping a project still opens it. Tests exercise the actual window-event routing seam.

### Task 050 — Preserve source quality during smart-object editing/refresh

**Depends on:** 048–049. **Targets:** `editor.rs` smart-object contents and `refresh_linked_sources`; placed asset cache rebuild.

Reuse the full-source builder when a linked file changes or embedded contents are committed. Preserve the placed layer's transform, mask, effects, identity, and selection. Handle changed source dimensions deliberately. Report a missing linked source and keep its last good cached appearance; do not replace it with empty pixels.

**Done/check:** replacing a linked portrait with a different resolution keeps composition placement predictable; undo restores the previous appearance. Embedded content editing does not reduce the source to canvas dimensions.

### Task 051 — Add an OS bitmap clipboard adapter

**Depends on:** 005, 028, 046. **Targets:** proposed `crates/app-shell/src/clipboard.rs`; app-shell manifest; shell lifecycle.

Add an injectable image clipboard service with a real OS implementation and deterministic fake. Reuse a compatible `arboard` dependency with image support intentionally enabled; verify its locked API/features rather than adding an unrelated clipboard stack. Normalize returned image bytes to straight RGBA with bounded dimensions. Handle clipboard busy/unsupported formats as ordinary recoverable errors and release resources at shutdown.

**Done/check:** the fake round-trips RGB and transparent RGBA; a Windows screenshot can be read in a manual test. The dependency choice is grounded in the [arboard clipboard API](https://docs.rs/arboard/3.6.1/arboard/struct.Clipboard.html), which exposes image get/set and documents platform-specific ownership/error behavior.

### Task 052 — Connect image Copy/Paste and freshness rules

**Depends on:** 048, 051. **Targets:** `menu_bridge.rs` copy/paste; menu context; clipboard service; shell keyboard routing.

Copy selection/Copy Merged to the OS image clipboard while retaining useful internal metadata. When pasting outside a text session, prefer the current external image payload over stale internal content; use an ownership/generation policy rather than always choosing an old editor buffer. Route paste through full-source placement with explicit center or paste-in-place semantics. Do not fetch image URLs from text automatically.

**Done/check:** browser/screenshot → editor and editor → another app work; external clipboard replacement is honored. Internal Copy → Paste still works when the OS clipboard is temporarily unavailable. Text fields retain text-paste behavior.

### Task 053 — Implement Paste Into using a real mask

**Depends on:** 009, 052. **Targets:** clipboard/placement service; proposed shared mask initialization helper.

Replace destructive alpha multiplication in Paste Into with a retained full image plus a selection-derived raster mask. Preserve offset/paste-in-place metadata and transform alignment. Keep original pixels recoverable by disabling the mask. Introduce a focused selection-to-coverage helper now; Task 057 will reuse and extend it for the four mask-creation actions. Respect the compositor's missing-tile coverage convention.

**Done/check:** Paste Into shows only the selected area; disabling its mask reveals the full original image. Undo removes both the new content layer and its mask in one step. Fractional selection coverage maps correctly through the pasted layer's transform.

### Task 054 — Verify asset reuse and clipboard workflows

**Depends on:** 046–053. **Targets:** thumbnail tests; OS manual test script; project fixtures.

Compose from a large background, portrait, and logos using Place, drop, and paste. Save, close, move/rename the original embedded source files, and reopen the project. Exercise cancel/error paths and linked-file disappearance. Keep clipboard-dependent manual checks separate from fake-service tests.

**Done/check:** full sources and transforms survive native persistence; oversized images are never permanently clipped by placement. Paste Into retains source pixels behind an editable mask. Run the full Phase 5 gate.

## Phase 6 — Build a reliable portrait cutout and mask workflow

### Task 055 — Connect mask thumbnail selection to the edit target

**Depends on:** 007, 014, 040, 054. **Targets:** Layers thumbnail UI; Properties focus; shell edit-target state; `tool_input.rs` context creation.

Clicking a raster-mask thumbnail selects that mask for editing; clicking content selects content. Show a clear border/target label. Route both pointer and off-pointer operations through the same target resolver and pin the target for each gesture. Keep quick mask a separate temporary mode that restores the previous target on exit.

**Done/check:** changing mask properties and painting a mask are no longer confused. Brush on the selected mask changes coverage tiles only; switching back paints content. Remove-mask/undo/tab-switch cases remain valid.

### Task 056 — Make selection edits undoable and composable

**Depends on:** 055. **Targets:** selection tool request handling in `tool_input.rs`; `Command::SetSelection`; `selection` algorithms.

Route marquee/lasso/wand/quick-selection results through history rather than directly assigning `document.selection`. Preserve replace/add/subtract/intersect modifiers and antialias coverage. Coalesce one selection gesture into one command. Pin source sampling and modifier state during the gesture.

**Done/check:** build a rough portrait selection with add/subtract, undo one stroke, and redo it without changing image pixels. Save/recovery preserves a committed selection and never replays a half-gesture.

### Task 057 — Implement distinct mask initialization operations

**Depends on:** 055–056. **Targets:** proposed shell mask service; `ui/src/menu.rs` mask resolution; shell menu bridge; coverage tile commands.

Implement Reveal All, Hide All, Reveal Selection, and Hide Selection as actual coverage data with the correct default outside stored tiles. Extend the selection-to-coverage helper from Task 053. Use the existing mask sampling convention; do not assume missing tiles mean white. Convert document-space selections into linked-mask local space as needed. Attach mask plus coverage atomically, with a clear rule for an already masked layer.

**Done/check:** a known two-color image produces four distinct expected results through real menu actions. Fractional selection coverage is retained, and undo restores the prior mask/data. Existing Paste Into tests continue to pass through the shared helper.

### Task 058 — Paint, erase, invert, and fill masks correctly

**Depends on:** 040, 055, 057. **Targets:** target adapter; `CoveragePatch`; brush/eraser routes; mask menu operations.

Map pointer positions/brush geometry into mask space. Use grayscale coverage and defined foreground/background shortcuts; constrain strokes by the active selection. Make invert/fill target the mask when appropriate. Prevent a four-channel ColorPatch from being written into a one-channel coverage slot. Preserve black/white mask editing colors without losing the user's prior content colors.

**Done/check:** black hides and white restores a moved/scaled portrait under the pointer. Erase and undo affect only the mask; source image tile hashes remain unchanged. Linked/unlinked masks behave as documented.

### Task 059 — Add mask visualization and target controls

**Depends on:** 058. **Targets:** canvas presentation overlay; Layers mask thumbnail/context menu; UI theme tokens.

Provide mask-only grayscale, tinted overlay, enable/disable, and return-to-composite controls. Reuse current enable/link/density/feather state. Show a real mask thumbnail and an obvious active-target indicator. Use shortcuts only after checking existing bindings; persist relevant preferences rather than hardcoding them in the painter.

**Done/check:** grayscale/overlay/composite views show the same mask geometry; disabling a mask reveals source content and never discards coverage. Mask-only viewing is not exported accidentally.

### Task 060 — Add a nondestructive edge-refinement dialog

**Depends on:** 057–059. **Targets:** proposed `crates/ui/src/dialogs/refine_mask.rs`; shell dialog host/mask session; `crates/selection/src/modify.rs`.

Expose feather, expand/contract (shift edge), smooth, and edge contrast using existing selection morphology where applicable. Preview changes against black, white, and transparent/checker backgrounds. Keep radius/units in document pixels and convert to target space consistently. Output an editable mask; cancel restores baseline coverage. Label this accurately as edge refinement, not automatic subject recognition.

**Done/check:** controlled synthetic edges show the expected width/softness changes without modifying source RGB. One confirmation is one undo entry; canceled previews do not leak into saved data.

### Task 061 — Add a targeted boundary-refine brush

**Depends on:** 060. **Targets:** mask refinement service; selection helpers; brush option UI.

Allow a user-marked narrow boundary band to receive local refinement while leaving definite foreground/background unchanged. Start with deterministic edge-aware behavior only if it improves measured fixtures; otherwise ship manual paint plus morphology and explicitly defer automatic matting. Do not relabel ordinary blur as hair extraction. Keep algorithm and preview work bounded to the painted region.

**Done/check:** fine-edge fixtures retain hard interior coverage and alter only the requested band. Real hair/skin-edge quality requires a visual comparison on suitable user-owned or licensed photos; synthetic tests alone do not pass that quality gate.

### Task 062 — Implement optional edge color cleanup as a separate edit

**Depends on:** 061. **Targets:** focused raster edge-cleanup operation; mask/refinement UI.

Add a clearly labeled color-fringe cleanup operation only where needed after masking, with radius/strength and preview. Preserve original image data by writing to a duplicate working layer or explicit reversible operation; coverage refinement must not silently recolor the portrait. Keep this separate from simple mask feathering.

**Done/check:** a known colored-background fringe is reduced when enabled; disabling/canceling restores original RGB. Existing foreground colors away from the boundary are unchanged. If quality is not adequate, record this optional enhancement within the card as deferred without blocking manual masking.

### Task 063 — Verify mask/source transform alignment

**Depends on:** 043, 057–062. **Targets:** transform/mask integration tests; compositor test fixtures.

Create a cutout mask, move/scale/rotate the content, unlink and move the mask, relink, refine edges, and edit again. Cover mask density/feather with effects and clip adjustments. Fix any local/document mapping discrepancies found; prefer shared helpers over operation-specific offsets.

**Done/check:** the mask remains under the intended portrait edge at all tested zooms. The same result survives save/reopen; undo/redo restores pixel hashes, mask parameters, and transforms together.

### Task 064 — Complete the manual portrait extraction acceptance walk

**Depends on:** 053, 055–063. **Targets:** manual acceptance script; thumbnail integration tests; evidence files.

Use a real ordinary-background portrait when available: rough selection, add/subtract, selection-to-mask, paint corrections, edge refinement, black/white background inspection, and placement over the thumbnail. If no suitable source photo is available, finish all deterministic checks and record the outstanding visual quality gate explicitly.

**Done/check:** manual cutout production is practical, undoable, and source-preserving. Do not add an ML runtime to bypass broken mask editing. Run the full phase gate, including Paste Into and mask-target painting together.

## Phase 7 — Finish the layered composition workflow

### Task 065 — Make layer-stack management practical

**Depends on:** 045, 064. **Targets:** `crates/ui/src/panels/layers.rs`; `view/docks.rs`; layer action routes.

Verify and finish rename, duplicate, delete, drag reorder, group/ungroup, collapse, visibility, and multi-select through actual controls. Keep effects/mask thumbnails readable, scroll the active row into view, and make click targets usable at Windows scaling. Preserve the existing tree and selection code; fix specific missing interactions rather than replacing the panel.

**Done/check:** organize 30–50 layers with nested groups, rename the portrait/headlines, duplicate an alternative, and move it into a group. A drop cannot form a cycle or duplicate a child. Undo restores order and content.

### Task 066 — Verify and repair thumbnail effects integration

**Depends on:** 020, 063, 065. **Targets:** `crates/ui/src/dialogs/layer_style.rs`; compositor effects; dirty bounds/thumbnails.

Exercise existing drop shadow, outside stroke, outer glow, and color overlay on text, portrait masks, and logos. Repair routing, parameter application, previews, or effect-halo invalidation where reproduced. Confirm effect ordering around masks and fills. Preserve the existing effect algorithms unless a targeted regression demonstrates a defect.

**Done/check:** changing shadow distance/stroke width visibly updates the canvas and thumbnail, saves/reopens, and undoes correctly. Shadows crossing tile boundaries do not show seams or stale remnants after movement.

### Task 067 — Make layer styles reusable

**Depends on:** 066. **Targets:** layer context menu; style dialog; `crates/asset-store/src/presets.rs`; shell preset wiring.

Verify existing style preset storage and add missing Copy/Paste Layer Style and named preset controls. Copy only style fields, not position, masks, text, or asset identity. Provide a complete replacement operation first; add selective merging only if explicitly represented in the UI.

**Done/check:** apply the same outline/shadow to two headline layers without changing their text or transforms. Presets survive restart and apply in one undoable step. If current controls already satisfy this, record evidence and avoid duplicate storage.

### Task 068 — Verify clipped adjustments and editable mask targeting

**Depends on:** 063, 065–066. **Targets:** adjustment Properties UI; clipping commands; compositor integration tests.

Add/edit Curves, Hue/Saturation, and Invert in the composition using existing adjustment types. Clip an adjustment to the portrait or place it within an appropriate group; edit its mask separately. Confirm intended scope across visible/hidden layers, group blending, and opacity.

**Done/check:** a portrait-only tonal adjustment leaves the headline/background unchanged; toggling it restores prior pixels; saved parameters and curves survive reload. Repair only missing routes or reproduced math/scope errors.

### Task 069 — Support repeated asset replacement without redoing layout

**Depends on:** 050, 065–068. **Targets:** smart-object actions; asset replacement service; UI context menu.

Add or verify Replace Contents on a placed image. Keep layer identity, transform, mask, effects, and group position. Define replacement sizing as fit to the existing source frame with aspect preserved, offering explicit actual-size behavior if needed. Do not infer identity from layer names.

**Done/check:** replace the portrait/logo while retaining the layout; undo restores old content and metadata. Replacing one shared asset has an explicit per-instance/shared-assets policy and does not unexpectedly change unrelated layers.

### Task 070 — Add a native reusable composition workflow

**Depends on:** 024, 065–069. **Targets:** document duplication/save-as routes; native fixture/template; documentation.

Verify Duplicate Document and Save As for making thumbnail variants. Provide a native sample composition with clearly named editable placeholder layers and licensed/generated assets. Open it as a new unsaved document rather than overwriting the source template. Use text content changes and Replace Contents for variants.

**Done/check:** creating two variants leaves the template unchanged; each remains independently editable and can be exported with different names. No separate template engine is necessary for this milestone.

### Task 071 — Deliver the native thumbnail editing milestone

**Depends on:** 065–070. **Targets:** thumbnail acceptance tests; native example; user workflow documentation.

Build the acceptance scene from scratch through the application: place sources, edit headlines, mask the portrait, transform objects, add effects/adjustments, organize groups, save/reopen, and export using the existing export dialog. Repeat with one content replacement. Avoid bypassing missing UI by constructing the final document entirely in code.

**Done/check:** the native workflow is usable and every required operation is reachable. Record the original `.rstudio`, exported image, screenshots, exact checks, and outstanding PSD limitations. This is M4, not the end of the full plan.

## Phase 8 — Support the existing PSD workflow honestly

### Task 072 — Define supported PSD fidelity, including failure policy

**Depends on:** 006; implement after 071 by default. **Targets:** `crates/app-shell/src/import.rs`; `crates/psd/src/model.rs`, `text.rs`, `descriptor.rs`; proposed `docs/PSD-THUMBNAIL-SUPPORT.md`.

Inventory what the current parser/writer can actually carry: editable text, effects, adjustment descriptors, masks, groups, ICC metadata, smart-object cached pixels, and transforms. Define three outcomes per feature: editable preservation, appearance-preserving raster fallback, or explicit unsupported/refusal. Keep original PSD bytes untouched on import. Do not promise exact compatibility with an unseen user PSD.

**Done/check:** a field-by-field matrix names the subset needed by the screenshot and each fallback. A correct merged preview is explicitly insufficient proof of an editable layer round-trip.

### Task 073 — Add independent PSD fixtures and expected results

**Depends on:** 072. **Targets:** `crates/psd/src/tests.rs`; interchange integration tests; project fixtures.

Create or obtain licensed PSD fixtures from an independent writer for styled type, translated/rotated content, drop shadow, masked portrait, Curves/Hue-Saturation/Invert, and nested groups. Record provenance and expected layer metadata plus a reference merged image. Include one intentionally unsupported descriptor and malformed/bounded inputs. Keep self-generated round-trip fixtures too, but do not use them as the sole interoperability evidence.

**Done/check:** fixtures expose E11/E12. If an independent writer is unavailable, that interoperability gate remains pending even if the internal parser/writer agrees with itself.

### Task 074 — Import the supported editable text subset

**Depends on:** 018–024, 072–073. **Targets:** PSD text/descriptor parser; `app-shell/src/import.rs` conversion.

Map supported text content, font family, size, fill color, weight/slant, style ranges, paragraph data, and transform into the native model. Use explicit defaults only when the format defines them. Keep source metadata/fallback pixels for unsupported text features and report substitutions or raster fallback by layer name. Split descriptor support into additional subcards if its parser exceeds one cohesive change.

**Done/check:** imported fixture headlines remain editable and preserve tested appearance; unsupported text retains a visible raster fallback with an accurate report. Missing fonts are reported rather than silently reflowed without notice.

### Task 075 — Import the required layer effects

**Depends on:** 066, 072–073. **Targets:** PSD effect descriptor parsing; `layer_common`; `LayerEffects` mapping.

Map drop shadow, solid stroke, solid color overlay, and outer glow first, including enabled flags, blend mode, color/alpha, radius, distance, angle, spread, and scale where represented. Use source coordinate/unit conventions deliberately. Preserve unsupported fields in retained metadata when possible, but never claim they are rendered because they were retained.

**Done/check:** supported effect fixtures match independent references within documented tolerances. The import report only removes a warning once its effect actually renders correctly.

### Task 076 — Complete masks, adjustments, groups, and profile mapping

**Depends on:** 063, 068, 072–075. **Targets:** PSD import helpers; mask/adjustment descriptor mapping; color metadata bridge.

Audit and fix the subset used by the scene: mask placement/flags, linkage, density/feather, group hierarchy/pass-through, clipping, Curves, Hue/Saturation, and Invert. Retain image profile information and use the color pipeline correctly. Preserve full layer extents rather than dropping off-canvas pixels. Treat ambiguous flag interpretation as a fixture-backed question, not a comment to trust.

**Done/check:** a nested masked portrait with a clipped adjustment imports with correct scope and bounds. Independent fixtures prove mask positioning and group blending, including a nontrivial transform.

### Task 077 — Add an actionable import fidelity report

**Depends on:** 074–076. **Targets:** `PsdNotes`/structured warnings; import completion UI; source metadata.

Show per-layer editable/raster-fallback/unsupported outcomes, missing fonts, and effect/adjustment limitations. Permit comparing the file's merged preview against the reconstructed document when available. Preserve the original path and encourage native Save As for further work. Never silently overwrite the imported original with a reduced representation.

**Done/check:** a deliberately unsupported fixture produces a visible report explaining exactly what changed, and its visual fallback is actually visible. Fully supported fixtures do not emit generic warnings unrelated to their data.

### Task 078 — Fix appearance-preserving PSD layer export

**Depends on:** 072–077. **Targets:** `psd_layers_for`; compositor subtree/region APIs; PSD channel writer.

Export rendered pixel records for layer kinds that currently produce `tally.no_pixels`, including correct transforms and bounds. Keep editable metadata separate from fallback raster channels. For backdrop-dependent unsupported adjustments/group blends, bake the required compositing scope or provide an explicit flattened export choice; do not render an isolated layer and pretend it preserves the final appearance.

**Done/check:** exported PSD layer channels contain visible text/shape/smart-object fallback imagery where required, not only a correct merged preview. Off-canvas extents and alpha are intentional; supported layers do not disappear in an independent reader.

### Task 079 — Export the supported editable text subset

**Depends on:** 074, 078. **Targets:** `crates/psd/src/text.rs`, `write.rs`, descriptors; native-to-PSD text bridge.

Serialize the text subset defined in Task 072, with styled native fields, transform, and valid fallback pixels. Keep unsupported text features explicit and preserve their appearance as raster fallback. Do not claim editable text export merely because text metadata bytes exist.

**Done/check:** an independent PSD editor can select, edit, and restyle the exported headline. Reimport verifies the supported fields and image. Unsupported text emits the exact documented fallback outcome.

### Task 080 — Export supported effects, masks, and adjustments

**Depends on:** 075–076, 078–079. **Targets:** PSD writer/descriptor mapping; shell export bridge.

Write the supported drop shadow/stroke/overlay/glow, mask parameters/placement, Curves/Hue-Saturation/Invert, hierarchy, clipping, and blend fields. Avoid double application when a rendered fallback already includes an effect. Keep unsupported/backdrop-dependent fallbacks explicit, with a pre-export summary of lost editability.

**Done/check:** an independent reader displays the correct result and can toggle/edit supported effects and adjustments. The fallback pixels and metadata do not produce double shadows or duplicated tonal changes.

### Task 081 — Run the PSD interchange acceptance matrix

**Depends on:** 073–080. **Targets:** interchange tests; PSD support document; manual evidence.

Import an independently authored composition, edit text/mask/position, save native, export PSD, and open it in an independent editor. Also export a native composition directly. Use synthetic/licensed fixtures for external tools; do not upload private user documents to a service as part of testing. Compare layer editability and rendered appearance separately; record tool versions and tolerances.

**Done/check:** each supported feature has independent evidence. Exact byte-perfect round-trip and unsupported Photoshop features remain outside scope. If the user's actual PSD later becomes available, treat it as an additional compatibility case, not proof that every PSD works.

## Phase 9 — Finish the daily-use workspace, export, and responsiveness

### Task 082 — Tune the workspace for thumbnail composition

**Depends on:** 071, 081. **Targets:** `crates/design` tokens; `ui/src/view` and dock layout; `app-shell/src/prefs.rs`.

Provide a useful default layout: narrow tool strip, contextual options, central pasteboard, Layers with masks/effects, and accessible History/Character/Properties. Reuse the existing neutral theme, density tokens, dock system, and saved preferences. Make rulers and zoom/document dimensions readable. Verify reset-layout and panel recovery. Do not copy account/advertising/social chrome from the screenshot.

**Done/check:** the acceptance composition remains editable at 1440 × 900 and common Windows display scaling without clipped essential controls. Saved layout restores, and Reset Layout recovers hidden panels.

### Task 083 — Verify navigation, guides, shortcuts, and selection visibility

**Depends on:** 042, 082. **Targets:** canvas camera/viewport/rulers; keymap; shell focus routing.

Verify Space-pan, zoom-to-cursor, Fit, 100%, numeric zoom, ruler guides, snapping, and active-layer/target visibility while panels have focus. Guides already persist; repair demonstrated gaps instead of adding another guide store. Check shortcuts against text, numeric input, dialogs, and IME.

**Done/check:** after panning/zooming at 33.33%, the next portrait drag selects and moves the intended object. Typing a tool-letter into text or a property field does not switch tools. Undo restores edits rather than viewport navigation.

### Task 084 — Verify thumbnail export dimensions, aspect, and color

**Depends on:** 071, 082–083. **Targets:** existing Export As dialog; `OpenDocument::export_job`; `crates/raster/src/export.rs`.

Use the existing 1280 × 720 preset and size/quality pipeline; add only missing convenience presets/controls. Preserve aspect and offer explicit fit/pad/crop for an approximately 16:9 source. Specify sRGB output, JPEG background/matte behavior, PNG transparency, and quality. Show exact encoded size after final export; distinguish proxy estimates from actual bytes. Treat platform upload limits as configurable/current external requirements, not hardcoded facts in this plan.

**Done/check:** export the 3628 × 2041 composition to exact 1280 × 720 without unnoticed stretching; re-decode and verify dimensions, alpha/matte, color, and actual byte count. Export does not change the working document's size or history.

### Task 085 — Make a readable small-preview and variant export workflow

**Depends on:** 070, 084. **Targets:** Navigator or export preview; existing export preset/batch interfaces.

Allow inspection at a small thumbnail display size and at 100% exported pixels. Keep preview labels clear about image pixels versus UI points. Reuse existing batch export for different sizes/formats and explicit names; report collisions before overwriting. Named composition variants may use Duplicate Document rather than introducing Layer Comps now.

**Done/check:** the two headlines remain readable in a deliberately small preview, and exported variants have their requested names/sizes. The preview shows the actual composition, never a placeholder ramp.

### Task 086 — Benchmark the real composition, then optimize measured costs

**Depends on:** 020, 039, 064, 071, 084. **Targets:** `tests/integration/tests/performance.rs`; presenter/dirty/cache/effect/text paths.

Measure release-build load, drag preview latency, text updates, mask strokes, save, export, and peak memory using the full-size scene with 30–50 mixed layers. Record CPU/GPU/RAM and cold/warm runs. Initial UX targets: pointer feedback within 50 ms, common edits within 100 ms once warm; treat these as goals to measure on the reference machine, not CI pass/fail timings or promises. Optimize the actual hot path: bounds, text shaping locks, mask/effect halos, dirty regions, and texture uploads.

**Done/check:** report before/after evidence; deterministic tests assert bounded recomputation/cache behavior. Verify preview quality settles to final quality and save/export still use full resolution. Do not invent benchmark numbers.

### Task 087 — Move long operations off the interaction thread

**Depends on:** 011, 013, 050, 084, 086. **Targets:** focused shell job service; import/export/refresh/refinement callers; event loop.

Where benchmarks show a blocking operation, use cancellable workers with document ID, source revision, and job generation. Keep document mutation on the authoritative shell thread. Reject stale completion after another edit, tab close, or replacement; use immutable snapshots/content-addressed references. Provide progress/status for operations that need it without recomputing full images each frame.

**Done/check:** interacting with another document during a slow import/refine/export remains responsive; cancel and out-of-order completion cannot mutate the wrong revision. Errors leave the previous document usable.

## Phase 10 — Prove reliability and finish the handoff

### Task 088 — Test native round-trip and font/source portability

**Depends on:** 018, 050, 070, 087. **Targets:** project-format tests; interchange/recovery tests; native full-scene fixture.

Save/reopen rich text, image extents, transforms, masks, effects, groups, adjustments, source references, and guides together. Open on a font-restricted environment to check substitution reporting. Embedded sources must not require their original file path; linked sources must show missing status while retaining their cached image.

**Done/check:** deterministic fonts produce identical composites after round-trip; all editable model data is equal. Missing dependencies are reported and do not prevent accessing unrelated layers.

### Task 089 — Test undo/redo and crash recovery across the workflow

**Depends on:** 088. **Targets:** history/session/autosave/journal tests; shell gesture tests.

Replay a sequence containing text edits, placement, multi-layer transform, mask strokes, adjustment changes, grouping, and asset replacement. Undo/redo across save markers and simulate journal recovery. Test failure partway through a multi-command operation and cancellation of an uncommitted preview.

**Done/check:** no doubled imports, half-masks, stale assets, or partially restored styles. Recovered pixels and model match the last committed sequence. Pending preview policy matches Tasks 011/031 rather than silently inventing recoverable edits.

### Task 090 — Run regression, compatibility, and resource-bound checks

**Depends on:** 081, 083–089. **Targets:** full workspace; CI; focused import/clipboard/project validation tests.

Run fmt, locked check, clippy with warnings denied, workspace tests, and existing audit/CI checks. Include malformed text/PSD data, huge image dimensions, empty/transparent layers, singular transforms, clipboard busy state, and stale worker completion. Verify Windows behavior and other supported CI platforms without suppressing platform failures.

**Done/check:** exact command results are recorded, skipped hardware checks are identified, and regressions in painting, crop, quick mask, legacy project loading, or export are fixed. No placeholder test or ignored failure is called a pass.

### Task 091 — Execute the complete human-facing acceptance walk

**Depends on:** 090. **Targets:** running desktop application; acceptance checklist below; evidence directory.

Perform the entire acceptance sequence with mouse/keyboard and actual dialogs, including PSD interoperability where available. Save screenshots showing text editing, transform handles over moving content, an active mask target, the organized layer stack, and the export dialog/result. Check the full scene, not only a blank canvas or token demo.

**Done/check:** every required checklist item passes or remains explicitly open. A recorded screenreader/tablet limitation unrelated to thumbnail work does not trigger unrelated implementation, but essential keyboard/pointer interactions must work.

### Task 092 — Reconcile documentation and deliver the working example

**Depends on:** 091. **Targets:** root `README.md`; `docs/parity-matrix.md`, `architecture.md`, existing remaining plans; progress and PSD support documents; native sample.

Update conflicting claims using verified evidence: rich text persistence, live transform previews, smart objects, mask targets, guides, clipboard, and actual PSD limitations. Link the reusable sample and short workflow instructions. Record optional deferred work separately. Keep historical plans as history instead of rewriting past checkmarks into fabricated verification.

**Done/check:** another user can build, open the sample, edit headline/portrait/logo, save, and export using documented steps. The final handoff states what changed, checks performed, unresolved limitations, and where to resume optional work.

## Optional Phase 11 — Faster cutouts and repeated production

These tasks improve throughput but do not replace Tasks 055–064. The screenshot does not prove any automatic extraction feature was used. They remain optional unless the user makes one-click subject extraction or template automation part of the required workflow.

### Task 093 — Evaluate a local subject-selection model with evidence

**Depends on:** 064, 086, 092. **Targets:** proposed `docs/LOCAL-SUBJECT-SELECTION.md`; isolated evaluation harness.

Evaluate a small number of current local segmentation/matting models against licensed portraits with hair, glasses, hands, and difficult backgrounds. Verify model-weight license, inference-runtime license, redistribution terms, memory, CPU latency, input normalization, output semantics, and offline operation from primary sources at implementation time. Keep model selection undecided until that evidence exists; do not assume a named model is available or commercially usable.

**Done/check:** record a reproducible chosen model/runtime/version/hash and quality/latency results or conclude that manual masks remain preferable. No dependency or model is shipped solely because it is popular.

### Task 094 — Add a bounded local inference service

**Depends on:** 093 with a viable selection. **Targets:** proposed isolated `crates/subject-selection` and shell worker bridge.

Wrap model loading, normalization, inference, resize/crop mapping, and probability/alpha conversion behind an injectable service. Use local files, explicit model configuration, cancellation, memory bounds, and versioned weight checks. Keep network download separate from inference and user-controlled; do not introduce a cloud fallback. Preserve coordinate mapping to full-resolution source pixels.

**Done/check:** fake-service tests and real offline inference produce a bounded mask for the intended source. Missing/corrupt weights report an actionable error while the rest of the editor continues working.

### Task 095 — Add Select Subject and Remove Background UI

**Depends on:** 057–061, 087, 094. **Targets:** selection/layer menus; subject job UI; mask service.

Select Subject produces an editable selection; Remove Background produces a retained image plus editable mask. Preview before commit and route the result through existing mask/history services. Pin the layer revision so a result for an earlier portrait cannot attach to a replacement. Reuse progress and cancel controls.

**Done/check:** one click yields a starting cutout that can be painted/refined, undone, and saved. No source pixels are deleted, and stale inference results are discarded.

### Task 096 — Validate automatic cutout quality and packaging

**Depends on:** 095. **Targets:** local inference tests; packaging scripts/notices; quality evidence.

Compare automatic masks with baseline manual cutouts on held-out licensed photos. Inspect hair, glasses, fingers, holes, semitransparency, and background halos at full size and thumbnail size. Package runtime/model requirements and notices honestly; provide a working editor when optional weights are absent.

**Done/check:** measured quality/latency justify shipping the feature, offline behavior is tested, and its limitations are stated. A successfully returned tensor is not a quality gate.

### Task 097 — Add a local reusable asset shelf

**Depends on:** 069–070, 092. **Targets:** existing asset/preset storage; proposed asset-shelf UI.

Index user-selected local backgrounds/logos/portraits with thumbnails, names, and optional tags. Dragging an asset uses the same placement service. Define copy-into-library vs linked-file semantics; do not crawl all personal folders or invent a cloud catalog. Avoid embedding third-party brand assets into the application distribution.

**Done/check:** a saved logo/background can be reused in another document and keeps full source quality. Missing linked files are visible and do not crash the shelf.

### Task 098 — Add explicit template fields and batch variants

**Depends on:** 070, 085, 092; asset shelf is optional. **Targets:** proposed template metadata in native format; template/variant UI; existing batch exporter.

Define explicit headline/portrait/logo fields by stable layer ID with validation, not layer-name guessing. Generate variants into separate documents/output names using existing text edit, Replace Contents, and export services. Add format migration if metadata is persisted. Start with a local structured input file and preview all changes before output; do not build a general scripting language.

**Done/check:** multiple variants retain editable source documents, use correct fields and filenames, and never overwrite the template. A failed row is reported independently without silently exporting a partially populated thumbnail.

## Full acceptance sequence

Use this as the product gate, not merely as a list of API calls. Check every step through the application. A synthetic fixture can validate mechanics; portrait edge quality and independent PSD compatibility need their own real references.

1. Create a 3628 × 2041 document; confirm dimensions and Fit/33.33% zoom. Also test the existing 1280 × 720 preset.
2. Place an oversized background and verify the full image fits without cropping its stored source.
3. Place a portrait, create a rough selection, refine it into a mask, and paint corrections directly on that mask.
4. Toggle mask-only/overlay/composite views and inspect on black and white backgrounds. Source pixels remain available.
5. Create a large white headline and a smaller green headline. Set family, weight, size, tracking, and placement through real controls.
6. Reenter the first headline, select a word, replace it, and restyle a range without creating a new layer.
7. Arrange the portrait in front of part of the headline using layer order, preserving independent editable layers.
8. Place or paste two logos/graphics with transparency; select them by displayed content and resize them around correct bounds.
9. Use aspect lock, rotation, numeric positioning, snapping, align, group, and multi-layer move. The pixels move during preview.
10. Cancel a transform and a text edit; verify no stray history entries or layers. Commit repeated gestures and undo/redo them.
11. Add/edit a shadow and outline using existing layer styles; apply a clipped Curves or Hue/Saturation adjustment to the portrait.
12. Organize 30–50 layers, toggle alternatives, rename groups, and replace one placed asset without rebuilding its layout.
13. Save native, close, reopen, and verify styled text, masks, sources, transforms, effects, groups, guides, and adjustment editability.
14. Duplicate the document to make a variant. Confirm the template/original remains unchanged.
15. Export 1280 × 720 JPEG and PNG with explicit aspect/matte/color policies; reopen exported files and verify dimensions, appearance, and byte count.
16. Import an independent PSD fixture, inspect its fidelity report, edit supported layers, export, and check it in an independent editor.
17. Recover a simulated interrupted session and verify the last committed composition without doubled or partial edits.
18. Repeat essential drag/type/mask operations at 33.33%, 100%, and 200% zoom and Windows 100%, 150%, and 200% scaling where available.
19. Record release-build responsiveness and memory on the actual composition. Check that long work can be canceled without corrupting state.

## Review rules that prevent another false completion

| Claimed capability | Evidence required |
|---|---|
| Text style works | A real control changes the model and rendered pixels; style survives native reopen. |
| Text is editable | Enter an existing layer, replace a middle word, preserve styles, confirm/cancel, undo. |
| Transform works | Actual object-bound handles and moving pixels; correct commit/cancel; layer kind/source preserved. |
| Auto-select works | The visible checkbox changes shell behavior and picks transformed, masked, non-raster content correctly. |
| Masks work | Thumbnail selects coverage target; four creation variants have correct pixels; painting changes only coverage. |
| Place works | Oversized source retains all pixels, is fitted via transform, and is still complete after reopen. |
| Clipboard works | External bitmap interchange is verified on the OS in addition to fake-service tests. |
| PSD import/export works | Independent reader/writer evidence checks layers and editability, not only the merged preview. |
| Fast enough | Recorded release-build results for the mixed-layer scene; no invented frame-rate claim. |
| Complete | Full acceptance sequence plus checks; each unavailable environment/device gate is named. |

## Suggested implementation modules and ownership

These names are proposals, not existing files to assume are present. Create only modules needed by the cards.

| Proposed module | Owns | Must not own |
|---|---|---|
| `layer-model/src/text.rs` | Persistent text data and validation | Font loading, shaping, filesystem, UI |
| `compositor/src/bounds.rs` | Reused content/visual bounds and bounded sampling support | UI selection state |
| `app-shell/src/edit_target.rs` | Validated active/selected content-mask target | Pixel algorithms |
| `app-shell/src/interaction_geometry.rs` | Shared coordinate conversion | A second camera implementation |
| `app-shell/src/edit_session.rs` | Preview ownership and commit/cancel lifecycle | Independently serialized document state |
| `app-shell/src/text_session.rs` | Draft text, caret/range, text input routing | A second text shaper |
| `app-shell/src/placement.rs` | Full-source placement and source replacement | Separate image codecs |
| `app-shell/src/clipboard.rs` | OS/fake bitmap interchange | URL fetching or image rendering |
| `app-shell/src/mask_edit.rs` | Targeted mask commands and refinement orchestration | Duplicate selection morphology |
| `ui/src/dialogs/refine_mask.rs` | Refinement controls and intents | Direct document mutation |
| `app-shell/src/jobs.rs` | Cancellable document/revision-bound jobs | Off-thread unsynchronized document mutation |
| `subject-selection` (optional crate) | Local model inference boundary | Editor history or cloud uploads |

## Handoff record template

```text
Task: NNN — exact title
Status: verified / in progress / blocked / optional deferred
Baseline commit and pre-existing working-tree changes:
What changed and why:
Files and key symbols:
Regression demonstrated before fix:
Commands actually run and exact results:
Real UI/OS/interoperability evidence, if required:
Known limitation or blocked check:
Next ready task and prerequisites:
```

Do not fill this template with anticipated results. The next executor should be able to continue from facts rather than another completion claim.
