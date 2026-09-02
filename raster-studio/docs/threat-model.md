# Threat Model

**This file lists only mitigations that exist in code, and names the file that
implements each.** Where nothing defends, it says so.

That rule is here because the previous version of this document claimed six
controls — a per-launch capability token, a loopback-only bind, a curated
workflow allow-list, a VRAM preflight, a supervised sidecar process, and a
pinned Python lockfile — for an AI runtime that was never written and has since
been removed entirely. A threat model asserting behaviour the code does not have
is worse than no threat model: it is a checklist someone will trust.

## Scope

Raster Studio is a single-process, local-first desktop application. It reads
files, edits them, and writes them back.

What that removes from the attack surface, verifiably:

| Not present | How to check |
| --- | --- |
| Any network code | No `std::net`, `TcpStream`, `TcpListener` or `UdpSocket` anywhere in the workspace. |
| Any HTTP or TLS stack | `cargo tree -p studio-desktop -e normal` contains no `reqwest`, `hyper`, `tokio`, `ureq`, `curl`, `rustls`, `native-tls` or `openssl`. |
| Any child process, interpreter or sidecar | No non-test code spawns a process. The only `std::process::Command` in the workspace is an `asset-store` test calling `mkfifo` to stage the FIFO case in §5. `tokio`'s `process` feature is declared in `[workspace.dependencies]` and requested by no member crate, so `tokio` is not in the binary at all. |
| Accounts, cloud storage, collaboration | Not implemented; explicit non-goals in [`PLAN.md`](PLAN.md). |
| Telemetry upload | `telemetry::DiagnosticBundle` is serialized to local JSON and defaults `upload_consented` to `false`. Nothing reads that flag, because nothing can upload. |

What remains is the classic desktop-application surface: **files that arrive
from other people.** A `.rstudio` package, a `.psd`, a PNG and an asset store
directory are all attacker-controlled input, and every length, offset, count and
path in one is chosen by whoever wrote the file.

The release profile sets `panic = "abort"`, so a panic in a parser is not an
error a host can catch. "Must not panic" is a security property here, not a
quality-of-implementation note.

## 1. Malicious `.rstudio` package

**Asset:** every file the user can read, and the project itself.

| Threat | Mitigation | Implemented in |
| --- | --- | --- |
| A manifest path that reads a file outside the package | `manifest.document_path` is **never joined onto the package directory.** The document is read from a fixed filename; the manifest field exists only so a package that disagrees can be refused. Every package-supplied name goes through `check()` first, which rejects empty names, NULs, absolute and rooted paths, Windows drive and UNC prefixes (including forward-slash spellings such as `C:/x` and `//server/share`), any `..` or `.` component, and any `\` anywhere — validated twice, once by splitting on `/` manually and once through `Path::components`, keeping the intersection of what the two platforms agree on. | `project-format/src/safepath.rs` — `check`, `safe_join` |
| A symlinked directory component redirecting a read out of the package | `safe_join` walks **every** component and refuses a link at any depth: `tiles/ab` being a link to `/etc` makes `tiles/ab/<hex>.tile` an open outside the package while every name involved is a plain word. `open_project` additionally refuses a symlinked `manifest.json`, `commands.journal`, `tiles/`, `assets/`, `previews/` or `ai/` before it reads anything. | `project-format/src/safepath.rs` — `reject_symlink`; `project-format/src/package.rs` — `open_project` |
| A package that writes into a file outside itself, for a whole session | `commands.journal` is the one file the application writes back into a package it did not build. A symlinked journal would be an arbitrary-file-write primitive: `append` writes attacker-chosen JSON into the target and `clear` truncates it. It is refused on open **and re-checked by every writer immediately before opening it**, since a link can be planted after the open. | `project-format/src/journal.rs`; `project-format/src/package.rs` |
| A declared size that becomes a huge allocation | Every read is capped **before** the allocation, against the file's metadata first and re-checked after the read so a file that grew in between is still refused. Aggregates are capped as well as per-item sizes, and the tile-count cap is enforced *while collecting* rather than by measuring the finished set. The bounds are tabulated in [`file-format.md`](file-format.md). | `project-format/src/safepath.rs` — `read_capped`; `tiles.rs`, `assets.rs`, `package.rs` (`FileCaps`, `TileCaps`, `AssetCaps`) |
| A package that saves and then never reopens | Every one of those bounds is applied on the way **out** as well, from the same accessor the load reads, so the two sides cannot drift. A save can fail loudly; it cannot succeed into a file that will not open. This is a fix, not a design flourish: an embedded asset over the store's blob limit and an asset index over 16 MiB each saved `Ok` and then failed every subsequent open with the user's only copy inside. | `project-format/src/assets.rs` (`caps`), `tiles.rs` (`TileCaps`), `package.rs` (`file_caps`) |
| Tampered or corrupted pixels | Every tile and asset blob is named by the BLAKE3 of its own bytes and re-hashed on read; a mismatch is `CorruptBlob`. The three files that are not content-addressed — `document.msgpack`, `assets/index.json`, `previews/preview.png` — are verified against the digest `Manifest::contents` records, with a *missing* entry treated as a failure rather than as permission to skip. | `project-format/src/tiles.rs`, `assets.rs`, `package.rs` — `verify_listed` |
| A rewritten manifest | The manifest carries a BLAKE3 seal over its own fields and over `contents`, computed in a canonical order-stable encoding. An empty `integrity` never verifies. | `project-format/src/manifest.rs` — `seal`, `verify_seal` |
| A document from a newer build decoding into nonsense | The format version is read out of the serialized document by a one-field probe **before** the document is decoded, and a version outside `1..=3` is refused by name. | `project-format/src/migrate.rs` |
| A version-1 document carrying fields no version-1 build could write | The 2→3 migration clears `pixels` and `selection` rather than trusting them — a v1 document with a pixel store is damaged or forged, and its tile references have no blobs behind them. | `project-format/src/migrate.rs` — `STEPS` |

### The ordering, which is itself a control

`open_project` refuses an unsafe `document_path` **before** it verifies
integrity. That is deliberate: a hostile package can produce a manifest whose
digest verifies, because the digest detects damage rather than malice. Path
handling must never depend on a package having passed a check that the attacker
also controls.

### What is *not* checked

- **The seal is not a signature.** There is no key, so anyone who can rewrite a
  file can recompute the digest. A package that verifies is intact, never
  authentic.
- **`commands.journal` is verified by nothing at all.** The application appends
  to it while the package is open, so a digest would be stale the moment the
  user drew something. A hostile package may put whatever it likes in it; the
  parser is defensive and stops at the first record it cannot read.
- **A `contents` entry naming any other file is sealed and then never read or
  verified.** The loader opens a fixed set of files and takes no instruction
  from the inventory about what else to look at.
- **A symlink check is not a no-follow open.** `std` has no portable
  `O_NOFOLLOW`/`FILE_FLAG_OPEN_REPARSE_POINT`, so a link planted between the
  `symlink_metadata` and the `open` would still be followed. Closing that window
  needs platform-specific code and has not been written.

## 2. Crash-safe save

**Asset:** the user's only copy of their work.

| Threat | Mitigation | Implemented in |
| --- | --- | --- |
| A partially written package replacing a good one | The whole package is built in a sibling directory and fsynced before anything is swapped. | `project-format/src/package.rs` — `build_package`; `atomic.rs` — `write_and_sync` |
| A crash mid-swap leaving no project where the user saved | The previous package is parked under a `.bak-` sibling and only removed once the forward rename succeeds. `open_project` runs `atomic::recover` first, before it concludes the project does not exist, and puts the backup back. | `project-format/src/atomic.rs` — `swap_into_place`, `recover` |
| Two concurrent saves deleting each other's work | Temp and backup siblings get names unique across threads (counter), processes (pid) and pid reuse (nanosecond clock), so no save ever removes a directory it did not create. | `project-format/src/atomic.rs` — `unique_sibling` |
| A silent rollback failure | A failed rollback returns `ProjectError::RollbackFailed` naming **both** directories left on disk, because with the destination empty either may be the only copy. Every exit from the swap window goes through one function, so there is one answer to "what is on disk now". | `project-format/src/atomic.rs` — `roll_back` |
| A durable file with a non-durable directory entry | Directories are fsynced after renames, not only the files inside them. **Off Unix this is a no-op** — `std` cannot open a directory handle on Windows — which is exactly why `recover` runs unconditionally rather than as a Unix-only fallback. | `project-format/src/atomic.rs` — `sync_dir`, `sync_tree` |
| Work lost between the last save and a crash | Every accepted command is appended to `commands.journal` as one buffer including its newline, and recovery replays only the suffix after the last save marker — anchored by the snapshot's digest, so a journal beside the wrong document is refused rather than replayed. Unclean shutdown is detected by a per-pid session marker plus a real OS liveness check, so a second running instance is never mistaken for a crash. | `project-format/src/journal.rs`; `app-shell/src/session.rs` |

**The residual loss is stated:** the previous package always survives an
interrupted save; the interrupted save itself is discarded rather than adopted,
and its full-size `.new-` sibling is left on disk for a human to delete.

## 3. Malicious `.psd`

**Asset:** the process. A `.psd` is parsed in-process with `panic = "abort"`.

| Threat | Mitigation | Implemented in |
| --- | --- | --- |
| A truncated or lying section causing an out-of-bounds index | Every multi-byte field is read through a bounds-checked `Cursor` that returns `PsdError::Truncated` instead of panicking, and each section is parsed through a **sub-cursor** carved to the length the file declared — so a section that lies can only damage itself. Where a byte-for-byte inner loop does index directly (PackBits, the ZIP row predictor, the channel interleavers), the bound is established in that same function from a length it is holding; nothing indexes on the strength of a count another function promised. | `psd/src/bytes.rs` — `Cursor`, `Cursor::sub`; `psd/src/packbits.rs`, `zip.rs`, `codec.rs` |
| A four-billion-layer header reserving four billion records | Every count is checked against `ReadOptions` **before** the `Vec` is reserved, and additionally against the bytes actually remaining. Defaults: 30 000 px per edge, 8 192 layers, 64 channels per layer, 4 096 name units, 8 192 descriptor items. | `psd/src/limits.rs` — `ReadOptions` |
| Many individually reasonable layers adding up to a hostile total | All decoded pixel bytes are drawn from one shrinking `Budget` shared by the whole read — 1 GiB by default. Per-field ceilings cannot express this. | `psd/src/limits.rs` — `Budget` |
| A decompression bomb | ZIP channels inflate through a `take` capped one byte past what the channel's geometry requires, so a bomb is refused after one extra byte. | `psd/src/zip.rs` |
| A stack overflow (an abort, not an error) from deep nesting | Descriptor parsing and group nesting are both depth-limited (32 and 64). Walking, writing, flattening and **dropping** the resulting tree are each written with an explicit stack, including `GroupData`'s `Drop` — defence in one layer is not defence. | `psd/src/limits.rs`, `descriptor.rs`, `model.rs`, `read.rs` |
| A header alone asking the allocator for fourteen gigabytes | `flatten` takes its canvas size from a header a caller can supply with no file behind it, so it draws every canvas from `WriteOptions::max_flatten_bytes` (2 GiB) and refuses before it reserves. | `psd/src/flatten.rs` |
| Colour modes read as RGB producing silently wrong pixels | CMYK, Lab, Indexed, Duotone, Multichannel and Bitmap are **refused by name** rather than approximated. PSB (`.psb`, version 2) is refused with `UnsupportedVersion`. | `psd/src/header.rs`, `read.rs` |

**Reachability, live:** `psd` is wired into `app-shell`. `OpenDocument::open_psd`
parses untrusted `.psd` bytes into a `Document`, and `export_psd_to` writes a
layered PSD back out; `looks_like_psd` picks that road on content.
`OpenDocument::export_to` routes a `.psd` destination to the layered writer
rather than refusing it. These defences therefore protect a user, not just a
library.

## 4. Malicious ordinary image

**Asset:** the process, on File ▸ Open and drag-and-drop.

| Threat | Mitigation | Implemented in |
| --- | --- | --- |
| A header declaring absurd dimensions | Every decode entry point takes `ImportLimits` and checks the header-declared dimensions **before** any pixel buffer is allocated. Defaults: 65 535 px per side, 268 Mpx, 4 GiB of decode allocation — the number checked is what *this pipeline* allocates, counting the source buffer and the RGBA conversion that is live alongside it. | `raster/src/codec.rs` — `ImportLimits`, `decode_alloc_bytes` |
| A crafted canvas: a few dozen bytes declaring a billion pixels per side | Refused at the same header check, by name (`LimitExceeded`), with no allocation — asserted by `a_crafted_canvas_is_refused_by_name_before_any_allocation` (P3.7). The `png`-crate `iCCP` inflation below remains the one allocation the header check cannot reach. | `raster/src/codec.rs` — `check_dimensions` |
| A single third-party codec becoming an unbounded blast radius | One codec facade. Nothing above `raster::codec` names `image`, so swapping or restricting a backend is a change in one module. | `raster/src/codec.rs` |
| An export path derived from file content | Nothing opens a path derived from content. `encode_to_path` writes exactly the path it is given, and preset-suggested file *names* are reduced to ASCII alphanumerics plus `- _ ( )` and space — so `..` cannot survive, no result is a hidden file, and no result can smuggle a second extension. Reserved Windows device names fall back to `export`. | `raster/src/export.rs` — `sanitize_file_stem` |
| A failed export destroying what was already at that path | `encode_to_path` encodes into a temporary file beside the destination and renames it over the top only once the bytes are on disk. | `raster/src/codec.rs` |

**Known hole, measured rather than assumed.** `ImportLimits::max_icc_bytes` is a
*retention* filter, not an allocation bound: the backing decoder materialises an
`iCCP` chunk in full — inflating it under the `png` crate's own default budget —
before this crate ever sees the length, because `image` 0.25's
`PngDecoder::set_limits` carries an upstream TODO saying it does not propagate
limits into `png`. The test
`an_oversized_icc_profile_is_dropped_but_was_already_allocated` measures exactly
that and will fail if it is ever fixed.

**Also stated:** the default limits are chosen so a legitimate file a person
deliberately opened is never refused. They are **not** tight enough for decoding
images arriving unattended; a few-kilobyte crafted PNG can still ask for a
gigabyte. A caller in that position must construct its own limits.

## 5. Untrusted asset-store directory

An `asset-store` root can arrive inside someone else's project package.

| Threat | Mitigation | Implemented in |
| --- | --- | --- |
| A symlinked store root materialising `blobs/` and `tmp/` outside it | The root is checked with `symlink_metadata` **first, and on its own**, before anything is created inside it; `blobs/` and `tmp/` are then checked too. `create_dir_all` alone is not enough — it tests `is_dir()`, which follows links. | `asset-store/src/disk.rs` — `Disk::open`, `is_real_dir` |
| A read blocking on a FIFO, or reading a device or a file outside the root | `read_blob` and `load_index` refuse anything that is not a regular file *before* `open`, and re-confirm through the open handle — on unix an `fstat` of the descriptor cannot be redirected, so a path swapped in between is caught. | `asset-store/src/disk.rs` — `regular_file_meta`, `open_regular_file` |
| Garbage collection deleting a file this crate never wrote | `scan_blobs` and `clean_tmp` refuse to descend through a link. | `asset-store/src/disk.rs` |
| An untrusted name reaching a path | Every path is built from a fixed name this crate chose (`blobs`, `tmp`, `index`) or from a hash it computed itself; a filename read back from disk is accepted only as exactly 64 hex characters. | `asset-store/src/disk.rs`, `hex.rs` |
| Corrupted blob bytes returned as valid data | `get` re-hashes on the way in from disk and reports a mismatch. (`put`'s dedup fast path is a presence-and-length `stat`, not a content check — it catches an unlinked or truncated file, not corruption that preserved the length. `get` is what catches that.) | `asset-store/src/lib.rs` |
| An oversized on-disk claim becoming an allocation | Every read is size-bounded before a buffer is allocated, from `StoreConfig`. | `asset-store/src/lib.rs` — `StoreConfig` |

**Reachability, stated:** these defences guard `AssetStore::open`, the
disk-backed variant, which is called only from that crate's own tests.
`project-format` builds the memory-only store, so no application path exercises
them today. They are correct and tested; they are not currently load-bearing.

## 6. The application's own state

`preferences.json`, `recent.json` and `sessions/{pid}.json` live in the
per-user config directory (`dirs::config_dir()`, or the temp directory if the
platform will not name one).

| Threat | Mitigation | Implemented in |
| --- | --- | --- |
| A corrupt or hostile preferences file stopping the app from starting, or dividing the layout by a zero UI scale | Loading is **infallible**: a missing, truncated or newer file falls back to defaults, and every field is clamped on the way in rather than trusted. | `app-shell/src/prefs.rs` — `Preferences::load`, `sanitized` |
| A stale session marker blocking a start, or one instance deleting another's recovery data | Markers are per-pid, not a lock. A run only ever writes and removes the file named after its own pid, and a marker whose pid still names a live process is skipped rather than offered — declining a recovery deletes the autosave it was offering, which may be another instance's only copy of an hour of work. | `app-shell/src/session.rs` — `SessionMarker`, `process_is_running` |

## 7. Licensing and updates — **not wired in**

`crates/licensing` verifies an Ed25519-signed entitlement against a public key,
with expiry and version-coverage checked after the signature. `crates/updater`
verifies an Ed25519 signature over an update manifest. Both hold only a public
key; both have tests.

**Neither crate has a dependent anywhere in the workspace.** No code path in
`app-shell` verifies an entitlement or an update manifest, nothing gates a
feature on one, and there is no updater — there is no network code to download
anything with. Until that changes, these are libraries, not controls, and this
section must not be read as saying the shipped application checks a licence or
authenticates an update.

The one policy statement that does hold today: no private signing key exists in
this repository.

## 8. Supply chain

- Third-party Rust dependencies are inventoried in
  [`../LICENSES/THIRD_PARTY_NOTICES.md`](../LICENSES/THIRD_PARTY_NOTICES.md),
  regenerated from the manifests and `Cargo.lock`.
- `Cargo.lock` is committed, so every build resolves the same versions.
- CI runs `cargo audit` alongside `fmt`, `clippy -D warnings` and the test suite.
- `#![forbid(unsafe_code)]` is set on ten crates: `adjustments`, `color`,
  `compositor`, `design`, `filters`, `project-format`, `selection`, `tools`,
  `ui` and `vector`. It is **not** workspace-wide. In the crates that lack it,
  the only `unsafe` in non-test code is in `app-shell/src/session.rs` — the OS
  call that asks whether a recorded pid is still alive (`OpenProcess` on
  Windows, `libc::kill(pid, 0)` on unix). The other `unsafe` in the workspace is
  the counting global allocator in `psd/src/probe.rs` and
  `raster/src/lib.rs`, both `#[cfg(test)]`, which exist so "validate before you
  allocate" can be asserted on bytes requested rather than on a wall-clock
  threshold that would measure the CI machine instead of the code.
  Notably, **`psd` — the parser most exposed to hostile input — does not carry
  the attribute**, though it contains no `unsafe` outside that test module.
- One feature-unification hazard, named where it happens: `image`'s codec
  features are requested in `crates/raster`, but Cargo unifies features across a
  workspace build, so `image` is compiled once with the union of every member's
  requests and the GIF, BMP, ICO and TGA decoders are linked into every crate in
  the workspace that depends on `image`. Treat that list as workspace-wide
  attack surface, not as one crate's private set.

## Out of scope

Multi-user collaboration, remote storage, mobile, and cloud sync. None is
shipped, so none has an attack surface yet. Nothing in this document should be
read as covering them if one is added.
