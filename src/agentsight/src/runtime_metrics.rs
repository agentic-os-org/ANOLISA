//! Low-overhead stage instrumentation for AgentSight pipeline benchmarks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

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

/// Records invocation count, output count, and elapsed time for one pipeline stage.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::Aggregator;
    use crate::analyzer::Analyzer;
    use crate::event::Event;
    use crate::parser::Parser;
    use crate::probes::sslsniff::SslEvent;
    use metrics_exporter_prometheus::PrometheusBuilder;

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
        assert!(snapshot.contains("agentsight_stage_outputs_total{stage=\"analyzer\""));
    }
}
