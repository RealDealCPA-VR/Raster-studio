# Architecture

Raster Studio is a local-first, GPU-native raster editor. **The editor is the
product**; the ComfyUI-based AI runtime is an optional, process-isolated
sidecar.

## System diagram

```
┌─────────────────────────────────────────────────────────────┐
│                    Native Rust Desktop App                    │
│  winit + wgpu + egui                                          │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────────┐   │
│  │ Workspace   │  │ Editor Core  │  │ GPU Compositor     │   │
│  │ UI / tools  │──│ document DAG │──│ tiles + shaders    │   │
│  └─────────────┘  │ history      │  │ textures + cache   │   │
│                   └──────────────┘  └────────────────────┘   │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────────┐   │
│  │ Asset store │  │ Project I/O  │  │ Licensing / Update │   │
│  │ tile cache  │  │ .rstudio pkg │  │ offline entitlement│   │
│  └─────────────┘  └──────────────┘  └────────────────────┘   │
└───────────────────────────┬───────────────────────────────────┘
                            │ authenticated localhost IPC
                            ▼
┌─────────────────────────────────────────────────────────────┐
│             Optional Local AI Runtime Sidecar                 │
│  Pinned ComfyUI + isolated Python env                         │
│  Curated workflow templates + model/profile manifest         │
│  Bound to 127.0.0.1; random per-launch token                 │
└─────────────────────────────────────────────────────────────┘
```

## Crate dependency graph

```
studio-desktop ──▶ app-shell ──▶ render ──▶ render-shaders
      │               │            └─────▶ raster
      │               ├─▶ editor-core ─▶ layer-model ─▶ color
      │               └─▶ raster
      ├─▶ project-format ─▶ editor-core
      └─▶ telemetry

runtime-manager ─▶ ai-runtime ─▶ ai-contracts
                       (adapter: only crate that knows ComfyUI exists)

ui ─▶ editor-core, layer-model, tools
tools ─▶ editor-core, layer-model
adjustments ─▶ color
asset-store ─▶ raster
licensing / updater ─▶ ed25519 (standalone)
```

Key rule: **`ai-contracts` is the firewall.** The document model and UI depend
only on typed AI request/response data. `ai-runtime` is the sole translator to
a ComfyUI workflow graph — no graph details leak upward. This preserves the GPL
boundary (see `../LICENSES/COMFYUI_SOURCE_AND_NOTICES.md`).

## Layering rules

- `layer-model` and `color` are leaf domain crates: no rendering, no I/O.
- `editor-core` owns the document, commands, and history — no GPU, no disk.
- `render` owns all wgpu; nothing above it touches the GPU directly.
- `project-format` owns all persistence; the document never serializes itself
  from within `editor-core` beyond `serde` derives.
- The UI is a *view*: it never mutates the document except by emitting commands.

## Non-negotiable principles (enforced by structure)

| Principle | Where enforced |
| --- | --- |
| No required cloud | No network crate in the editor's dependency graph; AI is opt-in sidecar |
| Always editable | Every edit is an invertible `Command`; adjustments are parametric |
| Responsive at scale | Tile-first raster model + GPU LRU cache (`raster`, `render`) |
| AI enters as layers/masks/metadata | `ai-contracts::AiResult` = assets + masks + provenance |
| Editor separate from GPL | `ai-contracts` firewall + process-isolated sidecar |
| Native format authoritative | `.rstudio` package with mandatory `format_version` |
