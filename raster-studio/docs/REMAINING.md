# Raster Studio — Remaining Work Spec

This is the working spec for what is still required to finish the project. It is
re-derived from the code on `main` (post wave-8) and from
[`parity-matrix.md`](parity-matrix.md), and it is kept honest per the project's
own rule: documentation is a claim, and claims get checked against the code.

Status: ⬜ not started · 🔶 in progress · ✅ done (compiles, tested, reviewed,
and the release gates stay green).

Every task must leave `cargo check --workspace --all-targets`, `clippy -D
warnings`, `fmt --check` and `cargo test --workspace` green.

---

## S0 — Documentation honesty (blocking)

wave-8 (commit `17410d3`) wired the Filter menu, PSD export *and* import,
text/pen/crop/transform/selection tools, layer-effects rendering and the
marching-ants overlay into the live application. The README's **Status** section
and `docs/parity-matrix.md` were written for the previous state and now
*under-claim* the code: they say the Filter menu is drawn but unwired, there is
no type tool and no pen tool, PSD is not linked into the binary, crop/free
transform "track a gesture but nothing applies it", a selection "draws no
marching ants", and layer effects "do not render". All of that is false on
`main` today.

- [x] S0.1 Rewrite README **Status** so "What you can do in the app" matches the
      wired reality, and move the newly reachable engines out of "not reachable".
- [x] S0.2 Update `docs/parity-matrix.md`: flip the 🔶 "reachability" rows that
      wave-8 closed (Filters, PSD read/write, Text, Vector/pen, Crop/slice/free
      transform, Selection overlay, Layer effects) and refresh the "Known gaps"
      list to name what is honestly left.
- [x] S0.3 Re-verify the four-file doc set (`architecture.md`, `render-pipeline.md`,
      `file-format.md`, `threat-model.md`) still describe the system that exists
      (fixed the stale "psd has no dependents / cannot open or save a PSD" claims
      in `architecture.md` and `threat-model.md`).

## S1 — Genuine remaining engine/UI gaps (from the matrix, Tier A first)

These are the rows that are honestly still 🔶 or ⬜ after wave-8, kept in
priority order. Each is a plan-implement-validate loop.

- [ ] S1.1 Channels can be isolated but not edited in place. Painting/filtering
      into a single channel is not implemented; every tool writes all components.
- [ ] S1.2 Smart objects exist as a layer kind but nothing renders them.
- [x] S1.3 Export is 8-bit; 16-bit sources are decoded, composited in f32 and
      written out at 8 bits/channel. `ExportFormat::supports_16_bit` exists but
      is not exercised by the export route.
      **Implemented.** A 16-bit source is recognized at open and exported at 16
      bits to the formats that carry them (`Canvas::to_rgba16`,
      `OpenDocument::composite_rgba16`, `export_to` branch) with a round-trip
      test; an 8-bit source keeps the byte-exact 8-bit path. Remaining: in-app
      tiles still composite at 8-bit-equivalent precision and `.rstudio` does
      not record the depth.
- [ ] S1.4 Tablet pressure: the engine consumes it but egui 0.29 carries none,
      so the shell must feed the native tablet stream.
- [ ] S1.5 Guides are view state, not saved/undoable (no command for them).
- [ ] S1.6 Layer and history thumbnails show glyphs, not pixels (no compositor
      pass per row / cache).
- [ ] S1.7 Embedded ICC profiles are preserved but not applied to a working
      space other than sRGB / Display P3.
- [x] S1.8 File Info (a metadata editor — `DocumentMeta` holds only a title and a
      size) and Print. Layer Via Cut/Layer Via Copy are already implemented; a
      `pick` that returns `None` for these makes the menu render them enabled
      while the shell reports them as unrouted, so each also needs the menu to
      show its reason.
      **Implemented (File Info).** File ▸ File Info… now opens a display-only
      metadata window: name, canvas size, colour space, origin path and source
      depth, hosted by the chrome like Preferences, closed by X, with
      `Action::ShowFileInfo` and a `Ctrl+Shift+I` binding. The menu item is
      genuinely performable; the two unrouted-reporting tests now use `Print`
      as their example instead. Remaining: Print needs an OS printing path.

## S2 — Ship polish (Tier B, lower priority)

- [x] S2.1 `cargo audit` clean on the pinned lockfile (CI already runs it).
      Ran `cargo audit` locally: 484 dependencies, zero known vulnerabilities
      (exit 0). Two unmaintained-crate notices (`paste`, `ttf-parser`) are
      warnings, not advisories, and do not fail the run.
- [x] S2.2 Windows installer + app icon + version stamping
      (apps/studio-desktop).
      Implemented: tools/make_icon.py generates assets/raster-studio.ico
      (valid ICO); an Inno Setup script (installer.iss) packages the binary,
      a Start-menu shortcut and an uninstaller using the icon; build.rs embeds
      a RASTER_VERSION_STAMP and main.rs's about() reports it with a test.
      Running iscc to emit an actual installer artifact needs the Inno Setup
      toolchain (not installed here) and is a release step, not code.
- [ ] S2.3 README with a real screenshot of the working app.

## Execution order

The plan-implement-validate loop runs each task in order: **plan** (read the
code, state the change), **implement** (make the edit), **validate** (run the
focused tests, then the full gate). Only when a task is green does the next
begin. Priority is S0 (docs must not lie) then the highest-ROI S1 items that are
headlessly testable.
