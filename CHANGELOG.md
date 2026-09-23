# Changelog

All notable changes to Raster Studio are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). Nothing has been tagged yet: every
entry below is unreleased, and the crate version is still `0.1.0`.

A line here is a claim, and claims get checked against the code: an entry
names what changed and, where it matters, how it was verified.

## [Unreleased]

Six fix waves, each from a fresh adversarial audit of the commit before it.
Every entry below is taken from its commit message and checked against the
code; where a wave left something open, its "Known gaps" says so. CI was green
on every wave commit: `53dd398`, `2caaa6c`, `02c7e1b`, `1c5b727`, `7bb295a`,
`b477a09` and `0e4a6fd` (run 35907846548).

### Wave 5 — `0e4a6fd`

#### Fixed

- **16-bit documents could not be saved.** The package's per-tile cap
  (`project_format::MAX_TILE_BYTES`) was the RGBA8 tile size, so every save
  of a document holding RGBA16 tiles was refused and the work existed only in
  memory. The cap is now one RGBA16 tile (512 KiB);
  `a_16_bit_document_saves_and_reopens_with_the_same_pixels` (Image ▸ Mode ▸
  16 Bits, Save, reopen, same tile bytes) and
  `a_16_bit_tile_saves_and_reopens_byte_for_byte` pin it.
- **The document was drawn across the whole window, under the panels.** It is
  now fitted, centred and drawn inside the canvas area (between the tool
  column, the docks, the bars and the status line); pointer mapping, overlays,
  zoom anchors and Fit use the same rectangle
  (`the_document_is_fitted_centred_and_confined_to_the_canvas_area`).
- **Free Transform and Move on a partial pixel selection moved the whole
  layer.** They now float the selected pixels, as one undo step
  (`tools::transform::float_selection`;
  `free_transform_with_a_selection_scales_only_the_selected_pixels`,
  `move_with_a_selection_moves_only_the_selected_pixels`). A selection that
  covers all of a layer's ink keeps the lossless whole-layer transform. Ctrl+T
  shows the handles at once, and the previous tool returns after the commit.
- **Switching tools or pressing Escape while typing lost the text.** Both now
  confirm it
  (`switching_tools_while_typing_commits_the_text_instead_of_deleting_it`,
  `escape_commits_the_typed_text_as_photopea_does`); AltGr characters type.
- A new layer mask is made from the selection and becomes the paint target;
  Save As renames the tab; File ▸ Open accepts a `.rstudio` project (pick the
  `manifest.json` inside it); recent-file paths are canonical.
- **The save journal hold** (see Wave 2) survives a crash at every step: the
  side file is absorbed exactly once, through a staged rename
  (`project_format::CommandJournal::absorb`), and every open absorbs a
  leftover before it reads the journal. A save snapshots only the tiles the
  document references.
- **Untrusted files:** `.cube` LUTs are read up to 16 MiB
  (`adjustments::extended::MAX_CUBE_FILE_BYTES`) and parsed with a bound on
  the table size; `actions.json` is read up to 256 MiB
  (`app_shell::actions_library::MAX_ACTIONS_FILE_BYTES`), its tiles are
  checked for size and hash before replay, and it is written atomically.
- Per-frame costs removed: history fingerprints, colour-drag previews, sampler
  composites, Color Lookup hashing, crop pixel tests, thumbnails, font lists.

#### Added

- Curves has a draggable curve over the histogram, with a channel choice
  (`ui::dialogs::adjustment_dialog::curve_widget`, `crates/ui/src/dialogs/curve_widget.rs`); Levels shows its histogram.
- Ctrl+L / M / U / B / I (Levels, Curves, Hue/Saturation, Color Balance,
  Invert), Backspace (Clear), Alt+Backspace and Ctrl+Backspace (fill with the
  foreground / background colour) are bound
  (`the_photoshop_adjustment_and_clear_chords_resolve_to_their_actions`).
- One swatch per colour well; HSB in percent; rulers, grid and the
  empty-document panels tidied.

### Wave 4 — `b477a09`

#### Added

- Live gesture feedback: the crop box (shaded outside, with a guide), the
  marquee rubber band, lasso and polygonal-lasso outlines, pen anchors, handles
  and the rubber segment, and slice rectangles are drawn while the gesture
  runs; Escape and Enter take them down.
- Paint strokes show while dragging, through the document's preview lens; the
  release commits one command equal to the last preview, and cancelling
  restores the bytes.
- A brush-size ring and per-tool cursors over the canvas; a right-click opens
  the canvas context menu.
- Crop: ratio presets, W × H × resolution, an overlay choice; Straighten and
  Delete Cropped Pixels are applied.
- Adjustments: Desaturate, Equalize, Shadows/Highlights, Replace Color, Color
  Lookup (a `.cube` file or built-in looks).
- 16-bit is reachable: Image ▸ Mode ▸ 8/16 Bits convert undoably, New
  Document accepts 16 bits, strokes on 16-bit layers stay 16-bit, PNG/TIFF
  export keeps 16 bits.
- Tools: Ruler (with Straighten Layer), Color Sampler, History Brush,
  Add / Delete / Convert Anchor, Pencil Auto Erase.
- File / Edit: File ▸ Export ▸ Slices, ICO and SVG export, Paste in Place,
  Paste Outside, Purge.
- Panels: the Paths work path and footer, named Actions with visible steps and
  save / load, History thumbnails and a source marker, a Brushes tip grid;
  swatches and brush presets persist.

#### Fixed

- A presenter test loads its own font, so it no longer races the shared font
  library (it flaked once on macOS at `7bb295a`).

#### Known gaps (as of this wave)

- 16-bit documents could not be saved as `.rstudio` (fixed in Wave 5).
- Most 16-bit edits compute at 8-bit precision; opening a 16-bit file decodes
  to 8-bit tiles; PSD export is 8-bit. Still open.

### Wave 3 — `1c5b727`, integrated in `7bb295a`

#### Added

- View ▸ Extras draw: rulers (with guide pulls), guides, smart guides during a
  Move drag, grid, pixel grid at 800% and above, layer edges, selection edges
  and a precise cursor. Snap is honoured, and a layer no longer snaps to its
  own edges.
- Pen: a click-drag makes a smooth anchor and Alt breaks a handle; Path and
  Shape modes with fill and stroke. Shapes: fill / stroke on-off, colour,
  width, corner radius; Custom Shape has a built-in library.
- Rotate View and Flip View reach the document camera (`render::Camera`
  carries a rotation and mirroring; screen-to-document mapping, the
  checkerboard and Reset View Rotation honour them). In `1c5b727` Flip was
  still greyed; `7bb295a` enabled it.
- Fourteen filters: Average, Blur, Blur More, Smart Blur, Sharpen, Sharpen
  More, Sharpen Edges, Displace, Facet, Fragment, Mezzotint, Extrude, Tiles,
  Trace Contour, each opening the preview dialog from the menu.
- Preferences apply live: keymap edits, Units (rulers and readouts; Picas
  persist), the wheel-zoom setting; Edit ▸ Keyboard Shortcuts opens the keymap
  page.
- Select ▸ Color Range, Modify amounts and Save / Load Selection are real
  dialogs; Image ▸ Mode has 8/16-bit rows; 16-bit tiles composite at full
  precision.
- `tests/integration/tests/tool_routes.rs` drives every palette tool through
  the real pointer route with a tool-specific assertion and one undoable
  history entry each.
- Properties: Transform (X/Y/W/H) and Align, shape, text and smart-object
  pages. Layers: double-click rename, name search. Character: kerning, scale,
  baseline shift, caps. Paragraph: indents and justify variants.
- A live W × H readout beside the pointer while a shape is dragged; a Channels
  saved-selection row opens Load Selection on that row.

#### Known gaps (as of this wave)

- View ▸ Proof Colors and Gamut Warning are greyed with a reason. Still so.

### Wave 2 — `02c7e1b`

#### Added

- Photopea's chrome: the menu, then a full-width options bar, then a tab strip
  only over the canvas; the docks stay up with no document; a start screen
  with New / Open / Templates cards and a recent grid with real thumbnails.
- Two dock columns: a narrow right one (Navigator | Info | Histogram,
  Color | Swatches, Brushes | Character | Paragraph) beside the wide one
  (Properties | Adjustments, History, Layers | Channels | Paths). A Histogram
  panel; the Navigator draws the composite; Info shows the colour under the
  pointer; channel thumbnails.
- The palette follows Photopea's grouping and order; Quick Mask and Screen
  Mode controls under the wells; F cycles Standard / Full Screen With Menu /
  Full Screen.
- Menus: Save as PSD…, Layer ▸ Align / Distribute / Lock / Rename / Stamp
  Visible, Edit ▸ Step Forward / Backward, Blending Options… (a real Blending
  page in the Layer Style dialog), Layer via Copy / Cut honour the selection,
  Select ▸ Refine Edge…, View ▸ 200% / New Guide… / Clear Guides / Lock
  Guides, Help ▸ About as a dialog, Trim… with options.

#### Changed

- **Saves:** unchanged tiles are reused (hard-linked from the package being
  replaced), tile payloads are deflate-compressed (`.tilez`, package layout
  version 3), one fsync batch per commit. Save, autosave, export, Export
  Layers and PSD parse run on a worker thread against a snapshot, with the
  document's commands journaled to a side file for the length of the save
  (the journal hold).
- Undo / redo and text drafts invalidate only the changed rectangle.
- `--shot` always captures a 1440×900 window and never persists geometry.

### Wave 1 — `2caaa6c`

#### Added (application)

- Layers-panel thumbnails are cached per layer (content fingerprint), at most
  two recomposited per frame.
- Every registry option of every tool reaches its tool through
  `set_setting`; the paint blend Mode combo composites brush, pencil, clone
  and pattern dabs; the gradient editor's stops and the active pattern reach
  the pointer route.
- Image ▸ Adjustments: all fifteen that existed then open a parameter dialog
  with live preview.
- The dark theme re-tuned to Photopea's relationship (panels lighter than the
  pasteboard), square 2 px corners.

The release-engineering half of this wave (W1-E) follows.

#### Release engineering (W1-E)

##### Fixed

- **CI stopped at the first failing test binary**, hiding a second failure
  for three days. `cargo test --workspace` now runs with `--no-fail-fast`.
- **The release job could not have run, and could not have produced an
  artifact if it had.** The workflow's `on.push` listed `branches: [main]`
  with no `tags:` entry, so a `v*` tag push never started the workflow and
  the job's `startsWith(github.ref, 'refs/tags/v')` gate could never be true;
  `on.push.tags: ['v*']` is added. `upload-artifact` — a JavaScript action,
  which ignores `defaults.run.working-directory` — looked for
  `target/installer/*.exe`, `target/packaging/*.dmg` and `*.deb` at the
  repository root while the files are under `raster-studio/target/`. The
  paths now carry the prefix. The job uploads workflow-run artifacts; it does
  not create a GitHub Release.
- The Inno Setup script's `SetupIconFile` pointed one directory too shallow
  (`..\..\assets\`, a folder that does not exist); it is `..\..\..\assets\`.
  The installer version comes from Cargo through `/DAppVersion` instead of a
  hard-coded `0.1.0`.
- `build-app.sh` and `build-deb.sh` were committed without the executable bit
  (`100644`); the release job would have failed on "Permission denied". Both
  are `100755` in the index now, and both read the `studio-desktop` version by
  package name instead of `packages[0]` (whichever workspace member cargo
  happened to list first).
- The two signing steps only echoed, and their `if: env.X != ''` read a
  step-level `env:` that an `if:` cannot see. They now decide inside the
  script: with no secret configured, the step summary says **UNSIGNED** in
  as many words and the step passes; with `WINDOWS_CERT` configured the real
  `signtool sign` + `verify /pa` run and gate the step; with `APPLE_ID`
  configured the macOS step fails on purpose, because notarisation is not
  wired yet and an artifact that looks signed but is not would be worse than
  a red job.
- **MSRV was wrong.** `rust-version = "1.82"` and "Rust 1.82" in both READMEs
  could not build the lockfile (`cosmic-text 0.17` and `smol_str 0.3` declare
  1.89). The manifest says `1.89`, the READMEs say 1.89, and a new `msrv` CI
  job runs `cargo +1.89 check --workspace --all-targets` (verified locally:
  `Finished` in 53 s, exit 0).
- `rust-toolchain.toml` pinned `channel = "stable"` under `-D warnings`, so a
  new stable's new lints would have turned CI red with no change in the
  tree. It pins `1.98.1`; the CI jobs read the channel from that file.
- **Help ▸ About showed the wrong version.** It printed the `app-shell`
  library's `CARGO_PKG_VERSION`; the desktop binary's git stamp existed but
  was only ever read by a test. The executable now hands its stamp to
  `app_shell::set_version_stamp` before launch and About shows
  `Raster Studio 0.1.0 (53dd398) — a layered raster editor` (driven through
  the real menu bridge in a test). `build.rs` re-runs when `.git/HEAD`, the
  branch ref or `packed-refs` change, so the stamp follows commits without a
  `cargo clean`. The unstamped fallback (`app-shell`'s own version, no
  commit) is a pure function of the stamp slot and is tested on its `None`
  branch, not only on a hand-built value.
- `app_shell::jobs::spawn_import` panicked (`.expect`) if the OS refused the
  worker thread. The refusal now arrives on the job's receiver as the import
  failure it is, and the existing poll route reports it.

##### Added

- **Windows:** the executable is a GUI-subsystem program (no console window
  behind the editor) that re-attaches to the parent's console when there is
  one, so `studio-desktop --shot out.png` from a terminal still prints its log
  and writes the PNG. It carries an embedded icon and a `VERSIONINFO`
  resource filled from `CARGO_PKG_VERSION` (Explorer's Details tab shows
  `0.1.0` / `0.1.0+git<hash>`), generated by `build.rs` through
  `embed-resource`.
- `LICENSES/` (the third-party notices) ships inside the Windows installer,
  the macOS `.app` (`Contents/Resources/LICENSES`) and the `.deb`
  (`/usr/share/doc/raster-studio`).
- A `workflow_dispatch` input, `release_dry_run`, runs the release job
  without a tag so it can be exercised before anyone tags.
- This file, and an Install section in the root README.

##### Known gaps

- The release job has still **never run** (no tag, no manual dry run); the
  fixes above are what a dry run will exercise. Still true after Wave 5.
- **Fixed in the same commit:** declaring the true MSRV turned on two lints
  clippy gates on it (`clippy::chunks_exact_to_as_chunks`,
  `clippy::manual_is_multiple_of`) at 85 sites. The wave-1 commit rewrote
  every one (`as_chunks`, `is_multiple_of`), and the CI clippy job under
  `-D warnings` has been green since.
- The runtime window icon (title bar / taskbar while running) is whatever
  Windows derives from the executable's resource; `app-shell` exposes no hook
  to hand winit an icon, so the shell does not set one itself.
- macOS: `Info.plist` names `raster-studio` as `CFBundleIconFile` but no
  `.icns` exists in the tree; notarisation is documented, not wired.

### Wave 0 — `53dd398`

Release blockers found by a fresh adversarial audit of `main`.

#### Fixed

- **Tool palette:** the footer painted an opaque full-column rect over every
  slot icon after they were drawn — why every screenshot since 2026-09-02
  showed only the colour wells. The footer is a pinned bottom panel, the
  slots scroll, glyphs are 20 pt in a 28 pt slot, and the missing
  `reset-colors` icon exists. The headless test asserts paint order.
- **Menu bar:** clicks went straight to `record()`/`perform`, bypassing the
  dialog host, so Image Size, Canvas Size, Arbitrary Rotation, Layer Style,
  Filter Gallery, every Filter, Export As, Fill and Stroke failed or ran at
  defaults. Menu clicks, context rows and panel controls share one route
  (`Chrome::route`); `every_enabled_menu_item_really_does_something` is red
  on the old code.
- **Shortcuts:** chords painted in the menus but unbound in the app keymap
  fall through to the menu table; Ctrl+Shift+I / Ctrl+Shift+E / Ctrl+J do
  what the menu paints; shifted punctuation chords fold back to the key
  winit reports.
- **CI (ubuntu, red since `cb9e91c`):** the substitute for a missing font
  family was the fixture family itself on a fontless runner. The engine
  honours `RASTER_STUDIO_FONT_DIRS` so the runner condition is reproducible;
  the test writes its headline in DejaVu Serif and asserts the substituted
  composite equals the substitute family's.
- **Panels:** action buttons no longer wear the Disabled colour, panel glyphs
  are 16 pt, the Layers block is two rows with the filter row on top and a
  Photopea-ordered footer, History rows span the panel.

#### Removed

- Twelve identical junk screenshots and the agent goal ledger from the
  repository; ignore rules for root-level scratch.

[Unreleased]: https://github.com/RealDealCPA-VR/Raster-studio/compare/53dd398...HEAD
