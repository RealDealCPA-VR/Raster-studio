# Feature Parity Matrix

Target: feature parity with Photopea for real editing work.

**This file states the truth.** A feature is ✅ only when it is implemented,
tested, reachable from the UI, and survives a save/reload round-trip. Anything
else is 🔶 (partial, with the gap named) or ⬜ (not started). Nothing is ever
marked done on the strength of a type existing.

Status: ✅ done · 🔶 partial · ⬜ not started

## Tiers

- **Tier A — Core.** Without all of these the app is not an image editor.
- **Tier B — Pro.** What makes it competitive with Photopea.
- **Tier C — Deferred.** Explicitly out of scope for this release, with reasons.
  Never implied to work.

---

## Tier A — Core

### Document & canvas
| Capability | Status | Notes |
| --- | --- | --- |
| Open PNG / JPEG / WebP / TIFF / BMP / GIF | 🔶 | decode works; ICC and 16-bit are dropped |
| New document with presets | ⬜ | |
| Pan / zoom / fit / 100% / rotate view | 🔶 | camera math exists; canvas renders black |
| Transparency checkerboard | 🔶 | drawn only outside the image, and stretches |
| Rulers, guides, smart guides, grid, snapping | ⬜ | |
| Multi-document tabs | ⬜ | |

### Layers
| Capability | Status | Notes |
| --- | --- | --- |
| Raster layers with real pixels | ⬜ | `RasterLayer` references nothing; no tile store |
| Layer tree, groups, reorder, rename, lock | 🔶 | tree works; `remove` orphans children, `move` can cycle |
| Visibility / opacity / fill | 🔶 | model only; never composited |
| Blend modes (27) | ⬜ | 6 exist, none of the non-separable four |
| Layer masks | ⬜ | `MaskId` is never constructed |
| Clipping masks | ⬜ | `ClipToBelow` honoured by nothing |
| Adjustment layers (non-destructive) | ⬜ | enum exists; no evaluator |
| Layer styles / effects | ⬜ | |
| Merge / flatten / duplicate / rasterize | ⬜ | |

### Editing
| Capability | Status | Notes |
| --- | --- | --- |
| Undo / redo with a history panel | 🔶 | engine is solid; panel is two text labels |
| Command journal + crash recovery | 🔶 | journal exists but is never written at runtime |
| Cut / copy / paste / clear | ⬜ | |
| Free transform (scale/rotate/skew/distort/perspective) | ⬜ | |
| Crop, trim, image size, canvas size, rotate canvas | ⬜ | |

### Selection
| Capability | Status | Notes |
| --- | --- | --- |
| Marquee (rect / ellipse / row / column) | ⬜ | `Selection::Rect` is data only |
| Lasso (free / polygonal / magnetic) | ⬜ | |
| Magic wand / quick select / color range | ⬜ | |
| Modify: feather, expand, contract, smooth, border | ⬜ | |
| Invert, grow, similar, transform selection | ⬜ | |
| Quick mask, save/load selection | ⬜ | |

### Tools
| Capability | Status | Notes |
| --- | --- | --- |
| Brush that actually paints | ⬜ | captures a stroke, emits no command |
| Eraser, pencil, paint bucket, gradient | ⬜ | |
| Clone stamp, healing, spot healing | ⬜ | |
| Dodge / burn / sponge, blur / sharpen / smudge | ⬜ | |
| Eyedropper, move, hand, zoom | ⬜ | |
| Tablet pressure | 🔶 | field exists on `PointerEvent`, unused |

### Adjustments
| Capability | Status | Notes |
| --- | --- | --- |
| Brightness/Contrast, Levels, Curves, Exposure | 🔶 | 3 CPU functions, orphaned; curve is linear not spline |
| Vibrance, Hue/Saturation, Color Balance | ⬜ | hue, lightness and colour balance absent entirely |
| B&W, Photo Filter, Channel Mixer, Invert | ⬜ | |
| Posterize, Threshold, Gradient Map, Selective Color | ⬜ | |
| Auto tone / contrast / color | ⬜ | |

### File
| Capability | Status | Notes |
| --- | --- | --- |
| Save / open the native `.rstudio` project | 🔶 | code works but the app never calls it |
| Pixel data persisted | ⬜ | `tiles/` and `assets/` are created empty |
| Export PNG / JPEG / WebP with presets | 🔶 | encoder exists; no colour pipeline, no UI |
| Drag-and-drop open, recent files | ⬜ | |

### Application
| Capability | Status | Notes |
| --- | --- | --- |
| Menu bar wired to real commands | ⬜ | no menu bar exists |
| Keyboard shortcuts | 🔶 | 4 of 11 actions do anything |
| Panels: Layers, History, Properties, Color, Swatches | 🔶 | Layers and History are minimal stubs |
| Tool options bar | ⬜ | flag exists, draws nothing |
| Apple-style visual design | ⬜ | default egui theme |
| Preferences | ⬜ | |

---

## Tier B — Pro

| Capability | Status | Notes |
| --- | --- | --- |
| Text layers with real shaping and fonts | ⬜ | `text-engine::is_available()` returns false |
| Vector paths, pen tool, shape layers | ⬜ | `ShapeLayer` is a placeholder SVG string |
| Filters: blur family | ⬜ | |
| Filters: sharpen, noise, distort, stylize, pixelate, render | ⬜ | |
| Smart objects | ⬜ | |
| PSD import | ⬜ | |
| PSD export | ⬜ | |
| Channels panel, per-channel editing | ⬜ | |
| Paths panel | ⬜ | |
| Colour management (ICC, display transform) | ⬜ | enum with 4 variants and no transforms |
| 16-bit per channel | ⬜ | `Rgba16` declared, nothing produces it |
| Actions / recorded command replay | 🔶 | commands are serialisable; no UI |
| Batch export | ⬜ | |
| Brush editor, gradient editor, layer style editor | ⬜ | |
| Autosave | ⬜ | |

---

## Tier C — Deferred (documented, not implied)

| Capability | Why deferred |
| --- | --- |
| Full CMYK / prepress / spot colours | Needs a real ICC engine and print workflow; large and orthogonal to core editing. |
| PDF and AI import | A full PDF interpreter is a project in itself. |
| Sketch / XD / Figma import | Proprietary formats with low overlap with raster editing. |
| Camera RAW | Per-sensor demosaic and profiles; belongs behind a stable colour pipeline. |
| Liquify, Vanishing Point, Puppet Warp | Deep mesh-warp tooling; after transform and vector land. |
| Content-aware fill / Select Subject | Requires ML inference we deliberately do not bundle. |
| Video and animation timeline | Out of scope for a raster editor v1. |
| Collaboration, cloud, mobile | Explicit non-goals: this is a local-first desktop app. |
| Perfect PSD round-tripping | We target correct reopen in Photoshop/Photopea, not byte fidelity. |

---

## Release gate

The release is ready when:

1. Every Tier A row is ✅.
2. Every Tier B row is ✅ or has moved to Tier C with a stated reason.
3. `cargo check --workspace --all-targets`, `cargo clippy`, and
   `cargo test --workspace` are green, including golden-image tests.
4. The app launches, opens a real image, edits it, saves, reopens, and exports
   correctly — verified by screenshot, not by prose.
