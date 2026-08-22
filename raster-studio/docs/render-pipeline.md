# Render Pipeline

GPU-native compositing built on **tiles**, not full-canvas textures.

## Tile-first strategy

- Never model an image as a permanently resident full-canvas GPU texture.
- Fixed-size tiles: `raster::TILE_SIZE` (256 to start; benchmark 512 for
  high-resolution workloads — it's a one-line change).
- Build a mip chain for every raster source and generated composite
  (`raster::mipmap`).
- Keep viewport-visible tiles + a prefetch border in a GPU LRU cache.
- Compress inactive CPU tiles and evict under memory pressure.
- Render only tiles intersecting the viewport (+ small border).
- On edit, invalidate only affected tiles **and** dependent composite tiles.

## Render graph

```
Document layer tree
  → dependency graph construction
  → visible-tile selection by zoom + viewport   (mip level from Camera.zoom)
  → source tile upload / cache lookup
  → masks + adjustment passes
  → blend / composite pass                       (composite.wgsl)
  → display transform pass                       (color management)
  → UI overlay pass                              (selection ants, handles)
  → swapchain present
```

Phase 0 implements a simplified path: one source texture → `quad.wgsl` with a
pan/zoom camera → present. The tile cache and per-layer blend passes layer on
top without changing `GpuContext`/shader plumbing.

## Shaders (`render-shaders`)

| Shader | Purpose | Status |
| --- | --- | --- |
| `quad.wgsl` | Fullscreen textured quad + camera + checkerboard | Phase 0 ✅ |
| `composite.wgsl` | Per-layer blend (Normal/Multiply/Screen/Overlay/Darken/Lighten) | Scaffolded |

Blend indices in `composite.wgsl` **must** match
`layer_model::BlendMode::shader_index()`. The CPU reference
`BlendMode::blend_channel()` is the ground truth for golden-image tests.

## Color pipeline

```
source decode
  → source ICC → linear working RGB      (color::srgb_to_linear)
  → linear-premultiplied compositing     (color::premultiply)
  → display ICC / monitor transform
  → presentation                         (color::linear_to_srgb)
```

v1 exposes sRGB only, but color-space metadata rides on every source and
document (`color::ColorSpace`) so no sRGB assumption is baked into layer/shader
APIs. Phase 3 swaps the placeholder transforms for a real ICC engine.

## Camera

`render::Camera` maps clip-space `(-1..1)` to source UV `(0..1)` via an affine
uploaded as a uniform — pan/zoom change uniforms only, never vertex data.
Zoom-toward-cursor keeps the anchor point stationary (unit-tested).

## Performance targets (Phase 0 gate)

- 60 FPS pan/zoom on a 4K image.
- Tracked later: frame time on 4K/8K/multi-layer, GPU cache hit rate, brush
  latency + dirty-tile count, flatten/export time. See `docs/parity-matrix.md`.
