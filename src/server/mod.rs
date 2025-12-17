//! HTTP server for health endpoints
//!
//! Provides Kubernetes health probes:
//! - `/healthz` - Liveness probe (process is running)
//! - `/readyz` - Readiness probe (controller is ready to serve)

mod health;

pub use health::{run_health_server, ReadinessState};

#[cfg(test)]
#[path = "health_test.rs"]
mod tests;
