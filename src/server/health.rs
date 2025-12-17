//! Health check endpoints for Kubernetes probes
//!
//! - `/healthz` - Liveness: Is the process alive?
//! - `/readyz` - Readiness: Is the controller ready to handle requests?

use axum::{extract::State, http::StatusCode, routing::get, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

/// Shared state for readiness tracking
///
/// The controller sets this to ready once it's fully initialized
/// and connected to the Kubernetes API.
#[derive(Debug, Clone)]
pub struct ReadinessState {
    ready: Arc<std::sync::atomic::AtomicBool>,
}

impl ReadinessState {
    /// Create a new readiness state (initially not ready)
    pub fn new() -> Self {
        Self {
            ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Mark the controller as ready
    pub fn set_ready(&self) {
        self.ready.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Check if the controller is ready
    pub fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for ReadinessState {
    fn default() -> Self {
        Self::new()
    }
}

/// Liveness probe handler
///
/// Always returns 200 OK - if this responds, the process is alive.
async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe handler
///
/// Returns 200 OK if ready, 503 Service Unavailable if not.
async fn readyz(State(readiness): State<ReadinessState>) -> StatusCode {
    if readiness.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Run the health server on the specified port
///
/// This function starts an HTTP server that responds to:
/// - GET /healthz - Always returns 200 OK (liveness)
/// - GET /readyz - Returns 200 OK if ready, 503 Service Unavailable if not
///
/// # Arguments
/// * `port` - The port to listen on
/// * `readiness` - Shared state for readiness tracking
///
/// # Returns
/// This function runs forever until the server is shut down
pub async fn run_health_server(port: u16, readiness: ReadinessState) -> Result<(), std::io::Error> {
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(readiness);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    // Log after successful bind - server is actually listening
    info!(port = %port, "Health server listening");

    axum::serve(listener, app)
        .await
        .map_err(std::io::Error::other)
}
