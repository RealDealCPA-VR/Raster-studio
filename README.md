# Raster Studio

**Your images, on your disk, in pixels you can trust.**

Raster Studio is a local-first image editor written in Rust — layers, masks,
selections, filters, text, vector paths and a tile engine built to chew on
large documents without breaking a sweat.

No account. No cloud. No telemetry. No network calls. It opens a file from
your disk, does the work in parallel Rust on your own CPU, and writes a file
back. What happens between those two moments is yours alone.

```bash
cd raster-studio
cargo run -p studio-desktop -- path/to/image.png
```

![Raster Studio's main window: a document on the flat pasteboard, the tool
column with its footer wells, tabbed dock panels and the status
bar](raster-studio/docs/main-window.png)

## The idea: one engine, no drift

Most image editors are either a web app you rent or a native app written in
1990. Raster Studio splits the difference in a way that is easy to say and
rare to pull off: **the pixel work is a pure CPU engine, and the GPU's only
job is to put the result on screen.**

Because the engine never needs a GPU to be *correct*, every pixel operation —
all 27 blend modes, every filter and adjustment, masking, clipping,
compositing, export — is an ordinary function that runs headlessly in tests.
There is exactly **one** implementation of each, so there is no CPU/GPU pair
sitting around waiting to disagree with each other. What the engine computes,
the screen shows; what the tests pin down, you get.

## Status

**This is a work in progress — an engine that is complete and battle-tested
wrapped in an application that is largely wired, with the corners still marked
out honestly.** The full spec in
[`docs/REMAINING.md`](raster-studio/docs/REMAINING.md) — the plan-implement-
validate todo list this project is built against — is **complete**: every item
was implemented, tested and reachable from the UI before it was checked off.
The row-by-row detail lives in
[`docs/parity-matrix.md`](raster-studio/docs/parity-matrix.md). Nothing there
is marked done unless it is implemented, tested and reachable — and where that
bar is not yet met, it says so in plain words. Documentation here is a claim,
and claims get checked against the code.

### What you can actually do in the app

- **Open almost anything.** PNG, JPEG, WebP, TIFF, GIF, BMP, ICO, TGA and
  **layered `.psd`** — the PSD reader lowers groups, masks, blend modes,
  channels and adjustments into a real document, and reports anything it
  cannot carry over. Pan, zoom, fit, all of it.
- **Paint.** Brush, pencil, eraser, clone, healing, gradient, bucket,
  dodge/burn, blur/sharpen/smudge and the rest mark the canvas. A stroke is
  one undo step that restores the prior pixels byte-for-byte — and with a
  stylus, pressure is wired end to end through the shell seam.
- **Draw and typeset.** Pen and shape tools make real vector layers; the type
  tool lays out text with real shaping.
- **Build a layer stack.** Groups, layer masks, clipping masks, adjustment
  layers, opacity and fill, all 27 blend modes — and layer effects (drop
  shadow, glows, satin, gradients) that the compositor really renders, with
  thumbnails that really show the pixels. Add **Solid Color and Gradient**
  fill layers, and **rasterize** any layer (text, shape, style, smart object)
  — or flatten the whole document to one layer.
- **Edit non-destructively.** Adjustment layers, filters and Image ▸
  Adjustments run against the live document and stay undoable. The Channels
  panel lets you isolate *and* paint into a single colour component. Resize
  the canvas undoably — **Crop to Selection**, **Trim** the transparent
  margin, or **rotate 90°** — each a single undoable step.
- **Make selections** that constrain painting and draw real marching ants;
  crop, slice and free-transform with real, undoable edits — and **Fill** or
  **Stroke** them with the foreground colour in one undoable step. Save a
  selection and bring it back with **Reselect / Save / Load Selection**.
- **Smart objects.** Convert a layer to a smart object, then open its contents
  in an embedded-document tab, edit them as a raster and commit them back as a
  single undo step.
- **Save and reopen** the native `.rstudio` package — composites to
  byte-identical output after a round trip — and export PNG, JPEG, WebP, TIFF,
  GIF, BMP, layered PSD, and **print as PDF**, with 16-bit sources honoured
  all the way out. Duplicate any document with one click, or **Close All**
  with the unsaved-changes prompt answered per document.

### What is honestly still remaining

Complete does not mean finished, and this project refuses to blur the line.
The short, named, current list:

- **Live editing runs at 8-bit precision.** Deep sources round-trip and export
  at the full 16 bits, and `.rstudio` records each layer's depth, but the live
  tiles composite at 8-bit-equivalent precision in-app (P2.5b in the
  production todo, open).
- **Native tablet events need a pen.** Pressure is wired through the shell
  seam and verified; actually subscribing to one device's winit tablet events
  needs hardware on the host.
- **The OS printer-spooler dialog** (Print ▸ As PDF is fully implemented).
- **Deferred Tier C rows** (documented with reasons in the parity matrix, not
  silently dropped): quick-mask *editing* refinements beyond the landed
  compose-and-paint loop, per-channel editing, a recording UI for Actions,
  batch export, paths, and Select Subject (no segmentation model ships, and
  the menu carries no item for it). No menu item in this build is disabled
  with a vague reason: every enabled item routes to real code, and the only
  refusal left in `unavailable_reason` besides the File-Info note is an
  adjustment clicked while its parameters still sit at the identity — the
  status line tells you to add it as an adjustment layer instead.

[`raster-studio/docs/parity-matrix.md`](raster-studio/docs/parity-matrix.md)
has the row-by-row detail. Nothing there is marked done unless it is
implemented, tested and reachable from the UI — and where that bar is not
met, it says so.

## Building

Requires a Rust toolchain (1.82 or newer) and, on Windows, the MSVC build tools.

```bash
cd raster-studio
cargo check --workspace --all-targets   # type-check everything
cargo test  --workspace                 # ~3400 tests
cargo run   -p studio-desktop           # launch
```

On Linux you need a Vulkan- or GL-capable environment for the window. GPU-backed
tests detect the absence of an adapter and skip themselves, so a headless CI
runner stays green.

Want a screenshot of the running app? The desktop binary can capture one of
its own frames: `studio-desktop --shot out.png image.png` renders a frame,
reads it back to the CPU and writes `out.png` — verified to produce a real
1440×900 PNG of the live GUI.

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
| `crates/color` | Colour spaces, conversions and the ICC matrix-shaper engine |
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
