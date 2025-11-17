use crate::crd::rollout::Rollout;
use futures::FutureExt;
use k8s_openapi::api::apps::v1::{ReplicaSet, ReplicaSetSpec};
use k8s_openapi::api::core::v1::PodTemplateSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::ObjectMeta;
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
    pub client: kube::Client,
}

impl Context {
    pub fn new(client: kube::Client) -> Self {
        Context { client }
    }

    pub fn new_mock() -> Self {
        // For testing, create a mock client
        // In real tests, we'd use a fake API server
        Context {
            client: kube::Client::try_default()
                .now_or_never()
                .unwrap()
                .unwrap_or_else(|_| {
                    // If no kubeconfig, create a dummy client for unit tests
                    panic!("Mock context requires kubeconfig or test environment")
                }),
        }
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

/// Build a ReplicaSet for a Rollout
///
/// Creates a ReplicaSet with:
/// - Name: {rollout-name}-{type} (e.g., "my-app-stable", "my-app-canary")
/// - Labels: pod-template-hash, rollouts.kulta.io/type
/// - Spec: from Rollout's template
pub fn build_replicaset(rollout: &Rollout, rs_type: &str, replicas: i32) -> ReplicaSet {
    let rollout_name = rollout
        .metadata
        .name
        .as_ref()
        .expect("Rollout must have name");
    let namespace = rollout.metadata.namespace.clone();

    // Compute pod template hash
    let pod_template_hash = compute_pod_template_hash(&rollout.spec.template);

    // Clone the pod template and add labels
    let mut template = rollout.spec.template.clone();
    let mut labels = template
        .metadata
        .as_ref()
        .and_then(|m| m.labels.clone())
        .unwrap_or_default();

    labels.insert("pod-template-hash".to_string(), pod_template_hash.clone());
    labels.insert("rollouts.kulta.io/type".to_string(), rs_type.to_string());

    // Update template metadata
    let mut template_metadata = template.metadata.unwrap_or_default();
    template_metadata.labels = Some(labels.clone());
    template.metadata = Some(template_metadata);

    // Build selector (must match pod labels)
    let selector = LabelSelector {
        match_labels: Some(labels.clone()),
        ..Default::default()
    };

    // Build ReplicaSet
    ReplicaSet {
        metadata: ObjectMeta {
            name: Some(format!("{}-{}", rollout_name, rs_type)),
            namespace,
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(ReplicaSetSpec {
            replicas: Some(replicas),
            selector,
            template: Some(template),
            ..Default::default()
        }),
        status: None,
    }
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
