//! SkillSec uses the process outputs without coupling their failures to business execution.
use asc_action_runtime::{
    Diagnostic, DiagnosticSink, ExecutionControl, Finalizer, SecurityEventSink, TelemetrySink,
    TelemetryStatus,
};
use asc_action_types::{SkillIdentity, SkillSecCommand};
use asc_capability_skill_sec::{
    SkillSecConfig, SkillSecService, executor::SkillSecExecutor, scanner::ScannerRegistry,
};
use asc_daemon_core::PeerCredentials;
use asc_security_events::SecurityEvent;
use asc_telemetry::TelemetryRecord;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Outputs {
    failure: &'static str,
    audit: AtomicUsize,
    telemetry: AtomicUsize,
    completed: AtomicUsize,
    events: Mutex<Vec<SecurityEvent>>,
    records: Mutex<Vec<TelemetryRecord>>,
}

impl SecurityEventSink for Outputs {
    fn write(&self, event: &SecurityEvent) {
        self.audit.fetch_add(1, Ordering::SeqCst);
        assert_ne!(self.failure, "audit", "synthetic sink failure");
        self.events.lock().unwrap().push(event.clone());
    }
}
impl TelemetrySink for Outputs {
    fn write(&self, record: &TelemetryRecord) -> TelemetryStatus {
        self.telemetry.fetch_add(1, Ordering::SeqCst);
        self.records.lock().unwrap().push(record.clone());
        assert_ne!(self.failure, "telemetry", "synthetic sink failure");
        TelemetryStatus::Written
    }
}
impl DiagnosticSink for Outputs {
    fn record(&self, event: &Diagnostic) {
        if matches!(event, Diagnostic::Completed { .. }) {
            self.completed.fetch_add(1, Ordering::SeqCst);
        }
        assert_ne!(self.failure, "diagnostics", "synthetic sink failure");
    }
}

#[test]
fn analyze_audits_once_without_ledger_writes_despite_independent_output_failures() {
    for failure in ["none", "audit", "telemetry", "diagnostics"] {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        let skill = temporary.path().join("skill");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: test\ndescription: safe\n---\nPRIVATE_SKILL_BODY",
        )
        .unwrap();
        let service = Arc::new(
            SkillSecService::new(
                SkillSecConfig {
                    state_dir: state.clone(),
                    managed_skill_dirs: Vec::new(),
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        let outputs = Arc::new(Outputs {
            failure,
            audit: AtomicUsize::new(0),
            telemetry: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            events: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
        });
        let application = asc_daemon::skill_application(
            Finalizer::new(outputs.clone(), outputs.clone(), outputs.clone()),
            SkillSecExecutor::new(service),
        );
        let outcome = application
            .skill_sec(
                PeerCredentials::new(1001, 1002, 1003),
                &ExecutionControl {
                    deadline: Instant::now() + Duration::from_secs(10),
                    cancelled: false,
                },
                SkillSecCommand::Analyze {
                    skill_dir: SkillIdentity::new(&skill).unwrap(),
                },
            )
            .unwrap();
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outputs.audit.load(Ordering::SeqCst), 1);
        assert_eq!(outputs.telemetry.load(Ordering::SeqCst), 1);
        assert_eq!(outputs.completed.load(Ordering::SeqCst), 1);
        let records = outputs.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            !serde_json::to_string(&records[0])
                .unwrap()
                .contains("PRIVATE_SKILL_BODY")
        );
        assert!(!skill.join(".skill-meta").exists());
        assert!(!state.join("signing-key.pk8").exists());
        if failure != "audit" {
            let events = outputs.events.lock().unwrap();
            assert_eq!(events[0].uid, 1001);
            assert_eq!(events[0].pid, 1003);
            assert_eq!(events[0].details["request"]["command"], "analyze");
            assert_eq!(events[0].details["request"]["skillCount"], 1);
            assert!(
                !serde_json::to_string(&events[0])
                    .unwrap()
                    .contains("PRIVATE_SKILL_BODY")
            );
        }
    }
}

#[test]
fn rpc_context_reaches_skill_audit_without_leaking_into_telemetry_or_the_next_request() {
    use opentelemetry::trace::TracerProvider as _;
    use serde_json::json;
    use tracing_subscriber::layer::SubscriberExt as _;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_sampler(opentelemetry_sdk::trace::Sampler::AlwaysOn)
        .build();
    let subscriber = tracing_subscriber::registry().with(
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("skillsec-rpc-test"))
            .with_context_activation(true),
    );
    let state = tempfile::tempdir().unwrap();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let service = Arc::new(
        SkillSecService::new(
            SkillSecConfig {
                state_dir: state.path().into(),
                managed_skill_dirs: Vec::new(),
            },
            ScannerRegistry::default(),
        )
        .unwrap(),
    );
    let outputs = Arc::new(Outputs::default());
    let dispatcher = asc_daemon_handler::DaemonDispatcher::new(
        asc_pap::PapService::new(
            Arc::new(asc_pap_repository_memory::ProcessLocalPapRepository::default()),
            Arc::new(asc_policy_engine::PolicyTemplateCompiler),
        ),
        Arc::new(asc_daemon_core::RootManagedPrincipalPolicy::default()),
        asc_daemon::skill_application(
            Finalizer::new(outputs.clone(), outputs.clone(), outputs.clone()),
            SkillSecExecutor::new(service),
        ),
    );
    tracing::subscriber::with_default(subscriber, || {
        for (index, agent) in [Some("codex"), None, Some("PRIVATE_UNAPPROVED_AGENT")]
            .into_iter()
            .enumerate()
        {
            let mut request = json!({
                "method": "action.skill_sec", "params": {"command": "list-scanners"}
            });
            if let Some(agent) = agent {
                request["traceContext"] = json!({
                    "version": 1,
                    "traceparent": "00-11111111111111111111111111111111-2222222222222222-01",
                    "baggage": format!("agentsec.agent.name={agent},agentsec.session.id=PRIVATE_SESSION,unknown=PRIVATE_UNKNOWN")
                });
                request["compatibility"] = json!({"version": 1, "traceId": " PRIVATE_COMPAT "});
            }
            let response = dispatcher.handle_with_control(
                asc_daemon_protocol::RequestId::new(format!("skillsec-{index}")).unwrap(),
                PeerCredentials::new(1001, 1002, 1003),
                &asc_daemon_service::DispatchControl::new(Instant::now() + Duration::from_secs(10)),
                serde_json::from_value(request).unwrap(),
            );
            assert_eq!(
                serde_json::to_value(response).unwrap()["result"]["success"],
                true
            );
        }
    });

    assert_eq!(outputs.audit.load(Ordering::SeqCst), 3);
    assert_eq!(outputs.telemetry.load(Ordering::SeqCst), 3);
    assert_eq!(outputs.completed.load(Ordering::SeqCst), 3);
    let events = outputs.events.lock().unwrap();
    let records = outputs.records.lock().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(records.len(), 3);
    for (index, (event, record)) in events.iter().zip(records.iter()).enumerate() {
        assert_eq!((event.uid, event.pid), (1001, 1003));
        assert_eq!(event.details["request"]["command"], "list-scanners");
        assert_eq!(
            event.trace_id,
            if index == 1 { "" } else { "PRIVATE_COMPAT" }
        );
        assert_eq!(
            event.session_id.as_deref(),
            (index != 1).then_some("PRIVATE_SESSION")
        );
        let telemetry = serde_json::to_value(record).unwrap();
        assert_eq!(
            telemetry["component.agent_name"],
            if index == 0 { "codex" } else { "" }
        );
        assert!(!telemetry.to_string().contains("PRIVATE_"));
        assert!(
            !serde_json::to_string(event)
                .unwrap()
                .contains("PRIVATE_UNKNOWN")
        );
    }
}
