use crate::crd::rollout::Rollout;
use futures::FutureExt;
use k8s_openapi::api::apps::v1::{ReplicaSet, ReplicaSetSpec};
use k8s_openapi::api::core::v1::PodTemplateSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{Api, ObjectMeta, PostParams};
use kube::runtime::controller::Action;
use kube::ResourceExt;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::{error, info};

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    KubeError(#[from] kube::Error),

    #[error("Rollout missing namespace")]
    MissingNamespace,

    #[error("Rollout missing name")]
    MissingName,
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

/// Ensure a ReplicaSet exists (create if missing)
///
/// This function is idempotent - it will:
/// - Return Ok if ReplicaSet already exists
/// - Create ReplicaSet if it doesn't exist (404)
/// - Return Err on other API errors
async fn ensure_replicaset_exists(
    rs_api: &Api<ReplicaSet>,
    rs: &ReplicaSet,
    rs_type: &str,
    replicas: i32,
) -> Result<(), ReconcileError> {
    let rs_name = rs.metadata.name.as_ref().unwrap();

    match rs_api.get(rs_name).await {
        Ok(_existing) => {
            // Already exists
            info!(
                replicaset = ?rs_name,
                rs_type = rs_type,
                "ReplicaSet already exists"
            );
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            // Not found, create it
            info!(
                replicaset = ?rs_name,
                rs_type = rs_type,
                replicas = replicas,
                "Creating ReplicaSet"
            );

            rs_api.create(&PostParams::default(), rs).await?;

            info!(
                replicaset = ?rs_name,
                rs_type = rs_type,
                "ReplicaSet created successfully"
            );
        }
        Err(e) => {
            error!(
                error = ?e,
                replicaset = ?rs_name,
                rs_type = rs_type,
                "Failed to get ReplicaSet"
            );
            return Err(ReconcileError::KubeError(e));
        }
    }

    Ok(())
}

/// Simple representation of HTTPBackendRef for testing
///
/// This is a simplified version of Gateway API HTTPBackendRef
/// focused on what we need for weight-based traffic splitting
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HTTPBackendRef {
    /// Name of the Kubernetes Service
    pub name: String,

    /// Port number on the service
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<i32>,

    /// Weight for traffic splitting (0-100)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<i32>,
}

/// Build HTTPRoute backendRefs with weights from Rollout
///
/// Creates a list of backend references with calculated weights:
/// - Stable service with calculated stable_weight
/// - Canary service with calculated canary_weight
///
/// # Returns
/// Vec of HTTPBackendRef with correct weights for current rollout step
pub fn build_backend_refs_with_weights(rollout: &Rollout) -> Vec<HTTPBackendRef> {
    // Get canary strategy
    let canary_strategy = match &rollout.spec.strategy.canary {
        Some(strategy) => strategy,
        None => return vec![], // No canary strategy
    };

    // Calculate current weights
    let (stable_weight, canary_weight) = calculate_traffic_weights(rollout);

    vec![
        HTTPBackendRef {
            name: canary_strategy.stable_service.clone(),
            port: Some(80), // Default HTTP port
            weight: Some(stable_weight),
        },
        HTTPBackendRef {
            name: canary_strategy.canary_service.clone(),
            port: Some(80),
            weight: Some(canary_weight),
        },
    ]
}

/// Build Gateway API HTTPRouteRulesBackendRefs with weights from Rollout
///
/// Converts our simple HTTPBackendRef representation to the actual Gateway API
/// HTTPRouteRulesBackendRefs type used in HTTPRoute resources.
///
/// # Returns
/// Vec of HTTPRouteRulesBackendRefs with correct weights for current rollout step
pub fn build_gateway_api_backend_refs(
    rollout: &Rollout,
) -> Vec<gateway_api::apis::standard::httproutes::HTTPRouteRulesBackendRefs> {
    use gateway_api::apis::standard::httproutes::HTTPRouteRulesBackendRefs;

    // Get canary strategy
    let canary_strategy = match &rollout.spec.strategy.canary {
        Some(strategy) => strategy,
        None => return vec![], // No canary strategy
    };

    // Calculate current weights
    let (stable_weight, canary_weight) = calculate_traffic_weights(rollout);

    vec![
        HTTPRouteRulesBackendRefs {
            name: canary_strategy.stable_service.clone(),
            port: Some(80), // Default HTTP port
            weight: Some(stable_weight),
            kind: Some("Service".to_string()),
            group: Some("".to_string()), // Core API group (empty string)
            namespace: None,             // Same namespace as HTTPRoute
            filters: None,               // No filters for now
        },
        HTTPRouteRulesBackendRefs {
            name: canary_strategy.canary_service.clone(),
            port: Some(80),
            weight: Some(canary_weight),
            kind: Some("Service".to_string()),
            group: Some("".to_string()),
            namespace: None,
            filters: None,
        },
    ]
}

/// Update HTTPRoute's backend refs with weighted backends from Rollout
///
/// This function mutates the HTTPRoute by updating the first rule's backend_refs
/// with the weighted backends calculated from the Rollout's current step.
///
/// # Arguments
/// * `rollout` - The Rollout resource with traffic weights
/// * `httproute` - The HTTPRoute resource to update (mutated in place)
///
/// # Behavior
/// - Updates the first rule's backend_refs (assumes single rule)
/// - Replaces existing backend_refs with weighted stable + canary
/// - Uses build_gateway_api_backend_refs() for the conversion
pub fn update_httproute_backends(
    rollout: &Rollout,
    httproute: &mut gateway_api::apis::standard::httproutes::HTTPRoute,
) {
    // Get the weighted backend refs from rollout
    let backend_refs = build_gateway_api_backend_refs(rollout);

    // Update the first rule's backend_refs
    // (KULTA assumes HTTPRoute has exactly one rule - the traffic splitting rule)
    if let Some(rules) = httproute.spec.rules.as_mut() {
        if let Some(first_rule) = rules.first_mut() {
            first_rule.backend_refs = Some(backend_refs);
        }
    }
}

/// Calculate traffic weights for stable and canary based on Rollout status
///
/// Returns (stable_weight, canary_weight) as percentages
///
/// # Logic
/// - If no status or no currentStepIndex: 100% stable, 0% canary
/// - If currentStepIndex >= steps.len(): 100% canary, 0% stable (rollout complete)
/// - Otherwise: Use setWeight from steps[currentStepIndex]
pub fn calculate_traffic_weights(rollout: &Rollout) -> (i32, i32) {
    // Get canary strategy
    let canary_strategy = match &rollout.spec.strategy.canary {
        Some(strategy) => strategy,
        None => return (100, 0), // No canary strategy, 100% stable
    };

    // Get current step index from status
    let current_step_index = match &rollout.status {
        Some(status) => status.current_step_index.unwrap_or(-1),
        None => -1, // No status yet, 100% stable
    };

    // If no step is active, default to 100% stable
    if current_step_index < 0 {
        return (100, 0);
    }

    // If step index is beyond available steps, rollout is complete (100% canary)
    if current_step_index as usize >= canary_strategy.steps.len() {
        return (0, 100);
    }

    // Get the canary weight from the current step
    let canary_weight = canary_strategy.steps[current_step_index as usize]
        .set_weight
        .unwrap_or(0);

    let stable_weight = 100 - canary_weight;

    (stable_weight, canary_weight)
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

/// Reconcile a Rollout resource
///
/// This function implements the main reconciliation logic:
/// 1. Creates stable ReplicaSet if missing
/// 2. Handles errors gracefully (404 = create, other errors = fail)
///
/// # Arguments
/// * `rollout` - The Rollout resource to reconcile
/// * `ctx` - Controller context (k8s client)
///
/// # Returns
/// * `Ok(Action)` - Next reconciliation action (requeue after 5 minutes)
/// * `Err(ReconcileError)` - Reconciliation error
pub async fn reconcile(rollout: Arc<Rollout>, ctx: Arc<Context>) -> Result<Action, ReconcileError> {
    // Validate rollout has required fields
    let namespace = rollout
        .namespace()
        .ok_or(ReconcileError::MissingNamespace)?;
    let name = rollout.name_any();

    info!(
        rollout = ?name,
        namespace = ?namespace,
        "Reconciling Rollout"
    );

    // TEMPORARILY DISABLED: ReplicaSet creation
    // (Template data not persisting due to CRD schema issue - will fix with x-kubernetes-preserve-unknown-fields)
    // For now, testing HTTPRoute update feature only
    /*
    // Create ReplicaSet API client
    let rs_api: Api<ReplicaSet> = Api::namespaced(ctx.client.clone(), &namespace);

    // Build and ensure stable ReplicaSet exists
    let stable_rs = build_replicaset(&rollout, "stable", rollout.spec.replicas);
    ensure_replicaset_exists(&rs_api, &stable_rs, "stable", rollout.spec.replicas).await?;

    // Build and ensure canary ReplicaSet exists (0 replicas initially)
    let canary_rs = build_replicaset(&rollout, "canary", 0);
    ensure_replicaset_exists(&rs_api, &canary_rs, "canary", 0).await?;
    */

    // Update HTTPRoute with weighted backends (if configured)
    if let Some(canary_strategy) = &rollout.spec.strategy.canary {
        if let Some(traffic_routing) = &canary_strategy.traffic_routing {
            if let Some(gateway_api_routing) = &traffic_routing.gateway_api {
                let httproute_name = &gateway_api_routing.http_route;

                info!(
                    rollout = ?name,
                    httproute = ?httproute_name,
                    "Updating HTTPRoute with weighted backends"
                );

                // Build the weighted backend refs
                let backend_refs = build_gateway_api_backend_refs(&rollout);

                // Create JSON patch to update HTTPRoute's first rule's backendRefs
                let patch_json = serde_json::json!({
                    "spec": {
                        "rules": [{
                            "backendRefs": backend_refs
                        }]
                    }
                });

                // Create HTTPRoute API client using DynamicObject
                use kube::api::{Api, Patch, PatchParams};
                use kube::core::DynamicObject;
                use kube::discovery::ApiResource;

                let ar = ApiResource {
                    group: "gateway.networking.k8s.io".to_string(),
                    version: "v1".to_string(),
                    api_version: "gateway.networking.k8s.io/v1".to_string(),
                    kind: "HTTPRoute".to_string(),
                    plural: "httproutes".to_string(),
                };

                let httproute_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), &namespace, &ar);

                // Apply the patch (Merge patch doesn't support force)
                match httproute_api
                    .patch(
                        httproute_name,
                        &PatchParams::default(),
                        &Patch::Merge(&patch_json),
                    )
                    .await
                {
                    Ok(_) => {
                        info!(
                            rollout = ?name,
                            httproute = ?httproute_name,
                            stable_weight = backend_refs.first().and_then(|b| b.weight),
                            canary_weight = backend_refs.get(1).and_then(|b| b.weight),
                            "HTTPRoute updated successfully"
                        );
                    }
                    Err(kube::Error::Api(err)) if err.code == 404 => {
                        error!(
                            rollout = ?name,
                            httproute = ?httproute_name,
                            "HTTPRoute not found - skipping traffic routing update"
                        );
                        // Don't fail the reconciliation, just skip HTTPRoute update
                    }
                    Err(e) => {
                        error!(
                            error = ?e,
                            rollout = ?name,
                            httproute = ?httproute_name,
                            "Failed to patch HTTPRoute"
                        );
                        return Err(ReconcileError::KubeError(e));
                    }
                }
            }
        }
    }

    // Requeue after 5 minutes
    Ok(Action::requeue(Duration::from_secs(300)))
}

#[cfg(test)]
#[path = "rollout_test.rs"]
mod tests;
