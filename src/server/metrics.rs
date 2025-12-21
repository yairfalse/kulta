//! Prometheus metrics for KULTA controller
//!
//! Exposes controller health and rollout activity metrics:
//! - Reconciliation counts and durations
//! - Rollout phase transitions
//! - Traffic weight distribution

use prometheus::{
    self, Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry,
    TextEncoder,
};
use std::sync::Arc;

/// Controller metrics registry
///
/// Thread-safe container for all Prometheus metrics.
/// Clone is cheap (Arc internally).
#[derive(Clone)]
pub struct ControllerMetrics {
    registry: Registry,
    /// Total reconciliations by result (success, error, skipped)
    pub reconciliations_total: IntCounterVec,
    /// Reconciliation duration in seconds
    pub reconciliation_duration_seconds: HistogramVec,
    /// Active rollouts by phase (Progressing, Paused, etc.)
    pub rollouts_active: IntGaugeVec,
    /// Traffic weight per rollout (0-100)
    pub traffic_weight: IntGaugeVec,
}

impl ControllerMetrics {
    /// Create a new metrics registry with all KULTA metrics
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        // Reconciliation counter
        let reconciliations_total = IntCounterVec::new(
            Opts::new(
                "kulta_reconciliations_total",
                "Total number of reconciliations",
            ),
            &["result"], // success, error, skipped
        )?;
        registry.register(Box::new(reconciliations_total.clone()))?;

        // Reconciliation duration histogram
        let reconciliation_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "kulta_reconciliation_duration_seconds",
                "Duration of reconciliation in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
            &["strategy"], // canary, blue_green, simple
        )?;
        registry.register(Box::new(reconciliation_duration_seconds.clone()))?;

        // Active rollouts gauge
        let rollouts_active = IntGaugeVec::new(
            Opts::new(
                "kulta_rollouts_active",
                "Number of active rollouts by phase",
            ),
            &["phase", "strategy"],
        )?;
        registry.register(Box::new(rollouts_active.clone()))?;

        // Traffic weight gauge
        let traffic_weight = IntGaugeVec::new(
            Opts::new(
                "kulta_traffic_weight",
                "Current canary traffic weight percentage",
            ),
            &["namespace", "rollout"],
        )?;
        registry.register(Box::new(traffic_weight.clone()))?;

        Ok(Self {
            registry,
            reconciliations_total,
            reconciliation_duration_seconds,
            rollouts_active,
            traffic_weight,
        })
    }

    /// Record a successful reconciliation
    pub fn record_reconciliation_success(&self, strategy: &str, duration_secs: f64) {
        self.reconciliations_total
            .with_label_values(&["success"])
            .inc();
        self.reconciliation_duration_seconds
            .with_label_values(&[strategy])
            .observe(duration_secs);
    }

    /// Record a failed reconciliation
    pub fn record_reconciliation_error(&self, strategy: &str, duration_secs: f64) {
        self.reconciliations_total
            .with_label_values(&["error"])
            .inc();
        self.reconciliation_duration_seconds
            .with_label_values(&[strategy])
            .observe(duration_secs);
    }

    /// Record a skipped reconciliation (not leader)
    pub fn record_reconciliation_skipped(&self) {
        self.reconciliations_total
            .with_label_values(&["skipped"])
            .inc();
    }

    /// Update traffic weight for a rollout
    pub fn set_traffic_weight(&self, namespace: &str, rollout: &str, weight: i64) {
        self.traffic_weight
            .with_label_values(&[namespace, rollout])
            .set(weight);
    }

    /// Update active rollout count for a phase
    pub fn set_rollouts_active(&self, phase: &str, strategy: &str, count: i64) {
        self.rollouts_active
            .with_label_values(&[phase, strategy])
            .set(count);
    }

    /// Encode all metrics to Prometheus text format
    pub fn encode(&self) -> Result<String, prometheus::Error> {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer)?;
        String::from_utf8(buffer).map_err(|e| {
            prometheus::Error::Msg(format!("Failed to encode metrics as UTF-8: {}", e))
        })
    }
}

/// Shared metrics handle for use across the controller
pub type SharedMetrics = Arc<ControllerMetrics>;

/// Create a new shared metrics instance
pub fn create_metrics() -> Result<SharedMetrics, prometheus::Error> {
    Ok(Arc::new(ControllerMetrics::new()?))
}
