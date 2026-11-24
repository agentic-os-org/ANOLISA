//! Secretless command-boundary fixtures; privileged cases run in isolated test children.

use super::tests::parse;
use super::*;
use asc_daemon_protocol::{DaemonRequest, RequestId};
use std::io::{BufRead as _, BufReader};
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::thread::{self, JoinHandle};

const TIMEOUT: Duration = Duration::from_secs(3);
const REAL: &str = "aaaaaaaaaaaaaaaaaaaa";
const FAKE: &str = "bbbbbbbbbbbbbbbbbbbb";
const GITHUB: &str = include_str!("../../../../crates/asc-gateway-plan/policies/github.cedar");

fn root_child() -> bool {
    if !cfg!(target_os = "linux") {
        eprintln!("skipped: credential injection requires Linux");
        return false;
    }
    if rustix::process::geteuid().is_root() {
        return true;
    }
    if !Command::new("sudo")
        .args(["-n", "true"])
        .status()
        .is_ok_and(|status| status.success())
    {
        eprintln!("skipped: passwordless sudo is unavailable");
        return false;
    }
    let mut child = Command::new("sudo");
    child.args(["-n", "env"]);
    // sudo normally drops the coverage runtime's output destination.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        let mut assignment = std::ffi::OsString::from("LLVM_PROFILE_FILE=");
        assignment.push(profile);
        child.arg(assignment);
    }
    let test = thread::current().name().unwrap().to_owned();
    let output = child
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", &test, "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "root child failed: {test}\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
    false
}

fn boundary_available() -> bool {
    if cfg!(target_os = "linux")
        && (rustix::process::geteuid().is_root()
            || std::env::var("AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK").as_deref() == Ok("1"))
    {
        true
    } else {
        root_child()
    }
}

fn success(result: Value) -> DaemonResponse<Value> {
    DaemonResponse::success(RequestId::new("test-response").unwrap(), result)
}

fn rejected() -> DaemonResponse<Value> {
    DaemonResponse::error(
        RequestId::new("test-rejection").unwrap(),
        "internal",
        "test admission rejected",
    )
}

struct Daemon {
    _directory: tempfile::TempDir,
    socket: PathBuf,
    server: JoinHandle<Vec<DaemonRequest>>,
}

impl Daemon {
    fn new(steps: Vec<(&'static str, Option<DaemonResponse<Value>>)>) -> Self {
        Self::with_inspector(steps, |_| {})
    }

    fn with_inspector(
        steps: Vec<(&'static str, Option<DaemonResponse<Value>>)>,
        inspect: impl Fn(&DaemonRequest) + Send + 'static,
    ) -> Self {
        // Whitespace in the endpoint exercises quoting of every emitted follow-up command.
        let directory = tempfile::Builder::new()
            .prefix("asc daemon ")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = directory.path().join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for (method, response) in steps {
                let deadline = Instant::now() + TIMEOUT;
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "no request for {method}");
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(TIMEOUT)).unwrap();
                let mut frame = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut frame)
                    .unwrap();
                let request: DaemonRequest = serde_json::from_str(&frame).unwrap();
                assert_eq!(request.method, method);
                inspect(&request);
                requests.push(request);
                if let Some(response) = response {
                    writeln!(stream, "{}", serde_json::to_string(&response).unwrap()).unwrap();
                }
            }
            requests
        });
        Self {
            _directory: directory,
            socket,
            server,
        }
    }

    fn finish(self) -> Vec<DaemonRequest> {
        self.server.join().unwrap()
    }
}

fn command() -> InjectCredentialCommand {
    parse(&["--owner-uid", "1000", "--process-name", "agent"]).unwrap()
}

fn root_directory() -> tempfile::TempDir {
    let root = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(0))
        .unwrap()
        .unwrap();
    tempfile::tempdir_in(root.dir).unwrap()
}

fn source() -> CedarPolicySource {
    CedarPolicySource {
        schema: CedarSchemaReference {
            schema_id: ResourceId::new(asc_policy_cedar::schema::SCHEMA_ID).unwrap(),
            revision: Revision::new(asc_policy_cedar::schema::HTTP_SCHEMA_REVISION).unwrap(),
        },
        text: GITHUB.into(),
    }
}

fn plan(directory: &Path) -> Plan {
    Plan {
        owner: ResolvedOwner {
            uid: 1000,
            gid: 1000,
            home: Some(directory.into()),
        },
        selector: command().selector.value(),
        id: "test-run".into(),
        policy_name: "inject-credential/github/1000/test-run".into(),
        staging: directory.join("staged"),
        delivered: directory.join("delivered"),
        rendered: render(
            &source(),
            &directory.join("staged/real"),
            &directory.join("staged/fake"),
        )
        .unwrap(),
    }
}

fn policy() -> Value {
    json!({"policyId": "test-policy", "revision": 2,
    "credentialBindings": [
        {"pairId": "pair-z", "realToken": REAL, "fakeToken": FAKE},
        {"pairId": "pair-a"}, {"pairId": "pair-z"}, {"pairId": null}
    ]})
}

#[test]
fn list_pagination_advances_by_received_items_and_stops_on_empty_page() {
    if !boundary_available() {
        return;
    }
    for pages in [
        vec![
            json!({"items": [{"n": 1}, {"n": 2}], "total": 3}),
            json!({"items": [{"n": 3}], "total": 3}),
        ],
        vec![
            json!({"items": [{"n": 1}, {"n": 2}], "total": 20}),
            json!({"items": [], "total": 20}),
        ],
        vec![json!({"total": 20})],
    ] {
        let expected: Vec<Value> = pages
            .iter()
            .flat_map(|page| page["items"].as_array().cloned().unwrap_or_default())
            .collect();
        let daemon = Daemon::new(
            pages
                .into_iter()
                .map(|page| (POLICY_SCOPES_LIST, Some(success(page))))
                .collect(),
        );
        assert_eq!(
            list_all(&daemon.socket, TIMEOUT, POLICY_SCOPES_LIST).unwrap(),
            expected
        );
        for (index, request) in daemon.finish().iter().enumerate() {
            assert_eq!(
                request.params,
                json!({"limit": LIST_PAGE, "offset": index * 2})
            );
        }
    }
    let daemon = Daemon::new(vec![(POLICY_SCOPES_LIST, Some(rejected()))]);
    let error = list_all(&daemon.socket, TIMEOUT, POLICY_SCOPES_LIST).unwrap_err();
    assert!(error.to_string().contains("test admission rejected"));
    assert_eq!(daemon.finish().len(), 1);
}

#[test]
fn duplicates_require_active_matching_owner_selector_and_builtin() {
    if !boundary_available() {
        return;
    }
    let command = command();
    let selector = command.selector.value();
    let matching = json!({"scopeId": "existing-scope", "status": "ACTIVE", "ownerUid": 1000,
    "selector": selector, "policySnapshots": [
        {"policyName": "ordinary-policy"},
        {"policyName": "inject-credential/github/1000/old"}
    ]});
    let mut unrelated = Vec::new();
    for (key, value) in [
        ("status", json!("DELETED")),
        ("ownerUid", json!(1001)),
        (
            "selector",
            serde_json::to_value(ScopeSelector::Process {
                matcher: ProcessMatcher::Name {
                    process_name: "other".into(),
                },
            })
            .unwrap(),
        ),
        (
            "policySnapshots",
            json!([{"policyName": "inject-credential/gitlab/1000/old"}]),
        ),
        (
            "policySnapshots",
            json!([{"policyName": "inject-credential/githubish/1000/old"}]),
        ),
        ("policySnapshots", Value::Null),
    ] {
        let mut scope = matching.clone();
        scope[key] = value;
        unrelated.push(scope);
    }
    let daemon = Daemon::new(vec![(
        POLICY_SCOPES_LIST,
        Some(success(json!({"items": unrelated, "total": 6}))),
    )]);
    command
        .reject_duplicate(&daemon.socket, TIMEOUT, 1000, &selector)
        .unwrap();
    daemon.finish();
    let daemon = Daemon::new(vec![(
        POLICY_SCOPES_LIST,
        Some(success(json!({"items": [matching], "total": 1}))),
    )]);
    let error = command
        .reject_duplicate(&daemon.socket, TIMEOUT, 1000, &selector)
        .unwrap_err();
    assert!(error.to_string().contains("existing-scope"));
    assert!(error.to_string().contains("delete it first"));
    daemon.finish();
}

fn binding(id: &str, scope: &str, phase: &str) -> Value {
    json!({"bindingId": id, "spec": {"scope": {"scopeId": scope, "process": {"pid": 123}}},
        "status": {"phase": phase}, "secret": REAL})
}

#[test]
fn binding_wait_filters_scope_polls_and_reports_ready_failure_or_timeout() {
    if !boundary_available() {
        return;
    }
    let scope = ResourceId::new("new-scope").unwrap();
    for (phase, expected_error) in [
        ("READY", None),
        ("APPLY_FAILED", Some("failed")),
        ("PENDING", Some("timed out")),
    ] {
        let daemon = Daemon::new(vec![(
            POLICY_BINDINGS_LIST,
            Some(success(json!({"items": [
            binding("ours", "new-scope", phase), binding("other", "old-scope", "APPLY_FAILED")
        ], "total": 2}))),
        )]);
        let (actual, result) = wait_bindings(&daemon.socket, TIMEOUT, &scope, Duration::ZERO);
        assert_eq!(
            actual,
            json!([{"bindingId": "ours", "pid": 123, "phase": phase}])
        );
        match expected_error {
            Some(message) => assert!(result.unwrap_err().to_string().contains(message)),
            None => result.unwrap(),
        }
        daemon.finish();
    }
    let daemon = Daemon::new(vec![(
        POLICY_BINDINGS_LIST,
        Some(success(json!({"items": [], "total": 0}))),
    )]);
    let (actual, result) = wait_bindings(&daemon.socket, TIMEOUT, &scope, Duration::ZERO);
    assert_eq!(actual, json!([]));
    assert!(result.unwrap_err().to_string().contains("timed out"));
    daemon.finish();

    let daemon = Daemon::new(vec![
        (
            POLICY_BINDINGS_LIST,
            Some(success(
                json!({"items": [binding("one", "new-scope", "READY"), binding("two", "new-scope", "PENDING")], "total": 2}),
            )),
        ),
        (
            POLICY_BINDINGS_LIST,
            Some(success(
                json!({"items": [binding("one", "new-scope", "READY"), binding("two", "new-scope", "READY")], "total": 2}),
            )),
        ),
    ]);
    let (actual, result) = wait_bindings(&daemon.socket, TIMEOUT, &scope, TIMEOUT);
    result.unwrap();
    assert_eq!(
        actual,
        json!([
            {"bindingId": "one", "pid": 123, "phase": "READY"},
            {"bindingId": "two", "pid": 123, "phase": "READY"}
        ])
    );
    assert_eq!(daemon.finish().len(), 2);

    for response in [Some(rejected()), None] {
        let daemon = Daemon::new(vec![(POLICY_BINDINGS_LIST, response)]);
        let (actual, result) = wait_bindings(&daemon.socket, TIMEOUT, &scope, TIMEOUT);
        assert_eq!(actual, Value::Null);
        assert!(result.is_err());
        assert_eq!(daemon.finish().len(), 1);
    }
}

#[test]
fn reports_gateway_guidance_and_unique_pair_ids_without_token_values() {
    if !boundary_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let plan = plan(directory.path());
    let created = Created {
        policy: policy(),
        policy_id: ResourceId::new("test-policy").unwrap(),
        revision: Revision::new(2).unwrap(),
        scope_id: ResourceId::new("test-scope").unwrap(),
        teardown: ["delete scope".into(), "delete policy".into()],
    };
    for (enabled, ready, ca) in [
        (false, false, None),
        (true, false, None),
        (
            true,
            true,
            Some(json!({"pem": "/run/test-ca.pem", "pkcs12": "/run/test-ca.p12"})),
        ),
        (true, true, None),
    ] {
        let mut steps = vec![(
            GATEWAY_STATUS,
            Some(success(json!({"enabled": enabled, "ready": ready}))),
        )];
        if ready {
            steps.push((
                GATEWAY_CA,
                Some(
                    ca.clone()
                        .map_or_else(rejected, |paths| success(json!({"caBundlePaths": paths}))),
                ),
            ));
        }
        let daemon = Daemon::new(steps);
        let report = command().report(&daemon.socket, TIMEOUT, &plan, &created, &json!([]));
        assert_eq!(
            report["gateway"],
            json!({"enabled": enabled, "ready": ready, "caBundlePaths": ca})
        );
        assert_eq!(report["pairIds"], json!(["pair-a", "pair-z"]));
        assert_eq!(report["teardown"], json!(["delete scope", "delete policy"]));
        let text = report.to_string();
        assert!(!text.contains(REAL) && !text.contains(FAKE));
        assert_eq!(text.contains("gateway enable"), !enabled && ca.is_none());
        if !enabled {
            assert!(text.contains(&format!("`{} gateway enable`", cli(&daemon.socket))));
        }
        assert_eq!(text.contains("SSL_CERT_FILE"), ca.is_some());
        assert!(text.contains("GH_TOKEN/GITHUB_TOKEN"));
        assert_eq!(daemon.finish().len(), if ready { 2 } else { 1 });
    }
    let daemon = Daemon::new(vec![(GATEWAY_STATUS, Some(rejected()))]);
    let report = command().report(&daemon.socket, TIMEOUT, &plan, &created, &Value::Null);
    assert_eq!(
        report["gateway"],
        json!({"enabled": false, "ready": false, "caBundlePaths": null})
    );
    daemon.finish();
}

struct InstalledFixture {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl InstalledFixture {
    fn directory(&mut self, path: &Path) -> io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
        let mut current = PathBuf::from("/");
        for component in path.components() {
            let std::path::Component::Normal(name) = component else {
                continue;
            };
            current.push(name);
            match std::fs::DirBuilder::new().mode(0o700).create(&current) {
                Ok(()) => self.directories.push(current.clone()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let metadata = std::fs::symlink_metadata(&current)?;
            if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o7022 != 0 {
                return Err(failure(format!(
                    "untrusted test fixture ancestor {}",
                    current.display()
                )));
            }
        }
        Ok(())
    }
}

impl Drop for InstalledFixture {
    fn drop(&mut self) {
        for file in self.files.iter().rev() {
            std::fs::remove_file(file).unwrap();
        }
        for directory in self.directories.iter().rev() {
            // Never recursively remove shared paths or another test's artifacts.
            std::fs::remove_dir(directory).unwrap();
        }
    }
}

struct PackagedFixture {
    _installed: InstalledFixture,
    directory: tempfile::TempDir,
}

impl PackagedFixture {
    fn new() -> Option<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut installed = InstalledFixture {
            files: Vec::new(),
            directories: Vec::new(),
        };
        for directory in [GATEWAY_DATADIR, STAGING_ROOT] {
            if let Err(error) = installed.directory(Path::new(directory)) {
                eprintln!("skipped packaged-template flow: {error}");
                return None;
            }
        }
        let template_path = command().template_path();
        match File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&template_path)
        {
            Ok(mut file) => {
                installed.files.push(template_path);
                file.write_all(GITHUB.as_bytes()).unwrap();
                file.sync_all().unwrap();
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("template fixture: {error}"),
        }
        Some(Self {
            _installed: installed,
            directory: root_directory(),
        })
    }
}

fn assert_packaged_dry_run(
    command: &mut InjectCredentialCommand,
    directory: &Path,
    template: &CedarPolicySource,
) {
    assert!(rule_count(template) > 0);
    let owner = nix::unistd::User::from_name("nobody").unwrap().unwrap();
    command.owner.owner_uid = Some(owner.uid.as_raw());
    command.real_token_file = directory.join("absent-real");
    command.fake_token_file = Some(directory.join("absent-fake"));
    command.dry_run = true;
    let socket = directory.join("unused.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        command
            .run(&socket, TIMEOUT, &mut stdout, &mut stderr)
            .unwrap(),
        0
    );
    let preview: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["ownerUid"], owner.uid.as_raw());
    assert_eq!(preview["rules"], rule_count(template));
    assert_eq!(
        preview["fakeTokenFile"],
        serde_json::to_value(command.fake_token_file.as_ref().unwrap()).unwrap()
    );
    assert!(stderr.is_empty());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!command.real_token_file.exists());
    assert!(!command.fake_token_file.as_ref().unwrap().exists());
}

/// A concurrent run holding the staging lock is rejected at once, before any daemon call.
fn assert_contended_run_is_rejected_before_daemon_calls(
    command: &InjectCredentialCommand,
    directory: &Path,
) {
    let _held = lock_root_directory(Path::new(STAGING_ROOT)).unwrap();
    let socket = directory.join("contended.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let error = command
        .run(&socket, TIMEOUT, &mut Vec::new(), &mut Vec::new())
        .unwrap_err()
        .to_string();
    assert!(error.contains("in use by another process"), "{error}");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn existing_fake_run_and_dry_run_preserve_packaged_asset_and_never_enable_gateway() {
    if !root_child() {
        return;
    }
    let Some(fixture) = PackagedFixture::new() else {
        return;
    };
    let directory = &fixture.directory;
    let template_path = command().template_path();
    let original = read_root_asset(&template_path, TEMPLATE_LIMIT).unwrap();
    let template = command().template().unwrap();
    let mut command = command();
    assert_packaged_dry_run(&mut command, directory.path(), &template);
    command.real_token_file = write_owner_file(directory.path(), "real", REAL, 0, 0).unwrap();
    command.fake_token_file = Some(write_owner_file(directory.path(), "fake", FAKE, 0, 0).unwrap());
    command.dry_run = false;
    assert_contended_run_is_rejected_before_daemon_calls(&command, directory.path());
    for phase in [None, Some("READY"), Some("APPLY_FAILED")] {
        command.wait_seconds = u32::from(phase.is_some());
        let mut steps = vec![
            (
                POLICY_SCOPES_LIST,
                Some(success(json!({"items": [], "total": 0}))),
            ),
            (POLICY_TEMPLATES_CREATE, Some(success(policy()))),
            (
                POLICY_SCOPES_CREATE,
                Some(success(json!({"scopeId": "test-scope"}))),
            ),
        ];
        if let Some(phase) = phase {
            steps.push((
                POLICY_BINDINGS_LIST,
                Some(success(
                    json!({"items": [binding("test-binding", "test-scope", phase)], "total": 1}),
                )),
            ));
        }
        steps.push((
            GATEWAY_STATUS,
            Some(success(json!({"enabled": false, "ready": false}))),
        ));
        let daemon = Daemon::with_inspector(steps, |request| {
            if request.method == POLICY_TEMPLATES_CREATE {
                let params: CreatePolicyParams =
                    serde_json::from_value(request.params.clone()).unwrap();
                let PolicySource::Cedar(source) = params.template else {
                    panic!("expected Cedar");
                };
                for rule in asc_policy_cedar::http::compile(&source).unwrap() {
                    assert_eq!(
                        read_token_file(Path::new(&rule.real_token_file), 0)
                            .unwrap()
                            .expose(),
                        REAL
                    );
                    assert_eq!(
                        read_token_file(Path::new(&rule.fake_token_file), 0)
                            .unwrap()
                            .expose(),
                        FAKE
                    );
                }
            }
        });
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = command
            .run(&daemon.socket, TIMEOUT, &mut stdout, &mut stderr)
            .unwrap();
        assert_eq!(code, u8::from(phase == Some("APPLY_FAILED")));
        assert_eq!(stderr.is_empty(), phase != Some("APPLY_FAILED"));
        let report: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(report["pairIds"], json!(["pair-a", "pair-z"]));
        assert_eq!(report["gateway"]["enabled"], false);
        assert_eq!(report["bindings"].is_null(), phase.is_none());
        let output = String::from_utf8(stdout.clone()).unwrap();
        assert!(!output.contains(REAL) && !output.contains(FAKE));
        assert_eq!(
            read_token_file(&command.real_token_file, 0)
                .unwrap()
                .expose(),
            REAL
        );
        assert_eq!(
            read_token_file(command.fake_token_file.as_ref().unwrap(), 0)
                .unwrap()
                .expose(),
            FAKE
        );
        let requests = daemon.finish();
        let name = requests[1].params["policyName"].as_str().unwrap();
        let id = name.rsplit('/').next().unwrap();
        assert!(!Path::new(STAGING_ROOT).join(id).exists());
        assert_eq!(
            read_root_asset(&template_path, TEMPLATE_LIMIT).unwrap(),
            original
        );
    }
}

#[test]
fn owner_resolution_and_output_directory_follow_passwd_not_caller_identity() {
    let user = nix::unistd::User::from_name("nobody").unwrap().unwrap();
    assert!(!user.uid.is_root());
    let uid = user.uid.as_raw().to_string();
    let unknown_uid = (4_000_000_000..4_000_000_100)
        .find(|uid| {
            nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(*uid))
                .unwrap()
                .is_none()
        })
        .unwrap()
        .to_string();
    for owner_args in [
        ["--owner", user.name.as_str()],
        ["--owner-uid", uid.as_str()],
    ] {
        let command = parse(&[owner_args[0], owner_args[1], "--process-name", "agent"]).unwrap();
        let owner = command.resolve_owner().unwrap();
        assert_eq!(
            (owner.uid, owner.gid, owner.home.clone()),
            (user.uid.as_raw(), user.gid.as_raw(), Some(user.dir.clone()))
        );
        assert_eq!(
            command.output_dir(&owner).unwrap(),
            user.dir.join(".config/agent-sec/gateway")
        );
    }
    for (args, message) in [
        (["--owner", "root"], "must not be root"),
        (["--owner", "asc-test-nonexistent-owner"], "unknown owner"),
        (
            ["--owner-uid", unknown_uid.as_str()],
            "must have a passwd entry",
        ),
    ] {
        let command = parse(&[args[0], args[1], "--process-name", "agent"]).unwrap();
        assert!(
            command
                .resolve_owner()
                .err()
                .unwrap()
                .to_string()
                .contains(message)
        );
    }
    let owner = ResolvedOwner {
        uid: 65534,
        gid: 65534,
        home: None,
    };
    let mut command = command();
    assert!(command.output_dir(&owner).is_err());
    command.output_dir = Some(PathBuf::from("output"));
    assert_eq!(
        command.output_dir(&owner).unwrap(),
        std::path::absolute("output").unwrap()
    );
}

#[test]
fn rendering_preserves_rules_and_rejects_non_utf8_paths() {
    use std::os::unix::ffi::OsStringExt as _;
    let template = source();
    let real = Path::new("/run/test/real");
    let fake = Path::new("/run/test/fake");
    let rendered = render(&template, real, fake).unwrap();
    let before = asc_policy_cedar::http::compile(&template).unwrap();
    let after = asc_policy_cedar::http::compile(&rendered).unwrap();
    assert_eq!(rule_count(&rendered), before.len());
    for (mut expected, actual) in before.into_iter().zip(after) {
        expected.real_token_file = real.to_str().unwrap().into();
        expected.fake_token_file = fake.to_str().unwrap().into();
        assert_eq!(actual, expected);
    }
    assert_eq!(template.text, GITHUB);
    let non_utf8 = PathBuf::from(std::ffi::OsString::from_vec(b"/run/invalid-\xff".to_vec()));
    for (real, fake) in [(&non_utf8 as &Path, fake), (real, &non_utf8 as &Path)] {
        assert!(
            render(&template, real, fake)
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
    }
    assert_eq!(
        rule_count(&CedarPolicySource {
            text: "invalid cedar".into(),
            ..template
        }),
        0
    );
}

#[test]
fn policy_then_scope_creation_reads_private_staging_and_removes_it() {
    if !root_child() {
        return;
    }
    let directory = root_directory();
    let plan = plan(directory.path());
    let expected_source = serde_json::to_value(PolicySource::Cedar(plan.rendered.clone())).unwrap();
    let staging = plan.staging.clone();
    let daemon = Daemon::with_inspector(
        vec![
            (POLICY_TEMPLATES_CREATE, Some(success(policy()))),
            (
                POLICY_SCOPES_CREATE,
                Some(success(json!({"scopeId": "test-scope"}))),
            ),
        ],
        move |request| {
            if request.method == POLICY_TEMPLATES_CREATE {
                assert_eq!(request.params["template"], expected_source);
                assert_eq!(
                    read_token_file(&staging.join("real"), 0).unwrap().expose(),
                    REAL
                );
                assert_eq!(
                    read_token_file(&staging.join("fake"), 0).unwrap().expose(),
                    FAKE
                );
            } else {
                assert!(!staging.exists(), "copies survive policy admission");
            }
        },
    );
    let created = create(
        &daemon.socket,
        TIMEOUT,
        &plan,
        &Secret::new(REAL.into()).unwrap(),
        FAKE,
    )
    .unwrap();
    assert_eq!(created.policy, policy());
    assert_eq!(created.scope_id.as_str(), "test-scope");
    let cli = cli(&daemon.socket);
    assert!(cli.starts_with("agent-sec-cli --socket '"));
    assert_eq!(
        created.teardown,
        [
            format!("{cli} scope delete --scope-id test-scope"),
            format!("{cli} policy delete --policy-id test-policy --revision 2"),
        ]
    );
    assert!(!plan.staging.exists());
    assert!(!plan.delivered.exists());
    let requests = daemon.finish();
    let scope: CreateScopeParams = serde_json::from_value(requests[1].params.clone()).unwrap();
    assert_eq!(scope.owner_uid, Some(1000));
    assert_eq!(scope.selector, plan.selector);
    assert_eq!(
        scope.policy_templates,
        vec![PolicyReference {
            policy_id: created.policy_id,
            policy_revision: created.revision
        }]
    );
}

#[test]
fn failed_policy_admission_preserves_exact_registration_recovery() {
    if !root_child() {
        return;
    }
    for response in [Some(rejected()), None] {
        let directory = root_directory();
        let plan = plan(directory.path());
        let daemon = Daemon::new(vec![(POLICY_TEMPLATES_CREATE, response)]);
        let error = create(
            &daemon.socket,
            TIMEOUT,
            &plan,
            &Secret::new(REAL.into()).unwrap(),
            FAKE,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("root-only recovery files retained"));
        let cli = cli(&daemon.socket);
        assert!(error.contains(&format!("`{cli} policy list`")));
        assert!(error.contains(&format!("`{cli} policy create --name")));
        assert!(!error.contains(REAL) && !error.contains(FAKE));
        assert_eq!(
            read_token_file(&plan.staging.join("real"), 0)
                .unwrap()
                .expose(),
            REAL
        );
        assert_eq!(
            read_token_file(&plan.staging.join("fake"), 0)
                .unwrap()
                .expose(),
            FAKE
        );
        let recovery =
            read_root_asset(&plan.staging.join("policy-source.json"), TEMPLATE_LIMIT).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&recovery).unwrap(),
            serde_json::to_value(PolicySource::Cedar(plan.rendered)).unwrap()
        );
        assert!(!plan.delivered.exists());
        assert_eq!(daemon.finish().len(), 1);
    }
}

#[test]
fn malformed_created_identifiers_do_not_admit_a_scope_or_leave_secret_copies() {
    if !root_child() {
        return;
    }
    for (policy, scope, missing) in [
        (json!({"revision": 2}), None, "policyId"),
        (
            json!({"policyId": "test-policy", "revision": 0}),
            None,
            "revision",
        ),
        (policy(), Some(json!({})), "scopeId"),
    ] {
        let directory = root_directory();
        let plan = plan(directory.path());
        let mut steps = vec![(POLICY_TEMPLATES_CREATE, Some(success(policy)))];
        if let Some(scope) = scope {
            steps.push((POLICY_SCOPES_CREATE, Some(success(scope))));
        }
        let expected_requests = steps.len();
        let daemon = Daemon::new(steps);
        let error = create(
            &daemon.socket,
            TIMEOUT,
            &plan,
            &Secret::new(REAL.into()).unwrap(),
            FAKE,
        )
        .err()
        .unwrap()
        .to_string();
        assert_eq!(error, format!("daemon result has no valid {missing}"));
        assert!(!plan.staging.exists());
        assert!(!plan.delivered.exists());
        assert_eq!(daemon.finish().len(), expected_requests);
    }
}

#[test]
fn private_input_rejects_wrong_owner_permissions_and_links() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    if !root_child() {
        return;
    }
    let directory = root_directory();
    let real = write_owner_file(directory.path(), "real", REAL, 0, 0).unwrap();
    assert_eq!(read_private(&real, 1000).unwrap().expose(), REAL);
    let link = directory.path().join("link");
    symlink(&real, &link).unwrap();
    assert!(read_private(&link, 1000).is_err());
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_private(&real, 1000).is_err());
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    nix::unistd::chown(&real, Some(nix::unistd::Uid::from_raw(1001)), None).unwrap();
    let error = read_private(&real, 1000).unwrap_err().to_string();
    assert!(error.contains("must be owned by root or the Agent owner"));
    assert!(!error.contains(REAL));
    assert!(read_private(&directory.path().join("missing"), 1000).is_err());
}

#[test]
fn scope_rejection_or_unknown_outcome_warns_about_committed_admission_without_retry() {
    if !root_child() {
        return;
    }
    for response in [Some(rejected()), None] {
        let directory = root_directory();
        let plan = plan(directory.path());
        let daemon = Daemon::new(vec![
            (POLICY_TEMPLATES_CREATE, Some(success(policy()))),
            (POLICY_SCOPES_CREATE, response),
        ]);
        let error = create(
            &daemon.socket,
            TIMEOUT,
            &plan,
            &Secret::new(REAL.into()).unwrap(),
            FAKE,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("Scope admission may have committed"));
        let cli = cli(&daemon.socket);
        assert!(error.contains(&format!("`{cli} policy delete --policy-id test-policy")));
        assert!(error.contains(&format!("`{cli} gateway key remove PAIR_ID`")));
        assert!(error.contains("scope") || error.contains("Scopes"));
        assert!(
            error.contains("test-policy") && error.contains("pair-a") && error.contains("pair-z")
        );
        assert!(!error.contains(REAL) && !error.contains(FAKE));
        assert!(!plan.staging.exists());
        assert!(!plan.delivered.exists());
        assert_eq!(daemon.finish().len(), 2);
    }
}

#[test]
fn follow_up_commands_name_the_socket_only_when_it_is_not_the_default() {
    assert_eq!(cli(Path::new(crate::DEFAULT_SOCKET)), "agent-sec-cli");
    assert_eq!(
        cli(Path::new("/tmp/a b/it's.sock")),
        "agent-sec-cli --socket '/tmp/a b/it'\\''s.sock'"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn generated_fake_delivery_drops_to_the_owner_and_never_replaces() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    const TARGET: &str = "ASC_INJECT_DELIVERY_TARGET";
    if !root_child() {
        return;
    }
    let owner = ResolvedOwner {
        uid: 1000,
        gid: 1000,
        home: None,
    };
    if let Some(base) = std::env::var_os(TARGET) {
        // The privilege drop is permanent, so each delivery runs in its own test child.
        let result = deliver_fake(
            &Path::new(&base).join("gateway"),
            "fake",
            FAKE,
            &owner,
            &["teardown-scope".into(), "teardown-policy".into()],
        );
        assert_eq!(rustix::process::geteuid().as_raw(), owner.uid);
        assert_eq!(rustix::process::getegid().as_raw(), owner.gid);
        assert!(nix::unistd::getgroups().unwrap().is_empty());
        match result {
            Ok(()) => println!("DELIVERY ok"),
            Err(error) => println!("DELIVERY failed: {error}"),
        }
        return;
    }
    // A root-owned ancestor chain the owner can traverse; the owner-owned base is trusted.
    let base = tempfile::tempdir_in("/").unwrap();
    nix::unistd::chown(
        base.path(),
        Some(nix::unistd::Uid::from_raw(owner.uid)),
        Some(nix::unistd::Gid::from_raw(owner.gid)),
    )
    .unwrap();
    let deliver = || {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", thread::current().name().unwrap(), "--nocapture"])
            .env(TARGET, base.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
    };
    assert!(deliver().contains("DELIVERY ok"));
    let directory = base.path().join("gateway");
    let path = directory.join("fake");
    for (path, mode) in [(&directory, 0o700), (&path, 0o600)] {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (owner.uid, owner.gid));
        assert_eq!(metadata.permissions().mode() & 0o7777, mode);
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FAKE);
    let replaced = deliver();
    assert!(replaced.contains(
        "DELIVERY failed: Policy and Scope were created but the fake file was not delivered"
    ));
    assert!(replaced.contains("tear down with: teardown-scope && teardown-policy"));
    assert!(!replaced.contains(FAKE));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FAKE);
}
