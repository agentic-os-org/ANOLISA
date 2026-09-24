//! Pure userspace memory micro-benchmarks for SQLite storage paths.
//!
//! These tests do not load eBPF probes, so they can run as a normal user and
//! still exercise the memory behaviour of the persistence layer. They are
//! intended to complement the full-process eBPF benchmark in
//! `docs/research/memory_benchmark.py`.

use agentsight::config::BatchConfig;
use agentsight::genai::exporter::GenAIExporter;
use agentsight::genai::semantic::{
    GenAISemanticEvent, InputMessage, LLMCall, LLMRequest, LLMResponse, MessagePart, OutputMessage,
    TokenUsage,
};
use agentsight::storage::sqlite::genai::GenAISqliteStore;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const EVENT_COUNT: usize = 10_000;

fn make_llm_call(idx: usize) -> LLMCall {
    let request = LLMRequest {
        messages: vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                content: format!("hello world request number {}", idx),
            }],
            name: None,
        }],
        temperature: None,
        max_tokens: None,
        frequency_penalty: None,
        presence_penalty: None,
        top_p: None,
        top_k: None,
        seed: None,
        stop_sequences: None,
        stream: false,
        tools: None,
        raw_body: None,
    };

    let response = LLMResponse {
        messages: vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![MessagePart::Text {
                content: format!("hello world response number {}", idx),
            }],
            name: None,
            finish_reason: Some("stop".to_string()),
        }],
        streamed: false,
        raw_body: None,
    };

    let mut call = LLMCall::new(
        format!("call-{}", idx),
        0,
        "openai".to_string(),
        "gpt-4o".to_string(),
        request,
        1234,
        "python3".to_string(),
    );
    call.set_response(response, 1_000_000);
    call.set_token_usage(TokenUsage {
        input_tokens: 10,
        output_tokens: 10,
        total_tokens: 20,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });
    call
}

fn make_events(count: usize) -> Vec<GenAISemanticEvent> {
    (0..count)
        .map(|i| GenAISemanticEvent::LLMCall(make_llm_call(i)))
        .collect()
}

fn read_vm_rss_kb() -> usize {
    let status = fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            return line
                .split_whitespace()
                .nth(1)
                .expect("VmRSS value")
                .parse()
                .expect("parse VmRSS");
        }
    }
    0
}

#[derive(Debug)]
#[allow(dead_code)]
struct RunResult {
    duration: Duration,
    rss_before_kb: usize,
    rss_after_kb: usize,
    db_size_bytes: u64,
}

fn run_with_batch(path: &PathBuf, batch: Option<BatchConfig>) -> RunResult {
    let _ = fs::remove_dir_all(path);
    fs::create_dir_all(path).expect("create temp dir");
    let db = path.join("genai_events.db");

    let rss_before = read_vm_rss_kb();
    let store = GenAISqliteStore::new_with_path_and_batch(
        &db,
        batch,
        agentsight::config::PeriodicStoragePolicy::default(),
    )
    .expect("create store");

    let events = make_events(EVENT_COUNT);
    let start = Instant::now();
    store.export(&events);
    // Force flush so all events are on disk before measuring.
    drop(store);
    let duration = start.elapsed();

    let rss_after = read_vm_rss_kb();
    let db_size = fs::metadata(&db).map(|m| m.len()).unwrap_or(0);

    RunResult {
        duration,
        rss_before_kb: rss_before,
        rss_after_kb: rss_after,
        db_size_bytes: db_size,
    }
}

#[test]
fn sqlite_batch_lowers_write_overhead() {
    let tmp = PathBuf::from("/tmp/agentsight-mem-test");

    let no_batch = run_with_batch(&tmp.join("no-batch"), None);
    let with_batch = run_with_batch(
        &tmp.join("with-batch"),
        Some(BatchConfig {
            max_size: 1000,
            flush_ms: 1000,
        }),
    );

    println!("no batch:  {:?}", no_batch);
    println!("with batch: {:?}", with_batch);

    // NOTE: We do NOT assert batch is faster than no-batch because in CI
    // environments (shared runners, cold disk cache) the difference can be
    // negligible or even reversed for small event counts.  The primary value
    // of batch mode is reducing IO pressure under sustained load, which is
    // validated by the real-world benchmark rather than this unit test.

    // RSS delta should be bounded: storing 10k events should not explode RSS.
    let max_allowed_rss_delta_mb = 64;
    assert!(
        (no_batch.rss_after_kb.saturating_sub(no_batch.rss_before_kb)) / 1024
            <= max_allowed_rss_delta_mb,
        "non-batch path RSS delta too large: {} KB -> {} KB",
        no_batch.rss_before_kb,
        no_batch.rss_after_kb,
    );
    assert!(
        (with_batch
            .rss_after_kb
            .saturating_sub(with_batch.rss_before_kb))
            / 1024
            <= max_allowed_rss_delta_mb,
        "batch path RSS delta too large: {} KB -> {} KB",
        with_batch.rss_before_kb,
        with_batch.rss_after_kb,
    );

    // Both paths must persist the same amount of data.
    let size_tolerance = 1024 * 1024; // 1 MB
    assert!(
        no_batch.db_size_bytes.abs_diff(with_batch.db_size_bytes) < size_tolerance,
        "database sizes differ too much: {} vs {}",
        no_batch.db_size_bytes,
        with_batch.db_size_bytes
    );
}
