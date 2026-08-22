# Feature Parity Matrix

Status legend: ✅ done · 🟡 scaffolded (types/stubs in place) · ⬜ not started

## Phase 0 — engine proof

| Capability | Goal | Status | Where |
| --- | --- | --- | --- |
| Native window + GPU canvas | 60 FPS pan/zoom on 4K | 🟡 | `app-shell`, `render::Canvas` |
| Image import/export | PNG + JPEG first | 🟡 | `raster::codec` (decode ✅, export API ✅) |
| Tile cache | Visible-tile render, mip selection | 🟡 | `raster::{tile,mipmap}` |
| Layers | Raster, visibility, opacity, reorder | ✅ | `layer-model`, `editor-core` |
| History | Undo/redo + journal recovery | ✅ | `editor-core::history`, `project-format::journal` |
| Project format | Save/load layered project | ✅ | `project-format` |

## Phase 1 — usable editor

| Capability | Goal | Status | Where |
| --- | --- | --- | --- |
| Groups & masks | Groups, raster masks, clipping | 🟡 | `layer-model` (groups ✅, mask ids ✅) |
| Blend modes | Common creative modes | 🟡 | `layer-model::BlendMode` + `composite.wgsl` |
| Core tools | Brush, eraser, crop, transform, selections | 🟡 | `tools` (brush ✅), `editor-core::selection` |
| Adjustments | Levels, curves, exposure, HSL, color balance | 🟡 | `adjustments` (CPU refs ✅) |
| Export | PNG/JPEG/WebP, scale, quality, batch presets | 🟡 | `raster::codec::ExportFormat` |
| Input | Tablet pressure | 🟡 | `tools::PointerEvent.pressure` |

## Phase 2 — commercial differentiator

| Capability | Goal | Status | Where |
| --- | --- | --- | --- |
| ComfyUI integration | Generate, inpaint, upscale, bg replace | 🟡 | `ai-contracts`, `ai-runtime` (transport stubbed) |
| AI provenance | Prompt/seed/model/workflow in project | ✅ (types) | `ai-contracts::GenerationProvenance` |
| Local runtime UX | Install/repair/status/model profiles | 🟡 | `runtime-manager`, `ai-runtime::RuntimeStatus` |
| Asset workflows | Templates, linked images, collection | 🟡 | `asset-store`, `workflows/` |
| Licensing | Offline entitlement + trial | ✅ | `licensing` |
| Reliability | Crash recovery, autosave, diagnostics | 🟡 | `project-format`, `telemetry` |

## Phase 3 — professional parity foundations

| Capability | Goal | Status | Where |
| --- | --- | --- | --- |
| Color management | ICC, display transform, soft proofing | 🟡 | `color` (pipeline shape ✅) |
| 16-bit workflows | 16-bit channels before 32-bit float | 🟡 | `raster::PixelFormat::Rgba16` |
| Text | Editable text layers + fonts | ⬜ | `text-engine` (postponed placeholder) |
| Vector | Shapes, paths, pen | ⬜ | `layer-model::ShapeLayer` |
| Smart objects | Embedded/linked, non-destructive | 🟡 | `layer-model::SmartObjectLayer` |
| Advanced selection | Subject/edge, refine edge/matte | ⬜ | `editor-core::Selection::Mask` |
| File support | TIFF/PSD import, scoped export | 🟡 | `raster::codec` (TIFF feature on) |

## Phase 4 — deeper Photoshop parity

| Capability | Goal | Status |
| --- | --- | --- |
| Retouching | Clone, heal, patch, content-aware-like | ⬜ |
| Filters | Smart filters, parametric effects | ⬜ |
| Actions/automation | Recorded commands + scripts | 🟡 (commands are already serializable/replayable) |
| Batch workflows | Headless/export pipeline | ⬜ |
| Print/prepress | CMYK, spot colors, print profiles, PDF | ⬜ |
| Collaboration | Optional, later | ⬜ (explicit non-goal for v1) |

## Milestone decision gates

| Gate | Evidence needed | Decision |
| --- | --- | --- |
| Canvas | Smooth 4K pan/zoom + correct render | Continue renderer investment |
| Non-destructive | Layers/masks/adjustments + save/reload stable | Begin customer validation |
| Workflow | One target workflow saves measurable time | Define first paid niche |
| AI | Curated local workflow works across supported GPUs | Bundle optional runtime |
| Commercial | Installer, license, recovery, diagnostics work | Begin private beta |
| Parity | Core matrix shows sustained adoption | Expand text/vector/color/PSD |

## Explicit non-goals for v1

Perfect PSD round-tripping · full CMYK/prepress · full Adobe-parity text ·
arbitrary custom ComfyUI nodes + auto Git install · cloud accounts / hosted
inference / collaboration / mobile · "replace every Photoshop feature".
