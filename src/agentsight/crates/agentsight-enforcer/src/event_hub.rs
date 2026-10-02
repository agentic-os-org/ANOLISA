//! Bounded non-blocking fan-out for raw violations and normalized security events.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard};

use agentsight_enforcement_protocol::{
    EnforcementStateEvent, HealthStatus, SecurityEvent, SecurityEventKind, ViolationEvent,
};
use uuid::Uuid;

/// Default pending events retained for each subscriber.
const DEFAULT_SUBSCRIBER_CAPACITY: usize = 256;

/// Non-blocking bounded publisher for violation events.
pub struct EventHub {
    capacity: usize,
    subscribers: Mutex<Vec<Subscriber>>,
    dropped_events: AtomicU64,
}

/// Delivery role assigned to a violation subscriber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriberClass {
    /// Evidence stream required before policy application is allowed.
    Required,
    /// Diagnostic observer that must not affect enforcement readiness.
    BestEffort,
}

struct Subscriber {
    id: Uuid,
    class: SubscriberClass,
    sender: SyncSender<ViolationEvent>,
}

struct SecuritySubscriber {
    sender: SyncSender<SecurityEvent>,
    dropped_events: u64,
    reported_dropped_events: u64,
}

/// Non-blocking bounded publisher for normalized security events.
pub(crate) struct SecurityEventHub {
    capacity: usize,
    subscribers: Mutex<Vec<SecuritySubscriber>>,
    dropped_events: AtomicU64,
    unassigned_dropped_events: AtomicU64,
    last_event: Mutex<Option<SecurityEvent>>,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new(DEFAULT_SUBSCRIBER_CAPACITY)
    }
}

impl EventHub {
    /// Creates a hub with a bounded queue for every subscriber.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            subscribers: Mutex::new(Vec::new()),
            dropped_events: AtomicU64::new(0),
        }
    }

    /// Registers one identified required or best-effort subscriber.
    pub(crate) fn subscribe(&self, id: Uuid, class: SubscriberClass) -> Receiver<ViolationEvent> {
        let (sender, receiver) = mpsc::sync_channel(self.capacity);
        let mut subscribers = self.subscribers();
        subscribers.retain(|subscriber| subscriber.id != id);
        subscribers.push(Subscriber { id, class, sender });
        receiver
    }

    /// Removes a subscriber without treating normal lifecycle exit as delivery loss.
    pub(crate) fn unsubscribe(&self, id: Uuid) {
        self.subscribers().retain(|subscriber| subscriber.id != id);
    }

    /// Publishes without allowing a slow subscriber to block policy lifecycle.
    pub fn publish(&self, event: ViolationEvent) {
        let mut dropped_deliveries = 0;
        let mut subscribers = self.subscribers();
        let mut required_present = false;
        subscribers.retain(|subscriber| {
            required_present |= subscriber.class == SubscriberClass::Required;
            match subscriber.sender.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    if subscriber.class == SubscriberClass::Required {
                        dropped_deliveries += 1;
                    }
                    true
                }
                Err(TrySendError::Disconnected(_)) => {
                    if subscriber.class == SubscriberClass::Required {
                        dropped_deliveries += 1;
                    }
                    false
                }
            }
        });
        if !required_present {
            dropped_deliveries += 1;
        }
        drop(subscribers);
        self.record_required_delivery_loss(dropped_deliveries);
    }

    /// Records events accepted by the local queue but lost at the required peer.
    pub(crate) fn record_required_delivery_loss(&self, count: u64) {
        if count == 0 {
            return;
        }
        // The count is diagnostic and sticky, so it does not order event data.
        let _ = self
            .dropped_events
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(count))
            });
    }

    /// Returns required subscriber deliveries lost since this hub was created.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    /// Marks backend health degraded after any required delivery is lost.
    pub(crate) fn reflect_delivery_loss(&self, mut health: HealthStatus) -> HealthStatus {
        let dropped_events = self.dropped_events();
        if dropped_events == 0 {
            return health;
        }
        let delivery_loss =
            format!("violation event delivery loss: dropped_events={dropped_events}");
        health.ready = false;
        health.message = Some(match health.message.take() {
            Some(message) if !message.is_empty() => format!("{message}; {delivery_loss}"),
            _ => delivery_loss,
        });
        health
    }

    #[cfg(test)]
    fn subscriber_count(&self) -> usize {
        self.subscribers().len()
    }

    fn subscribers(&self) -> MutexGuard<'_, Vec<Subscriber>> {
        self.subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for SecurityEventHub {
    fn default() -> Self {
        Self::new(DEFAULT_SUBSCRIBER_CAPACITY)
    }
}

impl SecurityEventHub {
    /// Creates a security hub with a bounded queue for every subscriber.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            subscribers: Mutex::new(Vec::new()),
            dropped_events: AtomicU64::new(0),
            unassigned_dropped_events: AtomicU64::new(0),
            last_event: Mutex::new(None),
        }
    }

    /// Registers an independent normalized security-event subscriber.
    pub(crate) fn subscribe(&self) -> Receiver<SecurityEvent> {
        let (sender, receiver) = mpsc::sync_channel(self.capacity);
        let mut subscribers = self.subscribers();
        let dropped_events = self.unassigned_dropped_events.swap(0, Ordering::Relaxed);
        subscribers.push(SecuritySubscriber {
            sender,
            dropped_events,
            reported_dropped_events: 0,
        });
        drop(subscribers);
        self.publish_pending_loss();
        receiver
    }

    /// Publishes without allowing evidence delivery to block policy decisions.
    // ActPlane publishes only after its raw provenance has been normalized.
    #[cfg_attr(not(feature = "mock-backend"), allow(dead_code))]
    pub(crate) fn publish(&self, event: SecurityEvent) {
        self.publish_pending_loss();
        *self.last_event() = Some(event.clone());
        let mut subscribers = self.subscribers();
        if subscribers.is_empty() {
            let _ = self.unassigned_dropped_events.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |current| Some(current.saturating_add(1)),
            );
            drop(subscribers);
            self.record_delivery_loss(1);
            return;
        }
        let mut dropped_deliveries = 0;
        subscribers.retain_mut(
            |subscriber| match subscriber.sender.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    subscriber.dropped_events = subscriber.dropped_events.saturating_add(1);
                    dropped_deliveries += 1;
                    true
                }
                Err(TrySendError::Disconnected(_)) => {
                    dropped_deliveries += 1;
                    false
                }
            },
        );
        drop(subscribers);
        self.record_delivery_loss(dropped_deliveries);
    }

    fn publish_pending_loss(&self) {
        let Some(last_event) = self.last_event().clone() else {
            return;
        };
        self.subscribers().retain_mut(|subscriber| {
            if subscriber.dropped_events == subscriber.reported_dropped_events {
                return true;
            }
            let recovery = evidence_loss_event(&last_event, subscriber.dropped_events);
            match subscriber.sender.try_send(recovery) {
                Ok(()) => {
                    subscriber.reported_dropped_events = subscriber.dropped_events;
                    true
                }
                Err(TrySendError::Full(_)) => true,
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    pub(crate) fn record_delivery_loss(&self, count: u64) {
        if count == 0 {
            return;
        }
        let _ = self
            .dropped_events
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(count))
            });
    }

    /// Marks backend health degraded after normalized evidence delivery loss.
    pub(crate) fn reflect_delivery_loss(&self, mut health: HealthStatus) -> HealthStatus {
        let dropped_events = self.dropped_events.load(Ordering::Relaxed);
        if dropped_events == 0 {
            return health;
        }
        let delivery_loss =
            format!("security event delivery loss: dropped_events={dropped_events}");
        health.ready = false;
        health.message = Some(match health.message.take() {
            Some(message) if !message.is_empty() => format!("{message}; {delivery_loss}"),
            _ => delivery_loss,
        });
        health
    }

    fn subscribers(&self) -> MutexGuard<'_, Vec<SecuritySubscriber>> {
        self.subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn last_event(&self) -> MutexGuard<'_, Option<SecurityEvent>> {
        self.last_event
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn evidence_loss_event(last_event: &SecurityEvent, dropped_events: u64) -> SecurityEvent {
    let (policy_id, policy_revision) = event_policy(&last_event.kind);
    SecurityEvent {
        event_id: Uuid::new_v4(),
        occurred_at_ns: last_event.occurred_at_ns.saturating_add(1),
        observed_at_ns: last_event.observed_at_ns.saturating_add(1),
        identity: last_event.identity.clone(),
        kind: SecurityEventKind::EnforcementState(EnforcementStateEvent {
            policy_id,
            policy_revision,
            code: "evidence_loss".into(),
            ready: false,
            message: format!("security event delivery loss: dropped_events={dropped_events}"),
            dropped_events: Some(dropped_events),
        }),
    }
}

fn event_policy(kind: &SecurityEventKind) -> (Option<String>, Option<u64>) {
    match kind {
        SecurityEventKind::FileAction(event) => {
            (Some(event.policy_id.clone()), Some(event.policy_revision))
        }
        SecurityEventKind::TaintTransition(event) => {
            (Some(event.policy_id.clone()), Some(event.policy_revision))
        }
        SecurityEventKind::NetworkAction(event) => {
            (Some(event.policy_id.clone()), Some(event.policy_revision))
        }
        SecurityEventKind::PolicyDecision(event) => {
            (Some(event.policy_id.clone()), Some(event.policy_revision))
        }
        SecurityEventKind::EnforcementState(event) => {
            (event.policy_id.clone(), event.policy_revision)
        }
    }
}

#[cfg(test)]
mod tests {
    use agentsight_enforcement_protocol::{
        Effect, EnforcementStateEvent, EventIdentity, HealthStatus, SecurityEvent,
        SecurityEventKind, ViolationEvent,
    };
    use uuid::Uuid;

    use super::*;

    fn violation() -> ViolationEvent {
        ViolationEvent {
            event_id: Uuid::new_v4(),
            binding_id: Uuid::new_v4(),
            agent_id: "event-hub-test".into(),
            session_id: None,
            policy_id: "policy".into(),
            policy_revision: "revision".into(),
            pid: 42,
            ppid: Some(1),
            process_start_time: 99,
            operation: "open".into(),
            target: "/tmp/secret".into(),
            effect: Effect::Block,
            blocked: true,
            killed: false,
            rule_id: None,
            reason: None,
            occurred_at_ns: 100,
            observed_at_ns: 101,
            actplane_revision: "test".into(),
        }
    }

    fn required(hub: &EventHub) -> Receiver<ViolationEvent> {
        hub.subscribe(Uuid::new_v4(), SubscriberClass::Required)
    }

    fn security_event() -> SecurityEvent {
        SecurityEvent {
            event_id: Uuid::new_v4(),
            occurred_at_ns: 100,
            observed_at_ns: 101,
            identity: EventIdentity {
                binding_id: Uuid::new_v4(),
                agent_id: "event-hub-test".into(),
                agent_name: None,
                session_id: None,
                conversation_id: None,
                tool_call_id: None,
                pid: 42,
                process_start_time: 99,
                ppid: Some(1),
                cgroup_id: None,
                protocol_version: agentsight_enforcement_protocol::PROTOCOL_VERSION,
                enforcer_version: "test".into(),
                actplane_revision: "test".into(),
            },
            kind: SecurityEventKind::EnforcementState(EnforcementStateEvent {
                policy_id: Some("policy".into()),
                policy_revision: Some(1),
                code: "fixture".into(),
                ready: true,
                message: "fixture".into(),
                dropped_events: None,
            }),
        }
    }

    #[test]
    fn full_subscriber_records_a_sticky_dropped_event_count() {
        let hub = EventHub::new(1);
        let subscriber = required(&hub);

        hub.publish(violation());
        hub.publish(violation());
        hub.publish(violation());
        assert_eq!(hub.dropped_events(), 2);

        subscriber
            .try_recv()
            .expect("the first event should remain queued");
        hub.publish(violation());
        assert_eq!(hub.dropped_events(), 2);
    }

    #[test]
    fn disconnected_subscriber_is_pruned_and_the_undelivered_event_is_recorded() {
        let hub = EventHub::new(1);
        let subscriber = required(&hub);
        drop(subscriber);

        hub.publish(violation());

        assert_eq!(hub.dropped_events(), 1);
        assert_eq!(hub.subscriber_count(), 0);
        assert!(
            !hub.reflect_delivery_loss(HealthStatus {
                ready: true,
                backend: "test".into(),
                capabilities:
                    agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
                message: None,
            })
            .ready
        );
    }

    #[test]
    fn event_without_subscribers_is_recorded_as_dropped() {
        let hub = EventHub::new(1);

        hub.publish(violation());

        assert_eq!(hub.dropped_events(), 1);
        assert!(
            !hub.reflect_delivery_loss(HealthStatus {
                ready: true,
                backend: "test".into(),
                capabilities:
                    agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
                message: None,
            })
            .ready
        );
    }

    #[test]
    fn one_successful_delivery_does_not_mask_another_queue_overflow() {
        let hub = EventHub::new(1);
        let fast_subscriber = required(&hub);
        let _slow_subscriber = required(&hub);

        hub.publish(violation());
        fast_subscriber
            .try_recv()
            .expect("the fast subscriber should drain its queue");
        hub.publish(violation());

        assert_eq!(hub.dropped_events(), 1);
        assert!(
            !hub.reflect_delivery_loss(HealthStatus {
                ready: true,
                backend: "test".into(),
                capabilities:
                    agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
                message: None,
            })
            .ready
        );
    }

    #[test]
    fn overflow_degrades_health_with_the_sticky_cumulative_count() {
        let hub = EventHub::new(1);
        let _subscriber = required(&hub);
        hub.publish(violation());
        hub.publish(violation());
        hub.publish(violation());

        let health = hub.reflect_delivery_loss(HealthStatus {
            ready: true,
            backend: "test".into(),
            capabilities:
                agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
            message: Some("runtime healthy".into()),
        });

        assert!(!health.ready);
        assert_eq!(
            health.message.as_deref(),
            Some("runtime healthy; violation event delivery loss: dropped_events=2")
        );
    }

    #[test]
    fn observer_disconnect_is_pruned_without_poisoning_required_delivery() {
        let hub = EventHub::new(1);
        let _required = hub.subscribe(Uuid::new_v4(), SubscriberClass::Required);
        let observer = hub.subscribe(Uuid::new_v4(), SubscriberClass::BestEffort);
        drop(observer);

        hub.publish(violation());

        assert_eq!(hub.dropped_events(), 0);
        assert_eq!(hub.subscriber_count(), 1);
        assert!(
            hub.reflect_delivery_loss(HealthStatus {
                ready: true,
                backend: "test".into(),
                capabilities:
                    agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
                message: None,
            })
            .ready
        );
    }

    #[test]
    fn observer_delivery_does_not_mask_required_queue_overflow() {
        let hub = EventHub::new(1);
        let _required = hub.subscribe(Uuid::new_v4(), SubscriberClass::Required);
        let observer = hub.subscribe(Uuid::new_v4(), SubscriberClass::BestEffort);

        hub.publish(violation());
        observer
            .try_recv()
            .expect("observer should drain its queue");
        hub.publish(violation());

        assert_eq!(hub.dropped_events(), 1);
        assert!(
            !hub.reflect_delivery_loss(HealthStatus {
                ready: true,
                backend: "test".into(),
                capabilities:
                    agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
                message: None,
            })
            .ready
        );
    }

    #[test]
    fn recovering_security_subscriber_receives_one_evidence_loss_event() {
        let hub = SecurityEventHub::new(4);
        hub.publish(security_event());

        let subscriber = hub.subscribe();
        let recovered = subscriber
            .try_recv()
            .expect("recovery must disclose lost evidence");
        let SecurityEventKind::EnforcementState(state) = recovered.kind else {
            panic!("recovery frame must be enforcement state");
        };
        assert_eq!(state.code, "evidence_loss");
        assert_eq!(state.dropped_events, Some(1));
        assert!(subscriber.try_recv().is_err());
    }

    #[test]
    fn successful_security_delivery_does_not_mask_another_queue_overflow() {
        let hub = SecurityEventHub::new(1);
        let fast_subscriber = hub.subscribe();
        let _slow_subscriber = hub.subscribe();

        hub.publish(security_event());
        fast_subscriber
            .try_recv()
            .expect("the fast subscriber should drain its queue");
        hub.publish(security_event());

        assert_eq!(hub.dropped_events.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn security_loss_recovery_is_tracked_for_each_subscriber() {
        let hub = SecurityEventHub::new(1);
        let fast_subscriber = hub.subscribe();
        let slow_subscriber = hub.subscribe();

        hub.publish(security_event());
        fast_subscriber
            .try_recv()
            .expect("the fast subscriber should drain its first event");
        hub.publish(security_event());
        fast_subscriber
            .try_recv()
            .expect("the fast subscriber should receive the second event");
        slow_subscriber
            .try_recv()
            .expect("the slow subscriber should drain its first event");

        hub.publish(security_event());

        let recovered = slow_subscriber
            .try_recv()
            .expect("the slow subscriber should receive its own loss report");
        let SecurityEventKind::EnforcementState(state) = recovered.kind else {
            panic!("recovery frame must be enforcement state");
        };
        assert_eq!(state.code, "evidence_loss");
        assert_eq!(state.dropped_events, Some(1));
        let fast_event = fast_subscriber
            .try_recv()
            .expect("the fast subscriber should not receive another subscriber's recovery");
        let SecurityEventKind::EnforcementState(state) = fast_event.kind else {
            panic!("fixture must be enforcement state");
        };
        assert_eq!(state.code, "fixture");
    }
}
