# Raster Studio

A local-first image editor written in Rust. Layers, masks, selections, filters,
text, vector paths, and a tile-based engine built for large documents.

No account, no cloud, no telemetry, no network calls. It opens a file from your
disk and writes one back.

```bash
cd raster-studio
cargo run -p studio-desktop -- path/to/image.png
```

## What it is

Most image editors are either a web app you rent or a native app that was
written in 1990. Raster Studio is a native desktop application with a CPU tile
engine and a GPU presentation layer: the pixel work happens in parallel Rust,
and the GPU's job is to put composited tiles on screen at display rate.

That split is the central design decision, and it buys something specific.
Because the engine never needs a GPU to be *correct*, every pixel operation —
all 27 blend modes, every filter, every adjustment, masking, clipping,
compositing, export — is an ordinary function that can be tested headlessly.
There is exactly one implementation of each, so there is no CPU/GPU pair to
drift apart.

## Status

**This is a work in progress, not a finished product — but it is well past the
scaffold.** The engine is complete and heavily tested, and the application on
top of it is largely wired: every engine capability below is reachable from the
UI, not merely present as a library. The honest list of what is *still* missing
is a short, named one, and [`docs/REMAINING.md`](raster-studio/docs/REMAINING.md)
keeps it current against the code.

### What you can actually do in the app

- Open PNG, JPEG, WebP, TIFF, GIF, BMP, ICO, TGA **and layered `.psd`** (the PSD
  reader lowers groups, masks, blend modes, channels and adjustments into a real
  document, reporting what cannot carry over), and pan, zoom and fit.
- **Paint.** Brush, pencil, eraser, clone, healing, gradient, bucket,
  dodge/burn, blur/sharpen/smudge and the rest of the tools mark the canvas. A
  stroke is one undo step, and undo restores the prior pixels byte-for-byte.
- **Draw and typeset.** Pen and shape tools produce real vector layers, and a
  type tool creates text layers with real shaping.
- Build a layer stack: groups, layer masks, clipping masks, adjustment layers,
  opacity and fill, all 27 blend modes, **and layer effects (drop shadow,
  glows, satin, gradients) that the compositor really renders**.
- **Edit non-destructively.** Adjustment layers, filters, and every Image ▸
  Adjustments operation apply against the live document and are undoable.
- Make selections, which constrain later painting and draw real marching ants.
- Crop, slice, and free transform — gestures apply real, undoable edits.
- Save and reopen the native `.rstudio` package — it composites to
  byte-identical output after a round trip.
- Export PNG, JPEG, WebP, TIFF, GIF and BMP, **and layered PSD**, with a correct
  colour pipeline.

### What is honestly still missing

These are the gaps, named rather than blurred. Each is tracked in
[`docs/REMAINING.md`](raster-studio/docs/REMAINING.md):

- **Channel editing** — the Channels panel isolates a component (really changes
  the canvas) but tools still paint all three components.
- **Smart objects** — the layer kind exists but nothing renders it.
- **16-bit export** — 16-bit sources now export at 16 bits to the formats that
  carry them (PNG/TIFF), but in-app editing still composites at 8-bit-equivalent
  precision.
- **Tablet pressure** — the engine consumes it, but egui 0.29 carries none, so
  the shell must still feed the native tablet stream.
- **Guides are view state** — not saved with the document and not undoable.
- **Layer/history thumbnails** show kind glyphs, not the pixels.
- **Embedded ICC profiles** are preserved but not applied to a working space.
- A minority of menu items are still drawn disabled with a named reason
  (Print, File Info, and the handful that need a dialog surface this build has
  not drawn). The count is pinned by a test.

[`raster-studio/docs/parity-matrix.md`](raster-studio/docs/parity-matrix.md) has the row-by-row detail.
Nothing there is marked done unless it is implemented, tested, and reachable
from the UI — and where that bar is not met, it says so.

## Building

Requires a Rust toolchain (1.82 or newer) and, on Windows, the MSVC build tools.

```bash
cd raster-studio
cargo check --workspace --all-targets   # type-check everything
cargo test  --workspace                 # ~2900 tests
cargo run   -p studio-desktop           # launch
```

On Linux you need a Vulkan- or GL-capable environment for the window. GPU-backed
tests detect the absence of an adapter and skip themselves, so a headless CI
runner stays green.

## Layout

| Crate | What it owns |
| --- | --- |
| `apps/studio-desktop` | The executable |
| `crates/app-shell` | Window, event loop, editor state, keymap, files, autosave |
| `crates/ui` | Menus, panels, canvas, dialogs, tool options |
| `crates/design` | The design system: tokens, theme, widgets |
| `crates/editor-core` | Document, commands, history, selection |
| `crates/layer-model` | Layer tree, blend modes, masks, effects |
| `crates/compositor` | The authoritative CPU tile compositor |
| `crates/raster` | Tiles, mipmaps, codecs, export |
| `crates/color` | Colour spaces and conversions |
| `crates/selection` | Selection algorithms |
| `crates/adjustments` | Adjustment operations |
| `crates/filters` | The filter library |
| `crates/tools` | Brush engine and the tool set |
| `crates/vector` | Bézier paths and rasterisation |
| `crates/text-engine` | Shaping, layout, glyph rasterisation |
| `crates/project-format` | The `.rstudio` package |
| `crates/asset-store` | Content-addressed blob storage |
| `crates/psd` | PSD read and write |
| `crates/render` | wgpu presentation |
| `crates/licensing`, `crates/updater`, `crates/telemetry` | Supporting pieces |

The layering is enforced by dependency direction: `layer-model`, `color` and
`vector` are leaf domain crates with no I/O; `compositor` is a pure function
from document to pixels; `render` owns all wgpu; `project-format` owns all
persistence; and `ui` never mutates the document — it emits commands, so undo
and redo behave the same no matter which control produced the edit.

## Contributing

Two rules carry most of the weight here:

1. **A test that passes against the unfixed code is not a test.** Break the
   thing you fixed and watch the test go red before you believe it.
2. **Do not write prose asserting behaviour the code does not have.** This
   project was rebuilt from a scaffold whose documentation described a working
   editor that did not compile. Documentation is a claim, and claims get
   checked.

CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets` with
`-D warnings`, `cargo test --workspace` on Linux and Windows, and `cargo audit`.

## Licence

See [`raster-studio/LICENSES/`](raster-studio/LICENSES/).
