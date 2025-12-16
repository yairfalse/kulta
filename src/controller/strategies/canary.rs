//! Canary deployment strategy
//!
//! Progressive traffic shifting with gradual rollout through defined steps.

use super::{reconcile_gateway_api_traffic, RolloutStrategy, StrategyError};
use crate::controller::rollout::{
    build_replicaset, calculate_replica_split, compute_desired_status, ensure_replicaset_exists,
    Context,
};
use crate::crd::rollout::{Rollout, RolloutStatus};
use async_trait::async_trait;
use k8s_openapi::api::apps::v1::ReplicaSet;
use kube::api::Api;
use kube::ResourceExt;
use tracing::info;

/// Canary strategy handler
///
/// Implements progressive canary deployment:
/// - Two ReplicaSets (stable + canary) with traffic-based scaling
/// - Gradual traffic weight increase (e.g., 10% → 50% → 100%)
/// - Pause steps (time-based or manual promotion)
/// - Metrics-based rollback support
pub struct CanaryStrategyHandler;

#[async_trait]
impl RolloutStrategy for CanaryStrategyHandler {
    fn name(&self) -> &'static str {
        "canary"
    }

    async fn reconcile_replicasets(
        &self,
        rollout: &Rollout,
        ctx: &Context,
    ) -> Result<(), StrategyError> {
        let namespace = rollout
            .namespace()
            .ok_or_else(|| StrategyError::MissingField("namespace".to_string()))?;
        let name = rollout.name_any();

        // Get current canary weight from status
        let current_weight = rollout
            .status
            .as_ref()
            .and_then(|s| s.current_weight)
            .unwrap_or(0);

        // Calculate replica split based on weight
        let (stable_replicas, canary_replicas) =
            calculate_replica_split(rollout.spec.replicas, current_weight);

        info!(
            rollout = ?name,
            strategy = "canary",
            total_replicas = rollout.spec.replicas,
            current_weight = current_weight,
            stable_replicas = stable_replicas,
            canary_replicas = canary_replicas,
            "Reconciling canary strategy ReplicaSets"
        );

        // Create ReplicaSet API client
        let rs_api: Api<ReplicaSet> = Api::namespaced(ctx.client.clone(), &namespace);

        // Build and ensure stable ReplicaSet exists
        let stable_rs = build_replicaset(rollout, "stable", stable_replicas)
            .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        ensure_replicaset_exists(&rs_api, &stable_rs, "stable", stable_replicas)
            .await
            .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        // Build and ensure canary ReplicaSet exists
        let canary_rs = build_replicaset(rollout, "canary", canary_replicas)
            .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        ensure_replicaset_exists(&rs_api, &canary_rs, "canary", canary_replicas)
            .await
            .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        info!(
            rollout = ?name,
            stable_replicas = stable_replicas,
            canary_replicas = canary_replicas,
            "Canary strategy ReplicaSets reconciled successfully"
        );

        Ok(())
    }

    async fn reconcile_traffic(
        &self,
        rollout: &Rollout,
        ctx: &Context,
    ) -> Result<(), StrategyError> {
        // Use shared helper for Gateway API traffic routing
        reconcile_gateway_api_traffic(rollout, ctx, "canary").await
    }

    fn compute_next_status(&self, rollout: &Rollout) -> RolloutStatus {
        // Use the existing compute_desired_status function which handles:
        // - Initialization
        // - Step progression
        // - Pause logic
        // - Completion detection
        compute_desired_status(rollout)
    }

    fn supports_metrics_analysis(&self) -> bool {
        // Canary strategy always supports metrics analysis if configured
        true
    }

    fn supports_manual_promotion(&self) -> bool {
        // Canary strategy supports manual promotion via kulta.io/promote annotation
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::rollout::{
        CanaryStep, CanaryStrategy, GatewayAPIRouting, PauseDuration, Phase, RolloutSpec,
        RolloutStrategy as RolloutStrategySpec, TrafficRouting,
    };
    use k8s_openapi::api::core::v1::PodTemplateSpec;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;

    fn create_canary_rollout(
        replicas: i32,
        current_weight: Option<i32>,
        steps: Vec<CanaryStep>,
    ) -> Rollout {
        Rollout {
            metadata: kube::api::ObjectMeta {
                name: Some("test-canary-rollout".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: RolloutSpec {
                replicas,
                selector: LabelSelector::default(),
                template: PodTemplateSpec::default(),
                strategy: RolloutStrategySpec {
                    simple: None,
                    canary: Some(CanaryStrategy {
                        canary_service: "app-canary".to_string(),
                        stable_service: "app-stable".to_string(),
                        steps,
                        traffic_routing: Some(TrafficRouting {
                            gateway_api: Some(GatewayAPIRouting {
                                http_route: "app-route".to_string(),
                            }),
                        }),
                        analysis: None,
                    }),
                    blue_green: None,
                },
            },
            status: current_weight.map(|weight| crate::crd::rollout::RolloutStatus {
                phase: Some(Phase::Progressing),
                current_step_index: Some(0),
                current_weight: Some(weight),
                replicas,
                ready_replicas: 0,
                updated_replicas: 0,
                message: None,
                pause_start_time: None,
                decisions: vec![],
            }),
        }
    }

    #[test]
    fn test_canary_strategy_name() {
        let strategy = CanaryStrategyHandler;
        assert_eq!(strategy.name(), "canary");
    }

    #[test]
    fn test_canary_strategy_supports_metrics_analysis() {
        let strategy = CanaryStrategyHandler;
        assert!(strategy.supports_metrics_analysis());
    }

    #[test]
    fn test_canary_strategy_supports_manual_promotion() {
        let strategy = CanaryStrategyHandler;
        assert!(strategy.supports_manual_promotion());
    }

    #[test]
    fn test_canary_strategy_compute_next_status_no_status() {
        let steps = vec![
            CanaryStep {
                set_weight: Some(10),
                pause: None,
            },
            CanaryStep {
                set_weight: Some(50),
                pause: Some(PauseDuration {
                    duration: Some("30s".to_string()),
                }),
            },
        ];
        let rollout = create_canary_rollout(3, None, steps);
        let strategy = CanaryStrategyHandler;

        let status = strategy.compute_next_status(&rollout);

        // Should initialize to step 0 with 10% weight
        assert_eq!(status.phase, Some(Phase::Progressing));
        assert_eq!(status.current_step_index, Some(0));
        assert_eq!(status.current_weight, Some(10));
    }

    #[test]
    fn test_canary_strategy_compute_next_status_with_status() {
        let steps = vec![
            CanaryStep {
                set_weight: Some(10),
                pause: None,
            },
            CanaryStep {
                set_weight: Some(100),
                pause: None,
            },
        ];
        let rollout = create_canary_rollout(3, Some(10), steps);
        let strategy = CanaryStrategyHandler;

        let status = strategy.compute_next_status(&rollout);

        // Should progress to step 1 (100% weight = completed)
        assert_eq!(status.phase, Some(Phase::Completed));
        assert_eq!(status.current_step_index, Some(1));
        assert_eq!(status.current_weight, Some(100));
    }

    // Note: reconcile_replicasets() and reconcile_traffic() require K8s API
    // These are tested in integration tests
}
