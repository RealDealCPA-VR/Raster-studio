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
| Raster layer pixels (8-bit, incl. RLE/ZIP channels) | **Editable** | Tiles stored at the layer's own bounds; part outside the canvas is dropped and named — "the part outside it was not kept". |
| Layer masks (8-bit coverage) | **Editable** | Imported as raster mask coverage at the mask's bounds, honouring the default colour. |
| Mask density / feather parameters | **Appearance fallback** | Coverage is kept; the parameters are not modelled — "the mask density or feather on … was not written". |
| Vector masks | **Appearance fallback** | Written as their rasterised coverage — "the vector mask on … was written as its rasterised coverage". A second, vector-derived mask "was not imported" by name. |
| Adjustment layers — **Invert** (`nvrt`) | **Editable** | The one adjustment whose whole definition is its name. |
| Adjustment layers — everything else | **Unsupported (explicit)** | The payload survives in `psd`'s model but this build will not invent slider values: the layer is kept empty and named — "adjustment layer(s) … were kept as empty layers; their effect is in the flattened image but not editable". |
| Type layers — parseable `Txt ` string | **Editable (reported substitution)** | The string imports as an editable `LayerKind::Text` under the `TySh` transform (any affine). The format carries no font/size/fill outside the engine data, so the editor's new-text defaults are used and named — "type layer(s) … were imported as editable text with the default font, size and fill — the source font is not in this build's supported subset". The raw `TySh` bytes stay in the model and survive a save verbatim. |
| Type layers — unparseable (`Txt ` missing or unreadable) | **Appearance fallback** | Pixels are imported; the text is not editable — "type layer(s) … were imported as pixels; the text is no longer editable". |
| Layer effects (drop shadow, stroke, …) | **Unsupported (explicit)** | Not imported — "layer effect(s) on … were not imported". (This editor's own effects are card 066/067 features; PSD effect descriptors are a separate decoder.) |
| Embedded ICC profile | **Unsupported (explicit)** | `psd::read` keeps resources as opaque bytes and the import leaves the profile behind by name — “… the colour profile — are not part of this document model and were left behind”. (Card 047's keep-and-retag contract is the generic raster-codec path, not `psd::read`.) |
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
| This editor's Text / Shape / SmartObject layers | **Appearance fallback** | Written as their pixels — "… were written as empty layers" when there is nothing to write (the same no-home note). |
| Effects this editor authored | **Unsupported (explicit)** | Not written; the note names the layers. |
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
- "{names} carried a second, vector-derived mask that was not imported"
- "{names} extend past the canvas; the part outside it was not kept"
- "{names} carry a transform a .psd cannot express; their pixels were written where they are stored"
- "{names} are a kind a .psd has no home for and were written as empty layers"
- "the mask density or feather on {names} was not written"
- "the vector mask on {names} was written as its rasterised coverage"
- "the blanket lock on {names} has no .psd equivalent and was not written"
- "{names} pass through *and* carry a blend mode; a .psd stores only the pass-through"

## Failure policy

1. **Nothing silent.** Every fallback and refusal pushes a status note
   naming the affected layers (two shown, then "and N more"); resources
   the document cannot carry (guides, paths, the colour profile) are
   counted and named as left behind.
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

## What would change rows

The rows move only with code: an editable row becomes possible when the
document model grows the vocabulary (e.g. effect descriptors, a type
decoder); a fallback becomes explicit only when the status note names
it. Update this file in the same commit that changes either side.
