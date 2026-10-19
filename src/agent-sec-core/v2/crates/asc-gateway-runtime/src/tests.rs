use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::nft::{self, Redirect};
use crate::{
    CredentialChange, GatewayAccount, GatewayConfig, GatewayError, GatewayErrorKind,
    GatewayService, Host, KeyRequest, ProcessIdentity,
};
use asc_credential_store::AgentName;

const REAL: &str = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
const OTHER_REAL: &str = "ghp_9999999999abcdefghijABCDEFGHIJ012345";
const DEV_UID: u32 = 4001;
const OPS_UID: u32 = 4002;
/// A user that is also a member of the gateway account's group.
const GATEWAY_GROUP_UID: u32 = 4003;
const CA: &str = "-----BEGIN CERTIFICATE-----\ngateway\n-----END CERTIFICATE-----\n";

#[derive(Default)]
struct HostLog {
    account: bool,
    cgroups: BTreeSet<String>,
    rulesets: Vec<String>,
    units: Vec<String>,
    attached: Vec<(String, u32)>,
    processes: BTreeMap<u32, TestProcess>,
    change_after_snapshot: BTreeSet<u32>,
    exit_after_snapshot: BTreeSet<u32>,
    change_after_attach: BTreeSet<u32>,
    exit_after_attach: BTreeSet<u32>,
    fail_attach: BTreeSet<u32>,
    wrong_cgroup: BTreeSet<u32>,
}

struct TestProcess {
    parent: Option<u32>,
    start_time: u64,
    uids: [u32; 4],
    groups: Vec<u32>,
    cgroup: String,
}

impl TestProcess {
    fn new(uid: u32, parent: Option<u32>) -> Self {
        Self {
            parent,
            start_time: 1,
            uids: [uid; 4],
            groups: vec![uid],
            cgroup: "/user.slice".into(),
        }
    }

    fn identity(&self, pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            parent_pid: self.parent.unwrap_or(0),
            start_time: self.start_time,
            uids: self.uids,
            groups: self.groups.clone(),
            pidfd: None,
        }
    }
}

#[derive(Clone, Default)]
struct FakeHost(Arc<Mutex<HostLog>>);

impl FakeHost {
    fn log(&self) -> std::sync::MutexGuard<'_, HostLog> {
        self.0.lock().unwrap()
    }
}

fn me() -> GatewayAccount {
    GatewayAccount {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    }
}

impl Host for FakeHost {
    fn gateway_account(&self, _user: &str) -> Result<Option<GatewayAccount>, GatewayError> {
        Ok(self.log().account.then(me))
    }

    fn user_groups(&self, uid: u32) -> Option<Vec<u32>> {
        match uid {
            DEV_UID | OPS_UID => Some(vec![uid]),
            GATEWAY_GROUP_UID => Some(vec![uid, me().gid]),
            _ => None,
        }
    }

    fn start_unit(&self, unit: &str) -> Result<(), GatewayError> {
        self.log().units.push(format!("start {unit}"));
        Ok(())
    }

    fn stop_unit(&self, unit: &str) -> Result<(), GatewayError> {
        self.log().units.push(format!("stop {unit}"));
        Ok(())
    }

    fn unit_active(&self, _unit: &str) -> bool {
        false
    }

    fn apply_nft(&self, ruleset: &str) -> Result<(), GatewayError> {
        self.log().rulesets.push(ruleset.to_owned());
        Ok(())
    }

    fn attach_to_scope(
        &self,
        scope: &str,
        slice: &str,
        process: &ProcessIdentity,
    ) -> Result<(), GatewayError> {
        self.verify_process(process)?;
        let pid = process.pid;
        let mut log = self.log();
        if log.fail_attach.contains(&pid) {
            return Err(GatewayError::unavailable("attachment rejected"));
        }
        log.cgroups.insert(format!("{slice}/{scope}"));
        log.attached.push((scope.to_owned(), pid));
        let changed = log.change_after_attach.contains(&pid);
        let wrong_cgroup = log.wrong_cgroup.contains(&pid);
        let target = log.processes.get_mut(&pid).unwrap();
        if !wrong_cgroup {
            target.cgroup = format!("/{slice}/{scope}");
        }
        if changed {
            target.start_time += 1;
        }
        if log.exit_after_attach.contains(&pid) {
            log.processes.remove(&pid);
        }
        Ok(())
    }

    fn remove_scope(&self, scope: &str) -> Result<(), GatewayError> {
        self.log()
            .cgroups
            .remove(&format!("agentsec.slice/{scope}"));
        Ok(())
    }

    fn process_cgroup(&self, pid: u32) -> Result<String, GatewayError> {
        self.log()
            .processes
            .get(&pid)
            .map(|process| process.cgroup.clone())
            .ok_or_else(|| GatewayError::unavailable("no such process"))
    }

    fn process_tree(&self, root: u32) -> Result<Vec<ProcessIdentity>, GatewayError> {
        let log = self.log();
        let mut pids = vec![root];
        let mut index = 0;
        while index < pids.len() {
            let parent = pids[index];
            pids.extend(
                log.processes
                    .iter()
                    .filter(|(_, process)| process.parent == Some(parent))
                    .map(|(pid, _)| *pid),
            );
            if pids.len() > crate::MAX_PROCESS_TREE {
                return Err(GatewayError::new(
                    GatewayErrorKind::ResourceExhausted,
                    "process tree exceeds 4096 processes",
                ));
            }
            index += 1;
        }
        let snapshot = pids
            .into_iter()
            .map(|pid| {
                let process = log
                    .processes
                    .get(&pid)
                    .ok_or_else(|| GatewayError::unavailable("no such process"))?;
                Ok(process.identity(pid))
            })
            .collect::<Result<Vec<_>, GatewayError>>()?;
        drop(log);
        let mut log = self.log();
        for pid in std::mem::take(&mut log.change_after_snapshot) {
            log.processes.get_mut(&pid).unwrap().start_time += 1;
        }
        for pid in std::mem::take(&mut log.exit_after_snapshot) {
            log.processes.remove(&pid);
        }
        Ok(snapshot)
    }

    fn scope_processes(&self, path: &str) -> Result<Vec<ProcessIdentity>, GatewayError> {
        let prefix = format!("/{path}");
        let log = self.log();
        let members: Vec<_> = log
            .processes
            .iter()
            .filter(|(_, process)| {
                process.cgroup == prefix
                    || process
                        .cgroup
                        .strip_prefix(&prefix)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
            .map(|(pid, process)| process.identity(*pid))
            .collect();
        if members.len() > crate::MAX_PROCESS_TREE {
            return Err(GatewayError::new(
                GatewayErrorKind::ResourceExhausted,
                "scope exceeds 4096 processes",
            ));
        }
        Ok(members)
    }

    fn verify_process(&self, process: &ProcessIdentity) -> Result<(), GatewayError> {
        let log = self.log();
        let current = log
            .processes
            .get(&process.pid)
            .ok_or_else(|| GatewayError::unavailable("process exited"))?;
        if current.start_time != process.start_time
            || current.uids != process.uids
            || current.groups != process.groups
        {
            return Err(GatewayError::new(
                GatewayErrorKind::PermissionDenied,
                "process identity changed",
            ));
        }
        Ok(())
    }

    fn cgroup_exists(&self, path: &str) -> bool {
        self.log().cgroups.contains(path)
    }
}

/// Stands in for the mitmproxy addon: acknowledges or rejects each published generation.
struct DataPlane {
    stop: Arc<AtomicBool>,
    reject: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DataPlane {
    fn start(runtime: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let reject = Arc::new(AtomicBool::new(false));
        let (stop_flag, reject_flag, runtime) = (stop.clone(), reject.clone(), runtime.to_owned());
        let thread = std::thread::spawn(move || {
            let mut loaded: Option<u64> = None;
            let mut written = String::new();
            while !stop_flag.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
                let status = runtime.join("status");
                let Ok(raw) = fs::read(runtime.join("active.json")) else {
                    continue;
                };
                let manifest: serde_json::Value = serde_json::from_slice(&raw).unwrap();
                let generation = manifest["generation"].as_u64().unwrap();
                let error = reject_flag
                    .load(Ordering::SeqCst)
                    .then_some("rejected by test");
                if error.is_none() {
                    loaded = Some(generation);
                }
                let health = serde_json::json!({
                    "version": 1,
                    "loadedGeneration": loaded,
                    "lastAttempt": {"generation": generation, "error": error},
                });
                // Write only on change so disable's directory removal does not race a rewrite.
                let health = health.to_string();
                if status.is_dir() && health != written {
                    let _ = fs::write(status.join("ca-cert.pem"), CA);
                    let _ = fs::write(status.join("health.json"), &health);
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
            let _ = thread.join();
        }
    }
}

struct Fixture {
    root: tempfile::TempDir,
    host: FakeHost,
    config: GatewayConfig,
    _plane: DataPlane,
    reject: Arc<AtomicBool>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        fs::create_dir(&data).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
        let bundle = root.path().join("system-bundle.pem");
        fs::write(
            &bundle,
            "-----BEGIN CERTIFICATE-----\nsystem\n-----END CERTIFICATE-----",
        )
        .unwrap();
        let mut config = GatewayConfig::system(&data);
        config.runtime_dir = root.path().join("run");
        config.system_ca_bundles = vec![root.path().join("missing.pem"), bundle];
        config.load_timeout = Duration::from_secs(5);
        let plane = DataPlane::start(&config.runtime_dir);
        let reject = plane.reject.clone();
        let host = FakeHost::default();
        host.log().account = true;
        Self {
            root,
            host,
            config,
            _plane: plane,
            reject,
        }
    }

    fn service(&self) -> GatewayService {
        GatewayService::new(self.config.clone(), Box::new(self.host.clone())).unwrap()
    }

    fn secret_file(&self, name: &str, value: &str) -> PathBuf {
        let path = self.root.path().join(name);
        fs::write(&path, format!("{value}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    fn register(&self, service: &GatewayService, uid: u32, agent: &str) {
        let pid = 100 + u32::try_from(self.host.log().processes.len()).unwrap();
        self.host
            .log()
            .processes
            .insert(pid, TestProcess::new(uid, None));
        service.register(uid, pid, agent).unwrap();
    }

    fn request(&self, agent: &str, value: &str) -> KeyRequest {
        KeyRequest {
            agent: agent.into(),
            preset: "github".into(),
            secret_file: self.secret_file(&format!("{agent}.token"), value),
            caller_uid: me().uid,
        }
    }

    fn runtime(&self, name: &str) -> PathBuf {
        self.config.runtime_dir.join(name)
    }

    fn runtime_files(&self) -> BTreeSet<String> {
        fs::read_dir(&self.config.runtime_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    }

    fn last_ruleset(&self) -> String {
        self.host.log().rulesets.last().cloned().unwrap_or_default()
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn agent(name: &str) -> AgentName {
    AgentName::new(name).unwrap()
}

#[test]
fn enable_requires_the_gateway_account() {
    let fixture = Fixture::new();
    fixture.host.log().account = false;
    let error = fixture.service().enable().unwrap_err();
    assert_eq!(error.kind(), GatewayErrorKind::Unavailable);
    assert!(!fixture.config.runtime_dir.exists());
}

#[test]
fn enable_publishes_a_generation_the_data_plane_loads() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    let added = service.add_key(&fixture.request("dev", REAL)).unwrap();
    assert_eq!(added.credential, CredentialChange::Created);
    assert!(!added.applied);
    assert!(!fixture.config.runtime_dir.exists());

    let status = service.enable().unwrap();
    assert!(status.enabled);
    assert_eq!(status.loaded_generation, Some(status.generation));
    assert_eq!(status.agents[0].port, 15001);
    let generation = status.generation;
    assert_eq!(
        fixture.runtime_files(),
        BTreeSet::from([
            "active.json".to_owned(),
            "ca-bundle.pem".to_owned(),
            format!("plan-dev-{generation}.json"),
            format!("secrets-{generation}.json"),
            "status".to_owned(),
        ])
    );
    assert_eq!(mode(&fixture.config.runtime_dir), 0o751);
    assert_eq!(mode(&fixture.runtime("status")), 0o755);
    assert_eq!(mode(&fixture.runtime("active.json")), 0o640);
    assert_eq!(
        mode(&fixture.runtime(&format!("secrets-{generation}.json"))),
        0o640
    );
    assert_eq!(mode(&fixture.runtime("ca-bundle.pem")), 0o644);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.runtime("active.json")).unwrap()).unwrap();
    assert_eq!(
        manifest,
        serde_json::json!({
            "version": 1,
            "generation": generation,
            "secrets": format!("secrets-{generation}.json"),
            "listeners": [{"port": 15001, "agent": "dev", "plan": format!("plan-dev-{generation}.json")}],
        })
    );
    let bundle = fs::read_to_string(fixture.runtime("ca-bundle.pem")).unwrap();
    assert!(bundle.contains("system") && bundle.ends_with(CA));
    let secrets =
        fs::read_to_string(fixture.runtime(&format!("secrets-{generation}.json"))).unwrap();
    assert!(secrets.contains(REAL));
    let state = fs::read_to_string(fixture.root.path().join("data/gateway/state.json")).unwrap();
    assert!(!state.contains(REAL), "the state file leaks the real value");
    assert_eq!(
        fixture.host.log().units,
        ["start agent-sec-gateway.service"]
    );
    assert!(fixture.last_ruleset().contains("socket cgroupv2"));
}

#[test]
fn changes_while_enabled_publish_new_generations_and_drop_old_files() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    let first = service.enable().unwrap().generation;
    fixture.register(&service, OPS_UID, "ops");
    let added = service.add_key(&fixture.request("ops", REAL)).unwrap();
    assert!(added.applied);
    assert_eq!(added.credential, CredentialChange::Unchanged);
    let status = service.status(0);
    assert!(status.generation > first);
    assert_eq!(status.loaded_generation, Some(status.generation));
    let files = fixture.runtime_files();
    assert!(
        files
            .iter()
            .all(|name| !name.ends_with(&format!("-{first}.json")))
    );
    assert!(files.contains(&format!("plan-ops-{}.json", status.generation)));
    assert_eq!(
        status
            .agents
            .iter()
            .map(|agent| agent.port)
            .collect::<Vec<_>>(),
        [15001, 15002]
    );

    let rotated = service
        .add_key(&fixture.request("dev", OTHER_REAL))
        .unwrap();
    assert_eq!(rotated.credential, CredentialChange::Replaced);
    let generation = service.status(0).generation;
    let secrets =
        fs::read_to_string(fixture.runtime(&format!("secrets-{generation}.json"))).unwrap();
    assert!(secrets.contains(OTHER_REAL) && !secrets.contains(REAL));

    let reloaded = fixture.service().status(0);
    assert_eq!(reloaded.agents.len(), 2);
    assert_eq!(reloaded.generation, generation);
}

#[test]
fn credentials_are_uid_scoped_readonly_and_independent_of_gateway_health() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    fixture.register(&service, OPS_UID, "ops");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    let attachments = fixture.host.log().attached.clone();
    let rulesets = fixture.host.log().rulesets.clone();
    let grant = service.credentials(DEV_UID, "dev").unwrap();
    assert!(grant == service.credentials(0, "dev").unwrap());
    assert_eq!(
        service.credentials(OPS_UID, "dev").err().unwrap().kind(),
        GatewayErrorKind::PermissionDenied
    );
    assert_eq!(
        service
            .credentials(DEV_UID, "missing")
            .err()
            .unwrap()
            .kind(),
        GatewayErrorKind::NotFound
    );
    assert_eq!(
        service
            .status(DEV_UID)
            .agents
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        vec![agent("dev")]
    );
    assert_eq!(
        service
            .status(OPS_UID)
            .agents
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        vec![agent("ops")]
    );
    assert!(service.status(9999).agents.is_empty());
    assert_eq!(service.status(0).agents.len(), 2);
    assert!(!fixture.runtime("ca-bundle.pem").exists());
    assert_eq!(fixture.host.log().attached, attachments);
    assert_eq!(fixture.host.log().rulesets, rulesets);
    service.enable().unwrap();
    fs::remove_file(fixture.runtime("status/health.json")).unwrap();
    assert!(grant == service.credentials(DEV_UID, "dev").unwrap());
    assert_eq!(grant.scope, "agent-sec-agent-dev.scope");
    assert_eq!(fixture.host.log().attached, attachments);
    let fake = grant.environment["GITHUB_TOKEN"].clone();
    assert_eq!(grant.environment["GH_TOKEN"], fake);
    assert_ne!(fake, REAL);
    assert!(fake.starts_with("ghp_") && fake.len() == REAL.len());
    let bundle = fixture.runtime("ca-bundle.pem").display().to_string();
    for variable in [
        "SSL_CERT_FILE",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "GIT_SSL_CAINFO",
        "NODE_EXTRA_CA_CERTS",
        "DENO_CERT",
    ] {
        assert_eq!(grant.environment[variable], bundle);
    }
    assert!(fixture.last_ruleset().contains(
        "socket cgroupv2 level 2 \"agentsec.slice/agent-sec-agent-dev.scope\" tcp dport { 80, 443 } counter redirect to :15001"
    ));
    assert!(service.status(0).agents[0].running);
}

#[test]
fn removing_keys_retires_fakes_then_the_credential() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    fixture.register(&service, OPS_UID, "ops");
    service.add_key(&fixture.request("ops", REAL)).unwrap();
    service.enable().unwrap();

    let removed = service.remove_key("dev", "github").unwrap();
    assert_eq!(removed.credential, CredentialChange::FakeRetired);
    assert!(removed.applied);
    let keys = service.list_keys().unwrap();
    assert_eq!(keys.agents.len(), 2);
    assert!(
        keys.agents
            .iter()
            .find(|entry| entry.name == agent("dev"))
            .unwrap()
            .presets
            .is_empty()
    );
    let fakes = &keys.credentials[0].fakes;
    assert!(
        fakes
            .iter()
            .any(|fake| fake.agent.as_str() == "dev" && fake.retired)
    );
    assert!(
        fakes
            .iter()
            .any(|fake| fake.agent.as_str() == "ops" && !fake.retired)
    );
    let manifest = fs::read_to_string(fixture.runtime("active.json")).unwrap();
    assert!(!manifest.contains("\"dev\""));

    assert_eq!(
        service.remove_key("dev", "github").unwrap_err().kind(),
        GatewayErrorKind::NotFound
    );
    let last = service.remove_key("ops", "github").unwrap();
    assert_eq!(last.credential, CredentialChange::Retired);
    let generation = service.status(0).generation;
    let secrets =
        fs::read_to_string(fixture.runtime(&format!("secrets-{generation}.json"))).unwrap();
    assert!(!secrets.contains(REAL));
}

#[test]
fn a_revocation_the_data_plane_rejects_stops_the_gateway() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    service.enable().unwrap();
    fixture.reject.store(true, Ordering::SeqCst);
    let error = service.remove_key("dev", "github").unwrap_err();
    assert_eq!(error.kind(), GatewayErrorKind::Internal);
    assert!(error.to_string().contains("rejected by test"), "{error}");
    assert!(error.to_string().contains("gateway stopped"), "{error}");
    // Older snapshots still hold the revoked value, so none may survive.
    assert!(!fixture.config.runtime_dir.exists());
    assert_eq!(
        fixture.host.log().units.last().map(String::as_str),
        Some("stop agent-sec-gateway.service")
    );
    // The removal stays saved, so a later enable cannot resurrect it.
    let saved = fixture.service().status(0);
    assert_eq!(saved.agents.len(), 1);
    assert!(saved.agents[0].presets.is_empty());
}

#[test]
fn a_rejected_addition_keeps_the_saved_configuration() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    fixture.register(&service, OPS_UID, "ops");
    service.enable().unwrap();
    fixture.reject.store(true, Ordering::SeqCst);
    let error = service.add_key(&fixture.request("ops", REAL)).unwrap_err();
    assert!(error.to_string().contains("saved"), "{error}");
    assert!(fixture.runtime("active.json").exists());
    assert_eq!(fixture.service().status(0).agents.len(), 2);
    fixture.reject.store(false, Ordering::SeqCst);
    let status = service.enable().unwrap();
    assert_eq!(status.loaded_generation, Some(status.generation));
}

#[test]
fn a_failed_enable_leaves_nothing_serving() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    fixture.reject.store(true, Ordering::SeqCst);
    service.enable().unwrap_err();
    assert!(!fixture.config.runtime_dir.exists());
    assert_eq!(fixture.last_ruleset(), nft::render_delete());
    assert_eq!(
        fixture.host.log().units.last().map(String::as_str),
        Some("stop agent-sec-gateway.service")
    );
    assert!(!service.status(0).enabled);
}

#[test]
fn disable_removes_runtime_files_and_routes() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    service.enable().unwrap();
    let status = service.disable().unwrap();
    assert!(!status.enabled);
    assert!(!fixture.config.runtime_dir.exists());
    assert_eq!(fixture.last_ruleset(), nft::render_delete());
    assert_eq!(
        fixture.host.log().units.last().map(String::as_str),
        Some("stop agent-sec-gateway.service")
    );
    assert_eq!(service.list_keys().unwrap().agents.len(), 1);
    assert!(!fixture.service().status(0).enabled);
}

#[test]
fn key_requests_are_validated() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    let kind = |request: KeyRequest| service.add_key(&request).unwrap_err().kind();

    assert_eq!(
        kind(fixture.request("unregistered", REAL)),
        GatewayErrorKind::NotFound
    );
    let mut request = fixture.request("dev", REAL);
    request.preset = "gitlab".into();
    assert_eq!(kind(request), GatewayErrorKind::InvalidArgument);
    let mut request = fixture.request("Dev", REAL);
    request.agent = "Dev".into();
    assert_eq!(kind(request), GatewayErrorKind::InvalidArgument);

    let mut request = fixture.request("dev", REAL);
    request.secret_file = PathBuf::from("dev.token");
    assert_eq!(kind(request), GatewayErrorKind::InvalidArgument);
    let request = fixture.request("dev", REAL);
    fs::set_permissions(&request.secret_file, fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(kind(request), GatewayErrorKind::InvalidArgument);
    let mut request = fixture.request("dev", REAL);
    let link = fixture.root.path().join("link.token");
    symlink(&request.secret_file, &link).unwrap();
    request.secret_file = link;
    assert_eq!(kind(request), GatewayErrorKind::InvalidArgument);
    let mut request = fixture.request("dev", REAL);
    request.caller_uid = me().uid + 1;
    assert_eq!(kind(request), GatewayErrorKind::InvalidArgument);
    let request = fixture.request("dev", "not a token");
    assert_eq!(kind(request), GatewayErrorKind::InvalidArgument);

    service.add_key(&fixture.request("dev", REAL)).unwrap();
    assert_eq!(
        service.register(OPS_UID, 100, "dev").unwrap_err().kind(),
        GatewayErrorKind::Conflict
    );
    assert_eq!(service.status(0).agents.len(), 1);
}

#[test]
fn listener_ports_are_a_fixed_capacity() {
    let fixture = Fixture::new();
    let mut config = fixture.config.clone();
    config.ports = 15001..=15001;
    let service = GatewayService::new(config, Box::new(fixture.host.clone())).unwrap();
    fixture.register(&service, DEV_UID, "dev");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    assert_eq!(
        service.register(OPS_UID, 101, "ops").unwrap_err().kind(),
        GatewayErrorKind::ResourceExhausted
    );
}

#[test]
fn corrupt_state_is_refused() {
    let fixture = Fixture::new();
    let directory = fixture.root.path().join("data/gateway");
    fs::create_dir(&directory).unwrap();
    fs::write(
        directory.join("state.json"),
        r#"{"version":1,"enabled":true,"generation":1,"agents":{"dev":{"uid":0,"port":15001,"presets":["github"]}}}"#,
    )
    .unwrap();
    let error = GatewayService::new(fixture.config.clone(), Box::new(fixture.host.clone()))
        .err()
        .unwrap();
    assert!(error.to_string().contains("root"), "{error}");
}

#[test]
fn registration_snapshots_children_while_disabled_and_refreshes_routes_when_enabled() {
    let fixture = Fixture::new();
    let service = fixture.service();
    {
        let mut log = fixture.host.log();
        log.processes.insert(77, TestProcess::new(DEV_UID, None));
        log.processes
            .insert(78, TestProcess::new(DEV_UID, Some(77)));
        log.processes
            .insert(79, TestProcess::new(DEV_UID, Some(78)));
    }
    let registered = service.register(DEV_UID, 77, "dev").unwrap();
    assert_eq!(registered.uid, DEV_UID);
    assert!(registered.presets.is_empty() && registered.running);
    assert_eq!(
        fixture
            .host
            .log()
            .attached
            .iter()
            .map(|(_, pid)| *pid)
            .collect::<Vec<_>>(),
        vec![77, 78, 79]
    );
    assert!(fixture.host.log().rulesets.is_empty());
    assert!(!fixture.config.runtime_dir.exists());
    assert!(!fixture.config.store_dir.exists());
    service.enable().unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.runtime("active.json")).unwrap()).unwrap();
    assert_eq!(manifest["listeners"], serde_json::json!([]));
    assert!(!fixture.last_ruleset().contains("socket cgroupv2"));
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    assert!(fixture.last_ruleset().contains("agent-sec-agent-dev.scope"));
    fixture
        .host
        .log()
        .processes
        .insert(80, TestProcess::new(DEV_UID, None));
    let before = service.status(0).generation;
    let repeated = service.register(DEV_UID, 80, "dev").unwrap();
    assert_eq!(repeated.port, registered.port);
    assert_eq!(service.status(0).generation, before);
    assert_eq!(fixture.host.log().attached.last().unwrap().1, 80);
    let attached = fixture.host.log().attached.clone();
    service.recover().unwrap();
    assert_eq!(fixture.host.log().attached, attached);
    assert_eq!(fixture.service().status(DEV_UID).agents[0].uid, DEV_UID);
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.config.state_dir.join("state.json")).unwrap())
            .unwrap();
    assert_eq!(
        persisted["agents"]["dev"],
        serde_json::json!({"uid": DEV_UID, "port": registered.port, "presets": ["github"]})
    );
}

#[test]
fn registration_rejects_foreign_uids_groups_and_scopes_before_any_move() {
    for index in 0..4 {
        let fixture = Fixture::new();
        let service = fixture.service();
        let mut child = TestProcess::new(DEV_UID, Some(77));
        child.uids[index] = OPS_UID;
        fixture
            .host
            .log()
            .processes
            .insert(77, TestProcess::new(DEV_UID, None));
        fixture.host.log().processes.insert(78, child);
        assert_eq!(
            service.register(DEV_UID, 77, "dev").unwrap_err().kind(),
            GatewayErrorKind::PermissionDenied
        );
        assert!(fixture.host.log().attached.is_empty());
        assert!(fixture.service().status(0).agents.is_empty());
    }
    let fixture = Fixture::new();
    let service = fixture.service();
    for uid in [0, 999_999, GATEWAY_GROUP_UID] {
        assert_eq!(
            service.register(uid, 77, "dev").unwrap_err().kind(),
            GatewayErrorKind::InvalidArgument
        );
    }
    fixture.register(&service, DEV_UID, "dev");
    fixture.host.log().attached.clear();
    assert_eq!(
        service.register(OPS_UID, 77, "dev").unwrap_err().kind(),
        GatewayErrorKind::Conflict
    );
    for cgroup in [
        "/agentsec.slice/agent-sec-agent-dev.scope",
        "/agentsec.slice/agent-sec-agent-dev.scope/child",
    ] {
        let mut process = TestProcess::new(DEV_UID, None);
        process.cgroup = cgroup.into();
        fixture.host.log().processes.insert(77, process);
        assert_eq!(
            service.register(DEV_UID, 77, "other").unwrap_err().kind(),
            GatewayErrorKind::Conflict
        );
    }
    let mut process = TestProcess::new(DEV_UID, None);
    process.groups.push(me().gid);
    fixture.host.log().processes.insert(77, process);
    assert_eq!(
        service.register(DEV_UID, 77, "other").unwrap_err().kind(),
        GatewayErrorKind::PermissionDenied
    );
    assert!(fixture.host.log().attached.is_empty());
}

#[test]
fn registration_failures_do_not_persist_a_usable_domain() {
    for failure in ["stale", "gone", "reuse", "exit", "attach", "cgroup"] {
        let fixture = Fixture::new();
        let service = fixture.service();
        {
            let mut log = fixture.host.log();
            log.processes.insert(77, TestProcess::new(DEV_UID, None));
            log.processes
                .insert(78, TestProcess::new(DEV_UID, Some(77)));
            match failure {
                "stale" => {
                    log.change_after_snapshot.insert(78);
                }
                "gone" => {
                    log.exit_after_snapshot.insert(78);
                }
                "reuse" => {
                    log.change_after_attach.insert(78);
                }
                "exit" => {
                    log.exit_after_attach.insert(78);
                }
                "attach" => {
                    log.fail_attach.insert(78);
                }
                "cgroup" => {
                    log.wrong_cgroup.insert(78);
                }
                _ => unreachable!(),
            }
        }
        assert!(service.register(DEV_UID, 77, "dev").is_err(), "{failure}");
        assert!(fixture.service().status(0).agents.is_empty(), "{failure}");
        if matches!(failure, "stale" | "gone") {
            assert!(fixture.host.log().attached.is_empty());
        }
        assert_eq!(
            service.credentials(DEV_UID, "dev").err().unwrap().kind(),
            GatewayErrorKind::NotFound
        );
        assert!(
            fixture.host.log().units.is_empty(),
            "target must not be stopped"
        );
    }
    let fixture = Fixture::new();
    let service = fixture.service();
    service.enable().unwrap();
    fixture.reject.store(true, Ordering::SeqCst);
    fixture
        .host
        .log()
        .processes
        .insert(77, TestProcess::new(DEV_UID, None));
    assert!(service.register(DEV_UID, 77, "dev").is_err());
    assert!(fixture.service().status(0).agents.is_empty());
    assert!(!fixture.config.runtime_dir.exists());
}

#[test]
fn oversized_registration_is_bounded_and_moves_nothing() {
    let fixture = Fixture::new();
    let service = fixture.service();
    {
        let mut log = fixture.host.log();
        log.processes.insert(1, TestProcess::new(DEV_UID, None));
        for pid in 2..=4097 {
            log.processes
                .insert(pid, TestProcess::new(DEV_UID, Some(1)));
        }
    }
    assert_eq!(
        service.register(DEV_UID, 1, "dev").unwrap_err().kind(),
        GatewayErrorKind::ResourceExhausted
    );
    assert!(fixture.host.log().attached.is_empty());
    assert!(service.status(0).agents.is_empty());
}

#[test]
fn unregister_revokes_only_its_domain_and_retains_shared_credentials() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    fixture.register(&service, OPS_UID, "ops");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    service.add_key(&fixture.request("ops", REAL)).unwrap();
    let dev_fake = service.credentials(DEV_UID, "dev").unwrap().environment["GH_TOKEN"].clone();
    let ops_fake = service.credentials(OPS_UID, "ops").unwrap().environment["GH_TOKEN"].clone();
    service.enable().unwrap();
    let status = service.unregister("dev").unwrap();
    assert_eq!(
        status
            .agents
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        vec![agent("ops")]
    );
    assert_eq!(
        service.credentials(DEV_UID, "dev").err().unwrap().kind(),
        GatewayErrorKind::NotFound
    );
    assert_eq!(
        service.credentials(OPS_UID, "ops").unwrap().environment["GH_TOKEN"],
        ops_fake
    );
    assert!(!fixture.last_ruleset().contains("agent-sec-agent-dev.scope"));
    let keys = service.list_keys().unwrap();
    assert!(
        keys.credentials[0]
            .fakes
            .iter()
            .any(|fake| fake.agent == agent("dev") && fake.retired)
    );
    let secrets =
        fs::read_to_string(fixture.runtime(&format!("secrets-{}.json", status.generation)))
            .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&secrets).unwrap();
    assert!(
        snapshot["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["fake"] != dev_fake)
    );
    assert!(
        snapshot["retired"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["fake"] == dev_fake)
    );
    assert!(secrets.contains(&ops_fake) && secrets.contains(REAL));
    let status = service.unregister("ops").unwrap();
    let secrets =
        fs::read_to_string(fixture.runtime(&format!("secrets-{}.json", status.generation)))
            .unwrap();
    assert!(!secrets.contains(REAL));
    let snapshot: serde_json::Value = serde_json::from_str(&secrets).unwrap();
    assert_eq!(snapshot["entries"], serde_json::json!([]));
    assert!(
        snapshot["retired"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["fake"] == ops_fake)
    );
    assert!(fixture.service().status(0).agents.is_empty());
    assert_eq!(
        service.unregister("ops").unwrap_err().kind(),
        GatewayErrorKind::NotFound
    );
}

#[test]
fn rejected_unregister_is_saved_and_stops_serving_revoked_values() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    service.add_key(&fixture.request("dev", REAL)).unwrap();
    service.enable().unwrap();
    fixture.reject.store(true, Ordering::SeqCst);
    assert!(
        service
            .unregister("dev")
            .unwrap_err()
            .to_string()
            .contains("gateway stopped")
    );
    assert!(fixture.service().status(0).agents.is_empty());
    assert!(!fixture.config.runtime_dir.exists());
}

#[test]
fn a_retired_live_scope_cannot_be_rebound_to_another_uid() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(&service, DEV_UID, "dev");
    service.unregister("dev").unwrap();
    fixture
        .host
        .log()
        .processes
        .insert(77, TestProcess::new(OPS_UID, None));
    assert_eq!(
        service.register(OPS_UID, 77, "dev").unwrap_err().kind(),
        GatewayErrorKind::PermissionDenied
    );
    assert!(service.status(0).agents.is_empty());
    service.register(DEV_UID, 100, "dev").unwrap();
}

#[test]
#[ignore = "root/systemd/cgroup-v2 integration; creates an isolated test scope"]
fn system_host_registers_existing_children_and_new_children_inherit_the_scope() {
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::process::CommandExt as _;
    use std::process::{Child, Command, Stdio};

    struct Session(Child);
    impl Drop for Session {
        fn drop(&mut self) {
            if let Some(mut stdin) = self.0.stdin.take() {
                let _ = stdin.write_all(b"next\nexit\n");
            }
            let _ = self.0.wait();
        }
    }
    assert_eq!(rustix::process::geteuid().as_raw(), 0, "requires root");
    let uid = 65534;
    assert!(
        nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
            .unwrap()
            .is_some(),
        "requires local uid 65534"
    );
    let root = tempfile::tempdir().unwrap();
    let mut config = GatewayConfig::system(root.path());
    config.slice = format!("ascgatewaytest{}.slice", std::process::id());
    config.gateway_user = "asc-gw-test-absent-account".into();
    let mut session = Session(Command::new("/bin/sh")
        .args(["-c", "sleep 120 & first=$!; printf '%s\\n' \"$first\"; read request; sleep 120 & second=$!; printf '%s\\n' \"$second\"; read request; kill \"$first\" \"$second\"; wait; exit 0"])
        .uid(uid).gid(uid).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap());
    let mut output = std::io::BufReader::new(session.0.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let first: u32 = line.trim().parse().unwrap();
    let host = crate::SystemHost;
    let captured = host.process_tree(session.0.id()).unwrap();
    assert!(captured.iter().any(|process| process.pid == first));
    let service = GatewayService::new(config.clone(), Box::new(crate::SystemHost)).unwrap();
    let status = service.register(uid, session.0.id(), "live-test").unwrap();
    assert!(status.running && status.presets.is_empty());
    let expected = format!("/{}/agent-sec-agent-live-test.scope", config.slice);
    assert_eq!(host.process_cgroup(first).unwrap(), expected);
    for process in &captured {
        host.verify_process(process).unwrap();
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
    let current = host.process_tree(session.0.id()).unwrap();
    assert!(current.iter().all(|process| process.uids == [uid; 4]));
    drop(session);
    assert_eq!(
        host.verify_process(&captured[0]).unwrap_err().kind(),
        GatewayErrorKind::Unavailable
    );
    assert!(service.unregister("live-test").unwrap().agents.is_empty());
}

#[test]
fn ruleset_redirects_each_scope_and_returns_loopback() {
    let (dev, ops) = (agent("dev"), agent("ops"));
    let rules = nft::render(&[
        Redirect {
            agent: &dev,
            cgroup: "agentsec.slice/agent-sec-agent-dev.scope",
            port: 15001,
        },
        Redirect {
            agent: &ops,
            cgroup: "agentsec.slice/agent-sec-agent-ops.scope",
            port: 15002,
        },
    ]);
    assert_eq!(
        rules,
        "table inet agent_sec_gateway\n\
         delete table inet agent_sec_gateway\n\
         table inet agent_sec_gateway {\n\
         \x20 chain redirect_out {\n\
         \x20   type nat hook output priority -100; policy accept;\n\
         \x20   ip daddr 127.0.0.0/8 return\n\
         \x20   ip6 daddr ::1 return\n\
         \x20   socket cgroupv2 level 2 \"agentsec.slice/agent-sec-agent-dev.scope\" tcp dport { 80, 443 } counter redirect to :15001 comment \"asc:agent:dev\"\n\
         \x20   socket cgroupv2 level 2 \"agentsec.slice/agent-sec-agent-ops.scope\" tcp dport { 80, 443 } counter redirect to :15002 comment \"asc:agent:ops\"\n\
         \x20 }\n\
         \x20 chain filter_out {\n\
         \x20   type filter hook output priority 0; policy accept;\n\
         \x20   socket cgroupv2 level 2 \"agentsec.slice/agent-sec-agent-dev.scope\" udp dport 443 counter drop comment \"asc:agent:dev\"\n\
         \x20   socket cgroupv2 level 2 \"agentsec.slice/agent-sec-agent-ops.scope\" udp dport 443 counter drop comment \"asc:agent:ops\"\n\
         \x20 }\n\
         }\n"
    );
    for unsafe_path in ["", "a//b", "a/../b", "a\"b", "a b", "/abs"] {
        assert!(!nft::is_safe_cgroup_path(unsafe_path), "{unsafe_path}");
    }
    assert!(nft::is_safe_cgroup_path(
        "agentsec.slice/agent-sec-agent-dev.scope"
    ));
}
