//! Stable userspace benchmarks for AgentSight parsing, aggregation, and metrics overhead.

use agentsight::ResponseSessionMapper;
use agentsight::aggregator::Aggregator;
use agentsight::analyzer::Analyzer;
use agentsight::event::Event;
use agentsight::genai::GenAIBuilder;
use agentsight::parser::{ParsedMessage, Parser};
use agentsight::probes::sslsniff::SslEvent;
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use metrics_exporter_prometheus::PrometheusBuilder;
use std::collections::HashMap;
use std::hint::black_box;
use std::rc::Rc;

fn ssl_event(payload: Vec<u8>, rw: i32, timestamp_ns: u64) -> Rc<SslEvent> {
    Rc::new(SslEvent {
        source: 0,
        timestamp_ns,
        delta_ns: 0,
        pid: 4242,
        tid: 4242,
        uid: 1000,
        len: payload.len() as u32,
        rw,
        comm: "benchmark-client".to_string(),
        buf: payload,
        is_handshake: false,
        ssl_ptr: 0x4242,
    })
}

fn openai_request(payload_bytes: usize) -> Vec<u8> {
    let body = serde_json::json!({
        "model": "benchmark-model",
        "messages": [{"role": "user", "content": "x".repeat(payload_bytes)}],
        "stream": false,
    })
    .to_string();
    format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: mock.local\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

fn openai_response() -> Vec<u8> {
    let body = serde_json::json!({
        "id": "benchmark-response",
        "object": "chat.completion",
        "model": "benchmark-model",
        "choices": [{
            "message": {"role": "assistant", "content": "benchmark response"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 12, "completion_tokens": 8, "total_tokens": 20}
    })
    .to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

fn http2_data_frame() -> Vec<u8> {
    let payload = br#"{"delta":"benchmark"}"#;
    let payload_len = payload.len();
    let mut frame = Vec::with_capacity(9 + payload_len);
    frame.extend_from_slice(&[
        ((payload_len >> 16) & 0xff) as u8,
        ((payload_len >> 8) & 0xff) as u8,
        (payload_len & 0xff) as u8,
        0x00, // DATA
        0x01, // END_STREAM
        0x00,
        0x00,
        0x00,
        0x01, // stream ID 1
    ]);
    frame.extend_from_slice(payload);
    frame
}

fn benchmark_parser(criterion: &mut Criterion) {
    let parser = Parser::new();
    let http1_request = ssl_event(openai_request(4096), 1, 1_000_000_000);
    let http1_response = ssl_event(openai_response(), 0, 1_100_000_000);
    let http2_data = ssl_event(http2_data_frame(), 0, 1_200_000_000);
    let sse = ssl_event(
        b"event: response.output_text.delta\ndata: {\"delta\":\"benchmark\"}\n\n".to_vec(),
        0,
        1_300_000_000,
    );

    assert!(
        matches!(
            parser
                .parse_ssl_event(Rc::clone(&http1_request))
                .messages
                .as_slice(),
            [ParsedMessage::Request(_)]
        ),
        "HTTP/1 request fixture must exercise the request parser"
    );
    assert!(
        matches!(
            parser
                .parse_ssl_event(Rc::clone(&http1_response))
                .messages
                .as_slice(),
            [ParsedMessage::Response(_)]
        ),
        "HTTP/1 response fixture must exercise the response parser"
    );
    assert!(
        matches!(
            parser
                .parse_ssl_event(Rc::clone(&http2_data))
                .messages
                .as_slice(),
            [ParsedMessage::Http2Frames(frames)]
                if frames.len() == 1 && frames[0].is_data()
        ),
        "HTTP/2 fixture must exercise the DATA frame parser"
    );
    assert!(
        matches!(
            parser.parse_ssl_event(Rc::clone(&sse)).messages.as_slice(),
            [ParsedMessage::SseEvent(_)]
        ),
        "SSE fixture must exercise the SSE parser"
    );

    let mut group = criterion.benchmark_group("parser");
    for (name, event) in [
        ("http1_request", http1_request),
        ("http1_response", http1_response),
        ("http2_data_frame", http2_data),
        ("sse_event", sse),
    ] {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let event = black_box(Rc::clone(&event));
                black_box(parser.parse_ssl_event(event))
            });
        });
    }
    group.finish();
}

struct PipelineInputs<'a> {
    parser: &'a Parser,
    analyzer: &'a Analyzer,
    response_mapper: &'a ResponseSessionMapper,
    pid_agent_name_cache: &'a HashMap<u32, String>,
    request: &'a SslEvent,
    response: &'a SslEvent,
}

fn run_userspace_pipeline(
    inputs: &PipelineInputs<'_>,
    aggregator: &mut Aggregator,
    genai_builder: &GenAIBuilder,
) -> usize {
    let request_result = inputs
        .parser
        .parse_event(Event::Ssl(inputs.request.clone()));
    drop(aggregator.process_result(request_result));

    let response_result = inputs
        .parser
        .parse_event(Event::Ssl(inputs.response.clone()));
    aggregator
        .process_result(response_result)
        .iter()
        .map(|result| {
            let analysis_results = inputs.analyzer.analyze_aggregated(result);
            let (output, _) = genai_builder.build_with_pending(
                &analysis_results,
                inputs.response_mapper,
                inputs.pid_agent_name_cache,
            );
            output.events.len()
        })
        .sum()
}

fn bench_pipeline(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    inputs: &PipelineInputs<'_>,
) {
    group.bench_function(name, |bencher| {
        bencher.iter_batched(
            || (Aggregator::new(), GenAIBuilder::new()),
            |(mut aggregator, genai_builder)| {
                black_box(run_userspace_pipeline(
                    inputs,
                    &mut aggregator,
                    &genai_builder,
                ))
            },
            BatchSize::SmallInput,
        );
    });
}

fn benchmark_userspace_pipeline(criterion: &mut Criterion) {
    let parser = Parser::new();
    let analyzer = Analyzer::new();
    let response_mapper = ResponseSessionMapper::new();
    let pid_agent_name_cache = HashMap::from([(4242, "benchmark-agent".to_string())]);
    let request = ssl_event(openai_request(4096), 1, 1_000_000_000);
    let response = ssl_event(openai_response(), 0, 1_100_000_000);
    let inputs = PipelineInputs {
        parser: &parser,
        analyzer: &analyzer,
        response_mapper: &response_mapper,
        pid_agent_name_cache: &pid_agent_name_cache,
        request: request.as_ref(),
        response: response.as_ref(),
    };

    let mut validator = Aggregator::new();
    let genai_builder = GenAIBuilder::new();
    assert!(
        run_userspace_pipeline(&inputs, &mut validator, &genai_builder) > 0,
        "pipeline fixture must produce at least one GenAI event"
    );

    let mut group = criterion.benchmark_group("pipeline");
    group.throughput(Throughput::Elements(1));
    bench_pipeline(&mut group, "http_json_4k", &inputs);

    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    agentsight::with_runtime_metrics_enabled(|| {
        metrics::with_local_recorder(&recorder, || {
            bench_pipeline(&mut group, "http_json_4k_with_metrics", &inputs);
        });
    });
    group.finish();

    let snapshot = handle.render();
    for metric in [
        "agentsight_stage_calls_total{stage=\"parser\"}",
        "agentsight_stage_calls_total{stage=\"aggregator\"}",
        "agentsight_stage_calls_total{stage=\"analyzer\"}",
        "agentsight_stage_calls_total{stage=\"genai\"}",
        "agentsight_stage_outputs_total",
        "agentsight_stage_duration_seconds",
    ] {
        assert!(
            snapshot.contains(metric),
            "real pipeline metrics snapshot is missing {metric}"
        );
    }
    println!("\nAgentSight pipeline metrics:\n{snapshot}");
}

fn emit_noop_metrics() {
    metrics::counter!("agentsight_benchmark_events_total", "stage" => "noop").increment(1);
    metrics::gauge!("agentsight_benchmark_queue_bytes", "stage" => "noop").set(4096.0);
    metrics::histogram!("agentsight_benchmark_duration_seconds", "stage" => "noop")
        .record(0.000_001);
}

fn emit_recorded_metrics() {
    metrics::counter!("agentsight_benchmark_events_total", "stage" => "recorded").increment(1);
    metrics::gauge!("agentsight_benchmark_queue_bytes", "stage" => "recorded").set(4096.0);
    metrics::histogram!("agentsight_benchmark_duration_seconds", "stage" => "recorded")
        .record(0.000_001);
}

fn benchmark_metrics_overhead(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("metrics");
    group.bench_function("no_recorder", |bencher| {
        bencher.iter(|| {
            emit_noop_metrics();
            black_box(())
        });
    });

    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, || {
        group.bench_function("prometheus_recorder", |bencher| {
            bencher.iter(|| {
                emit_recorded_metrics();
                black_box(())
            });
        });
    });
    group.finish();
    black_box(handle.render());
}

criterion_group!(
    benches,
    benchmark_parser,
    benchmark_userspace_pipeline,
    benchmark_metrics_overhead
);
criterion_main!(benches);
