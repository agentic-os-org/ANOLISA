//! Correlate per-step native callbacks without extending or replaying an event.

use super::{event_metadata, Runtime};
use crate::{rejected, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::{atomic::Ordering, Arc, TryLockError},
    thread,
    time::{Duration, Instant},
};

const MAX_HOOK_EVENTS: usize = 1024;

pub(super) struct Lease {
    digest: [u8; 32],
    handle: Option<Value>,
}

impl Runtime {
    pub(super) fn release_instance(&self, instance_id: &str, deadline: Instant) -> Result<Value> {
        {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get_mut(instance_id)
                .and_then(Option::as_mut)
                .ok_or_else(|| rejected("unknown_instance"))?;
            instance.closing = true;
        }
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
            if !active {
                self.instances
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .remove(instance_id);
                if !self.audit.healthy() {
                    return Err(rejected("audit_unavailable"));
                }
                return Ok(json!({"released": true}));
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
        value: Value,
        native_input: Vec<u8>,
        deadline: Instant,
    ) -> Result<Value> {
        event_metadata(&value)?;
        let session = value["session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| rejected("native_correlation_missing"))?;
        let call = value["tool"]["call_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| rejected("native_correlation_missing"))?;
        let encoded = serde_json::to_vec(&value).map_err(|_| rejected("event_encoding"))?;
        if native_input.len() > aw_provider::MAX_MESSAGE_BYTES
            || encoded.len() > aw_provider::MAX_MESSAGE_BYTES
        {
            return Err(rejected("event_size_limit"));
        }
        let correlation = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!([value["name"], session, call]))
                    .map_err(|_| rejected("event_encoding"))?
            )
        );
        let mut digest = Sha256::new();
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
        digest.update(&native_input);
        let digest: [u8; 32] = digest.finalize().into();
        let hooks = {
            let instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            Arc::clone(
                &instances
                    .get(&instance_id)
                    .and_then(Option::as_ref)
                    .filter(|instance| !instance.closing)
                    .ok_or_else(|| rejected("unknown_instance"))?
                    .hooks,
            )
        };
        // Serialize only opens for this instance, including initial audit claim.
        // Event workers do not acquire this lock, so preparation cannot deadlock.
        let mut hooks = loop {
            if Instant::now() >= deadline {
                return Err(rejected("event_open_timeout"));
            }
            match hooks.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(_)) => return Err(rejected("state_poisoned")),
                Err(TryLockError::WouldBlock) => thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(2)),
                ),
            }
        };
        if let Some(lease) = hooks.get(&correlation) {
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
            return Ok(handle.clone());
        }
        if hooks.len() >= MAX_HOOK_EVENTS {
            return Err(rejected("native_event_limit"));
        }
        // Retain failed opens too: retrying must not create a fresh event budget.
        hooks.insert(
            correlation.clone(),
            Lease {
                digest,
                handle: None,
            },
        );
        let handle = self.open(instance_id, value, Some(native_input), deadline)?;
        hooks.insert(
            correlation,
            Lease {
                digest,
                handle: Some(handle.clone()),
            },
        );
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
                host: Arc::new(host),
                closing: false,
                hooks: Arc::new(Mutex::new(BTreeMap::new())),
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
                Instant::now() + Duration::from_secs(1)
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
