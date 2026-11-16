//! Bounded instances and event workers; each worker borrows one Host on its own stack.

use crate::{
    audit::{self, Audit},
    rejected, Capabilities, EventHandle, Identity, Result,
};
use aw_core::admission::{AdmissionEngine, AdmissionError};
use aw_host::{Host, ProcessContext};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

mod binding;
mod dispatch;
mod evidence;
mod expired;
pub(crate) mod facts;
mod hooks;
mod input;
mod invocations;
mod lifecycle;
mod limits;
mod maintenance;
mod policy;
pub(crate) mod process;
mod registration;
mod scopes;
mod sdk;
mod sdk_control;
mod submissions;

use crate::MAX_STEPS_PER_BINDING as MAX_STEPS;

struct Instance {
    run: Option<crate::RunOutcome>,
    native_facts: Arc<Mutex<facts::NativeFacts>>,
    bytes: Vec<u8>,
    context: ProcessContext,
    reload: Option<policy::Reload>,
    policy_update: Arc<Mutex<()>>,
    release: Arc<Mutex<()>>,
    host: Arc<Host>,
    routes: Option<Arc<aw_core::routes::PreparedRoutes>>,
    closing: bool,
    hooks: Arc<Mutex<BTreeMap<String, hooks::Lease>>>,
    hook_open: Arc<Mutex<()>>,
    invocations: Arc<Mutex<invocations::State>>,
    observation: Option<Arc<evidence::Observation>>,
}

struct EventEntry {
    instance_id: String,
    sender: SyncSender<Job>,
    cancelled: Arc<AtomicBool>,
    context: Arc<evidence::Cell>,
}

enum Job {
    Completion {
        value: Value,
        input: Vec<u8>,
        reply: SyncSender<Result<Value>>,
    },
    Invoke {
        step: String,
        native_environment: Option<BTreeMap<std::ffi::OsString, std::ffi::OsString>>,
        reply: SyncSender<Result<Value>>,
    },
    Close {
        reply: SyncSender<Result<Value>>,
    },
}

pub(crate) struct Runtime {
    pub(crate) identity: Identity,
    bytes: Vec<u8>,
    admission: AdmissionEngine,
    pub(crate) limits: limits::Limits,
    pub(crate) resources: limits::Resources,
    pub(crate) stopped: Arc<AtomicBool>,
    pub(crate) audit: Audit,
    instances: Mutex<BTreeMap<String, Option<Instance>>>,
    events: Mutex<BTreeMap<String, EventEntry>>,
    expired: Mutex<BTreeMap<String, expired::ExpiredEvent>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    next: AtomicU64,
    calls: AtomicU64,
    contexts: evidence::Contexts,
    owners: Mutex<BTreeMap<String, scopes::Owner>>,
    sdk_controls: sdk_control::Registry,
    registrations: Mutex<BTreeMap<String, Arc<registration::Record>>>,
}

impl Runtime {
    #[cfg(test)]
    pub(crate) fn new(
        bytes: Vec<u8>,
        directory: &Path,
        stopped: Arc<AtomicBool>,
    ) -> Result<Arc<Self>> {
        let configuration = aw_config::Validator::new()?.parse(&bytes)?;
        Self::from_configuration(bytes, &configuration, directory, stopped)
    }

    pub(crate) fn from_configuration(
        bytes: Vec<u8>,
        configuration: &aw_config::Configuration,
        directory: &Path,
        stopped: Arc<AtomicBool>,
    ) -> Result<Arc<Self>> {
        let generation = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let revision = format!("{:x}", Sha256::digest(&bytes));
        let admission = AdmissionEngine::new().map_err(|error| match error {
            AdmissionError::Configuration(error) => error.into(),
            AdmissionError::Rejected(_) => rejected("binding_preflight"),
        })?;
        let contexts = evidence::Contexts::new(configuration)?;
        let limits = limits::Limits::from_configuration(configuration)?;
        Ok(Arc::new(Self {
            contexts,
            limits,
            resources: limits::Resources::default(),
            owners: Mutex::new(BTreeMap::new()),
            registrations: Mutex::new(BTreeMap::new()),
            sdk_controls: sdk_control::Registry::default(),
            identity: Identity {
                generation,
                config_revision: revision,
            },
            bytes,
            admission,
            stopped,
            audit: Audit::new(directory)?,
            instances: Mutex::new(BTreeMap::new()),
            events: Mutex::new(BTreeMap::new()),
            expired: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
            calls: AtomicU64::new(0),
        }))
    }

    fn identifier(&self) -> Result<String> {
        let next = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| rejected("id_exhausted"))?;
        Ok(format!(
            "{:x}",
            Sha256::digest(format!("{}:{next}", self.identity.generation))
        ))
    }

    fn prepare(
        &self,
        bytes: &[u8],
        key: &str,
        target: &str,
        capabilities: Capabilities,
        context: ProcessContext,
        limit: (Instant, &AtomicBool),
    ) -> Result<Host> {
        let (deadline, cancelled) = limit;
        // Reject excessive metadata and step counts before journaling or Provider discovery.
        if target.len() > 128
            || capabilities.version.len() > 128
            || capabilities.entrypoint.len() > 128
        {
            return Err(rejected("binding_metadata_limit"));
        }
        let candidate = self
            .admission
            .preflight(bytes, target, capabilities.into())
            .map_err(|error| match error {
                AdmissionError::Configuration(error) => error.into(),
                AdmissionError::Rejected(_) => rejected("binding_preflight"),
            })?;
        if candidate.requirements().len() > MAX_STEPS {
            return Err(rejected("step_limit"));
        }
        self.audit.claim(key, json!({"kind": "preparation", "identity": self.identity, "instance_id": key, "binding_id": target}))?;
        let result = (|| {
            let host =
                Host::discover(&candidate, context, deadline, cancelled).and_then(|prepared| {
                    let policy = match self.admission.admit(candidate, prepared.evidence()) {
                        Ok(policy) => policy,
                        Err(error) => return Err(prepared.into_failure(error.into())),
                    };
                    if cancelled.load(Ordering::Acquire) {
                        return Err(prepared
                            .into_failure(aw_host::Error::Execution(aw_exec::Error::Cancelled)));
                    }
                    if Instant::now() >= deadline {
                        return Err(prepared.into_failure(aw_host::Error::Execution(
                            aw_exec::Error::DeadlineExceeded,
                        )));
                    }
                    Host::from_admitted(policy, prepared)
                });
            match host {
                Ok(host) => {
                    self.audit.append(key, json!({"phase": "closed", "status": "ok", "calls": host.preparation().iter().map(audit::call).collect::<Vec<_>>()}))?;
                    Ok(host)
                }
                Err(error) => {
                    self.audit.append(key, json!({"phase": "closed", "status": "error", "calls": audit::preparation(&error)}))?;
                    Err(crate::Error::Attempt {
                        code: "preparation_failed".into(),
                        audit_key: key.into(),
                    })
                }
            }
        })();
        self.audit.release(key);
        result
    }

    fn open(
        self: &Arc<Self>,
        instance_id: String,
        value: Value,
        native_input: Option<Vec<u8>>,
        declared_status: Option<crate::ResultStatus>,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Value> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let instance = instances
            .get(&instance_id)
            .and_then(Option::as_ref)
            .filter(|instance| !instance.closing)
            .ok_or_else(|| rejected("unknown_or_draining_instance"))?;
        let result_status = if native_input.is_none() {
            facts::reported_result_status(
                &instance.host.capabilities().adapter,
                &value,
                declared_status,
            )?
        } else {
            None
        };
        let execution = self.event_execution(instance, &instance_id, &value)?;
        let lifecycle = value["name"]
            .as_str()
            .is_some_and(aw_adapter::callback::lifecycle);
        let mut invocation_protocol = if native_input.is_none() {
            if lifecycle {
                return Err(rejected("native_lifecycle_callback_required"));
            }
            let state = instance
                .invocations
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            if state.ledger.is_some() {
                return Err(rejected("invocation_correlated_event_required"));
            }
            Some(state)
        } else {
            None
        };
        let host = Arc::clone(&instance.host);
        if value["agent"]["instance_id"] != instance_id {
            return Err(rejected("event_instance_mismatch"));
        }
        let policy = host.configuration().as_value().clone();
        let metadata = if lifecycle {
            self.lifecycle_metadata(instance, &instance_id, &value)?
        } else {
            facts::event_metadata(&value)?
        };
        let key = if lifecycle {
            let key = self.lifecycle_key(&instance_id, &hooks::event_correlation(&value)?)?;
            if self.audit.lifecycle_claimed(&key)? {
                return self.reopen_stop(&key, &value);
            }
            key
        } else {
            self.identifier()?
        };
        let reported_event = if let Some(status) = result_status {
            let association = evidence::association(&host.capabilities().adapter, &value, None);
            let fact = self.hook_fact(
                &key,
                &host,
                &instance_id,
                &value,
                &association,
                Some(status),
            )?;
            if declared_status.is_some() && fact.is_none() {
                return Err(rejected("public_event_invalid"));
            }
            fact
        } else {
            None
        };
        let context = Arc::new(evidence::Cell::default());
        let (sender, receiver) = mpsc::sync_channel(MAX_STEPS + 1);
        let (ready, result) = mpsc::sync_channel(1);
        {
            let mut events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
            // Close checks this registry after marking the shared correlation;
            // it must not acknowledge cleanup before a late registration.
            if cancelled.load(Ordering::Acquire) {
                return Err(rejected("event_cancelled"));
            }
            let expired = self
                .expired
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            if lifecycle && (events.contains_key(&key) || expired.contains_key(&key)) {
                return Err(rejected("native_event_closed"));
            }
            if events.len() + expired.len() >= self.limits.events {
                return Err(self.overload("event_limit"));
            }
            events.insert(
                key.clone(),
                EventEntry {
                    instance_id: instance_id.clone(),
                    sender,
                    cancelled: Arc::clone(&cancelled),
                    context: Arc::clone(&context),
                },
            );
            if let Some(state) = invocation_protocol.as_mut() {
                // Commit protocol use with the reserved event slot.
                state.legacy_used = true;
            }
        }
        drop(invocation_protocol);
        drop(instances);
        let runtime = Arc::clone(self);
        let thread_key = key.clone();
        let worker = thread::Builder::new().name("aw-event".into()).spawn(move || {
            let automatic = native_input.is_some();
            let association = evidence::association(&host.capabilities().adapter, &value, native_input.as_deref());
            let candidate = value.clone();
            let ordered = automatic && ((value["name"] == "tool.before" && host.steps().iter().any(|step| step.event == "tool.before" && matches!(&step.execution, aw_core::admission::StepExecution::Provider { effects, .. } if effects.iter().any(|effect| effect == "replace_input")))) || aw_adapter::completion::ordered_after(&host.capabilities().adapter, &value,
                host.steps().iter().any(|step| step.event == "tool.after" && step.execution.replaces_result())));
            let event = match native_input {
                Some(input) => host.hook_event(value, input, deadline, &cancelled),
                None => host.event(value, deadline, &cancelled),
            }.map_err(|_| rejected("event_invalid"));
            let mut reserved = false;
            match event {
                Ok(event) => {
                    let seed = runtime.contexts.seed(json!({
                        "generation":runtime.identity.generation,"config_revision":host.revision(),
                        "binding_id":host.target(),"instance_id":instance_id,"event_id":thread_key,
                        "adapter":host.capabilities().adapter,
                    }), &candidate, association.clone(), event.deadline(), &policy).and_then(|seed| context.initialize(seed));
                    let candidate_revision = match seed {
                        Ok(revision) => revision,
                        Err(error) => {
                            let _ = ready.send(Err(error));
                            context.close();
                            runtime.remove_live_event(&thread_key, &cancelled);
                            return;
                        }
                    };
                    let handle = EventHandle { event_id: thread_key.clone(), instance_id: instance_id.clone(), steps: event.steps().map(|step| step.step_id.clone()).collect(), candidate_revision };
                    let public_event = if lifecycle {
                        Ok(None)
                    } else if result_status.is_some() {
                        Ok(reported_event)
                    } else {
                        runtime.hook_fact(&thread_key, &host, &instance_id, &candidate, &association, None)
                    };
                    let claimed = public_event.and_then(|public_event| {
                        let mut plan = json!({"kind": "event", "identity": runtime.identity,
                            "instance_id": instance_id, "binding_id": host.target(), "policy_revision":host.revision(), "host_event_id": event.id(), "event": metadata});
                        if let Some(event) = public_event { plan["executionEvent"] = event; }
                        else { plan["publicProjectionGap"] = json!(if lifecycle {
                            "aw.lifecycle_projection_unavailable"
                        } else { "aw.native_metadata_unavailable" }); }
                        lifecycle::mark_stop_plan(&candidate, &handle, &mut plan)?;
                        if lifecycle {
                            runtime.audit.claim_lifecycle(&thread_key, plan)
                        } else {
                            runtime.audit.claim(&thread_key, plan)
                        }
                    });
                    match claimed {
                        Ok(_) => {
                            reserved = true;
                            if let Err(error) = context.seed().ok_or_else(|| rejected("context_not_ready")).and_then(|seed| runtime.observe_seed(&instance_id, seed)) {
                                let _ = ready.send(Err(error));
                            } else {
                                let _ = ready.send(Ok(json!(handle)));
                            }
                            runtime.event_loop((&thread_key, &instance_id, &candidate), &event, (&cancelled, &context, execution.as_ref()), receiver, automatic, ordered);
                        }
                        Err(error) => { let _ = ready.send(Err(error)); }
                    }
                }
                Err(error) => { let _ = ready.send(Err(error)); }
            }
            if reserved {
                runtime.audit.release(&thread_key);
            }
            context.close();
            runtime.remove_live_event(&thread_key, &cancelled);
        });
        match worker {
            Ok(worker) => self
                .workers
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .push(worker),
            Err(error) => {
                self.events
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .remove(&key);
                return Err(error.into());
            }
        }
        result
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| rejected("event_open_timeout"))?
    }

    fn event_loop(
        &self,
        identity: (&str, &str, &Value),
        event: &aw_host::Event<'_>,
        lease: (&AtomicBool, &evidence::Cell, Option<&policy::Execution>),
        receiver: Receiver<Job>,
        automatic: bool,
        ordered: bool,
    ) {
        let (key, instance_id, original) = identity;
        let rewrite = Mutex::new(input::State::default());
        let (cancelled, context, execution) = lease;
        let mut close = None;
        let mut claimed = BTreeSet::new();
        let mut panicked = false;
        let completed_steps = AtomicU64::new(0);
        let completed_mask = AtomicU64::new(0);
        let step_count = event.steps().count() as u64;
        thread::scope(|scope| {
            let mut children = Vec::new();
            while Instant::now() < event.deadline()
                && !self.stopped.load(Ordering::Acquire)
                && !cancelled.load(Ordering::Acquire)
            {
                if automatic && completed_steps.load(Ordering::Acquire) == step_count {
                    break;
                }
                let job = match receiver.recv_timeout(
                    event
                        .deadline()
                        .saturating_duration_since(Instant::now())
                        .min(std::time::Duration::from_millis(20)),
                ) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                match job {
                    Job::Completion {
                        value,
                        input,
                        reply,
                    } => {
                        let result = event
                            .completion_view(value, input)
                            .map(|()| json!({}))
                            .map_err(|_| rejected("native_completion_invalid"));
                        let _ = reply.send(result);
                    }
                    Job::Close { reply } => {
                        close = Some(reply);
                        break;
                    }
                    Job::Invoke {
                        step,
                        native_environment,
                        reply,
                    } => {
                        if cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire)
                        {
                            let _ = reply.send(Err(rejected("event_cancelled")));
                            continue;
                        }
                        if let Err(error) = self.check_execution_admission(instance_id, execution) {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        let Some(index) = event
                            .steps()
                            .position(|candidate| candidate.step_id == step)
                        else {
                            let _ = reply.send(Err(rejected("step_unknown_or_claimed")));
                            continue;
                        };
                        if !claimed.insert(step.clone()) {
                            let _ = reply.send(Err(rejected("step_unknown_or_claimed")));
                            continue;
                        }
                        // Admission caps events at 64 steps, so every step has a bit.
                        let bit = 1u64 << index;
                        if self
                            .calls
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                (n < self.limits.calls as u64).then_some(n + 1)
                            })
                            .is_err()
                        {
                            completed_steps.fetch_add(1, Ordering::Release);
                            completed_mask.fetch_or(bit, Ordering::Release);
                            let _ = reply.send(Err(self.overload("call_limit")));
                            continue;
                        }
                        let completed_steps = &completed_steps;
                        let completed_mask = &completed_mask;
                        let rewrite = &rewrite;
                        children.push(scope.spawn(move || {
                            let _guard = CallGuard(&self.calls);
                            let result = (|| {
                                while ordered
                                    && completed_mask.load(Ordering::Acquire) & (bit - 1) != bit - 1
                                {
                                    if Instant::now() >= event.deadline()
                                        || cancelled.load(Ordering::Acquire)
                                        || self.stopped.load(Ordering::Acquire)
                                    {
                                        return Err(rejected("event_cancelled_or_expired"));
                                    }
                                    thread::sleep(std::time::Duration::from_millis(2));
                                }
                                self.check_execution_admission(instance_id, execution)?;
                                self.audit
                                    .append(key, json!({"phase": "started", "step_id": step}))?;
                                // Queueing and durable audit can outlive the admitted owner.
                                self.check_execution_admission(instance_id, execution)?;
                                let mut invocation = match native_environment {
                                    Some(environment) => {
                                        event.invoke_with_native_environment(&step, &environment)
                                    }
                                    None => event.invoke(&step),
                                }
                                .map_err(|_| rejected("step_rejected"))?;
                                let replacement = match self.adopt_input(
                                    instance_id,
                                    event,
                                    original,
                                    &mut invocation,
                                    rewrite,
                                    index + 1 == step_count as usize,
                                ) {
                                    Ok(input) => input,
                                    Err(_) => {
                                        invocation.result = Err(aw_host::Failure::Protocol(
                                            aw_provider::Error::Invalid(
                                                "input replacement adoption failed",
                                            ),
                                        ));
                                        invocation.failure_action =
                                            Some(aw_host::FailureAction::Block);
                                        None
                                    }
                                };
                                let (mut record, mut result) = audit::invocation(invocation);
                                if result["effects"].as_array().is_some_and(|effects| {
                                    effects
                                        .iter()
                                        .any(|effect| effect["type"] == "replace_input")
                                }) {
                                    result["effects"] = serde_json::json!([]);
                                }
                                if let Some(input) = replacement {
                                    result["input_replacement"] = input;
                                }
                                self.record_stop_reply(
                                    key,
                                    event,
                                    original,
                                    &step,
                                    &mut record,
                                    &result,
                                )?;
                                self.audit.append(key, record)?;
                                Ok(result)
                            })();
                            completed_steps.fetch_add(1, Ordering::Release);
                            completed_mask.fetch_or(bit, Ordering::Release);
                            let _ = reply.send(result);
                        }));
                    }
                }
            }
            cancelled.store(true, Ordering::Release);
            for child in children {
                panicked |= child.join().is_err();
            }
        });
        context.close();
        let status = if panicked {
            "worker_failed"
        } else if close.is_some() {
            "closed"
        } else if automatic && completed_steps.load(Ordering::Acquire) == step_count {
            "completed"
        } else if self.stopped.load(Ordering::Acquire) {
            "stopped"
        } else if Instant::now() >= event.deadline() {
            "expired"
        } else {
            "cancelled"
        };
        let completed = self.retire_event(
            (key, instance_id),
            event,
            &claimed,
            status,
            automatic,
            execution,
        );
        if let Some(reply) = close {
            let _ =
                reply.send(completed.map(|evidence| json!({"closed": true, "evidence": evidence})));
        }
        if panicked {
            self.stopped.store(true, Ordering::Release);
        }
    }

    pub(crate) fn maintain(&self, shutdown: bool) -> Result<()> {
        if !shutdown {
            self.maintain_scopes()?;
        }
        if shutdown {
            self.stopped.store(true, Ordering::Release);
            let mut events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
            for entry in events.values() {
                entry.cancelled.store(true, Ordering::Release);
            }
            // Dropping registry senders wakes idle event workers; in-flight handlers are bounded.
            events.clear();
            self.expired
                .lock()
                .map_err(|_| rejected("state_poisoned"))?
                .clear();
        }
        if shutdown {
            self.stop_observations()?;
        }
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let mut retained = Vec::new();
        let mut failed = false;
        for worker in workers.drain(..) {
            if shutdown || worker.is_finished() {
                failed |= worker.join().is_err();
            } else {
                retained.push(worker);
            }
        }
        *workers = retained;
        if failed {
            return Err(rejected("event_worker_failed"));
        }
        Ok(())
    }
}

struct CallGuard<'a>(&'a AtomicU64);
impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
