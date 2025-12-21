//! Tests for health endpoints
//!
//! TDD Cycle 1: Health server responds to /healthz

use super::*;
use crate::server::create_metrics;
use std::time::Duration;

/// Wait for server to be ready with retry logic
///
/// Retries connection up to max_retries times with exponential backoff.
/// More reliable than fixed sleep for test environments.
async fn wait_for_server(port: u16, max_retries: u32) -> reqwest::Client {
    let client = reqwest::Client::new();
    let mut delay = Duration::from_millis(10);

    for attempt in 1..=max_retries {
        match client
            .get(format!("http://127.0.0.1:{}/healthz", port))
            .timeout(Duration::from_millis(100))
            .send()
            .await
        {
            Ok(_) => return client,
            Err(_) if attempt < max_retries => {
                tokio::time::sleep(delay).await;
                delay = std::cmp::min(delay * 2, Duration::from_millis(200));
            }
            Err(e) => panic!("Server not ready after {} attempts: {}", max_retries, e),
        }
    }
    client
}

/// Test that health server starts and /healthz returns 200
#[tokio::test]
async fn test_healthz_returns_200() {
    // ARRANGE: Create readiness state and start server
    let readiness = ReadinessState::new();
    let metrics = create_metrics().expect("create metrics");
    let port = 18080; // Use high port for tests

    // Start server in background
    let server_readiness = readiness.clone();
    let server_metrics = metrics.clone();
    let server_handle =
        tokio::spawn(
            async move { run_health_server(port, server_readiness, server_metrics).await },
        );

    // Wait for server to be ready (with retry)
    let client = wait_for_server(port, 10).await;

    // ACT: Make request to /healthz
    let response = client
        .get(format!("http://127.0.0.1:{}/healthz", port))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("Failed to connect to health server");

    // ASSERT: Should return 200 OK
    assert_eq!(response.status(), 200, "Liveness probe should return 200");

    // Cleanup
    server_handle.abort();
}

/// Test that /readyz returns 503 when not ready
#[tokio::test]
async fn test_readyz_returns_503_when_not_ready() {
    // ARRANGE: Create readiness state (NOT ready by default)
    let readiness = ReadinessState::new();
    let metrics = create_metrics().expect("create metrics");
    assert!(!readiness.is_ready(), "Should start as not ready");

    let port = 18081;

    // Start server in background
    let server_readiness = readiness.clone();
    let server_metrics = metrics.clone();
    let server_handle =
        tokio::spawn(
            async move { run_health_server(port, server_readiness, server_metrics).await },
        );

    // Wait for server to be ready (with retry)
    let client = wait_for_server(port, 10).await;

    // ACT: Make request to /readyz
    let response = client
        .get(format!("http://127.0.0.1:{}/readyz", port))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("Failed to connect to health server");

    // ASSERT: Should return 503 Service Unavailable
    assert_eq!(
        response.status(),
        503,
        "Readiness probe should return 503 when not ready"
    );

    server_handle.abort();
}

/// Test that /readyz returns 200 when ready
#[tokio::test]
async fn test_readyz_returns_200_when_ready() {
    // ARRANGE: Create readiness state and mark as ready
    let readiness = ReadinessState::new();
    let metrics = create_metrics().expect("create metrics");
    readiness.set_ready();
    assert!(readiness.is_ready(), "Should be ready after set_ready()");

    let port = 18082;

    // Start server in background
    let server_readiness = readiness.clone();
    let server_metrics = metrics.clone();
    let server_handle =
        tokio::spawn(
            async move { run_health_server(port, server_readiness, server_metrics).await },
        );

    // Wait for server to be ready (with retry)
    let client = wait_for_server(port, 10).await;

    // ACT: Make request to /readyz
    let response = client
        .get(format!("http://127.0.0.1:{}/readyz", port))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("Failed to connect to health server");

    // ASSERT: Should return 200 OK
    assert_eq!(
        response.status(),
        200,
        "Readiness probe should return 200 when ready"
    );

    server_handle.abort();
}

/// Test ReadinessState basic functionality
#[test]
fn test_readiness_state_transitions() {
    let state = ReadinessState::new();

    // Initially not ready
    assert!(!state.is_ready());

    // After set_ready, should be ready
    state.set_ready();
    assert!(state.is_ready());

    // Clone should share state
    let cloned = state.clone();
    assert!(cloned.is_ready());
}

/// Test that /metrics returns Prometheus format
#[tokio::test]
async fn test_metrics_returns_prometheus_format() {
    // ARRANGE: Create readiness state and metrics
    let readiness = ReadinessState::new();
    let metrics = create_metrics().expect("create metrics");
    let port = 18083;

    // Record some metrics so they appear in output
    metrics.record_reconciliation_success("canary", 0.5);

    // Start server in background
    let server_readiness = readiness.clone();
    let server_metrics = metrics.clone();
    let server_handle =
        tokio::spawn(
            async move { run_health_server(port, server_readiness, server_metrics).await },
        );

    // Wait for server to be ready (with retry)
    let client = wait_for_server(port, 10).await;

    // ACT: Make request to /metrics
    let response = client
        .get(format!("http://127.0.0.1:{}/metrics", port))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("Failed to connect to metrics endpoint");

    // ASSERT: Should return 200 OK with Prometheus content type
    assert_eq!(response.status(), 200, "Metrics should return 200");

    let content_type = response
        .headers()
        .get("content-type")
        .expect("should have content-type")
        .to_str()
        .expect("content-type should be string");
    assert!(
        content_type.contains("text/plain"),
        "Should be text/plain for Prometheus"
    );

    // Check body contains our metrics
    let body = response.text().await.expect("should have body");
    assert!(
        body.contains("kulta_reconciliations_total"),
        "Should contain reconciliations counter"
    );
    assert!(
        body.contains("kulta_reconciliation_duration_seconds"),
        "Should contain duration histogram"
    );

    server_handle.abort();
}
