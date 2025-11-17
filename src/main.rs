use futures::StreamExt;
use kube::runtime::controller::Action;
use kube::runtime::{watcher, Controller};
use kube::{Api, Client};
use kulta::controller::{reconcile, Context, ReconcileError};
use kulta::crd::rollout::Rollout;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

/// Error policy for the controller
///
/// Determines how to handle reconciliation errors:
/// - Requeue after delay (exponential backoff)
pub fn error_policy(_rollout: Arc<Rollout>, error: &ReconcileError, _ctx: Arc<Context>) -> Action {
    error!("Reconcile error: {:?}", error);
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

    // Create Kubernetes client
    let client = Client::try_default()
        .await
        .expect("Failed to create Kubernetes client");

    info!("Connected to Kubernetes cluster");

    // Create API for Rollout resources
    let rollouts = Api::<Rollout>::all(client.clone());

    // Create controller context
    let ctx = Arc::new(Context::new(client.clone()));

    info!("Starting Rollout controller");

    // Run the controller
    Controller::new(rollouts, watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("Reconciled: {:?}", o),
                Err(e) => error!("Reconcile error: {:?}", e),
            }
        })
        .await;

    Ok(())
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
