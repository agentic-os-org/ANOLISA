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

/// Identity of one normalized security-event subscription.
///
/// Identities increase monotonically, so a subscription created after another one is the only
/// kind that can be its replacement after a collector restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecuritySubscriberId(u64);

struct SecuritySubscriber {
    id: SecuritySubscriberId,
    sender: SyncSender<SecurityEvent>,
    dropped_events: u64,
    reported_dropped_events: u64,
}

/// Losses that no live subscriber has taken ownership of yet.
struct OrphanedLoss {
    /// Only subscriptions newer than this one may disclose the loss.
    departed: SecuritySubscriberId,
    count: u64,
}

impl SecuritySubscriber {
    fn unreported_losses(&self) -> u64 {
        self.dropped_events
            .saturating_sub(self.reported_dropped_events)
    }
}

/// Non-blocking bounded publisher for normalized security events.
pub(crate) struct SecurityEventHub {
    capacity: usize,
    subscribers: Mutex<Vec<SecuritySubscriber>>,
    dropped_events: AtomicU64,
    next_subscriber_id: AtomicU64,
    // Locked only while `subscribers` is held so ownership moves atomically.
    orphaned_losses: Mutex<Vec<OrphanedLoss>>,
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
            next_subscriber_id: AtomicU64::new(1),
            orphaned_losses: Mutex::new(Vec::new()),
            last_event: Mutex::new(None),
        }
    }

    /// Registers an independent normalized security-event subscriber.
    #[cfg(test)]
    pub(crate) fn subscribe(&self) -> Receiver<SecurityEvent> {
        self.subscribe_tracked().1
    }

    /// Registers a subscriber whose identity attributes its own later delivery losses.
    pub(crate) fn subscribe_tracked(&self) -> (SecuritySubscriberId, Receiver<SecurityEvent>) {
        let (sender, receiver) = mpsc::sync_channel(self.capacity);
        let mut subscribers = self.subscribers();
        // Allocate under the subscriber lock so ID order is also registration order.
        let id = SecuritySubscriberId(self.next_subscriber_id.fetch_add(1, Ordering::Relaxed));
        subscribers.push(SecuritySubscriber {
            id,
            sender,
            dropped_events: 0,
            reported_dropped_events: 0,
        });
        self.assign_orphaned_losses(&mut subscribers);
        drop(subscribers);
        self.publish_pending_loss();
        (id, receiver)
    }

    /// Removes a remote subscriber before its receiver is drained, closing the publish race.
    pub(crate) fn unsubscribe(&self, id: SecuritySubscriberId) -> u64 {
        let mut subscribers = self.subscribers();
        let Some(index) = subscribers
            .iter()
            .position(|subscriber| subscriber.id == id)
        else {
            return 0;
        };
        subscribers.remove(index).unreported_losses()
    }

    /// Publishes without allowing evidence delivery to block policy decisions.
    // ActPlane publishes only after its raw provenance has been normalized.
    #[cfg_attr(not(feature = "mock-backend"), allow(dead_code))]
    pub(crate) fn publish(&self, event: SecurityEvent) {
        self.publish_pending_loss();
        *self.last_event() = Some(event.clone());
        let mut subscribers = self.subscribers();
        if subscribers.is_empty() {
            // Any future subscriber may disclose evidence lost while nobody listened.
            let departed = self.latest_subscriber_id();
            self.queue_orphaned_loss(OrphanedLoss { departed, count: 1 });
            drop(subscribers);
            self.record_delivery_loss(1);
            return;
        }
        let mut dropped_deliveries = 0;
        let mut departed = Vec::new();
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
                    departed.push(OrphanedLoss {
                        departed: subscriber.id,
                        count: subscriber.unreported_losses().saturating_add(1),
                    });
                    false
                }
            },
        );
        self.orphan_losses(&mut subscribers, departed);
        drop(subscribers);
        self.record_delivery_loss(dropped_deliveries);
    }

    /// Records events a subscriber pulled but could not deliver to its remote peer.
    ///
    /// The loss belongs to the departing subscription, so only a newer replacement may
    /// disclose it; an older peer never inherits a gap it did not experience.
    pub(crate) fn record_orphaned_delivery_loss(
        &self,
        subscriber: SecuritySubscriberId,
        represented_losses: u64,
        dropped_frames: u64,
    ) {
        if represented_losses > 0 {
            let mut subscribers = self.subscribers();
            self.orphan_losses(
                &mut subscribers,
                vec![OrphanedLoss {
                    departed: subscriber,
                    count: represented_losses,
                }],
            );
        }
        self.record_delivery_loss(dropped_frames);
    }

    /// Losses not yet disclosed to any live subscriber as an evidence-loss event.
    fn unreported_losses(&self) -> u64 {
        let subscribers = self.subscribers();
        let pending: u64 = subscribers
            .iter()
            .map(SecuritySubscriber::unreported_losses)
            .sum();
        let orphaned: u64 = self.orphaned().iter().map(|loss| loss.count).sum();
        pending.saturating_add(orphaned)
    }

    fn publish_pending_loss(&self) {
        let Some(last_event) = self.last_event().clone() else {
            return;
        };
        let mut subscribers = self.subscribers();
        self.assign_orphaned_losses(&mut subscribers);
        let mut departed = Vec::new();
        let mut dropped_recovery_frames = 0;
        subscribers.retain_mut(|subscriber| {
            if subscriber.dropped_events == subscriber.reported_dropped_events {
                return true;
            }
            let recovery = evidence_loss_event(&last_event, subscriber.unreported_losses());
            match subscriber.sender.try_send(recovery) {
                Ok(()) => {
                    subscriber.reported_dropped_events = subscriber.dropped_events;
                    true
                }
                Err(TrySendError::Full(_)) => true,
                Err(TrySendError::Disconnected(_)) => {
                    dropped_recovery_frames += 1;
                    departed.push(OrphanedLoss {
                        departed: subscriber.id,
                        count: subscriber.unreported_losses(),
                    });
                    false
                }
            }
        });
        self.orphan_losses(&mut subscribers, departed);
        self.record_delivery_loss(dropped_recovery_frames);
    }

    /// Queues losses of departed subscriptions and hands them to eligible replacements.
    fn orphan_losses(&self, subscribers: &mut [SecuritySubscriber], losses: Vec<OrphanedLoss>) {
        for loss in losses.into_iter().filter(|loss| loss.count > 0) {
            self.queue_orphaned_loss(loss);
        }
        self.assign_orphaned_losses(subscribers);
    }

    fn queue_orphaned_loss(&self, loss: OrphanedLoss) {
        let mut orphaned = self.orphaned();
        if let Some(existing) = orphaned
            .iter_mut()
            .find(|existing| existing.departed == loss.departed)
        {
            existing.count = existing.count.saturating_add(loss.count);
        } else {
            orphaned.push(loss);
        }
    }

    /// Moves each orphaned loss to every live subscription created after the departed one.
    ///
    /// Without a client identity the replacement cannot be told apart from another newer
    /// consumer, so every newer subscription discloses the gap: a conservative extra marker is
    /// preferable to an audit trail that silently omits lost evidence.
    fn assign_orphaned_losses(&self, subscribers: &mut [SecuritySubscriber]) {
        self.orphaned().retain(|loss| {
            let mut assigned = false;
            for subscriber in subscribers.iter_mut().filter(|s| s.id > loss.departed) {
                subscriber.dropped_events = subscriber.dropped_events.saturating_add(loss.count);
                assigned = true;
            }
            !assigned
        });
    }

    fn latest_subscriber_id(&self) -> SecuritySubscriberId {
        SecuritySubscriberId(
            self.next_subscriber_id
                .load(Ordering::Relaxed)
                .saturating_sub(1),
        )
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

    /// Marks backend health degraded while normalized evidence loss remains undisclosed.
    ///
    /// The cumulative count stays sticky, but once every loss has been queued to a live
    /// subscriber as an `evidence_loss` event the gap is part of that subscriber's stream, so a
    /// collector restart no longer blocks enforcement readiness indefinitely.
    pub(crate) fn reflect_delivery_loss(&self, mut health: HealthStatus) -> HealthStatus {
        let dropped_events = self.dropped_events.load(Ordering::Relaxed);
        if dropped_events == 0 {
            return health;
        }
        // Health is polled continuously, so retry pending disclosures here instead of waiting
        // for the next security event, which may never come once the Agent goes idle.
        self.publish_pending_loss();
        if self.unreported_losses() == 0 {
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

    fn orphaned(&self) -> MutexGuard<'_, Vec<OrphanedLoss>> {
        self.orphaned_losses
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
    fn repeated_events_without_subscribers_share_one_orphan_bucket() {
        let hub = SecurityEventHub::new(1);

        for _ in 0..1_000 {
            hub.publish(security_event());
        }

        let orphaned = hub.orphaned();
        assert_eq!(orphaned.len(), 1);
        assert_eq!(orphaned[0].count, 1_000);
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

    fn healthy() -> HealthStatus {
        HealthStatus {
            ready: true,
            backend: "test".into(),
            capabilities:
                agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
            message: None,
        }
    }

    #[test]
    fn disclosed_security_loss_no_longer_blocks_readiness() {
        let hub = SecurityEventHub::new(4);
        hub.publish(security_event());
        assert!(!hub.reflect_delivery_loss(healthy()).ready);

        let subscriber = hub.subscribe();
        let recovered = subscriber
            .try_recv()
            .expect("recovery must disclose lost evidence");
        let SecurityEventKind::EnforcementState(state) = recovered.kind else {
            panic!("recovery frame must be enforcement state");
        };
        assert_eq!(state.code, "evidence_loss");

        assert!(hub.reflect_delivery_loss(healthy()).ready);
        assert_eq!(hub.dropped_events.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn health_poll_discloses_overflow_after_the_subscriber_drains() {
        let hub = SecurityEventHub::new(1);
        let subscriber = hub.subscribe();
        hub.publish(security_event());
        hub.publish(security_event());
        assert!(!hub.reflect_delivery_loss(healthy()).ready);

        subscriber
            .try_recv()
            .expect("subscriber should drain the queued event");
        assert!(hub.reflect_delivery_loss(healthy()).ready);
        let recovered = subscriber
            .try_recv()
            .expect("health poll must deliver the pending loss report");
        let SecurityEventKind::EnforcementState(state) = recovered.kind else {
            panic!("recovery frame must be enforcement state");
        };
        assert_eq!(state.code, "evidence_loss");
        assert_eq!(state.dropped_events, Some(1));
    }

    #[test]
    fn recovery_markers_report_only_new_loss_since_the_last_marker() {
        let hub = SecurityEventHub::new(1);
        let subscriber = hub.subscribe();

        for expected_total in 1..=2 {
            hub.publish(security_event());
            hub.publish(security_event());
            subscriber
                .try_recv()
                .expect("subscriber should drain the delivered event");

            assert!(hub.reflect_delivery_loss(healthy()).ready);
            let recovered = subscriber
                .try_recv()
                .expect("health poll must deliver the new loss");
            let SecurityEventKind::EnforcementState(state) = recovered.kind else {
                panic!("recovery frame must be enforcement state");
            };
            assert_eq!(state.dropped_events, Some(1));
            assert_eq!(hub.dropped_events.load(Ordering::Relaxed), expected_total);
        }
    }

    #[test]
    fn departed_subscriber_loss_is_never_inherited_by_an_older_peer() {
        let hub = SecurityEventHub::new(4);
        let older = hub.subscribe();
        let gone = hub.subscribe();
        drop(gone);

        hub.publish(security_event());
        older
            .try_recv()
            .expect("older subscriber should receive the event");

        assert!(!hub.reflect_delivery_loss(healthy()).ready);
        assert!(older.try_recv().is_err());

        let replacement = hub.subscribe();
        let recovered = replacement
            .try_recv()
            .expect("replacement must disclose the departed subscriber's loss");
        let SecurityEventKind::EnforcementState(state) = recovered.kind else {
            panic!("recovery frame must be enforcement state");
        };
        assert_eq!(state.code, "evidence_loss");
        assert_eq!(state.dropped_events, Some(1));
        assert!(hub.reflect_delivery_loss(healthy()).ready);
        assert!(older.try_recv().is_err());
    }

    #[test]
    fn socket_write_loss_detected_after_replacement_reaches_the_replacement() {
        let hub = SecurityEventHub::new(4);
        let older = hub.subscribe();
        let (failing, failing_receiver) = hub.subscribe_tracked();
        hub.publish(security_event());
        older
            .try_recv()
            .expect("older subscriber should receive the event");
        failing_receiver
            .try_recv()
            .expect("failing subscriber should receive the event");
        let replacement = hub.subscribe();

        drop(failing_receiver);
        let pending = hub.unsubscribe(failing);
        hub.record_orphaned_delivery_loss(failing, pending.saturating_add(2), 1);

        assert!(hub.reflect_delivery_loss(healthy()).ready);
        let recovered = replacement
            .try_recv()
            .expect("replacement must receive the socket write loss");
        let SecurityEventKind::EnforcementState(state) = recovered.kind else {
            panic!("recovery frame must be enforcement state");
        };
        assert_eq!(state.dropped_events, Some(2));
        assert!(older.try_recv().is_err());
    }

    #[test]
    fn write_loss_is_not_attributed_to_the_failing_subscriber_itself() {
        let hub = SecurityEventHub::new(4);
        let (failing, failing_receiver) = hub.subscribe_tracked();
        hub.publish(security_event());
        failing_receiver
            .try_recv()
            .expect("failing subscriber should receive the event");

        let pending = hub.unsubscribe(failing);
        hub.record_orphaned_delivery_loss(failing, pending.saturating_add(1), 1);

        assert!(!hub.reflect_delivery_loss(healthy()).ready);
        assert!(failing_receiver.try_recv().is_err());
    }
}
