use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

// Readiness includes real SQLite initialization on shared CI storage.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
// Match the deployment stop budget, including drain, persistence, and runtime cleanup.
const EXIT_TIMEOUT: Duration = Duration::from_secs(45);

struct RunningBinary {
    child: Child,
    directory: PathBuf,
    socket_path: PathBuf,
}

impl Drop for RunningBinary {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn create_runtime_directory() -> PathBuf {
    // Ignore TMPDIR: runner-owned ancestors may not satisfy the daemon contract.
    let directory = Path::new("/tmp").join(format!(
        "asc-daemon-bootstrap-{}-{}",
        std::process::id(),
        DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    directory
}

fn configured_command(directory: &Path) -> Command {
    // Process tests never open the machine's production SkillSec key store or configuration.
    let config = directory.join("skillsec.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "stateDir": directory.join("state")
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-sec-daemon"));
    command.arg("--skillsec-config").arg(config);
    command
}

async fn rejected_without_root(running: &mut RunningBinary) -> bool {
    if rustix::process::geteuid().as_raw() == 0 {
        return false;
    }
    assert!(!wait_for_exit(running).await.success());
    let stderr = read_stderr(&running.directory);
    assert!(
        stderr.contains("SkillSec system daemon must run as root"),
        "{stderr}"
    );
    assert!(!running.socket_path.exists());
    true
}

fn stderr_log(directory: &Path) -> Stdio {
    std::fs::File::create(directory.join("stderr.log"))
        .unwrap()
        .into()
}

fn read_stderr(directory: &Path) -> String {
    std::fs::read_to_string(directory.join("stderr.log")).unwrap()
}

async fn wait_for_socket(running: &mut RunningBinary) {
    let started = Instant::now();
    let result = tokio::time::timeout(STARTUP_TIMEOUT, async {
        loop {
            if let Some(status) = running.child.try_wait().unwrap() {
                panic!(
                    "daemon exited before accepting connections ({status}): {}",
                    read_stderr(&running.directory)
                );
            }
            match UnixStream::connect(&running.socket_path).await {
                Ok(stream) => {
                    drop(stream);
                    return;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => {
                    panic!(
                        "daemon bootstrap connection failed: {error}; stderr: {}",
                        read_stderr(&running.directory)
                    );
                }
            }
        }
    })
    .await;
    if result.is_err() {
        let elapsed = started.elapsed();
        let _ = running.child.kill();
        let status = running.child.wait().unwrap();
        panic!(
            "daemon bootstrap timed out after {elapsed:?}; pid: {}; socket: {}; status after cleanup: {status}; stderr: {}",
            running.child.id(),
            running.socket_path.display(),
            read_stderr(&running.directory)
        );
    }
}

async fn wait_for_exit(running: &mut RunningBinary) -> std::process::ExitStatus {
    let started = Instant::now();
    tokio::time::timeout(EXIT_TIMEOUT, async {
        loop {
            if let Some(status) = running.child.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let elapsed = started.elapsed();
        let _ = running.child.kill();
        let status = running.child.wait().unwrap();
        panic!(
            "daemon exit timed out after {elapsed:?}; pid: {}; socket: {}; status after cleanup: {status}; stderr: {}",
            running.child.id(),
            running.socket_path.display(),
            read_stderr(&running.directory)
        );
    })
}

async fn request(path: &Path, payload: &[u8]) -> Value {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream.write_all(payload).await.unwrap();
    let mut response = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut response)
        .await
        .unwrap();
    assert_eq!(response.pop(), Some(b'\n'));
    serde_json::from_slice(&response).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dproc_002_003_and_partial_013_binary_registers_pap_and_cleans_socket() {
    run_binary_scenario(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dproc_configured_administrator_can_query_without_root() {
    run_binary_scenario(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_refuses_to_bind_when_sqlite_event_storage_is_unusable() {
    let directory = create_runtime_directory();
    let socket_path = directory.join("daemon.sock");
    let data_dir = directory.join("data");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&data_dir)
        .unwrap();
    std::fs::create_dir(data_dir.join("security-events.db")).unwrap();
    let child = configured_command(&directory)
        .env("AGENT_SEC_DATA_DIR", &data_dir)
        .args(["serve", "--socket"])
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_log(&directory))
        .spawn()
        .unwrap();

    let mut running = RunningBinary {
        child,
        directory,
        socket_path,
    };
    if rejected_without_root(&mut running).await {
        return;
    }
    assert!(!wait_for_exit(&mut running).await.success());
    let stderr = read_stderr(&running.directory);
    assert!(
        stderr.contains("security event storage unavailable"),
        "{stderr}"
    );
    assert!(!running.socket_path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_binds_when_jsonl_event_storage_is_unusable() {
    let directory = create_runtime_directory();
    let socket_path = directory.join("daemon.sock");
    let data_dir = directory.join("data");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&data_dir)
        .unwrap();
    std::fs::create_dir(data_dir.join("security-events.jsonl")).unwrap();
    let child = configured_command(&directory)
        .env("AGENT_SEC_DATA_DIR", &data_dir)
        .args(["serve", "--socket"])
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_log(&directory))
        .spawn()
        .unwrap();
    let mut running = RunningBinary {
        child,
        directory,
        socket_path,
    };

    if rejected_without_root(&mut running).await {
        return;
    }
    wait_for_socket(&mut running).await;
    assert!(data_dir.join("security-events.db").exists());

    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(running.child.id().to_string())
        .status()
        .unwrap();
    assert!(signal.success());
    assert!(wait_for_exit(&mut running).await.success());
    // Socket readiness does not flush the asynchronous diagnostic writer.
    let stderr = read_stderr(&running.directory);
    assert!(
        stderr.contains("JSONL security event log unavailable"),
        "{stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dproc_scope_binary_starts_discovery_and_stops_it_on_sigterm() {
    let directory = create_runtime_directory();
    let socket_path = directory.join("daemon.sock");
    let child = configured_command(&directory)
        .env("AGENT_SEC_DATA_DIR", directory.join("data"))
        .arg("--policy-admin-uid")
        .arg(std::fs::metadata(&directory).unwrap().uid().to_string())
        .args(["serve", "--socket"])
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_log(&directory))
        .spawn()
        .unwrap();
    let mut running = RunningBinary {
        child,
        directory,
        socket_path,
    };
    if rejected_without_root(&mut running).await {
        return;
    }
    wait_for_socket(&mut running).await;
    let policy = request(&running.socket_path, b"{\"method\":\"policy.templates.create\",\"params\":{\"policyName\":\"scope-policy\",\"template\":{\"specVersion\":\"0.1\",\"rules\":[{\"effect\":\"block\",\"category\":\"file\",\"action\":\"write\",\"target\":{\"type\":\"file\",\"path\":\"/protected\"},\"where\":{\"operation\":{\"eq\":\"delete\"}}}]}}}\n").await;
    for _ in 0..2 {
        let mut payload = serde_json::to_vec(&serde_json::json!({"method":"policy.scopes.create","params":{
            "selector":{"kind":"process","match":{"executable":env!("CARGO_BIN_EXE_agent-sec-daemon")}},
            "policyTemplates":[{"policyId":policy["result"]["policyId"],"policyRevision":policy["result"]["revision"]}]
        }})).unwrap();
        payload.push(b'\n');
        let created = request(&running.socket_path, &payload).await;
        assert!(created["result"]["scopeId"].is_string(), "{created}");
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if read_stderr(&running.directory).contains("selected new instances") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial scan should discover the daemon executable");
    assert!(
        Command::new("/bin/kill")
            .arg("-TERM")
            .arg(running.child.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    assert!(wait_for_exit(&mut running).await.success());
    assert!(!running.socket_path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dproc_removed_probe_option_rejects_before_binding_socket() {
    let directory = create_runtime_directory();
    let socket_path = directory.join("daemon.sock");
    let config = directory.join("probes.json");
    let child = Command::new(env!("CARGO_BIN_EXE_agent-sec-daemon"))
        .env("AGENT_SEC_DATA_DIR", directory.join("data"))
        .args(["serve", "--socket"])
        .arg(&socket_path)
        .arg("--agent-probes")
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_log(&directory))
        .spawn()
        .unwrap();
    let mut running = RunningBinary {
        child,
        directory,
        socket_path,
    };
    assert!(!wait_for_exit(&mut running).await.success());
    assert!(!running.socket_path.exists());
    assert!(read_stderr(&running.directory).contains("unknown argument: --agent-probes"));
}

async fn run_binary_scenario(configure_admin: bool) {
    let directory = create_runtime_directory();
    let socket_path = directory.join("daemon.sock");
    let data_dir = directory.join("data");
    let mut command = configured_command(&directory);
    command.env("AGENT_SEC_DATA_DIR", &data_dir);
    if configure_admin {
        let uid = std::fs::metadata(&directory).unwrap().uid();
        command.args(["--policy-admin-uid", &uid.to_string()]);
    }
    let child = command
        .args(["serve", "--socket"])
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_log(&directory))
        .spawn()
        .unwrap();
    let mut running = RunningBinary {
        child,
        directory,
        socket_path,
    };

    if rejected_without_root(&mut running).await {
        return;
    }
    wait_for_socket(&mut running).await;
    // DPROC-UDS-001: local users can reach the service; PAP still requires an administrator.
    assert_eq!(
        std::fs::metadata(&running.socket_path).unwrap().mode() & 0o7777,
        0o666
    );
    let scan = request(
        &running.socket_path,
        b"{\"method\":\"action.code_scan\",\"params\":{\"code\":\"echo socket-access\",\"language\":\"bash\",\"mode\":\"regex\"}}\n",
    )
    .await;
    assert_eq!(scan["result"]["verdict"], "pass");
    assert!(scan.get("error").is_none());
    // A read-only request exercises authorization without sending deployments
    // to the host's AgentSight. Binding delivery has separate component fixtures.
    let response = request(
        &running.socket_path,
        b"{\"method\":\"policy.templates.list\",\"params\":{\"limit\":10,\"offset\":0}}\n",
    )
    .await;
    uuid::Uuid::parse_str(response["requestId"].as_str().unwrap()).unwrap();
    if configure_admin || std::fs::metadata(&running.socket_path).unwrap().uid() == 0 {
        assert_eq!(
            response,
            serde_json::json!({
                "requestId": response["requestId"],
                "result": {"items": [], "total": 0}
            })
        );
    } else {
        assert_eq!(response["error"]["code"], "permission_denied");
    }

    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(running.child.id().to_string())
        .status()
        .unwrap();
    assert!(signal.success());
    let status = wait_for_exit(&mut running).await;

    assert!(status.success());
    assert!(!running.socket_path.exists());
}

async fn pap_request(path: &Path, method: &str, params: Value) -> Value {
    let mut payload =
        serde_json::to_vec(&serde_json::json!({"method":method,"params":params})).unwrap();
    payload.push(b'\n');
    request(path, &payload).await
}
fn restart_command(directory: &Path, socket: &Path) -> Child {
    configured_command(directory)
        .env("AGENT_SEC_DATA_DIR", directory.join("data"))
        .args(["serve", "--socket"])
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_log(directory))
        .spawn()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_policy_and_scope_survive_sigkill_and_database_lease_rejects_second_socket() {
    let directory = create_runtime_directory();
    let socket_path = directory.join("daemon.sock");
    let child = restart_command(&directory, &socket_path);
    let mut running = RunningBinary {
        child,
        directory,
        socket_path,
    };
    if rejected_without_root(&mut running).await {
        return;
    }
    wait_for_socket(&mut running).await;
    let template = serde_json::json!({"specVersion": "0.1", "rules": [{"effect": "block", "category": "file", "action": "write", "target": {"type": "file", "path": "/protected"}, "where": {"operation": {"eq": "delete"}}}]});
    let policy = pap_request(
        &running.socket_path,
        "policy.templates.create",
        serde_json::json!({"policyName":"original","template":template}),
    )
    .await;
    assert!(policy.get("error").is_none(), "{policy}");
    let policy_id = policy["result"]["policyId"].clone();
    // This impossible PID exercises real registry startup without contacting the host PEP.
    let scope = pap_request(
        &running.socket_path,
        "policy.scopes.create",
        serde_json::json!({
            "selector": {"kind": "pid", "pid": u32::MAX},
            "policyTemplates": [{"policyId": policy_id, "policyRevision": 1}]
        }),
    )
    .await;
    assert!(scope.get("error").is_none(), "{scope}");
    let scope_id = scope["result"]["scopeId"].clone();
    let updated = pap_request(
        &running.socket_path,
        "policy.templates.update",
        serde_json::json!({"policyId":policy_id,"policyName":"updated","template":template}),
    )
    .await;
    assert_eq!(updated["result"]["revision"], 2);
    assert_database_lease_excludes_second_socket(&running.directory);
    running.child.kill().unwrap();
    running.child.wait().unwrap();
    running.child = restart_command(&running.directory, &running.socket_path);
    wait_for_socket(&mut running).await;
    let restored = pap_request(
        &running.socket_path,
        "policy.scopes.get",
        serde_json::json!({"id":scope_id}),
    )
    .await;
    assert_eq!(restored["result"], scope["result"]);
    let current = pap_request(
        &running.socket_path,
        "policy.templates.get",
        serde_json::json!({"id":policy_id,"revision":2}),
    )
    .await;
    assert_eq!(current["result"], updated["result"]);
    for response in [&restored, &current] {
        let encoded = response.to_string();
        assert!(!encoded.contains("statusVersion") && !encoded.contains("cleanup"));
    }
    let deleted = pap_request(
        &running.socket_path,
        "policy.scopes.delete",
        serde_json::json!({"id":scope_id}),
    )
    .await;
    assert!(deleted.get("error").is_none(), "{deleted}");
    let missing = pap_request(
        &running.socket_path,
        "policy.scopes.get",
        serde_json::json!({"id":scope_id}),
    )
    .await;
    assert_eq!(missing["error"]["code"], "not_found");
    running.child.kill().unwrap();
    running.child.wait().unwrap();
    running.child = restart_command(&running.directory, &running.socket_path);
    wait_for_socket(&mut running).await;
    let missing = pap_request(
        &running.socket_path,
        "policy.scopes.get",
        serde_json::json!({"id":scope_id}),
    )
    .await;
    assert_eq!(missing["error"]["code"], "not_found");
    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(running.child.id().to_string())
        .status()
        .unwrap();
    assert!(signal.success());
    assert!(wait_for_exit(&mut running).await.success());
}

fn assert_database_lease_excludes_second_socket(directory: &Path) {
    let second_runtime = directory.join("second-runtime");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&second_runtime)
        .unwrap();
    let second = configured_command(directory)
        .env("AGENT_SEC_DATA_DIR", directory.join("data"))
        .args(["serve", "--socket"])
        .arg(second_runtime.join("second.sock"))
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("Policy database already owned"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
}
