//! Initial proposal acceptance tests using the real CLI, UDS, frozen models and kernel peer credentials.

mod common;

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use asc_daemon::{BootstrapConfig, serve};
use asc_daemon_core::{RootManagedPrincipalPolicy, intent::IntentService};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_daemon_service::{
    DispatchError, DispatchRequest, RequestDispatcher, ResponseDisposition, ShutdownToken,
};
use asc_model_client::provider::{ModelProvider, ProviderError};
use asc_pap::PapService;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use serde_json::{Value, json};

#[derive(Default)]
struct Replay {
    calls: AtomicUsize,
}
impl ModelProvider for Replay {
    fn complete(
        &self,
        _: &str,
        system: &str,
        input: &str,
        _: Instant,
    ) -> Result<String, ProviderError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert!(!system.contains("PRIVATE_FILE_BYTES"));
        assert!(!input.contains("PRIVATE_FILE_BYTES"));
        let input: Value = serde_json::from_str(input).unwrap();
        assert!(system.contains(asc_intent::CEDAR_SCHEMA));
        // Only the task text reaches the model; path bindings and protection layers are gone.
        assert_eq!(input.as_object().unwrap().len(), 1);
        assert!(input.get("paths").is_none());
        assert_eq!(input["task"].as_object().unwrap().len(), 2);
        let text = input["task"]["text"].as_str().unwrap();
        let (path, tree) = match text {
            "Read report.csv" | "Read report.csv\u{1b}[2J\u{202e}" => {
                ("/work/report.csv".to_owned(), false)
            }
            "Read current directory"
            | "Read file contents of the current directory and subdirectories" => {
                ("/work/docs".to_owned(), true)
            }
            "Read /outside/missing.csv" => ("/outside/missing.csv".into(), false),
            // A relative path is valid Cedar but not a checkable policy path, so the candidate is
            // rejected before rendering and must surface as an unsupported task rather than a
            // command failure.
            "Read work/report.csv" => {
                let policy = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"work/report.csv\" };";
                return Ok(json!({"policy":policy, "unresolved":[]}).to_string());
            }
            "Run tests" | "Only list names without reading content" => {
                let reason = if text == "Run tests" {
                    "Program path must be specified"
                } else {
                    "Listing names without reading content is not modeled"
                };
                return Ok(json!({"policy":"", "unresolved":[reason]}).to_string());
            }
            _ => return Err(ProviderError::InvalidResponse),
        };
        let exact = format!("resource.path == {}", json!(path));
        let condition = if tree {
            let prefix = format!("{}/", path.trim_end_matches('/'));
            format!(
                "({exact} || resource.path like \"{}*\")",
                prefix.replace('"', "\\\"").replace('*', "\\*")
            )
        } else {
            exact
        };
        let policy = format!(
            "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when {{ {condition} }};"
        );
        Ok(json!({"policy":policy,"unresolved":[]}).to_string())
    }
}

struct Recording {
    inner: DaemonDispatcher,
    requests: Arc<Mutex<Vec<(u32, Value)>>>,
    timeout: Option<Duration>,
}
impl RequestDispatcher for Recording {
    fn dispatch_timeout(&self, payload: &[u8]) -> Option<Duration> {
        self.timeout
            .or_else(|| self.inner.dispatch_timeout(payload))
    }
    fn dispatch(
        &self,
        request: DispatchRequest,
        response: &mut dyn std::io::Write,
    ) -> Result<ResponseDisposition, DispatchError> {
        self.requests.lock().unwrap().push((
            request.peer.uid(),
            serde_json::from_slice(&request.payload).unwrap(),
        ));
        self.inner.dispatch(request, response)
    }
}

struct Server {
    shutdown: ShutdownToken,
    task: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<(u32, Value)>>>,
}
impl Server {
    async fn start(
        socket: &Path,
        provider: Arc<dyn ModelProvider>,
        timeout: Option<Duration>,
    ) -> Self {
        let service = Arc::new(IntentService::new(provider, "fixture".into(), || {
            chrono::Utc::now().timestamp()
        }));
        let inner = DaemonDispatcher::new(
            PapService::new(Arc::new(SqlitePolicyRepository::temporary().unwrap())),
            Arc::new(RootManagedPrincipalPolicy::default()),
            asc_daemon::scan_application(
                asc_action_runtime::testing::discarding_finalizer(),
                Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
            ),
        )
        .with_intent(service);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let dispatcher = Arc::new(Recording {
            inner,
            requests: Arc::clone(&requests),
            timeout,
        });
        let mut config = BootstrapConfig::new(socket);
        config.socket_mode = 0o666;
        let shutdown = ShutdownToken::new();
        let signal = shutdown.clone();
        let task = tokio::spawn(async move {
            serve(config, dispatcher, Arc::new(JsonRejectionEncoder), signal)
                .await
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if tokio::net::UnixStream::connect(socket).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        Self {
            shutdown,
            task,
            requests,
        }
    }
    async fn stop(self) {
        self.shutdown.request();
        self.task.await.unwrap();
    }
}

fn cli(socket: &Path, directory: &Path, task: &str) -> Command {
    let mut cli = base_cli(socket, directory);
    cli.args(["intent", "propose", "--task", task]);
    cli
}

fn cli_command(socket: &Path, directory: &Path, command: &str) -> Command {
    let mut cli = base_cli(socket, directory);
    cli.args(["intent", "propose-command", "--command", command]);
    cli
}

fn base_cli(socket: &Path, directory: &Path) -> Command {
    let copied = directory.join("agent-sec-cli");
    let binary = if copied.exists() {
        copied
    } else {
        env!("CARGO_BIN_EXE_agent-sec-cli").into()
    };
    let mut cli = Command::new(binary);
    cli.env_clear()
        .env("PATH", "/usr/bin:/bin")
        // The fixture daemon runs as the test user, while the CLI still authenticates its peer by
        // UID; CI runs both as an unprivileged user, so the testing-only escape hatch has to
        // survive `env_clear`.
        .env("AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK", "1")
        .current_dir(directory)
        .arg("--socket")
        .arg(socket);
    // `cargo llvm-cov` instruments the CLI child through this path, which carries `%p` so
    // concurrent children keep separate counters; `env_clear` must not hide them from the gate.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        cli.env("LLVM_PROFILE_FILE", profile);
    }
    cli
}
fn output_json(output: &Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE_FILE_BYTES"));
    value
}
/// The candidate carries only model output and its source, with no program-generated fields.
fn assert_candidate_carries_only_model_output(candidate: &Value) {
    let mut keys: Vec<_> = candidate
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "artifact",
            "authorization_required",
            "candidate_hash",
            "created_at",
            "draft",
            "expires_at",
            "format_version",
            "notes",
            "status",
            "task_id",
        ]
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn formats_clarifications_and_real_peer_identity() {
    let directory = common::Directory::new();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
    let socket = directory.0.join("daemon.sock");
    let provider = Arc::new(Replay::default());
    let server = Server::start(&socket, provider.clone(), None).await;
    for (task, resolved) in [
        ("Read report.csv", true),
        ("Read /outside/missing.csv", true),
        ("Run tests", false),
        ("Only list names without reading content", false),
    ] {
        let output = cli(&socket, &directory.0, task)
            .args(["--format", "json"])
            .output()
            .unwrap();
        let value = output_json(&output, 0);
        assert_eq!(value["outcome"], "candidate");
        assert!(value.get("decision").is_none());
        assert_eq!(value["candidate"]["status"], "CANDIDATE");
        assert_eq!(
            value["candidate"]["format_version"],
            "asc.intent.candidate.v1"
        );
        assert_eq!(
            value["candidate"]["draft"]["format_version"],
            "asc.intent.draft.v1"
        );
        assert_eq!(value["candidate"]["artifact"]["language"], "cedar");
        let unresolved = value["candidate"]["draft"]["unresolved"]
            .as_array()
            .unwrap();
        assert_eq!(unresolved.is_empty(), resolved);
        assert_candidate_carries_only_model_output(&value["candidate"]);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE_FILE_BYTES"));
    }
    for format in ["text", "cedar"] {
        let output = cli(&socket, &directory.0, "Read report.csv")
            .args(["--format", format])
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("not effective") || text.contains("not approved or effective"));
    }
    let unmapped = output_json(
        &cli(
            &socket,
            &directory.0,
            "Only list names without reading content",
        )
        .args(["--format", "json"])
        .output()
        .unwrap(),
        0,
    );
    assert!(
        !unmapped["candidate"]["draft"]["unresolved"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(unmapped["candidate"]["artifact"]["text"], "");
    for format in ["text", "cedar"] {
        let output = cli(
            &socket,
            &directory.0,
            "Only list names without reading content",
        )
        .args(["--format", format])
        .output()
        .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("unresolved") && text.contains("not modeled"));
        assert!(!text.contains("permit("));
    }
    if rustix::process::geteuid().as_raw() == 0 {
        check_unprivileged_peer(&directory, &socket, &server);
    }
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, request)| request["method"] == "intent.propose"
                && request["params"]["format_version"] == "asc.intent.request.v1")
    );
    server.stop().await;
    let output = cli(&socket, &directory.0, "Read report.csv")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

fn check_unprivileged_peer(directory: &common::Directory, socket: &Path, server: &Server) {
    common::executable(&directory.0);
    let value = output_json(
        &cli(socket, &directory.0, "Read report.csv")
            .uid(65534)
            .gid(65534)
            .args(["--format", "json"])
            .output()
            .unwrap(),
        0,
    );
    assert!(
        value["candidate"]["task_id"]
            .as_str()
            .unwrap()
            .starts_with("intent-65534-")
    );
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(uid, _)| *uid == 65534)
    );
}

/// A task the model and schema cannot express stays a successful run: the JSON says so, the reasons
/// stay visible and no caller has to infer the outcome from an exit code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_task_is_a_successful_run() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let server = Server::start(&socket, Arc::new(Replay::default()), None).await;
    let unsupported = output_json(
        &cli(&socket, &directory.0, "Read work/report.csv")
            .args(["--format", "json"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(unsupported["outcome"], "needs_clarification");
    assert!(unsupported.get("candidate").is_none());
    assert!(
        !unsupported["reasons"].as_array().unwrap().is_empty(),
        "{unsupported}"
    );
    let text = String::from_utf8(
        cli(&socket, &directory.0, "Read work/report.csv")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(text.contains("cannot express this task"));
    assert!(text.contains("canonical absolute"));
    assert!(text.contains("Model Cedar draft (unconfirmed)"));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_carries_only_task_text_and_ttl() {
    let root = common::Directory::new();
    let caller = root.0.join("caller");
    fs::create_dir(&caller).unwrap();
    // Directory contents and links are no longer collected; presence must not affect the request.
    fs::write(caller.join("large"), vec![0; 1024 * 1024 + 1]).unwrap();
    fs::write(caller.join(".hidden"), "PRIVATE_FILE_BYTES").unwrap();
    symlink("/", caller.join("link")).unwrap();
    assert_ne!(std::env::current_dir().unwrap(), caller);
    let socket = root.0.join("daemon.sock");
    let server = Server::start(&socket, Arc::new(Replay::default()), None).await;
    let task = "Read file contents of the current directory and subdirectories";
    for format in ["json", "text"] {
        let output = cli(&socket, &caller, task)
            .args(["--format", format])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE_FILE_BYTES"));
        let requests = server.requests.lock().unwrap();
        let params = &requests.last().unwrap().1["params"];
        assert_eq!(params["format_version"], "asc.intent.request.v1");
        assert_eq!(params["task"], task);
        assert_eq!(params["ttl"], 1800);
        let keys: Vec<_> = params.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["format_version", "task", "ttl"]);
        let encoded = serde_json::to_string(params).unwrap();
        assert!(!encoded.contains(caller.to_str().unwrap()));
        assert!(!encoded.contains("PRIVATE_FILE_BYTES"));
    }
    let output = cli(&socket, &caller, task).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "Model Cedar draft (unconfirmed)",
        "Read file contents of the current directory and subdirectories",
        "resource.path like",
        "Validated Cedar",
        "file-content read",
        "not listing names only",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    assert!(!text.contains("PRIVATE_FILE_BYTES"), "{text}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_failure_never_claims_candidate_or_approval_delivery() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let server = Server::start(&socket, Arc::new(Replay::default()), None).await;
    let output = cli(&socket, &directory.0, "Read report.csv")
        .args(["--format", "json"])
        .stdout(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Candidate generated"));
    server.stop().await;
}

#[test]
fn path_arguments_and_oversized_tasks_never_reach_the_daemon() {
    let directory = common::Directory::new();
    let socket = directory.0.join("absent.sock");
    for flag in ["--workspace", "--attach", "--protect-file"] {
        let output = cli(&socket, &directory.0, "Read report.csv")
            .args([flag, "/tmp"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{flag}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(flag));
    }
    for task in [
        " ".to_owned(),
        "x".repeat(asc_daemon_protocol::intent::INTENT_TASK_BYTES + 1),
    ] {
        let output = cli(&socket, &directory.0, &task).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("budget"), "{stderr}");
        assert!(!stderr.contains("daemon transport"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untrusted_responses_never_render_a_candidate_or_leak_diagnostics() {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    let directory = common::Directory::new();
    let socket = directory.0.join("real.sock");
    let server = Server::start(&socket, Arc::new(Replay::default()), None).await;
    let valid = output_json(
        &cli(&socket, &directory.0, "Read report.csv")
            .args(["--format", "json"])
            .output()
            .unwrap(),
        0,
    );
    server.stop().await;
    for case in [
        "draft_version",
        "draft_goal",
        "task_length",
        "relative_path",
        "unknown_field",
        "status",
        "hash",
        "source",
        "artifact_rebuild",
        "forbidden",
        "oversized",
        "truncated",
        "error",
    ] {
        let mut result = valid.clone();
        corrupt_candidate(&mut result["candidate"], case);
        let socket = directory.0.join(format!("{case}.sock"));
        let listener = UnixListener::bind(&socket).unwrap();
        let mut bytes =
            serde_json::to_vec(&json!({"requestId":"fixture", "result":result})).unwrap();
        if case == "error" {
            bytes = serde_json::to_vec(&json!({"requestId":"fixture", "error":{"code":"unavailable", "message":"PRIVATE_FILE_BYTES"}})).unwrap();
        }
        if case == "truncated" {
            bytes.truncate(bytes.len() / 2);
        }
        bytes.push(b'\n');
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "intent.propose");
            // Older clients sent path fields and must be rejected, so only three fields appear here.
            let keys: Vec<_> = request["params"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, ["format_version", "task", "ttl"]);
            stream.write_all(&bytes).unwrap();
        });
        let output = cli(&socket, &directory.0, "Read report.csv")
            .args(["--format", "json"])
            .output()
            .unwrap();
        responder.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{case}");
        assert!(output.stdout.is_empty(), "{case}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE_FILE_BYTES"));
    }
}

fn corrupt_candidate(candidate: &mut Value, case: &str) {
    match case {
        "draft_version" => candidate["draft"]["format_version"] = json!("asc.intent.draft.v999"),
        "draft_goal" => candidate["draft"]["unresolved"] = Value::Null,
        "task_length" => candidate["draft"]["source"]["byte_length"] = json!(4096),
        "relative_path" => {
            candidate["draft"]["policy"] = json!(
                "permit(principal, action, resource) when { resource.path == \"relative\" };"
            );
        }
        "unknown_field" => candidate["unrecognized_authority"] = json!(true),
        "status" => candidate["status"] = json!("APPROVED"),
        "hash" => candidate["candidate_hash"] = json!("forged"),
        "source" => candidate["draft"]["source"]["sha256"] = json!("forged"),
        // draft.policy and artifact.text must agree: the client rebuilds the artifact.
        "artifact_rebuild" => {
            candidate["artifact"]["text"] = json!(
                "permit(principal, action == SecCore::Action::\"file.write\", resource is SecCore::File);"
            );
        }
        "forbidden" => candidate["artifact"]["language"] = json!("json"),
        "oversized" => candidate["artifact"]["text"] = json!("x".repeat(512 * 1024)),
        _ => {}
    }
    if !matches!(case, "hash" | "truncated" | "oversized" | "error") {
        rehash(candidate);
    }
}

fn rehash(candidate: &mut Value) {
    use sha2::{Digest as _, Sha256};
    fn sort(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.sort_keys();
                map.values_mut().for_each(sort);
            }
            Value::Array(values) => values.iter_mut().for_each(sort),
            _ => {}
        }
    }
    candidate.as_object_mut().unwrap().remove("candidate_hash");
    sort(candidate);
    candidate["candidate_hash"] = json!(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(candidate).unwrap())
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_while_waiting_for_daemon_never_delivers_a_candidate() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let (started, entered) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let server = Server::start(
        &socket,
        Arc::new(Blocking {
            started,
            release: release.clone(),
        }),
        None,
    )
    .await;
    let child = cli(&socket, &directory.0, "Read report.csv")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        Command::new("/bin/kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Candidate generated"));
    server.stop().await;
}

struct Blocking {
    started: std::sync::mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
}
impl ModelProvider for Blocking {
    fn complete(&self, _: &str, _: &str, _: &str, _: Instant) -> Result<String, ProviderError> {
        self.started.send(()).unwrap();
        let (lock, wake) = &*self.release;
        let (release, _) = wake
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(10), |value| {
                !*value
            })
            .unwrap();
        assert!(*release);
        Err(ProviderError::Transport)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uds_timeout_does_not_release_actual_execution_capacity() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let (started, entered) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let server = Server::start(
        &socket,
        Arc::new(Blocking {
            started,
            release: Arc::clone(&release),
        }),
        Some(Duration::from_millis(200)),
    )
    .await;
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
    common::executable(&directory.0);
    let uid = rustix::process::geteuid().as_raw();
    let owners = if uid == 0 {
        vec![0, 65531, 65532, 65533]
    } else {
        vec![uid]
    };
    for owner in &owners {
        let first = cli(&socket, &directory.0, "Read report.csv")
            .uid(*owner)
            .gid(*owner)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let output = first.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("deadline_exceeded"));
    }
    let rejected = if uid == 0 { vec![0, 65534] } else { vec![uid] };
    for owner in rejected {
        let output = cli(&socket, &directory.0, "Read report.csv")
            .uid(owner)
            .gid(owner)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("resource_exhausted"));
    }
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = cli(&socket, &directory.0, "Read report.csv")
            .output()
            .unwrap();
        if String::from_utf8_lossy(&output.stderr).contains("unavailable") {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    server.stop().await;
}

const COMMAND: &str = "read -r line < report.csv";
const COMMAND_POLICY: &str = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"/work/report.csv\" };";
const COMMAND_CAVEAT: &str = "cat resolved through PATH, exec target unknown";

/// Answers only the two command fixtures; any other command would change the wire contract.
struct CommandReplay {
    calls: AtomicUsize,
    cwd: String,
}
impl ModelProvider for CommandReplay {
    fn complete(
        &self,
        model: &str,
        system: &str,
        input: &str,
        _: Instant,
    ) -> Result<String, ProviderError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(model, "fixture");
        assert!(system.contains(asc_intent::CEDAR_SCHEMA));
        assert!(!system.contains("PRIVATE_FILE_BYTES"));
        let input: Value = serde_json::from_str(input).unwrap();
        let keys: Vec<_> = input
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["command", "cwd"]);
        assert_eq!(input["cwd"], self.cwd);
        match input["command"].as_str().unwrap() {
            COMMAND => Ok(
                json!({"policy":COMMAND_POLICY,"unresolved":[],"caveats":[COMMAND_CAVEAT]})
                    .to_string(),
            ),
            "deploy --env prod" => {
                Ok(json!({"policy":"","unresolved":["deployment target and runtime dependencies need confirmation"]}).to_string())
            }
            other => unreachable!("unexpected command {other}"),
        }
    }
}

fn command_provider(directory: &Path) -> Arc<CommandReplay> {
    let cwd = fs::canonicalize(directory).unwrap();
    Arc::new(CommandReplay {
        calls: AtomicUsize::new(0),
        cwd: cwd.to_str().unwrap().to_owned(),
    })
}

/// The command path sends the hook's own fields and nothing else, and a candidate stays unapproved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_proposals_send_only_command_and_cwd() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let cwd = fs::canonicalize(&directory.0).unwrap();
    let provider = command_provider(&directory.0);
    let server = Server::start(&socket, provider.clone(), None).await;
    let candidate = output_json(
        &cli_command(&socket, &directory.0, COMMAND)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(candidate["outcome"], "candidate");
    assert_eq!(candidate["policy"], COMMAND_POLICY);
    assert_eq!(candidate["caveats"], json!([COMMAND_CAVEAT]));
    assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
    {
        let requests = server.requests.lock().unwrap();
        let request = &requests.last().unwrap().1;
        assert_eq!(request["method"], "intent.propose_command");
        let params = &request["params"];
        let keys: Vec<_> = params
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["format_version", "command", "cwd"]);
        assert_eq!(params["format_version"], "asc.intent.command.request.v1");
        assert_eq!(params["command"], COMMAND);
        assert_eq!(params["cwd"], cwd.to_str().unwrap());
    }
    let text = String::from_utf8(
        cli_command(&socket, &directory.0, COMMAND)
            .args(["--format", "text"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(text.contains("not approved or effective"), "{text}");
    assert!(text.contains("What the candidate would allow"), "{text}");
    assert!(text.contains("/work/report.csv"), "{text}");
    assert!(text.contains("network"), "{text}");
    assert!(text.contains("Not covered by this candidate"), "{text}");
    assert!(text.contains(COMMAND_CAVEAT), "{text}");
    assert!(!text.contains("permit("), "{text}");
    let cedar = String::from_utf8(
        cli_command(&socket, &directory.0, COMMAND)
            .args(["--format", "cedar"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(cedar.contains("// Not covered:"), "{cedar}");
    server.stop().await;
}

/// A clarification and a locally rejected input both end without any Cedar to apply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_clarification_and_invalid_input_emit_no_cedar() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let server = Server::start(&socket, command_provider(&directory.0), None).await;
    let output = cli_command(&socket, &directory.0, "deploy --env prod")
        .args(["--format", "cedar"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Needs clarification"), "{text}");
    assert!(!text.contains("permit("), "{text}");
    server.stop().await;
    // These inputs are refused before any request exists, so the stopped socket is never reached.
    for cwd in ["/nonexistent-command-cwd", "relative/missing"] {
        let output = cli_command(&socket, &directory.0, COMMAND)
            .args(["--cwd", cwd])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{cwd}");
        assert!(output.stdout.is_empty(), "{cwd}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("working directory"),
            "{cwd}"
        );
    }
    for command in ["", "  "] {
        let output = cli_command(&socket, &directory.0, command)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{command:?}");
        assert!(output.stdout.is_empty(), "{command:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("non-empty"),
            "{command:?}"
        );
    }
    let oversized = "x".repeat(asc_intent::MAX_COMMAND_BYTES + 1);
    let output = cli_command(&socket, &directory.0, &oversized)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("64 KiB"));
}

/// Results that this build cannot reproduce as Cedar or explain are refused, not rendered.
#[test]
fn command_results_are_reverified_before_rendering() {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    let directory = common::Directory::new();
    for (case, result) in [
        (
            "relative_policy",
            json!({"outcome":"candidate","policy":"permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"relative\" };"}),
        ),
        (
            "no_reasons",
            json!({"outcome":"needs_clarification","reasons":[]}),
        ),
        (
            "mixed",
            json!({"outcome":"candidate","policy":"","reasons":["why"]}),
        ),
    ] {
        let socket = directory.0.join(format!("{case}.sock"));
        let listener = UnixListener::bind(&socket).unwrap();
        let mut bytes =
            serde_json::to_vec(&json!({"requestId":"fixture", "result":result})).unwrap();
        bytes.push(b'\n');
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "intent.propose_command");
            stream.write_all(&bytes).unwrap();
        });
        let output = cli_command(&socket, &directory.0, COMMAND)
            .output()
            .unwrap();
        responder.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{case}");
        assert!(output.stdout.is_empty(), "{case}");
    }
}
