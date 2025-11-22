use crate::crd::rollout::{Phase, Rollout};
use chrono::{DateTime, Utc};
#[cfg(test)]
use futures::FutureExt; // Only used in test helper new_mock()
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
use tracing::{error, info, warn};

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    KubeError(#[from] kube::Error),

    #[error("Rollout missing namespace")]
    MissingNamespace,

    #[error("Rollout missing name")]
    MissingName,

    #[error("ReplicaSet missing name in metadata")]
    ReplicaSetMissingName,

    #[error("Failed to serialize PodTemplateSpec: {0}")]
    SerializationError(String),
}

pub struct Context {
    pub client: kube::Client,
}

impl Context {
    pub fn new(client: kube::Client) -> Self {
        Context { client }
    }

    #[cfg(test)]
    #[allow(clippy::unwrap_used)] // Test helper - panicking is acceptable
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
///
/// # Errors
/// Returns SerializationError if PodTemplateSpec cannot be serialized to JSON
pub fn compute_pod_template_hash(template: &PodTemplateSpec) -> Result<String, ReconcileError> {
    // Serialize template to JSON for stable hashing
    let json = serde_json::to_string(template)
        .map_err(|e| ReconcileError::SerializationError(e.to_string()))?;

    // Hash the JSON string
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    let hash = hasher.finish();

    // Return 10-character hex string (like Kubernetes)
    Ok(format!("{:x}", hash)[..10].to_string())
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
    let rs_name = rs
        .metadata
        .name
        .as_ref()
        .ok_or(ReconcileError::ReplicaSetMissingName)?;

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

/// Initialize RolloutStatus for a new Rollout
///
/// Sets up initial status with:
/// - current_step_index = 0 (first step)
/// - phase = "Progressing"
/// - current_weight from first step's setWeight
///
/// # Arguments
/// * `rollout` - The Rollout to initialize status for
///
/// # Returns
/// RolloutStatus with initial values
pub fn initialize_rollout_status(rollout: &Rollout) -> crate::crd::rollout::RolloutStatus {
    use crate::crd::rollout::RolloutStatus;

    // Get canary strategy
    let canary_strategy = match &rollout.spec.strategy.canary {
        Some(strategy) => strategy,
        None => {
            // No canary strategy - return default status
            return RolloutStatus::default();
        }
    };

    // Get first step
    let first_step = canary_strategy.steps.first();

    // Get weight from first step (step 0)
    let first_step_weight = first_step.and_then(|step| step.set_weight).unwrap_or(0);

    // Check if first step has pause - set pause start time
    let pause_start_time = if let Some(step) = first_step {
        if step.pause.is_some() {
            // Set pause start time to now (RFC3339)
            Some(Utc::now().to_rfc3339())
        } else {
            None
        }
    } else {
        None
    };

    RolloutStatus {
        current_step_index: Some(0),
        current_weight: Some(first_step_weight),
        phase: Some(Phase::Progressing),
        message: Some(format!(
            "Starting canary rollout at step 0 ({}% traffic)",
            first_step_weight
        )),
        pause_start_time,
        ..Default::default()
    }
}

/// Check if rollout should progress to next step
///
/// Returns true if:
/// - Current step has no pause defined
/// - Phase is not "Paused"
///
/// # Arguments
/// * `rollout` - The Rollout to check
///
/// # Returns
/// true if should progress, false if should wait
pub fn should_progress_to_next_step(rollout: &Rollout) -> bool {
    // Get current status
    let status = match &rollout.status {
        Some(status) => status,
        None => return false, // No status yet, can't progress
    };

    // If phase is Paused, don't progress
    if status.phase == Some(Phase::Paused) {
        return false;
    }

    // Get current step index
    let current_step_index = match status.current_step_index {
        Some(idx) => idx,
        None => return false, // No step index, can't progress
    };

    // Get canary strategy
    let canary_strategy = match &rollout.spec.strategy.canary {
        Some(strategy) => strategy,
        None => return false, // No canary strategy
    };

    // Get current step
    let current_step = match canary_strategy.steps.get(current_step_index as usize) {
        Some(step) => step,
        None => return false, // Invalid step index
    };

    // Check if current step has pause
    if let Some(pause) = &current_step.pause {
        // Check for manual promotion annotation
        if has_promote_annotation(rollout) {
            return true; // Manual promotion overrides pause
        }

        // If pause has duration, check if elapsed
        if let Some(duration_str) = &pause.duration {
            if let Some(duration) = parse_duration(duration_str) {
                // Check if pause started
                if let Some(pause_start_str) = &status.pause_start_time {
                    // Parse pause start time (RFC3339)
                    if let Ok(pause_start) = DateTime::parse_from_rfc3339(pause_start_str) {
                        let now = Utc::now();
                        let elapsed = now.signed_duration_since(pause_start);

                        // If duration elapsed, can progress
                        if elapsed.num_seconds() >= duration.as_secs() as i64 {
                            return true;
                        }
                    }
                }
            }
        }

        // Pause is active and duration not elapsed
        return false;
    }

    // No pause - can progress
    true
}

/// Compute the desired status for a Rollout
///
/// This is the main function called by reconcile() to determine what status
/// should be written to K8s. It orchestrates initialization and progression.
///
/// Logic:
/// - If no status: initialize with step 0
/// - If status exists and should progress: advance to next step
/// - Otherwise: keep current status
///
/// # Arguments
/// * `rollout` - The Rollout to compute status for
///
/// # Returns
/// The desired RolloutStatus that should be written to K8s
pub fn compute_desired_status(rollout: &Rollout) -> crate::crd::rollout::RolloutStatus {
    // If no status, initialize
    if rollout.status.is_none() {
        return initialize_rollout_status(rollout);
    }

    // If should progress, advance to next step
    if should_progress_to_next_step(rollout) {
        return advance_to_next_step(rollout);
    }

    // Otherwise, return current status (no change)
    // This should always exist since we checked is_none() above, but use unwrap_or_default for safety
    rollout.status.as_ref().cloned().unwrap_or_default()
}

/// Advance rollout to next step
///
/// Calculates new status with:
/// - current_step_index incremented
/// - current_weight from new step
/// - phase = "Completed" if last step, else "Progressing"
///
/// # Arguments
/// * `rollout` - The Rollout to advance
///
/// # Returns
/// New RolloutStatus with updated step
pub fn advance_to_next_step(rollout: &Rollout) -> crate::crd::rollout::RolloutStatus {
    use crate::crd::rollout::RolloutStatus;

    // Get current status
    let current_status = match &rollout.status {
        Some(status) => status,
        None => {
            // No status yet - initialize
            return initialize_rollout_status(rollout);
        }
    };

    // Get current step index
    let current_step_index = current_status.current_step_index.unwrap_or(-1);
    let next_step_index = current_step_index + 1;

    // Get canary strategy
    let canary_strategy = match &rollout.spec.strategy.canary {
        Some(strategy) => strategy,
        None => {
            // No canary strategy - return current status
            return current_status.clone();
        }
    };

    // Check if next step exists
    if next_step_index as usize >= canary_strategy.steps.len() {
        // Reached end of steps - mark as completed
        return RolloutStatus {
            current_step_index: Some(next_step_index),
            current_weight: Some(100),
            phase: Some(Phase::Completed),
            message: Some("Rollout completed: 100% traffic to canary".to_string()),
            ..current_status.clone()
        };
    }

    // Get weight from next step
    let next_step = &canary_strategy.steps[next_step_index as usize];
    let next_weight = next_step.set_weight.unwrap_or(0);

    // Check if this is the final step (100% canary)
    let (phase, message) = if next_weight == 100 {
        (
            Phase::Completed,
            "Rollout completed: 100% traffic to canary".to_string(),
        )
    } else {
        (
            Phase::Progressing,
            format!(
                "Advanced to step {} ({}% traffic)",
                next_step_index, next_weight
            ),
        )
    };

    // Check if next step has pause - set pause start time
    let pause_start_time = if next_step.pause.is_some() {
        // Set pause start time to now (RFC3339)
        Some(Utc::now().to_rfc3339())
    } else {
        // Clear pause start time if no pause
        None
    };

    RolloutStatus {
        current_step_index: Some(next_step_index),
        current_weight: Some(next_weight),
        phase: Some(phase),
        message: Some(message),
        pause_start_time,
        ..current_status.clone()
    }
}

/// Build a ReplicaSet for a Rollout
///
/// Creates a ReplicaSet with:
/// - Name: {rollout-name}-{type} (e.g., "my-app-stable", "my-app-canary")
/// - Labels: pod-template-hash, rollouts.kulta.io/type, rollouts.kulta.io/managed
/// - Spec: from Rollout's template
///
/// The `rollouts.kulta.io/managed=true` label prevents Kubernetes Deployment
/// controllers from adopting KULTA-managed ReplicaSets.
///
/// # Errors
/// Returns error if Rollout is missing name or if PodTemplateSpec cannot be serialized
pub fn build_replicaset(
    rollout: &Rollout,
    rs_type: &str,
    replicas: i32,
) -> Result<ReplicaSet, ReconcileError> {
    let rollout_name = rollout
        .metadata
        .name
        .as_ref()
        .ok_or(ReconcileError::MissingName)?;
    let namespace = rollout.metadata.namespace.clone();

    // Compute pod template hash
    let pod_template_hash = compute_pod_template_hash(&rollout.spec.template)?;

    // Clone the pod template and add labels
    let mut template = rollout.spec.template.clone();
    let mut labels = template
        .metadata
        .as_ref()
        .and_then(|m| m.labels.clone())
        .unwrap_or_default();

    labels.insert("pod-template-hash".to_string(), pod_template_hash.clone());
    labels.insert("rollouts.kulta.io/type".to_string(), rs_type.to_string());
    labels.insert("rollouts.kulta.io/managed".to_string(), "true".to_string());

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
    Ok(ReplicaSet {
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
    })
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

    // Create ReplicaSet API client
    let rs_api: Api<ReplicaSet> = Api::namespaced(ctx.client.clone(), &namespace);

    info!(
        rollout = ?name,
        desired_replicas = rollout.spec.replicas,
        "Building ReplicaSets"
    );

    // Build and ensure stable ReplicaSet exists
    let stable_rs = build_replicaset(&rollout, "stable", rollout.spec.replicas)?;
    info!(
        rollout = ?name,
        rs_type = "stable",
        spec_replicas = ?stable_rs.spec.as_ref().and_then(|s| s.replicas),
        "Built stable ReplicaSet"
    );
    ensure_replicaset_exists(&rs_api, &stable_rs, "stable", rollout.spec.replicas).await?;

    // Build and ensure canary ReplicaSet exists (0 replicas initially)
    let canary_rs = build_replicaset(&rollout, "canary", 0)?;
    info!(
        rollout = ?name,
        rs_type = "canary",
        spec_replicas = ?canary_rs.spec.as_ref().and_then(|s| s.replicas),
        "Built canary ReplicaSet"
    );
    ensure_replicaset_exists(&rs_api, &canary_rs, "canary", 0).await?;

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
                // NOTE: KULTA assumes the HTTPRoute has exactly one rule for traffic splitting
                // This Merge patch only updates the backendRefs field of the first rule,
                // preserving other fields like matches, filters, etc.
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

                let httproute_api: Api<DynamicObject> =
                    Api::namespaced_with(ctx.client.clone(), &namespace, &ar);

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
                        warn!(
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

    // Check if promote annotation exists BEFORE computing status (to avoid race condition)
    let had_promote_annotation = has_promote_annotation(&rollout);
    let was_paused_before = rollout
        .status
        .as_ref()
        .map(|s| s.phase == Some(Phase::Paused))
        .unwrap_or(false);

    // Compute desired status (initialize or progress steps)
    let desired_status = compute_desired_status(&rollout);

    // Determine if we actually progressed due to the annotation
    // Only remove annotation if: had annotation AND was paused AND now progressing
    let progressed_due_to_annotation = had_promote_annotation
        && was_paused_before
        && rollout.status.as_ref() != Some(&desired_status);

    // Update Rollout status in Kubernetes if it changed
    if rollout.status.as_ref() != Some(&desired_status) {
        info!(
            rollout = ?name,
            current_step = ?desired_status.current_step_index,
            current_weight = ?desired_status.current_weight,
            phase = ?desired_status.phase,
            "Updating Rollout status"
        );

        // Create Rollout API client
        use kube::api::{Api, Patch, PatchParams};
        let rollout_api: Api<Rollout> = Api::namespaced(ctx.client.clone(), &namespace);

        // Create status subresource patch
        let status_patch = serde_json::json!({
            "status": desired_status
        });

        // Patch the status subresource
        match rollout_api
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status_patch))
            .await
        {
            Ok(_) => {
                info!(
                    rollout = ?name,
                    "Status updated successfully"
                );

                // Remove promote annotation only if it was actually used for progression
                if progressed_due_to_annotation {
                    info!(
                        rollout = ?name,
                        "Removing promote annotation after successful promotion"
                    );

                    // Create patch to remove the annotation
                    let remove_annotation_patch = serde_json::json!({
                        "metadata": {
                            "annotations": {
                                "kulta.io/promote": serde_json::Value::Null
                            }
                        }
                    });

                    match rollout_api
                        .patch(
                            &name,
                            &PatchParams::default(),
                            &Patch::Merge(&remove_annotation_patch),
                        )
                        .await
                    {
                        Ok(_) => {
                            info!(
                                rollout = ?name,
                                "Promote annotation removed successfully"
                            );
                        }
                        Err(e) => {
                            warn!(
                                error = ?e,
                                rollout = ?name,
                                "Failed to remove promote annotation (non-fatal)"
                            );
                            // Don't fail reconciliation if annotation removal fails
                        }
                    }
                }
            }
            Err(e) => {
                error!(
                    error = ?e,
                    rollout = ?name,
                    "Failed to update status"
                );
                return Err(ReconcileError::KubeError(e));
            }
        }
    }

    // Requeue after 30 seconds for faster pause progression checks
    Ok(Action::requeue(Duration::from_secs(30)))
}

/// Parse a duration string like "5m", "30s", "1h" into std::time::Duration
///
/// Supported formats:
/// - "30s" → 30 seconds
/// - "5m" → 5 minutes
/// - "2h" → 2 hours
///
/// # Arguments
/// * `duration_str` - Duration string to parse
///
/// # Returns
/// Some(Duration) if parse successful, None if invalid format
pub fn parse_duration(duration_str: &str) -> Option<Duration> {
    let duration_str = duration_str.trim();

    if duration_str.is_empty() {
        return None;
    }

    // Get the last character (unit)
    let unit = duration_str.chars().last()?;

    // Get the numeric part
    let number_str = &duration_str[..duration_str.len() - 1];
    let number: u64 = number_str.parse().ok()?;

    match unit {
        's' => Some(Duration::from_secs(number)),
        'm' => Some(Duration::from_secs(number * 60)),
        'h' => Some(Duration::from_secs(number * 3600)),
        _ => None,
    }
}

/// Check if Rollout has the promote annotation (kulta.io/promote=true)
///
/// This annotation is used to manually promote a rollout that is paused.
/// When present with value "true", the controller will progress to the next step
/// regardless of pause duration.
///
/// # Arguments
/// * `rollout` - The Rollout to check
///
/// # Returns
/// true if annotation exists with value "true", false otherwise
fn has_promote_annotation(rollout: &Rollout) -> bool {
    rollout
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kulta.io/promote"))
        .map(|value| value == "true")
        .unwrap_or(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Tests can use unwrap/expect for brevity
#[path = "rollout_test.rs"]
mod tests;
