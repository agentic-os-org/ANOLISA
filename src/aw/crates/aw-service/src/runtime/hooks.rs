//! Correlate per-step native callbacks without extending or replaying an event.

use super::{event_metadata, Job, Runtime};
use crate::{rejected, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, MutexGuard, TryLockError,
    },
    thread,
    time::{Duration, Instant},
};

const MAX_HOOK_EVENTS: usize = 1024;

pub(super) struct Lease {
    digest: [u8; 32],
    handle: Option<Value>,
    completion_digest: Option<[u8; 32]>,
    cancelled: Arc<AtomicBool>,
}

pub(super) fn correlation(event: &str, session: &str, call: &str) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([event, session, call]))
                .map_err(|_| rejected("event_encoding"))?
        )
    ))
}

pub(super) fn lock_until<'a, T>(
    hooks: &'a Mutex<T>,
    deadline: Instant,
    timeout: &str,
) -> Result<MutexGuard<'a, T>> {
    loop {
        if Instant::now() >= deadline {
            return Err(rejected(timeout));
        }
        match hooks.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => return Err(rejected("state_poisoned")),
            Err(TryLockError::WouldBlock) => thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(2)),
            ),
        }
    }
}

impl Runtime {
    pub(super) fn close_hook(
        &self,
        instance_id: &str,
        event: &str,
        session: &str,
        call: &str,
        deadline: Instant,
    ) -> Result<Value> {
        if !matches!(event, "tool.before" | "tool.after") || session.is_empty() || call.is_empty() {
            return Err(rejected("native_correlation_missing"));
        }
        let (hooks, invocations) = {
            let instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get(instance_id)
                .and_then(Option::as_ref)
                .ok_or_else(|| rejected("unknown_instance"))?;
            (
                Arc::clone(&instance.hooks),
                Arc::clone(&instance.invocations),
            )
        };
        // Keep the ledger guard until the cancellation claim is installed. A
        // concurrent retirement must not allow a late close to recreate state.
        let mut state = invocations.lock().map_err(|_| rejected("state_poisoned"))?;
        if let Some(ledger) = state.ledger.as_ref() {
            ledger.check_close(session, call)?;
        } else {
            state.legacy_used = true;
        }
        let key = correlation(event, session, call)?;
        let cancelled = {
            // The worker shares this flag even before its handle is ready.
            let mut hooks = lock_until(&hooks, deadline, "event_open_timeout")?;
            if state.ledger.is_none() && !hooks.contains_key(&key) && hooks.len() >= MAX_HOOK_EVENTS
            {
                return Err(rejected("native_event_limit"));
            }
            let lease = hooks.entry(key).or_insert(Lease {
                digest: [0; 32],
                handle: None,
                completion_digest: None,
                cancelled: Arc::new(AtomicBool::new(true)),
            });
            lease.cancelled.store(true, Ordering::Release);
            Arc::clone(&lease.cancelled)
        };
        drop(state);
        loop {
            {
                let events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
                if !events
                    .values()
                    .any(|entry| Arc::ptr_eq(&entry.cancelled, &cancelled))
                {
                    break;
                }
            }
            if Instant::now() >= deadline {
                return Err(rejected("event_close_timeout"));
            }
            thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5)),
            );
        }
        Ok(json!({"closed": true}))
    }

    pub(super) fn release_instance(&self, instance_id: &str, deadline: Instant) -> Result<Value> {
        let release = Arc::clone(
            &self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .get(instance_id)
                .and_then(Option::as_ref)
                .ok_or_else(|| rejected("unknown_instance"))?
                .release,
        );
        let _release = lock_until(&release, deadline, "instance_release_timeout")?;
        let update = {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let Some(instance) = instances.get_mut(instance_id).and_then(Option::as_mut) else {
                // A concurrent close acknowledged this same live binding first.
                return Ok(json!({"released": true}));
            };
            instance.closing = true;
            if let Some(cancelled) = instance
                .reload
                .as_ref()
                .and_then(|reload| reload.preparation_cancelled.as_ref())
            {
                cancelled.store(true, Ordering::Release);
            }
            Arc::clone(&instance.policy_update)
        };
        self.drain_scope_children(instance_id, deadline)?;
        loop {
            let active = {
                let events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
                let mut active = false;
                for entry in events
                    .values()
                    .filter(|entry| entry.instance_id == instance_id)
                {
                    entry.cancelled.store(true, Ordering::Release);
                    active = true;
                }
                active
            };
            let preparing = match update.try_lock() {
                Ok(_) => false,
                Err(TryLockError::WouldBlock) => true,
                Err(TryLockError::Poisoned(_)) => return Err(rejected("state_poisoned")),
            };
            if !active && !preparing && self.finish_observation(instance_id)? {
                self.release_source_sessions(instance_id)?;
                let observation = self.observation_metadata(instance_id)?;
                self.instances
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .remove(instance_id);
                self.owners
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .remove(instance_id);
                if !self.audit.healthy() {
                    return Err(rejected("audit_unavailable"));
                }
                return Ok(json!({"released": true, "observation":observation}));
            }
            if Instant::now() >= deadline {
                return Err(rejected("instance_release_timeout"));
            }
            thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5)),
            );
        }
    }

    pub(super) fn open_hook(
        self: &Arc<Self>,
        instance_id: String,
        mut value: Value,
        native_input: Vec<u8>,
        deadline: Instant,
    ) -> Result<Value> {
        event_metadata(&value)?;
        let session = value["session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| rejected("native_correlation_missing"))?
            .to_owned();
        let call = value["tool"]["call_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| rejected("native_correlation_missing"))?
            .to_owned();
        let (hooks, hook_open, invocations, replacement_after) = {
            let instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get(&instance_id)
                .and_then(Option::as_ref)
                .filter(|instance| !instance.closing)
                .ok_or_else(|| rejected("unknown_instance"))?;
            (
                Arc::clone(&instance.hooks),
                Arc::clone(&instance.hook_open),
                Arc::clone(&instance.invocations),
                instance
                    .host
                    .steps()
                    .iter()
                    .any(|step| step.event == "tool.after" && step.execution.replaces_result()),
            )
        };
        if native_input.len() > aw_provider::MAX_MESSAGE_BYTES {
            return Err(rejected("event_size_limit"));
        }
        let aw_adapter::completion::Comparison {
            value: comparison,
            input: compared_input,
            observed,
            completion_digest,
        } = aw_adapter::completion::normalize(&mut value, &native_input, replacement_after)
            .map_err(|error| rejected(error.code()))?;
        let encoded = serde_json::to_vec(&comparison).map_err(|_| rejected("event_encoding"))?;
        if native_input.len() > aw_provider::MAX_MESSAGE_BYTES
            || compared_input.len() > aw_provider::MAX_MESSAGE_BYTES
            || encoded.len() > aw_provider::MAX_MESSAGE_BYTES
        {
            return Err(rejected("event_size_limit"));
        }
        let correlation = correlation(
            value["name"]
                .as_str()
                .ok_or_else(|| rejected("event_invalid"))?,
            &session,
            &call,
        )?;
        let mut digest = Sha256::new();
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
        digest.update(&compared_input);
        let digest: [u8; 32] = digest.finalize().into();
        // Preserve shared-handle opens without excluding cancellation during I/O.
        let _opening = lock_until(&hook_open, deadline, "event_open_timeout")?;
        // Select a protocol under the same guard as EnableInvocations.
        let mut state = invocations.lock().map_err(|_| rejected("state_poisoned"))?;
        let managed = if let Some(ledger) = state.ledger.as_mut() {
            ledger.check(&value)?;
            true
        } else {
            state.legacy_used = true;
            false
        };
        drop(state);
        let mut leases = lock_until(&hooks, deadline, "event_open_timeout")?;
        if let Some(lease) = leases.get_mut(&correlation) {
            if lease.cancelled.load(Ordering::Acquire) {
                return Err(rejected("native_event_closed"));
            }
            if lease.digest != digest {
                return Err(rejected("native_correlation_mismatch"));
            }
            let handle = lease
                .handle
                .as_ref()
                .ok_or_else(|| rejected("native_event_closed"))?;
            let key = handle["event_id"]
                .as_str()
                .ok_or_else(|| rejected("event_invalid"))?;
            let (_, cancelled) = self
                .event_sender(key, &instance_id)
                .map_err(|_| rejected("native_event_closed"))?;
            if cancelled.load(Ordering::Acquire) {
                return Err(rejected("native_event_closed"));
            }
            if observed {
                if lease.completion_digest.is_some() && lease.completion_digest != completion_digest
                {
                    return Err(rejected("native_correlation_mismatch"));
                }
                if lease.completion_digest.is_none() {
                    let (sender, _) = self.event_sender(key, &instance_id)?;
                    let (reply, result) = mpsc::sync_channel(1);
                    sender
                        .try_send(Job::Completion {
                            value,
                            input: native_input,
                            reply,
                        })
                        .map_err(|_| rejected("event_busy_or_closed"))?;
                    result
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .map_err(|_| rejected("event_reply_unavailable"))??;
                    lease.completion_digest = completion_digest;
                }
            }
            return Ok(handle.clone());
        }
        if observed {
            return Err(rejected("native_completion_without_replacement"));
        }
        if !managed && leases.len() >= MAX_HOOK_EVENTS {
            return Err(rejected("native_event_limit"));
        }
        // Retain failed opens too: retrying must not create a fresh event budget.
        let cancelled = Arc::new(AtomicBool::new(false));
        leases.insert(
            correlation.clone(),
            Lease {
                digest,
                handle: None,
                completion_digest: None,
                cancelled: Arc::clone(&cancelled),
            },
        );
        // Initialization may wait on durable storage. Keep cancellation writable.
        drop(leases);
        let handle = self.open(instance_id, value, Some(native_input), deadline, cancelled)?;
        let mut leases = lock_until(&hooks, deadline, "event_open_timeout")?;
        let lease = leases
            .get_mut(&correlation)
            .ok_or_else(|| rejected("native_event_closed"))?;
        if lease.cancelled.load(Ordering::Acquire) {
            return Err(rejected("native_event_closed"));
        }
        lease.handle = Some(handle.clone());
        Ok(handle)
    }
}

#[cfg(test)]
mod tests;
