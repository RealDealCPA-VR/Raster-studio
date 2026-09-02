# Production TODO — Raster Studio → Photopea parity

**Goal:** ship-ready Raster Studio whose style and UI/UX match
[Photopea](https://www.photopea.com/).

> **Picking up work?** Start at
> [`CORRECTIONS-TODO.md`](CORRECTIONS-TODO.md) — every item still open in this
> file (the P5 and P6 queues below) pulled into one ordered, directly
> implementable list with a Validate line each. This file is the evidence and
> the history; that one is the work.

**How to use this file.** Each task is one self-contained unit of work: an
action, the files it touches, and a **Validate** line that is objectively
checkable. Do not tick a box without the Validate line passing. Every task must
additionally leave the four release gates green:

```bash
cd raster-studio
cargo fmt --check
cargo clippy --workspace --all-targets   # CI runs with RUSTFLAGS=-D warnings
cargo check --workspace --all-targets
cargo test  --workspace
cargo audit                              # the fifth gate — CI runs it and it can fail alone
```

> **`cargo audit` is a release gate, not a footnote.** It is a separate CI job
> that fails the build on its own, and a task that leaves the other four green
> while adding an advisory to `Cargo.lock` is not done. Run it after any change
> that touches `Cargo.toml` or `Cargo.lock`.

Work top-down: **P0** unblocks the largest amount of already-written code, **P1**
is the visual/interaction identity, **P2** closes named engine gaps, **P3** is
release engineering, **P4** is scope decisions, **P5** is the CI-blocking
regression queue (always drain it first).

---

## Verified baseline (measured 2026-09-02, post-corrections C1–C4)

| Fact | Value |
| --- | --- |
| `cargo check --workspace --all-targets` | exit 0 |
| `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| `cargo fmt --check` | exit 0 |
| `cargo test --workspace` | **3402 passed, 0 failed** |
| `cargo audit` | exit 0 (`.cargo/audit.toml` carries two named, expiring ignores — see `threat-model.md` §8) |
| App launches, renders, screenshots | yes — `studio-desktop --shot out.png img.png`; the shot runs 24 warm-up frames first (C1), so the image shows laid-out UI: populated tool column and, with no document, the start screen |
| Workspace size | 22 crates, ~280 `.rs` files, ~135k LOC |
| Tools implemented | 47 (`tools::ToolId::ALL`) |
| Menus | 9 (File Edit Image Layer Select Filter View Window Help) |
| Panels | 13 (`ui::dock::PanelId::ALL`) |

The two largest integration gaps of the 2026-09-01 baseline are **closed**:
all ten dialogs are hosted by the shell's dialog host (P0.1–P0.16), and the
canvas has a transform-gizmo overlay (P2.1–P2.3). The live open-items queue is
**P2.5b** (live tiles composite deeper than 8 bits — P2.5 split by C8), the
P5 regression queue, and the P6 documentation-truth queue; the ordered,
implementable list of everything still open is
[`CORRECTIONS-TODO.md`](CORRECTIONS-TODO.md). After C7 there are no disabled
menu items with vague reasons: the only `unavailable_reason` arms left are the
File-Info note and an adjustment clicked while its parameters sit at the
identity.

Reference list of everything currently greyed out:
`app_shell::menu_bridge::unavailable_reason` (`crates/app-shell/src/menu_bridge.rs`).

---

## P0 — Dialog surface (unblocks ~24 disabled menu items)

### P0.1 Build the dialog host
Add a `DialogHost` to `crates/app-shell/src/chrome.rs` owning
`Option<ActiveDialog>` (an enum over the ten `ui::dialogs` types), drawn after
the docks, with at most one open at a time. Route keys through
`ui::dialogs::resolve` so Escape cancels and Enter confirms, and suppress canvas
pointer/keyboard input while one is open.
**Validate:** a headless `chrome.rs` test opens a dialog via `MenuAction`,
asserts a modal is drawn, sends Escape, and asserts no document/command was
produced; a second test asserts a canvas click while a dialog is open routes no
`RoutedPointer`.

### P0.2 File ▸ New…
Route `MenuAction::NewDocument` through `ui::dialogs::NewDocumentDialog` instead
of `Editor::act_new_document`'s hardcoded `NEW_DOCUMENT_SIZE`
(`crates/app-shell/src/editor.rs:2260`).
**Validate:** a test confirms the dialog with 1920×1080 / transparent background
and asserts the new `Document` is 1920×1080 with a transparent base layer;
cancelling creates no document.

### P0.3 File ▸ Export As…
Replace the per-format `MenuAction::Export(f)` straight-to-picker path with
`ExportAsDialog` (format, quality, scale, background).
**Validate:** exporting the same document as JPEG at quality 30 and 95 produces
two files where `len(q30) < len(q95)`; exporting at scale 0.5 produces a PNG of
half the pixel dimensions.

### P0.4 Image ▸ Image Size…
Host `ImageSizeDialog` and add the resample command it needs to
`editor_core::Command` (the current reason is "no command that can carry that as
one undoable step").
**Validate:** `unavailable_reason(MenuAction::ImageSize) == None`; a test resizes
800×600 → 400×300, asserts the new size, then `undo()` restores 800×600 and the
original pixels byte-for-byte.

### P0.5 Image ▸ Canvas Size…
Host `CanvasSizeDialog` (anchor grid, relative mode) on the same undoable
canvas-resize command already used by Crop/Trim.
**Validate:** `unavailable_reason(MenuAction::CanvasSize) == None`; enlarging
100×100 → 200×200 anchored top-left leaves the original pixels at (0,0) and undo
restores; anchored centre offsets them by (50,50).

### P0.6 Image ▸ Image Rotation ▸ Arbitrary…
Add an angle dialog and route `MenuAction::RotateCanvas(Arbitrary)`.
**Validate:** `unavailable_reason` returns `None`; rotating 90° through the
arbitrary path produces the same pixels as the fixed 90° command.

### P0.7 Image ▸ Reveal All
Route `MenuAction::RevealAll` onto the canvas-resize command (grow the canvas to
the union of all layer content bounds).
**Validate:** a layer whose content extends past the canvas causes Reveal All to
grow the canvas to contain it; undo restores; `unavailable_reason == None`.

### P0.8 Filter parameter dialogs
Route every `MenuAction::Filter(id)` through `ui::dialogs::FilterDialog` (the
schema-generated one) with a live preview, instead of
`FilterParams::defaults` (`crates/app-shell/src/menu_bridge.rs:1107`).
**Validate:** `unavailable_reason` returns `None` for `FilterId::Custom` and
`FilterId::Offset`; a test drives Gaussian Blur through the dialog at radius 0
(no pixel changes) and radius 8 (pixels change), each one undo step.

### P0.9 Filter ▸ Filter Gallery
Draw the gallery over `ui::dialogs::filter_dialog::FILTERS` with a thumbnail
preview per filter.
**Validate:** `unavailable_reason(MenuAction::FilterGallery) == None`; a test
asserts the gallery lists exactly `FilterId::ALL.len()` entries and applying one
from it produces the same pixels as the menu item.

### P0.10 Layer ▸ Layer Style… / Blending Options…
Host `LayerStyleDialog` (see the standing comment at
`crates/app-shell/src/menu_bridge.rs:1885`).
**Validate:** setting a drop shadow through the dialog changes the composite,
arrives as exactly one history entry, and undo removes it.

### P0.11 Colour picker
Wire `ColorPickerDialog` to the foreground/background swatches, layer-effect
swatches, gradient stops and colour-valued filter params through
`ui::dialogs::color_edit::ColorEdit`; pass the screen sampler so the eyedropper
works.
**Validate:** a test clicks the foreground swatch, confirms the picker with a
known RGB, and asserts `Editor`'s foreground colour equals it.

### P0.12 Gradient editor
Open `GradientEditorDialog` from the gradient tool's options-bar ramp swatch
(`ui::view::ids::gradient_swatch`).
**Validate:** editing a stop through the dialog changes the gradient the bucket
of the next gradient stroke paints; a test asserts the stop list round-trips.

### P0.13 Brush editor
Open `BrushEditorDialog` from the Brushes panel.
**Validate:** a test changes hardness through the dialog and asserts
`tools::BrushSettings::hardness` on the editor changed.

### P0.14 Preferences — one implementation
`chrome.rs::preferences_window` (hand-rolled) and
`ui::dialogs::PreferencesDialog` (1522 LOC, unused) are duplicates. Keep the
`ui::dialogs` one; delete the other.
**Validate:** `grep -rn "preferences_window" crates/` returns nothing; the
Preferences window still edits theme, UI scale, autosave interval, history depth
and the keymap, asserted by the existing chrome tests ported over.

### P0.15 Edit ▸ Fill… / Stroke… dialogs
`FillDialog`/`StrokeDialog` currently apply the foreground colour with no
choices. Give them Photopea's options (fill: colour/pattern/blend/opacity;
stroke: width, location inside/centre/outside, blend, opacity).
**Validate:** a 3px outside stroke and a 3px inside stroke on the same selection
produce different pixels; each is one undo step.

### P0.16 Pattern and brush presets
Add a user-preset store to `asset-store` and route Edit ▸ Define Pattern / Define
Brush and the Pattern fill layer / Pattern Stamp tool into it.
**Validate:** `unavailable_reason` returns `None` for `DefinePattern`,
`DefineBrush` and `NewFillLayer(Pattern)`; defining a pattern from a selection
and filling with it reproduces the selection's pixels tiled.

---

## P1 — Photopea look and feel

Photopea reference points to match: compact dark-grey chrome, ~11–12px type, flat
1px borders, a single left tool column, all panels tabbed into groups on the
right, document tabs above the canvas, a flat pasteboard around the document, and
right-click context menus everywhere.

### P1.1 Restate the design target
`docs/parity-matrix.md` named the old design target by its former reference
brand. The goal is now Photopea. Update that row and the `design` crate's module
docs.
**Validate:** `grep -rn "the former reference brand name" docs/ crates/` returns
nothing — i.e. no live document or source string names the old target.

### P1.2 Dark by default, Photopea greys
`design::Theme::default()` is already `Dark`, but the shell overrides it from the
OS (`shell.rs:1254 system_theme`), so it launches light on a light-mode host.
Make Photopea's dark grey the shipped default (system-follow becomes an opt-in
preference), and retune `DARK_ROLES` in
`crates/design/src/tokens/palette.rs` to Photopea's neutral greys.
**Validate:** a fresh profile launches dark on a light-mode Windows host,
verified with `--shot`; the existing WCAG-AA contrast test still passes over the
new palette.

### P1.3 Density pass
Photopea's rows are roughly half the height of the current ones. Retune
`design::tokens::spacing::Metrics`, `Space` and `TypeScale` to a compact scale
(control height ~20px, body type ~11–12px, panel padding ~4px).
**Validate:** `--shot` at 1440×900 with a document open shows the Layers panel
fitting ≥ 12 layer rows (today: ~4); a token test pins the new control height and
type sizes so they cannot drift back.

### P1.4 Tabbed panel groups
`DockState` stacks panels as accordions. Add groups with a tab strip per group
and Photopea's default grouping: Layers/Channels/Paths, History/Actions,
Properties/Adjustments, Info/Navigator, Character/Paragraph.
**Validate:** unit tests on `DockState` for "clicking a tab raises that panel",
"dragging a panel into a group joins it", "closing the last tab removes the
group"; `--shot` shows one tab strip per group.

### P1.5 Default layout = Photopea's
`LayoutId::Essentials` currently opens a left rail (History, Color). Photopea has
no left dock. Move all panels to the right rail.
**Validate:** a `DockState::Essentials` test asserts `side_is_empty(Left)`;
`--shot` shows the canvas starting immediately right of the tool column.

### P1.6 Flat pasteboard around the document
`crates/render-shaders/src/shaders/quad.wgsl` paints the transparency
checkerboard over the whole viewport ("the checkerboard is the backdrop
everywhere"). Photopea paints a flat neutral pasteboard outside the document and
the checkerboard only inside it. Pass the document rect into the shader and
select backdrop-vs-checker on `inside`.
**Validate:** the existing `render/tests/gpu.rs` gains a case reading a pixel
outside the document bounds and asserting it equals `Canvas::backdrop()`, and a
pixel over a transparent document pixel and asserting it is a checker cell.

### P1.7 Document border and shadow
Draw Photopea's 1px border (and subtle shadow) around the document rect.
**Validate:** a `ui::canvas::paint` unit test asserts a border shape is emitted
on the document boundary at several zoom levels and none when no document is open.

### P1.8 Document tabs
`chrome.rs::tab_strip` draws full-width rows. Make them Photopea tabs: fixed
max-width, ellipsised title, dirty dot, close ×, middle-click to close, drag to
reorder, overflow chevron.
**Validate:** tests for middle-click close, drag reorder changing document order,
and a 30-character title being ellipsised rather than widening the strip.

### P1.9 Start screen
With no document the tab strip shows a sentence. Replace it with Photopea's start
screen: New / Open buttons and the recent-files list on the canvas area.
**Validate:** a `--shot` with no arguments shows the start screen; clicking the
recent entry at index 0 opens that file (headless test through `ChromeOutput`).

### P1.10 Tool-column footer
Add Photopea's bottom-of-column controls: foreground/background swatches with
swap (X) and reset (D), quick-mask toggle (Q), screen-mode cycle (F).
**Validate:** clicking the swap swatch exchanges the editor's foreground and
background colours; each control has a stable `ui::view::ids` id and a click test.

### P1.11 Tool flyouts and tooltips
Photopea opens a tool group's flyout on press-and-hold and shows
"Brush Tool (B)" in the tooltip. Today the flyout needs a second click or a
right-click and the tooltip carries no shortcut letter.
**Validate:** a test asserts the tooltip string for every tool ends with its
`ToolKey` letter in parentheses; a press-and-hold opens the flyout.

### P1.12 Right-click context menus
There are none anywhere (`grep -rn "context_menu" crates/` → 0). Add: canvas
(tool-specific: selection ops, transform, fill), layer row (duplicate, delete,
blending options, rasterize, merge, clipping mask), document tab (close, close
others, close all), panel header (☰ panel menu).
**Validate:** one test per menu asserting the item set for a representative
state, plus that every entry resolves to an enabled `Intent` or a
`Resolution::Disabled` with a reason (the same gate `menu.rs` already applies to
the menu bar).

### P1.13 Status bar
Photopea's bottom-left is an *editable* zoom percentage, the document dimensions,
and a "▸" menu of readouts. The current strip is a row of static labels.
**Validate:** typing `200` into the zoom field sets the camera zoom to 2.0 and
the canvas redraws; a test drives the field through `ChromeOutput`.

### P1.14 Canvas scrollbars
Photopea shows scrollbars on the right and bottom of the canvas viewport.
**Validate:** dragging the horizontal scrollbar pans the camera by the
proportional document distance (unit test on the camera, no window needed).

### P1.15 Icon weight pass
The current glyphs (`crates/ui/src/icons.rs`) render thin and low-contrast at
tool-column size — visible in a `--shot`. Rework stroke weight and size to
Photopea's monochrome set.
**Validate:** an icons test asserts a minimum ink coverage per icon at the
tool-button size, and `--shot` is reviewed side by side with Photopea.

### P1.16 Layers panel Photopea parity
Add the footer row Photopea has (link, fx, mask, adjustment, group, new, delete),
the lock row, a layer-kind filter, and a thumbnail-size control.
**Validate:** each footer button has a stable id and a click test asserting the
command it emits; the filter row hides non-matching rows in `LayersModel`.

### P1.17 Multi-layer selection
`editor_core::Document` stores one active layer, which is why
`Select ▸ All Layers` is disabled and Ctrl/Shift-click in the Layers panel does
nothing. Photopea multi-selects. Add a selection set to `Document`.
**Validate:** `unavailable_reason(MenuAction::SelectAllLayers) == None`;
Shift-clicking two rows selects both; Delete removes both as one undo step.

### P1.18 Panel collapse-to-icons
Photopea collapses a dock to an icon rail. Add it.
**Validate:** a `DockState` test for collapse/expand round-trip; the collapsed
rail's width is the icon width.

### P1.19 Fix clipped numeric fields
In the Color panel the H/S/B/Alpha numeric boxes are cut off by the panel edge
(reproducible in `--shot` at the default dock width).
**Validate:** a layout test asserts every numeric field's rect is contained by
its panel's rect at `MIN_DOCK_WIDTH`, the default width and `MAX_DOCK_WIDTH`.

---

## P2 — Canvas gizmos and named engine gaps

### P2.1 Transform gizmo overlay
The single highest-leverage missing piece: an on-canvas handle overlay
(8 handles, rotate zones, movable reference point, numeric fields in the options
bar, Shift/Alt modifiers, Enter/Escape commit/cancel).
**Validate:** `unavailable_reason` returns `None` for `FreeTransform` and
`Transform(Scale|Rotate|Skew|Distort|Perspective)`; dragging a corner handle
emits exactly one undoable command; a singular matrix is refused, not applied.

### P2.2 Transform Selection
Reuse P2.1's gizmo against `selection::transform_selection`.
**Validate:** `unavailable_reason(MenuAction::TransformSelection) == None`;
scaling a selection changes its mask and undo restores it.

### P2.3 Warp
Add a mesh gizmo and a mesh deformer.
**Validate:** `unavailable_reason(MenuAction::Transform(Warp)) == None`; a
control-point drag bends the layer and is one undo step.

### P2.4 Place Embedded… / Place Linked…
Add the nested-source model (a smart object whose `asset` is a source, not a
cache key) and use P2.1's gizmo for placement.
**Validate:** `unavailable_reason` returns `None` for both; placing a PNG creates
a smart-object layer that renders it, and Edit Contents… round-trips; a linked
source re-read from disk updates the parent.

### P2.5a `.rstudio` records the source depth — DONE
Deep sources round-trip: the project package stores each layer's bit depth and
restores it on open (`a_rstudio_package_round_trips_the_bit_depth`), and export
writes 16-bit data.
**Validate:** `.rstudio` round-trips the depth field. ✅

### P2.5b live tiles composite deeper than 8 bits — OPEN
The store holds 16-bit tiles and export writes them, but live compositing still
runs at 8-bit-equivalent precision: `OpenDocument::composite` returns `Vec<u8>`
(doc.rs), and `NewDocumentDialog` refuses to create a 16-bit document rather
than confirm one that would draw as garbage (its test pins the refusal with
that reason).
**Validate:** `composite` returns a deep buffer (or a depth-parameterised one);
a 16-bit gradient survives ten adjustment-layer edits without banding,
asserted by a histogram test (the existing banding test proves non-destructive
adjustment composition — live f32 parameters — which is necessary but not
sufficient for this); `NewDocumentDialog` offers 16-bit without refusing and
its refusal test is replaced by one that creates and composites a 16-bit
document; the parity-matrix row stays 🔶 until then.

### P2.6 ICC through the pipeline
`color::icc` parses and applies matrix-shaper profiles, but
`ColorSpace::IccProfile` carries no bytes, so a tagged image is preserved rather
than applied.
**Validate:** opening an image tagged with a non-sRGB matrix-shaper profile
composites through that profile; exporting re-tags it; a round-trip test pins the
pixel values.

### P2.7 Per-channel eraser and filters
Channel isolation masks `PaintTiles` and `FillRegion` only.
**Validate:** with the red channel as the edit target, the eraser clears only red
and a Gaussian Blur blurs only red; each undoes whole.

### P2.8 Quick mask
Compose the quick-mask overlay and its toggle (Q).
**Validate:** entering quick mask, painting, and exiting produces the selection
the painted coverage describes; the parity-matrix row flips to ✅.

### P2.9 Colour mode conversion
`Image ▸ Mode` is disabled because no command carries a full-document rewrite.
**Validate:** `unavailable_reason(MenuAction::SetColorMode(_)) == None`;
RGB → Grayscale → RGB is one undo step each way and the greyscale values match
`color`'s luminance.

### P2.10 Path Select / Direct Selection tools
Photopea's `A` tools are absent from `ToolId::ALL`. Add them.
**Validate:** clicking a path with Path Select selects it; dragging an anchor with
Direct Selection moves it as one undo step.

### P2.11 Actions panel
Commands are serialisable and replayable; there is no recording UI.
**Validate:** recording three edits, replaying them on a second document produces
the same composite bytes.

### P2.12 Select Subject — decide
Either bundle a segmentation model or remove the item from the Select menu and
move the row to Tier C.
**Validate:** whichever is chosen, no menu item is left permanently disabled for
a capability the project has decided not to ship.

---

## P3 — Release engineering

### P3.1 Panic policy
`profile.release` sets `panic = "abort"` and no panic hook exists, so a panic
kills the app with unsaved work in memory.
**Validate:** an injected panic writes an autosave and a `telemetry::
DiagnosticBundle` to the recovery directory before exiting; the next launch
offers recovery. Asserted by a test that runs the hook directly.

### P3.2 Wire or delete `licensing` / `updater`
Both crates are complete and tested with **zero dependents**
(`grep` over every `Cargo.toml`). Either integrate them (entitlement check at
launch, update check + About ▸ Check for Updates) or drop them from the
workspace.
**Validate:** either a test drives the app path that consumes them, or they are
gone from `Cargo.toml` `members` and the parity matrix says so.

### P3.3 Diagnostics export
`telemetry::DiagnosticBundle` is never populated (only `init_tracing` is called).
Add Help ▸ Export Diagnostics… writing a bundle with app version, OS, GPU adapter
and recent log lines.
**Validate:** the written JSON contains the real adapter name from the live
`wgpu` context and `upload_consented: false`.

### P3.4 macOS and Linux packaging
Only `apps/studio-desktop/packaging/raster-studio.iss` (Inno Setup, Windows)
exists.
**Validate:** a `.dmg`/`.app` and an AppImage or `.deb` are produced by a
documented command and launch on a clean machine.

### P3.5 Release CI
CI runs fmt/clippy/test/audit but builds no artifacts and does not cover macOS.
**Validate:** a tag push produces signed installers for all three platforms as
workflow artifacts; `macos-latest` is in the test matrix and green.

### P3.6 Code signing / notarisation
**Validate:** the Windows installer is Authenticode-signed and the macOS bundle
passes `spctl --assess`.

### P3.7 Decoder resource limits
`crates/raster/src/codec.rs:583` notes that decode limits are not propagated into
the `png` crate — a decompression-bomb surface.
**Validate:** a crafted PNG declaring an enormous canvas is refused with a named
error instead of allocating; add it to the threat-model doc.

### P3.8 Memory ceiling for very large documents
**Validate:** a documented tile-cache ceiling is enforced and a
16000×16000 × 10-layer document stays under it while remaining editable; asserted
in `tests/integration/tests/performance.rs`.

### P3.9 Wall-clock performance budget
`performance.rs` asserts cache *hit/miss counts*, not time.
**Validate:** a budget test asserts a partial recomposite of an 8000×6000
document completes under a stated millisecond ceiling on CI hardware.

### P3.10 `.rstudio` format versioning
**Validate:** a fixture written by an older format version opens through a
migration path, asserted by a checked-in fixture test.

### P3.11 Accessibility
No focus-ring navigation, no AccessKit, no screen-reader labels.
**Validate:** Tab reaches every control in the docks in visual order with a
visible focus ring; AccessKit reports a labelled node per control.

### P3.12 Localization scaffolding
Every string is a Rust literal. Photopea ships many languages.
**Validate:** strings resolve through a catalogue keyed by locale, and a test
asserts no user-facing literal remains in `crates/ui/src/view` or
`crates/ui/src/dialogs`.

### P3.13 README screenshot
`docs/REMAINING.md` S2.3 ends with "a future task can drop the resulting PNG into
the README" — it is still not there.
**Validate:** `README.md` embeds a committed `--shot` PNG of the current build.

### P3.14 Help destinations
Help ▸ Help / Release Notes / Report Issue are wired but need real destinations,
and `workspace.package.repository` is empty.
**Validate:** each opens a reachable URL; `repository` is set.

---

## Review checkpoint — 2026-09-02, commit `35a03d0`

Independent re-measurement by the reviewer (not copied from the ledger):

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 |
| `cargo clippy --workspace --all-targets` | exit 0, no warnings |
| `cargo test --workspace` | **3398 passed, 0 failed** (was 3288) |
| `cargo test -p app-shell --lib` | 395 passed, 0 failed |
| `cargo audit` | **exit 1 — 2 vulnerabilities** ❌ |
| `git rev-list --count origin/main...HEAD` | 0 — main is pushed, nothing local-only |

**60 of 61 tasks are closed** (P0.1–P0.16, P1.1–P1.19, P2.1–P2.12, P3.1–P3.11,
P3.13, P3.14, and P4 confirmed). Progress ledger: `.pi/goals/ACTIVE.md`.

Still open from the original list:

- **P3.12 Localization** — infrastructure landed (`crates/ui/src/strings.rs`:
  `Locale`, a process-wide active locale, a keyed table, `tr()`, three tests) and
  the Actions panel migrated as a proof slice. The migration wave is **not**
  done: 311 prose literals across 19 files in `crates/ui/src/{view,dialogs}`
  remain, and the no-literal lint that is this task's Validate line is not yet
  written. Correctly left unticked.
- **P3.4 / P3.6 / P3.11** are ticked "as far as this host allows" — packaging
  configs and signing commands are written but no `.dmg`/`.deb` has been built,
  no artifact has been signed, and no screen-reader walk has been performed.
  Treat these as **needing a verification pass on real hardware**, not as
  finished. See P5.5.

Two pieces of drift found while reviewing (neither breaks a test, both mislead
the next reader) are filed as P5.6 and P5.7.

---

## P5 — CI-blocking regressions (drain before resuming P3.12)

### P5.1 `cargo audit` fails on two high-severity advisories — **the live CI break**
`cargo audit` exits 1. The reported cause is **not** the two unmaintained-crate
warnings that appear at the tail of the log (`paste` RUSTSEC-2024-0436,
`ttf-parser` RUSTSEC-2026-0192) — those are already non-fatal, and the run says
so: `warning: 2 allowed warnings found`. The failure line is
`error: 2 vulnerabilities found!`, and both are `quick-xml 0.30.0`:

| ID | Severity | Fix |
| --- | --- | --- |
| RUSTSEC-2026-0194 — quadratic run time on duplicate attribute names | 7.5 high | upgrade to ≥ 0.41.0 |
| RUSTSEC-2026-0195 — unbounded namespace allocation, memory-exhaustion DoS | 7.5 high | upgrade to ≥ 0.41.0 |

Provenance (`cargo tree -i quick-xml@0.30.0 --target all`):

```
quick-xml 0.30.0 ← zbus_xml 4.0.0 ← zbus-lockstep 0.4.4 ← atspi 0.22.0
                 ← accesskit_unix 0.12.3 ← accesskit_winit 0.22.4 ← app-shell
```

**This chain entered the lockfile with P3.11 (AccessKit, commit `7a3ee3f`)** and
is Linux-only, which is why the ubuntu `audit` job is the one that breaks. The
same commit is also what pulled in the `paste` warning, via `accesskit_windows`.
`quick-xml` cannot simply be bumped: `zbus_xml 4.0` pins `^0.30`, so the version
is decided upstream.

Pick one and record which, with the reason:
- **(a)** Raise `accesskit_winit` past the `atspi`/`zbus` generation that carries
  `quick-xml 0.30`. Blocked today by `egui-winit 0.29`, which itself pins
  `accesskit_winit 0.22` — a mismatched bump produced two copies of the crate
  once already (see the ledger). Realistically this arrives with an egui upgrade.
- **(b)** `accesskit_winit = { version = "0.22", default-features = false }` to
  drop the AT-SPI (Linux) backend. Removes the advisory chain outright; costs
  Linux screen-reader support, which contradicts P3.11 — say so in the
  parity matrix if chosen.
- **(c)** A time-boxed `.cargo/audit.toml` ignore with a named expiry and a
  tracking issue. A suppression, not a fix — and the weakest option for a
  project whose first rule is that documentation is a claim.

**Validate:** `cargo audit` exits 0 from `raster-studio/`; the ubuntu `audit` CI
job is green; whichever option was taken is named in `docs/threat-model.md` with
its trade-off, and if it is (c) the ignore carries an expiry date.

### P5.2 Confirm the pasted `no_enabled_menu_item_resolves_to_a_no_op` failure is closed
The reported failure —

```
ReleaseNotes did not reach the status bar
  left:  Some("Release notes live at https://…/releases")
  right: Some("Opened https://…/releases")
```

— **no longer reproduces at `35a03d0`**; `cargo test -p app-shell --lib` is
395/395 locally. Commit `2d2fde9` is the fix: the three Help arms were folded
into `open_help_url`, which short-circuits on `cfg!(test)`.

Root cause, for the record: each Help arm returned `"Opened …"` or a fallback
**depending on the live result of `webbrowser::open`**, and the gate test calls
`perform` twice — once for the message-length check and once inside the
`assert_eq!` to build the expected value (`menu_bridge.rs:3363`). On a runner
with no browser the two calls disagreed. The pasted log therefore comes from a CI
run at or before `f7d8161`.

**Validate:** re-run the failing CI job at `35a03d0` (or later) and confirm it is
green; if it still fails, the diagnosis above is wrong and P5.3/P5.4 become
urgent rather than preventive.

### P5.3 Close the `cfg!(test)` hole around the Help URLs
`cfg!(test)` is true only while compiling the crate *under test*. Every
integration test — `tests/integration/`, `crates/app-shell/tests/gpu.rs`,
`crates/ui/tests/*` — links `app-shell` as an ordinary dependency where it is
**false**. Any of them that reaches a Help action will really launch a browser on
the CI runner and bring the flake straight back. The crate already has the right
pattern for this: the `FileDialogs` / `ScriptedDialogs` seam.
**Validate:** URL opening goes through an injected seam whose test double records
the URL instead of opening it; a test asserts the recorded URL for each of Help,
Release Notes and Report Issue; `grep -rn "cfg!(test)" crates/app-shell/src`
returns nothing that changes user-visible behaviour.

### P5.4 Stop the gate test invoking `perform` twice
`menu_bridge.rs:3363` computes the expected status by calling
`perform(action, &mut ed)` a second time. For any action with a side effect that
is a double execution, and it is what turned an environment-dependent message
into a hard failure. Capture the first call's `Ok(message)` and assert the status
against that value.
**Validate:** the `INFORMATIONAL` branch calls `perform` exactly once per action
(asserted by reading the code, and by a counter in a test double if the seam from
P5.3 lands); the test still fails if an informational action does not reach the
status bar.

### P5.5 Verify the three "as far as this host allows" tasks on real hardware
P3.4 (macOS/Linux packaging), P3.6 (signing/notarisation) and P3.11 (AccessKit)
are ticked with the honest caveat that this Windows host could not finish them.
They are not verified.
**Validate:** a `.dmg`/`.app` and a `.deb` or AppImage are built and launch on a
clean machine; the Windows installer is Authenticode-signed and the macOS bundle
passes `spctl --assess`; one screen reader (NVDA, VoiceOver or Orca) reads the
tool palette and the Layers panel aloud, recorded in the ledger.

### P5.6 Delete the four stale `unavailable_reason` arms
`unavailable_reason` still returns a reason for actions that are now enabled or
gone, and `resolve` only consults it when `pick` returns `None`, so these arms
are dead text that reads as a live limitation:
- `PlaceEmbedded | PlaceLinked` — says "the canvas has no gizmo overlay". The
  gizmo landed in P2.1 and both route to `editor.place_from_dialog` (P2.4).
- `SetColorMode(_)` — says "`editor_core` has no command that can carry that as
  one undoable step". P2.9 added one.
- `SelectSubject` — the item was removed from `select_menu()` by P2.12, so the
  arm is unreachable.
- `FileInfo` — deliberately retained to feed the unrouted-message path
  (`shell_action` routes the item and it performs). Keep it, but say *that* in
  the arm, because as written it reads as a disabled item.
**Validate:** each removed arm's action resolves to `Ok(_)` in a populated
context; `no_menu_item_falls_back_to_the_generic_refusal` still passes; the
remaining arms are exactly the ones a user can still hit.

### P5.7 Refresh the file header — it describes a state that no longer exists
The "Verified baseline" block and the two numbered findings above it still say
the dialogs have zero call sites and the canvas has no gizmo. Both were closed by
P0.1–P0.16 and P2.1. A future agent reading top-down is told the opposite of what
the code does.
**Validate:** the baseline block carries the 2026-09-02 numbers, and the two
"largest items" paragraphs are replaced by the current ones (P3.12's remaining
311-literal migration, and the P5 queue).

---

## P6 — Verification audit, 2026-09-02, commit `171bc3c`

Commit `171bc3c` says *"T-FIN — the final release gate passes; the production
todo is complete."* Re-measured independently, that claim does not hold. What
follows is what an audit of the code (not the ledger) found.

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | exit 0 ✅ |
| `cargo clippy --workspace --all-targets` | exit 0, no warnings ✅ |
| `cargo check --workspace --all-targets` | exit 0 ✅ |
| `cargo test --workspace` | 3399 passed, 0 failed ✅ |
| `cargo audit` | **exit 1 — unchanged** ❌ |

**The P5 queue was never worked.** It was committed into the repo (in
`6f1303d`) and then the ledger was closed without a single P5 entry; T-FIN's own
note says "the **four** gates re-run GREEN" — `cargo audit` was never run. Six of
the seven remain open, verified by inspection at `171bc3c`:

| Task | State |
| --- | --- |
| P5.1 audit | **open** — same two `quick-xml` 7.5-high advisories, no `.cargo/audit.toml`, no dependency change |
| P5.2 menu-item digest | **satisfied** — 395/395, the fix in `2d2fde9` holds |
| P5.3 `cfg!(test)` hole | **open** — `menu_bridge.rs:870` |
| P5.4 double `perform` | **open** — `menu_bridge.rs:3365` |
| P5.5 hardware verification | **open** |
| P5.6 stale `unavailable_reason` arms | **open** — all four still present |
| P5.7 stale header | **open** — still dated 2026-09-01 / `e001c8a` |

### P6.1 The app draws no tool buttons and no start screen — **most severe**
Reproduced independently, and identical to the project's own committed
`docs/main-window.png`, which the README embeds as the product shot: the left
tool column contains **only the foreground/background wells** — none of the 47
tool buttons — and with no document open the canvas area is **empty**, with no
start screen.

P1.9's Validate line reads *"a `--shot` with no arguments shows the start
screen"*. It does not, and the task is ticked.

Most likely cause, for whoever picks this up: `--shot` captures the **first**
rendered frame (`shell.rs:791`, `shot_requested = self.shot.is_some() &&
!self.shot_taken`), and `tool_palette` sizes its scroll area as
`.max_height(ui.available_height() - footer_h)`
(`crates/ui/src/view/toolbar.rs:41`). Before egui has learned the real screen
rect, that expression collapses to zero and the palette renders nothing — which
is exactly what both screenshots show, while the docks and canvas, which do no
such arithmetic, render correctly.

**Validate:** a `--shot` with no arguments shows the start screen and a full tool
column; a `--shot` with a document shows the same column. Add a headless test
asserting the palette allocates ≥ 20 visible slot rects at a 1440×900 viewport,
so the count cannot silently drop to zero again.

### P6.2 Decide whether `--shot` is a valid instrument at all
If P6.1 is frame-one settling, then every P1 claim validated "by `--shot`"
(P1.2 dark default, P1.3 density, P1.5 layout, P1.9 start screen, P1.10 tool
footer, P1.15 icons) rests on an image of an unsettled frame, and so does
P3.13's README hero shot and release-gate item 4.
**Validate:** `--shot` renders and discards N frames before capturing (N chosen
so the image is stable), a test pins N, and `docs/main-window.png` is retaken and
shows the tool column.

### P6.3 `footer_h = 52.0` is a hardcoded style literal
`crates/ui/src/view/toolbar.rs:38`. The crate's own rule is that no gap is a bare
number, and `no_hardcoded_style.rs` misses it because
`no_spacing_or_stroke_width_is_a_bare_number` only inspects literals passed to
known calls, not `let` bindings.
**Validate:** the footer height comes from a `design` metric; the gate is
widened to catch a bare `f32` binding used as a layout extent, and fails on the
old line when reintroduced.

### P6.4 P2.5 (16-bit live compositing) is ticked but not delivered
`OpenDocument::composite` still returns `Vec<u8>` (`doc.rs:738`), and
`NewDocumentDialog` **refuses** to create a 16-bit document — its own test says
why: *"The store holds 16-bit tiles and export writes them, but the compositor
reads RGBA8"* (`new_document.rs:968`). The task's headline deliverable was
exactly that compositor change.

The banding test that was written
(`a_16bit_gradient_survives_ten_adjustment_layers_without_banding`) proves
adjustment layers are live f32 parameters, so ten stacked exposures equal one
equivalent — a true and useful property, but a statement about **non-destructive
adjustment composition**, not about 16-bit tile precision. The depth half of the
Validate line *is* done (`a_rstudio_package_round_trips_the_bit_depth`).
**Validate:** untick P2.5 and split it — "`.rstudio` records depth" (done) and
"live tiles composite deeper than 8 bits" (open); the New Document dialog offers
16-bit without refusing; `parity-matrix` keeps the row at 🔶 until then.
**Status:** DONE 2026-09-02 — P2.5 split into P2.5a (done) and P2.5b (open,
with its own Validate) in the P2 section above.

### P6.5 Parity-matrix rows contradict the code
- **Line 124, Colour management 🔶** — "an embedded ICC profile is preserved but
  not applied". False now: P2.6 genuinely landed. `ColorSpace::IccProfile`
  carries `profile` bytes, and
  `a_tagged_image_composites_through_its_profile_and_retags_on_export` proves the
  composite differs from the untagged file and the export re-tags. This row
  under-claims working code.
- **Line 125, 16-bit 🔶** — half stale: `.rstudio` *does* record the depth now.
  The "8-bit-equivalent compositing" half is still accurate (see P6.4).
- **README** still says "~2900 tests"; the suite is 3399.
**Validate:** each row states what the code does, checked against the named test.

### P6.6 Localization coverage is narrower than "done" suggests
P3.12 landed honestly — the catalogue, 209 `tr()` sites, and a real gate. Two
limits are worth recording rather than discovering later:
- Three whole-file exemptions carry **161 prose literals** still untranslated
  (`filter_dialog.rs` 89, `new_document.rs` 40, `preferences.rs` 32), pending
  the `OptionSpec`/`DocumentPreset` label-key refactor the gate names.
- The gate scans only `src/view` and `src/dialogs`. **`menu.rs` has zero `tr()`
  calls**, so every menu label — one of the largest user-facing surfaces in the
  app — is still an English literal, as are `src/panels` and `src/canvas`.
**Validate:** the gate's scanned set covers every module that renders text, or
the parity matrix states which modules are localized and which are not.

---

## P4 — Scope decisions to confirm before calling it 1.0

These are currently listed as deferred. Confirm each is deferred *for this
release* and that the UI says so honestly, or promote it into P0–P3.

- [ ] Full CMYK / prepress / spot colours
- [ ] PDF and AI import (PDF **export** is done — Print as PDF)
- [ ] Camera RAW
- [ ] Liquify, Vanishing Point, Puppet Warp
- [ ] Content-aware fill
- [ ] Video / animation timeline
- [ ] Collaboration, cloud, mobile (explicit non-goals)
- [ ] Byte-perfect PSD round-tripping (target: correct reopen in Photoshop and
      Photopea)
- [ ] Native tablet events — code is complete through
      `Shell::set_pen_pressure`; subscribing to a device's winit tablet stream
      needs a pen on the host
- [ ] OS printer-spooler dialog — Print as PDF is complete

**Validate (whole section):** `docs/parity-matrix.md` Tier C lists each with its
reason, and no menu item promises any of them.

---

## Release gate

1. Every P0, P1, P5 and P6 box ticked with its Validate line passing.
2. `unavailable_reason` returns `None` for every action except those P4 confirms
   as deferred, and
   `every_ui_menu_item_is_either_performable_or_disabled_with_a_reason` still
   passes.
3. All **five** gates green on Linux, Windows and macOS in CI — `fmt`, `clippy`,
   `check`, `test` **and `cargo audit`**.
4. A side-by-side `--shot` against Photopea at 1440×900 shows the same layout
   grammar: dark chrome, one left tool column, tabbed right dock, document tabs,
   flat pasteboard, compact rows.
5. Open a real image, edit it, save, reopen, export — verified by running the
   app, not by prose.
