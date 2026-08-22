# Raster Studio — Task List

Derived from `raster-studio/docs/PLAN.md`. Every task is scoped to **one crate**,
carries its own tests, and is executed by a **doer/reviewer pair**.

Status: ⬜ not started · 🔶 in progress · ✅ done (compiles, tested, reviewed)

Gate for every wave: `cargo check --workspace --all-targets` and
`cargo test --workspace` both green before the next wave starts.

---

## Wave 1 — Foundation: give the engine pixels, make the canvas visible

### W1.1 `crates/raster` — tile engine ⬜
- Make `Tile.data` private behind accessors so the cached BLAKE3 hash cannot go stale.
- Add `TileGrid` / `TileStore`: `TileCoord -> Tile`, image→tiles, tiles→image,
  visible-tile iteration, partial edge tiles with a valid sub-rect.
- Add a real mip-chain builder. Fix `downsample_rgba8_2x`: premultiply before
  averaging, average in **linear** light, round instead of truncate, and reject
  zero dimensions and short slices instead of panicking.
- Clamp `level_dimensions` shift to avoid overflow at `level >= 32`.
- Add `Rgba16` support through the tile path.
- Strip unused deps (`anyhow`, `glam`, `bytemuck`).

### W1.2 `crates/color` — real color management ⬜
- Add `ColorSpace`-driven dispatch (`to_linear` / `from_linear`); today every
  call site hardcodes sRGB, which is exactly what the crate exists to prevent.
- Add HSL, HSV, and CIELAB conversions (needed by adjustments and color pickers).
- Add a LUT path for the hot `powf` conversions.
- Handle negative and >1.0 inputs without producing NaN.
- Align the transparency epsilon with the shader so golden tests can't drift.

### W1.3 `crates/layer-model` — the real layer model ⬜
- **Fix `move_layer` cycle → infinite recursion.** Reject moving a group into
  its own descendant.
- **Fix `remove` orphaning group children.** Removing a group must recursively
  remove or re-parent its subtree; no unreachable layers may remain in the map.
- Reject duplicate ids in `push_root`; enforce "a layer has at most one parent".
- Expand `BlendMode` to the full 27-mode set with correct separable and
  non-separable (Hue/Saturation/Color/Luminosity) math.
- Add masks (`MaskId` resolution), clipping groups, and layer-effect state.
- Derive `PartialEq` so undo/redo tests can assert whole-state equality.
- Add a `LayerTree` serde round-trip test with nested groups.

### W1.4 `crates/editor-core` — commands that can edit pixels ⬜
- **Make `Transaction` atomic**: roll back applied members on failure, or
  validate up front. Today a failed transaction leaves the document mutated and
  *not* undoable.
- **Guard `TransformLayer` against singular matrices** (currently stores NaN into
  the undo entry); use the unused `CommandError::NotInvertible`.
- Add pixel commands: paint tile-deltas, fill, clear, mask edits — referenced by
  content hash, not embedded, per the existing design note.
- Add `Selection::Mask` as a real mask-backed selection with bounds.
- Add active-layer id, dirty flag, and file path to `Document`.
- Validate opacity and other parameters instead of passing NaN through.
- Fix redo to store the freshly computed inverse.
- Extend `LayerPatch` to cover the rest of `Layer`, including *clearing* a mask.

### W1.5 `crates/render` + `render-shaders` — make the canvas actually draw ⬜
- **Fix `Camera::clip_to_uv`**: it returns a degenerate affine, so V varies with
  screen X and the image renders as a horizontal smear.
- **Fix the egui pass clearing the canvas** (`LoadOp::Clear` → `Load`). This is
  why the window is black.
- Add the V-flip to `quad.wgsl` so the image is not vertically mirrored.
- Draw the transparency checkerboard *inside* the image for alpha pixels, at a
  fixed pixel size, in the correct (sRGB-aware) tone.
- Add an offscreen render target + readback so the GPU path is testable and
  export/golden-image tests are possible at all.
- Stop the unconditional redraw spin; honour egui's repaint scheduling.
- Fix `Composite`'s blend against a transparent backdrop (`composite.wgsl`
  currently makes a source vanish over transparency).
- Give the canvas mipmaps so zoomed-out 4K/8K images stop aliasing.

### W1.6 `crates/design` (new) — Apple-style design system ⬜
- Token set: color (light + dark), type scale, spacing grid, radii, shadows,
  motion curves, elevation.
- An egui theme built from the tokens, plus themed primitives (buttons, sliders,
  segmented controls, popovers, sheets, list rows, inspector fields).
- Contrast-ratio tests over the palette; light/dark parity test.

---

## Wave 2 — Compositing and image operations

### W2.1 `crates/compositor` (new) — the authoritative CPU compositor ⬜
- `(document, region, zoom) -> pixels`, tile-parallel via `rayon`.
- Correct group traversal, opacity, blend modes, layer masks, clipping masks,
  and adjustment layers applied non-destructively.
- Linear premultiplied working space; dirty-tile invalidation.
- Golden-image tests per blend mode against known-good references.

### W2.2 `crates/adjustments` — the full adjustment set ⬜
- Wire it to `layer_model::AdjustmentKind` (the crate currently has **zero
  dependents** and no dispatcher).
- Remove the clamps that make exposure lossy and defeat non-destructive editing.
- Add: Brightness/Contrast, Vibrance, Hue/Saturation (hue and lightness are
  missing entirely), Color Balance (missing entirely), Black & White, Photo
  Filter, Channel Mixer, Invert, Posterize, Threshold, Gradient Map,
  Selective Color, Auto Tone/Contrast/Color.
- Replace piecewise-linear `curve` with a monotone cubic spline; validate and
  sort control points instead of returning garbage on duplicate x.

### W2.3 `crates/filters` (new) ⬜
- Blur: Gaussian, box, motion, radial, lens, surface.
- Sharpen: unsharp mask, smart sharpen.
- Noise: add noise, despeckle, dust & scratches, median, reduce noise.
- Distort: pinch, polar, ripple, shear, spherize, twirl, wave, zigzag.
- Stylize: emboss, find edges, oil paint, solarize, wind, diffuse.
- Pixelate: mosaic, crystallize, pointillize, halftone.
- Render: clouds, fibers, lens flare, gradient.
- Other: high pass, offset, minimum, maximum, custom convolution.
- Separable kernels and tile-parallel execution; golden tests for each family.

### W2.4 `crates/selection` (new) ⬜
- Mask-backed selection with per-pixel coverage (not just rects).
- Marquee (rect / ellipse / row / column), lasso (free / polygonal / magnetic),
  magic wand, quick select, color range.
- Modify: feather, expand, contract, smooth, border.
- Grow / similar, invert, transform selection, save/load selection, quick mask.
- Marching-ants outline extraction for the UI overlay.

---

## Wave 3 — Tools

### W3.1 `crates/tools` — a brush engine that paints ⬜
- Stamp-based brush: hardness, spacing, flow, opacity, pressure, smoothing.
- **Emit a real paint command.** Today `on_pointer_up` emits nothing, so the
  brush cannot mark the canvas.
- Eraser, background/magic eraser, pencil, color replacement.
- Clone stamp, pattern stamp, healing, spot healing, patch, red-eye.
- Gradient (linear/radial/angle/reflected/diamond), paint bucket, pattern fill.
- Blur / sharpen / smudge, dodge / burn / sponge.
- Move, crop, slice, eyedropper, hand, zoom, rotate-view.
- Free transform: scale, rotate, skew, distort, perspective, warp, with handles.

### W3.2 `crates/vector` (new) ⬜
- Cubic Bézier path model, boolean ops, hit testing.
- Anti-aliased scanline fill (nonzero + even-odd) and stroking (caps, joins,
  miter limit, dashes).
- Pen tool, curvature pen, freeform pen; direct/path selection.
- Shape primitives: rectangle, rounded rectangle, ellipse, polygon, star, line,
  custom shape. Replace `ShapeLayer.path_svg` with a real path.

### W3.3 `crates/text-engine` ⬜
- Real shaping via `cosmic-text`; system font enumeration and loading.
- Editable text layers: point and paragraph text, alignment, leading, tracking,
  kerning, faux bold/italic, per-character styling.
- Glyph rasterization into the tile compositor; text-on-path later.
- Bridge the duplicate `TextRun` / `TextLayer` types.

---

## Wave 4 — Persistence and interop

### W4.1 `crates/project-format` ⬜
- **Fix the path traversal**: `manifest.document_path` is untrusted and joined
  directly, so an absolute path or `../` reads arbitrary files.
- **Fix the two-step rename** that can leave *no* project on disk after a crash,
  and stop swallowing a failed rollback.
- Persist tiles, assets, masks, and a composite preview — currently those
  directories are created and left empty, so **no pixel data is ever saved**.
- Gate `document.format_version` on load and implement real `migrate()`.
- Make the journal crash-tolerant (stop at the first torn line, keep the prefix)
  and anchor it to the saved snapshot so replay cannot duplicate work.
- Add a package integrity hash. Fsync directories, not just files.
- Unique temp/backup names so concurrent saves cannot stomp each other.

### W4.2 `crates/asset-store` ⬜
- Add the disk-backed layer under the existing in-memory CAS (it currently has
  zero disk code and zero dependents).
- Merge the blob and refcount maps so they cannot desync; return errors on
  unknown release instead of silently no-oping.
- Bounded cache with LRU eviction and inactive-tile compression.

### W4.3 `crates/psd` (new) — original PSD implementation ⬜
- Read: header, color modes, layers, masks, groups, blend modes, opacity, text,
  adjustment layers, layer effects, RLE and ZIP channel decompression.
- Write: layered PSD that Photoshop and Photopea both reopen correctly.
- Round-trip tests against fixtures generated by our own writer, plus known-good
  reference files.

### W4.4 `crates/raster::codec` — import/export ⬜
- Preserve ICC profiles on decode (currently dropped) and 16-bit precision.
- Export pipeline: un-premultiply and linear→sRGB before encoding (missing today,
  so composited output would export wrong).
- Formats: PNG, JPEG, WebP, TIFF, GIF, BMP, ICO, TGA, SVG export, PDF export.
- Export presets: format, scale, quality, metadata, batch.
- Validate JPEG quality range; test the WebP path (currently untested).

---

## Wave 5 — The application (Apple-style UI/UX)

### W5.1 `crates/ui` — workspace shell ⬜
- Menu bar: File, Edit, Image, Layer, Select, Filter, View, Window, Help —
  every item wired to a real command or explicitly disabled.
- Tool palette, tool options bar (`show_tool_options` exists but draws nothing).
- Panels: Layers (with groups, masks, effects, blend/opacity, drag-reorder,
  thumbnails), History (with a real clickable stack, not two labels), Channels,
  Paths, Adjustments, Properties, Color, Swatches, Brushes, Character,
  Paragraph, Navigator, Info.
- Dockable, resizable, collapsible panel layout with saved workspaces.

### W5.2 `crates/ui` — canvas interaction ⬜
- Rulers, guides, smart guides, grid, snapping, pixel grid.
- Selection overlay (marching ants), transform handles, crop overlay.
- Zoom controls, fit/fill/100%, rotate view, multi-document tabs.
- Nothing may pan the canvas while the pointer is over a panel.

### W5.3 `crates/app-shell` — real application behavior ⬜
- Wire the **7 of 11 shortcut actions that currently do nothing** (undo, redo,
  save, open, export, new layer, delete layer).
- Full shortcut map with a customizable, conflict-checked binding table.
- Native file dialogs, drag-and-drop open, recent files, multi-window.
- Autosave + crash recovery through the journal.
- Replace `panic = "abort"` + `.expect()` GPU init with a real error path that
  shows the user a message instead of vanishing.
- Fix the one-frame command lag (commands are applied *after* tessellation).

### W5.4 Dialogs ⬜
- New document (presets), image size, canvas size, export as, preferences,
  color picker, gradient editor, brush editor, layer style editor.

---

## Wave 6 — Ship

### W6.1 Quality gates ⬜
- CI (`.github/workflows`): check, test, clippy, rustfmt, audit, on a matrix.
- Golden-image regression suite; performance gates expressed as **ratios**, never
  absolute wall-clock thresholds (those measure the CI hardware, not the code).
- Fuzz the PSD and `.rstudio` parsers — both read untrusted input.

### W6.2 Product ⬜
- README with real screenshots of the working app.
- Windows installer; app icon; version stamping.
- Honest `docs/parity-matrix.md`: every feature marked done is reachable and
  tested; everything deferred says so and says why.

---

## Cross-cutting hygiene ⬜
- Delete `crates/ai-runtime`, `crates/ai-contracts`, `apps/runtime-manager`,
  `workflows/`, `runtime/`, and the ComfyUI licence notices.
- Strip the unused dependencies found in the audit: `render` →
  {`thiserror`, `raw-window-handle`, `tracing`, `raster`}, `ui` → `tools`,
  `studio-desktop` → {`editor-core`, `project-format`}, `layer-model` → `color`,
  `editor-core` → {`anyhow`, `uuid`}, `adjustments` → {`serde`, `color`}.
- Rewrite `README.md`, `docs/architecture.md`, `docs/render-pipeline.md`,
  `docs/file-format.md`, and `docs/threat-model.md` to describe the system that
  exists. The threat model currently claims six mitigations that are not
  implemented.
- Bound history growth (`with_limit(0)` is unbounded and every entry clones a
  whole layer); bound the undone stack too.
