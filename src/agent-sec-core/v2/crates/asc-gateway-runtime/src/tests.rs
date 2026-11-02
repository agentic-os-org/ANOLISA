//! Execution and retirement contracts through real PAP persistence and public deployment ports.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asc_credential_store::{CredentialId, ScopeId};
use asc_pap::PapRepository;
use asc_policy_repository::{BindingStateRepository, BindingStateWrite, WriteResult};
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_target_contracts::TargetDeploymentClient;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::binding::{BindingStatus, PreparedBinding};
use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::process_discovery::ProcessIdentity as CanonicalProcess;
use asc_policy_types::scope::{PreparedScope, ProcessMatcher, ScopeSelector, ScopeStatus};
use asc_policy_types::target::{PreparedApply, TargetBindingPlan};

use crate::{
    GATEWAY_BINDING_FORMAT, GatewayAccount, GatewayBindingClient, GatewayConfig, GatewayError,
    GatewayErrorKind, GatewayService, Host, KeyRequest, ProcessIdentity,
};

const REAL: &str = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
const OTHER_REAL: &str = "ghp_9999999999abcdefghijABCDEFGHIJ012345";
const UID: u32 = 4001;
const CA: &str = "-----BEGIN CERTIFICATE-----\ngateway\n-----END CERTIFICATE-----\n";

struct TestProcess {
    parent: u32,
    start: u64,
    uid: u32,
    groups: Vec<u32>,
    cgroup: String,
}
impl TestProcess {
    fn new(parent: u32) -> Self {
        Self {
            parent,
            start: 1,
            uid: UID,
            groups: vec![UID],
            cgroup: "/user.slice".into(),
        }
    }
    fn pin(&self, pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            parent_pid: self.parent,
            start_time: self.start,
            uids: [self.uid; 4],
            groups: self.groups.clone(),
            pidfd: None,
        }
    }
}
#[derive(Default)]
struct HostLog {
    cgroup_failure: Option<&'static str>,
    account: bool,
    account_uid: Option<u32>,
    active: bool,
    processes: BTreeMap<u32, TestProcess>,
    moves: Vec<u32>,
    rules: Vec<String>,
    operations: Vec<&'static str>,
    failed_operation: Option<&'static str>,
    changed_on_attach: Option<u32>,
    fence: Option<(String, String)>,
    process_failure: Option<(u32, GatewayErrorKind)>,
    owner_sensitive_on_start: bool,
}
#[derive(Clone, Default)]
struct FakeHost(Arc<Mutex<HostLog>>);
impl FakeHost {
    fn log(&self) -> std::sync::MutexGuard<'_, HostLog> {
        self.0.lock().unwrap()
    }
}
fn account() -> GatewayAccount {
    GatewayAccount {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    }
}
impl Host for FakeHost {
    fn check_cgroup_v2(&self) -> Result<(), GatewayError> {
        if let Some(reason) = self.log().cgroup_failure {
            Err(GatewayError::unavailable(reason))
        } else {
            Ok(())
        }
    }

    fn gateway_account(&self, _: &str) -> Result<Option<GatewayAccount>, GatewayError> {
        let log = self.log();
        Ok(log.account.then(|| GatewayAccount {
            uid: log.account_uid.unwrap_or_else(|| account().uid),
            gid: account().gid,
        }))
    }
    fn user_groups(&self, uid: u32) -> Option<Vec<u32>> {
        let log = self.log();
        (uid == UID || uid == 4002).then(|| {
            if log.active && log.owner_sensitive_on_start {
                vec![uid, 0]
            } else {
                vec![uid]
            }
        })
    }
    fn check_sensitive_groups(&self, groups: &[u32]) -> Result<(), GatewayError> {
        if groups.contains(&0) {
            Err(GatewayError::new(
                GatewayErrorKind::PermissionDenied,
                "sensitive group",
            ))
        } else {
            Ok(())
        }
    }
    fn process_fence(&self, _: u32) -> Result<(String, String), GatewayError> {
        Ok(self
            .log()
            .fence
            .clone()
            .unwrap_or_else(|| ("boot-1".into(), "pid:[1]".into())))
    }
    fn start_unit(&self, _: &str) -> Result<(), GatewayError> {
        self.log().active = true;
        Ok(())
    }
    fn stop_unit(&self, _: &str) -> Result<(), GatewayError> {
        let mut log = self.log();
        log.operations.push("stop");
        if log.failed_operation == Some("stop") {
            return Err(GatewayError::unavailable("stop failed"));
        }
        log.active = false;
        Ok(())
    }
    fn unit_active(&self, _: &str) -> bool {
        self.log().active
    }
    fn apply_nft(&self, rules: &str) -> Result<(), GatewayError> {
        let mut log = self.log();
        log.rules.push(rules.into());
        if rules == crate::nft::render_delete() {
            log.operations.push("unroute");
            if log.failed_operation == Some("unroute") {
                return Err(GatewayError::unavailable("unroute failed"));
            }
        }
        Ok(())
    }
    fn attach_to_scope(
        &self,
        scope: &str,
        slice: &str,
        pin: &ProcessIdentity,
    ) -> Result<(), GatewayError> {
        self.verify_process(pin)?;
        let mut log = self.log();
        log.moves.push(pin.pid);
        let changed = log.changed_on_attach == Some(pin.pid);
        let process = log.processes.get_mut(&pin.pid).unwrap();
        process.cgroup = format!("/{slice}/{scope}");
        if changed {
            process.start += 1;
        }
        Ok(())
    }
    fn remove_scope(&self, _: &str) -> Result<(), GatewayError> {
        Ok(())
    }
    fn process_tree(&self, root: u32) -> Result<Vec<ProcessIdentity>, GatewayError> {
        let log = self.log();
        if let Some((pid, kind)) = log.process_failure
            && pid == root
        {
            return Err(GatewayError::new(kind, "cannot inspect process"));
        }
        let mut pids = vec![root];
        let mut index = 0;
        while index < pids.len() {
            let parent = pids[index];
            pids.extend(
                log.processes
                    .iter()
                    .filter(|(_, p)| p.parent == parent)
                    .map(|(pid, _)| *pid),
            );
            if pids.len() > crate::MAX_PROCESS_TREE {
                return Err(GatewayError::new(
                    GatewayErrorKind::ResourceExhausted,
                    "tree capacity",
                ));
            }
            index += 1;
        }
        pids.into_iter()
            .map(|pid| {
                log.processes
                    .get(&pid)
                    .map(|p| p.pin(pid))
                    .ok_or_else(GatewayError::process_exited)
            })
            .collect()
    }
    fn scope_processes(&self, path: &str) -> Result<Vec<ProcessIdentity>, GatewayError> {
        let expected = format!("/{path}");
        Ok(self
            .log()
            .processes
            .iter()
            .filter(|(_, p)| p.cgroup == expected)
            .map(|(pid, p)| p.pin(*pid))
            .collect())
    }
    fn verify_process(&self, pin: &ProcessIdentity) -> Result<(), GatewayError> {
        let log = self.log();
        let current = log
            .processes
            .get(&pin.pid)
            .ok_or_else(GatewayError::process_exited)?;
        if current.start != pin.start_time
            || [current.uid; 4] != pin.uids
            || current.groups != pin.groups
        {
            return Err(GatewayError::new(
                GatewayErrorKind::PermissionDenied,
                "process changed identity",
            ));
        }
        Ok(())
    }
    fn process_cgroup(&self, pid: u32) -> Result<String, GatewayError> {
        self.log()
            .processes
            .get(&pid)
            .map(|p| p.cgroup.clone())
            .ok_or_else(GatewayError::process_exited)
    }
    fn cgroup_exists(&self, path: &str) -> bool {
        self.log()
            .processes
            .values()
            .any(|p| p.cgroup == format!("/{path}"))
    }
}
struct DataPlane {
    stop: Arc<AtomicBool>,
    reject: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl DataPlane {
    fn start(runtime: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let reject = Arc::new(AtomicBool::new(false));
        let (stopped, rejected, root) = (stop.clone(), reject.clone(), runtime.to_owned());
        let thread = std::thread::spawn(move || {
            let mut loaded = None;
            let mut written = String::new();
            while !stopped.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
                let Ok(bytes) = fs::read(root.join("active.json")) else {
                    continue;
                };
                let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                let generation = manifest["generation"].as_u64().unwrap();
                let error = rejected
                    .load(Ordering::SeqCst)
                    .then_some("rejected by test");
                if error.is_none() {
                    loaded = Some(generation);
                }
                let health = serde_json::json!({"loadedGeneration": loaded, "lastAttempt": {"generation": generation, "error": error}}).to_string();
                if root.join("status").is_dir() && health != written {
                    let _ = fs::write(root.join("status/ca-cert.pem"), CA);
                    let tmp = root.join("status/.health.tmp");
                    if fs::write(&tmp, &health).is_ok() {
                        let _ = fs::rename(tmp, root.join("status/health.json"));
                    }
                    written = health;
                }
            }
        });
        Self {
            stop,
            reject,
            thread: Some(thread),
        }
    }
}
impl Drop for DataPlane {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
struct Fixture {
    host: FakeHost,
    repository: Arc<SqlitePolicyRepository>,
    config: GatewayConfig,
    plane: DataPlane,
    root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repository =
            Arc::new(SqlitePolicyRepository::open(&root.path().join("policy-state.db")).unwrap());
        let mut config = GatewayConfig::system(root.path());
        config.runtime_dir = root.path().join("run");
        let ca = root.path().join("system.pem");
        fs::write(&ca, CA).unwrap();
        config.system_ca_bundles = vec![ca];
        config.load_timeout = Duration::from_secs(2);
        let plane = DataPlane::start(&config.runtime_dir);
        let host = FakeHost::default();
        host.log().account = true;
        Self {
            host,
            repository,
            config,
            plane,
            root,
        }
    }
    fn service(&self) -> Arc<GatewayService> {
        Arc::new(
            GatewayService::new(
                self.config.clone(),
                Box::new(self.host.clone()),
                self.repository.clone(),
            )
            .unwrap(),
        )
    }
    fn key(&self, service: &GatewayService, real: &str) {
        let path = self.root.path().join("token");
        fs::write(&path, real).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let outcome = service
            .add_key(&KeyRequest {
                credential: "github".into(),
                secret_file: path,
                caller_uid: 0,
            })
            .unwrap();
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            serde_json::json!({"credential": "github"})
        );
    }
    fn secrets(&self) -> serde_json::Value {
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(self.config.runtime_dir.join("active.json")).unwrap())
                .unwrap();
        serde_json::from_slice(
            &fs::read(
                self.config
                    .runtime_dir
                    .join(manifest["secrets"].as_str().unwrap()),
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn scope(&self, name: &str, policies: usize) -> PreparedScope {
        let policy = |index: usize| -> PreparedPolicy {
            let document = serde_json::json!({"policyId": format!("policy:{name}-{index}"), "revision": 1,
            "policyName": format!("{name}-{index}"), "template": {"specVersion": "0.1", "rules": [{
                "effect": "allow", "category": "network", "action": "use_credential", "target": {
                    "type": "http_request", "credentialId": "github", "scheme": "https",
                    "host": "api.github.com", "path": format!("/policy-{index}/**"), "methods": ["GET"], "responseRewrite": "buffered"
                }, "because": "credential execution contract"
            }]}});
            serde_json::from_value(document).unwrap()
        };
        let policies: Vec<_> = (0..policies).map(policy).collect();
        for policy in &policies {
            self.repository.put_policy(policy).unwrap();
        }
        self.repository
            .put_scope(&PreparedScope {
                scope_id: ResourceId::new(name).unwrap(),
                selector: ScopeSelector::Process {
                    matcher: ProcessMatcher::Name {
                        process_name: "worker".into(),
                    },
                },
                policy_snapshots: policies,
                owner_uid: Some(UID),
                status: ScopeStatus::Active,
            })
            .unwrap()
    }
    fn instance(&self, scope: &PreparedScope, pid: u32, existing: &[u32]) -> Vec<PreparedBinding> {
        self.host
            .log()
            .processes
            .entry(pid)
            .or_insert_with(|| TestProcess::new(0));
        let identities: Vec<_> = existing
            .iter()
            .copied()
            .chain(std::iter::once(pid))
            .map(process)
            .collect();
        self.repository
            .sync_scope_instances(&scope.scope_id, &identities)
            .unwrap()
            .into_iter()
            .filter(|receipt| receipt.spec.scope.process.pid == pid)
            .map(|receipt| {
                self.transition(&receipt.spec, BindingStatus::Applying);
                receipt.spec.clone()
            })
            .collect()
    }
    fn transition(&self, binding: &PreparedBinding, status: BindingStatus) {
        let current = self
            .repository
            .get_binding_state(&binding.binding_id)
            .unwrap()
            .unwrap();
        let mut next = current.clone();
        next.binding.status = status.into();
        assert!(matches!(
            self.repository
                .compare_exchange_binding_state(&current, &BindingStateWrite::new(next))
                .unwrap(),
            WriteResult::Applied(_)
        ));
    }
    fn prepared(client: &GatewayBindingClient, binding: &PreparedBinding) -> PreparedApply {
        client
            .prepare_apply(&TargetBindingPlan {
                format: GATEWAY_BINDING_FORMAT.into(),
                content: serde_json::to_vec(binding).unwrap(),
            })
            .unwrap()
    }
    fn remove_record(&self, binding: &PreparedBinding) {
        let current = self
            .repository
            .get_binding_state(&binding.binding_id)
            .unwrap()
            .unwrap();
        assert!(matches!(
            self.repository
                .compare_exchange_binding_state(&current, &BindingStateWrite::delete())
                .unwrap(),
            WriteResult::Applied(_)
        ));
    }
    fn rules(&self) -> String {
        self.host.log().rules.last().cloned().unwrap_or_default()
    }
    fn mutate_database(&self, sql: &str, id: &ResourceId) {
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg("import sqlite3, sys\nwith sqlite3.connect(sys.argv[1]) as db:\n    assert db.execute(sys.argv[2], (sys.argv[3],)).rowcount == 1")
            .arg(self.root.path().join("policy-state.db"))
            .arg(sql)
            .arg(id.as_str())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "SQLite fault injection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
fn process(pid: u32) -> CanonicalProcess {
    CanonicalProcess {
        boot_id: "boot-1".into(),
        pid_namespace: "pid:[1]".into(),
        pid,
        start_time: 1,
    }
}

#[test]
fn initialization_and_empty_recovery_are_pure_and_import_does_not_authorize_or_issue_fake() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.recover().unwrap();
    fixture.service().recover().unwrap();
    assert!(!fixture.config.state_dir.exists() && !fixture.config.store_dir.exists());
    assert!(!fixture.config.runtime_dir.exists());
    fixture.key(&service, REAL);
    service.recover().unwrap();
    assert!(!fixture.config.state_dir.exists());
    assert_eq!(
        serde_json::to_value(service.status(0)).unwrap(),
        serde_json::json!({"enabled": false, "ready": false, "scopes": []})
    );
    assert_eq!(
        serde_json::to_value(service.list_keys().unwrap()).unwrap(),
        serde_json::json!({"credentials": ["github"]})
    );
    assert_eq!(
        service.credentials(UID, "unprepared").err().unwrap().kind(),
        GatewayErrorKind::NotFound
    );
    assert!(fixture.host.log().moves.is_empty() && fixture.host.log().operations.is_empty());
}
#[test]
fn preparation_is_durable_without_pid_or_proxy_and_credentials_require_ready_generation_and_ca() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("prepared", 1);
    service.prepare_scope(&scope).unwrap();
    assert_eq!(
        fixture.service().status(UID).scopes,
        service.status(UID).scopes
    );
    assert!(!fixture.config.runtime_dir.exists());
    assert!(fixture.host.log().moves.is_empty());
    assert_eq!(
        serde_json::to_value(service.status(UID)).unwrap(),
        serde_json::json!({"enabled": false, "ready": false,
            "scopes": [{"scopeId": "prepared", "bindings": []}]})
    );
    assert_eq!(
        service.credentials(UID, "prepared").err().unwrap().kind(),
        GatewayErrorKind::Unavailable
    );
    fs::write(
        &fixture.config.system_ca_bundles[0],
        format!("# system trust\n{CA}"),
    )
    .unwrap();
    service.enable().unwrap();
    assert_eq!(
        serde_json::to_value(service.status(UID)).unwrap(),
        serde_json::json!({"enabled": true, "ready": true,
            "scopes": [{"scopeId": "prepared", "bindings": []}]})
    );
    let grant = service.credentials(UID, "prepared").unwrap();
    let grant_json = serde_json::to_value(&grant).unwrap();
    assert_eq!(
        grant_json,
        serde_json::json!({"scopeId": "prepared", "environment": grant.environment})
    );
    assert!(!grant_json.to_string().contains(REAL));
    assert_ne!(grant.environment["GH_TOKEN"], REAL);
    assert_eq!(
        grant.environment["GH_TOKEN"],
        grant.environment["GITHUB_TOKEN"]
    );
    assert_eq!(
        service.credentials(4002, "prepared").err().unwrap().kind(),
        GatewayErrorKind::PermissionDenied
    );
}
#[test]
fn ready_and_credentials_reject_unavailable_ca_generation_or_proxy() {
    let mut fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("prepared", 1);
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    let grant = service.credentials(UID, "prepared").unwrap();
    // Stop the writer before injecting health/CA faults so observations cannot race a repair.
    fixture.plane.stop.store(true, Ordering::SeqCst);
    fixture.plane.thread.take().unwrap().join().unwrap();
    let bundle = fixture.config.runtime_dir.join("ca-bundle.pem");
    let published = fs::read(&bundle).unwrap();
    let certificate = fixture.config.runtime_dir.join("status/ca-cert.pem");
    fs::remove_file(&certificate).unwrap();
    assert!(!service.status(UID).ready);
    assert!(service.credentials(UID, "prepared").is_err());
    fs::write(&certificate, CA).unwrap();
    assert!(service.status(UID).ready);
    for invalid in [
        "not a CA bundle",
        "-----BEGIN CERTIFICATE-----\nother gateway\n-----END CERTIFICATE-----\n",
    ] {
        fs::write(&bundle, invalid).unwrap();
        assert!(!service.status(UID).ready);
        assert_eq!(
            service.credentials(UID, "prepared").err().unwrap().kind(),
            GatewayErrorKind::Unavailable
        );
        assert_eq!(fs::read(&bundle).unwrap(), invalid.as_bytes());
    }
    fs::remove_file(&bundle).unwrap();
    assert!(!service.status(UID).ready);
    assert_eq!(
        service.credentials(UID, "prepared").err().unwrap().kind(),
        GatewayErrorKind::Unavailable
    );
    assert!(!bundle.exists());
    symlink(&fixture.config.system_ca_bundles[0], &bundle).unwrap();
    assert!(!service.status(UID).ready);
    assert!(service.credentials(UID, "prepared").is_err());
    assert!(
        fs::symlink_metadata(&bundle)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&bundle).unwrap();
    fs::write(&bundle, &published).unwrap();
    fs::remove_file(&fixture.config.system_ca_bundles[0]).unwrap();
    assert_eq!(
        service.credentials(UID, "prepared").unwrap().environment,
        grant.environment
    );
    assert_eq!(fs::read(&bundle).unwrap(), published);
    assert!(service.status(UID).ready);
    let health_path = fixture.config.runtime_dir.join("status/health.json");
    let health = fs::read(&health_path).unwrap();
    let generation = service.lock().state.generation;
    fs::write(
        &health_path,
        serde_json::to_vec(&serde_json::json!({"loadedGeneration": generation - 1})).unwrap(),
    )
    .unwrap();
    assert!(!service.status(UID).ready);
    assert!(service.credentials(UID, "prepared").is_err());
    fs::remove_file(&health_path).unwrap();
    assert!(!service.status(UID).ready);
    assert!(service.credentials(UID, "prepared").is_err());
    fs::write(&health_path, health).unwrap();
    assert!(service.status(UID).ready);
    fixture.host.log().active = false;
    assert!(!service.status(UID).ready);
    assert!(service.credentials(UID, "prepared").is_err());
    fixture.host.log().active = true;
    assert!(service.status(UID).ready);
    service.disable().unwrap();
    assert!(!service.status(UID).enabled && !service.status(UID).ready);
    assert!(!fixture.host.log().active);
    assert!(service.credentials(UID, "prepared").is_err());
}
#[test]
fn unsupported_cgroup_v2_refuses_empty_enable_without_host_or_state_mutation() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.host.log().cgroup_failure = Some("cgroup v2 is unavailable");
    service.recover().unwrap();
    assert_eq!(
        service.enable().unwrap_err().kind(),
        GatewayErrorKind::Unavailable
    );
    assert!(!service.status(0).enabled && !service.status(0).ready);
    assert!(!fixture.config.state_dir.exists());
    assert!(!fixture.config.store_dir.exists());
    assert!(!fixture.config.runtime_dir.exists());
    assert!(fixture.host.log().operations.is_empty());
    assert!(!fixture.host.log().active);
}
#[test]
fn readiness_and_credentials_recheck_cgroup_v2_and_recovery_fails_closed() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("cgroup-health", 1);
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    assert!(service.status(UID).ready);
    fixture.host.log().cgroup_failure = Some("cgroup v2 is unavailable");
    let responsibilities = service.status(UID).scopes;
    assert!(!service.status(UID).ready);
    assert_eq!(
        service
            .credentials(UID, "cgroup-health")
            .err()
            .unwrap()
            .kind(),
        GatewayErrorKind::Unavailable
    );
    assert!(fixture.host.log().active);
    assert!(fixture.host.log().operations.is_empty());
    assert!(service.recover().is_err());
    assert!(!fixture.host.log().active);
    assert_eq!(fixture.host.log().operations, vec!["stop", "unroute"]);
    assert_eq!(fixture.rules(), crate::nft::render_delete());
    assert!(!fixture.config.runtime_dir.exists());
    assert_eq!(service.status(UID).scopes, responsibilities);
}
#[test]
fn readiness_validates_ca_owned_by_the_actual_gateway_account() {
    let mut fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("gateway-owner", 1);
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    fixture.plane.stop.store(true, Ordering::SeqCst);
    fixture.plane.thread.take().unwrap().join().unwrap();
    let certificate = fixture.config.runtime_dir.join("status/ca-cert.pem");
    std::os::unix::fs::chown(&certificate, Some(65534), None).unwrap();
    assert!(!service.status(UID).ready);
    fixture.host.log().account_uid = Some(65534);
    assert!(service.credentials(UID, "gateway-owner").is_ok());
    assert!(service.status(UID).ready);
    fixture.host.log().account = false;
    assert!(!service.status(UID).ready);
}
#[test]
fn admission_rejects_root_and_missing_owner_without_process_scan() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let scope = fixture.scope("admission", 1);
    for uid in [None, Some(0), Some(9999)] {
        let mut scope = scope.clone();
        scope.owner_uid = uid;
        assert!(service.check_scope(&scope).is_err());
    }
    assert!(fixture.host.log().moves.is_empty());
    assert!(!fixture.config.store_dir.exists());
}
#[test]
fn canonical_snapshot_and_applying_phase_fence_side_effects() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("fenced", 1);
    service.prepare_scope(&scope).unwrap();
    let binding = fixture.instance(&scope, 77, &[]).remove(0);
    let client = GatewayBindingClient::new(service.clone());
    let prepared = Fixture::prepared(&client, &binding);
    fixture.transition(&binding, BindingStatus::Ready);
    assert!(client.create(&prepared).error.is_some());
    assert!(fixture.host.log().moves.is_empty());
    let mut stale = scope.clone();
    stale.policy_snapshots[0].revision = Revision::new(2).unwrap();
    assert!(service.prepare_scope(&stale).is_err());
}
#[test]
fn boot_namespace_start_uid_and_descendant_groups_are_verified_before_moves() {
    for fault in ["boot", "namespace", "start", "uid", "groups"] {
        let fixture = Fixture::new();
        let service = fixture.service();
        fixture.key(&service, REAL);
        let scope = fixture.scope("identity", 1);
        service.prepare_scope(&scope).unwrap();
        let binding = fixture.instance(&scope, 77, &[]).remove(0);
        fixture
            .host
            .log()
            .processes
            .insert(78, TestProcess::new(77));
        match fault {
            "boot" => fixture.host.log().fence = Some(("boot-2".into(), "pid:[1]".into())),
            "namespace" => fixture.host.log().fence = Some(("boot-1".into(), "pid:[2]".into())),
            "start" => fixture.host.log().processes.get_mut(&77).unwrap().start = 2,
            "uid" => fixture.host.log().processes.get_mut(&78).unwrap().uid = 4002,
            "groups" => fixture
                .host
                .log()
                .processes
                .get_mut(&78)
                .unwrap()
                .groups
                .push(0),
            _ => unreachable!(),
        }
        let client = GatewayBindingClient::new(service);
        let plan = TargetBindingPlan {
            format: GATEWAY_BINDING_FORMAT.into(),
            content: serde_json::to_vec(&binding).unwrap(),
        };
        let prepared = client.prepare_apply(&plan).unwrap();
        assert!(client.create(&prepared).error.is_some());
        assert!(fixture.host.log().moves.is_empty(), "{fault}");
    }
}
#[test]
fn shared_process_multi_policy_is_idempotent_and_cleanup_preserves_other_instances() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("shared", 2);
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    let first = fixture.instance(&scope, 77, &[]);
    let second = fixture.instance(&scope, 88, &[77]);
    let client = GatewayBindingClient::new(service.clone());
    let targets: Vec<_> = first
        .iter()
        .chain(&second)
        .map(|binding| {
            let prepared = Fixture::prepared(&client, binding);
            assert!(client.create(&prepared).error.is_none());
            assert!(client.create(&prepared).error.is_none());
            prepared.target
        })
        .collect();
    assert_eq!(fixture.host.log().moves, vec![77, 88]);
    assert_eq!(fixture.rules().matches("counter redirect").count(), 2);
    assert!(client.delete(&targets[..1]).error.is_none());
    assert_eq!(fixture.rules().matches("counter redirect").count(), 2);
    assert!(client.delete(&targets[1..2]).error.is_none());
    assert_eq!(fixture.rules().matches("counter redirect").count(), 1);
    assert!(fixture.host.log().processes.contains_key(&77));
    assert!(service.credentials(UID, "shared").is_ok());
    assert!(fixture.secrets()["retired"].as_array().unwrap().is_empty());
    fixture.host.log().processes.remove(&88);
    service.recover().unwrap();
    assert!(!fixture.rules().contains("counter redirect"));
}
#[test]
fn recovery_isolates_invalid_ready_instances_but_unknown_host_failures_close_all_routes() {
    for fault in [
        "reused", "uid", "groups", "cgroup", "exited", "unknown", "internal",
    ] {
        let fixture = Fixture::new();
        let service = fixture.service();
        fixture.key(&service, REAL);
        let scope = fixture.scope("recovery-isolation", 1);
        service.prepare_scope(&scope).unwrap();
        service.enable().unwrap();
        let stale = fixture.instance(&scope, 77, &[]).remove(0);
        let healthy = fixture.instance(&scope, 88, &[77]).remove(0);
        let client = GatewayBindingClient::new(service.clone());
        for binding in [&stale, &healthy] {
            assert!(
                client
                    .create(&Fixture::prepared(&client, binding))
                    .error
                    .is_none()
            );
            fixture.transition(binding, BindingStatus::Ready);
        }
        assert_eq!(fixture.rules().matches("counter redirect").count(), 2);
        let responsibilities = service.status(0).scopes;
        let stale_cgroup = fixture.host.log().processes[&77].cgroup.clone();
        let healthy_cgroup = fixture.host.log().processes[&88].cgroup.clone();
        drop(client);
        drop(service);
        {
            let mut log = fixture.host.log();
            match fault {
                "reused" => log.processes.get_mut(&77).unwrap().start += 1,
                "uid" => log.processes.get_mut(&77).unwrap().uid = 4002,
                "groups" => log.processes.get_mut(&77).unwrap().groups.push(0),
                "cgroup" => log.processes.get_mut(&77).unwrap().cgroup = "/user.slice".into(),
                "exited" => {
                    log.processes.remove(&77);
                }
                "unknown" => log.process_failure = Some((77, GatewayErrorKind::Unavailable)),
                "internal" => log.process_failure = Some((77, GatewayErrorKind::Internal)),
                _ => unreachable!(),
            }
            log.operations.clear();
        }
        let restarted = fixture.service();
        let result = restarted.recover();
        if matches!(fault, "unknown" | "internal") {
            assert!(result.is_err(), "{fault}");
            assert!(!fixture.host.log().active, "{fault}");
            assert_eq!(fixture.rules(), crate::nft::render_delete(), "{fault}");
            assert_eq!(fixture.host.log().operations, vec!["stop", "unroute"]);
        } else {
            result.unwrap();
            assert!(fixture.host.log().active, "{fault}");
            assert_eq!(
                fixture.rules().matches("counter redirect").count(),
                1,
                "{fault}"
            );
            assert!(
                fixture
                    .rules()
                    .contains(healthy_cgroup.trim_start_matches('/')),
                "{fault}"
            );
            assert!(
                !fixture
                    .rules()
                    .contains(stale_cgroup.trim_start_matches('/')),
                "{fault}"
            );
            assert!(fixture.host.log().operations.is_empty(), "{fault}");
        }
        assert_eq!(restarted.status(0).scopes, responsibilities, "{fault}");
        assert_eq!(
            fixture.service().status(0).scopes,
            responsibilities,
            "{fault}"
        );
        assert_eq!(fixture.host.log().moves, vec![77, 88], "{fault}");
    }
}

#[test]
fn failed_attachment_retains_cleanup_and_stops_proxy_without_killing_targets() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("failed", 1);
    service.prepare_scope(&scope).unwrap();
    let binding = fixture.instance(&scope, 77, &[]).remove(0);
    fixture.host.log().changed_on_attach = Some(77);
    let client = GatewayBindingClient::new(service.clone());
    let prepared = Fixture::prepared(&client, &binding);
    assert!(client.create(&prepared).error.is_some());
    assert_eq!(
        serde_json::to_value(service.status(0).scopes).unwrap(),
        serde_json::json!([{"scopeId": "failed", "bindings": [binding.binding_id]}])
    );
    assert!(!fixture.host.log().active);
    assert!(fixture.host.log().processes.contains_key(&77));
    assert_eq!(fixture.rules(), crate::nft::render_delete());
    assert!(client.delete(&[prepared.target]).error.is_none());
}
#[test]
fn publication_rechecks_owner_permissions_before_routing() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("owner-changed", 1);
    service.prepare_scope(&scope).unwrap();
    let responsibilities = service.status(0).scopes;
    fixture.host.log().owner_sensitive_on_start = true;
    assert_eq!(
        service.enable().unwrap_err().kind(),
        GatewayErrorKind::PermissionDenied
    );
    assert!(!fixture.host.log().active);
    assert_eq!(fixture.host.log().operations, vec!["stop", "unroute"]);
    assert_eq!(fixture.rules(), crate::nft::render_delete());
    assert_eq!(service.status(0).scopes, responsibilities);
}
#[test]
fn scope_deletion_retires_fakes_before_binding_cleanup_and_requires_explicit_release() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("retired", 1);
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    let binding = fixture.instance(&scope, 77, &[]).remove(0);
    let client = GatewayBindingClient::new(service.clone());
    let prepared = Fixture::prepared(&client, &binding);
    assert!(client.create(&prepared).error.is_none());
    let fake = service.credentials(UID, "retired").unwrap().environment["GH_TOKEN"].clone();
    let deleting = fixture
        .repository
        .begin_scope_delete(&scope.scope_id)
        .unwrap()
        .unwrap();
    service.revoke_scope(&deleting).unwrap();
    assert_eq!(
        service.credentials(UID, "retired").err().unwrap().kind(),
        GatewayErrorKind::Conflict
    );
    let snapshot = fixture.secrets();
    assert_eq!(snapshot["entries"], serde_json::json!([]));
    assert_eq!(
        snapshot["retired"],
        serde_json::json!([{"fake": fake, "credential": "github", "scopeId": "retired"}])
    );
    assert_eq!(
        service.list_keys().unwrap().credentials,
        vec![CredentialId::new("github").unwrap()]
    );
    assert!(!fixture.rules().contains("counter redirect"));
    assert!(service.release_scope(&scope.scope_id).is_err());
    assert!(client.create(&prepared).error.is_some());
    assert!(service.prepare_scope(&scope).is_err());
    assert!(client.delete(&[prepared.target]).error.is_none());
    fixture
        .repository
        .finish_scope_discovery(&scope.scope_id)
        .unwrap();
    fixture.transition(&binding, BindingStatus::Deleting);
    assert_eq!(
        service.release_scope(&scope.scope_id).unwrap_err().kind(),
        GatewayErrorKind::Conflict
    );
    assert!(service.status(0).scopes[0].bindings.is_empty());
    fixture.remove_record(&binding);
    service.release_scope(&scope.scope_id).unwrap();
    assert!(service.status(0).scopes.is_empty());
}
#[test]
fn committed_deletion_with_failed_canonical_read_stops_proxy_and_withdraws_routes() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("deletion-read-failure", 1);
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    let binding = fixture.instance(&scope, 77, &[]).remove(0);
    let client = GatewayBindingClient::new(service.clone());
    assert!(
        client
            .create(&Fixture::prepared(&client, &binding))
            .error
            .is_none()
    );
    fixture.transition(&binding, BindingStatus::Ready);
    assert!(fixture.host.log().active);
    assert_eq!(fixture.rules().matches("counter redirect").count(), 1);
    let deleting = fixture
        .repository
        .begin_scope_delete(&scope.scope_id)
        .unwrap()
        .unwrap();
    let responsibilities = service.status(0).scopes;
    // Break the real repository's leased database path only after deletion committed.
    let database = fixture.root.path().join("policy-state.db");
    let retained = fixture.root.path().join("retained-policy-state.db");
    fs::rename(&database, &retained).unwrap();
    fixture.host.log().operations.clear();
    assert!(asc_pap::ScopeExecution::revoke(service.as_ref(), &deleting).is_err());
    assert!(!fixture.host.log().active);
    assert_eq!(fixture.host.log().operations, vec!["stop", "unroute"]);
    assert_eq!(fixture.rules(), crate::nft::render_delete());
    assert!(!fixture.config.runtime_dir.exists());
    assert_eq!(service.status(0).scopes, responsibilities);
    assert_eq!(fixture.service().status(0).scopes, responsibilities);
    assert!(fixture.host.log().processes.contains_key(&77));
    fs::rename(retained, database).unwrap();
    assert_eq!(
        fixture.repository.get_scope(&scope.scope_id).unwrap(),
        deleting
    );
}

#[test]
fn empty_scope_and_rejected_revocation_retain_durable_retirement_fence() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("empty", 1);
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    fixture.plane.reject.store(true, Ordering::SeqCst);
    let deleting = fixture
        .repository
        .begin_scope_delete(&scope.scope_id)
        .unwrap()
        .unwrap();
    assert!(service.revoke_scope(&deleting).is_err());
    let restarted = fixture.service();
    assert!(restarted.lock().state.scopes[&ScopeId::new("empty").unwrap()].revoked);
    assert_eq!(
        restarted.credentials(UID, "empty").err().unwrap().kind(),
        GatewayErrorKind::Conflict
    );
    assert!(!fixture.host.log().active);
    assert!(!fixture.config.runtime_dir.exists());
    assert!(service.prepare_scope(&scope).is_err());
    fixture
        .repository
        .finish_scope_discovery(&scope.scope_id)
        .unwrap();
    service.release_scope(&scope.scope_id).unwrap();
    assert!(service.status(0).scopes.is_empty());
}
#[test]
fn ready_scope_release_isolates_unrelated_malformed_bindings_but_orphans_fail_closed() {
    for orphan in [false, true] {
        let fixture = Fixture::new();
        let service = fixture.service();
        fixture.key(&service, REAL);
        let scope = fixture.scope("release-isolation", 1);
        service.prepare_scope(&scope).unwrap();
        let unrelated = fixture.scope("unrelated", 1);
        let binding = fixture.instance(&unrelated, 88, &[]).remove(0);
        let deleting = fixture
            .repository
            .begin_scope_delete(&scope.scope_id)
            .unwrap()
            .unwrap();
        service.revoke_scope(&deleting).unwrap();
        fixture
            .repository
            .finish_scope_discovery(&scope.scope_id)
            .unwrap();
        assert_eq!(
            fixture.repository.get_scope(&scope.scope_id).unwrap(),
            deleting
        );
        assert!(
            fixture
                .repository
                .scope_execution_ready_to_release(&scope.scope_id)
                .unwrap()
        );
        if orphan {
            fixture
                .repository
                .finish_scope_execution(&scope.scope_id)
                .unwrap();
            assert!(matches!(
                fixture.repository.get_scope(&scope.scope_id),
                Err(asc_pap::PapError::NotFound)
            ));
        }
        // Valid JSON with a malformed Binding envelope passes SQL checks, not PAP decoding.
        fixture.mutate_database(
            "UPDATE bindings SET spec_json='{}' WHERE binding_id=?",
            &binding.binding_id,
        );
        let responsibility = service.status(0).scopes;
        let result = service.release_scope(&scope.scope_id);
        if orphan {
            assert_eq!(result.unwrap_err().kind(), GatewayErrorKind::Unavailable);
            assert_eq!(service.status(0).scopes, responsibility);
        } else {
            result.unwrap();
            assert!(service.status(0).scopes.is_empty());
        }
        assert_eq!(fixture.service().status(0).scopes, service.status(0).scopes);
        // Probe last: corruption closes the repository connection for subsequent reads.
        assert!(matches!(
            fixture.repository.list_bindings(1000, 0),
            Err(asc_pap::PapError::Persistence)
        ));
    }
}
#[test]
fn orphan_scope_release_requires_revocation_and_no_local_or_canonical_bindings() {
    for fence in ["unrevoked", "attached", "canonical-binding", "empty"] {
        let fixture = Fixture::new();
        let service = fixture.service();
        fixture.key(&service, REAL);
        let scope = fixture.scope("orphan-release", 1);
        service.prepare_scope(&scope).unwrap();
        if matches!(fence, "attached" | "canonical-binding") {
            let binding = fixture.instance(&scope, 77, &[]).remove(0);
            if fence == "attached" {
                let client = GatewayBindingClient::new(service.clone());
                assert!(
                    client
                        .create(&Fixture::prepared(&client, &binding))
                        .error
                        .is_none()
                );
                fixture.transition(&binding, BindingStatus::Deleting);
                fixture.remove_record(&binding);
            }
        }
        if fence != "unrevoked" {
            let deleting = fixture
                .repository
                .begin_scope_delete(&scope.scope_id)
                .unwrap()
                .unwrap();
            service.revoke_scope(&deleting).unwrap();
        }
        // Simulate lost canonical ownership while retaining the execution responsibility.
        fixture.mutate_database("DELETE FROM scopes WHERE scope_id=?", &scope.scope_id);
        assert!(matches!(
            fixture.repository.get_scope(&scope.scope_id),
            Err(asc_pap::PapError::NotFound)
        ));
        let responsibility = service.status(0).scopes;
        let result = service.release_scope(&scope.scope_id);
        if fence == "empty" {
            result.unwrap();
            assert!(service.status(0).scopes.is_empty());
        } else {
            assert_eq!(
                result.unwrap_err().kind(),
                GatewayErrorKind::Conflict,
                "{fence}"
            );
            assert_eq!(service.status(0).scopes, responsibility, "{fence}");
        }
        assert_eq!(fixture.service().status(0).scopes, service.status(0).scopes);
    }
}
#[test]
fn conflicting_import_preserves_fake_and_real_and_reimport_never_restores_retired_scope() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("import", 1);
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    let grant = service.credentials(UID, "import").unwrap();
    let fake = grant.environment["GH_TOKEN"].clone();
    let snapshot = fixture.secrets();
    assert_eq!(snapshot["entries"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["entries"][0]["fake"], fake);
    assert_eq!(snapshot["entries"][0]["real"], REAL);
    assert_eq!(snapshot["retired"], serde_json::json!([]));

    let path = fixture.root.path().join("token");
    fs::write(&path, OTHER_REAL).unwrap();
    let error = service
        .add_key(&KeyRequest {
            credential: "github".into(),
            secret_file: path,
            caller_uid: 0,
        })
        .unwrap_err();
    assert_eq!(error.kind(), GatewayErrorKind::Conflict);
    for secret in [REAL, OTHER_REAL, fake.as_str()] {
        assert!(!error.to_string().contains(secret));
    }
    assert!(service.credentials(UID, "import").unwrap() == grant);
    assert_eq!(fixture.secrets(), snapshot);
    assert!(fixture.host.log().active);
    assert!(service.status(UID).ready);

    fixture.key(&service, REAL);
    assert!(service.credentials(UID, "import").unwrap() == grant);
    let unchanged = fixture.secrets();
    assert_eq!(unchanged["entries"], snapshot["entries"]);
    assert_eq!(unchanged["retired"], snapshot["retired"]);

    assert_eq!(
        serde_json::to_value(service.remove_key("github").unwrap()).unwrap(),
        serde_json::json!({"credential": "github"})
    );
    assert_eq!(
        serde_json::to_value(service.list_keys().unwrap()).unwrap(),
        serde_json::json!({"credentials": []})
    );
    assert_eq!(
        service.credentials(UID, "import").err().unwrap().kind(),
        GatewayErrorKind::Unavailable
    );
    let retired = serde_json::json!([
        {"fake": fake, "credential": "github", "scopeId": "import"}
    ]);
    let removed = fixture.secrets();
    assert_eq!(removed["entries"], serde_json::json!([]));
    assert_eq!(removed["retired"], retired);

    fixture.key(&service, OTHER_REAL);
    service.prepare_scope(&scope).unwrap();
    drop(service);
    let restarted = fixture.service();
    restarted.recover().unwrap();
    assert_eq!(
        restarted.list_keys().unwrap().credentials,
        vec![CredentialId::new("github").unwrap()]
    );
    assert_eq!(
        restarted.credentials(UID, "import").err().unwrap().kind(),
        GatewayErrorKind::Unavailable
    );
    let reimported = fixture.secrets();
    assert_eq!(reimported["entries"], serde_json::json!([]));
    assert_eq!(reimported["retired"], retired);
}
#[test]
fn recovery_uses_pinned_scope_not_latest_policy_and_rejects_cross_scope_process_overlap() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.key(&service, REAL);
    let scope = fixture.scope("pinned", 1);
    service.prepare_scope(&scope).unwrap();
    let binding = fixture.instance(&scope, 77, &[]).remove(0);
    let client = GatewayBindingClient::new(service.clone());
    assert!(
        client
            .create(&Fixture::prepared(&client, &binding))
            .error
            .is_none()
    );
    let mut newer = scope.policy_snapshots[0].clone();
    newer.revision = Revision::new(2).unwrap();
    let PolicySource::SecCore(template) = &mut newer.template else {
        panic!("expected native test policy");
    };
    template.rules[0].because = Some("new revision".into());
    fixture.repository.put_policy(&newer).unwrap();
    service.enable().unwrap();
    let generation = service.lock().state.generation;
    let plan: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture
                .config
                .runtime_dir
                .join(format!("plan-pinned-{generation}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        plan,
        serde_json::json!({
            "format": "agent-sec-gateway-plan/v1",
            "scopeId": "pinned",
            "policies": [{"id": scope.policy_snapshots[0].policy_id, "revision": 1}],
            "credentials": ["github"],
            "rules": [{
                "source": {"policyId": scope.policy_snapshots[0].policy_id,
                    "policyRevision": 1, "ruleIndex": 0},
                "match": {"scheme": "https", "host": "api.github.com", "path": "/policy-0/**",
                    "methods": ["GET"], "credentialId": "github"},
                "responseRewrite": "buffered",
                "because": "credential execution contract"
            }]
        })
    );
    let other = fixture.scope("other", 1);
    service.prepare_scope(&other).unwrap();
    let other_binding = fixture.instance(&other, 77, &[]).remove(0);
    let before = fixture.host.log().moves.clone();
    assert!(
        client
            .create(&Fixture::prepared(&client, &other_binding))
            .error
            .is_some()
    );
    assert_eq!(fixture.host.log().moves, before);
}
#[test]
fn private_secret_file_and_disable_order_preserve_security_boundaries() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let path = fixture.root.path().join("token");
    fs::write(&path, REAL).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let mut request = KeyRequest {
        credential: "github".into(),
        secret_file: path.clone(),
        caller_uid: 0,
    };
    assert!(service.add_key(&request).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    request.caller_uid = UID;
    assert_eq!(
        service.add_key(&request).unwrap_err().kind(),
        GatewayErrorKind::PermissionDenied
    );
    request.caller_uid = 0;
    let link = fixture.root.path().join("link");
    symlink(path, &link).unwrap();
    request.secret_file = link;
    assert!(service.add_key(&request).is_err());
    fixture.key(&service, REAL);
    service.enable().unwrap();
    let ca = fs::read(fixture.config.runtime_dir.join("ca-bundle.pem")).unwrap();
    fixture.host.log().operations.clear();
    service.disable().unwrap();
    assert_eq!(fixture.host.log().operations, vec!["unroute", "stop"]);
    assert_eq!(
        fs::read(fixture.config.runtime_dir.join("ca-bundle.pem")).unwrap(),
        ca
    );
}
#[test]
fn disable_failures_preserve_host_fences_and_persist_disabled_after_runtime_cleanup_failure() {
    for fault in ["unroute", "stop", "runtime"] {
        let fixture = Fixture::new();
        let service = fixture.service();
        fixture.key(&service, REAL);
        let scope = fixture.scope("disable-retry", 1);
        service.prepare_scope(&scope).unwrap();
        service.enable().unwrap();
        let runtime = &fixture.config.runtime_dir;
        let retained = fixture.root.path().join("retained-runtime");
        let ca = fs::read(runtime.join("ca-bundle.pem")).unwrap();
        if fault == "runtime" {
            fs::rename(runtime, &retained).unwrap();
            symlink(&retained, runtime).unwrap();
        } else {
            fixture.host.log().failed_operation = Some(fault);
        }
        fixture.host.log().operations.clear();
        let error = service.disable().unwrap_err();
        let expected_enabled = fault != "runtime";
        assert_eq!(service.status(0).enabled, expected_enabled, "{fault}");
        assert_eq!(fixture.service().status(0), service.status(0), "{fault}");
        assert!(runtime.join("active.json").exists(), "{fault}");
        assert_eq!(fs::read(runtime.join("ca-bundle.pem")).unwrap(), ca);
        assert_eq!(
            fixture.host.log().operations,
            if fault == "unroute" {
                vec!["unroute"]
            } else {
                vec!["unroute", "stop"]
            },
            "{fault}"
        );
        drop(service);
        let restarted = fixture.service();
        if fault == "runtime" {
            assert!(error.to_string().contains("retry disable"));
            let before = fixture.host.log().operations.clone();
            restarted.recover().unwrap();
            assert_eq!(fixture.host.log().operations, before);
            assert!(!fixture.host.log().active);
            assert!(restarted.credentials(UID, "disable-retry").is_err());
            assert!(runtime.join("active.json").exists());
            fs::remove_file(runtime).unwrap();
            fs::rename(&retained, runtime).unwrap();
        }
        fixture.host.log().failed_operation = None;
        restarted.disable().unwrap();
        let disabled = restarted.status(0);
        assert!(!disabled.enabled && !disabled.ready);
        assert!(!fixture.host.log().active);
        assert_eq!(fixture.service().status(0), disabled);
        assert!(!runtime.join("active.json").exists());
        assert_eq!(fs::read(runtime.join("ca-bundle.pem")).unwrap(), ca);
        assert_eq!(
            restarted.list_keys().unwrap().credentials,
            vec![CredentialId::new("github").unwrap()]
        );
    }
}
#[test]
fn nft_paths_cannot_escape_owned_table_or_redirect_loopback() {
    let id = ScopeId::new("domain").unwrap();
    let rules = crate::nft::render(&[crate::nft::Redirect {
        scope_id: &id,
        cgroup: "agentsec.slice/instance.scope",
        port: 15001,
    }]);
    assert!(
        rules.contains("ip daddr 127.0.0.0/8 return") && rules.contains("ip6 daddr ::1 return")
    );
    for path in ["", "a//b", "a/../b", "a\"b", "a b", "/abs"] {
        assert!(!crate::nft::is_safe_cgroup_path(path));
    }
}

#[test]
#[ignore = "root/systemd/cgroup-v2 integration; creates an isolated test scope"]
fn system_host_pins_existing_descendants_and_future_children_inherit_execution_scope() {
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::process::CommandExt as _;
    use std::process::{Child, Command, Stdio};

    struct Session(Child);
    impl Drop for Session {
        fn drop(&mut self) {
            if let Some(mut input) = self.0.stdin.take() {
                let _ = input.write_all(b"next\nexit\n");
            }
            let _ = self.0.wait();
        }
    }
    assert_eq!(rustix::process::geteuid().as_raw(), 0, "requires root");
    let uid = 65534;
    let mut session = Session(Command::new("/bin/sh")
        .args(["-c", "sleep 120 & first=$!; printf '%s\\n' \"$first\"; read request; sleep 120 & second=$!; printf '%s\\n' \"$second\"; read request; kill \"$first\" \"$second\"; wait; exit 0"])
        .uid(uid).gid(uid).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap());
    let mut output = std::io::BufReader::new(session.0.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let first: u32 = line.trim().parse().unwrap();
    let host = crate::SystemHost;
    let pins = host.process_tree(session.0.id()).unwrap();
    assert!(pins.iter().any(|pin| pin.pid == first));
    let slice = format!("ascgatewaytest{}.slice", std::process::id());
    let scope = format!("asc-gateway-test-{}.scope", session.0.id());
    let expected = format!("/{slice}/{scope}");
    for pin in &pins {
        let (boot, namespace) = host.process_fence(pin.pid).unwrap();
        assert!(!boot.is_empty() && namespace.starts_with("pid:["));
        host.attach_to_scope(&scope, &slice, pin).unwrap();
        host.verify_process(pin).unwrap();
        assert_eq!(host.process_cgroup(pin.pid).unwrap(), expected);
    }
    session
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"next\n")
        .unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let second: u32 = line.trim().parse().unwrap();
    assert_eq!(host.process_cgroup(second).unwrap(), expected);
    assert!(
        host.process_tree(session.0.id())
            .unwrap()
            .iter()
            .all(|pin| pin.uids == [uid; 4])
    );
    drop(session);
    assert_eq!(
        host.verify_process(&pins[0]).unwrap_err().kind(),
        GatewayErrorKind::Unavailable
    );
    host.remove_scope(&scope).unwrap();
}
