//! Tests for leader election

use super::leader::*;
use std::time::Duration;

/// Test LeaderState initial value
#[test]
fn test_leader_state_initially_not_leader() {
    let state = LeaderState::new();
    assert!(!state.is_leader(), "Should not be leader initially");
}

/// Test LeaderState transitions
#[test]
fn test_leader_state_transitions() {
    let state = LeaderState::new();

    assert!(!state.is_leader());

    state.set_leader(true);
    assert!(state.is_leader());

    state.set_leader(false);
    assert!(!state.is_leader());
}

/// Test LeaderState clones share state
#[test]
fn test_leader_state_clones_share_state() {
    let state = LeaderState::new();
    let state2 = state.clone();

    assert!(!state.is_leader());
    assert!(!state2.is_leader());

    state.set_leader(true);

    assert!(state.is_leader());
    assert!(state2.is_leader(), "Clone should reflect same leader state");
}

/// Test LeaderConfig from_env defaults
#[test]
fn test_leader_config_defaults() {
    // Clear env vars to test defaults
    std::env::remove_var("POD_NAME");
    std::env::remove_var("POD_NAMESPACE");
    std::env::remove_var("HOSTNAME");

    let config = LeaderConfig::from_env();

    assert!(
        config.holder_id.starts_with("kulta-"),
        "Should have UUID fallback"
    );
    assert_eq!(config.lease_namespace, "kulta-system");
    assert_eq!(config.lease_name, "kulta-controller-leader");
    assert_eq!(
        config.lease_duration_seconds,
        DEFAULT_LEASE_TTL.as_secs() as i32
    );
    assert_eq!(config.renew_interval, DEFAULT_RENEW_INTERVAL);
}

/// Test LeaderConfig reads from env
#[test]
fn test_leader_config_from_env() {
    std::env::set_var("POD_NAME", "test-pod-123");
    std::env::set_var("POD_NAMESPACE", "test-namespace");

    let config = LeaderConfig::from_env();

    assert_eq!(config.holder_id, "test-pod-123");
    assert_eq!(config.lease_namespace, "test-namespace");

    // Clean up
    std::env::remove_var("POD_NAME");
    std::env::remove_var("POD_NAMESPACE");
}

/// Test default constants are reasonable
#[test]
fn test_lease_timing_constants() {
    // Lease TTL should be reasonable (not too short, not too long)
    assert!(DEFAULT_LEASE_TTL >= Duration::from_secs(10));
    assert!(DEFAULT_LEASE_TTL <= Duration::from_secs(60));

    // Renew interval should be roughly 1/3 of TTL
    assert!(DEFAULT_RENEW_INTERVAL < DEFAULT_LEASE_TTL);
    assert!(DEFAULT_RENEW_INTERVAL >= Duration::from_secs(3));
}
