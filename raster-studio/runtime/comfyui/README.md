# ComfyUI Runtime — Build & Packaging Recipes

This directory holds **recipes and packaging scripts**, not mutable application
code. The runtime is a separately-distributed, process-isolated component (see
`LICENSES/COMFYUI_SOURCE_AND_NOTICES.md` for the GPL boundary).

## Layout

```
runtime/comfyui/
├── README.md            # this file
├── build.sh             # (TBD) fetch pinned ComfyUI + build isolated venv
└── dist/                # (git-ignored) built distribution — the payload
    └── venv/            # (git-ignored) isolated Python environment
```

## Principles

- **Pinned, reproducible.** ComfyUI commit is pinned in
  `../custom-nodes-lock.json`; Python deps in `../requirements-lock.txt`.
- **Isolated environment.** Never install into a system Python; always a
  dedicated venv under `dist/venv/`.
- **Loopback only.** The launcher (in `ai-runtime`) binds the server to
  `127.0.0.1` with a random per-launch capability token.
- **No auto node-install in the product.** Node installation is a build-time
  concern here, not a runtime feature exposed in the editor UI.

## Building (outline — implement in build.sh)

1. Clone ComfyUI at the pinned commit into `dist/`.
2. Create `dist/venv/` and install `../requirements-lock.txt` (with hashes).
3. Install pinned custom nodes from `../custom-nodes-lock.json`.
4. Record every component's version + hash + license into the release
   inventory (`LICENSES/COMFYUI_SOURCE_AND_NOTICES.md`).
5. Retain corresponding source for all GPL binaries distributed.
