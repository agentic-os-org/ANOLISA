//! Application-polled control intents never transfer process ownership to the CLI.

use super::{process::Process, Runtime};
use crate::{
    rejected, Availability, RegistrationCommand, RegistrationHandle, RegistrationKey, Result,
    SdkControl, SdkIntent,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, Mutex},
    time::{Duration, Instant},
};

const LIVE_WINDOW: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct Registry(Mutex<BTreeMap<String, Application>>);

struct Application {
    process: Process,
    target: String,
    mode: Availability,
    seen: Instant,
    handle: Option<RegistrationHandle>,
    request: Option<Pending>,
}

struct Pending {
    id: String,
    intent: SdkIntent,
    expires: Instant,
    timeout_ms: u64,
    state: &'static str,
    error: Option<String>,
    handle: Option<RegistrationHandle>,
    previous: Option<RegistrationHandle>,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

impl Pending {
    fn snapshot(&mut self) -> Value {
        if self.state == "pending" && Instant::now() >= self.expires {
            self.state = "expired";
        }
        json!({"request_id":self.id,"intent":self.intent,"state":self.state,
            "remaining_ms":self.expires.saturating_duration_since(Instant::now()).as_millis() as u64,
            "error":self.error,"handle":self.handle})
    }
}

impl Application {
    fn snapshot(&mut self, id: &str) -> Value {
        json!({"application_id":id,"process":self.process.metadata(),"target":self.target,
            "availability":self.mode,"connected":self.handle.is_some(),
            "instance_id":self.handle.as_ref().map(|h| &h.binding.instance_id),
            "request":self.request.as_mut().map(Pending::snapshot)})
    }
}

impl Runtime {
    fn sdk_application_key(&self, key: &RegistrationKey, peer: &Process) -> Result<String> {
        // Direct SDK calls must come from the application itself, not its CLI child.
        peer.registration_owner(&key.owner)?;
        if !valid_id(&key.request_id) {
            return Err(rejected("sdk_control_key"));
        }
        Ok(format!(
            "{:x}",
            Sha256::digest(format!(
                "{}:{}:{}:{}",
                self.identity.generation, key.owner.pid, key.owner.start_ticks, key.request_id
            ))
        ))
    }

    fn sdk_ready_handle(
        &self,
        handle: &RegistrationHandle,
        peer: &Process,
        deadline: Instant,
    ) -> Result<Value> {
        let snapshot = self.registration(
            RegistrationCommand::Query {
                key: handle.key.clone(),
            },
            peer,
            deadline,
            &AtomicBool::new(false),
        )?;
        peer.registration_owner(&handle.key.owner)?;
        if snapshot["handle"] != json!(handle)
            || snapshot["state"] != "ready"
            || snapshot["sdk_protocol"] != crate::SDK_VERSION
        {
            return Err(rejected("sdk_control_not_ready"));
        }
        Ok(snapshot["availability"].clone())
    }

    pub(crate) fn sdk_control(
        &self,
        command: SdkControl,
        peer: &Process,
        deadline: Instant,
    ) -> Result<Value> {
        peer.check()?;
        if Instant::now() >= deadline {
            return Err(rejected("deadline_exceeded"));
        }
        let candidate = match &command {
            SdkControl::Poll {
                handle: Some(handle),
                ..
            }
            | SdkControl::Acknowledge {
                handle: Some(handle),
                error: None,
                ..
            } => Some((
                handle.clone(),
                self.sdk_ready_handle(handle, peer, deadline)?,
            )),
            _ => None,
        };
        let owner = match &command {
            SdkControl::Poll { key, .. }
            | SdkControl::Withdraw { key }
            | SdkControl::Acknowledge { key, .. } => Some(self.sdk_application_key(key, peer)?),
            _ => None,
        };
        let mut apps = self
            .sdk_controls
            .0
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        apps.retain(|_, app| {
            app.process.check().is_ok()
                && (app.seen.elapsed() < LIVE_WINDOW
                    || app.request.as_ref().is_some_and(|request| {
                        request.state == "pending" && Instant::now() < request.expires
                    }))
        });
        if let Some((handle, _)) = &candidate {
            if apps.iter().any(|(id, app)| {
                Some(id) != owner.as_ref()
                    && app.handle.as_ref().is_some_and(|current| {
                        current.binding.instance_id == handle.binding.instance_id
                    })
            }) {
                return Err(rejected("sdk_control_registration_conflict"));
            }
        }
        match command {
            SdkControl::Poll {
                key: _,
                target,
                availability,
                handle,
            } => {
                let id = owner.ok_or_else(|| rejected("sdk_control_owner"))?;
                if target.is_empty() || target.len() > 128 {
                    return Err(rejected("sdk_control_target"));
                }
                if let Some((candidate, mode)) = candidate {
                    if candidate.binding.target != target || mode != json!(availability) {
                        return Err(rejected("sdk_control_registration_conflict"));
                    }
                }
                if !apps.contains_key(&id) {
                    if apps.len() >= self.limits.bindings {
                        return Err(rejected("sdk_application_limit"));
                    }
                    apps.insert(
                        id.clone(),
                        Application {
                            process: peer.clone(),
                            target: target.clone(),
                            mode: availability,
                            seen: Instant::now(),
                            handle: None,
                            request: None,
                        },
                    );
                }
                let app = apps
                    .get_mut(&id)
                    .ok_or_else(|| rejected("sdk_application_unavailable"))?;
                if app.target != target || app.mode != availability {
                    return Err(rejected("sdk_control_key_conflict"));
                }
                app.seen = Instant::now();
                app.handle = handle;
                Ok(app.snapshot(&id))
            }
            SdkControl::Withdraw { .. } => {
                let id = owner.ok_or_else(|| rejected("sdk_control_owner"))?;
                apps.remove(&id);
                Ok(json!({"withdrawn":true}))
            }
            SdkControl::List => Ok(
                json!({"applications":apps.iter_mut().map(|(id, app)| app.snapshot(id)).collect::<Vec<_>>()}),
            ),
            SdkControl::Request {
                application_id,
                request_id,
                intent,
                timeout_ms,
            } => {
                if !valid_id(&request_id) || !(100..=30000).contains(&timeout_ms) {
                    return Err(rejected("sdk_control_request_invalid"));
                }
                let app = apps
                    .get_mut(&application_id)
                    .ok_or_else(|| rejected("sdk_application_unavailable"))?;
                if let Some(pending) = &mut app.request {
                    let snapshot = pending.snapshot();
                    if pending.id == request_id {
                        if pending.intent != intent || pending.timeout_ms != timeout_ms {
                            return Err(rejected("sdk_control_request_conflict"));
                        }
                        return Ok(snapshot);
                    }
                    if snapshot["state"] == "pending" {
                        return Err(rejected("sdk_control_busy"));
                    }
                }
                self.lifecycle_record("sdk_control_requested", &application_id,
                    json!({"request_id":request_id,"intent":intent,"process":app.process.metadata()}))?;
                app.request = Some(Pending {
                    id: request_id,
                    intent,
                    timeout_ms,
                    expires: Instant::now() + Duration::from_millis(timeout_ms),
                    state: "pending",
                    previous: app.handle.clone(),
                    error: None,
                    handle: None,
                });
                Ok(app
                    .request
                    .as_mut()
                    .ok_or_else(|| rejected("sdk_control_request_unknown"))?
                    .snapshot())
            }
            SdkControl::Query {
                application_id,
                request_id,
            } => {
                let app = apps
                    .get_mut(&application_id)
                    .ok_or_else(|| rejected("sdk_application_unavailable"))?;
                let pending = app
                    .request
                    .as_mut()
                    .filter(|r| r.id == request_id)
                    .ok_or_else(|| rejected("sdk_control_request_unknown"))?;
                Ok(pending.snapshot())
            }
            SdkControl::Acknowledge {
                key: _,
                request_id,
                handle,
                error,
            } => {
                let id = owner.ok_or_else(|| rejected("sdk_control_owner"))?;
                let app = apps
                    .get_mut(&id)
                    .ok_or_else(|| rejected("sdk_application_unavailable"))?;
                if error.as_ref().is_some_and(|code| !valid_id(code))
                    || (error.is_some() && handle.is_some())
                {
                    return Err(rejected("sdk_control_result_invalid"));
                }
                if let Some((candidate, mode)) = candidate {
                    if candidate.binding.target != app.target || mode != json!(app.mode) {
                        return Err(rejected("sdk_control_registration_conflict"));
                    }
                }
                let pending = app
                    .request
                    .as_mut()
                    .filter(|r| r.id == request_id)
                    .ok_or_else(|| rejected("sdk_control_request_unknown"))?;
                let snapshot = pending.snapshot();
                if pending.state == "expired" {
                    return Err(rejected("sdk_control_request_expired"));
                }
                if pending.state != "pending" {
                    if json!(pending.handle) != json!(handle) || pending.error != error {
                        return Err(rejected("sdk_control_result_conflict"));
                    }
                    return Ok(snapshot);
                }
                if error.is_none() && (pending.intent == SdkIntent::Attach) != handle.is_some() {
                    return Err(rejected("sdk_control_result_invalid"));
                }
                if error.is_none() && pending.intent == SdkIntent::Detach {
                    if let Some(old) = &pending.previous {
                        let instances = self
                            .instances
                            .lock()
                            .map_err(|_| rejected("state_poisoned"))?;
                        if instances.contains_key(&old.binding.instance_id) {
                            return Err(rejected("sdk_control_still_attached"));
                        }
                    }
                }
                self.lifecycle_record(
                    "sdk_control_acknowledged",
                    &id,
                    json!({"request_id":request_id,"intent":pending.intent,"error":error,
                        "instance_id":handle.as_ref().map(|h| &h.binding.instance_id)}),
                )?;
                pending.state = if error.is_some() {
                    "failed"
                } else {
                    "completed"
                };
                pending.error = error;
                pending.handle = handle.clone();
                if pending.error.is_none() {
                    app.handle = handle;
                }
                app.seen = Instant::now();
                Ok(pending.snapshot())
            }
        }
    }
}
