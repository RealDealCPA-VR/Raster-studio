# Raster Studio — the cargo workspace

The project overview lives in the [repository root README](../README.md). This
file covers only what you need to build and work inside the workspace.

```bash
# from this directory
cargo check --workspace --all-targets   # type-check everything
cargo test  --workspace                 # ~3,000 tests
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
| `crates/` | The 22 library crates — see the root README for the map |
| `docs/PLAN.md` | The audit, the architecture decisions, and the build order |
| `docs/parity-matrix.md` | Feature-by-feature status, kept honest |
| `docs/architecture.md` | The crate graph and the layering rules |
| `docs/render-pipeline.md` | The CPU compositor and what the GPU does |
| `docs/file-format.md` | What a `.rstudio` package contains |
| `docs/threat-model.md` | Only the mitigations that exist in code |
| `tests/integration` | End-to-end tests over the engine the app runs |

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
is created) — see `apps/studio-desktop/packaging/README.md`.
