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

Requires Rust 1.82 or newer and, on Windows, the MSVC build tools. On Linux you
need a Vulkan- or GL-capable environment for the window; GPU-backed tests detect
the absence of an adapter and skip themselves, so headless CI stays green.

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
`-D warnings`, `cargo test --workspace` on Linux and Windows, and `cargo audit`.
Note that a warning is an error there, and that an item used only under
`#[cfg(windows)]` is dead code on Linux.
