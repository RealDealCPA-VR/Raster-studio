# Corrections TODO — open items from the production audit

**What this is.** Every item still open against
[`PRODUCTION-TODO.md`](PRODUCTION-TODO.md), pulled into one ordered, directly
implementable list. This is a *subset* of that file, not a replacement: the P5
and P6 sections there carry the audit evidence, this file carries the work.

**Audited at** commit `171bc3c`, 2026-09-02. That commit says *"the production
todo is complete"*; it is not. Four gates are green (3399 tests, clippy, fmt,
check), the fifth (`cargo audit`) fails, the P5 queue was never worked, and the
app's own committed screenshot shows a window with no tool buttons.

---

## How to work this file

Run every task as **plan → implement → validate**. A task is done when its
**Validate** line passes *and* all five gates are green:

```bash
cd raster-studio
cargo fmt --check
cargo clippy --workspace --all-targets     # CI uses RUSTFLAGS=-D warnings
cargo check --workspace --all-targets
cargo test  --workspace
cargo audit                                # the fifth gate — it fails on its own
```

Two project rules apply to every item here, and the audit exists because they
slipped:

1. **A test that passes against the unfixed code is not a test.** Break the fix
   and watch the test go red before believing it.
2. **Do not tick a box on the strength of a passing headless test when the task's
   Validate line names something visible.** C2 and C3 are ticked tasks whose
   Validate lines were never actually checked against the running app.

Order matters where stated. **C1 gates C2 and C3** — do it first, because it may
resolve both.

---

## Group A — Release blockers

### C1 — Decide whether `--shot` captures a usable frame — **DONE 2026-09-02**
**Why.** `--shot` grabs the **first** rendered frame: `shell.rs:791` sets
`shot_requested = self.shot.is_some() && !self.shot_taken`, and `shell.rs:980`
captures on that frame. egui learns layout over several frames — the project
already knows this, and `dialog_host`'s tests run three passes for exactly that
reason. Every P1 visual claim (P1.2 dark default, P1.3 density, P1.5 layout,
P1.9 start screen, P1.10 tool footer, P1.15 icons), the README hero image
(P3.13) and release-gate item 4 all rest on this instrument.

**Where.** `crates/app-shell/src/shell.rs:791`, `:980`.

**Do.** Render and discard N frames before capturing; pick N as the smallest
value at which two consecutive captures are byte-identical. Keep the process
exiting after the shot.

**Validate.** `--shot` run twice on the same input produces byte-identical PNGs;
a unit test pins N with a comment saying what it is for; the captured image shows
the tool column populated (which is C2's evidence) and, with no document, the
start screen (C3's).

**Validate.** `--shot` run twice on the same input produces byte-identical PNGs;
a unit test pins N with a comment saying what it is for; the captured image shows
the tool column populated (which is C2's evidence) and, with no document, the
start screen (C3's).

**Done 2026-09-02.** `shell.rs` gained `SHOT_WARMUP_FRAMES` (24) + a
`shot_frames` counter: `--shot` drives 24 warm-up frames (request_redraw +
`ControlFlow::Poll`) before the capture frame, then exits as before. N=24 is
the smallest value where two consecutive captures are byte-identical (N=3
still differed by 1563 bytes — egui's fade animation had not converged).
Validate: two runs byte-identical (sha 9db2b1b1503e); the shot shows the
populated column and the start screen. The Note's second branch held: the
instrument was innocent, and C2/C3 were genuine.
**Note.** If N frames does *not* populate the tool column and start screen, the
instrument was innocent and C2/C3 are genuine layout bugs — implement them as
written below.

---

### C2 — The tool column renders no tool buttons — **DONE 2026-09-02**
**Why.** In a live `--shot`, the left column contains **only the
foreground/background wells** — none of the 47 tools. Reproduced independently
and pixel-identical to the project's own committed `docs/main-window.png`, which
the README embeds as the product shot. A published screenshot of an image editor
with no toolbar is a release blocker on sight.

**Where.** `crates/ui/src/view/toolbar.rs:38-41`:

```rust
let footer_h = 52.0;
egui::ScrollArea::vertical()
    .auto_shrink([false, true])
    .max_height(ui.available_height() - footer_h)
```

**Do.** The footer is drawn *after* the scroll area in the flow, and in the
screenshot the wells sit at the **top** of the column — so the scroll area
resolved to roughly zero height. `ui.available_height()` is the suspect: before
egui knows the real screen rect, `available_height() - 52.0` clamps to zero and
the palette draws nothing. Make the palette's height independent of a
first-frame measurement (reserve the footer with a bottom-up layout, or clamp the
subtraction at a floor that still shows rows).

**Validate.** A headless test at a 1440×900 viewport asserts the palette
allocates **≥ 20 visible slot rects** with non-zero area, so the count can never
silently fall to zero again; and a `--shot` (after C1) shows a populated column.
Break it by restoring the old expression and watch the test go red.

**Done 2026-09-02 (with a deeper root cause).** Two defects were found:
(1) exactly the suspect first-frame clamp (`available_height() - 52` resolving
to ~zero — the fix direction named here); and (2) — the load-bearing one — the
palette's `ScrollArea` batch itself never rasterized in this renderer setup:
the tessellated mesh reached the GPU correct in every probe (clip
`[[0,77]-[40,819]]`, 1264 verts, 1674 tris, bounds `[[6.5,78.5]-[33.5,760.5]]`,
466 opaque `#C0C0C0` icon vertices, all 23 slots `is_rect_visible`), yet the
presented surface showed ink only for the first two slots. Removing the
`ScrollArea` (the registry's eight groups fit ~700 px at the design height,
so the column no longer scrolls) made every slot paint. `toolbar.rs` now lays
the palette out flat with the finding documented in place; the
`available_height` clamp became moot with the scroll gone. Validate: headless
`the_palette_shows_its_tool_icons_across_the_warmup_frames` (1440×900, ≥ 20
icon shapes — red with the old zero-height expression, re-verified); the
post-C1 `--shot` shows ink in all 20 sampled column bands (was 2).
---

### C3 — The start screen never appears — **DONE 2026-09-02**
**Why.** P1.9's Validate line reads *"a `--shot` with no arguments shows the
start screen"*. It does not — the canvas area is empty — and the task is ticked.
The code is present and called (`chrome.rs:1309`, called from `chrome.rs:666`),
so this is a drawing/layout failure, not a missing feature.

**Where.** `crates/app-shell/src/chrome.rs:1309` — an `egui::Area` anchored
`Align2::CENTER_CENTER`. An Area anchored to the centre of a screen rect that is
still the default lands off-screen or at zero size, which is the same first-frame
story as C2 and is why C1 comes first.

**Do.** Confirm against C1's result. If the Area still does not paint after N
frames, anchor it to the measured central-panel rect rather than the context
centre.

**Validate.** A headless test asserts the "Raster Studio" title and the New and
Open buttons are painted when `editor.documents()` is empty, and are absent when
a document is open; a no-argument `--shot` shows them.

**Done 2026-09-02.** The Area was never the bug: with C1's warm-up frames it
anchors `CENTER_CENTER` correctly against a correct `ctx.screen_rect()`
(probed live: `[[0,0]-[1440,900]]`, content `min_rect` `[[580,380]-[860,521]]`).
What hid it was C2's poisoned renderer batch — the palette `ScrollArea`'s
batch failed to rasterize and took the start screen's (and the menu bar's)
pixels with it. With the flat palette the title, New and Open buttons paint in
the `--shot` (551 bright ink px in the start-screen rect). Validate: NEW
headless `the_start_screen_title_and_buttons_are_painted_only_when_empty`
walks `FullOutput::shapes` for the galley strings (painted, not merely laid
out) when `documents()` is empty and asserts absence with a document open;
break-the-fix verified by inverting the empty guard (test went red). The
no-argument `--shot` shows them.
---

### C4 — `cargo audit` fails (the fifth gate) — **DONE 2026-09-02**
**Why.** `cargo audit` exits 1. The two unmaintained-crate notices at the tail of
the log (`paste` RUSTSEC-2024-0436, `ttf-parser` RUSTSEC-2026-0192) are **not**
the cause — the run reports `warning: 2 allowed warnings found` for exactly
those. The failing line is `error: 2 vulnerabilities found!`, both in
`quick-xml 0.30.0`:

| ID | Severity | Fix |
| --- | --- | --- |
| RUSTSEC-2026-0194 — quadratic run time checking duplicate attribute names | 7.5 high | ≥ 0.41.0 |
| RUSTSEC-2026-0195 — unbounded namespace allocation, memory-exhaustion DoS | 7.5 high | ≥ 0.41.0 |

Provenance — `cargo tree -i quick-xml@0.30.0 --target all`:

```
quick-xml 0.30.0 ← zbus_xml 4.0.0 ← zbus-lockstep 0.4.4 ← atspi 0.22.0
                 ← accesskit_unix 0.12.3 ← accesskit_winit 0.22.4 ← app-shell
```

The chain entered the lockfile with **P3.11 (AccessKit, commit `7a3ee3f`)** and
is Linux-only, which is why the ubuntu `audit` job is the one that breaks. The
same commit brought the `paste` notice in via `accesskit_windows`. `quick-xml`
cannot be bumped directly: `zbus_xml 4.0` pins `^0.30`.

**Do.** Choose one and record the choice with its reason:

- **(a)** Raise `accesskit_winit` past the `atspi`/`zbus` generation carrying
  `quick-xml 0.30`. Blocked today by `egui-winit 0.29`, which pins
  `accesskit_winit 0.22`; a mismatched bump already produced two copies of the
  crate once (see the ledger). Realistically arrives with an egui upgrade.
- **(b)** `accesskit_winit = { version = "0.22", default-features = false }` —
  drops the AT-SPI backend and the whole advisory chain. Costs Linux
  screen-reader support, which contradicts P3.11; say so in the parity matrix.
- **(c)** A `.cargo/audit.toml` ignore with a named expiry and a tracking issue.
  A suppression, not a fix, and the weakest option for a project whose first rule
  is that documentation is a claim.

**Validate.** `cargo audit` exits 0 from `raster-studio/`; the ubuntu `audit` CI
job is green; the choice is named in `docs/threat-model.md` with its trade-off,
and if it is (c) the ignore carries an expiry date.

**Done 2026-09-02 — choice (c), documented.** The chain cannot be bumped:
`zbus_xml 4.0` pins `quick-xml ^0.30`, and `accesskit_winit` cannot rise past
`egui-winit 0.29`'s pin (a mismatched bump already duplicated the crate once).
Dropping the AT-SPI backend (option (b)) was rejected: it trades a Linux-only
parsing advisory for losing Linux screen-reader support, contradicting P3.11.
`.cargo/audit.toml` ignores RUSTSEC-2026-0194/0195 with the reason and an
expiry (2027-03-01, lifted by the egui upgrade); `docs/threat-model.md`
§8 records the choice and its trade-off. Validate: `cargo audit` exits 0 from
`raster-studio/` (remaining output: the two pre-allowed unmaintained
warnings); the ubuntu audit job runs the same command in the same directory
and reads the same config. On a Windows host the Linux job itself is verified
by configuration, not execution (C14's caveat).
---

## Group B — Test integrity

### C5 — Close the `cfg!(test)` hole around the Help URLs — **DONE 2026-09-02**
**Why.** `menu_bridge.rs:870` suppresses browser launches with `cfg!(test)`.
That is true only while compiling the crate *under test*. Every integration test
— `tests/integration/`, `crates/app-shell/tests/gpu.rs`, `crates/ui/tests/*` —
links `app-shell` as an ordinary dependency where it is **false**. Any of them
that reaches a Help action will really open three browser tabs on the CI runner
and bring back the flake that `2d2fde9` was written to kill. It also leaves the
shipped `webbrowser::open` path with **zero** test coverage.

**Where.** `crates/app-shell/src/menu_bridge.rs:865-877` (`open_help_url`). The
crate already has the right pattern: the `FileDialogs` / `ScriptedDialogs` seam.

**Do.** Route URL opening through an injected seam. The test double records the
URL instead of opening it; the shipped implementation calls `webbrowser::open`.

**Validate.** A test asserts the recorded URL for each of Help, Release Notes and
Report Issue; `grep -rn "cfg!(test)" crates/app-shell/src` returns nothing that
changes user-visible behaviour.

**Done 2026-09-02.** New seam in `dialogs.rs`: `UrlLauncher` with the shipped
`BrowserUrls` (`webbrowser::open`) and the `RecordingUrls` test double — the
same injected-seam shape as `FileDialogs`. `Editor` owns a launcher
(default `BrowserUrls`) behind `set_url_launcher`/`url_launcher_mut`;
`open_help_url` now asks the editor's launcher instead of branching on
`cfg!(test)`. Validate: `grep -rn "cfg!(test)" crates/app-shell/src` → 0
hits; NEW test `the_help_menu_opens_the_recorded_urls_through_the_injected_seam`
asserts all three recorded URLs (wiki/releases/issues/new) and the
"Opened {url}" statuses; the digest loop injects a recorder so no CI runner
ever opens a tab. Break-the-fix: bypassing the launcher (the old suppression's
observable behaviour) leaves the recorder empty and the test red.
---

### C6 — Stop the menu digest invoking `perform` twice — **DONE 2026-09-02**
**Why.** `menu_bridge.rs:3365` builds the expected status by calling
`perform(action, &mut ed)` a **second** time:

```rust
assert_eq!(
    ed.status().map(str::to_string),
    Some(perform(action, &mut ed).unwrap()),
    "{action:?} did not reach the status bar"
);
```

For any action with a side effect that is a double execution, and it is what
turned an environment-dependent message into the hard CI failure. C5 hides the
symptom for Help; the pattern still applies to every `INFORMATIONAL` action.

**Where.** `crates/app-shell/src/menu_bridge.rs:3357-3368`.

**Do.** Capture the first call's `Ok(message)` and assert the status against that
captured value.

**Validate.** The `INFORMATIONAL` branch calls `perform` exactly once per action;
the test still fails when an informational action does not reach the status bar
(prove it by breaking one).

---

## Group C — Correctness of claims

### C7 — Delete the four stale `unavailable_reason` arms — **DONE 2026-09-02**
**Why.** `resolve` consults `unavailable_reason` only when `pick` returns `None`,
so these arms are dead text that reads as a live product limitation:

| Line | Arm | Why it is stale |
| --- | --- | --- |
| `menu_bridge.rs:458` | `PlaceEmbedded \| PlaceLinked` | says "the canvas has no gizmo overlay"; the gizmo landed in P2.1 and both route to `editor.place_from_dialog` (P2.4) |
| `menu_bridge.rs:496` | `SetColorMode(_)` | says `editor_core` has no command for it; P2.9 added one |
| `menu_bridge.rs:510` | `SelectSubject` | the item was removed from `select_menu()` by P2.12, so the arm is unreachable |
| `menu_bridge.rs:462` | `FileInfo` | **keep** — deliberately retained to feed the unrouted-message path (`shell_action` routes it and it performs). Reword so it reads as that, not as a disabled item. |

**Validate.** Each removed arm's action resolves to `Ok(_)` in a populated
context; `no_menu_item_falls_back_to_the_generic_refusal` still passes; the
arms that remain are exactly the ones a user can still hit.

---

### C8 — Untick and split P2.5 (16-bit live compositing) — **DONE 2026-09-02**
**Why.** The task's headline deliverable was deeper live compositing. It was not
implemented. `OpenDocument::composite` still returns `Vec<u8>`
(`crates/app-shell/src/doc.rs:738`), and `NewDocumentDialog` now **refuses** to
create a 16-bit document — its own test says why:

> *"The store holds 16-bit tiles and export writes them, but the compositor reads
> RGBA8 — so the dialog refuses the depth with a reason rather than confirming a
> document that would draw as garbage."* — `crates/ui/src/dialogs/new_document.rs:968`

The banding test that was written
(`a_16bit_gradient_survives_ten_adjustment_layers_without_banding`,
`editor_tests.rs:146`) proves adjustment layers are live f32 parameters, so ten
stacked exposures equal one equivalent. That is true and worth keeping, but it is
a statement about **non-destructive adjustment composition**, not about 16-bit
tile precision. The depth half of the Validate line *is* done
(`a_rstudio_package_round_trips_the_bit_depth`).

**Do.** In `PRODUCTION-TODO.md`, untick P2.5 and split it:
- **P2.5a `.rstudio` records the source depth** — done, keep ticked.
- **P2.5b live tiles composite deeper than 8 bits** — open.

**Validate.** P2.5b's own Validate: `composite` returns a deep buffer (or a
depth-parameterised one); `NewDocumentDialog` offers 16-bit without refusing, and
its refusal test is replaced by one that creates and composites a 16-bit
document; the parity-matrix row stays 🔶 until then.

**Done 2026-09-02.** PRODUCTION-TODO's P2.5 split into **P2.5a** (`.rstudio`
records the source depth — done, Validate ✅) and **P2.5b** (live tiles
composite deeper than 8 bits — OPEN, with the full Validate: a deep-buffer
composite, a non-banding 16-bit gradient, the New Document dialog offering
16-bit without refusing, the refusal test replaced). P6.4 marked as resolved
by the split. No code changed: this item is a documentation-truth fix; the
P2.5b work itself remains open by design.
---

### C9 — `footer_h = 52.0` is a hardcoded style literal the gate misses — **DONE 2026-09-02**
**Why.** `crates/ui/src/view/toolbar.rs:38`. The crate's stated rule is that no
gap is a bare number — `no_hardcoded_style.rs` exists to enforce it — but
`no_spacing_or_stroke_width_is_a_bare_number` only inspects literals passed to
known calls, so a `let` binding used as a layout extent slips through.

**Do.** Take the footer height from a `design` metric, and widen the gate.

**Validate.** The gate fails on the old `let footer_h = 52.0;` line when
reintroduced (add it to `the_gate_actually_catches_something`'s fixtures), and
passes on the token-based replacement.

---

## Group D — Documentation truth

The project's own contributing rule is *"do not write prose asserting behaviour
the code does not have."* These three items are that rule applied in both
directions — the docs currently both over-claim and under-claim.

### C10 — Two parity-matrix rows contradict the code — **DONE 2026-09-02**
**Where.** `docs/parity-matrix.md`.

- **Line 124, Colour management 🔶** — "an embedded ICC profile is preserved but
  not applied". **False, and it under-claims working code.** P2.6 genuinely
  landed: `ColorSpace::IccProfile` carries `profile` bytes, and
  `a_tagged_image_composites_through_its_profile_and_retags_on_export`
  (`editor_tests.rs:309`) proves the composite differs from the untagged file and
  that export re-tags.
- **Line 125, 16-bit 🔶** — half stale: `.rstudio` *does* record the depth now.
  The "8-bit-equivalent compositing" half remains accurate (see C8).

**Validate.** Each row states what the code does, and names the test that proves
it.

**Done 2026-09-02.** Colour management flipped to ✅ with the proving test
named; the 16-bit row rewritten to the true state (`.rstudio` records the
depth, live compositing still 8-bit-equivalent, P2.5b named). While there, the
matrix's "Known gaps" section was brought to the same truth: the
linked-smart-objects and ICC bullets (both delivered in P2.4/P2.6) removed,
the 8-bit bullet corrected (`.rstudio` records depth), the stale
disabled-menu-items bullet replaced with the post-C7 reality, and the
per-channel bullet rewritten after verifying the code — adjustments ride
`mask_paint_to_channel` like filters do (the original bullet's
"adjustment application not yet masked" was false), so the honest remainder
is alpha/mask targets not being isolatable plus no per-channel histogram.
---

### C11 — The README's "still remaining" list is badly out of date — **DONE 2026-09-02**
**Why.** `README.md:86-114` still tells readers that Place Embedded/Linked, the
interactive transforms, Image/Canvas Size, arbitrary rotation, Offset, the Custom
filter, the Filter gallery, Pattern fill and Define Pattern/Brush are all
disabled, and that ICC is "not applied to the working space". Every one of those
landed in P0/P2. The README under-sells the product substantially. The build
line also says `# ~2900 tests`; the suite is **3399**.

**Do.** Rewrite that section against the current `unavailable_reason` (which
after C7 lists only `ApplyAdjustment`-at-identity and the P4 deferrals), and fix
the test count. Retake the hero image after C1/C2/C3.

**Validate.** Every capability the section names as remaining still has a live
`unavailable_reason` arm or a P4 Tier C row; the test count matches
`cargo test --workspace`; the embedded screenshot shows a populated tool column.

**Done 2026-09-02.** The "still remaining" section rewritten against the
current code: the linked-smart-objects and ICC bullets removed (both landed),
the 8-bit bullet kept with the P2.5b reference, the disabled-menu-items bullet
replaced by the Tier-C deferrals plus the post-C7 refusal reality, the test
count corrected to ~3400 (measured 3402), and the hero image retaken with the
C1/C2-fixed build (populated column, document on the pasteboard, docks, status
bar). Validate: every named remaining item maps to a live refusal arm or a
Tier C row; the count matches `cargo test --workspace`; the screenshot shows a
populated tool column.
---

### C12 — Refresh the `PRODUCTION-TODO.md` header — **DONE 2026-09-02**
**Why.** Its "Verified baseline" block is still dated 2026-09-01 / `e001c8a`,
and the two numbered findings under it still say the dialogs have zero call sites
and the canvas has no gizmo. Both were closed by P0.1–P0.16 and P2.1. An agent
reading top-down is told the opposite of what the code does.

**Validate.** The baseline block carries current numbers and commit, and the two
"largest items" paragraphs are replaced by the live P5/P6 queue.

**Done 2026-09-02.** The baseline block re-measured (3402 tests, audit exit 0
with the two named expiring ignores, the C1 warm-up noted), the two stale
"largest items" paragraphs (zero dialog call sites; no gizmo overlay) replaced
by the closed-gaps statement and the live open queue (P2.5b, P5, P6).
---

## Group E — Coverage and verification debt

### C13 — Localization coverage is narrower than "done" implies — **DONE 2026-09-02 (scope stated)**
**Why.** P3.12 landed honestly — a catalogue, 209 `tr()` sites and a real gate —
but two limits should be recorded rather than rediscovered:

- Three whole-file exemptions in
  `crates/ui/tests/no_localized_literals.rs:30-46` still carry **161 prose
  literals**: `filter_dialog.rs` 89, `new_document.rs` 40, `preferences.rs` 32.
  They await the `tools::OptionSpec` / `DocumentPreset` label-key refactor the
  gate's own comment names (the gradient editor's `name_key` is the pattern).
- The gate scans only `src/view` and `src/dialogs`. **`crates/ui/src/menu.rs`
  has zero `tr()` calls**, so every menu label — one of the largest user-facing
  text surfaces in the app — is still an English literal, as are `src/panels`
  and `src/canvas`.

**Do.** Either widen the gate's `SCANNED` set and migrate, or state the scope
explicitly in the parity matrix. Do not leave "localization: done" standing
against a menu bar that cannot translate.

**Validate.** The gate covers every module that renders text, **or** the parity
matrix names exactly which modules are localized and which are not, and the
161-literal exemption count is stated with the refactor that will clear it.

**Done 2026-09-02 — the documentation path.** The parity matrix gained a
Localization 🔶 row that states the scope exactly: the catalogue and its 209
`tr()` sites cover `src/view` + `src/dialogs` under the gate; `src/menu.rs`
(zero `tr()` calls — every menu label), `src/panels` and `src/canvas` are not
localized; the three whole-file exemptions carry the 161 prose literals (the
counts as measured by the audit and restated by P6.6); and the refactor that
clears them is the `tools::OptionSpec`/`DocumentPreset` label-key change the
gate's own comment names. Widening the gate and migrating menu.rs remains the
real fix and stays open in P6.6; this item closes the false "done" impression
the validate asks for.
---

### C14 — Verify the three host-limited tasks on real hardware
**Why.** P3.4 (macOS/Linux packaging), P3.6 (signing/notarisation) and P3.11
(AccessKit) are ticked with the honest caveat that a Windows host could not
finish them. Configs and commands exist; nothing has been built, signed or heard.

**Verified 2026-09-02, as far as one Windows host allows — with two real
defects found and fixed.** The screen-reader half is no longer a claim about
wiring: the running app was queried live through UI Automation (the API NVDA
consumes on Windows). The first probe saw **0 elements** — the AT client
received nothing but the bare window. Root causes, both fixed:

1. `egui-winit 0.29` never calls `Context::enable_accesskit()` when the
   adapter requests the initial tree, so egui kept emitting an empty tree
   update. `shell.rs` now enables it on `InitialTreeRequested`.
2. Enabling only flips a flag — the tree rides the next egui pass, and an
   idle document window never repaints on its own. The handler now
   `request_redraw()`s.

With those fixed (plus explicit `WidgetInfo::labeled` on the hand-painted
controls — tool slots, layer rows, start-screen buttons), the live walk sees
**71 UIA elements / 43 names** on the start screen and **149 elements / 52
names** with a document: every tool slot by name ("Brush Tool (B); Click
again, or right-click, for more tools", "Rectangular Marquee (M)", …), the
nine menus, the Layers panel's controls (Lock, Mask, Clip to layer below,
Blend, Opacity, Kind, Snapshot…), the layer row itself, the tab title and the
document dimensions. A screen reader now has real text to read where before
it had a skeleton of unnamed nodes.

Still requiring machines this host does not have (the box therefore stays
unticked):

- **The `.deb` chain is now verified end-to-end on clean Debian** —
  2026-09-02, in a `rust:1-bookworm` container (the cleanest Linux this host
  can produce): a fresh `cargo build --release -p studio-desktop` compiled,
  `build-deb.sh` packaged `raster-studio_0.1.0_amd64.deb`, `dpkg -i`
  installed it to `/usr/bin/raster-studio`, and the **installed** binary (not
  the build-tree one) launched under Xvfb + lavapipe and wrote a
  `--shot` — `docs/linux-shot.png` — whose pixels match the Windows shots
  (20/20 tool-column bands, the start screen present). **The verification
  caught a real release-blocking bug**: both packaging scripts
  `cd "$(dirname "$0")/../../.."`, which lands in `apps/` — one level short
  of the workspace root — so the scripts had never been executable as
  written (only `bash -n`-checked). Fixed to four levels up in both
  scripts; the Linux script now runs to completion.
- A `.dmg`/`.app` build and `spctl --assess` need macOS tooling
  (`hdiutil`/`codesign`); the bundle script's path bug was fixed alongside
  the `.deb` one, but executing it still needs a Mac.
- Authenticode signing with a **trusted** certificate and `spctl --assess`
  need the signing identities. The CI release job carries the hooks behind
  `WINDOWS_CERT`/`APPLE_ID` and says in its summary when they are absent.
  This host additionally could not produce even a self-signed check under
  Windows PowerShell 5.1 (its certificate-store tooling is broken — the
  `Cert:` drive does not load), but **PowerShell 7 on the same machine
  worked**: a self-signed CodeSigning certificate was created, `signtool
  sign` signed the release binary (`Number of files successfully Signed: 1`,
  0 errors), and `Get-AuthenticodeSignature` read the signature back with
  the expected signer subject. The pipeline itself is proven; only the
  trusted identity is missing.
- A human listening to NVDA/VoiceOver/Orca read the palette and Layers panel
  aloud. The UIA walk above is the machine-verifiable part of that claim:
  the names a screen reader would speak are now present and enumerated.
  A real NVDA (2025.1.2, portable) was downloaded, installed and launched
  against the running app in pursuit of this — the reader ran, but its
  speech-viewer window did not open from a pre-seeded config, and pursuing
  it further meant driving a talking screen reader over the user's live
  desktop session; the attempt was stopped and fully cleaned up (portable
  copy and installer deleted, no NVDA processes left). What a screen reader
  would read remains proven by the UIA enumeration; the *listening* stays a
  human act.

**Validate.** A `.dmg`/`.app` and a `.deb` or AppImage are built and launch on a
clean machine; the Windows installer is Authenticode-signed and the macOS bundle
passes `spctl --assess`; one screen reader (NVDA, VoiceOver or Orca) reads the
tool palette and the Layers panel aloud, recorded in the ledger.

---

## Completion checklist

| ID | Item | Group | Blocks release |
| --- | --- | --- | --- |
| [x] C1 | `--shot` frame-one capture | A | yes |
| [x] C2 | Tool column renders no buttons | A | yes |
| [x] C3 | Start screen never appears | A | yes |
| [x] C4 | `cargo audit` fails | A | yes |
| [x] C5 | `cfg!(test)` hole | B | no |
| [x] C6 | Double `perform` in the digest | B | no |
| [x] C7 | Stale `unavailable_reason` arms | C | no |
| [x] C8 | Untick and split P2.5 | C | yes — it is a false claim |
| [x] C9 | `footer_h` literal + gate | C | no |
| [x] C10 | Parity-matrix rows | D | yes — it is a false claim |
| [x] C11 | README remaining-list + test count | D | yes — it is a false claim |
| [x] C12 | `PRODUCTION-TODO.md` header | D | no |
| [x] C13 | Localization coverage | E | no |
| [ ] C14 | Hardware verification | E | yes |

C14 **remains open on one narrow point** — launching the macOS bundle on a
Mac (`hdiutil`/`codesign`/`spctl`), signing with a trusted identity, and a
human listening to the reader. Everything else was verified **live on this
host** (2026-09-02):

- **Linux, end-to-end on clean Debian** (rust:1-bookworm container): fresh
  release build → `build-deb.sh` → `dpkg -i` → the installed
  `/usr/bin/raster-studio` launched under Xvfb + lavapipe and rendered a
  `--shot` (`docs/linux-shot.png`) pixel-matching the Windows shots. The
  verification **caught a real release-blocking bug**: both packaging scripts
  `cd` one level short of the workspace root and had never been executable
  as written — fixed in both (Linux verified by running it; macOS fixed,
  execution awaits a Mac).
- **Authenticode pipeline**: PowerShell 7 created a self-signed CodeSigning
  cert, `signtool sign` signed the release binary (1 file, 0 errors), and
  `Get-AuthenticodeSignature` read the signature back. Only the trusted CA
  identity remains (CI secrets).
- **Screen-reader surface**: UIA walk — 72/44 elements/names on the start
  screen, 149/52 with a document; every keyed tool by name, the nine menus,
  the Layers panel controls and layer rows. Got there by fixing two real
  defects: egui-winit 0.29 never calls `Context::enable_accesskit` (the tree
  stayed empty) and nothing repainted after enabling so an idle window never
  shipped the tree; the hand-painted controls gained explicit
  `WidgetInfo::labeled` names. The localization gate caught a separator
  literal in the first labelling attempt — the label is the tooltip's first
  line, which reads better aloud anyway. A real NVDA portable was installed
  and launched to close the loop; driving a talking reader over the user's
  live desktop was judged too intrusive, so the attempt was recorded and
  fully rolled back — the listening remains a human act.

Until a Mac launch and a human reader happen, this box stays honestly
unticked.

**Release gate for this file:** every box ticked with its Validate line passing,
all five gates green on Linux, Windows and macOS in CI, and a fresh `--shot`
placed side by side with Photopea showing the same layout grammar — dark chrome,
one populated left tool column, tabbed right dock, document tabs, flat
pasteboard, compact rows.
