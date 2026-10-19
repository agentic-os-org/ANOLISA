//! Gateway control-plane use cases served by the daemon.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::ops::RangeInclusive;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use asc_credential_store::{
    AgentName, CredentialId, CredentialStore, CredentialSummary, MAX_SECRET_BYTES, PutOutcome,
    Secret,
};
use asc_gateway_plan::{Preset, preset_plan};
use serde::Serialize;

use crate::GatewayError;
use crate::error::GatewayErrorKind;
use crate::files::Owner;
use crate::host::{GatewayAccount, Host};
use crate::nft::{self, Redirect};
use crate::runtime::{PublishedAgent, RuntimeDirectory};
use crate::state::{AgentEntry, State, StoredPreset};

/// Paths, names and limits of one gateway installation.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Root-only directory holding `state.json`.
    pub state_dir: PathBuf,
    /// Root-only credential store directory.
    pub store_dir: PathBuf,
    /// tmpfs directory shared with the data plane.
    pub runtime_dir: PathBuf,
    /// System account the data plane runs as.
    pub gateway_user: String,
    /// systemd unit of the data plane.
    pub unit: String,
    /// systemd slice holding the Agent scopes; must be a top-level slice.
    pub slice: String,
    /// Listener ports, one per Agent domain; must match the unit's listeners.
    pub ports: RangeInclusive<u16>,
    /// System trust stores, first existing one wins.
    pub system_ca_bundles: Vec<PathBuf>,
    /// How long to wait for the data plane to load a generation.
    pub load_timeout: Duration,
}

impl GatewayConfig {
    /// Production layout below the daemon data directory.
    pub fn system(data_dir: &Path) -> Self {
        Self {
            state_dir: data_dir.join("gateway"),
            store_dir: data_dir.join("credentials"),
            runtime_dir: PathBuf::from("/run/agent-sec-gateway"),
            gateway_user: "agent-sec-gw".into(),
            unit: "agent-sec-gateway.service".into(),
            slice: "agentsec.slice".into(),
            ports: 15001..=15008,
            system_ca_bundles: vec![
                PathBuf::from("/etc/pki/tls/certs/ca-bundle.crt"),
                PathBuf::from("/etc/ssl/certs/ca-certificates.crt"),
            ],
            load_timeout: Duration::from_secs(20),
        }
    }
}

/// Request to protect a credential for an Agent domain.
#[derive(Debug, Clone)]
pub struct KeyRequest {
    /// Registered Agent domain.
    pub agent: String,
    /// Built-in preset name.
    pub preset: String,
    /// Absolute path of a file owned by the caller, mode 0600 or stricter, holding the real value.
    pub secret_file: PathBuf,
    /// Kernel-authenticated uid of the administrator making the request.
    pub caller_uid: u32,
}

/// Value-free view of the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    /// Whether the gateway is enabled.
    pub enabled: bool,
    /// Whether the data-plane unit is active.
    pub active: bool,
    /// Last generation handed to the data plane.
    pub generation: u64,
    /// Generation the data plane reports as loaded.
    pub loaded_generation: Option<u64>,
    /// Configured Agent domains.
    pub agents: Vec<AgentStatus>,
}

/// One configured Agent domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    /// Agent domain name.
    pub name: AgentName,
    /// Local user allowed to run it.
    pub uid: u32,
    /// Listener port.
    pub port: u16,
    /// Configured presets.
    pub presets: Vec<String>,
    /// Whether its scope currently has processes.
    pub running: bool,
}

/// Result of a key change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyOutcome {
    /// Agent domain.
    pub agent: AgentName,
    /// Preset.
    pub preset: String,
    /// What happened to the stored real value.
    pub credential: CredentialChange,
    /// Whether the running gateway now serves the change; false when it is disabled.
    pub applied: bool,
}

/// Effect of a key change on the stored credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialChange {
    /// A new real value was stored.
    Created,
    /// The stored value was already identical.
    Unchanged,
    /// The real value was rotated; every Agent domain keeps its fake.
    Replaced,
    /// The Agent domain's fake was retired; other domains still use the credential.
    FakeRetired,
    /// No Agent domain uses the credential anymore; its real value was dropped.
    Retired,
}

/// Credentials and the Agent domains using them, without values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyList {
    /// Stored credentials with abbreviated fakes.
    pub credentials: Vec<CredentialSummary>,
    /// Configured Agent domains.
    pub agents: Vec<AgentStatus>,
}

/// Read-only environment containing fakes and the prospective CA bundle path.
///
/// Holds the domain's fakes, so it deliberately has no `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialGrant {
    /// Agent domain.
    pub agent: AgentName,
    /// Registered systemd scope; querying does not move any process.
    pub scope: String,
    /// Fake credentials and CA variables; the CA file may not exist until enable.
    pub environment: BTreeMap<String, String>,
}

struct Inner {
    state: State,
    store: Option<CredentialStore>,
}

/// Secret gateway control plane; all operations are serialized.
pub struct GatewayService {
    config: GatewayConfig,
    host: Box<dyn Host>,
    inner: Mutex<Inner>,
}

const CA_VARIABLES: [&str; 6] = [
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "NODE_EXTRA_CA_CERTS",
    "DENO_CERT",
];

impl GatewayService {
    /// Loads the persisted state; nothing on the host changes.
    ///
    /// # Errors
    /// Returns a failure when the state file is unreadable or corrupt.
    pub fn new(config: GatewayConfig, host: Box<dyn Host>) -> Result<Self, GatewayError> {
        let state = State::load(&config.state_dir, &config.ports)?;
        Ok(Self {
            config,
            host,
            inner: Mutex::new(Inner { state, store: None }),
        })
    }

    /// Re-applies an enabled gateway after a daemon or host restart.
    ///
    /// # Errors
    /// Returns the apply failure; the configuration stays as persisted.
    pub fn recover(&self) -> Result<(), GatewayError> {
        let mut inner = self.lock();
        if !inner.state.enabled {
            return Ok(());
        }
        let desired = inner.state.clone();
        self.apply(&mut inner, &desired, false)
    }

    /// Starts serving the configured Agent domains.
    ///
    /// # Errors
    /// Returns missing prerequisites or a data plane that does not come up.
    pub fn enable(&self) -> Result<GatewayStatus, GatewayError> {
        let mut inner = self.lock();
        let mut desired = inner.state.clone();
        desired.enabled = true;
        if let Err(error) = self.apply(&mut inner, &desired, false) {
            if !inner.state.enabled {
                // Nothing may keep serving a gateway the state calls disabled.
                let _ = self.host.stop_unit(&self.config.unit);
                let _ = self.host.apply_nft(&nft::render_delete());
                let _ = self.runtime(None).remove_all();
            }
            return Err(error);
        }
        desired.generation = inner.state.generation;
        desired.save(&self.config.state_dir)?;
        inner.state = desired;
        Ok(self.status_of(&inner.state))
    }

    /// Stops the data plane and removes every runtime file, the secrets snapshot included.
    ///
    /// Configured keys are kept for a later enable.
    ///
    /// # Errors
    /// Returns the first host failure after attempting every cleanup step.
    pub fn disable(&self) -> Result<GatewayStatus, GatewayError> {
        let mut inner = self.lock();
        let stopped = self.host.stop_unit(&self.config.unit);
        let unrouted = self.host.apply_nft(&nft::render_delete());
        let removed = self.runtime(None).remove_all();
        let mut desired = inner.state.clone();
        desired.enabled = false;
        desired.save(&self.config.state_dir)?;
        inner.state = desired;
        stopped?;
        unrouted?;
        removed?;
        Ok(self.status_of(&inner.state))
    }

    /// Reports global health and only the caller's domains; root sees every domain.
    pub fn status(&self, caller_uid: u32) -> GatewayStatus {
        let inner = self.lock();
        let mut status = self.status_of(&inner.state);
        status
            .agents
            .retain(|entry| caller_uid == 0 || entry.uid == caller_uid);
        status
    }

    /// Stores the real value for `preset` and grants `agent` a fake for it.
    ///
    /// The configuration is saved before it is applied, so a failed apply is
    /// reported but retried by the next enable or daemon start.
    ///
    /// # Errors
    /// Returns invalid input, conflicts with the existing domain, exhausted
    /// ports, store failures, or an apply failure.
    pub fn add_key(&self, request: &KeyRequest) -> Result<KeyOutcome, GatewayError> {
        let agent = AgentName::new(request.agent.clone())?;
        let preset = parse_preset(&request.preset)?;
        let mut inner = self.lock();
        let mut desired = inner.state.clone();
        let entry = desired.agents.get_mut(&agent).ok_or_else(|| {
            GatewayError::new(
                GatewayErrorKind::NotFound,
                format!("Agent domain {agent} is not registered; register it first"),
            )
        })?;
        if let Some(account) = self.account()? {
            self.check_isolated(entry.uid, account)?;
        }
        let secret = read_secret_file(&request.secret_file, request.caller_uid)?;
        entry.presets.insert(StoredPreset(preset));
        let spec = preset.credential_spec();
        let credential = spec.id.clone();
        let store = self.store(&mut inner, true)?;
        let put = store.put(spec, secret)?;
        store.issue_fake(&credential, &agent)?;
        let change = match put {
            PutOutcome::Created => CredentialChange::Created,
            PutOutcome::Unchanged => CredentialChange::Unchanged,
            PutOutcome::Replaced => CredentialChange::Replaced,
        };
        self.commit(&mut inner, desired, false)?;
        Ok(KeyOutcome {
            agent,
            preset: preset.name().to_owned(),
            credential: change,
            applied: inner.state.enabled,
        })
    }

    /// Withdraws `preset` from `agent`, retaining its registered UID binding.
    ///
    /// If the running gateway cannot be updated, it is stopped so the removed
    /// key does not keep working.
    ///
    /// # Errors
    /// Returns unknown names, store failures, or an apply failure.
    pub fn remove_key(&self, agent: &str, preset: &str) -> Result<KeyOutcome, GatewayError> {
        let agent = AgentName::new(agent.to_owned())?;
        let preset = parse_preset(preset)?;
        let mut inner = self.lock();
        let mut desired = inner.state.clone();
        let Some(entry) = desired.agents.get_mut(&agent) else {
            return Err(GatewayError::new(
                GatewayErrorKind::NotFound,
                format!("Agent domain {agent} is not configured"),
            ));
        };
        if !entry.presets.remove(&StoredPreset(preset)) {
            return Err(GatewayError::new(
                GatewayErrorKind::NotFound,
                format!("Agent domain {agent} has no {} key", preset.name()),
            ));
        }
        let credential = preset.credential_spec().id;
        let shared = desired.preset_in_use(preset, &agent);
        // Persist the withdrawal before touching the store: a store failure then
        // leaves an unused value behind rather than a domain without its fake.
        desired.save(&self.config.state_dir)?;
        inner.state = desired.clone();
        let retired = (|| {
            let store = self.store(&mut inner, true)?;
            if shared {
                store.retire_fake(&credential, &agent)?;
                Ok::<_, GatewayError>(CredentialChange::FakeRetired)
            } else {
                store.remove(&credential)?;
                Ok(CredentialChange::Retired)
            }
        })();
        let change = retired.map_err(|error| self.fail_closed(error))?;
        self.commit(&mut inner, desired, true)?;
        Ok(KeyOutcome {
            agent,
            preset: preset.name().to_owned(),
            credential: change,
            applied: inner.state.enabled,
        })
    }

    /// Lists credentials and Agent domains without values.
    ///
    /// # Errors
    /// Returns store failures other than a store that was never created.
    pub fn list_keys(&self) -> Result<KeyList, GatewayError> {
        let mut inner = self.lock();
        let credentials = if std::fs::symlink_metadata(&self.config.store_dir).is_ok() {
            self.store(&mut inner, false)?.list()
        } else {
            Vec::new()
        };
        Ok(KeyList {
            credentials,
            agents: self.agents_of(&inner.state),
        })
    }

    /// Registers an existing process tree in a UID-bound domain, even while disabled.
    ///
    /// Root authorization is enforced by the transport. No PID is persisted and
    /// no target process is stopped or killed on failure. Registration snapshots
    /// existing descendants; it does not freeze concurrent forks.
    ///
    /// # Errors
    /// Returns invalid users, a conflicting binding or scope, an oversized tree,
    /// changed/exited processes, persistence failures, or host/apply failures.
    pub fn register(&self, uid: u32, pid: u32, agent: &str) -> Result<AgentStatus, GatewayError> {
        let agent = AgentName::new(agent.to_owned())?;
        if uid == 0 {
            return Err(GatewayError::invalid("an Agent domain cannot run as root"));
        }
        let account = self.account()?;
        if let Some(account) = account {
            self.check_isolated(uid, account)?;
        } else if self.host.user_groups(uid).is_none() {
            return Err(GatewayError::invalid(format!(
                "no local user has uid {uid}"
            )));
        }
        let mut inner = self.lock();
        let original = inner.state.clone();
        let mut desired = original.clone();
        if let Some(entry) = desired.agents.get(&agent) {
            if entry.uid != uid {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    format!(
                        "Agent domain {agent} belongs to uid {}; unregister it first",
                        entry.uid
                    ),
                ));
            }
        } else {
            let port = desired.free_port(&self.config.ports).ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::ResourceExhausted,
                    "every gateway listener port is in use",
                )
            })?;
            desired.agents.insert(
                agent.clone(),
                AgentEntry {
                    uid,
                    port,
                    presets: BTreeSet::new(),
                },
            );
        }
        let processes = self.capture_and_attach(&original, &agent, uid, pid, account)?;
        let expected = format!("/{}/{}", self.config.slice, scope_name(&agent));
        let result = if desired.agents == original.agents {
            if desired.enabled {
                self.route(&desired)
            } else {
                Ok(())
            }
        } else {
            self.commit(&mut inner, desired, false)
        };
        let result = result.and_then(|()| {
            // One bounded verification snapshot detects missed concurrent forks;
            // it never retries or moves new children outside the captured tree.
            let current = self.host.process_tree(pid)?;
            for process in current.iter().chain(&processes) {
                self.check_process_domain(process, uid, account)?;
                self.verify_attachment(process, &expected)?;
            }
            Ok(())
        });
        if let Err(error) = result {
            // A failed new registration must not become a usable persisted domain.
            if !original.agents.contains_key(&agent)
                && let Err(rollback) = self.commit(&mut inner, original, true)
            {
                return Err(error.context(&format!("registration rollback failed: {rollback}")));
            }
            return Err(error);
        }
        self.agents_of(&inner.state)
            .into_iter()
            .find(|entry| entry.name == agent)
            .ok_or_else(|| GatewayError::internal("registered domain disappeared"))
    }

    /// Queries fakes and CA variables without admission, process movement or health checks.
    ///
    /// # Errors
    /// Returns an unknown domain, a caller other than root or its UID, or a store failure.
    pub fn credentials(
        &self,
        caller_uid: u32,
        agent: &str,
    ) -> Result<CredentialGrant, GatewayError> {
        let agent = AgentName::new(agent.to_owned())?;
        let mut inner = self.lock();
        let entry = inner.state.agents.get(&agent).cloned().ok_or_else(|| {
            GatewayError::new(
                GatewayErrorKind::NotFound,
                format!("Agent domain {agent} is not registered"),
            )
        })?;
        if caller_uid != 0 && caller_uid != entry.uid {
            return Err(GatewayError::new(
                GatewayErrorKind::PermissionDenied,
                format!("Agent domain {agent} belongs to uid {}", entry.uid),
            ));
        }
        let mut environment = BTreeMap::new();
        if !entry.presets.is_empty() {
            let store = self.store(&mut inner, false)?;
            for preset in entry.presets() {
                let fake = store
                    .fake(&preset.credential_spec().id, &agent)
                    .ok_or_else(|| {
                        GatewayError::internal(format!("Agent domain {agent} has no fake"))
                    })?;
                for variable in preset_variables(preset) {
                    environment.insert((*variable).to_owned(), fake.to_owned());
                }
            }
        }
        let bundle = self.runtime(None).ca_bundle_path().display().to_string();
        for variable in CA_VARIABLES {
            environment.insert(variable.to_owned(), bundle.clone());
        }
        Ok(CredentialGrant {
            scope: scope_name(&agent),
            agent,
            environment,
        })
    }

    /// Removes a domain and revokes its fakes; processes are left running in their scope.
    ///
    /// # Errors
    /// Returns unknown names, persistence/store failures, or a fail-closed apply failure.
    pub fn unregister(&self, agent: &str) -> Result<GatewayStatus, GatewayError> {
        let agent = AgentName::new(agent.to_owned())?;
        let mut inner = self.lock();
        let mut desired = inner.state.clone();
        desired.agents.remove(&agent).ok_or_else(|| {
            GatewayError::new(
                GatewayErrorKind::NotFound,
                format!("Agent domain {agent} is not registered"),
            )
        })?;
        desired.save(&self.config.state_dir)?;
        inner.state = desired.clone();
        let retired = (|| {
            if std::fs::symlink_metadata(&self.config.store_dir).is_ok() {
                let store = self.store(&mut inner, false)?;
                // Include orphan fakes from a key addition whose state write failed.
                let credentials: Vec<_> = store
                    .list()
                    .into_iter()
                    .filter(|record| {
                        record
                            .fakes
                            .iter()
                            .any(|fake| fake.agent == agent && !fake.retired)
                    })
                    .map(|record| record.id)
                    .collect();
                for credential in credentials {
                    let shared = desired.agents.values().any(|entry| {
                        entry
                            .presets()
                            .iter()
                            .any(|preset| preset.credential_spec().id == credential)
                    });
                    if shared {
                        store.retire_fake(&credential, &agent)?;
                    } else {
                        store.remove(&credential)?;
                    }
                }
            }
            Ok::<(), GatewayError>(())
        })();
        retired.map_err(|error| self.fail_closed(error))?;
        self.commit(&mut inner, desired, true)?;
        let scope = scope_name(&agent);
        let path = format!("{}/{scope}", self.config.slice);
        if self.host.scope_processes(&path)?.is_empty() {
            self.host.remove_scope(&scope)?;
        }
        Ok(self.status_of(&inner.state))
    }

    fn capture_and_attach(
        &self,
        state: &State,
        agent: &AgentName,
        uid: u32,
        pid: u32,
        account: Option<GatewayAccount>,
    ) -> Result<Vec<crate::ProcessIdentity>, GatewayError> {
        let scope = scope_name(agent);
        let path = format!("{}/{scope}", self.config.slice);
        let members = self.host.scope_processes(&path)?;
        let mut processes = self.host.process_tree(pid)?;
        if processes.first().map(|process| process.pid) != Some(pid) {
            return Err(GatewayError::internal(
                "process snapshot does not start with the root",
            ));
        }
        // Retired scopes may still contain live processes of their former UID.
        // Validate both snapshots before moving even the new root.
        for process in processes.iter().chain(&members) {
            self.check_process_domain(process, uid, account)?;
            self.check_scope_overlap(state, agent, process.pid)?;
        }
        let tree_len = processes.len();
        let mut seen: BTreeSet<u32> = processes.iter().map(|process| process.pid).collect();
        processes.extend(
            members
                .into_iter()
                .filter(|process| seen.insert(process.pid)),
        );
        if processes.len() > crate::MAX_PROCESS_TREE {
            return Err(GatewayError::new(
                GatewayErrorKind::ResourceExhausted,
                "process tree and existing scope exceed 4096 processes",
            ));
        }
        let expected = format!("/{path}");
        for process in &processes[..tree_len] {
            self.host.verify_process(process)?;
            self.check_scope_overlap(state, agent, process.pid)?;
            self.host
                .attach_to_scope(&scope, &self.config.slice, process)?;
            self.host.verify_process(process)?;
            if self.host.process_cgroup(process.pid)? != expected {
                return Err(GatewayError::unavailable(format!(
                    "process {} did not move into {expected}",
                    process.pid
                )));
            }
        }
        for process in &processes {
            self.verify_attachment(process, &expected)?;
        }
        Ok(processes)
    }

    fn check_process_domain(
        &self,
        process: &crate::ProcessIdentity,
        uid: u32,
        account: Option<GatewayAccount>,
    ) -> Result<(), GatewayError> {
        if process.uids != [uid; 4]
            || account.is_some_and(|account| process.groups.contains(&account.gid))
        {
            return Err(GatewayError::new(
                GatewayErrorKind::PermissionDenied,
                format!("process {} has a foreign UID or gateway group", process.pid),
            ));
        }
        self.host.verify_process(process)
    }

    fn verify_attachment(
        &self,
        process: &crate::ProcessIdentity,
        scope: &str,
    ) -> Result<(), GatewayError> {
        self.host.verify_process(process)?;
        let path = self.host.process_cgroup(process.pid)?;
        if path != scope
            && !path
                .strip_prefix(scope)
                .is_some_and(|suffix| suffix.starts_with('/'))
        {
            return Err(GatewayError::unavailable(format!(
                "process {} left {scope}",
                process.pid
            )));
        }
        self.host.verify_process(process)
    }

    fn check_scope_overlap(
        &self,
        state: &State,
        agent: &AgentName,
        pid: u32,
    ) -> Result<(), GatewayError> {
        let path = self.host.process_cgroup(pid)?;
        for name in state.agents.keys().filter(|name| *name != agent) {
            let foreign = format!("/{}/{}", self.config.slice, scope_name(name));
            if path == foreign
                || path
                    .strip_prefix(&foreign)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    format!("process {pid} already belongs to Agent domain {name}"),
                ));
            }
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panicked operation leaves either the old or the new state, both consistent on disk.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn account(&self) -> Result<Option<GatewayAccount>, GatewayError> {
        self.host.gateway_account(&self.config.gateway_user)
    }

    /// Rejects an Agent user that could read the gateway's secrets snapshot.
    fn check_isolated(&self, uid: u32, account: GatewayAccount) -> Result<(), GatewayError> {
        let groups = self
            .host
            .user_groups(uid)
            .ok_or_else(|| GatewayError::invalid(format!("no local user has uid {uid}")))?;
        if uid == account.uid || groups.contains(&account.gid) {
            return Err(GatewayError::invalid(format!(
                "uid {uid} is the gateway account or in its group and could read the real keys"
            )));
        }
        Ok(())
    }

    fn runtime(&self, account: Option<GatewayAccount>) -> RuntimeDirectory {
        let account = account.unwrap_or(GatewayAccount { uid: 0, gid: 0 });
        RuntimeDirectory {
            root: self.config.runtime_dir.clone(),
            gateway: Owner {
                uid: account.uid,
                gid: account.gid,
            },
        }
    }

    fn store<'a>(
        &self,
        inner: &'a mut Inner,
        create: bool,
    ) -> Result<&'a mut CredentialStore, GatewayError> {
        if inner.store.is_none() {
            let store = if create {
                CredentialStore::create(&self.config.store_dir)?
            } else {
                CredentialStore::open(&self.config.store_dir)?
            };
            inner.store = Some(store);
        }
        inner
            .store
            .as_mut()
            .ok_or_else(|| GatewayError::internal("credential store is not open"))
    }

    /// Persists `desired` and applies it when the gateway is enabled.
    fn commit(
        &self,
        inner: &mut Inner,
        mut desired: State,
        revoking: bool,
    ) -> Result<(), GatewayError> {
        desired.generation = desired.generation.max(inner.state.generation);
        desired.save(&self.config.state_dir).map_err(|error| {
            if revoking {
                self.fail_closed(error)
            } else {
                error
            }
        })?;
        inner.state = desired.clone();
        if !desired.enabled {
            return Ok(());
        }
        self.apply(inner, &desired, revoking).map_err(|error| {
            error.context("the domain/key change is saved but the gateway did not apply it")
        })?;
        desired.generation = inner.state.generation;
        inner.state = desired;
        Ok(())
    }

    /// Publishes `desired` as a new generation and waits until the data plane serves it.
    fn apply(
        &self,
        inner: &mut Inner,
        desired: &State,
        revoking: bool,
    ) -> Result<(), GatewayError> {
        let result = (|| {
            let account = self.account()?.ok_or_else(|| {
                GatewayError::unavailable(format!(
                    "gateway account {} does not exist; install the gateway first",
                    self.config.gateway_user
                ))
            })?;
            for entry in desired.agents.values() {
                self.check_isolated(entry.uid, account)?;
            }
            let runtime = self.runtime(Some(account));
            let generation = inner.state.generation.max(desired.generation) + 1;
            let mut reserved = inner.state.clone();
            reserved.generation = generation;
            reserved.save(&self.config.state_dir)?;
            inner.state.generation = generation;
            self.publish_and_route(inner, desired, &runtime, generation)
        })();
        if let Err(error) = result {
            if revoking {
                return Err(self.fail_closed(error));
            }
            return Err(error);
        }
        Ok(())
    }

    fn fail_closed(&self, error: GatewayError) -> GatewayError {
        // Loaded and older snapshots may still contain a revoked fake or real value.
        let stopped = self.host.stop_unit(&self.config.unit);
        let removed = self.runtime(None).remove_all();
        let cleanup: Vec<String> = [stopped.err(), removed.err()]
            .into_iter()
            .flatten()
            .map(|failure| failure.to_string())
            .collect();
        if cleanup.is_empty() {
            error.context("gateway stopped so the removed key cannot be used")
        } else {
            error.context(&format!(
                "the gateway may still serve the removed key; stop it manually ({})",
                cleanup.join("; ")
            ))
        }
    }

    fn publish_and_route(
        &self,
        inner: &mut Inner,
        desired: &State,
        runtime: &RuntimeDirectory,
        generation: u64,
    ) -> Result<(), GatewayError> {
        runtime.prepare()?;
        let mut plans = Vec::with_capacity(desired.agents.len());
        let mut credentials = BTreeSet::<CredentialId>::new();
        for (agent, entry) in &desired.agents {
            if entry.presets.is_empty() {
                continue;
            }
            let presets = entry.presets();
            credentials.extend(presets.iter().map(|preset| preset.credential_spec().id));
            plans.push(PublishedAgent {
                agent,
                port: entry.port,
                plan: preset_plan(agent, &presets)?.to_json(),
            });
        }
        let credentials: Vec<CredentialId> = credentials.into_iter().collect();
        let store = self.store(inner, true)?;
        runtime.publish(store, &credentials, &plans, generation)?;
        self.host.start_unit(&self.config.unit)?;
        runtime.await_generation(generation, self.config.load_timeout)?;
        runtime.publish_ca_bundle(&self.config.system_ca_bundles)?;
        self.route(desired)?;
        runtime.remove_stale(generation)
    }

    /// Replaces the nftables table with redirects for every running Agent domain.
    fn route(&self, state: &State) -> Result<(), GatewayError> {
        let mut last = None;
        // A scope can vanish between the existence check and the load; retry once.
        for _ in 0..2 {
            let scopes: Vec<(&AgentName, String, u16)> = state
                .agents
                .iter()
                .filter(|(_, entry)| !entry.presets.is_empty())
                .map(|(agent, entry)| {
                    (
                        agent,
                        format!("{}/{}", self.config.slice, scope_name(agent)),
                        entry.port,
                    )
                })
                .filter(|(_, path, _)| self.host.cgroup_exists(path))
                .collect();
            if let Some((_, path, _)) = scopes
                .iter()
                .find(|(_, path, _)| !nft::is_safe_cgroup_path(path))
            {
                return Err(GatewayError::internal(format!("unsafe cgroup path {path}")));
            }
            let redirects: Vec<Redirect<'_>> = scopes
                .iter()
                .map(|(agent, path, port)| Redirect {
                    agent,
                    cgroup: path,
                    port: *port,
                })
                .collect();
            match self.host.apply_nft(&nft::render(&redirects)) {
                Ok(()) => return Ok(()),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| GatewayError::internal("nftables update failed")))
    }

    fn status_of(&self, state: &State) -> GatewayStatus {
        GatewayStatus {
            enabled: state.enabled,
            active: self.host.unit_active(&self.config.unit),
            generation: state.generation,
            loaded_generation: self.runtime(None).health().loaded_generation,
            agents: self.agents_of(state),
        }
    }

    fn agents_of(&self, state: &State) -> Vec<AgentStatus> {
        state
            .agents
            .iter()
            .map(|(name, entry)| AgentStatus {
                name: name.clone(),
                uid: entry.uid,
                port: entry.port,
                presets: entry
                    .presets
                    .iter()
                    .map(|preset| preset.0.name().to_owned())
                    .collect(),
                running: self.host.cgroup_exists(&format!(
                    "{}/{}",
                    self.config.slice,
                    scope_name(name)
                )),
            })
            .collect()
    }
}

/// systemd scope holding an Agent domain's processes.
pub fn scope_name(agent: &AgentName) -> String {
    format!("agent-sec-agent-{agent}.scope")
}

fn parse_preset(name: &str) -> Result<Preset, GatewayError> {
    Preset::from_str(name).map_err(|_| GatewayError::invalid(format!("unknown preset {name:?}")))
}

fn preset_variables(preset: Preset) -> &'static [&'static str] {
    match preset {
        Preset::Github => &["GITHUB_TOKEN", "GH_TOKEN"],
    }
}

/// Reads a real value from a file only `owner` can read; one trailing newline is dropped.
fn read_secret_file(path: &Path, owner: u32) -> Result<Secret, GatewayError> {
    if !path.is_absolute() {
        return Err(GatewayError::invalid(
            "the secret file path must be absolute",
        ));
    }
    let flags = rustix::fs::OFlags::NOFOLLOW.union(rustix::fs::OFlags::NONBLOCK);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits().cast_signed())
        .open(path)
        .map_err(|error| GatewayError::invalid(format!("cannot open the secret file: {error}")))?;
    let metadata = file.metadata().map_err(|error| {
        GatewayError::invalid(format!("cannot inspect the secret file: {error}"))
    })?;
    if !metadata.is_file() {
        return Err(GatewayError::invalid(
            "the secret file must be a regular file",
        ));
    }
    if metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(GatewayError::invalid(
            "the secret file must belong to the caller and be unreadable by group and others",
        ));
    }
    let limit = MAX_SECRET_BYTES as u64 + 2;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| GatewayError::invalid(format!("cannot read the secret file: {error}")))?;
    if bytes.len() as u64 > limit {
        return Err(GatewayError::invalid("the secret file is too large"));
    }
    if bytes.ends_with(b"\n") {
        bytes.pop();
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
    }
    let text =
        String::from_utf8(bytes).map_err(|_| GatewayError::invalid("the secret is not text"))?;
    Ok(Secret::new(text)?)
}
