//! End-to-end `action.code_scan` over a real Unix socket.
//!
//! Proves the wiring the unit tests cannot: that the method is registered, that
//! a non-administrator local peer is authorized to call it, and that the
//! capability's `ScanResult` reaches the caller as the method result. PAP is
//! reused only to satisfy the dispatcher's application port; these tests never
//! administer policy.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use asc_action_runtime::{
    Diagnostic, DiagnosticSink, Finalizer, SecurityEventSink, TelemetrySink, TelemetryStatus,
};
use asc_security_events::SecurityEvent;
use asc_telemetry::TelemetryRecord;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use asc_daemon::{BootstrapConfig, serve};
use asc_daemon_core::{PeerCredentials, PrincipalPolicy, PrincipalRole};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_engine::PolicyTemplateCompiler;
use serde_json::{Value, json};
use tokio::net::UnixStream;

mod support;

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

/// A principal policy that assigns one fixed role to every peer.
#[derive(Clone, Copy)]
struct FixedRolePolicy(PrincipalRole);

impl PrincipalPolicy for FixedRolePolicy {
    fn role_for(&self, _peer: PeerCredentials) -> PrincipalRole {
        self.0
    }
}

#[derive(Default)]
struct Outputs {
    audit: Mutex<Vec<SecurityEvent>>,
    telemetry: Mutex<Vec<TelemetryRecord>>,
}
impl SecurityEventSink for Outputs {
    fn write(&self, event: &SecurityEvent) {
        self.audit.lock().unwrap().push(event.clone());
    }
}
impl TelemetrySink for Outputs {
    fn write(&self, record: &TelemetryRecord) -> TelemetryStatus {
        self.telemetry.lock().unwrap().push(record.clone());
        TelemetryStatus::Written
    }
}
impl DiagnosticSink for Outputs {
    fn record(&self, _: &Diagnostic) {}
}

struct RunningDaemon {
    output: Arc<Outputs>,
    directory: PathBuf,
    socket_path: PathBuf,
    shutdown: asc_daemon_service::ShutdownToken,
    task: tokio::task::JoinHandle<()>,
}

impl RunningDaemon {
    async fn start(role: PrincipalRole) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "asc-daemon-code-scan-{}-{}",
            std::process::id(),
            DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let socket_path = directory.join("daemon.sock");
        let application = PapService::new(
            Arc::new(ProcessLocalPapRepository::default()),
            Arc::new(PolicyTemplateCompiler),
        );
        let output = Arc::new(Outputs::default());
        let dispatcher = Arc::new(DaemonDispatcher::new(
            application,
            Arc::new(FixedRolePolicy(role)),
            asc_daemon::scan_application(
                Finalizer::new(output.clone(), output.clone(), output.clone()),
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
        Self {
            output,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pii_scan_finalizes_safe_outputs_with_peer_identity_and_business_trace() {
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser).await;
    let trace = json!({"traceId":" trace ","sessionId":"session","agentName":" codex ","uid":424_242,"pid":424_242});
    let response = support::request_json(
        &daemon.socket_path,
        &json!({"method": "action.pii_scan", "params": {
            "text": "PRIVATE_INPUT alice@company.cn", "rawEvidence":true,
            "redactOutput":true,"traceContext":trace
        }}),
    )
    .await;
    assert_eq!(response["result"]["verdict"], "warn", "{response}");
    assert!(
        response["result"]["redacted_text"]
            .as_str()
            .unwrap()
            .contains("PRIVATE_INPUT")
    );
    for (extra, error) in [
        (json!({"unknown":"PRIVATE_PARAMETER"}), "invalid_request"),
        (json!({"source":"PRIVATE_SOURCE"}), "invalid_argument"),
        (json!({"maxBytes":0}), "invalid_argument"),
    ] {
        let mut params = json!({"text":"PRIVATE_INPUT","traceContext":trace});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let response = support::request_json(
            &daemon.socket_path,
            &json!({"method":"action.pii_scan","params":params}),
        )
        .await;
        assert_eq!(response["error"]["code"], error, "{response}");
        assert!(!response.to_string().contains("PRIVATE"));
    }
    for (method, error) in [
        ("policy.templates.list", "permission_denied"),
        ("action.unknown", "unknown_method"),
    ] {
        let response =
            support::request_json(&daemon.socket_path, &json!({"method":method,"params":{}})).await;
        assert_eq!(response["error"]["code"], error);
    }
    {
        let audit = daemon.output.audit.lock().unwrap();
        let telemetry = daemon.output.telemetry.lock().unwrap();
        assert_eq!(audit.len(), 4);
        assert_eq!(telemetry.len(), 4);
        for (index, event) in audit.iter().enumerate() {
            assert_eq!(event.uid, rustix::process::getuid().as_raw());
            assert_eq!(event.pid, std::process::id());
            assert_eq!(event.trace_id, "trace");
            assert_eq!(event.session_id.as_deref(), Some("session"));
            let serialized = serde_json::to_string(event).unwrap();
            assert!(!serialized.contains("PRIVATE"));
            assert!(!serialized.contains("alice@company.cn"));
            if index > 0 {
                assert_eq!(event.details["request"], json!({}));
                assert_eq!(event.details["error_type"], "invalid_parameters");
            }
            let scalar = serde_json::to_value(&telemetry[index]).unwrap();
            assert_eq!(scalar["component.agent_name"], "codex");
            assert_eq!(scalar["seccore.event_type"], "pii_scan");
            assert_eq!(
                scalar["seccore.result"],
                if index == 0 { "succeeded" } else { "failed" }
            );
            if index == 0 {
                assert_eq!(scalar["seccore.verdict"], "warn");
                assert!(scalar["seccore.elapsed_ms"].is_number());
            } else {
                assert!(scalar.get("seccore.verdict").is_none());
                assert_eq!(scalar["seccore.error_type"], "invalid_parameters");
            }
            for forbidden in [
                "PRIVATE",
                "alice@company.cn",
                "findings",
                "redacted_text",
                "raw_evidence",
                "session",
                "trace",
            ] {
                assert!(!scalar.to_string().contains(forbidden));
            }
        }
    }
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_user_scans_dangerous_bash_and_receives_the_verdict() {
    // The role is LocalUser, not PolicyAdministrator: reaching a scan result at
    // all is what proves the method's LocalUser access policy, since every PAP
    // method denies this same peer.
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser).await;
    let response = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "action.code_scan",
            "params": {"code": "rm -rf /tmp/test", "language": "bash"}
        }),
    )
    .await;
    let result = &response["result"];
    assert_eq!(result["ok"], json!(true), "unexpected response {response}");
    assert_eq!(result["verdict"], json!("warn"));
    assert_eq!(result["language"], json!("bash"));
    assert!(
        result["findings"]
            .as_array()
            .is_some_and(|findings| !findings.is_empty()),
        "expected at least one finding: {response}"
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dotted_action_alias_is_not_registered() {
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser).await;
    let response = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "action.code.scan",
            "params": {"code": "echo hello", "language": "bash"}
        }),
    )
    .await;
    assert_eq!(
        response["error"]["code"],
        json!("unknown_method"),
        "{response}"
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_python_in_bash_is_reported_as_python() {
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser).await;
    let response = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "action.code_scan",
            "params": {
                "code": r#"python3 -c "import os; os.system('rm -rf /')""#,
                "language": "bash"
            }
        }),
    )
    .await;
    assert_eq!(
        response["result"]["language"],
        json!("python"),
        "{response}"
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsupported_language_is_rejected_as_invalid_argument() {
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser).await;
    let response = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "action.code_scan",
            "params": {"code": "puts 1", "language": "ruby"}
        }),
    )
    .await;
    assert_eq!(
        response["error"]["code"],
        json!("invalid_argument"),
        "{response}"
    );
    assert!(response.get("result").is_none());
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_parameters_are_rejected_as_invalid_request() {
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser).await;
    let response = support::request_json(
        &daemon.socket_path,
        &json!({"method": "action.code_scan", "params": {"language": "bash"}}),
    )
    .await;
    assert_eq!(
        response["error"]["code"],
        json!("invalid_request"),
        "{response}"
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_result_carries_the_full_v1_field_contract() {
    let daemon = RunningDaemon::start(PrincipalRole::LocalUser).await;
    let response = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "action.code_scan",
            "params": {"code": "echo hello", "language": "bash"}
        }),
    )
    .await;
    let Value::Object(result) = response["result"].clone() else {
        panic!("scan result is not an object: {response}");
    };
    // The parsed value re-sorts keys, so this asserts the field *set*, not its
    // order; the wire byte order is pinned by the capability crate's own
    // serialization test. Every V1 field must be present and named identically.
    let mut keys: Vec<&str> = result.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "elapsed_ms",
            "engine_version",
            "findings",
            "language",
            "ok",
            "summary",
            "verdict",
        ],
        "scan result fields diverged from the V1 contract"
    );
    assert_ne!(result["engine_version"], json!("unknown"));
    assert!(
        result["engine_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    );
    daemon.stop().await;
}
