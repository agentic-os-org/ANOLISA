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

fn correlation(event: &str, session: &str, call: &str) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([event, session, call]))
                .map_err(|_| rejected("event_encoding"))?
        )
    ))
}

fn lock_hooks<T>(hooks: &Mutex<T>, deadline: Instant) -> Result<MutexGuard<'_, T>> {
    loop {
        if Instant::now() >= deadline {
            return Err(rejected("event_open_timeout"));
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
        let hooks = Arc::clone(
            &self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .get(instance_id)
                .and_then(Option::as_ref)
                .ok_or_else(|| rejected("unknown_instance"))?
                .hooks,
        );
        let key = correlation(event, session, call)?;
        let cancelled = {
            // The worker shares this flag even before its handle is ready.
            let mut hooks = lock_hooks(&hooks, deadline)?;
            if !hooks.contains_key(&key) && hooks.len() >= MAX_HOOK_EVENTS {
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
        let update = {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get_mut(instance_id)
                .and_then(Option::as_mut)
                .ok_or_else(|| rejected("unknown_instance"))?;
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
        let (hooks, hook_open, replacement_after) = {
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
        let _opening = lock_hooks(&hook_open, deadline)?;
        let mut leases = lock_hooks(&hooks, deadline)?;
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
        if leases.len() >= MAX_HOOK_EVENTS {
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
        let mut leases = lock_hooks(&hooks, deadline)?;
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
mod tests {
    use super::*;

    use crate::runtime::{EventEntry, Instance};
    use aw_host::{Host, ProcessContext};
    use aw_provider::admission::AdapterCapabilities;
    use std::{
        collections::BTreeMap,
        fs,
        sync::{
            atomic::{AtomicBool, AtomicU64},
            mpsc, Mutex,
        },
    };

    #[test]
    fn release_timeout_keeps_instance_draining_until_cleanup_can_finish() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/service-release-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(directory.parent().unwrap()).unwrap();
        fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let bytes = include_bytes!("../../../aw-config/examples/aw.minimal.yaml");
        let stopped = Arc::new(AtomicBool::new(false));
        let runtime = Runtime::new(bytes.to_vec(), &directory.join("audit"), stopped).unwrap();
        let host = Host::prepare(
            bytes,
            "qoder",
            AdapterCapabilities {
                adapter: "qoder".into(),
                version: "fixture".into(),
                entrypoint: "fixture".into(),
                events: BTreeMap::from([
                    ("tool.before".into(), vec!["observe".into(), "block".into()]),
                    ("tool.after".into(), vec!["observe".into()]),
                ]),
            },
            ProcessContext {
                cwd: directory.clone(),
                environment: BTreeMap::new(),
                stderr_bytes: 1024,
            },
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap();
        runtime.instances.lock().unwrap().insert(
            "fixture".into(),
            Some(Instance {
                bytes: bytes.to_vec(),
                context: ProcessContext {
                    cwd: directory.clone(),
                    environment: BTreeMap::new(),
                    stderr_bytes: 1024,
                },
                reload: None,
                policy_update: Arc::new(Mutex::new(())),
                host: Arc::new(host),
                closing: false,
                observation: None,
                hooks: Arc::new(Mutex::new(BTreeMap::new())),
                hook_open: Arc::new(Mutex::new(())),
            }),
        );
        let hooks = Arc::clone(
            &runtime.instances.lock().unwrap()["fixture"]
                .as_ref()
                .unwrap()
                .hooks,
        );
        let guard = hooks.lock().unwrap();
        let locked = runtime.open_hook(
            "fixture".into(),
            json!({
                "name": "tool.before", "session_id": "session", "tool": {"call_id": "call"}
            }),
            vec![],
            Instant::now() + Duration::from_millis(10),
        );
        assert!(
            matches!(locked, Err(crate::Error::Rejected(code)) if code == "event_open_timeout")
        );
        drop(guard);
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, _receiver) = mpsc::sync_channel(1);
        runtime.events.lock().unwrap().insert(
            "busy".into(),
            EventEntry {
                instance_id: "fixture".into(),
                sender,
                cancelled: Arc::clone(&cancelled),
                context: Arc::new(super::super::evidence::Cell::default()),
            },
        );
        assert!(
            matches!(runtime.release_instance("fixture", Instant::now() + Duration::from_millis(10)),
            Err(crate::Error::Rejected(code)) if code == "instance_release_timeout")
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            runtime.instances.lock().unwrap()["fixture"]
                .as_ref()
                .unwrap()
                .closing
        );
        assert!(runtime
            .open(
                "fixture".into(),
                json!({}),
                None,
                Instant::now() + Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            )
            .is_err());
        runtime.events.lock().unwrap().remove("busy");
        assert_eq!(
            runtime
                .release_instance("fixture", Instant::now() + Duration::from_secs(1))
                .unwrap()["released"],
            true
        );
        assert!(runtime.instances.lock().unwrap().is_empty());
    }
}
