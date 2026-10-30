//! Bounded instances and event workers; each worker borrows one Host on its own stack.

use crate::{
    audit::{self, Audit},
    rejected, Capabilities, EventHandle, Identity, Result,
};
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
mod hooks;
mod invocations;
mod limits;
mod policy;
pub(crate) mod process;
mod scopes;

const MAX_STEPS: usize = 64;

struct Instance {
    bytes: Vec<u8>,
    context: ProcessContext,
    reload: Option<policy::Reload>,
    policy_update: Arc<Mutex<()>>,
    release: Arc<Mutex<()>>,
    host: Arc<Host>,
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
    pub(crate) limits: limits::Limits,
    pub(crate) resources: limits::Resources,
    pub(crate) stopped: Arc<AtomicBool>,
    pub(crate) audit: Audit,
    instances: Mutex<BTreeMap<String, Option<Instance>>>,
    events: Mutex<BTreeMap<String, EventEntry>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    next: AtomicU64,
    calls: AtomicU64,
    contexts: evidence::Contexts,
    owners: Mutex<BTreeMap<String, scopes::Owner>>,
}

impl Runtime {
    pub(crate) fn new(
        bytes: Vec<u8>,
        directory: &Path,
        stopped: Arc<AtomicBool>,
    ) -> Result<Arc<Self>> {
        let generation = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let revision = format!("{:x}", Sha256::digest(&bytes));
        let contexts = evidence::Contexts::new(&bytes)?;
        let limits = limits::Limits::parse(&bytes)?;
        Ok(Arc::new(Self {
            contexts,
            limits,
            resources: limits::Resources::default(),
            owners: Mutex::new(BTreeMap::new()),
            identity: Identity {
                generation,
                config_revision: revision,
            },
            bytes,
            stopped,
            audit: Audit::new(directory)?,
            instances: Mutex::new(BTreeMap::new()),
            events: Mutex::new(BTreeMap::new()),
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
        let config = aw_config::Validator::new()?.parse(bytes)?;
        let capabilities = capabilities.into();
        let steps = aw_provider::admission::preflight(&config, target, &capabilities)
            .map_err(|_| rejected("binding_preflight"))?;
        if steps.len() > MAX_STEPS {
            return Err(rejected("step_limit"));
        }
        self.audit.claim(key, json!({"kind": "preparation", "identity": self.identity, "instance_id": key, "binding_id": target}))?;
        let result = (|| {
            let host = Host::prepare(bytes, target, capabilities, context, deadline, cancelled);
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
        self.check_scope_owner(&instance_id)?;
        let instance = instances
            .get(&instance_id)
            .and_then(Option::as_ref)
            .filter(|instance| !instance.closing)
            .ok_or_else(|| rejected("unknown_or_draining_instance"))?;
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
            if events.len() >= self.limits.events {
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
            let ordered = automatic && aw_adapter::completion::ordered_after(&host.capabilities().adapter, &value,
                host.steps().iter().any(|step| step.event == "tool.after" && step.execution.replaces_result()));
            let event = match native_input {
                Some(input) => host.hook_event(value, input, deadline, &cancelled),
                None => host.event(value, deadline, &cancelled),
            }.map_err(|_| rejected("event_invalid"));
            match event {
                Ok(event) => {
                    let seed = runtime.contexts.seed(json!({
                        "generation":runtime.identity.generation,"config_revision":host.revision(),
                        "binding_id":host.target(),"instance_id":instance_id,"event_id":thread_key,
                        "adapter":host.capabilities().adapter,
                    }), &candidate, association, event.deadline(), &policy).and_then(|seed| context.initialize(seed));
                    let candidate_revision = match seed {
                        Ok(revision) => revision,
                        Err(error) => {
                            let _ = ready.send(Err(error));
                            context.close();
                            if let Ok(mut events) = runtime.events.lock() { events.remove(&thread_key); }
                            return;
                        }
                    };
                    let handle = EventHandle { event_id: thread_key.clone(), instance_id: instance_id.clone(), steps: event.steps().map(|step| step.step_id.clone()).collect(), candidate_revision };
                    let claimed = runtime.audit.claim(&thread_key, json!({"kind": "event", "identity": runtime.identity,
                        "instance_id": instance_id, "binding_id": host.target(), "policy_revision":host.revision(), "host_event_id": event.id(), "event": metadata}));
                    match claimed {
                        Ok(_) => {
                            if let Err(error) = context.seed().ok_or_else(|| rejected("context_not_ready")).and_then(|seed| runtime.observe_seed(&instance_id, seed)) {
                                let _ = ready.send(Err(error));
                            } else {
                                let _ = ready.send(Ok(json!(handle)));
                            }
                            runtime.event_loop((&thread_key, &instance_id), &event, (&cancelled, &context), receiver, automatic, ordered);
                        }
                        Err(error) => { let _ = ready.send(Err(error)); }
                    }
                }
                Err(error) => { let _ = ready.send(Err(error)); }
            }
            runtime.audit.release(&thread_key);
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

    fn event_loop(
        &self,
        identity: (&str, &str),
        event: &aw_host::Event<'_>,
        lease: (&AtomicBool, &evidence::Cell),
        receiver: Receiver<Job>,
        automatic: bool,
        ordered: bool,
    ) {
        let (key, instance_id) = identity;
        let (cancelled, context) = lease;
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
                                self.check_scope_owner(instance_id)?;
                                self.audit
                                    .append(key, json!({"phase": "started", "step_id": step}))?;
                                // Queueing and durable audit can outlive the admitted owner.
                                self.check_scope_owner(instance_id)?;
                                let invocation = match native_environment {
                                    Some(environment) => {
                                        event.invoke_with_native_environment(&step, &environment)
                                    }
                                    None => event.invoke(&step),
                                }
                                .map_err(|_| rejected("step_rejected"))?;
                                let (record, result) = audit::invocation(invocation);
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
        let completed = self.audit.append(key, json!({"phase": "closed", "status": if panicked { "worker_failed" } else if close.is_some() { "closed" } else if automatic && completed_steps.load(Ordering::Acquire) == step_count { "completed" } else if self.stopped.load(Ordering::Acquire) { "stopped" } else if Instant::now() >= event.deadline() { "expired" } else { "cancelled" }, "attempted_steps": claimed.len()}));
        // Release registry membership before acknowledging close so immediate unbind works.
        if let Ok(mut events) = self.events.lock() {
            events.remove(key);
        }
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

fn event_metadata(value: &Value) -> Result<Value> {
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
