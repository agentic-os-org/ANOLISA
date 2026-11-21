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
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

mod binding;
mod dispatch;
mod event_loop;
mod evidence;
mod expired;
pub(crate) mod facts;
mod hooks;
mod input;
mod invocations;
mod limits;
mod maintenance;
mod policy;
pub(crate) mod process;
mod provider_binding;
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
    shadow: Mutex<Option<event_loop::ShadowEvent>>,
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
            shadow: Mutex::new(None),
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
        let execution = self.event_execution(instance, &instance_id, &value)?;
        if native_input.is_none() {
            let mut state = instance
                .invocations
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            if state.ledger.is_some() {
                return Err(rejected("invocation_correlated_event_required"));
            }
            // Enable and legacy admission share the protocol lock. Once used,
            // even a completed legacy event cannot switch to managed identity.
            state.legacy_used = true;
        }
        let host = Arc::clone(&instance.host);
        if value["agent"]["instance_id"] != instance_id {
            return Err(rejected("event_instance_mismatch"));
        }
        let policy = host.configuration().as_value().clone();
        let metadata = event_metadata(&value)?;
        let key = self.identifier()?;
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
            if events.len()
                + self
                    .expired
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .len()
                >= self.limits.events
            {
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
        }
        drop(instances);
        let runtime = Arc::clone(self);
        let thread_key = key.clone();
        let worker = thread::Builder::new().name("aw-event".into()).spawn(move || {
            let automatic = native_input.is_some();
            let association = evidence::association(&host.capabilities().adapter, &value, native_input.as_deref());
            let candidate = value.clone();
            let ordered = automatic && ((value["name"] == "tool.before" && host.steps().iter().any(|step| step.event == "tool.before" && matches!(&step.execution, aw_core::admission::StepExecution::Provider { effects, .. } if effects.iter().any(|effect| effect == "replace_input")))) || aw_adapter::completion::ordered_after(&host.capabilities().adapter, &value,
                host.steps().iter().any(|step| step.event == "tool.after" && step.execution.replaces_result())));
            let binding_active = || runtime.check_provider_admission(&instance_id, execution.as_ref()).is_ok();
            let obligation_audit = |update: aw_host::ObligationUpdate<'_>| runtime.audit.obligation(&thread_key, update).is_ok();
            let event = match native_input {
                Some(input) => host.hook_event(value, input, deadline, &cancelled),
                None => host.event(value, deadline, &cancelled),
            }.map_err(|_| rejected("event_invalid")).and_then(|event| {
                match runtime.provider_binding(&host, &instance_id, &candidate, &association, execution.as_ref())? {
                    Some(binding) => event.with_binding(binding, &binding_active).map_err(|_| rejected("event_invalid")),
                    None => Ok(event),
                }
            });
            match event {
                Ok(event) => {
                    let event = event.with_obligation_audit(&obligation_audit);
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
                            if let Ok(mut events) = runtime.events.lock() { events.remove(&thread_key); }
                            return;
                        }
                    };
                    let handle = EventHandle { event_id: thread_key.clone(), instance_id: instance_id.clone(), steps: event.callback_steps().into_iter().map(|step| step.step_id.clone()).collect(), candidate_revision };
                    let public_event = runtime.hook_fact(&thread_key, &host, &instance_id, &candidate, &association);
                    let claimed = public_event.and_then(|public_event| {
                        let mut plan = json!({"kind": "event", "identity": runtime.identity,
                            "instance_id": instance_id, "binding_id": host.target(), "policy_revision":host.revision(), "host_event_id": event.id(), "event": metadata});
                        if let Some(event) = public_event { plan["executionEvent"] = event; }
                        else { plan["publicProjectionGap"] = json!("aw.native_metadata_unavailable"); }
                        runtime.audit.claim(&thread_key, plan)
                    });
                    match claimed {
                        Ok(_) => {
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
            context.close();
            if let Ok(mut events) = runtime.events.lock() { events.remove(&thread_key); }
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

    fn event_sender(
        &self,
        key: &str,
        instance: &str,
    ) -> Result<(SyncSender<Job>, Arc<AtomicBool>)> {
        let events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
        let entry = events
            .get(key)
            .filter(|entry| entry.instance_id == instance)
            .ok_or_else(|| rejected("unknown_event"))?;
        Ok((entry.sender.clone(), Arc::clone(&entry.cancelled)))
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
            self.cancel_shadow(|_| true)?;
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

fn event_metadata(value: &Value) -> Result<Value> {
    if value["name"] == "input.submit" {
        let fields = [&value["session_id"], &value["turn_id"]];
        if fields.iter().any(|field| {
            !field
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= 512)
        }) {
            return Err(rejected("event_metadata_limit"));
        }
        return Ok(json!({"name":"input.submit", "session_id":fields[0], "turn_id":fields[1]}));
    }
    let fields = [
        &value["name"],
        &value["session_id"],
        &value["tool"]["name"],
        &value["tool"]["native_name"],
        &value["tool"]["call_id"],
    ];
    if fields
        .iter()
        .any(|field| !(field.is_null() || field.as_str().is_some_and(|text| text.len() <= 512)))
    {
        return Err(rejected("event_metadata_limit"));
    }
    let mut metadata = json!({"name": fields[0], "session_id": fields[1], "tool": {
        "name": fields[2], "native_name": fields[3], "call_id": fields[4],
    }});
    if let Some(source) = aw_adapter::callback::correlation_source(value) {
        metadata["correlation_source"] = json!(source);
    }
    Ok(metadata)
}
