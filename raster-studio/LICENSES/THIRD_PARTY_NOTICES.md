# Third-Party Notices

Every third-party component that ships in a Raster Studio release is a Rust
crate compiled into the `studio-desktop` binary. There is no bundled runtime, no
interpreter, no sidecar process and no downloaded component — see
[`../docs/architecture.md`](../docs/architecture.md).

Regenerated from the workspace manifests and `Cargo.lock`. **Keep it accurate:**
re-run the commands below whenever a dependency is added, removed or bumped.

## Regenerating this file

Everything here is reproducible with stock `cargo`:

```bash
cd raster-studio

# The full shipped graph, one line per package, with its SPDX expression.
# --target all covers the Linux- and macOS-only branches too.
cargo tree -p studio-desktop -e normal --target all \
  --prefix none --format '{p}|{l}' | sort -u

# Just what a host build links (drop --target all).
cargo tree -p studio-desktop -e normal --prefix none --format '{p}|{l}' | sort -u

# Why a particular package is present.
cargo tree -p studio-desktop -e normal --target all -i <crate>
```

`-e normal` is what makes the output a *shipping* inventory: it excludes
dev-dependencies, so `tempfile` and the `dejavu` test font — which exist only so
the test suite does not depend on what fonts a machine happens to have installed
— are correctly absent.

A prettier HTML inventory can be produced with
[`cargo-about`](https://github.com/EmbarkStudios/cargo-about), but that requires
an `about.toml`/`about.hbs` pair, neither of which is checked in, so no such
file is generated today. (The previous version of this document told readers to
"see `rust-dependencies.html`". No such file is present in this repository, and
nothing generates one, which is why the inventory is inline here instead.)

## Direct dependencies

These are the crates named in the workspace manifests. Versions are as resolved
in the committed `Cargo.lock`.

| Crate | Version | License | Used by |
| --- | --- | --- | --- |
| `anyhow` | 1.0.104 | MIT OR Apache-2.0 | `app-shell`, `asset-store`, `licensing`, `project-format`, `render`, `studio-desktop` |
| `thiserror` | 2.0.20 | MIT OR Apache-2.0 | most crates |
| `serde` | 1.0.229 | MIT OR Apache-2.0 | most crates |
| `serde_json` | 1.0.151 | MIT OR Apache-2.0 | `app-shell`, `licensing`, `project-format`, `telemetry`, `updater` |
| `rmp-serde` | 1.3.1 | MIT | `app-shell`, `project-format` — the `document.msgpack` encoder |
| `blake3` | 1.8.6 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | `asset-store`, `project-format`, `raster` — content addressing and the package seal |
| `rayon` | 1.12.0 | MIT OR Apache-2.0 | `compositor`, `filters`, `selection` — tile-parallel pixel work |
| `glam` | 0.29.3 | MIT OR Apache-2.0 | geometry throughout |
| `uuid` | 1.24.1 | Apache-2.0 OR MIT | `layer-model` — layer and mask identity |
| `winit` | 0.30.13 | Apache-2.0 | `app-shell` — window and event loop |
| `wgpu` | 22.1.0 | MIT OR Apache-2.0 | `app-shell`, `render` — GPU abstraction |
| `egui` | 0.29.1 | MIT OR Apache-2.0 | `design`, `ui` |
| `egui-wgpu` | 0.29.1 | MIT OR Apache-2.0 | `app-shell` |
| `egui-winit` | 0.29.1 | MIT OR Apache-2.0 | `app-shell` |
| `bytemuck` | 1.25.2 | Zlib OR Apache-2.0 OR MIT | `raster`, `render` — GPU uniforms; `&[u16]` as `&[u8]` without a copy |
| `pollster` | 0.4.0 | Apache-2.0 OR MIT | `app-shell`, `render` — blocking on adapter/device requests |
| `image` | 0.25.10 | MIT OR Apache-2.0 | `raster` — PNG, JPEG, WebP, TIFF, GIF, BMP, ICO, TGA |
| `flate2` | 1.1.9 | MIT OR Apache-2.0 | `psd` — the ZIP channel encodings |
| `cosmic-text` | 0.17.2 | MIT OR Apache-2.0 | `text-engine` — shaping, layout, glyph rasterisation |
| `tracing` | 0.1.44 | MIT | diagnostics |
| `tracing-subscriber` | 0.3.23 | MIT | `telemetry` |
| `rfd` | 0.15.4 | MIT | `app-shell` — native file and message dialogs |
| `dirs` | 5.0.1 | MIT OR Apache-2.0 | `app-shell` — the per-user config directory |
| `windows-sys` | 0.59.0 | MIT OR Apache-2.0 | `app-shell`, Windows only — process liveness |
| `libc` | 0.2.189 | MIT OR Apache-2.0 | `app-shell`, unix only — process liveness |

### Declared, but not in the shipped binary

`crates/licensing` and `crates/updater` have **no dependents in the workspace**
(see [`../docs/architecture.md`](../docs/architecture.md)), so nothing they pull
in reaches `studio-desktop`. Their dependencies are listed here anyway, because
they are workspace crates and the moment either is wired in these become
shipping components:

| Crate | Version | License | Used by |
| --- | --- | --- | --- |
| `ed25519-dalek` | 2.2.0 | BSD-3-Clause | `licensing`, `updater` |
| `curve25519-dalek` | 4.1.3 | BSD-3-Clause | via `ed25519-dalek` |
| `subtle` | 2.6.1 | BSD-3-Clause | via `curve25519-dalek` |
| `ed25519` | 2.2.3 | Apache-2.0 OR MIT | via `ed25519-dalek` |
| `signature` | 2.2.0 | Apache-2.0 OR MIT | via `ed25519-dalek` |
| `zeroize` | 1.9.0 | Apache-2.0 OR MIT | via `curve25519-dalek` |
| `sha2` | 0.10.9 | MIT OR Apache-2.0 | via `ed25519-dalek` |
| `rand` | 0.8.7 | MIT OR Apache-2.0 | `licensing` (key generation, release-side helper) |

Dev-only, **not shipped**: `tempfile` 3.27.0 (MIT OR Apache-2.0) and `dejavu`
2.37.0 (a permissively licensed font family, embedded so shaping tests are
deterministic).

Declared in `[workspace.dependencies]` and used by **no** member crate, so they
are absent from `Cargo.lock`'s shipped graph and from the binary: `tokio`,
`rusqlite`, `notify`, `crossbeam-channel`, `postcard`, `raw-window-handle` (as a
direct dependency; it arrives transitively through `winit`/`wgpu`). They are
listed here so nobody concludes from the workspace manifest that an SQLite
build, an async runtime or a filesystem watcher ships.

## The full graph

For a Windows host build, `studio-desktop`'s shipped graph resolves to **222**
distinct third-party package versions; across all targets (`--target all`, which
adds the X11/Wayland and AppKit branches) it is **406**.

License expressions across that full set:

| Expression | Packages |
| --- | --- |
| MIT / Apache-2.0 dual, in any spelling or order | 240 |
| MIT | 88 |
| Apache-2.0 | 9 |
| `Apache-2.0 AND MIT` | 1 |
| Unicode-3.0 (the ICU crates) | 18 |
| Permissive triples including Zlib | 15 |
| Zlib alone | 2 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 7 |
| `Unlicense OR MIT` | 4 |
| `MIT OR Apache-2.0 OR LGPL-2.1-or-later` (`r-efi`) | 2 |
| BSD-2-Clause / BSD-3-Clause, alone or in a permissive choice | 9 |
| BSL-1.0 | 2 |
| MPL-2.0 | 1 |
| ISC | 1 |
| CC0-1.0, alone or in a permissive choice | 3 |
| `Apache-2.0 OR GPL-2.0-only` (`self_cell`) | 1 |
| `0BSD OR MIT OR Apache-2.0` | 1 |
| `(MIT OR Apache-2.0) AND Unicode-3.0` (`unicode-ident`) | 1 |
| `(MIT OR Apache-2.0) AND OFL-1.1 AND LicenseRef-UFL-1.0` (`epaint_default_fonts`) | 1 |
| **Total** | **406** |

Regenerate that table with:

```bash
cargo tree -p studio-desktop -e normal --target all --prefix none --format '{p}|{l}' \
  | sort -u | grep -v '(\*)' | grep -v '|Proprietary' \
  | cut -d'|' -f2 | sort | uniq -c | sort -rn
```

(`|Proprietary` is this workspace's own crates; excluding them is what makes the
count a third-party count.)

## Licenses that need a note

Everything above is permissive or offers a permissive option. The ones worth
naming individually:

- **`option-ext` 0.2.0 — MPL-2.0.** The only weak-copyleft dependency in the
  graph. It arrives as `dirs` → `dirs-sys` → `option-ext`. MPL-2.0 is *file*-scoped:
  it obliges publication of modifications to that crate's own source files, and
  imposes nothing on the rest of the binary. Raster Studio does not modify it.
- **`tiny-skia`, `tiny-skia-path`, `moxcms`, `pxfm`, `num_enum`, `arrayref`,
  `zerocopy` — BSD-2/3-Clause**, alone or as one option of a permissive choice.
  These carry an attribution and no-endorsement obligation; the notices belong in
  the shipped licence file. `tiny-skia` reaches the graph only through
  `sctk-adwaita` → `winit` and is therefore Linux/Wayland-only; `moxcms` and
  `pxfm` come in through `image`. (`ed25519-dalek`, `curve25519-dalek` and
  `subtle` are BSD-3-Clause too, but are not in the shipped graph — see
  "Declared, but not in the shipped binary" above.)
- **`clipboard-win`, `error-code` — BSL-1.0.** Boost licence, attribution only,
  and it explicitly waives the notice requirement for binary distribution.
  They arrive through `egui-winit` → `arboard` and are Windows-only.
- **`libloading` — ISC.** Attribution only. Arrives through `wgpu-hal` → `ash`.
- **The ICU crates (`icu_*`, `zerovec`, `yoke`, `tinystr`, `litemap`,
  `writeable`, `potential_utf`, `zerotrie`, `zerofrom`) — Unicode-3.0.** The
  Unicode licence: permissive, attribution required.
- **`self_cell` 1.3.0 — `Apache-2.0 OR GPL-2.0-only`.** A dual licence, and we
  take the **Apache-2.0** option. It is not a GPL dependency. It arrives through
  `cosmic-text`.
- **`r-efi` — `MIT OR Apache-2.0 OR LGPL-2.1-or-later`.** A triple choice; take
  MIT. Not an LGPL dependency.
- **`epaint_default_fonts` 0.29.1 — `(MIT OR Apache-2.0) AND OFL-1.1 AND
  LicenseRef-UFL-1.0`.** This one embeds **font files** in the binary (egui's
  default typefaces), and the SIL Open Font Licence and Ubuntu Font Licence
  obligations travel with them: the fonts must be distributed with their
  licences, must not be sold on their own, and reserved font names must not be
  reused for modified versions. Raster Studio does not modify or rename them.

## No copyleft boundary

There is **no GPL-, AGPL-, or LGPL-only dependency anywhere in the shipped
graph.** The only copyleft licence present at all is MPL-2.0, on one crate, at
file scope. The two expressions above that mention GPL or LGPL are dual/triple
licences with permissive alternatives, and the permissive alternative is what
applies here.

Earlier revisions of this project bundled a GPL-covered ComfyUI runtime as a
separate process and carried a `COMFYUI_SOURCE_AND_NOTICES.md` describing that
boundary and the legal review it required before distribution. **That runtime
was removed in full** ([`../docs/PLAN.md`](../docs/PLAN.md) §D3): the crates, the
workflow templates, the Python environment and the notices file are all gone,
and with them the boundary. Nothing in this repository links, bundles or invokes
GPL-covered code.

## Rust and the standard library

The Rust toolchain and `std` are MIT OR Apache-2.0.
[`../rust-toolchain.toml`](../rust-toolchain.toml) selects the `stable` channel
with `rustfmt` and `clippy`; the workspace MSRV, declared in
[`../Cargo.toml`](../Cargo.toml), is 1.82 on edition 2021.

## Raster Studio itself

Every workspace member declares `license = "Proprietary"` in
`[workspace.package]`. This directory currently holds only this notices file —
there is no product licence text checked in yet. That is a gap to fill before a
release, not a claim that one exists.
