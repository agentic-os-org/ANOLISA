//! Service-owned evidence selection; native dispatch never depends on these optional reads.

mod association;
mod observation;
mod projection;
pub(super) use observation::Observation;

#[cfg(test)]
mod tests;

use super::Runtime;
use crate::{agentsight, deadline, rejected, Result};
use aw_contracts::{canonical, Registry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) use association::association;

#[derive(Clone, Deserialize)]
struct Settings {
    #[serde(default = "auto_mode")]
    mode: String,
    source_id: String,
    endpoint: String,
    token_file: Option<PathBuf>,
    query_timeout_ms: u64,
    query_bytes: usize,
    max_items: usize,
    max_bytes: usize,
}

impl Settings {
    fn client(&self) -> std::result::Result<agentsight::Client, agentsight::Error> {
        // The connector ignores this path for loopback HTTP; HTTPS requires it at admission.
        agentsight::Client::connect(
            &self.endpoint,
            self.token_file.as_deref().unwrap_or_else(|| Path::new("")),
        )
    }
}

fn source_matches_adapter(adapter: &Value, name: &Value) -> bool {
    // Source labels corroborate the framework only, never the native session or process.
    matches!(
        (adapter.as_str(), name.as_str()),
        (Some("qoder"), Some("qoder" | "Qoder"))
            | (Some("hermes"), Some("hermes" | "Hermes"))
            | (Some("openclaw"), Some("openclaw" | "OpenClaw"))
            | (Some("qwenpaw"), Some("qwenpaw"))
    )
}

pub(super) struct Contexts {
    document: Value,
    sources: BTreeMap<String, Settings>,
    registry: Registry,
    // Claims live until the owning instance has drained all its observation work.
    sessions: Mutex<SessionClaims>,
}

#[derive(Default)]
struct SessionClaims {
    active: BTreeSet<String>,
    owners: BTreeMap<String, String>,
}

#[derive(Clone)]
pub(super) struct Seed {
    subject: Value,
    candidate: Value,
    constraints: Value,
    association: Value,
    settings: Option<Settings>,
    deadline: Instant,
    deadline_ns: String,
    received_at_ms: u64,
    after: bool,
}

#[derive(Default)]
pub(super) struct Cell {
    seed: OnceLock<Seed>,
    state: Mutex<State>,
}

#[derive(Default)]
enum State {
    #[default]
    Empty,
    Loading,
    Ready(Value),
    Failed,
    Closed,
}

impl Cell {
    pub(super) fn initialize(&self, seed: Seed) -> Result<String> {
        let revision = seed.candidate["digest"]
            .as_str()
            .ok_or_else(|| rejected("context_encoding"))?
            .to_owned();
        self.seed
            .set(seed)
            .map_err(|_| rejected("context_already_open"))?;
        Ok(revision)
    }

    pub(super) fn seed(&self) -> Option<&Seed> {
        self.seed.get()
    }

    pub(super) fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = State::Closed;
        }
    }
}

impl Contexts {
    pub(super) fn new(bytes: &[u8]) -> Result<Self> {
        let document = aw_config::Validator::new()?
            .parse(bytes)?
            .as_value()
            .clone();
        let mut sources = BTreeMap::new();
        for (target, agent) in document["spec"]["agents"]
            .as_object()
            .ok_or_else(|| rejected("context_configuration"))?
        {
            if let Some(value) = agent.get("evidence") {
                let mut settings: Settings = serde_json::from_value(value.clone())
                    .map_err(|_| rejected("context_configuration"))?;
                settings.endpoint = agentsight::origin(&settings.endpoint)
                    .map_err(|_| rejected("evidence_endpoint_invalid"))?;
                if settings.endpoint.starts_with("https://")
                    && !settings
                        .token_file
                        .as_deref()
                        .is_some_and(Path::is_absolute)
                {
                    return Err(rejected("evidence_token_path_not_absolute"));
                }
                sources.insert(target.clone(), settings);
            }
        }
        Ok(Self {
            document,
            sources,
            registry: Registry::new().map_err(contract)?,
            sessions: Mutex::new(SessionClaims::default()),
        })
    }

    pub(super) fn activate(&self, instance: &str) -> Result<()> {
        self.sessions
            .lock()
            .map_err(|_| rejected("context_state"))?
            .active
            .insert(instance.into());
        Ok(())
    }

    pub(super) fn seed(
        &self,
        subject: Value,
        event: &Value,
        association: Value,
        deadline: Instant,
    ) -> Result<Seed> {
        let target = subject["binding_id"]
            .as_str()
            .ok_or_else(|| rejected("context_subject"))?;
        let constraints = projection::content(&json!({
            "execution": self.document["spec"]["execution"],
            "events": self.document["spec"]["events"],
            "config_revision": subject["config_revision"],
        }))?;
        Ok(Seed {
            settings: self.sources.get(target).cloned(),
            subject,
            candidate: projection::content(&json!({"name":event["name"], "tool":event["tool"]}))?,
            constraints,
            association,
            deadline,
            deadline_ns: deadline::encode(deadline)?.to_string(),
            received_at_ms: now_ms()?,
            after: event["name"] == "tool.after",
        })
    }

    fn query(
        &self,
        seed: &Seed,
        deadline: Instant,
    ) -> (Option<agentsight::QueryResult>, &'static str, &'static str) {
        let Some(settings) = &seed.settings else {
            return (None, "not_configured", "source_not_configured");
        };
        if settings.mode == "off" {
            return (None, "not_queried", "source_disabled");
        }
        if seed.association["status"] != "verified" {
            return (None, "not_queried", "native_identity_unverified");
        }
        let Some(session) = seed.association["session_id"].as_str() else {
            return (None, "not_queried", "native_identity_unverified");
        };
        let ownership = (|| {
            let key = canonical::document_digest(&json!([
                settings.endpoint,
                seed.subject["adapter"],
                session
            ]))
            .map_err(contract)?;
            let instance = seed.subject["instance_id"]
                .as_str()
                .ok_or_else(|| rejected("context_subject"))?;
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| rejected("context_state"))?;
            if !sessions.active.contains(instance) {
                return Err(rejected("source_instance_closed"));
            }
            if let Some(owner) = sessions.owners.get(&key) {
                if owner != instance {
                    return Err(rejected("source_session_conflict"));
                }
            } else {
                if sessions.owners.len() >= 1024 {
                    return Err(rejected("source_session_limit"));
                }
                sessions.owners.insert(key, instance.into());
            }
            Ok(())
        })();
        if let Err(error) = ownership {
            return (
                None,
                "not_queried",
                match error {
                    crate::Error::Rejected(code) if code == "source_instance_closed" => {
                        "source_instance_closed"
                    }
                    crate::Error::Rejected(code) if code == "source_session_conflict" => {
                        "source_session_conflict"
                    }
                    crate::Error::Rejected(code) if code == "source_session_limit" => {
                        "source_session_limit"
                    }
                    _ => "source_association_unavailable",
                },
            );
        }
        let result = settings
            .client()
            .and_then(|client| client.trajectory_until(session, deadline, settings.query_bytes));
        match result {
            Ok(query) => {
                let (status, gap) = match &query.outcome {
                    agentsight::Outcome::Available { trajectory, .. }
                        if source_matches_adapter(
                            &seed.subject["adapter"],
                            &trajectory["agent"]["name"],
                        ) =>
                    {
                        ("available", "coverage_unknown")
                    }
                    agentsight::Outcome::Available { .. } => {
                        return (None, "unknown", "source_adapter_mismatch")
                    }
                    agentsight::Outcome::Unknown { .. } => {
                        ("unknown", "record_or_store_unavailable")
                    }
                };
                (Some(query), status, gap)
            }
            Err(error) => (None, "unavailable", source_failure(&error)),
        }
    }
}

impl Runtime {
    pub(super) fn freeze_context(
        &self,
        key: &str,
        instance: &str,
        revision: &str,
        deadline: Instant,
    ) -> Result<Value> {
        let (cell, cancelled) = {
            let events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
            let entry = events
                .get(key)
                .filter(|entry| entry.instance_id == instance)
                .ok_or_else(|| rejected("unknown_event"))?;
            (Arc::clone(&entry.context), Arc::clone(&entry.cancelled))
        };
        let seed = cell
            .seed
            .get()
            .ok_or_else(|| rejected("context_not_ready"))?;
        let deadline = deadline.min(seed.deadline);
        check(deadline, &cancelled, &self.stopped)?;
        if seed.candidate["digest"] != revision {
            return Err(rejected("stale_candidate"));
        }
        {
            let mut state = cell.state.lock().map_err(|_| rejected("context_state"))?;
            match &*state {
                State::Ready(value) => return Ok(value.clone()),
                State::Empty => *state = State::Loading,
                State::Loading => return Err(rejected("context_busy")),
                _ => return Err(rejected("context_closed_or_failed")),
            }
        }
        let query_deadline = deadline.min(
            Instant::now()
                + Duration::from_millis(
                    seed.settings.as_ref().map_or(1000, |s| s.query_timeout_ms),
                ),
        );
        let (query, status, gap) = self.contexts.query(seed, query_deadline);
        let prepared = check(deadline, &cancelled, &self.stopped)
            .and_then(|()| projection::snapshot(seed, query, status, gap, deadline))
            .and_then(|value| {
                aw_contracts::evidence::validate_context(
                    &self.contexts.registry,
                    &value,
                    &seed.subject,
                    revision,
                )
                .map_err(contract)?;
                Ok(value)
            });
        // Event closure takes this same short lock before journaling its terminal
        // record. HTTP reads hold no event, instance, audit or snapshot lock.
        let mut state = cell.state.lock().map_err(|_| rejected("context_state"))?;
        if matches!(*state, State::Closed) {
            return Err(rejected("context_closed_or_failed"));
        }
        *state = State::Failed;
        check(deadline, &cancelled, &self.stopped)?;
        let value = prepared?;
        self.audit.append(key, json!({
            "phase":"context_frozen", "context_revision":value["context_revision"],
            "candidate_revision":revision, "source_revision":value["source_revision"],
            "evidence_refs":value["evidence"].as_array().ok_or_else(|| rejected("context_encoding"))?.iter().map(|e| &e["evidence_id"]).collect::<Vec<_>>(),
            "gaps":value["gaps"], "projection_version":value["projection_version"],
        }))?;
        check(deadline, &cancelled, &self.stopped)?;
        *state = State::Ready(value.clone());
        Ok(value)
    }
}

fn check(deadline: Instant, cancelled: &AtomicBool, stopped: &AtomicBool) -> Result<()> {
    if Instant::now() >= deadline
        || cancelled.load(Ordering::Acquire)
        || stopped.load(Ordering::Acquire)
    {
        return Err(rejected("context_cancelled_or_expired"));
    }
    Ok(())
}

fn now_ms() -> Result<u64> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| rejected("clock_range"))?
        .as_millis();
    if ms > canonical::MAX_SAFE_INTEGER as u128 {
        return Err(rejected("clock_range"));
    }
    Ok(ms as u64)
}

fn contract(_: aw_contracts::Error) -> crate::Error {
    rejected("evidence_contract")
}

fn auto_mode() -> String {
    "auto".into()
}

fn source_failure(error: &agentsight::Error) -> &'static str {
    match error {
        agentsight::Error::Deadline => "source_deadline",
        agentsight::Error::Limit => "source_limit",
        agentsight::Error::Authentication | agentsight::Error::Credential => {
            "source_authentication"
        }
        agentsight::Error::UnsupportedVersion => "source_version",
        agentsight::Error::InvalidResponse => "source_invalid",
        _ => "source_unavailable",
    }
}
