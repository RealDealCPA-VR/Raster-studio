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

## What works today

- **Documents.** Open PNG, JPEG, WebP, TIFF, GIF, BMP, ICO and TGA; open and
  save layered PSD; save and reopen the native `.rstudio` package with pixels,
  masks, history and selections intact.
- **Layers.** A real tree with groups, layer masks, clipping masks,
  non-destructive adjustment layers, layer effects, and all 27 Photoshop blend
  modes including the four non-separable ones.
- **Selections.** Marquee, lasso (freehand, polygonal, magnetic), magic wand,
  quick select and colour range, with per-pixel fractional coverage — feather,
  expand, contract, smooth and border are real morphology, not rectangle maths.
- **Tools.** A stamp-based brush with hardness, spacing, flow, opacity and
  pressure; eraser, clone, healing, patch, red-eye, gradient, bucket,
  blur/sharpen/smudge, dodge/burn/sponge, shapes, crop, and free transform with
  scale, rotate, skew, distort, perspective and a warp mesh.
- **Adjustments.** Levels, Curves (a monotone cubic spline, not line segments),
  Exposure, Brightness/Contrast, Vibrance, Hue/Saturation, Colour Balance,
  Black & White, Photo Filter, Channel Mixer, Invert, Posterize, Threshold,
  Gradient Map, Selective Colour, and the Auto commands.
- **Filters.** Blur (separable Gaussian, box, motion, radial, lens, surface),
  sharpen, noise, distort, stylize, pixelate, render and custom convolution.
- **Text.** Real shaping through `cosmic-text` — bidi, ligatures, kerning and
  contextual forms — with point and paragraph text and per-character styling.
- **Vector.** Cubic Bézier paths, anti-aliased fill under both winding rules,
  stroke-to-outline with caps/joins/dashes, boolean ops, and SVG path I/O.
- **Colour.** A linear premultiplied working space, sRGB/Display P3 dispatch,
  HSL/HSV/CIELAB, and an export pipeline that converts back correctly.
- **Undo.** Every edit is an invertible command. A brush stroke touching a
  hundred tiles is one undo step, and undo restores the prior pixels exactly.

See [`docs/parity-matrix.md`](docs/parity-matrix.md) for the feature-by-feature
status, including what is deliberately **not** done yet. That file is kept
honest: nothing is marked done unless it is implemented, tested, and reachable
from the UI.

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
