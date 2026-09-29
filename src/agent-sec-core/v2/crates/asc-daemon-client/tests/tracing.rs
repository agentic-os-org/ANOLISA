use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use asc_daemon_client::{ClientError, call};
use asc_daemon_protocol::{DaemonRequest, DaemonResponse};
use serde_json::json;
use uuid::Uuid;

struct Endpoint {
    directory: PathBuf,
    path: PathBuf,
    listener: UnixListener,
}

impl Endpoint {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("asc-client-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self {
            directory,
            path,
            listener,
        }
    }

    fn accept(&self) -> UnixStream {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    return stream;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "client did not connect");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
    }

    fn no_connection(&self) {
        assert_eq!(
            self.listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    fn exchange<T: Send>(
        &self,
        input: &DaemonRequest,
        timeout: Duration,
        server: impl FnOnce(&Self) -> T + Send,
    ) -> Result<DaemonResponse, ClientError> {
        let result = thread::scope(|scope| {
            let server = scope.spawn(|| server(self));
            let result = call(&self.path, input, timeout);
            // A returned peer stays open until after call finishes, allowing
            // tests to prove LF completion and deadlines without relying on EOF.
            let _peer = server.join().unwrap();
            result
        });
        self.no_connection();
        result
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn request() -> DaemonRequest {
    DaemonRequest {
        trace_context: None,
        compatibility: None,
        method: "test.method".to_owned(),
        params: json!({}),
    }
}

#[test]
fn explicit_carrier_isolates_ambient_context_without_mutating_or_retrying_request() {
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_sampler(opentelemetry_sdk::trace::Sampler::AlwaysOff)
        .build();
    let subscriber = tracing_subscriber::registry().with(
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("test"))
            .with_context_activation(true),
    );
    tracing::subscriber::set_global_default(subscriber).unwrap();
    {
        let parent = asc_observability::bind_trace_context_input(
            &asc_observability::Context::new(),
            &json!({"sessionId":"ambient", "trace_id":"caller-label"}),
        )
        .unwrap();
        let span =
            asc_observability::parent_span(tracing::info_span!(parent: None,"caller"), parent);
        span.in_scope(|| {
            let ambient = asc_observability::snapshot();
            for explicit in [false,true] {
                let mut input = request();
                if explicit { input.trace_context = Some(asc_daemon_protocol::TraceCarrierV1::from_headers(std::collections::HashMap::new())); }
                let original = input.clone();
                let endpoint = Endpoint::new();
                let expected_trace = ambient.trace_id.clone().unwrap();
                let result = endpoint.exchange(&input, Duration::from_secs(1), |peer| {
                    let mut stream = peer.accept();
                    let mut bytes = Vec::new(); BufReader::new(&stream).read_until(b'\n', &mut bytes).unwrap();
                    let wire: DaemonRequest = serde_json::from_slice(&bytes).unwrap();
                    let context = wire.trace_context.unwrap();
                    if explicit {
                        assert!(!context.traceparent.unwrap().contains(&expected_trace));
                        assert!(context.baggage.is_none()); assert!(wire.compatibility.is_none());
                    } else {
                        assert!(context.traceparent.unwrap().contains(&expected_trace));
                        assert!(context.baggage.unwrap().contains("ambient"));
                        assert_eq!(wire.compatibility.unwrap().trace_id.as_deref(), Some("caller-label"));
                    }
                    // A daemon rejection is returned once; never strip context
                    // and replay a business request after an error.
                    stream.write_all(b"{\"requestId\":\"request-1\",\"error\":{\"code\":\"invalid_request\",\"message\":\"invalid envelope\"}}\n").unwrap();
                    stream
                }).unwrap();
                assert!(matches!(result, DaemonResponse::Error(_)));
                assert_eq!(input, original);
                assert_eq!(asc_observability::snapshot(), ambient);
            }
        });
    }
}
