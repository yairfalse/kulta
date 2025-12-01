//! Prometheus metrics integration for automated rollback
//!
//! This module handles querying Prometheus and evaluating metrics against thresholds.

use serde::Deserialize;
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

/// Prometheus instant query response format
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Used in parse_prometheus_instant_query, will be used in production
struct PrometheusResponse {
    status: String,
    data: PrometheusData,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Used in parse_prometheus_instant_query, will be used in production
struct PrometheusData {
    result: Vec<PrometheusResult>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Used in parse_prometheus_instant_query, will be used in production
struct PrometheusResult {
    value: (i64, String), // [timestamp, value_as_string]
}

/// Parse Prometheus instant query response and extract metric value
///
/// Parses the JSON response from Prometheus /api/v1/query endpoint
/// and returns the first metric value as f64.
#[allow(dead_code)] // Used in tests, will be used in production metrics analysis
fn parse_prometheus_instant_query(json_response: &str) -> Result<f64, PrometheusError> {
    let response: PrometheusResponse = serde_json::from_str(json_response)
        .map_err(|e| PrometheusError::ParseError(format!("Invalid JSON: {}", e)))?;

    if response.status != "success" {
        return Err(PrometheusError::HttpError(format!(
            "Prometheus query failed with status: {}",
            response.status
        )));
    }

    let result = response
        .data
        .result
        .first()
        .ok_or(PrometheusError::NoData)?;

    let value = result
        .value
        .1
        .parse::<f64>()
        .map_err(|e| PrometheusError::ParseError(format!("Invalid value: {}", e)))?;

    Ok(value)
}

/// Prometheus client for executing queries
#[derive(Clone)]
pub struct PrometheusClient {
    #[cfg(not(test))]
    address: String,
    #[cfg(test)]
    mock_response: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl PrometheusClient {
    /// Create new Prometheus client
    #[cfg(not(test))]
    pub fn new(address: String) -> Self {
        Self { address }
    }

    /// Create mock client for testing
    #[cfg(test)]
    pub fn new_mock() -> Self {
        Self {
            mock_response: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Set mock response for testing
    #[cfg(test)]
    pub fn set_mock_response(&self, response: String) {
        if let Ok(mut mock) = self.mock_response.lock() {
            *mock = Some(response);
        }
    }

    /// Execute instant query against Prometheus
    ///
    /// Queries the /api/v1/query endpoint and returns the first metric value.
    #[cfg(not(test))]
    pub async fn query_instant(&self, query: &str) -> Result<f64, PrometheusError> {
        let url = format!("{}/api/v1/query", self.address);
        let client = reqwest::Client::new();

        let response = client
            .get(&url)
            .query(&[("query", query)])
            .send()
            .await
            .map_err(|e| PrometheusError::HttpError(format!("HTTP request failed: {}", e)))?;

        let body = response
            .text()
            .await
            .map_err(|e| PrometheusError::HttpError(format!("Failed to read response: {}", e)))?;

        parse_prometheus_instant_query(&body)
    }

    /// Execute instant query (mock version for tests)
    #[cfg(test)]
    pub async fn query_instant(&self, _query: &str) -> Result<f64, PrometheusError> {
        let mock = self
            .mock_response
            .lock()
            .map_err(|_| PrometheusError::HttpError("Lock poisoned".to_string()))?;
        let response = mock
            .as_ref()
            .ok_or_else(|| PrometheusError::HttpError("No mock response set".to_string()))?;
        parse_prometheus_instant_query(response)
    }

    /// Evaluate a metric by name against threshold
    ///
    /// Builds the appropriate PromQL query from the metric name template,
    /// executes it, and compares the result to the threshold.
    ///
    /// # Arguments
    /// * `metric_name` - Template name ("error-rate", "latency-p95", "latency-p99")
    /// * `rollout_name` - Name of the rollout
    /// * `revision` - Revision label ("canary" or "stable")
    /// * `threshold` - Threshold value (metric must be below this)
    ///
    /// # Returns
    /// * `Ok(true)` - Metric is healthy (below threshold)
    /// * `Ok(false)` - Metric is unhealthy (above or equal to threshold)
    /// * `Err(_)` - Query execution failed
    pub async fn evaluate_metric(
        &self,
        metric_name: &str,
        rollout_name: &str,
        revision: &str,
        threshold: f64,
    ) -> Result<bool, PrometheusError> {
        // Build query from template
        let query = match metric_name {
            "error-rate" => build_error_rate_query(rollout_name, revision),
            "latency-p95" => build_latency_p95_query(rollout_name, revision),
            _ => {
                return Err(PrometheusError::InvalidQuery(format!(
                    "Unknown metric template: {}",
                    metric_name
                )))
            }
        };

        // Execute query
        let value = self.query_instant(&query).await?;

        // Compare to threshold (healthy if < threshold)
        Ok(value < threshold)
    }

    /// Evaluate all metrics from analysis config
    ///
    /// Iterates through all metrics and evaluates each one.
    /// Returns Ok(true) only if ALL metrics are healthy.
    ///
    /// # Arguments
    /// * `metrics` - List of metrics from Rollout's analysis config
    /// * `rollout_name` - Name of the rollout
    /// * `revision` - Revision label ("canary" or "stable")
    ///
    /// # Returns
    /// * `Ok(true)` - All metrics healthy (below thresholds)
    /// * `Ok(false)` - One or more metrics unhealthy
    /// * `Err(_)` - Query execution failed
    pub async fn evaluate_all_metrics(
        &self,
        metrics: &[crate::crd::rollout::MetricConfig],
        rollout_name: &str,
        revision: &str,
    ) -> Result<bool, PrometheusError> {
        // Empty metrics list = no constraints = healthy
        if metrics.is_empty() {
            return Ok(true);
        }

        // Evaluate each metric
        for metric in metrics {
            let is_healthy = self
                .evaluate_metric(&metric.name, rollout_name, revision, metric.threshold)
                .await?;

            // If ANY metric is unhealthy, return false immediately
            if !is_healthy {
                return Ok(false);
            }
        }

        // All metrics passed
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TDD Cycle 2 Part 1: RED - Test building PromQL query from template
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

    // TDD Cycle 2 Part 2: RED - Test parsing Prometheus instant query response
    #[test]
    fn test_parse_prometheus_response_with_data() {
        // Valid Prometheus instant query response
        let json_response = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {},
                        "value": [1234567890, "5.2"]
                    }
                ]
            }
        }"#;

        match parse_prometheus_instant_query(json_response) {
            Ok(value) => assert_eq!(value, 5.2),
            Err(e) => panic!("Should parse valid response, got error: {}", e),
        }
    }

    #[test]
    fn test_parse_prometheus_response_no_data() {
        // Empty result (no data points)
        let json_response = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": []
            }
        }"#;

        let result = parse_prometheus_instant_query(json_response);
        assert!(matches!(result, Err(PrometheusError::NoData)));
    }

    #[test]
    fn test_parse_prometheus_response_invalid_json() {
        let json_response = "not valid json";

        let result = parse_prometheus_instant_query(json_response);
        assert!(matches!(result, Err(PrometheusError::ParseError(_))));
    }

    // TDD Cycle 2 Part 3: RED - Test executing Prometheus query
    #[tokio::test]
    async fn test_prometheus_client_query_instant() {
        let client = PrometheusClient::new_mock();

        // Mock successful response
        let mock_response = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {},
                        "value": [1234567890, "12.5"]
                    }
                ]
            }
        }"#;
        client.set_mock_response(mock_response.to_string());

        // Execute query
        let query = "rate(http_requests_total[2m])";
        let result = client.query_instant(query).await;

        match result {
            Ok(value) => assert_eq!(value, 12.5),
            Err(e) => panic!("Should successfully query, got error: {}", e),
        }
    }

    #[tokio::test]
    async fn test_prometheus_client_query_no_data() {
        let client = PrometheusClient::new_mock();

        // Mock empty response
        let mock_response = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": []
            }
        }"#;
        client.set_mock_response(mock_response.to_string());

        let query = "rate(http_requests_total[2m])";
        let result = client.query_instant(query).await;

        assert!(matches!(result, Err(PrometheusError::NoData)));
    }

    // TDD Cycle 3 Part 2: RED - Test evaluating error-rate metric
    #[tokio::test]
    async fn test_evaluate_error_rate_healthy() {
        let client = PrometheusClient::new_mock();

        // Mock response: error rate = 2.5% (healthy, below threshold)
        let mock_response = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {},
                        "value": [1234567890, "2.5"]
                    }
                ]
            }
        }"#;
        client.set_mock_response(mock_response.to_string());

        // Evaluate error-rate metric with threshold = 5.0
        let rollout_name = "my-app";
        let revision = "canary";
        let threshold = 5.0;

        let result = client
            .evaluate_metric("error-rate", rollout_name, revision, threshold)
            .await;

        match result {
            Ok(is_healthy) => assert!(is_healthy, "Error rate 2.5% should be healthy (< 5.0%)"),
            Err(e) => panic!("Should evaluate successfully, got error: {}", e),
        }
    }

    #[tokio::test]
    async fn test_evaluate_error_rate_unhealthy() {
        let client = PrometheusClient::new_mock();

        // Mock response: error rate = 8.0% (unhealthy, exceeds threshold)
        let mock_response = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {},
                        "value": [1234567890, "8.0"]
                    }
                ]
            }
        }"#;
        client.set_mock_response(mock_response.to_string());

        // Evaluate error-rate metric with threshold = 5.0
        let rollout_name = "my-app";
        let revision = "canary";
        let threshold = 5.0;

        let result = client
            .evaluate_metric("error-rate", rollout_name, revision, threshold)
            .await;

        match result {
            Ok(is_healthy) => assert!(!is_healthy, "Error rate 8.0% should be unhealthy (> 5.0%)"),
            Err(e) => panic!("Should evaluate successfully, got error: {}", e),
        }
    }

    // TDD Cycle 3 Part 3: RED - Test evaluating all metrics from config
    #[tokio::test]
    async fn test_evaluate_all_metrics_all_healthy() {
        use crate::crd::rollout::MetricConfig;

        let client = PrometheusClient::new_mock();

        // Mock response: error rate = 2.5% (healthy)
        let mock_response = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {},
                        "value": [1234567890, "2.5"]
                    }
                ]
            }
        }"#;
        client.set_mock_response(mock_response.to_string());

        // Define metrics to evaluate
        let metrics = vec![
            MetricConfig {
                name: "error-rate".to_string(),
                threshold: 5.0,
                interval: None,
                failure_threshold: None,
                min_sample_size: None,
            },
            MetricConfig {
                name: "latency-p95".to_string(),
                threshold: 100.0,
                interval: None,
                failure_threshold: None,
                min_sample_size: None,
            },
        ];

        let rollout_name = "my-app";
        let revision = "canary";

        let result = client
            .evaluate_all_metrics(&metrics, rollout_name, revision)
            .await;

        match result {
            Ok(is_healthy) => assert!(is_healthy, "All metrics should be healthy"),
            Err(e) => panic!("Should evaluate successfully, got error: {}", e),
        }
    }

    #[tokio::test]
    async fn test_evaluate_all_metrics_one_unhealthy() {
        use crate::crd::rollout::MetricConfig;

        let client = PrometheusClient::new_mock();

        // Mock response: error rate = 8.0% (unhealthy)
        let mock_response = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {},
                        "value": [1234567890, "8.0"]
                    }
                ]
            }
        }"#;
        client.set_mock_response(mock_response.to_string());

        // Define metrics (error-rate will be unhealthy)
        let metrics = vec![MetricConfig {
            name: "error-rate".to_string(),
            threshold: 5.0,
            interval: None,
            failure_threshold: None,
            min_sample_size: None,
        }];

        let rollout_name = "my-app";
        let revision = "canary";

        let result = client
            .evaluate_all_metrics(&metrics, rollout_name, revision)
            .await;

        match result {
            Ok(is_healthy) => assert!(
                !is_healthy,
                "Should be unhealthy when error-rate exceeds threshold"
            ),
            Err(e) => panic!("Should evaluate successfully, got error: {}", e),
        }
    }

    #[tokio::test]
    async fn test_evaluate_all_metrics_empty_list() {
        let client = PrometheusClient::new_mock();

        // Empty metrics list should be considered healthy (nothing to fail)
        let metrics = vec![];
        let rollout_name = "my-app";
        let revision = "canary";

        let result = client
            .evaluate_all_metrics(&metrics, rollout_name, revision)
            .await;

        match result {
            Ok(is_healthy) => assert!(is_healthy, "Empty metrics list should be healthy"),
            Err(e) => panic!("Should evaluate successfully, got error: {}", e),
        }
    }
}
