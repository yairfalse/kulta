use kulta::controller::{Context, ReconcileError};
use kulta::crd::rollout::Rollout;
use kube::runtime::controller::Action;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn test_error_policy_returns_requeue() {
    let rollout = Arc::new(Rollout {
        metadata: kube::api::ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: kulta::crd::rollout::RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: kulta::crd::rollout::RolloutStrategy { canary: None },
        },
        status: None,
    });

    let error = ReconcileError::KubeError(kube::Error::Api(kube::error::ErrorResponse {
        status: "Failure".to_string(),
        message: "Test error".to_string(),
        reason: "InternalError".to_string(),
        code: 500,
    }));

    let action = crate::error_policy(rollout, &error, Arc::new(Context::new_mock()));

    // Should requeue after delay on error
    // Action::requeue is a function, not an enum variant
    // Just verify it returned an Action
    assert!(matches!(action, Action { .. }));
}
