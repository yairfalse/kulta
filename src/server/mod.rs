//! HTTP server for health and metrics endpoints
//!
//! Provides Kubernetes health probes:
//! - `/healthz` - Liveness probe (process is running)
//! - `/readyz` - Readiness probe (controller is ready to serve)
//! - `/metrics` - Prometheus metrics endpoint
//!
//! Also provides:
//! - Graceful shutdown handling for SIGTERM/SIGINT
//! - Leader election for multi-replica safety

mod health;
pub mod leader;
pub mod metrics;
pub mod shutdown;

pub use health::{run_health_server, ReadinessState};
pub use leader::{run_leader_election, LeaderConfig, LeaderState};
pub use metrics::{create_metrics, ControllerMetrics, SharedMetrics};
pub use shutdown::{shutdown_channel, wait_for_signal, ShutdownController, ShutdownSignal};

#[cfg(test)]
#[path = "health_test.rs"]
mod health_tests;

#[cfg(test)]
#[path = "shutdown_test.rs"]
mod shutdown_tests;

#[cfg(test)]
#[path = "leader_test.rs"]
mod leader_tests;

#[cfg(test)]
#[path = "metrics_test.rs"]
mod metrics_tests;
