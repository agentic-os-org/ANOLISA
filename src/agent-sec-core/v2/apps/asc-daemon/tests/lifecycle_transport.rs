//! Transport timeout/disconnection must not bypass the shared terminal lifecycle.
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use asc_action_runtime::{
    ActionRuntime, AuditProjector, CapabilityExecutor, Diagnostic, DiagnosticSink,
    ExecutionControl, Finalizer, SecurityEventSink, TelemetrySink, TelemetryStatus, WarmupStatus,
};
use asc_action_types::{
    ActionId, ActionOutcome, AuditProjection, CodeScanRequest, PromptScanRequest,
    PromptScanWarmupRequest,
};
use asc_daemon::{BootstrapConfig, serve};
use asc_daemon_core::{ActionService, RootManagedPrincipalPolicy};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_daemon_service::ShutdownToken;
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_engine::PolicyTemplateCompiler;
use asc_security_events::SecurityEvent;
use asc_telemetry::TelemetryRecord;
use serde_json::{Map, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Notify, oneshot};

struct ControlledExecutor {
    entered: Mutex<Option<oneshot::Sender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl CapabilityExecutor for ControlledExecutor {
    type Request = CodeScanRequest;
    fn execute(&self, _: &ExecutionControl, _: &CodeScanRequest) -> ActionOutcome {
        self.entered
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        ActionOutcome {
            success: true,
            exit_code: 0,
            error: None,
            error_type: String::new(),
            data: json!({"ok":true,"verdict":"pass"})
                .as_object()
                .unwrap()
                .clone(),
        }
    }
}
struct Projector;
impl AuditProjector for Projector {
    type Request = CodeScanRequest;
    fn project(&self, _: &CodeScanRequest, outcome: &ActionOutcome) -> AuditProjection {
        AuditProjection::Completed {
            request: Map::new(),
            result: outcome.data.clone(),
            failure: None,
        }
    }
}
// The scenario drives `action.code_scan` only; the prompt-scan registration
// just has to satisfy the service's constructor.
struct UnusedPromptScan;
impl CapabilityExecutor for UnusedPromptScan {
    type Request = PromptScanRequest;
    fn execute(&self, _: &ExecutionControl, _: &PromptScanRequest) -> ActionOutcome {
        unreachable!("the lifecycle scenario never calls action.prompt_scan")
    }
}
struct UnusedPromptScanProjector;
impl AuditProjector for UnusedPromptScanProjector {
    type Request = PromptScanRequest;
    fn project(&self, _: &PromptScanRequest, outcome: &ActionOutcome) -> AuditProjection {
        AuditProjection::Completed {
            request: Map::new(),
            result: outcome.data.clone(),
            failure: None,
        }
    }
}
// The warmup registration likewise just satisfies the service's constructor.
struct UnusedWarmup;
impl asc_action_runtime::CapabilityWarmup for UnusedWarmup {
    type Request = PromptScanWarmupRequest;
    fn warmup(&self, _: &PromptScanWarmupRequest) -> WarmupStatus {
        unreachable!("the lifecycle scenario never calls action.prompt_scan.warmup")
    }
}
#[derive(Default)]
struct Outputs {
    audit: Mutex<Vec<SecurityEvent>>,
    telemetry: Mutex<Vec<TelemetryRecord>>,
    completed: Notify,
}
impl SecurityEventSink for Outputs {
    fn write(&self, event: &SecurityEvent) {
        self.audit.lock().unwrap().push(event.clone());
    }
}
impl TelemetrySink for Outputs {
    fn write(&self, record: &TelemetryRecord) -> TelemetryStatus {
        self.telemetry.lock().unwrap().push(record.clone());
        self.completed.notify_one();
        TelemetryStatus::Written
    }
}
impl DiagnosticSink for Outputs {
    fn record(&self, _: &Diagnostic) {}
}

#[derive(Clone, Copy)]
enum Scenario {
    Timeout,
    Disconnect,
    Shutdown,
}

async fn scenario(kind: Scenario) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let output = Arc::new(Outputs::default());
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let runtime = ActionRuntime::new(
        ActionId::CodeScan,
        ControlledExecutor {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
        },
        Projector,
        Finalizer::new(output.clone(), output.clone(), output.clone()),
    );
    let prompt_runtime = ActionRuntime::new(
        ActionId::PromptScan,
        UnusedPromptScan,
        UnusedPromptScanProjector,
        Finalizer::new(output.clone(), output.clone(), output.clone()),
    );
    let dispatcher = Arc::new(DaemonDispatcher::new(
        PapService::new(
            Arc::new(ProcessLocalPapRepository::default()),
            Arc::new(PolicyTemplateCompiler),
        ),
        Arc::new(RootManagedPrincipalPolicy::default()),
        Arc::new(ActionService::new(runtime, prompt_runtime, UnusedWarmup)),
    ));
    let shutdown = ShutdownToken::new();
    let service_shutdown = shutdown.clone();
    let mut config = BootstrapConfig::new(&socket);
    config.service.dispatch_timeout = if matches!(kind, Scenario::Timeout) {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(5)
    };
    let service = tokio::spawn(serve(
        config,
        dispatcher,
        Arc::new(JsonRejectionEncoder),
        service_shutdown,
    ));
    let mut stream = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(stream) = UnixStream::connect(&socket).await {
                break stream;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stream.write_all(b"{\"method\":\"action.code_scan\",\"params\":{\"code\":\"echo hi\",\"language\":\"bash\"}}\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(output.audit.lock().unwrap().is_empty());
    match kind {
        Scenario::Timeout => {
            let mut line = String::new();
            tokio::time::timeout(
                Duration::from_secs(5),
                BufReader::new(stream).read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            let response: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(response["error"]["code"], "deadline_exceeded");
        }
        Scenario::Disconnect => drop(stream),
        Scenario::Shutdown => {
            shutdown.request();
            assert!(!service.is_finished());
            drop(stream);
        }
    }
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), output.completed.notified())
        .await
        .unwrap();
    shutdown.request();
    tokio::time::timeout(Duration::from_secs(5), service)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(output.audit.lock().unwrap().len(), 1);
    assert_eq!(output.telemetry.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_does_not_suppress_eventual_finalization() {
    scenario(Scenario::Timeout).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnected_caller_does_not_suppress_finalization() {
    scenario(Scenario::Disconnect).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_drain_allows_inflight_finalization() {
    scenario(Scenario::Shutdown).await;
}
