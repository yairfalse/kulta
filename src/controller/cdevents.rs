//! CDEvents emission for rollout observability.
//! See `docs/design/cdevents-observability.md` for specification.

use crate::crd::rollout::{Rollout, RolloutStatus};
use cloudevents::Event;
use serde_json::json;
use thiserror::Error;

#[cfg(test)]
use std::sync::{Arc, Mutex};

#[derive(Debug, Error)]
pub enum CDEventsError {
    #[error("cdevents error: {0}")]
    Generic(String),
}

/// CDEvents sink for emitting events
pub struct CDEventsSink {
    #[cfg(not(test))]
    enabled: bool,
    #[cfg(not(test))]
    sink_url: Option<String>,
    #[cfg(test)]
    mock_events: Arc<Mutex<Vec<Event>>>,
}

#[cfg(not(test))]
impl Default for CDEventsSink {
    fn default() -> Self {
        Self::new()
    }
}

impl CDEventsSink {
    /// Create a new CDEvents sink (production mode)
    ///
    /// Configuration from environment variables:
    /// - KULTA_CDEVENTS_ENABLED: "true" to enable CDEvents emission (default: false)
    /// - KULTA_CDEVENTS_SINK_URL: HTTP endpoint URL for CloudEvents (optional)
    ///
    /// # Returns
    /// A CDEventsSink configured from environment variables
    #[cfg(not(test))]
    pub fn new() -> Self {
        let enabled = std::env::var("KULTA_CDEVENTS_ENABLED")
            .unwrap_or_else(|_| "false".to_string())
            == "true";

        let sink_url = std::env::var("KULTA_CDEVENTS_SINK_URL").ok();

        CDEventsSink { enabled, sink_url }
    }

    #[cfg(test)]
    pub fn new_mock() -> Self {
        CDEventsSink {
            mock_events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[cfg(test)]
    #[allow(clippy::unwrap_used)] // Test helper can use unwrap
    pub fn get_emitted_events(&self) -> Vec<Event> {
        self.mock_events.lock().unwrap().clone()
    }

    #[cfg(test)]
    #[allow(clippy::unwrap_used)] // Test helper can use unwrap
    fn emit_event(&self, event: Event) {
        self.mock_events.lock().unwrap().push(event);
    }

    /// Send CloudEvent to HTTP sink (production mode)
    #[cfg(not(test))]
    async fn send_event(&self, event: &Event) -> Result<(), CDEventsError> {
        if !self.enabled {
            return Ok(()); // CDEvents disabled, skip
        }

        let Some(url) = &self.sink_url else {
            return Ok(()); // No sink URL configured, skip
        };

        // Send CloudEvent as JSON via HTTP POST
        let client = reqwest::Client::new();
        client
            .post(url)
            .header("Content-Type", "application/cloudevents+json")
            .json(event)
            .send()
            .await
            .map_err(|e| CDEventsError::Generic(format!("HTTP POST failed: {}", e)))?;

        Ok(())
    }
}

/// Emit CDEvent based on status transition
///
/// This function determines which CDEvent to emit based on the phase transition
/// and sends it to the configured sink.
pub async fn emit_status_change_event(
    rollout: &Rollout,
    old_status: &Option<RolloutStatus>,
    new_status: &RolloutStatus,
    sink: &CDEventsSink,
) -> Result<(), CDEventsError> {
    use crate::crd::rollout::Phase;

    // Detect transition: None → Progressing = service.deployed
    let is_initialization =
        old_status.is_none() && matches!(new_status.phase, Some(Phase::Progressing));

    // Detect step progression: Progressing → Progressing (different step)
    let is_step_progression = match (old_status, &new_status.phase) {
        (Some(old), Some(Phase::Progressing)) => {
            matches!(old.phase, Some(Phase::Progressing))
                && old.current_step_index != new_status.current_step_index
        }
        _ => false,
    };

    // Detect rollback: Any → Failed
    let is_rollback = matches!(new_status.phase, Some(Phase::Failed));

    // Detect completion: Progressing → Completed
    let is_completion = matches!(new_status.phase, Some(Phase::Completed));

    if is_initialization {
        // Build service.deployed event
        let event = build_service_deployed_event(rollout, new_status)?;

        // Emit to sink
        #[cfg(test)]
        sink.emit_event(event);
        #[cfg(not(test))]
        sink.send_event(&event).await?;

        Ok(())
    } else if is_step_progression {
        // Build service.upgraded event
        let event = build_service_upgraded_event(rollout, new_status)?;

        // Emit to sink
        #[cfg(test)]
        sink.emit_event(event);
        #[cfg(not(test))]
        sink.send_event(&event).await?;

        Ok(())
    } else if is_rollback {
        // Build service.rolledback event
        let event = build_service_rolledback_event(rollout, new_status)?;

        // Emit to sink
        #[cfg(test)]
        sink.emit_event(event);
        #[cfg(not(test))]
        sink.send_event(&event).await?;

        Ok(())
    } else if is_completion {
        // Build service.published event
        let event = build_service_published_event(rollout, new_status)?;

        // Emit to sink
        #[cfg(test)]
        sink.emit_event(event);
        #[cfg(not(test))]
        sink.send_event(&event).await?;

        Ok(())
    } else {
        // No event for other transitions (yet)
        Ok(())
    }
}

/// Build a service.deployed CDEvent
fn build_service_deployed_event(
    rollout: &Rollout,
    status: &RolloutStatus,
) -> Result<Event, CDEventsError> {
    use cdevents_sdk::latest::service_deployed;
    use cdevents_sdk::{CDEvent, Subject};

    let image = extract_image_from_rollout(rollout)?;

    let namespace = rollout
        .metadata
        .namespace
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("rollout missing namespace".to_string()))?;
    let name = rollout
        .metadata
        .name
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("rollout missing name".to_string()))?;

    let cdevent = CDEvent::from(
        Subject::from(service_deployed::Content {
            artifact_id: image
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid artifact_id: {}", e)))?,
            environment: service_deployed::ContentEnvironment {
                id: format!("{}/{}", namespace, name).try_into().map_err(|e| {
                    CDEventsError::Generic(format!("Invalid environment id: {}", e))
                })?,
                source: Some(
                    format!(
                        "/apis/argoproj.io/v1alpha1/namespaces/{}/rollouts/{}",
                        namespace, name
                    )
                    .try_into()
                    .map_err(|e| {
                        CDEventsError::Generic(format!("Invalid environment source: {}", e))
                    })?,
                ),
            },
        })
        .with_id(
            format!("/rollouts/{}/initialization", name)
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid subject id: {}", e)))?,
        )
        .with_source(
            "https://kulta.io/controller"
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid subject source: {}", e)))?,
        ),
    )
    .with_id(
        uuid::Uuid::new_v4()
            .to_string()
            .try_into()
            .map_err(|e| CDEventsError::Generic(format!("Invalid event id: {}", e)))?,
    )
    .with_source(
        "https://kulta.io"
            .try_into()
            .map_err(|e| CDEventsError::Generic(format!("Invalid event source: {}", e)))?,
    )
    .with_custom_data(build_kulta_custom_data(rollout, status, "initialization"));

    let cloudevent: Event = cdevent
        .try_into()
        .map_err(|e| CDEventsError::Generic(format!("Failed to convert to CloudEvent: {}", e)))?;

    Ok(cloudevent)
}

/// Build a service.upgraded CDEvent
fn build_service_upgraded_event(
    rollout: &Rollout,
    status: &RolloutStatus,
) -> Result<Event, CDEventsError> {
    use cdevents_sdk::latest::service_upgraded;
    use cdevents_sdk::{CDEvent, Subject};

    // Extract image from rollout spec (artifact_id)
    let image = extract_image_from_rollout(rollout)?;

    // Extract namespace and name
    let namespace = rollout
        .metadata
        .namespace
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("Rollout missing namespace".to_string()))?;
    let name = rollout
        .metadata
        .name
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("Rollout missing name".to_string()))?;

    let step_index = status.current_step_index.unwrap_or(0);

    // Build CDEvent
    let cdevent = CDEvent::from(
        Subject::from(service_upgraded::Content {
            artifact_id: image
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid artifact_id: {}", e)))?,
            environment: service_upgraded::ContentEnvironment {
                id: format!("{}/{}", namespace, name).try_into().map_err(|e| {
                    CDEventsError::Generic(format!("Invalid environment id: {}", e))
                })?,
                source: Some(
                    format!(
                        "/apis/argoproj.io/v1alpha1/namespaces/{}/rollouts/{}",
                        namespace, name
                    )
                    .try_into()
                    .map_err(|e| {
                        CDEventsError::Generic(format!("Invalid environment source: {}", e))
                    })?,
                ),
            },
        })
        .with_id(
            format!("/rollouts/{}/step/{}", name, step_index)
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid subject id: {}", e)))?,
        )
        .with_source(
            "https://kulta.io/controller"
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid subject source: {}", e)))?,
        ),
    )
    .with_id(
        uuid::Uuid::new_v4()
            .to_string()
            .try_into()
            .map_err(|e| CDEventsError::Generic(format!("Invalid event id: {}", e)))?,
    )
    .with_source(
        "https://kulta.io"
            .try_into()
            .map_err(|e| CDEventsError::Generic(format!("Invalid event source: {}", e)))?,
    )
    .with_custom_data(build_kulta_custom_data(rollout, status, "step_advanced"));

    // Convert to CloudEvent
    let cloudevent: Event = cdevent
        .try_into()
        .map_err(|e| CDEventsError::Generic(format!("Failed to convert to CloudEvent: {}", e)))?;

    Ok(cloudevent)
}

/// Build a service.rolledback CDEvent
fn build_service_rolledback_event(
    rollout: &Rollout,
    status: &RolloutStatus,
) -> Result<Event, CDEventsError> {
    use cdevents_sdk::latest::service_rolledback;
    use cdevents_sdk::{CDEvent, Subject};

    let image = extract_image_from_rollout(rollout)?;

    let namespace = rollout
        .metadata
        .namespace
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("Rollout missing namespace".to_string()))?;
    let name = rollout
        .metadata
        .name
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("Rollout missing name".to_string()))?;

    let cdevent = CDEvent::from(
        Subject::from(service_rolledback::Content {
            artifact_id: image
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid artifact_id: {}", e)))?,
            environment: service_rolledback::ContentEnvironment {
                id: format!("{}/{}", namespace, name).try_into().map_err(|e| {
                    CDEventsError::Generic(format!("Invalid environment id: {}", e))
                })?,
                source: Some(
                    format!(
                        "/apis/argoproj.io/v1alpha1/namespaces/{}/rollouts/{}",
                        namespace, name
                    )
                    .try_into()
                    .map_err(|e| {
                        CDEventsError::Generic(format!("Invalid environment source: {}", e))
                    })?,
                ),
            },
        })
        .with_id(
            format!("/rollouts/{}/rollback", name)
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid subject id: {}", e)))?,
        )
        .with_source(
            "https://kulta.io/controller"
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid subject source: {}", e)))?,
        ),
    )
    .with_id(
        uuid::Uuid::new_v4()
            .to_string()
            .try_into()
            .map_err(|e| CDEventsError::Generic(format!("Invalid event id: {}", e)))?,
    )
    .with_source(
        "https://kulta.io"
            .try_into()
            .map_err(|e| CDEventsError::Generic(format!("Invalid event source: {}", e)))?,
    )
    .with_custom_data(build_kulta_custom_data(rollout, status, "analysis_failed"));

    let cloudevent: Event = cdevent
        .try_into()
        .map_err(|e| CDEventsError::Generic(format!("Failed to convert to CloudEvent: {}", e)))?;

    Ok(cloudevent)
}

/// Build a service.published CDEvent
fn build_service_published_event(
    rollout: &Rollout,
    status: &RolloutStatus,
) -> Result<Event, CDEventsError> {
    use cdevents_sdk::latest::service_published;
    use cdevents_sdk::{CDEvent, Subject};

    // Extract namespace and name
    let namespace = rollout
        .metadata
        .namespace
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("Rollout missing namespace".to_string()))?;
    let name = rollout
        .metadata
        .name
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("Rollout missing name".to_string()))?;

    // Build CDEvent
    let cdevent = CDEvent::from(
        Subject::from(service_published::Content {
            environment: Some(service_published::ContentEnvironment {
                id: format!("{}/{}", namespace, name).try_into().map_err(|e| {
                    CDEventsError::Generic(format!("Invalid environment id: {}", e))
                })?,
                source: Some(
                    format!(
                        "/apis/argoproj.io/v1alpha1/namespaces/{}/rollouts/{}",
                        namespace, name
                    )
                    .try_into()
                    .map_err(|e| {
                        CDEventsError::Generic(format!("Invalid environment source: {}", e))
                    })?,
                ),
            }),
        })
        .with_id(
            format!("/rollouts/{}/completed", name)
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid subject id: {}", e)))?,
        )
        .with_source(
            "https://kulta.io/controller"
                .try_into()
                .map_err(|e| CDEventsError::Generic(format!("Invalid subject source: {}", e)))?,
        ),
    )
    .with_id(
        uuid::Uuid::new_v4()
            .to_string()
            .try_into()
            .map_err(|e| CDEventsError::Generic(format!("Invalid event id: {}", e)))?,
    )
    .with_source(
        "https://kulta.io"
            .try_into()
            .map_err(|e| CDEventsError::Generic(format!("Invalid event source: {}", e)))?,
    )
    .with_custom_data(build_kulta_custom_data(rollout, status, "completed"));

    let cloudevent: Event = cdevent
        .try_into()
        .map_err(|e| CDEventsError::Generic(format!("Failed to convert to CloudEvent: {}", e)))?;

    Ok(cloudevent)
}

/// Build KULTA customData for CDEvents
fn build_kulta_custom_data(
    rollout: &Rollout,
    status: &RolloutStatus,
    decision_reason: &str,
) -> serde_json::Value {
    let strategy = if rollout.spec.strategy.canary.is_some() {
        "canary"
    } else {
        "simple"
    };

    let total_steps = rollout
        .spec
        .strategy
        .canary
        .as_ref()
        .map(|c| c.steps.len())
        .unwrap_or(0);

    json!({
        "kulta": {
            "version": "v1",
            "rollout": {
                "name": rollout.metadata.name.as_deref().unwrap_or("unknown"),
                "namespace": rollout.metadata.namespace.as_deref().unwrap_or("default"),
                "uid": rollout.metadata.uid.as_deref().unwrap_or(""),
                "generation": rollout.metadata.generation.unwrap_or(0)
            },
            "strategy": strategy,
            "step": {
                "index": status.current_step_index.unwrap_or(0),
                "total": total_steps,
                "traffic_weight": status.current_weight.unwrap_or(0)
            },
            "decision": {
                "reason": decision_reason
            }
        }
    })
}

/// Extract image from rollout's pod template
fn extract_image_from_rollout(rollout: &Rollout) -> Result<String, CDEventsError> {
    let containers = &rollout
        .spec
        .template
        .spec
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("pod template missing spec".to_string()))?
        .containers;

    let first_container = containers
        .first()
        .ok_or_else(|| CDEventsError::Generic("pod template has no containers".to_string()))?;

    let image = first_container
        .image
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("container missing image".to_string()))?;

    Ok(image.clone())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Tests can use unwrap/expect for brevity
#[path = "cdevents_test.rs"]
mod tests;
