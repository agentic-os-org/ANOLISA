//! Dual-UID acceptance for `events` over one system socket.
//!
//! This is the acceptance the single-peer tests cannot give: two real UIDs
//! connect to the same running daemon over the same socket, and each sees
//! only its own rows while root — through the same production CLI — reads
//! every owner and may narrow with `--owner-uid`. The per-UID children run
//! the real `agent-sec-cli` binary with the UID adopted before exec
//! (`CommandExt::uid`), so the daemon's scope decision rides on
//! kernel-authenticated peer credentials end to end.
//!
//! Requires root (adopting a foreign UID is a root privilege); skipped with
//! a note on single-UID runners, where the same-peer isolation acceptance
//! in `asc-daemon`'s `sec_query_protocol` covers the non-root side.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use asc_action_runtime::Finalizer;
use asc_daemon::{BootstrapConfig, scan_application, serve};
use asc_daemon_core::{PeerCredentials, PrincipalPolicy, PrincipalRole};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_persistence_sqlite::security_events::SqliteEventWriter;
use asc_persistence_sqlite::security_events::query_source::SqliteEventQuerySource;
use asc_security_events::SecurityEvent;
use serde_json::{Map, Value, json};
use tokio::net::UnixStream;

/// A third owner that certainly never connects.
const FOREIGN_UID: u32 = 42_424_242;

#[derive(Clone, Copy)]
struct FixedRolePolicy(PrincipalRole);

impl PrincipalPolicy for FixedRolePolicy {
    fn role_for(&self, _peer: PeerCredentials) -> PrincipalRole {
        self.0
    }
}

struct DiscardingOutputs;

impl asc_action_runtime::SecurityEventSink for DiscardingOutputs {
    fn write(&self, _: &SecurityEvent) {}
}
impl asc_action_runtime::TelemetrySink for DiscardingOutputs {
    fn enabled(&self) -> bool {
        false
    }
    fn write(&self, _: &asc_telemetry::TelemetryRecord) -> asc_action_runtime::TelemetryStatus {
        asc_action_runtime::TelemetryStatus::Skipped
    }
}
impl asc_action_runtime::DiagnosticSink for DiscardingOutputs {
    fn record(&self, _: &asc_action_runtime::Diagnostic) {}
}

struct RunningDaemon {
    directory: PathBuf,
    socket_path: PathBuf,
    shutdown: asc_daemon_service::ShutdownToken,
    task: tokio::task::JoinHandle<()>,
}

impl RunningDaemon {
    async fn start(database: &Path) -> Self {
        let directory =
            std::env::temp_dir().join(format!("asc-cli-events-dual-uid-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let socket_path = directory.join("daemon.sock");
        let application = PapService::new(Arc::new(ProcessLocalPapRepository::default()));
        let outputs = Arc::new(DiscardingOutputs);
        let dispatcher = Arc::new(
            DaemonDispatcher::new(
                application,
                Arc::new(FixedRolePolicy(PrincipalRole::LocalUser)),
                scan_application(
                    Finalizer::new(outputs.clone(), outputs.clone(), outputs.clone()),
                    Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
                ),
            )
            .with_security_queries(
                SqliteEventQuerySource::new(database).expect("query source opens"),
            ),
        );
        let shutdown = asc_daemon_service::ShutdownToken::new();
        let service_shutdown = shutdown.clone();
        let mut config = BootstrapConfig::new(&socket_path);
        // The production system socket is connectable by every local user.
        config.socket_mode = 0o666;
        config.service.request_read_timeout = Duration::from_millis(50);
        let task = tokio::spawn(async move {
            serve(
                config,
                dispatcher,
                Arc::new(JsonRejectionEncoder),
                service_shutdown,
            )
            .await
            .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(probe) = UnixStream::connect(&socket_path).await {
                    drop(probe);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("daemon should accept connections on its socket");
        Self {
            directory,
            socket_path,
            shutdown,
            task,
        }
    }

    async fn stop(self) {
        self.shutdown.request();
        self.task.await.unwrap();
        std::fs::remove_dir(self.directory).unwrap();
    }
}

/// Runs the production CLI as `uid` and returns (exit code, stdout, stderr).
///
/// The binary is copied to a world-traversable scratch path first: the cargo
/// target tree lives under the builder's home, which the adopted UID cannot
/// traverse.
fn cli_as(uid: u32, socket: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;
    let scratch =
        std::env::temp_dir().join(format!("asc-cli-dual-uid-{}-{}", std::process::id(), uid));
    std::fs::copy(env!("CARGO_BIN_EXE_agent-sec-cli"), &scratch).expect("copy the CLI");
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o755))
        .expect("world-executable copy");
    let output = Command::new(&scratch)
        .uid(uid)
        .arg("--socket")
        .arg(socket)
        .args(args)
        .output()
        .expect("spawn agent-sec-cli");
    std::fs::remove_file(&scratch).ok();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn event_ids(stdout: &str) -> Vec<String> {
    let id_of = |value: &Value| value["event_id"].as_str().unwrap_or_default().to_owned();
    if let Ok(parsed) = serde_json::from_str::<Value>(stdout) {
        if let Some(items) = parsed.as_array() {
            return items.iter().map(id_of).collect();
        }
    }
    // jsonl output: one event object per line.
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("event line: {error}; line={line:?}"));
            id_of(&value)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_real_uids_share_one_system_socket_through_the_cli() {
    if rustix::process::geteuid().as_raw() != 0 {
        eprintln!("skipping: adopting foreign UIDs requires root");
        return;
    }
    let directory = std::env::temp_dir().join(format!(
        "asc-cli-events-dual-uid-dir-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("scratch dir");
    let database = directory.join("security-events.db");
    let writer = SqliteEventWriter::new(&database).expect("writer");
    for (id, uid, category) in [
        ("first-1", 1000_u32, "exec"),
        ("first-2", 1000, "network"),
        ("second-1", 2000, "exec"),
        ("foreign-1", FOREIGN_UID, "exec"),
    ] {
        let mut event = SecurityEvent::new("sandbox_prehook", category, Map::new());
        id.clone_into(&mut event.event_id);
        event.uid = uid;
        writer.write(&event);
    }
    writer.close_at(1000.0);
    let daemon = RunningDaemon::start(&database).await;

    // UID 1000 sees exactly its own two rows, newest first, as a v1 array.
    let (code, stdout, stderr) = cli_as(1000, &daemon.socket_path, &["events", "--output", "json"]);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(event_ids(&stdout), vec!["first-2", "first-1"], "{stdout}");
    let parsed: Value = serde_json::from_str(&stdout).expect("array");
    assert!(
        parsed[0]["details"].is_object(),
        "v1 JSON keeps the details payload"
    );

    // UID 2000 sees exactly its own row over the same socket.
    let (code, stdout, stderr) = cli_as(2000, &daemon.socket_path, &["events", "--output", "json"]);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(event_ids(&stdout), vec!["second-1"], "{stdout}");

    // The count projection agrees per UID.
    let (code, stdout, stderr) = cli_as(1000, &daemon.socket_path, &["events", "--count"]);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(stdout.trim(), "2", "count prints the bare number");

    // A non-root caller cannot select another owner.
    let (code, _stdout, stderr) = cli_as(
        1000,
        &daemon.socket_path,
        &["events", "--owner-uid", "2000"],
    );
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("owner_uid"),
        "the rejection names the unauthorized filter: {stderr}"
    );

    // Root, through the same CLI, reads every owner by default and may
    // narrow with the filter.
    let (code, stdout, stderr) = cli_as(0, &daemon.socket_path, &["events", "--output", "jsonl"]);
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(
        event_ids(&stdout).len(),
        4,
        "root's default scope is all owners"
    );

    let (code, stdout, stderr) = cli_as(
        0,
        &daemon.socket_path,
        &["events", "--output", "json", "--owner-uid", "42424242"],
    );
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(event_ids(&stdout), vec!["foreign-1"], "{stdout}");

    daemon.stop().await;
    std::fs::remove_dir_all(&directory).ok();
}
