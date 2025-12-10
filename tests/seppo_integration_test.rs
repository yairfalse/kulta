//! KULTA Integration Tests using Seppo
//!
//! These tests verify KULTA's behavior against a real Kubernetes cluster.
//! Run with: KULTA_RUN_SEPPO_TESTS=1 cargo test --test seppo_integration_test
//!
//! Requirements:
//! - Kind cluster running (or any K8s cluster)
//! - KULTA CRD installed: kubectl apply -f <(cargo run --bin gen-crd)
//! - Gateway API CRDs installed

#![allow(clippy::expect_used)] // Integration tests can use expect for clarity

use k8s_openapi::api::apps::v1::ReplicaSet;
use kube::api::ObjectMeta;
use kulta::crd::rollout::{
    BlueGreenStrategy, CanaryStep, CanaryStrategy, Rollout, RolloutSpec, RolloutStrategy,
    SimpleStrategy,
};

/// Skip if KULTA_RUN_SEPPO_TESTS is not set
fn should_skip() -> bool {
    std::env::var("KULTA_RUN_SEPPO_TESTS").is_err()
}

fn create_pod_template(app_name: &str) -> k8s_openapi::api::core::v1::PodTemplateSpec {
    use k8s_openapi::api::core::v1::{Container, PodSpec, PodTemplateSpec};

    PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some([(String::from("app"), app_name.to_string())].into()),
            ..Default::default()
        }),
        spec: Some(PodSpec {
            containers: vec![Container {
                name: "nginx".to_string(),
                image: Some("nginx:1.21".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }),
    }
}

/// Test that applying a canary Rollout creates stable and canary ReplicaSets
#[seppo::test]
#[ignore] // Requires cluster + CRDs
async fn test_canary_rollout_creates_replicasets(ctx: TestContext) {
    if should_skip() {
        println!("⏭️  Skipping seppo tests (set KULTA_RUN_SEPPO_TESTS=1)");
        return;
    }

    // ARRANGE: Create a canary Rollout
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-canary".to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some([("app".to_string(), "test-canary".to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template("test-canary"),
            strategy: RolloutStrategy {
                simple: None,
                blue_green: None,
                canary: Some(CanaryStrategy {
                    stable_service: "test-canary-stable".to_string(),
                    canary_service: "test-canary-canary".to_string(),
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
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    // ACT: Apply the Rollout
    ctx.apply(&rollout).await.expect("Should apply Rollout");

    // Wait for controller to reconcile
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // ASSERT: List all ReplicaSets and filter by label
    let rs_list: Vec<ReplicaSet> = ctx.list().await.expect("Should list ReplicaSets");

    let managed_rs: Vec<_> = rs_list
        .iter()
        .filter(|rs| {
            rs.metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("rollouts.kulta.io/managed"))
                == Some(&"true".to_string())
        })
        .collect();

    assert!(
        managed_rs.len() >= 2,
        "Expected at least 2 managed ReplicaSets, got {}",
        managed_rs.len()
    );

    // Verify stable ReplicaSet exists
    let stable = managed_rs.iter().find(|rs| {
        rs.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("rollouts.kulta.io/type"))
            == Some(&"stable".to_string())
    });
    assert!(stable.is_some(), "Should have stable ReplicaSet");

    // Verify canary ReplicaSet exists
    let canary = managed_rs.iter().find(|rs| {
        rs.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("rollouts.kulta.io/type"))
            == Some(&"canary".to_string())
    });
    assert!(canary.is_some(), "Should have canary ReplicaSet");

    println!("✅ Canary Rollout created correct ReplicaSets");
}

/// Test that applying a blue-green Rollout creates active and preview ReplicaSets
#[seppo::test]
#[ignore] // Requires cluster + CRDs
async fn test_blue_green_rollout_creates_replicasets(ctx: TestContext) {
    if should_skip() {
        println!("⏭️  Skipping seppo tests (set KULTA_RUN_SEPPO_TESTS=1)");
        return;
    }

    // ARRANGE: Create a blue-green Rollout
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-bluegreen".to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some([("app".to_string(), "test-bluegreen".to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template("test-bluegreen"),
            strategy: RolloutStrategy {
                simple: None,
                canary: None,
                blue_green: Some(BlueGreenStrategy {
                    active_service: "test-bg-active".to_string(),
                    preview_service: "test-bg-preview".to_string(),
                    auto_promotion_enabled: None,
                    auto_promotion_seconds: None,
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    // ACT: Apply the Rollout
    ctx.apply(&rollout).await.expect("Should apply Rollout");

    // Wait for controller to reconcile
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // ASSERT: List all ReplicaSets and filter
    let rs_list: Vec<ReplicaSet> = ctx.list().await.expect("Should list ReplicaSets");

    let managed_rs: Vec<_> = rs_list
        .iter()
        .filter(|rs| {
            rs.metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("rollouts.kulta.io/managed"))
                == Some(&"true".to_string())
        })
        .collect();

    assert!(
        managed_rs.len() >= 2,
        "Expected at least 2 managed ReplicaSets"
    );

    // Verify active and preview ReplicaSets exist
    let active = managed_rs.iter().find(|rs| {
        rs.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("rollouts.kulta.io/type"))
            == Some(&"active".to_string())
    });
    assert!(active.is_some(), "Should have active ReplicaSet");

    let preview = managed_rs.iter().find(|rs| {
        rs.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("rollouts.kulta.io/type"))
            == Some(&"preview".to_string())
    });
    assert!(preview.is_some(), "Should have preview ReplicaSet");

    println!("✅ Blue-green Rollout created correct ReplicaSets");
}

/// Test that simple strategy creates single ReplicaSet
#[seppo::test]
#[ignore] // Requires cluster + CRDs
async fn test_simple_rollout_creates_single_replicaset(ctx: TestContext) {
    if should_skip() {
        println!("⏭️  Skipping seppo tests (set KULTA_RUN_SEPPO_TESTS=1)");
        return;
    }

    // ARRANGE: Create a simple Rollout
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some("test-simple".to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector {
                match_labels: Some([("app".to_string(), "test-simple".to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template("test-simple"),
            strategy: RolloutStrategy {
                canary: None,
                blue_green: None,
                simple: Some(SimpleStrategy { analysis: None }),
            },
        },
        status: None,
    };

    // ACT: Apply the Rollout
    ctx.apply(&rollout).await.expect("Should apply Rollout");

    // Wait for controller to reconcile
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // ASSERT: Should have exactly one managed ReplicaSet
    let rs_list: Vec<ReplicaSet> = ctx.list().await.expect("Should list ReplicaSets");

    let managed_rs: Vec<_> = rs_list
        .iter()
        .filter(|rs| {
            rs.metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("rollouts.kulta.io/managed"))
                == Some(&"true".to_string())
        })
        .collect();

    assert_eq!(
        managed_rs.len(),
        1,
        "Simple strategy should create exactly 1 ReplicaSet"
    );

    let rs = managed_rs[0];
    assert_eq!(
        rs.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("rollouts.kulta.io/type")),
        Some(&"simple".to_string()),
        "Should be labeled as simple type"
    );

    println!("✅ Simple Rollout created correct ReplicaSet");
}
