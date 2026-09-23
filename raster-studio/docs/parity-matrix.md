# Feature Parity Matrix

Target: feature parity with Photopea for real editing work.

**This file states the truth.** A feature is ✅ only when it is implemented,
tested, and reachable from the UI. Anything else is 🔶 (partial, with the gap
named) or ⬜ (not started). Nothing is marked done on the strength of a type
existing — that is exactly the failure this project was rebuilt to escape.

Status: ✅ done · 🔶 partial (gap named) · ⬜ not started · 🚫 decided not to
ship (reason given) · ❌ deliberately limited (reason given)

**Re-checked at `0e4a6fd` (2026-09-23)**, after six fix waves (see the root
`CHANGELOG.md`), against a read-only audit of `b477a09` and the wave-5 diff;
every row whose notes changed cites the code or test it rests on.

> **Reachability is part of the bar.** A final audit found rows marked done whose
> feature the application had no route to. Those are corrected below and the
> engine/UI split is stated explicitly: 🔶 now covers "the library works and is
> tested, but you cannot get to it from the app". See README "Status".


## Tiers

- **Tier A — Core.** Without all of these the app is not an image editor.
- **Tier B — Pro.** What makes it competitive with Photopea.
- **Tier C — Deferred.** Explicitly out of scope for this release, with reasons.

---

## Tier A — Core

### Document & canvas
| Capability | Status | Notes |
| --- | --- | --- |
| Open PNG / JPEG / WebP / TIFF / GIF / BMP / ICO / TGA | ✅ | ICC preserved; File ▸ Open decodes on a worker thread, while drag-and-drop, recent files and the command-line argument open on the UI thread; on every route a 16-bit source opens as RGBA8 tiles in a document that records 16 bits (`OpenDocument::record_source_depth`; test `file_open_on_the_worker_records_a_sixteen_bit_source`) — Image ▸ Mode ▸ 16 Bits widens the tiles |
| New document with presets | ✅ | screen, print and social presets; colour mode and background |
| Pan / zoom / fit / 100% / rotate view / flip view | ✅ | zoom-to-cursor keeps the point under the pointer fixed |
| Transparency checkerboard | ✅ | fixed pixel size, drawn inside the image for alpha pixels |
| Rulers, guides, smart guides, grid, snapping | ✅ | guides are saved with the document and changed by an undoable `Command::SetGuides`; View ▸ New Guide / Clear / Lock Guides; rulers honour the Units preference; pixel grid at 800% and above |
| Multi-document tabs | ✅ | the document is fitted, centred and drawn inside the canvas area, not under the panels (W5; `the_document_is_fitted_centred_and_confined_to_the_canvas_area`) |

### Layers
| Capability | Status | Notes |
| --- | --- | --- |
| Raster layers with real pixels | ✅ | content-addressed tiles |
| Layer tree, groups, reorder, rename, lock | ✅ | a drag that would nest a group in its own child is refused |
| Visibility / opacity / fill | ✅ | |
| Blend modes (27) | ✅ | includes Hue/Saturation/Color/Luminosity via W3C SetLum/SetSat |
| Layer masks | ✅ | density and feather honoured by the compositor |
| Clipping masks | ✅ | Porter-Duff atop against the base layer's alpha |
| Adjustment layers (non-destructive) | ✅ | apply to the backdrop beneath them; themselves clippable |
| Layer styles / effects | ✅ | drop shadow, inner/outer glow, satin, colour and gradient overlays — rendered by the compositor (`effects::render`) |
| Merge / flatten / duplicate / rasterize | ✅ | |

### Editing
| Capability | Status | Notes |
| --- | --- | --- |
| Undo / redo with a history panel | ✅ | clickable stack with snapshots; a stroke is one step |
| Command journal + crash recovery | ✅ | anchored to a save marker, so replay cannot duplicate work; commands made while a save runs go to a `.journal-hold` side file and are absorbed exactly once afterwards, or on the next open after a crash (W2, W5) |
| Cut / copy / paste / clear | ✅ | incl. Copy Merged, Paste Special (Paste in Place, Paste Into, Paste Outside), Layer Via Cut/Copy |
| Edit ▸ Purge (Clipboard / Histories / All) | ✅ | W4-H; Histories and All ask first — the second choice within 10 s confirms (a status-line confirmation, not a modal: the dialog seam has no yes/no question for it yet) |
| Free transform (scale/rotate/skew/distort/perspective/warp) | ✅ | interactive gestures apply real, undoable commands; singular matrices are refused rather than writing NaN; with a partial pixel selection, Free Transform and Move float only the selected pixels as one undo step (W5, `tools::transform::float_selection`; `free_transform_with_a_selection_scales_only_the_selected_pixels`, `move_with_a_selection_moves_only_the_selected_pixels`) |
| Crop, trim, image size, canvas size, rotate canvas | ✅ | crop (ratio presets, W×H×resolution, straighten, Delete Cropped Pixels, overlay — W4), trim, the Image Size and Canvas Size dialogs and the rotations each apply as one undo step; a crop's rotation and scale ride the layer transforms rather than being baked |

### Selection
| Capability | Status | Notes |
| --- | --- | --- |
| Marquee (rect / ellipse / row / column) | ✅ | anti-aliased |
| Lasso (free / polygonal / magnetic) | ✅ | |
| Magic wand / quick select / colour range | ✅ | tolerance, contiguous flag, anti-aliasing |
| Select Subject | 🚫 | decided Tier C: no segmentation model ships, and the menu carries no item for it (P2.12) |
| Modify: feather, expand, contract, smooth, border | ✅ | true morphology on fractional coverage |
| Invert, grow, similar, transform selection | ✅ | |
| Quick mask, save/load selection | ✅ | quick mask composes (`Q` / Select ▸ Edit in Quick Mask Mode: edits land in a scratch mask, leaving converts the painted coverage into the selection); selection itself (outline, marching ants, save/load) is reachable |

### Tools
| Capability | Status | Notes |
| --- | --- | --- |
| Brush that actually paints | ✅ | one undoable command per stroke; overlapping dabs do not darken |
| Eraser, pencil, paint bucket, gradient | ✅ | |
| Clone stamp, healing, spot healing, patch, red-eye | ✅ | |
| Content-aware fill, Spot Healing Content-Aware type, Content-Aware Scale | 🔶 | W7-I, no model: Edit ▸ Fill ▸ Contents: Content-Aware synthesises the selection from the rest of the layer by multi-scale PatchMatch (`filters::patchmatch`, 7-px patches, propagation + random search, weighted EM voting, fixed seed; bounded to the selection's bounding box plus a context margin) on a job worker (`app_shell::menu_bridge::content_aware_job`; the result lands as one undo step on the next frame, and is discarded if the document, layer, selection or pixels changed while it ran); the Spot Healing Brush's Type option (Proximity Match / Content-Aware) runs the same synthesis over the brushed area on release; Edit ▸ Content-Aware Scale seam-carves the active layer (gradient-magnitude energy) on the same worker to 80/90/110/125% of the canvas width or height, centred. Tests from the dialog to the pixels: `shell::w7i_tests::*`. Gaps: the spot heal's synthesis still runs on the UI thread at release; the fill refuses a context window over 2 M pixels; Content-Aware Scale has fixed steps, no interactive handles and no protect-skin or protection channel; the Content-Aware spot heal's live preview shows the Proximity Match answer and only the release synthesises |
| Dodge / burn / sponge, blur / sharpen / smudge | ✅ | |
| Eyedropper, move, hand, zoom, rotate view | ✅ | |
| Ruler (Straighten Layer), Color Sampler, History Brush | ✅ | W4-G |
| Pencil Auto Erase, Pattern Stamp, Colour Replacement, Background / Magic Eraser, Pattern Fill, Slice, Refine Boundary | ✅ | every palette tool is driven through the real pointer route by `tests/integration/tests/tool_routes.rs`; `tools::ToolId::ALL` has 64 tools |
| Perspective Crop, Vertical Type, Horizontal / Vertical Type Mask, Mixer Brush, Artboard, Curvature Pen, Freeform Pen | 🔶 | W7-F: each is a registry tool in its Photopea slot (Crop; Type; Brush; Move; Pen) with a drawn icon and an options schema, driven through the real pointer route by `tool_routes.rs`, one undo step each except Vertical Type, which is two history entries like the Type tool (the click's empty layer, then the confirmed run) (`perspective_crop_rectifies_a_dragged_and_adjusted_quad_as_one_step`, `vertical_type_lays_the_typed_run_down_a_column`, `the_type_masks_turn_the_typed_glyphs_into_one_undoable_selection`, `mixer_brush_carries_the_red_it_picked_up_into_the_blue`, `artboard_drag_makes_one_artboard_group_with_a_white_plate`, `curvature_pen_clicks_then_enter_make_one_smooth_shape_layer`, `freeform_pen_drag_is_fitted_into_one_short_curved_shape_layer`). **Perspective Crop** drags a quad, drags its corners, and Enter resamples the active layer through a homography into the upright rect (size from the quad's edges or the W / H options) and crops the canvas to it, one transaction; only the active raster layer is rectified (the others are cropped, not warped) and a text/shape layer is refused. **Vertical Type** stores `Paragraph::vertical` (append-only serde) and `text_engine` lays it out by re-positioning the horizontally shaped glyphs upright, one em cell per cluster, one column per paragraph, columns right to left (cosmic-text has no vertical mode): no rotated Latin runs, no vertical punctuation forms, no decorations, a box does not wrap it, and the caret and hit-test geometry stay horizontal; `compositor::text::hash_layer` hashes the flag, so the run cache and tile key never serve a vertical run the horizontal pixels of the same string and style. **Type Masks** type into a temporary layer and their confirm turns its glyph coverage into the selection and takes the layer away (one history entry). **Mixer Brush** carries a reservoir (Wet, Load, Mix, Flow, Load / Clean after each stroke) simulated dab by dab at release; no live preview while the button is down. **Artboard** drags a group whose raster plate (`RasterLayer::artboard`, append-only) names the rect and background and is filled with it; contents are not clipped to the artboard and there is no File ▸ Export Artboards. **Curvature Pen** clicks points a Catmull-Rom curve passes through; **Freeform Pen** fits a freehand drag (Ramer-Douglas-Peucker at Curve Fit) into smooth anchors; both publish through the Pen's mode, paint and combine. |
| Live stroke preview, brush-size ring, per-tool cursors, right-click canvas menu | ✅ | W4-B/C; the right button opens the menu and never reaches a tool (`a_right_click_on_the_canvas_opens_its_menu_and_a_row_performs`) |
| Tablet pressure | 🔶 | winit `Touch` events (pen force included) drive the mouse's pointer route with the force as the stroke's pressure (`pen_input.rs` via `Shell::set_pen_pressure`); a finger with no force paints at full pressure; the OS's emulated mouse for the same contact is dropped; losing window focus mid-contact drops the contact and resets the pressure to full (winit on Windows never reports a cancelled contact). Size from Pressure and Flow from Pressure toggles exist, no Opacity from Pressure; no tilt/rotation/hover/eraser end. Verified with synthetic events only, not yet with a physical pen |

### Adjustments
| Capability | Status | Notes |
| --- | --- | --- |
| Brightness/Contrast, Levels, Curves, Exposure | ✅ | Curves is a Fritsch-Carlson monotone spline, edited as a draggable curve over the histogram with a channel choice (W5, `ui::dialogs::adjustment_dialog::curve_widget`); Levels shows its histogram; Ctrl+L / Ctrl+M open them |
| Vibrance, Hue/Saturation, Colour Balance | ✅ | |
| B&W, Photo Filter, Channel Mixer, Invert | ✅ | Invert opens no dialog: Image ▸ Adjustments ▸ Invert or Ctrl+I applies it at once (W5-E, `AdjustmentId::has_dialog`, ui/src/menu.rs; test `ctrl_i_inverts_the_layer_at_once_without_a_dialog`) |
| Posterize, Threshold, Gradient Map, Selective Colour | ✅ | |
| Auto tone / contrast / colour | ✅ | |
| Desaturate, Equalize | ✅ | W4-E: Image ▸ Adjustments, no dialog, one undo step; Desaturate is Shift+Ctrl+U and keeps linear luminance; Equalize reads the histogram of the selected pixels. Destructive-only, as in Photopea |
| Shadows/Highlights, Replace Color | ✅ | W4-E: dialogs with live preview; Shadows/Highlights reads a blurred luminance over its radius; Replace Color has a selection preview and samples from a click. Destructive-only, as in Photopea |
| HDR Toning | ✅ | W7-G: Image ▸ Adjustments ▸ HDR Toning, dialog with live preview, one undo step, destructive-only as in Photopea. Local adaptation on the active layer: log2 luminance split by two self-guided filters into base, local contrast (Edge Glow radius, gain `1 + strength`) and fine detail (gain `1 + detail`); gamma compresses the base about middle grey, exposure shifts it in stops; vibrance/saturation after (`adjustments::hdr`). Not Photoshop's: no 32-bit merge, no shadow/highlight sliders or toning curve, and no Method choice (Local Adaptation only). Tests `zero_strength_and_detail_is_the_identity`, `strength_increases_local_contrast`, `hdr_toning_confirmed_from_its_dialog_raises_local_contrast_as_one_step` |
| Match Color | ✅ | W7-G: Image ▸ Adjustments ▸ Match Color, dialog with live preview and a Source list of every open document's merged image and every other pixel layer; Luminance, Color Intensity, Fade, Neutralize; one undo step, destructive-only. Reinhard mean/deviation transfer per CIELAB channel (`adjustments::match_color`); the target is measured at full size from the selected pixels when applied, the source from a copy downsampled to 1024 px. Tests `matching_moves_the_target_means_to_the_source`, `full_fade_is_the_identity`, `match_color_confirmed_from_its_dialog_moves_the_layer_means_to_the_source_layer` |
| Color Lookup (3D LUT) | ✅ | W4-E: a `.cube` file (file picker) or one of five built-in looks (Invert, Warm, Cool, Sepia, High Contrast). Image ▸ Adjustments bakes it as one undo step; Layer ▸ New Adjustment Layer ▸ Color Lookup creates the layer at the identity table, and Layer ▸ Edit Adjustment… or the Properties panel's "Open editor…" reopens the same dialog on that layer, whose OK rewrites the layer's table as one undo step |

### File
| Capability | Status | Notes |
| --- | --- | --- |
| Save / open the native `.rstudio` project | ✅ | integrity-sealed, version-gated, crash-safe swap; tiles deflate-compressed and reused from the package being replaced; saved on a worker thread with the journal held; File ▸ Open accepts a package through its `manifest.json` (W5) |
| Pixel data persisted | ✅ | reopening composites to byte-identical output |
| Export PNG / JPEG / WebP / TIFF / GIF / BMP / ICO / SVG with presets | ✅ | correct un-premultiply and linear→sRGB on the way out; ICO carries 16/32/48/256 px entries; SVG wraps the raster composite as an embedded PNG (vector shape layers are not written as paths yet) |
| WebP lossy with a quality slider | ❌ | W4-H: kept lossless on purpose. The WebP encoder already in the tree (`image-webp` 0.2, behind `image` 0.25) writes lossless VP8L only. Pure-Rust lossy encoders exist on crates.io and were evaluated on 2026-09-23 (encode 256×128 and 257×131 RGBA test images at quality 50 and 90, decode with `image-webp` 0.2.4, measure RGB PSNR; `cargo audit` on a lockfile holding the three permissively licensed crates found no advisories): `zenwebp` 0.4 is AGPL-3.0-only (or a commercial licence), so it is out on licence; `webp-rust` 0.3.1 (MIT) measured 15–17 dB PSNR at every quality, and its quality-90 stream for the 257×131 image was rejected by `image-webp` with `BitStreamError`; `vaam-image-webp` 0.1.0 (MIT/Apache-2.0) reached 30–40 dB on the 256×128 images but 22–23 dB on the 257×131 ones, and was first published on 2026-09-22; `tiny-webp` 0.1.0 (MIT/Apache-2.0, no dependencies with `default-features = false`) measured 34–44 dB on all four images, but it is a single 0.1.0 release first published on 2026-09-04 with 23 downloads. `tiny-webp` is the candidate to adopt once it has a track record; adopting it means a workspace manifest change and a quality slider in Export As. Until then the Export As row is labelled “WebP (lossless)” and has no quality control |
| File ▸ Export ▸ Slices | ✅ | W4-H: one file per committed Slice-tool region, `<document>_01.<ext>`…, in the last confirmed Export As format and settings (PNG before any) |
| Drag-and-drop open, recent files | ✅ | |

### Application
| Capability | Status | Notes |
| --- | --- | --- |
| Menu bar wired to real commands | ✅ | nine menus; every enabled item routes to real code (`every_enabled_menu_item_really_does_something`, `no_enabled_menu_item_resolves_to_a_no_op`); View ▸ Proof Colors / Gamut Warning and Image ▸ Mode ▸ Lab / CMYK / Indexed are enabled since W7-D |
| Keyboard shortcuts | ✅ | full customisable keymap with conflict detection; Photoshop's Ctrl+L / M / U / B / I, Backspace, Alt+Backspace and Ctrl+Backspace are bound (W5) |
| Panels | ✅ | 15 (`ui::dock::PanelId::ALL`): Layers, History, Adjustments, Properties, Color, Swatches, Brushes, Character, Paragraph, Navigator, Info, Channels, Paths, Actions, Histogram — in two dock columns (W2) |
| Tool options bar | ✅ | generated from each tool's options schema |
| Photopea visual design | 🔶 | one token system, light and dark, WCAG AA asserted by test; Photopea's chrome order and two dock columns landed in W2; the fit and finish is still converging on Photopea’s density and neutral greys |
| Preferences | ✅ | persisted, including the keymap editor; keymap edits apply live; Units drive rulers and readouts; the scratch directory is an editable text field (no folder picker) |
| Accessibility (screen readers / AccessKit) | 🔶 | egui's `accesskit` feature is on: the adapter is initialised at window build, its action requests route through a typed user event, and egui publishes a labelled node per widget. **No keyboard focus navigation:** Tab is withheld from egui and toggles the panels (`app-shell/src/shell.rs` `withhold_from_egui`; `tab_is_never_handed_to_egui_unless_egui_is_recording_it`). The on-device screen-reader walk needs a host with assistive tooling (C14). |

---

## Tier B — Pro

| Capability | Status | Notes |
| --- | --- | --- |
| Text layers with real shaping and fonts | ✅ | bidi, ligatures, kerning, contextual forms via cosmic-text; a Type click inside a text layer places the caret; switching tools or Escape confirms the text (W5) |
| Vector paths, pen tool, shape layers | ✅ | the Pen drags out smooth anchors and Alt breaks a handle; Add / Delete / Convert Anchor, Path / Direct Selection; shapes with fill / stroke, width and corner radius; Custom Shape library |
| Filters: blur family | ✅ | separable Gaussian; every filter in the Filter menu applies against the live document |
| Filters: sharpen, noise, distort, stylize, pixelate, render | ✅ | the whole library is reachable from the Filter menu |
| Filters: Photopea one-click and parity rows | ✅ | Blur ▸ Average, Blur, Blur More, Smart Blur; Sharpen ▸ Sharpen, Sharpen More, Sharpen Edges; Distort ▸ Displace (map: the layer itself or a seeded cloud field — no external PSD map picker); Pixelate ▸ Facet, Fragment, Mezzotint; Stylize ▸ Extrude, Tiles, Trace Contour. Each opens the generated FilterDialog with live preview (`the_photopea_parity_filters_preview_through_their_dialogs`) |
| Filter ▸ Liquify… / Edit ▸ Puppet Warp | 🔶 | W7-H. **Liquify** is a modal dialog with its own preview canvas (the layer copied at most 512 texels on the long side): tools Forward Warp, Reconstruct, Twirl Clockwise, Pucker, Bloat, Push Left, Freeze Mask and Thaw Mask; brush Size (document pixels), Pressure and Density; Reset. The engine is `filters::liquify::LiquifyField`, a backward displacement field (`f32` per cell, at most 512 cells on the long side) upsampled and applied with bilinear resampling; strokes compose, Reconstruct scales the field toward zero, the freeze mask holds content still. OK applies the field to the active pixel layer at full resolution (folded by the selection) as one undo step; Cancel writes nothing. **Puppet Warp** is a modal dialog over the layer's ink: `filters::puppet::PuppetMesh` triangulates every grid cell holding ink (alpha > 0, dilated one cell; density Fewer / Normal / More); a click adds a pin at the nearest vertex, dragging moves it, a secondary click removes it; Rigid mode is as-rigid-as-possible (local/global iteration, uniform weights), Linear is an inverse-distance blend; Enter commits one undo step. Tests: `liquify::tests::*`, `puppet::tests::*` (filters), `dialogs::liquify::tests::*`, `dialogs::puppet_warp::tests::*` (ui, including pointer drags on the drawn canvas), `menu_bridge::tests::w7h_liquify_opens_from_the_menu_and_lands_as_one_undo_step`, `menu_bridge::tests::w7h_puppet_warp_opens_from_the_menu_and_lands_as_one_undo_step`. Gaps: Liquify has no Twirl Counterclockwise, Smooth or Turbulence tool, no tablet pressure, no mesh save/load and no backdrop or show-mesh view options; Puppet Warp runs in a dialog rather than on the canvas, pins snap to mesh vertices, there is no pin depth, pin rotation or Expansion, and the mesh follows the ink on a grid rather than a traced outline; the tool, mode and density names are English literals from the engine, not catalogue keys |
| Image ▸ Mode ▸ Lab / CMYK / Indexed | 🔶 | W7-D. All five modes convert, each as one undo step (`editor_core::color_mode::convert_color_mode`: the mode flag plus one `PaintTiles` per layer). Tiles stay RGBA (Photopea's approach). **Lab** changes the flag only (8-bit sRGB round-trips). **CMYK** clamps every colour the documented naive ink model cannot print to its round trip (`color::cmyk`: sRGB approximations of SWOP cyan/magenta/yellow as subtractive filters, maximum GCR lowered by bisection where the inks cannot print the remainder, least-squares separation; the primaries' values are pinned in its docs and by `the_primaries_round_trip_to_the_documented_values`) — **not** an ICC press profile. **Indexed** opens Image ▸ Mode ▸ Indexed Color… (`ui::dialogs::IndexedColorDialog`: palette Exact / Web (216) / Uniform (never more entries than the count: a grey ramp below 8, otherwise an evenly spaced RGB box that fits, e.g. 16 -> 2x4x2, 256 -> 6x7x6 = 252) / Adaptive median cut, 2-256 colours, dither None / Floyd-Steinberg diffusion, run per 256-px tile) and quantises every layer onto one document palette (`color::quantize`). The raster exporter can write CMYK JPEG (baseline, Adobe APP14, inverted samples), CMYK TIFF (uncompressed, separated) and palette PNG-8 (`raster::export::ink`, chosen by `ExportPreset::ink`), and both app routes select it from the document's mode: File ▸ Export (`jobs::run_file_export` via `raster::export::encode_rgba8_in_ink`) and Export As (`ExportPreset::for_color_mode`, which also drops a 16-bit row to 8 bits when it writes ink); GIF export of an indexed document writes its colours exactly. Export As shows a note (`ExportAsDialog::ink_note`) when the selected format writes the document as RGB instead — always for Lab. Info adds a `Lab` or `CMYK` row for a document in that mode (`ui::panels::navigator::mode_readout`) and the Color panel has a CMYK notation (C/M/Y/K percentages through the same ink model). View ▸ Proof Colors / Gamut Warning: `app_shell::presenter::ProofView` shows the `color::cmyk` round trip or paints the theme's Warning token where it moves a pixel by more than `GAMUT_THRESHOLD`, fed from the View flags every frame by `CanvasPresenter::read_view_settings` (the shell's one per-frame call). Tests: `color::cmyk::tests::*`, `color::quantize::tests::*`, `editor_core::color_mode::tests::*`, `raster::export::ink::tests::*`, `menu_bridge::color_mode::tests::*` (menu route, one undo step, the dialog, GIF round trip, File ▸ Export of a CMYK document writing a four-component Adobe JPEG, Export As writing CMYK JPEG/TIFF and PNG-8, the Lab note), `chrome::tests::proof_colors_and_gamut_warning_tick_from_the_menu_and_change_the_canvas_texture`, `ui::view::docks::tests::the_color_panel_reads_cmyk_and_info_reads_the_documents_mode`, `ui::dialogs::indexed_color::tests::*`, `ui::dialogs::export_as::tests::the_dialog_says_how_a_non_rgb_document_is_written_and_draws_it`. Gaps: no encoder writes Lab (a Lab document exports RGB, and Export As says so; File ▸ Export, which has no dialog, does not); Levels/Curves offer no L/a/b channels; the Color panel does not switch notation when the mode changes (pick CMYK or Lab yourself); Indexed does not flatten, so blending between semi-transparent layers can composite colours outside the palette; the proof is an 8-bit display transform on the naive ink model, not an ICC soft proof |
| Filter ▸ Convert for Smart Filters / smart filters | ✅ | W7-E. Convert for Smart Filters converts the active layer to a smart object (greyed with `SMART_FILTERS_ALREADY` over one); a filter from the Filter menu's filter rows confirmed over a smart object is appended to `SmartObjectLayer::filters` (key, parameters, eye, opacity, blend mode) as one `SetLayerKind` undo step and the source tiles are never rewritten. The compositor renders the source over its whole extent and runs the stack bottom-up through the Filter dialogs' own table (`compositor::smart`, cached by source-tile + stack hash). The Layers panel draws a "Smart Filters" block under the layer — eye, delete, double-click re-opens the dialog at the stored parameters over the object's own pixels and the confirm replaces that entry — also when another layer was active (the double-click selects the object; `arm_smart_filter_edit` / `filter_dialog_source` / `run_filter_invocation` work from the requested layer, not the pre-selection active one). The stack saves in `.rstudio`; a PSD export writes the filtered appearance as the layer's pixels (not editable smart-filter data). Not yet: the Filter Gallery, Last Filter, Liquify and Puppet Warp stay greyed over a smart object (they need editable pixels; none of them joins the stack); filter masks, per-filter Blending Options UI (opacity/blend are stored and rendered, but no dialog edits them), dragging to reorder, mip levels > 0 filter at full-resolution parameters. Tests: `smart::composite_tests::*`, `menu_bridge::tests::smart_filters::*` (incl. `double_clicking_a_non_active_smart_objects_filter_reopens_it_at_its_params`, a real Layers-panel double-click through `Chrome::ui`), `docks::tests::a_smart_objects_filters_are_rows_under_it_with_eye_delete_and_re_edit` |
| Smart objects | ✅ | placed raster sources with editable transforms, embedded + linked origins, replace-contents refresh, full-extent storage; verified through real routes (cards 044–050, interchange tests) |
| PSD import | ✅ | groups, masks (with density/feather), blend modes, all four channel encodings, editable-text subset, the four mapped effects, ICC retention, full layer extents; a per-layer fidelity report (`PsdNotes`) names exactly what did not map — see `docs/PSD-THUMBNAIL-SUPPORT.md` |
| PSD export | 🔶 | layered export verified by independent re-read (structure, masks, effects as editable lfx2 descriptors, appearance-preserving text/shape/smart-object fallback pixels, merged preview matching the composite at ≤1/255); NOT yet verified in Photoshop/Photopea (card 081's manual step is pending) and editable text export (TySh) is blocked on an independently verified engine-data payload — see `docs/PSD-THUMBNAIL-SUPPORT.md` |
| Channels panel | 🔶 | isolate, then paint, erase, fill, filter or bake an adjustment into one RGB component (`mask_paint_to_channel`); saved-selection rows load; thumbnails. Gap: alpha / mask coverage cannot be isolated; no per-channel histogram |
| Paths panel | ✅ | the Pen's uncommitted path is the work path (`the_pens_uncommitted_path_is_the_paths_panels_work_path`); save as a named path; Fill, Stroke, Load as Selection, Make Work Path from Selection, New, Delete (W4) |
| Colour management | ✅ | sRGB and Display P3 are real; an embedded ICC profile is carried, composites through its profile and re-tags on export — `a_tagged_image_composites_through_its_profile_and_retags_on_export` |
| 16-bit per channel | 🔶 | a user can work at 16 bits: New Document accepts 16 bits (RGBA16 base tiles), Image > Mode > 8/16 Bits/Channel converts every raster tile in one undoable step (8 -> 16 lossless, 16 -> 8 rounded), a tool stroke on a 16-bit layer lands as RGBA16 tiles keeping the untouched pixels' 16-bit codes, the compositor composites RGBA16 tiles in `f32`, a 16-bit document exports 16 bits to PNG/TIFF (`mode_sixteen_then_a_brush_stroke_then_a_png_export_round_trips_at_sixteen_bits`), and it saves and reopens as `.rstudio` with the same RGBA16 tiles (W5, `a_16_bit_document_saves_and_reopens_with_the_same_pixels`); since W7-C filters, adjustments, Fill, Clear, Free Transform (resampling modes and a floated selection), Edit > Transform's flips and turns, every Image > Image Rotation (90, 180, Arbitrary, flips), Image Size, Crop to Selection and Trim read and write a 16-bit layer at 16 bits (`brightness_on_a_sixteen_bit_document_is_not_rounded_through_eight_bits`, `image_size_half_then_double_on_a_sixteen_bit_ramp_keeps_every_code`, `free_transform_distort_on_a_sixteen_bit_layer_resamples_at_sixteen_bits`, `an_opened_sixteen_bit_png_free_transformed_by_a_sub_pixel_stays_sixteen_bit`, `image_rotation_ninety_on_a_sixteen_bit_document_moves_every_code`), and Canvas Size keeps every code; the painting tools, Stroke, Apply Mask, Defringe, Layer via Copy/Cut, Grayscale and Content-Aware Fill still compute at 8-bit precision (P2.5b, see the gaps list) |
| Actions / recorded command replay | 🔶 | the Actions panel records, stops and replays (`recording_three_edits_replays_onto_a_second_document`); named actions show their steps and save to / load from `actions.json`, size-capped and written atomically (W5). Gap: no File ▸ Automate ▸ Batch over a folder |
| Batch export | 🔶 | Export As runs several presets in one run (`raster::export::export_batch_to_dir`); Export Layers and Export Slices write one file per layer / slice. Gap: no batch over several documents or files |
| Brush / gradient / layer-style editors | 🔶 | the Brush editor, the Gradient editor (stops paint) and the Layer Style dialog (ten effects plus a Blending Options page) are hosted dialogs; all ten effects render in the compositor (`compositor::effects::render`). W7-B: a pattern fill carries its own pixels (`layer_model::PatternTile`, content-hashed into the tile cache key), so Pattern Overlay and pattern-filled glows/strokes draw (blend mode, opacity, scale, angle, offset, link with layer), and the document saves and reopens with the pattern; the dialog's Pattern Overlay page picks one of the Define Pattern presets and shows a swatch (`compositor/src/pattern_tests.rs`, `app-shell/src/doc_pattern_tests.rs`). Gaps: PSD import/export carry no pattern effects (reported as unmapped); the dialog has no control that sets a glow or stroke fill to a pattern (such a fill renders when a document or style carries one). The Layer Style dialog's own preview is a labelled approximate schematic; the canvas shows the real composite |
| Autosave | ✅ | |
| Localization | 🔶 | Scope, stated exactly (P3.12/P6.6): the string catalogue (`crates/ui/src/strings.rs`) and its ~470 `tr()` call sites cover `src/view` and `src/dialogs`, enforced by the `no_localized_literals` gate. NOT localized: `src/menu.rs` (every menu label — a large user-facing surface — is still an English literal), `src/panels` and `src/canvas` (0 `tr()` calls). Two whole-file exemptions (`filter_dialog.rs`, `new_document.rs`) and three named literals in `preferences.rs` remain (`ui/tests/no_localized_literals.rs`); the file exemptions clear with the `tools::OptionSpec`/`DocumentPreset` label-key refactor the gate's own comment names (the gradient editor's `name_key` is the pattern). No claim of translation support beyond the catalogue's locale keying is made. |

---

## Tier C — Deferred (documented, not implied)

| Capability | Why deferred |
| --- | --- |
| Full CMYK / prepress / spot colours | Needs a real ICC engine and a print workflow; large and orthogonal to core editing. (Image ▸ Mode ▸ CMYK exists since W7-D on a documented naive ink model — see its Tier B row — but there is no press profile, no ink channels and no spot colours.) |
| PDF and AI import | A PDF interpreter is a project in itself. |
| Sketch / XD / Figma import | Proprietary formats with little overlap with raster editing. |
| Camera RAW | Per-sensor demosaic and profiles; belongs behind a finished colour pipeline. |
| Vanishing Point | Perspective-plane tooling (grid planes drawn on the canvas, painting and cloning in plane space). Liquify and Puppet Warp are no longer deferred: see the W7-H row under Tier B. |
| Select Subject / Object Selection | Requires a segmentation model we deliberately do not bundle. (Content-aware fill needs no model — it is PatchMatch, and it exists; see Tools.) |
| Filter ▸ Render ▸ Lighting Effects | A per-pixel light rig (spot/omni/infinite lights, a bump channel, material gloss) with an interactive on-canvas light editor; the parameter-schema dialog cannot express the light handles, and a slider-only version would not be Lighting Effects. |
| Licensing and auto-update crates | Dropped from the workspace (P3.2); neither crate exists: both were complete and tested with zero dependents — entitlement checks and update feeds are release-engineering for a shipped product, not this build. |
| Video and animation timeline | Out of scope for a raster editor v1. |
| Collaboration, cloud, mobile | Explicit non-goals: this is a local-first desktop app. |
| Perfect PSD round-tripping | We target correct reopen in Photoshop and Photopea, not byte fidelity. |

---

## Known gaps in what is marked done

Kept here rather than buried, because a ✅ with a footnote is still a claim:

- **16-bit editing is reachable; not every pixel path is deep yet (P2.5b, partly done).**
  *Done (W3-H):* the compositor reads an RGBA16 tile at its own depth and
  composites it in `f32` (`fill_layer`) -
  `a_sixteen_bit_ramp_composites_at_sixteen_bit_precision`,
  `a_sixteen_bit_ramp_composited_then_exported_equals_its_source`.
  *Done (W4-F):* users reach it. New Document accepts 16 bits and builds the
  base layer from RGBA16 tiles (`a_sixteen_bit_new_document_is_created_at_sixteen_bits`);
  Image > Mode > 8/16 Bits/Channel is enabled (the current depth is the
  checked, greyed row) and runs one undoable Transaction
  (`Command::SetMetaBitDepth` plus a per-layer tile rewrite; undo is
  byte-exact - `converting_to_eight_bits_then_undoing_restores_the_sixteen_bit_tiles_byte_exact`);
  the tools read a 16-bit tile rounded to 8 bits and their output is widened
  back at the apply boundary, keeping the exact 16-bit code of every pixel
  the tool did not change; export follows the document's depth, not only the
  source's (`mode_sixteen_then_a_brush_stroke_then_a_png_export_round_trips_at_sixteen_bits`).
  The app-shell's whole-layer reader (`pixels::read_layer`, behind Edit >
  Transform, filters, adjustments baked to pixels, fill, stroke, clear,
  layer via copy/cut, apply mask, defringe) and the other tile readers
  (Image Size, Canvas Size's background fill, Reveal All's bounds, Image >
  Mode > Grayscale, channel-limited painting) read an RGBA16 tile rounded to
  8 bits, and their RGBA8 output is widened back to RGBA16 at the apply
  boundary (`mode_sixteen_then_flip_rotate_filter_and_adjustment_match_the_eight_bit_document`,
  `an_opened_sixteen_bit_png_flipped_twice_is_the_identity`,
  `image_size_on_a_sixteen_bit_document_matches_the_eight_bit_document`,
  `canvas_size_fill_on_a_sixteen_bit_document_keeps_the_old_pixels`,
  `grayscale_on_a_sixteen_bit_document_converts_its_tiles`). Image > Mode >
  8 Bits/Channel dithers (Photoshop's default "Use Dither";
  `mode_eight_from_the_menu_dithers_between_codes_and_keeps_exact_codes`).
  *Done (W7-C):* the menu's whole-layer edits run at the layer's own depth
  in a 16-bit document. `OpenDocument::layer_rgba16` reads every tile at 16
  bits (an RGBA8 tile widened losslessly) and `layer_rgba16_command` writes
  RGBA16 tiles; `filters::FilterBuffer::from_rgba16` / `to_rgba16` carry the
  layer into and out of the f32 engines. Every filter and adjustment applied
  from the menu or its dialog (`edit_active_pixels`), Fill, Clear, Edit >
  Transform's flips and 90/180 turns and Image > Image Rotation 180 and flips
  (`remap_*`), and Image > Image Size (`resample_layer16_edits`) take that
  road; an 8-bit document keeps the RGBA8 road unchanged. Round 2: Free
  Transform's resample (Distort, Perspective, Warp and a floated selection)
  loads its plane with `tools::patch::ColorPatch::load_native`, which reads
  a tile through `TileAccess::native_bytes` (the tool context's
  `DocumentTiles` answers with the stored bytes, bypassing `NarrowedReads`),
  decodes RGBA16 at full precision and commits RGBA16 tiles, writing every
  pixel it did not change back with its exact stored code. Round 3: the
  plane takes the document's depth from the store
  (`TileAccess::sixteen_bit_document`, which the tool context's
  `DocumentTiles` answers from `meta.bit_depth` via `at_document_depth`)
  rather than from tile lengths, so a 16-bit document whose tiles are still
  RGBA8 (every opened 16-bit PNG or TIFF) widens them losslessly and
  resamples at 16 bits too
  (`an_opened_sixteen_bit_png_free_transformed_by_a_sub_pixel_stays_sixteen_bit`,
  `a_native_plane_in_a_sixteen_bit_document_widens_rgba8_tiles`); the whole-layer
  Scale/Rotate/Skew stays a layer transform (no resample). Image > Image
  Rotation 90 CW/CCW and Arbitrary (`rotated90_layer16`, `rotated_layer16`)
  and Crop to Selection / Trim (`reframed_layer16`) read the layer's
  composite at 16 bits and write RGBA16 tiles. Tests:
  `brightness_on_a_sixteen_bit_document_is_not_rounded_through_eight_bits`
  (the result equals the f32 engine's 16-bit answer, over 90% of samples off
  the 8-bit grid), `gaussian_zero_levels_and_curves_identity_keep_every_sixteen_bit_code`,
  `rotate_ninety_twice_on_a_sixteen_bit_document_moves_every_code_exactly`,
  `image_size_half_then_double_on_a_sixteen_bit_ramp_keeps_every_code`
  (interior within one code), `a_half_opacity_fill_on_a_sixteen_bit_document_is_computed_at_sixteen_bits`,
  `canvas_size_on_a_sixteen_bit_document_keeps_every_code_exactly`,
  `rgba16_round_trips_within_one_code`,
  `free_transform_distort_on_a_sixteen_bit_layer_resamples_at_sixteen_bits`,
  `a_floating_free_transform_on_a_sixteen_bit_layer_keeps_every_code`,
  `image_rotation_ninety_on_a_sixteen_bit_document_moves_every_code`,
  `image_rotation_arbitrary_on_a_sixteen_bit_document_interpolates_at_sixteen_bits`,
  `crop_to_selection_on_a_sixteen_bit_document_keeps_every_code`,
  `clear_under_a_partial_selection_keeps_a_sixteen_bit_remainder`,
  `near_identity_levels_and_curves_through_the_menu_stay_at_sixteen_bits`,
  `a_native_plane_over_a_sixteen_bit_tile_keeps_every_code`,
  `a_native_plane_writes_untouched_sixteen_bit_pixels_back_exactly`.
  *Still open:* the painting tools (brushes, bucket, gradient and the rest
  still read through `NarrowedReads` into the RGBA8 `ColorPatch::load`),
  Stroke, Apply Mask, Defringe, Layer via Copy/Cut, Grayscale and Edit >
  Fill > Content-Aware (it synthesises the fill from `pixels::read_layer`;
  Content-Aware Scale runs at 16 bits) still compute at 8-bit precision: a
  pixel they change comes back as a widened 8-bit code. The Filter dialog's preview is
  rendered from an 8-bit read (the applied result is 16-bit). There is no user switch for the 16 -> 8 dither (it is always
  on from the menu). `open_path` still decodes a 16-bit source to RGBA8 tiles
  (the document's depth says 16, the first edit of a tile widens it). `.psd`
  export is 8-bit: RGBA16 layer tiles are rounded to RGBA8 on the way out
  (`import::rgba_from_tiles` reads through `raster::rgba8_view`; pinned by
  `a_sixteen_bit_document_saves_as_psd_with_the_eight_bit_twins_layer_pixels`),
  so a 16-bit document saved as PSD loses its 16-bit precision. The tile
  readers that need only coverage (the compositor's `alpha_bounds` for the
  Move tool and the Properties panel, the Move tool's auto-select pick) read
  RGBA16 alpha through `raster::tile_alpha16`.
- **Stylus pressure is verified with synthetic events only.** winit `Touch`
  events feed `Shell::set_pen_pressure` through `pen_input.rs` and drive the
  mouse's pointer route; the shell tests use synthetic events, and a
  physical pen on each platform is still needed to confirm the OS's event
  order. There is no Opacity from Pressure toggle (Flow from Pressure is the
  nearest), and tilt, rotation, hover and the eraser end are not read.
- **No keyboard focus navigation.** Tab is withheld from egui and toggles
  the panels; controls are reached with the pointer.
- **The OS printer-spooler dialog.** Print ▸ As PDF renders the composite to a
  tested single-page PDF; talking to an actual printer spooler is OS-only and
  not part of the build.
- **No View toggle is refused any more:** View ▸ Proof Colors and Gamut
  Warning are enabled and change the canvas since W7-D, and so are Image ▸
  Mode ▸ Lab / CMYK / Indexed (`ui::view_flag_unavailable` stays as the one
  table a future refusal would go in). Every
  enabled item routes to real code; `menu_bridge::unavailable_reason` keeps
  only the File-Info note (no XMP). An adjustment dialog refuses to confirm
  an all-identity setting. The route coverage is pinned by `menu_bridge`'s
  `no_enabled_menu_item_resolves_to_a_no_op` digest.
- **Per-channel masking stops at colour components.** The Channels panel
  isolates, paints into, erases within, fills, filters and bakes adjustments
  into a single RGB component — every one rides `mask_paint_to_channel` at
  the command boundary (`the_eraser_through_the_red_channel_clears_only_red`,
  `gaussian_blur_through_the_red_channel_blurs_only_red`), so the masked
  command reaches history and the journal. An alpha or mask-coverage target
  paints normally rather than being isolatable, and the panel has no
  per-channel histogram.
- **Absent and not yet placed in Tier C:** Content-Aware Move, a separate
  Slice Select tool;
  Lens Correction, Adaptive Wide Angle, the Blur Gallery (Field / Iris /
  Tilt-Shift); Layer Comps and Tool Presets panels; File ▸ Automate / Batch
  and Scripts; vector masks (PSD import rasterises them).
- **Release job never run.** CI's `release` job (a `v*` tag or a manual
  `release_dry_run`) has not run; no tag exists.

## Release gate

1. Every Tier A row is ✅ or has its gap named above.
2. Every Tier B row is ✅, named as partial, or moved to Tier C with a reason.
3. `cargo check --workspace --all-targets`, `cargo clippy` with `-D warnings`,
   `cargo fmt --check` and `cargo test --workspace` are green.
4. The app launches, opens a real image, edits it, saves, reopens and exports
   correctly — verified by running it, not by prose.
