//! Simple deployment strategy
//!
//! Standard Kubernetes rolling update with CDEvents observability.
//! No traffic splitting - just deploy, monitor metrics, and emit events.

use super::{RolloutStrategy, StrategyError};
use crate::controller::rollout::{build_replicaset_for_simple, ensure_replicaset_exists, Context};
use crate::crd::rollout::{Phase, Rollout, RolloutStatus};
use async_trait::async_trait;
use k8s_openapi::api::apps::v1::ReplicaSet;
use kube::api::Api;
use kube::ResourceExt;
use tracing::info;

/// Simple strategy handler
///
/// Implements standard rolling update behavior:
/// - Single ReplicaSet with all replicas
/// - No traffic routing (direct pod access)
/// - Optional metrics-based rollback
/// - Always completes immediately (no steps)
pub struct SimpleStrategyHandler;

#[async_trait]
impl RolloutStrategy for SimpleStrategyHandler {
    fn name(&self) -> &'static str {
        "simple"
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
            strategy = "simple",
            replicas = rollout.spec.replicas,
            "Reconciling simple strategy ReplicaSets"
        );

        // Build single ReplicaSet with all replicas
        let rs = build_replicaset_for_simple(rollout, rollout.spec.replicas)
            .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        // Create ReplicaSet API client
        let rs_api: Api<ReplicaSet> = Api::namespaced(ctx.client.clone(), &namespace);

        // Ensure ReplicaSet exists (idempotent)
        ensure_replicaset_exists(&rs_api, &rs, "simple", rollout.spec.replicas)
            .await
            .map_err(|e| StrategyError::ReplicaSetReconciliationFailed(e.to_string()))?;

        info!(
            rollout = ?name,
            replicas = rollout.spec.replicas,
            "Simple strategy ReplicaSets reconciled successfully"
        );

        Ok(())
    }

    async fn reconcile_traffic(
        &self,
        _rollout: &Rollout,
        _ctx: &Context,
    ) -> Result<(), StrategyError> {
        // Simple strategy doesn't manage traffic routing
        // Pods are accessed directly via Services (no weighted routing)
        Ok(())
    }

    fn compute_next_status(&self, rollout: &Rollout) -> RolloutStatus {
        // Simple strategy always completes immediately (no steps)
        RolloutStatus {
            phase: Some(Phase::Completed),
            current_step_index: None,
            current_weight: None,
            message: Some(format!(
                "Simple rollout completed: {} replicas updated",
                rollout.spec.replicas
            )),
            replicas: rollout.spec.replicas,
            ready_replicas: 0,
            updated_replicas: 0,
            pause_start_time: None,
            decisions: vec![],
        }
    }

    fn supports_metrics_analysis(&self, rollout: &Rollout) -> bool {
        // Simple strategy can support metrics analysis if configured
        rollout
            .spec
            .strategy
            .simple
            .analysis
            .is_some()
    }

    fn supports_manual_promotion(&self) -> bool {
        // Simple strategy doesn't have steps, so no manual promotion
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::rollout::{
        AnalysisConfig, MetricConfig, PrometheusConfig, RolloutSpec,
        RolloutStrategy as RolloutStrategySpec, SimpleStrategy,
    };
    use k8s_openapi::api::core::v1::PodTemplateSpec;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;

    fn create_simple_rollout(replicas: i32, with_analysis: bool) -> Rollout {
        let analysis = if with_analysis {
            Some(AnalysisConfig {
                prometheus: Some(PrometheusConfig {
                    address: Some("http://prometheus:9090".to_string()),
                }),
                failure_policy: None,
                warmup_duration: None,
                metrics: vec![MetricConfig {
                    name: "error-rate".to_string(),
                    threshold: 5.0,
                    interval: None,
                    failure_threshold: None,
                    min_sample_size: None,
                }],
            })
        } else {
            None
        };

        Rollout {
            metadata: kube::api::ObjectMeta {
                name: Some("test-simple-rollout".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: RolloutSpec {
                replicas,
                selector: LabelSelector::default(),
                template: PodTemplateSpec::default(),
                strategy: RolloutStrategySpec {
                    simple: Some(SimpleStrategy { analysis }),
                    canary: None,
                    blue_green: None,
                },
            },
            status: None,
        }
    }

    #[test]
    fn test_simple_strategy_name() {
        let strategy = SimpleStrategyHandler;
        assert_eq!(strategy.name(), "simple");
    }

    #[test]
    fn test_simple_strategy_compute_next_status() {
        let rollout = create_simple_rollout(5, false);
        let strategy = SimpleStrategyHandler;

        let status = strategy.compute_next_status(&rollout);

        assert_eq!(status.phase, Some(Phase::Completed));
        assert_eq!(status.current_step_index, None);
        assert_eq!(status.current_weight, None);
        // Check message contains expected text
        match status.message {
            Some(msg) => assert!(msg.contains("5 replicas updated")),
            None => panic!("status should have a message"),
        }
    }

    #[test]
    fn test_simple_strategy_does_not_support_metrics_analysis() {
        let strategy = SimpleStrategyHandler;

        // Simple strategy returns false for metrics analysis
        // Actual metrics check happens in reconcile() if analysis config exists
        assert!(!strategy.supports_metrics_analysis());
    }

    #[test]
    fn test_simple_strategy_does_not_support_manual_promotion() {
        let strategy = SimpleStrategyHandler;
        assert!(!strategy.supports_manual_promotion());
    }

    #[tokio::test]
    async fn test_simple_strategy_reconcile_traffic_is_noop() {
        let rollout = create_simple_rollout(3, false);
        let ctx = Context::new_mock();
        let strategy = SimpleStrategyHandler;

        // Traffic reconciliation should be no-op
        let result = strategy.reconcile_traffic(&rollout, &ctx).await;
        assert!(result.is_ok());
    }

    // Note: reconcile_replicasets() requires real K8s API or extensive mocking
    // Integration tests will cover this in tests/integration_test.rs
}
