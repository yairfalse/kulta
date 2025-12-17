use futures::StreamExt;
use kube::runtime::controller::Action;
use kube::runtime::{watcher, Controller};
use kube::{Api, Client};
use kulta::controller::cdevents::CDEventsSink;
use kulta::controller::prometheus::PrometheusClient;
use kulta::controller::{reconcile, Context, ReconcileError};
use kulta::crd::rollout::Rollout;
use kulta::server::{run_health_server, ReadinessState};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// Default port for health endpoints
const HEALTH_PORT: u16 = 8080;

/// Error policy for the controller
///
/// Determines how to handle reconciliation errors:
/// - Requeue after delay (exponential backoff)
///
/// Uses `warn!` since reconciliation errors are expected and trigger retries.
pub fn error_policy(_rollout: Arc<Rollout>, error: &ReconcileError, _ctx: Arc<Context>) -> Action {
    warn!("Reconcile error (will retry): {:?}", error);
    Action::requeue(Duration::from_secs(10))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Starting KULTA progressive delivery controller");

    // Create readiness state (initially not ready)
    let readiness = ReadinessState::new();

    // Start health server in background
    let health_readiness = readiness.clone();
    tokio::spawn(async move {
        if let Err(e) = run_health_server(HEALTH_PORT, health_readiness).await {
            warn!(error = %e, "Health server failed");
        }
    });
    info!(port = HEALTH_PORT, "Health server task spawned");

    // Create Kubernetes client
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "Failed to create Kubernetes client");
            return Err(e.into());
        }
    };

    info!("Connected to Kubernetes cluster");

    // Create API for Rollout resources
    let rollouts = Api::<Rollout>::all(client.clone());

    // Create CDEvents sink (configured from env vars)
    let cdevents_sink = CDEventsSink::new();
    info!(
        enabled = std::env::var("KULTA_CDEVENTS_ENABLED").unwrap_or_else(|_| "false".to_string()),
        "CDEvents sink configured"
    );

    // Create Prometheus client (configured from env var)
    let prometheus_address =
        std::env::var("KULTA_PROMETHEUS_ADDRESS").unwrap_or_else(|_| "".to_string());
    let prometheus_client = if prometheus_address.is_empty() {
        info!("Prometheus address not configured - metrics analysis disabled");
        PrometheusClient::new("http://localhost:9090".to_string()) // Dummy address, metrics will be skipped
    } else {
        info!(address = %prometheus_address, "Prometheus client configured");
        PrometheusClient::new(prometheus_address)
    };

    // Create controller context
    let ctx = Arc::new(Context::new(
        client.clone(),
        cdevents_sink,
        prometheus_client,
    ));

    // Mark as ready - controller is initialized and about to start
    readiness.set_ready();
    info!("Controller ready, starting reconciliation loop");

    // Run the controller
    // Note: error_policy already logs errors with warn!, so we only log success here
    Controller::new(rollouts, watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            if let Ok(o) = res {
                info!("Reconciled: {:?}", o);
            }
            // Errors are logged in error_policy, no duplicate logging
        })
        .await;

    Ok(())
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
