//! KULTA Integration Tests using Seppo
//!
//! These tests verify KULTA's behavior against a real Kubernetes cluster.
//! Run with: KULTA_RUN_SEPPO_TESTS=1 cargo test --test seppo_integration_test -- --ignored
//!
//! Requirements:
//! - Kind cluster running (or any K8s cluster)
//! - KULTA CRD installed: kubectl apply -f <(cargo run --bin gen-crd)
//! - Gateway API CRDs installed
//! - KULTA controller running

#![allow(clippy::expect_used)] // Integration tests can use expect for clarity

use gateway_api::apis::standard::httproutes::{
    HTTPRoute, HTTPRouteRules, HTTPRouteRulesBackendRefs, HTTPRouteSpec,
};
use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::api::core::v1::Service;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{ObjectMeta, Patch, PatchParams};
use kube::Api;
use kulta::crd::rollout::{
    BlueGreenStrategy, CanaryStep, CanaryStrategy, PauseDuration, Phase, Rollout, RolloutSpec,
    RolloutStrategy, SimpleStrategy, TrafficRouting,
};
use seppo::Context;
use std::time::Duration;

/// Skip if KULTA_RUN_SEPPO_TESTS is not set
fn should_skip() -> bool {
    std::env::var("KULTA_RUN_SEPPO_TESTS").is_err()
}

fn create_pod_template(app_name: &str, image: &str) -> k8s_openapi::api::core::v1::PodTemplateSpec {
    use k8s_openapi::api::core::v1::{Container, PodSpec, PodTemplateSpec};

    PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some([(String::from("app"), app_name.to_string())].into()),
            ..Default::default()
        }),
        spec: Some(PodSpec {
            containers: vec![Container {
                name: "app".to_string(),
                image: Some(image.to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }),
    }
}

fn create_service(name: &str, namespace: &str, app_label: &str) -> Service {
    use k8s_openapi::api::core::v1::{ServicePort, ServiceSpec};
    use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

    Service {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some([(String::from("app"), app_label.to_string())].into()),
            ports: Some(vec![ServicePort {
                port: 80,
                target_port: Some(IntOrString::Int(80)),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn create_httproute(name: &str, namespace: &str, stable_svc: &str, canary_svc: &str) -> HTTPRoute {
    HTTPRoute {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: HTTPRouteSpec {
            rules: Some(vec![HTTPRouteRules {
                backend_refs: Some(vec![
                    HTTPRouteRulesBackendRefs {
                        name: stable_svc.to_string(),
                        weight: Some(100),
                        ..Default::default()
                    },
                    HTTPRouteRulesBackendRefs {
                        name: canary_svc.to_string(),
                        weight: Some(0),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }]),
            ..Default::default()
        },
        status: None,
    }
}

/// Helper to wait for Rollout to reach a specific phase
async fn wait_for_phase(ctx: &Context, name: &str, expected: Phase, timeout_secs: u64) -> Rollout {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let rollout: Rollout = ctx.get(name).await.expect("Should get Rollout");

        if let Some(status) = &rollout.status {
            if let Some(phase) = &status.phase {
                if *phase == expected {
                    return rollout;
                }
            }
        }

        if start.elapsed() > timeout {
            panic!(
                "Timeout waiting for Rollout {} to reach phase {:?}. Current status: {:?}",
                name, expected, rollout.status
            );
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Helper to wait for Rollout to reach a specific step
async fn wait_for_step(ctx: &Context, name: &str, step: i32, timeout_secs: u64) -> Rollout {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let rollout: Rollout = ctx.get(name).await.expect("Should get Rollout");

        if let Some(status) = &rollout.status {
            if status.current_step_index == Some(step) {
                return rollout;
            }
        }

        if start.elapsed() > timeout {
            panic!(
                "Timeout waiting for Rollout {} to reach step {}. Current status: {:?}",
                name, step, rollout.status
            );
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Helper to get managed ReplicaSets for a rollout
async fn get_managed_replicasets(ctx: &Context, rollout_name: &str) -> Vec<ReplicaSet> {
    let rs_list: Vec<ReplicaSet> = ctx.list().await.expect("Should list ReplicaSets");

    rs_list
        .into_iter()
        .filter(|rs| {
            rs.metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("rollouts.kulta.io/rollout"))
                == Some(&rollout_name.to_string())
        })
        .collect()
}

/// Helper to get ReplicaSet by type (stable/canary/active/preview/simple)
fn get_rs_by_type<'a>(replicasets: &'a [ReplicaSet], rs_type: &str) -> Option<&'a ReplicaSet> {
    replicasets.iter().find(|rs| {
        rs.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("rollouts.kulta.io/type"))
            == Some(&rs_type.to_string())
    })
}

// =============================================================================
// CANARY STRATEGY TESTS
// =============================================================================

/// Test full canary rollout lifecycle: Initializing → Progressing (steps) → Completed
#[seppo::test]
#[ignore]
async fn test_canary_full_lifecycle(ctx: TestContext) {
    if should_skip() {
        return;
    }

    let name = "canary-lifecycle";

    // ARRANGE: Create services required for traffic routing
    let stable_svc = create_service(&format!("{}-stable", name), &ctx.namespace, name);
    let canary_svc = create_service(&format!("{}-canary", name), &ctx.namespace, name);
    ctx.apply(&stable_svc).await.expect("Create stable service");
    ctx.apply(&canary_svc).await.expect("Create canary service");

    // Create HTTPRoute for traffic management
    let httproute = create_httproute(
        name,
        &ctx.namespace,
        &format!("{}-stable", name),
        &format!("{}-canary", name),
    );
    ctx.apply(&httproute).await.expect("Create HTTPRoute");

    // Create canary Rollout with steps: 20% → 50% → 100%
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 4, // Use 4 for nice percentages
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.21"),
            strategy: RolloutStrategy {
                simple: None,
                blue_green: None,
                canary: Some(CanaryStrategy {
                    stable_service: format!("{}-stable", name),
                    canary_service: format!("{}-canary", name),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(25),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(75),
                            pause: None,
                        },
                    ],
                    traffic_routing: Some(TrafficRouting {
                        gateway_api: Some(kulta::crd::rollout::GatewayAPIRouting {
                            http_route: name.to_string(),
                        }),
                    }),
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    // ACT: Apply the Rollout
    ctx.apply(&rollout).await.expect("Apply Rollout");

    // ASSERT: Wait for Progressing phase
    let rollout = wait_for_phase(&ctx, name, Phase::Progressing, 30).await;
    assert!(rollout.status.is_some(), "Status should be set");

    // Verify initial step (step 0 = 25% weight)
    let status = rollout.status.as_ref().unwrap();
    assert_eq!(status.current_step_index, Some(0), "Should start at step 0");

    // Wait for step progression and verify replica distribution
    let rollout = wait_for_step(&ctx, name, 1, 60).await;
    let status = rollout.status.as_ref().unwrap();
    assert_eq!(
        status.current_weight,
        Some(50),
        "Step 1 should be 50% weight"
    );

    // Verify ReplicaSet replica distribution at 50%
    let replicasets = get_managed_replicasets(&ctx, name).await;
    assert!(
        replicasets.len() >= 2,
        "Should have stable and canary ReplicaSets"
    );

    let stable_rs = get_rs_by_type(&replicasets, "stable").expect("Should have stable RS");
    let canary_rs = get_rs_by_type(&replicasets, "canary").expect("Should have canary RS");

    let stable_replicas = stable_rs
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(0);
    let canary_replicas = canary_rs
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(0);

    // At 50%, expect roughly even split
    assert_eq!(
        stable_replicas + canary_replicas,
        4,
        "Total should be 4 replicas"
    );
    assert!(
        canary_replicas >= 1,
        "Canary should have at least 1 replica at 50%"
    );

    // Wait for completion
    let rollout = wait_for_phase(&ctx, name, Phase::Completed, 120).await;
    let status = rollout.status.as_ref().unwrap();
    assert_eq!(status.phase, Some(Phase::Completed));

    // Verify final state: all traffic to new version (was canary, now stable)
    let replicasets = get_managed_replicasets(&ctx, name).await;
    let stable_rs = get_rs_by_type(&replicasets, "stable").expect("Should have stable RS");
    let stable_replicas = stable_rs
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(0);
    assert_eq!(
        stable_replicas, 4,
        "All replicas should be stable after completion"
    );

    // Verify decisions history is populated
    assert!(!status.decisions.is_empty(), "Should have decision history");

    println!("✅ Canary full lifecycle test passed");
}

/// Test canary with pause step and manual promotion
#[seppo::test]
#[ignore]
async fn test_canary_pause_and_promote(ctx: TestContext) {
    if should_skip() {
        return;
    }

    let name = "canary-pause";

    // ARRANGE: Create services
    let stable_svc = create_service(&format!("{}-stable", name), &ctx.namespace, name);
    let canary_svc = create_service(&format!("{}-canary", name), &ctx.namespace, name);
    ctx.apply(&stable_svc).await.expect("Create stable service");
    ctx.apply(&canary_svc).await.expect("Create canary service");

    // Create Rollout with pause at step 0
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.21"),
            strategy: RolloutStrategy {
                simple: None,
                blue_green: None,
                canary: Some(CanaryStrategy {
                    stable_service: format!("{}-stable", name),
                    canary_service: format!("{}-canary", name),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(30),
                            pause: Some(PauseDuration { duration: None }), // Manual pause
                        },
                        CanaryStep {
                            set_weight: Some(100),
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
    ctx.apply(&rollout).await.expect("Apply Rollout");

    // Wait for Paused phase
    let rollout = wait_for_phase(&ctx, name, Phase::Paused, 30).await;
    let status = rollout.status.as_ref().unwrap();
    assert_eq!(status.current_step_index, Some(0), "Should pause at step 0");
    assert!(
        status.pause_start_time.is_some(),
        "Should have pause start time"
    );

    // Verify it stays paused for a bit
    tokio::time::sleep(Duration::from_secs(3)).await;
    let rollout: Rollout = ctx.get(name).await.expect("Get Rollout");
    assert_eq!(
        rollout.status.as_ref().and_then(|s| s.phase.as_ref()),
        Some(&Phase::Paused),
        "Should still be paused"
    );

    // ACT: Add promote annotation
    let rollout_api: Api<Rollout> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "rollouts.kulta.io/promote": "true"
            }
        }
    });
    rollout_api
        .patch(
            name,
            &PatchParams::apply("seppo-test"),
            &Patch::Merge(&patch),
        )
        .await
        .expect("Add promote annotation");

    // ASSERT: Wait for completion (should continue past pause)
    let rollout = wait_for_phase(&ctx, name, Phase::Completed, 60).await;
    let status = rollout.status.as_ref().unwrap();
    assert_eq!(status.phase, Some(Phase::Completed));

    println!("✅ Canary pause and promote test passed");
}

/// Test that status.decisions tracks rollout history
#[seppo::test]
#[ignore]
async fn test_status_decisions_tracking(ctx: TestContext) {
    if should_skip() {
        return;
    }

    let name = "decisions-track";

    // ARRANGE: Create services
    let stable_svc = create_service(&format!("{}-stable", name), &ctx.namespace, name);
    let canary_svc = create_service(&format!("{}-canary", name), &ctx.namespace, name);
    ctx.apply(&stable_svc).await.expect("Create stable service");
    ctx.apply(&canary_svc).await.expect("Create canary service");

    // Create simple canary with 2 steps
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 2,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.21"),
            strategy: RolloutStrategy {
                simple: None,
                blue_green: None,
                canary: Some(CanaryStrategy {
                    stable_service: format!("{}-stable", name),
                    canary_service: format!("{}-canary", name),
                    steps: vec![CanaryStep {
                        set_weight: Some(50),
                        pause: None,
                    }],
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    // ACT: Apply and wait for completion
    ctx.apply(&rollout).await.expect("Apply Rollout");
    let rollout = wait_for_phase(&ctx, name, Phase::Completed, 60).await;

    // ASSERT: Verify decisions are recorded
    let status = rollout.status.as_ref().unwrap();
    assert!(
        !status.decisions.is_empty(),
        "Should have recorded decisions"
    );

    // Each decision should have timestamp and action
    for decision in &status.decisions {
        assert!(
            !decision.timestamp.is_empty(),
            "Decision should have timestamp"
        );
        // action is always set (it's not Option)
    }

    // Should have multiple decisions recorded
    assert!(
        status.decisions.len() >= 2,
        "Should have multiple decisions recorded"
    );

    println!("✅ Status decisions tracking test passed");
}

// =============================================================================
// BLUE-GREEN STRATEGY TESTS
// =============================================================================

/// Test blue-green rollout with manual promotion
#[seppo::test]
#[ignore]
async fn test_blue_green_promotion(ctx: TestContext) {
    if should_skip() {
        return;
    }

    let name = "bg-promote";

    // ARRANGE: Create active and preview services
    let active_svc = create_service(&format!("{}-active", name), &ctx.namespace, name);
    let preview_svc = create_service(&format!("{}-preview", name), &ctx.namespace, name);
    ctx.apply(&active_svc).await.expect("Create active service");
    ctx.apply(&preview_svc)
        .await
        .expect("Create preview service");

    // Create blue-green Rollout (no auto-promotion)
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 2,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.21"),
            strategy: RolloutStrategy {
                simple: None,
                canary: None,
                blue_green: Some(BlueGreenStrategy {
                    active_service: format!("{}-active", name),
                    preview_service: format!("{}-preview", name),
                    auto_promotion_enabled: Some(false),
                    auto_promotion_seconds: None,
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    // ACT: Apply the Rollout
    ctx.apply(&rollout).await.expect("Apply Rollout");

    // Wait for Preview phase
    let rollout = wait_for_phase(&ctx, name, Phase::Preview, 30).await;
    let status = rollout.status.as_ref().unwrap();
    assert_eq!(status.phase, Some(Phase::Preview));

    // Verify ReplicaSets created
    let replicasets = get_managed_replicasets(&ctx, name).await;
    assert!(
        replicasets.len() >= 2,
        "Should have active and preview ReplicaSets"
    );

    let active_rs = get_rs_by_type(&replicasets, "active").expect("Should have active RS");
    let preview_rs = get_rs_by_type(&replicasets, "preview").expect("Should have preview RS");

    let active_replicas = active_rs
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(0);
    let preview_replicas = preview_rs
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(0);

    // In Preview: both should have full replicas
    assert_eq!(active_replicas, 2, "Active should have 2 replicas");
    assert_eq!(preview_replicas, 2, "Preview should have 2 replicas");

    // ACT: Promote
    let rollout_api: Api<Rollout> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "rollouts.kulta.io/promote": "true"
            }
        }
    });
    rollout_api
        .patch(
            name,
            &PatchParams::apply("seppo-test"),
            &Patch::Merge(&patch),
        )
        .await
        .expect("Add promote annotation");

    // ASSERT: Wait for completion
    let rollout = wait_for_phase(&ctx, name, Phase::Completed, 60).await;
    assert_eq!(
        rollout.status.as_ref().and_then(|s| s.phase.as_ref()),
        Some(&Phase::Completed)
    );

    // After promotion, active RS should be scaled down
    let replicasets = get_managed_replicasets(&ctx, name).await;
    let active_rs = get_rs_by_type(&replicasets, "active");
    let preview_rs = get_rs_by_type(&replicasets, "preview").expect("Should have preview RS");

    let preview_replicas = preview_rs
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(0);
    assert_eq!(
        preview_replicas, 2,
        "Preview (now active) should have all replicas"
    );

    if let Some(active) = active_rs {
        let active_replicas = active.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);
        assert_eq!(active_replicas, 0, "Old active should be scaled to 0");
    }

    println!("✅ Blue-green promotion test passed");
}

/// Test blue-green with auto-promotion
#[seppo::test]
#[ignore]
async fn test_blue_green_auto_promotion(ctx: TestContext) {
    if should_skip() {
        return;
    }

    let name = "bg-auto";

    // ARRANGE: Create services
    let active_svc = create_service(&format!("{}-active", name), &ctx.namespace, name);
    let preview_svc = create_service(&format!("{}-preview", name), &ctx.namespace, name);
    ctx.apply(&active_svc).await.expect("Create active service");
    ctx.apply(&preview_svc)
        .await
        .expect("Create preview service");

    // Create blue-green with auto-promotion after 5 seconds
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 2,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.21"),
            strategy: RolloutStrategy {
                simple: None,
                canary: None,
                blue_green: Some(BlueGreenStrategy {
                    active_service: format!("{}-active", name),
                    preview_service: format!("{}-preview", name),
                    auto_promotion_enabled: Some(true),
                    auto_promotion_seconds: Some(5),
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    // ACT: Apply the Rollout
    ctx.apply(&rollout).await.expect("Apply Rollout");

    // Should reach Preview first
    wait_for_phase(&ctx, name, Phase::Preview, 30).await;

    // ASSERT: Should auto-promote to Completed
    let rollout = wait_for_phase(&ctx, name, Phase::Completed, 30).await;
    assert_eq!(
        rollout.status.as_ref().and_then(|s| s.phase.as_ref()),
        Some(&Phase::Completed),
        "Should auto-promote to Completed"
    );

    println!("✅ Blue-green auto-promotion test passed");
}

// =============================================================================
// HTTPROUTE TESTS
// =============================================================================

/// Test that HTTPRoute weights are updated during canary progression
#[seppo::test]
#[ignore]
async fn test_httproute_weight_updates(ctx: TestContext) {
    if should_skip() {
        return;
    }

    let name = "httproute-weights";

    // ARRANGE: Create services
    let stable_svc = create_service(&format!("{}-stable", name), &ctx.namespace, name);
    let canary_svc = create_service(&format!("{}-canary", name), &ctx.namespace, name);
    ctx.apply(&stable_svc).await.expect("Create stable service");
    ctx.apply(&canary_svc).await.expect("Create canary service");

    // Create HTTPRoute
    let httproute = create_httproute(
        name,
        &ctx.namespace,
        &format!("{}-stable", name),
        &format!("{}-canary", name),
    );
    ctx.apply(&httproute).await.expect("Create HTTPRoute");

    // Create Rollout with traffic routing
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 2,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.21"),
            strategy: RolloutStrategy {
                simple: None,
                blue_green: None,
                canary: Some(CanaryStrategy {
                    stable_service: format!("{}-stable", name),
                    canary_service: format!("{}-canary", name),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(30),
                            pause: None,
                        },
                        CanaryStep {
                            set_weight: Some(70),
                            pause: None,
                        },
                    ],
                    traffic_routing: Some(TrafficRouting {
                        gateway_api: Some(kulta::crd::rollout::GatewayAPIRouting {
                            http_route: name.to_string(),
                        }),
                    }),
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    // ACT: Apply and wait for step 0 (30% weight)
    ctx.apply(&rollout).await.expect("Apply Rollout");
    wait_for_step(&ctx, name, 0, 30).await;

    // Give controller time to update HTTPRoute
    tokio::time::sleep(Duration::from_secs(2)).await;

    // ASSERT: Check HTTPRoute weights at step 0
    let httproute_api: Api<HTTPRoute> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    let httproute: HTTPRoute = httproute_api.get(name).await.expect("Get HTTPRoute");

    let backend_refs = httproute
        .spec
        .rules
        .as_ref()
        .and_then(|r| r.first())
        .and_then(|r| r.backend_refs.as_ref())
        .expect("Should have backend refs");

    // Find stable and canary weights
    let stable_weight = backend_refs
        .iter()
        .find(|b| b.name.contains("stable"))
        .and_then(|b| b.weight)
        .unwrap_or(0);
    let canary_weight = backend_refs
        .iter()
        .find(|b| b.name.contains("canary"))
        .and_then(|b| b.weight)
        .unwrap_or(0);

    // At step 0 (30% canary)
    assert_eq!(stable_weight, 70, "Stable should have 70% weight");
    assert_eq!(canary_weight, 30, "Canary should have 30% weight");

    // Wait for step 1 (70% weight)
    wait_for_step(&ctx, name, 1, 60).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let httproute: HTTPRoute = httproute_api.get(name).await.expect("Get HTTPRoute");
    let backend_refs = httproute
        .spec
        .rules
        .as_ref()
        .and_then(|r| r.first())
        .and_then(|r| r.backend_refs.as_ref())
        .expect("Should have backend refs");

    let stable_weight = backend_refs
        .iter()
        .find(|b| b.name.contains("stable"))
        .and_then(|b| b.weight)
        .unwrap_or(0);
    let canary_weight = backend_refs
        .iter()
        .find(|b| b.name.contains("canary"))
        .and_then(|b| b.weight)
        .unwrap_or(0);

    assert_eq!(stable_weight, 30, "Stable should have 30% weight at step 1");
    assert_eq!(canary_weight, 70, "Canary should have 70% weight at step 1");

    println!("✅ HTTPRoute weight updates test passed");
}

// =============================================================================
// SIMPLE STRATEGY TESTS
// =============================================================================

/// Test simple strategy creates single ReplicaSet and completes
#[seppo::test]
#[ignore]
async fn test_simple_strategy_lifecycle(ctx: TestContext) {
    if should_skip() {
        return;
    }

    let name = "simple-lifecycle";

    // Create simple Rollout
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 3,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.21"),
            strategy: RolloutStrategy {
                canary: None,
                blue_green: None,
                simple: Some(SimpleStrategy { analysis: None }),
            },
        },
        status: None,
    };

    // ACT: Apply and wait for completion
    ctx.apply(&rollout).await.expect("Apply Rollout");
    let rollout = wait_for_phase(&ctx, name, Phase::Completed, 60).await;

    // ASSERT: Verify status
    let status = rollout.status.as_ref().unwrap();
    assert_eq!(status.phase, Some(Phase::Completed));
    assert_eq!(status.replicas, 3);

    // Verify exactly one ReplicaSet
    let replicasets = get_managed_replicasets(&ctx, name).await;
    assert_eq!(
        replicasets.len(),
        1,
        "Simple strategy should create exactly 1 RS"
    );

    let rs = &replicasets[0];
    assert_eq!(
        rs.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("rollouts.kulta.io/type")),
        Some(&"simple".to_string())
    );

    let replicas = rs.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);
    assert_eq!(replicas, 3, "ReplicaSet should have 3 replicas");

    println!("✅ Simple strategy lifecycle test passed");
}

// =============================================================================
// IMAGE UPDATE TESTS
// =============================================================================

/// Test that updating pod template image triggers new rollout
#[seppo::test]
#[ignore]
async fn test_image_update_triggers_rollout(ctx: TestContext) {
    if should_skip() {
        return;
    }

    let name = "image-update";

    // ARRANGE: Create services
    let stable_svc = create_service(&format!("{}-stable", name), &ctx.namespace, name);
    let canary_svc = create_service(&format!("{}-canary", name), &ctx.namespace, name);
    ctx.apply(&stable_svc).await.expect("Create stable service");
    ctx.apply(&canary_svc).await.expect("Create canary service");

    // Create initial Rollout with nginx:1.20
    let rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 2,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.20"),
            strategy: RolloutStrategy {
                simple: None,
                blue_green: None,
                canary: Some(CanaryStrategy {
                    stable_service: format!("{}-stable", name),
                    canary_service: format!("{}-canary", name),
                    steps: vec![CanaryStep {
                        set_weight: Some(50),
                        pause: None,
                    }],
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    // Apply and wait for completion
    ctx.apply(&rollout).await.expect("Apply initial Rollout");
    wait_for_phase(&ctx, name, Phase::Completed, 60).await;

    // Get pod-template-hash of current stable
    let replicasets = get_managed_replicasets(&ctx, name).await;
    let initial_stable = get_rs_by_type(&replicasets, "stable").expect("Should have stable RS");
    let initial_hash = initial_stable
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get("pod-template-hash"))
        .cloned();

    // ACT: Update image to nginx:1.21
    let updated_rollout = Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ctx.namespace.clone()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas: 2,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, "nginx:1.21"), // New image
            strategy: RolloutStrategy {
                simple: None,
                blue_green: None,
                canary: Some(CanaryStrategy {
                    stable_service: format!("{}-stable", name),
                    canary_service: format!("{}-canary", name),
                    steps: vec![CanaryStep {
                        set_weight: Some(50),
                        pause: None,
                    }],
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    ctx.apply(&updated_rollout)
        .await
        .expect("Apply updated Rollout");

    // Wait for new rollout to start (Progressing phase)
    wait_for_phase(&ctx, name, Phase::Progressing, 30).await;

    // ASSERT: Verify new canary RS exists with different hash
    let replicasets = get_managed_replicasets(&ctx, name).await;
    let canary_rs = get_rs_by_type(&replicasets, "canary").expect("Should have new canary RS");

    let canary_hash = canary_rs
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get("pod-template-hash"))
        .cloned();

    assert_ne!(
        initial_hash, canary_hash,
        "New canary should have different template hash"
    );

    // Verify canary has new image
    let canary_image = canary_rs
        .spec
        .as_ref()
        .and_then(|s| s.template.as_ref())
        .and_then(|t| t.spec.as_ref())
        .and_then(|s| s.containers.first())
        .and_then(|c| c.image.as_ref());

    assert_eq!(
        canary_image,
        Some(&"nginx:1.21".to_string()),
        "Canary should have new image"
    );

    // Wait for completion
    wait_for_phase(&ctx, name, Phase::Completed, 90).await;

    println!("✅ Image update triggers rollout test passed");
}
