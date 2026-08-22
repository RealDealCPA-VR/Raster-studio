# Raster Studio

A local-first, GPU-native raster editor built in Rust, with **optional** local
AI workflows powered by a pinned ComfyUI sidecar. **The editor is the product.**
ComfyUI is a local AI runtime, not the primary interface.

## Product principles (non-negotiable)

1. No required cloud account or inference API.
2. The document remains editable after every operation (non-destructive by default).
3. The application stays responsive at large image sizes (4K/8K/multi-layer).
4. AI outputs enter the document as **layers, masks, and recorded metadata**.
5. The proprietary editor is **architecturally separate** from GPL-covered ComfyUI.
6. The native `.rstudio` project format is authoritative; PSD interop is phased
   and never blocks the core editor.

## Architecture

```
Native Rust Desktop App  (winit + wgpu + egui)
  ├── Workspace UI / tools
  ├── Editor Core        document DAG + history
  ├── GPU Compositor     tiles + shaders + texture cache
  ├── Asset store        tile cache, embedded/linked assets
  ├── Project I/O        .rstudio package
  └── Licensing / Update offline entitlement
            │ authenticated localhost IPC
            ▼
Optional Local AI Runtime Sidecar
  Pinned ComfyUI + isolated Python env, curated workflows,
  bound to 127.0.0.1 with a random per-launch token.
```

**Core rule:** the editor speaks a stable, typed internal AI protocol
(`ai-contracts`). Only the `ai-runtime` adapter translates an action into a
ComfyUI workflow graph. ComfyUI graph details never touch the document model.

## Workspace layout

| Path | Purpose |
| --- | --- |
| `apps/studio-desktop` | The desktop executable: startup, menus, wiring |
| `apps/runtime-manager` | Optional standalone AI-runtime manager |
| `crates/app-shell` | winit loop, windows, shortcut routing |
| `crates/ui` | Panels, docks, commands, tool options (egui) |
| `crates/editor-core` | Document, commands, history, selection |
| `crates/layer-model` | Layer tree, blend state, masks, groups |
| `crates/render` | wgpu renderer + compositing graph |
| `crates/render-shaders` | WGSL sources + pipeline descriptors |
| `crates/raster` | Tiles, mipmaps, codecs, pixel formats |
| `crates/tools` | Brush, lasso, crop, transform, clone |
| `crates/adjustments` | Levels, curves, HSL, blur, etc. |
| `crates/project-format` | `.rstudio` package read/write/migrations |
| `crates/asset-store` | Content-addressed blobs, cache, linked assets |
| `crates/color` | Color spaces, ICC transforms, proofing |
| `crates/text-engine` | (postponed) editable text layers |
| `crates/ai-contracts` | Typed AI requests/responses only |
| `crates/ai-runtime` | Sidecar lifecycle + local IPC client |
| `crates/licensing` | Ed25519 offline entitlement + trial state |
| `crates/updater` | Signed update manifest handling |
| `crates/telemetry` | Local diagnostics, optional opt-in send |

## Getting started

```bash
# From this directory (raster-studio/)
cargo check --workspace          # type-check everything
cargo test  --workspace          # unit + golden-image tests
cargo run   -p studio-desktop    # launch the editor
```

> On Linux you need a Vulkan/GL-capable environment for the wgpu window.
> Headless CI should run `cargo check`/`cargo test` (non-GPU tests) only.

## Roadmap (phases)

- **Phase 0 — engine proof:** window + GPU canvas, PNG/JPEG, tiles, raster
  layers, history, project save/load. *Currently scaffolded.*
- **Phase 1 — usable editor:** groups/masks/clipping, blend modes, core tools,
  adjustments, export presets, tablet pressure.
- **Phase 2 — commercial differentiator:** ComfyUI integration, AI provenance,
  runtime UX, licensing, reliability.
- **Phase 3 — parity foundations:** color management, 16-bit, text, vector,
  smart objects, advanced selection, TIFF/PSD import.
- **Phase 4 — deeper parity:** retouching, smart filters, actions, batch,
  prepress.

See `docs/parity-matrix.md` for the full feature matrix.

## Licensing & GPL boundary

The Rust editor is proprietary. The ComfyUI runtime is a **separately
distributed, process-isolated** component. See `LICENSES/` for the third-party
inventory and the GPL/supply-chain checklist. Obtain legal review before
commercial distribution.
