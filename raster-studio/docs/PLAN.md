# Raster Studio — Build Plan to Photopea Parity

> Written 2026-08-22 after a full audit of the scaffold. This document is the
> authority on **what we are building, in what order, and why**. `TODO.md` is
> the task list derived from it.

## 1. Where the project actually stood

The scaffold described itself as a working Phase-0 vertical slice. It was not.
Verified findings, all reproduced:

| Claim in the old docs | Reality |
| --- | --- |
| "cargo check / cargo test pass" | Workspace **did not compile** (5 errors). Cargo was never installed in the environment it was written in. |
| "window + GPU canvas, 60 FPS pan/zoom" | App opens, then renders a **black canvas**. Verified by screenshot. |
| "Layers ✅, History ✅, Project format ✅" | Real code, but the renderer never reads `Document.layers`, and the app never calls save/load. |
| "Image import/export 🟡" | `decode_path` works and is the only engine code the binary touches. |
| Tile cache | No tile store, no tile grid, no mip chain exists. |
| Brush tool | Captures points, emits **no command**. Cannot paint a pixel. |

Root causes, in order of severity:

1. **Nothing owns pixels.** `RasterLayer` holds `Option<AssetId>` and nothing
   resolves it. There is no `Command` variant that mutates a pixel. The engine
   is structurally incapable of editing an image.
2. **The renderer is disconnected from the document.** The startup image is a
   loose GPU texture; the document is empty. One fullscreen quad, no compositor.
3. **Two render bugs make even that quad invisible**: `Camera::clip_to_uv`
   returns a degenerate affine (V varies with screen X), and the egui pass uses
   `LoadOp::Clear`, erasing the canvas pass beneath it.
4. **Six crates are orphans** — `adjustments`, `asset-store`, `licensing`,
   `updater`, `text-engine`, and `raster`'s tile module have zero dependents.
5. **State-corrupting bugs** in the parts that do work: `LayerTree::move_layer`
   allows cycles (infinite recursion), `remove` orphans group children,
   `Transaction` applies partially with no rollback and no history entry, and
   `project-format` has a path-traversal read via `manifest.document_path`.

What is genuinely good and worth keeping: the command/inverse/history design in
`editor-core`, the `LayerTree` shape, `raster::codec`, the sRGB/premultiply math
in `color`, and the CPU/GPU blend-math parity between `blend.rs` and
`composite.wgsl`.

## 2. Architectural decisions

### D1 — Stay native Rust (winit + wgpu + egui)

The toolchain builds here (Rust 1.98 + MSVC 2022 + Win SDK, verified). Native
GPU-accelerated editing at 4K/8K is this project's reason to exist versus a web
editor. Rewriting the stack would discard the one thing that differentiates it.

### D2 — The CPU tile engine is authoritative; the GPU presents

This is the keystone decision.

Photopea itself runs its entire pixel pipeline on the CPU in the browser. A Rust
tile engine over `rayon` is comfortably faster than that, so we do not need a
GPU compositor to reach parity — and we gain something worth more:

- **Every pixel operation is headlessly unit-testable.** No GPU, no window, no
  device loss, no driver variance. Golden-image tests run in CI.
- Blend modes, adjustments, filters, and selections have exactly **one**
  implementation, so CPU/GPU drift is impossible by construction.
- The GPU keeps the job it is genuinely best at: presenting composited tiles and
  running pan/zoom at display rate.

GPU acceleration for specific hot paths (large blurs, big composites) is a later
optimization behind the same trait, never a second source of truth.

### D3 — Remove the ComfyUI dependency entirely

Per the project directive that this be fully original with no dependency on
another project. This deletes `crates/ai-runtime`, `crates/ai-contracts`,
`apps/runtime-manager`, `workflows/`, `runtime/`, and the GPL-boundary notices.

It also removes: the GPL/proprietary boundary risk, the "obtain legal review
before distribution" blocker, an external Python runtime, and six unimplemented
safety controls that the threat model claimed were in place.

Only **one line of real code** referenced ComfyUI (`dist_path` in a default);
the coupling was 99% documentation. The editor's compile graph is untouched.

### D4 — An Apple-style design system as its own crate

`crates/design` owns tokens (color, type scale, spacing, radii, shadows, motion,
elevation) and themed egui widgets. The design language is calm, light-first,
high-contrast-on-demand, generous in spacing, restrained in chrome: content is
the interface, panels recede. Tokens are plain data, so they are unit-testable
and a dark/light pair can be verified for contrast ratios programmatically.

Concentrating this in one crate keeps polish from being smeared across every
panel, and lets the whole app be re-skinned from one file.

### D5 — Honest, tiered parity

"Photopea parity" is a large surface. We define it explicitly so completeness is
a fact rather than a feeling. See `docs/parity-matrix.md` for the tiers:

- **Tier A — Core.** The editor is genuinely usable for real work. Must be
  complete and correct before anything else starts.
- **Tier B — Pro.** The features that make it competitive with Photopea.
- **Tier C — Deferred.** Documented as out of scope for this release, with the
  reason. Nothing in Tier C is ever silently implied to work.

A feature is "done" only when it has tests, is reachable from the UI, and
survives save/reload.

## 3. Target crate graph

```
apps/studio-desktop
  └── app-shell ── ui ── design
        │          └── editor-core
        ├── render ── render-shaders          (presentation only)
        └── editor-core
              ├── layer-model ── color
              ├── compositor  ── raster, layer-model, color   (CPU, authoritative)
              ├── selection   ── raster
              ├── adjustments ── color
              ├── filters     ── raster, color
              ├── tools       ── raster, vector, text-engine
              ├── vector
              ├── text-engine
              └── project-format ── asset-store, raster, psd
```

New crates: `compositor`, `filters`, `selection`, `vector`, `design`, `psd`.
Deleted crates: `ai-contracts`, `ai-runtime`, `runtime-manager`.

Layering rules (unchanged in spirit, enforced by dependency direction):
- `layer-model`, `color`, `vector` are leaf domain crates: no I/O, no GPU.
- `compositor` is pure CPU and pure function: `(document, region) -> pixels`.
- `render` owns all wgpu. Nothing above it touches the GPU.
- `project-format` owns all persistence.
- `ui` is a view: it emits commands and never mutates the document.

## 4. Build order

Each wave ends with `cargo check --workspace --all-targets` and
`cargo test --workspace` green. No wave starts before the previous one is green.

**Wave 1 — Foundation.** Fix the state-corrupting bugs; give the engine pixels.
Tile store, color dispatch, 27 blend modes, masks, pixel commands, and the three
render bugs that make the canvas black. Ends with: *an image visibly renders.*

**Wave 2 — Compositing and image ops.** CPU compositor (groups, masks, clipping,
blend, opacity), the full adjustment set, the filter set, the selection engine.
Ends with: *adjustments and filters visibly change the image.*

**Wave 3 — Tools.** Brush engine that actually paints, eraser, clone, heal,
gradient, bucket, dodge/burn/smudge, shapes, crop, move/transform, text, pen.
Ends with: *you can draw, select, and transform.*

**Wave 4 — Persistence and interop.** `.rstudio` with real tile persistence and
migrations, PSD read/write, export presets. Ends with: *work survives a restart.*

**Wave 5 — The application.** The Apple-style UI: menu bar, tool palette, tool
options, all panels, dialogs, rulers/guides/grid/snap, shortcuts, drag-and-drop,
preferences, multi-document. Ends with: *it feels like a product.*

**Wave 6 — Ship.** CI, perf gates, docs, screenshots, installer, release.

## 5. How work is executed

Every task runs as a **doer/reviewer pair**:

- The **doer** implements the task and its tests.
- The **reviewer** reads the *real git diff* — not the doer's report — and
  mutation-checks each new test: revert the fix, confirm the test goes red,
  restore it. A test that passes against the unfixed code is not a test.
- The reviewer rejects with specifics; the pair loops until it accepts.

This exists because the audit above is what happens without it: a scaffold that
documented itself as working, in prose, while not compiling.

## 6. Definition of done

1. `cargo check --workspace --all-targets` clean, `cargo clippy` clean.
2. `cargo test --workspace` green, including golden-image tests.
3. Every Tier A and Tier B feature is reachable from the UI and survives a
   save/reload round-trip.
4. The app launches, opens a real image, edits it, and exports it correctly —
   verified by screenshot, not by assertion in prose.
5. `docs/parity-matrix.md` states the truth about every feature, including the
   deferred ones.
