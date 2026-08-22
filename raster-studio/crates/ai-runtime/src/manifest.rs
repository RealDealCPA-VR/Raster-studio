//! Workflow manifests + a registry with capability checks.
//!
//! Manifests declare what a curated workflow needs (inputs, nodes, VRAM,
//! accelerators). Before submitting any job we check the requested operation
//! against the manifest — this is the "capability check before submission"
//! safety control.

use serde::{Deserialize, Serialize};

/// A single required custom node, pinned by version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredNode {
    pub id: String,
    pub version: String,
}

/// A curated, version-pinned workflow definition (matches `workflows/manifests/*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowManifest {
    pub id: String,
    pub version: String,
    pub required_runtime: String,
    pub required_nodes: Vec<RequiredNode>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub minimum_vram_gb: u32,
    pub supported_accelerators: Vec<String>,
}

/// Describes what the currently-installed runtime can offer.
#[derive(Debug, Clone)]
pub struct RuntimeCapabilities {
    pub available_vram_gb: u32,
    pub accelerator: String,
    pub installed_node_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapabilityError {
    #[error("workflow '{0}' not found in registry")]
    UnknownWorkflow(String),
    #[error("insufficient VRAM: need {need} GB, have {have} GB")]
    InsufficientVram { need: u32, have: u32 },
    #[error("accelerator '{have}' not supported (want one of {supported:?})")]
    UnsupportedAccelerator {
        have: String,
        supported: Vec<String>,
    },
    #[error("missing required node: {0}")]
    MissingNode(String),
}

/// A set of known workflow manifests, keyed by id.
#[derive(Debug, Default)]
pub struct WorkflowRegistry {
    manifests: Vec<WorkflowManifest>,
}

impl WorkflowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, m: WorkflowManifest) {
        self.manifests.push(m);
    }

    pub fn get(&self, id: &str) -> Option<&WorkflowManifest> {
        self.manifests.iter().find(|m| m.id == id)
    }

    /// Verify the runtime can run `workflow_id`. Run this **before** submitting.
    pub fn check(
        &self,
        workflow_id: &str,
        caps: &RuntimeCapabilities,
    ) -> Result<(), CapabilityError> {
        let m = self
            .get(workflow_id)
            .ok_or_else(|| CapabilityError::UnknownWorkflow(workflow_id.to_string()))?;

        if caps.available_vram_gb < m.minimum_vram_gb {
            return Err(CapabilityError::InsufficientVram {
                need: m.minimum_vram_gb,
                have: caps.available_vram_gb,
            });
        }
        if !m.supported_accelerators.contains(&caps.accelerator) {
            return Err(CapabilityError::UnsupportedAccelerator {
                have: caps.accelerator.clone(),
                supported: m.supported_accelerators.clone(),
            });
        }
        for node in &m.required_nodes {
            if !caps.installed_node_ids.contains(&node.id) {
                return Err(CapabilityError::MissingNode(node.id.clone()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inpaint_manifest() -> WorkflowManifest {
        WorkflowManifest {
            id: "inpaint-standard".into(),
            version: "1.0.0".into(),
            required_runtime: ">=0.0.0".into(),
            required_nodes: vec![RequiredNode {
                id: "core".into(),
                version: "pinned".into(),
            }],
            inputs: vec!["image".into(), "mask".into(), "prompt".into()],
            outputs: vec!["image".into()],
            minimum_vram_gb: 12,
            supported_accelerators: vec!["cuda".into()],
        }
    }

    #[test]
    fn passes_when_capable() {
        let mut reg = WorkflowRegistry::new();
        reg.register(inpaint_manifest());
        let caps = RuntimeCapabilities {
            available_vram_gb: 16,
            accelerator: "cuda".into(),
            installed_node_ids: vec!["core".into()],
        };
        assert!(reg.check("inpaint-standard", &caps).is_ok());
    }

    #[test]
    fn fails_on_low_vram() {
        let mut reg = WorkflowRegistry::new();
        reg.register(inpaint_manifest());
        let caps = RuntimeCapabilities {
            available_vram_gb: 8,
            accelerator: "cuda".into(),
            installed_node_ids: vec!["core".into()],
        };
        assert_eq!(
            reg.check("inpaint-standard", &caps),
            Err(CapabilityError::InsufficientVram { need: 12, have: 8 })
        );
    }

    #[test]
    fn fails_on_missing_node() {
        let mut reg = WorkflowRegistry::new();
        reg.register(inpaint_manifest());
        let caps = RuntimeCapabilities {
            available_vram_gb: 16,
            accelerator: "cuda".into(),
            installed_node_ids: vec![],
        };
        assert!(matches!(
            reg.check("inpaint-standard", &caps),
            Err(CapabilityError::MissingNode(_))
        ));
    }
}
