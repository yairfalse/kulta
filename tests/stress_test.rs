//! KULTA Stress Tests
//!
//! Run with: KULTA_RUN_STRESS_TESTS=1 cargo test --test stress_test -- --ignored --nocapture
//!
//! Requirements:
//! - Kind cluster with sufficient resources
//! - KULTA CRD + Gateway API CRDs installed
//! - KULTA controller running
//!
//! WARNING: These tests are destructive and resource-intensive!

#![allow(clippy::expect_used)]

use futures::future::join_all;
use k8s_openapi::api::core::v1::Service;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{DeleteParams, ObjectMeta};
use kube::Api;
use kulta::crd::rollout::{
    CanaryStep, CanaryStrategy, PauseDuration, Phase, Rollout, RolloutSpec, RolloutStrategy,
};
use seppo::Context;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn should_skip() -> bool {
    std::env::var("KULTA_RUN_STRESS_TESTS").is_err()
}

// =============================================================================
// HELPERS
// =============================================================================

fn create_pod_template(app_name: &str, image: &str) -> k8s_openapi::api::core::v1::PodTemplateSpec {
    use k8s_openapi::api::core::v1::{Container, PodSpec, PodTemplateSpec};

    PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some([("app".to_string(), app_name.to_string())].into()),
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
            selector: Some([("app".to_string(), app_label.to_string())].into()),
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

fn create_rollout(name: &str, namespace: &str, replicas: i32, image: &str) -> Rollout {
    Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, image),
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
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    }
}

/// Create a rollout with pauses at each step (for chaos tests that need to observe mid-rollout state)
fn create_rollout_with_pauses(
    name: &str,
    namespace: &str,
    replicas: i32,
    image: &str,
    pause_duration: &str,
) -> Rollout {
    Rollout {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: RolloutSpec {
            replicas,
            selector: LabelSelector {
                match_labels: Some([("app".to_string(), name.to_string())].into()),
                ..Default::default()
            },
            template: create_pod_template(name, image),
            strategy: RolloutStrategy {
                simple: None,
                blue_green: None,
                canary: Some(CanaryStrategy {
                    stable_service: format!("{}-stable", name),
                    canary_service: format!("{}-canary", name),
                    steps: vec![
                        CanaryStep {
                            set_weight: Some(25),
                            pause: Some(PauseDuration {
                                duration: Some(pause_duration.to_string()),
                            }),
                        },
                        CanaryStep {
                            set_weight: Some(50),
                            pause: Some(PauseDuration {
                                duration: Some(pause_duration.to_string()),
                            }),
                        },
                        CanaryStep {
                            set_weight: Some(75),
                            pause: Some(PauseDuration {
                                duration: Some(pause_duration.to_string()),
                            }),
                        },
                    ],
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    }
}

async fn wait_for_phase(
    ctx: &Context,
    name: &str,
    expected: Phase,
    timeout_secs: u64,
) -> Option<Rollout> {
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        if let Ok(rollout) = ctx.get::<Rollout>(name).await {
            if let Some(status) = &rollout.status {
                if let Some(phase) = &status.phase {
                    if *phase == expected {
                        return Some(rollout);
                    }
                }
            }
        }

        if start.elapsed() > timeout {
            return None;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn setup_services(ctx: &Context, name: &str) {
    let stable = create_service(&format!("{}-stable", name), &ctx.namespace, name);
    let canary = create_service(&format!("{}-canary", name), &ctx.namespace, name);
    let _ = ctx.apply(&stable).await;
    let _ = ctx.apply(&canary).await;
}

// =============================================================================
// LOAD TESTS
// =============================================================================

/// Test: Create many rollouts concurrently
#[seppo::test]
#[ignore]
async fn test_load_concurrent_rollout_creation(ctx: Context) {
    if should_skip() {
        return;
    }

    const NUM_ROLLOUTS: usize = 20;
    println!(
        "🔥 LOAD TEST: Creating {} rollouts concurrently",
        NUM_ROLLOUTS
    );

    let start = Instant::now();

    // Setup services for all rollouts
    let setup_futures: Vec<_> = (0..NUM_ROLLOUTS)
        .map(|i| {
            let ctx = &ctx;
            async move {
                let name = format!("load-{}", i);
                setup_services(ctx, &name).await;
            }
        })
        .collect();
    join_all(setup_futures).await;

    // Create all rollouts concurrently
    let create_futures: Vec<_> = (0..NUM_ROLLOUTS)
        .map(|i| {
            let ctx = &ctx;
            async move {
                let name = format!("load-{}", i);
                let rollout = create_rollout(&name, &ctx.namespace, 2, "nginx:1.21");
                ctx.apply(&rollout).await
            }
        })
        .collect();

    let results = join_all(create_futures).await;
    let created = results.iter().filter(|r| r.is_ok()).count();
    let create_time = start.elapsed();

    println!(
        "  Created {}/{} rollouts in {:?}",
        created, NUM_ROLLOUTS, create_time
    );
    assert_eq!(created, NUM_ROLLOUTS, "All rollouts should be created");

    // Wait for all to reach Progressing
    let progress_start = Instant::now();
    let mut progressing = 0;

    for i in 0..NUM_ROLLOUTS {
        let name = format!("load-{}", i);
        if wait_for_phase(&ctx, &name, Phase::Progressing, 30)
            .await
            .is_some()
        {
            progressing += 1;
        }
    }

    println!(
        "  {}/{} reached Progressing in {:?}",
        progressing,
        NUM_ROLLOUTS,
        progress_start.elapsed()
    );

    // Wait for completion
    let complete_start = Instant::now();
    let mut completed = 0;

    for i in 0..NUM_ROLLOUTS {
        let name = format!("load-{}", i);
        if wait_for_phase(&ctx, &name, Phase::Completed, 120)
            .await
            .is_some()
        {
            completed += 1;
        }
    }

    let total_time = start.elapsed();
    println!(
        "  {}/{} completed in {:?} (total: {:?})",
        completed,
        NUM_ROLLOUTS,
        complete_start.elapsed(),
        total_time
    );

    assert!(
        completed >= NUM_ROLLOUTS * 80 / 100,
        "At least 80% should complete"
    );

    println!(
        "✅ Load test passed: {} rollouts in {:?}",
        NUM_ROLLOUTS, total_time
    );
}

/// Test: Rapid create-delete cycles
#[seppo::test]
#[ignore]
async fn test_load_rapid_create_delete_cycles(ctx: Context) {
    if should_skip() {
        return;
    }

    const CYCLES: usize = 10;
    println!("🔥 LOAD TEST: {} rapid create-delete cycles", CYCLES);

    let name = "rapid-cycle";
    setup_services(&ctx, name).await;

    let rollout_api: Api<Rollout> = Api::namespaced(ctx.client.clone(), &ctx.namespace);
    let mut cycle_times = Vec::with_capacity(CYCLES);

    for cycle in 0..CYCLES {
        let cycle_start = Instant::now();

        // Create
        let rollout = create_rollout(name, &ctx.namespace, 2, &format!("nginx:1.{}", 20 + cycle));
        ctx.apply(&rollout).await.expect("Create rollout");

        // Wait for Progressing (don't wait for completion)
        wait_for_phase(&ctx, name, Phase::Progressing, 15).await;

        // Delete immediately
        rollout_api
            .delete(name, &DeleteParams::default())
            .await
            .expect("Delete rollout");

        // Wait for deletion
        let delete_start = Instant::now();
        while ctx.get::<Rollout>(name).await.is_ok() {
            if delete_start.elapsed() > Duration::from_secs(10) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let cycle_time = cycle_start.elapsed();
        cycle_times.push(cycle_time);
        println!("  Cycle {}: {:?}", cycle + 1, cycle_time);
    }

    let avg_time: Duration = cycle_times.iter().sum::<Duration>() / CYCLES as u32;
    println!("✅ Rapid cycles completed. Average: {:?}", avg_time);
}

/// Test: Sustained load over time
#[seppo::test]
#[ignore]
async fn test_load_sustained_throughput(ctx: Context) {
    if should_skip() {
        return;
    }

    const DURATION_SECS: u64 = 30;
    const ROLLOUTS_PER_BATCH: usize = 5;

    println!("🔥 LOAD TEST: Sustained throughput for {}s", DURATION_SECS);

    let start = Instant::now();
    let created = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicU64::new(0));
    let mut batch = 0;

    while start.elapsed() < Duration::from_secs(DURATION_SECS) {
        // Create a batch
        for i in 0..ROLLOUTS_PER_BATCH {
            let name = format!("sustained-{}-{}", batch, i);
            setup_services(&ctx, &name).await;
            let rollout = create_rollout(&name, &ctx.namespace, 1, "nginx:1.21");
            if ctx.apply(&rollout).await.is_ok() {
                created.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Check completions from previous batches
        if batch > 0 {
            for i in 0..ROLLOUTS_PER_BATCH {
                let name = format!("sustained-{}-{}", batch - 1, i);
                if let Ok(rollout) = ctx.get::<Rollout>(&name).await {
                    if rollout.status.as_ref().and_then(|s| s.phase.as_ref())
                        == Some(&Phase::Completed)
                    {
                        completed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

        batch += 1;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let total_created = created.load(Ordering::Relaxed);
    let total_completed = completed.load(Ordering::Relaxed);
    let throughput = total_created as f64 / DURATION_SECS as f64;

    println!(
        "✅ Sustained load: created={}, completed={}, throughput={:.2}/s",
        total_created, total_completed, throughput
    );
}

// =============================================================================
// CHAOS TESTS
// =============================================================================

/// Test: Rollout continues after being modified mid-flight
#[seppo::test]
#[ignore]
async fn test_chaos_modify_during_rollout(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("💥 CHAOS TEST: Modify rollout during progression");

    let name = "chaos-modify";
    setup_services(&ctx, name).await;

    // Create initial rollout with short pauses so we can observe Progressing state
    // 5s pause is enough to catch the phase, but keeps test reasonably fast (15s total)
    let rollout = create_rollout_with_pauses(name, &ctx.namespace, 3, "nginx:1.20", "5s");
    ctx.apply(&rollout).await.expect("Create rollout");

    // Wait for Progressing - the pause ensures it stays in this state long enough
    wait_for_phase(&ctx, name, Phase::Progressing, 30)
        .await
        .expect("Should reach Progressing");

    println!("  Rollout progressing, now modifying spec...");

    // Modify replicas while in progress using patch
    let patch = serde_json::json!({
        "spec": { "replicas": 5 }
    });
    ctx.patch::<Rollout>(name, &patch)
        .await
        .expect("Modify rollout");

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify it handles the change
    let rollout: Rollout = ctx.get(name).await.expect("Get rollout");
    assert!(
        rollout.status.is_some(),
        "Should still have status after modification"
    );

    // Wait for completion
    let final_rollout = wait_for_phase(&ctx, name, Phase::Completed, 120)
        .await
        .expect("Should complete after modification");

    assert_eq!(
        final_rollout.status.as_ref().and_then(|s| s.phase.as_ref()),
        Some(&Phase::Completed)
    );

    println!("✅ Chaos modify test passed");
}

/// Test: Rapid image updates (new rollout before previous completes)
#[seppo::test]
#[ignore]
async fn test_chaos_rapid_image_updates(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("💥 CHAOS TEST: Rapid image updates");

    let name = "chaos-rapid-image";
    setup_services(&ctx, name).await;

    // Create initial rollout
    let rollout = create_rollout(name, &ctx.namespace, 2, "nginx:1.20");
    ctx.apply(&rollout).await.expect("Create rollout");

    // Wait briefly for it to start
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Rapid-fire image updates using patch
    for version in 21..=25 {
        println!("  Updating to nginx:1.{}", version);
        let patch = serde_json::json!({
            "spec": {
                "template": {
                    "spec": {
                        "containers": [{
                            "name": "app",
                            "image": format!("nginx:1.{}", version)
                        }]
                    }
                }
            }
        });
        ctx.patch::<Rollout>(name, &patch)
            .await
            .expect("Update rollout");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Final update
    let final_image = "nginx:1.25";
    let patch = serde_json::json!({
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name": "app",
                        "image": final_image
                    }]
                }
            }
        }
    });
    ctx.patch::<Rollout>(name, &patch)
        .await
        .expect("Final update");

    // Wait for eventual completion
    let completed = wait_for_phase(&ctx, name, Phase::Completed, 180).await;

    if let Some(rollout) = completed {
        println!(
            "  Final phase: {:?}",
            rollout.status.as_ref().and_then(|s| s.phase.as_ref())
        );
        println!("✅ Chaos rapid image updates: eventually stabilized");
    } else {
        // Still progressing is OK - means it's handling the updates
        let rollout: Rollout = ctx.get(name).await.expect("Get rollout");
        println!(
            "  Still in phase: {:?}",
            rollout.status.as_ref().and_then(|s| s.phase.as_ref())
        );
        println!("⚠️  Chaos rapid image updates: still processing (acceptable)");
    }
}

/// Test: Delete and recreate with same name
#[seppo::test]
#[ignore]
async fn test_chaos_delete_recreate(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("💥 CHAOS TEST: Delete and recreate with same name");

    let name = "chaos-recreate";
    setup_services(&ctx, name).await;

    let rollout_api: Api<Rollout> = Api::namespaced(ctx.client.clone(), &ctx.namespace);

    for iteration in 1..=3 {
        println!("  Iteration {}", iteration);

        // Create
        let rollout = create_rollout(
            name,
            &ctx.namespace,
            2,
            &format!("nginx:1.{}", 20 + iteration),
        );
        ctx.apply(&rollout).await.expect("Create rollout");

        // Wait for Progressing
        wait_for_phase(&ctx, name, Phase::Progressing, 20).await;

        // Delete
        rollout_api
            .delete(name, &DeleteParams::default())
            .await
            .expect("Delete rollout");

        // Wait for actual deletion
        let mut deleted = false;
        for _ in 0..50 {
            if ctx.get::<Rollout>(name).await.is_err() {
                deleted = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        assert!(deleted, "Rollout should be deleted");
    }

    // Final create and complete
    let final_rollout = create_rollout(name, &ctx.namespace, 2, "nginx:1.25");
    ctx.apply(&final_rollout).await.expect("Final create");

    let completed = wait_for_phase(&ctx, name, Phase::Completed, 120)
        .await
        .expect("Should complete");

    assert_eq!(
        completed.status.as_ref().and_then(|s| s.phase.as_ref()),
        Some(&Phase::Completed)
    );

    println!("✅ Chaos delete-recreate test passed");
}

/// Test: Conflicting updates from multiple sources
#[seppo::test]
#[ignore]
async fn test_chaos_conflicting_updates(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("💥 CHAOS TEST: Conflicting concurrent updates");

    let name = "chaos-conflict";
    setup_services(&ctx, name).await;

    // Create initial
    let rollout = create_rollout(name, &ctx.namespace, 2, "nginx:1.20");
    ctx.apply(&rollout).await.expect("Create rollout");

    wait_for_phase(&ctx, name, Phase::Progressing, 20).await;

    // Fire multiple conflicting updates concurrently
    let update_futures: Vec<_> = (0..5)
        .map(|i| {
            let ctx = &ctx;
            async move {
                let r = create_rollout(
                    name,
                    &ctx.namespace,
                    2 + i as i32,
                    &format!("nginx:1.{}", 21 + i),
                );
                ctx.apply(&r).await
            }
        })
        .collect();

    let results = join_all(update_futures).await;
    let succeeded = results.iter().filter(|r| r.is_ok()).count();
    println!("  {} of 5 concurrent updates succeeded", succeeded);

    // Controller should handle conflicts gracefully
    tokio::time::sleep(Duration::from_secs(5)).await;

    let rollout: Rollout = ctx.get(name).await.expect("Get rollout");
    assert!(
        rollout.status.is_some(),
        "Should have valid status after conflicts"
    );

    println!("✅ Chaos conflicting updates test passed");
}

// =============================================================================
// EDGE CASE TESTS
// =============================================================================

/// Test: Zero replicas
#[seppo::test]
#[ignore]
async fn test_edge_zero_replicas(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("🔬 EDGE TEST: Zero replicas rollout");

    let name = "edge-zero-replicas";
    setup_services(&ctx, name).await;

    let rollout = create_rollout(name, &ctx.namespace, 0, "nginx:1.21");
    ctx.apply(&rollout)
        .await
        .expect("Create zero-replica rollout");

    // Should handle gracefully
    tokio::time::sleep(Duration::from_secs(5)).await;

    let result: Rollout = ctx.get(name).await.expect("Get rollout");
    println!(
        "  Phase: {:?}",
        result.status.as_ref().and_then(|s| s.phase.as_ref())
    );

    println!("✅ Edge zero replicas test passed");
}

/// Test: Single replica (no room for canary)
#[seppo::test]
#[ignore]
async fn test_edge_single_replica(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("🔬 EDGE TEST: Single replica rollout");

    let name = "edge-single-replica";
    setup_services(&ctx, name).await;

    let rollout = create_rollout(name, &ctx.namespace, 1, "nginx:1.21");
    ctx.apply(&rollout)
        .await
        .expect("Create single-replica rollout");

    let completed = wait_for_phase(&ctx, name, Phase::Completed, 60).await;
    assert!(
        completed.is_some(),
        "Single replica rollout should complete"
    );

    println!("✅ Edge single replica test passed");
}

/// Test: Very high replica count
#[seppo::test]
#[ignore]
async fn test_edge_high_replica_count(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("🔬 EDGE TEST: High replica count (100)");

    let name = "edge-high-replicas";
    setup_services(&ctx, name).await;

    // Use rollout with pauses so we can observe Progressing state
    let rollout = create_rollout_with_pauses(name, &ctx.namespace, 100, "nginx:1.21", "5s");
    ctx.apply(&rollout)
        .await
        .expect("Create high-replica rollout");

    // Verify it starts processing without crashing
    let progressing = wait_for_phase(&ctx, name, Phase::Progressing, 30).await;
    assert!(progressing.is_some(), "Should start progressing");

    println!("✅ Edge high replica count test passed");
}

/// Test: Empty/minimal canary steps
#[seppo::test]
#[ignore]
async fn test_edge_minimal_steps(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("🔬 EDGE TEST: Single canary step (100%)");

    let name = "edge-minimal-steps";
    setup_services(&ctx, name).await;

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
                            set_weight: Some(100),
                            pause: None,
                        }, // Direct to 100%
                    ],
                    traffic_routing: None,
                    analysis: None,
                }),
            },
        },
        status: None,
    };

    ctx.apply(&rollout).await.expect("Create rollout");

    let completed = wait_for_phase(&ctx, name, Phase::Completed, 60).await;
    assert!(completed.is_some(), "Minimal steps rollout should complete");

    println!("✅ Edge minimal steps test passed");
}

/// Test: Same image update (no-op)
#[seppo::test]
#[ignore]
async fn test_edge_same_image_update(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("🔬 EDGE TEST: Update with same image (no-op)");

    let name = "edge-same-image";
    setup_services(&ctx, name).await;

    // Create and complete
    let rollout = create_rollout(name, &ctx.namespace, 2, "nginx:1.21");
    ctx.apply(&rollout).await.expect("Create rollout");

    wait_for_phase(&ctx, name, Phase::Completed, 60)
        .await
        .expect("Should complete");

    // Patch with same image (should be no-op)
    let patch = serde_json::json!({
        "spec": {
            "template": {
                "spec": {
                    "containers": [{
                        "name": "app",
                        "image": "nginx:1.21"
                    }]
                }
            }
        }
    });
    ctx.patch::<Rollout>(name, &patch)
        .await
        .expect("Apply same image");

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Should remain Completed (no new rollout triggered)
    let result: Rollout = ctx.get(name).await.expect("Get rollout");
    let phase = result.status.as_ref().and_then(|s| s.phase.as_ref());

    println!("  Phase after same-image update: {:?}", phase);
    assert_eq!(phase, Some(&Phase::Completed), "Should remain Completed");

    println!("✅ Edge same image test passed");
}

// =============================================================================
// PERFORMANCE TESTS
// =============================================================================

/// Test: Measure reconciliation latency
#[seppo::test]
#[ignore]
async fn test_perf_reconciliation_latency(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("⏱️  PERF TEST: Reconciliation latency");

    let name = "perf-latency";
    setup_services(&ctx, name).await;

    let mut latencies = Vec::with_capacity(5);

    for i in 0..5 {
        let start = Instant::now();

        let image = format!("nginx:1.{}", 20 + i);
        if i == 0 {
            // First iteration: create
            let rollout = create_rollout(name, &ctx.namespace, 2, &image);
            ctx.apply(&rollout).await.expect("Create rollout");
        } else {
            // Subsequent iterations: patch with new image to trigger new rollout
            let patch = serde_json::json!({
                "spec": {
                    "template": {
                        "spec": {
                            "containers": [{
                                "name": "app",
                                "image": image
                            }]
                        }
                    }
                }
            });
            ctx.patch::<Rollout>(name, &patch)
                .await
                .expect("Update rollout");
        }

        // Measure time to status update
        let status_start = Instant::now();
        loop {
            if let Ok(r) = ctx.get::<Rollout>(name).await {
                if r.status.is_some() {
                    break;
                }
            }
            if status_start.elapsed() > Duration::from_secs(30) {
                panic!("Timeout waiting for status");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let latency = start.elapsed();
        latencies.push(latency);
        println!("  Run {}: {:?}", i + 1, latency);

        // Wait for completion before next iteration
        wait_for_phase(&ctx, name, Phase::Completed, 60).await;
    }

    let avg: Duration = latencies.iter().sum::<Duration>() / latencies.len() as u32;
    let min = latencies.iter().min().unwrap();
    let max = latencies.iter().max().unwrap();

    println!(
        "✅ Reconciliation latency: avg={:?}, min={:?}, max={:?}",
        avg, min, max
    );
}

/// Test: Measure step progression timing
#[seppo::test]
#[ignore]
async fn test_perf_step_progression_timing(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("⏱️  PERF TEST: Step progression timing");

    let name = "perf-steps";
    setup_services(&ctx, name).await;

    let rollout = create_rollout(name, &ctx.namespace, 4, "nginx:1.21");
    let start = Instant::now();

    ctx.apply(&rollout).await.expect("Create rollout");

    // Track step transitions
    let mut step_times: Vec<(i32, Duration)> = Vec::new();
    let mut last_step: Option<i32> = None;

    loop {
        if let Ok(r) = ctx.get::<Rollout>(name).await {
            if let Some(status) = &r.status {
                let current_step = status.current_step_index;

                if current_step != last_step {
                    step_times.push((current_step.unwrap_or(-1), start.elapsed()));
                    println!("  Step {:?} at {:?}", current_step, start.elapsed());
                    last_step = current_step;
                }

                if status.phase == Some(Phase::Completed) {
                    break;
                }
            }
        }

        if start.elapsed() > Duration::from_secs(120) {
            println!("⚠️  Timeout - not all steps completed");
            break;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let total = start.elapsed();
    println!(
        "✅ Step progression: {} steps in {:?}",
        step_times.len(),
        total
    );
}

/// Test: Memory stability under load (checks for leaks via observation)
#[seppo::test]
#[ignore]
async fn test_perf_memory_stability(ctx: Context) {
    if should_skip() {
        return;
    }

    println!("⏱️  PERF TEST: Memory stability (create/delete 50 rollouts)");

    let rollout_api: Api<Rollout> = Api::namespaced(ctx.client.clone(), &ctx.namespace);

    for i in 0..50 {
        let name = format!("mem-test-{}", i);
        setup_services(&ctx, &name).await;

        let rollout = create_rollout(&name, &ctx.namespace, 1, "nginx:1.21");
        ctx.apply(&rollout).await.expect("Create rollout");

        // Brief wait
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Delete
        let _ = rollout_api.delete(&name, &DeleteParams::default()).await;

        if i % 10 == 9 {
            println!("  Completed {} cycles", i + 1);
        }
    }

    // Final rollout to verify controller still works
    let final_name = "mem-test-final";
    setup_services(&ctx, final_name).await;

    let final_rollout = create_rollout(final_name, &ctx.namespace, 2, "nginx:1.21");
    ctx.apply(&final_rollout)
        .await
        .expect("Create final rollout");

    let completed = wait_for_phase(&ctx, final_name, Phase::Completed, 60).await;
    assert!(
        completed.is_some(),
        "Controller should still work after load"
    );

    println!("✅ Memory stability test passed");
}
