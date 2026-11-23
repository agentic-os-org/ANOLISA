//! Ingests normalized enforcer events and creates explainable risk cases.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agentsight_audit::{AuditService, AuditServiceError};
use agentsight_enforcement_protocol::{
    EnforcementStateEvent, EventIdentity, SecurityEvent, SecurityEventKind,
};
use thiserror::Error;

use super::SecurityStore;
use super::delivery::{DeliveryOutcome, retry_delivered_event};
use crate::enforcement::{EnforcementClient, EnforcementError};

/// Security ingestion and correlation failures.
#[derive(Debug, Error)]
pub enum SecurityCoordinatorError {
    /// Local persistence or query failed.
    #[error(transparent)]
    Store(#[from] super::SecurityStoreError),
    /// A decision referenced evidence that has not been persisted.
    #[error("security decision references missing {kind} event {event_id}")]
    MissingEvidence {
        /// Expected evidence role.
        kind: &'static str,
        /// Missing stable event identifier.
        event_id: uuid::Uuid,
    },
    /// A second subscription worker was requested for the same coordinator.
    #[error("security coordinator is already running")]
    AlreadyRunning,
    /// Spawning the subscription worker failed.
    #[error("failed to start security coordinator: {0}")]
    Thread(#[from] std::io::Error),
}

fn map_audit_error(error: AuditServiceError) -> SecurityCoordinatorError {
    match error {
        AuditServiceError::Store(error) => SecurityCoordinatorError::Store(error),
        AuditServiceError::MissingEvidence { kind, event_id } => {
            SecurityCoordinatorError::MissingEvidence { kind, event_id }
        }
    }
}

/// AgentSight-owned normalized-event ingestor and risk correlator.
pub struct SecurityCoordinator {
    client: EnforcementClient,
    audit: Arc<AuditService>,
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    last_context: Arc<Mutex<Option<EventContext>>>,
}

#[derive(Clone)]
struct EventContext {
    identity: EventIdentity,
    policy_id: Option<String>,
    policy_revision: Option<u64>,
}

impl SecurityCoordinator {
    /// Creates a coordinator for one local enforcer and security store.
    pub fn new(client: EnforcementClient, store: Arc<SecurityStore>) -> Self {
        Self::with_service(client, Arc::new(AuditService::new(store.audit_store())))
    }

    /// Creates a coordinator over the process-wide audit service.
    pub fn with_service(client: EnforcementClient, audit: Arc<AuditService>) -> Self {
        Self {
            client,
            audit,
            stop: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            last_context: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the configured enforcer client.
    pub fn client(&self) -> &EnforcementClient {
        &self.client
    }

    /// Persists one event and creates an idempotent case for audit/enforce decisions.
    ///
    /// # Errors
    ///
    /// Returns a typed storage or missing-evidence error.
    pub fn ingest(&self, event: SecurityEvent) -> Result<(), SecurityCoordinatorError> {
        *self
            .last_context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(event_context(&event));
        self.audit.ingest(event).map_err(map_audit_error)
    }

    /// Starts the reconnecting normalized-event subscription worker.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityCoordinatorError::AlreadyRunning`] for duplicate
    /// workers or a thread-spawn error.
    pub fn start(&self) -> Result<JoinHandle<()>, SecurityCoordinatorError> {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(SecurityCoordinatorError::AlreadyRunning);
        }
        self.stop.store(false, Ordering::Release);
        let client = self.client.clone();
        let audit = Arc::clone(&self.audit);
        let stop = Arc::clone(&self.stop);
        let running = Arc::clone(&self.running);
        let last_context = Arc::clone(&self.last_context);
        thread::Builder::new()
            .name("agentsight-security-events".into())
            .spawn(move || {
                subscription_loop(client, audit, &stop, &last_context);
                running.store(false, Ordering::Release);
            })
            .map_err(|error| {
                self.running.store(false, Ordering::Release);
                error.into()
            })
    }

    /// Requests a running subscription worker to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn subscription_loop(
    client: EnforcementClient,
    audit: Arc<AuditService>,
    stop: &AtomicBool,
    last_context: &Mutex<Option<EventContext>>,
) {
    let mut backoff = Duration::from_millis(100);
    let mut disconnected = false;
    let mut log_state = SubscriptionLogState::default();
    while !stop.load(Ordering::Acquire) {
        match client.subscribe_security_events() {
            Ok(mut subscription) => {
                if disconnected {
                    record_subscription_state(&audit, last_context, true, "subscription_restored");
                }
                disconnected = false;
                if matches!(log_state.on_connected(), SubscriptionLog::Restored) {
                    log::info!("security event subscription restored");
                }
                loop {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    match subscription.next_event() {
                        Ok(Some(event)) => {
                            backoff = Duration::from_millis(100);
                            *last_context
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(event_context(&event));
                            let evidence_loss = evidence_loss_event(&event);
                            match retry_delivered_event(
                                &event,
                                stop,
                                |candidate| {
                                    audit.ingest(candidate.clone()).map_err(|error| {
                                        log::error!(
                                            "security event {} ingestion failed: {error}",
                                            candidate.event_id
                                        );
                                        error
                                    })
                                },
                                |_| {
                                    audit.store().insert_event(&evidence_loss).map(|_| ()).map_err(
                                        |error| {
                                            log::error!(
                                                "failed to persist evidence-loss marker {}: {error}",
                                                evidence_loss.event_id
                                            );
                                            error
                                        },
                                    )
                                },
                                |duration| sleep_until_retry(stop, duration),
                            ) {
                                DeliveryOutcome::Stored => {}
                                DeliveryOutcome::Stopped => return,
                                DeliveryOutcome::EvidenceLossRecorded => {
                                    log::warn!(
                                        "security event {} could not be correlated; reconnecting after recording evidence loss",
                                        event.event_id
                                    );
                                    disconnected = true;
                                    break;
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            if matches!(log_state.on_disconnect(), SubscriptionLog::Failure) {
                                log::warn!("security event subscription disconnected: {error}");
                            }
                            if !disconnected {
                                record_subscription_state(
                                    &audit,
                                    last_context,
                                    false,
                                    "subscription_lost",
                                );
                            }
                            disconnected = true;
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                match log_state.on_connect_failure(&error) {
                    // No enforcer deployed: expected idle mode, notice once.
                    SubscriptionLog::IdleWithoutEnforcer => log::info!(
                        "enforcer socket not present at {}; running without security-event subscription (deploy agentsight-enforcer to enable enforcement)",
                        client.socket_path().display()
                    ),
                    SubscriptionLog::Failure => {
                        log::warn!("security event subscription failed: {error}")
                    }
                    SubscriptionLog::Silent | SubscriptionLog::Restored => {}
                }
                if !disconnected {
                    record_subscription_state(&audit, last_context, false, "subscription_lost");
                    disconnected = true;
                }
            }
        }
        sleep_until_retry(stop, backoff);
        backoff = backoff.saturating_mul(2).min(Duration::from_secs(5));
    }
}

/// Decides which subscription log line (if any) to emit on each loop turn.
///
/// Mirrors the enforcement coordinator's `AvailabilityLogTransitions`: logging is
/// edge-triggered so a deployment without an enforcer records the expected idle
/// state once instead of warning on every reconnect attempt. The retry backoff
/// caps at five seconds, which otherwise floods the log in that mode.
#[derive(Default)]
struct SubscriptionLogState {
    /// Whether a live subscription has ever been held.
    ever_connected: bool,
    /// Whether the current outage has already produced a log line.
    failure_logged: bool,
}

/// The single log line a subscription state transition should emit.
enum SubscriptionLog {
    /// Emit nothing.
    Silent,
    /// One-time notice that no enforcer is deployed (expected idle mode).
    IdleWithoutEnforcer,
    /// A genuine subscription failure or mid-stream loss.
    Failure,
    /// The subscription was re-established after an outage.
    Restored,
}

impl SubscriptionLogState {
    /// Classifies a failure to open a subscription.
    fn on_connect_failure(&mut self, error: &EnforcementError) -> SubscriptionLog {
        if self.failure_logged {
            return SubscriptionLog::Silent;
        }
        self.failure_logged = true;
        if !self.ever_connected && enforcer_absent(error) {
            SubscriptionLog::IdleWithoutEnforcer
        } else {
            SubscriptionLog::Failure
        }
    }

    /// Classifies a live subscription that dropped mid-stream.
    fn on_disconnect(&mut self) -> SubscriptionLog {
        if self.failure_logged {
            return SubscriptionLog::Silent;
        }
        self.failure_logged = true;
        SubscriptionLog::Failure
    }

    /// Classifies a newly (re)established subscription.
    fn on_connected(&mut self) -> SubscriptionLog {
        self.ever_connected = true;
        if std::mem::take(&mut self.failure_logged) {
            SubscriptionLog::Restored
        } else {
            SubscriptionLog::Silent
        }
    }
}

/// Whether a subscription failure means "no enforcer is deployed" rather than a
/// transient fault: the socket path itself is missing (`ENOENT`), so reconnecting
/// cannot help until an enforcer appears.
fn enforcer_absent(error: &EnforcementError) -> bool {
    matches!(error, EnforcementError::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
}

fn evidence_loss_event(event: &SecurityEvent) -> SecurityEvent {
    let context = event_context(event);
    let now = unix_epoch_ns();
    SecurityEvent {
        event_id: uuid::Uuid::new_v4(),
        occurred_at_ns: now,
        observed_at_ns: now,
        identity: context.identity,
        kind: SecurityEventKind::EnforcementState(EnforcementStateEvent {
            policy_id: context.policy_id,
            policy_revision: context.policy_revision,
            code: "evidence_loss".into(),
            ready: false,
            message: "normalized security event could not be durably correlated; subscription disconnected".into(),
            dropped_events: Some(1),
        }),
    }
}

fn record_subscription_state(
    audit: &AuditService,
    last_context: &Mutex<Option<EventContext>>,
    ready: bool,
    code: &str,
) {
    let context = last_context
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let Some(context) = context else {
        return;
    };
    let now = unix_epoch_ns();
    let event = SecurityEvent {
        event_id: uuid::Uuid::new_v4(),
        occurred_at_ns: now,
        observed_at_ns: now,
        identity: context.identity,
        kind: SecurityEventKind::EnforcementState(EnforcementStateEvent {
            policy_id: context.policy_id,
            policy_revision: context.policy_revision,
            code: code.into(),
            ready,
            message: if ready {
                "normalized security event subscription restored".into()
            } else {
                "normalized security event subscription lost; reconnecting".into()
            },
            dropped_events: None,
        }),
    };
    if let Err(error) = audit.store().insert_event(&event) {
        log::error!("failed to persist security subscription state: {error}");
    }
}

fn event_context(event: &SecurityEvent) -> EventContext {
    let (policy_id, policy_revision) = match &event.kind {
        SecurityEventKind::FileAction(value) => {
            (Some(value.policy_id.clone()), Some(value.policy_revision))
        }
        SecurityEventKind::TaintTransition(value) => {
            (Some(value.policy_id.clone()), Some(value.policy_revision))
        }
        SecurityEventKind::NetworkAction(value) => {
            (Some(value.policy_id.clone()), Some(value.policy_revision))
        }
        SecurityEventKind::PolicyDecision(value) => {
            (Some(value.policy_id.clone()), Some(value.policy_revision))
        }
        SecurityEventKind::EnforcementState(value) => {
            (value.policy_id.clone(), value.policy_revision)
        }
    };
    EventContext {
        identity: event.identity.clone(),
        policy_id,
        policy_revision,
    }
}

fn sleep_until_retry(stop: &AtomicBool, duration: Duration) {
    let mut remaining = duration;
    while !stop.load(Ordering::Acquire) && !remaining.is_zero() {
        let interval = remaining.min(Duration::from_millis(50));
        thread::sleep(interval);
        remaining = remaining.saturating_sub(interval);
    }
}

fn unix_epoch_ns() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    nanos.min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    fn absent_error() -> EnforcementError {
        EnforcementError::Io(io::Error::from(io::ErrorKind::NotFound))
    }

    fn refused_error() -> EnforcementError {
        EnforcementError::Io(io::Error::from(io::ErrorKind::ConnectionRefused))
    }

    #[test]
    fn missing_enforcer_logs_idle_notice_once_not_every_retry() {
        let mut state = SubscriptionLogState::default();
        assert!(matches!(
            state.on_connect_failure(&absent_error()),
            SubscriptionLog::IdleWithoutEnforcer
        ));
        // The reconnect loop repeats this every five seconds; all must be silent.
        for _ in 0..10 {
            assert!(matches!(
                state.on_connect_failure(&absent_error()),
                SubscriptionLog::Silent
            ));
        }
    }

    #[test]
    fn enforcer_appearing_later_reports_restored_once() {
        let mut state = SubscriptionLogState::default();
        assert!(matches!(
            state.on_connect_failure(&absent_error()),
            SubscriptionLog::IdleWithoutEnforcer
        ));
        assert!(matches!(state.on_connected(), SubscriptionLog::Restored));
        // Steady healthy state produces no further logs.
        assert!(matches!(state.on_connected(), SubscriptionLog::Silent));
    }

    #[test]
    fn live_disconnect_warns_once_per_outage() {
        let mut state = SubscriptionLogState::default();
        // Fresh healthy connect is silent.
        assert!(matches!(state.on_connected(), SubscriptionLog::Silent));
        // Stream drops: one failure, then silence across reconnect attempts.
        assert!(matches!(state.on_disconnect(), SubscriptionLog::Failure));
        assert!(matches!(
            state.on_connect_failure(&refused_error()),
            SubscriptionLog::Silent
        ));
        // Recovery logs once.
        assert!(matches!(state.on_connected(), SubscriptionLog::Restored));
    }

    #[test]
    fn failure_after_connection_is_not_idle_even_if_socket_vanishes() {
        let mut state = SubscriptionLogState::default();
        assert!(matches!(state.on_connected(), SubscriptionLog::Silent));
        // A socket that disappears after we once connected is a real fault.
        assert!(matches!(
            state.on_connect_failure(&absent_error()),
            SubscriptionLog::Failure
        ));
    }

    #[test]
    fn enforcer_absent_matches_only_missing_socket() {
        assert!(enforcer_absent(&absent_error()));
        assert!(!enforcer_absent(&refused_error()));
    }
}
