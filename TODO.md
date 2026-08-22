# Project To-Do List — per module

> State assessment (Aug 2026): **Phase-0 vertical-slice scaffold**. The vertical
> slice works end-to-end (document → commands → history → journal → save/load →
> codec → GPU texture → pan/zoom canvas). Roughly half the crates contain real,
> tested logic; the rest are honest, clearly-labeled placeholders. There are **no**
> `todo!()`/`unimplemented!()`/`#[allow(dead_code)]` anywhere — deferrals are
> data-level (reserved enum variants) or prose comments.
>
> Status legend: ✅ done · 🔶 partial/scaffold · ⬜ not started.
> Keep changes **modular**: each item below is scoped to exactly one crate and
> must respect the layering rules in `docs/architecture.md` (e.g. `render` owns
> all wgpu; `project-format` owns all persistence; `ai-contracts` is the firewall;
> UI only emits commands). No model crate may touch the GPU or disk.

## Phase 0 — finish the engine proof (current)

### `crates/app-shell` 🔶
- [x] **Wire `shortcuts` into real input routing.** `on_keyboard()` turns a winit
      `KeyEvent` into a `Chord` and `dispatch_action()` performs zoom actions via
      the camera (done this session; document-bound actions logged pending the
      editor bus).
- [x] **Integrate egui into the frame.** egui-winit/egui-wgpu renderer set up in
      `resumed`; `Workspace` drawn as an overlay in `redraw()`; panels' commands
      applied through the document `History` (done this session).
- [ ] **Route canvas pan/zoom only when not over a panel.** Mouse/scroll still
      pans/zooms the camera even when the pointer is over an egui panel. Gate
      mouse handling on `egui` focus (`egui_state` consuming the event) so panels
      get exclusive input.

### `crates/ui` 🔶
- [x] **Wire panels to emit commands.** `Workspace` holds a `selected` layer +
      command outbox (`drain_commands`); `layers_panel` emits Create/Delete/
      SetLayerProperties (add, delete-selected, toggle visibility); never mutates
      the document directly (done this session).
- [ ] **Implement `show_tool_options`.** The flag exists but nothing draws tool
      options yet. Add an options panel bound to the active `tool::ToolId`.

### `crates/editor-core` 🔶
- [x] **Fix `DeleteLayer` undo re-parenting.** `apply` now captures `(parent,
      index)` via `current_location` and returns a `Transaction[CreateLayer +
      MoveLayer]` inverse restoring the exact prior spot (done this session).
- [x] **Add `Command` serialization tests at the unit level.** Added
      `command_variants_serde_roundtrip` covering all six variants JSON losslessly
      (done this session).
- [ ] Begin filling `Selection::Mask` (masks inside selections) — extend
      `Selection` beyond `None`/`Rect`.

### `crates/layer-model` 🔶
- [ ] **Make `RasterLayer` hold tile references** (`source_asset` is deliberately
      minimal now). Add tile-set attachment so raster layers actually reference
      pixel content.
- [ ] Equip `ShapeLayer` with a real path representation (`path_svg` string is a
      placeholder) or formally defer it with a typed marker.

### `crates/render` 🔶
- [ ] **Wire the unused `COMPOSITE_WGSL` into a compositing pass.** Only the
      single-source `quad.wgsl` path runs. Add per-layer blend passes backed by
      the composite shader (indices must stay in sync with
      `layer_model::BlendMode::shader_index()`).
- [ ] **Add tile-based viewport rendering + GPU tile LRU cache** per
      `docs/render-pipeline.md` (visible tiles + prefetch border, mip selection
      from `Camera.zoom`).
- [ ] On edit, add dirty-tile invalidation into dependent composite tiles.

### `crates/render-shaders` 🔶
- [ ] Keep `composite.wgsl` blend indices pinned to `BlendMode::shader_index()`;
      add golden-image coverage as more blend passes come online. (Shader source
      itself is complete — mostly downstream wiring.)

### `crates/raster` 🔶
- [ ] **Add a tile-grid layer over primitives** (canvas → tile addressing,
      visible-tile iteration) so `render`/`asset-store` can use content-addressed
      tiles (256→512 TILE_SIZE is a one-line change worth benchmarking).
- [ ] Add tile compression for inactive tiles / memory-pressure eviction (CPU
      side), as referenced by the render pipeline.

### `crates/project-format` 🔶
- [ ] **Populate `tiles/`, `assets/`, `ai/`, `previews/`** — currently the package
      is saved but these directories stay empty. Wire tile/blob persistence and a
      composite thumbnail (`previews/composite-preview.webp`).
- [ ] **Implement `migrate()`** (currently a no-op passthrough) on
      `format_version` to satisfy the versioning contract in `docs/file-format.md`.
- [ ] Implement **linked-asset collection** ("collect assets" mode →
      `assets_collected = true` for portable packages).

### `crates/asset-store` 🔶
- [ ] **Add the disk-backed layer.** Store refcounted blobs on disk keyed by
      BLAKE3 hash (extend the in-memory `AssetStore`, don't replace it).
- [ ] Add a **GPU-LRU tile cache** layer to integrate with `render`'s tile model.

### `crates/color` 🔶
- [ ] (Phase 3) Swap placeholder `DisplayP3`/`IccProfile` transforms for a real
      ICC engine (LittleCMS binding or equivalent); keep the metadata `ColorSpace`
      shape untouched.

### `crates/adjustments` 🔶
- [ ] Add `hue`/`saturation` and `ColorBalance`. `layer_model::AdjustmentKind`
      declares them but `adjustments` only implements levels/exposure/saturation/
      curve.
- [ ] Add GPU counterparts later; keep the existing CPU functions as the ground
      truth for golden tests.

### `crates/tools` 🔶
- [ ] **Make `BrushTool` actually paint.** It captures a stroke (points/pressure)
      but `on_pointer_up` never emits a paint `Command` — the tile-paint path
      comment is still pending. **Blocker (assessed this session):** embedding the
      tool `BrushStroke` in `editor_core::Command` would invert the `tools →
      editor-core` dependency, and `Command` refs pixel payloads by id — so the
      paint command + tile-delta application must land together with the
      `render`/`raster` tile-paint path (Phase 1). Do the render tile path first.
- [ ] Implement remaining tools declared in `ToolId`: Move, Crop, Eraser,
      selections, Lasso, Wand, Clone, Gradient, Pen. (Start with Eraser/Transform
      reusing the brush/delta path.)

## Phase 1 — usable editor

- [ ] `crates/layer-model`: masks + clipping + group blend passes (group ids exist
      already).
- [ ] `crates/tools`: tablet pressure plumbing through `PointerEvent.pressure` →
      stroke.
- [ ] `crates/render`: layer selection/visual feedback (selection ants, handles)
      in the UI overlay pass.
- [ ] `crates/project-format` + `crates/raster`: export presets (PNG/JPEG/WebP,
      scale, quality, batch) — codec `ExportFormat` exists.

## Phase 2 — commercial differentiator

### `crates/ai-runtime` 🔶 (the only crate with explicit `TODO(phase-2)`)
- [ ] **sidecar::start()** — spawn the pinned ComfyUI child process targeting
      `127.0.0.1` with a random per-launch `CapabilityToken`; tie child lifetime
      to the host app.
- [ ] **sidecar::stop()** — graceful kill / orphan cleanup.
- [ ] **sidecar::submit()** — build the workflow graph (adapter: operation →
      graph), perform HTTP IPC against the sidecar, stream progress, support
      cancellation. This is the *only* component that translates to ComfyUI.

### `crates/ui` + `crates/ai-runtime` 🔶
- [ ] Runtime install/repair/status/model-profile UX (runtime-manager surfaces).

### `apps/runtime-manager` 🔶
- [ ] Complete now that sidecar transport lands: real start/stop/status/repair
      flow (currently a demonstrational stub that exits).

### `crates/updater` 🔶
- [ ] Wire `verify_manifest` into an update check + apply path. (Trust decision is
      complete; no download/apply by design.)

### `crates/licensing` 🔶
- [ ] Embed/wire the Ed25519 **public key** into the app and add the `verify` call
      site (key management currently left to the app).

### `crates/telemetry` 🔶
- [ ] Wire crash handling → `DiagnosticBundle`; gate the (not-yet-implemented)
      network upload behind explicit opt-in.

### `workflows/` 🔶
- [ ] Add the workflow graph subdirs referenced by `workflows/README.md`
      (`product-photo/`, `inpaint/`, `upscale/`) — only `manifests/` exists today;
      pin custom nodes in `runtime/custom-nodes-lock.json`.

## Phase 3 — professional parity foundations

- [ ] `crates/text-engine` ⬜: real implementation (shaping via cosmic-text/swash,
      fonts, glyph→tile compositor). Deliberately postponed; everything here is
      placeholder.
- [ ] `crates/color`: real ICC engine (see above).
- [ ] `crates/raster::PixelFormat::Rgba16` → 16-bit channel workflows.
- [ ] `crates/render` + `crates/color`: display transform / soft proofing.
- [ ] `crates/layer-model`: fill out `SmartObjectLayer` (Phase 3).
- [ ] `crates/editor-core::Selection::Mask`: subject/edge selection, refine.
- [ ] `crates/raster::codec`: TIFF import (feature already on) + scoped export.

## Phase 4 — deeper parity

- [ ] Retouching (clone/heal/patch/content-aware-like) in `crates/tools`.
- [ ] Smart filters / parametric effects (`crates/adjustments` + `layer-model`).
- [ ] Actions/automation replay surfaced in UI (`editor-core::Command` are already
      serializable/replayable).
- [ ] Batch/headless export pipeline (`project-format` + `raster`).

## Cross-cutting / repo hygiene
- [ ] Add CI (`.github/workflows`) — `cargo check --workspace`, `cargo test
      --workspace` (non-GPU), `cargo clippy`, `cargo audit`, rustfmt. Toolchain
      already pinned via `rust-toolchain.toml`.
- [ ] Add a `.gitignore` (exclude `target/` and `runtime/comfyui/` artifacts).
- [ ] No network crate in the editor's non-sidecar dependency graph (verify once
      AI transport is added, per threat model).
- [ ] Verify editor↔ComfyUI GPL boundary after sidecar transport lands
      (`docs/threat-model.md`, `LICENSES/`).

## Verification commands (run from `raster-studio/`)

```
cargo check --workspace   # type-check all modules
cargo test --workspace    # unit + golden-image tests (non-GPU in headless CI)
```

> Note: `cargo`/`rustup` were not on PATH in this environment, so a fresh
> `cargo check/test` could not be run here. Re-run before/after each module change.
