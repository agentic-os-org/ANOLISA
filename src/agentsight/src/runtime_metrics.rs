//! Low-overhead pipeline instrumentation and Prometheus snapshot export.

use anyhow::{Context, Result, anyhow};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Environment variable selecting the optional runtime metrics output file.
pub(crate) const METRICS_FILE_ENV: &str = "AGENTSIGHT_METRICS_FILE";
/// Environment variable selecting the runtime metrics export interval in seconds.
pub(crate) const METRICS_INTERVAL_ENV: &str = "AGENTSIGHT_METRICS_INTERVAL_SECS";
const DEFAULT_EXPORT_INTERVAL: Duration = Duration::from_secs(1);
static OBSERVABILITY_ENABLED: AtomicBool = AtomicBool::new(false);

/// Runs a callback with pipeline instrumentation enabled, then restores the prior state.
pub(crate) fn with_observability_enabled<T>(callback: impl FnOnce() -> T) -> T {
    struct ObservabilityGuard(bool);

    impl Drop for ObservabilityGuard {
        fn drop(&mut self) {
            OBSERVABILITY_ENABLED.store(self.0, Ordering::Relaxed);
        }
    }

    let _guard = ObservabilityGuard(OBSERVABILITY_ENABLED.swap(true, Ordering::Relaxed));
    callback()
}

/// Point-in-time gauges and cumulative counters for the capture pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RuntimeMetrics {
    pub event_channel_bytes: u64,
    pub event_channel_budget_bytes: u64,
    pub channel_length: u64,
    pub channel_dropped: u64,
    pub ring_buffer_dropped: u64,
    pub connection_cache_bytes: u64,
    pub pending_genai_count: u64,
    pub pending_genai_bytes: u64,
    pub pending_connection_count: u64,
    pub pending_connection_bytes: u64,
    pub eviction_count: u64,
    pub completed: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CounterSnapshot {
    channel_dropped: u64,
    ring_buffer_dropped: u64,
    eviction_count: u64,
    completed: u64,
}

impl From<RuntimeMetrics> for CounterSnapshot {
    fn from(metrics: RuntimeMetrics) -> Self {
        Self {
            channel_dropped: metrics.channel_dropped,
            ring_buffer_dropped: metrics.ring_buffer_dropped,
            eviction_count: metrics.eviction_count,
            completed: metrics.completed,
        }
    }
}

/// Records the invocation count, output count, and elapsed time of one pipeline stage.
pub(crate) struct StageTimer {
    stage: &'static str,
    started: Option<Instant>,
}

impl StageTimer {
    /// Starts one timed invocation of a named pipeline stage.
    pub(crate) fn start(stage: &'static str) -> Self {
        Self::start_if(stage, OBSERVABILITY_ENABLED.load(Ordering::Relaxed))
    }

    /// Records the number of values produced by this invocation.
    pub(crate) fn record_outputs(&self, outputs: usize) {
        if self.started.is_none() {
            return;
        }
        metrics::counter!("agentsight_stage_outputs_total", "stage" => self.stage)
            .increment(outputs as u64);
    }

    fn start_if(stage: &'static str, enabled: bool) -> Self {
        let started = enabled.then(Instant::now);
        if started.is_some() {
            metrics::counter!("agentsight_stage_calls_total", "stage" => stage).increment(1);
        }
        Self { stage, started }
    }
}

impl Drop for StageTimer {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            metrics::histogram!("agentsight_stage_duration_seconds", "stage" => self.stage)
                .record(started.elapsed().as_secs_f64());
        }
    }
}

/// Records one event entering the AgentSight userspace pipeline.
pub(crate) fn record_event_received(source: &'static str) {
    if !OBSERVABILITY_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    metrics::counter!("agentsight_events_received_total", "source" => source).increment(1);
}

/// Rate-limited writer that publishes complete Prometheus snapshots atomically.
pub(crate) struct MetricsFileExporter {
    path: PathBuf,
    handle: PrometheusHandle,
    export_interval: Duration,
    last_attempt: Option<Instant>,
    last_counters: CounterSnapshot,
}

impl std::fmt::Debug for MetricsFileExporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MetricsFileExporter")
            .field("path", &self.path)
            .field("export_interval", &self.export_interval)
            .field("last_attempt", &self.last_attempt)
            .field("last_counters", &self.last_counters)
            .finish_non_exhaustive()
    }
}

impl MetricsFileExporter {
    /// Builds and installs an exporter when [`METRICS_FILE_ENV`] names a non-empty path.
    pub(crate) fn from_env() -> Result<Option<Self>> {
        let Some(value) = std::env::var_os(METRICS_FILE_ENV) else {
            return Ok(None);
        };
        if value.is_empty() {
            return Ok(None);
        }

        let path = PathBuf::from(value);
        let export_interval = match std::env::var(METRICS_INTERVAL_ENV) {
            Ok(value) => parse_export_interval(&value)?,
            Err(std::env::VarError::NotPresent) => DEFAULT_EXPORT_INTERVAL,
            Err(error) => return Err(anyhow!("invalid {METRICS_INTERVAL_ENV}: {error}")),
        };
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let exporter = Self::new(path, handle, export_interval)?;
        metrics::set_global_recorder(recorder).map_err(|_| {
            anyhow!("cannot install AgentSight metrics recorder: a global recorder is already set")
        })?;
        describe_metrics();
        OBSERVABILITY_ENABLED.store(true, Ordering::Relaxed);

        Ok(Some(exporter))
    }

    fn new(path: PathBuf, handle: PrometheusHandle, export_interval: Duration) -> Result<Self> {
        validate_output_path(&path)?;
        Ok(Self {
            path,
            handle,
            export_interval,
            last_attempt: None,
            last_counters: CounterSnapshot::default(),
        })
    }

    /// Publishes a snapshot when the configured export interval elapsed.
    pub(crate) fn maybe_export(&mut self, snapshot: RuntimeMetrics) -> Result<bool> {
        if !self.is_due() {
            return Ok(false);
        }
        // A broken destination must not turn a full event queue into a retry
        // and log storm. Retry on the next normal sampling interval.
        self.last_attempt = Some(Instant::now());
        self.export(snapshot)?;
        Ok(true)
    }

    /// Reports whether collecting the next snapshot would result in a write.
    pub(crate) fn is_due(&self) -> bool {
        self.last_attempt
            .is_none_or(|last| last.elapsed() >= self.export_interval)
    }

    /// Publishes a final snapshot regardless of the sampling interval.
    pub(crate) fn export(&mut self, snapshot: RuntimeMetrics) -> Result<()> {
        self.record_snapshot(snapshot);
        self.handle.run_upkeep();
        write_atomic(&self.path, self.handle.render().as_bytes())
    }

    fn record_snapshot(&mut self, snapshot: RuntimeMetrics) {
        metrics::gauge!("agentsight_event_channel_bytes").set(snapshot.event_channel_bytes as f64);
        metrics::gauge!("agentsight_event_channel_budget_bytes")
            .set(snapshot.event_channel_budget_bytes as f64);
        metrics::gauge!("agentsight_event_channel_length").set(snapshot.channel_length as f64);
        metrics::gauge!("agentsight_connection_cache_bytes")
            .set(snapshot.connection_cache_bytes as f64);
        metrics::gauge!("agentsight_pending_genai_count").set(snapshot.pending_genai_count as f64);
        metrics::gauge!("agentsight_pending_genai_bytes").set(snapshot.pending_genai_bytes as f64);
        metrics::gauge!("agentsight_pending_connection_count")
            .set(snapshot.pending_connection_count as f64);
        metrics::gauge!("agentsight_pending_connection_bytes")
            .set(snapshot.pending_connection_bytes as f64);

        let current = CounterSnapshot::from(snapshot);
        increment_counter(
            "agentsight_channel_dropped_total",
            current.channel_dropped,
            self.last_counters.channel_dropped,
        );
        increment_counter(
            "agentsight_ring_buffer_dropped_total",
            current.ring_buffer_dropped,
            self.last_counters.ring_buffer_dropped,
        );
        increment_counter(
            "agentsight_connection_evictions_total",
            current.eviction_count,
            self.last_counters.eviction_count,
        );
        increment_counter(
            "agentsight_events_completed_total",
            current.completed,
            self.last_counters.completed,
        );
        self.last_counters = current;
    }
}

fn parse_export_interval(value: &str) -> Result<Duration> {
    let seconds: u64 = value
        .parse()
        .with_context(|| format!("{METRICS_INTERVAL_ENV} must be a positive integer in seconds"))?;
    if seconds == 0 {
        return Err(anyhow!(
            "{METRICS_INTERVAL_ENV} must be a positive integer in seconds"
        ));
    }
    Ok(Duration::from_secs(seconds))
}

fn validate_output_path(path: &Path) -> Result<()> {
    if path.file_name().is_none() {
        return Err(anyhow!("{METRICS_FILE_ENV} must name a file"));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create metrics directory {}", parent.display()))?;
    }
    Ok(())
}

fn increment_counter(name: &'static str, current: u64, previous: u64) {
    let delta = current.checked_sub(previous).unwrap_or(current);
    metrics::counter!(name).increment(delta);
}

fn describe_metrics() {
    metrics::describe_counter!(
        "agentsight_events_received_total",
        "Events received by the AgentSight userspace pipeline"
    );
    metrics::describe_counter!(
        "agentsight_events_completed_total",
        "Events removed from the probe queue for processing"
    );
    metrics::describe_counter!(
        "agentsight_channel_dropped_total",
        "Events rejected by userspace channel admission"
    );
    metrics::describe_counter!(
        "agentsight_ring_buffer_dropped_total",
        "Kernel ring-buffer reservations that could not be submitted"
    );
    metrics::describe_counter!(
        "agentsight_connection_evictions_total",
        "HTTP connections or streams evicted from correlation caches"
    );
    metrics::describe_counter!(
        "agentsight_stage_calls_total",
        "Invocations of an AgentSight processing stage"
    );
    metrics::describe_counter!(
        "agentsight_stage_outputs_total",
        "Values produced by an AgentSight processing stage"
    );
    metrics::describe_histogram!(
        "agentsight_stage_duration_seconds",
        "Wall-clock duration of AgentSight processing stages"
    );
    metrics::describe_gauge!(
        "agentsight_event_channel_bytes",
        "Estimated bytes currently reserved by the event channel"
    );
    metrics::describe_gauge!(
        "agentsight_event_channel_budget_bytes",
        "Configured event-channel byte budget"
    );
    metrics::describe_gauge!(
        "agentsight_event_channel_length",
        "Events currently waiting in the userspace channel"
    );
    metrics::describe_gauge!(
        "agentsight_connection_cache_bytes",
        "Logical payload bytes retained for connection correlation"
    );
    metrics::describe_gauge!(
        "agentsight_pending_genai_count",
        "GenAI events waiting for session correlation"
    );
    metrics::describe_gauge!(
        "agentsight_pending_genai_bytes",
        "Estimated bytes retained by pending GenAI events"
    );
    metrics::describe_gauge!(
        "agentsight_pending_connection_count",
        "HTTP connections or streams awaiting correlation"
    );
    metrics::describe_gauge!(
        "agentsight_pending_connection_bytes",
        "Logical bytes retained by incomplete HTTP connections or streams"
    );
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("metrics path must name a file: {}", path.display()))?;
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("failed to create metrics snapshot {}", temporary.display()))?;
    if let Err(error) = file.write_all(contents) {
        let _ = fs::remove_file(&temporary);
        return Err(error)
            .with_context(|| format!("failed to write metrics snapshot {}", temporary.display()));
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error)
            .with_context(|| format!("failed to publish metrics snapshot {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::Aggregator;
    use crate::analyzer::Analyzer;
    use crate::event::Event;
    use crate::parser::Parser;
    use crate::probes::sslsniff::SslEvent;
    use metrics_exporter_prometheus::PrometheusRecorder;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DIRECTORY_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    fn test_directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "agentsight-runtime-metrics-test-{}-{}",
            std::process::id(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn test_exporter(path: PathBuf) -> (MetricsFileExporter, PrometheusRecorder) {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        (
            MetricsFileExporter::new(path, handle, DEFAULT_EXPORT_INTERVAL)
                .expect("create exporter"),
            recorder,
        )
    }

    fn ssl_event(payload: Vec<u8>, rw: i32, timestamp_ns: u64) -> SslEvent {
        SslEvent {
            source: 0,
            timestamp_ns,
            delta_ns: 0,
            pid: 4242,
            tid: 4242,
            uid: 1000,
            len: payload.len() as u32,
            rw,
            comm: "metrics-test-client".to_string(),
            buf: payload,
            is_handshake: false,
            ssl_ptr: 0x4242,
        }
    }

    fn http_message(start_line: &str, body: &str) -> Vec<u8> {
        format!(
            "{start_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn exporter_atomically_replaces_a_complete_prometheus_snapshot() {
        let directory = test_directory();
        let path = directory.join("metrics.prom");
        let (mut exporter, recorder) = test_exporter(path.clone());
        metrics::with_local_recorder(&recorder, || {
            exporter
                .export(RuntimeMetrics {
                    channel_length: 17,
                    completed: 23,
                    ..RuntimeMetrics::default()
                })
                .expect("export snapshot");
        });

        let contents = fs::read_to_string(&path).expect("read snapshot");
        assert!(contents.contains("agentsight_event_channel_length 17"));
        assert!(contents.contains("agentsight_events_completed_total 23"));
        assert_eq!(
            fs::read_dir(&directory)
                .expect("read test directory")
                .count(),
            1,
            "atomic temporary file must not remain after publish"
        );

        fs::remove_file(path).expect("remove test snapshot");
        fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn exporter_converts_cumulative_sources_to_counter_deltas() {
        let directory = test_directory();
        let path = directory.join("metrics.prom");
        let (mut exporter, recorder) = test_exporter(path.clone());
        metrics::with_local_recorder(&recorder, || {
            exporter
                .export(RuntimeMetrics {
                    channel_dropped: 5,
                    completed: 10,
                    ..RuntimeMetrics::default()
                })
                .expect("first export");
            exporter
                .export(RuntimeMetrics {
                    channel_dropped: 7,
                    completed: 14,
                    ..RuntimeMetrics::default()
                })
                .expect("second export");
        });

        let contents = fs::read_to_string(&path).expect("read snapshot");
        assert!(contents.contains("agentsight_channel_dropped_total 7"));
        assert!(contents.contains("agentsight_events_completed_total 14"));

        fs::remove_file(path).expect("remove test snapshot");
        fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn exporter_rate_limits_intermediate_snapshots() {
        let directory = test_directory();
        let path = directory.join("metrics.prom");
        let (mut exporter, recorder) = test_exporter(path.clone());
        exporter.export_interval = Duration::from_secs(10);
        metrics::with_local_recorder(&recorder, || {
            assert!(
                exporter
                    .maybe_export(RuntimeMetrics {
                        completed: 1,
                        ..RuntimeMetrics::default()
                    })
                    .expect("first export")
            );
            assert!(!exporter.is_due());
            assert!(
                !exporter
                    .maybe_export(RuntimeMetrics {
                        completed: 2,
                        ..RuntimeMetrics::default()
                    })
                    .expect("rate-limited export")
            );
        });
        assert!(
            fs::read_to_string(&path)
                .expect("read snapshot")
                .contains("agentsight_events_completed_total 1")
        );

        exporter.last_attempt = Some(Instant::now() - Duration::from_secs(11));
        metrics::with_local_recorder(&recorder, || {
            assert!(
                exporter
                    .maybe_export(RuntimeMetrics {
                        completed: 2,
                        ..RuntimeMetrics::default()
                    })
                    .expect("export after configured interval")
            );
        });
        assert!(
            fs::read_to_string(&path)
                .expect("read updated snapshot")
                .contains("agentsight_events_completed_total 2")
        );

        fs::remove_file(path).expect("remove test snapshot");
        fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn exporter_rate_limits_failed_write_attempts() {
        let directory = test_directory();
        let path = directory.join("metrics.prom");
        let (mut exporter, recorder) = test_exporter(path);
        fs::remove_dir(&directory).expect("remove metrics directory");

        metrics::with_local_recorder(&recorder, || {
            assert!(
                exporter
                    .maybe_export(RuntimeMetrics::default())
                    .expect_err("missing parent must reject the write")
                    .to_string()
                    .contains("failed to create metrics snapshot")
            );
        });
        assert!(!exporter.is_due());
    }

    #[test]
    fn stage_timer_records_calls_outputs_and_duration() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let timer = StageTimer::start_if("parser", true);
            timer.record_outputs(2);
        });

        let snapshot = handle.render();
        assert!(snapshot.contains("agentsight_stage_calls_total{stage=\"parser\"} 1"));
        assert!(snapshot.contains("agentsight_stage_outputs_total{stage=\"parser\"} 2"));
        assert!(snapshot.contains("agentsight_stage_duration_seconds{stage=\"parser\""));
    }

    #[test]
    fn disabled_stage_timer_is_silent() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let timer = StageTimer::start_if("parser", false);
            timer.record_outputs(2);
        });

        assert!(handle.render().is_empty());
    }

    #[test]
    fn received_events_and_metric_descriptions_appear_in_prometheus_output() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            describe_metrics();
            with_observability_enabled(|| record_event_received("ssl"));
            metrics::gauge!("agentsight_event_channel_bytes").set(128.0);
            metrics::histogram!("agentsight_stage_duration_seconds", "stage" => "parser")
                .record(0.01);
        });
        handle.run_upkeep();

        let snapshot = handle.render();
        assert!(snapshot.contains("agentsight_events_received_total{source=\"ssl\"} 1"));
        assert!(snapshot.contains(
            "# HELP agentsight_events_received_total Events received by the AgentSight userspace pipeline"
        ));
        assert!(snapshot.contains(
            "# HELP agentsight_event_channel_bytes Estimated bytes currently reserved by the event channel"
        ));
        assert!(snapshot.contains(
            "# HELP agentsight_stage_duration_seconds Wall-clock duration of AgentSight processing stages"
        ));
    }

    #[test]
    fn pipeline_stages_emit_metrics() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        crate::with_runtime_metrics_enabled(|| {
            metrics::with_local_recorder(&recorder, || {
                let parser = Parser::new();
                let analyzer = Analyzer::new();
                let mut aggregator = Aggregator::new();
                let request_body = serde_json::json!({
                    "model": "metrics-test-model",
                    "messages": [{"role": "user", "content": "hello"}],
                    "stream": false,
                })
                .to_string();
                let response_body = serde_json::json!({
                    "id": "metrics-test-response",
                    "object": "chat.completion",
                    "model": "metrics-test-model",
                    "choices": [{
                        "message": {"role": "assistant", "content": "world"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                })
                .to_string();

                let request = parser.parse_event(Event::Ssl(ssl_event(
                    http_message("POST /v1/chat/completions HTTP/1.1", &request_body),
                    1,
                    1_000_000_000,
                )));
                assert!(aggregator.process_result(request).is_empty());
                let connections = aggregator.connection_metrics();
                assert_eq!(connections.pending_connection_count, 1);
                assert!(connections.pending_connection_bytes > 0);

                let response = parser.parse_event(Event::Ssl(ssl_event(
                    http_message("HTTP/1.1 200 OK", &response_body),
                    0,
                    1_100_000_000,
                )));
                let aggregated = aggregator.process_result(response);
                assert_eq!(aggregated.len(), 1);
                assert!(!analyzer.analyze_aggregated(&aggregated[0]).is_empty());
            });
        });

        let snapshot = handle.render();
        assert!(snapshot.contains("agentsight_stage_calls_total{stage=\"parser\"} 2"));
        assert!(snapshot.contains("agentsight_stage_calls_total{stage=\"aggregator\"} 2"));
        assert!(snapshot.contains("agentsight_stage_calls_total{stage=\"analyzer\"} 1"));
        assert!(snapshot.contains("agentsight_stage_outputs_total{stage=\"analyzer\"}"));
    }

    #[test]
    fn exporter_rejects_a_path_without_a_file_name() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let error =
            MetricsFileExporter::new(PathBuf::new(), recorder.handle(), DEFAULT_EXPORT_INTERVAL)
                .expect_err("missing file name");
        assert!(error.to_string().contains(METRICS_FILE_ENV));
    }

    #[test]
    fn export_interval_accepts_positive_seconds_only() {
        assert_eq!(
            parse_export_interval("10").expect("valid interval"),
            Duration::from_secs(10)
        );
        for value in ["", "0", "1.5", "-1", "invalid"] {
            let error = parse_export_interval(value).expect_err("invalid interval");
            assert!(error.to_string().contains(METRICS_INTERVAL_ENV));
        }
    }

    #[test]
    fn atomic_writer_does_not_overwrite_an_existing_temporary_path() {
        let directory = test_directory();
        fs::create_dir_all(&directory).expect("create test directory");
        let path = directory.join("metrics.prom");
        let temporary = directory.join(format!(".metrics.prom.{}.tmp", std::process::id()));
        fs::write(&temporary, b"owned by another writer").expect("seed temporary path");

        let error = write_atomic(&path, b"new metrics").expect_err("existing path must fail");

        assert!(
            error
                .to_string()
                .contains("failed to create metrics snapshot")
        );
        assert_eq!(
            fs::read(&temporary).expect("read seeded path"),
            b"owned by another writer"
        );
        assert!(!path.exists());
        fs::remove_file(temporary).expect("remove seeded path");
        fs::remove_dir(directory).expect("remove test directory");
    }
}
