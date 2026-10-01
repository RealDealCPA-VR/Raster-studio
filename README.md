# Raster Studio

**A Photoshop-class image editor for your desktop. Your files stay on your disk.**

Raster Studio is a native image editor written in Rust for Windows, macOS and
Linux. It aims at feature and workflow parity with Photopea, with the same
menus, tools, panels and shortcuts, and it runs entirely on your machine. It
needs no account and uses no cloud, and its own code makes no network calls.

![Raster Studio's main window: the menu bar and the Move tool's options bar
along the top, the tool column on the left, a layered 3628x2041 project on
the canvas, and the Navigator, Color, Brushes, Properties, History and Layers
panels on the right](raster-studio/docs/main-window.png)

## Why Raster Studio

- **Private by design.** Files are opened from and saved to your disk. Nothing
  is uploaded.
- **Familiar.** The layout, menus and shortcuts follow Photopea and Photoshop,
  so existing skills carry over.
- **Works with Photoshop files.** PSD and PSB files open with their layers,
  masks, effects, text, smart objects, adjustment layers and artboards, and
  save back as PSD.
- **One engine you can trust.** Every pixel operation runs in a single CPU
  engine; the GPU only displays the result. The same code is tested headlessly,
  so what the tests check is what you see.

## What it does

| Area | Highlights |
| --- | --- |
| **Layers** | Groups, masks and vector masks, clipping, 27 blend modes, layer styles (all ten Photoshop effects), adjustment and fill layers, smart objects with smart filters, artboards, layer comps |
| **Tools** | 69 tools on the palette: selections (marquee, lasso, magnetic lasso, magic wand, quick and object selection), brush, pencil, eraser, clone, healing, patch, content-aware move, gradient, paint bucket, pen and shape tools, type (horizontal, vertical, on a path, warped), crop, perspective crop, slices, ruler and more |
| **Adjustments and filters** | 22 adjustments (Levels, Curves, Hue/Saturation, Camera Raw and others) and 71 filters, including a Filter Gallery, Liquify, Puppet Warp, Blur Gallery, Vanishing Point, Lighting Effects and Fourier |
| **Selections** | Select Subject, Color Range, Refine Edge, Magic Cut, Quick Mask, saved selections and alpha channels |
| **Colour** | RGB, Grayscale, CMYK, Lab, Indexed, Duotone and Bitmap modes; ICC profiles with Assign and Convert; a colour-managed canvas; editing a single colour channel |
| **Text** | Character and paragraph styles, type on a path, warped text with a custom mesh, and 39 interface languages |
| **Animation and video** | Frame animation and a video timeline with keyframes and easing; MP4 video opens as video layers; export to MP4 (H.264), animated GIF, APNG and WebP |
| **Automation** | Actions (recorded, played and exchanged as Photoshop `.atn` files), Batch, and a JavaScript scripting window with a Photoshop-style API |
| **Panels** | 27 panels: Layers, Channels, Paths, History, Properties, Adjustments, Color, Swatches, Brushes, Styles, Character, Paragraph, Navigator, Histogram, Info, Actions, Animation and more |

**Opens** PSD/PSB, PNG, JPEG, WebP, TIFF (including layered), GIF, BMP, ICO,
TGA, SVG, PDF and Illustrator, EPS, HEIC and AVIF, JPEG XL, OpenEXR, HDR, DNG
and most camera RAW files, Krita, GIMP, Paint.NET, Sketch, XD and Figma,
Clip Studio and more: over 60 file types in all.

**Saves and exports** PSD/PSB, the native `.rstudio` project format, PNG,
JPEG, WebP, TIFF, GIF, BMP, AVIF, JPEG XL, EXR, SVG, PDF, EMF, DXF, MP4 and
others, plus Export As, export of slices and artboards, and Quick Export.

The full feature reference, with each claim tied to the code and tests that
prove it, is in [docs/FEATURES.md](raster-studio/docs/FEATURES.md).

## Project status

- **Builds and tests are green** on Windows, macOS and Linux (6,534 tests,
  with lint, formatting, minimum-Rust-version and dependency-audit checks).
- **Installers build.** A release dry run produces a Windows installer, a
  macOS disk image and a Debian/Ubuntu package. No version has been tagged
  yet, so there is no public download.
- **Licence: not chosen yet** (see [Licence](#licence)).
- **Not yet verified inside Photoshop or Photopea themselves.** PSD files
  written by Raster Studio are checked with this build and an independent PSD
  reader, not by reopening them in Adobe's or Photopea's apps.

## What is still missing vs Photopea

These are the known differences, grouped by area. The
[parity matrix](raster-studio/docs/parity-matrix.md) lists every item with its
reason and the code it concerns.

**Not offered at all**
- Online and account features: cloud storage, sharing and publishing, the
  template gallery, the online font library, plugins and collaboration (by
  design: the app is offline). Share and Remove BG appear in menus but are
  disabled.
- AI and model-based features (generative fill, AI background removal).
  Select Subject, Magic Cut and Object Selection are classical colour-based
  methods, not semantic ones.
- Filter ▸ Adaptive Wide Angle, Take a Picture (camera capture), and a mobile
  or tablet layout.
- Right-to-left languages (Arabic, Hebrew, Persian), complex-script languages
  (Thai, Lao, Tamil, Bengali, Tibetan), Georgian and Ethiopic, and a few others
  Photopea offers: 39 of its 59 languages are available. Some text (dialog
  bodies, status messages, tooltips) is still English in every language.
- Keyboard navigation between controls (Tab hides the panels, as in
  Photopea), and no screen-reader testing yet.

**Files and formats**
- Camera RAW: Canon CR3, compressed Nikon NEF, compressed Fuji and Olympus
  files, packed Panasonic RW2 and lossless Sony ARW are refused (convert to
  DNG). Other RAW files open with a generic colour matrix, so colours are
  less accurate than a camera profile would give, and there is no RAW
  development dialog beyond Camera Raw's Basic panel.
- HEIC and AVIF: HDR images (PQ, HLG, BT.2020) open as ordinary sRGB, and
  image sequences open as a single still.
- 32-bit HDR documents save to PSD as 8-bit, and most exports clip HDR values.
- Vector files lose some detail. Encrypted PDFs are refused, and a PDF page
  with content the layer reader cannot keep opens as a single picture. EPS
  text in embedded fonts, gradients and patterns are simplified. Sketch, XD
  and Figma open without symbols, effects, masks or extra pages. EMF and WMF
  skip some drawing features.
- Clip Studio, Pixelmator Pro, CorelDRAW and InDesign files open as their
  preview image only, not as layers. Some Krita, GIMP, Paint.NET and DICOM
  variants are not read.
- Tool presets (`.tpl`) are read but cannot be loaded into the Tool Presets
  panel yet.
- Lossy JPEG XL export (only lossless is written).

**Video and animation**
- No audio: video is imported and exported without sound.
- No Cut (split clip) tool on the timeline. Decoded video frames are not
  saved with the project, so they are decoded again on reopen.
- Only H.264 and AV1 video opens; HEVC, VP8 and VP9 are refused.

**Editing and tools**
- Some operations on 16-bit and 32-bit documents still compute at 8-bit
  precision (Stroke, the Clone Stamp and Healing Brush source, Content-Aware
  Fill and a few others), and CMYK inks are separated at 8 bits.
- CMYK uses a simple ink model, not an ICC press profile. Duotone inks are
  baked into the pixels and cannot be re-edited, and a Duotone document saves
  as Duotone PSD only when its inks are known (otherwise as RGB, with a note).
- An existing vector mask's path cannot be edited; it has to be redrawn.
- Some Photopea dialogs act at once instead of opening: Fill Path, Stroke Path
  and Make Selection on the pen tool's menu. Divide Slices applies on OK
  rather than live.
- Liquify, Puppet Warp, Blur Gallery, Vanishing Point, Lighting Effects and
  Perspective Warp work in a dialog preview rather than on the canvas, and
  each has fewer options than Photoshop's.
- Camera Raw has the Basic panel and a tone curve only. Lens Correction has
  no lens-profile database, and HDR Toning is not Photoshop's algorithm.
- Several filters and Filter Gallery effects are this project's own
  renderings, so they look similar to Photopea's but not pixel-identical.
- Brush engine: no Texture, Dual Brush or Wet Edges options. Pen tablets
  report pressure only (no tilt, rotation or eraser end), and that has been
  tested with simulated input, not a physical pen.
- Content-aware fill, heal and scale show no percentage progress, and very
  large areas are refused.

**Scripting and actions**
- Scripts cover a subset of Photoshop's scripting API: no `executeAction`,
  no filters or adjustments called from a script, and no file access a
  script chooses itself.
- Actions replay the Photoshop steps this build knows and skip the rest with
  a reason; steps recorded as pixel edits replay as pixels and are left out
  of an `.atn` export.

**Platform**
- Printing opens the system print dialog on Windows only; on macOS and
  Linux, Print writes a PDF for the system viewer.
- The macOS app is not notarised, so Gatekeeper warns on first launch.

## Getting started

There is no published download yet. To build from source you need
[Rust](https://rustup.rs) (the exact compiler version installs itself on the
first build).

```bash
git clone https://github.com/RealDealCPA-VR/Raster-studio
cd Raster-studio/raster-studio
cargo run --release -p studio-desktop
```

The finished binary is `target/release/studio-desktop` (`.exe` on Windows).
You also need a C and C++ compiler (the Microsoft C++ build tools on
Windows, Xcode's command-line tools on macOS, GCC or Clang on Linux), and on
Linux the X11 or Wayland development packages listed in
[`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Documentation

| Document | What it covers |
| --- | --- |
| [docs/FEATURES.md](raster-studio/docs/FEATURES.md) | Every feature in detail, with its limits and the tests behind it |
| [docs/parity-matrix.md](raster-studio/docs/parity-matrix.md) | Row-by-row comparison with Photopea |
| [docs/architecture.md](raster-studio/docs/architecture.md) | How the crates fit together |
| [docs/file-format.md](raster-studio/docs/file-format.md) | The `.rstudio` project format |
| [docs/threat-model.md](raster-studio/docs/threat-model.md) | How untrusted files are handled safely |
| [CHANGELOG.md](CHANGELOG.md) | What each development wave changed |

## For developers

```bash
cd raster-studio
cargo test --workspace          # the full test suite
cargo clippy --workspace --all-targets -- -D warnings
```

The workspace has 22 members: the desktop app, 20 library crates (engine,
compositor, codecs, PSD, tools, UI, design system and others) and the
integration tests. `raster-studio/rust-toolchain.toml` pins the compiler; the
minimum supported Rust version is 1.89. `studio-desktop --shot out.png
file.png` renders one 1440x900 frame to a PNG, which is how the screenshot
above was made.

Two rules carry most of the weight in this codebase:

1. **A test that passes against the unfixed code is not a test.** Break the
   thing you fixed and watch the test fail before you trust it.
2. **Documentation is a claim.** Do not describe behaviour the code does not
   have; every feature listed here is implemented, tested and reachable from
   the user interface.

CI runs on every push to `main` and on pull requests; the Actions tab shows
whether the current commit passes.

## Licence

**No licence has been chosen yet.** The workspace manifest says
`license = "Proprietary"`, but the repository is public and has no LICENSE
file, so it grants no one permission to use, copy or modify this code.
Choosing a licence is the owner's decision.

Third-party dependencies and their licences are listed in
[`raster-studio/LICENSES/THIRD_PARTY_NOTICES.md`](raster-studio/LICENSES/THIRD_PARTY_NOTICES.md).
