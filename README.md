# Raster Studio

**Your images, on your disk, in pixels you can trust.**

Raster Studio is a local-first image editor written in Rust — layers, masks,
selections, filters, text, vector paths and a tile engine built for large
documents. Its target is feature parity with Photopea for real editing work.

No account. No cloud. No telemetry. The application's own code makes no
network calls; the only thing that reaches the network is your web browser,
which the Help menu opens at fixed GitHub pages. It opens a file from your
disk, does the work in parallel Rust on your own CPU, and writes a file back.

```bash
cd raster-studio
cargo run -p studio-desktop -- path/to/image.png
```

![Raster Studio's main window with a 3628×2041 layered project open at 20.8%:
the menu bar and the Move tool's options bar across the top, the tool column
on the left with the colour wells at its foot, rulers along the top and left of the canvas, a
narrow dock column (Navigator, Color, Brushes) beside a wide one (Properties,
History, and a Layers panel listing groups and a masked layer), and the status
bar](raster-studio/docs/main-window.png)

## The idea: one engine, no drift

**The pixel work is a pure CPU engine, and the GPU's only job is to put the
result on screen.**

Because the engine never needs a GPU to be *correct*, every pixel operation —
all 27 blend modes, every filter and adjustment, masking, clipping,
compositing, export — is an ordinary function that runs headlessly in tests.
There is exactly **one** implementation of each, so there is no CPU/GPU pair
to disagree with each other. What the engine computes, the screen shows.

## Status

**A work in progress: a tested engine inside an application that is largely
wired, with the gaps listed below by name.** Nothing is claimed here unless it
is implemented, tested and reachable from the UI. Documentation is a claim,
and claims get checked against the code.

Six fix waves have landed on `main` since the last spec was closed (`53dd398`,
`2caaa6c`, `02c7e1b`, `1c5b727` + `7bb295a`, `b477a09`, `0e4a6fd`); the
[CHANGELOG](CHANGELOG.md) says what each changed. The row-by-row state lives
in [`docs/parity-matrix.md`](raster-studio/docs/parity-matrix.md).
[`docs/REMAINING.md`](raster-studio/docs/REMAINING.md) is the historical
2026-08 spec; all its items are closed and it no longer describes the code.

### What you can do in the app

- **Open** PNG, JPEG, WebP, TIFF, GIF, BMP, ICO and TGA, and layered `.psd`
  (groups, masks, blend modes, channels, adjustments, the mapped effects, an
  editable-text subset, with a per-layer report of what did not map). File ▸
  Open decodes on a background thread; drag-and-drop, recent files and a
  file named on the command line open on the UI thread. Drag-and-drop, recent files, and a start
  screen with New / Open / Templates and a recent-files grid. File ▸ Open also
  opens a `.rstudio` project when you pick the `manifest.json` inside it.
- **Work in Photopea's layout:** the menu bar, a full-width options bar,
  document tabs over the canvas, and two dock columns holding 15 panels
  (Layers, Channels, Paths, Properties, Adjustments, History, Navigator, Info,
  Histogram, Color, Swatches, Brushes, Character, Paragraph, Actions). The
  document is fitted and drawn inside the canvas area, between the tool column,
  the docks and the bars. Workspaces: Essentials, Painting, Photography,
  Minimal. F cycles the screen modes. Light and dark themes.
- **Navigate:** pan, zoom (to the cursor, 100%, 200%), fit, rotate view (with
  Reset View Rotation) and flip view. Rulers in any unit, guides (saved with
  the document), smart guides, grid, pixel grid at 800% and above, snapping,
  layer and selection edges, precise cursor.
- **Paint** with the 55 tools of a grouped, Photopea-ordered palette (56
  counting Free Transform, which is `Ctrl+T` and a menu item, not a button),
  including brush, pencil (Auto Erase), eraser, background and magic eraser;
  clone and pattern stamp, history brush; healing, spot healing, patch, red
  eye, colour replacement; gradient (editable stops), paint bucket, pattern
  fill; blur, sharpen, smudge, dodge, burn and sponge. A stroke shows while you
  drag and commits as one undo step; a brush-size ring and per-tool cursors
  follow the pointer. Brush presets and swatches persist.
- **Select** with the marquees, lasso, polygonal and magnetic lasso, magic
  wand and quick select; Color Range, Modify (border, smooth, expand, contract,
  feather), Grow, Similar, Refine Edge, Transform Selection, quick mask, and
  Save / Load / Reselect. Gestures draw live, show marching ants and are
  undoable.
- **Draw and type:** the Pen drags out smooth curves (Alt breaks a handle);
  add, delete and convert anchor points; path and direct selection. Shape
  tools, including a custom-shape library, with fill, stroke, width and corner
  radius. The Paths panel fills, strokes, loads as a selection and makes a
  path from a selection. The Type tool shapes text for real; click back into a
  text layer to edit it; switching tools or pressing Escape keeps what you
  typed. Character: kerning, scale, baseline shift, caps. Paragraph: indents
  and justification.
- **Build a layer stack:** groups, masks (a new mask is made from the
  selection and becomes the paint target), clipping masks and all 27 blend
  modes; opacity and fill, locks, rename, align / distribute, Stamp Visible;
  Solid Color and Gradient fill layers, and a Pattern fill layer (tiled as
  pixels from the last Define Pattern); adjustment layers edited in
  Properties; layer styles (ten effects, all of which render on the canvas;
  Pattern Overlay tiles a pattern picked from the ones Define Pattern made,
  saved with the document — plus
  Blending Options), copy / paste style and style presets; smart objects
  (embedded or linked; edit the contents, then commit) with smart filters —
  Filter ▸ Convert for Smart Filters, then a filter picked from the Filter
  menu's filter rows joins the object's stack instead of rewriting its pixels
  (the Filter Gallery, Last Filter, Liquify and Puppet Warp stay greyed over
  a smart object): each shows as a row under the layer with an eye, a delete
  button and double-click to re-open its dialog at its stored settings (the
  object need not be the active layer), each change is one undo step, and
  the stack saves with the project (a PSD export writes the filtered look as
  pixels); rasterize,
  merge, flatten.
- **Adjust:** the 22 Image ▸ Adjustments. Nineteen open a live-preview
  dialog: Brightness/Contrast, Levels (with its histogram), Curves (a
  draggable curve over the histogram, per channel), Exposure, Vibrance,
  Hue/Saturation, Color Balance, Black & White, Photo Filter, Channel Mixer,
  Posterize, Threshold, Gradient Map, Selective Color, Shadows/Highlights,
  HDR Toning (local-adaptation tone mapping: a guided-filter base/detail split
  of log luminance with edge-glow radius and strength, gamma, exposure,
  detail, vibrance, saturation), Match Color (a mean/deviation transfer in
  CIELAB from any open document's merged image or every other pixel layer, with
  luminance, colour intensity, fade and neutralize), Replace Color and Color
  Lookup (a `.cube` file or five built-in looks).
  Desaturate, Equalize and Invert ask nothing and apply at once. Auto Tone,
  Auto Contrast and Auto Color are separate items.
  Ctrl+L / M / U / B / I reach Levels, Curves, Hue/Saturation, Color Balance
  and Invert.
- **Filter:** 55 filters in eight groups, each with a live-preview dialog,
  plus Filter Gallery and Last Filter. Filters respect the selection and an
  isolated channel.
- **Transform and crop:** Free Transform (scale, rotate, skew, distort,
  perspective, warp); with a partial pixel selection, Free Transform and Move
  lift just the selected pixels, as one undo step. Crop with ratio presets,
  W × H × resolution, straighten and Delete Cropped Pixels. Trim, Crop to
  Selection, Reveal All, Image Size, Canvas Size, rotate or flip the canvas.
  The Ruler tool straightens a layer; a four-point Color Sampler reads values.
- **Channels:** isolate one RGB component, then paint, erase, fill, filter or
  bake an adjustment into it alone.
- **16-bit:** create a 16-bit document, or convert with Image ▸ Mode (one
  undoable step). It composites at 16 bits, saves and reopens as `.rstudio`
  with its 16-bit tiles, and exports 16-bit PNG and TIFF. Filters,
  adjustments, Fill, Clear, Free Transform, every flip and rotation, Image
  Size, Canvas Size, Crop to Selection and Trim compute at 16 bits; a few
  edits still round to 8 bits (see below).
- **Edit:** Cut, Copy, Copy Merged, Paste, Paste in Place, Paste Into, Paste
  Outside, Clear (Delete or Backspace); Fill and Stroke dialogs, with
  Alt+Backspace / Ctrl+Backspace filling with the foreground / background
  colour; Fill ▸ Contents: Content-Aware (PatchMatch synthesis from the rest
  of the layer, also the Spot Healing Brush's Content-Aware type) and Edit ▸
  Content-Aware Scale (seam carving, 80/90/110/125% steps per axis); Define
  Pattern and Define Brush; Step Forward / Backward; a History
  panel with thumbnails and a source marker; Purge; customisable keyboard
  shortcuts that apply at once; a right-click canvas menu.
- **Record and replay** actions in the Actions panel; named actions show their
  steps and save to disk.
- **Save** the native `.rstudio` package: compressed tiles, unchanged tiles
  reused, written off the UI thread, with autosave and crash recovery
  (commands made while a save runs are journaled beside the package, so a
  crash during a save loses none of them).
- **Export** PNG, JPEG, WebP (lossless), TIFF, GIF, BMP, ICO and SVG (the
  raster embedded as a PNG), with presets and several presets in one run;
  Export Layers and File ▸ Export ▸ Slices; layered PSD (8-bit); print as PDF.
  Duplicate a document, Close Others, Close All.

### What is still missing vs Photopea

**Absent, and deferred for this release** — the Tier C rows of the parity
matrix, each with its reason there:

| Missing | Why |
| --- | --- |
| ICC-accurate CMYK, spot colours, Lab files, L/a/b in Levels/Curves | Since W7-D: Image ▸ Mode ▸ Lab / CMYK / Indexed convert (one undo step each; CMYK on a documented naive ink model, not an ICC press profile; Indexed through its own dialog); File ▸ Export and Export As write a CMYK document as CMYK JPEG/TIFF and an Indexed one as a palette PNG (GIF keeps its colours); Export As says when a format writes the document as RGB instead (always, for Lab); Info adds a Lab or CMYK row for a document in that mode and the Color panel has a CMYK notation beside Lab; View ▸ Proof Colors and Gamut Warning are enabled and change the canvas. Still missing: a press profile and spot colours, any Lab file (Lab goes out as RGB, and the dialog says so), L/a/b channels in Levels/Curves, and Indexed flattening (semi-transparent layers can still blend colours outside the palette). |
| Select Subject, Object Selection | No segmentation model ships. |
| Vanishing Point | Perspective-plane tooling. (Liquify and Puppet Warp now exist: Filter ▸ Liquify… and Edit ▸ Puppet Warp, each applied as one undo step; their gaps are in the parity matrix.) |
| Lighting Effects | Needs on-canvas light handles the parameter dialog cannot express. |
| Camera RAW, PDF / AI import, Sketch / XD / Figma | Per-sensor demosaic; a PDF interpreter; proprietary formats. |
| Video and timeline; collaboration, cloud, mobile | Out of scope; non-goals. |

**Absent, and not yet decided:** Content-Aware Move, a separate Slice
Select tool; Lens
Correction, Adaptive Wide Angle and the Blur Gallery (Field / Iris /
Tilt-Shift); Layer Comps and Tool Presets panels; File ▸ Automate / Batch and
Scripts; XMP in File Info; vector masks (PSD import rasterises them); an
external map picker for Displace; lossy WebP (kept lossless on purpose — no
pure-Rust lossy encoder has passed evaluation yet, see the parity matrix).

**Known gaps in what exists:**

- **The W7-F tools have named limits.** Perspective Crop, Vertical Type, the two
  Type Masks, Mixer Brush, Artboard, Curvature Pen and Freeform Pen are palette
  tools now. Each is one undo step except Vertical Type, which is two like the
  Type tool (the click's empty layer, then the confirmed run). Perspective Crop
  rectifies the active raster layer only; vertical type is upright glyphs in
  columns (no rotated Latin, no vertical punctuation forms, horizontal caret
  geometry); the Mixer Brush has no live preview while
  painting; artboards do not clip their contents and File > Export Artboards does
  not exist. The parity matrix has the details.
- **Stylus pressure is verified with synthetic events only.** winit's `Touch`
  events (Windows `WM_POINTER` pens and fingers) now drive the same pointer
  route as the mouse, with the force as the stroke's pressure
  (`app-shell/src/pen_input.rs` through `Shell::set_pen_pressure`), and the
  OS's emulated mouse for that contact is dropped. Losing window focus
  mid-contact drops the contact and its pressure, since winit on Windows
  never reports a cancelled contact. Tests drive synthetic
  events; no physical pen has been tried. The options bar has Size from
  Pressure and Flow from Pressure; there is no separate Opacity from Pressure
  toggle, and pen tilt, rotation, hover and the eraser end are not read.
- **Tab does not move keyboard focus.** Tab toggles the panels (Photopea's
  Hide/Show Panels) and is withheld from egui; controls are reached with the
  pointer. AccessKit is wired, but no screen-reader walk has been done on a
  real host.
- **Some 16-bit edits still compute at 8-bit precision.** Filters,
  adjustments, Fill, Clear, Free Transform (its resampling modes and a
  floated selection, including on an opened 16-bit PNG or TIFF whose tiles
  are still 8-bit), Edit ▸ Transform's flips and turns, every Image ▸
  Image Rotation (90°, 180°, Arbitrary, flips), Image Size, Canvas Size,
  Crop to Selection and Trim read and write a 16-bit layer at 16 bits. The
  painting tools, Stroke, Apply Mask, Defringe, Layer via Copy/Cut,
  Grayscale, Edit ▸ Fill ▸ Content-Aware (it synthesises the fill from an
  8-bit read) and the Filter dialog's preview still read a 16-bit tile
  rounded to 8 bits (the pixels they leave alone keep their 16-bit codes).
  Opening a 16-bit PNG or TIFF decodes it to 8-bit tiles, and PSD export is
  8-bit.
- **Pattern effects do not cross PSD.** A Pattern Overlay (and a pattern-filled
  glow or stroke) renders, and is saved inside the `.rstudio` document with its
  pixels, but PSD import does not read Photoshop's pattern data (the fidelity
  report lists "pattern overlay" as unmapped) and PSD export writes no pattern
  effects. The Layer Style dialog picks the overlay's pattern, but has no
  control yet that sets a glow's or stroke's fill to a pattern.
- **Channels** cannot isolate alpha or a mask, and have no per-channel
  histogram.
- **Localisation** covers the view and dialog code only; menu labels,
  `src/panels` and `src/canvas` are English literals.
- **SVG export** writes the raster, not vector paths. **PSD export** has been
  checked with an independent reader but not yet reopened in Photoshop or
  Photopea, and editable text export is blocked.
- **Print** writes a PDF; there is no OS printer-spooler dialog.
- **Content-aware:** Fill and Content-Aware Scale run on a job worker, but
  the Spot Healing Brush's Content-Aware type still synthesises on the UI
  thread at release; the fill refuses a context window over 2 M pixels, and
  Content-Aware Scale has fixed steps, no interactive handles and no
  protect-skin option.
- **Packaging:** no runtime window icon, no macOS `.icns`, no notarisation,
  and the release job has never run (below).

## Install

Nothing has been tagged, so there is no download. The workflow in
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) has a `release` job
that runs on a `v*` tag push, or on a manual run (`workflow_dispatch`) with
`release_dry_run` ticked. It builds three installers and uploads them as
workflow-run artifacts; it does not create a GitHub Release. **It has never
produced anything**: no tag exists and no manual run has been made.

| Platform | Artifact the job would build | Installs |
| --- | --- | --- |
| Windows 10/11, x64 | `RasterStudio-<version>-Setup.exe` (Inno Setup) | the editor, a Start-menu shortcut, the third-party licence notices |
| macOS 11+ | `RasterStudio-<version>.dmg` | `RasterStudio.app`, ad-hoc signed — Gatekeeper will warn until notarisation is configured |
| Debian/Ubuntu, amd64 | `raster-studio_<version>_amd64.deb` | `/usr/bin/raster-studio`, a desktop entry, notices under `/usr/share/doc/raster-studio` |

Until then, build from source:

```bash
git clone https://github.com/RealDealCPA-VR/Raster-studio
cd Raster-studio/raster-studio
cargo build --release -p studio-desktop
# the binary is target/release/studio-desktop(.exe)
```

`raster-studio/rust-toolchain.toml` pins the compiler to **1.98.1**; `rustup`
installs it on the first `cargo` command. The MSRV — the oldest compiler that
can build the lockfile, `rust-version` in `raster-studio/Cargo.toml` — is
**1.89**, and CI checks it with `cargo +1.89 check`. Windows needs the MSVC
build tools (and `rc.exe` from the Windows SDK for the executable's icon and
version resource); Linux needs the X11/Wayland development headers listed in
the workflow.

## Building

```bash
cd raster-studio
cargo check --workspace --all-targets   # type-check everything
cargo test  --workspace                 # ~4,500 tests
cargo run   -p studio-desktop           # launch
```

On Linux you need a Vulkan- or GL-capable environment for the window.
GPU-backed tests detect the absence of an adapter and skip themselves rather
than fail, so they can run on a runner without a GPU.

The desktop binary can capture one of its own frames:
`studio-desktop --shot out.png image.png` renders a 1440×900 frame, reads it
back to the CPU and writes `out.png`. The screenshot above was made that way,
from `tests/project-fixtures/thumbnail-scene-3628x2041.rstudio`.

## Layout

The workspace (`raster-studio/Cargo.toml`) has 22 members: the app, 20
library crates and the integration tests.

| Member | What it owns |
| --- | --- |
| `apps/studio-desktop` | The executable |
| `crates/app-shell` | Window, event loop, editor state, keymap, files, background jobs, autosave |
| `crates/ui` | Menus, panels, canvas widget, dialogs, tool options |
| `crates/design` | The design system: tokens, theme, widgets |
| `crates/editor-core` | Document, commands, history, selection |
| `crates/layer-model` | Layer tree, blend modes, masks, effects data |
| `crates/compositor` | The authoritative CPU tile compositor, including layer effects |
| `crates/raster` | Tiles, mipmaps, codecs, export |
| `crates/color` | Colour spaces, conversions and the ICC matrix-shaper engine |
| `crates/selection` | Selection algorithms |
| `crates/adjustments` | Adjustment operations |
| `crates/filters` | The filter library |
| `crates/tools` | Brush engine and the tool set |
| `crates/vector` | Bézier paths and rasterisation |
| `crates/text-engine` | Shaping, layout, glyph rasterisation |
| `crates/project-format` | The `.rstudio` package |
| `crates/asset-store` | Content-addressed blob storage, and the presets file |
| `crates/psd` | PSD read and write |
| `crates/render` | wgpu presentation |
| `crates/render-shaders` | WGSL shader sources (quad, composite, mipmap) embedded as strings |
| `crates/telemetry` | Local tracing setup and the diagnostics bundle (no network) |
| `tests/integration` | End-to-end tests over the engine the app runs |

The layering is enforced by dependency direction: `layer-model`, `color` and
`vector` are leaf domain crates with no I/O; `compositor` is a deterministic
function from document to pixels (it keeps input-keyed caches and a
process-wide font engine, but does no file I/O); `render` owns all wgpu;
`project-format` owns the `.rstudio` package; and `ui` never mutates the
document — it emits commands, so undo and redo behave the same whichever
control produced the edit. See
[`docs/architecture.md`](raster-studio/docs/architecture.md).

## Contributing

Two rules carry most of the weight:

1. **A test that passes against the unfixed code is not a test.** Break the
   thing you fixed and watch the test go red before you believe it.
2. **Do not write prose asserting behaviour the code does not have.** This
   project was rebuilt from a scaffold whose documentation described a working
   editor that did not compile.

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs on pushes to
`main`, on pull requests and by hand: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets` with `-D warnings`, `cargo check` and
`cargo test --workspace --no-fail-fast` on Linux, Windows and macOS, an MSRV
check (`cargo +1.89 check`), and `cargo audit`. Whether the current commit
passes is what the Actions tab says, not this paragraph.
[`CHANGELOG.md`](CHANGELOG.md) records what each wave changed.

## Licence

**No licence has been chosen yet.** The workspace manifest declares
`license = "Proprietary"`, but the repository is public and contains no
LICENSE file, so it grants no one a licence to use, copy or modify this code.
Choosing one is the owner's decision and is still open.

The third-party dependencies' licences and notices are listed in
[`raster-studio/LICENSES/THIRD_PARTY_NOTICES.md`](raster-studio/LICENSES/THIRD_PARTY_NOTICES.md).
