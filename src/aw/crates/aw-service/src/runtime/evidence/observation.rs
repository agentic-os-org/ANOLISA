//! Instance-owned optional reads outlive callbacks, but never outlive bounded release.

use super::{association, contract, now_ms, projection, Seed};
use crate::{rejected, runtime::Runtime, Result, RunOutcome};
use aw_contracts::canonical;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_CALLBACKS: usize = 256;
const QUERY_BUDGET: Duration = Duration::from_secs(1);

pub(in crate::runtime) struct Observation {
    key: String,
    closing: AtomicBool,
    starting: AtomicBool,
    sender: SyncSender<Seed>,
    worker: Mutex<Option<JoinHandle<()>>>,
    state: Mutex<State>,
}

struct State {
    metadata: Value,
    latest: Option<Value>,
    claims: BTreeMap<String, String>,
    callbacks: usize,
    closed: bool,
}

struct Startup(Arc<Observation>);
impl Drop for Startup {
    fn drop(&mut self) {
        self.0.starting.store(false, Ordering::Release);
    }
}

impl Observation {
    fn metadata(&self) -> Result<Value> {
        Ok(self
            .state
            .lock()
            .map_err(|_| rejected("observation_state"))?
            .metadata
            .clone())
    }

    fn gap(&self, runtime: &Runtime, reason: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| rejected("observation_state"))?;
        let gaps = state.metadata["gaps"]
            .as_array_mut()
            .ok_or_else(|| rejected("observation_state"))?;
        if !gaps.iter().any(|gap| gap == reason) {
            gaps.push(json!(reason));
            runtime.audit.append(
                &self.key,
                json!({"phase":"observation_gap", "reason":reason}),
            )?;
        }
        Ok(())
    }

    fn enqueue(&self, runtime: &Runtime, seed: &Seed) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(rejected("instance_draining"));
        }
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| rejected("observation_state"))?;
            if self.closing.load(Ordering::Acquire) || state.closed {
                return Err(rejected("instance_draining"));
            }
            if state.callbacks >= MAX_CALLBACKS {
                drop(state);
                return self.gap(runtime, "observation_callback_limit");
            }
            runtime.audit.append(&self.key, json!({"phase":"native_callback", "event_id":seed.subject["event_id"],
                "candidate_revision":seed.candidate["digest"], "association":seed.association, "received_at_ms":seed.received_at_ms}))?;
            state.callbacks += 1;
            state.metadata["callbacks"] = json!(state.callbacks);
            state.metadata["association"] = seed.association.clone();
        }
        if seed.association["status"] != "verified" {
            return self.gap(runtime, "native_identity_unverified");
        }
        let bytes = serde_json::to_vec(&seed.candidate)
            .map_err(|_| rejected("observation_encoding"))?
            .len()
            + serde_json::to_vec(&seed.constraints)
                .map_err(|_| rejected("observation_encoding"))?
                .len();
        if bytes
            > seed
                .settings
                .as_ref()
                .map_or(262144, |settings| settings.max_bytes)
        {
            return self.gap(runtime, "observation_input_limit");
        }
        match self.sender.try_send(seed.clone()) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => self.gap(runtime, "observation_queue_full"),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.gap(runtime, "observation_worker_unavailable")
            }
        }
    }

    fn run(&self, runtime: &Runtime, receiver: Receiver<Seed>) -> Result<()> {
        loop {
            if runtime.stopped.load(Ordering::Acquire) {
                break;
            }
            let seed = if self.closing.load(Ordering::Acquire) {
                match receiver.try_recv() {
                    Ok(seed) => seed,
                    Err(_) => break,
                }
            } else {
                match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(seed) => seed,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            };
            let budget = seed.settings.as_ref().map_or(QUERY_BUDGET, |settings| {
                Duration::from_millis(settings.query_timeout_ms).min(QUERY_BUDGET)
            });
            let deadline = seed.deadline.min(Instant::now() + budget);
            if Instant::now() >= deadline {
                self.gap(runtime, "observation_deadline")?;
                continue;
            }
            let (query, status, gap) = runtime.contexts.query(&seed, deadline);
            let projected = projection::snapshot(&seed, query, status, gap, deadline);
            let mut state = self
                .state
                .lock()
                .map_err(|_| rejected("observation_state"))?;
            state.metadata["source_state"] = json!(status);
            state.metadata["source_reason"] = json!(gap);
            state.metadata["observed_at_ms"] = json!(now_ms()?);
            match projected {
                Ok(value) => {
                    aw_contracts::evidence::validate_context(
                        &runtime.contexts.registry,
                        &value,
                        &seed.subject,
                        seed.candidate["digest"]
                            .as_str()
                            .ok_or_else(|| rejected("context_encoding"))?,
                    )
                    .map_err(contract)?;
                    runtime.audit.append(&self.key, json!({"phase":"evidence_observed", "event_id":seed.subject["event_id"],
                        "projection_revision":value["context_revision"], "source_revision":value["source_revision"],
                        "source_state":status,"gaps":value["gaps"]}))?;
                    state.metadata["projection_revision"] = value["context_revision"].clone();
                    state.metadata["source_revision"] = value["source_revision"].clone();
                    state.metadata["evidence_gaps"] = value["gaps"].clone();
                    state.latest = Some(value);
                }
                Err(_) => {
                    state.latest = None;
                    state.metadata["projection_revision"] = Value::Null;
                    state.metadata["source_revision"] = Value::Null;
                    drop(state);
                    self.gap(runtime, "observation_projection_unavailable")?;
                }
            }
        }
        Ok(())
    }

    fn finish(&self, runtime: &Runtime) -> Result<bool> {
        self.closing.store(true, Ordering::Release);
        if self.starting.load(Ordering::Acquire) {
            return Ok(false);
        }
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| rejected("observation_state"))?;
        if worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return Ok(false);
        }
        if let Some(worker) = worker.take() {
            worker
                .join()
                .map_err(|_| rejected("observation_worker_failed"))?;
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| rejected("observation_state"))?;
        if !state.closed {
            runtime.audit.append(&self.key, json!({"phase":"closed", "outcome":state.metadata["run"],
                "source_state":state.metadata["source_state"],"gaps":state.metadata["gaps"], "coverage":"unknown"}))?;
            runtime.audit.release(&self.key);
            state.latest = None;
            state.closed = true;
        }
        Ok(true)
    }
}

impl Runtime {
    pub(in crate::runtime) fn start_observation(
        self: &Arc<Self>,
        instance_id: &str,
        deadline: Instant,
    ) -> Result<Value> {
        let (observation, receiver, settings) = {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get_mut(instance_id)
                .and_then(Option::as_mut)
                .filter(|instance| !instance.closing)
                .ok_or_else(|| rejected("unknown_instance"))?;
            if let Some(observation) = &instance.observation {
                return observation.metadata();
            }
            let Some(settings) = self
                .contexts
                .sources
                .get(instance.host.target())
                .filter(|settings| settings.mode != "off")
                .cloned()
            else {
                return Ok(
                    json!({"audit_key":null,"source_state":"disabled","query_state":"disabled","coverage":"unknown"}),
                );
            };
            let key = self.identifier()?;
            let (sender, receiver) = mpsc::sync_channel(2);
            self.audit.claim(
                &key,
                json!({"kind":"run", "identity":self.identity, "instance_id":instance_id,
                "binding_id":instance.host.target(),"phase":"launch_prepared"}),
            )?;
            let observation = Arc::new(Observation {
                key: key.clone(),
                closing: AtomicBool::new(false),
                starting: AtomicBool::new(true),
                sender,
                worker: Mutex::new(None),
                state: Mutex::new(State {
                    metadata: json!({"audit_key":key,"instance_id":instance_id,
                    "source_state":"unknown","query_state":"unknown","source_reason":"awaiting_native_identity",
                    "coverage":"unknown","ebpf":"unknown","collection":"unknown","callbacks":0,
                    "gaps":["coverage_unknown","source_process_identity_unavailable"],"run":{"state":"launch_prepared"}}),
                    latest: None,
                    claims: BTreeMap::new(),
                    callbacks: 0,
                    closed: false,
                }),
            });
            instance.observation = Some(Arc::clone(&observation));
            (observation, receiver, settings)
        };
        let _startup = Startup(Arc::clone(&observation));
        let deadline = deadline.min(
            Instant::now() + QUERY_BUDGET.min(Duration::from_millis(settings.query_timeout_ms)),
        );
        let probe = settings
            .client()
            .and_then(|client| client.probe_until(deadline, settings.query_bytes));
        {
            let mut state = observation
                .state
                .lock()
                .map_err(|_| rejected("observation_state"))?;
            state.metadata["query_state"] = json!(if probe.is_ok() {
                "ready"
            } else {
                "unavailable"
            });
            if let Err(error) = probe {
                state.metadata["source_reason"] = json!(super::source_failure(&error));
            }
            self.audit.append(
                &observation.key,
                json!({"phase":"observation_prepared","query_state":state.metadata["query_state"],
                "source_reason":state.metadata["source_reason"],"coverage":"unknown"}),
            )?;
        }
        let runtime = Arc::clone(self);
        let owner = Arc::clone(&observation);
        let worker = thread::Builder::new()
            .name("aw-observation".into())
            .spawn(move || {
                if owner.run(&runtime, receiver).is_err() {
                    let _ = owner.gap(&runtime, "observation_worker_failed");
                }
            })?;
        *observation
            .worker
            .lock()
            .map_err(|_| rejected("observation_state"))? = Some(worker);
        observation.metadata()
    }

    fn observation(&self, instance_id: &str) -> Result<Option<Arc<Observation>>> {
        Ok(self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?
            .get(instance_id)
            .and_then(Option::as_ref)
            .ok_or_else(|| rejected("unknown_instance"))?
            .observation
            .clone())
    }

    pub(in crate::runtime) fn observation_status(&self) -> Result<Vec<Value>> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        instances
            .values()
            .filter_map(|instance| instance.as_ref()?.observation.as_ref())
            .map(|o| o.metadata())
            .collect()
    }

    pub(in crate::runtime) fn observe_seed(&self, instance_id: &str, seed: &Seed) -> Result<()> {
        if let Some(observation) = self.observation(instance_id)? {
            observation.enqueue(self, seed)?;
        }
        Ok(())
    }

    pub(in crate::runtime) fn observe_hook(
        &self,
        instance_id: &str,
        event: Value,
        native: Vec<u8>,
        deadline: Instant,
    ) -> Result<Value> {
        let (observation, host) = {
            let instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get(instance_id)
                .and_then(Option::as_ref)
                .filter(|instance| !instance.closing)
                .ok_or_else(|| rejected("unknown_instance"))?;
            (
                instance
                    .observation
                    .clone()
                    .ok_or_else(|| rejected("observation_disabled"))?,
                Arc::clone(&instance.host),
            )
        };
        if !matches!(event["name"].as_str(), Some("tool.before" | "tool.after"))
            || event["agent"]["instance_id"] != instance_id
            || event["agent"]["binding_id"] != host.target()
            || event["agent"]["adapter"] != host.capabilities().adapter
            || host.steps().iter().any(|step| event["name"] == step.event)
            || native.len() > aw_provider::MAX_MESSAGE_BYTES
            || serde_json::to_vec(&event)
                .map_err(|_| rejected("observation_encoding"))?
                .len()
                > aw_provider::MAX_MESSAGE_BYTES
        {
            return Err(rejected("observation_callback_invalid"));
        }
        let association = association(&host.capabilities().adapter, &event, Some(&native));
        if association["status"] != "verified" {
            return Err(rejected("native_identity_unverified"));
        }
        let correlation =
            canonical::document_digest(&json!([event["name"], association])).map_err(contract)?;
        let digest = canonical::digest(&native);
        {
            let mut state = observation
                .state
                .lock()
                .map_err(|_| rejected("observation_state"))?;
            if let Some(previous) = state.claims.get(&correlation) {
                return if previous == &digest {
                    Ok(json!({"observed":true,"duplicate":true}))
                } else {
                    Err(rejected("native_correlation_mismatch"))
                };
            }
            if state.claims.len() >= MAX_CALLBACKS {
                drop(state);
                observation.gap(self, "observation_callback_limit")?;
                return Err(rejected("observation_callback_limit"));
            }
            state.claims.insert(correlation, digest);
        }
        let seed = self.contexts.seed(json!({"generation":self.identity.generation,"config_revision":self.identity.config_revision,
            "binding_id":host.target(),"instance_id":instance_id,"event_id":self.identifier()?,"adapter":host.capabilities().adapter}),
            &event, association, deadline)?;
        observation.enqueue(self, &seed)?;
        Ok(json!({"observed":true,"duplicate":false}))
    }

    pub(in crate::runtime) fn report_run(
        &self,
        instance_id: &str,
        outcome: RunOutcome,
    ) -> Result<Value> {
        let observation = self
            .observation(instance_id)?
            .ok_or_else(|| rejected("observation_disabled"))?;
        let mut state = observation
            .state
            .lock()
            .map_err(|_| rejected("observation_state"))?;
        if observation.closing.load(Ordering::Acquire) || state.closed {
            return Err(rejected("instance_draining"));
        }
        let current = state.metadata["run"]["state"].as_str().unwrap_or("");
        let valid = match &outcome {
            RunOutcome::Started { pid } => current == "launch_prepared" && *pid > 0,
            RunOutcome::Exited { code, signal } => {
                current == "started"
                    && matches!((code, signal), (Some(0..=255), None) | (None, Some(1..=64)))
            }
            RunOutcome::Failed => matches!(current, "launch_prepared" | "started"),
        };
        if !valid {
            return Err(rejected("run_transition_invalid"));
        }
        self.audit.append(
            &observation.key,
            json!({"phase":"run_reported","outcome":outcome}),
        )?;
        state.metadata["run"] =
            serde_json::to_value(outcome).map_err(|_| rejected("observation_encoding"))?;
        Ok(json!({"recorded":true}))
    }

    pub(in crate::runtime) fn observation_metadata(
        &self,
        instance_id: &str,
    ) -> Result<Option<Value>> {
        self.observation(instance_id)?
            .map(|observation| observation.metadata())
            .transpose()
    }

    pub(in crate::runtime) fn finish_observation(&self, instance_id: &str) -> Result<bool> {
        match self.observation(instance_id)? {
            Some(observation) => observation.finish(self),
            None => Ok(true),
        }
    }

    pub(in crate::runtime) fn release_source_sessions(&self, instance_id: &str) -> Result<()> {
        let mut sessions = self
            .contexts
            .sessions
            .lock()
            .map_err(|_| rejected("context_state"))?;
        // Closing and releasing claims are atomic with query admission. A delayed
        // freeze request cannot reclaim a session after the instance has drained.
        sessions.active.remove(instance_id);
        sessions.owners.retain(|_, owner| owner != instance_id);
        Ok(())
    }

    pub(in crate::runtime) fn stop_observations(&self) -> Result<()> {
        let observations: Vec<_> = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?
            .values()
            .filter_map(|instance| instance.as_ref()?.observation.clone())
            .collect();
        for observation in &observations {
            observation.closing.store(true, Ordering::Release);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        for observation in observations {
            while !observation.finish(self)? {
                if Instant::now() >= deadline {
                    return Err(rejected("observation_shutdown_timeout"));
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }
}
