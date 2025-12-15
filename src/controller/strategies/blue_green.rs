//! Blue-Green deployment strategy
//!
//! Maintains two full environments (active and preview).
//! Traffic is 100% to active until promotion, then instant switch to preview.

use super::{RolloutStrategy, StrategyError};
use crate::controller::rollout::{
    build_gateway_api_backend_refs, build_replicasets_for_blue_green, ensure_replicaset_exists,
    initialize_rollout_status, Context,
};
use crate::crd::rollout::{Rollout, RolloutStatus};
use async_trait::async_trait;
use k8s_openapi::api::apps::v1::ReplicaSet;
use kube::api::Api;
use kube::ResourceExt;
use tracing::{error, info, warn};

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
        // Check if blue-green strategy has traffic routing configured
        let blue_green = match &rollout.spec.strategy.blue_green {
            Some(strategy) => strategy,
            None => {
                // No blue-green strategy defined (shouldn't happen if we're selected)
                return Ok(());
            }
        };

        let traffic_routing = match &blue_green.traffic_routing {
            Some(routing) => routing,
            None => {
                // No traffic routing configured - this is OK, traffic routing is optional
                return Ok(());
            }
        };

        let gateway_api_routing = match &traffic_routing.gateway_api {
            Some(routing) => routing,
            None => {
                // No Gateway API routing configured
                return Ok(());
            }
        };

        let namespace = rollout
            .namespace()
            .ok_or_else(|| StrategyError::MissingField("namespace".to_string()))?;
        let name = rollout.name_any();
        let httproute_name = &gateway_api_routing.http_route;

        info!(
            rollout = ?name,
            httproute = ?httproute_name,
            strategy = "blue-green",
            "Updating HTTPRoute with weighted backends"
        );

        // Build the weighted backend refs (100/0 or 0/100 based on phase)
        let backend_refs = build_gateway_api_backend_refs(rollout);

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

        let httproute_api: Api<DynamicObject> =
            Api::namespaced_with(ctx.client.clone(), &namespace, &ar);

        // Apply the patch
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
                    active_weight = backend_refs.first().and_then(|b| b.weight),
                    preview_weight = backend_refs.get(1).and_then(|b| b.weight),
                    "HTTPRoute updated successfully (blue-green)"
                );
                Ok(())
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                // HTTPRoute not found - this is non-fatal, traffic routing is optional
                warn!(
                    rollout = ?name,
                    httproute = ?httproute_name,
                    "HTTPRoute not found - skipping traffic routing update"
                );
                Ok(())
            }
            Err(e) => {
                error!(
                    error = ?e,
                    rollout = ?name,
                    httproute = ?httproute_name,
                    "Failed to patch HTTPRoute"
                );
                Err(StrategyError::TrafficReconciliationFailed(e.to_string()))
            }
        }
    }

    fn compute_next_status(&self, rollout: &Rollout) -> RolloutStatus {
        // For blue-green, use initialize_rollout_status which sets Preview phase
        // The reconcile() loop will handle promotion logic
        initialize_rollout_status(rollout)
    }

    fn supports_metrics_analysis(&self) -> bool {
        // Blue-green can support metrics analysis if configured
        true
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
    fn test_blue_green_strategy_supports_metrics_analysis() {
        let strategy = BlueGreenStrategyHandler;
        assert!(strategy.supports_metrics_analysis());
    }

    #[test]
    fn test_blue_green_strategy_supports_manual_promotion() {
        let strategy = BlueGreenStrategyHandler;
        assert!(strategy.supports_manual_promotion());
    }

    #[test]
    fn test_blue_green_strategy_compute_next_status() {
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

    // Note: reconcile_replicasets() and reconcile_traffic() require K8s API
    // These are tested in integration tests
}
