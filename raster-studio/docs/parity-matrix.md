# Feature Parity Matrix

Target: feature parity with Photopea for real editing work.

**This file states the truth.** A feature is ✅ only when it is implemented,
tested, and reachable from the UI. Anything else is 🔶 (partial, with the gap
named) or ⬜ (not started). Nothing is marked done on the strength of a type
existing — that is exactly the failure this project was rebuilt to escape.

Status: ✅ done · 🔶 partial · ⬜ not started

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
| Layer styles / effects | 🔶 | parametric data, editor dialog and persistence exist; the compositor does not yet render them |
| Merge / flatten / duplicate / rasterize | ✅ | |

### Editing
| Capability | Status | Notes |
| --- | --- | --- |
| Undo / redo with a history panel | ✅ | clickable stack with snapshots; a stroke is one step |
| Command journal + crash recovery | ✅ | anchored to a save marker, so replay cannot duplicate work |
| Cut / copy / paste / clear | ✅ | |
| Free transform (scale/rotate/skew/distort/perspective/warp) | ✅ | singular matrices are refused rather than writing NaN |
| Crop, trim, image size, canvas size, rotate canvas | ✅ | |

### Selection
| Capability | Status | Notes |
| --- | --- | --- |
| Marquee (rect / ellipse / row / column) | ✅ | anti-aliased |
| Lasso (free / polygonal / magnetic) | ✅ | |
| Magic wand / quick select / colour range | ✅ | tolerance, contiguous flag, anti-aliasing |
| Modify: feather, expand, contract, smooth, border | ✅ | true morphology on fractional coverage |
| Invert, grow, similar, transform selection | ✅ | |
| Quick mask, save/load selection | ✅ | |

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
| Menu bar wired to real commands | ✅ | nine menus; every item is wired or explicitly disabled |
| Keyboard shortcuts | ✅ | full customisable keymap with conflict detection |
| Panels | ✅ | Layers, History, Adjustments, Properties, Colour, Swatches, Brushes, Channels, Paths, Navigator, Info |
| Tool options bar | ✅ | generated from each tool's options schema |
| Apple-style visual design | 🔶 | one token system, light and dark, WCAG AA asserted by test; the fit and finish is not yet at the standard the name implies |
| Preferences | ✅ | persisted, including the keymap editor |

---

## Tier B — Pro

| Capability | Status | Notes |
| --- | --- | --- |
| Text layers with real shaping and fonts | ✅ | bidi, ligatures, kerning, contextual forms via cosmic-text |
| Vector paths, pen tool, shape layers | ✅ | |
| Filters: blur family | ✅ | separable Gaussian; a blur of a constant image returns that constant |
| Filters: sharpen, noise, distort, stylize, pixelate, render | ✅ | |
| Smart objects | ⬜ | the layer kind exists; nothing renders it |
| PSD import | ✅ | groups, masks, blend modes, all four channel encodings |
| PSD export | ✅ | reopens correctly in Photoshop and Photopea |
| Channels panel | 🔶 | isolation is real and changes the canvas; per-channel *editing* does not exist |
| Paths panel | ✅ | |
| Colour management | 🔶 | sRGB and Display P3 are real; an embedded ICC profile is preserved but not applied |
| 16-bit per channel | 🔶 | decoded and stored without loss; the compositor works in f32 and exports 8-bit |
| Actions / recorded command replay | 🔶 | commands are serialisable and replayable; there is no recording UI |
| Batch export | ✅ | multiple presets in one run |
| Brush / gradient / layer-style editors | ✅ | |
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

- **Layer effects do not render.** The data model, the editor and persistence
  are complete; the compositor ignores them. A styled layer looks unstyled.
- **Guides are view state.** They are not saved with the document and not
  undoable, because there is no command for them.
- **ICC profiles are preserved, not applied.** A tagged image survives a
  round-trip unchanged, but the working space is still sRGB or Display P3.
- **Export is 8-bit.** 16-bit sources are decoded and composited without loss
  and then written out at 8 bits per channel.
- **Tablet pressure needs the shell.** egui 0.29's input carries no pressure, so
  without the native tablet stream every sample is a mouse at full pressure.
- **The visual design is not finished.** The token system, theming and contrast
  gates are real; the craft of the layout is not yet at the level the phrase
  "Apple-style" sets as the bar.
- **Layer and history thumbnails are not rendered.** The Layers panel paints a
  glyph for the layer's *kind* over a transparency checkerboard, and each
  History row paints a glyph for the kind of edit the step was. Neither shows
  the pixels. Rendering them means a compositor pass per row, cached per layer
  revision and per history step, and that cache does not exist; the glyphs are
  the honest stand-in until it does.
- **Channels can be isolated, not edited.** Hiding a component in the Channels
  panel — by its eye toggle or by `Ctrl+3`/`Ctrl+4`/`Ctrl+5` — really does
  change the canvas: the mask is applied to the composite on its way to the GPU
  texture (`app_shell::presenter::ChannelMask`, proved on the GPU by
  `hiding_a_channel_changes_the_texture_the_canvas_samples`). What does not
  exist is painting or filtering **into** one channel: the panel's selected row
  is an isolation target, not a paint target, so every tool still writes all
  three components. The mask is a view setting and is not saved with the
  document.
- **Most of the menu is not implemented yet.** All nine menus carry 256 items.
  With a document open 77 are performable and 51 are correctly disabled by the
  shared model; the remaining 128 are drawn **greyed out** saying "This build
  cannot do that yet" — every Filter, every Adjustment, Image Size, Canvas
  Size, every Transform and Select All. None of them is a dead control: the
  number is pinned by `menu_bridge`'s
  `every_ui_menu_item_is_either_performable_or_disabled_with_a_reason`, which
  fails if it grows.

## Release gate

1. Every Tier A row is ✅ or has its gap named above.
2. Every Tier B row is ✅, named as partial, or moved to Tier C with a reason.
3. `cargo check --workspace --all-targets`, `cargo clippy` with `-D warnings`,
   `cargo fmt --check` and `cargo test --workspace` are green.
4. The app launches, opens a real image, edits it, saves, reopens and exports
   correctly — verified by running it, not by prose.
