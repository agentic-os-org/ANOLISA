use asc_daemon::{BootstrapConfig, serve};
use asc_daemon_core::{PeerCredentials, PrincipalPolicy, PrincipalRole};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_observability::AgentFieldProcessor;
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_engine::PolicyTemplateCompiler;
use opentelemetry::trace::{Status, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider, SpanData};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    net::UnixStream,
};
use tracing_subscriber::layer::SubscriberExt as _;

const WATCHDOG: Duration = Duration::from_secs(10);
const CANARY: &str = "DO_NOT_EXPORT_SECRET_PAYLOAD";

struct ControlledPolicy {
    mode: AtomicUsize,
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl PrincipalPolicy for ControlledPolicy {
    fn role_for(&self, _: PeerCredentials) -> PrincipalRole {
        match self.mode.swap(0, Ordering::SeqCst) {
            1 => panic!("{CANARY}"),
            2 => return PrincipalRole::LocalUser,
            3 => {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv_timeout(WATCHDOG).unwrap();
            }
            _ => {}
        }
        PrincipalRole::PolicyAdministrator
    }
}

async fn read(stream: UnixStream) -> Value {
    let mut bytes = Vec::new();
    tokio::time::timeout(
        WATCHDOG,
        BufReader::new(stream).read_until(b'\n', &mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
async fn call(path: &Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(path).await.unwrap();
    let mut bytes = serde_json::to_vec(&request).unwrap();
    bytes.push(b'\n');
    stream.write_all(&bytes).await.unwrap();
    read(stream).await
}

async fn wait_for_socket(path: &Path) {
    tokio::time::timeout(WATCHDOG, async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn has_error(span: &SpanData, category: &str) -> bool {
    span.status == Status::error(category.to_owned())
        && span
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "error.type" && kv.value.to_string() == category)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uds_failures_preserve_business_and_context() {
    let all = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .with_span_processor(AgentFieldProcessor)
        .with_simple_exporter(all.clone())
        .build();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer("test"))
                .with_context_activation(true)
                .with_error_events_to_status(false),
        ),
    )
    .unwrap();
    let (entered, started) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let policy = Arc::new(ControlledPolicy {
        mode: AtomicUsize::new(0),
        entered,
        release: Mutex::new(wait),
    });
    let dispatcher = Arc::new(DaemonDispatcher::new(
        PapService::new(
            Arc::new(ProcessLocalPapRepository::default()),
            Arc::new(PolicyTemplateCompiler),
        ),
        policy.clone(),
        asc_daemon::scan_application(asc_action_runtime::testing::discarding_finalizer()),
    ));
    let directory =
        std::env::temp_dir().join(format!("asc-otel-failures-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let path = directory.join("daemon.sock");
    let mut config = BootstrapConfig::new(&path);
    config.service.max_connections = 1;
    config.service.request_read_timeout = Duration::from_millis(100);
    config.service.dispatch_timeout = WATCHDOG;
    let shutdown = asc_daemon_service::ShutdownToken::new();
    let stopping = shutdown.clone();
    let task = tokio::spawn(async move {
        serve(config, dispatcher, Arc::new(JsonRejectionEncoder), stopping)
            .await
            .unwrap();
    });
    wait_for_socket(&path).await;
    let list = json!({"method":"policy.templates.list"});
    assert!(call(&path, list.clone()).await.get("result").is_some());
    for index in 0..12 {
        let created = call(
            &path,
            json!({"method":"policy.templates.create", "params": {
                "policyName": format!("policy-{index}"),
                "template":{"kind":"prevent_file_deletion", "files":["/protected"]}
            }}),
        )
        .await;
        assert!(created.get("result").is_some(), "{created}");
    }
    assert_eq!(
        call(&path, list.clone()).await["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
    let clean = exercise_failures(&path, &policy, &started, &release).await;
    shutdown.request();
    task.await.unwrap();
    provider.force_flush().unwrap();
    let spans = all.get_finished_spans().unwrap();
    check_failure_spans(&spans, &clean);
    check_compiler_parentage(&spans);
    std::fs::remove_dir_all(directory).unwrap();
}

async fn exercise_failures(
    path: &Path,
    policy: &ControlledPolicy,
    started: &mpsc::Receiver<()>,
    release: &mpsc::Sender<()>,
) -> Value {
    let list = json!({"method":"policy.templates.list"});

    for (request, code) in [
        (
            json!({"method":CANARY, "params":{"secret":CANARY}}),
            "unknown_method",
        ),
        (
            json!({"method":"policy.templates.list", "traceContext":{"version":2}}),
            "invalid_request",
        ),
        (
            json!({"method":"policy.templates.create", "params":{"policyName":"bad", "template":{"kind":"prevent_file_deletion", "files":[]}}}),
            "invalid_argument",
        ),
    ] {
        assert_eq!(call(path, request).await["error"]["code"], code);
    }
    policy.mode.store(2, Ordering::SeqCst);
    assert_eq!(
        call(path, list.clone()).await["error"]["code"],
        "permission_denied"
    );
    policy.mode.store(1, Ordering::SeqCst);
    assert_eq!(call(path, list.clone()).await["error"]["code"], "internal");
    // A real handler unwind is followed by a clean new request.
    let clean = call(path, list.clone()).await;
    assert_eq!(clean["result"]["items"].as_array().unwrap().len(), 12);

    policy.mode.store(3, Ordering::SeqCst);
    let request_path = path.to_owned();
    let pending = tokio::spawn(async move {
        call(&request_path, json!({"method":"policy.templates.list"})).await
    });
    started.recv_timeout(WATCHDOG).unwrap();
    assert_eq!(
        call(path, list).await["error"]["code"],
        "resource_exhausted"
    );
    release.send(()).unwrap();
    assert!(pending.await.unwrap().get("result").is_some());
    assert_eq!(
        read(UnixStream::connect(path).await.unwrap()).await["error"]["code"],
        "deadline_exceeded"
    );
    clean
}

fn check_failure_spans(spans: &[SpanData], clean: &Value) {
    for (name, code) in [
        ("daemon.request", "unknown_method"),
        ("daemon.reject", "invalid_request"),
        ("daemon.request", "invalid_argument"),
        ("daemon.request", "permission_denied"),
        ("daemon.request", "request_panicked"),
        ("daemon.reject", "internal"),
        ("daemon.reject", "resource_exhausted"),
        ("daemon.reject", "deadline_exceeded"),
    ] {
        assert!(
            spans.iter().any(|s| s.name == name && has_error(s, code)),
            "missing {name}/{code}"
        );
    }
    for span in spans {
        assert!(!format!("{span:?}").contains(CANARY));
        if span.name == "daemon.reject" {
            assert_eq!(span.parent_span_id, opentelemetry::trace::SpanId::INVALID);
            assert!(
                span.attributes
                    .iter()
                    .all(|kv| !kv.key.as_str().starts_with("agentsec.session"))
            );
        }
    }
    let failed = spans
        .iter()
        .find(|s| has_error(s, "request_panicked"))
        .unwrap();
    let clean_span = spans
        .iter()
        .find(|s| {
            s.attributes.iter().any(|kv| {
                kv.key.as_str() == "rpc.request_id"
                    && kv.value.to_string() == clean["requestId"].as_str().unwrap()
            })
        })
        .unwrap();
    assert_ne!(
        failed.span_context.trace_id(),
        clean_span.span_context.trace_id()
    );
    assert_ne!(clean_span.status, Status::error("request_panicked"));
}

// Internal span inspection, independent of the production local-only runtime.
fn check_compiler_parentage(spans: &[SpanData]) {
    let compiled: Vec<_> = spans
        .iter()
        .filter(|span| span.name == "policy.compile")
        .collect();
    assert_eq!(compiled.len(), 13); // Twelve valid creates and one invalid template.
    let mut failures = 0;
    for child in compiled {
        let parent = spans
            .iter()
            .find(|span| span.span_context.span_id() == child.parent_span_id)
            .unwrap();
        assert_eq!(parent.name, "pap.create_policy");
        assert_eq!(
            parent.span_context.trace_id(),
            child.span_context.trace_id()
        );
        let request = spans
            .iter()
            .find(|span| span.span_context.span_id() == parent.parent_span_id)
            .unwrap();
        assert_eq!(request.name, "daemon.request");
        assert_eq!(
            request.span_context.trace_id(),
            parent.span_context.trace_id()
        );
        if matches!(request.status, Status::Error { .. }) {
            failures += 1;
        }
        assert!(!format!("{child:?}").contains("/protected"));
    }
    assert_eq!(failures, 1);
}
