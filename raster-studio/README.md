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

**This is a work in progress, not a finished product.** The engine is largely
complete and heavily tested; the application on top of it is partly wired. The
split matters, so it is spelled out rather than blurred.

### What you can actually do in the app

- Open PNG, JPEG, WebP, TIFF, GIF, BMP, ICO and TGA, and pan, zoom and fit.
- **Paint.** Brush, pencil, eraser, clone, gradient, bucket, dodge/burn and the
  rest of the stamp-based tools mark the canvas. A stroke is one undo step, and
  undo restores the prior pixels byte-for-byte.
- Build a layer stack: groups, layer masks, clipping masks, adjustment layers,
  opacity and fill, and all 27 blend modes, all composited correctly.
- Edit adjustment-layer parameters and see the result live.
- Make selections, which constrain later painting.
- Save and reopen the native `.rstudio` package — it composites to
  byte-identical output after a round trip.
- Export PNG, JPEG, WebP, TIFF, GIF and BMP with a correct colour pipeline.

### What exists in the engine but is not reachable from the UI yet

These are implemented and tested as libraries, and the application does not yet
have a route to them:

- **Filters** — the whole library (blur, sharpen, noise, distort, stylize,
  pixelate, render, convolution). The Filter menu is drawn but not wired.
- **PSD read and write** — `crates/psd` is complete and is not linked into the
  application.
- **Text** — shaping, layout and rasterisation work; there is no type tool to
  create a text layer with.
- **Vector paths** — the geometry and rasteriser work; there is no pen tool.
- **Crop, slice and free transform** — the gestures track correctly but nothing
  applies them.
- **Selection overlay** — a selection changes the document but draws no
  marching ants, so it is currently invisible.
- **Layer effects** — the data model, editor and persistence are complete; the
  compositor does not render them.
- Roughly half the menu items are drawn disabled, marked "This build cannot do
  that yet".

[`docs/parity-matrix.md`](docs/parity-matrix.md) has the row-by-row detail.
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

See [`LICENSES/`](LICENSES/).
