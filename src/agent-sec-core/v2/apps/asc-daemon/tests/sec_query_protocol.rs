//! End-to-end `sec.*` owner-scoped queries over a real Unix socket.
//!
//! Proves the wiring the unit tests cannot: that the v1 query methods are
//! registered, that a local peer is authorized to call them, and that the
//! rows another owner wrote into the shared system store are invisible —
//! in lists, in aggregates, in grouped counts, and through a direct
//! `event_id` lookup (issue #6608).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use asc_action_runtime::Finalizer;
use asc_daemon::{BootstrapConfig, scan_application, serve};
use asc_daemon_core::{PeerCredentials, PrincipalPolicy, PrincipalRole};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder, SqliteEventQuerySource};
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_persistence_sqlite::security_events::SqliteEventWriter;
use asc_security_events::SecurityEvent;
use serde_json::{Map, json};
use tokio::net::UnixStream;

mod support;

use support::request_json;

/// A foreign owner that certainly did not open this socket.
const FOREIGN_UID: u32 = 42_424_242;

#[derive(Clone, Copy)]
struct FixedRolePolicy(PrincipalRole);

impl PrincipalPolicy for FixedRolePolicy {
    fn role_for(&self, _peer: PeerCredentials) -> PrincipalRole {
        self.0
    }
}

struct RunningDaemon {
    directory: PathBuf,
    socket_path: PathBuf,
    shutdown: asc_daemon_service::ShutdownToken,
    task: tokio::task::JoinHandle<()>,
}

impl RunningDaemon {
    async fn start(role: PrincipalRole, database: &Path) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "asc-daemon-sec-query-{}-{}",
            std::process::id(),
            support_directory_id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let socket_path = directory.join("daemon.sock");
        let application = PapService::new(Arc::new(ProcessLocalPapRepository::default()));
        // The action runtime is not under test; a discarding finalizer keeps
        // the scan methods wired without writing into the query store.
        let outputs = Arc::new(DiscardingOutputs);
        let dispatcher = Arc::new(
            DaemonDispatcher::new(
                application,
                Arc::new(FixedRolePolicy(role)),
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
        wait_for_socket(&socket_path).await;
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

fn support_directory_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
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

async fn wait_for_socket(path: &Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(probe) = UnixStream::connect(path).await {
                drop(probe);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("daemon should accept connections on its socket");
}

/// Seeds the shared store with two owners' rows.
fn seed_two_owners(database: &Path) {
    let own_uid = rustix::process::getuid().as_raw();
    let writer = SqliteEventWriter::new(database).expect("writer");
    for (id, uid, category, verdict) in [
        ("own-1", own_uid, "exec", Some("deny")),
        ("own-2", own_uid, "network", None),
        ("foreign-1", FOREIGN_UID, "exec", Some("allow")),
    ] {
        let mut event = SecurityEvent::new("sandbox_prehook", category, Map::new());
        id.clone_into(&mut event.event_id);
        event.uid = uid;
        event.session_id = Some("session-shared".to_owned());
        if let Some(verdict) = verdict {
            event.details.insert("verdict".to_owned(), json!(verdict));
        }
        writer.write(&event);
    }
    writer.close_at(1000.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_peer_reads_only_its_own_events_through_the_socket() {
    let directory = tempfile::tempdir().expect("temp dir");
    let database = directory.path().join("security-events.db");
    seed_two_owners(&database);
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser, &database).await;

    let listed = request_json(
        &daemon.socket_path,
        &json!({"method": "sec.events.list", "params": {}}),
    )
    .await;
    let ids: Vec<&str> = listed["result"]["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| item["event_id"].as_str().expect("event id"))
        .collect();
    assert_eq!(ids.len(), 2, "only the caller's own rows: {listed}");
    assert!(ids.contains(&"own-1"));
    assert!(ids.contains(&"own-2"));
    assert!(
        !ids.contains(&"foreign-1"),
        "another owner's event must not be listed"
    );

    let summary = request_json(
        &daemon.socket_path,
        &json!({"method": "sec.summary", "params": {}}),
    )
    .await;
    assert_eq!(summary["result"]["total"], json!(2));
    assert_eq!(
        summary["result"]["by_category"],
        json!({"exec": 1, "network": 1}),
        "the foreign exec row must not leak into the aggregates"
    );

    let grouped = request_json(
        &daemon.socket_path,
        &json!({"method": "sec.events.count_by", "params": {"group_by": "verdict"}}),
    )
    .await;
    assert_eq!(
        grouped["result"]["items"],
        json!([{"value": "deny", "count": 1}]),
        "the foreign allow verdict must not leak into the groups"
    );

    let own = request_json(
        &daemon.socket_path,
        &json!({"method": "sec.events.get", "params": {"event_id": "own-1"}}),
    )
    .await;
    assert_eq!(own["result"]["found"], json!(true));
    assert_eq!(own["result"]["event"]["verdict"], json!("deny"));

    let foreign = request_json(
        &daemon.socket_path,
        &json!({"method": "sec.events.get", "params": {"event_id": "foreign-1"}}),
    )
    .await;
    let missing = request_json(
        &daemon.socket_path,
        &json!({"method": "sec.events.get", "params": {"event_id": "never-written"}}),
    )
    .await;
    assert_eq!(foreign["result"]["found"], json!(false));
    assert_eq!(missing["result"]["found"], json!(false));
    assert_eq!(foreign["result"]["event"], json!(null));
    assert_eq!(missing["result"]["event"], json!(null));

    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_policy_administrator_is_still_scoped_to_its_own_rows() {
    let directory = tempfile::tempdir().expect("temp dir");
    let database = directory.path().join("security-events.db");
    seed_two_owners(&database);
    let daemon = RunningDaemon::start(PrincipalRole::PolicyAdministrator, &database).await;

    let listed = request_json(
        &daemon.socket_path,
        &json!({"method": "sec.events.list", "params": {}}),
    )
    .await;
    assert_eq!(
        listed["result"]["total"],
        json!(2),
        "administration authority is not a cross-owner audit grant"
    );

    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_query_parameters_are_rejected_over_the_socket() {
    let directory = tempfile::tempdir().expect("temp dir");
    let database = directory.path().join("security-events.db");
    seed_two_owners(&database);
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser, &database).await;

    for (params, code) in [
        (json!({"limit": 0}), "invalid_argument"),
        (json!({"result": "exploded"}), "invalid_argument"),
        (json!({"since": "not a timestamp"}), "invalid_argument"),
        (json!({"event_id": "own-1"}), "invalid_argument"),
        (json!({"ownerUid": 0}), "invalid_request"),
        (json!({"limit": true}), "invalid_request"),
    ] {
        let response = request_json(
            &daemon.socket_path,
            &json!({"method": "sec.events.list", "params": params}),
        )
        .await;
        assert_eq!(
            response["error"]["code"],
            json!(code),
            "{params} must be rejected as {code}: {response}"
        );
    }

    // The v1 dashboard's count_by never paginates.
    let response = request_json(
        &daemon.socket_path,
        &json!({"method": "sec.events.count_by", "params": {"group_by": "category", "limit": 10}}),
    )
    .await;
    assert_eq!(response["error"]["code"], json!("invalid_argument"));

    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfigured_query_store_fails_closed() {
    let directory = tempfile::tempdir().expect("temp dir");
    let database = directory.path().join("security-events.db");
    let writer = SqliteEventWriter::new(&database).expect("writer");
    let mut event = SecurityEvent::new("sandbox_prehook", "exec", Map::new());
    "own-1".clone_into(&mut event.event_id);
    writer.write(&event);
    writer.close_at(1000.0);

    // A dispatcher assembled without with_security_queries must reject every
    // sec.* call rather than reading an arbitrary database.
    let socket_dir = tempfile::tempdir().expect("socket dir");
    let socket_path = socket_dir.path().join("daemon.sock");
    let application = PapService::new(Arc::new(ProcessLocalPapRepository::default()));
    let outputs = Arc::new(DiscardingOutputs);
    let dispatcher = Arc::new(DaemonDispatcher::new(
        application,
        Arc::new(FixedRolePolicy(PrincipalRole::LocalUser)),
        scan_application(
            Finalizer::new(outputs.clone(), outputs.clone(), outputs.clone()),
            Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
        ),
    ));
    let shutdown = asc_daemon_service::ShutdownToken::new();
    let service_shutdown = shutdown.clone();
    let mut config = BootstrapConfig::new(&socket_path);
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
    wait_for_socket(&socket_path).await;

    let response = request_json(
        &socket_path,
        &json!({"method": "sec.events.list", "params": {}}),
    )
    .await;
    assert_eq!(response["error"]["code"], json!("unavailable"));

    shutdown.request();
    task.await.unwrap();
}
