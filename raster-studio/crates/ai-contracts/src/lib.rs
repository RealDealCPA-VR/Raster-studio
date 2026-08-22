//! The **stable, typed AI protocol** the editor speaks.
//!
//! This crate is the firewall between the document model and any AI runtime.
//! It contains *only* plain data types — no ComfyUI, no HTTP, no process
//! handling. `ai-runtime` is the sole component allowed to translate these into
//! a ComfyUI workflow graph. Nothing about ComfyUI's graph format may leak into
//! these types.
//!
//! AI results always come back as **assets + masks + provenance**, so they
//! enter the document as ordinary editable layers/masks with reproducibility
//! metadata attached.

use serde::{Deserialize, Serialize};

/// A reference to an asset the runtime produced or consumed, by content hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRef {
    pub hash_hex: String,
    pub mime: String,
}

/// A reference to a mask asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaskRef {
    pub hash_hex: String,
}

/// The complete set of AI operations the editor UI can request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AiOperation {
    Generate(GenerateRequest),
    Inpaint(InpaintRequest),
    Outpaint(OutpaintRequest),
    BackgroundReplace(BackgroundReplaceRequest),
    Upscale(UpscaleRequest),
    Restore(RestoreRequest),
}

impl AiOperation {
    /// Workflow manifest id this operation maps to.
    pub fn workflow_id(&self) -> &'static str {
        match self {
            AiOperation::Generate(_) => "generate-standard",
            AiOperation::Inpaint(_) => "inpaint-standard",
            AiOperation::Outpaint(_) => "outpaint-standard",
            AiOperation::BackgroundReplace(_) => "background-replace",
            AiOperation::Upscale(_) => "upscale-standard",
            AiOperation::Restore(_) => "restore-standard",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub prompt: String,
    pub negative_prompt: String,
    pub width: u32,
    pub height: u32,
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InpaintRequest {
    pub image: AssetRef,
    pub mask: MaskRef,
    pub prompt: String,
    pub negative_prompt: String,
    pub seed: Option<u64>,
    /// Denoise strength within the masked region, 0..=1.
    pub strength: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutpaintRequest {
    pub image: AssetRef,
    pub prompt: String,
    /// Pixels to extend on each side: [left, top, right, bottom].
    pub extend: [u32; 4],
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundReplaceRequest {
    pub image: AssetRef,
    pub subject_mask: MaskRef,
    pub prompt: String,
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpscaleRequest {
    pub image: AssetRef,
    pub scale: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreRequest {
    pub image: AssetRef,
}

/// The result of an AI operation. Everything needed to insert editable layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiResult {
    pub output_assets: Vec<AssetRef>,
    pub generated_masks: Vec<MaskRef>,
    pub provenance: GenerationProvenance,
}

/// Reproducibility metadata stored with the document (`ai/generation-metadata.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationProvenance {
    pub workflow_id: String,
    pub workflow_version: String,
    pub runtime_version: String,
    pub model_ids: Vec<String>,
    pub seed: Option<u64>,
    pub parameters: serde_json::Value,
}

/// Progress/status stream item emitted while a job runs (for UI + cancellation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobStatus {
    Queued,
    Running { percent: f32 },
    Succeeded,
    Cancelled,
    Failed { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_ids_are_stable() {
        let op = AiOperation::Inpaint(InpaintRequest {
            image: AssetRef {
                hash_hex: "ab".into(),
                mime: "image/png".into(),
            },
            mask: MaskRef {
                hash_hex: "cd".into(),
            },
            prompt: "a cat".into(),
            negative_prompt: String::new(),
            seed: Some(42),
            strength: 0.8,
        });
        assert_eq!(op.workflow_id(), "inpaint-standard");
        // Round-trips through JSON (the IPC wire format).
        let s = serde_json::to_string(&op).unwrap();
        let _back: AiOperation = serde_json::from_str(&s).unwrap();
    }
}
