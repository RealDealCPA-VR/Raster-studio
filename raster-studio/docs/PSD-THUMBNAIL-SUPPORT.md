# PSD support — fidelity matrix and failure policy

Card 072's contract, brought up to date at `1c34b9c` (wave 16, on the
branch `wip/wave16-partial`): what the
`.psd` / `.psb` reader and writer in `crates/psd` carry into and out of this
editor, and what happens when they cannot. Every row below is grounded in
source. The import conversion is `crates/app-shell/src/import.rs`
(`document_from_psd`: `psd::read` → the document model), with its child
modules `psd_live.rs` (shape layers, smart objects, 16-bit samples),
`psd_vector_mask.rs` (vector masks), `psd_resources.rs` (guides, paths,
alpha channels, slices), `spot_channel.rs` (W13X-4: spot channels) and
`psd_colour_modes.rs` (W16-B: colour modes); the export back-trip is `psd_from_document` in the
same file (the document model → `psd::write`). The byte layouts live in the
`psd` crate: `adjustments.rs`, `effects.rs` + `effects_rest.rs`, `fill.rs`,
`pattern.rs`, `placed.rs` (W16-J: with `smart_filters.rs`), `shape.rs`
(W16-G: with `live_origin.rs`), `text.rs` + `engine_data.rs`,
`resource.rs` and (W16-B) `colour_modes.rs`.

Every **fallback** row names the note the import or export puts in its
report (see `Tally::record` and the other `notes.push` calls in `import.rs`).
The notes are the user-visible failure policy — nothing is silently dropped.

**A correct merged preview is explicitly insufficient proof of an
editable layer round-trip.** The flattened composite is only the
appearance; the rows below are about the layers and their parameters.
Original PSD bytes are never modified on import — `psd::read` takes a
byte slice, and opening a file writes nothing.

**What "verified" means here.** Unless a row says otherwise, a mapping is
verified by this build's own tests: a file this build writes (or a
hand-built one) read back by this build's own reader. The independent-reader
evidence is listed at the end, and so is what has **not** been checked in
Photoshop or Photopea themselves.

## Outcome vocabulary

- **Editable** — the feature lands in the document model and stays
  editable (parameters, re-orderable, undoable).
- **Appearance fallback** — the feature's *effect* is preserved as pixels
  (or recorded prose) but the editable definition is not; the report
  names exactly what was lost.
- **Unsupported (explicit)** — the feature has no model here and the
  import or export says so, by name, in its notes.

## Import matrix (`psd::read` → editor document)

| Feature | Outcome | Notes |
|---|---|---|
| File versions | **Editable** | Version 1 (`.psd`) and version 2 (`.psb`, 64-bit lengths, canvases to 300 000 px) open through the same road (`PsdHeader::read_any`; W10-F). |
| Colour mode | **Editable** (RGB, Grayscale, CMYK, Lab, Indexed, Bitmap, Duotone) / **Explicit** (Multichannel) | W16-B: `psd::ColorMode` has every Photoshop mode (Bitmap, Grayscale, Indexed, RGB, CMYK, Multichannel, Duotone, Lab); only an unassigned mode code is refused by name. `psd::colour_modes::to_working_rgb` decodes each layer and the composite with this build's own colour science (`app_shell::import::psd_colour_modes`): CMYK (inverted per Adobe) through `color::cmyk`, Lab (8 or 16 bits, offset-encoded) through `color::model`, Indexed through its palette and transparent index, 1-bit Bitmap unpacked (its layer section skipped), Duotone as its grey base printed through the ink record's colours and curves; the document opens in the matching mode (`DocumentMeta::color_mode`). Named in the report: "this 16-bit {mode} document is edited at 8-bit precision per channel" (a 16-bit CMYK / Lab file: the tiles are 8-bit RGBA in every mode but RGB and Grayscale); a Duotone ink record that cannot be read ("the Duotone ink record could not be read ({why}); the document opened as its Grayscale base"), a colour-book ink (shown with a stand-in colour); Multichannel, which has no document mode ("Multichannel has no document mode here: …", its first three inks as C, M, Y in an RGB document, one ink as Grayscale, further channels left out). The Duotone record layout is not yet checked against a Photoshop-written file. |
| Bit depth | **Editable** (8, 16) / **Appearance fallback** (32) | An 8-bit file opens as an 8-bit document; a 16-bit file opens as a 16-bit document whose layer tiles keep every sample (W9-M, `psd_live::deep_tile_edits`). A 32-bit file is converted down to 8 bits with the note "this is a 32-bit-per-channel document; Raster Studio edits 8, so its pixels were converted down" — although the editor has 32 Bits/Channel documents since W10-H, this road does not use them. |
| Layer tree, groups (open/collapsed, PassThrough vs Isolated) | **Editable** | `LayerKind::Group` with `GroupBlending`; children keep order. |
| Layer name, bounds, visibility | **Editable** | |
| Blend mode, opacity, fill opacity | **Editable** | Modelled on both sides (`psd::BlendMode` ↔ `BlendMode`). |
| Clipping ("clipped to layer below") | **Editable** | `ClippingMode::ClipToBelow`. |
| Layer locks (composite/pixels, position, transparency protect) | **Editable** | Round-trip both directions (import `layer_common`, export `psd_layers_for`). A PSD never produces a blanket lock; the "no .psd equivalent" note is export-side only. |
| Raster layer pixels (8- or 16-bit, raw / RLE / ZIP channels) | **Editable** | Tiles stored at the layer's own bounds; **full extents preserved** — ink outside the canvas is kept in the tile store and moves into view with the layer rather than being dropped at import. |
| Layer masks | **Editable** | Imported as raster mask coverage at the mask's bounds, honouring the default colour; the default-colour region extends past the canvas the same way the layer extents do. |
| Mask density / feather parameters | **Editable** | Card 076: density (`0..=255`) maps onto the model's `0.0..=1.0`; feather is stored in pixels, already the model's `feather_px` unit (a `.psd` mask has no space of its own beyond its layer's). The import note only fires for values the model refuses — a non-finite feather. W11-C: export writes them too (export matrix). |
| Vector masks (`vmsk` / `vsms`) | **Editable** | W9-G: a non-shape layer's path becomes a live vector mask (`psd_vector_mask::path_from_psd`), with its density and feather from the mask parameter block. The mask record's rendering of the vector is not imported as pixels; a second (`real`) record becomes the pixel mask. A `real` record with no path block is the one case left: "{names} carried a second, vector-derived mask that was not imported". |
| Shape layers (path + `SoCo` / `GdFl` / `PtFl` fill + `vstk` stroke) | **Editable** | W9-M: a live shape layer — path in canvas pixels with an identity transform, solid / gradient / pattern fill, stroke width, colour, opacity, cap, join, alignment and dash (`psd_live::shape_from_psd`). W16-G: a `vogk` live rectangle (with `keyOriginRRectRadii`), ellipse or line (with Photopea's `keyOriginLineArr*` arrowheads) under a pure translation opens as a live shape (`psd::live_origin`), editable in Properties' Live Shape section. A shape whose path does not parse, or whose pattern fill names a pattern the file does not carry, does not open as a live shape layer. |
| Fill layers (`SoCo` / `GdFl` / `PtFl` with no path) | **Editable** | W9-B: live Solid Color / Gradient / Pattern fill layers, evaluated by the compositor, re-editable from Properties. A noise gradient, or a pattern fill naming a pattern the file does not carry, is not mapped: the fill key then goes to the adjustment decoder, which does not know it, so the layer is kept empty and named by the adjustment note below. |
| Adjustment layers | **Editable** | W11-A: all sixteen adjustment keys a `.psd` defines — Invert (`nvrt`), Levels (`levl`), Curves (`curv`), Brightness/Contrast (`brit`), Hue/Saturation (`hue2`), Color Balance (`blnc`), Black & White (`blwh`), Photo Filter (`phfl`), Channel Mixer (`mixr`), Posterize (`post`), Threshold (`thrs`), Gradient Map (`grdm`), Selective Color (`selc`), Exposure (`expA`), Vibrance (`vibA`), Color Lookup (`clrL`, with its embedded `.cube`) — open as live adjustment layers (`psd::adjustments::decode`, bounded). Settings the model cannot hold (per-range Hue/Saturation settings, gradient colour midpoints, curves beyond red, green and blue) are named — "the {what} of “{layer}” did not import; the layer's other settings did". |
| Adjustment layers — payloads that do not decode | **Unsupported (explicit)** | A malformed payload, Photo Filter version 3 (XYZ colour), a Color Lookup with no embedded `.cube` or Curves stored as 256-entry maps: the bytes survive in the `psd` crate's model, but this build will not invent slider values — the layer is kept empty and named ("adjustment layer(s) this build cannot evaluate …", below). |
| Type layers — string and engine data | **Editable** | W9-C: the `Txt ` string (or the engine data's own text) imports as a `LayerKind::Text`, styled from the text engine data (a bounded parser): each run's font, size, fill, tracking, faux bold / italic, underline / strikethrough and super / subscript; the first run's leading, caps, horizontal / vertical scale and baseline shift; the first paragraph's alignment, indents and spacing; point or box frame. The `TySh` transform is placed at Photoshop's anchor (point text's first baseline at its aligned edge, measured by shaping the layer; box text by its box corner). A font this machine lacks is kept by name and its stand-in reported — "the font “{font}” used by {names} is not installed; “{substitute}” stands in for it until it is". A later run's different leading / caps / scale / baseline shift, manual kerning and a later paragraph that differs are named, not applied. The raw `TySh` bytes stay in the model. |
| Type layers — engine data absent or unreadable | **Editable (reported substitution)** | The string still imports as editable text with the editor's new-text defaults, named — "type layer(s) … were imported as editable text with the default font, size and fill — the source font is not in this build's supported subset"; unreadable engine data also gets "the text styling of “{layer}” could not be read ({reason}); it was imported with the default font, size and fill". |
| Type layers — no parseable string | **Appearance fallback** | Pixels are imported; the text is not editable — "type layer(s) … were imported as pixels; the text is no longer editable". The engine-data keys this build does not read are neither applied nor reported; the `TySh` warp descriptor is read onto the layer (W15-C, `psd::text::warp_spec`; Shell Lower / Upper and Vertical orientation open as Arc Lower / Upper, horizontal, named in the report). |
| Layer effects — all ten kinds | **Editable** | Card 075 (drop shadow, solid stroke, colour overlay, outer glow), W8-D (pattern overlay, resolved against the file's patterns) and W11-B (inner shadow, inner glow, bevel and emboss, satin, gradient overlay, through `effects_rest.rs`, the mapping the `.asl` import shares): blend mode, colour, opacity, sizes and distances scaled by the block's `Scl `, angles, and the drop-shadow / inner-shadow / glow / bevel contours (`TrnS`). Photoshop CC's repeated effects map when their `...Multi` lists sit inside the `lfx2` descriptor. An effect the file switches off stays absent. The verbatim `lfx2` bytes are also retained on every layer, but **retention is not rendering**. |
| Layer effects — what does not map | **Unsupported (explicit)** | W13-B: a gradient- or pattern-filled stroke and a gradient-filled glow now map (a pattern stroke through the file's patterns), and a gradient overlay's `Ofst` maps as a percentage of the layer's box (or the canvas). Still named: a pattern stroke naming a pattern the file does not carry, a pattern-filled glow, a noise gradient, an effect with required fields missing, an unknown effect key, and a pattern overlay naming a pattern the file does not carry: named per kind — "the {kinds} effect(s) on {names} were not imported". The separate `lmfx` block Photoshop CC writes for repeated effects is not read and not reported. A block that cannot be decoded at all keeps the blanket note — "layer effect(s) on {names} were not imported". |
| Patterns (`Patt` / `Pat2` / `Pat3`) | **Editable** / **Unsupported (explicit)** | W8-D: 8-bit RGB or greyscale, raw or RLE, feed pattern overlays and pattern fills. Any other image mode, 16- or 32-bit depth, or ZIP compression is refused and noted ("…; layers that use it keep no pattern"). |
| Smart objects (`SoLd` / `PlLd` + `lnk2` / `lnk3` / `lnkD`) | **Editable** | W9-M: an embedded placed file becomes a live smart object — the file is its asset, its decoded pixels its source, the four placed corners its transform (`psd_live::smart_from_psd`). W16-J: a `filterFX` descriptor in `SoLd` opens as the object's live smart filters (`psd::placed::smart_filters`: Average, Blur, Blur More, Box Blur, Gaussian Blur, Motion Blur, Surface Blur, Sharpen, Sharpen More, Sharpen Edges, Unsharp Mask, Add Noise, Despeckle, Dust & Scratches, Median, Mosaic, High Pass, Maximum, Minimum, each with its switch, opacity and blend mode), and a `liFE` entry in `lnkE` opens as a **linked** smart object read from the path it names. The `filterFX` layout follows Photopea's writer and is not yet checked against a Photoshop-written file. |
| Smart objects — missing, unreadable or undecodable file, or a filter this build lacks | **Appearance fallback** | The cached pixels import as a raster layer — "the smart object “{layer}” was imported as pixels: {why}" (e.g. "its linked file {path} cannot be read: …", "it links to a file outside the document without naming its path", "its smart filter(s) {names} have no equivalent in this build"). A linked-file entry the reader refuses is named too ("…; smart objects that place it open as pixels"). |
| Guides (resource 1032) | **Editable** | W11-C: the document's guides (`psd_resources::import_resources`). Guide locks are not a `.psd` concept. |
| Saved paths (2000–2997) and the work path (1025) | **Editable** | W11-C: path layers (no-fill, no-stroke shape layers, the Paths panel's rows); the work path arrives as a saved path called "Work Path". A path with no drawable geometry is named: "the path {name} has no geometry this build can draw and was not kept". |
| Alpha channels (merged-image extra channels named by 1006 / 1045) | **Editable** | W11-C: saved selections (the Channels panel's alpha rows). One that cannot be kept is named: "the alpha channel {name} could not be kept as a saved selection: {reason}". |
| Spot channels (extra channels marked spot in DisplayInfo, 1077) | **Editable** | W13X-4: `spot_channel::adopt_psd_spots` moves each channel whose DisplayInfo record has kind 2 out of the saved selections and into `Document::spot_channels`, with its ink colour and solidity (the record's opacity). A spot channel whose plane cannot become a mask is skipped without a note. The DisplayInfo resource itself is not in the import's list of mapped resources, so it is still counted among the resources "left behind" (next rows) even though its spot records were read. |
| Slices (1050) | **Editable** | W11-C: user and layer slices (version 6, and versions 7-8 as a descriptor, tested on a descriptor this build writes) with name, URL and alt text become the document's slices, loaded into the Slice tools' store; Photoshop's auto-generated fill slices are skipped. |
| Embedded ICC profile | **Retained (metadata)** | Card 076: resource 1039 is extracted (`psd::resource::icc_profile`) and its bytes ride in the document's colour space (`ColorSpace::IccProfile`). The pixels are NOT transformed at import — deliberate, documented: they load verbatim, the compositor converts matrix-shaper profiles at render, and a profile this engine cannot parse falls back to identity (`is_transform_supported`) rather than being silently reinterpreted. A profile that is measurably sRGB (`MatrixShaper::is_srgb_equivalent`) is recorded as sRGB. When a profile is retained, the resources note drops "the colour profile" from its list. |
| Other image resources | **Unsupported (explicit)** | Everything but guides, slices, paths, alpha-channel names, the resolution (every writer synthesises one, so it is not reported) and a retained profile is counted and named as left behind (failure policy 1) — DisplayInfo (1077) included, although W13X-4 reads its spot records (row above). |
| Layer colour labels (`lclr`) | **Supported** | W13-B: `lclr` 0..=7 (none, red, orange, yellow, green, blue, violet, gray) opens as the layer's colour label (`layer_model::ColorLabel::from_psd_index`, kept in the document's extras). Only an index past those eight is dropped and named — "the colour label on {names} is not one of the eight this build knows and was not kept". |
| Pass-through + blend mode on one group | **Appearance fallback** | Export-side: a `.psd` "stores only the" pass-through; the blend mode is named as lost. |
| Files with no layers / no flattened image | **Explicit** | "this file has no layers, so its flattened image became one layer", or "this file has neither layers nor a flattened image; the canvas is empty". |
| A damaged file the reader could repair | **Explicit** | "the file was read with a repair: {warning}". |

## Export matrix (`psd_from_document` → `psd::write`)

| Feature | Outcome | Notes |
|---|---|---|
| File version | **Editable** | Chosen by the file name first: any export named `.psb` is written as version 2, whatever the canvas size (`doc::write_atomically` re-writes version-1 bytes with `psd::write_psb`). Under a `.psd` name a canvas up to 30 000 px is written as version 1; past that (to 300 000 px) the writer produces `.psb` bytes, which a `.psd` name refuses, naming `.psb`, and Save as PSD offers `.psb` first for such a canvas (W11-H). |
| Colour mode | **Editable** (RGB, Grayscale, CMYK, Lab, Indexed) / **Appearance fallback** (Bitmap, Duotone) | W16-B: Save as PSD writes a CMYK, Lab, Indexed or Grayscale document in its own mode (`psd::colour_modes::from_working_rgb`: CMYK through `color::cmyk::rgb8_to_cmyk`, inverted; Lab offset-encoded; Grayscale as Rec. 601 luma, a 16-bit one at 16 bits; Indexed flat, the document's own colours as the palette, a median cut past 256, with a transparent index), so a CMYK or Lab file opened and saved comes back within one code for separations this ink model makes (an ink split it would not choose, such as a rich black, is re-separated). Named in the save notes: "a Bitmap document is saved as Grayscale (its black and white pixels kept): this build does not write 1-bit .psd files"; "a Duotone document is saved as RGB with its inks applied: the ink record is not written back"; "an Indexed .psd holds one flat image: the layers were merged into it"; a median-cut palette. |
| Bit depth | **Editable** (8, 16) / **Appearance fallback** (32) | A 16-bit document is written as a 16-bit file (raster layers at their stored samples; rendered previews, masks and the merged image are 8-bit values widened exactly). A 32 Bits/Channel document is written as an 8-bit file: its `f32` tiles are clipped and rounded (`raster::rgba8_view`), and no note says so. |
| Layer tree, groups, order, names, bounds, visibility | **Editable** | Round-trips. |
| Blend mode, opacity, fill opacity, clipping, locks | **Editable** | A blanket lock is named: "the blanket lock on {names} has no .psd equivalent and was not written". |
| Raster pixels, layer masks | **Editable** | A layer whose transform a `.psd` cannot express is written where its pixels are stored — "{names} carry a transform a .psd cannot express; their pixels were written where they are stored". |
| Mask density / feather | **Editable** | W11-C: written in the mask record's parameter block. Only a pixel mask with no stored pixels, which writes no record, still gets "the mask density or feather on {names} was not written". |
| Vector masks | **Editable** | W9-G: `vmsk` path records (through the layer's and the mask's pose) and the vector density / feather pair; with a pixel mask too, the first record is the vector's rendering and the pixel mask the `real` one. A vector-only mask is written with no rendered coverage, so a reader that ignores `vmsk` sees no mask. A mask whose path cannot be written, or an older vector-kind mask with no path, goes out as coverage — "the vector mask on {names} was written as its rasterised coverage". |
| Adjustment layers | **Editable** | W11-A: every kind a `.psd` has a key for is written under that key (`psd::adjustments::encode`; Curves as version 1). Auto, Desaturate, Equalize, Shadows/Highlights, Replace Color, HDR Toning and Match Color have no `.psd` adjustment layer, and a setting the layout cannot spell (Brightness past ±150/255, a Black & White weight outside -200%..+300%, Posterize 256, curve inputs that collide once quantised) is refused rather than clamped. W16-J: each is written as a pixel layer showing its effect on the layers under it, never as an empty layer, and named — "adjustment layer(s) {names} have no .psd adjustment layer; each was written as a pixel layer showing its effect on the layers under it, which is no longer editable as an adjustment". |
| Fill layers | **Editable** | W9-B: `SoCo` / `GdFl` / `PtFl`, pattern pixels in the document's `Patt` block. `SoCo` has no alpha, so a translucent colour's alpha rides the layer's fill opacity. |
| Shape layers | **Editable** | W9-M: `vmsk` path in canvas pixels, `SoCo` (or `GdFl` / `PtFl`) fill, `vstk` stroke, `vogk` live rectangle for an axis-aligned rectangle (W16-G: a live rectangle with its corner radii, a live ellipse and a live line with its arrowheads, under a pure translation; no star `vogk`), over the rendered pixels. A translucent solid fill, a stroke under a non-uniform transform, a path with an arc, a pattern fill under any transform or a gradient fill under more than a translation keeps the card-078 raster fallback — "shape and smart-object layer(s) ({names}) cannot stay editable in a .psd; their rendered appearance was written as a raster layer's pixels". |
| Smart objects | **Editable** (embedded or linked, with the nineteen mapped smart filters) / **Appearance fallback** (another filter, a Wrap/Mirror edge, a smart-filter mask) | W9-M: `SoLd` + `PlLd` naming the asset, whose bytes go in the document's `lnk2` block. W16-J: a linked object is a `liFE` entry in `lnkE` naming its file (native `originalPath` and a `file://` `fullPath`; no cached copy); its smart filters ride in `SoLd` as Photoshop's `filterFX` descriptor for the nineteen filters the import row lists. An object carrying any other filter, a Wrap/Mirror edge setting or a smart-filter mask keeps the raster fallback, named by the note above with the reason. |
| Type layers | **Editable** | Card 079 + W9-C: a complete `TySh` block — string, transform at Photoshop's anchor, bounds and an engine-data payload with every style run (font with its weight in the name, size, fill, tracking, …), the paragraph and the frame — over the layer's rendered pixels as fallback. Every text layer is named — "type layer(s) ({names}) were exported with the editable text subset; styling beyond it is covered by the layer's raster fallback" — and styling the writer cannot spell is named per layer ("the {what} of “{layer}” could not be written to its editable text; the layer's raster fallback shows it as it was"). |
| Layer effects — all ten kinds | **Editable** | Card 080 + W8-D + W11-B: a real `lfx2` descriptor, the exact inverse of the import mapping, contours included; a pattern overlay's pattern goes in the `Patt` block. Rendered fallback layers strip effects from their baked pixels so nothing draws twice. |
| Layer effects — what is not written | **Unsupported (explicit)** | W13-B: a gradient- or pattern-filled stroke, a gradient-filled glow, a gradient overlay's offset (as `Ofst` percentages of the written layer box, or the canvas) and the extra instances of a repeated effect (as the kind's `...Multi` list: `dropShadowMulti`, `innerShadowMulti`, `frameFXMulti`, `solidFillMulti`, `gradientFillMulti`) are written now. Still named: a pattern-filled glow (Photoshop's glows have no pattern fill), a pattern with no pixels, and an offset on a layer with an empty box: "the {kinds} effect(s) on {names} were not imported" is the shared wording, and the export report lists the kinds. |
| Guides | **Editable** | W11-C: resource 1032. Guide locks are not written — "guide locks have no .psd equivalent and were not written". |
| Path layers | **Editable** | W11-C: a no-fill, no-stroke, unstyled shape layer is written as a saved-path resource from 2000, not as a layer record (so it does not come back doubled); a styled one (hidden, masked, with effects, clipping, a non-Normal blend or reduced opacity / fill) stays a layer record. Past the 998 saved paths a `.psd` holds, a path is named and not written. The Paths panel's unsaved Work Path is not written. |
| Saved selections | **Editable** | W11-C: named alpha channels in the merged image (1006 + 1045), up to the 56-channel ceiling (52 on an RGBA file); the ones past it are named in the save notes and the save goes ahead. |
| Spot channels | **Editable** | W13X-4: after the saved selections, as more named channels of the merged image, with a DisplayInfo (1077) record per extra channel marking each spot one with its ink and solidity (`spot_channel::push_psd_spots`). One past the channel ceiling is named — "the spot channel {name} is past the channels this .psd can hold and was not written". The merged image is this build's composite, which already shows the ink. |
| Slices | **Editable** | W11-C: a version-6 1050 resource of user slices with name, URL and alt text. A slice's target, message and cell text are not written. |
| Layer colour labels | **Supported** | W13-B: a labelled layer's record carries `lclr` with the label's index (`ColorLabel::psd_index`); an unlabelled layer writes none. |
| Pass-through group with a blend mode | **Appearance fallback** | "{names} pass through *and* carry a blend mode; a .psd stores only the pass-through". |
| The merged (flattened) image | **Editable** | Taken from this application's compositor, not from the `psd` crate's fallback flattener. |

## Tally notes (verbatim)

The report notes collected per category by `Tally::record`
(`{names}` is the affected layers, two shown then "and N more") — the
honesty gate in `import.rs`
(`the_psd_support_matrix_names_every_fallback_the_import_emits`) asserts
this file keeps quoting every one:

- "the colour label on {names} is not one of the eight this build knows and was not kept"
- "adjustment layer(s) this build cannot evaluate ({names}) were kept as empty layers; their effect is in the flattened image but not editable"
- "type layer(s) ({names}) were imported as pixels; the text is no longer editable"
- "type layer(s) ({names}) were imported as editable text with the default font, size and fill — the source font is not in this build's supported subset"
- "layer effect(s) on {names} were not imported"
- "the {kinds} effect(s) on {names} were not imported" ({kinds} names the unmapped effect kinds, e.g. "satin and inner shadow")
- "{names} carried a second, vector-derived mask that was not imported"
- "{names} carry a transform a .psd cannot express; their pixels were written where they are stored"
- "shape and smart-object layer(s) ({names}) cannot stay editable in a .psd; their rendered appearance was written as a raster layer's pixels"
- "type layer(s) ({names}) were exported with the editable text subset; styling beyond it is covered by the layer's raster fallback"
- "the mask density or feather on {names} was not written"
- "the vector mask on {names} was written as its rasterised coverage"
- "the blanket lock on {names} has no .psd equivalent and was not written"
- "{names} pass through *and* carry a blend mode; a .psd stores only the pass-through"

The other notes (per layer or per resource, not tallied) are quoted in the
rows above: the text-styling, font-substitution and unwritten-styling notes
(W9-C), the adjustment-settings note (W11-A), the smart-object and pattern
refusals (W9-M, W8-D, W16-J), the colour-mode notes (W16-B), the rasterised-adjustment note (W16-J), the 32-bit, repair and no-layers notes,
and the path, alpha-channel, guide-lock and saved-selection notes of
`psd_resources.rs` (W11-C).

## Failure policy

1. **Nothing silent.** Every fallback and refusal pushes a note
   naming the affected layers (two shown, then "and N more"); resources
   the document cannot carry are counted and named as left behind:
   "{n} image resource(s) — the colour profile — are not part of this document model and were left behind"
   (the list reads "the colour profile, other resources", or "other
   resources", depending on what was dropped). When a profile is embedded
   and retained, "the colour profile" is dropped from that list. Guides,
   slices, saved and work paths and the alpha-channel names (W11-C) are
   mapped, so they are not counted.
2. **No invented numbers.** A payload this build cannot evaluate is
   never decoded into guessed parameters ("inventing one would put the
   wrong numbers behind a slider"), and a setting the writer cannot spell
   is refused by name, never clamped.
3. **Original bytes untouched.** Import never writes to the source file.
4. **Loud refusals over bad output.** Malformed input fails through
   `PsdError` with the byte offset; limits (tile/allocation ceilings in
   `crates/psd/src/limits.rs`) apply on the way in and are re-applied by
   the import path.
5. **No unseen-file promises.** This matrix describes what the parser
   and writer carry today; compatibility with a PSD produced by an
   unseen tool version is asserted only by the fixtures of card 073.

## The fidelity report (card 077)

A `.psd` whose import loses anything shows an actionable report right
away — an information notice, not an error, because the document did
open. The report contains:

- the fallback notes above, verbatim, with the affected layers named;
- one line per layer from the per-layer outcome list
  (`PsdNotes::layers`): *editable* (mapped exactly, or editable text
  with a substituted font), *raster fallback* (text became pixels, a
  vector mask became coverage, a smart object opened as pixels), or
  *unsupported* (the layer arrived empty — an adjustment this build
  cannot evaluate) — each with the reason;
- the original file's path, a statement that the original was not
  modified, and the encouragement to continue in the native `.rstudio`
  format via File ▸ Save As.

A file whose import loses nothing produces **no report at all** — no
generic warnings unrelated to its data. The file's own flattened
preview is retained on the import (`PsdImport::merged_preview`), and
`PsdImport::compare_merged_preview(tolerance)` offers the comparison
against the reconstructed document when a caller wants it; it is
offered, never asserted, because a preview written by a crude
flattener legitimately differs from a correct reconstruction.

Exporting a `.psd` back over the imported original's own path is
refused (`DocumentError::OriginalOverwrite`): the original is never
silently replaced by this build's reduced representation of it. Choose
another name, or Save As a native `.rstudio` first.

## What would change rows

The rows move only with code: an editable row becomes possible when the
document model grows the vocabulary; a fallback becomes explicit only when
a note names it. Update this file in the same commit that changes either
side.

## Card 081 — external evidence status

The locally runnable acceptance workflow (import independently authored
bytes → edit through the real routes → save native → export PSD → reopen
through the independent `psd::read`) passes with editability and
appearance compared separately. **Pending manual external evidence:**
opening an export in Photoshop and/or Photopea, recording tool versions
and tolerances. This is host-bound like the hardware checks in
`docs/CORRECTIONS-TODO.md` (C14). A user-supplied real PSD is an
additional compatibility case, never proof that every PSD works.

## Independent-reader verification (cards 079/081 — executed 2026-09-20)

`docs/evidence/card091-export.psd` is a layered export of the card-073
acceptance scene with the headline edited through the real route. Two
readers with **no code in common with this project** were run against it
(full transcript: `docs/evidence/psd-independent-reader-output.txt`):

- **psd-tools 1.19.0**: layer structure and names, the edited headline as
  a genuine *type* layer whose text reads `SOLD TODAY` with the layer's
  transform, the portrait's layer mask, and the complete `lfx2` effects
  descriptor (master switch, drop shadow blur/distance/opacity, stroke,
  colour overlay).
- **Pillow 12.1.0**: its own PSD decoder reads the file's flattened image
  and it matches the application's exported composite **byte-for-byte
  (max channel difference 0)**.

Known reader quirks recorded, not hidden: psd-tools keeps the spec's
terminating NUL in layer names and its naive compositor ignores layer
effects (it is not a full blending engine), so appearance was compared
through the flattened image with Pillow instead.

That evidence predates waves 9-13X. Wave 9 recorded (in the parity matrix's
PSD export row, with no transcript committed) that psd-tools 1.19.0 reads
the W9-M fixture — written by the ignored test
`psd_live::tests::w9m_fixture_for_independent_readers` — as a shape layer,
an embedded smart object and a 16-bit pixel layer.

## What remains unverified

- **Nothing on this page has been opened in Photoshop or Photopea
  themselves.** Every export row above is proven by this build's own reader
  and, for the rows named in the previous section, by psd-tools and Pillow.
- **The W11 mappings have been read back by this build only:** the
  adjustment-layer payloads (their layouts follow Adobe's published
  specification and are checked by round trips through
  `psd::adjustments`), the five W11-B effects, and the guides, paths, alpha
  channels, slices and mask parameters of W11-C (slice versions 7-8 are
  tested on a descriptor this build writes, not on a Photoshop file).
- **The W13-B and W13X-4 mappings have been read back by this build
  only:** colour labels (`lclr`), the gradient- and pattern-filled strokes,
  gradient-filled glows, a gradient overlay's `Ofst` and the `...Multi`
  repeated-effect lists (`psd` `effects::w11b_rest_effect_tests::*`,
  `app-shell` `import::tests::every_colour_label_round_trips_through_a_psd_as_its_lclr_index`
  and `repeated_gradient_pattern_and_offset_effects_survive_a_psd_save_and_reopen`),
  and spot channels in DisplayInfo (`spot_channel::tests::*`).
- **The W9-C text engine data** (every style run, auto leading, the anchor)
  is re-read by this build's own importer; whether Photoshop re-typesets it
  the same way is unchecked, which is why the rendered pixels ride under
  every text layer.
- Driving Photoshop's or Photopea's UI to restyle a layer from one of these
  files remains a manual step.
