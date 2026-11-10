//! PAP-backed Scope lifecycle, credential administration and data-plane publication.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use asc_credential_store::{
    CredentialId, CredentialState, CredentialStore, PairInput, ScopeId, read_token_file,
};
use asc_gateway_plan::{Plan, compile_scope};
use asc_pap::PapRepository;
use asc_policy_types::binding::BindingStatus;
use asc_policy_types::identifiers::ResourceId;
use asc_policy_types::scope::{PreparedScope, ScopeStatus};
use serde::Serialize;

use crate::binding::RouteFault;
use crate::files::Owner;
use crate::host::{GatewayAccount, Host};
use crate::nft::{self, Redirect};
use crate::runtime::{PublishedScope, RuntimeDirectory};
use crate::state::{Domain, State};
use crate::{GatewayError, GatewayErrorKind, ProcessIdentity};

/// Installation paths and bounded listener capacity.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Root-only execution projection.
    pub state_dir: PathBuf,
    /// Root-only credential store.
    pub store_dir: PathBuf,
    /// tmpfs files shared with the data plane.
    pub runtime_dir: PathBuf,
    /// Dedicated data-plane account.
    pub gateway_user: String,
    /// Data-plane systemd unit.
    pub unit: String,
    /// Top-level systemd slice for execution instances.
    pub slice: String,
    /// One port per Scope, shared by its instances.
    pub ports: RangeInclusive<u16>,
    /// First available system trust bundle is extended with the gateway CA.
    pub system_ca_bundles: Vec<PathBuf>,
    /// Generation acknowledgement deadline.
    pub load_timeout: Duration,
}
impl GatewayConfig {
    /// Production layout under the daemon data directory.
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

/// Value-free gateway health and execution projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    /// Persisted administrative enablement.
    pub enabled: bool,
    /// Enabled data plane has acknowledged its current publication and CA.
    pub ready: bool,
    /// Caller-visible Scope projections.
    pub scopes: Vec<ScopeStatusView>,
}
/// Non-secret Scope execution state, never an authorization source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeStatusView {
    /// Canonical assignment identity.
    pub scope_id: ScopeId,
    /// Attached Binding responsibilities.
    pub bindings: Vec<ResourceId>,
}
/// Credential mutation result without values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyOutcome {
    /// Managed credential identifier.
    pub credential: CredentialId,
}
/// Value-free inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyList {
    /// Currently imported credential identifiers.
    pub credentials: Vec<CredentialId>,
}
/// Shared public trust bundle, available only while the data plane is ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaBundle {
    /// Public CA bundle on the daemon host, shared by all Scopes.
    pub ca_bundle_path: PathBuf,
}

pub(crate) struct Inner {
    pub(crate) state: State,
    store: Option<CredentialStore>,
    /// Execution pins are rebuilt from canonical Bindings on recovery, never from PID alone.
    pub(crate) pins: BTreeMap<ResourceId, Vec<ProcessIdentity>>,
}
/// Serialized execution service. PAP remains the sole authorization authority.
pub struct GatewayService {
    pub(crate) config: GatewayConfig,
    pub(crate) host: Box<dyn Host>,
    pub(crate) repository: Arc<dyn PapRepository>,
    inner: Mutex<Inner>,
}

impl GatewayService {
    /// Loads the execution projection without host mutation or authorization on success.
    /// # Errors
    /// Rejects unreadable, corrupt or unsupported projections after attempting to stop
    /// the proxy and withdraw routing and runtime files; retains state and credentials.
    pub fn new(
        config: GatewayConfig,
        host: Box<dyn Host>,
        repository: Arc<dyn PapRepository>,
    ) -> Result<Self, GatewayError> {
        let state = State::load(&config.state_dir, &config.ports)
            .map_err(|error| withdraw_on_failure(&config, host.as_ref(), error))?;
        if !config
            .state_dir
            .join(crate::state::STATE_FILE_NAME)
            .exists()
            && (std::fs::symlink_metadata(&config.runtime_dir).is_ok()
                || host.unit_active(&config.unit)
                || host
                    .routing_active()
                    .map_err(|error| withdraw_on_failure(&config, host.as_ref(), error))?)
        {
            return Err(withdraw_on_failure(
                &config,
                host.as_ref(),
                GatewayError::internal(
                    "gateway projection is missing while an old data plane remains",
                ),
            ));
        }
        Ok(Self {
            config,
            host,
            repository,
            inner: Mutex::new(Inner {
                state,
                store: None,
                pins: BTreeMap::new(),
            }),
        })
    }
    pub(crate) fn canonical(&self, scope: &PreparedScope) -> Result<(), GatewayError> {
        let current = self.repository.get_scope(&scope.scope_id)?;
        if current != *scope || current.status != ScopeStatus::Active {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Scope is inactive or its pinned snapshot differs",
            ));
        }
        Ok(())
    }
    /// Compiles the pinned assignment and validates its nonroot owner without PID discovery.
    /// # Errors
    /// Rejects unsupported policy semantics, absent owners and sensitive groups.
    pub fn check_scope(&self, scope: &PreparedScope) -> Result<Plan, GatewayError> {
        let plan = compile_scope(scope)?;
        let uid = scope
            .owner_uid
            .ok_or_else(|| GatewayError::invalid("credential Scope requires an owner UID"))?;
        self.check_owner(uid, self.account()?)?;
        Ok(plan)
    }
    pub(crate) fn validate_scope_pairs(&self, scope: &PreparedScope) -> Result<(), GatewayError> {
        let mut plan = self.check_scope(scope)?;
        let existing = match self.repository.get_scope(&scope.scope_id) {
            Ok(current) if current == *scope && current.status == ScopeStatus::Active => true,
            Ok(_) => {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    "Scope snapshot is not canonical",
                ));
            }
            Err(asc_pap::PapError::NotFound) => false,
            Err(error) => return Err(error.into()),
        };
        let mut inner = self.lock();
        let store = self.store(&mut inner, false)?;
        let summaries = store.list();
        if existing {
            let retired: BTreeSet<_> = summaries
                .iter()
                .filter(|record| record.state == CredentialState::Retired)
                .map(|record| &record.id)
                .collect();
            plan.credentials.retain(|id| !retired.contains(id));
        }
        store.validate_scope_pairs(&plan.credentials)?;
        if !existing {
            let active: BTreeSet<_> = self
                .canonical_assignments()?
                .into_iter()
                .flat_map(|(_, ids)| ids)
                .collect();
            if summaries.iter().any(|record| {
                plan.credentials.contains(&record.id)
                    && !record.fakes.is_empty()
                    && !active.contains(&record.id)
            }) {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    "pair's last canonical Scope has closed; pending revocation cannot grant a new Scope",
                ));
            }
        }
        Ok(())
    }

    /// Durably projects an active assignment using its registered fakes without starting agents.
    /// Disabled gateways can prepare; credentials remain unavailable until CA/generation readiness.
    /// Startup recovery of an admitted Scope uses [`Self::restore_scope`] instead.
    /// # Errors
    /// Rejects stale canonical snapshots, retired assignments or credentials without
    /// an active registered fake.
    pub fn prepare_scope(&self, scope: &PreparedScope) -> Result<(), GatewayError> {
        let mut inner = self.lock();
        let plan = self
            .canonical(scope)
            .and_then(|()| self.sync_credential_scopes(&mut inner))
            .and_then(|()| self.prepare_locked(&mut inner, scope))
            .map_err(|error| self.fail_closed(error))?;
        self.verify_issued_fakes(&mut inner, scope, &plan)?;
        self.publish_prepared(&mut inner, scope)
    }

    /// Restores an already admitted Scope during startup recovery.
    ///
    /// A retired credential assignment keeps its tombstone, so restoration must
    /// not reject the Scope: independent file Policies still need discovery.
    /// Publication keeps excluding credentials without an active fake.
    /// # Errors
    /// Rejects stale canonical snapshots or assignments that cannot be projected.
    pub fn restore_scope(&self, scope: &PreparedScope) -> Result<(), GatewayError> {
        let mut inner = self.lock();
        self.canonical(scope)
            .and_then(|()| self.sync_credential_scopes(&mut inner))
            .and_then(|()| self.prepare_locked(&mut inner, scope))
            .map_err(|error| self.fail_closed(error))?;
        self.publish_prepared(&mut inner, scope)
    }

    fn publish_prepared(
        &self,
        inner: &mut Inner,
        scope: &PreparedScope,
    ) -> Result<(), GatewayError> {
        if inner.state.enabled {
            self.apply(inner).map_err(|error| self.fail_closed(error))?;
        }
        self.canonical(scope)
            .map_err(|error| self.fail_closed(error))
    }
    /// A retired credential assignment keeps its tombstone, so preparation must reject
    /// the Scope instead of reporting success without a fake.
    fn verify_issued_fakes(
        &self,
        inner: &mut Inner,
        scope: &PreparedScope,
        plan: &Plan,
    ) -> Result<(), GatewayError> {
        if plan.credentials.is_empty() {
            return Ok(());
        }
        let id = ScopeId::new(scope.scope_id.as_str())?;
        let store = self.store(inner, false)?;
        for credential in &plan.credentials {
            if store.fake(credential, &id).is_none() {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    format!(
                        "credential {credential} is retired for this Scope; register a new real/fake pair and create a new Scope"
                    ),
                ));
            }
        }
        Ok(())
    }
    fn prepare_locked(
        &self,
        inner: &mut Inner,
        scope: &PreparedScope,
    ) -> Result<Plan, GatewayError> {
        self.canonical(scope)?;
        let plan = self.check_scope(scope)?;
        let id = ScopeId::new(scope.scope_id.as_str())?;
        let uid = scope
            .owner_uid
            .ok_or_else(|| GatewayError::invalid("Scope has no owner UID"))?;
        if let Some(domain) = inner.state.scopes.get(&id) {
            if domain.revoked || domain.uid != uid {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    "Scope projection is retired or belongs to another owner",
                ));
            }
        } else {
            let port = inner.state.free_port(&self.config.ports).ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::ResourceExhausted,
                    "gateway listener capacity exhausted",
                )
            })?;
            inner.state.scopes.insert(
                id.clone(),
                Domain {
                    uid,
                    port,
                    revoked: false,
                    bindings: BTreeMap::new(),
                },
            );
            inner.state.save(&self.config.state_dir)?;
        }
        let store = self.store(inner, false)?;
        let retired: BTreeSet<_> = store
            .list()
            .into_iter()
            .filter(|record| record.state == CredentialState::Retired)
            .map(|record| record.id)
            .collect();
        for credential in &plan.credentials {
            if !retired.contains(credential) && store.fake(credential, &id).is_none() {
                return Err(GatewayError::unavailable("Scope pair has no active fake"));
            }
        }
        self.canonical(scope)
            .map_err(|error| self.fail_closed(error))?;
        Ok(plan)
    }
    /// Withdraws every fake and route after PAP has durably closed Scope admission.
    /// Keeps projection and Binding cleanup responsibility; never removes real keys.
    /// # Errors
    /// Failures stop the proxy and withdraw traffic, retaining responsibility for retry.
    pub fn revoke_scope(&self, scope: &PreparedScope) -> Result<(), GatewayError> {
        let mut inner = self.lock();
        // Deletion admission is already closed: even the first canonical read failure
        // must withdraw the previously published authorization.
        let result = (|| {
            self.revoke_locked(&mut inner, scope)?;
            self.sync_credential_scopes(&mut inner)?;
            if inner.state.enabled {
                self.apply(&mut inner)?;
            }
            Ok(())
        })();
        result.map_err(|error| self.fail_closed(error))
    }
    fn revoke_locked(&self, inner: &mut Inner, scope: &PreparedScope) -> Result<(), GatewayError> {
        let current = self.repository.get_scope(&scope.scope_id)?;
        if current != *scope || current.status != ScopeStatus::Deleting {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Scope deletion intent is not canonical",
            ));
        }
        let id = ScopeId::new(scope.scope_id.as_str())?;
        if let Some(domain) = inner.state.scopes.get_mut(&id) {
            domain.revoked = true;
        }
        inner.state.save(&self.config.state_dir)
    }
    /// Releases even an empty Scope after all canonical Binding cleanup is complete.
    /// # Errors
    /// Rejects live responsibility or an assignment whose admission remains active.
    pub fn release_scope(&self, id: &ResourceId) -> Result<(), GatewayError> {
        let mut inner = self.lock();
        match self.repository.get_scope(id) {
            Ok(scope) => {
                if scope.status != ScopeStatus::Deleting
                    || !self.repository.scope_execution_ready_to_release(id)?
                {
                    return Err(GatewayError::new(
                        GatewayErrorKind::Conflict,
                        "canonical Scope cleanup is not complete",
                    ));
                }
            }
            Err(asc_pap::PapError::NotFound) => {
                // Without a canonical Scope, readiness cannot prove that no Binding owns it.
                let mut offset = 0;
                loop {
                    let page = self.repository.list_bindings(1000, offset)?;
                    if page
                        .items
                        .iter()
                        .any(|view| view.spec.scope.scope_id == *id)
                    {
                        return Err(GatewayError::new(
                            GatewayErrorKind::Conflict,
                            "Scope still owns canonical Bindings",
                        ));
                    }
                    if page.items.is_empty() {
                        break;
                    }
                    offset += u32::try_from(page.items.len())
                        .map_err(|_| GatewayError::internal("invalid Binding page"))?;
                }
            }
            Err(error) => return Err(error.into()),
        }
        let scope_id = ScopeId::new(id.as_str())?;
        if let Some(domain) = inner.state.scopes.get(&scope_id)
            && (!domain.revoked || !domain.bindings.is_empty())
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Scope cleanup is incomplete",
            ));
        }
        inner.state.scopes.remove(&scope_id);
        inner.state.save(&self.config.state_dir)
    }
    /// Revalidates canonical assignments and attached Binding identities after restart.
    /// Never loads latest Policy content or restores authority from projections.
    /// # Errors
    /// Any unsafe or unavailable recovery stops the proxy and removes traffic.
    pub fn recover(&self) -> Result<(), GatewayError> {
        let mut inner = self.lock();
        let result = self.recover_locked(&mut inner);
        result.map_err(|error| self.fail_closed(error))
    }
    fn canonical_assignments(&self) -> Result<Vec<(ScopeId, Vec<CredentialId>)>, GatewayError> {
        let mut after = None;
        let mut assignments = Vec::new();
        loop {
            let scopes = self.repository.scan_scopes(after.as_ref(), 128)?;
            if scopes.is_empty() {
                break;
            }
            for scope in &scopes {
                if scope.status == ScopeStatus::Active && scope.uses_credentials() {
                    assignments.push((
                        ScopeId::new(scope.scope_id.as_str())?,
                        compile_scope(scope)?.credentials,
                    ));
                }
            }
            after = scopes.last().map(|scope| scope.scope_id.clone());
        }
        Ok(assignments)
    }

    fn sync_credential_scopes(&self, inner: &mut Inner) -> Result<(), GatewayError> {
        let retired: BTreeSet<_> = if self.config.store_dir.exists() {
            self.store(inner, false)?
                .list()
                .into_iter()
                .filter(|record| record.state == CredentialState::Retired)
                .map(|record| record.id)
                .collect()
        } else {
            BTreeSet::new()
        };
        let mut assignments = self.canonical_assignments()?;
        for (_, ids) in &mut assignments {
            // Canonical snapshots outlive global key retirement; tombstones grant nothing.
            ids.retain(|id| !retired.contains(id));
        }
        if !assignments.is_empty() || self.config.store_dir.exists() {
            self.store(inner, false)?.sync_scopes(&assignments)?;
        }
        Ok(())
    }

    fn recover_locked(&self, inner: &mut Inner) -> Result<(), GatewayError> {
        self.sync_credential_scopes(inner)?;
        let mut after = None;
        let mut canonical_ids = BTreeSet::new();
        loop {
            let scopes = self.repository.scan_scopes(after.as_ref(), 128)?;
            if scopes.is_empty() {
                break;
            }
            for scope in &scopes {
                canonical_ids.insert(ScopeId::new(scope.scope_id.as_str())?);
                if !scope
                    .policy_snapshots
                    .iter()
                    .any(asc_policy_types::policy::PreparedPolicy::uses_credentials)
                {
                    continue;
                }
                match scope.status {
                    // Recovery keeps the tolerant path so one retired assignment cannot
                    // stop the gateway for every other Scope.
                    ScopeStatus::Active => {
                        self.prepare_locked(inner, scope)?;
                    }
                    ScopeStatus::Deleting => self.revoke_locked(inner, scope)?,
                }
            }
            after = scopes.last().map(|scope| scope.scope_id.clone());
        }
        for (id, domain) in &mut inner.state.scopes {
            if !canonical_ids.contains(id) {
                domain.revoked = true;
            }
        }
        self.recover_attachments(inner)?;
        // An untouched, disabled installation has no gateway projection to persist.
        if inner.state != State::default() || self.config.state_dir.exists() {
            inner.state.save(&self.config.state_dir)?;
        }
        if inner.state.enabled {
            self.apply(inner)?;
        }
        Ok(())
    }
    /// Enables canonical assignments without launching any agent process.
    /// # Errors
    /// Rejects missing prerequisites or publication/acknowledgement failures.
    pub fn enable(&self) -> Result<(), GatewayError> {
        let mut inner = self.lock();
        self.host.check_cgroup_v2()?;
        inner.state.enabled = true;
        let result = self
            .recover_locked(&mut inner)
            .and_then(|()| inner.state.save(&self.config.state_dir));
        if let Err(error) = result {
            inner.state.enabled = false;
            let error = match inner.state.save(&self.config.state_dir) {
                Ok(()) => error,
                Err(persist) => error.context(&format!(
                    "disabled intent was not persisted ({persist}); retry gateway disable before restart"
                )),
            };
            return Err(self.fail_closed(error));
        }
        Ok(())
    }
    /// Withdraws routing before stopping the proxy, retaining credentials and public CA.
    /// # Errors
    /// Host failures retain runtime files and enablement for explicit retry. After both host
    /// operations succeed, cleanup failures still persist disabled intent and require retry.
    pub fn disable(&self) -> Result<(), GatewayError> {
        let mut inner = self.lock();
        self.host.apply_nft(&nft::render_delete())?;
        self.host.stop_unit(&self.config.unit)?;
        let cleanup = self.runtime(None).remove_except_ca();
        // A stopped proxy must not be re-enabled on restart just because file cleanup failed.
        inner.state.enabled = false;
        let persisted = inner.state.save(&self.config.state_dir);
        match (cleanup, persisted) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => {
                Err(error.context("gateway disabled; runtime cleanup failed; retry disable"))
            }
            (Ok(()), Err(error)) => Err(error),
            (Err(cleanup), Err(error)) => Err(error.context(&format!(
                "runtime cleanup also failed ({cleanup}); disabled intent was not persisted; retry disable"
            ))),
        }
    }
    /// Reports non-secret execution projections visible to the authenticated UID.
    pub fn status(&self, caller_uid: u32) -> GatewayStatus {
        let inner = self.lock();
        let mut status = self.status_of(&inner.state);
        status.scopes.retain(|scope| {
            caller_uid == 0 || inner.state.scopes[&scope.scope_id].uid == caller_uid
        });
        status
    }
    /// Globally retires a credential, preserving every other credential grant.
    /// The principal boundary must restrict this administrator operation to root.
    /// # Errors
    /// Retirement/publication failures stop the data plane and withdraw routing.
    pub fn remove_key(&self, credential: &str) -> Result<KeyOutcome, GatewayError> {
        let credential = CredentialId::new(credential)?;
        let mut inner = self.lock();
        self.store(&mut inner, false)?
            .remove(&credential)
            .map_err(|error| self.fail_closed(error.into()))?;
        if inner.state.enabled {
            self.apply(&mut inner)
                .map_err(|error| self.fail_closed(error))?;
        }
        Ok(KeyOutcome { credential })
    }
    /// Lists currently imported credential identifiers without values or execution state.
    /// # Errors
    /// Returns unreadable/corrupt store errors.
    pub fn list_keys(&self) -> Result<KeyList, GatewayError> {
        let mut inner = self.lock();
        let credentials = if self.config.store_dir.exists() {
            self.store(&mut inner, false)?
                .list()
                .into_iter()
                .filter(|record| record.state == CredentialState::Active)
                .map(|record| record.id)
                .collect()
        } else {
            Vec::new()
        };
        Ok(KeyList { credentials })
    }
    /// Returns the shared public CA only while the current generation is ready.
    /// # Errors
    /// Rejects disabled, unloaded or otherwise unready data planes.
    pub fn ca(&self) -> Result<CaBundle, GatewayError> {
        let inner = self.lock();
        if !self.status_of(&inner.state).ready {
            return Err(GatewayError::unavailable(
                "gateway generation or CA is not ready",
            ));
        }
        let runtime = self.runtime(self.account()?);
        Ok(CaBundle {
            ca_bundle_path: runtime.published_ca_bundle()?,
        })
    }
    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn account(&self) -> Result<Option<GatewayAccount>, GatewayError> {
        self.host.gateway_account(&self.config.gateway_user)
    }
    pub(crate) fn check_owner(
        &self,
        uid: u32,
        account: Option<GatewayAccount>,
    ) -> Result<(), GatewayError> {
        if uid == 0 {
            return Err(GatewayError::invalid("credential Scope cannot run as root"));
        }
        let groups = self
            .host
            .user_groups(uid)
            .ok_or_else(|| GatewayError::invalid("Scope owner does not exist"))?;
        self.host.check_sensitive_groups(&groups)?;
        if account.is_some_and(|account| uid == account.uid || groups.contains(&account.gid)) {
            return Err(GatewayError::invalid(
                "Scope owner can read gateway secrets",
            ));
        }
        Ok(())
    }
    pub(crate) fn check_process(
        &self,
        process: &ProcessIdentity,
        uid: u32,
    ) -> Result<(), GatewayError> {
        if process.uids != [uid; 4]
            || self
                .account()?
                .is_some_and(|account| process.groups.contains(&account.gid))
        {
            return Err(GatewayError::new(
                GatewayErrorKind::PermissionDenied,
                "process has foreign UIDs or gateway group",
            ));
        }
        self.host.check_sensitive_groups(&process.groups)?;
        self.host.verify_process(process)
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
            inner.store = Some(if create {
                CredentialStore::create(&self.config.store_dir)?
            } else {
                CredentialStore::open(&self.config.store_dir)?
            });
        }
        inner
            .store
            .as_mut()
            .ok_or_else(|| GatewayError::internal("credential store is not open"))
    }
    /// Compiles only canonical pinned snapshots; retired/missing fakes cannot be reissued here.
    pub(crate) fn active_plans(
        &self,
        inner: &mut Inner,
    ) -> Result<BTreeMap<ScopeId, (PreparedScope, Plan)>, GatewayError> {
        let summaries = if self.config.store_dir.exists() {
            self.store(inner, false)?.list()
        } else {
            Vec::new()
        };
        let mut plans = BTreeMap::new();
        for (id, domain) in &inner.state.scopes {
            if domain.revoked {
                continue;
            }
            let resource = ResourceId::new(id.as_str())
                .map_err(|_| GatewayError::internal("invalid projected Scope"))?;
            let scope = match self.repository.get_scope(&resource) {
                Ok(scope) if scope.status == ScopeStatus::Active => scope,
                Ok(_) | Err(asc_pap::PapError::NotFound) => continue,
                Err(error) => return Err(error.into()),
            };
            let mut plan = self.check_scope(&scope)?;
            if scope.owner_uid != Some(domain.uid) {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    "canonical Scope owner changed",
                ));
            }
            let available: BTreeSet<_> = summaries
                .iter()
                .filter(|record| {
                    record.state == CredentialState::Active
                        && record
                            .fakes
                            .iter()
                            .any(|fake| fake.scope_id == *id && !fake.retired)
                })
                .map(|record| record.id.clone())
                .collect();
            plan.credentials
                .retain(|credential| available.contains(credential));
            plan.rules
                .retain(|rule| available.contains(&rule.matcher.credential_id));
            plan.validate()?;
            plans.insert(id.clone(), (scope, plan));
        }
        Ok(plans)
    }
    pub(crate) fn apply(&self, inner: &mut Inner) -> Result<(), GatewayError> {
        self.host.check_cgroup_v2()?;
        let account = self
            .account()?
            .ok_or_else(|| GatewayError::unavailable("dedicated gateway account is absent"))?;
        let runtime = self.runtime(Some(account));
        let plans = self.active_plans(inner)?;
        let generation = inner
            .state
            .generation
            .checked_add(1)
            .ok_or_else(|| GatewayError::internal("generation exhausted"))?;
        inner.state.generation = generation;
        inner.state.save(&self.config.state_dir)?;
        runtime.prepare()?;
        let mut credentials = BTreeSet::new();
        let published: Vec<_> = plans
            .iter()
            .map(|(id, (_, plan))| {
                credentials.extend(plan.credentials.iter().cloned());
                PublishedScope {
                    scope_id: id,
                    port: inner.state.scopes[id].port,
                    plan: plan.to_json(),
                }
            })
            .collect();
        for (scope, _) in plans.values() {
            self.canonical(scope)?;
        }
        runtime.publish(
            self.store(inner, true)?,
            &credentials.into_iter().collect::<Vec<_>>(),
            &published,
            generation,
        )?;
        for (scope, _) in plans.values() {
            self.canonical(scope)?;
        }
        self.host.start_unit(&self.config.unit)?;
        runtime.await_generation(generation, self.config.load_timeout)?;
        for (scope, _) in plans.values() {
            self.canonical(scope)
                .map_err(|error| self.fail_closed(error))?;
        }
        runtime.publish_ca_bundle(&self.config.system_ca_bundles)?;
        self.route(inner, &plans)?;
        runtime.remove_stale(generation)
    }
    pub(crate) fn route(
        &self,
        inner: &Inner,
        plans: &BTreeMap<ScopeId, (PreparedScope, Plan)>,
    ) -> Result<BTreeMap<ResourceId, GatewayError>, GatewayError> {
        let mut withheld = BTreeMap::new();
        if !inner.state.enabled {
            return Ok(withheld);
        }
        let mut routes = BTreeMap::new();
        let mut denied = BTreeSet::new();
        let mut accepted = Vec::new();
        for (id, (scope, plan)) in plans {
            let domain = &inner.state.scopes[id];
            // Publication waits may change owner permissions, but not the immutable compiled snapshot.
            self.check_owner(domain.uid, self.account()?)?;
            if plan.credentials.is_empty() {
                continue;
            }
            for (binding_id, attachment) in &domain.bindings {
                let binding = match self.repository.get_binding(binding_id) {
                    Ok(view) => view,
                    Err(asc_pap::PapError::NotFound) => continue,
                    Err(error) => return Err(error.into()),
                };
                if !matches!(
                    binding.status.phase,
                    BindingStatus::Applying | BindingStatus::Ready
                ) || !Self::binding_matches(&binding.spec, scope, attachment)
                {
                    continue;
                }
                if let Err(fault) = self.check_pinned(inner, binding_id, attachment, domain.uid)? {
                    let (error, invalid_members) = match fault {
                        RouteFault::Stale(error) => (error, false),
                        RouteFault::Members(error) => (error, true),
                    };
                    if invalid_members || inner.pins.contains_key(binding_id) {
                        diagnose(
                            "gateway withheld routes from an unverifiable Binding",
                            format_args!(
                                "binding_id={binding_id} invalid_members={invalid_members} error={error}"
                            ),
                        );
                    }
                    // Negative membership evidence is authoritative for the cgroup, whichever
                    // Binding references it and in whatever order they are checked.
                    if invalid_members {
                        denied.extend(attachment.cgroups().map(str::to_owned));
                    }
                    withheld.insert(binding_id.clone(), error);
                    continue;
                }
                accepted.push((binding_id, attachment.cgroup.as_str()));
                for path in attachment.cgroups() {
                    if self.host.cgroup_exists(path) {
                        routes.insert(path.to_owned(), (id, domain.port));
                    }
                }
            }
        }
        routes.retain(|path, _| !denied.contains(path));
        // A Binding that verified before another found an invalid member in its root cgroup
        // has lost its own route, so it must not be reported as deployed either.
        for (binding_id, root) in accepted {
            if denied.contains(root) {
                withheld.insert(
                    binding_id.clone(),
                    GatewayError::new(
                        GatewayErrorKind::Conflict,
                        "Binding root cgroup holds an invalid member",
                    ),
                );
            }
        }
        for (scope, _) in plans.values() {
            self.canonical(scope)?;
        }
        let redirects: Vec<_> = routes
            .iter()
            .map(|(cgroup, (id, port))| Redirect {
                scope_id: id,
                cgroup,
                port: *port,
            })
            .collect();
        self.host.apply_nft(&nft::render(&redirects))?;
        for (scope, _) in plans.values() {
            self.canonical(scope)
                .map_err(|error| self.fail_closed(error))?;
        }
        Ok(withheld)
    }
    pub(crate) fn fail_closed(&self, error: GatewayError) -> GatewayError {
        withdraw_on_failure(&self.config, self.host.as_ref(), error)
    }
    fn status_of(&self, state: &State) -> GatewayStatus {
        let ready = state.enabled
            && self.host.check_cgroup_v2().is_ok()
            && self.host.unit_active(&self.config.unit)
            && self.account().ok().flatten().is_some_and(|account| {
                let runtime = self.runtime(Some(account));
                runtime.health().loaded_generation == Some(state.generation)
                    && runtime.published_ca_bundle().is_ok()
            });
        GatewayStatus {
            enabled: state.enabled,
            ready,
            scopes: Self::scopes_of(state),
        }
    }
    fn scopes_of(state: &State) -> Vec<ScopeStatusView> {
        state
            .scopes
            .iter()
            .map(|(id, domain)| ScopeStatusView {
                scope_id: id.clone(),
                bindings: domain.bindings.keys().cloned().collect(),
            })
            .collect()
    }
}

/// Writes a gateway fault and its original error to the daemon diagnostic log.
pub(crate) fn diagnose(event: &str, detail: std::fmt::Arguments<'_>) {
    tracing::warn!(target: "asc_process_diagnostic", "{event}: {detail}");
}

fn withdraw_on_failure(
    config: &GatewayConfig,
    host: &dyn Host,
    error: GatewayError,
) -> GatewayError {
    diagnose(
        "gateway failing closed: stopping proxy and withdrawing traffic",
        format_args!("error={error}"),
    );
    // Construction can fail before an instance exists; never trust the projection to decide cleanup.
    let runtime = RuntimeDirectory {
        root: config.runtime_dir.clone(),
        gateway: Owner { uid: 0, gid: 0 },
    };
    // Do not short-circuit: another fence may still succeed when any one withdrawal fails.
    let cleanup = [
        ("stop proxy", host.stop_unit(&config.unit)),
        ("withdraw routing", host.apply_nft(&nft::render_delete())),
        ("remove runtime", runtime.remove_all()),
    ];
    let failures: Vec<_> = cleanup
        .into_iter()
        .filter_map(|(operation, result)| result.err().map(|error| format!("{operation}: {error}")))
        .collect();
    if failures.is_empty() {
        error.context("gateway stopped and traffic withdrawn; cleanup responsibility retained")
    } else {
        tracing::error!(
            target: "asc_process_diagnostic",
            failures = %failures.join("; "),
            "gateway fail-closed cleanup failed; operator must stop proxy and withdraw traffic"
        );
        error.context(&format!(
            "gateway fail-closed cleanup failed ({}); operator must stop proxy and withdraw traffic; cleanup responsibility retained",
            failures.join("; ")
        ))
    }
}

fn registration_failure(error: &asc_credential_store::CredentialStoreError) -> asc_pap::PapError {
    use asc_credential_store::CredentialStoreError;
    match error {
        CredentialStoreError::Invalid { .. }
        | CredentialStoreError::Conflict(_)
        | CredentialStoreError::DuplicateValue(_)
        | CredentialStoreError::Retired(_) => asc_pap::PapError::InvalidCedar(error.to_string()),
        _ => asc_pap::PapError::Persistence,
    }
}

impl asc_pap::CredentialRegistration for GatewayService {
    fn register(
        &self,
        source: &asc_policy_types::cedar::CedarPolicySource,
    ) -> Result<Vec<asc_policy_types::policy::CredentialBinding>, asc_pap::PapError> {
        use sha2::{Digest as _, Sha256};

        let rules = asc_policy_cedar::credential_rules(source)
            .map_err(|error| asc_pap::PapError::InvalidCedar(error.diagnostic()))?;
        let source_bytes =
            serde_json::to_vec(source).map_err(|_| asc_pap::PapError::Persistence)?;
        let key = format!("{:x}", Sha256::digest(&source_bytes));
        let mut inner = self.lock();
        let cached = if self.config.store_dir.exists() {
            self.store(&mut inner, false)
                .map_err(|_| asc_pap::PapError::Persistence)?
                .lookup_registration(&key)
                .map_err(|error| registration_failure(&error))?
        } else {
            None
        };
        let pairs = if let Some(pairs) = cached {
            pairs
        } else {
            let inputs = rules
                .iter()
                .map(|rule| {
                    let real = read_token_file(Path::new(&rule.real_token_file), 0)?;
                    let fake = read_token_file(Path::new(&rule.fake_token_file), 0)?;
                    asc_credential_store::validate_pair(real.expose(), fake.expose())?;
                    Ok(PairInput {
                        real,
                        fake: fake.expose().to_owned(),
                    })
                })
                .collect::<Result<Vec<_>, asc_credential_store::CredentialStoreError>>()
                .map_err(|_| {
                    asc_pap::PapError::InvalidCedar(
                        "unsafe token file or invalid credential pair".into(),
                    )
                })?;
            self.store(&mut inner, true)
                .map_err(|_| asc_pap::PapError::Persistence)?
                .register_policy_pairs(&key, inputs)
                .map_err(|error| registration_failure(&error))?
        };
        if inner.state.enabled {
            self.apply(&mut inner).map_err(|error| {
                self.fail_closed(error);
                asc_pap::PapError::Unavailable
            })?;
        }
        Ok(rules
            .into_iter()
            .zip(pairs)
            .map(|(rule, pair)| asc_policy_types::policy::CredentialBinding {
                rule_id: rule.rule_id,
                pair_id: pair.as_str().to_owned(),
            })
            .collect())
    }
}
