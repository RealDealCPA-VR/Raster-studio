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
| Cut / copy / paste / clear | ✅ | incl. Copy Merged, Paste Special (Paste in Place, Paste Into, Paste Outside), Layer Via Cut/Copy |
| Edit ▸ Purge (Clipboard / Histories / All) | ✅ | W4-H; Histories and All ask first — the second choice within 10 s confirms (a status-line confirmation, not a modal: the dialog seam has no yes/no question for it yet) |
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
| Desaturate, Equalize | ✅ | W4-E: Image ▸ Adjustments, no dialog, one undo step; Desaturate is Shift+Ctrl+U and keeps linear luminance; Equalize reads the histogram of the selected pixels. Destructive-only, as in Photopea |
| Shadows/Highlights, Replace Color | ✅ | W4-E: dialogs with live preview; Shadows/Highlights reads a blurred luminance over its radius; Replace Color has a selection preview and samples from a click. Destructive-only, as in Photopea |
| Color Lookup (3D LUT) | ✅ | W4-E: a `.cube` file (file picker) or one of five built-in looks (Invert, Warm, Cool, Sepia, High Contrast). Image ▸ Adjustments bakes it as one undo step; Layer ▸ New Adjustment Layer ▸ Color Lookup creates the layer at the identity table, and Layer ▸ Edit Adjustment… or the Properties panel's "Open editor…" reopens the same dialog on that layer, whose OK rewrites the layer's table as one undo step |

### File
| Capability | Status | Notes |
| --- | --- | --- |
| Save / open the native `.rstudio` project | ✅ | integrity-sealed, version-gated, crash-safe swap |
| Pixel data persisted | ✅ | reopening composites to byte-identical output |
| Export PNG / JPEG / WebP / TIFF / GIF / BMP / ICO / SVG with presets | ✅ | correct un-premultiply and linear→sRGB on the way out; ICO carries 16/32/48/256 px entries; SVG wraps the raster composite as an embedded PNG (vector shape layers are not written as paths yet) |
| WebP lossy with a quality slider | ❌ | W4-H: kept lossless on purpose. The WebP encoder already in the tree (`image-webp` 0.2, behind `image` 0.25) writes lossless VP8L only. Pure-Rust lossy encoders exist on crates.io and were evaluated on 2026-09-23 (encode 256×128 and 257×131 RGBA test images at quality 50 and 90, decode with `image-webp` 0.2.4, measure RGB PSNR; `cargo audit` on a lockfile holding the three permissively licensed crates found no advisories): `zenwebp` 0.4 is AGPL-3.0-only (or a commercial licence), so it is out on licence; `webp-rust` 0.3.1 (MIT) measured 15–17 dB PSNR at every quality, and its quality-90 stream for the 257×131 image was rejected by `image-webp` with `BitStreamError`; `vaam-image-webp` 0.1.0 (MIT/Apache-2.0) reached 30–40 dB on the 256×128 images but 22–23 dB on the 257×131 ones, and was first published on 2026-09-22; `tiny-webp` 0.1.0 (MIT/Apache-2.0, no dependencies with `default-features = false`) measured 34–44 dB on all four images, but it is a single 0.1.0 release first published on 2026-09-04 with 23 downloads. `tiny-webp` is the candidate to adopt once it has a track record; adopting it means a workspace manifest change and a quality slider in Export As. Until then the Export As row is labelled “WebP (lossless)” and has no quality control |
| File ▸ Export ▸ Slices | ✅ | W4-H: one file per committed Slice-tool region, `<document>_01.<ext>`…, in the last confirmed Export As format and settings (PNG before any) |
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
| Filters: Photopea one-click and parity rows | ✅ | Blur ▸ Average, Blur, Blur More, Smart Blur; Sharpen ▸ Sharpen, Sharpen More, Sharpen Edges; Distort ▸ Displace (map: the layer itself or a seeded cloud field — no external PSD map picker); Pixelate ▸ Facet, Fragment, Mezzotint; Stylize ▸ Extrude, Tiles, Trace Contour. Each opens the generated FilterDialog with live preview (`the_photopea_parity_filters_preview_through_their_dialogs`) |
| Filter ▸ Convert for Smart Filters | 🚫 | greyed out with its reason (`ui::menu::SMART_FILTERS_UNSUPPORTED`): `layer_model::SmartObjectLayer` holds only an asset id and a link flag — no filter stack — and the compositor renders the source with nothing re-applied |
| Smart objects | ✅ | placed raster sources with editable transforms, embedded + linked origins, replace-contents refresh, full-extent storage; verified through real routes (cards 044–050, interchange tests) |
| PSD import | ✅ | groups, masks (with density/feather), blend modes, all four channel encodings, editable-text subset, the four mapped effects, ICC retention, full layer extents; a per-layer fidelity report (`PsdNotes`) names exactly what did not map — see `docs/PSD-THUMBNAIL-SUPPORT.md` |
| PSD export | 🔶 | layered export verified by independent re-read (structure, masks, effects as editable lfx2 descriptors, appearance-preserving text/shape/smart-object fallback pixels, merged preview matching the composite at ≤1/255); NOT yet verified in Photoshop/Photopea (card 081's manual step is pending) and editable text export (TySh) is blocked on an independently verified engine-data payload — see `docs/PSD-THUMBNAIL-SUPPORT.md` |
| Channels panel | 🔶 | isolation is real and changes the canvas; per-channel *editing* is still not implemented — see the gaps list |
| Paths panel | 🔶 | |
| Colour management | ✅ | sRGB and Display P3 are real; an embedded ICC profile is carried, composites through its profile and re-tags on export — `a_tagged_image_composites_through_its_profile_and_retags_on_export` |
| 16-bit per channel | 🔶 | a user can work at 16 bits: New Document accepts 16 bits (RGBA16 base tiles), Image > Mode > 8/16 Bits/Channel converts every raster tile in one undoable step (8 -> 16 lossless, 16 -> 8 rounded), a tool stroke on a 16-bit layer lands as RGBA16 tiles keeping the untouched pixels' 16-bit codes, the compositor composites RGBA16 tiles in `f32`, and a 16-bit document exports 16 bits to PNG/TIFF (`mode_sixteen_then_a_brush_stroke_then_a_png_export_round_trips_at_sixteen_bits`); the whole-layer edits (transforms, filters, adjustments, fills, Image Size, Canvas Size, Grayscale) read a 16-bit tile rounded to 8 bits and land 16-bit tiles again, so they compute at 8-bit precision (P2.5b, see the gaps list) |
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
| Filter ▸ Render ▸ Lighting Effects | A per-pixel light rig (spot/omni/infinite lights, a bump channel, material gloss) with an interactive on-canvas light editor; the parameter-schema dialog cannot express the light handles, and a slider-only version would not be Lighting Effects. |
| Image ▸ Adjustments ▸ HDR Toning | Photoshop's HDR Toning flattens the image and runs a local-adaptation tone mapper (edge glow, detail, shadow/highlight, a curve) meant for 32-bit merges; this build has no 32-bit document mode for it to act on, and a slider set without the local adaptation would not be HDR Toning. |
| Image ▸ Adjustments ▸ Match Color | Matches statistics against *another open document or layer* as source; that needs a cross-document source picker and luminance/colour-intensity/fade model this build does not have, and Photopea's own implementation is minimal. |
| Vertical Type tool | Deferred by W4-G. The text engine lays out horizontal lines only: `crates/text-engine/src/lib.rs` states "Vertical writing modes are not implemented", and neither `text_engine::TextRun` nor `layer_model::TextLayer` carries an orientation. Adding a Vertical Type entry to the palette would create an ordinary horizontal text layer under a vertical tool's name, so no `ToolId` exists for it. It needs vertical shaping and line layout (glyph rotation for Latin runs, upright CJK, right-to-left column order) plus a stored orientation on the layer; the tool can then be a Type variant. |
| Smart Filters (non-destructive filter stacks on smart objects) | The smart-object layer kind carries no filter stack and the compositor has no re-apply pass; Convert for Smart Filters stays greyed with that reason until both exist. |
| Licensing and auto-update crates | Dropped from the workspace (P3.2): both were complete and tested with zero dependents — entitlement checks and update feeds are release-engineering for a shipped product, not this build. |
| Video and animation timeline | Out of scope for a raster editor v1. |
| Collaboration, cloud, mobile | Explicit non-goals: this is a local-first desktop app. |
| Perfect PSD round-tripping | We target correct reopen in Photoshop and Photopea, not byte fidelity. |

---

## Known gaps in what is marked done

Kept here rather than buried, because a ✅ with a footnote is still a claim:

- **16-bit editing is reachable; not every pixel path is deep yet (P2.5b, partly done).**
  *Done (W3-H):* the compositor reads an RGBA16 tile at its own depth and
  composites it in `f32` (`fill_layer`) -
  `a_sixteen_bit_ramp_composites_at_sixteen_bit_precision`,
  `a_sixteen_bit_ramp_composited_then_exported_equals_its_source`.
  *Done (W4-F):* users reach it. New Document accepts 16 bits and builds the
  base layer from RGBA16 tiles (`a_sixteen_bit_new_document_is_created_at_sixteen_bits`);
  Image > Mode > 8/16 Bits/Channel is enabled (the current depth is the
  checked, greyed row) and runs one undoable Transaction
  (`Command::SetMetaBitDepth` plus a per-layer tile rewrite; undo is
  byte-exact - `converting_to_eight_bits_then_undoing_restores_the_sixteen_bit_tiles_byte_exact`);
  the tools read a 16-bit tile rounded to 8 bits and their output is widened
  back at the apply boundary, keeping the exact 16-bit code of every pixel
  the tool did not change; export follows the document's depth, not only the
  source's (`mode_sixteen_then_a_brush_stroke_then_a_png_export_round_trips_at_sixteen_bits`).
  The app-shell's whole-layer reader (`pixels::read_layer`, behind Edit >
  Transform, filters, adjustments baked to pixels, fill, stroke, clear,
  layer via copy/cut, apply mask, defringe) and the other tile readers
  (Image Size, Canvas Size's background fill, Reveal All's bounds, Image >
  Mode > Grayscale, channel-limited painting) read an RGBA16 tile rounded to
  8 bits, and their RGBA8 output is widened back to RGBA16 at the apply
  boundary (`mode_sixteen_then_flip_rotate_filter_and_adjustment_match_the_eight_bit_document`,
  `an_opened_sixteen_bit_png_flipped_twice_is_the_identity`,
  `image_size_on_a_sixteen_bit_document_matches_the_eight_bit_document`,
  `canvas_size_fill_on_a_sixteen_bit_document_keeps_the_old_pixels`,
  `grayscale_on_a_sixteen_bit_document_converts_its_tiles`). Image > Mode >
  8 Bits/Channel dithers (Photoshop's default "Use Dither";
  `mode_eight_from_the_menu_dithers_between_codes_and_keeps_exact_codes`).
  *Still open:* every one of those edits computes at 8-bit precision: a
  pixel it changes (and every pixel a transform moves) comes back as a
  widened 8-bit code, so the 16-bit depth survives only in the pixels an edit
  leaves in place. There is no user switch for the 16 -> 8 dither (it is always
  on from the menu). `open_path` still decodes a 16-bit source to RGBA8 tiles
  (the document's depth says 16, the first edit of a tile widens it). `.psd`
  export is 8-bit: RGBA16 layer tiles are rounded to RGBA8 on the way out
  (`import::rgba_from_tiles` reads through `raster::rgba8_view`; pinned by
  `a_sixteen_bit_document_saves_as_psd_with_the_eight_bit_twins_layer_pixels`),
  so a 16-bit document saved as PSD loses its 16-bit precision. The tile
  readers that need only coverage (the compositor's `alpha_bounds` for the
  Move tool and the Properties panel, the Move tool's auto-select pick) read
  RGBA16 alpha through `raster::tile_alpha16`.
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
