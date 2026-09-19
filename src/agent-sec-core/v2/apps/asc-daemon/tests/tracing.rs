use asc_daemon::{BootstrapConfig, serve};
use asc_daemon_core::{PeerCredentials, PrincipalPolicy, PrincipalRole};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_engine::PolicyTemplateCompiler;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider};
use std::os::unix::fs::DirBuilderExt as _;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tracing_subscriber::layer::SubscriberExt as _;

struct PausedPolicy(Mutex<mpsc::Receiver<()>>);
impl PrincipalPolicy for PausedPolicy {
    fn role_for(&self, _: PeerCredentials) -> PrincipalRole {
        self.0
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        PrincipalRole::PolicyAdministrator
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracing_span_remains_open_after_dispatch_timeout_until_work_finishes() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .with_span_processor(asc_observability::AgentFieldProcessor)
        .with_simple_exporter(exporter.clone())
        .build();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer("test"))
                .with_context_activation(true),
        ),
    )
    .unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let pap = PapService::new(
        Arc::new(ProcessLocalPapRepository::default()),
        Arc::new(PolicyTemplateCompiler),
    );
    let dispatcher = Arc::new(DaemonDispatcher::new(
        pap,
        Arc::new(PausedPolicy(Mutex::new(release_rx))),
        asc_daemon::scan_application(asc_action_runtime::testing::discarding_finalizer()),
    ));
    let directory =
        std::env::temp_dir().join(format!("asc-otel-lifetime-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let path = directory.join("daemon.sock");
    let mut config = BootstrapConfig::new(&path);
    config.service.dispatch_timeout = Duration::from_millis(50);
    let shutdown = asc_daemon_service::ShutdownToken::new();
    let service_shutdown = shutdown.clone();
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
    wait_for_socket(&path).await;
    let mut stream = UnixStream::connect(&path).await.unwrap();
    stream
        .write_all(b"{\"method\":\"policy.templates.list\"}\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        BufReader::new(stream).read_until(b'\n', &mut response),
    )
    .await
    .unwrap()
    .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(response["error"]["code"], "deadline_exceeded");
    assert!(
        !exporter
            .get_finished_spans()
            .unwrap()
            .iter()
            .any(|span| span.name == "daemon.request"),
        "timeout must not end still-running work"
    );
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !exporter
            .get_finished_spans()
            .unwrap()
            .iter()
            .any(|span| span.name == "daemon.request")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let request = spans
        .iter()
        .find(|span| span.name == "daemon.request")
        .unwrap();
    assert!(
        request.end_time.duration_since(request.start_time).unwrap() >= Duration::from_millis(50)
    );
    assert!(
        request
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "cancel_requested" && kv.value.to_string() == "true")
    );
    shutdown.request();
    task.await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

async fn wait_for_socket(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
