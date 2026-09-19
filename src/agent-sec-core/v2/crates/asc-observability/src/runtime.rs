use opentelemetry::{
    KeyValue,
    trace::{TraceContextExt as _, TracerProvider as _},
};
use opentelemetry_sdk::{
    Resource,
    trace::{Sampler, SdkTracerProvider},
};
use std::time::{Duration, Instant};

mod logs;
use logs::{DiagnosticWriter, LogWorker};
use tracing_subscriber::{Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _};

/// Process-owned SDK lifetime. Construct and shut down from synchronous main.
pub struct TelemetryRuntime {
    provider: SdkTracerProvider,
    writer: DiagnosticWriter,
    logs: Option<LogWorker>,
}
impl TelemetryRuntime {
    /// Queues a process diagnostic independently of the correlation log filter.
    /// This is best effort and must not be used for CLI business results.
    pub fn report(&self, message: &str) {
        self.writer.emit(format!("{message}\n").into_bytes());
    }

    /// Clones the bounded diagnostic writer for background process tasks.
    pub fn reporter(&self) -> impl Fn(&str) + Send + Sync + use<> {
        let writer = self.writer.clone();
        move |message| writer.emit(format!("{message}\n").into_bytes())
    }

    /// Flushes once, bounded by the caller's shutdown budget. Failure only loses
    /// diagnostics; it must not alter the business exit code or retry a request.
    pub fn shutdown(self, timeout: Duration) {
        let started = Instant::now();
        if self.provider.shutdown_with_timeout(timeout).is_err() {
            self.writer.emit(b"otel: shutdown_incomplete\n".to_vec());
        }
        if let Some(logs) = self.logs {
            logs.shutdown(timeout.saturating_sub(started.elapsed()));
        }
    }
}

/// Reports a fatal initialization diagnostic without waiting indefinitely for stderr.
/// Call only on a process exit path before a runtime is available. If a worker
/// cannot be created, the diagnostic is dropped; there is no synchronous fallback.
pub fn report_startup_error(message: &str) {
    if let Ok((writer, worker)) = DiagnosticWriter::start(std::io::stderr()) {
        writer.emit(format!("{message}\n").into_bytes());
        worker.shutdown(Duration::from_millis(50));
    }
}

/// Process-singleton initialization, called once from synchronous main.
/// Installs a real, unsampled SDK for local correlation only. No exporter is
/// installed; OTEL_* settings cannot enable export or override fixed sampling.
/// `RUST_LOG` filters local correlation records independently of the span bridge.
/// Replaces the process panic hook with a bounded, payload-free diagnostic.
///
/// # Errors
/// Returns a fixed initialization error before accepting requests if another
/// global subscriber owns the process, or the bridge cannot produce valid IDs.
pub fn init_runtime(service_name: &'static str) -> Result<TelemetryRuntime, &'static str> {
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::AlwaysOff)
        .with_resource(
            Resource::builder_empty()
                .with_attributes([
                    KeyValue::new("service.name", service_name),
                    KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                ])
                .build(),
        )
        .with_max_attributes_per_span(32)
        .with_max_events_per_span(0)
        .with_max_links_per_span(8)
        .build();
    let tracer = provider.tracer(service_name);
    let bridge = tracing_opentelemetry::layer()
        .with_tracer(tracer)
        .with_context_activation(true)
        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.is_span()
                && (metadata.target().starts_with("asc_") || metadata.target() == "agent_sec_cli")
        }));
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    // Also carries startup/shutdown diagnostics regardless of the RUST_LOG filter.
    // A failed worker disables diagnostics; never fall back to blocking stderr.
    let (writer, log_worker) = DiagnosticWriter::start(std::io::stderr()).map_or_else(
        |_| (DiagnosticWriter::default(), None),
        |(writer, worker)| (writer, Some(worker)),
    );
    let logs = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(writer.clone())
        .with_current_span(false)
        .with_span_list(false)
        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.target() == "asc_observability::diagnostic"
        }))
        .with_filter(filter);
    // Library warnings remain visible even when correlation logging is disabled.
    // Keep the original message text and use the same bounded output queue.
    let process_logs = tracing_subscriber::fmt::layer()
        .without_time()
        .with_ansi(false)
        .with_level(false)
        .with_target(false)
        .with_writer(writer.clone())
        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.is_event() && metadata.target() == "asc_process_diagnostic"
        }));
    tracing_subscriber::registry()
        .with(bridge)
        .with(logs)
        .with(process_logs)
        .try_init()
        .map_err(|_| "subscriber_conflict")?;
    opentelemetry::global::set_tracer_provider(provider.clone());
    opentelemetry::global::set_text_map_propagator(
        opentelemetry::propagation::TextMapCompositePropagator::new(vec![
            Box::new(opentelemetry_sdk::propagation::TraceContextPropagator::new()),
            Box::new(opentelemetry_sdk::propagation::BaggagePropagator::new()),
        ]),
    );
    let span = tracing::info_span!("otel.bootstrap");
    let valid = span.in_scope(|| {
        opentelemetry::Context::current()
            .span()
            .span_context()
            .is_valid()
    });
    if !valid {
        return Err("invalid_sdk_identity");
    }
    // The default panic hook writes synchronously even when a handler catches
    // the unwind. Keep that implicit diagnostic off the request's I/O path too.
    // Do not include panic payloads, which may contain request data.
    let panic_writer = writer.clone();
    std::panic::set_hook(Box::new(move |_| {
        panic_writer.emit(b"runtime: panic\n".to_vec());
    }));
    Ok(TelemetryRuntime {
        provider,
        writer,
        logs: log_worker,
    })
}
