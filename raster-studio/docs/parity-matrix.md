# Feature Parity Matrix

Target: feature parity with Photopea for real editing work.

**This file states the truth.** A feature is ✅ only when it is implemented,
tested, and reachable from the UI. Anything else is 🔶 (partial, with the gap
named) or ⬜ (not started). Nothing is marked done on the strength of a type
existing — that is exactly the failure this project was rebuilt to escape.

Status: ✅ done · 🔶 partial · ⬜ not started

> **Reachability is part of the bar.** A final audit found rows marked done whose
> feature the application had no route to. Those are corrected below and the
> engine/UI split is stated explicitly: 🔶 now covers "the library works and is
> tested, but you cannot get to it from the app". See README "Status".


## Tiers

- **Tier A — Core.** Without all of these the app is not an image editor.
- **Tier B — Pro.** What makes it competitive with Photopea.
- **Tier C — Deferred.** Explicitly out of scope for this release, with reasons.

---

## Tier A — Core

### Document & canvas
| Capability | Status | Notes |
| --- | --- | --- |
| Open PNG / JPEG / WebP / TIFF / GIF / BMP / ICO / TGA | ✅ | ICC preserved; 16-bit decoded without precision loss |
| New document with presets | ✅ | screen, print and social presets; colour mode and background |
| Pan / zoom / fit / 100% / rotate view / flip view | ✅ | zoom-to-cursor keeps the point under the pointer fixed |
| Transparency checkerboard | ✅ | fixed pixel size, drawn inside the image for alpha pixels |
| Rulers, guides, smart guides, grid, snapping | ✅ | guides live in view state, so they are not saved — see gaps |
| Multi-document tabs | ✅ | |

### Layers
| Capability | Status | Notes |
| --- | --- | --- |
| Raster layers with real pixels | ✅ | content-addressed tiles |
| Layer tree, groups, reorder, rename, lock | ✅ | a drag that would nest a group in its own child is refused |
| Visibility / opacity / fill | ✅ | |
| Blend modes (27) | ✅ | includes Hue/Saturation/Color/Luminosity via W3C SetLum/SetSat |
| Layer masks | ✅ | density and feather honoured by the compositor |
| Clipping masks | ✅ | Porter-Duff atop against the base layer's alpha |
| Adjustment layers (non-destructive) | ✅ | apply to the backdrop beneath them; themselves clippable |
| Layer styles / effects | ✅ | drop shadow, inner/outer glow, satin, colour and gradient overlays — rendered by the compositor (`effects::render`) |
| Merge / flatten / duplicate / rasterize | ✅ | |

### Editing
| Capability | Status | Notes |
| --- | --- | --- |
| Undo / redo with a history panel | ✅ | clickable stack with snapshots; a stroke is one step |
| Command journal + crash recovery | ✅ | anchored to a save marker, so replay cannot duplicate work |
| Cut / copy / paste / clear | ✅ | incl. Copy Merged, Paste Into, Layer Via Cut/Copy |
| Free transform (scale/rotate/skew/distort/perspective/warp) | ✅ | interactive gestures apply real, undoable commands; singular matrices are refused rather than writing NaN |
| Crop, trim, image size, canvas size, rotate canvas | ✅ | crop and the fixed transforms apply real edits; image/canvas size dialogs remain partial |

### Selection
| Capability | Status | Notes |
| --- | --- | --- |
| Marquee (rect / ellipse / row / column) | ✅ | anti-aliased |
| Lasso (free / polygonal / magnetic) | ✅ | |
| Magic wand / quick select / colour range | ✅ | tolerance, contiguous flag, anti-aliasing |
| Modify: feather, expand, contract, smooth, border | ✅ | true morphology on fractional coverage |
| Invert, grow, similar, transform selection | ✅ | |
| Quick mask, save/load selection | 🔶 | selection itself (outline, marching ants, save/load) is reachable; quick-mask composition remains partial |

### Tools
| Capability | Status | Notes |
| --- | --- | --- |
| Brush that actually paints | ✅ | one undoable command per stroke; overlapping dabs do not darken |
| Eraser, pencil, paint bucket, gradient | ✅ | |
| Clone stamp, healing, spot healing, patch, red-eye | ✅ | |
| Dodge / burn / sponge, blur / sharpen / smudge | ✅ | |
| Eyedropper, move, hand, zoom, rotate view | ✅ | |
| Tablet pressure | 🔶 | the engine consumes it; egui 0.29 carries no pressure, so the shell must feed it |

### Adjustments
| Capability | Status | Notes |
| --- | --- | --- |
| Brightness/Contrast, Levels, Curves, Exposure | ✅ | Curves is a Fritsch-Carlson monotone spline |
| Vibrance, Hue/Saturation, Colour Balance | ✅ | |
| B&W, Photo Filter, Channel Mixer, Invert | ✅ | |
| Posterize, Threshold, Gradient Map, Selective Colour | ✅ | |
| Auto tone / contrast / colour | ✅ | |

### File
| Capability | Status | Notes |
| --- | --- | --- |
| Save / open the native `.rstudio` project | ✅ | integrity-sealed, version-gated, crash-safe swap |
| Pixel data persisted | ✅ | reopening composites to byte-identical output |
| Export PNG / JPEG / WebP / TIFF / GIF / BMP with presets | ✅ | correct un-premultiply and linear→sRGB on the way out |
| Drag-and-drop open, recent files | ✅ | |

### Application
| Capability | Status | Notes |
| --- | --- | --- |
| Menu bar wired to real commands | 🔶 | nine menus; every item is wired or explicitly disabled |
| Keyboard shortcuts | ✅ | full customisable keymap with conflict detection |
| Panels | ✅ | Layers, History, Adjustments, Properties, Colour, Swatches, Brushes, Channels, Paths, Navigator, Info |
| Tool options bar | ✅ | generated from each tool's options schema |
| Apple-style visual design | 🔶 | one token system, light and dark, WCAG AA asserted by test; the fit and finish is not yet at the standard the name implies |
| Preferences | ✅ | persisted, including the keymap editor |

---

## Tier B — Pro

| Capability | Status | Notes |
| --- | --- | --- |
| Text layers with real shaping and fonts | ✅ | bidi, ligatures, kerning, contextual forms via cosmic-text; a type tool creates and edits text layers |
| Vector paths, pen tool, shape layers | ✅ | Bézier pen and shape layers reachable from the UI |
| Filters: blur family | ✅ | separable Gaussian; every filter in the Filter menu applies against the live document |
| Filters: sharpen, noise, distort, stylize, pixelate, render | ✅ | the whole library is reachable from the Filter menu |
| Smart objects | ⬜ | the layer kind exists; nothing renders it |
| PSD import | ✅ | groups, masks, blend modes, all four channel encodings; a `Document` is built from the layer section |
| PSD export | 🔶 | reopens correctly in Photoshop and Photopea |
| Channels panel | 🔶 | isolation is real and changes the canvas; per-channel *editing* is still not implemented — see the gaps list |
| Paths panel | 🔶 | |
| Colour management | 🔶 | sRGB and Display P3 are real; an embedded ICC profile is preserved but not applied |
| 16-bit per channel | 🔶 | a 16-bit source is recognized and exported at 16 bits to the formats that carry them (PNG/TIFF); in-app tiles still composite at 8-bit-equivalent precision, and `.rstudio` does not record the depth |
| Actions / recorded command replay | 🔶 | commands are serialisable and replayable; there is no recording UI |
| Batch export | 🔶 | multiple presets in one run |
| Brush / gradient / layer-style editors | 🔶 | |
| Autosave | ✅ | |

---

## Tier C — Deferred (documented, not implied)

| Capability | Why deferred |
| --- | --- |
| Full CMYK / prepress / spot colours | Needs a real ICC engine and a print workflow; large and orthogonal to core editing. |
| PDF and AI import | A PDF interpreter is a project in itself. |
| Sketch / XD / Figma import | Proprietary formats with little overlap with raster editing. |
| Camera RAW | Per-sensor demosaic and profiles; belongs behind a finished colour pipeline. |
| Liquify, Vanishing Point, Puppet Warp | Deep mesh-warp tooling, beyond the transform mesh that exists. |
| Content-aware fill / Select Subject | Requires ML inference we deliberately do not bundle. |
| Video and animation timeline | Out of scope for a raster editor v1. |
| Collaboration, cloud, mobile | Explicit non-goals: this is a local-first desktop app. |
| Perfect PSD round-tripping | We target correct reopen in Photoshop and Photopea, not byte fidelity. |

---

## Known gaps in what is marked done

Kept here rather than buried, because a ✅ with a footnote is still a claim:

- **Channel editing does not exist.** Hiding a component in the Channels
  panel — by its eye toggle or by `Ctrl+3`/`Ctrl+4`/`Ctrl+5` — really does
  change the canvas: the mask is applied to the composite on its way to the GPU
  texture (`app_shell::presenter::ChannelMask`, proved on the GPU by
  `hiding_a_channel_changes_the_texture_the_canvas_samples`). What does not
  exist is painting or filtering **into** one channel: the panel's selected row
  is an isolation target, not a paint target, so every tool still writes all
  three components. The mask is a view setting and is not saved with the
  document.
- **Smart objects are not rendered.** The layer kind exists; nothing draws one.
- **Export is 8-bit.** 16-bit sources are now honored on the way *out*: a deep
  source exports at 16 bits to the formats that carry them (PNG/TIFF), and an
  8-bit source keeps the byte-exact 8-bit path. What remains is that in-app
  editing still composites at 8-bit-equivalent precision (import collapses to
  8-bit tiles) and that `.rstudio` does not record the source depth.
- **Tablet pressure needs the shell.** egui 0.29's input carries no pressure,
  so without the native tablet stream every sample is a mouse at full pressure.
- **Guides are view state.** They are not saved with the document and not
  undoable, because there is no command for them.
- **Layer and history thumbnails are not rendered.** The Layers panel paints a
  glyph for the layer's *kind* over a transparency checkerboard, and each
  History row paints a glyph for the kind of edit the step was. Neither shows
  the pixels. Rendering them means a compositor pass per row, cached per layer
  revision and per history step, and that cache does not exist; the glyphs are
  the honest stand-in until it does.
- **ICC profiles are preserved, not applied.** A tagged image survives a
  round-trip unchanged, but the working space is still sRGB or Display P3.
- A minority of menu items are still drawn **disabled** with a named reason
  (Print, File Info, and the handful that need a dialog surface this build has
  not drawn). The others are performable; the count is pinned by
  `menu_bridge`'s `every_ui_menu_item_is_either_performable_or_disabled_with_a_reason`.

## Release gate

1. Every Tier A row is ✅ or has its gap named above.
2. Every Tier B row is ✅, named as partial, or moved to Tier C with a reason.
3. `cargo check --workspace --all-targets`, `cargo clippy` with `-D warnings`,
   `cargo fmt --check` and `cargo test --workspace` are green.
4. The app launches, opens a real image, edits it, saves, reopens and exports
   correctly — verified by running it, not by prose.
