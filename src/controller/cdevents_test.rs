use super::*;
use crate::crd::rollout::{
    CanaryStep, CanaryStrategy, Phase, Rollout, RolloutSpec, RolloutStatus, RolloutStrategy,
};
use cloudevents::AttributesReader;
use kube::api::ObjectMeta;

// TDD Cycle 1: RED - Test that service.deployed event is emitted when rollout initializes
#[tokio::test]
async fn test_emit_service_deployed_on_initialization() {
    // ARRANGE: Create test rollout with no status (new rollout)
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-app".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: Default::default(),
            template: create_test_pod_template("nginx:1.0"),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![CanaryStep {
                        set_weight: Some(10),
                        pause: None,
                    }],
                    traffic_routing: None,
                }),
            },
        },
        status: None, // No status yet - this is a new rollout
    };

    // Create mock CDEvents sink
    let sink = CDEventsSink::new_mock();

    // Old status (None - new rollout)
    let old_status = None;

    // New status (Initializing → Progressing)
    let new_status = RolloutStatus {
        phase: Some(Phase::Progressing),
        current_step_index: Some(0),
        current_weight: Some(10),
        ..Default::default()
    };

    // ACT: Emit CDEvent for status change
    emit_status_change_event(&rollout, &old_status, &new_status, &sink)
        .await
        .unwrap();

    // ASSERT: Verify service.deployed event was emitted
    let events = sink.get_emitted_events();
    assert_eq!(events.len(), 1, "Expected exactly 1 event");

    let event = &events[0];
    assert_eq!(
        event.ty(),
        "dev.cdevents.service.deployed.0.2.0",
        "Expected service.deployed event"
    );

    // Verify event contains correct data
    let _cdevent: cdevents_sdk::CDEvent = event.clone().try_into().unwrap();
    // TODO: Verify artifact_id, environment, etc.
}

// Helper to create test pod template
fn create_test_pod_template(image: &str) -> k8s_openapi::api::core::v1::PodTemplateSpec {
    use k8s_openapi::api::core::v1::{Container, PodSpec, PodTemplateSpec};

    PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some([("app".to_string(), "test-app".to_string())].into()),
            ..Default::default()
        }),
        spec: Some(PodSpec {
            containers: vec![Container {
                name: "nginx".to_string(),
                image: Some(image.to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }),
    }
}
