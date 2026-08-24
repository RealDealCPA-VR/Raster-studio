# Architecture

Raster Studio is a native, local-first desktop image editor. One process, one
window, no server, no sidecar, no network stack.

The keystone decision is stated in [`PLAN.md`](PLAN.md) §D2 and is what the rest
of this document falls out of:

> **The CPU tile engine is authoritative. The GPU presents.**

`crates/compositor` answers "given this document and this region, what pixels
does the user see?" — in parallel Rust, on the CPU, in linear premultiplied
`f32`. The GPU's whole job is to put that answer on screen at display rate.

Two things follow, and they are the reason for the split:

- **Every pixel operation is headlessly testable.** No window, no adapter, no
  driver variance. Blend modes, adjustments, filters, selections, masking,
  clipping and export are ordinary functions over buffers, and the test suite
  calls them directly.
- **There is exactly one implementation of each.** A GPU compositor running
  beside a CPU one is two sources of truth that drift. Here there is nothing to
  drift from: `crates/render` never composites a document.

## The pixel path, end to end

```
  a file on disk                      the user's pointer
        │                                     │
        ▼                                     ▼
  raster::codec                        tools::Tool  (a gesture)
   (bounded decode)                            │
        │                                      ▼
        └──────────────▶  editor_core::Command  ──▶ editor_core::History
                                    │                  (invertible, journalled)
                                    ▼
                         editor_core::Document
                         (a layer tree + tile *hashes*)
                                    │
                app_shell::dirty ◀──┤ which tiles did that touch?
                                    ▼
              compositor::TileCompositor  ×  compositor::TileSource
              (CPU, rayon, linear premultiplied f32; cache keyed by inputs)
                                    │
                                    ▼
                     app_shell::presenter::CanvasPresenter
                     (one document-sized RGBA8 GPU texture,
                      updated per dirty tile, mips regenerated)
                                    │
                                    ▼
                render::Canvas + render_shaders::QUAD_WGSL
                (pan/zoom camera affine, transparency checkerboard)
                                    │
                                    ▼
                     egui pass (LoadOp::Load) ──▶ swapchain
```

The document is the only source of pixels on screen. `OpenDocument::composite`
is the single entry point the canvas has; there is no second path in which an
image is drawn without being in the document.

## Crate graph

Solid edges are real `[dependencies]` entries in the manifests.

```
apps/studio-desktop
  ├── telemetry                                  (tracing init, local bundles)
  └── app-shell ── winit · wgpu · egui-wgpu · egui-winit · rfd · dirs
        ├── ui ── design
        │     └── editor-core · layer-model · tools · compositor · selection
        │        · filters · adjustments · vector · text-engine · raster · color
        ├── render ── render-shaders                      (presentation only)
        ├── compositor
        ├── project-format
        ├── tools
        ├── editor-core
        ├── layer-model
        ├── design
        └── raster

compositor      ── editor-core · layer-model · raster · color · adjustments
project-format  ── editor-core · layer-model · raster · asset-store · compositor
tools           ── editor-core · layer-model · raster · color
                   · filters · selection · vector
selection       ── editor-core · raster · color
filters         ── raster · color
adjustments     ── color · layer-model
editor-core     ── layer-model · color · raster
text-engine     ── layer-model · cosmic-text
psd             ── layer-model
asset-store     ── raster
raster          ── color · image
design          ── egui

leaves:   color · vector · layer-model · render-shaders
detached: licensing · updater   (no crate in the workspace depends on either)
```

`tests/integration` sits above everything and drives `app_shell::doc::OpenDocument`
— the engine the application itself runs — rather than a look-alike.

## Layering rules

These are enforced by dependency direction, and each is stated at the strength
the manifests actually support.

| Rule | Status |
| --- | --- |
| `color`, `vector` and `layer-model` are leaf domain crates: no I/O, no GPU, no document | Holds. Their only dependencies are `serde`, `glam`, `uuid` and `thiserror`. |
| `compositor` is a pure function `(document, source, region, level) -> pixels` | Holds. No `std::fs`, no `wgpu`, no globals; it takes tile bytes through the `TileSource` trait. |
| `editor-core` owns the document, commands and history — no GPU, no disk | Holds. It names `raster` only for the *vocabulary* of tile identity and geometry. |
| `ui` is a view: it never mutates the document | Holds. Nothing in `crates/ui` holds a `&mut Document`; controls resolve to an `Intent` that the shell performs. Its only `std::fs` calls are inside `#[cfg(test)]` source-scanning tests. |
| All wgpu lives below `render` | **Partly.** `render` owns every pipeline, shader and texture type, and no crate other than `app-shell` and `render` names `wgpu`. But `app-shell` owns the window, the surface, the command encoder and the egui pass, so it names `wgpu` too. The accurate rule is: *`render` owns the GPU work; `app-shell` owns the frame.* Nothing above `app-shell` touches either. |
| `project-format` owns all persistence | **Of the project.** The `.rstudio` package, its tiles, assets, preview and journal are entirely its business. The application's *own* state — `preferences.json`, `recent.json`, `sessions/{pid}.json`, scratch autosaves — is `app-shell`'s (`prefs.rs`, `recent.rs`, `session.rs`), and image import/export file I/O is `raster`'s (`codec.rs`, `export.rs`). |

## What each crate owns

| Crate | Owns |
| --- | --- |
| `apps/studio-desktop` | The executable: tracing init, argv, `app_shell::launch` |
| `app-shell` | Window, event loop, surface, frame, editor state, documents, keymap, preferences, recent files, session markers, autosave, crash recovery, dirty-tile tracking, the canvas presenter |
| `ui` | Menus, panels, canvas widget, dialogs, tool options — as values, not mutations |
| `design` | Tokens (colour, type scale, 4pt grid, radii, elevation, motion), the egui theme, themed widgets |
| `editor-core` | `Document`, `Command`, `History`, the `PixelStore` of tile hashes, `Selection` |
| `layer-model` | Layer tree, groups, masks, effects data, and the reference math for all 27 blend modes |
| `compositor` | The authoritative CPU tile compositor, its tile cache, and the adjustment application path |
| `raster` | Tiles, tile grids, mip chains, pixel formats, the codec facade, export |
| `color` | Colour spaces, transfer functions, premultiply, CIELAB, HSL/HSV |
| `selection` | Marquee, lasso, wand, colour range, morphology on fractional coverage, outline extraction |
| `adjustments` | Parametric, non-destructive adjustment math |
| `filters` | Blur, sharpen, noise, distort, stylize, pixelate, render, convolution — tile-parallel over `rayon` |
| `tools` | The brush engine and every interactive tool: gestures in, one undoable command out |
| `vector` | Bézier paths, one anti-aliased coverage rasteriser, stroke-to-outline, booleans, SVG path I/O |
| `text-engine` | Font enumeration and matching, shaping and layout via `cosmic-text`, glyph rasterisation |
| `project-format` | The `.rstudio` package: read, write, migrate, verify, recover |
| `asset-store` | Content-addressed blob storage, in memory or write-through to disk |
| `psd` | `.psd` read and write, written from the published format documentation |
| `render` | wgpu: context, textures, mip generation, the camera affine, the quad pass, offscreen readback |
| `render-shaders` | The WGSL sources (`quad`, `composite`, `mipmap`) as embedded constants |
| `licensing` | Offline Ed25519 entitlement verification |
| `updater` | Ed25519 verification of an update manifest |
| `telemetry` | `tracing` initialisation and a local diagnostic bundle |

## What is built but not wired in

Stated here rather than left for someone to discover:

- **`psd` is now wired.** `app-shell` depends on `psd` and converts both ways:
  `OpenDocument::open_psd` builds a real `Document` (groups, masks, blend
  modes, channels, adjustments) from the layer section, and
  `OpenDocument::export_psd_to` lowers a document back to a layered PSD.
  `OpenDocument::export_to` routes a `.psd` destination there rather than
  flattening, so "Save as PSD" keeps its layers, and `looks_like_psd` picks the
  document road on content rather than extension. What the format cannot carry
  in either direction is reported in `PsdNotes` rather than dropped silently.
- **`render::CompositePass` and `composite.wgsl` are unused by the application.**
  They are constructed only in `crates/render/tests/gpu.rs`. Nothing composites
  on the GPU; see [`render-pipeline.md`](render-pipeline.md).
- **`licensing` and `updater` have no dependents.** No code path in `app-shell`
  verifies an entitlement or an update manifest. They are library code with
  their own tests, not a shipped control.
- **`asset-store`'s disk backend is not on the application's path.**
  `project-format` builds the memory-only store (`assets::new_store`), and
  `AssetStore::open` — the write-through, symlink-checked, refcount-journalled
  variant — is called only from that crate's own tests.
- **`layer-model::LayerEffects` is data the compositor ignores.** A styled layer
  saves, reloads and edits; it renders unstyled.

## Non-goals, and where they are enforced

| Principle | Where it holds |
| --- | --- |
| No cloud, no account, no telemetry upload | There is no networking code in the workspace — no `std::net`, no socket type, no HTTP or TLS crate anywhere in the binary's dependency graph. `telemetry::DiagnosticBundle` is written locally and defaults `upload_consented` to `false`. |
| No AI sidecar, no external runtime | Removed in full ([`PLAN.md`](PLAN.md) §D3). No process is spawned, no interpreter is bundled, and there is no copyleft boundary to police — see [`../LICENSES/THIRD_PARTY_NOTICES.md`](../LICENSES/THIRD_PARTY_NOTICES.md). |
| Always editable | Every user-visible edit is an invertible `editor_core::Command`; adjustments are parametric; pixels are referenced by content hash so a stroke across a hundred tiles is one small command and one undo step. |
| The native format is authoritative | `.rstudio` carries a mandatory package version *and* a mandatory document version, an integrity seal, the pixels, and a command journal. See [`file-format.md`](file-format.md). |
| Correctness does not require a GPU | `compositor` is the only thing that decides what a pixel is. GPU-backed tests detect the absence of an adapter and skip. |
