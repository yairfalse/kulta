//! Leader election for multi-replica safety
//!
//! Uses Kubernetes Lease resources to ensure only one controller
//! instance is actively reconciling at a time.
//!
//! Implementation uses the coordination.k8s.io/v1 Lease API directly.

use chrono::Utc;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::Client;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Default lease TTL (how long leadership is valid)
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(15);

/// Default renew interval (should be ~1/3 of TTL)
pub const DEFAULT_RENEW_INTERVAL: Duration = Duration::from_secs(5);

/// Leader election configuration
#[derive(Clone)]
pub struct LeaderConfig {
    /// Unique identifier for this instance (usually pod name)
    pub holder_id: String,
    /// Name of the Lease resource
    pub lease_name: String,
    /// Namespace for the Lease resource
    pub lease_namespace: String,
    /// How long leadership is valid (in seconds)
    pub lease_duration_seconds: i32,
    /// How often to renew leadership
    pub renew_interval: Duration,
}

impl LeaderConfig {
    /// Create config from environment variables
    ///
    /// Uses:
    /// - `POD_NAME` for holder_id (falls back to hostname or UUID)
    /// - `POD_NAMESPACE` for lease_namespace (falls back to "kulta-system")
    pub fn from_env() -> Self {
        let holder_id = std::env::var("POD_NAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| format!("kulta-{}", uuid::Uuid::new_v4()));

        let lease_namespace =
            std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "kulta-system".to_string());

        Self {
            holder_id,
            lease_name: "kulta-controller-leader".to_string(),
            lease_namespace,
            lease_duration_seconds: DEFAULT_LEASE_TTL.as_secs() as i32,
            renew_interval: DEFAULT_RENEW_INTERVAL,
        }
    }
}

/// Shared state for leader status
#[derive(Clone)]
pub struct LeaderState {
    is_leader: Arc<AtomicBool>,
}

impl LeaderState {
    /// Create new leader state (initially not leader)
    pub fn new() -> Self {
        Self {
            is_leader: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Check if this instance is currently the leader
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::SeqCst)
    }

    /// Update leader status
    ///
    /// Used internally by leader election loop and by main() when
    /// running in single-instance mode (no leader election).
    pub fn set_leader(&self, is_leader: bool) {
        self.is_leader.store(is_leader, Ordering::SeqCst);
    }
}

impl Default for LeaderState {
    fn default() -> Self {
        Self::new()
    }
}

/// Try to acquire or renew leadership
///
/// Returns true if we are now the leader, false otherwise.
async fn try_acquire_or_renew(
    api: &Api<Lease>,
    config: &LeaderConfig,
) -> Result<bool, kube::Error> {
    let now = Utc::now();
    let now_micro = MicroTime(now);

    // Try to get existing lease
    match api.get(&config.lease_name).await {
        Ok(existing) => {
            let spec = existing.spec.as_ref();
            let current_holder = spec.and_then(|s| s.holder_identity.as_ref());
            let renew_time = spec.and_then(|s| s.renew_time.as_ref());
            let lease_duration = spec.and_then(|s| s.lease_duration_seconds);

            // Check if we already hold the lease
            if current_holder == Some(&config.holder_id) {
                // We hold it, renew
                debug!(holder_id = %config.holder_id, "Renewing lease");
                let patch = serde_json::json!({
                    "spec": {
                        "renewTime": now_micro,
                        "leaseDurationSeconds": config.lease_duration_seconds
                    }
                });
                api.patch(
                    &config.lease_name,
                    &PatchParams::default(),
                    &Patch::Merge(&patch),
                )
                .await?;
                return Ok(true);
            }

            // Check if lease is expired
            let is_expired = match (renew_time, lease_duration) {
                (Some(MicroTime(renew)), Some(duration)) => {
                    let expiry = *renew + chrono::Duration::seconds(duration as i64);
                    now > expiry
                }
                _ => true, // No renew time or duration = expired
            };

            if is_expired {
                // Lease expired, try to acquire
                debug!(holder_id = %config.holder_id, "Lease expired, attempting to acquire");
                let transitions = spec.and_then(|s| s.lease_transitions).unwrap_or(0);

                let patch = serde_json::json!({
                    "spec": {
                        "holderIdentity": config.holder_id,
                        "acquireTime": now_micro,
                        "renewTime": now_micro,
                        "leaseDurationSeconds": config.lease_duration_seconds,
                        "leaseTransitions": transitions + 1
                    }
                });

                api.patch(
                    &config.lease_name,
                    &PatchParams::default(),
                    &Patch::Merge(&patch),
                )
                .await?;
                return Ok(true);
            }

            // Lease held by someone else and not expired
            debug!(
                holder_id = %config.holder_id,
                current_holder = ?current_holder,
                "Lease held by another instance"
            );
            Ok(false)
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            // Lease doesn't exist, create it
            info!(holder_id = %config.holder_id, "Creating new lease");
            let lease = Lease {
                metadata: kube::api::ObjectMeta {
                    name: Some(config.lease_name.clone()),
                    namespace: Some(config.lease_namespace.clone()),
                    ..Default::default()
                },
                #[allow(clippy::needless_update)]
                spec: Some(k8s_openapi::api::coordination::v1::LeaseSpec {
                    holder_identity: Some(config.holder_id.clone()),
                    acquire_time: Some(now_micro.clone()),
                    renew_time: Some(now_micro),
                    lease_duration_seconds: Some(config.lease_duration_seconds),
                    lease_transitions: Some(0),
                    ..Default::default()
                }),
            };

            match api.create(&PostParams::default(), &lease).await {
                Ok(_) => Ok(true),
                // If another replica created the lease first, treat it as a normal race
                // and retry acquisition logic on the next interval.
                Err(kube::Error::Api(api_err)) if api_err.code == 409 => {
                    info!(
                        holder_id = %config.holder_id,
                        "Lease already created by another holder; will retry acquisition on next interval"
                    );
                    Ok(false)
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// Run leader election loop
///
/// Continuously tries to acquire/renew leadership.
/// Updates `state` with current leadership status.
/// Returns when shutdown signal is received.
pub async fn run_leader_election(
    client: Client,
    config: LeaderConfig,
    state: LeaderState,
    mut shutdown: crate::server::ShutdownSignal,
) {
    let api: Api<Lease> = Api::namespaced(client, &config.lease_namespace);

    info!(
        holder_id = %config.holder_id,
        lease_name = %config.lease_name,
        lease_namespace = %config.lease_namespace,
        "Starting leader election"
    );

    // Note: tokio::time::interval fires its first tick immediately.
    // This is intentional so we try to acquire/renew leadership right away
    // on startup; config.renew_interval applies to subsequent renewals.
    let mut renew_interval = tokio::time::interval(config.renew_interval);

    loop {
        tokio::select! {
            _ = renew_interval.tick() => {
                match try_acquire_or_renew(&api, &config).await {
                    Ok(is_leader) => {
                        let was_leader = state.is_leader();
                        state.set_leader(is_leader);

                        if is_leader && !was_leader {
                            info!(holder_id = %config.holder_id, "Acquired leadership");
                        } else if !is_leader && was_leader {
                            warn!(holder_id = %config.holder_id, "Lost leadership");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Leader election error");
                        // On error, assume we're not leader (safe fallback)
                        if state.is_leader() {
                            warn!(holder_id = %config.holder_id, "Lost leadership due to error");
                            state.set_leader(false);
                        }
                    }
                }
            }
            _ = shutdown.wait() => {
                info!("Leader election shutting down");
                // Note: We don't explicitly release the lease on shutdown.
                // It will expire naturally after lease_duration_seconds.
                // This is safer than trying to release, which could fail.
                break;
            }
        }
    }
}
