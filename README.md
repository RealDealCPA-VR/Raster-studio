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

![Raster Studio's main window with a 3628×2041 layered project fitted at
19.7%: the menu bar, and the Move tool's options bar with Auto-Select, Select
Groups, Show Transform Controls and the Align and Distribute buttons across the
top; the tool column on the left with the colour wells at its foot; rulers
along the top and left of the canvas; a narrow dock column (Navigator, Color
with its HSB sliders, Brushes) beside a wide one (Properties, History with its
Open step, and a Layers panel listing the Alternatives and Logos groups, a
masked Portrait layer and the selected Background); and the status bar with
the zoom, the canvas size, the active tool and the file that was
opened](raster-studio/docs/main-window.png)

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

Eleven waves have landed on `main` since the last spec was closed: six fix
waves (`53dd398`, `2caaa6c`, `02c7e1b`, `1c5b727` + `7bb295a`, `b477a09`,
`0e4a6fd`), then five Photopea-parity waves (wave 7 `8b6c399`, wave 8
`f9329d0`, wave 9 `9a61faa`, wave 10 `05ec9b1`, wave 11 `fe978d3`). The
labels W7-A … W11-I below name the wave that brought a feature. The
[CHANGELOG](CHANGELOG.md) says what each wave changed. The row-by-row state lives
in [`docs/parity-matrix.md`](raster-studio/docs/parity-matrix.md).
[`docs/REMAINING.md`](raster-studio/docs/REMAINING.md) is the historical
2026-08 spec; all its items are closed and it no longer describes the code.

### What you can do in the app

- **Open** PNG, JPEG, WebP, TIFF, GIF, BMP, ICO, TGA and SVG (rasterised at
  its own size by `resvg`, a gzip `.svgz` too, inflated to at most 64 MiB;
  `<image>` references to local files are not followed); W10-F: Netpbm
  PPM / PGM / PBM (ASCII and binary, up to 16 bits), DDS (uncompressed and
  BC1-BC3 / DXT1-DXT5, top mip level), JPEG XL (`jxl-oxide`, pure Rust) and
  GIMP `.xcf` (8-bit RGB / grey, opened as a layered document: one layer
  per GIMP layer at its offset, groups (pass-through kept), opacity,
  visibility, the common blend modes and Dissolve; what does not map -
  Grain extract / merge and unknown modes open as Normal, applied masks
  are baked into alpha, legacy GIMP 2.8 modes blend in linear light here -
  is listed in an "XCF import report" on File > Open; indexed and
  deeper-than-8-bit files are refused by name). W11-H: OpenEXR and Radiance
  `.hdr` open as 32 Bits/Channel documents (the file's linear float samples
  in `f32` tiles, sRGB-encoded with the curve extended past 1.0, so nothing
  brighter than diffuse white is lost; both File > Open roads),
  Apple `.icns` (its largest PNG, ARGB or 24-bit entry; JPEG 2000 entries
  are skipped), Amiga IFF ILBM / PBM (1-8 planes including EHB and HAM6/8,
  24 and 32 planes, ByteRun1) and Krita `.kra` (its merged image only: the
  layers are not read). W13-D: PDF and PDF-compatible Illustrator `.ai`
  pages are rendered by `hayro` 0.4 (pure Rust) at one pixel per point on
  white paper (JPEG 2000 images inside a PDF are not drawn); a
  multi-page file opens one artboard per page (up to 100 pages; encrypted
  PDFs are refused by name). WMF and EMF draw their common GDI records
  (pens, brushes, shapes, polygons, Béziers, EMF paths, world transforms,
  text, DIB bitmaps) through `resvg`; arcs, clipping, dash styles and EMF+
  records are not drawn. EPS, Paint.NET `.pdn`, Sketch, Adobe XD and a
  ZIP-packaged Figma `.fig` open only the preview the file carries (EPS:
  its TIFF, WMF or EPSI preview, since no PostScript interpreter exists
  here; PDN: its flattened thumbnail, since the layers sit in a .NET
  BinaryFormatter graph this build does not read) and the status line says
  it is the preview; a bare `fig-kiwi` Figma canvas is refused by name.
  W13-C: a DNG opens as a document that records 16 Bits/Channel, developed by this
  build's own code (uncompressed or lossless-JPEG raw data, black/white
  levels, white balance from the camera, an edge-directed demosaic, the
  camera colour matrix to sRGB, crop and orientation; no contrast curve,
  lens-correction opcodes or camera profiles). Canon CR2/CR3, Nikon NEF,
  Sony ARW, Fujifilm RAF, Olympus ORF and Panasonic RW2 are refused by
  name: every Rust reader for them is LGPL or AGPL.
  AVIF and HEIC do not open and say why: `rav1d`, the pure-Rust AV1
  decoder this build evaluated, aborts the process on a damaged file (the
  other pure-Rust ones are 0.0.x releases; the BSD `rusty_av1d` fork has
  the same `unwrap()`), and the pure-Rust HEVC decoders are AGPL-licensed
  (`heic`) or, measured for `heic-rs` 0.1.1, panic on damaged files, which
  the release build turns into a crash. Layered `.psd`
  and (W10-F) `.psb`, Photoshop's large-document format (64-bit lengths,
  canvases past 30 000 px), in RGB or greyscale (opened as RGB; other colour
  modes are refused by name) at 8 or 16 bits (a 32-bit file is converted
  down to 8), open as layered documents (groups, masks with their density
  and feather, blend modes; Invert and, since W11-A, Levels, Curves,
  Brightness/Contrast, Hue/Saturation, Color Balance, Black & White, Photo
  Filter, Channel Mixer, Posterize, Threshold, Gradient Map, Selective
  Color, Exposure, Vibrance and Color Lookup adjustment layers as live
  adjustment layers, a payload that does not decode kept as an empty layer
  and named; W11-C: the guides, saved paths and work path, named alpha
  channels and slices; the ten layer effects, with contours on the shadows, glows and bevel (W11-B; W13-B: gradient- and pattern-filled strokes, gradient-filled glows, a gradient overlay's offset and repeated `...Multi` instances too; a pattern-filled glow is named in the report instead), each layer's colour label (`lclr`, W13-B),
  type layers as editable text with each run's font, size, fill, tracking
  and faux bold/italic, the first run's leading and caps and the first
  paragraph's alignment and indents, read from the text engine data, placed
  at Photoshop's anchor (point text's first baseline at its aligned edge,
  box text by its box corner) (W9-C;
  a font this machine lacks keeps its name and its stand-in is named in the
  report, as are a later run's different leading or caps, manual kerning
  and a later paragraph's different style, which are not applied; W9-M: vector shape layers as live
  shape layers — path, solid / gradient / pattern fill, stroke — embedded
  smart objects (`SoLd`/`PlLd` + `lnk2`/`lnk3`/`lnkD`) as live smart objects
  over their placed file, and a 16-bit file as a 16-bit document), with a
  per-layer report of what did not map).
  File ▸ Open (and drag-and-drop) also reads Photoshop resource files into
  their libraries: `.pat` (Photoshop or GIMP) patterns into the pattern
  presets, `.grd` (version 5) gradients: the first one becomes the gradient tools'
  ramp, and every one is a chip in the Gradient Editor's preset strip (after
  the built-in presets; kept in the presets file across restarts), `.aco`/`.ase` swatches into the
  Swatches panel, and an RGB `.icc`/`.icm` profile assigned to the active
  document. `.csh` custom shapes join the Custom Shape tool's Shape list in
  the options bar after the built-in library (kept in the presets file across restarts) and draw like any
  built-in shape; a `.atn` (version 16) becomes a new set in the Actions
  library (W13-E, see Record and replay). File ▸
  Open decodes on a background thread; drag-and-drop, recent files and a
  file named on the command line open on the UI thread. With no document
  open, a start screen offers New / Open / Templates and a recent-files
  grid. File ▸ Open also
  opens a `.rstudio` project when you pick the `manifest.json` inside it.
  W11-D: every route — the File ▸ Open picker, drag-and-drop (onto an open
  document or an empty window), File ▸ Open Recent and a file named on the
  command line — sends a library file to its importer through one routing
  table (`Editor::open_resource_file` / `Editor::open_any`): `.abr` brushes,
  `.asl` styles, the resource files above, `.ttf`/`.otf`/`.ttc`/`.otc` fonts,
  and a `.cube` 3D LUT, which becomes a Color Lookup adjustment layer on the
  active document (one undo step; refused with the reason when no document
  is open). An image dropped onto an open document is still placed as a
  layer. Edit ▸ Paste, from the menu or with Ctrl+V, with no document open
  makes a new document the size of the clipboard's image and pastes it
  (Photopea); with an empty clipboard it creates nothing and says why on the
  status line. File ▸ Revert (under Save
  as PSD) reads the document's `.rstudio` package, or the image it was
  opened from, again and puts its layers, pixels, canvas size, selection,
  guides and document records back as one history step labelled "Revert",
  so Undo takes it back and it asks nothing first; it is greyed for a
  document never saved or with no unsaved changes, and refused for a 16- or
  32-bit document whose file has another bit depth. File ▸ New does not yet
  pre-fill the clipboard image's size.
- **Work in Photopea's layout:** the menu bar, a full-width options bar,
  document tabs over the canvas, and two dock columns holding 26 panels
  (`ui::dock::PanelId::ALL`): Layers, Channels, Paths, Properties,
  Adjustments, History, Navigator, Info, Histogram, Color, Swatches, Brushes,
  Character, Paragraph and Actions; W10-I's Animation; W10-B's Layer Comps,
  Tool Presets, Glyphs, Notes, Character Styles and Paragraph Styles; and
  W11-I's CSS (the active layer's position and size, opacity, fill colour or
  gradient, border and border-radius, box-shadow / text-shadow and text
  colour and font as CSS, with a Copy button); and W13-N's Styles (every
  saved or `.asl`-loaded layer style as a swatch: a click applies it to the
  active layer, + saves the active layer's style), Document Info (size,
  print size, resolution, mode and depth, profile, layer count, memory of
  the distinct pixel tiles) and Guide Guy (margins, columns and rows with
  gutters, centre guides, previewed on a canvas thumbnail and applied as
  one undo step). The
  document is fitted and drawn inside the canvas area, between the tool column,
  the docks and the bars. Workspaces: Essentials, Painting, Photography,
  Minimal. F cycles the screen modes. Light and dark themes (only these two:
  Photopea's other themes are not selectable, because the appearance is the
  two-valued `design::Theme` that the chrome's theme install and the
  Preferences dialog's `ThemeChoice` both match on exhaustively, and W13-N
  did not own those files).
- **Navigate:** pan, zoom (to the cursor, 100%, 200%), fit, rotate view (with
  Reset View Rotation) and flip view. Held-key gestures (W11-F): Space is the
  Hand; Ctrl+Space / Alt+Space turn a click into Zoom In / Zoom Out at the
  point; the wheel follows Photopea (W13-M): Alt inverts the Scroll Wheel
  Zooms preference (with it off Alt+wheel zooms about the pointer, with it
  on Alt+wheel pans), Ctrl+wheel pans sideways when it does not zoom, and
  Shift+wheel pans sideways;
  holding Ctrl lends the Move tool (not on the pen, path, type, transform,
  crop, slice and artboard tools) and letting go gives the tool back. On the
  Brush, Pencil, Colour Replacement, Mixer Brush, Paint Bucket and Gradient,
  Alt-click samples into the foreground through the Eyedropper (its own
  options) and paints nothing. W13-I: the Eyedropper has a Sample choice
  (Current Layer, Current & Below, All Layers, the default reading the
  composite), and while it is dragged on the canvas a ring around the
  pointer shows the colour being picked (upper half) over the foreground
  the drag started from (lower half). On every stroke tool a Shift-click paints a
  straight line from where the last stroke ended. Rulers in any unit, guides (saved with
  the document), smart guides, grid, pixel grid at 800% and above, snapping,
  layer and selection edges, precise cursor. View ▸ Snap To picks the snap
  targets (Guides, Grid, Layers, Slices, Document Bounds; All / None); View ▸
  Extras (Ctrl+H) hides the grid, guides, smart guides, layer edges,
  slices and the marching ants at once and brings back exactly what was
  showing; View ▸ Show ▸
  Slices draws the committed slices; View ▸ New Guide Layout… (columns,
  rows, gutters, margins) and New Guides from Shape each add their guides as
  one undo step. Alt+Ctrl+T duplicates the layer (or floats the selection)
  and free-transforms the copy; Shift+[ / Shift+] step the brush hardness by
  25%; the number keys set the painting tool's opacity (1 = 10% … 0 = 100%,
  two quick digits an exact value).
- **Paint** with the 68 tools of a grouped, Photopea-ordered palette (69
  counting Free Transform, which is `Ctrl+T` and a menu item, not a button),
  including brush, pencil (Auto Erase), eraser, background and magic eraser
  (W13-H: Brush, Pencil and Eraser Symmetry, mirroring every dab Vertical,
  Horizontal, Dual Axis, Diagonal, Radial or Mandala about the canvas
  centre, with 2-32 segments; Eraser Mode Brush / Pencil / Block, the Block
  a 16-pixel square at full strength; Colour Replacement Mode (Hue,
  Saturation, Colour, Luminosity), Sampling (Continuous, Once, Background
  Swatch), Limits (Discontiguous, Contiguous, Find Edges) and Anti-alias;
  Background Eraser Sampling (starting on Once, not Photopea's Continuous),
  Limits and Protect Foreground Colour; Sharpen Protect Detail. The symmetry axis cannot be moved yet, there is no
  on-canvas axis overlay, and the Block is 16 document pixels rather than
  Photoshop's 16 screen pixels);
  clone and pattern stamp, history brush; healing, spot healing, patch,
  Content-Aware Move (drag a selection: Move fills where it was with the
  PatchMatch content-aware fill and blends it in at the drop, Extend copies
  it; one undo step), red eye, colour replacement; gradient (editable stops) and paint bucket, both
  with a blend Mode and Opacity, pattern
  fill; blur, sharpen, smudge, dodge, burn and sponge (W11-I: Dodge / Burn
  Protect Tones and Sponge Vibrance, both on by default, and the Gradient's
  Transparency toggle, off ignoring the ramp's opacity stops; W13-I: the
  Gradient's Style also offers Shape Burst, where the ramp follows the
  distance in from the edge of the layer's own opaque pixels, one drag
  length deep). The clone stamp, both
  healing brushes, blur, sharpen and smudge have a Sample choice (Current
  Layer, Current & Below, All Layers): sampling the composite, they retouch
  onto the active layer even when it is empty. A stroke shows while you
  drag and commits as one undo step; a brush-size ring and per-tool cursors
  follow the pointer. Brush presets and swatches persist. Brushes have
  seeded, replayable dynamics (size, angle and roundness jitter, scatter
  and count, opacity and flow jitter, and per-stroke colour jitter), and a
  brush can have a sampled tip: Edit ▸ Define Brush Preset makes one from
  the active layer's pixels inside the selection, and File ▸ Open of a
  Photoshop `.abr` (v6+ sampled brushes; bounded, including a cap on the
  total decoded pixels) adds its brushes to the Brushes panel. Tip pixels
  are stored as binary files beside the presets, not in the JSON.
- **Select** with the marquees, lasso, polygonal and magnetic lasso, magic
  wand and quick select, each combining by New / Add / Subtract / Intersect /
  Exclude (XOR); the rectangular and elliptical marquees have a Style of
  Normal, Fixed Ratio (W : H) or Fixed Size (W x H px); Color Range, Modify (border, smooth, expand, contract,
  feather), Grow, Similar, Refine Edge, Transform Selection, quick mask, and
  Save / Load / Reselect. Select ▸ Subject selects the region that stands
  out most from the frame's border with no neural model (saliency seeds +
  GrabCut graph cut, run on a worker, one undo step); it is colour-driven,
  not semantic, and a flat image selects nothing and says so. W11-G: the
  Object Selection tool (third in the wand slot, W) takes a dragged
  rectangle, runs the same GrabCut from it on a worker and lands the object
  it finds, combined by the gesture's mode, as one undo step (an idle window
  wakes for it, as for Select ▸ Subject). Gestures draw live, show marching ants and are
  undoable. Ctrl+click a layer thumbnail to select its pixels (Ctrl+Shift
  adds, Ctrl+Alt subtracts, Ctrl+Shift+Alt intersects); on a mask thumbnail
  it selects the mask's coverage; the layer row's right-click menu has Select
  Pixels too. On a mask thumbnail (W13-M, as in Photopea) Shift+click
  disables the mask, crossing the thumbnail out in red, and enables it again;
  Alt+click shows the mask alone on the canvas (Alt+Shift+click as the
  rubylith overlay) and again puts the image back. On the canvas's key route
  (Photopea's `mskView` keys, whether or not the Layers panel is open) the
  backslash key toggles the active layer's mask as a rubylith overlay, the
  backquote shows it alone, and Escape puts the image back.
  Tool letters follow Photopea: the bare letter picks its group and, pressed
  again, keeps the tool; Shift+the letter steps to the next tool of the
  group. Ctrl+F is Photopea's Find, which here opens Help > Search Commands
  (also Ctrl+Shift+P), and Last Filter is Alt+Ctrl+F as in Photopea. Not
  done: entering a group from another tool picks the group's first tool,
  where Photopea picks the one last used from that group (the editor keeps
  no per-group memory); the Layers panel's search field has no chord (Ctrl+F
  is Find).
- **Draw and type:** the Pen drags out smooth curves (Alt breaks a handle);
  add, delete and convert anchor points; path and direct selection. Shape
  tools, including a Spiral (turns, inner radius, direction) and a
  custom-shape library that Edit ▸ Define Custom Shape adds to (the active
  shape layer or the current path, kept in the shape presets), with fill,
  stroke, width and corner radius; a shape's fill can be a colour, the current gradient or the current
  pattern, and its stroke has inside / centre / outside alignment, caps,
  corners and a dash pattern (dash and gap in the options bar; Properties sets
  an even dash). W13-I: the Line tool draws arrowheads at its start and/or
  end, with width and length as a percentage of the weight and a concavity,
  into the shape itself. The shape tools'
  Path mode makes a Work Path instead of a layer; Path Select can unite /
  subtract / intersect / exclude and align a path's components; Layer ▸
  Combine Shapes merges the selected shape layers with the same boolean ops.
  The Paths panel fills, strokes, loads as a selection and makes a
  path from a selection. The Type tool shapes text for real; click back into a
  text layer to edit it; switching tools or pressing Escape keeps what you
  typed. Character: kerning, scale, baseline shift, caps. Paragraph: indents
  and justification. Layer ▸ Text ▸ Warp Text… opens a dialog (style,
  bend, horizontal and vertical distortion) and Layer ▸ Text ▸ Warp Style
  sets a style in one click (Arc, Arc Lower, Arc Upper, Arch, Bulge, Flag,
  Wave, Fish, Rise, Fisheye, Inflate, Squeeze, Twist); the warp bends the
  rendered glyph outlines on the canvas and the text stays editable, but the
  caret and click-to-edit still follow the unwarped layout. A Type click on
  the Work Path (or the selected path) or on a shape layer's outline, where
  the outline is drawn, flows the text along it (Type on a Path); Layer ▸
  Text ▸ Convert to Shape turns the glyph outlines into a shape layer. The
  options bar's Font list offers the installed families, and File ▸ Open of
  a .ttf / .otf / .ttc loads a font for the session.
- **Build a layer stack:** groups, masks (a new mask is made from the
  selection and becomes the paint target), clipping masks and all 27 blend
  modes; opacity and fill, locks, rename, align / distribute (W11-I: two or
  more selected layers align to each other, one layer to the canvas, a
  pixel selection wins over both), Stamp Visible;
  live Solid Color / Gradient / Pattern fill layers (W9-B: each menu row
  opens its dialog — the colour picker, a gradient page with the ramp
  editor, style, angle, scale, reverse and dither, or a pattern page — and
  the layer stays a live source the compositor evaluates over the whole
  canvas, shaped by its mask; re-edit it from its Properties page or Layer ▸
  Edit Adjustment…, Rasterize turns it into pixels, and it saves, reopens
  and round-trips through PSD as `SoCo` / `GdFl` / `PtFl`); adjustment layers edited in
  Properties (W11-A: a `.psd`'s Invert, Levels, Curves, Brightness/Contrast,
  Hue/Saturation, Color Balance, Black & White, Photo Filter, Channel Mixer,
  Posterize, Threshold, Gradient Map, Selective Color, Exposure, Vibrance and
  Color Lookup layers open as live adjustment layers and are written back
  under their own keys (`psd::adjustments`); a malformed payload, a Photo
  Filter stored as XYZ (version 3), a Color Lookup without an embedded
  `.cube` or Curves stored as 256-entry maps is named in the import report
  and kept as an empty layer, and per-range Hue/Saturation settings or
  gradient midpoints the model cannot hold are named; Auto, Desaturate,
  Equalize, Shadows/Highlights, Replace Color, HDR Toning and Match Color
  have no `.psd` adjustment layer and export empty with a note, as does a
  setting the layout cannot store, e.g. Brightness past ±150/255, a Black &
  White weight below -200% or Posterize 256, named with its reason rather
  than clamped. The byte layouts follow Adobe's published specification and
  are proven only by round trips through this build: they have not yet been
  checked against files written by Photoshop or Photopea); layer styles (ten effects, all of which render on the canvas;
  Pattern Overlay tiles a pattern picked from the ones Define Pattern made,
  saved with the document — plus
  Blending Options with Blend If per channel; drop and outer-glow shadows
  blend with their own mode against what is beneath the layer; contours
  on shadows, glows and bevel; several drop shadows, inner shadows,
  strokes and colour / gradient overlays per style; W13-G: Layer Style ▸
  Create Layers splits a style into raster layers — exterior passes under
  the layer, interior effects clipped to it, strokes above — that composite
  the same within 2/255 (not for a non-Normal stroke, a non-Normal interior
  effect under a fill below 100% or an overlapping stroke under an opacity
  below 100%; 8-bit documents only), and Scale Effects ▸ 25 / 50 / 75 /
  150 / 200% scales every size and distance — fixed rows, no percent
  dialog), copy / paste style and
  style presets (Layer ▸ Layer Style ▸ Apply Style Preset… opens the Layer
  Style dialog on a Styles grid of them, and File ▸ Open of a Photoshop
  `.asl` library adds its styles: shadows, glows, bevel, satin, colour /
  gradient / pattern overlays, solid strokes, contours and repeated
  effects; what it cannot map, such as a gradient-filled glow or stroke,
  or a style's Blending Options set away from the default (fill opacity,
  opacity, blend mode, Blend If: a style preset carries effects only), is
  named in the status line); Layer ▸ Link Layers and the Layers panel's
  link button make an independent link group per click (moving one member
  moves its own group only; the panel's button unlinks what its badge shows
  linked; an older document's single link chain opens as one group); smart objects
  (embedded or linked; edit the contents, then commit; W10-I: Layer ▸ Smart
  Object ▸ New Smart Object via Copy gives the copy its own source,
  Export Contents… writes the embedded file byte for byte (a linked one's
  file is copied), Convert to Layers replaces the object in place with a
  group of its contents — a layered PSD's own layers, each posed by the
  object's transform, otherwise one pixel layer at the object's transform —
  and Relink to File… repoints a linked object (greyed, with the reason,
  over an embedded one), each one undo step; W11-E: Convert to Linked…
  writes the embedded source byte for byte to a file you pick and links
  the object to it, Embed Linked reads the linked file back into the
  document, each one undo step for every object sharing the source; W13-G:
  Reset Transform puts the object back at its source's size, upright, about
  its centre, and Stack Mode ▸ Median / Mean / Minimum / Maximum and the
  other seven statistics bakes the statistic across a layered PSD source's
  layers into a new layer above the object, which is kept hidden — baked,
  not live) with
  smart filters —
  Filter ▸ Convert for Smart Filters, then a filter picked from the Filter
  menu's filter rows joins the object's stack instead of rewriting its pixels
  (the Filter Gallery, Last Filter, Liquify and Puppet Warp stay greyed over
  a smart object): each shows as a row under the layer with an eye, a delete
  button and double-click to re-open its dialog at its stored settings (the
  object need not be the active layer); W10-I: a filter's name drags onto
  another filter's row to reorder the stack, and its blending-options button
  opens a row with the filter's own blend mode and opacity; each change is
  one undo step (one drag of the opacity slider is one step), and the stack
  saves with the project (a PSD export writes the filtered look as pixels).
  W10-I: the filters share a filter mask, as in Photopea — Layer ▸ Smart
  Filter ▸ Add / Edit / Disable-Enable / Delete Filter Mask, or the same
  controls on the Smart Filters row (an Add Filter Mask button, then the
  mask's thumbnail well with an eye and a delete button). A new mask is
  white; clicking its thumbnail aims painting at it, and the brush and
  other painting tools paint it (black hides the filters, showing the
  object's unfiltered source there); Duplicate Layer and New Smart Object
  via Copy give the copy its own filter mask, and Image Size resamples it;
  rasterize,
  merge, flatten; W10-I: Layer ▸ Hide Layers / Show Layers (every selected
  layer, one undo step) and Layer ▸ Matting ▸ Remove Black / White Matte
  (partly transparent pixels un-mixed from a black or white matte, one undo
  step); the layer row's right-click menu has Photopea's rows: Blending
  Options, Duplicate, Delete, Convert to Smart Object, Rasterize Layer,
  Enable / Disable Layer Mask, Create / Release Clipping Mask, Link Layers,
  Select Pixels, Copy / Paste / Clear Layer Style, Merge Down, Merge
  Visible, Flatten Image, and (W11-E) the colour labels No Color / Red /
  Orange / Yellow / Green / Blue / Violet / Gray (also Layer ▸ Color
  Label): one undo step for every selected layer, a chip of that colour in
  the row's left margin, saved with the `.rstudio` document and carried by
  Duplicate Layer, and (W13-B) read from and written to a PSD's `lclr`. W11-E: Ctrl+E over
  two or more selected layers is Merge Layers (the row reads so on the
  menu bar and in the row menu): the selected layers, and every layer
  inside a selected group, composite into one layer in the topmost one's
  slot, named after it, one undo step (inside an unselected group the
  result stays there, the group's own opacity / blend / mask not baked
  in); Layer ▸
  Arrange ▸ Reverse reverses the selected layers' order (siblings of one
  group; the unselected layers keep their slots), one undo step; Layer ▸
  Select Linked Layers adds every layer in the selection's link groups;
  Layer ▸ New Layer Based Slice adds a slice over the active layer's ink,
  named after the layer and picked for Slice Options, one undo step (since
  W11-E every slice edit is one History step); Edit ▸ Transform ▸ Again (Shift+Ctrl+T) re-applies the
  last committed whole-layer Free Transform (scale / rotate / skew) to the
  active layer and Again with Copy (Shift+Alt+Ctrl+T) applies it to a
  duplicate, one undo step each (the record is kept for the session; a
  Distort / Perspective / Warp or a floated selection is not recorded).
  Duplicate Layer… has a Destination combo listing every
  open document, and a Layers-panel row dropped on another document's tab
  copies the layer there: the copy (a group with its children, masks,
  effects and a smart object's asset included) lands on top of the target at
  the same position as one undo step in the target, which becomes active.
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
- **Filter:** 71 filters (69 in ten groups, plus two top-level rows), each with a live-preview dialog,
  plus Filter Gallery and Last Filter. The Filter Gallery also holds
  Photoshop's six gallery sets as 47 parameterised effects (Artistic 15,
  Brush Strokes 8, Distort 3, Sketch 14, Stylize 1, Texture 6) in a stackable effect list (New,
  Delete, Up / Down, an eye per entry), previewed at 100% and applied as ONE
  undo step. Distort ▸ Displace over a pixel layer takes an external map — any
  open document, flattened, or an image file (Load…) — with Stretch to Fit /
  Tile and Wrap Around / Repeat Edge Pixels. Filter ▸ Vanishing Point… lays a
  four-corner perspective plane (grid drawn through its homography) and
  clone-stamps or pastes the clipboard inside it with perspective-correct
  scaling; OK is one undo step. Filters respect the selection and an
  isolated channel. Filter ▸ Blur Gallery ▸ Field Blur, Iris Blur,
  Tilt-Shift, Path Blur and Spin Blur each open a dialog with draggable
  handles over a bounded preview (Field pins, the Iris ellipse and focus,
  the Tilt-Shift band, the path's points, the Spin centre and radii); OK
  applies the blur to the active layer as one undo step.
  Filter ▸ Camera Raw… (the Basic panel: temperature, tint, exposure in
  stops, contrast, highlights, shadows, whites, blacks, texture, clarity,
  dehaze, vibrance, saturation, plus a four-region parametric tone curve,
  all on linear light) and Filter ▸ Lens Correction… (barrel/pincushion
  distortion, red/cyan and blue/yellow fringe, vignette amount and
  midpoint, vertical/horizontal perspective, angle, scale) are rows of the
  Filter menu itself; Filter ▸ Render ▸ Lighting Effects… lights the layer
  with one spot, point or infinite light (colour, intensity, focus, radius,
  angle, ambience, a texture-channel bump) placed by dragging on the
  preview; Filter ▸ Other ▸ HSB/HSL… rewrites the RGB channels as HSB or
  HSL values and back, as Photoshop's does. Each is one undo step, or a
  smart filter on a smart object.
  W13-J adds the rest of Photopea's Filter menu, each a generated dialog
  with live preview, one undo step, or a smart filter, with Photopea's own
  controls, ranges and defaults (read from its filter descriptors and
  dialogs): Distort ▸ Kaleidoscope (Mirrors 2-20, Angle) and Dents (Scale,
  Refraction, Turbulence); Pixelate ▸ Shape Mosaic (Cell Size; Square,
  Circle or Star; Spread XY / X / Y; Monochromatic; Invert); Other ▸
  Repeat (Scale, Row Shift, Space X / Y, Auto Color, Angle), Color to
  Alpha (colour, black by default; Transparency and Opacity Threshold),
  Dither (Palette: Black & White, RGB 2x2x2 / 4x4x4 / 8x8x4; Method: None,
  Floyd-Steinberg, Bayer 4x4) and Particles (Count, Size, Depth,
  Brightness, Color, Time, Turbulence, Blink, Fall); 3D ▸ Normal Map
  (Photopea's Generate Normals: Blur, Scale, Invert, High / Medium / Low
  detail) and Texture Dilation (Crop, Radius); Fourier ▸ Fourier Transform
  and Inverse Fourier Transform (log magnitude and phase as an editable
  image, with a pure-Rust FFT of the crate's own). Shape Mosaic, Repeat,
  Color to Alpha, Dither, Particles, Kaleidoscope, Normal Map and Texture
  Dilation follow Photopea's algorithms, ported from its code (Particles
  with its random generator, so a seed scatters them the same way). The
  gallery's Distort set is Diffuse Glow, Glass (Photopea's seven textures
  and Invert Texture) and Ocean Ripple, and its Stylize set is Glowing
  Edges, with Photopea's sliders and defaults. Not done: Flame is not
  Photopea's — Photopea's draws only along the active path, with eighteen
  controls, and the filter pipeline hands a filter no path, so this one
  renders seeded flames at random with its own five controls; Dents'
  noise, Glass's textures and the four gallery effects' pictures are this
  build's, not Photopea's; Dents' Detail and seed are fixed at Photopea's
  defaults, as its dialog does not show them. A Fourier round trip is
  within 1/255 in a 16-bit document (tested through the menu; 32-bit is
  not tested); an 8-bit document stores the spectrum in 8 bits and comes
  back off by several levels (7/255 at worst on the test picture), and
  the status bar says so and names 16 Bits/Channel when Fourier Transform
  runs there.
- **Transform and crop:** Free Transform (scale, rotate, skew, distort,
  perspective, warp), with an options bar that reads the live box back —
  reference-point grid, X / Y, W / H %, Link, Angle, H / V Skew,
  Interpolation (Nearest / Bilinear / Bicubic) — and eleven warp presets
  (Arc, Arch, Bulge, Flag, Wave, Fish, Rise, Fisheye, Inflate, Squeeze,
  Twist) with a Bend; a typed field or a picked preset reshapes the live
  box on the canvas in the same frame (the first edit after the options
  bar's Reset included), and Enter commits what the fields say. The Move tool's options bar has Align (left, centre, right, top,
  middle, bottom) and Distribute (horizontally, vertically) buttons that run
  the Layer menu's commands; with a partial pixel selection, Free Transform and Move
  lift just the selected pixels, as one undo step. W13-I: the Move bar's
  Quick Export button (also File > Export > Quick Export Layer as PNG)
  writes the active layer alone, at canvas size over transparent, as one
  PNG where the user picks. W13-A: Alt+drag with the Move tool moves a copy —
  the selected layer(s) are duplicated above their sources, or, with a pixel
  selection, a copy of the selected pixels is laid down and the originals
  stay — as one undo step; Ctrl+Alt+drag does the same from the tools Ctrl
  already lends the Move tool on (not from the Hand, Zoom, Rotate View, pen,
  path, type, Free Transform, crop, slice or Artboard tools, which keep
  Ctrl). A group is refused rather than copied empty, and there is no
  arrow-key nudge yet, so no Alt+arrow copy either. Crop with ratio presets (Free, Original, 1:1,
  4:3, 16:9, 3:2, 5:4, W × H × resolution), straighten, Delete Cropped
  Pixels and (W13-I) Content-Aware: the box may then be dragged past the
  canvas, and the canvas the crop adds is filled from the active pixel
  layer with the PatchMatch content-aware fill, in the crop's one undo step
  (8-bit documents only — a 16- or 32-bit document crops unfilled and the
  status line says so; the active raster layer only, synthesised while the
  crop waits); with Delete Cropped Pixels also on, what the crop deletes
  stays deleted in the tiles the fill shares. Trim, Crop to
  Selection, Reveal All, Image Size, Canvas Size, rotate or flip the canvas.
  The Ruler tool straightens a layer; a four-point Color Sampler reads values.
- **Channels:** isolate one RGB component, then paint, erase, fill, filter or
  bake an adjustment into it alone. W10-B: a layer-mask row shows the mask
  alone in grayscale and aims painting at it, and a saved selection's eye
  opens it as an alpha channel you can paint, stored back when you close it
  (see Known gaps).
- **16-bit:** create a 16-bit document, or convert with Image ▸ Mode (one
  undoable step). It composites at 16 bits, saves and reopens as `.rstudio`
  with its 16-bit tiles, and exports 16-bit PNG and TIFF. Filters,
  adjustments, Fill, Clear, Free Transform, every flip and rotation, Image
  Size, Canvas Size, Crop to Selection and Trim compute at 16 bits; a few
  edits still round to 8 bits (see below).
- **32-bit (W10-H):** Image ▸ Mode ▸ 32 Bits/Channel (RGB and Grayscale
  documents) converts every layer tile to `f32` in one undoable step (8/16 →
  32 is lossless; 32 → 16/8 clips values above 1.0, with no HDR Toning on
  the way down). Values above 1.0 are kept: the compositor, the filters and
  adjustments that run through the shared whole-layer route (Filter ▸ Blur,
  for one), Rotate 180°/Flip Canvas/the layer flips, Edit ▸ Clear, Edit ▸
  Fill, Image Size and `.rstudio` save/reopen carry them, and
  File ▸ Export to `.tif` writes a 32-bit float TIFF of the linear
  composite. Limits: samples are stored in the document's encoding, not
  linear light as in Photoshop; tools and every other edit compute at 8 or
  16 bits on the clipped layer and land back as `f32`: a sample keeps its
  old `f32` value where the output matches it and it is inside 0..1, and
  keeps an HDR value only under a stroke of a brush, eraser, fill,
  gradient, toning or Red Eye tool whose output equals the whole old pixel
  clipped. The boundary cannot tell a pixel the stroke left alone from one
  it painted to that clipped colour, so painting the clipped colour over
  HDR (white at 100% over a 3.0 highlight, for one) leaves the HDR value in
  place and a float-TIFF export keeps it; elsewhere (Free Transform, Patch, Clone, any other 8/16-bit edit) HDR values in the tiles the edit
  writes clip to 1.0.
  A 32-bit layer cannot be dragged into an 8- or 16-bit document. There is
  no 32-bit New Document. W11-H: an EXR or HDR opens into a 32-bit
  document, and File > Export to `.exr` (and an Export As EXR row at 100%)
  writes its float composite, values above 1.0 included; other formats and
  scaled rows still write the clipped 8/16-bit composite, and Save as PSD
  writes an 8-bit file of clipped layers.
- **Edit:** Cut, Copy, Copy Merged, Paste, Paste in Place, Paste Into, Paste
  Outside, Clear (Delete or Backspace); Fill and Stroke dialogs, with
  Alt+Backspace / Ctrl+Backspace filling with the foreground / background
  colour; Fill ▸ Contents: Content-Aware (PatchMatch synthesis from the rest
  of the layer, also the Spot Healing Brush's Content-Aware type) and Edit ▸
  Content-Aware Scale (seam carving: ▸ With Handles, Alt+Shift+Ctrl+C, is
  the Free Transform box in its Content-Aware mode — drag the handles, set
  the Amount, Enter commits one step — and 80/90/110/125% presets per axis
  follow it); Define
  Pattern and Define Brush; Step Forward / Backward; a History
  panel with thumbnails and a source marker; Purge; customisable keyboard
  shortcuts that apply at once; a right-click canvas menu. W11-G: Help ▸
  Keyboard Shortcut Sheet… (Shift+/, the `?` key) lists every chord the live
  keymap answers and filters as you type; Help ▸ Search Commands…
  (Ctrl+Shift+P) filters, by name, the menu rows the menu bar enables at
  that moment (the same per-frame state: Paste after a Copy, Revert for a
  saved file) and runs the one you pick (a row with a dialog opens it); Alt+[ / Alt+] / Alt+, / Alt+. select
  the layer below / above / the bottom / the top layer; Shift+Alt+N / M / S /
  O and Photoshop's other blend-mode letters, and Shift+Plus / Shift+Minus,
  set the active layer's blend mode, or, with a painting tool active
  (Brush, Pencil, Clone Stamp, Pattern Stamp, Gradient, Paint Bucket), that
  tool's options-bar Mode as Photoshop does; Ctrl+Shift+F is Fade, Ctrl+Alt+R Refine
  Edge, Ctrl+P Print. W10-G: Edit ▸
  Fade <last step>… (the row names the step, e.g. "Fade Apply Invert…";
  opacity and blend mode for the last filter, adjustment, fill or stroke
  against the pixels it replaced, one step replacing that step; pixels the
  step did not change, such as those outside its selection, are kept
  exactly; greyed, with the reason, after anything else or after an undo);
  Edit ▸ Preset Manager… (brushes, swatches, gradients, patterns, styles,
  custom shapes and tool presets: rename, delete, reorder, import / export
  as JSON; swatches and tool presets are the Swatches and Tool Presets
  panels' lists, updated on the next frame); Edit ▸
  Auto-Align Layers… (the bottom selected layer is the reference; Auto =
  FAST corners + steered BRIEF + brute-force matching + RANSAC similarity,
  Reposition = phase correlation; the result is layer transforms, one undo
  step); Edit ▸ Auto-Blend Layers… (Panorama: minimum-difference seams
  feathered across each overlap; Stack: focus stacking by per-pixel
  Laplacian energy; the result is one new merged layer on top, not per-layer
  masks, and the sources are kept); Edit ▸ Perspective
  Warp (a dialog, not on the canvas: draw quads in Layout, drag corners in
  Warp, one homography per quad, one undo step).
- **Record and replay** actions in the Actions panel; named actions show their
  steps and save to disk. W13-E: actions live in sets. File ▸ Open of a
  Photoshop `.atn` (version 16) adds its set; each step keeps its parameters
  and plays through the same route as the menu row or dialog OK: new layer;
  select all, none, inverse or a rectangle; Fill; Image Size; Canvas Size;
  Invert; Desaturate; Equalize; Brightness/Contrast; Gaussian Blur; Unsharp
  Mask; Median; rotate or flip the canvas or the layer; Save. A step with no
  equivalent here stays listed, marked "skipped on play" with the reason,
  and Play names it on the status line. An unchecked step is passed over.
  Under its flat list the Actions panel draws the sets as a tree (set,
  then its actions, then each step with a check box and the skip reason).
  Its buttons make a new set, rename the selected set in place, choose the
  set recordings join, export the set as `.atn` (the parser reads it back),
  load a `.atn`, play from the selected step and delete the set. Known
  gaps: the export writes a recorded step only when it is a new layer or a rectangle
  or empty selection; any other recorded step is left out and counted.
  Batch plays only an action's recorded edits, not its imported steps.
- **Cut out, merge channels, automate, convert type** (W13-N): Select ▸
  Magic Cut… paints foreground / background strokes over the active layer
  in a window; GrabCut turns them into a mask, Refine Edge's smooth / shift /
  feather run over it, and OK lands it as the selection, a layer mask or a
  new layer, one undo step (the pane previews a copy at most 512 px long,
  so a layer past the GPU's texture limit opens; while the window is up no
  keymap chord reaches the document, and the cut lands only on the document
  and layer it was painted over; in Full Screen Mode, which has no menu bar
  to host it, the row refuses to open the window and says to leave Full
  Screen first, as do Merge Channels, Resize Images and Generate Mockups). Image ▸ Merge Channels… asks which open
  grayscale document of one size feeds red, green and blue, and builds a
  new RGB document (RGB only; it is in the Image menu, not the Channels
  panel's menu). File ▸
  Automate ▸ PDF Presentation… writes every open document as one page of
  `Presentation.pdf` (each page its document's size at 72 ppi); Resize
  Images… fits every image of a folder into a box and writes it in its own
  format; Crop and Straighten Photos opens each photo scanned onto a flat
  background as its own straightened document; Generate Mockups… shows each
  image of a folder in the active smart object and exports a PNG per image,
  leaving the document as it was. Layer ▸ Text ▸ Convert to Point Text puts
  a line break where each line of a box wrapped (text past a fixed box's
  bottom is kept, unlike Photoshop); Convert to Paragraph Text boxes point
  text to its laid-out size; one undo step each. Not built: New Spot Channel
  (no spot-channel record in `layer_model` and no ink compositing in
  `compositor`, neither owned by W13-N) and a Custom warp style (an editable
  warp mesh needs a new `layer_model::text::WarpStyle` variant and the Warp
  Text dialog, also outside W13-N).
- **Run scripts** (W13-K): File ▸ Script… opens a code box, a Run button and
  an output log. Scripts are Photoshop-DOM JavaScript, run by an embedded
  pure-Rust engine (`boa_engine`): `app.documents` / `activeDocument` /
  `documents.add` / `foregroundColor` / `backgroundColor` / `open`;
  `doc.width` / `height` / `name` / `resolution` (always 72: the document
  stores none), `layers` / `artLayers` / `layerSets` (with `add` and
  `getByName`), `activeLayer`, `resizeImage` / `resizeCanvas` / `crop` /
  `flatten` / `mergeVisibleLayers` / `saveAs`; `layer.name` / `opacity` /
  `fillOpacity` / `visible` / `blendMode` / `kind` (set to `LayerKind.TEXT`)
  / `bounds` / `parent` and `translate` / `resize` / `rotate` / `duplicate`
  / `remove` / `merge`; `textItem.contents` / `size` / `font` / `color` /
  `position`; `selection.select` (a polygon, replace / extend / diminish /
  intersect) / `selectAll` / `deselect` / `invert` / `fill` / `clear` /
  `bounds`; `alert`, `console.log`, `$.writeln`. Each call goes through an
  existing editor route (a menu action, `Editor::dispatch` or an editor
  command such as the one Select All applies), and a run is ONE undo step
  per document it changed (`Script` in History). A run is stopped by a step
  budget and a time limit, so an endless loop ends with a message; a syntax
  error or an uncaught exception is reported in the log. A script has no
  file or network access of its own: `app.open` and `saveAs` open the
  platform pickers. A `.jsx` / `.js` opened with File ▸ Open or dropped on
  the window opens in the Script window and runs only when Run is pressed.
- **Save** the native `.rstudio` package: compressed tiles, unchanged tiles
  reused, written off the UI thread, with autosave and crash recovery
  (commands made while a save runs are journaled beside the package, so a
  crash during a save loses none of them).
- **Animations** (W9-J, Photopea's convention): an animated GIF, APNG or
  animated WebP opens as one raster layer per frame, named
  `_a_Frame <n>,<delay ms>`, frame 1 at the bottom and the only one visible
  (GIF disposal and frame offsets composited; at most 1000 frames and 1 GiB of
  decoded frames — past that, or with a damaged later frame, the file opens as
  its first frame as before). Export As offers **Animated** on GIF, PNG (APNG)
  and WebP rows when the document has `_a_` layers (on by default; the
  caption counts the frames) and writes one looping frame per top-level
  `_a_` layer, bottom first, with every other layer as the document has it
  (a hidden non-frame layer shows in no frame); a
  document without `_a_` layers exports a still. W10-I: Window ▸ Animation
  is the frame timeline over those layers — one thumbnail per frame in play
  order with its delay field under it (editing it renames the layer's
  `,<ms>`; a drag of the field lands once, on the release, and a typed
  value once, on Enter, each as one undo step), clicking a frame shows it alone, Play / Stop previews the frames
  at their own delays, an onion-skin switch draws the previous frame faintly
  under the current one, and Add / Duplicate / Delete frame are ordinary
  undoable layer edits (playback and onion skin are view state, never
  history). W13-G: Layer ▸ Animation ▸ Make Frames / Unmake Frames rename
  the selected top-level layers to and from `_a_<name>,100`, and Merge
  replaces every top-level layer with one flattened frame per `_a_` frame
  (the exported animation is unchanged; 8-bit documents only), each one
  undo step. Also W13-G: Layer ▸ New ▸ Artboard from Layers wraps the
  selected top-level layers in a transparent artboard at their ink bounds,
  and Layer ▸ Layer Mask ▸ From Transparency moves a pixel layer's alpha
  into a new mask (8-bit documents only), each one undo step.
- **Video timeline and MP4 export** (W13-L). The Animation panel has a
  Frames / Timeline switch (document state, one undo step). In Timeline
  mode every top-level layer gets a bar from its in point to its out point
  (drag either end) carrying opacity and position keyframes: Opacity key /
  Position key add one at the playhead to the active layer holding its
  current value, a key drags to a new time and the trash button deletes the
  selected one; values interpolate linearly between keys and hold outside
  them. The panel sets the frame rate (1-60 fps) and the length (0.1 s to
  10 min). Clicking or dragging the ruler moves the playhead and puts each
  tracked layer's visibility, opacity and position at that time on the
  layers on every frame of the drag, so the canvas follows the scrub; Play
  does the same for each frame it advances (the panel's well also shows
  the layer thumbnails at that time) and Stop leaves the canvas on the
  frame it stopped at. As in Photopea, moving the playhead is not an edit:
  no undo step, no unsaved-changes flag, and undoing a timeline edit
  leaves the playhead where it is. Every other edit is one undo step, and
  the timeline is saved in the `.rstudio` package. File ▸ Export As ▸ MP4
  (or the Export As dialog's Format list) offers **MP4**
  (AV1 through the pure-Rust `rav1e` encoder, in an MP4 container this
  build writes itself; 8-bit 4:2:0, a quality field, the row's size or
  scale, at least 16x16, transparency flattened onto white): with
  Animated on, a Timeline-mode document writes one frame per 1/fps second
  rendered through the compositor, a Frames-mode document one frame per
  `_a_` layer with its own delay (the dialog's caption names which: the
  timeline's frame count, fps and length, or the `_a_` layers); a still
  writes one frame. Not done: the
  timeline rows are top-level layers only (a group moves as one), there is
  no easing (linear only), no scale / rotation keys, no audio, and no
  H.264 (only AV1: `openh264` needs Cisco's C library). Because a seek
  writes the tracked values straight onto the layers, undoing an older
  layer edit after a scrub can leave a tracked value off the timeline
  until the playhead next moves. Opening a video
  file (MP4, MOV, WebM, AVI) is refused by name: no permissively licensed
  pure-Rust video decoder exists (the pure-Rust AV1 decoder `rav1d` aborts
  on damaged input), so there are no video layers.
- **Export** PNG, JPEG, WebP (lossless, and since W11-H lossy, below), TIFF,
  GIF, BMP, TGA, ICO, SVG (at 100%, shape layers as `<path>` and text as
  `<text>`, the rest as embedded PNGs; see Known gaps) and (W10-F) PPM / PGM /
  PBM, DDS (uncompressed
  BGRA or BC3 / DXT5) and AVIF (8-bit 4:4:4 with alpha and a quality
  slider, `ravif`; Export As estimates its size but cannot preview it, as
  nothing here reads AVIF back); W11-H: EXR (32-bit float, linear light,
  premultiplied; from the 8/16-bit composite, or a 32-bit document's float
  composite), lossless 8-bit JPEG XL (`zune-jpegxl`, pure Rust; 2x2 px or
  larger) and lossy WebP with a Quality setting (`tiny-webp`, pure Rust: one
  VP8 key frame, exact but uncompressed alpha) as Export As rows, EXR and
  JPEG XL by name from File ▸ Export too; and File ▸ Export / Save as PSD to
  a `.psb` writes Photoshop's large document format (version 2, 64-bit
  lengths); Save as PSD on a canvas past 30 000 px offers `.psb` first, and
  such a canvas under a `.psd` name is refused, naming `.psb`. Export As has
  presets and writes several rows (format and scale) in one run;
  Export Layers and File ▸ Export ▸ Slices (the Slice Select tool moves,
  resizes by an edge or corner, and Alt+click or Delete on the picked slice
  deletes the committed slices; the canvas draws the picked slice
  emphasised and labels every slice by its name; each slice keeps its name
  through edits and is exported under it, `<document>_NN` for the Slice
  tool's own names, a given name sanitised to a safe file stem (characters
  other than ASCII letters, digits, `- _ ( )` and space become `_`, cut at
  64); File ▸ Export ▸ Slice Options… edits the picked slice's name
  (refused when it would export to another slice's file), URL and alt text, and a slice with a URL or alt text makes the
  export also write `<document>.html`, laying out the images with their
  links and alt text; W11-I: the slices, with their names, URLs and alt
  text, are saved in the `.rstudio` document and restored when it is
  opened (every open route, File ▸ Open's background job included), and a
  slice edit in one document never touches another's saved set);
  layered PSD (W9-M: 16-bit for a
  16-bit document; shape layers as vector shape layers, embedded unfiltered
  smart objects as placed layers with their file embedded, pattern overlays
  with their pattern); print as PDF.
  Duplicate a document, Close Others, Close All.
- **File automation and data-driven exports (W10-E).** File ▸ Automate ▸
  Batch… plays a recorded Action (from the Actions panel's library) on every
  image in a folder and saves each result to another folder in a chosen
  format / JPEG quality / scale; Convert Formats… does the same with no
  Action. Both run on the job worker with "n of N" progress on the status
  line, skip a file that fails and list it in `batch-errors.txt`. An Action
  replays what it recorded: parametric steps (a new adjustment or fill
  layer, a layer property) apply to each file as themselves, a pixel step
  replays its recorded pixels. Image ▸ Variables ▸ Define… binds
  text-replacement variables to text layers and visibility variables to any
  layer; Data Sets… imports a CSV (header = variable names, one row per
  set), previews a set on the document (one undo step) and exports one
  flattened file per set. File ▸ Export ▸ Color Lookup Tables… writes the
  visible adjustment-layer stack, sampled on a 17 / 33 / 65-point identity
  lattice, as a `.cube`; File ▸ Export ▸ PDF… writes a raster PDF on an
  image-size (at a chosen ppi), A4 or Letter page with File Info as its
  `/Info`. Image ▸ Vectorize Bitmap… traces the active layer into one shape
  layer per colour (median-cut posterize, a stacked colour layering so the
  shapes tile the image, crack-following contours, Schneider Bézier fitting
  — documented in `vector::trace`), one undo step. File ▸ File Info… edits
  the XMP title, author, description, keywords and copyright; Export As
  writes them into PNG, JPEG and TIFF files and carries a JPEG / PNG
  source's EXIF into JPEG exports (a Metadata checkbox turns both off).
  File Info and the variables are kept per open document for the session: a
  `.rstudio` save does not store them yet.
- **Profiles, Reduce Colors, Wavelet Decompose, Pattern Preview, slice rows**
  (W13-F). Edit ▸ Assign Profile ▸ sRGB / Adobe RGB (1998) / Display P3 /
  ProPhoto RGB / Profile from File… re-tags an RGB document without
  changing a pixel number, one undo step (`Command::SetMetaColorSpace`).
  Edit ▸ Convert to Profile… is a dialog: the destination profile, the
  rendering intent (Perceptual, Saturation, Relative or Absolute
  Colorimetric) and black point compensation; it rewrites every pixel
  layer's numbers through linear light so the picture keeps its colours
  and re-tags, numbers and tag in one undo step. These are matrix-shaper
  profiles with no perceptual tables, so Perceptual and Saturation convert
  as Relative Colorimetric (the dialog says so); Absolute Colorimetric
  scales by the two profiles' `wtpt` media whites; colours outside the
  target clip. The document's own built-in profile is ticked in Assign
  Profile and cannot be the Convert destination. Adobe RGB and ProPhoto are
  written as spec-conformant ICC profiles (`color::icc::matrix_shaper_profile`),
  so an export carries them. Image ▸ Reduce Colors… (palette source, any
  count from 2 to 256, dither) puts the active layer on a palette, one undo
  step; the document stays RGB. Image ▸ Wavelet Decompose… (2 to 7 scales)
  hides the active layer and adds a residual and N Linear Light detail
  layers above it, one undo step; the stack recomposites to the source
  within 1/255 on opaque pixels. View ▸ Pattern Preview draws the
  document's composite repeated around the canvas, a view toggle. View ▸
  Slices from Guides (also a button in the Slice tool's options bar) cuts
  the canvas into one slice per guide cell and View ▸ Clear Slices removes
  them, one undo step each. Not yet: Wavelet Decompose and Reduce Colors
  work on 8-bit RGB pixel layers, and Wavelet Decompose is greyed on a
  document tagged with anything but sRGB (it solves the split against the
  sRGB decode); Pattern Preview draws the copies from a composite capped
  at 2048 px on its long edge, up to 12 copies out.

### What is still missing vs Photopea

**Absent, and deferred for this release** — the Tier C rows of the parity
matrix, each with its reason there:

| Missing | Why |
| --- | --- |
| ICC-accurate CMYK, spot colours, Lab files | Since W7-D: Image ▸ Mode ▸ Lab / CMYK / Indexed convert (one undo step each; CMYK on a documented naive ink model, not an ICC press profile; Indexed through its own dialog); File ▸ Export and Export As write a CMYK document as CMYK JPEG/TIFF and an Indexed one as a palette PNG (GIF keeps its colours) — since W8-B the palette PNG always writes: an image past 256 RGBA colours (a soft stroke painted after the conversion) is re-quantised with 1-bit alpha, as Photoshop's Indexed stores it; Export As says when a format writes the document as RGB instead (always, for Lab), and since W8-B File ▸ Export says so in the status line; Info adds a Lab or CMYK row for a document in that mode, and since W8-B the Color panel switches to Lab / CMYK / Gray (K%) notation when the document is in that mode (the user can still pick another); since W8-B Image ▸ Adjustments ▸ Levels and Curves on a Lab document list Lightness / a / b (no composite row; they open on Lightness, so a first move keeps greys neutral) and preview and apply on those channels, and since W10-H so does a Levels/Curves *adjustment layer* in a Lab document; since W10-H Indexed Color flattens a layered document (one undo step, and the status line says so), Image ▸ Mode ▸ Bitmap… (threshold, pattern / diffusion dither, halftone screen) and Duotone… (1-4 inks with curves, baked into the pixels) convert from Grayscale, and Image ▸ Apply Image… / Calculations… exist; View ▸ Proof Colors and Gamut Warning are enabled and change the canvas. Still missing: a press profile and spot colours, any Lab file (Lab goes out as RGB, and both export routes say so), and re-editable duotone inks. |
| Proprietary camera RAW files; the vector artwork of EPS / Sketch / XD / Figma files and Paint.NET layers | W13-C: DNG opens, but CR2 / CR3 / NEF / ARW / RAF / ORF / RW2 are refused by name (no permissively licensed reader exists; convert to DNG); W13-D: PDF / AI pages open (see Open above), but EPS needs a PostScript interpreter and Sketch / XD / Figma / PDN their proprietary document models, so only their embedded previews open. |
| Video layers, audio, H.264 | W13-L added the video timeline (per-layer in/out bars, linear opacity and position keyframes, a playhead whose scrub and playback move the canvas live with no history step, saved in `.rstudio`) and MP4 export (AV1, File ▸ Export As ▸ MP4), see Video timeline and MP4 export above. Still missing: opening a video file as a video layer (no permissively licensed pure-Rust decoder, so MP4 / MOV / WebM / AVI are refused by name), audio, H.264 output (`openh264` needs a C library), easing and scale / rotation keys. |
| Collaboration, cloud storage, sharing online, mobile | Non-goals: this is a local-first desktop application whose own code makes no network calls, so nothing that needs a server is offered. |

**Absent, and not yet decided** (the parity matrix lists the same):

- Filter ▸ Adaptive Wide Angle.
- Scripts beyond the DOM subset under Run scripts (W13-K): no
  `executeAction` / action descriptors, no adjustments or filters called
  from a script, no `doc.close()`, no file reads or writes a script chooses
  itself (by design: `app.open` and `saveAs` ask with the pickers), and a
  `feather` on `selection.select` is refused. A single builtin call that
  runs long by itself (`"x".repeat(1e9)`) is not interrupted until it
  returns.
- Take a Picture (capturing an image from a camera).
- Generative and neural-model features (AI fill, model-based cut-outs): no
  model ships. Select ▸ Subject and the Object Selection tool are classical
  (saliency and GrabCut), colour-driven rather than semantic.
- Lossy JPEG XL export: File ▸ Export and Export As write lossless JPEG XL
  only, because no pure-Rust lossy JPEG XL encoder exists.
- Opening AVIF or HEIC: both are refused by name, with the reason (see Open
  above): no pure-Rust decoder this build accepts. AVIF *export* works.

**Known gaps in what exists:**

- **Vector masks are live; their path cannot be edited in place.** W9-G: a
  layer's vector mask (a path with its own enable, density, feather and
  invert) is rasterised by the compositor and multiplied with the pixel mask;
  Layer > Vector Mask > Reveal All / Hide All / Current Path / Delete /
  Disable-Enable are one undo step each. The Layers panel draws a vector-mask
  thumbnail (the path's rendering, crossed out while disabled), and the
  Properties panel edits the vector mask's density, feather, invert and
  enable (a vector-only mask shows only those rows). Layer Mask > Apply bakes
  the pixel mask and keeps the vector mask. A `.psd`'s `vmsk`/`vsms` imports
  as a live vector mask and exports as path records; with a pixel mask too,
  export writes the vector's rendering as the first mask record and the pixel
  mask as the `real` one. An existing vector mask's path cannot be edited in
  place. The parity matrix has the details.
- **The W7-F tools have named limits.** Perspective Crop, Vertical Type, the two
  Type Masks, Mixer Brush, Artboard, Curvature Pen and Freeform Pen are palette
  tools now. Each is one undo step except Vertical Type, which is two like the
  Type tool (the click's empty layer, then the confirmed run). W8-C closed the
  follow-ups: Perspective Crop draws corner handles and a grid and rectifies
  every pixel layer (text, shape and smart-object layers and layer masks are
  cropped, not warped); vertical type's caret and click hit-test follow the
  column, though it is still upright glyphs (no rotated Latin, no vertical
  punctuation forms); the Mixer Brush previews while it is dragged; the Type
  Masks show the quick-mask red outside the glyphs while typing and combine by
  the options bar's New / Add / Subtract / Intersect; artboards clip their
  contents and File > Export > Artboards to Files writes one image each;
  File > New's Artboard box makes the new canvas one artboard ("Artboard 1"
  with an empty "Layer 1" inside), and the Properties panel lists the
  artboards and shows the active one's position, size and background. The
  parity matrix has the details.
- **Stylus pressure is verified with synthetic events only.** winit's `Touch`
  events (Windows `WM_POINTER` pens and fingers) now drive the same pointer
  route as the mouse, with the force as the stroke's pressure
  (`app-shell/src/pen_input.rs` through `Shell::set_pen_pressure`), and the
  OS's emulated mouse for that contact is dropped. Losing window focus
  mid-contact drops the contact and its pressure, since winit on Windows
  never reports a cancelled contact. Tests drive synthetic
  events; no physical pen has been tried. The options bar has Size from
  Pressure, Flow from Pressure and Opacity from Pressure (each dab's alpha
  times the pressure). winit reports a pen's zero pressure as "no force", so
  a contact id that has reported a force or hovered is treated as a pen and
  its forceless samples as zero pressure; an id never seen either way (a
  finger) paints at full pressure. This is keyed on the contact id, not a
  device type: Windows reuses contact ids and pens and fingers share one
  id pool, so a finger that gets an id a pen used earlier paints at zero
  pressure until that id ages out of the remembered pen ids. Contacts still
  down when focus is lost have their moves dropped, rather than read as a
  hovering pen, until their lift is reported or they touch down again; a
  pen whose lift was lost (winit on Windows reports no cancelled contact)
  therefore does not move the pointer or the brush ring while hovering
  until its next touch-down. Otherwise a hovering pen moves the pointer and
  the brush ring; the ring stays where the pen left range until the mouse
  moves. Pen tilt, rotation and the eraser end are not read, and the
  Brushes panel's presets do not carry the Opacity from Pressure switch.
- **Tab does not move keyboard focus.** Tab toggles the panels (Photopea's
  Hide/Show Panels) and is withheld from egui; controls are reached with the
  pointer. AccessKit is wired, but no screen-reader walk has been done on a
  real host.
- **Some 16-bit edits still compute at 8-bit precision.** Filters,
  adjustments, Fill, Clear, Free Transform (its resampling modes and a
  floated selection, including on an opened 16-bit PNG or TIFF whose tiles
  are still 8-bit), Edit ▸ Transform's flips and turns, every Image ▸
  Image Rotation (90°, 180°, Arbitrary, flips), Image Size, Canvas Size,
  Crop to Selection and Trim read and write a 16-bit layer at 16 bits. Since
  W10-H Brush, Pencil, Eraser and the other stroke tools blend and write
  16-bit dabs, but Clone Stamp and the Healing Brush read their *source*
  rounded to 8 bits, so cloned content lands as widened 8-bit codes. The bucket, gradient and other non-stroke tools, Stroke,
  Apply Mask, Defringe, Layer via Copy/Cut,
  Grayscale, Edit ▸ Fill ▸ Content-Aware (it synthesises the fill from an
  8-bit read) and the Filter dialog's preview still read a 16-bit tile
  rounded to 8 bits (the pixels they leave alone keep their 16-bit codes).
  Opening a 16-bit PNG or TIFF decodes it to 8-bit tiles. PSD export of a
  16-bit document writes its raster layers' 16-bit samples, but a shape's,
  smart object's or text layer's rendered preview and the merged image are
  8-bit values widened to 16 bits.
- **Pattern-filled glows and strokes do not cross PSD, and cannot be set in
  the dialog.** A Pattern Overlay (and a pattern-filled glow or stroke)
  renders, and is saved inside the `.rstudio`
  document with its pixels. PSD import reads the file's patterns (the
  `Patt`/`Pat2`/`Pat3` blocks) when they are 8-bit RGB or greyscale, raw or
  RLE; a pattern in any other image mode (indexed, CMYK, Lab ...), at any
  other depth (16- or 32-bit) or with ZIP compression is refused and noted in
  the import report. It maps a pattern overlay onto the Pattern Overlay
  effect, and (W9-B) a Pattern fill layer onto a live pattern fill layer with
  its scale, phase, angle and link; an overlay or fill naming a refused
  pattern, or one the file does not carry, is still listed as unmapped. PSD
  export (W9-M) writes a pattern overlay whose pattern has pixels as a
  `patternFill` effect, and a pattern fill layer as a `PtFl` layer, both
  with the pattern in the file's `Patt` block; a pattern-filled glow or
  stroke is still named as not written. The Layer
  Style dialog picks the overlay's pattern, but has no control yet that sets a
  glow's or stroke's fill to a pattern.
- **Channels** isolate a colour component (and paint, fill, filter or adjust
  it alone), a layer mask (selecting its row shows it alone in
  grayscale and aims painting at it) and a saved selection (W10-B: its eye
  opens it as a hidden scratch layer's mask, shown alone in grayscale and
  painted like any mask, and a second click stores the painted coverage
  back, in the close's own undo step, so undo takes the painting back;
  Close Alpha Channel is greyed while none is open); there is no
  per-channel histogram, and the open alpha channel's scratch layer shows in
  the Layers panel while it is open.
- **W10-B panels** — Layer Comps (new / apply / update / delete / previous /
  next; visibility, position and appearance, saved in the document), Tool
  Presets (the active tool with its touched options, kept in the
  preferences file), Glyphs (the active text layer's font, drawn in that
  font; a click inserts at the caret of a live typing session, or appends
  to the layer's text when nobody is typing), Notes (the Note tool, in the
  Eyedropper slot, pins one where you click, and New Note at the view's
  centre; numbered pins are drawn on the canvas while View > Extras is on;
  saved in the document, never exported), Character Styles and Paragraph
  Styles (redefining a style restyles every layer wearing it, one undo
  step). The six panels' labels, buttons and hints go through the strings
  catalogue. Gaps: three names the panels create are still English
  literals (a new comp's default name `Layer Comp {n}`, a new note's
  starting text `Note`, and the `Tool` fallback in a new tool preset's
  name); a note's text is edited in the Notes panel, not in a popup on
  the canvas, and a pin cannot be dragged; there is no View > Show > Notes
  of its own (View > Extras hides the pins).
- **Localisation** covers the view and dialog code only (and, since W10-B,
  the labels and hints of the Layer Comps, Tool Presets, Glyphs, Notes and
  Character / Paragraph Styles panels); menu labels, the other `src/panels` and `src/canvas` are
  English literals.
- **SVG export** (W10-F): File ▸ Export to `.svg` (an SVG row in the
  picker) and Export As's SVG rows at 100% write each solid shape layer
  as a `<path>` (fill and stroke colour as the composite draws them, fill
  opacity on the element so a translucent fill does not show through the
  stroke), single-style text as `<text>` (line positions
  approximate the shaper's), a group below 100% as one `<g opacity>`, a
  base layer with the layers clipped to it as one embedded PNG, and every
  other layer as its own embedded PNG, flattening to one image when a
  blend mode, adjustment layer, or a group with effects, a mask, a
  transform or an artboard cannot stack; an Export As SVG row at another scale is still the
  embedded-image SVG at that size. **PSD export** has been
  checked with an independent reader but not yet reopened in Photoshop or
  Photopea; a text layer is written with every style run (font with its
  weight in the name, size, fill), proportional leading as auto leading and
  the transform at Photoshop's anchor in its engine data (W9-C; a weight
  between the named ones is written as the nearest and the report says so)
  and re-imports with them, but that payload has
  not been opened in Photoshop either, so its raster fallback rides along.
  W9-M's shape (`vmsk` + `SoCo`/`GdFl`/`PtFl` + `vstk`, `vogk` for a
  rectangle) and smart-object (`SoLd` + `PlLd` + `lnk2`) layers and 16-bit
  files are read back by this build and by psd-tools 1.19 (kinds, path,
  stroke, embedded PNG, corners, 16-bit samples), not yet by Photoshop. A
  shape with a translucent fill, a stroke under a non-uniform transform, an
  arc in its path or a pattern fill under a transform, and a linked or
  smart-filtered smart object, still export as rendered pixels (named in the
  report). W11-B: all ten layer effects are written as editable `lfx2`
  descriptors: inner shadow (`IrSh`), inner glow (`IrGl`), bevel and emboss
  (`ebbl`), satin (`ChFX`) and gradient overlay (`GrFl`) join the drop
  shadow, stroke, colour overlay, outer glow and pattern overlay, with the
  shadow, glow and bevel contours, and PSD import maps all ten back through
  the one decoder the `.asl` import also uses (checked by re-reading the
  written file in this build, not yet in Photoshop). W13-B: a gradient- or
  pattern-filled stroke, a gradient-filled glow, a gradient overlay's offset
  (as `Ofst` percentages of the written layer box, or of the canvas when the
  ramp is not aligned with the layer) and the extra instances of a repeated
  effect (as `dropShadowMulti`, `innerShadowMulti`, `frameFXMulti`,
  `solidFillMulti` and `gradientFillMulti` lists inside `lfx2`) are written
  and read back; a pattern-filled glow (Photoshop's glows have none), a
  pattern with no pixels and an offset on a layer with an empty box are
  named in the report. The separate `lmfx` block Photoshop CC writes is
  still not read.
- **PSD document resources (W11-C).** Opening a `.psd` puts its guides
  (resource 1032) on the document, its saved paths (2000-2997) and work
  path (1025) in the Paths panel as path layers (no fill, no stroke; the
  work path arrives as a saved path called "Work Path"), and its named
  alpha channels (1006/1045) in the Channels panel as saved selections,
  and its user and layer slices (1050; Photoshop's auto-generated fill
  slices are skipped) in the document's slice set, which File > Export >
  Slices and the Slice tools' edits load into the slice store; Save as PSD writes them back the same
  way (path layers as saved-path resources, not as layer records; slices
  as version-6 user slices with name, URL and alt text) and writes a pixel
  mask's density and feather in the mask record. A no-fill, no-stroke
  shape layer that is hidden or carries a mask, effects, clipping, a blend
  mode or reduced opacity/fill is written as a layer record, not a path.
  A .psd holds 56 channels, so saved selections past that room (52 on an
  RGBA file) are named in the save notes and not written; the save itself
  goes ahead. Guide locks, the Paths panel's unsaved Work Path and a slice's target,
  message and cell text are not written. Checked by re-reading the written
  file in this build, not yet in Photoshop.
- **Print** (W13-M) opens the system Print dialog on Windows, owned by the
  app window, and sends the flattened image to the chosen printer (one page,
  centred at 72 ppi, scaled down to fit); with no usable printer it falls back
  to writing a PDF. **File > Print as PDF…** writes the print-ready PDF on
  every platform. macOS and Linux Print also writes that PDF for the system
  viewer to print: no print dialog is wired there.
- **Content-aware:** Fill, Content-Aware Scale and the Spot Healing Brush's
  Content-Aware type run on a job worker (the heal lands as one undo step
  when it finishes; a heal released while another content-aware job runs
  waits in a queue and heals the pixels as they are when its turn comes; Esc,
  or the window losing focus, drops every running or queued heal; a heal is
  dropped if its document is no longer the active one when it finishes; a
  Fill or Scale is refused while any content-aware job runs); there is no
  percentage progress, only a running timer on the status line; the fill refuses a context window over 2 M pixels, and
  Content-Aware Scale has no protect-skin or protection-channel option, and
  its box previews as a plain scale until Enter seam-carves it.
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
cargo test  --workspace                 # ~5,600 #[test] functions
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
