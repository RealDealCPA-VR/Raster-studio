# ComfyUI Source & Notices (GPL boundary)

The optional local AI runtime bundles **ComfyUI** and a curated set of custom
nodes, Python packages, and models. ComfyUI is licensed under the **GPL**.
This document records how we satisfy the license and keep it separable from the
proprietary editor.

## Separation architecture (why this is compliant-by-design)

- ComfyUI runs as a **separate operating-system process** (a sidecar), launched
  on demand and bound to `127.0.0.1`.
- The Rust editor communicates with it over **authenticated localhost HTTP/IPC**
  only — never by importing, statically linking, or dynamically linking any
  GPL Python/C code.
- The editor's typed AI protocol lives in `crates/ai-contracts`; the only code
  that knows ComfyUI exists is the adapter in `crates/ai-runtime`.
- The runtime is **distributed separately** (or as a clearly separable
  download/component), with its own source-availability obligations honored.

> A process boundary strongly supports the "mere aggregation" argument but is
> **not** a complete legal conclusion. **Obtain legal review before commercial
> distribution.**

## Inventory (fill in per release)

| Component | Version | Commit / hash | License | Source URL |
| --- | --- | --- | --- | --- |
| ComfyUI | `PINNED` | `TBD` | GPL-3.0 | https://github.com/comfyanonymous/ComfyUI |
| custom-node: … | | | | |
| python: torch | | | | |
| model: … | | | | (record model license separately) |

Locks that must accompany every release:

- `runtime/requirements-lock.txt` — exact Python package pins + hashes.
- `runtime/custom-nodes-lock.json` — custom nodes with commit hashes.

## Corresponding source

For every GPL binary we distribute, retain and make available the exact
corresponding source (or a written offer). Record the retention location here:

- Corresponding-source archive location: `TBD`
- Build instructions: `runtime/comfyui/README.md`

## Model & asset licenses

Model weights carry **their own** licenses (often *not* GPL and sometimes
non-commercial). Track each model's license and redistribution terms in the
inventory table above before bundling.
