# PSD thumbnail support — fidelity matrix and failure policy

Card 072's contract: what the `.psd` reader/writer in `crates/psd` can
actually carry for the thumbnail workflow, and what happens when it
cannot. Every row below is grounded in source: the import conversion is
`crates/app-shell/src/import.rs` (`psd::read` → the document model; the
export back-trip is `psd::write`), and every **fallback** row names the
exact status-bar note the import emits (see `Tally::record`). The notes
are the user-visible failure policy — nothing is silently dropped.

**A correct merged preview is explicitly insufficient proof of an
editable layer round-trip.** The flattened composite is only the
appearance; the rows below are about the layers and their parameters.
Original PSD bytes are never modified on import — `psd::read` takes a
byte slice, and opening a file writes nothing.

## Outcome vocabulary

- **Editable** — the feature lands in the document model and stays
  editable (parameters, re-orderable, undoable).
- **Appearance fallback** — the feature's *effect* is preserved as pixels
  (or recorded prose) but the editable definition is not; the status bar
  names exactly what was lost.
- **Unsupported (explicit)** — the feature has no model here and the
  import says so, by name, in the status notes.

## Import matrix (`psd::read` → editor document)

| Feature | Outcome | Notes |
|---|---|---|
| Canvas size, bit depth, colour mode (8-bit RGB paths) | **Editable** | Canvas becomes the document; other depths/modes fall through the same converter the codecs use. |
| Layer tree, groups (open/collapsed, PassThrough vs Isolated) | **Editable** | `LayerKind::Group` with `GroupBlending`; children keep order. |
| Layer name, bounds, visibility | **Editable** | |
| Blend mode, opacity, fill opacity | **Editable** | Modelled on both sides (`psd::BlendMode` ↔ `BlendMode`). |
| Clipping ("clipped to layer below") | **Editable** | `ClippingMode::ClipToBelow`. |
| Raster layer pixels (8-bit, incl. RLE/ZIP channels) | **Editable** | Tiles stored at the layer's own bounds; **full extents preserved** — ink outside the canvas is kept in the tile store and moves into view with the layer (or shows in an extended-region composite) rather than being dropped at import. |
| Layer masks (8-bit coverage) | **Editable** | Imported as raster mask coverage at the mask's bounds, honouring the default colour; the default-colour region extends past the canvas the same way the layer extents do. |
| Mask density / feather parameters | **Editable** | Card 076: density (`0..=255`) maps onto the model's `0.0..=1.0`; feather is stored in pixels, already the model's `feather_px` unit (a `.psd` mask has no space of its own beyond its layer's). The import note only fires for values the model refuses — a non-finite feather. (Export still does not write the parameters — see the export matrix.) |
| Vector masks | **Appearance fallback** | Written as their rasterised coverage — "the vector mask on … was written as its rasterised coverage". A second, vector-derived mask "was not imported" by name. |
| Adjustment layers — **Invert** (`nvrt`) | **Editable** | The one adjustment whose whole definition is its name. |
| Adjustment layers — everything else | **Unsupported (explicit)** | The payload survives in `psd`'s model but this build will not invent slider values: the layer is kept empty and named — "adjustment layer(s) … were kept as empty layers; their effect is in the flattened image but not editable". |
| Type layers — parseable `Txt ` string | **Editable (reported substitution)** | The string imports as an editable `LayerKind::Text` under the `TySh` transform (any affine). The format carries no font/size/fill outside the engine data, so the editor's new-text defaults are used and named — "type layer(s) … were imported as editable text with the default font, size and fill — the source font is not in this build's supported subset". The raw `TySh` bytes stay in the model and survive a save verbatim. |
| Type layers — unparseable (`Txt ` missing or unreadable) | **Appearance fallback** | Pixels are imported; the text is not editable — "type layer(s) … were imported as pixels; the text is no longer editable". |
| Layer effects — drop shadow, solid stroke, solid colour overlay, outer glow | **Editable** | Card 075: decoded from the `lfx2` descriptor into parameters (`crates/psd/src/effects.rs`): the enabled flag and master switch, blend mode, colour (stored gamma-encoded as document-space 0..1 — decoded to linear at render by the compositor, the same convention the 8-bit pixel path uses), opacity, radius/size, distance, angle and spread (stored as a fraction of size), and the block scale applied to every pixel length. An effect the file switches off stays absent — nothing is invented. The verbatim `lfx2` bytes are also retained on every layer, but **retention is not rendering**. |
| Layer effects — everything else | **Unsupported (explicit)** | Inner shadow/glow, bevel, satin, gradient/pattern overlays, a gradient or pattern stroke, or any effect with required fields missing: named per effect kind — "the {kinds} effect(s) on {names} were not imported". A block that cannot be decoded at all keeps the blanket note — "layer effect(s) on {names} were not imported". |
| Embedded ICC profile | **Retained (metadata)** | Card 076: resource 1039 is extracted (`psd::resource::icc_profile`) and its bytes ride in the document's colour space (`ColorSpace::IccProfile`, hashed like every other carrier of the variant). The pixels are NOT transformed at import — deliberate, documented: they load verbatim, the compositor converts matrix-shaper profiles at render, and a profile this engine cannot parse falls back to identity (`is_transform_supported`) rather than being silently reinterpreted. A profile that is measurably sRGB (`MatrixShaper::is_srgb_equivalent`, sampled over primaries and tone curves) is recorded as sRGB — exact, so the bytes are redundant. When a profile is retained, the resources note drops "the colour profile" from its list. |
| Smart objects (cached composite pixels) | **Appearance fallback** | The cached pixels import as a raster layer; the placed-source identity does not survive the trip (a `.psd` stores a different structure). |
| Layer transforms (arbitrary affines) | **Appearance fallback** | Where a `.psd` cannot express the stored transform, pixels are written where they are stored — "… their pixels were written where they are stored". |
| Layer locks (composite/pixels, transparency protect) | **Editable** | Round-trip both directions (import `import.rs:899–905`, export `psd_layers_for`). A PSD never produces a blanket lock; the "no .psd equivalent" note is export-side only — a blanket lock authored here "has no .psd equivalent and was not written". |
| Layer colour labels | **Unsupported (explicit)** | Not shown by this layers panel and not kept — named. |
| Pass-through + blend mode on one group | **Appearance fallback** | A `.psd` "stores only the" pass-through; the blend mode is named as lost. |
| Files with no layers / no flattened image | **Explicit** | Notes state the flattened image became one layer, or the canvas is empty. |

## Export matrix (`psd::write` — the back-trip)

| Feature | Outcome | Notes |
|---|---|---|
| Layer tree, groups, order, names, bounds, visibility | **Editable** | Round-trips. |
| Blend mode, opacity, fill opacity, clipping | **Editable** | |
| Raster pixels, layer masks (8-bit coverage) | **Editable** | |
| Invert adjustment | **Editable** | |
| Editable text (TySh) | **Not written (explicit)** | Card 079: a synthesized `TySh` needs a complete engine-data payload; a partial one makes Photoshop discard the whole layer (worse than the honest raster fallback below). Blocked on independent-editor verification of a synthesized payload. |
| This editor's Text / Shape / SmartObject layers | **Appearance fallback** | Card 078: the layer is rendered alone through the compositor (its real transform, mask detached so the mask channel cannot double-apply) and the rendered pixels are written as the record's channels — an independent reader shows the layer. The note names the fallback. |
| Drop shadow, stroke (solid), colour overlay, outer glow | **Editable** | Card 080: written as a real `lfx2` descriptor block (the exact inverse of the import mapping) — an independent reader can toggle and restyle them. Fallback-rendered layers strip effects from their baked pixels so nothing draws twice. |
| Inner shadow / inner glow / bevel / satin / gradient & pattern overlays, non-solid effect fills | **Unsupported (explicit)** | Not written; named per effect kind by the export note. |
| Mask density/feather authored here | **Unsupported (explicit)** | Not written; named. |
| Arbitrary per-layer affines | **Appearance fallback** | Pixels are written at their stored locations (the note above). |

## Tally notes (verbatim)

The import's status notes, verbatim from `Tally::record`
(`{names}` is the affected layers, two shown then "and N more") — the
honesty gate in `import.rs` asserts this file keeps quoting every one:

- "the colour label on {names} is not shown by this layers panel and was not kept"
- "adjustment layer(s) this build cannot evaluate ({names}) were kept as empty layers; their effect is in the flattened image but not editable"
- "type layer(s) ({names}) were imported as pixels; the text is no longer editable"
- "type layer(s) ({names}) were imported as editable text with the default font, size and fill — the source font is not in this build's supported subset"
- "layer effect(s) on {names} were not imported"
- "the {kinds} effect(s) on {names} were not imported" ({kinds} names the unmapped effect kinds, e.g. "satin and inner shadow")
- "{names} carried a second, vector-derived mask that was not imported"
- "{names} carry a transform a .psd cannot express; their pixels were written where they are stored"
- "text, shape and smart-object layer(s) ({names}) cannot stay editable in a .psd; their rendered appearance was written as a raster layer's pixels"
- "the mask density or feather on {names} was not written"
- "the vector mask on {names} was written as its rasterised coverage"
- "the blanket lock on {names} has no .psd equivalent and was not written"
- "{names} pass through *and* carry a blend mode; a .psd stores only the pass-through"

## Failure policy

1. **Nothing silent.** Every fallback and refusal pushes a status note
   naming the affected layers (two shown, then "and N more"); resources
   the document cannot carry are counted and named as left behind:
   "{n} image resource(s) — guides, paths, the colour profile — are not part of this document model and were left behind".
   When a profile is embedded and retained, "the colour profile" is
   dropped from that list; when no profile is embedded, or the embedded
   one is measurably sRGB (treated as sRGB, bytes redundant), the wording
   above stands.
2. **No invented numbers.** A payload this build cannot evaluate is
   never decoded into guessed parameters ("inventing one would put the
   wrong numbers behind a slider").
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
  vector mask became coverage), or *unsupported* (the layer arrived
  empty — a kind with no home here, or an adjustment this build cannot
  evaluate) — each with the reason;
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
document model grows the vocabulary (e.g. effect descriptors, a type
decoder); a fallback becomes explicit only when the status note names
it. Update this file in the same commit that changes either side.
