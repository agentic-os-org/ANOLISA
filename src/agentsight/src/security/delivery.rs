//! Bounded-memory delivery retry semantics for normalized security events.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agentsight_enforcement_protocol::SecurityEvent;

const INGEST_ATTEMPTS: usize = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeliveryOutcome {
    /// The original event reached durable storage and correlation.
    Stored,
    /// The original event failed, but its explicit loss marker is durable.
    EvidenceLossRecorded,
    /// Shutdown interrupted delivery before either outcome became durable.
    Stopped,
}

/// Keeps one delivered event in memory until it is durable or its loss is durable.
pub(super) fn retry_delivered_event<Ingest, IngestError, Record, RecordError, Pause>(
    event: &SecurityEvent,
    stop: &AtomicBool,
    mut ingest: Ingest,
    mut record_loss: Record,
    mut pause: Pause,
) -> DeliveryOutcome
where
    Ingest: FnMut(&SecurityEvent) -> Result<(), IngestError>,
    Record: FnMut(&SecurityEvent) -> Result<(), RecordError>,
    Pause: FnMut(Duration),
{
    let mut backoff = INITIAL_BACKOFF;
    for attempt in 0..INGEST_ATTEMPTS {
        if stop.load(Ordering::Acquire) {
            return DeliveryOutcome::Stopped;
        }
        if ingest(event).is_ok() {
            return DeliveryOutcome::Stored;
        }
        if attempt + 1 < INGEST_ATTEMPTS {
            pause(backoff);
            backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
        }
    }

    // Never read another socket event until the explicit loss marker is durable.
    loop {
        if stop.load(Ordering::Acquire) {
            return DeliveryOutcome::Stopped;
        }
        if record_loss(event).is_ok() {
            return DeliveryOutcome::EvidenceLossRecorded;
        }
        pause(backoff);
        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use agentsight_enforcement_protocol::{
        EnforcementStateEvent, EventIdentity, SecurityEvent, SecurityEventKind,
    };

    use super::{DeliveryOutcome, retry_delivered_event};

    fn event() -> SecurityEvent {
        SecurityEvent {
            event_id: uuid::Uuid::new_v4(),
            occurred_at_ns: 1,
            observed_at_ns: 1,
            identity: EventIdentity {
                binding_id: uuid::Uuid::new_v4(),
                agent_id: "agent-1".into(),
                agent_name: None,
                session_id: None,
                conversation_id: None,
                tool_call_id: None,
                pid: 42,
                process_start_time: 7,
                ppid: None,
                cgroup_id: None,
                protocol_version: agentsight_enforcement_protocol::PROTOCOL_VERSION,
                enforcer_version: "test".into(),
                actplane_revision: "test".into(),
            },
            kind: SecurityEventKind::EnforcementState(EnforcementStateEvent {
                policy_id: None,
                policy_revision: None,
                code: "fixture".into(),
                ready: true,
                message: "fixture".into(),
                dropped_events: None,
            }),
        }
    }

    #[test]
    fn delivered_event_retries_the_same_stable_id_until_durable() {
        let event = event();
        let mut attempts = Vec::new();
        let mut loss_attempts = 0;

        let outcome = retry_delivered_event(
            &event,
            &AtomicBool::new(false),
            |candidate| {
                attempts.push(candidate.event_id);
                if attempts.len() < 3 {
                    Err("transient")
                } else {
                    Ok(())
                }
            },
            |_| {
                loss_attempts += 1;
                Ok::<_, &'static str>(())
            },
            |_| {},
        );

        assert_eq!(outcome, DeliveryOutcome::Stored);
        assert_eq!(attempts, vec![event.event_id; 3]);
        assert_eq!(loss_attempts, 0);
    }

    #[test]
    fn exhausted_ingestion_disconnects_only_after_evidence_loss_is_durable() {
        let event = event();
        let mut attempts = 0;
        let mut loss_ids = Vec::new();

        let outcome = retry_delivered_event(
            &event,
            &AtomicBool::new(false),
            |_| {
                attempts += 1;
                Err::<(), _>("permanent")
            },
            |candidate| {
                loss_ids.push(candidate.event_id);
                if loss_ids.len() == 1 {
                    Err("transient")
                } else {
                    Ok(())
                }
            },
            |_| {},
        );

        assert_eq!(outcome, DeliveryOutcome::EvidenceLossRecorded);
        assert_eq!(attempts, 3);
        assert_eq!(loss_ids, vec![event.event_id; 2]);
    }
}
