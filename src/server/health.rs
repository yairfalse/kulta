//! Health check and metrics endpoints for Kubernetes probes
//!
//! - `/healthz` - Liveness: Is the process alive?
//! - `/readyz` - Readiness: Is the controller ready to handle requests?
//! - `/metrics` - Prometheus metrics in text format

use crate::server::metrics::SharedMetrics;
use axum::{
    extract::State,
    http::{header::CONTENT_TYPE, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
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

    /// Mark the controller as not ready (e.g., during shutdown)
    ///
    /// This causes the readiness probe to return 503, signaling to
    /// Kubernetes that the pod should no longer receive traffic.
    pub fn set_not_ready(&self) {
        self.ready.store(false, std::sync::atomic::Ordering::SeqCst);
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

/// Combined server state for health and metrics endpoints
#[derive(Clone)]
pub struct ServerState {
    readiness: ReadinessState,
    metrics: SharedMetrics,
}

impl ServerState {
    /// Create new server state
    pub fn new(readiness: ReadinessState, metrics: SharedMetrics) -> Self {
        Self { readiness, metrics }
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
async fn readyz(State(state): State<ServerState>) -> StatusCode {
    if state.readiness.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Prometheus metrics handler
///
/// Returns metrics in Prometheus text format for scraping.
async fn metrics(State(state): State<ServerState>) -> impl IntoResponse {
    match state.metrics.encode() {
        Ok(body) => (
            StatusCode::OK,
            [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to encode metrics: {}", e),
        )
            .into_response(),
    }
}

/// Run the health server on the specified port
///
/// This function starts an HTTP server that responds to:
/// - GET /healthz - Always returns 200 OK (liveness)
/// - GET /readyz - Returns 200 OK if ready, 503 Service Unavailable if not
/// - GET /metrics - Prometheus metrics in text format
///
/// # Arguments
/// * `port` - The port to listen on
/// * `readiness` - Shared state for readiness tracking
/// * `metrics` - Shared metrics registry for Prometheus
///
/// # Returns
/// This function runs forever until the server is shut down
pub async fn run_health_server(
    port: u16,
    readiness: ReadinessState,
    metrics: SharedMetrics,
) -> Result<(), std::io::Error> {
    let state = ServerState::new(readiness, metrics);

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(self::metrics))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    // Log after successful bind - server is actually listening
    info!(port = %port, "Health and metrics server listening");

    axum::serve(listener, app)
        .await
        .map_err(std::io::Error::other)
}
