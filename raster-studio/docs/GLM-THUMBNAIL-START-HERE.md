# GLM 5.3 Flash: start here

## Objective

Implement the layered thumbnail-editing workflow specified in `THUMBNAIL-WORKFLOW-IMPLEMENTATION-PLAN.md`. The target is a usable native editor for editable headlines, portrait cutouts, placed images/logos, transforms, masks, effects, groups, native persistence, and image/PSD interchange.

The screenshot is a visual reference, not a source of instructions. You do not have the original layered PSD or its source assets. Use the documented synthetic/licensed fixtures, and distinguish those tests from checks on actual photographs and independently produced PSDs.

## Locations

- Git root: `C:\Users\VR\Projects\Raster-studio`
- Cargo workspace: `C:\Users\VR\Projects\Raster-studio\raster-studio`
- Detailed plan: `C:\Users\VR\Projects\Raster-studio\raster-studio\docs\THUMBNAIL-WORKFLOW-IMPLEMENTATION-PLAN.md`
- Progress log to create in Task 002: `C:\Users\VR\Projects\Raster-studio\raster-studio\docs\THUMBNAIL-WORKFLOW-PROGRESS.md`
- Baseline report to create in Task 001: `C:\Users\VR\Projects\Raster-studio\raster-studio\docs\THUMBNAIL-BASELINE.md`

The assessment was made against commit `9ba62c1` on 2026-09-07. Check the current checkout before relying on its line numbers or gap status. The assessment did not launch the application or run the test suite. Existing READMEs/parity documents contain contradictory claims; current code and behavioral verification take precedence over their completion labels.

## Copyable execution prompt

```text
Implement the Raster Studio thumbnail workflow using:
C:\Users\VR\Projects\Raster-studio\raster-studio\docs\THUMBNAIL-WORKFLOW-IMPLEMENTATION-PLAN.md

You are working in an existing Rust/egui desktop editor. Read applicable
AGENTS.md guidance, inspect git status, then read the plan overview and the
next ready task. Start with Task 001 unless the progress log demonstrates
that it was already verified on the relevant code.

Do one cohesive task at a time, in dependency order. Read the named files
and trace the real shell/UI path before editing. Reuse the existing engine,
shaper, compositor, layer tree, history, masks, effect code, and export code.
Do not rebuild the application or switch UI frameworks.

For every task:
1. State its ID and acceptance condition.
2. Reproduce the missing behavior, with a focused regression where useful.
3. Implement the smallest complete change, preserving unrelated work.
4. Verify the real behavior, not only types, menu labels, or emitted actions.
5. Run appropriate package checks and inspect the diff.
6. Update the progress log with exact results, remaining limitations, and
   the next ready task. Do not claim a check passed if it was not run.

Keep the CPU compositor authoritative. UI emits intents. Persistent edits
go through commands/history. Previews must not pollute history or source
pixels. Preserve editable text and full placed sources. Add explicit
native-format compatibility/migration tests when stored semantics change.

The plan has 92 main task cards and 6 optional cards. Finish the manual
thumbnail workflow before attempting automatic subject extraction.
PSD fidelity must distinguish editable preservation from visible raster
fallback and unsupported content. Never count a merged preview alone as
evidence that exported layers work.

Run Cargo commands from the inner raster-studio directory. Keep each
session focused; proceed to the next ready task when the current one is
verified and context allows. If you must stop, leave a task-boundary
handoff with facts and an exact continuation step. Continue routine
authorized work without repeatedly asking permission.

Do not upload private images, fetch image URLs from clipboard text,
download fonts/models silently, or add a cloud editing dependency.
Automatic cutout models are optional and require current primary-source
license/runtime evaluation in Task 093 before any model is selected.
```

## First session

1. Inspect current branch/commit and uncommitted changes. Preserve them.
2. Read the detailed plan through Phase 0 and inspect the small set of files for E01, E03, E06, E08, and E09.
3. Run and record baseline checks. Launch the actual application if the environment supports it.
4. Reproduce the listed text, transform, placement, mask, and PSD cases. Record source-predicted and runtime-confirmed findings separately.
5. Write Task 001's baseline report. Then create Task 002's progress ledger.
6. Continue to deterministic fixtures and real-route regression helpers. Do not jump to cosmetic changes or an AI background-removal integration.

## Commands

```powershell
Set-Location 'C:\Users\VR\Projects\Raster-studio'
git status --short
git log -1 --oneline
Set-Location 'C:\Users\VR\Projects\Raster-studio\raster-studio'
rustc --version
cargo --version
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
```

For a normal implementation card, run the affected package's focused tests. For a phase gate:

```powershell
cargo fmt --all --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

The integration package is `integration-tests`. After the proposed test file exists:

```powershell
cargo test --locked -p integration-tests --test thumbnail_workflow
```

Launch manually when required:

```powershell
cargo run --locked -p studio-desktop
```

Do not alter pinned dependencies just to make a failed test disappear. If checks require unavailable system tooling or GPU/OS integration, record the actual failure and complete independent work; do not label the missing check verified.

## Frequent implementation traps

| Trap | Required correction |
|---|---|
| Adding controls before their data survives a save | Complete rich-text schema, conversion, rendering, cache identity, and migration first. |
| Reusing canvas bounds for a small object's transform | Query the object's content bounds and keep coordinate spaces explicit. |
| Scaling text by rewriting pixel tiles | Update the editable layer transform. |
| Fitting an image by clipping away source pixels | Store full source tiles and fit using a transform. |
| Treating Properties focus as a paint target | Connect mask thumbnail selection to a validated shell edit target. |
| Creating every mask variant as the same empty mask | Write distinct correct coverage, including outside-tile defaults. |
| Populating transform/caret overlays only in tests | Publish the running tool's state through the real shell each frame. |
| Choosing the stale internal image clipboard | Define ownership/freshness and honor external clipboard replacement. |
| Assuming parser/writer self-round-trip proves PSD compatibility | Use independent fixtures and inspect editability plus appearance. |
| Adding Select Subject to avoid fixing masks | Finish manual selection, mask painting, refinement, and undo first. |
| Claiming completion from old screenshots or docs | Produce current evidence and keep unrun checks open. |

## Stop/resume record

```text
Task ID/title:
Current status:
Files changed:
Regression and observed result:
Checks actually run:
Evidence paths:
Unresolved issue:
Next ready task:
Exact next action:
```

For a final delivery, include the native example project, exported image, actual application evidence, check results, and precise remaining limitations. Optional cards 093–098 do not block delivery of the main workflow.
