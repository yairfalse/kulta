use futures::StreamExt;
use kube::runtime::controller::Action;
use kube::runtime::{watcher, Controller};
use kube::{Api, Client};
use kulta::controller::cdevents::CDEventsSink;
use kulta::controller::prometheus::PrometheusClient;
use kulta::controller::{reconcile, Context, ReconcileError};
use kulta::crd::rollout::Rollout;
use kulta::server::{
    run_health_server, run_leader_election, shutdown_channel, wait_for_signal, LeaderConfig,
    LeaderState, ReadinessState,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// Default port for health endpoints
const HEALTH_PORT: u16 = 8080;

/// Check if leader election is enabled via env var
fn is_leader_election_enabled() -> bool {
    std::env::var("KULTA_LEADER_ELECTION")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

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

    // Create shutdown channel for coordinated shutdown
    let (shutdown_controller, shutdown_signal) = shutdown_channel();

    // Create readiness state (initially not ready)
    let readiness = ReadinessState::new();

    // Create leader state
    let leader_state = LeaderState::new();

    // Start health server in background
    let health_readiness = readiness.clone();
    let health_handle = tokio::spawn(async move {
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
            // Abort health server to avoid leaving it running orphaned
            health_handle.abort();
            return Err(e.into());
        }
    };

    info!("Connected to Kubernetes cluster");

    // Start leader election if enabled
    let leader_election_enabled = is_leader_election_enabled();
    let leader_handle = if leader_election_enabled {
        let leader_client = client.clone();
        let leader_config = LeaderConfig::from_env();
        let leader_state_clone = leader_state.clone();
        let leader_shutdown = shutdown_signal.clone();

        info!(
            holder_id = %leader_config.holder_id,
            "Leader election enabled"
        );

        Some(tokio::spawn(async move {
            run_leader_election(
                leader_client,
                leader_config,
                leader_state_clone,
                leader_shutdown,
            )
            .await;
        }))
    } else {
        info!("Leader election disabled - running as single instance");
        // If no leader election, we're always the leader
        leader_state.set_leader(true);
        None
    };

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
    let ctx = if leader_election_enabled {
        Arc::new(Context::new_with_leader(
            client.clone(),
            cdevents_sink,
            prometheus_client,
            leader_state.clone(),
        ))
    } else {
        Arc::new(Context::new(
            client.clone(),
            cdevents_sink,
            prometheus_client,
        ))
    };

    // Mark as ready - controller is initialized and about to start
    //
    // Note: Readiness indicates "controller is healthy and initialized", NOT "is the active leader".
    // All replicas report ready even if leader election is enabled. This is intentional because:
    // 1. Non-leaders may become leaders at any time if the current leader fails
    // 2. The controller gracefully skips reconciliation when not leader (no errors)
    // 3. Kubernetes services/traffic should route to all healthy replicas for HA
    readiness.set_ready();
    info!("Controller ready, starting reconciliation loop");

    // Create the controller stream
    // Note: error_policy already logs errors with warn!, so we only log success here
    let controller = Controller::new(rollouts, watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            if let Ok(o) = res {
                info!("Reconciled: {:?}", o);
            }
            // Errors are logged in error_policy, no duplicate logging
        });

    // Run controller until shutdown signal received
    tokio::select! {
        _ = controller => {
            info!("Controller stream ended");
        }
        signal = wait_for_signal() => {
            info!(signal = signal, "Initiating graceful shutdown");
            // Mark not ready so K8s stops sending traffic during shutdown
            readiness.set_not_ready();
        }
    }

    // Trigger shutdown for all components
    shutdown_controller.shutdown();

    // Graceful shutdown sequence
    info!("Stopping components...");

    if let Some(handle) = leader_handle {
        handle.abort();
    }
    health_handle.abort();

    info!("KULTA controller shut down gracefully");
    Ok(())
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
