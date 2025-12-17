//! Tests for health endpoints
//!
//! TDD Cycle 1: Health server responds to /healthz

use super::*;
use std::time::Duration;

/// Wait for server to be ready with exponential backoff
/// Returns Ok(()) when server responds, Err after max retries
async fn wait_for_server(port: u16, max_retries: u32) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let mut delay = Duration::from_millis(10);
    for attempt in 0..max_retries {
        match client
            .get(format!("http://127.0.0.1:{}/healthz", port))
            .send()
            .await
        {
            Ok(_) => return Ok(()),
            Err(_) => {
                tokio::time::sleep(delay).await;
                delay = std::cmp::min(delay * 2, Duration::from_millis(500));
                if attempt == max_retries - 1 {
                    return Err(format!("Server not ready after {} attempts", max_retries));
                }
            }
        }
    }
    Err("Server not ready".to_string())
}

/// TDD RED: Test that health server starts and /healthz returns 200
#[tokio::test]
async fn test_healthz_returns_200() {
    // ARRANGE: Create readiness state and start server
    let readiness = ReadinessState::new();
    let port = 18080; // Use high port for tests

    // Start server in background
    let server_readiness = readiness.clone();
    let server_handle =
        tokio::spawn(async move { run_health_server(port, server_readiness).await });

    // Wait for server to be ready (robust retry loop)
    wait_for_server(port, 10)
        .await
        .expect("Server should start");

    // ACT: Make request to /healthz
    let client = reqwest::Client::new();
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

/// TDD RED: Test that /readyz returns 503 when not ready
#[tokio::test]
async fn test_readyz_returns_503_when_not_ready() {
    // ARRANGE: Create readiness state (NOT ready by default)
    let readiness = ReadinessState::new();
    assert!(!readiness.is_ready(), "Should start as not ready");

    let port = 18081;

    // Start server in background
    let server_readiness = readiness.clone();
    let server_handle =
        tokio::spawn(async move { run_health_server(port, server_readiness).await });

    // Wait for server to be ready (robust retry loop)
    wait_for_server(port, 10)
        .await
        .expect("Server should start");

    // ACT: Make request to /readyz
    let client = reqwest::Client::new();
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

/// TDD RED: Test that /readyz returns 200 when ready
#[tokio::test]
async fn test_readyz_returns_200_when_ready() {
    // ARRANGE: Create readiness state and mark as ready
    let readiness = ReadinessState::new();
    readiness.set_ready();
    assert!(readiness.is_ready(), "Should be ready after set_ready()");

    let port = 18082;

    // Start server in background
    let server_readiness = readiness.clone();
    let server_handle =
        tokio::spawn(async move { run_health_server(port, server_readiness).await });

    // Wait for server to be ready (robust retry loop)
    wait_for_server(port, 10)
        .await
        .expect("Server should start");

    // ACT: Make request to /readyz
    let client = reqwest::Client::new();
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
