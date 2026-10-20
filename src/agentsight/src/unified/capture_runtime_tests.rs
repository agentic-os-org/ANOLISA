//! Replay capture inputs through the production runtime without kernel attachments.

use super::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};

/// Keep live-probe construction available while replay fixtures own no kernel objects.
pub(super) struct TestProbes {
    live: Option<crate::probes::Probes>,
    events: RefCell<VecDeque<Event>>,
}

impl Deref for TestProbes {
    type Target = crate::probes::Probes;
    fn deref(&self) -> &Self::Target {
        self.live
            .as_ref()
            .expect("replay must not access kernel probes")
    }
}

impl DerefMut for TestProbes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.live
            .as_mut()
            .expect("replay must not access kernel probes")
    }
}

impl TestProbes {
    pub(super) fn new_with_cgroup_filter(
        pids: &[u32],
        uid: Option<u32>,
        filewatch: bool,
        dns: bool,
        tcp: &[crate::config::TcpTarget],
        cgroup: bool,
        limits: &crate::config::RuntimeLimits,
    ) -> Result<Self> {
        Ok(Self {
            live: Some(crate::probes::Probes::new_with_cgroup_filter(
                pids, uid, filewatch, dns, tcp, cgroup, limits,
            )?),
            events: RefCell::new(VecDeque::new()),
        })
    }

    pub(super) fn run(&self) -> Result<TestPoller> {
        Ok(TestPoller {
            _live: Some(self.deref().run()?),
        })
    }

    pub(super) fn try_recv(&self) -> Option<Event> {
        self.events
            .borrow_mut()
            .pop_front()
            .or_else(|| self.live.as_ref().and_then(crate::probes::Probes::try_recv))
    }

    pub(super) fn channel_watermarks(&self) -> ChannelWatermarks {
        self.live.as_ref().map_or(
            ChannelWatermarks {
                in_flight_bytes: 0,
                budget_bytes: 1024 * 1024,
                dropped_over_budget: 0,
            },
            crate::probes::Probes::channel_watermarks,
        )
    }

    pub(super) fn channel_length(&self) -> usize {
        self.live.as_ref().map_or(
            self.events.borrow().len(),
            crate::probes::Probes::channel_length,
        )
    }

    pub(super) fn channel_dropped(&self) -> usize {
        self.live
            .as_ref()
            .map_or(0, crate::probes::Probes::channel_dropped)
    }

    pub(super) fn ring_buffer_dropped(&self) -> Result<u64> {
        self.live
            .as_ref()
            .map_or(Ok(0), crate::probes::Probes::ring_buffer_dropped)
    }
}

/// A replay has no poller thread to join.
pub(super) struct TestPoller {
    _live: Option<crate::probes::ProbesPoller>,
}

pub(super) struct RuntimeFixture {
    sight: Option<AgentSight>,
    pub(super) directory: PathBuf,
}

impl RuntimeFixture {
    pub(super) fn new(percent: u8) -> Self {
        let directory = super::tests::unique_tmp_dir("capture-runtime");
        let mut config = AgentsightConfig::default();
        config.storage.base_path = directory.clone();
        config.runtime_limits.cpu_budget_percent = percent;
        let database_manager = Arc::new(
            DatabaseManager::new(DatabaseRole::Trace, trace_database_specs(&config)).unwrap(),
        );
        let store = Arc::new(
            GenAISqliteStore::new_with_path(
                &config.storage.genai_path(),
                crate::config::PeriodicStoragePolicy::default(),
            )
            .unwrap(),
        );
        let budget = (percent != 0)
            .then(|| Arc::new(crate::utils::cpu_budget::CpuBudget::new(percent).unwrap()));
        let storage = AgentSight::create_storage(&config.storage.primary_path(), &config).unwrap();
        Self {
            directory,
            sight: Some(AgentSight {
                probes: TestProbes {
                    live: None,
                    events: RefCell::new(VecDeque::new()),
                },
                core: PipelineCore::new(&config, Analyzer::new()),
                genai_builder: GenAIBuilder::new(),
                genai_exporters: vec![Box::new(Arc::clone(&store))],
                genai_sqlite_store: Some(store),
                interruption_detector: InterruptionDetector::disabled(),
                interruption_store: None,
                storage,
                scanner: AgentScanner::from_rules(&[], &[]),
                _poller: TestPoller { _live: None },
                running: Arc::new(AtomicBool::new(true)),
                event_count: 0,
                filewatch_callback: None,
                response_mapper: ResponseSessionMapper::new(),
                pending_genai: PendingGenAiQueue::default(),
                completions: completion::CompletionScheduler::new(budget),
                retro_session_fixup: lru::LruCache::new(std::num::NonZeroUsize::new(128).unwrap()),
                runtime_limits: config.runtime_limits,
                ffi_sender: None,
                last_drain_check: std::time::Instant::now(),
                last_watermark_log: std::time::Instant::now(),
                pid_agent_name_cache: lru::LruCache::new(std::num::NonZeroUsize::new(128).unwrap()),
                resource_targets: Arc::new(RwLock::new(HashMap::new())),
                resource_sampler_handle: None,
                http_domains: Vec::new(),
                pending_logtail: Arc::new(Mutex::new(None)),
                deadloop_kill_enabled: false,
                deadloop_kill_after_count: 3,
                process_killer: Arc::new(crate::utils::process::LibcProcessKiller),
                features: config.features,
                database_manager,
                metrics_exporter: None,
            }),
        }
    }

    pub(super) fn rows(&self) -> Vec<String> {
        let db = rusqlite::Connection::open(self.directory.join("genai_events.db")).unwrap();
        db.prepare("SELECT trace_id FROM genai_events WHERE status='complete' ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }
}

impl Deref for RuntimeFixture {
    type Target = AgentSight;
    fn deref(&self) -> &Self::Target {
        self.sight.as_ref().unwrap()
    }
}
impl DerefMut for RuntimeFixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.sight.as_mut().unwrap()
    }
}
impl Drop for RuntimeFixture {
    fn drop(&mut self) {
        drop(self.sight.take());
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn event_processing_keeps_owned_completions_behind_cpu_admission() {
    use crate::utils::cpu_budget::WorkClass;

    let mut fixture = RuntimeFixture::new(100);
    let budget = fixture.completions.budget.clone().unwrap();
    let permit = budget.acquire(WorkClass::Maintenance).unwrap();
    let request = r#"{"model":"fixture","messages":[{"role":"user","content":"hello"}]}"#;
    let response = r#"{"id":"core-runtime","object":"chat.completion","created":1,"model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":"reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}}"#;
    for (rw, bytes) in [
        (
            1,
            format!(
                "POST /v1/chat/completions HTTP/1.1\r\nHost: fixture.test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{request}",
                request.len()
            ),
        ),
        (
            0,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{response}",
                response.len()
            ),
        ),
    ] {
        fixture
            .probes
            .events
            .borrow_mut()
            .push_back(captured(rw, bytes.into_bytes()));
    }
    assert_eq!(fixture.try_process(), Some(1));
    assert_eq!(fixture.try_process(), Some(2));
    assert_eq!(fixture.try_process(), None);
    assert_eq!(fixture.completions.len(), 1);
    assert!(fixture.rows().is_empty());
    drop(permit);
    fixture.flush_completions();
    assert_eq!(fixture.rows(), ["core-runtime"]);
    assert_eq!(fixture.completions.bytes, 0);
    assert!(fixture.core.drain_pending().is_empty());
}

#[test]
fn raw_sse_fragmentation_matches_pre_extraction_delivery() {
    let mut config = AgentsightConfig::default();
    config.runtime_limits.retain_raw_sse = true;
    let mut core = PipelineCore::new(&config, Analyzer::new());
    let parser = crate::parser::Parser::with_max_headers(config.max_headers);
    let mut aggregator =
        Aggregator::with_limits(config.connection_capacity, &config.runtime_limits)
            .with_http_max_headers(config.max_headers);
    let analyzer = Analyzer::new();
    let request =
        r#"{"model":"fixture","messages":[{"role":"user","content":"hello"}],"stream":true}"#;
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: fixture.test\r\nContent-Length: {}\r\n\r\n{request}",
        request.len()
    );
    let chunk = br#"data: {"id":"fragment-fixture","model":"fixture","choices":[{"index":0,"delta":{"content":"reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}}

"#;
    let split = chunk.len() / 2;
    let mut reference = Vec::new();
    let mut actual = Vec::new();
    for (rw, bytes) in [
        (1, request.into_bytes()),
        (
            0,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".to_vec(),
        ),
        (0, chunk[..split].to_vec()),
        (0, chunk[split..].to_vec()),
        (0, b"data: [DONE]\n\n".to_vec()),
    ] {
        // Raw retention bypassed the compact fast path before extraction as well.
        for result in aggregator.process_result(parser.parse_event(captured(rw, bytes.clone()))) {
            reference.extend(analyzer.analyze_for_owned_delivery(result, true));
        }
        for result in core.process_event(captured(rw, bytes)) {
            actual.extend(core.analyze_for_owned_delivery(result, true));
        }
    }
    let semantic = |analysis: &[_]| {
        let (output, _) = GenAIBuilder::new().build_with_pending(
            analysis,
            &ResponseSessionMapper::new(),
            &HashMap::new(),
        );
        let [GenAISemanticEvent::LLMCall(call)] = output.events.as_slice() else {
            panic!("expected one call from retained evidence");
        };
        serde_json::json!({
            "model": call.model,
            "request": call.request,
            "response": call.response,
            "usage": call.token_usage,
        })
    };
    assert_eq!(semantic(&actual), semantic(&reference));
    assert!(core.drain_pending().is_empty());
}

fn captured(rw: i32, bytes: Vec<u8>) -> Event {
    Event::Ssl(crate::probes::sslsniff::SslEvent {
        source: 0,
        timestamp_ns: 1_000_000_000,
        delta_ns: 0,
        pid: std::process::id(),
        tid: 1,
        uid: 0,
        len: bytes.len() as u32,
        rw,
        comm: "fixture".into(),
        buf: bytes,
        is_handshake: false,
        ssl_ptr: 0x4242,
    })
}
