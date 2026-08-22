# Curated AI Workflows

Version-pinned ComfyUI workflow templates and their capability **manifests**.
The editor only ever runs workflows declared here — never arbitrary graphs.

```
workflows/
├── manifests/          capability manifests (validated by ai-runtime)
│   ├── inpaint-standard.json
│   └── upscale-standard.json
├── product-photo/      workflow graph + assets for product-photo edits
├── inpaint/            workflow graph for the inpaint vertical slice
└── upscale/            workflow graph for upscaling
```

## Manifest schema

A manifest declares what a workflow needs so `ai-runtime` can run a capability
check **before** submitting a job (VRAM preflight, accelerator, required nodes).
See `crates/ai-runtime/src/manifest.rs` for the authoritative types.

| Field | Meaning |
| --- | --- |
| `id` | Stable workflow id referenced by `AiOperation::workflow_id()` |
| `version` | Workflow template version (semver) |
| `required_runtime` | Minimum ComfyUI runtime version range |
| `required_nodes` | Custom nodes (pinned) the graph depends on |
| `inputs` / `outputs` | Typed I/O the adapter maps to `ai-contracts` |
| `minimum_vram_gb` | Preflight VRAM gate |
| `supported_accelerators` | e.g. `cuda`, `rocm` |

## Adding a workflow

1. Drop the ComfyUI graph JSON in a new subdirectory.
2. Add a manifest under `manifests/`.
3. Pin any new custom nodes in `runtime/custom-nodes-lock.json`.
4. Map it to a typed `AiOperation` in `ai-contracts` if it's a new capability.
