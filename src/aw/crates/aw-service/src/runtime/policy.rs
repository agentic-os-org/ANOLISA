//! Per-instance immutable policies; request admission and activation share one gate.

use super::{Instance, Runtime};
use crate::{rejected, Error, LifecyclePhase, Operation, Request, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

mod process;
use process::Process;

pub(super) struct Reload {
    process: Option<Process>,
    ready: bool,
    pending_bridge: Option<u32>,
    process_audit: Option<String>,
    bridge_audit: Option<String>,
    requests: BTreeSet<String>,
    tools: BTreeSet<String>,
    pub(super) preparation_cancelled: Option<Arc<AtomicBool>>,
}

impl Runtime {
    pub(super) fn enable_reload(&self, id: &str) -> Result<Value> {
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let instance = instance(&mut instances, id)?;
        if instance.reload.is_some() {
            return Err(rejected("reload_already_enabled"));
        }
        aw_adapter::reload::supported(instance.host.capabilities())
            .map_err(|_| rejected("unsupported"))?;
        instance.reload = Some(Reload {
            process: None,
            ready: false,
            pending_bridge: None,
            process_audit: None,
            bridge_audit: None,
            requests: BTreeSet::new(),
            tools: BTreeSet::new(),
            preparation_cancelled: None,
        });
        Ok(json!({"reloadable":true,"policy_revision":instance.host.revision()}))
    }

    pub(super) fn associate_process(&self, id: &str, pid: u32) -> Result<Value> {
        let process = Process::read(pid)?;
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let state = instance(&mut instances, id)?
            .reload
            .as_mut()
            .ok_or_else(|| rejected("unsupported"))?;
        if state.process.is_some() {
            return Err(rejected("process_already_associated"));
        }
        if let Some(pid) = state.pending_bridge {
            process.corroborate(pid)?;
        }
        let key = self.policy_record(
            "process_association",
            id,
            json!({"process":process.metadata(),"native_bridge_pid":state.pending_bridge}),
        )?;
        state.ready = state.pending_bridge.is_some();
        state.pending_bridge = None;
        state.process_audit = Some(key);
        state.process = Some(process);
        Ok(json!({"associated":true}))
    }

    pub(super) fn lifecycle(
        &self,
        id: &str,
        pid: u32,
        phase: LifecyclePhase,
        key: &str,
    ) -> Result<Value> {
        if key.len() > 512 || key.chars().any(char::is_control) {
            return Err(rejected("lifecycle_identity"));
        }
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let instance = instance(&mut instances, id)?;
        let state = instance
            .reload
            .as_mut()
            .ok_or_else(|| rejected("unsupported"))?;
        if matches!(phase, LifecyclePhase::Ready) && state.process.is_none() {
            if !key.is_empty() || state.pending_bridge.is_some() {
                return Err(rejected("bridge_already_ready"));
            }
            Process::read(pid)?;
            // Spawn and native startup can race. The launcher must corroborate
            // this pending registration before readiness or request admission.
            state.pending_bridge = Some(pid);
            return Ok(json!({"ready":false}));
        }
        state
            .process
            .as_ref()
            .ok_or_else(|| rejected("process_not_associated"))?
            .corroborate(pid)?;
        match phase {
            LifecyclePhase::Ready => {
                if !key.is_empty() || state.ready {
                    return Err(rejected("bridge_already_ready"));
                }
                state.bridge_audit =
                    Some(self.policy_record("bridge_ready", id, json!({"pid":pid}))?);
                state.ready = true;
            }
            LifecyclePhase::RequestStart | LifecyclePhase::ToolStart => {
                if !state.ready || key.is_empty() {
                    return Err(rejected("bridge_not_ready"));
                }
                let tool = matches!(phase, LifecyclePhase::ToolStart);
                if tool && state.requests.is_empty() {
                    return Err(rejected("request_not_admitted"));
                }
                let claims = if tool {
                    &mut state.tools
                } else {
                    &mut state.requests
                };
                if claims.len() >= 64 || !claims.insert(key.into()) {
                    return Err(rejected("busy"));
                }
            }
            LifecyclePhase::RequestEnd | LifecyclePhase::ToolEnd => {
                if !state.ready || key.is_empty() {
                    return Err(rejected("lifecycle_identity"));
                }
                let claims = if matches!(phase, LifecyclePhase::ToolEnd) {
                    &mut state.tools
                } else {
                    &mut state.requests
                };
                claims.remove(key);
            }
        }
        Ok(json!({"policy_revision":instance.host.revision(),"ready":state.ready}))
    }

    pub(super) fn hook_plan(&self, id: &str, event: &str) -> Result<Value> {
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let instance = instance(&mut instances, id)?;
        let state = instance
            .reload
            .as_ref()
            .ok_or_else(|| rejected("unsupported"))?;
        if !state.ready || (state.requests.is_empty() && state.tools.is_empty()) {
            return Err(rejected("request_not_admitted"));
        }
        state
            .process
            .as_ref()
            .ok_or_else(|| rejected("process_not_associated"))?
            .check()?;
        let plan = aw_adapter::reload::plan(
            instance.host.configuration().as_value(),
            instance.host.target(),
            instance.host.steps(),
            event,
        )
        .map_err(|_| rejected("native_policy_unsupported"))?;
        Ok(json!({"policy_revision":instance.host.revision(),"plan":plan}))
    }

    pub(super) fn policy_status(&self) -> Result<Value> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        Ok(Value::Array(instances.iter().filter_map(|(id, instance)| instance.as_ref().map(|instance| {
            let state = instance.reload.as_ref();
            json!({"instance_id":id,"target":instance.host.target(),"policy_revision":instance.host.revision(),
                "adapter":instance.host.capabilities().adapter,"native_version":instance.host.capabilities().version,
                "entrypoint":instance.host.capabilities().entrypoint,"cwd":instance.context.cwd,
                "reloadable":state.is_some(),"ready":state.is_some_and(|s| s.ready),
                "process_audit":state.and_then(|s| s.process_audit.as_ref()),"bridge_audit":state.and_then(|s| s.bridge_audit.as_ref()),
                "requests":state.map(|s| s.requests.len()),"tools":state.map(|s| s.tools.len()),
                "process":state.and_then(|s| s.process.as_ref()).map(Process::metadata)})
        })).collect()))
    }

    fn policy_record(&self, kind: &str, id: &str, metadata: Value) -> Result<String> {
        let key = self.identifier()?;
        self.audit.claim(
            &key,
            json!({"kind":kind,"identity":self.identity,"instance_id":id,"metadata":metadata}),
        )?;
        let result = self
            .audit
            .append(&key, json!({"phase":"closed","status":"ok"}));
        self.audit.release(&key);
        result?;
        Ok(key)
    }

    fn idle(&self, instance: &Instance, id: &str) -> Result<()> {
        let state = instance
            .reload
            .as_ref()
            .ok_or_else(|| rejected("unsupported"))?;
        state
            .process
            .as_ref()
            .ok_or_else(|| rejected("process_not_associated"))?
            .check()?;
        if !state.ready
            || !state.requests.is_empty()
            || !state.tools.is_empty()
            || self
                .events
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .values()
                .any(|entry| entry.instance_id == id)
        {
            return Err(rejected("busy"));
        }
        Ok(())
    }

    pub(crate) fn update_policy(
        &self,
        request: &Request,
        peer: (u32, i32),
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Value> {
        let Operation::UpdatePolicy {
            instance_id,
            expected_revision,
            config,
        } = &request.operation
        else {
            return Err(rejected("invalid_update_operation"));
        };
        let key = self.identifier()?;
        let previous = self.policy_revision(instance_id)?;
        // Malformed caller metadata must not exceed the durable journal's bounds.
        let instance_metadata = (instance_id.len() <= 64).then_some(instance_id);
        let revision_metadata = (expected_revision.len() <= 64).then_some(expected_revision);
        self.audit.claim(
            &key,
            json!({"kind":"policy_update", "identity":self.identity,
            "instance_id":instance_metadata, "caller":{"uid":peer.0,"pid":peer.1},
            "previous_revision":previous, "expected_revision":revision_metadata,
            "candidate_revision":format!("{:x}",Sha256::digest(config.as_bytes()))}),
        )?;
        let result = (|| {
            if peer.1 <= 0 {
                return Err(rejected("peer_pid_unavailable"));
            }
            let deadline = request.admit(&self.identity)?;
            if instance_metadata.is_none() || revision_metadata.is_none() {
                return Err(rejected("invalid_update_metadata"));
            }
            if self.stopped.load(Ordering::Acquire) {
                return Err(rejected("service_stopping"));
            }
            self.activate_policy(
                instance_id,
                expected_revision,
                config.as_bytes(),
                &key,
                deadline,
                cancelled,
            )
        })();
        let result = result.or_else(|error| {
            let (code, preparation) = match error {
                Error::Rejected(code) => (code, None),
                Error::Attempt { code, audit_key } => (code, Some(audit_key)),
                Error::Configuration(_) => ("configuration_rejected".into(), None),
                Error::Io(_) => ("service_failure".into(), None),
            };
            let active = self.policy_revision(instance_id)?;
            self.audit.append(
                &key,
                json!({"phase":"closed",
                "status":if code=="busy" {"busy"} else {"rejected"},
                "reason":code,"policy_revision":active,"preparation_audit":preparation}),
            )?;
            Err(Error::Attempt {
                code,
                audit_key: key.clone(),
            })
        });
        self.audit.release(&key);
        result
    }

    fn policy_revision(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?
            .get(id)
            .and_then(Option::as_ref)
            .map(|instance| instance.host.revision().to_owned()))
    }

    fn activate_policy(
        &self,
        id: &str,
        expected: &str,
        bytes: &[u8],
        key: &str,
        deadline: Instant,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Value> {
        let started = Instant::now();
        let (baseline, target, capabilities, context, update) = {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instance(&mut instances, id)?;
            self.idle(instance, id)?;
            if instance.host.revision() != expected {
                return Err(rejected("policy_revision_conflict"));
            }
            (
                instance.bytes.clone(),
                instance.host.target().to_owned(),
                instance.host.capabilities().clone(),
                instance.context.clone(),
                instance.policy_update.clone(),
            )
        };
        // Preparation does not block native requests. A concurrent request wins
        // admission and causes the final activation check to return busy.
        let _preparing = update.try_lock().map_err(|_| rejected("busy"))?;
        {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            // Release fences new preparation here, then cancels the admitted
            // update and drains its mutex before acknowledging instance cleanup.
            instance(&mut instances, id)?
                .reload
                .as_mut()
                .ok_or_else(|| rejected("unsupported"))?
                .preparation_cancelled = Some(Arc::clone(cancelled));
        }
        let validator = aw_config::Validator::new()?;
        let candidate = validator.parse(bytes)?;
        validator
            .parse(&baseline)?
            .validate_policy_update(&candidate)?;
        aw_adapter::reload::validate(candidate.as_value(), &target, &capabilities)
            .map_err(|_| rejected("native_policy_unsupported"))?;
        let attempt = self.identifier()?;
        let host = self.prepare(
            bytes,
            &attempt,
            &target,
            capabilities.into(),
            context,
            (deadline, cancelled),
        )?;
        let activated = (|| {
            let prepared = Instant::now();
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instance(&mut instances, id)?;
            self.idle(instance, id)?;
            if instance.host.revision() != expected {
                return Err(rejected("policy_revision_conflict"));
            }
            if cancelled.load(Ordering::Acquire)
                || self.stopped.load(Ordering::Acquire)
                || Instant::now() >= deadline
            {
                return Err(rejected("policy_update_cancelled"));
            }
            self.audit.append(key, json!({"phase":"closed","status":"active",
                "previous_revision":expected,"policy_revision":host.revision(),"preparation_audit":attempt}))?;
            let revision = host.revision().to_owned();
            instance.host = std::sync::Arc::new(host);
            instance.bytes = bytes.to_vec();
            // Completed native correlation claims remain retained across updates;
            // an old callback cannot receive a fresh execution budget.
            Ok(
                json!({"policy_revision":revision,"previous_revision":expected,"instance_id":id,"audit_key":key,"preparation_ms":prepared.duration_since(started).as_millis(),"activation_ms":prepared.elapsed().as_millis()}),
            )
        })();
        activated.map_err(|error| match error {
            Error::Rejected(code) => Error::Attempt {
                code,
                audit_key: attempt,
            },
            other => other,
        })
    }
}

fn instance<'a>(
    instances: &'a mut std::collections::BTreeMap<String, Option<Instance>>,
    id: &str,
) -> Result<&'a mut Instance> {
    instances
        .get_mut(id)
        .and_then(Option::as_mut)
        .filter(|instance| !instance.closing)
        .ok_or_else(|| rejected("unknown_or_draining_instance"))
}
