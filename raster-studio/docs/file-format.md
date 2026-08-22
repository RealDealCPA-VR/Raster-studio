# File Format — the `.rstudio` package

The native project format is **authoritative** and is a *directory package*,
not a monolithic file. This enables atomic saves, crash recovery, tile
deduplication, and portable "collect assets" projects.

## Layout

```
project.rstudio/
├── manifest.json          # package layout version + integrity + doc path
├── document.msgpack       # serialized Document (MessagePack)
├── commands.journal       # JSONL of accepted commands (crash recovery/replay)
├── previews/
│   ├── thumbnail.webp
│   └── composite-preview.webp
├── tiles/                 # content-addressed tile blobs (BLAKE3 hash names)
├── assets/                # embedded/linked asset records
└── ai/
    └── generation-metadata.json   # AI provenance records
```

## Versioning — two independent versions

| Version | Field | Owner | Bump when |
| --- | --- | --- | --- |
| Package layout | `manifest.json` → `manifest_version` | `project-format::Manifest` | The *package* gains/moves files |
| Document model | `document.msgpack` → `meta.format_version` | `editor-core::DocumentMeta` | The *document schema* changes |

Both are **mandatory**. On open, `load_project` reads the manifest first and
refuses versions newer than it understands; document migrations run in
`project-format::package::migrate` keyed on `format_version`.

## Atomicity & durability

`save_project` writes a **fresh package** to a sibling temp directory, `fsync`s
every file, then renames it over the destination (with rollback of the previous
package on failure). A partially-written package can never replace a good one.

## Crash recovery

Every accepted command is appended to `commands.journal` (one JSON object per
line) and `fsync`ed. Recovery = load the last saved `document.msgpack`, then
replay any journal entries recorded after it. After a successful full save the
journal is cleared. Verified end-to-end in
`tests/integration/tests/vertical_slice.rs`.

## Content addressing & dedup

Tiles and assets are keyed by BLAKE3 hash (`raster::TileHash`,
`asset-store::BlobHash`). Identical tiles/assets are stored once; refcounting
drives GC. This keeps large multi-layer documents compact and makes
"did this change?" a hash comparison.

## Portable projects

"Collect assets" mode embeds linked assets into `assets/` and sets
`manifest.assets_collected = true`, producing a self-contained package that can
be moved between machines.
