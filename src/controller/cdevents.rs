//! CDEvents integration for observability
//!
//! This module handles emission of CDEvents to track the lifecycle of progressive rollouts.

use crate::crd::rollout::{Rollout, RolloutStatus};
use cloudevents::Event;
use thiserror::Error;

#[cfg(test)]
use std::sync::{Arc, Mutex};

#[derive(Debug, Error)]
pub enum CDEventsError {
    #[error("CDEvents error: {0}")]
    Generic(String),
}

/// CDEvents sink for emitting events
pub struct CDEventsSink {
    #[allow(dead_code)] // Will be used for production HTTP sink
    enabled: bool,
    #[cfg(test)]
    mock_events: Arc<Mutex<Vec<Event>>>,
}

impl CDEventsSink {
    #[cfg(test)]
    pub fn new_mock() -> Self {
        CDEventsSink {
            enabled: true,
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
}

/// Emit CDEvent based on status transition
///
/// This function determines which CDEvent to emit based on the phase transition
/// and sends it to the configured sink.
pub async fn emit_status_change_event(
    rollout: &Rollout,
    old_status: &Option<RolloutStatus>,
    new_status: &RolloutStatus,
    #[cfg_attr(not(test), allow(unused_variables))] sink: &CDEventsSink,
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

    if is_initialization {
        // Build service.deployed event
        #[cfg_attr(not(test), allow(unused_variables))]
        let event = build_service_deployed_event(rollout, new_status)?;

        // Emit to sink (test only for now)
        #[cfg(test)]
        sink.emit_event(event);

        Ok(())
    } else if is_step_progression {
        // Build service.upgraded event
        #[cfg_attr(not(test), allow(unused_variables))]
        let event = build_service_upgraded_event(rollout, new_status)?;

        // Emit to sink (test only for now)
        #[cfg(test)]
        sink.emit_event(event);

        Ok(())
    } else if is_rollback {
        // Build service.rolledback event
        #[cfg_attr(not(test), allow(unused_variables))]
        let event = build_service_rolledback_event(rollout, new_status)?;

        // Emit to sink (test only for now)
        #[cfg(test)]
        sink.emit_event(event);

        Ok(())
    } else {
        // No event for other transitions (yet)
        Ok(())
    }
}

/// Build a service.deployed CDEvent
fn build_service_deployed_event(
    rollout: &Rollout,
    _status: &RolloutStatus,
) -> Result<Event, CDEventsError> {
    use cdevents_sdk::latest::service_deployed;
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

    // Build CDEvent
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
    );

    // Convert to CloudEvent
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
    );

    // Convert to CloudEvent
    let cloudevent: Event = cdevent
        .try_into()
        .map_err(|e| CDEventsError::Generic(format!("Failed to convert to CloudEvent: {}", e)))?;

    Ok(cloudevent)
}

/// Build a service.rolledback CDEvent
fn build_service_rolledback_event(
    rollout: &Rollout,
    _status: &RolloutStatus,
) -> Result<Event, CDEventsError> {
    use cdevents_sdk::latest::service_rolledback;
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

    // Build CDEvent
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
    );

    // Convert to CloudEvent
    let cloudevent: Event = cdevent
        .try_into()
        .map_err(|e| CDEventsError::Generic(format!("Failed to convert to CloudEvent: {}", e)))?;

    Ok(cloudevent)
}

/// Extract image from rollout's pod template
fn extract_image_from_rollout(rollout: &Rollout) -> Result<String, CDEventsError> {
    let containers = &rollout
        .spec
        .template
        .spec
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("Pod template missing spec".to_string()))?
        .containers;

    let first_container = containers
        .first()
        .ok_or_else(|| CDEventsError::Generic("Pod template has no containers".to_string()))?;

    let image = first_container
        .image
        .as_ref()
        .ok_or_else(|| CDEventsError::Generic("Container missing image".to_string()))?;

    Ok(image.clone())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Tests can use unwrap/expect for brevity
#[path = "cdevents_test.rs"]
mod tests;
