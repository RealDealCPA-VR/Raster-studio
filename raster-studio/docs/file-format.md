# File Format — the `.rstudio` package

The native project format is authoritative and is a **directory package**, not
a single file. Implemented in [`crates/project-format`](../crates/project-format);
`save_project_with` writes one, `open_project` reads one.

## What a save actually writes

```text
project.rstudio/
├── manifest.json                       layout version, app version, integrity seal
├── document.msgpack                    the serialized Document (MessagePack)
├── commands.journal                    JSONL: accepted commands + save markers
├── previews/
│   └── preview.png                     composite thumbnail, longest edge 512 by default
├── tiles/
│   └── <2 hex>/<64 hex>.tilez | .tile  the pixels: deflate-compressed (.tilez), or raw
│                                       (.tile) when compression would not shrink them;
│                                       named by the BLAKE3 of the decompressed pixels
├── assets/
│   ├── index.json                      one record per asset
│   └── <2 hex>/<64 hex>.blob           embedded asset bytes, content-addressed
└── ai/                                 reserved; created empty, nothing writes into it
```

`ai/` is a leftover reserved name. `build_package` creates the directory and
`open_project` checks it is not a symlink; no code in the workspace puts
anything in it or reads anything out of it.

### The order things are written, and why manifest is last

`build_package` builds the whole package in a uniquely named sibling directory
and only then swaps it into place:

1. Create the temp directory and `previews/`, `tiles/`, `assets/`, `ai/`.
2. **`document.msgpack`** — `rmp_serde::to_vec_named`, specifically. `Document`'s
   hand-written `Serialize` omits `selection`, `pixels`, `active_layer`,
   `stored_selection`, `saved_selections`, `layer_selection` and `assets`
   while they are empty,
   so its field *count* varies with its content and a positional encoder would
   read the wrong field back. Its digest goes into `manifest.contents`.
3. **`tiles/`** — every distinct tile hash the document references, layers and
   masks alike, in sorted order so two saves of the same document do the same
   work. For each hash: if the package being replaced already holds a blob that
   verifies (inflated and re-hashed), it is hard-linked into the new package
   (copied where hard links fail) and the tile source is not consulted;
   otherwise the pixels are taken from the `TileBytes` source, checked against
   their hash, deflate-compressed (`flate2`, `Compression::fast()`; stored raw
   as `.tile` if that is not smaller) and written without an fsync. The pass
   ends with one batch of fsyncs over exactly the files it created. Reuse
   applies only when the destination's `manifest.json` is a file and its
   `tiles/` is not a symlink. Tiles are the only compressed files; everything
   else is stored uncompressed.
4. **`assets/`** — the index and, when collecting, the embedded blobs. The
   index's digest goes into `contents`.
5. **`previews/preview.png`** — rendered by `compositor::composite_region`, the
   same function that draws the canvas. Its digest goes into `contents`.
6. **`commands.journal`** — the previous package's *valid prefix* is copied
   across, then a save marker is appended.
7. **`manifest.json`** — last, because it describes everything above. Then
   sealed.
8. `sync_tree` fsyncs the files' *directories* (a file fsync says nothing about
   the durability of the directory entry pointing at it). Reused hard links are
   not fsynced; directory fsync is Unix-only.
9. `atomic::swap_into_place`.

`save_project(path, doc)` is the convenience wrapper for callers holding a
document and nothing else. It uses the `NoTiles` source and therefore **fails**
if the document references a tile that the package being replaced does not
already hold, rather than writing a package with missing pixels; a save over a
package holding every referenced tile succeeds, because every blob is reused.
Real saves go through `save_project_with`, which takes a `TileBytes` source and
a `SaveOptions` (application version string, whether to write a preview and at
what size, whether to collect linked assets, the asset list, and an optional
`SaveProgress` — `SaveOptions::reporting_to` — whose tile counters another
thread can poll; the application's status bar reads it).

## Two independent versions, both mandatory

| Version | Where | Current | Oldest read |
| --- | --- | --- | --- |
| Package layout | `manifest.json` → `manifest_version` | `3` | `2` |
| Document model | `document.msgpack` → `meta.format_version` | `4` | `1` |

They are separate because the package can gain files without the document schema
changing, and vice versa.

Layout **version 1 is refused, not migrated**: it carried no integrity data and
stored no pixels, so "migrating" one would mean producing a v2 package claiming
verified contents it never had. No v1 package ever shipped.

Layout **version 3** allows a tile blob to be stored deflate-compressed as
`tiles/xx/<hash>.tilez`; the hash still names the decompressed pixels. A v2
reader would report such a blob as missing, so the version moved: a v3 package
cannot be opened by a v2-era build. A v2 package opens unchanged — the reader
tries `.tilez`, then `.tile`.

Document versions 1 and 2 migrate through `project_format::migrate`. Neither
step transforms data — every field version 3 added has a serde default — but the
2→3 step *repairs*: it clears `pixels` and `selection`, because no pre-version-3
build could write either, so a version-1 document carrying them is damaged or
forged and its tile references have no blobs behind them. **Version 4** carries
the persisted rich-text schema for text layers (styled spans, paragraph settings,
frame, kerning — plan card 018); its migration is the schema's own serde
defaults, so a version-3 text layer loads black, regular, auto-leading point
text — exactly what the pre-version-4 renderer produced. The version is read out
of the serialized document by a one-field probe **before** the document is
decoded, so a file from a build with a newer `format_version` produces a
sentence about versions rather than a MessagePack error about an unknown field.
Additions made *within* a version (appended enum variants such as the five
adjustment kinds W4 added, defaulted fields) are not gated: an older build of
the same version fails to decode an unknown variant and silently drops unknown
fields.

The document also persists `meta.bit_depth` and `meta.color_mode` (a 16-bit
document reopens as 16-bit), the guides (changed by the undoable
`Command::SetGuides`), `stored_selection` and `saved_selections`.

## Integrity: what the seal proves

`Manifest::integrity` is a BLAKE3 over the manifest's own fields plus
`contents`, computed in a canonical, order-stable encoding that does not depend
on the JSON writer. `contents` holds a size and BLAKE3 per file that is **not**
content-addressed. An empty `integrity` never verifies.

Precisely three files are covered: `document.msgpack`, `assets/index.json` and
`previews/preview.png`. Each is verified against its `contents` entry whenever
the package carries it, and a *missing* entry is a failure rather than
permission to skip the check.

A `contents` entry naming anything else is checked for path safety, sealed with
the rest of the manifest, and then **never read or verified** — the loader opens
a fixed set of files and takes no instruction from the inventory about what else
to look at. `a_contents_entry_naming_anything_else_is_sealed_but_never_checked`
pins that, and fails if it ever changes.

Tile and asset blobs are deliberately absent from `contents`: the filename *is*
the BLAKE3 of the bytes, so every read re-hashes and compares. Listing them would
duplicate the check and make the manifest grow with the pixel count.

**The seal is not a signature.** There is no key, so anyone who can rewrite a
file can recompute the digest. It detects corruption and tampering after the
fact; it does not establish authenticity. That is exactly why the loader's path
handling runs *before* integrity is consulted.

## Content addressing

```text
tiles/<first two hex digits>/<64 hex digits>.tilez | .tile
assets/<first two hex digits>/<64 hex digits>.blob
```

The name is the BLAKE3 of the content: for a tile, of its decompressed pixels,
whichever form the file is stored in; for an asset, of the file's bytes
(`raster::TileHash`, `asset_store::BlobHash` — the same hash function, so the
two addressing schemes are one). Consequences:

- Identical tiles are stored once, however many layers, masks, mip levels or
  history states reference them. A flat fill across a layer is one blob.
  `SaveReport::tiles` reports the dedup win.
- Every blob is self-verifying. A blob that does not hash to its own name is
  `ProjectError::CorruptBlob` on read — and on *write*, because a `TileBytes`
  source that files bytes under the wrong hash is caught where the caller can
  still be told which tile. A `.tilez` that does not inflate is also
  `CorruptBlob`; one that would inflate past `MAX_TILE_BYTES` is
  `FileTooLarge`, capped during the inflate. The reuse path applies the same
  verification; a failing old blob is re-produced from the source, not carried
  forward.
- The two-digit shard keeps directories to a few thousand entries on filesystems
  that get slow with a hundred thousand.

## The journal and the save marker

`commands.journal` is one JSON record per line, and there are two kinds: an
accepted `Command`, and a save marker carrying the `DocumentDigest` of the
snapshot written beside it.

**Recovery is snapshot plus the suffix after the last save marker.** It is not a
rebuild-from-zero log: replaying the whole journal onto a loaded document
reapplies work the snapshot already holds, and two "create layer" records become
two layers. The marker's digest is what pairs a journal with its document, so a
journal beside the wrong snapshot is refused rather than replayed.

A record is written as one buffer containing its payload *and* its newline, so
an interrupted append leaves a partial line at the end and never a payload with
a missing terminator in the middle. Reading stops at the first record it cannot
parse and keeps the valid prefix, reporting `JournalRecovery::truncated` — a
half-written last line is the signature of a crash, and refusing the whole file
over it would throw away every command the user did complete.

A save copies the previous journal's valid prefix — measured on the buffer it
already read, never from a second read of a file the application is still
appending to — and then writes the new marker.

Three deliberate asymmetries:

- `commands.journal` is **not** listed in `contents` and is verified by nothing.
  The application appends to it while the package is open, so a digest of it
  would be stale the moment the user drew something.
- It is the only package file with **no write-side size bound**. The application
  grows it between saves rather than a save writing it whole, and `open_project`
  never reads it, so no size it reaches can cost the user the project. A save
  meeting an over-size journal starts a fresh one.
- It is the one file the application writes back into a package it did not
  build, so it gets the **symlink check that nothing else needs twice**: on open,
  and again by every writer immediately before opening it.

## Bounds, applied in both directions

The rule this format holds: *a package with more in it than this reader will
load is a package this writer must not produce.* Each bound is one number,
reached through one accessor by both sides, so a save can fail loudly but cannot
succeed into a file that will not reopen.

| File | Bound | Value |
| --- | --- | --- |
| `document.msgpack` | `MAX_DOCUMENT_BYTES` | 1 GiB |
| `manifest.json` | `MAX_MANIFEST_BYTES` | 64 MiB |
| `previews/preview.png` | `MAX_PREVIEW_BYTES` | 64 MiB |
| one tile blob (on disk, and inflated) | `MAX_TILE_BYTES` | one `TILE_SIZE²` RGBA16 tile — 512 KiB (an RGBA8 tile is half that) |
| distinct tiles per package | `MAX_PACKAGE_TILES` | 1 048 576 |
| all tile data | `MAX_TILE_DATA_BYTES` | 8 GiB |
| one asset | `MAX_ASSET_BYTES` | 512 MiB |
| assets per package | `MAX_ASSETS` | 65 536 |
| `assets/index.json` | `MAX_INDEX_BYTES` | 16 MiB |
| all distinct asset data | `MAX_ASSET_DATA_BYTES` | 2 GiB |
| `commands.journal` | `MAX_JOURNAL_BYTES` | 512 MiB — **read side only** |

The aggregates are not implied by the per-item caps: a million tiles at
`MAX_TILE_BYTES` is 512 GiB against an 8 GiB budget, and `MAX_ASSETS ×
MAX_ASSET_BYTES` is 32 TiB against 2 GiB. The tile-count cap is enforced *while
collecting* rather than afterwards, so `TooManyTiles.count` is the point at
which collecting stopped (`max + 1`) — the total is exactly what is never
computed.

Two of these were one-sided once, and both cost a file: an embedded asset over
the store's blob limit, and an asset index over 16 MiB, each saved `Ok` and then
failed every subsequent open with the user's only copy inside. A third was
wrong in the other direction: until W5 the tile cap was the RGBA8 size, so
every save of a 16-bit document was refused
(`a_16_bit_document_saves_and_reopens_with_the_same_pixels` pins the fix).

## Untrusted input

A package arrives from other people; every length, offset, count and path in one
is chosen by whoever wrote the file.

- **No path from a package is ever joined onto a directory.** `Path::join` with
  an absolute path discards the base, which turns one line of JSON into "read any
  file on this machine". The document is read from a fixed filename;
  `Manifest::document_path` exists so a package that disagrees can be *refused*.
  See [`threat-model.md`](threat-model.md).
- **Every allocation is bounded** by a named constant checked before the read.
- **Symlinks are refused** at every component of a path, not only the last.

## Durability and crash recovery

The save builds the package in a uniquely named sibling
(`P.rstudio.new-<pid>-<nanos>-<n>`), fsyncs the files it wrote (new tile blobs
as one batch; reused hard links are not fsynced) and its directories (Unix
only), then swaps: the previous package is renamed to a `.bak-` sibling, the new one is
renamed into place, and only then is the backup removed. A crash inside that
window leaves the backup on disk, which is what makes it recoverable —
`open_project` runs `atomic::recover` **first**, before it concludes anything,
because "the package is missing" is a state a crash produces and a state this
reader can fix.

Names are unique so two concurrent saves never collide and no save deletes a
directory it did not create. A rollback failure is returned as
`ProjectError::RollbackFailed`, naming *both* directories left on disk, because
with the destination empty either one may be the only copy of the user's work.

What that costs, precisely: **the previous package always survives; the
interrupted save does not.** `recover` renames the backup back and never adopts
the `.new-` temp, even though the temp is a complete, fsynced, sealed package.
So a crash between the two renames returns the user to their last successful
save. What can never happen is the state the old code left: no project at the
path the user saved to.

## Portable projects

`SaveOptions::collect_assets` reads every `AssetInput::Linked` file and embeds
its bytes, setting `manifest.assets_collected`. Reading an arbitrary path is
safe there and only there: the path comes from the *application* at save time
(the user picked the file), not from a package. The **load** side never opens a
`Linked` path — that would be the traversal bug in a new costume. It reports the
link and lets the application decide.

## Known limits

Stated rather than implied:

- **Loading is eager.** Every tile blob a document references is read into
  memory. A disk-backed, lazily faulted tile store is future work; the caps
  above are what stand in for it today.
- **Integrity is not authenticity.** No signature. See above.
- **Directory fsync is a no-op off Unix.** `std` cannot open a directory handle
  on Windows. Interrupted-save recovery is what covers the gap, and it runs on
  every platform.
- **An interrupted save leaves a `.new-` sibling that nothing reclaims.** A
  process that dies between the two renames cleans nothing up, and `recover`
  deliberately never touches a `.new-` directory — one may belong to a save
  still running in another process. Disk usage, not data loss; an age-gated
  sweep would have to guess how long a legitimate save may take.
- **A command journalled while a save is running is held beside the package.**
  The application saves a *snapshot* on a worker thread and keeps applying
  commands while it runs. For the length of the save those commands are
  journaled to a side file, `P.rstudio.journal-hold` (`app-shell`,
  `editor::journal_hold_path`), which the swap never touches. When the save
  lands (or fails), `CommandJournal::absorb` appends them after the new save
  marker — where they belong, since every one was accepted after the snapshot
  was taken. The absorb is exactly-once: the side file is first renamed to a
  staged name recording the journal's length, then appended, then deleted, and
  every open finishes whatever step a crash interrupted before it reads the
  journal (`app-shell/src/journal_hold_tests.rs`). A staged file that does not
  match is set aside, never replayed twice.
- **A symlink check is not a no-follow open.** `std` offers no portable
  `O_NOFOLLOW`, so a link planted in the microseconds between the
  `symlink_metadata` and the `open` would still be followed. Closing that needs
  platform-specific code and has not been written.
- **The channel-isolation mask and the camera are not saved.** They are view
  state, held by `app-shell`. Guides *are* saved: a `Document` field changed
  through the undoable `Command::SetGuides`.
