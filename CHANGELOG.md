# Changelog

All notable changes to Raster Studio are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). Nothing has been tagged yet: every
entry below is unreleased, and the crate version is still `0.1.0`.

A line here is a claim, and claims get checked against the code: an entry
names what changed and, where it matters, how it was verified.

## [Unreleased]

Six fix waves (0-5), each from a fresh adversarial audit of the commit
before it, then the Photopea-parity waves 7-11, 13 and 13X, each from an
audit of Photopea's features against this build. Every entry below is taken
from its commit message and checked against the code; where a wave left
something open, its "Known gaps" says so, and the parity matrix
(`raster-studio/docs/parity-matrix.md`) carries the row-by-row detail. The
older entries keep their gaps as they stood at their commit; where a later
wave closed one, the entry says which.

CI, as the Actions tab reports it: the six fix waves are seven commits, each
green on its own run (`53dd398` 35792257651, `2caaa6c` 35799963384,
`02c7e1b` 35817391510, `1c5b727` 35871476210 and its integration `7bb295a`
35876205854, `b477a09` 35893831056, `0e4a6fd` 35907846548); waves 7-10 are
green (`8b6c399` 35931490726, `f9329d0` 35939564122, `9a61faa` 35957810847,
`05ec9b1` 36026393650). Wave 11 (`fe978d3`, run 36048266245) **failed**: its
`test (macos-latest)` job failed and the other test, lint and audit jobs passed. The CI fixes
below ended that: `c52c407` (run 36051745607) was still red on macOS, as was
the docs commit `6e0287d` (run 36052957676), and `22e31a7` (run
36054206009) is green. Wave 13 (`06abd74`, run 36084801672) is
green. Wave 13X (`25b66e0`, run 36102799475) is green (the release job is skipped on every run: no tag).

### Wave 14 — docs honesty after waves 13 and 13X (uncommitted)

#### Added

- **File ▸ Export… is a menu row.** Single-file export by extension was
  reachable only by its chord (Ctrl+Alt+Shift+S, the keymap's default for
  `Action::Export`) and was not in Help ▸ Search Commands. The File menu
  now has an **Export…** row above the Export As submenu
  (`ui::menu::MenuAction::ExportByName`, greyed with no document), which
  `app_shell::menu_bridge` routes to `Action::Export`: the platform save
  picker, then the format the typed extension names
  (`doc::export_format_for`, plus `.svg` and `.psd` / `.psb`). Help ▸ Search
  Commands lists it as "File > Export…", since the palette is built from
  the menu bar. Known gap: the row paints no chord. Ctrl+Alt+Shift+S still
  performs it, but painting the chord beside the row needs
  `keymap::menu_twin` to name the row, which this change does not touch.
  Verified by `app-shell`
  `menu_bridge::tests::file_export_is_a_menu_row_command_search_finds_and_writes_by_extension`
  (the row in the File menu, the palette entry, and the menu bar's click
  route writing a JPEG from a `.jpg` name), red with the row taken out of
  the File menu.

#### Changed

- The READMEs, the parity matrix, the PSD support matrix and the
  architecture, file-format and threat-model docs re-checked against the
  code after waves 13 and 13X: every count re-derived (69 `ToolId`s, 68 of
  them on the palette; 26 panels; 71 filters; 22 adjustments; 27 blend
  modes; 36 extensions in `IMAGE_EXTENSIONS`; 19 formats in
  `ExportFormat::writable()`), the contradictions a confirming audit of
  `6e0287d` found corrected, and the "still missing" lists stated from the
  parity rows' own "Not done" notes. `docs/main-window.png` retaken.

### Wave 13X — `25b66e0`

Follow-ups to wave 13: nine doer/reviewer pairs.

#### Added

- **Arrow-key nudge** (W13X-1). The arrow keys nudge (`Action::Nudge`,
  `tool_input::move_duplicate::nudge`): with the Move tool the active
  layer(s), or the selected pixels with the ants, move 1 px, 10 px with
  Shift, and Alt+arrow duplicates first through W13-A's copy path (N+1
  layers, one step); with a selection tool the arrows move the outline
  only. Each press is its own history step; a held arrow repeats (Alt
  copies on the press only); a focused text field or an open Type run keeps
  the arrows (`shell::nudge_tests`). Known gap: Ctrl+arrow is not bound.
- **A colour-managed canvas** (W13X-2). Every upload is converted from the
  document's profile to the sRGB texture
  (`app_shell::presenter::DisplayTransform`, the numbers untouched), so
  W13-F's Assign Profile changes what the canvas shows and Convert to
  Profile keeps it (Adobe RGB to sRGB within 2 codes; sRGB to Adobe RGB
  within 2 codes on at least 97% of channels, and further (up to what one
  8-bit Adobe RGB code spans on screen, 7 codes seen) on saturated pixels
  near sRGB's gamut edge; ProPhoto and Display P3 not measured); a profile
  the engine cannot transform (not a matrix-shaper) is shown as its
  numbers, unconverted; and Pattern Preview's copies go through the same
  conversion and hold the canvas's bytes. Known gaps: the display
  conversion targets sRGB, not the monitor's profile; Pattern Preview skips
  the canvas's view-only passes (Proof Colors, channel eyes, mask view);
  the Navigator / History whole-canvas preview (`chrome::composite_preview`)
  is not colour-managed yet and disagrees with the canvas on a non-sRGB
  document.
- **Scale Effects… is a percent dialog** (W13X-3): 1–1000%, with a Preview
  image of the document at that percent
  (`scale_effects_asks_for_any_percent_*` drives menu, dialog, 37% and
  Enter). Stack Mode's enable count walks the embedded PSD's layer records,
  so group dividers, a group's children and hidden layers are not counted
  and a source holding one group is greyed; at 16 and 32 bits the records
  are read from the `Lr16` / `Lr32` block. Known gaps: the Scale Effects
  preview is an image inside the dialog, not a live canvas preview; Stack
  Mode stays enabled, then refuses, for a linked source (the file is not
  read to grey the row), an embedded one whose records cannot be walked, or
  one that fails to decode.
- **Photopea's themes, spot channels and the Channels panel menu**
  (W13X-4). Edit ▸ Preferences… ▸ Theme and Window ▸ Appearance offer seven
  themes: the app's own Light and Dark (modelled on Photopea's White and
  Dark Grey, not copies: their pasteboard, buttons, text and accent differ,
  and Light's panel too) plus five of Photopea's — Light Grey, Blue, Dark
  Blue, Purple and Black on Photopea's `pp.js` panel, pasteboard, button,
  button-hover, text and accent numbers except five that fail the design
  contrast gates: Light Grey's text (#222221 for #393837) and accent
  (#0B5CC4 for #3482F6), Blue's and Purple's accent (#5B9BF0 for #3482F6)
  and Dark Blue's panel (#303445 for #222531), a list a `design` test pins;
  `RASTER_SHOT_THEME` picks one for a `--shot`. The Channels panel's menu
  has New Spot Channel… (name, ink, solidity; the selection becomes its
  coverage; composited as ink, saved in `.rstudio`, written to and read
  from `.psd` as a spot channel) and Merge Channels…. Known gaps:
  Photopea's exact White and Dark Grey are not offered; a spot channel
  cannot yet be edited, renamed, painted or deleted once made. Verified by
  `design` `theme::tests::*` and `token_gates`, `app-shell`
  `prefs::tests::picking_a_theme_in_preferences_installs_its_visuals` and
  `spot_channel::tests::*`, `ui` `panels::channels::w13x4_channels_tests::*`,
  `compositor` `spot::tests::*` and `editor-core` `spot::tests::*`.
- **Custom warp and Flame along a path** (W13X-5). Warp Text… ▸ Custom,
  then the Move tool drags the handles of a 4x4 Bezier mesh drawn over the
  text, one undo step a drag, the mesh saved with the layer. Filter ▸
  Render ▸ Flame burns along the Paths panel's current path with
  Photopea's controls and defaults, refuses with "Make a path first"
  without one, and keeps its path as a smart filter. Known gaps: the PSD
  writer writes no text warp at all (every text layer is `warpNone`), so a
  Custom warp is not in a saved PSD; Flame's renderer is this build's, not
  a port of Photopea's. Verified by `text-engine` `warp::custom_tests::*`,
  `app-shell` `warp_custom::tests::w13x5_*` (the mesh drawn on the canvas, a
  dragged handle bending the text in one undo step, a press off the handles
  not taken), `ui`
  `dialogs::warp_text::tests::w13x5_custom_is_offered_and_keeps_the_layers_mesh`
  and `layer-model` `text::w9k_warp_path_tests::a_custom_mesh_is_append_only_serde`.
- **Small gaps** (W13X-6). The Background Eraser samples Continuously by
  default, as in Photopea (Once is a choice); a group is copied with all
  its children (nested groups included), as one step, by Alt+drag and by
  Duplicate Layer (the menu and Ctrl+Alt+J); a tool letter enters its group
  at the tool last used from it this session (the first tool when none was
  used yet), whether that tool was picked by letter, palette or fly-out,
  not remembered across sessions; the file name a script passes to
  `app.open` or `saveAs` prefills the platform picker's file-name box (its
  last path component only; the user still chooses the file).
- **PDF import dialog and Paint.NET layers** (W13X-7). A PDF / AI of two or
  more pages first asks, in an import dialog, which pages (thumbnails, each
  a toggle), at what resolution (18-1200 dpi) and whether they open as
  artboards or as separate documents; several such files opened at once
  ask in turn; File ▸ Revert reads back the same choice. A Paint.NET `.pdn`
  opens as its layers (name, opacity, visibility, blend mode) through a new
  bounded .NET BinaryFormatter reader (no new dependency; a class's names
  are shared by its objects, the parsed graph is held to 64 MiB, a pixel
  block longer than its layer is refused and each block is dropped once
  its layer is built); when the reader cannot follow a file its thumbnail
  opens and the status line says why; File ▸ Revert reads the layers back
  (and refuses, rather than flatten to the thumbnail, when they can no
  longer be read). Known gaps: Paint.NET's gzip-wrapped older layout and
  object graphs the reader does not recognise open the thumbnail (the
  reader is checked against synthetic files, not files saved by
  Paint.NET); a one-page PDF opens without the dialog. Verified by
  `raster::codec::formats::vector_docs::pdn::tests::*`,
  `dialogs::pdf_import::tests::*` and `editor::open_any::w13x7::tests::*`.
- **Sketch, XD and Figma open as layers** (W13X-8): ZIP-packaged or a bare
  `fig-kiwi` canvas; DEFLATE, or Zstandard through the new `ruzstd` 0.9
  decoder (MIT); on every open route: artboards, groups, vector shape
  layers with fill and stroke, text layers (string, font, size, colour) and
  raster layers for bitmaps, with an import report naming what did not
  map; the preview opens only when the layers cannot be read, saying why,
  and File ▸ Revert rebuilds the layers. The kiwi decoder charges list
  growth against its memory budget. Known gaps: the first page only;
  symbols / components not expanded; gradients, effects, non-union boolean
  operations, per-run text styles, blend modes and masks reported and not
  kept (masked layers open unclipped); a Figma `VECTOR` with no stored
  outline drawn as its box. Verified by
  `raster::codec::formats::vector_docs::design_files::tests::*` and
  `editor::open_any::import_design::tests::*`.
- **Timeline easing, scale and rotation** (W13X-9). Scale and rotation keys
  (about the layer centre, the canvas-sized pixel box's centre) join
  opacity and position; each key carries Linear / Ease In / Ease Out / Hold
  interpolation (the panel's Scale key / Rotation key buttons and four
  interpolation buttons for the selected key; the preview well draws each
  thumbnail as a quad at the playhead's transform); `mp4::probe` refuses a
  sample table naming more than `MAX_PROBE_SAMPLES` (1 000 000) frames or
  more than the file holds before allocating by it. A Scale key on a
  mirrored layer keys the negative factor (the sign is kept, the magnitude
  floored at 0.001), and a wave-13 position key still means the
  transform's translation: a track's position keys become centres
  (`M(c) - c`, marked by the appended `LayerTrack::centred`) only when it
  first keys scale or rotation, migrated so the frame does not move.
  Opening a video file is still refused by name: re-checked, `rav1d` /
  `re_rav1d` (BSD-2-Clause) abort the process on damaged input and release
  builds abort on panic, and `rav1d-safe` is AGPL-3.0. Known gaps: keying
  scale or rotation drops a layer's shear; the pivot is the canvas-sized
  pixel box's centre, not the painted content's; no audio; no H.264.
  Verified by `editor_core::timeline::tests` (eased and hold values at t,
  scale / rotation about the centre, a flipped layer keying its negative
  scale, a wave-13 position key on a scaled layer staying the translation
  and migrating unmoved),
  `app_shell::timeline::tests::scale_and_rotation_keys_change_the_rendered_frame`,
  `ui::panels::animation::timeline::tests::rotation_keys_and_interpolation_from_the_panel_turn_the_layer_and_its_preview`
  and `raster::codec::formats::mp4::tests::probe_refuses_an_absurd_frame_count_before_allocating`.

### Wave 13 — `06abd74`

Gaps from the confirming Photopea-parity audit of `6e0287d`. The known gaps
below are as they stood at `06abd74`; where wave 13X closed one, the entry
says so.

- **W13-N: Styles / Document Info / Guide Guy panels, Magic Cut, Merge
  Channels, four File ▸ Automate rows, Convert to Point / Paragraph Text.**
  Window ▸ Styles lists the style presets as swatches (a click applies one
  to the active layer, + saves the active layer's); Document Info shows
  size, resolution, mode / depth, profile, layer count and tile memory;
  Guide Guy previews margins, gutters, columns / rows and centre guides and
  applies them as one step. Select ▸ Magic Cut… turns painted foreground /
  background strokes into a GrabCut mask refined by Refine Edge, landed as
  the selection, a layer mask or a new layer (bounded preview texture;
  keymap chords held off while it is up; lands only on the document it was
  painted over). Image ▸ Merge Channels… (a Red / Green / Blue source
  picker),
  File ▸ Automate ▸ PDF Presentation… / Resize Images… / Crop and
  Straighten Photos / Generate Mockups…, and Layer ▸ Text ▸ Convert to
  Point / Paragraph Text. Not built in this wave: New Spot Channel, a
  Custom warp style and Photopea's extra themes (all three came in wave
  13X: W13X-4 and W13X-5). Verified by `ui` `panels::w13n_panel_tests::*`,
  `app-shell` `menu_bridge::w13n_ops::tests::*` and `text-engine`
  `frame_convert::tests::*`.
- **W13-K: File ▸ Script.** Photoshop-DOM JavaScript runs in-process on
  an embedded pure-Rust engine (`boa_engine` 0.21, Unlicense OR MIT): a
  code box, Run and an output log; the DOM subset (documents, layers, layer
  sets, text items, selection, resize / crop / flatten, `alert`) goes
  through the existing menu and command routes, a run is one undo step per
  document, a step budget and time limit stop an endless loop, and a
  `.jsx` opened or dropped runs only when Run is pressed. Verified by
  `app-shell` `script::tests::*` (each mutation-checked).
- **W13-F: Assign / Convert to Profile, Reduce Colors, Wavelet Decompose,
  Pattern Preview, Clear Slices, Slices from Guides.** Edit ▸ Assign
  Profile ▸ (sRGB, Adobe RGB (1998), Display P3, ProPhoto RGB, a profile
  from a file) re-tags without changing a number; Edit ▸ Convert to
  Profile… (destination, rendering intent, black point compensation)
  rewrites every pixel layer through linear light; each is one undo step
  that carries the tag (`editor_core::Command::SetMetaColorSpace`). Image ▸
  Reduce Colors… (palette, 2-256 colours, dither) and Image ▸ Wavelet
  Decompose… (2-7 scales; a residual and N Linear Light detail layers that
  recomposite to the layer within 1/255) are dialogs. View ▸ Pattern
  Preview repeats the composite around the canvas; View ▸ Slices from
  Guides (also on the Slice tool's options bar) and Clear Slices are one
  undo step each. Every new label and message goes through
  `ui::strings::tr`. `color::icc` writes matrix-shaper profiles with their
  media white and reads real ones (the `acsp` signature at byte 36, `para`
  parameters as s15Fixed16, kind 4 as ICC.1:2010 table 68 has it, the
  `wtpt` tag). Verified by `app_shell::menu_bridge::menu_w13f::tests::*`
  through the menu bar, the dialog host and whole chrome frames. Known
  gaps: Perceptual and Saturation convert as Relative Colorimetric
  (matrix-shaper profiles have no tables for them); Wavelet Decompose needs
  an sRGB-tagged 8-bit document.
- **W13-A: Alt+drag with the Move tool duplicates.** Pressed with Alt
  held, a Move drag duplicates the moved layer(s) above their sources and
  moves the copies, or, with a pixel selection, lays a copy of the selected
  pixels down and keeps the originals; either is one undo step, and
  Ctrl+Alt+drag does the same from the tools Ctrl already lends the Move
  tool on (`tool_input::move_duplicate::tests`). Known gaps: no arrow-key nudge exists, so there
  is no Alt+arrow copy (closed by W13X-1); a group is refused rather than
  copied empty (closed by W13X-6).
- **W13-E: action sets and `.atn`.** File ▸ Open reads a Photoshop `.atn`
  (version 16) into a new set in the Actions library, where it had been
  refused. Mapped steps (new layer, selections, Fill, Image / Canvas Size,
  Invert, Desaturate, Equalize, Brightness/Contrast, Gaussian Blur, Unsharp
  Mask, Median, rotate / flip, Save) play through the menu and dialog
  routes. Any other step is listed as skipped and named when Play runs. The
  Actions panel draws the Set -> Action -> Steps tree, with New set,
  Rename, Record into, Export set as `.atn`, Load `.atn`, Play from step,
  Delete set and a check box per step. The exported set reads back through
  the parser (`atn::tests`, and `actions_library::atn_route_tests`, which
  click the drawn controls in the real chrome). Known gaps: the export writes only new-layer and rectangle / empty
  selection steps, counting the rest; Batch plays recorded edits only.
- **W13-G: the last Layer-menu rows.** Layer Style ▸ Create Layers splits
  a style into raster layers (exterior passes under the layer, interior
  effects clipped to it, strokes above) that recomposite the styled original
  within 2/255, and Layer Style ▸ Scale Effects ▸ 25/50/75/150/200% scales
  every size and distance; New ▸ Artboard from Layers wraps the selected
  top-level layers in a transparent artboard at their ink bounds; Layer
  Mask ▸ From Transparency moves the alpha into a new mask; Smart Object ▸
  Reset Transform (source size, upright, same centre) and Stack Mode ▸ the
  eleven statistics (baked into a new layer above the hidden object);
  Animation ▸ Make Frames / Unmake Frames / Merge over the `_a_` frame
  layers. Each is one undo step and tested through the menu bar's click
  route (`menu_bridge::layer_ops_w13::tests`). Each row is greyed with the
  reason the click would give for: no style, a switched-off style (Create
  Layers), a locked layer anywhere (Merge), an empty layer (From
  Transparency), a source with no recorded size (Reset Transform) and an
  embedded PSD declaring fewer than two layers (Stack Mode; at 16 and 32
  bits the count is read from the `Lr16` / `Lr32` block, so a deep layered
  source is not greyed by mistake).
  Known gaps: Scale Effects is fixed percentages, not a percent dialog (the
  dialog host is outside this item's files; W13X-3 made it one, and
  changed the Stack Mode count too); Stack Mode stays enabled, then
  refuses, when a linked source cannot be read, an embedded one fails to
  decode, or all but one of its layers are hidden; Stack Mode is not live; Create Layers
  is inexact for a non-Normal stroke, a non-Normal interior effect under a
  fill below 100%, and an overlapping stroke under an opacity below 100%;
  Create Layers, From Transparency, Stack Mode and Merge are 8-bit only.

- **W13-J: the rest of Photopea's Filter menu.** Distort ▸ Kaleidoscope and
  Dents, Pixelate ▸ Shape Mosaic, Render ▸ Flame, Other ▸ Repeat, Color to
  Alpha, Dither and Particles, the new 3D (Normal Map, Texture Dilation) and
  Fourier (Fourier Transform, Inverse Fourier Transform) submenus, and the
  Filter Gallery's Distort (Diffuse Glow, Glass, Ocean Ripple) and Stylize
  (Glowing Edges) sets; each a live-preview dialog, one undo step or a smart
  filter, with Photopea's controls and defaults — every one but Flame, whose
  Photopea original draws only along a path (W13X-5 gave Flame the path
  and Photopea's controls). Shape Mosaic, Repeat, Color to
  Alpha, Dither, Particles, Kaleidoscope, Normal Map and Texture Dilation
  are ported from Photopea's filter code. The FFT is the crate's own
  (radix-2 plus Bluestein, no new dependency); Fourier then Inverse restores
  the image within 1/255 on the float pipeline and in a 16-bit document
  (tested through the menu); on an 8-bit document the status bar says the
  round trip will not be exact and names 16 Bits/Channel. Known gaps: Flame's
  controls and placement are this build's, and Dents' noise, Glass's
  textures and the gallery effects' pictures are not Photopea's.

- **W13-I: tool options — Line arrowheads, Content-Aware crop, Shape Burst,
  the Eyedropper's Sample choice and ring.** The Line tool draws arrowheads
  at its start and/or end (width and length as a percentage of the weight,
  concavity) into its outline; the Crop tool's Content-Aware box lets the
  box run past the canvas and fills the canvas the crop adds on the active
  raster layer with the PatchMatch fill, in the crop's one undo step; the
  Gradient's Style gains Shape Burst (distance in from the layer's alpha
  edge); the Eyedropper's Sample is Current Layer / Current & Below / All
  Layers (the default now reads the composite in the running app), and a
  ring at the pointer shows the new colour over the old while it is
  dragged. Verified through the registry-built tools
  (`shape::w13i_tests`, `gradient::w13i_tests`,
  `edit::option_tests::content_aware_lets_the_crop_box_run_past_the_canvas`),
  the pointer and commit routes (`tool_input::w13i_tests`) and headless
  options-bar frames (`ui::view::eyedropper_ring::tests`). The Move bar
  gains Quick Export (File > Export > Quick Export Layer as PNG): the
  active layer alone, as one PNG where the user picks. Content-Aware with
  Delete Cropped Pixels keeps the cropped pixels deleted. Known gaps: the
  Content-Aware crop fills only the active layer of an 8-bit document (a
  16- or 32-bit document crops unfilled, with a status note), on
  the interaction thread; a tool preset saved with the old Sample All
  Layers box drops it.

- **W13-L: a video timeline and MP4 export.** The Animation panel gains a
  Frames / Timeline switch; Timeline mode shows a bar per top-level layer
  (in / out points) with opacity and position keyframes (add at the
  playhead, drag, delete), fps and length fields, and a ruler whose scrub
  (and Play, frame by frame) puts the values at that time on the layers
  through `ui::Intent::SeekTimeline` → `Editor::seek_timeline`, so the
  canvas follows live; moving the playhead is no history step and does not
  dirty the document. The timeline is document state (`Document::timeline`,
  `Command::SetTimeline`, one undo step per edit, the playhead kept on
  undo) saved in `.rstudio`. File ▸ Export As ▸ MP4 (and the dialog's
  Format list) offers MP4: AV1 from `rav1e` (BSD-2-Clause, pure Rust, already
  in the tree) in an MP4 container written in `raster::codec::formats::mp4`,
  with a quality field; an animated row writes the timeline at its frame
  rate, rendered through the compositor, or the `_a_` frames with their
  delays. Opening a video file is refused by name (no permissive pure-Rust
  decoder). Verified by `editor_core::timeline::tests`,
  `raster::codec::formats::mp4::tests` (box structure read back: frame
  count, size, durations), `app_shell::timeline::tests` (save / open,
  Export As through `Editor::request_export` and the File menu row),
  `shell::w13l_tests` (a ruler drag and playback in real chrome frames move
  the canvas composite before the release, with no history step) and
  `ui::panels::animation::timeline::tests` (the drawn panel), and by
  ffmpeg 8.1 decoding a written file.
- **W13-C: DNG opens; the vendor RAWs, HEIC and AVIF are refused with the
  exact reason.** `raster::codec::formats::raw` develops a DNG with its own
  code (no new dependency): uncompressed or lossless-JPEG raw data, black
  and white levels, `AsShotNeutral` white balance, an edge-directed Bayer
  demosaic, `ColorMatrix` to sRGB, `BaselineExposure`, the sRGB curve,
  `DefaultCrop` and `Orientation`, into a document that records 16 bits; the
  file picker offers `.dng`. CR2, CR3, NEF, ARW, RAF, ORF and RW2 are
  recognised by content and refused naming the format (their Rust readers
  are LGPL/AGPL). HEIC stays refused: `heic-rs` 0.1.1, the permissive
  decoder, panicked on 3 of 4000 damaged files, which `panic = "abort"`
  makes a crash. Verified against DNGs built from a known scene
  (`formats::raw::tests`), and through File > Open's import job and
  `OpenDocument::open_image` (Open Recent, startup files, drops).

- **W13-H: paint symmetry and the retouching options.** Brush, Pencil and
  Eraser gain a Symmetry drop-down (Vertical, Horizontal, Dual Axis,
  Diagonal, Radial, Mandala; 2-32 segments) that mirrors every dab about the
  canvas centre; the Eraser a Mode (Brush, Pencil, Block); Colour
  Replacement a Mode (Hue, Saturation, Colour, Luminosity), Sampling
  (Continuous, Once, Background Swatch), Limits (Discontiguous, Contiguous,
  Find Edges) and Anti-alias; the Background Eraser Sampling (Once by
  default, unlike Photopea's Continuous; Continuous since W13X-6), Limits and
  Protect Foreground Colour; Sharpen Protect Detail. Verified by the pixels
  each choice paints (`tools::stroke_options::tests`) and through the drawn
  options bar to a real press (`tool_input` `w13h_*` tests). The axis is
  fixed at the canvas centre.

- **W13-D: PDF / AI, WMF / EMF, and the preview formats open.** PDF and
  PDF-compatible `.ai` pages render through `hayro` 0.4 (pure Rust,
  Apache-2.0) at one pixel per point; a multi-page file opens one
  artboard per page on every open route and File > Revert rebuilds them.
  WMF / EMF draw their common GDI records through `resvg`. EPS (TIFF / WMF /
  EPSI preview), Paint.NET (thumbnail), Sketch, XD and ZIP-packaged Figma
  files open their embedded preview and the status line says so; a bare
  `fig-kiwi` canvas is refused by name. `.svgz` joins the Open filter and
  `.cube` gets one. Verified by `raster::codec::formats::{pdf,metafile,
  vector_docs}::tests::*` and `editor::open_any::open_pages::tests::*`.
  Known gaps: no PostScript interpreter (EPS artwork), no Paint.NET layers
  (closed by W13X-7), no Sketch / XD / Figma vector artwork (closed by
  W13X-8); arcs, clipping, dash styles and EMF+ are not drawn; encrypted
  PDFs are refused; JPEG 2000 images in a PDF are not drawn; no page picker
  (closed by W13X-7).
- **W13-M: Photopea's gestures and a real Print dialog.** A bare tool
  letter picks its group and keeps the tool when pressed again, and Shift +
  the letter steps through the group (one rule, `ui::keys::tool_for_letter`,
  for both key routes); Ctrl+F opens Find (Help > Search Commands) and Last
  Filter moves to Alt+Ctrl+F, as in Photopea; the wheel follows Photopea (Alt
  inverts Scroll Wheel Zooms, Ctrl+wheel pans sideways); on a mask thumbnail
  Shift+click disables/enables the mask under a red cross and Alt+click shows
  the mask alone; backslash (rubylith overlay), backquote (mask alone) and
  Escape are canvas keys on the shell's key route; on Windows File > Print
  opens the system Print dialog owned by the app window and prints the
  flattened image through GDI, and the new File > Print as PDF… keeps the
  PDF route (the only Print route on macOS/Linux). Verified by
  `shell::w13m_tests::*`, `editor::print::tests::*` (including a real GDI job
  on "Microsoft Print to PDF", skipped where that printer is absent) and
  `editor::tests::a_tool_letter_keeps_its_tool_and_the_step_cycles_within_its_group`.
  Known gaps: entering a group picks its first tool, not the last used
  (closed by W13X-6); the dialog itself is not driven by a test; no print dialog on macOS/Linux.
- **W13-B: PSD colour labels and the remaining layer-effect forms.** A
  `.psd` layer's `lclr` 0..=7 opens as its colour label and a labelled
  layer is saved with its `lclr` (the false "is not shown by this layers
  panel" note is gone; only an unknown index is named). Layer-effect
  export writes, and import reads back, a gradient- or pattern-filled
  stroke, a gradient-filled outer/inner glow, a gradient overlay's offset
  (`Ofst`, a percentage of the layer box) and repeated instances as the
  `...Multi` lists. Verified by `psd` `effects::w11b_rest_effect_tests::*`
  and `app-shell` `import::tests::every_colour_label_round_trips_through_a_psd_as_its_lclr_index`
  / `repeated_gradient_pattern_and_offset_effects_survive_a_psd_save_and_reopen`;
  not yet checked in Photoshop.

### CI fixes — `c52c407`, `22e31a7`

#### Fixed

- The shortcut-sheet test (`app-shell` `dialog_host_w11g`) failed on the
  macOS runner only: the sheet shows the menu bar's spelling, Shift+Cmd+P
  on macOS, while the test hard-coded Ctrl+Shift+P. `c52c407` spells its
  expectations through `keymap::shortcut_of_chord`, the function the sheet
  itself uses; `22e31a7` does the same for the filtered row's chord, whose
  assertion still used the Windows spelling. Test-only; no product change.

### Docs pass — `6e0287d`

- README, this changelog (one entry per wave 7-11), the parity matrix
  (re-checked at `fe978d3`), the PSD support document, the architecture,
  file-format and threat-model docs and the `app-shell` module docs
  reconciled with the code after waves 7-11; a fresh README screenshot.
  A confirming audit of this commit found fourteen contradictions still
  in the docs; wave 14 corrects the ones waves 13 and 13X left.

### Wave 11 — `fe978d3`

The last gaps from the parity audit of `05ec9b1`.

#### Added

- **PSD adjustment layers open and save live** (W11-A,
  `psd::adjustments::{decode, encode}`): Levels, Curves,
  Brightness/Contrast, Hue/Saturation, Color Balance, Black & White, Photo
  Filter, Channel Mixer, Posterize, Threshold, Gradient Map, Selective Color,
  Exposure, Vibrance and Color Lookup, each under its own key (Invert, which
  opened and saved before, moved into the same module). A payload that
  does not decode is kept as an empty layer and named in the import report;
  a setting the layout cannot store is named on export, never clamped. The
  layouts follow Adobe's published specification and are verified by round
  trips through this build only, not against files Photoshop wrote.
- **PSD inner shadow, inner glow, bevel and emboss, satin and gradient
  overlay** import and export (W11-B, `crates/psd/src/effects_rest.rs`), with contours,
  through the one mapping the `.asl` import also uses. A gradient overlay's
  offset, a gradient- or pattern-filled glow or stroke, and the extra
  instances of a repeated effect are named, not written.
- **PSD guides, saved paths and the work path, alpha channels and slices**
  round-trip (W11-C, `crates/app-shell/src/psd_resources.rs`), and a pixel mask's
  density and feather are written.
- **Every open route handles resource files** (W11-D: the File ▸ Open picker,
  drag-and-drop, Open Recent and the command line, through
  `Editor::open_resource_file` / `Editor::open_any`); Edit ▸ Paste with no
  document opens the clipboard image as a new document; File ▸ Revert.
- **Layer operations** (W11-E): Merge Layers for a multi-selection (Ctrl+E),
  Edit ▸ Transform ▸ Again / Again with Copy, Layer ▸ Arrange ▸ Reverse,
  Select Linked Layers, Smart Object ▸ Convert to Linked / Embed Linked,
  layer colour labels, New Layer Based Slice.
- **Held-key gestures** (W11-F): Alt-click samples a colour while painting,
  Shift-click paints a straight line, Ctrl is a temporary Move tool,
  Ctrl+Space / Alt+Space zoom in / out, Alt+wheel zooms.
- **Object Selection tool; chords; two Help rows** (W11-G): layer navigation
  (Alt+[ / ] / , / .) and blend-mode chords, Ctrl+Shift+F Fade, Ctrl+Alt+R
  Refine Edge, Ctrl+P Print, Help ▸ Keyboard Shortcut Sheet and Help ▸
  Search Commands.
- **Formats** (W11-H): OpenEXR and Radiance HDR open as 32 Bits/Channel
  documents, EXR export, ICNS, IFF and KRA (its merged image) open, Save as
  PSB; lossless JPEG XL export (`zune-jpegxl`) and lossy WebP export with a
  quality setting (`tiny-webp`).
- **CSS panel and tool options** (W11-I): Window ▸ CSS; Align to the
  selected layers' bounds; Dodge / Burn Protect Tones, Sponge Vibrance,
  Gradient Transparency; slices saved with the `.rstudio` document.

#### Changed

- Slice edits are one History step each (`Command::SetSlices`), as in
  Photoshop; undo, redo and history jumps resync the slice store.
- Tests that pinned superseded behaviour (EXR refused, satin not written,
  slices not in history) now pin the new behaviour.

#### Known gaps

- Colour labels are not written to or read from a `.psd` (`lclr`; closed by
  W13-B); Transform
  Again repeats only a whole-layer scale / rotate / skew; the Object Selection
  tool has no hover-to-highlight finder; an Export As EXR row at a scale
  other than 100% writes the clipped 8/16-bit composite; no lossy JPEG XL.

### Wave 10 — `05ec9b1`

Finishes the partial, unverified wave-10 commit `8289491` (stopped by a usage
limit) with eleven doer/reviewer pairs and three follow-up pairs.

#### Added

- **Tools** (W10-A): Content-Aware Move, Slice Select (with File ▸ Export ▸
  Slice Options… and an HTML page carrying each slice's URL and alt text),
  the Spiral shape, Edit ▸ Define Custom Shape, independent link groups.
- **Panels** (W10-B): Layer Comps, Tool Presets, Glyphs, Notes (and the Note
  tool), Character Styles and Paragraph Styles; the Channels panel views and
  edits layer-mask and alpha (saved-selection) channels.
- **Filters** (W10-C, W10-D): Camera Raw (the Basic panel), Lens Correction,
  Lighting Effects, HSB/HSL; the Filter Gallery's Artistic, Brush Strokes,
  Sketch and Texture sets; Displace with an external map; Vanishing Point.
- **File** (W10-E): Automate ▸ Batch / Convert Formats, Image ▸ Variables,
  Export Color Lookup Tables, Image ▸ Vectorize Bitmap, Export PDF, File Info
  (XMP), EXIF carried into exports.
- **Formats** (W10-F): PSB, XCF (layered), PBM / PGM / PPM, DDS and JPEG XL
  open; PBM / PGM / PPM, DDS and AVIF export; SVG export with shape layers as
  real `<path>` elements and text as `<text>`. HEIC and AVIF decoding are
  refused with the reason (no acceptable pure-Rust decoder).
- **Edit** (W10-G): Preset Manager, Fade, Auto-Align Layers, Auto-Blend
  Layers, Perspective Warp.
- **Image** (W10-H): Mode ▸ Bitmap, Duotone and 32 Bits/Channel (`f32`
  tiles, a tested 32-bit Fill), Apply Image, Calculations; Lab adjustment
  layers evaluate in Lab; Indexed Color flattens; 16-bit strokes write
  16-bit dabs. The package's per-tile cap
  (`project_format::tiles::MAX_TILE_BYTES`) is now one `f32` tile (1 MiB),
  so a 32-bit document saves.
- **Layer** (W10-I): Smart Object ▸ Export Contents / New via Copy / Convert
  to Layers / Relink to File; Matting ▸ Remove Black / White Matte; Hide
  Layers; a smart-filter mask, reordering and per-filter blending; Photopea's
  layer-row context menu; the Animation (frame timeline) panel.
- **View** (W10-J): the Snap To submenu, Extras (Ctrl+H), Show ▸ Slices, New
  Guide Layout, New Guides from Shape; Alt+Ctrl+T, Shift+[ / ] hardness,
  number-key opacity; an Artboard option in New Document and an artboard
  Properties page; interactive Content-Aware Scale.
- **Select ▸ Subject** (W10-K) is back without a model: saliency and GrabCut
  on the job worker.

#### Fixed

- `Command::SetSavedSelection`, deleted by a concurrent file restore, is
  restored, with an undo test.

### Wave 9 — `9a61faa`

Daily and frequent gaps from an exhaustive audit against Photopea's own Learn
pages.

#### Added

- Ctrl / Ctrl+Shift / Ctrl+Alt-click on a layer or mask thumbnail loads its
  pixels as a selection (new / add / subtract / intersect); a Select Pixels
  row.
- Live Solid Color / Gradient / Pattern fill layers with their dialogs,
  re-editable, round-tripping through PSD (`SoCo` / `GdFl` / `PtFl`).
- PSD type layers keep their fonts, sizes, colours, runs and paragraphs (a
  bounded EngineData parser); the `TySh` transform follows Photoshop's
  baseline anchor on import and export.
- Clone Stamp, Healing, Spot Healing, Blur, Sharpen and Smudge sample
  Current / Current & Below / All Layers.
- Brush engine: shape, scatter, colour and transfer dynamics with a
  deterministic seed; sampled tips; Define Brush from pixels; `.abr` import.
- Shapes: Path mode, gradient / pattern fills, stroke alignment / cap / join
  / dash, path combine and align, Layer ▸ Combine Shapes.
- Live vector masks (compositor, menu, Properties, PSD `vmsk`).
- Layer styles: exterior effects blend against the backdrop, contours, Blend
  If, multiple effect instances, a style picker, `.asl` import.
- Duplicate Layer into another open document; drag a layer onto a tab.
- Animated GIF / APNG / WebP open as `_a_` frame layers and export animated.
- Text: Warp Text, Type on a Path, Convert to Shape, installed fonts in the
  options bar, `.ttf` / `.otf` open.
- Free Transform numeric options and warp presets; XOR selections and Fixed
  Ratio / Fixed Size marquees; Gradient / Paint Bucket blend modes; the Move
  tool's align / distribute buttons.
- The PSD round trip keeps shape layers, smart objects (`SoLd` / `PlLd`) and
  16-bit depth.
- SVG and `.pat` / `.grd` / `.csh` / `.aco` / `.ase` / `.icc` resources open;
  TGA export.
- Filter ▸ Blur Gallery: Field, Iris, Tilt-Shift, Path and Spin Blur.

### Wave 8 — `f9329d0`

Follow-ups from the wave-7 reviews.

#### Added

- Opacity from Pressure; a pen touching down at zero pressure lays a
  zero-weight first dab; the brush ring follows a hovering pen.
- Levels and Curves offer L / a / b in Lab mode; the Color panel follows the
  document's mode; File ▸ Export of a Lab document says it writes RGB.
- Artboards clip their children and export one file each (a layer outside
  every artboard is left out, pinned by a route test); Perspective Crop has
  corner handles and rectifies every layer; the Mixer Brush previews while
  dragging; the Type Masks show the quick-mask overlay and honour add /
  subtract / intersect; vertical type has a caret and hit-testing.
- The Spot Healing Brush's Content-Aware type runs on the worker and commits
  one step; PSD pattern resources import and map onto pattern effects.

#### Fixed

- Indexed documents always export (palette alpha is quantised too), through
  one shared colour-mode export path.
- Previewing the Mixer Brush no longer changes what it commits (new test).
- The options bar's Reset sits beside the tool name, so a long options row
  can no longer push it off the window.

### Wave 7 — `8b6c399`

Closing the Photopea gaps.

#### Added

- Stylus pressure: winit `Touch` / pen force drives the stroke (size and flow
  from pressure); emulated mouse events are ignored during a contact.
  Verified with synthetic events only; a physical pen still has to confirm
  it.
- Pattern Overlay and pattern-filled strokes / glows render; patterns travel
  with the effect, so save / reopen, undo and export keep them.
- 16-bit documents are edited at 16-bit precision by filters, adjustments,
  fills, transforms, Image Size and Canvas Size.
- Image ▸ Mode ▸ Lab, CMYK (a documented naive RGB-CMYK model, no ICC press
  profile) and Indexed (a palette dialog with dither); View ▸ Proof Colors
  and Gamut Warning show the CMYK round trip.
- Smart filters: Filter ▸ Convert for Smart Filters; filters on a smart
  object stay editable, toggleable and removable, and save with the
  document.
- Tools: Perspective Crop, Vertical Type, Horizontal / Vertical Type Mask,
  Mixer Brush, Artboard, Curvature Pen, Freeform Pen.
- Adjustments: HDR Toning, Match Color.
- Filter ▸ Liquify and Edit ▸ Puppet Warp.
- Content-aware fill (PatchMatch, no model): Edit ▸ Fill ▸ Content-Aware,
  the Spot Healing Brush's Content-Aware type, Edit ▸ Content-Aware Scale.

### Docs pass — `bf12cd8`

- README, the workspace README, this changelog, the parity matrix and the
  architecture / file-format / threat-model docs re-verified against the
  code after the six fix waves.
- **Fixed:** File ▸ Open decodes on a worker and never recorded a 16-bit
  source's depth, unlike drag-and-drop and recent files. Both routes now
  share `OpenDocument::record_source_depth` (route test and mutation check).

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
