use super::*;
use crate::crd::rollout::{
    CanaryStep, CanaryStrategy, GatewayAPIRouting, Phase, Rollout, RolloutSpec, RolloutStatus,
    RolloutStrategy, TrafficRouting,
};
use kube::api::ObjectMeta;

// Helper function to create a test Rollout with canary strategy
fn create_test_rollout_with_canary() -> Rollout {
    Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some(
                    vec![("app".to_string(), "test-app".to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            template: k8s_openapi::api::core::v1::PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(
                        vec![("app".to_string(), "test-app".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                }),
                spec: Some(k8s_openapi::api::core::v1::PodSpec {
                    containers: vec![k8s_openapi::api::core::v1::Container {
                        name: "app".to_string(),
                        image: Some("nginx:1.0".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![], // Tests will set their own steps
                    traffic_routing: None,
                }),
            },
        },
        status: None,
    }
}

#[tokio::test]
async fn test_reconcile_creates_stable_replicaset() {
    // Create a mock Rollout resource
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some(
                    vec![("app".to_string(), "test-app".to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            template: k8s_openapi::api::core::v1::PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(
                        vec![("app".to_string(), "test-app".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                }),
                spec: Some(k8s_openapi::api::core::v1::PodSpec {
                    containers: vec![k8s_openapi::api::core::v1::Container {
                        name: "app".to_string(),
                        image: Some("nginx:1.0".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None, // No HTTPRoute for ReplicaSet unit tests
                }),
            },
        },
        status: None,
    };

    // Test that build_replicaset creates a stable ReplicaSet with correct properties
    // (Full reconcile integration test requires real K8s cluster - see CI integration tests)
    let stable_rs = build_replicaset(&rollout, "stable", rollout.spec.replicas).unwrap();

    // Verify stable ReplicaSet has correct properties
    assert_eq!(
        stable_rs.metadata.name.as_deref(),
        Some("test-rollout-stable")
    );
    assert_eq!(stable_rs.metadata.namespace.as_deref(), Some("default"));
    assert_eq!(stable_rs.spec.as_ref().unwrap().replicas, Some(3));

    // Verify rollouts.kulta.io/managed label exists (prevents Deployment adoption)
    let rs_labels = stable_rs.metadata.labels.as_ref().unwrap();
    assert_eq!(
        rs_labels.get("rollouts.kulta.io/managed"),
        Some(&"true".to_string())
    );
}

#[tokio::test]
async fn test_compute_pod_template_hash() {
    // Test that we can generate stable pod-template-hash for ReplicaSets
    let pod_template = k8s_openapi::api::core::v1::PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(
                vec![("app".to_string(), "test-app".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        }),
        spec: Some(k8s_openapi::api::core::v1::PodSpec {
            containers: vec![k8s_openapi::api::core::v1::Container {
                name: "app".to_string(),
                image: Some("nginx:1.0".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }),
    };

    let hash1 = compute_pod_template_hash(&pod_template).unwrap();
    let hash2 = compute_pod_template_hash(&pod_template).unwrap();

    // Same template should produce same hash
    assert_eq!(hash1, hash2);
    assert_eq!(hash1.len(), 10); // 10-character hash like Kubernetes

    // Different template should produce different hash
    let mut different_template = pod_template.clone();
    if let Some(ref mut spec) = different_template.spec {
        spec.containers[0].image = Some("nginx:2.0".to_string());
    }

    let hash3 = compute_pod_template_hash(&different_template).unwrap();
    assert_ne!(hash1, hash3);
}

#[tokio::test]
async fn test_build_replicaset_spec() {
    // Test that we can build a ReplicaSet from a Rollout
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some(
                    vec![("app".to_string(), "test-app".to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            template: k8s_openapi::api::core::v1::PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(
                        vec![("app".to_string(), "test-app".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                }),
                spec: Some(k8s_openapi::api::core::v1::PodSpec {
                    containers: vec![k8s_openapi::api::core::v1::Container {
                        name: "app".to_string(),
                        image: Some("nginx:1.0".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![],
                    traffic_routing: None,
                }),
            },
        },
        status: None,
    };

    // Build stable ReplicaSet
    let rs = build_replicaset(&rollout, "stable", 3).unwrap();

    assert_eq!(rs.metadata.name.as_deref(), Some("test-rollout-stable"));
    assert_eq!(rs.metadata.namespace.as_deref(), Some("default"));
    assert_eq!(rs.spec.as_ref().unwrap().replicas, Some(3));

    // Verify pod-template-hash label exists
    let labels = &rs
        .spec
        .as_ref()
        .unwrap()
        .template
        .as_ref()
        .unwrap()
        .metadata
        .as_ref()
        .unwrap()
        .labels;
    assert!(labels.as_ref().unwrap().contains_key("pod-template-hash"));

    // Verify rollouts.kulta.io/type label
    assert_eq!(
        labels.as_ref().unwrap().get("rollouts.kulta.io/type"),
        Some(&"stable".to_string())
    );
}

#[tokio::test]
async fn test_reconcile_creates_canary_replicaset() {
    // Test that reconcile creates BOTH stable and canary ReplicaSets
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some(
                    vec![("app".to_string(), "test-app".to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            template: k8s_openapi::api::core::v1::PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(
                        vec![("app".to_string(), "test-app".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                }),
                spec: Some(k8s_openapi::api::core::v1::PodSpec {
                    containers: vec![k8s_openapi::api::core::v1::Container {
                        name: "app".to_string(),
                        image: Some("nginx:1.0".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![CanaryStep {
                        set_weight: Some(20),
                        pause: None,
                    }],
                    traffic_routing: None, // No HTTPRoute for ReplicaSet unit tests
                }),
            },
        },
        status: None,
    };

    // Build canary ReplicaSet (should have 0 replicas initially)
    let canary_rs = build_replicaset(&rollout, "canary", 0).unwrap();

    // Verify canary ReplicaSet has correct properties
    assert_eq!(
        canary_rs.metadata.name.as_deref(),
        Some("test-rollout-canary")
    );
    assert_eq!(canary_rs.metadata.namespace.as_deref(), Some("default"));
    assert_eq!(canary_rs.spec.as_ref().unwrap().replicas, Some(0));

    // Verify canary has rollouts.kulta.io/type=canary label
    let labels = &canary_rs
        .spec
        .as_ref()
        .unwrap()
        .template
        .as_ref()
        .unwrap()
        .metadata
        .as_ref()
        .unwrap()
        .labels;
    assert_eq!(
        labels.as_ref().unwrap().get("rollouts.kulta.io/type"),
        Some(&"canary".to_string())
    );

    // Verify canary has rollouts.kulta.io/managed label (prevents Deployment adoption)
    let rs_labels = canary_rs.metadata.labels.as_ref().unwrap();
    assert_eq!(
        rs_labels.get("rollouts.kulta.io/managed"),
        Some(&"true".to_string())
    );
}

#[tokio::test]
async fn test_replicaset_has_kulta_managed_label() {
    // TDD Cycle 15 - RED: Test that KULTA-managed ReplicaSets have unique labels
    // to prevent adoption by Kubernetes Deployment controllers
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some(
                    vec![("app".to_string(), "test-app".to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            template: k8s_openapi::api::core::v1::PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(
                        vec![("app".to_string(), "test-app".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                }),
                spec: Some(k8s_openapi::api::core::v1::PodSpec {
                    containers: vec![k8s_openapi::api::core::v1::Container {
                        name: "app".to_string(),
                        image: Some("nginx:1.0".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![],
                    traffic_routing: None,
                }),
            },
        },
        status: None,
    };

    let stable_rs = build_replicaset(&rollout, "stable", 3).unwrap();

    // Verify ReplicaSet metadata has rollouts.kulta.io/managed label
    let rs_labels = stable_rs.metadata.labels.as_ref().unwrap();
    assert_eq!(
        rs_labels.get("rollouts.kulta.io/managed"),
        Some(&"true".to_string()),
        "ReplicaSet must have rollouts.kulta.io/managed=true label to prevent Deployment adoption"
    );

    // Verify pod template labels have rollouts.kulta.io/managed label
    let pod_labels = stable_rs
        .spec
        .as_ref()
        .unwrap()
        .template
        .as_ref()
        .unwrap()
        .metadata
        .as_ref()
        .unwrap()
        .labels
        .as_ref()
        .unwrap();
    assert_eq!(
        pod_labels.get("rollouts.kulta.io/managed"),
        Some(&"true".to_string()),
        "Pod template must have rollouts.kulta.io/managed=true label"
    );

    // Verify selector includes rollouts.kulta.io/managed label
    let selector_labels = stable_rs
        .spec
        .as_ref()
        .unwrap()
        .selector
        .match_labels
        .as_ref()
        .unwrap();
    assert_eq!(
        selector_labels.get("rollouts.kulta.io/managed"),
        Some(&"true".to_string()),
        "Selector must include rollouts.kulta.io/managed=true to prevent Deployment adoption"
    );

    // Verify canary also has the label
    let canary_rs = build_replicaset(&rollout, "canary", 0).unwrap();
    let canary_selector_labels = canary_rs
        .spec
        .as_ref()
        .unwrap()
        .selector
        .match_labels
        .as_ref()
        .unwrap();
    assert_eq!(
        canary_selector_labels.get("rollouts.kulta.io/managed"),
        Some(&"true".to_string()),
        "Canary selector must also include rollouts.kulta.io/managed=true"
    );
}

#[tokio::test]
async fn test_build_both_stable_and_canary_replicasets() {
    // Test that we can build both stable and canary ReplicaSets
    // This test ensures both types are buildable before reconcile uses them
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 5,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some(
                    vec![("app".to_string(), "test-app".to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            template: k8s_openapi::api::core::v1::PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(
                        vec![("app".to_string(), "test-app".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                }),
                spec: Some(k8s_openapi::api::core::v1::PodSpec {
                    containers: vec![k8s_openapi::api::core::v1::Container {
                        name: "app".to_string(),
                        image: Some("nginx:2.0".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![],
                    traffic_routing: None,
                }),
            },
        },
        status: None,
    };

    // Build both ReplicaSets
    let stable_rs = build_replicaset(&rollout, "stable", rollout.spec.replicas).unwrap();
    let canary_rs = build_replicaset(&rollout, "canary", 0).unwrap();

    // Verify stable ReplicaSet
    assert_eq!(
        stable_rs.metadata.name.as_deref(),
        Some("test-rollout-stable")
    );
    assert_eq!(stable_rs.spec.as_ref().unwrap().replicas, Some(5));
    assert_eq!(
        stable_rs
            .spec
            .as_ref()
            .unwrap()
            .template
            .as_ref()
            .unwrap()
            .metadata
            .as_ref()
            .unwrap()
            .labels
            .as_ref()
            .unwrap()
            .get("rollouts.kulta.io/type"),
        Some(&"stable".to_string())
    );

    // Verify canary ReplicaSet
    assert_eq!(
        canary_rs.metadata.name.as_deref(),
        Some("test-rollout-canary")
    );
    assert_eq!(canary_rs.spec.as_ref().unwrap().replicas, Some(0));
    assert_eq!(
        canary_rs
            .spec
            .as_ref()
            .unwrap()
            .template
            .as_ref()
            .unwrap()
            .metadata
            .as_ref()
            .unwrap()
            .labels
            .as_ref()
            .unwrap()
            .get("rollouts.kulta.io/type"),
        Some(&"canary".to_string())
    );

    // Verify both share the same pod-template-hash (same template)
    let stable_hash = stable_rs
        .spec
        .as_ref()
        .unwrap()
        .template
        .as_ref()
        .unwrap()
        .metadata
        .as_ref()
        .unwrap()
        .labels
        .as_ref()
        .unwrap()
        .get("pod-template-hash")
        .unwrap();

    let canary_hash = canary_rs
        .spec
        .as_ref()
        .unwrap()
        .template
        .as_ref()
        .unwrap()
        .metadata
        .as_ref()
        .unwrap()
        .labels
        .as_ref()
        .unwrap()
        .get("pod-template-hash")
        .unwrap();

    assert_eq!(stable_hash, canary_hash);
}

#[tokio::test]
async fn test_calculate_traffic_weights_step0() {
    // Test weight calculation for canary step 0 (20%)
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(100),
                            pause: None,
                        },
                    ],
                    traffic_routing: None, // No HTTPRoute for ReplicaSet unit tests
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0), // First step: 20% canary
            ..Default::default()
        }),
    };

    // Calculate weights for step 0
    let (stable_weight, canary_weight) = calculate_traffic_weights(&rollout);

    assert_eq!(canary_weight, 20);
    assert_eq!(stable_weight, 80);
}

#[tokio::test]
async fn test_calculate_traffic_weights_step1() {
    // Test weight calculation for canary step 1 (50%)
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(1), // Second step: 50% canary
            ..Default::default()
        }),
    };

    // Calculate weights for step 1
    let (stable_weight, canary_weight) = calculate_traffic_weights(&rollout);

    assert_eq!(canary_weight, 50);
    assert_eq!(stable_weight, 50);
}

#[tokio::test]
async fn test_calculate_traffic_weights_no_step() {
    // Test weight calculation when no step is active (100% stable)
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![CanaryStep {
                        set_weight: Some(20),
                        pause: None,
                    }],
                    traffic_routing: None,
                }),
            },
        },
        status: None, // No status yet, default to 100% stable
    };

    // Calculate weights when no step is active
    let (stable_weight, canary_weight) = calculate_traffic_weights(&rollout);

    assert_eq!(canary_weight, 0);
    assert_eq!(stable_weight, 100);
}

#[tokio::test]
async fn test_calculate_traffic_weights_complete() {
    // Test weight calculation when rollout is complete (100% canary)
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(100),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(1), // Last step: 100% canary
            ..Default::default()
        }),
    };

    // Calculate weights for final step
    let (stable_weight, canary_weight) = calculate_traffic_weights(&rollout);

    assert_eq!(canary_weight, 100);
    assert_eq!(stable_weight, 0);
}

#[tokio::test]
async fn test_calculate_traffic_weights_beyond_steps() {
    // Test weight calculation when step index is beyond available steps
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![CanaryStep {
                        set_weight: Some(20),
                        pause: None,
                    }],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(5), // Beyond available steps (only 1 step)
            ..Default::default()
        }),
    };

    // When step index exceeds steps, rollout is complete (100% canary)
    let (stable_weight, canary_weight) = calculate_traffic_weights(&rollout);

    assert_eq!(canary_weight, 100);
    assert_eq!(stable_weight, 0);
}

#[tokio::test]
async fn test_build_httproute_backend_weights() {
    // Test building HTTPRoute backendRefs with correct weights
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![CanaryStep {
                        set_weight: Some(20),
                        pause: None,
                    }],
                    traffic_routing: None, // No HTTPRoute for ReplicaSet unit tests
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0), // 20% canary
            ..Default::default()
        }),
    };

    // Build backendRefs with weights from rollout
    let backend_refs = build_backend_refs_with_weights(&rollout);

    // Should have 2 backends: stable (80%) and canary (20%)
    assert_eq!(backend_refs.len(), 2);

    // Find stable backend
    let stable = backend_refs
        .iter()
        .find(|b| b.name == "test-app-stable")
        .expect("Should have stable backend");
    assert_eq!(stable.weight, Some(80));

    // Find canary backend
    let canary = backend_refs
        .iter()
        .find(|b| b.name == "test-app-canary")
        .expect("Should have canary backend");
    assert_eq!(canary.weight, Some(20));
}

#[tokio::test]
async fn test_convert_to_gateway_api_backend_refs() {
    // Test conversion from our HTTPBackendRef to gateway-api HTTPRouteRulesBackendRefs
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![CanaryStep {
                        set_weight: Some(20),
                        pause: None,
                    }],
                    traffic_routing: Some(TrafficRouting {
                        gateway_api: Some(GatewayAPIRouting {
                            http_route: "test-route".to_string(),
                        }),
                    }),
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0), // 20% canary
            ..Default::default()
        }),
    };

    // Convert to gateway-api backend refs
    let gateway_backend_refs = build_gateway_api_backend_refs(&rollout);

    // Should have 2 backends: stable (80%) and canary (20%)
    assert_eq!(gateway_backend_refs.len(), 2);

    // Verify stable backend
    let stable = gateway_backend_refs
        .iter()
        .find(|b| b.name == "test-app-stable")
        .expect("Should have stable backend");
    assert_eq!(stable.weight, Some(80));
    assert_eq!(stable.port, Some(80));
    assert_eq!(stable.kind.as_deref(), Some("Service"));
    assert_eq!(stable.group.as_deref(), Some(""));

    // Verify canary backend
    let canary = gateway_backend_refs
        .iter()
        .find(|b| b.name == "test-app-canary")
        .expect("Should have canary backend");
    assert_eq!(canary.weight, Some(20));
    assert_eq!(canary.port, Some(80));
    assert_eq!(canary.kind.as_deref(), Some("Service"));
    assert_eq!(canary.group.as_deref(), Some(""));
}

#[tokio::test]
async fn test_gateway_api_backend_refs_no_canary_strategy() {
    // Test that we return empty vec when no canary strategy exists
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy { canary: None }, // No canary strategy
        },
        status: None,
    };

    // Should return empty vec when no canary strategy
    let gateway_backend_refs = build_gateway_api_backend_refs(&rollout);
    assert_eq!(gateway_backend_refs.len(), 0);
}

// TDD Cycle 16: Automatic Step Progression
// RED: Test that reconcile progresses through canary steps automatically

#[tokio::test]
async fn test_initialize_rollout_status() {
    // Test that a new Rollout gets initialized with status.currentStepIndex = 0
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: None, // No status yet - should be initialized
    };

    // Function to test: initialize_rollout_status
    // Should return a RolloutStatus with:
    // - current_step_index = 0 (start at first step)
    // - phase = "Progressing"
    // - current_weight = 20 (from step 0)
    let status = initialize_rollout_status(&rollout);

    assert_eq!(status.current_step_index, Some(0));
    assert_eq!(status.phase, Some(Phase::Progressing));
    assert_eq!(status.current_weight, Some(20));
    assert_eq!(
        status.message,
        Some("Starting canary rollout at step 0 (20% traffic)".to_string())
    );
}

#[tokio::test]
async fn test_should_progress_to_next_step() {
    // Test that we detect when it's time to progress to the next step
    // For now: progress immediately (no pause, no analysis)
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None, // No pause - should progress immediately
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0),
            phase: Some(Phase::Progressing),
            ..Default::default()
        }),
    };

    // Function to test: should_progress_to_next_step
    // Returns true if:
    // - No pause defined in current step
    // - (Future: metrics look good)
    let should_progress = should_progress_to_next_step(&rollout);

    assert!(should_progress, "Should progress when no pause is defined");
}

#[tokio::test]
async fn test_should_not_progress_when_paused() {
    // Test that we DON'T progress when current step has pause
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: Some(crate::crd::rollout::PauseDuration {
                                duration: Some("5m".to_string()),
                            }),
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0),
            phase: Some(Phase::Paused), // Currently paused
            ..Default::default()
        }),
    };

    let should_progress = should_progress_to_next_step(&rollout);

    assert!(!should_progress, "Should NOT progress when paused");
}

#[tokio::test]
async fn test_advance_to_next_step() {
    // Test advancing from step 0 to step 1
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0),
            current_weight: Some(20),
            phase: Some(Phase::Progressing),
            ..Default::default()
        }),
    };

    // Function to test: advance_to_next_step
    // Returns new RolloutStatus with:
    // - current_step_index = 1
    // - current_weight = 50
    // - phase = "Progressing"
    let new_status = advance_to_next_step(&rollout);

    assert_eq!(new_status.current_step_index, Some(1));
    assert_eq!(new_status.current_weight, Some(50));
    assert_eq!(new_status.phase, Some(Phase::Progressing));
    assert_eq!(
        new_status.message,
        Some("Advanced to step 1 (50% traffic)".to_string())
    );
}

#[tokio::test]
async fn test_advance_to_final_step() {
    // Test advancing to the last step marks rollout as Complete
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(100), // Final step: 100% canary
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0),
            current_weight: Some(20),
            phase: Some(Phase::Progressing),
            ..Default::default()
        }),
    };

    // Advance from step 0 to step 1 (final step)
    let new_status = advance_to_next_step(&rollout);

    assert_eq!(new_status.current_step_index, Some(1));
    assert_eq!(new_status.current_weight, Some(100));

    // When reaching final step (100% canary), phase should be "Completed"
    assert_eq!(new_status.phase, Some(Phase::Completed));
    assert_eq!(
        new_status.message,
        Some("Rollout completed: 100% traffic to canary".to_string())
    );
}

// TDD Cycle 17: Integrate Step Progression into Reconcile
// RED: Test that reconcile updates Rollout status

#[tokio::test]
async fn test_compute_desired_status_for_new_rollout() {
    // Test that a new Rollout (no status) gets initialized
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: None, // No status - should be initialized
    };

    // Function to test: compute_desired_status
    // Returns the status that should be written to K8s
    let desired_status = compute_desired_status(&rollout);

    // Should initialize to step 0
    assert_eq!(desired_status.current_step_index, Some(0));
    assert_eq!(desired_status.current_weight, Some(20));
    assert_eq!(desired_status.phase, Some(Phase::Progressing));
}

#[tokio::test]
async fn test_compute_desired_status_progresses_step() {
    // Test that a Rollout at step 0 progresses to step 1
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: None, // No pause - should progress
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0),
            current_weight: Some(20),
            phase: Some(Phase::Progressing),
            ..Default::default()
        }),
    };

    // Should progress to step 1
    let desired_status = compute_desired_status(&rollout);

    assert_eq!(desired_status.current_step_index, Some(1));
    assert_eq!(desired_status.current_weight, Some(50));
    assert_eq!(desired_status.phase, Some(Phase::Progressing));
}

#[tokio::test]
async fn test_compute_desired_status_respects_pause() {
    // Test that a Rollout at a paused step doesn't progress
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-rollout".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector::default(),
            template: k8s_openapi::api::core::v1::PodTemplateSpec::default(),
            strategy: RolloutStrategy {
                canary: Some(CanaryStrategy {
                    canary_service: "test-app-canary".to_string(),
                    stable_service: "test-app-stable".to_string(),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(20),
                            pause: Some(crate::crd::rollout::PauseDuration {
                                duration: Some("5m".to_string()),
                            }),
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                    ],
                    traffic_routing: None,
                }),
            },
        },
        status: Some(RolloutStatus {
            current_step_index: Some(0),
            current_weight: Some(20),
            phase: Some(Phase::Paused),
            ..Default::default()
        }),
    };

    // Should NOT progress (paused)
    let desired_status = compute_desired_status(&rollout);

    // Should stay at step 0
    assert_eq!(desired_status.current_step_index, Some(0));
    assert_eq!(desired_status.current_weight, Some(20));
    assert_eq!(desired_status.phase, Some(Phase::Paused));
}

// TDD Cycle 18: Pause Duration Parsing

#[test]
fn test_parse_duration_seconds() {
    use std::time::Duration;

    let duration = parse_duration("30s").expect("Should parse '30s'");
    assert_eq!(duration, Duration::from_secs(30));
}

#[test]
fn test_parse_duration_minutes() {
    use std::time::Duration;

    let duration = parse_duration("5m").expect("Should parse '5m'");
    assert_eq!(duration, Duration::from_secs(300)); // 5 * 60
}

#[test]
fn test_parse_duration_hours() {
    use std::time::Duration;

    let duration = parse_duration("2h").expect("Should parse '2h'");
    assert_eq!(duration, Duration::from_secs(7200)); // 2 * 3600
}

#[test]
fn test_parse_duration_invalid_unit() {
    let duration = parse_duration("5x");
    assert!(duration.is_none(), "Should return None for invalid unit");
}

#[test]
fn test_parse_duration_empty_string() {
    let duration = parse_duration("");
    assert!(duration.is_none(), "Should return None for empty string");
}

#[test]
fn test_parse_duration_no_number() {
    let duration = parse_duration("s");
    assert!(duration.is_none(), "Should return None when no number");
}

// TDD Cycle 18: Time-based Pause Progression

#[test]
fn test_should_progress_when_pause_duration_elapsed() {
    use crate::crd::rollout::{CanaryStep, PauseDuration, RolloutStatus};
    use chrono::{Duration, Utc};

    // Create a rollout with a step that has a 5m pause
    let mut rollout = create_test_rollout_with_canary();

    // Set step with 5 minute pause
    if let Some(ref mut canary) = rollout.spec.strategy.canary {
        canary.steps = vec![
            CanaryStep {
                set_weight: Some(20),
                pause: Some(PauseDuration {
                    duration: Some("5m".to_string()),
                }),
            },
            CanaryStep {
                set_weight: Some(100),
                pause: None,
            },
        ];
    }

    // Set status with pause that started 6 minutes ago
    let pause_start = Utc::now() - Duration::minutes(6);
    rollout.status = Some(RolloutStatus {
        current_step_index: Some(0),
        current_weight: Some(20),
        phase: Some(Phase::Progressing),
        message: Some("At step 0".to_string()),
        pause_start_time: Some(pause_start.to_rfc3339()),
        ..Default::default()
    });

    // Should progress because duration elapsed
    assert!(
        should_progress_to_next_step(&rollout),
        "Should progress when pause duration elapsed"
    );
}

#[test]
fn test_should_not_progress_when_pause_duration_not_elapsed() {
    use crate::crd::rollout::{CanaryStep, PauseDuration, RolloutStatus};
    use chrono::{Duration, Utc};

    // Create a rollout with a step that has a 5m pause
    let mut rollout = create_test_rollout_with_canary();

    // Set step with 5 minute pause
    if let Some(ref mut canary) = rollout.spec.strategy.canary {
        canary.steps = vec![
            CanaryStep {
                set_weight: Some(20),
                pause: Some(PauseDuration {
                    duration: Some("5m".to_string()),
                }),
            },
            CanaryStep {
                set_weight: Some(100),
                pause: None,
            },
        ];
    }

    // Set status with pause that started 2 minutes ago
    let pause_start = Utc::now() - Duration::minutes(2);
    rollout.status = Some(RolloutStatus {
        current_step_index: Some(0),
        current_weight: Some(20),
        phase: Some(Phase::Progressing),
        message: Some("At step 0".to_string()),
        pause_start_time: Some(pause_start.to_rfc3339()),
        ..Default::default()
    });

    // Should NOT progress because duration not elapsed
    assert!(
        !should_progress_to_next_step(&rollout),
        "Should not progress when pause duration not elapsed"
    );
}

#[test]
fn test_advance_sets_pause_start_time() {
    use crate::crd::rollout::{CanaryStep, PauseDuration, RolloutStatus};

    // Create rollout with step that has pause
    let mut rollout = create_test_rollout_with_canary();

    // Set step with pause
    if let Some(ref mut canary) = rollout.spec.strategy.canary {
        canary.steps = vec![
            CanaryStep {
                set_weight: Some(20),
                pause: Some(PauseDuration {
                    duration: Some("5m".to_string()),
                }),
            },
            CanaryStep {
                set_weight: Some(100),
                pause: None,
            },
        ];
    }

    // Set initial status (before step 0)
    rollout.status = Some(RolloutStatus {
        current_step_index: Some(-1),
        current_weight: Some(0),
        phase: Some(Phase::Initializing),
        message: Some("Starting".to_string()),
        pause_start_time: None,
        ..Default::default()
    });

    // Advance to step 0 (which has pause)
    let new_status = advance_to_next_step(&rollout);

    // Should set pause_start_time
    assert!(
        new_status.pause_start_time.is_some(),
        "Should set pause_start_time when advancing to step with pause"
    );

    // Verify it's a valid RFC3339 timestamp
    use chrono::DateTime;
    let timestamp = new_status.pause_start_time.unwrap();
    assert!(
        DateTime::parse_from_rfc3339(&timestamp).is_ok(),
        "pause_start_time should be valid RFC3339"
    );
}

#[test]
fn test_advance_clears_pause_start_time_when_no_pause() {
    use crate::crd::rollout::{CanaryStep, PauseDuration, RolloutStatus};

    // Create rollout with step that has no pause
    let mut rollout = create_test_rollout_with_canary();

    // Set steps: first has pause, second doesn't
    if let Some(ref mut canary) = rollout.spec.strategy.canary {
        canary.steps = vec![
            CanaryStep {
                set_weight: Some(20),
                pause: Some(PauseDuration {
                    duration: Some("5m".to_string()),
                }),
            },
            CanaryStep {
                set_weight: Some(100),
                pause: None,
            },
        ];
    }

    // Set status at step 0 with pause_start_time set
    rollout.status = Some(RolloutStatus {
        current_step_index: Some(0),
        current_weight: Some(20),
        phase: Some(Phase::Progressing),
        message: Some("At step 0".to_string()),
        pause_start_time: Some("2025-01-01T00:00:00Z".to_string()),
        ..Default::default()
    });

    // Advance to step 1 (which has no pause)
    let new_status = advance_to_next_step(&rollout);

    // Should clear pause_start_time
    assert!(
        new_status.pause_start_time.is_none(),
        "Should clear pause_start_time when advancing to step without pause"
    );
}

// TDD Cycle 18: Manual Promotion

#[test]
fn test_has_promote_annotation() {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    // Create rollout with promote annotation
    let mut rollout = create_test_rollout_with_canary();

    let mut annotations = BTreeMap::new();
    annotations.insert("kulta.io/promote".to_string(), "true".to_string());

    rollout.metadata = ObjectMeta {
        name: Some("test".to_string()),
        namespace: Some("default".to_string()),
        annotations: Some(annotations),
        ..Default::default()
    };

    // has_promote_annotation is private, so we test through should_progress_to_next_step
    // which calls it internally

    // Add a pause step
    use crate::crd::rollout::{CanaryStep, PauseDuration, RolloutStatus};
    if let Some(ref mut canary) = rollout.spec.strategy.canary {
        canary.steps = vec![
            CanaryStep {
                set_weight: Some(20),
                pause: Some(PauseDuration { duration: None }), // Indefinite pause
            },
            CanaryStep {
                set_weight: Some(100),
                pause: None,
            },
        ];
    }

    rollout.status = Some(RolloutStatus {
        current_step_index: Some(0),
        current_weight: Some(20),
        phase: Some(Phase::Progressing),
        message: Some("At step 0".to_string()),
        pause_start_time: Some("2025-01-01T00:00:00Z".to_string()),
        ..Default::default()
    });

    // Should progress due to promote annotation
    assert!(
        should_progress_to_next_step(&rollout),
        "Should progress when promote annotation is set"
    );
}

#[test]
fn test_should_progress_when_promoted() {
    use crate::crd::rollout::{CanaryStep, PauseDuration, RolloutStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    // Create rollout with indefinite pause
    let mut rollout = create_test_rollout_with_canary();

    if let Some(ref mut canary) = rollout.spec.strategy.canary {
        canary.steps = vec![
            CanaryStep {
                set_weight: Some(20),
                pause: Some(PauseDuration { duration: None }), // Indefinite pause
            },
            CanaryStep {
                set_weight: Some(100),
                pause: None,
            },
        ];
    }

    // Set status at paused step
    rollout.status = Some(RolloutStatus {
        current_step_index: Some(0),
        current_weight: Some(20),
        phase: Some(Phase::Progressing),
        message: Some("At step 0".to_string()),
        pause_start_time: Some("2025-01-01T00:00:00Z".to_string()),
        ..Default::default()
    });

    // WITHOUT annotation - should not progress
    assert!(
        !should_progress_to_next_step(&rollout),
        "Should not progress indefinite pause without promotion"
    );

    // WITH annotation - should progress
    let mut annotations = BTreeMap::new();
    annotations.insert("kulta.io/promote".to_string(), "true".to_string());
    rollout.metadata = ObjectMeta {
        name: Some("test".to_string()),
        namespace: Some("default".to_string()),
        annotations: Some(annotations),
        ..Default::default()
    };

    assert!(
        should_progress_to_next_step(&rollout),
        "Should progress indefinite pause with promotion annotation"
    );
}

// TDD Cycle 1: RED - Test replica calculation for canary scaling
#[test]
fn test_calculate_replica_split_0_percent() {
    let (stable, canary) = calculate_replica_split(3, 0);
    assert_eq!(stable, 3, "0% weight should give all replicas to stable");
    assert_eq!(canary, 0, "0% weight should give 0 canary replicas");
}

#[test]
fn test_calculate_replica_split_10_percent() {
    let (stable, canary) = calculate_replica_split(3, 10);
    assert_eq!(stable, 2, "10% of 3 should give 2 stable replicas");
    assert_eq!(canary, 1, "10% of 3 should give 1 canary replica (ceil)");
}

#[test]
fn test_calculate_replica_split_50_percent() {
    let (stable, canary) = calculate_replica_split(3, 50);
    assert_eq!(stable, 1, "50% of 3 should give 1 stable replica");
    assert_eq!(canary, 2, "50% of 3 should give 2 canary replicas (ceil)");
}

#[test]
fn test_calculate_replica_split_100_percent() {
    let (stable, canary) = calculate_replica_split(3, 100);
    assert_eq!(stable, 0, "100% weight should give 0 stable replicas");
    assert_eq!(canary, 3, "100% weight should give all replicas to canary");
}

#[test]
fn test_calculate_replica_split_with_rounding() {
    // 33% of 3 = 0.99, should ceil to 1
    let (stable, canary) = calculate_replica_split(3, 33);
    assert_eq!(canary, 1, "33% of 3 should ceil to 1 canary replica");
    assert_eq!(stable, 2, "Remaining should be 2 stable replicas");
}

#[test]
fn test_calculate_replica_split_large_count() {
    let (stable, canary) = calculate_replica_split(10, 25);
    assert_eq!(canary, 3, "25% of 10 should ceil to 3 canary replicas");
    assert_eq!(stable, 7, "Remaining should be 7 stable replicas");
}
