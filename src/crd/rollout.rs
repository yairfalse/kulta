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
    pub selector: LabelSelector,

    /// Template describes the pods that will be created
    pub template: PodTemplateSpec,

    /// Deployment strategy (currently only canary)
    pub strategy: RolloutStrategy,
}

fn default_replicas() -> i32 {
    1
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

    /// Analysis configuration for automated metrics-based rollback
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<AnalysisConfig>,
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

/// Analysis configuration for automated rollback based on metrics
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct AnalysisConfig {
    /// Prometheus configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prometheus: Option<PrometheusConfig>,

    /// Warmup duration before starting metrics analysis (e.g., "1m", "30s")
    #[serde(rename = "warmupDuration", skip_serializing_if = "Option::is_none")]
    pub warmup_duration: Option<String>,

    /// List of metrics to monitor
    #[serde(default)]
    pub metrics: Vec<MetricConfig>,
}

/// Prometheus configuration
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct PrometheusConfig {
    /// Prometheus server address (e.g., "http://prometheus:9090")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// Metric configuration for analysis
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct MetricConfig {
    /// Metric name/template (error-rate, latency-p95, latency-p99)
    pub name: String,

    /// Threshold value (metric must be below this)
    pub threshold: f64,

    /// Check interval (e.g., "30s", "1m")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,

    /// Number of consecutive failures before rollback
    #[serde(rename = "failureThreshold", skip_serializing_if = "Option::is_none")]
    pub failure_threshold: Option<i32>,

    /// Minimum sample size required for metric evaluation
    #[serde(rename = "minSampleSize", skip_serializing_if = "Option::is_none")]
    pub min_sample_size: Option<i32>,
}

/// Phase of a Rollout
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum Phase {
    #[default]
    Initializing,
    Progressing,
    Paused,
    Completed,
    Failed,
}

/// Action taken by the controller
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum DecisionAction {
    Initialize,
    StepAdvance,
    Promotion,
    Rollback,
    Pause,
    Resume,
    Complete,
}

/// Reason for the decision
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum DecisionReason {
    AnalysisPassed,
    AnalysisFailed,
    PauseDurationExpired,
    ManualPromotion,
    ManualRollback,
    Timeout,
    Initialization,
}

/// Metric snapshot at decision time
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct MetricSnapshot {
    pub value: f64,
    pub threshold: f64,
    pub passed: bool,
}

/// Decision record for observability
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct Decision {
    pub timestamp: String,
    pub action: DecisionAction,
    #[serde(rename = "fromStep", skip_serializing_if = "Option::is_none")]
    pub from_step: Option<i32>,
    #[serde(rename = "toStep", skip_serializing_if = "Option::is_none")]
    pub to_step: Option<i32>,
    pub reason: DecisionReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<std::collections::HashMap<String, MetricSnapshot>>,
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
    pub phase: Option<Phase>,

    /// Human-readable message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Timestamp when current pause started (RFC3339 format)
    #[serde(rename = "pauseStartTime", skip_serializing_if = "Option::is_none")]
    pub pause_start_time: Option<String>,

    /// Decision history for observability
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<Decision>,
}

#[cfg(test)]
#[path = "rollout_test.rs"]
mod tests;
