# Render Pipeline

**The CPU composites. The GPU presents.** Nothing composites twice.

```
editor_core::Document                      layer tree + tile *hashes*
        │
        │   compositor::TileCompositor::composite_region(doc, source, region, level, opts)
        ▼
compositor::Canvas                         linear premultiplied f32 RGBA
        │
        │   Canvas::to_rgba8(&doc.meta.color_space)
        ▼
straight-alpha RGBA8                       app_shell::presenter::ChannelMask applied here
        │
        │   queue.write_texture(...) — whole canvas, or one rect per dirty tile
        ▼
one document-sized GPU texture (Rgba8UnormSrgb, full mip chain)
        │
        │   render::Canvas + render_shaders::QUAD_WGSL, camera affine as a uniform
        ▼
surface view  ──▶  egui chrome pass (LoadOp::Load)  ──▶  present
```

Everything above the third arrow is ordinary Rust over buffers. That is the
point: `cargo test -p compositor` exercises the real pixel pipeline with no
window and no adapter.

## The CPU compositor (`crates/compositor`)

### Working space

Compositing happens in **linear, premultiplied `f32`**. Stored pixels are
straight-alpha 8-bit RGBA in the document's `color::ColorSpace`; they are
decoded with `color::to_linear` and premultiplied at the moment they are read,
and results leave through `Canvas::to_rgba8`, which unpremultiplies, encodes
with `color::from_linear`, and quantizes. That is the only place in the crate
that produces gamma-encoded values.

Two documented exceptions, both because the operation is *defined* on encoded
values: `BlendSpace::Encoded` (opt-in, for reproducing another editor's look)
and four of the five adjustment kinds evaluated in `compositor::adjust`.

Premultiplication is not cosmetic. Bilinear resampling of a transformed layer
averages neighbouring samples, and averaging *straight* colour across a
transparent edge drags a transparent pixel's meaningless RGB into the result as
a dark halo.

### The traversal (`compositor::composite`)

Siblings are visited bottom-up and groups recurse depth-first, so "the backdrop"
is a real buffer that a `Multiply` or an adjustment layer can read.

- An **isolated group** composites its children into its own transparent buffer
  and blends that down once with the group's opacity and mode. Applying the
  group's opacity per child instead double-applies it wherever two children
  overlap.
- A **pass-through group** blends children directly against what is under the
  group, and is honoured only when the group's blend mode is `Normal`, its
  transform is the identity, and nothing clips to it — each of those three needs
  a separate buffer to act on, which is precisely what pass-through is not.
- A **clipping group** — a run of `ClipToBelow` layers plus the base beneath
  them — renders the base's own content, composites each clipped layer *atop*
  it (Porter-Duff `atop`, so a clipped layer can recolour the base but never
  extend it), then blends the finished buffer down with the **base's** blending
  options.
- An **adjustment layer** contributes no pixels. It rewrites the accumulator
  beneath it, weighted by opacity, fill opacity and its mask, and leaves alpha
  alone.

### Region independence

Every operation is either pointwise or reads a bounded neighbourhood of
**stored** data (a mask's feather, a transformed layer's pre-image). Nothing
reads a neighbouring pixel of an intermediate result.

That is what makes compositing a region equal to the same sub-rect of
compositing everything — and therefore what lets each tile go to a different
`rayon` worker with no seam, and lets the cache below be correct.

### Bounded allocation

Three ceilings, none of which changes a pixel:

| Bound | Value | What it does |
| --- | --- | --- |
| `Canvas::MAX_CANVAS_PIXELS` | `1 << 26` | Refuses a request for a buffer larger than can be held, with `CompositeError::RegionTooLarge`, instead of attempting an allocation that would abort the process. |
| `MAX_PREIMAGE_PIXELS` | `1 << 22` | A heavily minified layer's pre-image is huge. It is first intersected with the layer's stored content bounds; if it is still too large the *destination* is split and the halves composited separately — exact, by region independence, and the same total work. |
| `TAP_WINDOW` | `8` | Splitting stops at one destination pixel, where bilinear sampling reads the four samples around one point. An 8-pixel window there holds every sample that can be read, so clamping to it is exact too. |

The one thing genuinely given up is a mask feather so large its sampled buffer
could not be allocated: the blur radii are scaled down to what fits rather than
the frame being failed. It takes a feather of thousands of pixels to reach, and
the clamp is folded into the cache key so the answer stays region-independent.

## The tile cache and dirty-tile invalidation

Two mechanisms, and it matters which one is load-bearing.

### `compositor::cache::TileCompositor` — correctness

Every cached tile is keyed by a **hash of the inputs that produced it**: the
document's geometry and colour space, the compositing options, every layer
property that reaches the maths, and the content hash of each layer and mask
tile the traversal reads at that coordinate. A cached tile is reused only while
that key still matches, so a stale tile is a contradiction rather than a race —
if anything it depended on changed, the key changed.

The cache holds `DEFAULT_CACHE_TILES` (128) composited tiles. At `TILE_SIZE`
256 and RGBA `f32` one tile is 1 MiB, so that is a 128 MiB ceiling. When it
grows past capacity it is trimmed to the tiles of the most recent request — a
working-set policy, not an LRU, because the access pattern *is* a working set
(the visible viewport) and per-entry LRU bookkeeping would cost more than the
hits it saved. `CacheStats` exposes hits and misses so "a small edit only
recomputed the tiles that changed" is an assertion rather than a claim.

### `app_shell::dirty::DirtyTiles` — an optimisation over that baseline

`touched_by(&Command)` records which tiles an accepted command invalidated, and
the presenter re-uploads only those. It is matched exhaustively with no
wildcard, so a new `Command` variant has to state its reach:

| Command | Reach |
| --- | --- |
| `PaintTiles`, `FillRegion`, `ClearRegion` | Exactly the level-0 coordinates in the tile delta. A delta touching any mip level other than 0 marks everything, because level 1 is not a rectangle of the level-0 texture. |
| `Transaction` | The union of its members; one member that marks everything wins. |
| `CreateLayer`, `DeleteLayer`, `RestoreLayers`, `MoveLayer`, `SetLayerProperties`, `TransformLayer` | Everything. These change how existing pixels composite rather than which pixels exist, and there is no cheaper honest answer without walking the layer's tile map. |

Because the compositor's key already guarantees freshness, a coordinate this
module fails to mention costs a key computation, never a wrong pixel. Being
wrong here is expensive, not incorrect.

## What the GPU actually does

`crates/render` owns every pipeline; `app-shell` owns the frame. Between them,
per redraw:

1. **`CanvasPresenter::sync`** brings one document-sized `Rgba8UnormSrgb`
   texture into step with the document.
   - No texture yet, a canvas resize, or a switch to a different document tab →
     composite the whole canvas and create the texture. (The document id is half
     that test: one presenter serves the window, so two tabs of identical size
     would otherwise show the previous document's pixels.)
   - Otherwise → one `write_texture` per dirty tile, clipped to the canvas by
     `tile_upload_rect` so a layer's out-of-canvas tiles are never written past
     the texture.
   - A channel-visibility change dirties no tile — the document did not move —
     so the presenter marks everything and re-uploads once.
   - After any level-0 write, `MipGenerator` regenerates the chain with one
     render pass per level (`mipmap.wgsl`). Skipping it makes an edit invisible
     until the user zooms in.
2. **`render::Canvas::render`** draws that texture as a fullscreen quad
   (`quad.wgsl`) through the camera affine, over a transparency checkerboard
   that shows both outside the image bounds and behind transparent pixels
   inside them. With no document open the shell clears to the theme's canvas
   backdrop instead.
3. **The egui pass** draws the chrome into the same view with
   `LoadOp::Load`, so it composites over the canvas rather than erasing it.
4. Present.

### Shaders (`crates/render-shaders`)

| Shader | Purpose | Used by the app |
| --- | --- | --- |
| `quad.wgsl` | Fullscreen textured quad, camera affine, transparency checkerboard | Yes — this is the canvas |
| `mipmap.wgsl` | Bilinear mip downsampler, one draw per level, premultiplying before it averages and un-premultiplying after | Yes — after every texture write |
| `composite.wgsl` | One layer-over-destination blend, mode selected by index | **No.** `render::CompositePass` is constructed only in `crates/render/tests/gpu.rs`. It is a spike, not a stage of the pipeline: the compositor is on the CPU. |

Conventions all three obey: clip-space `y = +1` is the top of the target and
maps to `v = 0`; shading is in linear light with every literal colour stored
pre-linearized; source textures and all their mip levels carry **straight**
alpha (`composite.wgsl` is the premultiplied exception).

### The camera

`render::Camera` maps clip space `(-1..1)` to source UV `(0..1)` through an
affine uploaded as a uniform, so pan and zoom change uniforms only, never vertex
data. Zoom is clamped to `MIN_ZOOM` (0.01) … `MAX_ZOOM` (64.0) — published
constants, so a Navigator slider or a typed zoom level clamps to the same range
a wheel gesture does. `zoom_at` keeps the anchor point stationary under the
cursor.

## Colour

```
source decode          raster::codec, with an ICC profile carried, not applied
  → to linear          color::to_linear          (sRGB / Linear sRGB / Display P3)
  → composite          linear premultiplied f32  (compositor)
  → to display         color::from_linear        (Canvas::to_rgba8)
  → present            hardware sRGB encode on an *-Srgb target,
                       or the shader's own OETF on a plain unorm target
```

`ColorSpace::IccProfile` exists as a variant and is **not implemented**: no ICC
engine is linked, `is_transform_supported` returns `false` for it, and the
infallible entry points fall back to identity. A tagged image round-trips with
its profile intact; the working space is still sRGB or Display P3.

## Rendering without a window

`GpuContext::headless` acquires a device with no surface, and `OffscreenTarget`
renders into a texture and pulls the result back as tightly packed RGBA8. That
is the seam the GPU tests use — and each of them prints `SKIP` and returns when
no adapter can be created, so a headless CI runner stays green. On Windows the
WARP adapter means they normally do run.

Nothing else needs it: thumbnails and export do not go through the GPU. The
`.rstudio` composite preview is rendered by `compositor::composite_region` in
horizontal strips (`project_format::preview`), and export flattens through
`raster::export`.

## Not done, and not approximated

Each of these changes what a document looks like, and none is silently faked:

- **Layer effects do not render.** `layer_model::LayerEffects` — drop shadow,
  stroke, glow, overlays, bevel, satin — is parsed, edited and persisted, and
  ignored by the compositor. Because of that, `fill_opacity` is currently
  multiplied straight into the layer's opacity; once effects exist it must stop
  applying to them.
- **Text, shape and smart-object layers have no rasterizer in the compositor,**
  so they contribute nothing and cannot serve as a clipping base.
- **A vector layer mask with no rasterized tiles is ignored** rather than read
  as zero coverage — which would hide the layer completely. Wrong in the
  direction that keeps the user's content on screen. Tiles stored under the
  mask's id are used whatever the kind says, so a future rasterizer needs no
  change here.
- **Blend mode on an adjustment layer is ignored**: an adjustment is a weighted
  replacement of the backdrop's colour, never an `over`.
- **Mip tiles are read, not built.** Asking for level *n* reads the tiles stored
  at level *n*; the compositor does not downsample level 0 on the fly. Building
  the chain is `raster::MipChain`'s job. The GPU texture's mip chain is a
  separate thing and *is* generated.
- **Selection does not mask the composite.** A selection scopes edits, not what
  the document looks like.
- **No GPU acceleration for hot paths.** Large blurs and big composites run on
  the CPU. When that changes it goes behind the same trait, as an optimisation —
  never as a second source of truth.
