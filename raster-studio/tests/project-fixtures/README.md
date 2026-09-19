# Project fixtures

Deterministic, generated composition fixtures for the thumbnail workflow
(plan Task 003). Every file here is produced by code, never sampled:

```text
cargo test -p integration-tests --test thumbnail_workflow -- --ignored --nocapture materialize_full_size_assets_and_project
```

The generators live in `tests/integration/src/fixture/thumbnail.rs`
(`background_rgba8`, `oversized_source_rgba8`, `logo_rgba8`,
`portrait_rgba8`, `portrait_mask_coverage`, `build_scene`, `write_assets`).
Re-running the command above reproduces these bytes exactly (the saved
*package's* `document.msgpack` differs between runs only by the random
layer/mask UUIDs; the four images are byte-stable).

## Files

| File | Size | Content | FNV-1a 64 |
|---|---|---|---|
| `thumbnail-background.png` | 3628 × 2041, fully opaque, vertical gradient (top stop `16,24,36`, bottom stop `40,52,64`) | `background_rgba8` | `b77518300ecf6b9b` |
| `thumbnail-oversized.png` | 5442 × 3628 (1.5 × canvas width, 16/9 × height — strictly larger than the canvas on both axes), opaque, labelled corners TL red `200,32,32` / TR green `32,190,70` / BL blue `40,96,230` / BR yellow `235,205,45`, white centre dot, x- and y-ramps differ | `oversized_source_rgba8` | `554b804f03a4e141` |
| `thumbnail-logo.png` | 512 × 512 (4h/9 side), RGBA: opaque teal ring `30,160,170,255`, fully transparent centre hole (r < 0.22 × side), transparent wedge notch (20°–90° screen angle), feathered outer edge (0.48 × side), strictly partial-alpha band between | `logo_rgba8` | `a545943c30300bad` |
| `thumbnail-portrait.png` | 1361 × 1814 (3w/8 × 8h/9), RGBA: synthetic head-and-shoulders silhouette — opaque interior, transparent exterior, ~2.5 px feathered boundary. **Synthetic coverage: it is not a photograph and proves nothing about real hair/glasses extraction.** | `portrait_rgba8` | `bb94cb7186395215` |
| `thumbnail-scene-3628x2041.rstudio/` | native package of the full-size assembled scene | `build_scene(3628, 2041)` | per-file hashes printed by the materialize run (layer UUIDs make `document.msgpack` run-unique) |

## The assembled scene (`build_scene`)

Root stack, top to bottom: `Alternatives` (group) → `Portrait tone`
(adjustment, `ClipToBelow`) → `Portrait` (raster + raster mask) → `Headline`
(text, DejaVu Sans, positioned by layer transform) → `Subhead` (text) →
`Background` (raster). `Alternatives` holds, top to bottom: `Headline A`
(text), `Logos` (nested group holding `Logo`), `Headline B` (text, hidden).
The portrait's mask coverage is a horizontal ramp across its width —
deliberately different from its own alpha.

The small CI scene is the same builder at 256 × 144; both sizes share the
proportion table in `layout()`.

## Rules

- Fixtures are generated from code, never committed from personal photos or
  installed fonts. The text fixture font is the embedded DejaVu Sans
  (`dejavu` crate).
- Composites are byte-deterministic within a process; cross-machine byte
  identity is not claimed (installed-font seeding may change glyph
  resolution), while the four image files are byte-deterministic everywhere.
- Re-generate rather than hand-editing: no file here is a source of truth.
