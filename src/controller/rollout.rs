use crate::controller::cdevents::emit_status_change_event;
use crate::controller::prometheus::PrometheusClient;
use crate::crd::rollout::{Phase, Rollout, RolloutStatus};
use chrono::{DateTime, Utc};
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

    #[error("Invalid Rollout spec: {0}")]
    ValidationError(String),
}

pub struct Context {
    pub client: kube::Client,
    pub cdevents_sink: Arc<crate::controller::cdevents::CDEventsSink>,
    pub prometheus_client: Arc<PrometheusClient>,
}

impl Context {
    pub fn new(
        client: kube::Client,
        cdevents_sink: crate::controller::cdevents::CDEventsSink,
        prometheus_client: PrometheusClient,
    ) -> Self {
        Context {
            client,
            cdevents_sink: Arc::new(cdevents_sink),
            prometheus_client: Arc::new(prometheus_client),
        }
    }

    #[cfg(test)]
    #[allow(clippy::unwrap_used)] // Test helper - panicking is acceptable
    pub fn new_mock() -> Self {
        // For testing, create a mock client that doesn't require kubeconfig
        // Use a minimal Config - the client won't actually be used in unit tests
        let mut config = kube::Config::new("https://localhost:8080".parse().unwrap());
        config.default_namespace = "default".to_string();
        config.accept_invalid_certs = true;

        let client = kube::Client::try_from(config).unwrap();

        Context {
            client,
            cdevents_sink: Arc::new(crate::controller::cdevents::CDEventsSink::new_mock()),
            prometheus_client: Arc::new(PrometheusClient::new_mock()),
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

/// Calculate how to split total replicas between stable and canary
///
/// Given total replicas and canary weight percentage, calculates:
/// - canary_replicas = ceil(total * weight / 100)
/// - stable_replicas = total - canary_replicas
///
/// # Arguments
/// * `total_replicas` - Total number of replicas desired (from rollout.spec.replicas)
/// * `canary_weight` - Percentage of traffic to canary (0-100)
///
/// # Returns
/// Tuple of (stable_replicas, canary_replicas)
///
/// # Examples
/// ```ignore
/// let (stable, canary) = calculate_replica_split(3, 0);
/// assert_eq!(stable, 3); // 0% weight → all stable
/// assert_eq!(canary, 0);
///
/// let (stable, canary) = calculate_replica_split(3, 50);
/// assert_eq!(stable, 1); // 50% of 3 → 1 stable, 2 canary (ceil)
/// assert_eq!(canary, 2);
/// ```
fn calculate_replica_split(total_replicas: i32, canary_weight: i32) -> (i32, i32) {
    // Calculate canary replicas (ceiling to ensure at least 1 if weight > 0)
    let canary_replicas = if canary_weight == 0 {
        0
    } else if canary_weight == 100 {
        total_replicas
    } else {
        ((total_replicas as f64 * canary_weight as f64) / 100.0).ceil() as i32
    };

    // Stable gets the remainder
    let stable_replicas = total_replicas - canary_replicas;

    (stable_replicas, canary_replicas)
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
        Ok(existing) => {
            // Check if replicas need scaling
            let current_replicas = existing.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);

            if current_replicas != replicas {
                // Replicas need updating - scale the ReplicaSet
                info!(
                    replicaset = ?rs_name,
                    rs_type = rs_type,
                    current = current_replicas,
                    desired = replicas,
                    "Scaling ReplicaSet"
                );

                // Create scale patch
                use kube::api::{Patch, PatchParams};
                let scale_patch = serde_json::json!({
                    "spec": {
                        "replicas": replicas
                    }
                });

                rs_api
                    .patch(
                        rs_name,
                        &PatchParams::default(),
                        &Patch::Merge(&scale_patch),
                    )
                    .await?;

                info!(
                    replicaset = ?rs_name,
                    rs_type = rs_type,
                    replicas = replicas,
                    "ReplicaSet scaled successfully"
                );
            } else {
                // Already at correct scale
                info!(
                    replicaset = ?rs_name,
                    rs_type = rs_type,
                    replicas = replicas,
                    "ReplicaSet already at correct scale"
                );
            }
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

/// Build a ReplicaSet for a simple strategy Rollout
///
/// Creates a single ReplicaSet (no stable/canary split) with:
/// - Name: {rollout-name} (no suffix)
/// - Labels: pod-template-hash, kulta.io/managed-by
/// - Spec: from Rollout's template
///
/// # Errors
/// Returns error if Rollout is missing name or if PodTemplateSpec cannot be serialized
pub fn build_replicaset_for_simple(
    rollout: &Rollout,
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
    labels.insert("kulta.io/managed-by".to_string(), "kulta".to_string());

    // Update template metadata
    let mut template_metadata = template.metadata.unwrap_or_default();
    template_metadata.labels = Some(labels.clone());
    template.metadata = Some(template_metadata);

    // Build selector (must match pod labels)
    let selector = LabelSelector {
        match_labels: Some(labels.clone()),
        ..Default::default()
    };

    // Build ReplicaSet - no suffix for simple strategy
    Ok(ReplicaSet {
        metadata: ObjectMeta {
            name: Some(rollout_name.clone()),
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

/// Validate Rollout specification
///
/// Validates runtime constraints that cannot be enforced via CRD schema.
/// This is necessary because our current CRD uses x-kubernetes-preserve-unknown-fields.
///
/// # Arguments
/// * `rollout` - The Rollout resource to validate
///
/// # Returns
/// * `Ok(())` - Validation passed
/// * `Err(String)` - Validation error message
fn validate_rollout(rollout: &Rollout) -> Result<(), String> {
    // Validate replicas >= 0
    if rollout.spec.replicas < 0 {
        return Err(format!(
            "spec.replicas must be >= 0, got {}",
            rollout.spec.replicas
        ));
    }

    // Validate canary strategy if present
    if let Some(canary) = &rollout.spec.strategy.canary {
        // Validate canary service name is not empty
        if canary.canary_service.is_empty() {
            return Err("spec.strategy.canary.canaryService cannot be empty".to_string());
        }

        // Validate stable service name is not empty
        if canary.stable_service.is_empty() {
            return Err("spec.strategy.canary.stableService cannot be empty".to_string());
        }

        // Validate each step
        for (i, step) in canary.steps.iter().enumerate() {
            // Validate weight is in 0-100 range
            if let Some(weight) = step.set_weight {
                if !(0..=100).contains(&weight) {
                    return Err(format!(
                        "steps[{}].setWeight must be 0-100, got {}",
                        i, weight
                    ));
                }
            }

            // Validate pause duration if present
            if let Some(pause) = &step.pause {
                if let Some(duration) = &pause.duration {
                    if parse_duration(duration).is_none() {
                        return Err(format!("steps[{}].pause.duration invalid: {}", i, duration));
                    }
                }
            }
        }

        // Validate traffic routing if present
        if let Some(traffic_routing) = &canary.traffic_routing {
            if let Some(gateway) = &traffic_routing.gateway_api {
                // Validate HTTPRoute name is not empty
                if gateway.http_route.is_empty() {
                    return Err(
                        "spec.strategy.canary.trafficRouting.gatewayAPI.httpRoute cannot be empty"
                            .to_string(),
                    );
                }
            }
        }
    }

    Ok(())
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

    // Validate Rollout spec (runtime validation since CRD has no schema)
    if let Err(validation_error) = validate_rollout(&rollout) {
        error!(
            rollout = ?name,
            error = ?validation_error,
            "Rollout spec validation failed"
        );
        return Err(ReconcileError::ValidationError(validation_error));
    }

    // Create ReplicaSet API client
    let rs_api: Api<ReplicaSet> = Api::namespaced(ctx.client.clone(), &namespace);

    // Calculate replica split based on current canary weight
    let current_weight = rollout
        .status
        .as_ref()
        .and_then(|s| s.current_weight)
        .unwrap_or(0);

    let (stable_replicas, canary_replicas) =
        calculate_replica_split(rollout.spec.replicas, current_weight);

    info!(
        rollout = ?name,
        total_replicas = rollout.spec.replicas,
        current_weight = current_weight,
        stable_replicas = stable_replicas,
        canary_replicas = canary_replicas,
        "Calculated ReplicaSet scaling"
    );

    // Build and ensure stable ReplicaSet exists with calculated replicas
    let stable_rs = build_replicaset(&rollout, "stable", stable_replicas)?;
    info!(
        rollout = ?name,
        rs_type = "stable",
        spec_replicas = ?stable_rs.spec.as_ref().and_then(|s| s.replicas),
        "Built stable ReplicaSet"
    );
    ensure_replicaset_exists(&rs_api, &stable_rs, "stable", stable_replicas).await?;

    // Build and ensure canary ReplicaSet exists with calculated replicas
    let canary_rs = build_replicaset(&rollout, "canary", canary_replicas)?;
    info!(
        rollout = ?name,
        rs_type = "canary",
        spec_replicas = ?canary_rs.spec.as_ref().and_then(|s| s.replicas),
        "Built canary ReplicaSet"
    );
    ensure_replicaset_exists(&rs_api, &canary_rs, "canary", canary_replicas).await?;

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

        // Emit CDEvent for status change (non-fatal if it fails)
        if let Err(e) = emit_status_change_event(
            &rollout,
            &rollout.status,
            &desired_status,
            &ctx.cdevents_sink,
        )
        .await
        {
            warn!(
                error = ?e,
                rollout = ?name,
                "Failed to emit CDEvent (non-fatal, continuing)"
            );
        }

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

    // Calculate optimal requeue interval based on pause state
    let requeue_interval = calculate_requeue_interval_from_rollout(&rollout, &desired_status);
    Ok(Action::requeue(requeue_interval))
}

/// Calculate optimal requeue interval based on rollout pause state
///
/// This function reduces unnecessary API calls by calculating the next check time
/// based on the pause duration and elapsed time.
///
/// # Arguments
/// * `pause_start` - Optional pause start timestamp
/// * `pause_duration` - Optional pause duration
///
/// # Returns
/// * Optimal requeue interval (minimum 5s, maximum 300s)
///
/// # Examples
/// ```ignore
/// use chrono::{Utc, Duration as ChronoDuration};
/// use std::time::Duration;
///
/// // Paused with 10s duration, 2s elapsed
/// let pause_start = Utc::now() - ChronoDuration::seconds(2);
/// let pause_duration = Duration::from_secs(10);
/// let interval = calculate_requeue_interval(Some(&pause_start), Some(pause_duration));
/// assert!(interval.as_secs() >= 8 && interval.as_secs() <= 10);
///
/// // Not paused
/// let interval = calculate_requeue_interval(None, None);
/// assert_eq!(interval, Duration::from_secs(30));
/// ```
fn calculate_requeue_interval(
    pause_start: Option<&DateTime<Utc>>,
    pause_duration: Option<Duration>,
) -> Duration {
    const MIN_REQUEUE: Duration = Duration::from_secs(5); // Minimum 5s
    const MAX_REQUEUE: Duration = Duration::from_secs(300); // Maximum 5min
    const DEFAULT_REQUEUE: Duration = Duration::from_secs(30); // Default 30s

    match (pause_start, pause_duration) {
        (Some(start), Some(duration)) => {
            // Calculate elapsed time since pause started
            let now = Utc::now();
            let elapsed = now.signed_duration_since(*start);
            let elapsed_secs = elapsed.num_seconds().max(0) as u64;

            // Calculate remaining time until pause completes
            let remaining_secs = duration.as_secs().saturating_sub(elapsed_secs);

            // Clamp to MIN..MAX range
            let optimal = Duration::from_secs(remaining_secs);
            optimal.clamp(MIN_REQUEUE, MAX_REQUEUE)
        }
        _ => {
            // No pause or manual pause → use default interval
            DEFAULT_REQUEUE
        }
    }
}

/// Helper to extract pause information from Rollout and RolloutStatus
fn calculate_requeue_interval_from_rollout(rollout: &Rollout, status: &RolloutStatus) -> Duration {
    let pause_start = status
        .pause_start_time
        .as_ref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    // Get current step's pause duration
    let pause_duration = status.current_step_index.and_then(|step_index| {
        rollout
            .spec
            .strategy
            .canary
            .as_ref()
            .and_then(|canary| canary.steps.get(step_index as usize))
            .and_then(|step| step.pause.as_ref())
            .and_then(|pause| pause.duration.as_ref())
            .and_then(|dur_str| parse_duration(dur_str))
    });

    calculate_requeue_interval(pause_start.as_ref(), pause_duration)
}

/// Parse a duration string like "5m", "30s", "1h" into std::time::Duration
///
/// Supported formats:
/// - "30s" → 30 seconds (max 24h = 86400s)
/// - "5m" → 5 minutes (max 24h = 1440m)
/// - "2h" → 2 hours (max 1 week = 168h)
///
/// # Validation Rules
/// - Zero duration is rejected (minimum 1s)
/// - Seconds limited to 24h (86400s) - use hours for longer durations
/// - Minutes limited to 24h (1440m) - use hours for longer durations
/// - Hours limited to 1 week (168h) - prevents typos like "999999h"
///
/// # Arguments
/// * `duration_str` - Duration string to parse
///
/// # Returns
/// Some(Duration) if parse successful and within limits, None if invalid or out of range
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

    // Reject zero duration
    if number == 0 {
        return None;
    }

    // Validate and convert based on unit
    match unit {
        's' => {
            // Seconds: max 24h (86400s)
            if number <= 86400 {
                Some(Duration::from_secs(number))
            } else {
                None // Reject: use hours for durations > 24h
            }
        }
        'm' => {
            // Minutes: max 24h (1440m)
            // Use checked_mul to prevent overflow
            if number <= 1440 {
                number.checked_mul(60).map(Duration::from_secs)
            } else {
                None // Reject: use hours for durations > 24h
            }
        }
        'h' => {
            // Hours: max 1 week (168h)
            // Use checked_mul to prevent overflow
            if number <= 168 {
                number.checked_mul(3600).map(Duration::from_secs)
            } else {
                None // Reject: likely a typo (e.g., "8760h" = 1 year)
            }
        }
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
