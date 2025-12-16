//! Blue-Green deployment strategy
//!
//! Maintains two full environments (active and preview).
//! Traffic is 100% to active until promotion, then instant switch to preview.

use super::{reconcile_gateway_api_traffic, RolloutStrategy, StrategyError};
use crate::controller::rollout::{
    build_replicasets_for_blue_green, ensure_replicaset_exists, has_promote_annotation, Context,
};
use crate::crd::rollout::{Phase, Rollout, RolloutStatus};
use async_trait::async_trait;
use k8s_openapi::api::apps::v1::ReplicaSet;
use kube::api::Api;
use kube::ResourceExt;
use tracing::info;

/// Blue-Green strategy handler
///
/// Implements blue-green deployment:
/// - Two full-size ReplicaSets (active + preview)
/// - Instant traffic cutover (no gradual shift)
/// - Preview environment for testing before promotion
/// - Optional auto-promotion after duration
pub struct BlueGreenStrategyHandler;

#[async_trait]
impl RolloutStrategy for BlueGreenStrategyHandler {
    fn name(&self) -> &'static str {
        "blue-green"
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

        info!(
            rollout = ?name,
            strategy = "blue-green",
            replicas = rollout.spec.replicas,
            "Reconciling blue-green strategy ReplicaSets"
        );

        // Build both ReplicaSets (active + preview) at full size
        let (active_rs, preview_rs) =
            build_replicasets_for_blue_green(rollout, rollout.spec.replicas)
                .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        // Create ReplicaSet API client
        let rs_api: Api<ReplicaSet> = Api::namespaced(ctx.client.clone(), &namespace);

        // Ensure active ReplicaSet exists
        ensure_replicaset_exists(&rs_api, &active_rs, "active", rollout.spec.replicas)
            .await
            .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        // Ensure preview ReplicaSet exists
        ensure_replicaset_exists(&rs_api, &preview_rs, "preview", rollout.spec.replicas)
            .await
            .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        info!(
            rollout = ?name,
            active_replicas = rollout.spec.replicas,
            preview_replicas = rollout.spec.replicas,
            "Blue-green strategy ReplicaSets reconciled successfully"
        );

        Ok(())
    }

    async fn reconcile_traffic(
        &self,
        rollout: &Rollout,
        ctx: &Context,
    ) -> Result<(), StrategyError> {
        // Use shared helper for Gateway API traffic routing
        reconcile_gateway_api_traffic(rollout, ctx, "blue-green").await
    }

    fn compute_next_status(&self, rollout: &Rollout) -> RolloutStatus {
        // Check current status
        let current_phase = rollout.status.as_ref().and_then(|s| s.phase.clone());

        match current_phase {
            // Already completed - stay completed
            Some(Phase::Completed) => RolloutStatus {
                phase: Some(Phase::Completed),
                message: Some(
                    "Blue-green rollout completed: preview promoted to active".to_string(),
                ),
                replicas: rollout.spec.replicas,
                ..Default::default()
            },

            // In preview phase - check for promotion
            Some(Phase::Preview) => {
                if has_promote_annotation(rollout) {
                    // Promote: transition to Completed
                    info!(
                        rollout = ?rollout.name_any(),
                        "Blue-green promotion triggered via annotation"
                    );
                    RolloutStatus {
                        phase: Some(Phase::Completed),
                        message: Some(
                            "Blue-green rollout completed: preview promoted to active".to_string(),
                        ),
                        replicas: rollout.spec.replicas,
                        ..Default::default()
                    }
                } else {
                    // Stay in preview, waiting for promotion
                    RolloutStatus {
                        phase: Some(Phase::Preview),
                        message: Some(
                            "Blue-green rollout: preview environment ready, awaiting promotion"
                                .to_string(),
                        ),
                        replicas: rollout.spec.replicas,
                        ..Default::default()
                    }
                }
            }

            // No status or other phase - initialize to Preview
            _ => RolloutStatus {
                phase: Some(Phase::Preview),
                message: Some("Blue-green rollout: preview environment ready".to_string()),
                replicas: rollout.spec.replicas,
                ..Default::default()
            },
        }
    }

    fn supports_metrics_analysis(&self) -> bool {
        // Blue-green rollouts never reach the Progressing phase, so metrics analysis is not supported.
        false
    }

    fn supports_manual_promotion(&self) -> bool {
        // Blue-green supports manual promotion
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::rollout::{
        BlueGreenStrategy, GatewayAPIRouting, Phase, RolloutSpec,
        RolloutStrategy as RolloutStrategySpec, TrafficRouting,
    };
    use k8s_openapi::api::core::v1::PodTemplateSpec;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;

    fn create_blue_green_rollout(replicas: i32) -> Rollout {
        Rollout {
            metadata: kube::api::ObjectMeta {
                name: Some("test-bg-rollout".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: RolloutSpec {
                replicas,
                selector: LabelSelector::default(),
                template: PodTemplateSpec::default(),
                strategy: RolloutStrategySpec {
                    simple: None,
                    canary: None,
                    blue_green: Some(BlueGreenStrategy {
                        active_service: "app-active".to_string(),
                        preview_service: "app-preview".to_string(),
                        auto_promotion_enabled: None,
                        auto_promotion_seconds: None,
                        traffic_routing: Some(TrafficRouting {
                            gateway_api: Some(GatewayAPIRouting {
                                http_route: "app-route".to_string(),
                            }),
                        }),
                        analysis: None,
                    }),
                },
            },
            status: None,
        }
    }

    #[test]
    fn test_blue_green_strategy_name() {
        let strategy = BlueGreenStrategyHandler;
        assert_eq!(strategy.name(), "blue-green");
    }

    #[test]
    fn test_blue_green_strategy_does_not_support_metrics_analysis() {
        let strategy = BlueGreenStrategyHandler;
        // Blue-green doesn't support metrics analysis because it never
        // enters Progressing phase (goes directly to Preview)
        assert!(!strategy.supports_metrics_analysis());
    }

    #[test]
    fn test_blue_green_strategy_supports_manual_promotion() {
        let strategy = BlueGreenStrategyHandler;
        assert!(strategy.supports_manual_promotion());
    }

    #[test]
    fn test_blue_green_strategy_compute_next_status_initializes_to_preview() {
        let rollout = create_blue_green_rollout(5);
        let strategy = BlueGreenStrategyHandler;

        let status = strategy.compute_next_status(&rollout);

        // Blue-green should start in Preview phase
        assert_eq!(status.phase, Some(Phase::Preview));
        assert_eq!(status.current_step_index, None); // No steps in blue-green
        assert_eq!(status.current_weight, None); // No weight in blue-green
        match status.message {
            Some(msg) => assert!(msg.contains("preview environment ready")),
            None => panic!("status should have a message"),
        }
    }

    #[test]
    fn test_blue_green_strategy_stays_in_preview_without_annotation() {
        let mut rollout = create_blue_green_rollout(5);
        // Set status to Preview (already initialized)
        rollout.status = Some(RolloutStatus {
            phase: Some(Phase::Preview),
            message: Some("Preview ready".to_string()),
            replicas: 5,
            ..Default::default()
        });

        let strategy = BlueGreenStrategyHandler;
        let status = strategy.compute_next_status(&rollout);

        // Should stay in Preview without promotion annotation
        assert_eq!(status.phase, Some(Phase::Preview));
        match status.message {
            Some(msg) => assert!(msg.contains("awaiting promotion")),
            None => panic!("status should have a message"),
        }
    }

    #[test]
    fn test_blue_green_strategy_promotes_to_completed_with_annotation() {
        use std::collections::BTreeMap;

        let mut rollout = create_blue_green_rollout(5);
        // Set status to Preview
        rollout.status = Some(RolloutStatus {
            phase: Some(Phase::Preview),
            message: Some("Preview ready".to_string()),
            replicas: 5,
            ..Default::default()
        });
        // Add promote annotation
        let mut annotations = BTreeMap::new();
        annotations.insert("kulta.io/promote".to_string(), "true".to_string());
        rollout.metadata.annotations = Some(annotations);

        let strategy = BlueGreenStrategyHandler;
        let status = strategy.compute_next_status(&rollout);

        // Should transition to Completed
        assert_eq!(status.phase, Some(Phase::Completed));
        match status.message {
            Some(msg) => assert!(msg.contains("promoted to active")),
            None => panic!("status should have a message"),
        }
    }

    #[test]
    fn test_blue_green_strategy_stays_completed() {
        let mut rollout = create_blue_green_rollout(5);
        // Set status to Completed
        rollout.status = Some(RolloutStatus {
            phase: Some(Phase::Completed),
            message: Some("Completed".to_string()),
            replicas: 5,
            ..Default::default()
        });

        let strategy = BlueGreenStrategyHandler;
        let status = strategy.compute_next_status(&rollout);

        // Should stay Completed
        assert_eq!(status.phase, Some(Phase::Completed));
    }

    // Note: reconcile_replicasets() and reconcile_traffic() require K8s API
    // These are tested in integration tests
}
