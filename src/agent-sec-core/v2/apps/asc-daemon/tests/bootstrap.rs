use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

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

fn unique_directory() -> PathBuf {
    std::env::temp_dir().join(format!(
        "asc-daemon-bootstrap-{}-{}",
        std::process::id(),
        DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

async fn wait_for_socket(path: &Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match UnixStream::connect(path).await {
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
                    panic!("daemon bootstrap failed before accepting connections: {error}")
                }
            }
        }
    })
    .await
    .expect("daemon bootstrap should accept connections");
}

async fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the foreground daemon should exit within the deadline")
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
    let directory = unique_directory();
    std::fs::create_dir(&directory).unwrap();
    let socket_path = directory.join("daemon.sock");
    let data_dir = directory.join("data");
    std::fs::create_dir(&data_dir).unwrap();
    std::fs::create_dir(data_dir.join("security-events.db")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-sec-daemon"))
        .env("AGENT_SEC_DATA_DIR", &data_dir)
        .args(["serve", "--socket"])
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    assert!(!wait_for_exit(&mut child).await.success());
    assert!(!socket_path.exists());
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_binds_when_jsonl_event_storage_is_unusable() {
    let directory = unique_directory();
    std::fs::create_dir(&directory).unwrap();
    let socket_path = directory.join("daemon.sock");
    let data_dir = directory.join("data");
    std::fs::create_dir(&data_dir).unwrap();
    std::fs::create_dir(data_dir.join("security-events.jsonl")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_agent-sec-daemon"))
        .env("AGENT_SEC_DATA_DIR", &data_dir)
        .args(["serve", "--socket"])
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut running = RunningBinary {
        child,
        directory,
        socket_path,
    };

    wait_for_socket(&running.socket_path).await;
    assert!(data_dir.join("security-events.db").exists());

    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(running.child.id().to_string())
        .status()
        .unwrap();
    assert!(signal.success());
    assert!(wait_for_exit(&mut running.child).await.success());
}

async fn run_binary_scenario(configure_admin: bool) {
    let directory = unique_directory();
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = directory.join("daemon.sock");
    let data_dir = directory.join("data");
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-sec-daemon"));
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
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut running = RunningBinary {
        child,
        directory,
        socket_path,
    };

    wait_for_socket(&running.socket_path).await;
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
    let status = wait_for_exit(&mut running.child).await;

    assert!(status.success());
    assert!(!running.socket_path.exists());
}
