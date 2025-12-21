//! Tests for controller metrics

use super::metrics::{create_metrics, ControllerMetrics};

#[test]
fn test_metrics_creation() {
    let metrics = ControllerMetrics::new().expect("should create metrics");

    // Record some values so metrics appear in output
    // (Prometheus only outputs metrics with values)
    metrics.record_reconciliation_success("canary", 0.1);
    metrics.set_rollouts_active("Progressing", "canary", 1);
    metrics.set_traffic_weight("default", "test", 50);

    // Verify metrics can be encoded
    let output = metrics.encode().expect("should encode metrics");
    assert!(output.contains("kulta_reconciliations_total"));
    assert!(output.contains("kulta_reconciliation_duration_seconds"));
    assert!(output.contains("kulta_rollouts_active"));
    assert!(output.contains("kulta_traffic_weight"));
}

#[test]
fn test_record_reconciliation_success() {
    let metrics = ControllerMetrics::new().expect("should create metrics");

    metrics.record_reconciliation_success("canary", 0.5);
    metrics.record_reconciliation_success("canary", 1.2);
    metrics.record_reconciliation_success("blue_green", 0.3);

    let output = metrics.encode().expect("should encode metrics");

    // Check counter incremented
    assert!(output.contains("kulta_reconciliations_total{result=\"success\"} 3"));

    // Check histogram has observations
    assert!(output.contains("kulta_reconciliation_duration_seconds_count{strategy=\"canary\"} 2"));
    assert!(
        output.contains("kulta_reconciliation_duration_seconds_count{strategy=\"blue_green\"} 1")
    );
}

#[test]
fn test_record_reconciliation_error() {
    let metrics = ControllerMetrics::new().expect("should create metrics");

    metrics.record_reconciliation_error("canary", 2.0);

    let output = metrics.encode().expect("should encode metrics");

    assert!(output.contains("kulta_reconciliations_total{result=\"error\"} 1"));
    assert!(output.contains("kulta_reconciliation_duration_seconds_count{strategy=\"canary\"} 1"));
}

#[test]
fn test_record_reconciliation_skipped() {
    let metrics = ControllerMetrics::new().expect("should create metrics");

    metrics.record_reconciliation_skipped();
    metrics.record_reconciliation_skipped();

    let output = metrics.encode().expect("should encode metrics");

    assert!(output.contains("kulta_reconciliations_total{result=\"skipped\"} 2"));
}

#[test]
fn test_set_traffic_weight() {
    let metrics = ControllerMetrics::new().expect("should create metrics");

    metrics.set_traffic_weight("default", "my-app", 25);
    metrics.set_traffic_weight("production", "backend", 50);

    let output = metrics.encode().expect("should encode metrics");

    assert!(output.contains("kulta_traffic_weight{namespace=\"default\",rollout=\"my-app\"} 25"));
    assert!(
        output.contains("kulta_traffic_weight{namespace=\"production\",rollout=\"backend\"} 50")
    );
}

#[test]
fn test_set_rollouts_active() {
    let metrics = ControllerMetrics::new().expect("should create metrics");

    metrics.set_rollouts_active("Progressing", "canary", 3);
    metrics.set_rollouts_active("Paused", "canary", 1);
    metrics.set_rollouts_active("Completed", "blue_green", 5);

    let output = metrics.encode().expect("should encode metrics");

    assert!(output.contains("kulta_rollouts_active{phase=\"Progressing\",strategy=\"canary\"} 3"));
    assert!(output.contains("kulta_rollouts_active{phase=\"Paused\",strategy=\"canary\"} 1"));
    assert!(output.contains("kulta_rollouts_active{phase=\"Completed\",strategy=\"blue_green\"} 5"));
}

#[test]
fn test_create_shared_metrics() {
    let metrics = create_metrics().expect("should create shared metrics");

    // Verify Arc sharing works
    let metrics2 = metrics.clone();
    metrics.record_reconciliation_success("simple", 0.1);

    let output = metrics2.encode().expect("should encode from clone");
    assert!(output.contains("kulta_reconciliations_total{result=\"success\"} 1"));
}

#[test]
fn test_histogram_buckets() {
    let metrics = ControllerMetrics::new().expect("should create metrics");

    // Record values in different buckets
    metrics.record_reconciliation_success("canary", 0.005); // < 0.01
    metrics.record_reconciliation_success("canary", 0.03); // < 0.05
    metrics.record_reconciliation_success("canary", 0.8); // < 1.0
    metrics.record_reconciliation_success("canary", 3.0); // < 5.0

    let output = metrics.encode().expect("should encode metrics");

    // Verify histogram has proper bucket structure
    assert!(output
        .contains("kulta_reconciliation_duration_seconds_bucket{strategy=\"canary\",le=\"0.01\"}"));
    assert!(output
        .contains("kulta_reconciliation_duration_seconds_bucket{strategy=\"canary\",le=\"1\"}"));
    assert!(output
        .contains("kulta_reconciliation_duration_seconds_bucket{strategy=\"canary\",le=\"+Inf\"}"));
    assert!(output.contains("kulta_reconciliation_duration_seconds_sum{strategy=\"canary\"}"));
    assert!(output.contains("kulta_reconciliation_duration_seconds_count{strategy=\"canary\"} 4"));
}

#[test]
fn test_metrics_new_is_infallible_in_practice() {
    // ControllerMetrics::new() returns Result but should never fail
    // in normal operation (only fails if prometheus registry is broken)
    let metrics = ControllerMetrics::new().expect("should create metrics");

    // Record a value so metric appears in output
    metrics.record_reconciliation_success("simple", 0.1);

    // Verify basic functionality works
    let output = metrics.encode().expect("should encode metrics");
    assert!(output.contains("kulta_reconciliations_total"));
}
