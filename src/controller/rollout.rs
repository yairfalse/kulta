use crate::crd::rollout::Rollout;
use k8s_openapi::api::core::v1::PodTemplateSpec;
use kube::runtime::controller::Action;
use serde_json;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    KubeError(#[from] kube::Error),
}

pub struct Context {
    // Mock context for testing
}

impl Context {
    pub fn new_mock() -> Self {
        Context {}
    }
}

/// Compute a stable 10-character hash for a PodTemplateSpec
///
/// This mimics Kubernetes' pod-template-hash label behavior:
/// - Serialize the template to JSON (deterministic)
/// - Hash the JSON bytes
/// - Return 10-character hex string
pub fn compute_pod_template_hash(template: &PodTemplateSpec) -> String {
    // Serialize template to JSON for stable hashing
    let json = serde_json::to_string(template).expect("Failed to serialize PodTemplateSpec");

    // Hash the JSON string
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    let hash = hasher.finish();

    // Return 10-character hex string (like Kubernetes)
    format!("{:x}", hash)[..10].to_string()
}

/// Reconcile function for Rollout controller
pub async fn reconcile(
    _rollout: Arc<Rollout>,
    _ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    // Minimal implementation - just return success
    // GREEN phase: make the test pass

    Ok(Action::requeue(Duration::from_secs(300)))
}

#[cfg(test)]
#[path = "rollout_test.rs"]
mod tests;
