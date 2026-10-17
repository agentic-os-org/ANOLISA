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
                parser: Parser::default(),
                aggregator: Aggregator::new(),
                analyzer: Analyzer::new(),
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
