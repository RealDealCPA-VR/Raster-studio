# Third-Party Notices

This file inventories third-party components distributed with, or used to
build, Raster Studio. Keep it accurate: every dependency that ships in a
release must be listed here with its license and version.

Generate the Rust dependency inventory with:

```bash
cargo install cargo-about
cargo about generate about.hbs > LICENSES/rust-dependencies.html
```

## Rust dependencies

See `rust-dependencies.html` (generated). Notable permissively-licensed crates:

| Crate | License | Notes |
| --- | --- | --- |
| winit | Apache-2.0 | Windowing |
| wgpu | MIT / Apache-2.0 | GPU abstraction |
| egui | MIT / Apache-2.0 | Immediate-mode UI |
| image | MIT / Apache-2.0 | Codecs |
| glam | MIT / Apache-2.0 | Math |
| serde | MIT / Apache-2.0 | Serialization |
| ed25519-dalek | BSD-3-Clause | Licensing signatures |
| rusqlite (bundled) | MIT (SQLite: public domain) | Settings/index |

None of the above are copyleft. The proprietary editor may link these freely.

## The ComfyUI runtime is tracked separately

ComfyUI and its Python ecosystem are **GPL-covered** and are handled in
`COMFYUI_SOURCE_AND_NOTICES.md`. They are distributed as a separate,
process-isolated component and are **never** linked into the Rust editor.
