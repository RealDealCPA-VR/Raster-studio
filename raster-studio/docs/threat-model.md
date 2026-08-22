# Threat Model

Scope: a **local-first desktop application** with an optional local AI runtime.
No required cloud account or inference API — so the classic server-side attack
surface is largely absent by design. The remaining concerns:

## 1. The AI runtime sidecar (highest priority)

**Asset:** the user's images and the local GPU.
**Threats & mitigations:**

| Threat | Mitigation | Where |
| --- | --- | --- |
| Other local processes submit jobs to our ComfyUI | Random **per-launch capability token**; every request must present it | `ai-runtime::CapabilityToken` |
| Runtime exposed on the network (LAN) | Bind **only** to `127.0.0.1`; refuse non-loopback binds | `ai-runtime::SidecarHandle::new` |
| Arbitrary graph / node-install / shell / remote URLs | Only curated, version-pinned workflows; capability check before submit; no node-install in product UI | `ai-runtime::manifest`, `workflows/` |
| Runaway job exhausts VRAM / disk | Job-size + dimension limits, VRAM preflight, cancellation | `ai-runtime::sidecar::preflight` |
| Sidecar crash/orphan process | Child lifetime tied to app; monitored; stop/restart controls | `ai-runtime::sidecar` |
| Malicious/corrupted model output | Treat outputs as untrusted; decode through the codec facade; validate dimensions | `raster::codec`, contracts |

## 2. Licensing & updates

| Threat | Mitigation | Where |
| --- | --- | --- |
| Forged entitlement | Ed25519 signature over canonical claims; **only public key** in app | `licensing` |
| Downgrade/expiry bypass | Expiry + version-coverage checks after signature | `licensing::verify` |
| Malicious update pushed via CDN compromise | Update manifest must carry a valid Ed25519 signature; payload hash verified | `updater::verify_manifest` |
| Private signing key leakage | Key lives **only** in the release system, never in the repo or app | process/policy |

## 3. Project files & assets

| Threat | Mitigation | Where |
| --- | --- | --- |
| Corrupted/partial save on crash | Atomic temp-write + fsync + rename, with rollback | `project-format::save_project` |
| Malformed `.rstudio` package | Version-gated load; typed decode errors; reject non-packages | `project-format::load_project` |
| Malicious linked asset path | Linked assets recorded explicitly; "collect assets" embeds; decode is sandboxed to the codec facade | `asset-store`, `raster::codec` |
| Tile/asset tampering | Content-addressed by BLAKE3; hash mismatch is detectable | `raster::TileHash`, `asset-store::BlobHash` |

## 4. Privacy

- **No image ever leaves the machine** unless the user explicitly runs a cloud
  action (there is none in v1) — AI inference is local via the sidecar.
- Telemetry is **opt-in**; diagnostic bundles are built locally and never
  uploaded without explicit consent (`telemetry::DiagnosticBundle`).

## 5. Supply chain

- Rust deps inventoried (`LICENSES/THIRD_PARTY_NOTICES.md`); prefer `cargo-audit`
  in CI.
- ComfyUI + Python + custom nodes + models pinned by version/commit/hash
  (`runtime/requirements-lock.txt`, `runtime/custom-nodes-lock.json`).

## Out of scope (v1)

Multi-user/collaboration, remote storage, mobile — none are prerequisites and
none are shipped, so their attack surface does not exist yet.
