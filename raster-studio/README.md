# Raster Studio — the cargo workspace

The project overview lives in the [repository root README](../README.md). This
file covers only what you need to build and work inside the workspace.

```bash
# from this directory
cargo check --workspace --all-targets   # type-check everything
cargo test  --workspace                 # 6,534 #[test] functions
cargo run   -p studio-desktop           # launch
cargo run   -p studio-desktop -- img.png
```

`rust-toolchain.toml` pins the compiler this workspace builds with (1.98.1;
`rustup` fetches it on the first `cargo` command). The MSRV — the oldest
compiler that can build the lockfile, `rust-version` in `Cargo.toml` — is 1.89,
and CI checks it with `cargo +1.89 check`. Windows needs the MSVC build tools
(and the Windows SDK's `rc.exe` for the release build's icon and version
resource). On Linux you need a Vulkan- or GL-capable environment for the
window; GPU-backed tests detect the absence of an adapter and skip themselves
rather than fail, so they can run on a runner without a GPU.

## Where things are

| Path | What it holds |
| --- | --- |
| `apps/studio-desktop` | The executable |
| `crates/` | The 20 library crates (map below) |
| `docs/PLAN.md` | The audit, the architecture decisions, and the build order |
| `docs/PRODUCTION-TODO.md`, `docs/CORRECTIONS-TODO.md` | The production plan and the correction queue — history; each header says what is still open |
| `../CHANGELOG.md` | What each fix and parity wave changed |
| `docs/FEATURES.md` | Every feature in detail, with its limits and tests |
| `docs/parity-matrix.md` | Feature-by-feature status against Photopea, kept honest |
| `docs/PSD-THUMBNAIL-SUPPORT.md` | What the `.psd` / `.psb` reader and writer carry, and what each report note means |
| `docs/architecture.md` | The crate graph and the layering rules |
| `docs/render-pipeline.md` | The CPU compositor and what the GPU does |
| `docs/file-format.md` | What a `.rstudio` package contains |
| `docs/threat-model.md` | Only the mitigations that exist in code |
| `tests/integration` | End-to-end tests over the engine the app runs |

## The crates

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
function from document to pixels; `render` owns all wgpu; `project-format`
owns the `.rstudio` package; and `ui` never mutates the document — it emits
commands, so undo and redo behave the same whichever control produced the
edit. See [`docs/architecture.md`](docs/architecture.md).

## The two rules

1. **A test that passes against the unfixed code is not a test.** Break the
   thing you fixed and watch the test go red before you believe it.
2. **Do not write prose asserting behaviour the code does not have.** This
   workspace was rebuilt from a scaffold whose documentation described a working
   editor that did not compile.

CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets` with
`-D warnings`, `cargo test --workspace --no-fail-fast` on Linux, Windows and
macOS, `cargo +1.89 check` (the MSRV), and `cargo audit`. Note that a warning
is an error there, and that an item used only under `#[cfg(windows)]` is dead
code on Linux. The workflow also starts on a `v*` tag push (`on.push.tags`),
and on a tag — or on a manual run with `release_dry_run` ticked — its `release`
job builds the three installers as workflow-run artifacts (no GitHub Release
is created) — see `apps/studio-desktop/packaging/README.md`. It has run as a
dry run (2026-10-01, run 36819294457) and built all three installers; no tag
exists yet.

What the app can and cannot do yet, and the licence situation (no licence has
been chosen), are in the [root README](../README.md).
