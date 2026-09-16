//! `SkillFS` compatibility boundary: authenticated notify, configured resolver and one background worker.

mod auth;
mod resolver;
#[cfg(test)]
mod tests;
mod worker;

use asc_action_runtime::Finalizer;
use asc_action_types::CallerIdentity;
use asc_capability_skill_guard::{GuardError, SkillGuardService, SkillIdentity, SkillRoot};
use asc_daemon_service::{ConnectionSession, DispatchError, PeerCredentials, SessionStep};
use auth::{Frame, NOTIFY_CLIENT, NOTIFY_SERVER};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// Root-owned configuration for `SkillFS` mounts sharing the daemon's public socket.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillFsConfig {
    /// Separate HMAC secret; never the `SkillGuard` signing key.
    pub auth_key_file: PathBuf,
    /// Explicit canonical/live bindings, authenticated by the configured kernel UID.
    pub mounts: Vec<Mount>,
}

/// One `SkillFS` instance and its shared backing directory.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mount {
    /// Private `SkillFS` control endpoint, owned by `peer_uid`.
    pub control_socket: PathBuf,
    /// Canonical identity prefix exposed to consumers.
    pub canonical_root: PathBuf,
    /// Physical backing prefix visible to the root daemon.
    pub live_root: PathBuf,
    /// Kernel UID of the `SkillFS` process, used in both directions.
    pub peer_uid: u32,
}

/// Errors at the authenticated transport and configured mapping boundary.
#[derive(Debug, thiserror::Error)]
pub enum SkillFsError {
    /// Invalid peer, handshake or business-frame authentication.
    #[error("SkillFS authentication failed")]
    Authentication,
    /// Bounded control call expired.
    #[error("SkillFS control deadline exceeded")]
    Timeout,
    /// Invalid configuration or resolver response.
    #[error("SkillFS: {0}")]
    Invalid(&'static str),
    /// The background worker cannot currently admit a notification.
    #[error("SkillFS: {0}")]
    Unavailable(&'static str),
    /// Socket or filesystem failure.
    #[error("SkillFS I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid bounded JSON.
    #[error("SkillFS JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Canonical identity or physical directory validation failed.
    #[error(transparent)]
    Guard(#[from] GuardError),
}

impl From<rustix::io::Errno> for SkillFsError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}
impl From<std::io::ErrorKind> for SkillFsError {
    fn from(error: std::io::ErrorKind) -> Self {
        Self::Io(error.into())
    }
}

/// Daemon-owned bridge sharing the same service and audit finalizer as interactive commands.
pub struct SkillFsBridge {
    resolver: Arc<resolver::Resolver>,
    queue: Arc<worker::Queue>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl SkillFsBridge {
    /// Validates mount bindings, loads the HMAC key and starts one bounded background worker.
    ///
    /// # Errors
    /// Rejects overlapping roots, unsafe keys and worker startup failures.
    pub fn start(
        config: SkillFsConfig,
        service: Arc<SkillGuardService>,
        finalizer: Finalizer,
    ) -> Result<Self, SkillFsError> {
        validate_config(&config)?;
        let secret = resolver::load_secret(&config.auth_key_file)?;
        let resolver = Arc::new(resolver::Resolver {
            mounts: config.mounts,
            secret,
        });
        let queue = Arc::new(worker::Queue::default());
        let handle = worker::start(queue.clone(), resolver.clone(), service, finalizer)?;
        Ok(Self {
            resolver,
            queue,
            worker: Mutex::new(Some(handle)),
        })
    }

    /// Resolves source/live aliases before taking a Skill lock; never falls back inside a mount.
    ///
    /// # Errors
    /// Rejects unavailable/untrusted peers, incompatible mappings and stale directory identities.
    pub fn resolve(
        &self,
        identity: &SkillIdentity,
        deadline: Instant,
    ) -> Result<SkillRoot, GuardError> {
        self.resolver
            .resolve(identity, deadline)
            .map_err(|e| GuardError::Integrity(e.to_string()))
    }

    /// Reschedules explicitly registered mount Skills after startup recovery, without replaying events.
    /// Call once before socket admission. The source registry already has an 8 MiB storage bound.
    ///
    /// # Errors
    /// Reports a stopped or poisoned worker.
    pub fn schedule_reconcile(&self, identities: Vec<SkillIdentity>) -> Result<(), SkillFsError> {
        let identities = identities
            .into_iter()
            .filter(|identity| {
                self.resolver
                    .mounts
                    .iter()
                    .any(|mount| identity.path().starts_with(&mount.canonical_root))
            })
            .collect();
        self.queue.schedule_startup(identities)
    }

    /// Bounded operational counters; accepting a notify does not mean activation has finished.
    pub fn status(&self) -> Value {
        self.queue.status()
    }

    /// Stops admission to the queue and joins the current bounded operation.
    /// Registered Skills are rescheduled at daemon startup; mounts notify newly discovered Skills.
    ///
    /// # Errors
    /// Reports poisoned state or a worker panic.
    pub fn shutdown(&self) -> Result<(), SkillFsError> {
        self.queue.stop();
        if let Some(worker) = self
            .worker
            .lock()
            .map_err(|_| SkillFsError::Invalid("worker state poisoned"))?
            .take()
        {
            worker
                .join()
                .map_err(|_| SkillFsError::Invalid("worker failed"))?;
        }
        Ok(())
    }

    pub(crate) fn start_session(
        &self,
        peer: PeerCredentials,
        payload: &[u8],
    ) -> Result<Option<asc_daemon_service::StartedSession>, DispatchError> {
        let is_auth = serde_json::from_slice::<Value>(payload)
            .ok()
            .is_some_and(|v| v.get("type").is_some());
        if !is_auth {
            return Ok(None);
        }
        let begin = || -> Result<_, SkillFsError> {
            Frame::parse(payload, "auth.init")?;
            if !self
                .resolver
                .mounts
                .iter()
                .any(|m| m.peer_uid == peer.uid())
            {
                return Err(SkillFsError::Authentication);
            }
            let nonce = auth::nonce()?;
            Ok((
                Session {
                    resolver: self.resolver.clone(),
                    queue: self.queue.clone(),
                    peer,
                    nonce,
                    state: State::Proof,
                },
                SessionStep {
                    responses: vec![Frame::encode("auth.challenge", Some(&nonce), None)],
                    complete: false,
                },
            ))
        };
        let (session, step) = begin().map_err(|_| DispatchError)?;
        Ok(Some((Box::new(session), step)))
    }
}

impl Drop for SkillFsBridge {
    fn drop(&mut self) {
        self.queue.stop();
    }
}

fn validate_config(config: &SkillFsConfig) -> Result<(), SkillFsError> {
    SkillIdentity::new(&config.auth_key_file)?;
    if config.mounts.is_empty() || config.mounts.len() > 64 {
        return Err(SkillFsError::Invalid("configure 1..64 mounts"));
    }
    let mut prefixes: Vec<&Path> = Vec::new();
    for mount in &config.mounts {
        SkillIdentity::new(&mount.control_socket)?;
        // An ordinary (non-in-place) mount exposes its source directly through shared_path.
        if mount.canonical_root != mount.live_root
            && (mount.canonical_root.starts_with(&mount.live_root)
                || mount.live_root.starts_with(&mount.canonical_root))
        {
            return Err(SkillFsError::Invalid(
                "mount roots must be equal or disjoint",
            ));
        }
        for path in [&mount.canonical_root, &mount.live_root] {
            SkillIdentity::new(path)?;
            if prefixes
                .iter()
                .any(|other| path.starts_with(other) || other.starts_with(path))
            {
                return Err(SkillFsError::Invalid(
                    "roots belonging to different mounts must not overlap",
                ));
            }
        }
        prefixes.extend([mount.canonical_root.as_path(), mount.live_root.as_path()]);
    }
    Ok(())
}

enum State {
    Proof,
    Payload,
    Tag(Vec<u8>),
    Complete,
}
struct Session {
    resolver: Arc<resolver::Resolver>,
    queue: Arc<worker::Queue>,
    peer: PeerCredentials,
    nonce: [u8; 32],
    state: State,
}
impl ConnectionSession for Session {
    fn advance(&mut self, payload: &[u8]) -> Result<SessionStep, DispatchError> {
        self.advance_authenticated(payload)
            .map_err(|_| DispatchError)
    }
}
impl Session {
    fn advance_authenticated(&mut self, payload: &[u8]) -> Result<SessionStep, SkillFsError> {
        let state = std::mem::replace(&mut self.state, State::Complete);
        match state {
            State::Proof => {
                let proof = Frame::parse(payload, "auth.proof")?.proof()?;
                auth::verify(
                    &self.resolver.secret,
                    NOTIFY_CLIENT,
                    &self.nonce,
                    None,
                    &proof,
                )?;
                let tag = auth::sign(&self.resolver.secret, NOTIFY_SERVER, &self.nonce, None);
                self.state = State::Payload;
                Ok(SessionStep {
                    responses: vec![Frame::encode("auth.ok", None, Some(tag.as_ref()))],
                    complete: false,
                })
            }
            State::Payload => {
                self.state = State::Tag(payload.to_vec());
                Ok(SessionStep {
                    responses: Vec::new(),
                    complete: false,
                })
            }
            State::Tag(request) => {
                let tag = Frame::parse(payload, "auth.frame")?.proof()?;
                auth::verify(
                    &self.resolver.secret,
                    NOTIFY_CLIENT,
                    &self.nonce,
                    Some(&request),
                    &tag,
                )?;
                let (id, result) = match serde_json::from_slice::<Notify>(&request) {
                    Ok(notify) => (Some(notify.id.clone()), self.enqueue(notify)),
                    Err(_) => (
                        None,
                        Err(SkillFsError::Invalid("invalid SkillFS notify v2 request")),
                    ),
                };
                let response = notify_response(id.as_deref(), result);
                let bytes = serde_json::to_vec(&response)?;
                let tag = auth::sign(
                    &self.resolver.secret,
                    NOTIFY_SERVER,
                    &self.nonce,
                    Some(&bytes),
                );
                Ok(SessionStep {
                    responses: vec![bytes, Frame::encode("auth.frame", None, Some(tag.as_ref()))],
                    complete: true,
                })
            }
            State::Complete => Err(SkillFsError::Authentication),
        }
    }

    fn enqueue(&self, notify: Notify) -> Result<Value, SkillFsError> {
        if notify.method != "skill_ledger.skillfs_notify_change"
            || notify.id.len() > 128
            || notify.params.schema_version != 2
            || !(1..=120_000).contains(&notify.timeout_ms)
            || !notify.trace_context.is_object()
            || notify.params.paths.len() > 64
            || ![
                "mkdir",
                "create",
                "write",
                "rename",
                "unlink",
                "rmdir",
                "setattr",
                "truncate",
                "reconcile",
            ]
            .contains(&notify.params.event_kind.as_str())
        {
            return Err(SkillFsError::Invalid("unsupported notify envelope"));
        }
        self.resolver.notify_identity(
            &notify.params.canonical_skill_dir,
            self.peer.uid(),
            &notify.params.skill_id,
        )?;
        for path in &notify.params.paths {
            if path.is_empty()
                || path.len() > 4096
                || path.contains('\0')
                || path.starts_with('/')
                || path.split('/').any(|p| matches!(p, "" | "." | ".."))
            {
                return Err(SkillFsError::Invalid("invalid notify relative path"));
            }
        }
        let ignored = !notify.params.paths.is_empty()
            && notify
                .params
                .paths
                .iter()
                .all(|p| p.split('/').next() == Some(".skill-meta"));
        let mut paths = notify.params.paths;
        paths.sort();
        paths.dedup();
        let skill = json!({
            "canonicalSkillDir":notify.params.canonical_skill_dir.path(),
            "skillName":notify.params.canonical_skill_dir.name(),
            "reportedSkillId":notify.params.skill_id,
            "eventKinds":[notify.params.event_kind],
            "paths":paths,
        });
        if ignored {
            return Ok(json!({"schemaVersion":2,"accepted":true,"ignored":true,
                "reason":"metadata-only change","skill":skill}));
        }
        let newly_queued = self.queue.enqueue(
            notify.params.canonical_skill_dir,
            CallerIdentity {
                uid: self.peer.uid(),
                gid: self.peer.gid(),
                pid: self.peer.pid(),
            },
        )?;
        Ok(
            json!({"schemaVersion":2,"accepted":true,"ignored":false,"queued":true,"coalesced":!newly_queued,"skill":skill}),
        )
    }
}

fn notify_response(id: Option<&str>, result: Result<Value, SkillFsError>) -> Value {
    // Keep the V1 response envelope; the request id remains daemon-owned.
    let mut response = json!({"request_id":uuid::Uuid::new_v4().to_string(),
        "ok":true,"data":{},"stdout":"","stderr":"","exit_code":0});
    if let Some(id) = id {
        response["id"] = id.into();
    }
    match result {
        Ok(data) => response["data"] = data,
        Err(error) => {
            let code = if matches!(error, SkillFsError::Unavailable(_)) {
                "unavailable"
            } else {
                "bad_request"
            };
            let message = error.to_string();
            response["ok"] = false.into();
            response["exit_code"] = 1.into();
            response["stderr"] = message.clone().into();
            response["error"] = json!({"code":code,"message":message});
        }
    }
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Notify {
    id: String,
    method: String,
    params: Change,
    #[serde(default = "empty_object")]
    trace_context: Value,
    #[serde(default = "notify_timeout")]
    timeout_ms: u64,
}
fn empty_object() -> Value {
    json!({})
}
fn notify_timeout() -> u64 {
    5000
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Change {
    schema_version: u32,
    canonical_skill_dir: SkillIdentity,
    skill_id: String,
    event_kind: String,
    paths: Vec<String>,
}
