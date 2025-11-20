use k8s_openapi::api::core::v1::PodTemplateSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Rollout is a Custom Resource for managing progressive delivery
///
/// Compatible with Argo Rollouts API for easy migration
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "kulta.io",
    version = "v1alpha1",
    kind = "Rollout",
    namespaced,
    status = "RolloutStatus",
    printcolumn = r#"{"name":"Desired", "type":"integer", "jsonPath":".spec.replicas"}"#,
    printcolumn = r#"{"name":"Current", "type":"integer", "jsonPath":".status.replicas"}"#,
    printcolumn = r#"{"name":"Ready", "type":"integer", "jsonPath":".status.ready_replicas"}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "jsonPath":".metadata.creationTimestamp"}"#
)]
pub struct RolloutSpec {
    /// Number of desired pods
    #[serde(default = "default_replicas")]
    pub replicas: i32,

    /// Label selector for pods
    #[schemars(schema_with = "any_object")]
    pub selector: LabelSelector,

    /// Template describes the pods that will be created
    #[schemars(schema_with = "any_object")]
    pub template: PodTemplateSpec,

    /// Deployment strategy (currently only canary)
    pub strategy: RolloutStrategy,
}

fn default_replicas() -> i32 {
    1
}

fn any_object(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    use schemars::schema::*;
    use serde_json::json;

    SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        extensions: {
            let mut ext = std::collections::BTreeMap::new();
            ext.insert(
                "x-kubernetes-preserve-unknown-fields".to_string(),
                json!(true),
            );
            ext
        },
        ..Default::default()
    }
    .into()
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct RolloutStrategy {
    /// Canary deployment strategy
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canary: Option<CanaryStrategy>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct CanaryStrategy {
    /// Name of the service that selects canary pods
    #[serde(rename = "canaryService")]
    pub canary_service: String,

    /// Name of the service that selects stable pods
    #[serde(rename = "stableService")]
    pub stable_service: String,

    /// Steps define the canary rollout progression
    #[serde(default)]
    pub steps: Vec<CanaryStep>,

    /// Traffic routing configuration
    #[serde(rename = "trafficRouting", skip_serializing_if = "Option::is_none")]
    pub traffic_routing: Option<TrafficRouting>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct CanaryStep {
    /// Set the percentage of traffic to route to canary
    #[serde(rename = "setWeight", skip_serializing_if = "Option::is_none")]
    pub set_weight: Option<i32>,

    /// Pause the rollout
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause: Option<PauseDuration>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct PauseDuration {
    /// Duration in seconds (e.g., "30s", "5m")
    /// If not specified, pauses indefinitely until manually resumed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct TrafficRouting {
    /// Gateway API configuration (KULTA-specific)
    #[serde(rename = "gatewayAPI", skip_serializing_if = "Option::is_none")]
    pub gateway_api: Option<GatewayAPIRouting>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct GatewayAPIRouting {
    /// Name of the HTTPRoute to manipulate
    #[serde(rename = "httpRoute")]
    pub http_route: String,
}

/// Status of the Rollout
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, JsonSchema)]
pub struct RolloutStatus {
    /// Total number of non-terminated pods
    #[serde(default)]
    pub replicas: i32,

    /// Number of ready replicas
    #[serde(rename = "readyReplicas", default)]
    pub ready_replicas: i32,

    /// Number of updated replicas (canary)
    #[serde(rename = "updatedReplicas", default)]
    pub updated_replicas: i32,

    /// Current canary step index (0-indexed)
    #[serde(rename = "currentStepIndex", skip_serializing_if = "Option::is_none")]
    pub current_step_index: Option<i32>,

    /// Current canary weight percentage
    #[serde(rename = "currentWeight", skip_serializing_if = "Option::is_none")]
    pub current_weight: Option<i32>,

    /// Phase of the rollout (Initializing, Progressing, Paused, Completed, Failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// Human-readable message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Timestamp when current pause started (RFC3339 format)
    #[serde(rename = "pauseStartTime", skip_serializing_if = "Option::is_none")]
    pub pause_start_time: Option<String>,
}

#[cfg(test)]
#[path = "rollout_test.rs"]
mod tests;
