//! Prometheus metrics integration for automated rollback
//!
//! This module handles querying Prometheus and evaluating metrics against thresholds.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrometheusError {
    #[error("Prometheus HTTP error: {0}")]
    HttpError(String),

    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    #[error("Failed to parse response: {0}")]
    ParseError(String),

    #[error("No data returned from Prometheus")]
    NoData,
}

/// Build PromQL query for error rate metric
///
/// Calculates: (5xx errors / total requests) * 100
#[allow(dead_code)] // Used in tests, will be used in production metrics analysis
fn build_error_rate_query(rollout_name: &str, revision: &str) -> String {
    format!(
        r#"sum(rate(http_requests_total{{status=~"5..",rollout="{}",revision="{}"}}[2m])) / sum(rate(http_requests_total{{rollout="{}",revision="{}"}}[2m])) * 100"#,
        rollout_name, revision, rollout_name, revision
    )
}

/// Build PromQL query for latency p95 metric
///
/// Uses histogram_quantile to calculate 95th percentile
#[allow(dead_code)] // Used in tests, will be used in production metrics analysis
fn build_latency_p95_query(rollout_name: &str, revision: &str) -> String {
    format!(
        r#"histogram_quantile(0.95, rate(http_request_duration_seconds_bucket{{rollout="{}",revision="{}"}}[2m]))"#,
        rollout_name, revision
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // TDD Cycle 2: RED - Test building PromQL query from template
    #[test]
    fn test_build_error_rate_query() {
        let rollout_name = "my-app";
        let revision = "canary";

        let query = build_error_rate_query(rollout_name, revision);

        // Should build query that calculates error rate for canary pods
        assert!(query.contains("http_requests_total"));
        assert!(query.contains(r#"status=~"5..""#));
        assert!(query.contains(rollout_name));
        assert!(query.contains(revision));
    }

    #[test]
    fn test_build_latency_p95_query() {
        let rollout_name = "my-app";
        let revision = "stable";

        let query = build_latency_p95_query(rollout_name, revision);

        // Should use histogram_quantile for p95
        assert!(query.contains("histogram_quantile"));
        assert!(query.contains("0.95"));
        assert!(query.contains(rollout_name));
        assert!(query.contains(revision));
    }
}
