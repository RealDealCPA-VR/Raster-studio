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
| Select Subject | 🚫 | decided Tier C: no segmentation model ships, and the menu carries no item for it (P2.12) |
| Accessibility (screen readers / AccessKit) | ✅ (wired) | egui's `accesskit` feature is on: the adapter is initialised at window build, its action requests route through a typed user event, and egui publishes a labelled node per widget; keyboard focus follows egui's Tab navigation with its focused-widget visuals. The on-device screen-reader walk needs assistive tooling on the host. |
| Modify: feather, expand, contract, smooth, border | ✅ | true morphology on fractional coverage |
| Invert, grow, similar, transform selection | ✅ | |
| Quick mask, save/load selection | ✅ | quick mask composes (`Q` / Select ▸ Edit in Quick Mask Mode: edits land in a scratch mask, leaving converts the painted coverage into the selection); selection itself (outline, marching ants, save/load) is reachable |

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
| Photopea visual design | 🔶 | one token system, light and dark, WCAG AA asserted by test; the fit and finish is converging on Photopea’s density and neutral greys (P1 wave) |
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
| Colour management | ✅ | sRGB and Display P3 are real; an embedded ICC profile is carried, composites through its profile and re-tags on export — `a_tagged_image_composites_through_its_profile_and_retags_on_export` |
| 16-bit per channel | 🔶 | a 16-bit source is recognized, exported at 16 bits to the formats that carry them (PNG/TIFF) and `.rstudio` records the depth (`a_rstudio_package_round_trips_the_bit_depth`); in-app tiles still composite at 8-bit-equivalent precision (P2.5b, open) |
| Actions / recorded command replay | 🔶 | commands are serialisable and replayable; there is no recording UI |
| Batch export | 🔶 | multiple presets in one run |
| Brush / gradient / layer-style editors | 🔶 | |
| Autosave | ✅ | |
| Localization | 🔶 | Scope, stated exactly (P3.12/P6.6): the string catalogue (`crates/ui/src/strings.rs`) and its 209 `tr()` call sites cover `src/view` and `src/dialogs`, enforced by the `no_localized_literals` gate. NOT localized: `src/menu.rs` (every menu label — a large user-facing surface — is still an English literal), `src/panels` and `src/canvas`. Three whole-file exemptions carry **161 prose literals** (`filter_dialog.rs` 89, `new_document.rs` 40, `preferences.rs` 32); they clear with the `tools::OptionSpec`/`DocumentPreset` label-key refactor the gate's own comment names (the gradient editor's `name_key` is the pattern). No claim of translation support beyond the catalogue's locale keying is made. |

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
| Licensing and auto-update crates | Dropped from the workspace (P3.2): both were complete and tested with zero dependents — entitlement checks and update feeds are release-engineering for a shipped product, not this build. |
| Video and animation timeline | Out of scope for a raster editor v1. |
| Collaboration, cloud, mobile | Explicit non-goals: this is a local-first desktop app. |
| Perfect PSD round-tripping | We target correct reopen in Photoshop and Photopea, not byte fidelity. |

---

## Known gaps in what is marked done

Kept here rather than buried, because a ✅ with a footnote is still a claim:

- **Live editing composites at 8-bit-equivalent precision (P2.5b, open).**
  Deep sources are honoured on export (a 16-bit source writes 16 bits to
  PNG/TIFF; an 8-bit source keeps the byte-exact path), `.rstudio` records
  each layer's depth (`a_rstudio_package_round_trips_the_bit_depth`), but the
  live tiles still composite at 8 bits and the New Document dialog refuses
  16-bit rather than confirm a document that would draw as garbage.
- **Native tablet events need a pen.** Pressure is wired through the shell
  seam (`Shell::set_pen_pressure`) and the stroke engine is pressure-aware,
  but subscribing to one device's winit tablet events requires hardware on
  the host.
- **The OS printer-spooler dialog.** Print ▸ As PDF renders the composite to a
  tested single-page PDF; talking to an actual printer spooler is OS-only and
  not part of the build.
- **Disabled menu items are gone; one conditional refusal remains.** After
  C7, every enabled menu item routes to real code. The only refusal in
  `unavailable_reason` besides the File-Info note is an adjustment clicked
  while its parameters still sit at the identity — the status line says to
  add it as an adjustment layer and edit it in Properties instead. The
  route coverage is pinned by `menu_bridge`'s
  `no_enabled_menu_item_resolves_to_a_no_op` digest.
- **Per-channel masking stops at colour components.** The Channels panel
  isolates, paints into, erases within, fills, filters and bakes adjustments
  into a single RGB component — every one rides `mask_paint_to_channel` at
  the command boundary (`the_eraser_through_the_red_channel_clears_only_red`,
  `gaussian_blur_through_the_red_channel_blurs_only_red`), so the masked
  command reaches history and the journal. An alpha or mask-coverage target
  paints normally rather than being isolatable, and the panel has no
  per-channel histogram.
- **Quick mask composes** (Tier C, landed): `Q` toggles it, pixel edits land
  in a scratch mask, and leaving turns the painted coverage into the
  selection; the selection itself, its outline, marching ants and save/load
  are all reachable.
## Release gate

1. Every Tier A row is ✅ or has its gap named above.
2. Every Tier B row is ✅, named as partial, or moved to Tier C with a reason.
3. `cargo check --workspace --all-targets`, `cargo clippy` with `-D warnings`,
   `cargo fmt --check` and `cargo test --workspace` are green.
4. The app launches, opens a real image, edits it, saves, reopens and exports
   correctly — verified by running it, not by prose.
