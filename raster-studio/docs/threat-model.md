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
| Any sidecar process | No sidecar. The one interpreter is in-process: W13-K's JavaScript engine for File ▸ Script (§4b). The one process the application starts is the user's web browser, from the Help menu (fixed `https://github.com/...` URLs), through the `webbrowser` crate (`app-shell/src/dialogs.rs`, `BrowserUrls`). The build script runs `git` for the version stamp (`apps/studio-desktop/build.rs`). The only other `std::process::Command` in the workspace is an `asset-store` test calling `mkfifo` to stage the FIFO case in §5. `tokio`'s `process` feature is declared in `[workspace.dependencies]` and requested by no member crate, so `tokio` is not in the binary at all. |
| Accounts, cloud storage, collaboration | Not implemented; explicit non-goals in [`PLAN.md`](PLAN.md). |
| Telemetry upload | `telemetry::DiagnosticBundle` is serialized to local JSON and defaults `upload_consented` to `false`. Nothing reads that flag, because nothing can upload. |

What remains is the classic desktop-application surface: **files that arrive
from other people.** A `.rstudio` package, a `.psd`, an ordinary image, a
PDF or design file (Sketch, XD, Figma, Paint.NET), a camera DNG, a `.cube`
LUT, a `.atn` action set, a `.jsx` script, clipboard image data and an asset
store directory are all attacker-controlled input, and every length, offset, count and path in one is
chosen by whoever wrote the file. `preferences.json`, `presets.json`,
`actions.json` and `recent.json` are trusted only as far as the per-user config
directory is (§6).

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
| A declared size that becomes a huge allocation | Every read is capped **before** the allocation, against the file's metadata first and re-checked after the read so a file that grew in between is still refused (in that race, only after the grown file was read into memory). Aggregates are capped as well as per-item sizes, and the tile-count cap is enforced *while collecting* rather than by measuring the finished set. The bounds are tabulated in [`file-format.md`](file-format.md). | `project-format/src/safepath.rs` — `read_capped`; `tiles.rs`, `assets.rs`, `package.rs` (`FileCaps`, `TileCaps`, `AssetCaps`) |
| A package that saves and then never reopens | Every one of those bounds is applied on the way **out** as well, from the same accessor the load reads, so the two sides cannot drift. A save can fail loudly; it cannot succeed into a file that will not open. This is a fix, not a design flourish: an embedded asset over the store's blob limit and an asset index over 16 MiB each saved `Ok` and then failed every subsequent open with the user's only copy inside. | `project-format/src/assets.rs` (`caps`), `tiles.rs` (`TileCaps`), `package.rs` (`file_caps`) |
| A compressed tile blob (`.tilez`) that inflates without bound | The inflate is capped at `MAX_TILE_BYTES` (one RGBA `f32` tile, 1 MiB, since W10-H) while it runs; a blob that would inflate past it is `FileTooLarge`, and one that does not inflate is `CorruptBlob` (`a_compressed_blob_that_inflates_past_the_cap_is_refused`). The cap is the same number on the write side. | `project-format/src/tiles.rs` — `inflate_capped`, `TileCaps` |
| Tampered or corrupted pixels | Every tile and asset blob is named by the BLAKE3 of its own bytes and re-hashed on read; a mismatch is `CorruptBlob`. The three files that are not content-addressed — `document.msgpack`, `assets/index.json`, `previews/preview.png` — are verified against the digest `Manifest::contents` records, with a *missing* entry treated as a failure rather than as permission to skip. | `project-format/src/tiles.rs`, `assets.rs`, `package.rs` — `verify_listed` |
| A rewritten manifest | The manifest carries a BLAKE3 seal over its own fields and over `contents`, computed in a canonical order-stable encoding. An empty `integrity` never verifies. | `project-format/src/manifest.rs` — `seal`, `verify_seal` |
| A document from a newer build decoding into nonsense | The format version is read out of the serialized document by a one-field probe **before** the document is decoded, and a version outside `1..=4` is refused by name. | `project-format/src/migrate.rs` |
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
| A four-billion-layer header reserving four billion records | Every count is checked against `ReadOptions` **before** the `Vec` is reserved, and additionally against the bytes actually remaining. Defaults: 30 000 px per edge (300 000 for a `.psb`, `max_psb_dimension`), 8 192 layers, 64 channels per layer, 4 096 name units, 8 192 descriptor items. | `psd/src/limits.rs` — `ReadOptions` |
| Many individually reasonable layers adding up to a hostile total | All decoded pixel bytes are drawn from one shrinking `Budget` shared by the whole read — 1 GiB by default. Per-field ceilings cannot express this. | `psd/src/limits.rs` — `Budget` |
| A decompression bomb | ZIP channels inflate through a `take` capped one byte past what the channel's geometry requires, so a bomb is refused after one extra byte. | `psd/src/zip.rs` |
| A stack overflow (an abort, not an error) from deep nesting | Descriptor parsing and group nesting are both depth-limited (32 and 64). Walking, writing, flattening and **dropping** the resulting tree are each written with an explicit stack, including `GroupData`'s `Drop` — defence in one layer is not defence. | `psd/src/limits.rs`, `descriptor.rs`, `model.rs`, `read.rs` |
| A header alone asking the allocator for fourteen gigabytes | `flatten` takes its canvas size from a header a caller can supply with no file behind it, so it draws every canvas from `WriteOptions::max_flatten_bytes` (2 GiB) and refuses before it reserves. | `psd/src/flatten.rs` |
| Colour modes read as RGB producing silently wrong pixels | CMYK, Lab, Indexed, Duotone, Multichannel and Bitmap are **refused by name** rather than approximated. Since W10-F a `.psb` (version 2) is read through the same bounded cursor with 64-bit lengths (`PsdHeader::read_any`); any other version is refused with `UnsupportedVersion`. | `psd/src/header.rs`, `read.rs` |

**Reachability, live:** `psd` is wired into `app-shell`. File ▸ Open reads the
file on an import worker, refusing anything over 2 GiB before reading
(`app-shell/src/jobs.rs`), and parses it there with
`import::document_from_psd`; `OpenDocument::open_psd` is the synchronous route
with the same bound. `export_psd_to` writes a layered PSD back out (temp file,
then rename); `looks_like_psd` picks that road on content. A worker panic still
aborts the process.
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
| A layer name choosing an Export Layers file name | Export Layers names each file through `app-shell`'s own `safe_file_name`, not `sanitize_file_stem`. It keeps any Unicode letter or digit, space, `_` and `-`, turns everything else into `_` and trims dots, so no separator or `..` survives. **Not covered:** it has no length cap and no reserved-name check (a layer named `CON` becomes `CON.png`), and two layers with the same name write the same file, the second over the first. | `app-shell/src/editor.rs` — `safe_file_name`; `jobs.rs` |
| A failed export destroying what was already at that path | `encode_to_path` encodes into a temporary file beside the destination and renames it over the top only once the bytes are on disk; a layered PSD is written the same way (`doc::write_atomically`). **Not covered:** File ▸ Print (PDF) and Help ▸ Export Diagnostics write directly over the chosen path. | `raster/src/codec.rs`; `app-shell/src/doc.rs` |

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

### Other files the user opens

| Input | What bounds it | Implemented in |
| --- | --- | --- |
| A `.cube` LUT (Image ▸ Adjustments ▸ Color Lookup, and since W13-D File ▸ Open, which makes it a Color Lookup layer through the same reader) | The file is refused from its metadata over 16 MiB (`MAX_CUBE_FILE_BYTES`) and the read itself is bounded to the cap. The parser accepts an edge of 2..=65, collects no more table rows than that edge holds, and refuses non-finite values, 1D and non-unit-domain files. | `adjustments/src/extended.rs` — `read_cube_file`, `Lut3d::parse_cube` |
| File ▸ Place / Replace Contents | The source file is read whole with `std::fs::read` (no size cap) before decoding; the decode is then bounded by `ImportLimits`. | `app-shell/src/editor.rs` |
| Clipboard images | Refused over 16 384 px per side — after `arboard` has already materialised them. | `app-shell/src/clipboard.rs` |
| Recent-file thumbnails | The start screen reads a recent package's `previews/preview.png` with plain `std::fs::read`: no size cap, no symlink refusal, no digest check. The decode is bounded by `ImportLimits`. | `app-shell/src/chrome.rs` — `recent_thumb_image` |
| System fonts and `RASTER_STUDIO_FONT_DIRS` | Parsed in-process by the text engine's font loader. | `text-engine/src/font.rs` |
| A font file opened with File ▸ Open (`.ttf` `.otf` `.ttc` `.otc`, W9-K) | Read whole with `std::fs::read` (**no size cap**) and parsed in-process for the session. | `app-shell/src/editor.rs` — `load_font_file` |
| Other image formats (W10-F, W11-H): Netpbm, DDS, JPEG XL, GIMP `.xcf`, OpenEXR, Radiance `.hdr`, `.icns`, IFF, Krita `.kra` | Each reader checks the declared size against `ImportLimits` before allocating (OpenEXR: the header and every part's data and display window, before `image` decodes a block), and each has truncation and bit-flip sweeps that error without panicking. A `.kra` is read through this crate's own bounded ZIP reader (stored / deflate, inflate capped at the allocation ceiling, ZIP64 and encryption refused). | `raster/src/formats/` |
| SVG and `.svgz` (W9-N) | The file, and a `.svgz`'s inflated text, are each capped at 64 MiB (inflated through a capped reader); `<image>` references resolve for `data:` URLs only; the raster is bounded by `ImportLimits`. | `raster/src/codec.rs` — `svg_import` |
| AVIF and HEIC | Refused by name before any decoding: no decoder for either is linked (AVIF is encode-only). | `raster/src/formats/avif.rs`, `formats/mod.rs` |
| PDF and PDF-compatible `.ai` (W13-D, W13X-7) | The file is read through a cap of four times `ImportLimits::max_alloc_bytes`; `hayro` 0.4 (pure Rust, `#![forbid(unsafe_code)]`) parses objects lazily; every page's pixel size is checked against `ImportLimits` before a buffer exists (at most 100 pages, `MAX_PAGES`; 18-1200 dpi); encrypted files are refused by name. **Not covered:** a panic inside `hayro` is caught (`guarded`, `catch_unwind`) only in builds that unwind (the release profile aborts), and a render has no time bound, so a pathological content stream is as slow as `hayro` makes it; the import dialog's thumbnails render on the interaction thread. | `raster/src/formats/pdf.rs`; `app-shell/src/editor_open_pages.rs`, `editor_open_w13x7.rs` |
| WMF / EMF (W13-D) | Every record's length is checked against the file before it is read; the walk stops at the end-of-file record or at `MAX_RECORDS` (1 000 000); counts inside a record are checked against the record; the SVG handed to `resvg` is capped at `MAX_SVG_ELEMENTS` (200 000); embedded bitmaps go through the BMP reader with the caller's limits, their total bounded by the allocation ceiling. | `raster/src/formats/metafile.rs` |
| EPS previews and the ZIP-packaged Sketch / XD / Figma files (W13-D, W13X-8) | This crate's own ZIP reader checks every offset against the file before following it, walks the central directory at most once per declared entry, refuses ZIP64 and encrypted archives by name, and inflates through a reader capped at the allocation ceiling; the EPS header's offsets and an EPSI preview's declared size are checked before a buffer exists. Sketch / XD JSON: each entry at most `MAX_JSON_BYTES` (64 MiB), nested at most `MAX_JSON_DEPTH` (256), a layer tree at most `MAX_LAYER_DEPTH` (64) deep and `MAX_NODES` (20 000) nodes; bitmaps decoded under `ImportLimits` with their total held to its allocation ceiling. | `raster/src/formats/vector_docs.rs`, `design_files.rs`; `app-shell/src/import_design.rs` |
| A Figma `fig-kiwi` canvas (W13X-8) | Both chunks (raw DEFLATE, or Zstandard through `ruzstd` 0.9) inflate through readers capped at `MAX_CHUNK_BYTES` (256 MiB); the schema is held to `MAX_DEFINITIONS` (4 096) definitions of at most `MAX_FIELDS` (1 024) fields and names to `MAX_NAME_BYTES` (256); the decoder nests at most `MAX_KIWI_DEPTH` (64) deep and produces at most `MAX_KIWI_VALUES` (8 000 000) values; everything it builds (value slots, keys, enum names, strings, byte arrays, and the spare capacity a growing list gains) is charged against `MAX_KIWI_DECODED_BYTES` (512 MiB); an array reserves at most `MAX_KIWI_RESERVE` (4 096) slots up front whatever length it declares. **Not charged:** the allocator's per-allocation overhead and the old buffer alive while a growing list is copied, so the real peak is somewhat above 512 MiB. | `raster/src/formats/design_fig.rs` |
| A Paint.NET `.pdn` (W13X-7) | The file is read through a cap of four times `ImportLimits::max_alloc_bytes`. The MS-NRBF (.NET BinaryFormatter) reader checks every length and count against the bytes that remain before allocating; record nesting is held to `MAX_DEPTH` (64), values to `MAX_VALUES` (1 048 576), layers to `MAX_LAYERS` (1 000), and the parsed graph's memory to `MAX_GRAPH_BYTES` (64 MiB), every kept string, value and object charged before it is stored (a class's names stored once and shared). A pixel block longer than its layer's `stride * height` is refused; each block is charged against the import limit on top of every layer's pixels before its buffer exists, gzip chunks inflate straight into it, and a chunk inflating to more or fewer bytes than its place is refused. The reader instantiates no .NET types: it only walks the records. | `raster/src/formats/pdn.rs`; `app-shell/src/editor_open_w13x7.rs` |
| A camera DNG, and the vendor RAWs (W13-C) | The TIFF structure is walked by this crate's own reader; the image size is checked against `ImportLimits` (`check_decode`) and the sample buffer against the allocation ceiling before either exists; CR2, CR3, NEF, ARW, RAF, ORF and RW2 are recognised from their signatures (the sniff reads at most `SNIFF_BYTES`, 256 KiB) and refused by name, so no vendor decompressor runs at all. | `raster/src/formats/raw.rs`, `ljpeg.rs` |
| A `.atn` action set (W13-E) | Read through the resource-file route (64 MiB cap, `MAX_RESOURCE_BYTES`); counts are checked against `MAX_ENTRIES` (16 384) before anything is reserved, strings against `MAX_ATN_STRING` (4 096), and descriptors go through the `psd` crate's depth- and count-limited reader. A step is only ever mapped onto this application's own menu and dialog routes (`atn::interpret`); an unmapped step is listed as skipped and never executed. | `asset-store/src/resources/atn.rs`; `app-shell/src/atn_play.rs` |
| A video file (MP4, MOV, WebM, AVI) | Refused by name from its signature before anything is parsed (`formats::mp4::video_refusal`): no video decoder is linked. `mp4::probe`, which reads this build's own MP4 exports back in tests, refuses a sample table naming more than `MAX_PROBE_SAMPLES` (1 000 000) frames or more than the file holds before sizing anything by it. | `raster/src/formats/mod.rs`, `formats/mp4.rs` |
| A `.abr` brush library (W9-E) | Refused from its metadata over 64 MiB (`MAX_ABR_BYTES`); at most 1 000 brushes, 5 000 px a side, and 256 Mi decoded pixels in total, taken before each tip is decoded, so a PackBits bomb is refused rather than expanded. | `asset-store/src/abr.rs`; `app-shell/src/editor.rs` — `import_abr` |
| A `.asl` style library (W9-H) | Read whole with `std::fs::read` (**no size cap**); at most 100 000 styles (`MAX_ASL_STYLES`); its patterns are read under the `psd` crate's decode `Budget`. | `asset-store/src/asl.rs`; `app-shell/src/menu_bridge/asl_import.rs` |
| `.pat` `.grd` `.csh` `.aco` `.ase` `.icc` resource files (W9-N) | The whole file is capped at 64 MiB (`MAX_RESOURCE_BYTES`) before it is read, every read goes through the `psd` crate's bounds-checked `Cursor`, and every declared count is checked against a limit (`MAX_ENTRIES`, `MAX_STOPS`, `MAX_KNOTS`; an ICC profile at 16 MiB) before anything is reserved. | `asset-store/src/resources/` |

A linked smart object's path would be read by `refresh_linked_sources`, but no
UI route calls it yet. Layer ▸ Smart Object ▸ Embed Linked (W11-E) does read
the linked file: whole, with `std::fs::read` and **no size cap**
(`app-shell/src/layer_ops_w11e.rs` — `embed_linked`). Export formats add no
input surface.

## 4b. Scripts (File ▸ Script, W13-K)

**Asset:** the user's files and the process. A `.jsx` / `.js` someone sends
is attacker-chosen code.

| Threat | Mitigation | Implemented in |
| --- | --- | --- |
| A script run without the user choosing to | A `.jsx` / `.js` opened with File ▸ Open or dropped on the window only fills the Script window; it runs when Run is pressed. The file is refused over `MAX_SCRIPT_BYTES` (1 MiB). | `app-shell/src/script.rs` |
| A script reading or writing files, or reaching the network | The engine (`boa_engine` 0.21, default features off) runs on a thread of its own whose one native function is a channel to the interaction thread, which answers each DOM call through an existing editor route; nothing on the engine thread can touch the editor, a file or the network. `app.open` and `saveAs` open the platform pickers (the script's file name only prefills the name box, its last path component); the user chooses the file. | `app-shell/src/script/engine.rs`, `script/host.rs`, `script/prelude.js` |
| An endless loop or runaway recursion hanging the window | The evaluation yields every `CHUNK` (10 000) VM cost units and is dropped past a budget of 400 000 000 units or 20 s of wall time (`ScriptLimits`); `boa`'s own per-loop iteration limit (50 000 000) and recursion limit (512) cover callbacks a builtin runs; every DOM call checks the wall clock before it is sent. **Not covered:** a single builtin call that runs long by itself (a huge `String.prototype.repeat`) is not interrupted until it returns, and its allocation is bounded only by the allocator. | `app-shell/src/script/engine.rs` |
| A script's edits being unrecoverable | A run is one undo step per document it changed (`Script` in History). | `app-shell/src/script/host.rs` |

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

`preferences.json` (which also carries swatches and brush presets),
`recent.json`, `sessions/{pid}.json`, `actions.json` and `presets.json` live in
the per-user config directory (`dirs::config_dir()`, or the temp directory if
the platform will not name one).

| Threat | Mitigation | Implemented in |
| --- | --- | --- |
| A corrupt or hostile preferences file stopping the app from starting, or dividing the layout by a zero UI scale | Loading is **infallible**: a missing, truncated or newer file falls back to defaults, and every scalar setting is clamped on the way in rather than trusted. Swatch colours and brush-preset ranges are not clamped; only non-finite entries are dropped. | `app-shell/src/prefs.rs` — `Preferences::load`, `sanitized` |
| A hostile `actions.json` | Refused from its metadata over 256 MiB (`MAX_ACTIONS_FILE_BYTES`) and read through a bounded `take`; every recorded tile must be an RGBA8, RGBA16 or (W10-H) RGBA `f32` layer tile or an 8-bit mask tile filed under its own hash, or the whole file is refused. Saved atomically. | `app-shell/src/actions_library.rs` |
| A hostile `presets.json` | **Not defended.** It is read whole with no cap, and a pattern preset with a zero width or height, or a short `rgba8`, panics when sampled (`PatternPreset::pixel`) — an abort under `panic = "abort"`. The file is loaded at startup. | `asset-store/src/presets.rs` |
| A stale session marker blocking a start, or one instance deleting another's recovery data | Markers are per-pid, not a lock. A run only ever writes and removes the file named after its own pid, and a marker whose pid still names a live process is skipped rather than offered — declining a recovery deletes the autosave it was offering, which may be another instance's only copy of an hour of work. | `app-shell/src/session.rs` — `SessionMarker`, `process_is_running` |

## 7. Licensing and updates — **none**

No licensing or update code exists in the workspace: the `licensing` and
`updater` crates were dropped (P3.2). Nothing checks a licence, and there is no
network code to download an update with.

## 8. Supply chain

- Third-party Rust dependencies are inventoried in
  [`../LICENSES/THIRD_PARTY_NOTICES.md`](../LICENSES/THIRD_PARTY_NOTICES.md),
  regenerated from the manifests and `Cargo.lock`.
- `Cargo.lock` is committed, so every build resolves the same versions.
- CI runs `cargo audit` alongside `fmt`, `clippy -D warnings` and the test suite.
- Advisory policy for the two known findings (C4): the lockfile carries
  `quick-xml 0.30` (RUSTSEC-2026-0194 — quadratic run time on duplicate
  attribute names; RUSTSEC-2026-0195 — unbounded namespace allocation, both
  7.5 high) through the Linux-only AT-SPI accessibility chain
  `accesskit_unix 0.12.3 → atspi 0.22 → zbus-lockstep 0.4.4 → zbus_xml 4.0 →
  quick-xml ^0.30`, which arrived with P3.11 (AccessKit screen-reader
  support). The fix version (≥ 0.41.0) cannot be reached by a direct bump
  (`zbus_xml 4.0` pins `^0.30`), and raising `accesskit_winit` past that
  generation is blocked today by `egui-winit 0.29`'s own pin. Of the three
  remedies, the chosen one is **(c)**: `.cargo/audit.toml` ignores both
  advisories with a named expiry (**2027-03-01**) and the reason inline.
  The trade-off, stated plainly: this is a suppression, not a fix — the
  affected surface is Linux-only XML parsing inside the screen-reader bridge
  (no document XML is ever parsed with `quick-xml`), and dropping the
  accessibility backend instead (option (b)) was rejected because it would
  trade a parsing-DoS advisory for losing Linux screen-reader support
  entirely. The fix arrives with the egui upgrade that lifts the
  `accesskit_winit` pin; the ignore entries are deleted then.
- `#![forbid(unsafe_code)]` is set on ten crates: `adjustments`, `color`,
  `compositor`, `design`, `filters`, `project-format`, `selection`, `tools`,
  `ui` and `vector`. It is **not** workspace-wide. In the crates that lack it,
  the only `unsafe` in non-test code is in `app-shell/src/session.rs` — the OS
  call that asks whether a recorded pid is still alive (`OpenProcess` on
  Windows, `libc::kill(pid, 0)` on unix) — and, on Windows only,
  `apps/studio-desktop/src/main.rs`'s `console::attach_to_parent`, which
  re-attaches the GUI-subsystem executable to its parent's console (W1). The other `unsafe` in the workspace is
  the counting global allocator in `psd/src/probe.rs` and
  `raster/src/lib.rs`, both `#[cfg(test)]`, which exist so "validate before you
  allocate" can be asserted on bytes requested rather than on a wall-clock
  threshold that would measure the CI machine instead of the code.
  Notably, **`psd` — the parser most exposed to hostile input — does not carry
  the attribute**, though it contains no `unsafe` outside that test module.
- One feature-unification hazard, named where it happens: `image`'s codec
  features are requested in `crates/raster`, but Cargo unifies features across a
  workspace build, so `image` is compiled once with the union of every member's
  requests (`crates/raster/Cargo.toml` asks for `gif`, `bmp`, `ico`, `tga`,
  `avif`, `exr` and `hdr`) and the GIF, BMP, ICO, TGA, OpenEXR and Radiance HDR
  decoders (and the AVIF encoder) are linked into every crate in the
  workspace that depends on `image`. Treat that list as workspace-wide
  attack surface, not as one crate's private set.

## Out of scope

Multi-user collaboration, remote storage, mobile, and cloud sync. None is
shipped, so none has an attack surface yet. Nothing in this document should be
read as covering them if one is added.
