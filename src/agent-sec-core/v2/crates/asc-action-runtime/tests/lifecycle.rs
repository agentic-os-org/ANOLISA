//! Common lifecycle conformance using the implemented code-scan identity.
use asc_action_runtime::*;
use asc_action_types::*;
use asc_observability::{Context, bind_trace_context_input};
use asc_security_events::SecurityEvent;
use asc_telemetry::TelemetryRecord;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Outputs {
    audit: Mutex<Vec<SecurityEvent>>,
    telemetry: Mutex<Vec<Value>>,
    diagnostics: Mutex<Vec<Diagnostic>>,
    fail_audit: bool,
    fail_telemetry: bool,
    fail_diagnostics: bool,
}
impl SecurityEventSink for Outputs {
    fn write(&self, event: &SecurityEvent) {
        self.audit.lock().unwrap().push(event.clone());
        assert!(!self.fail_audit, "SECRET_SINK");
    }
}
impl TelemetrySink for Outputs {
    fn write(&self, record: &TelemetryRecord) -> TelemetryStatus {
        self.telemetry
            .lock()
            .unwrap()
            .push(serde_json::to_value(record).unwrap());
        assert!(!self.fail_telemetry, "SECRET_TELEMETRY");
        TelemetryStatus::Written
    }
}
impl DiagnosticSink for Outputs {
    fn record(&self, diagnostic: &Diagnostic) {
        self.diagnostics.lock().unwrap().push(*diagnostic);
        assert!(!self.fail_diagnostics, "SECRET_DIAGNOSTIC");
    }
}
fn finalizer(output: &Arc<Outputs>) -> Finalizer {
    Finalizer::new(output.clone(), output.clone(), output.clone())
}
fn caller() -> CallerIdentity {
    CallerIdentity {
        uid: 1001,
        gid: 1002,
        pid: 1003,
    }
}
fn control() -> ExecutionControl {
    // An expired transport/cancellation snapshot must not suppress finalization.
    ExecutionControl {
        deadline: Instant::now(),
        cancelled: true,
    }
}
fn outcome() -> ActionOutcome {
    ActionOutcome {
        success: true,
        exit_code: 0,
        error: None,
        error_type: String::new(),
        data: json!({"verdict":"deny", "elapsed_ms":7, "raw_evidence":"SECRET_RESULT"})
            .as_object()
            .unwrap()
            .clone(),
    }
}
struct CodeExecutor;
impl CapabilityExecutor for CodeExecutor {
    type Request = ActionOutcome;
    fn execute(&self, _: &ExecutionControl, request: &ActionOutcome) -> ActionOutcome {
        request.clone()
    }
}
struct Projector;
impl AuditProjector for Projector {
    type Request = ActionOutcome;
    fn project(&self, _: &ActionOutcome, result: &ActionOutcome) -> AuditProjection {
        AuditProjection::Completed {
            request: json!({"safe_length":12}).as_object().unwrap().clone(),
            result: result.data.clone(),
            failure: (!result.success).then(|| Failure {
                error: result.error.clone(),
                error_type: result.error_type.clone(),
                exit_code: result.exit_code,
            }),
        }
    }
}

#[test]
fn smc_004_005_012_013_code_scan_matches_v1_goldens() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/v2/fixtures/scan-lifecycle-v1.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let mut input = case["audit"].clone();
        input["agent_name"] = case["agent"].clone();
        input["unknown"] = json!("SECRET_UNKNOWN");
        let context = bind_trace_context_input(&Context::new(), &input).unwrap();
        let _guard = context.attach();
        let outputs = Arc::new(Outputs::default());
        let finalizer = finalizer(&outputs);
        assert_eq!(case["action"], "code_scan");
        let runtime = ActionRuntime::new(ActionId::CodeScan, CodeExecutor, Projector, finalizer);
        let expected = ActionOutcome {
            success: case["success"].as_bool().unwrap(),
            exit_code: case["exit_code"].as_i64().unwrap(),
            error: case["error"].as_str().map(str::to_owned),
            error_type: case["error_type"].as_str().unwrap().into(),
            data: case["data"].as_object().unwrap().clone(),
        };
        assert_eq!(
            runtime.invoke(&control(), &caller(), &expected).unwrap(),
            expected
        );
        let events = outputs.audit.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].uid, events[0].pid), (1001, 1003));
        let mut event = serde_json::to_value(&events[0]).unwrap();
        for field in ["event_id", "timestamp", "pid", "uid"] {
            event.as_object_mut().unwrap().remove(field);
        }
        assert_eq!(event, case["audit"]);
        let records = outputs.telemetry.lock().unwrap();
        assert_eq!(records.len(), 1);
        let mut telemetry = records[0].clone();
        assert_eq!(telemetry["seccore.timestamp"], events[0].timestamp);
        telemetry["component.version"] = json!("<version>");
        telemetry["seccore.timestamp"] = json!("<timestamp>");
        assert_eq!(telemetry, case["telemetry"]);
        assert!(!telemetry.to_string().contains("SECRET"));
        assert!(matches!(
            outputs.diagnostics.lock().unwrap().last(),
            Some(Diagnostic::Completed { .. })
        ));
    }
}

#[test]
fn smc_007_failures_of_either_sink_or_diagnostics_preserve_outcomes_and_other_attempts() {
    for (audit, telemetry, diagnostic) in [
        (true, false, false),
        (false, true, false),
        (true, true, true),
    ] {
        let output = Arc::new(Outputs {
            fail_audit: audit,
            fail_telemetry: telemetry,
            fail_diagnostics: diagnostic,
            ..Outputs::default()
        });
        let runtime = ActionRuntime::new(
            ActionId::CodeScan,
            CodeExecutor,
            Projector,
            finalizer(&output),
        );
        let expected = outcome();
        assert_eq!(
            runtime.invoke(&control(), &caller(), &expected).unwrap(),
            expected
        );
        assert_eq!(output.audit.lock().unwrap().len(), 1);
        assert_eq!(output.telemetry.lock().unwrap().len(), 1);
    }
}

struct BrokenProjector;
impl AuditProjector for BrokenProjector {
    type Request = ActionOutcome;
    fn project(&self, _: &ActionOutcome, _: &ActionOutcome) -> AuditProjection {
        panic!("SECRET_PROJECTOR")
    }
}
#[test]
fn projection_failure_emits_minimal_audit_and_still_projects_telemetry_from_outcome() {
    let output = Arc::new(Outputs::default());
    let runtime = ActionRuntime::new(
        ActionId::CodeScan,
        CodeExecutor,
        BrokenProjector,
        finalizer(&output),
    );
    let expected = outcome();
    assert_eq!(
        runtime.invoke(&control(), &caller(), &expected).unwrap(),
        expected
    );
    let events = output.audit.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].details["error_type"], "AuditProjectionError");
    assert_eq!(
        events[0].result,
        asc_security_events::EventResult::Succeeded
    );
    assert!(
        !serde_json::to_string(&events[0])
            .unwrap()
            .contains("SECRET")
    );
    assert_eq!(
        output.telemetry.lock().unwrap()[0]["seccore.verdict"],
        "deny"
    );
}

struct BrokenExecutor;
impl CapabilityExecutor for BrokenExecutor {
    type Request = ActionOutcome;
    fn execute(&self, _: &ExecutionControl, _: &ActionOutcome) -> ActionOutcome {
        panic!("SECRET_EXECUTOR")
    }
}
#[test]
fn smc_006_014_unhandled_execution_failure_is_finalized_once_and_safely_returned() {
    let output = Arc::new(Outputs::default());
    let runtime = ActionRuntime::new(
        ActionId::CodeScan,
        BrokenExecutor,
        Projector,
        finalizer(&output),
    );
    assert_eq!(
        runtime.invoke(&control(), &caller(), &outcome()),
        Err(InvokeError)
    );
    let events = output.audit.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].result, asc_security_events::EventResult::Failed);
    assert_eq!(events[0].details["error_type"], "InternalExecutionError");
    assert!(
        !serde_json::to_string(&events[0])
            .unwrap()
            .contains("SECRET")
    );
    let records = output.telemetry.lock().unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].get("seccore.exit_code").is_none());
}

struct GatedAudit {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    elapsed: Arc<Mutex<Duration>>,
}
impl SecurityEventSink for GatedAudit {
    fn write(&self, _: &SecurityEvent) {
        let started = Instant::now();
        self.entered.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        *self.elapsed.lock().unwrap() = started.elapsed();
    }
}
#[test]
fn synchronous_finalization_precedes_return_and_duration_includes_output_attempts() {
    let output = Arc::new(Outputs::default());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let sink_elapsed = Arc::new(Mutex::new(Duration::ZERO));
    let finalizer = Finalizer::new(
        Arc::new(GatedAudit {
            entered: entered_tx,
            release: Mutex::new(release_rx),
            elapsed: sink_elapsed.clone(),
        }),
        output.clone(),
        output.clone(),
    );
    let runtime = ActionRuntime::new(ActionId::CodeScan, CodeExecutor, Projector, finalizer);
    let worker = std::thread::spawn(move || {
        result_tx
            .send(runtime.invoke(&control(), &caller(), &outcome()))
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        result_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(output.telemetry.lock().unwrap().is_empty());
    release_tx.send(()).unwrap();
    result_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    let diagnostics = output.diagnostics.lock().unwrap();
    let Diagnostic::Completed { duration, .. } = diagnostics.last().unwrap() else {
        panic!("missing completion")
    };
    assert!(*duration > Duration::ZERO);
    assert!(*duration >= *sink_elapsed.lock().unwrap());
    assert_eq!(output.telemetry.lock().unwrap().len(), 1);
}

struct DisabledTelemetry;
impl TelemetrySink for DisabledTelemetry {
    fn enabled(&self) -> bool {
        false
    }
    fn write(&self, _: &TelemetryRecord) -> TelemetryStatus {
        panic!("disabled telemetry was called")
    }
}
#[test]
fn disabled_telemetry_is_skipped_by_lifecycle_without_suppressing_audit() {
    let output = Arc::new(Outputs::default());
    let runtime = ActionRuntime::new(
        ActionId::CodeScan,
        CodeExecutor,
        Projector,
        Finalizer::new(output.clone(), Arc::new(DisabledTelemetry), output.clone()),
    );
    let expected = outcome();
    assert_eq!(
        runtime.invoke(&control(), &caller(), &expected).unwrap(),
        expected
    );
    assert_eq!(output.audit.lock().unwrap().len(), 1);
    assert!(
        output
            .diagnostics
            .lock()
            .unwrap()
            .contains(&Diagnostic::Telemetry {
                action: ActionId::CodeScan,
                status: TelemetryStatus::Skipped,
            })
    );
}
