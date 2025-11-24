//! Metrics collection and analysis

use std::collections::HashMap;
use std::error::Error;

/// Metrics collector for tracking deployment health
pub struct MetricsCollector {
    baseline: Option<MetricsSnapshot>,
    current: Option<MetricsSnapshot>,
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub requests_total: HashMap<String, u64>, // label -> count
    pub latency_p50: f64,
    pub latency_p95: f64,
    pub latency_p99: f64,
    pub error_5xx_count: u64,
}

#[derive(Debug, Clone)]
pub struct MetricsDelta {
    pub requests_delta: HashMap<String, u64>,
    pub error_rate: f64,
    pub latency_change_p95: f64,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            baseline: None,
            current: None,
        }
    }

    /// Scrape metrics from Prometheus endpoint
    pub async fn scrape(&mut self, _url: &str) -> Result<(), Box<dyn Error>> {
        // TODO: Implement actual Prometheus scraping
        // For now, return mock data
        let snapshot = MetricsSnapshot {
            timestamp: chrono::Utc::now(),
            requests_total: HashMap::new(),
            latency_p50: 0.0,
            latency_p95: 0.0,
            latency_p99: 0.0,
            error_5xx_count: 0,
        };

        self.current = Some(snapshot);
        Ok(())
    }

    /// Set current snapshot as baseline
    pub fn set_baseline(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(current) = &self.current {
            self.baseline = Some(current.clone());
            Ok(())
        } else {
            Err("no current snapshot to set as baseline".into())
        }
    }

    /// Calculate delta between baseline and current
    pub fn get_delta(&self) -> Option<MetricsDelta> {
        let baseline = self.baseline.as_ref()?;
        let current = self.current.as_ref()?;

        let mut requests_delta = HashMap::new();
        for (label, current_count) in &current.requests_total {
            let baseline_count = baseline.requests_total.get(label).unwrap_or(&0);
            requests_delta.insert(label.clone(), current_count.saturating_sub(*baseline_count));
        }

        let total_requests: u64 = requests_delta.values().sum();
        let error_rate = if total_requests > 0 {
            current.error_5xx_count as f64 / total_requests as f64
        } else {
            0.0
        };

        let latency_change_p95 = current.latency_p95 - baseline.latency_p95;

        Some(MetricsDelta {
            requests_delta,
            error_rate,
            latency_change_p95,
        })
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}
