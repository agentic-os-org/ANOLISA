//! Bounded instances and event workers; each worker borrows one Host on its own stack.

use crate::{
    audit::{self, Audit},
    rejected, Binding, Capabilities, EventHandle, Identity, Operation, Result,
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

const MAX_BINDINGS: usize = 16;
const MAX_EVENTS: usize = 32;
const MAX_STEPS: usize = 64;
const MAX_CALLS: usize = 64;

struct Instance {
    host: Arc<Host>,
}

struct EventEntry {
    instance_id: String,
    sender: SyncSender<Job>,
    cancelled: Arc<AtomicBool>,
}

enum Job {
    Invoke {
        step: String,
        reply: SyncSender<Result<Value>>,
    },
    Close {
        reply: SyncSender<Result<Value>>,
    },
}

pub(crate) struct Runtime {
    pub(crate) identity: Identity,
    bytes: Vec<u8>,
    pub(crate) stopped: Arc<AtomicBool>,
    pub(crate) audit: Audit,
    instances: Mutex<BTreeMap<String, Option<Instance>>>,
    events: Mutex<BTreeMap<String, EventEntry>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    next: AtomicU64,
    calls: AtomicU64,
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
        Ok(Arc::new(Self {
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

    pub(crate) fn handle(
        self: &Arc<Self>,
        operation: Operation,
        deadline: Instant,
    ) -> Result<Value> {
        if Instant::now() >= deadline {
            return Err(rejected("deadline_exceeded"));
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err(rejected("service_stopping"));
        }
        match operation {
            Operation::Status => Ok(
                json!({"pid": std::process::id(), "bindings": self.instances.lock().map_err(|_| rejected("state_poisoned"))?.len(),
                "events": self.events.lock().map_err(|_| rejected("state_poisoned"))?.len(), "audit_healthy": self.audit.healthy(),
                "guarantee": "native_hook", "adoption": "unverified"}),
            ),
            Operation::Bind {
                target,
                capabilities,
                cwd,
                environment,
            } => self.bind(target, capabilities, cwd, environment, deadline),
            Operation::OpenEvent { instance_id, event } => self.open(instance_id, event, deadline),
            Operation::InvokeStep {
                event_id,
                instance_id,
                step_id,
            } => {
                let (sender, cancelled) = self.event_sender(&event_id, &instance_id)?;
                let (reply, result) = mpsc::sync_channel(1);
                sender
                    .try_send(Job::Invoke {
                        step: step_id,
                        reply,
                    })
                    .map_err(|_| rejected("event_busy_or_closed"))?;
                result
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| {
                        cancelled.store(true, Ordering::Release);
                        rejected("event_reply_unavailable")
                    })?
            }
            Operation::CloseEvent {
                event_id,
                instance_id,
            } => {
                let (sender, _) = self.event_sender(&event_id, &instance_id)?;
                let (reply, result) = mpsc::sync_channel(1);
                sender
                    .try_send(Job::Close { reply })
                    .map_err(|_| rejected("event_busy_or_closed"))?;
                result
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| rejected("event_reply_unavailable"))?
            }
            Operation::Unbind { instance_id } => {
                let mut instances = self
                    .instances
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?;
                if self
                    .events
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .values()
                    .any(|entry| entry.instance_id == instance_id)
                {
                    return Err(rejected("instance_has_events"));
                }
                instances
                    .remove(&instance_id)
                    .ok_or_else(|| rejected("unknown_instance"))?;
                Ok(json!({"released": true}))
            }
            Operation::Audit { key } => self.audit.read(&key),
            Operation::Stop => {
                self.stopped.store(true, Ordering::Release);
                Ok(json!({"stopping": true}))
            }
        }
    }

    fn bind(
        &self,
        target: String,
        capabilities: Capabilities,
        cwd: String,
        environment: BTreeMap<String, String>,
        deadline: Instant,
    ) -> Result<Value> {
        let instance_id = self.identifier()?;
        {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            if instances.len() >= MAX_BINDINGS {
                return Err(rejected("binding_limit"));
            }
            instances.insert(instance_id.clone(), None);
        }
        let result = self.prepare(
            &instance_id,
            &target,
            capabilities,
            cwd,
            environment,
            deadline,
        );
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        match result {
            Ok(host) => {
                instances.insert(
                    instance_id.clone(),
                    Some(Instance {
                        host: Arc::new(host),
                    }),
                );
                Ok(json!(Binding {
                    identity: self.identity.clone(),
                    instance_id: instance_id.clone(),
                    target,
                    audit_key: instance_id
                }))
            }
            Err(error) => {
                instances.remove(&instance_id);
                Err(error)
            }
        }
    }

    fn prepare(
        &self,
        key: &str,
        target: &str,
        capabilities: Capabilities,
        cwd: String,
        environment: BTreeMap<String, String>,
        deadline: Instant,
    ) -> Result<Host> {
        // Reject excessive metadata and step counts before journaling or Provider discovery.
        if target.len() > 128
            || capabilities.version.len() > 128
            || capabilities.entrypoint.len() > 128
        {
            return Err(rejected("binding_metadata_limit"));
        }
        let config = aw_config::Validator::new()?.parse(&self.bytes)?;
        let capabilities = capabilities.into();
        let steps = aw_provider::admission::preflight(&config, target, &capabilities)
            .map_err(|_| rejected("binding_preflight"))?;
        if steps.len() > MAX_STEPS {
            return Err(rejected("step_limit"));
        }
        self.audit.claim(key, json!({"kind": "preparation", "identity": self.identity, "instance_id": key, "binding_id": target}))?;
        let result = (|| {
            let host = Host::prepare(
                &self.bytes,
                target,
                capabilities,
                ProcessContext {
                    cwd: cwd.into(),
                    environment: environment
                        .into_iter()
                        .map(|(k, v)| (k.into(), v.into()))
                        .collect(),
                    stderr_bytes: 64 * 1024,
                },
                deadline,
                &self.stopped,
            );
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
        deadline: Instant,
    ) -> Result<Value> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let host = Arc::clone(
            &instances
                .get(&instance_id)
                .and_then(Option::as_ref)
                .ok_or_else(|| rejected("unknown_instance"))?
                .host,
        );
        if value["agent"]["instance_id"] != instance_id {
            return Err(rejected("event_instance_mismatch"));
        }
        let metadata = event_metadata(&value)?;
        let key = self.identifier()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel(MAX_STEPS + 1);
        let (ready, result) = mpsc::sync_channel(1);
        {
            let mut events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
            if events.len() >= MAX_EVENTS {
                return Err(rejected("event_limit"));
            }
            events.insert(
                key.clone(),
                EventEntry {
                    instance_id: instance_id.clone(),
                    sender,
                    cancelled: Arc::clone(&cancelled),
                },
            );
        }
        drop(instances);
        let runtime = Arc::clone(self);
        let thread_key = key.clone();
        let worker = thread::Builder::new().name("aw-event".into()).spawn(move || {
            let event = host.event(value, deadline, &cancelled).map_err(|_| rejected("event_invalid"));
            match event {
                Ok(event) => {
                    let handle = EventHandle { event_id: thread_key.clone(), instance_id: instance_id.clone(), steps: event.steps().map(|step| step.step_id.clone()).collect() };
                    let claimed = runtime.audit.claim(&thread_key, json!({"kind": "event", "identity": runtime.identity,
                        "instance_id": instance_id, "binding_id": host.target(), "host_event_id": event.id(), "event": metadata}));
                    match claimed {
                        Ok(_) => {
                            let _ = ready.send(Ok(json!(handle)));
                            runtime.event_loop(&thread_key, &event, &cancelled, receiver);
                        }
                        Err(error) => { let _ = ready.send(Err(error)); }
                    }
                }
                Err(error) => { let _ = ready.send(Err(error)); }
            }
            runtime.audit.release(&thread_key);
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
        key: &str,
        event: &aw_host::Event<'_>,
        cancelled: &AtomicBool,
        receiver: Receiver<Job>,
    ) {
        let mut close = None;
        let mut claimed = BTreeSet::new();
        let mut panicked = false;
        thread::scope(|scope| {
            let mut children = Vec::new();
            while Instant::now() < event.deadline()
                && !self.stopped.load(Ordering::Acquire)
                && !cancelled.load(Ordering::Acquire)
            {
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
                    Job::Close { reply } => {
                        close = Some(reply);
                        break;
                    }
                    Job::Invoke { step, reply } => {
                        if cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire)
                        {
                            let _ = reply.send(Err(rejected("event_cancelled")));
                            continue;
                        }
                        if !event.steps().any(|candidate| candidate.step_id == step)
                            || !claimed.insert(step.clone())
                        {
                            let _ = reply.send(Err(rejected("step_unknown_or_claimed")));
                            continue;
                        }
                        if self
                            .calls
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                (n < MAX_CALLS as u64).then_some(n + 1)
                            })
                            .is_err()
                        {
                            let _ = reply.send(Err(rejected("call_limit")));
                            continue;
                        }
                        children.push(scope.spawn(move || {
                            let _guard = CallGuard(&self.calls);
                            let result = (|| {
                                self.audit
                                    .append(key, json!({"phase": "started", "step_id": step}))?;
                                let invocation =
                                    event.invoke(&step).map_err(|_| rejected("step_rejected"))?;
                                let (record, result) = audit::invocation(invocation);
                                self.audit.append(key, record)?;
                                Ok(result)
                            })();
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
        let completed = self.audit.append(key, json!({"phase": "closed", "status": if panicked { "worker_failed" } else if close.is_some() { "closed" } else if self.stopped.load(Ordering::Acquire) { "stopped" } else if Instant::now() >= event.deadline() { "expired" } else { "cancelled" }, "attempted_steps": claimed.len()}));
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
        if shutdown {
            self.stopped.store(true, Ordering::Release);
            let mut events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
            for entry in events.values() {
                entry.cancelled.store(true, Ordering::Release);
            }
            // Dropping registry senders wakes idle event workers; in-flight handlers are bounded.
            events.clear();
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
    Ok(json!({"name": fields[0], "session_id": fields[1], "tool": {
        "name": fields[2], "native_name": fields[3], "call_id": fields[4],
    }}))
}
