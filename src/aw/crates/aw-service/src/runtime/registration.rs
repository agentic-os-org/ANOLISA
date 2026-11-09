//! Recoverable independent preparation reuses the normal Host and scope lifetime.

use super::{process::Process, scopes::Owner, Runtime};
use crate::{rejected, Error, RegistrationCommand, RegistrationHandle, RegistrationKey, Result};
use aw_host::ProcessContext;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

mod peer;
#[cfg(test)]
mod tests;

const CONFIRMATION_WINDOW: Duration = Duration::from_secs(30);
// Retain replay fences for the whole generation. Saturation rejects new keys;
// eviction could silently re-run preparation after a lost response.
const MAX_REGISTRATIONS: usize = 4096;

pub(super) struct Record {
    id: String,
    fingerprint: String,
    process: Process,
    cancellation: Arc<AtomicBool>,
    snapshot: Mutex<Value>,
}

impl Runtime {
    pub(crate) fn registration(
        &self,
        command: RegistrationCommand,
        peer: &Process,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        if Instant::now() >= deadline {
            return Err(rejected("deadline_exceeded"));
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err(rejected("service_stopping"));
        }
        match command {
            command @ (RegistrationCommand::Register { .. }
            | RegistrationCommand::RegisterNative { .. }) => {
                self.register(command, peer, deadline, cancelled)
            }
            RegistrationCommand::Query { key } => {
                let record = self.registration_record(&key, peer)?;
                self.registration_snapshot(&record)
            }
            RegistrationCommand::Ready {
                handle,
                policy_revision,
            } => {
                let record = self.registration_handle(&handle, peer)?;
                let instances = self
                    .instances
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?;
                let instance = instances
                    .get(&record.id)
                    .and_then(Option::as_ref)
                    .filter(|instance| !instance.closing)
                    .ok_or_else(|| rejected("registration_unavailable"))?;
                let mut owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
                let owner = owners
                    .get_mut(&record.id)
                    .ok_or_else(|| rejected("registration_unavailable"))?;
                owner.process.caller(peer)?;
                if owner.cancelled.load(Ordering::Acquire)
                    || owner.acknowledge_by.is_some_and(|at| Instant::now() >= at)
                {
                    return Err(rejected("registration_confirmation_expired"));
                }
                if policy_revision != instance.host.revision() {
                    return Err(rejected("registration_policy_mismatch"));
                }
                if owner.acknowledge_by.is_some() {
                    self.lifecycle_record(
                        "registration_ready",
                        &record.id,
                        json!({"policy_revision":policy_revision,"process":owner.process.metadata(),
                            "evidence":"adapter_acknowledgement"}),
                    )?;
                    owner.acknowledge_by = None;
                    record
                        .snapshot
                        .lock()
                        .map_err(|_| rejected("state_poisoned"))?["state"] = json!("ready");
                }
                drop(owners);
                drop(instances);
                self.registration_snapshot(&record)
            }
            RegistrationCommand::Release { handle } => {
                let record = self.registration_handle(&handle, peer)?;
                record.cancellation.store(true, Ordering::Release);
                record
                    .snapshot
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?["state"] = json!("closing");
                match self.release_instance(&record.id, deadline) {
                    Err(Error::Rejected(code)) if code == "unknown_instance" => {}
                    result => {
                        result?;
                    }
                }
                record
                    .snapshot
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?["state"] = json!("released");
                self.registration_snapshot(&record)
            }
        }
    }

    fn registration_key(key: &RegistrationKey) -> Result<String> {
        if key.request_id.is_empty()
            || key.request_id.len() > 128
            || !key
                .request_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        {
            return Err(rejected("registration_request_id"));
        }
        Ok(format!(
            "{}:{}:{}",
            key.owner.pid, key.owner.start_ticks, key.request_id
        ))
    }

    fn registration_record(&self, key: &RegistrationKey, peer: &Process) -> Result<Arc<Record>> {
        let process = Process::read(key.owner.pid)?;
        process.registration_owner(&key.owner)?;
        process.caller(peer)?;
        let records = self
            .registrations
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let record = records
            .get(&Self::registration_key(key)?)
            .ok_or_else(|| rejected("registration_unknown"))?;
        record.process.registration_owner(&key.owner)?;
        Ok(Arc::clone(record))
    }

    fn registration_handle(
        &self,
        handle: &RegistrationHandle,
        peer: &Process,
    ) -> Result<Arc<Record>> {
        if handle.binding.identity != self.identity {
            return Err(rejected("stale_service"));
        }
        let record = self.registration_record(&handle.key, peer)?;
        let snapshot = record
            .snapshot
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        if snapshot["handle"] != json!(handle) {
            return Err(rejected("registration_handle_mismatch"));
        }
        drop(snapshot);
        Ok(record)
    }

    fn registration_snapshot(&self, record: &Record) -> Result<Value> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        let mut snapshot = record
            .snapshot
            .lock()
            .map_err(|_| rejected("state_poisoned"))?
            .clone();
        if matches!(
            snapshot["state"].as_str(),
            Some("pending_ready" | "ready" | "closing")
        ) {
            match instances.get(&record.id) {
                None => {
                    snapshot["state"] = json!(if snapshot["state"] == "pending_ready" {
                        "expired"
                    } else {
                        "released"
                    })
                }
                Some(Some(instance)) => {
                    if let Some(owner) = owners.get(&record.id) {
                        snapshot["confirmation_remaining_ms"] =
                            json!(owner.acknowledge_by.map(|at| at
                                .saturating_duration_since(Instant::now())
                                .as_millis()
                                as u64));
                        if instance.closing
                            || owner.cancelled.load(Ordering::Acquire)
                            || owner.acknowledge_by.is_some_and(|at| Instant::now() >= at)
                        {
                            snapshot["state"] = json!("closing");
                        }
                    }
                }
                Some(None) => {}
            }
        }
        Ok(snapshot)
    }

    fn register(
        &self,
        command: RegistrationCommand,
        peer: &Process,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&command).map_err(|_| rejected("registration_encoding"))?
            )
        );
        let native = matches!(&command, RegistrationCommand::RegisterNative { .. });
        let (key, target, capabilities, cwd, environment, config) = match command {
            RegistrationCommand::Register {
                key,
                target,
                capabilities,
                cwd,
                environment,
                config,
            }
            | RegistrationCommand::RegisterNative {
                key,
                target,
                capabilities,
                cwd,
                environment,
                config,
            } => (key, target, capabilities, cwd, environment, config),
            _ => return Err(rejected("registration_action")),
        };
        if native && !matches!(capabilities.adapter.as_str(), "openclaw" | "hermes") {
            return Err(rejected("registration_adapter_unsupported"));
        }
        let process = Process::read(key.owner.pid)?;
        process.registration_owner(&key.owner)?;
        process.caller(peer)?;
        let map_key = Self::registration_key(&key)?;
        let record = {
            let mut records = self
                .registrations
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            if let Some(record) = records.get(&map_key) {
                if record.fingerprint != fingerprint {
                    return Err(rejected("registration_request_conflict"));
                }
                let record = Arc::clone(record);
                drop(records);
                return self.registration_snapshot(&record);
            }
            if records.len() >= MAX_REGISTRATIONS {
                return Err(rejected("registration_limit"));
            }
            let record = Arc::new(Record {
                id: self.identifier()?,
                fingerprint,
                process,
                cancellation: Arc::new(AtomicBool::new(false)),
                snapshot: Mutex::new(json!({"state":"preparing","key":key})),
            });
            records.insert(map_key, Arc::clone(&record));
            record
        };
        let result = (|| {
            let bytes = config
                .as_ref()
                .map_or(self.bytes.as_slice(), |value| value.as_bytes());
            let validator = aw_config::Validator::new()?;
            let document = validator.parse(bytes)?;
            validator
                .parse(&self.bytes)?
                .validate_policy_update(&document)?;
            let candidate = self
                .admission
                .preflight(bytes, &target, capabilities.clone().into())
                .map_err(|_| rejected("binding_preflight"))?;
            // Use the ordinary factory's route projection, including native input,
            // Skill and result handling constraints; no restricted reload profile.
            let routes = if native {
                aw_adapter::reload::validate(
                    document.as_value(),
                    &target,
                    &capabilities.clone().into(),
                )
                .map_err(|_| rejected("registration_adapter_unsupported"))?;
                json!({"events": aw_adapter::reload::routes()})
            } else {
                aw_adapter::reload::scope_policy(
                    document.as_value(),
                    &target,
                    candidate.requirements(),
                    &capabilities.clone().into(),
                )
                .map_err(|_| rejected("registration_adapter_unsupported"))?
            };
            if !std::path::Path::new(&cwd).is_absolute() {
                return Err(rejected("registration_cwd"));
            }
            self.owners
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .insert(
                    record.id.clone(),
                    Owner::independent(
                        record.process.clone(),
                        native,
                        Arc::clone(&record.cancellation),
                        deadline,
                    ),
                );
            let finished = AtomicBool::new(false);
            let prepared = thread::scope(|scope| {
                scope.spawn(|| {
                    while !finished.load(Ordering::Acquire) {
                        if cancelled.load(Ordering::Acquire)
                            || self.stopped.load(Ordering::Acquire)
                            || Instant::now() >= deadline
                            || record.process.check().is_err()
                            || peer.check().is_err()
                        {
                            record.cancellation.store(true, Ordering::Release);
                            break;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                });
                let result = self.bind_id(
                    record.id.clone(),
                    bytes,
                    target,
                    capabilities,
                    ProcessContext {
                        cwd: cwd.into(),
                        environment,
                        stderr_bytes: 64 * 1024,
                    },
                    (deadline, &record.cancellation),
                );
                finished.store(true, Ordering::Release);
                result
            })?;
            let binding =
                serde_json::from_value(prepared).map_err(|_| rejected("registration_binding"))?;
            let handle = RegistrationHandle { key, binding };
            let instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get(&record.id)
                .and_then(Option::as_ref)
                .ok_or_else(|| rejected("registration_cancelled"))?;
            let mut owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
            let owner = owners
                .get_mut(&record.id)
                .ok_or_else(|| rejected("registration_cancelled"))?;
            record.process.caller(peer)?;
            if cancelled.load(Ordering::Acquire)
                || record.cancellation.load(Ordering::Acquire)
                || self.stopped.load(Ordering::Acquire)
                || Instant::now() >= deadline
            {
                return Err(rejected("registration_cancelled"));
            }
            self.lifecycle_record(
                "registration_prepared",
                &record.id,
                json!({"process":record.process.metadata(),"request_id":handle.key.request_id}),
            )?;
            owner.acknowledge_by = Some(Instant::now() + CONFIRMATION_WINDOW);
            *record
                .snapshot
                .lock()
                .map_err(|_| rejected("state_poisoned"))? = json!({
                "state":"pending_ready", "handle":handle, "routes":routes,
                "policy_revision":instance.host.revision(),
                "confirmation_timeout_ms":CONFIRMATION_WINDOW.as_millis() as u64
            });
            Ok(())
        })();
        if let Err(error) = result {
            record.cancellation.store(true, Ordering::Release);
            // Failed preparation has already dropped its Host. If completion raced
            // cancellation, retain cleanup ownership until release has drained it.
            let exists = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .contains_key(&record.id);
            if exists {
                let _ = self.release_instance(&record.id, Instant::now() + Duration::from_secs(1));
            } else {
                self.owners
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .remove(&record.id);
            }
            let (code, audit_key) = match &error {
                Error::Rejected(code) => (code.as_str(), None),
                Error::Attempt { code, audit_key } => (code.as_str(), Some(audit_key)),
                _ => ("service_failure", None),
            };
            *record
                .snapshot
                .lock()
                .map_err(|_| rejected("state_poisoned"))? =
                json!({"state":"failed","error":code,"audit_key":audit_key});
            return Err(error);
        }
        self.registration_snapshot(&record)
    }
}
